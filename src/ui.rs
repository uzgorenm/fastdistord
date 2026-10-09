//! Native, event-driven desktop interface on the fastframe resident shell.
//!
//! Audio and account actions are dispatched to the backend; static public CDN images
//! use a bounded worker. A password edit owns the only UI copy of the token.

mod chat;
mod images;
mod sounds;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, mpsc::Sender};
use std::time::{Duration, Instant};

use egui::{Align, Color32, FontId, Layout, RichText, Stroke, Vec2};
use fastframe_shell::{Closed, Headless, Held, Resident, Shell, Waker};
use fastframe_tray::{Config, Event, MenuItem, Tray};
use zeroize::{Zeroize, Zeroizing};

use crate::audio::TxGate;
use crate::model::{Command, DeviceChoice, Phase, UiState};

mod hotkey;
mod theme;

const METER_INTERVAL: Duration = Duration::from_millis(100);
const TOKEN_EDIT_ID: &str = "session_token";
const MESSAGE_EDIT_ID: &str = "message_draft";

/// Runs the native window and tray until the user explicitly quits.
///
/// Closing the window keeps an existing call alive only if a usable tray is
/// present. The shell returns all UI state to the next window it creates.
pub fn run(
    state: Arc<Mutex<UiState>>,
    commands: Sender<Command>,
    gate: Arc<TxGate>,
) -> anyhow::Result<()> {
    let waker = Waker::default();
    let repaint_waker = waker.clone();
    commands
        .send(Command::SetUiRepaint(Arc::new(move || {
            repaint_waker.wake()
        })))
        .map_err(|_| anyhow::anyhow!("The voice service has stopped."))?;
    let app = VoiceApp::new(state, commands, gate, &waker);
    Shell::new(app, &waker)
        .idle(fastframe_tray::idle)
        .run(|lease| {
            let options = eframe::NativeOptions {
                viewport: egui::ViewportBuilder::default()
                    .with_title("fastdistord")
                    .with_inner_size([960.0, 660.0])
                    .with_min_inner_size([740.0, 540.0])
                    .with_icon(egui::IconData {
                        rgba: icon_rgba(64),
                        width: 64,
                        height: 64,
                    }),
                renderer: eframe::Renderer::Glow,
                ..Default::default()
            };
            eframe::run_native(
                "fastdistord",
                options,
                Box::new(move |cc| {
                    theme::install(&cc.egui_ctx);
                    let mut app = lease.take(&cc.egui_ctx);
                    if let Some(tray) = &mut app.tray {
                        tray.attach();
                    }
                    app.hidden = false;
                    app.send(Command::SetUiVisible(true));
                    app.wants_show = false;
                    Ok(Box::new(Window {
                        app,
                        recovery_checked: false,
                        was_focused: true,
                        was_not_visible: false,
                    }))
                }),
            )
        })
        .map_err(|err| anyhow::anyhow!("Could not run the native window: {err}"))
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ServerSection {
    #[default]
    Chat,
    Voice,
}

struct VoiceApp {
    images: images::Cache,
    sounds: sounds::Player,
    sound_error: Option<String>,
    state: Arc<Mutex<UiState>>,
    commands: Sender<Command>,
    gate: Arc<TxGate>,
    ptt: hotkey::PushToTalk,
    tray: Option<Tray>,
    token: Zeroizing<String>,
    risk_accepted: bool,
    remember: bool,
    qr_login: Option<crate::qr_login::Login>,
    qr_code: Option<(qrcode::QrCode, std::time::Instant)>,
    qr_status: String,
    qr_retry_until: Option<Instant>,
    settings_open: bool,
    selected_input: Option<String>,
    selected_output: Option<String>,
    browsing_channel: Option<u64>,
    quit_requested: bool,
    hide_intent: bool,
    hidden: bool,
    wants_show: bool,
    shutdown_sent: bool,
    last_tray_revision: Option<u64>,
    local_error: Option<String>,
    text_open: bool,
    friends_open: bool,
    last_server: Option<u64>,
    expanded_server: Option<u64>,
    server_sections: HashMap<u64, ServerSection>,
    last_dm: Option<u64>,
    last_server_chat: Option<u64>,
    pending_chat_restore: Option<u64>,
    pending_dm_restore: Option<u64>,
    call_label: String,
    message_draft: String,
    draft_channel: Option<u64>,
    draft_reset_pending: bool,
    last_sent_revision: u64,
}

impl VoiceApp {
    fn new(
        state: Arc<Mutex<UiState>>,
        commands: Sender<Command>,
        gate: Arc<TxGate>,
        waker: &Waker,
    ) -> Self {
        let tray_waker = waker.clone();
        let tray = Tray::spawn(
            Config {
                id: "fastdistord",
                title: "fastdistord".into(),
                icon: icon_rgba,
                template_icon: Some(template_icon_rgba),
                themed_icon: false,
                menu_on_click: false,
                menu: vec![
                    MenuItem::action("show", "Show fastdistord"),
                    MenuItem::Separator,
                    MenuItem::action("mute", "Unmute microphone"),
                    MenuItem::action("deafen", "Deafen"),
                    MenuItem::action("leave", "Leave voice channel").enabled(false),
                    MenuItem::Separator,
                    MenuItem::action("quit", "Quit fastdistord"),
                ],
            },
            move || tray_waker.wake(),
        );
        let ptt = hotkey::PushToTalk::start(Arc::clone(&gate), waker.clone());
        Self {
            images: images::Cache::default(),
            sounds: sounds::Player::new({
                let sound_waker = waker.clone();
                move || sound_waker.wake()
            }),
            sound_error: None,
            state,
            commands,
            gate,
            ptt,
            tray,
            token: Zeroizing::new(String::new()),
            risk_accepted: false,
            remember: crate::credential::remembered(),
            qr_login: None,
            qr_code: None,
            qr_status: String::new(),
            qr_retry_until: None,
            settings_open: false,
            selected_input: None,
            selected_output: None,
            browsing_channel: None,
            quit_requested: false,
            hide_intent: false,
            hidden: false,
            wants_show: false,
            shutdown_sent: false,
            last_tray_revision: None,
            local_error: None,
            text_open: true,
            friends_open: true,
            last_server: None,
            expanded_server: None,
            server_sections: HashMap::new(),
            last_dm: None,
            last_server_chat: None,
            pending_chat_restore: None,
            pending_dm_restore: None,
            call_label: String::new(),
            message_draft: String::new(),
            draft_channel: None,
            draft_reset_pending: false,
            last_sent_revision: 0,
        }
    }

    fn snapshot(&self) -> UiState {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn send(&mut self, command: Command) {
        if matches!(
            &command,
            Command::Leave
                | Command::Logout
                | Command::Quit
                | Command::CallDm(_)
                | Command::Join { .. }
                | Command::SetDeafened(true)
                | Command::SetCallSounds(false)
        ) {
            self.sounds.stop();
        }

        if matches!(&command, Command::Logout | Command::Quit) {
            self.images.suspend();
            self.qr_login = None;
            self.qr_code = None;
            self.last_server = None;
            self.expanded_server = None;
            self.server_sections.clear();
            self.last_dm = None;
            self.last_server_chat = None;
            self.pending_chat_restore = None;
        }
        if matches!(
            &command,
            Command::SelectGuild(_)
                | Command::SelectTextChannel(_)
                | Command::SelectDm(_)
                | Command::OpenDm(_)
                | Command::Logout
                | Command::Quit
        ) {
            self.message_draft.zeroize();
            self.draft_channel = None;
            self.draft_reset_pending = true;
        }
        // Privacy-closing controls act immediately, even if the backend is
        // busy. Opening transmission is left to the authoritative backend.
        match &command {
            Command::SetMuted(true) => self.gate.set_muted(true),
            Command::SetDeafened(true) => self.gate.set_deafened(true),
            Command::Leave | Command::Join { .. } | Command::Reconnect => {
                self.gate.set_suppressed(true)
            }
            Command::Logout | Command::Quit => self.gate.fail_closed(),
            _ => {}
        }
        if self.commands.send(command).is_err() {
            self.gate.fail_closed();
            self.local_error =
                Some("The voice service has stopped. Quit and restart the app.".into());
        }
    }

    fn release_ptt(&self) {
        self.ptt.clear();
    }

    fn background(&mut self, ctx: &egui::Context) {
        let sound_state = self.snapshot();
        if let Some(error) = self.sounds.sync(&sound_state) {
            self.sound_error = Some(error.into());
        }

        if self.draft_reset_pending {
            clear_message_editor(ctx);
        }
        let events: Vec<_> = self.tray.as_ref().map(Tray::events).unwrap_or_default();
        let (muted, deafened, has_account, phase, revision, ptt_enabled) = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (
                state.muted,
                state.deafened,
                state.account.is_some(),
                state.phase,
                state.revision,
                state.ptt_enabled,
            )
        };
        self.ptt.configure(ptt_enabled);
        for event in events {
            match event {
                Event::Toggle if !self.hidden => {
                    self.hide_intent = true;
                    self.release_ptt();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                Event::Toggle | Event::Show | Event::Menu("show") => {
                    if self.hidden {
                        self.wants_show = true;
                    } else {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    }
                }
                Event::Menu("mute") => {
                    let retry = !muted
                        && sound_state.phase == Phase::VoiceReady
                        && sound_state.confirmed_muted == Some(true);
                    self.send(Command::SetMuted(if retry { false } else { !muted }));
                }
                Event::Menu("deafen") => {
                    let retry = !deafened && sound_state.confirmed_deafened == Some(true);
                    self.send(Command::SetDeafened(if retry { false } else { !deafened }));
                }
                Event::Menu("leave") => {
                    self.release_ptt();
                    self.send(Command::Leave);
                }
                Event::Menu("quit") => self.request_quit(ctx),
                Event::Menu(_) => {}
            }
        }
        if self.last_tray_revision != Some(revision) {
            if let Some(tray) = &mut self.tray {
                tray.set_label(
                    "mute",
                    if muted {
                        "Unmute microphone"
                    } else if sound_state.phase == Phase::VoiceReady
                        && sound_state.confirmed_muted == Some(true)
                    {
                        "Unmute on Discord"
                    } else if !self.gate.transmit_allowed() {
                        "Cancel pending unmute"
                    } else {
                        "Mute microphone"
                    },
                );
                tray.set_label("deafen", if deafened { "Undeafen" } else { "Deafen" });
                tray.set_enabled(
                    "mute",
                    has_account
                        && !sound_state.deafened
                        && !sound_state.server_deafened
                        && sound_state.confirmed_deafened != Some(true),
                );
                tray.set_enabled("deafen", has_account);
                tray.set_enabled("leave", has_voice_session(phase));
                tray.set_tooltip(format!(
                    "fastdistord\n{}\n{}",
                    voice_phase_label(&sound_state),
                    crate::calls::mic_reason(&sound_state, self.gate.transmit_allowed())
                ));
            }
            self.last_tray_revision = Some(revision);
        }
    }

    fn request_quit(&mut self, ctx: &egui::Context) {
        self.quit_requested = true;
        self.hide_intent = false;
        self.release_ptt();
        self.send_shutdown();
        clear_message_editor(ctx);
        if !self.hidden {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn send_shutdown(&mut self) {
        if !self.shutdown_sent {
            self.shutdown_sent = true;
            self.sounds.stop();
            self.images.suspend();
            self.gate.fail_closed();
            self.ptt.stop();
            self.token.zeroize();
            self.message_draft.zeroize();
            self.draft_reset_pending = true;
            self.release_ptt();
            let _ = self.commands.send(Command::Quit);
        }
    }

    fn navigation(&mut self, ui: &mut egui::Ui, state: &UiState) {
        let width = ((ui.available_width() - ui.spacing().item_spacing.x) * 0.5).max(60.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !state.chat_sending,
                    egui::Button::selectable(self.friends_open, "Friends")
                        .min_size(Vec2::new(width, 34.0)),
                )
                .clicked()
                && !self.friends_open
            {
                self.friends_open = true;
            }
            if ui
                .add_enabled(
                    !state.chat_sending,
                    egui::Button::selectable(!self.friends_open, "Servers")
                        .min_size(Vec2::new(width, 34.0)),
                )
                .clicked()
                && self.friends_open
            {
                self.friends_open = false;
                self.expanded_server = None;
            }
        });
        ui.add_space(18.0);
        egui::ScrollArea::vertical()
            .id_salt("sidebar_list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.friends_open {
                    if ui
                        .add_enabled(
                            !state.social_busy && !state.chat_busy && !state.chat_sending,
                            egui::Button::new("Refresh friends").small(),
                        )
                        .clicked()
                    {
                        self.send(Command::RefreshSocial);
                    }
                    if state.social_busy {
                        ui.label(RichText::new("Loading…").color(theme::SECONDARY));
                    }
                    if !state.social_status.is_empty() {
                        ui.label(
                            RichText::new(&state.social_status)
                                .size(12.0)
                                .color(theme::SECONDARY),
                        );
                    }
                    for friend in &state.friends {
                        let dm = state
                            .direct_channels
                            .iter()
                            .find(|d| d.recipients.len() == 1 && d.recipients[0].id == friend.id);
                        let selected = dm.is_some_and(|d| state.selected_dm == Some(d.id));
                        ui.horizontal(|ui| {
                            self.profile_avatar(ui, state, friend.id, &friend.name, false);
                            if ui
                                .add_enabled(
                                    !state.chat_sending && (dm.is_some() || !state.social_busy),
                                    navigation_row(selected, &friend.name)
                                        .truncate()
                                        .min_size(Vec2::new(ui.available_width(), 36.0)),
                                )
                                .on_hover_text(&friend.name)
                                .clicked()
                            {
                                self.text_open = true;
                                if let Some(dm) = dm {
                                    self.last_dm = Some(dm.id);
                                    self.send(Command::SelectDm(dm.id));
                                } else {
                                    self.send(Command::OpenDm(friend.id));
                                }
                            }
                        });
                    }
                    let other: Vec<_> = state
                        .direct_channels
                        .iter()
                        .filter(|d| {
                            d.recipients.len() != 1
                                || !state.friends.iter().any(|f| f.id == d.recipients[0].id)
                        })
                        .collect();
                    if !other.is_empty() {
                        ui.add_space(12.0);
                        section_label(ui, "CONVERSATIONS");
                        ui.add_space(6.0);
                    }
                    for dm in other {
                        ui.horizontal(|ui| {
                            if dm.recipients.len() == 1 {
                                self.profile_avatar(
                                    ui,
                                    state,
                                    dm.recipients[0].id,
                                    &dm.name,
                                    false,
                                );
                            } else {
                                portrait(ui, &dm.name, None, None, false);
                            }
                            if ui
                                .add_enabled(
                                    !state.chat_sending,
                                    navigation_row(state.selected_dm == Some(dm.id), &dm.name)
                                        .truncate()
                                        .min_size(Vec2::new(ui.available_width(), 36.0)),
                                )
                                .on_hover_text(&dm.name)
                                .clicked()
                            {
                                self.text_open = true;
                                self.last_dm = Some(dm.id);
                                self.send(Command::SelectDm(dm.id));
                            }
                        });
                    }
                    if state.friends.is_empty()
                        && state.direct_channels.is_empty()
                        && !state.social_busy
                        && state.social_status.is_empty()
                    {
                        ui.label("No friends or conversations.");
                    }
                } else {
                    for guild in &state.guilds {
                        ui.push_id(guild.id, |ui| {
                            let selected = state.selected_guild == Some(guild.id);
                            ui.horizontal(|ui| {
                                let texture = guild.icon.as_ref().and_then(|hash| {
                                    self.images.get(images::Key {
                                        guild: true,
                                        id: guild.id,
                                        hash: hash.clone(),
                                    })
                                });
                                portrait(ui, &guild.name, texture.as_ref(), None, false);
                                if ui
                                    .add_enabled(
                                        !state.chat_sending,
                                        navigation_row(selected, &guild.name)
                                            .truncate()
                                            .min_size(Vec2::new(ui.available_width(), 36.0)),
                                    )
                                    .on_hover_text(&guild.name)
                                    .clicked()
                                {
                                    if selected {
                                        self.expanded_server =
                                            toggle_server(self.expanded_server, guild.id);
                                    } else {
                                        self.expanded_server = Some(guild.id);
                                        self.last_server = Some(guild.id);
                                        self.last_server_chat = None;
                                        self.pending_chat_restore = None;
                                        self.browsing_channel = None;
                                        self.text_open = true;
                                        self.send(Command::SelectGuild(guild.id));
                                    }
                                }
                            });
                            if selected && self.expanded_server == Some(guild.id) {
                                ui.indent("channels", |ui| {
                                    let section = self.server_sections.entry(guild.id).or_default();
                                    ui.horizontal(|ui| {
                                        ui.selectable_value(section, ServerSection::Chat, "Chat");
                                        ui.selectable_value(section, ServerSection::Voice, "Voice");
                                    });
                                    ui.add_space(4.0);
                                    let section = *section;
                                    if section == ServerSection::Chat
                                        && state.text_channels.is_empty()
                                    {
                                        ui.label(
                                            RichText::new("No text channels available.")
                                                .small()
                                                .color(theme::SECONDARY),
                                        );
                                    }
                                    for channel in state
                                        .text_channels
                                        .iter()
                                        .filter(|_| section == ServerSection::Chat)
                                    {
                                        if ui
                                            .add_enabled(
                                                !state.chat_sending,
                                                navigation_row(
                                                    self.text_open
                                                        && state.selected_text_channel
                                                            == Some(channel.id),
                                                    format!("# {}", channel.name),
                                                )
                                                .truncate()
                                                .min_size(Vec2::new(ui.available_width(), 32.0)),
                                            )
                                            .on_hover_text(&channel.name)
                                            .clicked()
                                        {
                                            self.text_open = true;
                                            self.browsing_channel = None;
                                            self.last_server_chat = Some(channel.id);
                                            self.send(Command::SelectTextChannel(channel.id));
                                        }
                                    }
                                    if section == ServerSection::Voice
                                        && !state.channels.iter().any(|c| c.guild_id == guild.id)
                                    {
                                        ui.label(
                                            RichText::new("No voice channels available.")
                                                .small()
                                                .color(theme::SECONDARY),
                                        );
                                    }
                                    for channel in state.channels.iter().filter(|c| {
                                        c.guild_id == guild.id && section == ServerSection::Voice
                                    }) {
                                        let active = state.selected_channel == Some(channel.id)
                                            && state.selected_call_guild == Some(guild.id)
                                            && has_voice_session(state.phase);
                                        let label = if active {
                                            format!("● {}", channel.name)
                                        } else {
                                            format!("Join · {}", channel.name)
                                        };
                                        let enabled = active
                                            || !matches!(
                                                state.phase,
                                                Phase::Connecting
                                                    | Phase::Joining
                                                    | Phase::Reconnecting
                                            );
                                        if ui
                                            .add_enabled(
                                                enabled,
                                                navigation_row(
                                                    !self.text_open
                                                        && self.browsing_channel
                                                            == Some(channel.id),
                                                    label,
                                                )
                                                .truncate()
                                                .min_size(Vec2::new(ui.available_width(), 32.0)),
                                            )
                                            .on_hover_text(if active {
                                                format!("{} · connected", channel.name)
                                            } else {
                                                format!("Join {}", channel.name)
                                            })
                                            .clicked()
                                        {
                                            self.text_open = false;
                                            self.browsing_channel = Some(channel.id);
                                            if !active {
                                                self.call_label =
                                                    format!("{} / {}", guild.name, channel.name);
                                                self.release_ptt();
                                                self.local_error = None;
                                                self.send(Command::Join {
                                                    guild_id: guild.id,
                                                    channel_id: channel.id,
                                                });
                                            }
                                        }
                                    }
                                });
                                ui.add_space(8.0);
                            }
                        });
                    }
                    if state.guilds.is_empty() {
                        ui.label("No servers available.");
                    }
                }
            });
    }

    fn connect_panel(&mut self, ui: &mut egui::Ui, state: &UiState) {
        ui.set_max_width(440.0);
        ui.horizontal(|ui| {
            ui.heading("Connect to Discord");
            if ui.small_button("Settings").clicked() {
                self.settings_open = true;
                self.send(Command::RefreshDevices);
            }
        });
        ui.add_space(6.0);
        ui.label(RichText::new("Voice and text, in a small native app.").color(theme::SECONDARY));
        ui.add_space(16.0);
        ui.label("This unofficial login gives Fastdistord broad access to your Discord account and may put your account at risk.");
        ui.collapsing("Details", |ui| {
            ui.label("This is a personal-account session, not a limited OAuth grant. Discord may restrict your account. Only approve a code you started here. Logout removes local access; use Discord’s device/session controls to revoke access, or change your Discord password to end all sessions.");
            ui.hyperlink_to("Discord account policy", "https://support.discord.com/hc/en-us/articles/115002192352-Automated-User-Accounts-Self-Bots");
        });
        ui.add_space(12.0);
        #[cfg(target_os = "macos")]
        ui.add_enabled_ui(self.qr_login.is_none(), |ui| {
            ui.checkbox(&mut self.remember, "Remember me — save in macOS Keychain");
        });
        ui.label(
            RichText::new(if self.remember {
                "Save after successful login and reconnect on future launches. macOS may ask for Keychain access."
            } else {
                "Session only. Automatic login is disabled."
            })
            .size(12.0)
            .color(theme::SECONDARY),
        );
        if !state.login_storage_status.is_empty() {
            ui.label(&state.login_storage_status);
        }
        let connecting = matches!(state.phase, Phase::Connecting | Phase::Reconnecting);
        let events: Vec<_> = self
            .qr_login
            .as_ref()
            .map(|login| login.events.try_iter().collect())
            .unwrap_or_default();
        for event in events {
            match event {
                crate::qr_login::Event::Code(code, expires) => {
                    self.qr_code = Some((code, expires));
                    self.qr_status = "Scan with Discord on your phone, then approve there.".into();
                }
                crate::qr_login::Event::AwaitingApproval => {
                    self.qr_code = None;
                    self.qr_status = "Scanned. Approve or cancel on your phone.".into();
                }
                crate::qr_login::Event::Token(mut token) => {
                    self.qr_code = None;
                    self.qr_login = None;
                    self.qr_status.clear();
                    self.send(Command::Connect {
                        token: std::mem::take(&mut *token),
                        risk_accepted: true,
                        remember: self.remember,
                    });
                }
                crate::qr_login::Event::Failed(message, retry_after) => {
                    self.qr_retry_until =
                        retry_after.and_then(|delay| Instant::now().checked_add(delay));
                    self.qr_code = None;
                    self.qr_login = None;
                    self.qr_status = message;
                }
            }
        }
        if let Some((_, expires)) = &self.qr_code
            && std::time::Instant::now() >= *expires
        {
            self.qr_code = None;
            self.qr_login = None;
            self.qr_status = "Code expired. Choose Connect for a fresh code.".into();
        }
        if let Some((code, _)) = &self.qr_code {
            // Four-module quiet zone, integer pixel modules, opaque black/white.
            let module = (216.0 / (code.width() + 8) as f32).floor().max(1.0);
            let size = module * (code.width() + 8) as f32;
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
            ui.painter().rect_filled(rect, 0.0, Color32::WHITE);
            for y in 0..code.width() {
                for x in 0..code.width() {
                    if code[(x, y)] == qrcode::Color::Dark {
                        let pos =
                            rect.min + Vec2::new((x + 4) as f32 * module, (y + 4) as f32 * module);
                        ui.painter().rect_filled(
                            egui::Rect::from_min_size(pos, Vec2::splat(module)),
                            0.0,
                            Color32::BLACK,
                        );
                    }
                }
            }
        }
        if self.qr_login.is_some() {
            ui.ctx().request_repaint_after(Duration::from_secs(1));
            if ui.button("Cancel login").clicked() {
                self.qr_login = None;
                self.qr_code = None;
                self.qr_status = "Login canceled.".into();
            }
        } else if primary_button(
            ui,
            "Connect with QR code",
            !connecting
                && self
                    .qr_retry_until
                    .is_none_or(|until| Instant::now() >= until),
        )
        .clicked()
        {
            self.token.zeroize();
            self.risk_accepted = true;
            self.qr_status = "Creating a fresh login code…".into();
            match crate::qr_login::Login::start(ui.ctx().clone()) {
                Ok(login) => self.qr_login = Some(login),
                Err(_) => {
                    self.qr_status = "Could not start login. Choose Connect to try again.".into()
                }
            }
        }
        if !self.qr_status.is_empty() {
            ui.label(&self.qr_status);
        }
        if let Some(until) = self.qr_retry_until.filter(|until| *until > Instant::now()) {
            ui.label(format!(
                "QR login available in {} seconds",
                until
                    .saturating_duration_since(Instant::now())
                    .as_secs()
                    .saturating_add(1)
            ));
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        }
        #[cfg(target_os = "macos")]
        if primary_button(
            ui,
            "Connect from Keychain",
            !connecting && self.qr_login.is_none(),
        )
        .clicked()
        {
            self.risk_accepted = true;
            self.send(Command::ConnectSaved {
                risk_accepted: true,
            });
        }
        ui.collapsing("Use an existing session credential", |ui| {
            ui.checkbox(
                &mut self.risk_accepted,
                "I understand the account-access risk.",
            );
            ui.add(
                egui::TextEdit::singleline(&mut *self.token)
                    .id(egui::Id::new(TOKEN_EDIT_ID))
                    .password(true)
                    .hint_text("Enter only locally"),
            );
            if primary_button(
                ui,
                "Connect",
                connect_allowed(&self.token, self.risk_accepted, connecting)
                    && self.qr_login.is_none(),
            )
            .clicked()
            {
                let token = std::mem::take(&mut *self.token);
                self.send(Command::Connect {
                    token,
                    risk_accepted: true,
                    remember: self.remember,
                });
            }
        });
        if let Some(mut edit_state) =
            egui::TextEdit::load_state(ui.ctx(), egui::Id::new(TOKEN_EDIT_ID))
        {
            edit_state.clear_undoer();
            edit_state.store(ui.ctx(), egui::Id::new(TOKEN_EDIT_ID));
        }
        ui.add_space(9.0);
        ui.label(
            RichText::new(
                "Microphone muted by default. Audio devices open only when you join voice.",
            )
            .size(12.0)
            .color(theme::SECONDARY),
        );
    }

    fn call_panel(&mut self, ui: &mut egui::Ui, state: &UiState) {
        ui.add_space(7.0);
        ui.heading(if self.call_label.is_empty() {
            "Voice"
        } else {
            &self.call_label
        });
        status_badge(ui, state);
        ui.add_space(16.0);
        if state.phase != Phase::VoiceReady {
            ui.label(
                RichText::new(if has_voice_session(state.phase) {
                    if state.phase == Phase::VoiceWaiting {
                        voice_wait_detail(state)
                    } else {
                        "Connecting encrypted voice…"
                    }
                } else {
                    "Choose a voice channel on the left to join."
                })
                .color(theme::SECONDARY),
            );
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("participants")
            .show(ui, |ui| {
                if state.participants.is_empty() {
                    ui.label(RichText::new("Waiting for participants…").color(theme::SECONDARY));
                }
                for person in &state.participants {
                    ui.push_id(person.id, |ui| {
                        egui::Frame::new()
                            .fill(if person.speaking {
                                theme::SPEAKING_BG
                            } else {
                                Color32::TRANSPARENT
                            })
                            .corner_radius(10)
                            .inner_margin(egui::Margin::symmetric(13, 10))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    self.profile_avatar(
                                        ui,
                                        state,
                                        person.id,
                                        &person.name,
                                        person.speaking,
                                    );
                                    ui.add_space(8.0);
                                    ui.add(
                                        egui::Label::new(RichText::new(&person.name).strong())
                                            .truncate(),
                                    )
                                    .on_hover_text(&person.name);
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        let label = if person.deafened {
                                            "Deafened"
                                        } else if person.muted {
                                            "Muted"
                                        } else if person.speaking {
                                            "Speaking"
                                        } else {
                                            "Listening"
                                        };
                                        ui.label(RichText::new(label).size(12.0).color(
                                            if person.speaking {
                                                theme::SUCCESS
                                            } else {
                                                theme::SECONDARY
                                            },
                                        ));
                                    });
                                });
                            });
                        ui.add_space(6.0);
                    });
                }
            });
    }

    fn sync_text_draft(&mut self, ctx: &egui::Context, state: &UiState) {
        if self.draft_reset_pending
            || state.account.is_none()
            || self.draft_channel != state.selected_text_channel
            || self.last_sent_revision != state.sent_revision
        {
            self.message_draft.zeroize();
            clear_message_editor(ctx);
            self.draft_reset_pending = false;
            self.draft_channel = state.selected_text_channel;
            self.last_sent_revision = state.sent_revision;
        }
    }

    fn text_panel(&mut self, ui: &mut egui::Ui, state: &UiState) {
        let selected = state.selected_text_channel;
        let dm = state
            .direct_channels
            .iter()
            .find(|d| Some(d.id) == state.selected_dm);
        let guild_channel = state.text_channels.iter().find(|c| Some(c.id) == selected);
        let Some(channel_id) = selected.filter(|_| dm.is_some() || guild_channel.is_some()) else {
            ui.add_space(28.0);
            ui.heading(if self.friends_open {
                "Friends"
            } else {
                "Your servers"
            });
            ui.label(
                RichText::new(if self.friends_open {
                    "Choose a friend to chat or call."
                } else {
                    "Choose a channel on the left."
                })
                .color(theme::SECONDARY),
            );
            if !state.chat_status.is_empty()
                && state.chat_status != "Choose a text channel to read its latest 50 messages."
            {
                ui.label(RichText::new(&state.chat_status).color(theme::DANGER));
            }
            return;
        };
        let name = dm
            .map(|d| d.name.clone())
            .unwrap_or_else(|| format!("# {}", guild_channel.unwrap().name));
        ui.horizontal(|ui| {
            if let Some(person) = dm
                .and_then(|d| d.recipients.first())
                .filter(|_| dm.is_some_and(|d| d.recipients.len() == 1))
            {
                self.profile_avatar(ui, state, person.id, &person.name, false);
            }
            ui.add_sized(
                Vec2::new((ui.available_width() - 150.0).max(60.0), 34.0),
                egui::Label::new(RichText::new(&name).heading())
                    .halign(Align::Min)
                    .truncate(),
            )
            .on_hover_text(&name);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(!state.chat_busy, egui::Button::new("Refresh").small())
                    .on_hover_text("Refresh messages")
                    .clicked()
                {
                    self.send(Command::RefreshMessages);
                }
                if dm.is_some_and(|d| d.recipients.len() == 1) {
                    let active = state.selected_call_dm == Some(channel_id)
                        && has_voice_session(state.phase);
                    if active {
                        if ui.button("End call").clicked() {
                            self.release_ptt();
                            self.send(Command::Leave);
                        }
                    } else if ui
                        .add_enabled(
                            !matches!(
                                state.phase,
                                Phase::Connecting | Phase::Joining | Phase::Reconnecting
                            ),
                            egui::Button::new("Call"),
                        )
                        .clicked()
                    {
                        self.call_label = name.clone();
                        self.release_ptt();
                        self.send(Command::CallDm(channel_id));
                    }
                }
            });
        });
        ui.add_space(16.0);
        if state.chat_busy {
            ui.label(RichText::new("Loading messages…").color(theme::SECONDARY));
        } else if !state.chat_status.is_empty()
            && state.chat_status != "Latest 50 messages · Refresh to check for updates."
            && state.chat_status != "Message sent."
        {
            ui.label(RichText::new(&state.chat_status).color(theme::DANGER));
        }
        let composer_width = (ui.available_width() - 66.0).max(80.0);
        let composer_height = ui
            .fonts_mut(|fonts| {
                fonts
                    .layout(
                        self.message_draft.clone(),
                        FontId::proportional(14.0),
                        theme::SECONDARY,
                        composer_width - 12.0,
                    )
                    .size()
                    .y
            })
            .clamp(20.0, 80.0)
            + 12.0;
        let counter_height = if self.message_draft.chars().count() > 1800 {
            24.0
        } else {
            0.0
        };
        let composer_gap = 4.0;
        let composer_spacing = composer_gap + 2.0 * ui.spacing().item_spacing.y;
        egui::ScrollArea::vertical()
            .id_salt(("messages", channel_id))
            .max_height(
                (ui.available_height() - composer_height - composer_spacing - counter_height)
                    .max(60.0),
            )
            .auto_shrink([false, true])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for (index, message) in state.messages.iter().enumerate() {
                    let previous = index.checked_sub(1).and_then(|i| state.messages.get(i));
                    let continuation = chat::continues(previous, message);
                    if index != 0 {
                        ui.add_space(if continuation { 6.0 } else { 16.0 });
                    }
                    ui.push_id(message.id, |ui| {
                        if let Some(text) = crate::messaging::system_label(
                            message,
                            state.account.as_ref().map(|a| a.id),
                        ) {
                            ui.label(RichText::new(text).size(12.0).color(theme::SECONDARY));
                        } else {
                            ui.horizontal_top(|ui| {
                                if continuation {
                                    ui.allocate_exact_size(
                                        Vec2::new(32.0, 0.0),
                                        egui::Sense::hover(),
                                    );
                                } else {
                                    self.profile_avatar(
                                        ui,
                                        state,
                                        message.author_id,
                                        &message.author_name,
                                        false,
                                    );
                                }
                                ui.vertical(|ui| {
                                    ui.set_width(ui.available_width());
                                    ui.spacing_mut().item_spacing.y = 4.0;
                                    if !continuation {
                                        ui.label(
                                            RichText::new(&message.author_name).font(
                                                fastframe_fonts::Weight::SemiBold.font_id(13.0),
                                            ),
                                        );
                                    }
                                    ui.add(
                                        egui::Label::new(if message.content.is_empty() {
                                            "[Attachment or non-text message]"
                                        } else {
                                            &message.content
                                        })
                                        .wrap()
                                        .selectable(true),
                                    );
                                });
                            });
                        }
                    });
                }
                if state.messages.is_empty() && !state.chat_busy {
                    ui.label(RichText::new("No messages yet.").color(theme::SECONDARY));
                }
            });
        ui.add_space(composer_gap);
        ui.horizontal(|ui| {
            egui::ScrollArea::vertical()
                .id_salt(("composer", channel_id))
                .max_width(composer_width)
                .max_height(92.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.set_width(composer_width);
                    ui.add_enabled(
                        !state.chat_sending,
                        egui::TextEdit::multiline(&mut self.message_draft)
                            .id(egui::Id::new(MESSAGE_EDIT_ID))
                            .char_limit(crate::messaging::MAX_MESSAGE_CHARS)
                            .hint_text("Message…")
                            .desired_rows(1)
                            .desired_width(composer_width - 12.0),
                    );
                });
            let valid = crate::messaging::send_payload(&self.message_draft).is_ok();
            if ui
                .add_enabled(
                    valid && !state.chat_busy && !state.chat_sending,
                    egui::Button::new(if state.chat_sending { "…" } else { "Send" })
                        .min_size(Vec2::new(56.0, 32.0)),
                )
                .on_hover_text(if state.chat_sending {
                    "Sending message…"
                } else {
                    "Send message · Enter inserts a new line"
                })
                .clicked()
            {
                self.send(Command::SendMessage {
                    channel_id,
                    content: self.message_draft.clone(),
                });
            }
        });
        if self.message_draft.chars().count() > 1800 {
            ui.label(
                RichText::new(format!("{}/2000", self.message_draft.chars().count()))
                    .small()
                    .color(theme::SECONDARY),
            );
        }
    }

    fn profile_avatar(
        &mut self,
        ui: &mut egui::Ui,
        state: &UiState,
        id: u64,
        name: &str,
        speaking: bool,
    ) {
        let profile = state.profiles.get(&id);
        let texture = profile.and_then(|p| p.avatar.as_ref()).and_then(|hash| {
            self.images.get(images::Key {
                guild: false,
                id,
                hash: hash.clone(),
            })
        });
        portrait(
            ui,
            name,
            texture.as_ref(),
            Some(profile.map_or(crate::profiles::Presence::Unknown, |p| p.presence)),
            speaking,
        );
    }

    fn controls(&mut self, ui: &mut egui::Ui, state: &UiState) {
        ui.horizontal_wrapped(|ui| {
            let active = has_voice_session(state.phase);
            if let Some(account) = &state.account {
                self.profile_avatar(ui, state, account.id, &account.name, false);
                ui.add_sized(
                    Vec2::new(96.0, 32.0),
                    egui::Label::new(RichText::new(&account.name).strong()).truncate(),
                )
                .on_hover_text(format!(
                    "{} · {}",
                    account.name,
                    state
                        .profiles
                        .get(&account.id)
                        .map_or(crate::profiles::Presence::Unknown, |p| p.presence)
                        .label()
                ));
            }
            let transmitting = self.gate.transmit_allowed() && state.phase == Phase::VoiceReady;
            let retry_unmute = !state.muted
                && state.phase == Phase::VoiceReady
                && state.confirmed_muted == Some(true);
            let mic_label = if retry_unmute {
                "Unmute on Discord"
            } else if state.muted {
                "Unmute microphone"
            } else if state.phase == Phase::VoiceReady
                && state.confirmed_muted == Some(false)
                && state.confirmed_deafened == Some(false)
                && !state.server_suppressed
            {
                "Mute microphone"
            } else if !transmitting {
                "Cancel unmute · microphone remains closed"
            } else {
                "Mute microphone"
            };
            if control_icon(
                ui,
                ControlIcon::Mic,
                !transmitting,
                !state.deafened && !state.server_deafened && state.confirmed_deafened != Some(true),
                mic_label,
            )
            .on_hover_text(crate::calls::mic_reason(state, transmitting))
            .clicked()
            {
                self.send(Command::SetMuted(if retry_unmute {
                    false
                } else {
                    !state.muted
                }));
            }
            let effective_deaf =
                state.deafened || state.server_deafened || state.confirmed_deafened == Some(true);
            if control_icon(
                ui,
                ControlIcon::Headphones,
                effective_deaf,
                !state.server_deafened,
                if effective_deaf { "Undeafen" } else { "Deafen" },
            )
            .clicked()
            {
                self.send(Command::SetDeafened(
                    if !state.deafened && state.confirmed_deafened == Some(true) {
                        false
                    } else {
                        !state.deafened
                    },
                ));
            }
            if control_icon(ui, ControlIcon::Settings, false, true, "Settings").clicked() {
                self.settings_open = true;
                self.selected_input = state.selected_input.clone();
                self.selected_output = state.selected_output.clone();
                self.send(Command::RefreshDevices);
            }
            if let Some(call) = &state.current_call {
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.set_width((ui.available_width() - 180.0).clamp(100.0, 260.0));
                    ui.add(egui::Label::new(RichText::new(&call.target).strong()).truncate())
                        .on_hover_text(&call.target);
                    let mic_reason = crate::calls::mic_reason(state, transmitting);
                    let detail = if call.stage == crate::calls::Stage::Active
                        || state.server_suppressed
                        || matches!(
                            state.microphone_permission,
                            crate::microphone::Permission::Denied
                                | crate::microphone::Permission::Restricted
                        ) {
                        mic_reason
                    } else {
                        call.stage.label()
                    };
                    let seconds = (call.elapsed
                        + call
                            .active_since
                            .map_or(Duration::ZERO, |start| start.elapsed()))
                    .as_secs();
                    ui.add(
                        egui::Label::new(RichText::new(detail).size(12.0).color(theme::SECONDARY))
                            .truncate(),
                    )
                    .on_hover_text(format!(
                        "{}\n{}\n{} · {}:{:02}",
                        call.stage.label(),
                        mic_reason,
                        if state.participants.is_empty() {
                            "Waiting for participant state".into()
                        } else {
                            format!("{} participants", state.participants.len())
                        },
                        seconds / 60,
                        seconds % 60
                    ));
                });
                if ui
                    .small_button(if call.stage == crate::calls::Stage::Connecting {
                        "Cancel"
                    } else {
                        "Leave"
                    })
                    .clicked()
                {
                    self.release_ptt();
                    self.send(Command::Leave);
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if state.ptt_enabled {
                    let talk = ui.add_enabled(
                        state.phase == Phase::VoiceReady && !state.muted && !state.deafened,
                        egui::Button::new("Hold to talk"),
                    );
                    self.ptt.local_pressed(
                        (talk.is_pointer_button_down_on()
                            || (talk.has_focus()
                                && ui.input(|i| {
                                    i.key_down(egui::Key::Space) || i.key_down(egui::Key::Enter)
                                })))
                            && ui.input(|i| i.focused),
                    );
                } else {
                    self.ptt.local_pressed(false);
                }
                if active {
                    meter(
                        ui,
                        state.input_level,
                        state.phase == Phase::VoiceReady && !state.muted && !state.deafened,
                    );
                }
            });
        });
    }

    fn settings(&mut self, ctx: &egui::Context, state: &UiState) {
        if !self.settings_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .default_pos(egui::pos2((ctx.content_rect().width() - 460.0).max(24.0) * 0.5, 24.0))
            .default_height((ctx.content_rect().height() - 100.0).clamp(320.0, 580.0))
            .max_height((ctx.content_rect().height() - 80.0).max(180.0))
            .vscroll(true)
            .show(ctx, |ui| {
                ui.label(RichText::new("Audio devices").font(fastframe_fonts::Weight::SemiBold.font_id(16.0)));
                ui.add_space(9.0);
                ui.label("Microphone");
                device_picker(ui, "input_device", &mut self.selected_input, &state.input_devices);
                ui.add_space(9.0);
                ui.label("Speakers or headphones");
                device_picker(ui, "output_device", &mut self.selected_output, &state.output_devices);
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Refresh devices").clicked() {
                        self.send(Command::RefreshDevices);
                    }
                    if ui.button("Save devices").clicked() {
                        self.release_ptt();
                        self.send(Command::SetDevices {
                            input: self.selected_input.clone(),
                            output: self.selected_output.clone(),
                        });
                    }
                });
                ui.add_space(10.0);
                ui.label(RichText::new("Saved devices are used on your next call. Leave and rejoin to switch devices during a call.").size(12.0).color(theme::SECONDARY));
                ui.add_space(12.0);
                let mut ptt = state.ptt_enabled;
                if ui.checkbox(&mut ptt, "Push-to-talk").changed() {
                    self.release_ptt();
                    self.send(Command::SetPtt(ptt));
                }
                if ptt || !self.ptt.available() { ui.label(RichText::new(self.ptt.status()).size(12.0).color(theme::SECONDARY)); }
                let mut volume = state.output_volume;
                if ui.add(egui::Slider::new(&mut volume, 0.0..=2.0).text("Voice volume")).changed() {
                    self.send(Command::SetOutputVolume(volume));
                }
                let mut call_sounds = state.call_sounds;
                if ui.checkbox(&mut call_sounds, "Call sounds").changed() { self.send(Command::SetCallSounds(call_sounds)); }
                let mut sound_volume = state.sound_volume;
                if ui.add_enabled(call_sounds, egui::Slider::new(&mut sound_volume, 0.0..=1.0).text("Call sound volume")).changed() { self.send(Command::SetSoundVolume(sound_volume)); }
                if let Some(error) = &self.sound_error { ui.label(RichText::new(error).size(12.0).color(theme::DANGER)); }
                ui.label(state.microphone_permission.label());
                if storage_notice(&state.login_storage_status) { ui.label(&state.login_storage_status); }
                if matches!(state.microphone_permission, crate::microphone::Permission::Denied | crate::microphone::Permission::Restricted) {
                    ui.label("Allow microphone access in System Settings → Privacy & Security → Microphone, then rejoin.");
                }
                ui.collapsing("Privacy & diagnostics", |ui| {
                    ui.label("Use headphones. Echo cancellation is not available.");
                    let mut tracing = state.voice_handshake.enabled();
                    if ui.checkbox(&mut tracing, "Keep a redacted connection trace").changed() { state.voice_handshake.set_enabled(tracing); }
                    ui.label(state.voice_handshake.summary());
                    if tracing {
                        if ui.button("Copy connection trace").clicked() { ui.ctx().copy_text(state.voice_handshake.trace()); }
                        ui.label("Stored in memory only. Includes connection events and timing, never keys, account IDs or message content.");
                    }
                    ui.label("The Hold to talk button releases when this window loses focus. Audio is not recorded.");
                    ui.label(if self.tray.as_ref().is_some_and(Tray::is_shown) {
                        "Calls continue in the tray when you close this window. Quit ends the call."
                    } else {
                        "Closing this window ends the call and quits."
                    });
                });
                ui.add_space(12.0);
                ui.collapsing("About", |ui| {
                    ui.label(RichText::new("built with vibes by Mehmet Serhat Uzgoren").size(12.0).color(theme::SECONDARY));
                    ui.label(RichText::new(format!("Build {}", option_env!("FASTDISTORD_BUILD_COMMIT").unwrap_or("development"))).size(12.0).color(theme::SECONDARY));
                    ui.label("Unofficial Discord client. Personal-account access may break or lead to account restrictions.");
                });
                ui.add_space(15.0);
                ui.separator();
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(state.account.is_some(), egui::Button::new(if cfg!(target_os = "macos") { "Log out & forget login" } else { "Log out" })).clicked() {
                        self.release_ptt();
                        self.token.zeroize();
                        self.risk_accepted = false;
                        self.remember = false;

                        self.browsing_channel = None;
                        self.qr_login = None;
                        self.qr_code = None;
                        self.send(Command::Logout);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button(RichText::new("Quit fastdistord").color(theme::DANGER)).clicked() {
                            self.request_quit(ctx);
                        }
                    });
                });
            });
        self.settings_open = open;
    }

    fn draw(&mut self, ui: &mut egui::Ui) {
        let state = self.snapshot();
        let ctx = ui.ctx().clone();
        if self.friends_open
            && let Some(dm) = state.selected_dm
        {
            self.last_dm = Some(dm);
        }
        if self.friends_open
            && !state.social_busy
            && state.selected_guild.is_none()
            && let Some(dm) = self.pending_dm_restore.take()
            && state.direct_channels.iter().any(|d| d.id == dm)
        {
            self.send(Command::SelectDm(dm));
        }
        if let Some(channel) = self.pending_chat_restore
            && state.text_channels.iter().any(|c| c.id == channel)
        {
            self.pending_chat_restore = None;
            self.send(Command::SelectTextChannel(channel));
        }
        self.sync_text_draft(&ctx, &state);
        self.images.sync(state.account.as_ref().map(|a| a.id), &ctx);
        if state.account.is_some() {
            egui::Panel::bottom("call_controls")
                .resizable(false)
                .frame(
                    egui::Frame::new()
                        .fill(theme::PANEL)
                        .inner_margin(egui::Margin::symmetric(14, 8)),
                )
                .show(ui, |ui| self.controls(ui, &state));
            egui::Panel::left("navigation")
                .exact_size((ctx.content_rect().width() * 0.32).clamp(180.0, 240.0))
                .resizable(false)
                .frame(
                    egui::Frame::new()
                        .fill(theme::PANEL)
                        .inner_margin(egui::Margin::symmetric(14, 16)),
                )
                .show(ui, |ui| self.navigation(ui, &state));
        }
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(theme::BACKGROUND)
                    .inner_margin(egui::Margin::symmetric(26, 20)),
            )
            .show(ui, |ui| {
                if let Some(error) = &self.local_error {
                    ui.colored_label(theme::DANGER, error);
                    ui.add_space(9.0);
                }
                if state.account.is_some() && storage_notice(&state.login_storage_status) {
                    ui.label(
                        RichText::new(&state.login_storage_status)
                            .small()
                            .color(theme::WARNING),
                    );
                    ui.add_space(7.0);
                }
                if !state.status.is_empty() {
                    ui.label(
                        RichText::new(if state.phase == Phase::VoiceWaiting {
                            voice_wait_detail(&state)
                        } else {
                            &state.status
                        })
                        .size(12.0)
                        .color(if state.phase == Phase::Failed {
                            theme::DANGER
                        } else {
                            theme::SECONDARY
                        }),
                    );
                    ui.add_space(7.0);
                }
                if state.account.is_some() && state.phase == Phase::Failed {
                    ui.horizontal(|ui| {
                        if primary_button(ui, "Reconnect", true).clicked() {
                            self.release_ptt();
                            self.send(Command::Reconnect);
                        }
                    });
                    ui.add_space(12.0);
                }
                if state.account.is_none() {
                    self.message_draft.zeroize();
                    self.draft_channel = None;
                    self.last_sent_revision = state.sent_revision;
                    egui::ScrollArea::vertical()
                        .id_salt("connect_panel")
                        .show(ui, |ui| {
                            let inset = ((ui.available_width() - 440.0) * 0.5).max(0.0);
                            ui.horizontal(|ui| {
                                ui.add_space(inset);
                                ui.vertical(|ui| self.connect_panel(ui, &state));
                            });
                        });
                } else {
                    if self.text_open {
                        self.text_panel(ui, &state);
                    } else {
                        self.call_panel(ui, &state);
                    }
                }
            });
        self.settings(&ctx, &state);

        // No idle animation or network polling. Backend/tray events wake us.
        // Only a visible, running input meter asks for a 10 Hz repaint.
        if meter_needs_repaint(
            state.phase,
            state.muted || !self.gate.transmit_allowed(),
            state.deafened,
            ctx.input(|i| {
                i.viewport().minimized.unwrap_or(false) || i.viewport().occluded.unwrap_or(false)
            }),
            self.hidden,
        ) {
            ctx.request_repaint_after(METER_INTERVAL);
        }
    }
}

