//! Native, event-driven desktop interface on the fastframe resident shell.
//!
//! This module never opens a socket or an audio device. Explicit UI actions
//! are sent to the backend. A password edit owns the only UI copy of the token.

use std::sync::{Arc, Mutex, mpsc::Sender};
use std::time::Duration;

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

struct VoiceApp {
    state: Arc<Mutex<UiState>>,
    commands: Sender<Command>,
    gate: Arc<TxGate>,
    ptt: hotkey::PushToTalk,
    tray: Option<Tray>,
    token: Zeroizing<String>,
    risk_accepted: bool,
    remember: bool,
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
    message_draft: String,
    draft_channel: Option<u64>,
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
            state,
            commands,
            gate,
            ptt,
            tray,
            token: Zeroizing::new(String::new()),
            risk_accepted: false,
            remember: false,
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
            text_open: false,
            message_draft: String::new(),
            draft_channel: None,
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
                Event::Menu("mute") => self.send(Command::SetMuted(!muted)),
                Event::Menu("deafen") => self.send(Command::SetDeafened(!deafened)),
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
                    } else {
                        "Mute microphone"
                    },
                );
                tray.set_label("deafen", if deafened { "Undeafen" } else { "Deafen" });
                tray.set_enabled("mute", has_account);
                tray.set_enabled("deafen", has_account);
                tray.set_enabled("leave", has_voice_session(phase));
                tray.set_tooltip(format!("fastdistord\n{}", phase_label(phase)));
            }
            self.last_tray_revision = Some(revision);
        }
    }

    fn request_quit(&mut self, ctx: &egui::Context) {
        self.quit_requested = true;
        self.hide_intent = false;
        self.release_ptt();
        self.send_shutdown();
        if !self.hidden {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn send_shutdown(&mut self) {
        if !self.shutdown_sent {
            self.shutdown_sent = true;
            self.gate.fail_closed();
            self.ptt.stop();
            self.token.zeroize();
            self.release_ptt();
            let _ = self.commands.send(Command::Quit);
        }
    }

    fn header(&mut self, ui: &mut egui::Ui, state: &UiState) {
        ui.horizontal(|ui| {
            waveform(ui, theme::ACCENT, Vec2::new(25.0, 28.0));
            ui.add_space(5.0);
            ui.label(
                RichText::new("fastdistord").font(fastframe_fonts::Weight::SemiBold.font_id(20.0)),
            );
            ui.add_space(10.0);
            ui.selectable_value(&mut self.text_open, false, "Voice");
            ui.selectable_value(&mut self.text_open, true, "Text");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Settings").clicked() {
                    self.settings_open = true;
                    self.selected_input = state.selected_input.clone();
                    self.selected_output = state.selected_output.clone();
                    self.send(Command::RefreshDevices);
                }
                ui.add_space(8.0);
                status_badge(ui, state.phase);
            });
        });
    }

    fn navigation(&mut self, ui: &mut egui::Ui, state: &UiState) {
        ui.set_width(206.0);
        section_label(ui, "SERVERS");
        ui.add_space(8.0);
        let guild_height = (ui.available_height() * 0.36).clamp(85.0, 185.0);
        egui::ScrollArea::vertical()
            .id_salt("servers")
            .max_height(guild_height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if state.guilds.is_empty() {
                    ui.label(
                        RichText::new(if state.account.is_some() {
                            "No servers available"
                        } else {
                            "Connect your account to see servers"
                        })
                        .size(13.0)
                        .color(theme::SECONDARY),
                    );
                }
                for guild in &state.guilds {
                    let selected = state.selected_guild == Some(guild.id);
                    if ui
                        .add_enabled(
                            !state.chat_sending,
                            egui::Button::new(&guild.name)
                                .truncate()
                                .selected(selected)
                                .min_size(Vec2::new(ui.available_width(), 35.0)),
                        )
                        .on_hover_text(&guild.name)
                        .clicked()
                    {
                        self.browsing_channel = None;
                        self.send(Command::SelectGuild(guild.id));
                    }
                }
            });
        ui.add_space(15.0);
        ui.separator();
        ui.add_space(12.0);
        section_label(
            ui,
            if self.text_open {
                "TEXT CHANNELS"
            } else {
                "VOICE CHANNELS"
            },
        );
        ui.add_space(8.0);
        if self.text_open {
            egui::ScrollArea::vertical()
                .id_salt("text_channels")
                .show(ui, |ui| {
                    for channel in &state.text_channels {
                        if ui
                            .add_enabled(
                                !state.chat_sending,
                                egui::Button::new(format!("# {}", channel.name))
                                    .truncate()
                                    .selected(state.selected_text_channel == Some(channel.id))
                                    .min_size(Vec2::new(ui.available_width(), 34.0)),
                            )
                            .on_hover_text(&channel.name)
                            .clicked()
                        {
                            self.send(Command::SelectTextChannel(channel.id));
                        }
                    }
                    if state.text_channels.is_empty() {
                        ui.label("Choose a server to see text channels.");
                    }
                });
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("voice_channels")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut count = 0;
                for channel in &state.channels {
                    if Some(channel.guild_id) != state.selected_guild {
                        continue;
                    }
                    count += 1;
                    let selected =
                        self.browsing_channel.or(state.selected_channel) == Some(channel.id);
                    let active = state.selected_channel == Some(channel.id)
                        && has_voice_session(state.phase);
                    let text = if active {
                        format!("•  {}", channel.name)
                    } else {
                        format!("   {}", channel.name)
                    };
                    if ui
                        .add_sized(
                            [ui.available_width(), 34.0],
                            egui::Button::new(text).truncate().selected(selected),
                        )
                        .on_hover_text(format!("{} · select channel", channel.name))
                        .clicked()
                    {
                        self.browsing_channel = Some(channel.id);
                    }
                }
                if count == 0 {
                    ui.label(
                        RichText::new(if state.selected_guild.is_some() {
                            "No voice channels found"
                        } else {
                            "Choose a server"
                        })
                        .size(13.0)
                        .color(theme::SECONDARY),
                    );
                }
            });
    }

    fn connect_panel(&mut self, ui: &mut egui::Ui, state: &UiState) {
        ui.add_space(10.0);
        ui.label(
            RichText::new("A little less between you and your call.")
                .font(fastframe_fonts::Weight::SemiBold.font_id(26.0)),
        );
        ui.add_space(9.0);
        ui.label(
            RichText::new("A compact, local desktop voice client.")
                .size(15.0)
                .color(theme::SECONDARY),
        );
        ui.add_space(16.0);
        ui.label(
            RichText::new("Connect your Discord account")
                .strong()
                .size(16.0),
        );
        ui.add_space(9.0);
        ui.label("This experimental personal-account adapter is unofficial. Discord prohibits automating normal user accounts; using it may put your account at risk.");
        ui.add_space(5.0);
        ui.hyperlink_to("Read Discord’s self-bot policy", "https://support.discord.com/hc/en-us/articles/115002192352-Automated-User-Accounts-Self-Bots");
        ui.add_space(16.0);
        ui.label(RichText::new("Session token").strong());
        let connecting = matches!(state.phase, Phase::Connecting | Phase::Reconnecting);
        ui.add_enabled(
            !connecting,
            egui::TextEdit::singleline(&mut *self.token)
                .id(egui::Id::new(TOKEN_EDIT_ID))
                .password(true)
                .hint_text("Paste a token locally")
                .desired_width(f32::INFINITY),
        );
        if let Some(mut edit_state) =
            egui::TextEdit::load_state(ui.ctx(), egui::Id::new(TOKEN_EDIT_ID))
        {
            edit_state.clear_undoer();
            edit_state.store(ui.ctx(), egui::Id::new(TOKEN_EDIT_ID));
        }
        ui.label(
            RichText::new(if self.remember {
                "Saved only in macOS Keychain when you connect. Never logged."
            } else {
                "Kept in this process only. Never saved to a file or logged."
            })
            .size(12.0)
            .color(theme::SECONDARY),
        );
        #[cfg(target_os = "macos")]
        {
            ui.add_space(8.0);
            ui.checkbox(&mut self.remember, "Remember in macOS Keychain");
        }
        ui.add_space(12.0);
        ui.checkbox(
            &mut self.risk_accepted,
            "I understand the account risk and want to enable this adapter.",
        );
        ui.add_space(12.0);
        let can_connect = connect_allowed(&self.token, self.risk_accepted, connecting);
        if primary_button(
            ui,
            if connecting {
                "Connecting…"
            } else {
                "Connect account"
            },
            can_connect,
        )
        .clicked()
        {
            let token = std::mem::take(&mut *self.token);
            self.local_error = None;
            self.send(Command::Connect {
                token,
                risk_accepted: true,
                remember: self.remember,
            });
        }
        #[cfg(target_os = "macos")]
        {
            ui.add_space(8.0);
            if ui
                .add_enabled(
                    !connecting && self.risk_accepted,
                    egui::Button::new("Connect with saved credential"),
                )
                .clicked()
            {
                self.token.zeroize();
                self.local_error = None;
                self.send(Command::ConnectSaved {
                    risk_accepted: true,
                });
            }
        }
        ui.add_space(9.0);
        ui.label(
            RichText::new(
                "Your microphone stays closed until you choose Join voice. Microphone mute is on by default.",
            )
            .size(12.0)
            .color(theme::SECONDARY),
        );
    }

    fn call_panel(&mut self, ui: &mut egui::Ui, state: &UiState) {
        let selected = self.browsing_channel.or(state.selected_channel);
        let channel = selected.and_then(|id| state.channels.iter().find(|c| c.id == id));
        ui.add_space(7.0);
        let Some(channel) = channel else {
            ui.add_space(32.0);
            waveform(ui, theme::SECONDARY, Vec2::new(54.0, 48.0));
            ui.add_space(16.0);
            let ongoing = has_voice_session(state.phase);
            ui.label(
                RichText::new(if ongoing {
                    "Your voice session"
                } else {
                    "Find your voice channel"
                })
                .size(25.0)
                .strong(),
            );
            ui.add_space(9.0);
            ui.label(RichText::new(if ongoing {
                "Browsing servers does not leave your current call. Select a channel to switch, or use Leave below."
            } else {
                "Choose a server and channel on the left, then join when you’re ready."
            }).color(theme::SECONDARY));
            ui.add_space(15.0);
            if ongoing {
                status_badge(ui, state.phase);
            } else {
                ui.label(
                    RichText::new("Microphone off · no active voice session")
                        .size(12.0)
                        .color(theme::SECONDARY),
                );
            }
            return;
        };
        let active = state.selected_channel == Some(channel.id) && has_voice_session(state.phase);
        let joining = active && matches!(state.phase, Phase::Joining | Phase::Reconnecting);
        let guild_name = state
            .guilds
            .iter()
            .find(|g| g.id == channel.guild_id)
            .map(|g| g.name.as_str())
            .unwrap_or("Server");
        ui.label(RichText::new(guild_name).size(13.0).color(theme::SECONDARY));
        ui.add_space(4.0);
        ui.label(
            RichText::new(&channel.name).font(fastframe_fonts::Weight::SemiBold.font_id(27.0)),
        );
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if active {
                status_badge(ui, state.phase);
                ui.add_space(12.0);
                if ui.button("Leave channel").clicked() {
                    self.release_ptt();
                    self.send(Command::Leave);
                }
            } else {
                let can_join = state.account.is_some()
                    && !matches!(
                        state.phase,
                        Phase::Connecting | Phase::Joining | Phase::Reconnecting
                    );
                let switching = has_voice_session(state.phase);
                if primary_button(
                    ui,
                    if switching {
                        "Switch to this channel"
                    } else {
                        "Join voice"
                    },
                    can_join,
                )
                .clicked()
                {
                    self.local_error = None;
                    self.release_ptt();
                    self.send(Command::Join {
                        guild_id: channel.guild_id,
                        channel_id: channel.id,
                    });
                }
                if switching {
                    ui.label(
                        RichText::new("Leaves your current call")
                            .size(12.0)
                            .color(theme::SECONDARY),
                    );
                }
            }
        });
        ui.add_space(22.0);
        ui.separator();
        ui.add_space(17.0);
        if active && !joining {
            section_label(
                ui,
                &format!("IN THIS CALL  ·  {}", state.participants.len()),
            );
            ui.add_space(11.0);
            egui::ScrollArea::vertical()
                .id_salt("participants")
                .show(ui, |ui| {
                    if state.participants.is_empty() {
                        ui.label(
                            RichText::new("Waiting for participant information…")
                                .color(theme::SECONDARY),
                        );
                    }
                    for person in &state.participants {
                        ui.push_id(person.id, |ui| {
                            egui::Frame::new()
                                .fill(if person.speaking {
                                    theme::SPEAKING_BG
                                } else {
                                    theme::SURFACE
                                })
                                .corner_radius(8)
                                .inner_margin(egui::Margin::symmetric(13, 10))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        avatar(ui, &person.name, person.speaking);
                                        ui.add_space(8.0);
                                        ui.label(RichText::new(&person.name).strong());
                                        ui.with_layout(
                                            Layout::right_to_left(Align::Center),
                                            |ui| {
                                                let (label, color) = if person.deafened {
                                                    ("Deafened", theme::SECONDARY)
                                                } else if person.muted {
                                                    ("Muted", theme::SECONDARY)
                                                } else if person.speaking {
                                                    ("Speaking", theme::SUCCESS)
                                                } else {
                                                    ("Listening", theme::SECONDARY)
                                                };
                                                ui.label(
                                                    RichText::new(label).size(12.0).color(color),
                                                );
                                            },
                                        );
                                    });
                                });
                            ui.add_space(6.0);
                        });
                    }
                });
        } else {
            ui.add_space(21.0);
            waveform(
                ui,
                if joining {
                    theme::WARNING
                } else {
                    theme::SECONDARY
                },
                Vec2::new(48.0, 38.0),
            );
            ui.add_space(15.0);
            ui.label(
                RichText::new(if joining {
                    "Setting up the encrypted voice connection"
                } else {
                    "Ready when you are"
                })
                .size(18.0)
                .strong(),
            );
            ui.add_space(8.0);
            ui.label(
                RichText::new(if joining {
                    "The call is ready only after voice transport and encryption are established."
                } else {
                    if state.muted || state.deafened { "Join to see who’s here. Your microphone is muted." } else if state.ptt_enabled { "Join to see who’s here. Hold the push-to-talk shortcut or button to transmit." } else { "Your microphone is enabled. Joining opens it and can transmit immediately." }
                })
                .color(theme::SECONDARY),
            );
        }
    }

    fn text_panel(&mut self, ui: &mut egui::Ui, state: &UiState) {
        if self.draft_channel != state.selected_text_channel {
            self.message_draft.zeroize();
            self.draft_channel = state.selected_text_channel;
        }
        if self.last_sent_revision != state.sent_revision {
            self.message_draft.zeroize();
            self.last_sent_revision = state.sent_revision;
        }
        let Some(channel) = state
            .text_channels
            .iter()
            .find(|c| Some(c.id) == state.selected_text_channel)
        else {
            ui.heading("Choose a text channel");
            ui.label("Read the latest 50 messages and send plain text. Switching channels clears an unsent draft.");
            return;
        };
        ui.horizontal(|ui| {
            ui.heading(format!("# {}", channel.name));
            if ui
                .add_enabled(!state.chat_busy, egui::Button::new("Refresh"))
                .clicked()
            {
                self.send(Command::RefreshMessages);
            }
        });
        ui.label(
            RichText::new(&state.chat_status)
                .size(12.0)
                .color(theme::SECONDARY),
        );
        ui.add_space(8.0);
        egui::ScrollArea::vertical()
            .id_salt(("messages", channel.id))
            .max_height((ui.available_height() - 160.0).max(60.0))
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for message in &state.messages {
                    ui.label(RichText::new(&message.author_name).strong());
                    ui.add(
                        egui::Label::new(if message.content.is_empty() {
                            "[No plain text]"
                        } else {
                            &message.content
                        })
                        .wrap()
                        .selectable(true),
                    );
                    ui.add_space(10.0);
                }
                if state.messages.is_empty() && !state.chat_busy {
                    ui.label("No messages in this snapshot.");
                }
            });
        ui.separator();
        ui.label(format!("Message to # {}", channel.name));
        ui.add_enabled(
            !state.chat_sending,
            egui::TextEdit::multiline(&mut self.message_draft)
                .char_limit(crate::messaging::MAX_MESSAGE_CHARS)
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );
        ui.horizontal(|ui| {
            let valid = crate::messaging::send_payload(&self.message_draft).is_ok();
            if primary_button(
                ui,
                if state.chat_sending {
                    "Sending…"
                } else {
                    "Send message"
                },
                valid && !state.chat_busy,
            )
            .clicked()
            {
                self.send(Command::SendMessage {
                    channel_id: channel.id,
                    content: self.message_draft.clone(),
                });
            }
            ui.label(format!("{}/2000", self.message_draft.chars().count()));
        });
        ui.label(
            RichText::new(
                "Plain text · mentions and link embeds suppressed · no automatic send retries",
            )
            .size(12.0)
            .color(theme::SECONDARY),
        );
    }

    fn controls(&mut self, ui: &mut egui::Ui, state: &UiState) {
        ui.horizontal(|ui| {
            if let Some(account) = &state.account {
                avatar(ui, &account.name, false);
                ui.vertical(|ui| {
                    ui.label(RichText::new(&account.name).strong());
                    ui.label(
                        RichText::new(if state.muted {
                            "Microphone muted"
                        } else if state.ptt_enabled {
                            "Push to talk"
                        } else {
                            "Microphone enabled"
                        })
                        .size(11.0)
                        .color(if state.muted {
                            theme::SECONDARY
                        } else {
                            theme::SUCCESS
                        }),
                    );
                });
            } else {
                ui.label(RichText::new("Not connected").color(theme::SECONDARY));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(
                        has_voice_session(state.phase),
                        egui::Button::new(RichText::new("Leave").color(theme::DANGER)),
                    )
                    .clicked()
                {
                    self.release_ptt();
                    self.send(Command::Leave);
                }
                if ui
                    .add_enabled(
                        state.account.is_some(),
                        egui::Button::new(if state.deafened { "Undeafen" } else { "Deafen" })
                            .selected(state.deafened),
                    )
                    .on_hover_text("Deafen silences playback and blocks microphone transmission")
                    .clicked()
                {
                    self.send(Command::SetDeafened(!state.deafened));
                }
                if ui
                    .add_enabled(
                        state.account.is_some() && !state.deafened,
                        egui::Button::new(if state.muted { "Unmute" } else { "Mute" })
                            .selected(state.muted),
                    )
                    .on_hover_text("Mute prevents your microphone audio from being transmitted")
                    .clicked()
                {
                    self.send(Command::SetMuted(!state.muted));
                }
                let mut ptt = state.ptt_enabled;
                if ui
                    .add_enabled(
                        state.account.is_some(),
                        egui::Checkbox::new(&mut ptt, "Push to talk"),
                    )
                    .changed()
                {
                    self.release_ptt();
                    self.send(Command::SetPtt(ptt));
                }
            });
        });
        ui.add_space(9.0);
        ui.horizontal(|ui| {
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
            ui.label(RichText::new("Input").size(11.0).color(theme::SECONDARY));
            meter(
                ui,
                state.input_level,
                state.phase == Phase::VoiceReady && !state.muted && !state.deafened,
            );
            ui.add_space(15.0);
            ui.label(RichText::new("Output").size(11.0).color(theme::SECONDARY));
            let mut volume = state.output_volume;
            if ui
                .add(egui::Slider::new(&mut volume, 0.0..=2.0).show_value(false))
                .on_hover_text(format!("Output volume: {:.0}%", volume * 100.0))
                .changed()
            {
                self.send(Command::SetOutputVolume(volume));
            }
            ui.label(
                RichText::new(format!("{:.0}%", volume * 100.0))
                    .size(11.0)
                    .color(theme::SECONDARY),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if state.ptt_enabled {
                    ui.label(
                        RichText::new(if self.ptt.available() {
                            "PTT: Ctrl + Shift + Space"
                        } else {
                            "PTT: hold the Talk button"
                        })
                        .size(11.0)
                        .color(theme::SECONDARY),
                    );
                } else {
                    ui.label(
                        RichText::new("Headphones recommended")
                            .size(11.0)
                            .color(theme::SECONDARY),
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
            .default_width(430.0)
            .default_pos(egui::pos2((ctx.content_rect().width() - 430.0).max(24.0) * 0.5, 24.0))
            .default_height((ctx.content_rect().height() - 100.0).clamp(320.0, 580.0))
            .max_height((ctx.content_rect().height() - 80.0).max(180.0))
            .vscroll(true)
            .show(ctx, |ui| {
                ui.label(RichText::new("Audio devices").size(16.0).strong());
                ui.add_space(9.0);
                ui.label("Microphone");
                device_picker(ui, "input_device", &mut self.selected_input, &state.input_devices);
                ui.add_space(9.0);
                ui.label("Headphones / speakers");
                device_picker(ui, "output_device", &mut self.selected_output, &state.output_devices);
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Refresh devices").clicked() {
                        self.send(Command::RefreshDevices);
                    }
                    if ui.button("Apply devices").clicked() {
                        self.release_ptt();
                        self.send(Command::SetDevices {
                            input: self.selected_input.clone(),
                            output: self.selected_output.clone(),
                        });
                    }
                });
                ui.add_space(10.0);
                ui.label(RichText::new("Device changes apply to your next Join. Leave and rejoin to switch an active call. System default uses your operating system’s selected device.").size(12.0).color(theme::SECONDARY));
                ui.add_space(12.0);
                ui.collapsing("Audio diagnostics", |ui| {
                    ui.label(RichText::new(&state.audio_diagnostics).monospace().size(11.0));
                });
                ui.add_space(15.0);
                ui.separator();
                ui.add_space(12.0);
                ui.label(RichText::new("Voice & privacy").size(16.0).strong());
                ui.add_space(8.0);
                ui.label("Use headphones. This client has no acoustic echo cancellation.");
                ui.add_space(7.0);
                ui.label(self.ptt.status());
                ui.add_space(7.0);
                ui.label("Losing focus releases an in-window press. No audio is recorded or saved.");
                ui.add_space(7.0);
                ui.label(if self.tray.as_ref().is_some_and(Tray::is_shown) {
                    "Closing the window keeps calls running in the tray. Choose Quit to disconnect and stop."
                } else {
                    "No usable system tray was found. Closing the window disconnects and quits."
                });
                ui.add_space(15.0);
                ui.separator();
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(state.account.is_some(), egui::Button::new(if cfg!(target_os = "macos") { "Log out & forget credential" } else { "Log out" })).clicked() {
                        self.release_ptt();
                        self.token.zeroize();
                        self.risk_accepted = false;
                        self.remember = false;
                        self.browsing_channel = None;
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
        egui::Panel::top("header")
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(theme::BACKGROUND)
                    .inner_margin(egui::Margin::symmetric(20, 14)),
            )
            .show(ui, |ui| self.header(ui, &state));
        egui::Panel::bottom("call_controls")
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(theme::PANEL)
                    .inner_margin(egui::Margin::symmetric(18, 12))
                    .stroke(Stroke::new(1.0, theme::BORDER)),
            )
            .show(ui, |ui| self.controls(ui, &state));
        egui::Panel::left("navigation")
            .exact_size(234.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(theme::PANEL)
                    .inner_margin(egui::Margin::same(14)),
            )
            .show(ui, |ui| self.navigation(ui, &state));
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(theme::BACKGROUND)
                    .inner_margin(egui::Margin::symmetric(28, 18)),
            )
            .show(ui, |ui| {
                if let Some(error) = &self.local_error {
                    ui.colored_label(theme::DANGER, error);
                    ui.add_space(9.0);
                }
                if !state.status.is_empty() {
                    ui.label(RichText::new(&state.status).size(12.0).color(
                        if state.phase == Phase::Failed {
                            theme::DANGER
                        } else {
                            theme::SECONDARY
                        },
                    ));
                    ui.add_space(7.0);
                }
                if state.account.is_some() && state.phase == Phase::Failed {
                    ui.horizontal(|ui| {
                        if primary_button(ui, "Reconnect account", true).clicked() {
                            self.release_ptt();
                            self.send(Command::Reconnect);
                        }
                        ui.label(
                            RichText::new("Then choose Join to return to voice.")
                                .size(12.0)
                                .color(theme::SECONDARY),
                        );
                    });
                    ui.add_space(12.0);
                }
                if state.account.is_none() {
                    self.message_draft.zeroize();
                    self.draft_channel = None;
                    self.last_sent_revision = state.sent_revision;
                    egui::ScrollArea::vertical()
                        .id_salt("connect_panel")
                        .show(ui, |ui| self.connect_panel(ui, &state));
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
            state.muted,
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

fn connect_allowed(token: &str, risk_accepted: bool, connecting: bool) -> bool {
    risk_accepted && !connecting && !token.trim().is_empty()
}

fn has_voice_session(phase: Phase) -> bool {
    matches!(
        phase,
        Phase::Joining | Phase::VoiceReady | Phase::Reconnecting
    )
}

fn phase_label(phase: Phase) -> &'static str {
    match phase {
        Phase::Offline => "Offline",
        Phase::Connecting => "Connecting account",
        Phase::SignalingReady => "Account connected",
        Phase::Joining => "Joining voice",
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
            .font(fastframe_fonts::Weight::SemiBold.font_id(10.0))
            .color(theme::SECONDARY),
    );
}

fn status_badge(ui: &mut egui::Ui, phase: Phase) {
    let color = match phase {
        Phase::VoiceReady | Phase::SignalingReady => theme::SUCCESS,
        Phase::Connecting | Phase::Joining | Phase::Reconnecting => theme::WARNING,
        Phase::Failed => theme::DANGER,
        Phase::Offline => theme::SECONDARY,
    };
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(8.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 3.0, color);
        ui.label(RichText::new(phase_label(phase)).size(12.0).color(color));
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

fn avatar(ui: &mut egui::Ui, name: &str, speaking: bool) {
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
    response.on_hover_text(name);
}

fn waveform(ui: &mut egui::Ui, color: Color32, size: Vec2) {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    for (i, fraction) in [0.30, 0.60, 1.0, 0.60, 0.30].into_iter().enumerate() {
        let x = rect.left() + rect.width() * (i as f32 + 0.5) / 5.0;
        let half = rect.height() * fraction * 0.44;
        ui.painter().line_segment(
            [
                egui::pos2(x, rect.center().y - half),
                egui::pos2(x, rect.center().y + half),
            ],
            Stroke::new((size.x / 9.0).max(2.0), color),
        );
    }
}

fn meter(ui: &mut egui::Ui, level: f32, enabled: bool) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(95.0, 7.0), egui::Sense::hover());
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