impl Resident for VoiceApp {
    fn closed(&self) -> Closed {
        if !self.quit_requested
            && self.hide_intent
            && self.tray.as_ref().is_some_and(Tray::is_shown)
        {
            Closed::Hide
        } else {
            Closed::Quit
        }
    }

    fn window_gone(&mut self) {
        self.qr_login = None;
        self.qr_code = None;
        self.hidden = true;
        self.send(Command::SetUiVisible(false));
        self.hide_intent = false;
        self.wants_show = false;
        self.release_ptt();
        self.token.zeroize();
    }

    fn headless_frame(&mut self, ctx: &egui::Context) -> Headless {
        self.background(ctx);
        if self.quit_requested {
            Headless::Quit
        } else if self.wants_show || !self.tray.as_ref().is_some_and(Tray::is_shown) {
            // Losing the tray must not strand an invisible microphone process.
            Headless::Show
        } else {
            Headless::Wait
        }
    }

    fn shutdown(&mut self) {
        self.send_shutdown();
    }
}

impl Drop for VoiceApp {
    fn drop(&mut self) {
        // Shell::run intentionally skips Resident::shutdown on window errors.
        // Always stop backend/audio even if window creation or rendering fails.
        self.send_shutdown();
    }
}

struct Window {
    app: Held<VoiceApp>,
    recovery_checked: bool,
    was_focused: bool,
    was_not_visible: bool,
}

impl eframe::App for Window {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if !std::mem::replace(&mut self.recovery_checked, true) {
            fastframe_shell::window::recover_offscreen(ctx, frame);
        }
        self.app.background(ctx);
        let (focused, close_requested, not_visible) = ctx.input(|i| {
            (
                i.focused,
                i.viewport().close_requested(),
                i.viewport().minimized.unwrap_or(false) || i.viewport().occluded.unwrap_or(false),
            )
        });
        if (self.was_focused && !focused)
            || (!self.was_not_visible && not_visible)
            || close_requested
        {
            self.app.release_ptt();
        }
        self.was_focused = focused;
        if self.was_not_visible != not_visible {
            self.app.send(Command::SetUiVisible(!not_visible));
        }
        self.was_not_visible = not_visible;
        if close_requested && !self.app.quit_requested {
            self.app.hide_intent = self.app.tray.as_ref().is_some_and(Tray::is_shown);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.app.draw(ui);
    }

    fn persist_egui_memory(&self) -> bool {
        false
    }
}

fn clear_message_editor(ctx: &egui::Context) {
    // Undo points contain prior draft strings. Erasing the visible draft alone
    // would allow Cmd/Ctrl+Z to restore content from an earlier channel/account.
    if let Some(mut state) = egui::TextEdit::load_state(ctx, egui::Id::new(MESSAGE_EDIT_ID)) {
        state.clear_undoer();
        state.store(ctx, egui::Id::new(MESSAGE_EDIT_ID));
    }
}

fn connect_allowed(token: &str, risk_accepted: bool, connecting: bool) -> bool {
    risk_accepted && !connecting && !token.trim().is_empty()
}

fn toggle_server(expanded: Option<u64>, clicked: u64) -> Option<u64> {
    if expanded == Some(clicked) {
        None
    } else {
        Some(clicked)
    }
}

fn navigation_row<'a>(selected: bool, label: impl egui::IntoAtoms<'a>) -> egui::Button<'a> {
    egui::Button::selectable(selected, ()).left_text(label)
}

fn has_voice_session(phase: Phase) -> bool {
    matches!(
        phase,
        Phase::Joining | Phase::VoiceReady | Phase::VoiceWaiting | Phase::Reconnecting
    )
}

fn voice_phase_label(state: &UiState) -> &'static str {
    if state.phase == Phase::VoiceWaiting && state.voice_handshake.idle_without_peer() {
        "Joined · You’re alone"
    } else {
        phase_label(state.phase)
    }
}

fn voice_wait_detail(state: &UiState) -> &'static str {
    if state.voice_handshake.idle_without_peer() {
        "You’re alone. Microphone waits for encrypted voice."
    } else {
        "Joined. Waiting for encrypted voice; microphone closed."
    }
}

fn phase_label(phase: Phase) -> &'static str {
    match phase {
        Phase::Offline => "Offline",
        Phase::Connecting => "Connecting account",
        Phase::SignalingReady => "Account connected",
        Phase::Joining => "Joining voice",
        Phase::VoiceWaiting => "Joined · encryption pending",
        Phase::VoiceReady => "Voice connected",
        Phase::Reconnecting => "Reconnecting",
        Phase::Failed => "Connection failed",
    }
}

fn meter_needs_repaint(
    phase: Phase,
    muted: bool,
    deafened: bool,
    minimized: bool,
    hidden: bool,
) -> bool {
    phase == Phase::VoiceReady && !muted && !deafened && !minimized && !hidden
}

fn section_label(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .font(fastframe_fonts::Weight::SemiBold.font_id(11.0))
            .color(theme::SECONDARY),
    );
}

fn status_badge(ui: &mut egui::Ui, state: &UiState) {
    let phase = state.phase;
    let color = match phase {
        Phase::VoiceReady | Phase::SignalingReady => theme::SUCCESS,
        Phase::Connecting | Phase::Joining | Phase::VoiceWaiting | Phase::Reconnecting => {
            theme::WARNING
        }
        Phase::Failed => theme::DANGER,
        Phase::Offline => theme::SECONDARY,
    };
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(8.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 3.0, color);
        ui.label(
            RichText::new(voice_phase_label(state))
                .size(12.0)
                .color(color),
        );
    });
}

fn primary_button(ui: &mut egui::Ui, label: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(label).strong().color(theme::ON_ACCENT))
            .fill(theme::ACCENT)
            .min_size(Vec2::new(130.0, 35.0)),
    )
}

fn device_picker(
    ui: &mut egui::Ui,
    id: &str,
    selected: &mut Option<String>,
    devices: &[DeviceChoice],
) {
    let selected_text = match selected.as_deref() {
        None => "System default",
        Some(id) => devices
            .iter()
            .find(|device| device.id == id)
            .map(|device| device.name.as_str())
            .unwrap_or("Selected device unavailable"),
    };
    egui::ComboBox::from_id_salt(id)
        .width(ui.available_width())
        .selected_text(selected_text)
        .show_ui(ui, |ui| {
            ui.selectable_value(selected, None, "System default");
            for device in devices {
                let label = if device.is_default {
                    format!("{} · default", device.name)
                } else {
                    device.name.clone()
                };
                ui.selectable_value(selected, Some(device.id.clone()), label)
                    .on_hover_text(&device.id);
            }
        });
}

#[derive(Clone, Copy)]
enum ControlIcon {
    Mic,
    Headphones,
    Settings,
}
fn control_icon(
    ui: &mut egui::Ui,
    icon: ControlIcon,
    selected: bool,
    enabled: bool,
    label: &str,
) -> egui::Response {
    let response = ui.add_enabled(
        enabled,
        egui::Button::new("")
            .min_size(Vec2::splat(32.0))
            .selected(selected)
            .fill(if selected {
                theme::DANGER.gamma_multiply(0.18)
            } else {
                Color32::TRANSPARENT
            })
            .corner_radius(10),
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
    let center = response.rect.center();
    let p = ui.painter();
    let color = if selected {
        theme::DANGER
    } else if enabled {
        theme::TEXT
    } else {
        theme::SECONDARY
    };
    let stroke = Stroke::new(1.6, color);
    let line = |a: [f32; 2], b: [f32; 2]| {
        p.line_segment([center + Vec2::from(a), center + Vec2::from(b)], stroke)
    };
    match icon {
        ControlIcon::Mic => {
            p.rect_stroke(
                egui::Rect::from_center_size(center - Vec2::new(0.0, 3.0), Vec2::new(6.0, 11.0)),
                3,
                stroke,
                egui::StrokeKind::Middle,
            );
            p.add(egui::Shape::line(
                vec![
                    center + Vec2::new(-6.0, -2.0),
                    center + Vec2::new(-6.0, 3.0),
                    center + Vec2::new(-3.0, 6.0),
                    center + Vec2::new(3.0, 6.0),
                    center + Vec2::new(6.0, 3.0),
                    center + Vec2::new(6.0, -2.0),
                ],
                stroke,
            ));
            line([0.0, 6.0], [0.0, 9.0]);
            line([-4.0, 9.0], [4.0, 9.0]);
        }
        ControlIcon::Headphones => {
            let points = (0..=12)
                .map(|i| {
                    let angle = std::f32::consts::PI + i as f32 * std::f32::consts::PI / 12.0;
                    center + Vec2::new(angle.cos() * 8.0, angle.sin() * 8.0)
                })
                .collect();
            p.add(egui::Shape::line(points, stroke));
            for x in [-7.0, 7.0] {
                p.rect_stroke(
                    egui::Rect::from_center_size(center + Vec2::new(x, 3.0), Vec2::new(4.0, 8.0)),
                    2,
                    stroke,
                    egui::StrokeKind::Middle,
                );
            }
        }
        ControlIcon::Settings => {
            p.circle_stroke(center, 6.0, stroke);
            p.circle_stroke(center, 2.0, stroke);
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::TAU / 8.0;
                let d = Vec2::new(a.cos(), a.sin());
                p.line_segment([center + d * 6.0, center + d * 9.0], stroke);
            }
        }
    }
    if selected {
        line([-9.0, -9.0], [9.0, 9.0]);
    }
    response.on_hover_text(label)
}

fn portrait(
    ui: &mut egui::Ui,
    name: &str,
    texture: Option<&egui::TextureHandle>,
    presence: Option<crate::profiles::Presence>,
    speaking: bool,
) {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(32.0), egui::Sense::hover());
    let fill = if speaking {
        theme::SPEAKING_BG
    } else {
        theme::AVATAR
    };
    ui.painter().circle_filled(rect.center(), 15.0, fill);
    if speaking {
        ui.painter()
            .circle_stroke(rect.center(), 15.0, Stroke::new(1.5, theme::SUCCESS));
    }
    if let Some(texture) = texture {
        egui::Image::new((texture.id(), rect.size()))
            .corner_radius(16)
            .paint_at(ui, rect);
    } else {
        let initial = name
            .chars()
            .next()
            .unwrap_or('?')
            .to_uppercase()
            .collect::<String>();
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            initial,
            FontId::proportional(13.0),
            theme::TEXT,
        );
    }
    if let Some(presence) = presence {
        use crate::profiles::Presence;
        let color = match presence {
            Presence::Online => theme::SUCCESS,
            Presence::Idle => Color32::from_rgb(240, 180, 60),
            Presence::Dnd => theme::DANGER,
            Presence::Offline | Presence::Unknown => theme::SECONDARY,
        };
        let center = rect.right_bottom() - Vec2::splat(4.0);
        ui.painter().circle_filled(center, 5.5, theme::SURFACE);
        if presence == Presence::Unknown {
            ui.painter()
                .circle_stroke(center, 3.5, Stroke::new(1.2, color));
        } else {
            ui.painter().circle_filled(center, 3.5, color);
        }
        response.on_hover_text(format!("{name} · {}", presence.label()));
    } else {
        response.on_hover_text(name);
    }
}

fn storage_notice(status: &str) -> bool {
    !status.is_empty()
        && !matches!(
            status,
            "Remembered in macOS Keychain; reconnects on launch. Quit preserves login."
                | "Connected using macOS Keychain. Remembered for future launches; Quit preserves login."
                | "Connected for this launch only; automatic login disabled."
        )
}

fn meter(ui: &mut egui::Ui, level: f32, enabled: bool) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(48.0, 7.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 3, theme::BORDER);
    let level = if enabled && level.is_finite() {
        level.clamp(0.0, 1.0)
    } else {
        0.0
    };
    if level > 0.0 {
        let mut fill = rect;
        fill.set_width(rect.width() * level);
        ui.painter().rect_filled(
            fill,
            3,
            if level > 0.9 {
                theme::WARNING
            } else {
                theme::SUCCESS
            },
        );
    }
    response.on_hover_text(if enabled {
        "Live microphone input level"
    } else {
        "Microphone meter inactive"
    });
}

fn icon_rgba(size: usize) -> Vec<u8> {
    draw_icon(size, false)
}

fn template_icon_rgba(size: usize) -> Vec<u8> {
    draw_icon(size, true)
}

fn draw_icon(size: usize, template: bool) -> Vec<u8> {
    let mut pixels = vec![0; size.saturating_mul(size).saturating_mul(4)];
    if size == 0 {
        return pixels;
    }
    for y in 0..size {
        for x in 0..size {
            let nx = (x as f32 + 0.5) / size as f32;
            let ny = (y as f32 + 0.5) / size as f32;
            let index = (y * size + x) * 4;
            if !template && (nx - 0.5).hypot(ny - 0.5) < 0.48 {
                pixels[index..index + 4].copy_from_slice(&[22, 26, 34, 255]);
            }
            for (bar, half) in [0.12, 0.22, 0.32, 0.22, 0.12].into_iter().enumerate() {
                let cx = 0.22 + bar as f32 * 0.14;
                if (nx - cx).abs() < 0.042 && (ny - 0.5).abs() < half {
                    pixels[index..index + 4].copy_from_slice(if template {
                        &[0, 0, 0, 255]
                    } else {
                        &[148, 173, 255, 255]
                    });
                }
            }
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(state: UiState) -> (VoiceApp, std::sync::mpsc::Receiver<Command>) {
        let state = Arc::new(Mutex::new(state));
        let (commands, rx) = std::sync::mpsc::channel();
        let gate = Arc::new(TxGate::default());
        let ptt = hotkey::PushToTalk::start(gate.clone(), Waker::default());
        let app = VoiceApp {
            images: images::Cache::default(),
            sounds: sounds::Player::new(|| {}),
            sound_error: None,
            state,
            commands,
            gate,
            ptt,
            tray: None,
            token: Zeroizing::new(String::new()),
            risk_accepted: false,
            remember: false,
            qr_login: None,
            qr_code: None,
            qr_status: String::new(),
            qr_retry_until: None,
            settings_open: false,
            selected_input: None,
            selected_output: None,
            browsing_channel: None,
            quit_requested: false,
            hide_intent: false,
            hidden: false,
            wants_show: false,
            shutdown_sent: false,
            last_tray_revision: None,
            local_error: None,
            text_open: true,
            friends_open: true,
            last_server: None,
            expanded_server: None,
            server_sections: HashMap::new(),
            last_dm: None,
            last_server_chat: None,
            pending_chat_restore: None,
            pending_dm_restore: None,
            call_label: String::new(),
            message_draft: String::new(),
            draft_channel: None,
            draft_reset_pending: false,
            last_sent_revision: 0,
        };
        (app, rx)
    }

    fn ui_frame(
        app: &mut VoiceApp,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> Vec<(String, egui::Pos2)> {
        fn collect(shape: &egui::epaint::Shape, labels: &mut Vec<(String, egui::Pos2)>) {
            match shape {
                egui::epaint::Shape::Text(text) => labels.push((
                    text.galley.job.text.clone(),
                    text.pos + text.galley.size() * 0.5,
                )),
                egui::epaint::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, labels);
                    }
                }
                _ => {}
            }
        }
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(960.0, 660.0),
                )),
                events,
                ..Default::default()
            },
            |ui| app.draw(ui),
        );
        let mut labels = Vec::new();
        for shape in &output.shapes {
            collect(&shape.shape, &mut labels);
        }
        output.textures_delta.clear();
        labels
    }
    fn click_label(
        app: &mut VoiceApp,
        ctx: &egui::Context,
        label: &str,
    ) -> Vec<(String, egui::Pos2)> {
        let labels = ui_frame(app, ctx, vec![]);
        let pos = labels
            .iter()
            .find(|(text, _)| text == label)
            .unwrap_or_else(|| panic!("Missing {label}; labels: {labels:?}"))
            .1;
        ui_frame(
            app,
            ctx,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        ui_frame(
            app,
            ctx,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        )
    }

    // Covers the explicit requirement that navigation/filter changes never issue call/chat commands.
    #[test]
    fn server_navigation_and_filters_preserve_conversation_call_and_draft() {
        let state = UiState {
            account: Some(crate::model::Account {
                id: 1,
                name: "Demo".into(),
                avatar: None,
            }),
            phase: Phase::VoiceWaiting,
            selected_guild: Some(10),
            selected_text_channel: Some(50),
            selected_channel: Some(100),
            guilds: vec![crate::model::Guild {
                id: 10,
                name: "Test guild".into(),
                icon: None,
            }],
            text_channels: vec![crate::messaging::TextChannel {
                id: 50,
                guild_id: 10,
                name: "general".into(),
            }],
            channels: vec![crate::model::Channel {
                id: 100,
                guild_id: 10,
                name: "Lounge".into(),
            }],
            ..Default::default()
        };
        let (mut app, rx) = fixture(state);
        app.draft_channel = Some(50);
        app.message_draft = "Unsent draft".into();
        app.expanded_server = Some(10);
        let ctx = egui::Context::default();
        theme::install(&ctx);
        ui_frame(&mut app, &ctx, vec![]);
        click_label(&mut app, &ctx, "Servers");
        assert_eq!(app.expanded_server, None);
        let labels = click_label(&mut app, &ctx, "Test guild");
        assert_eq!(app.expanded_server, Some(10));
        assert!(labels.iter().any(|(text, _)| text == "# general"));
        assert!(!labels.iter().any(|(text, _)| text == "Join · Lounge"));
        let labels = click_label(&mut app, &ctx, "Voice");
        assert!(labels.iter().any(|(text, _)| text == "Join · Lounge"));
        assert!(
            !labels
                .iter()
                .any(|(text, pos)| text == "# general" && pos.x < 240.0)
        );
        click_label(&mut app, &ctx, "Friends");
        click_label(&mut app, &ctx, "Servers");
        assert_eq!(app.expanded_server, None);
        click_label(&mut app, &ctx, "Test guild");
        assert!(app.server_sections.get(&10) == Some(&ServerSection::Voice));
        assert_eq!(app.message_draft, "Unsent draft");
        assert!(app.text_open);
        let state = app.snapshot();
        assert_eq!(state.selected_text_channel, Some(50));
        assert_eq!(state.selected_channel, Some(100));
        assert!(rx.try_recv().is_err());
    }

    // Prevents a repeated submit while a send is pending; failed drafts stay intact.
    #[test]
    fn composer_sends_once_per_click_and_preserves_pending_draft() {
        let state = UiState {
            account: Some(crate::model::Account {
                id: 1,
                name: "Demo".into(),
                avatar: None,
            }),
            phase: Phase::SignalingReady,
            selected_text_channel: Some(50),
            text_channels: vec![crate::messaging::TextChannel {
                id: 50,
                guild_id: 10,
                name: "general".into(),
            }],
            ..Default::default()
        };
        let (mut app, rx) = fixture(state);
        app.draft_channel = Some(50);
        app.message_draft = "Safe synthetic message".into();
        let ctx = egui::Context::default();
        theme::install(&ctx);
        ui_frame(&mut app, &ctx, vec![]);
        click_label(&mut app, &ctx, "Send");
        assert!(
            matches!(rx.try_recv(), Ok(Command::SendMessage { channel_id: 50, content }) if content == "Safe synthetic message")
        );
        assert!(rx.try_recv().is_err());
        app.state.lock().unwrap().chat_sending = true;
        click_label(&mut app, &ctx, "…");
        assert!(rx.try_recv().is_err());
        assert_eq!(app.message_draft, "Safe synthetic message");
    }

    #[test]
    fn storage_failures_remain_visible_after_removing_success_banner() {
        assert!(!storage_notice(
            "Remembered in macOS Keychain; reconnects on launch. Quit preserves login."
        ));
        assert!(storage_notice(
            "Connected for this launch, but Remember me was not enabled: failure"
        ));
        assert!(storage_notice("Unknown storage status"));
    }

    #[test]
    fn friends_refresh_and_explicit_dm_click_work_after_server_browsing() {
        let friend = crate::social::Friend {
            id: 2,
            name: "Restart friend".into(),
            avatar: None,
        };
        let state = UiState {
            account: Some(crate::model::Account {
                id: 1,
                name: "Demo".into(),
                avatar: None,
            }),
            phase: Phase::SignalingReady,
            selected_guild: Some(10),
            selected_text_channel: Some(50),
            friends: vec![friend.clone()],
            direct_channels: vec![crate::social::DirectChannel {
                id: 20,
                name: "Private conversation".into(),
                recipients: vec![friend],
                last_message_id: None,
            }],
            ..Default::default()
        };
        let (mut app, rx) = fixture(state);
        app.friends_open = false;
        app.draft_channel = Some(50);
        app.message_draft = "Preserve until a conversation is selected".into();
        let ctx = egui::Context::default();
        theme::install(&ctx);
        click_label(&mut app, &ctx, "Friends");
        assert!(rx.try_recv().is_err());
        click_label(&mut app, &ctx, "Refresh friends");
        assert!(matches!(rx.try_recv(), Ok(Command::RefreshSocial)));
        assert!(!app.message_draft.is_empty());
        assert_eq!(app.snapshot().selected_guild, Some(10));
        click_label(&mut app, &ctx, "Restart friend");
        assert!(matches!(rx.try_recv(), Ok(Command::SelectDm(20))));
        assert!(app.message_draft.is_empty());
        // Apply the coordinator's completed selection without network or login.
        {
            let mut s = app.state.lock().unwrap();
            s.selected_guild = None;
            s.selected_dm = Some(20);
            s.selected_text_channel = Some(20);
        }
        click_label(&mut app, &ctx, "Call");
        assert!(matches!(rx.try_recv(), Ok(Command::CallDm(20))));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn alone_label_requires_consistent_roster_and_never_claims_media_ready() {
        let mut state = UiState {
            phase: Phase::VoiceWaiting,
            ..Default::default()
        };
        assert_eq!(voice_phase_label(&state), "Joined · encryption pending");
        state.voice_handshake.set_roster_count(Some(1));
        assert_eq!(voice_phase_label(&state), "Joined · You’re alone");
        assert!(voice_wait_detail(&state).contains("Microphone waits"));
        state.voice_handshake.record(songbird::DaveStage::Peers, 2);
        assert_eq!(voice_phase_label(&state), "Joined · encryption pending");
        state.voice_handshake.record(songbird::DaveStage::Peers, 1);
        state.voice_handshake.set_roster_count(Some(2));
        assert_eq!(voice_phase_label(&state), "Joined · encryption pending");
        state.voice_handshake.set_roster_count(None);
        assert_eq!(voice_phase_label(&state), "Joined · encryption pending");
        state.voice_handshake.set_roster_count(Some(1));
        state
            .voice_handshake
            .record(songbird::DaveStage::MlsFailed, 30);
        assert_eq!(voice_phase_label(&state), "Joined · encryption pending");
        state.phase = Phase::VoiceReady;
        assert_eq!(voice_phase_label(&state), "Voice connected");
    }

    #[test]
    fn clearing_message_editor_revokes_prior_draft_undo_and_redo() {
        use egui::{text::CCursorRange, text_edit::TextEditState};
        let ctx = egui::Context::default();
        let mut state = TextEditState::default();
        let empty = (CCursorRange::default(), String::new());
        let private = (CCursorRange::default(), "private unsent draft".to_owned());
        let mut undoer = state.undoer();
        undoer.add_undo(&empty);
        undoer.add_undo(&private);
        assert_eq!(undoer.undo(&private), Some(&empty));
        assert!(undoer.has_redo(&empty));
        state.set_undoer(undoer);
        state.store(&ctx, egui::Id::new(MESSAGE_EDIT_ID));
        clear_message_editor(&ctx);
        let state = TextEditState::load(&ctx, egui::Id::new(MESSAGE_EDIT_ID)).unwrap();
        let mut undoer = state.undoer();
        assert!(undoer.undo(&empty).is_none());
        assert!(undoer.redo(&empty).is_none());
    }

    #[test]
    fn no_meter_timer_when_idle_muted_deafened_minimized_or_hidden() {
        assert!(!meter_needs_repaint(
            Phase::Offline,
            false,
            false,
            false,
            false
        ));
        assert!(!meter_needs_repaint(
            Phase::SignalingReady,
            false,
            false,
            false,
            false
        ));
        assert!(!meter_needs_repaint(
            Phase::VoiceReady,
            true,
            false,
            false,
            false
        ));
        assert!(!meter_needs_repaint(
            Phase::VoiceReady,
            false,
            true,
            false,
            false
        ));
        assert!(!meter_needs_repaint(
            Phase::VoiceReady,
            false,
            false,
            true,
            false
        ));
        assert!(!meter_needs_repaint(
            Phase::VoiceReady,
            false,
            false,
            false,
            true
        ));
        assert!(meter_needs_repaint(
            Phase::VoiceReady,
            false,
            false,
            false,
            false
        ));
        assert_eq!(METER_INTERVAL, Duration::from_millis(100));
    }

    #[test]
    fn personal_adapter_requires_explicit_risk_and_a_nonempty_token() {
        assert!(!connect_allowed("", true, false));
        assert!(!connect_allowed("   ", true, false));
        assert!(!connect_allowed("test-only-placeholder", false, false));
        assert!(!connect_allowed("test-only-placeholder", true, true));
        assert!(connect_allowed("test-only-placeholder", true, false));
    }

    #[test]
    fn signaling_ready_is_not_reported_as_a_voice_connection() {
        assert_eq!(phase_label(Phase::SignalingReady), "Account connected");
        assert!(!has_voice_session(Phase::SignalingReady));
        assert!(has_voice_session(Phase::VoiceReady));
    }

    #[test]
    fn icon_is_correct_rgba_size_and_transparent_outside_glyph() {
        assert_eq!(icon_rgba(64).len(), 64 * 64 * 4);
        assert_eq!(template_icon_rgba(36).len(), 36 * 36 * 4);
        assert!(icon_rgba(0).is_empty());
        assert_eq!(&icon_rgba(32)[..4], &[0, 0, 0, 0]);
        assert!(
            icon_rgba(32)
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[3] == 255)
        );
    }
}
