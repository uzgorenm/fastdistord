use crate::{
    account::{self, Gateway, GatewayCommand, GatewayEvent, PersonalAccount},
    audio::{self, AudioConfig, TxGate},
    model::*,
    recovery::{AUTHORITY_TIMEOUT, CONNECT_TIMEOUT, Retry, RetryBudget},
    transport::{
        AudioOpenFailure, TransientVoiceFailure, Transport, TransportEvent, VoiceConnection,
    },
};
use anyhow::Result;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};
use tokio::sync::mpsc as async_mpsc;
type Shared = Arc<Mutex<UiState>>;
type Repaint = Option<Arc<dyn Fn() + Send + Sync>>;
fn update(state: &Shared, repaint: &Repaint, f: impl FnOnce(&mut UiState)) {
    if let Ok(mut s) = state.lock() {
        f(&mut s);
        s.revision = s.revision.wrapping_add(1);
    }
    if let Some(wake) = repaint {
        wake();
    }
}
fn snapshot(state: &Shared) -> UiState {
    state.lock().map(|s| s.clone()).unwrap_or_default()
}
fn reset_chat(
    state: &Shared,
    repaint: &Repaint,
    generation: &mut u64,
    task: &mut Option<tokio::task::JoinHandle<()>>,
) {
    *generation = generation.wrapping_add(1);
    if let Some(task) = task.take() {
        task.abort();
    }
    update(state, repaint, |s| {
        s.text_channels.clear();
        s.selected_text_channel = None;
        s.messages.clear();
        s.chat_busy = false;
        s.chat_sending = false;
        s.chat_status = "Choose a text channel to read its latest 50 messages.".into();
    });
}
enum ResultEvent {
    Account(
        u64,
        Result<(PersonalAccount, Account, Vec<Guild>)>,
        Option<zeroize::Zeroizing<String>>,
    ),
    Channels(u64, u64, Result<Vec<Channel>>),
    TextChannels(u64, u64, u64, Result<Vec<crate::messaging::TextChannel>>),
    Messages(u64, u64, u64, Result<Vec<crate::messaging::ChatMessage>>),
    Sent(u64, u64, u64, Result<crate::messaging::ChatMessage>),
    Transport(u64, Result<Transport>),
}
#[derive(Default)]
struct PendingVoice {
    guild: u64,
    channel: u64,
    session: Option<String>,
    server: Option<(String, String)>,
    allow_initial_connect: bool,
}
impl PendingVoice {
    fn take_tokens(&mut self) -> Option<(String, String, String)> {
        if self.session.is_none() || self.server.is_none() {
            return None;
        }
        let session = self.session.take()?;
        let (endpoint, token) = self.server.take()?;
        Some((session, endpoint, token))
    }
}
/// Invalidates both coordinator results and the pre-send session lease before cleanup.
fn cancel_voice(
    gate: &TxGate,
    generation: &mut u64,
    transport: &mut Option<Transport>,
    pending: &mut Option<PendingVoice>,
    task: &mut Option<tokio::task::JoinHandle<()>>,
) {
    gate.set_suppressed(true);
    gate.set_ptt_pressed(None);
    gate.begin_session();
    *generation = generation.wrapping_add(1);
    if let Some(task) = task.take() {
        task.abort();
    }
    pending.take();
    transport.take();
}
#[derive(Default)]
struct CallRecovery {
    info: Option<VoiceConnection>,
    config: Option<AudioConfig>,
    retry: Option<Retry>,
    budget: RetryBudget,
    device_failure: bool,
}
impl CallRecovery {
    fn revoke(&mut self) {
        *self = Self::default();
    }
    fn authorized(&self, pending: Option<&PendingVoice>) -> bool {
        matches!((&self.info,&self.config,pending),(Some(info),Some(config),Some(p)) if config.input_device.is_some()&&config.output_device.is_some()&&info.guild_id==p.guild && info.channel_id==p.channel && p.session.as_deref()==Some(info.session_id.as_str()))
    }
    fn schedule(&mut self, generation: u64, pending: Option<&PendingVoice>) -> bool {
        if !self.authorized(pending) {
            self.retry = None;
            return false;
        }
        self.retry = self.budget.schedule(generation, Instant::now());
        self.retry.is_some()
    }
}
fn launch_transport(
    info: VoiceConnection,
    config: AudioConfig,
    gate: Arc<TxGate>,
    id: u64,
    tx: async_mpsc::Sender<ResultEvent>,
    events: async_mpsc::UnboundedSender<(u64, TransportEvent)>,
) -> tokio::task::JoinHandle<()> {
    let expected_session = gate.session();
    tokio::spawn(async move {
        let (event_tx, mut event_rx) = async_mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                if events.send((id, event)).is_err() {
                    break;
                }
            }
        });
        let result = tokio::time::timeout(
            CONNECT_TIMEOUT,
            Transport::connect(info, gate, config, event_tx, expected_session),
        )
        .await
        .unwrap_or_else(|_| Err(TransientVoiceFailure.into()));
        let _ = tx.send(ResultEvent::Transport(id, result)).await;
    })
}
pub fn spawn(
    state: Shared,
    commands: mpsc::Receiver<Command>,
    gate: Arc<TxGate>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        else {
            return;
        };
        let (tx, rx) = async_mpsc::channel(64);
        let bridge = std::thread::spawn(move || {
            while let Ok(command) = commands.recv() {
                let quit = matches!(command, Command::Quit);
                if tx.blocking_send(command).is_err() || quit {
                    break;
                }
            }
        });
        runtime.block_on(run(state, rx, gate));
        let _ = bridge.join();
    })
}
async fn run(state: Shared, mut commands: async_mpsc::Receiver<Command>, gate: Arc<TxGate>) {
    let mut recovery = CallRecovery::default();
    let mut signal_ready = false;
    let mut signal_resume = None;
    let mut signal_retry: Option<Retry> = None;
    let mut signal_budget = RetryBudget::default();
    let mut signal_deadline: Option<Instant> = None;
    let mut recovery_tick = tokio::time::interval(Duration::from_millis(100));
    recovery_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut roster = HashMap::<(u64, u64), (u64, Participant)>::new();
    let mut ui_visible = true;
    let mut repaint: Repaint = None;
    let mut client: Option<Arc<PersonalAccount>> = None;
    let mut gateway: Option<Gateway> = None;
    let mut transport: Option<Transport> = None;
    let (mut generation, mut join_generation) = (0_u64, 0_u64);
    let mut server_suppressed = false;
    let mut pending: Option<PendingVoice> = None;
    let mut connect_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut voice_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut chat_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut chat_generation = 0_u64;
    let (results_tx, mut results) = async_mpsc::channel(8);
    let (voice_tx, mut voice_events) = async_mpsc::unbounded_channel();
    let mut meter_tick = tokio::time::interval(std::time::Duration::from_millis(100));
    meter_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    enum Incoming {
        Command(Option<Command>),
        Result(Option<ResultEvent>),
        Gateway(Option<GatewayEvent>),
        Voice(Option<(u64, TransportEvent)>),
        Meter,
        Recovery,
    }
    loop {
        let incoming = tokio::select! {
            biased;
            command=commands.recv()=>Incoming::Command(command),
            event=async {if let Some(g)=&mut gateway{g.events.recv().await}else{std::future::pending().await}}=>Incoming::Gateway(event),
            event=results.recv()=>Incoming::Result(event),
            event=voice_events.recv()=>Incoming::Voice(event),
            _=recovery_tick.tick(),if signal_retry.is_some()||recovery.retry.is_some()||signal_deadline.is_some()=>Incoming::Recovery,
            _=meter_tick.tick(),if transport.is_some()&&ui_visible&&!gate.is_muted()&&!gate.is_deafened()=>Incoming::Meter,
        };
        match incoming {
            Incoming::Command(command) => {
                let Some(command) = command else {
                    break;
                };
                if matches!(
                    &command,
                    Command::Connect { .. }
                        | Command::Logout
                        | Command::Join { .. }
                        | Command::Leave
                        | Command::Reconnect
                        | Command::Quit
                ) {
                    recovery.revoke();
                    signal_retry = None;
                    signal_resume = None;
                    signal_deadline = None;
                    signal_budget = RetryBudget::default();
                }
                #[cfg(target_os = "macos")]
                if matches!(&command, Command::ConnectSaved { .. }) {
                    recovery.revoke();
                    signal_retry = None;
                    signal_resume = None;
                    signal_deadline = None;
                    signal_budget = RetryBudget::default();
                }
                match command {
                    Command::SetUiRepaint(callback) => {
                        repaint = Some(callback);
                    }
                    Command::SetUiVisible(visible) => ui_visible = visible,
                    Command::Connect {
                        token,
                        risk_accepted,
                        remember,
                    } => {
                        reset_chat(&state, &repaint, &mut chat_generation, &mut chat_task);
                        generation = generation.wrapping_add(1);
                        join_generation = join_generation.wrapping_add(1);
                        gate.begin_session();
                        gate.fail_closed();
                        transport.take();
                        gateway.take();
                        client.take();
                        roster.clear();
                        pending = None;
                        if let Some(task) = connect_task.take() {
                            task.abort();
                        }
                        if let Some(task) = voice_task.take() {
                            task.abort();
                        }
                        update(&state, &repaint, |s| {
                            s.phase = Phase::Connecting;
                            s.status = "Checking account access…".into();
                            s.account = None;
                            s.guilds.clear();
                            s.channels.clear();
                            s.participants.clear();
                        });
                        let tx = results_tx.clone();
                        let id = generation;
                        connect_task = Some(tokio::spawn(async move {
                            let saved = zeroize::Zeroizing::new(if remember {
                                token.clone()
                            } else {
                                String::new()
                            });
                            let result = PersonalAccount::connect(token, risk_accepted).await;
                            let saved = if remember { Some(saved) } else { None };
                            let _ = tx.send(ResultEvent::Account(id, result, saved)).await;
                        }));
                    }
                    #[cfg(target_os = "macos")]
                    Command::ConnectSaved { risk_accepted } => {
                        if !risk_accepted {
                            continue;
                        }
                        match crate::credential::load() {
                            Ok(token) => {
                                reset_chat(&state, &repaint, &mut chat_generation, &mut chat_task);
                                generation = generation.wrapping_add(1);
                                join_generation = join_generation.wrapping_add(1);
                                gate.begin_session();
                                gate.fail_closed();
                                transport.take();
                                gateway.take();
                                client.take();
                                roster.clear();
                                pending = None;
                                if let Some(task) = connect_task.take() {
                                    task.abort();
                                }
                                if let Some(task) = voice_task.take() {
                                    task.abort();
                                }
                                let id = generation;
                                let tx = results_tx.clone();
                                connect_task = Some(tokio::spawn(async move {
                                    let result = PersonalAccount::connect(token, true).await;
                                    let _ = tx.send(ResultEvent::Account(id, result, None)).await;
                                }));
                                update(&state, &repaint, |s| {
                                    s.phase = Phase::Connecting;
                                    s.status = "Connecting with saved credential…".into();
                                });
                            }
                            Err(error) => {
                                update(&state, &repaint, |s| s.status = error.to_string())
                            }
                        }
                    }
                    Command::Reconnect => {
                        if let Some(c) = &client {
                            gate.set_suppressed(true);
                            gate.begin_session();
                            transport.take();
                            join_generation = join_generation.wrapping_add(1);
                            if let Some(task) = voice_task.take() {
                                task.abort();
                            }
                            pending = None;
                            signal_ready = false;
                            gateway = Some(c.gateway());
                            update(&state, &repaint, |s| {
                                s.phase = Phase::Reconnecting;
                                s.status =
                                    "Reconnecting signaling; voice stays disconnected.".into();
                                s.participants.clear();
                                s.selected_channel = None;
                            });
                        }
                    }
                    Command::Logout => {
                        reset_chat(&state, &repaint, &mut chat_generation, &mut chat_task);
                        let forgotten = crate::credential::forget();
                        generation = generation.wrapping_add(1);
                        join_generation = join_generation.wrapping_add(1);
                        gate.begin_session();
                        gate.fail_closed();
                        transport.take();
                        pending = None;
                        if let Some(task) = connect_task.take() {
                            task.abort();
                        }
                        if let Some(task) = voice_task.take() {
                            task.abort();
                        }
                        if let Some(g) = gateway.take() {
                            let _ = g.commands.try_send(GatewayCommand::Close);
                        }
                        client.take();
                        roster.clear();
                        update(&state, &repaint, |s| {
                            let revision = s.revision;
                            *s = UiState::default();
                            s.revision = revision;
                            s.status = if forgotten.is_ok() {
                                "Signed out. Credential removed from memory and Keychain.".into()
                            } else {
                                "Signed out, but Keychain deletion failed. Remove fastdistord.personal-account in Keychain Access.".into()
                            };
                        });
                    }
                    Command::SelectGuild(guild) => {
                        if snapshot(&state).chat_sending {
                            continue;
                        }
                        if let Some(c) = client.clone() {
                            if !snapshot(&state).guilds.iter().any(|g| g.id == guild) {
                                continue;
                            }
                            reset_chat(&state, &repaint, &mut chat_generation, &mut chat_task);
                            let tx = results_tx.clone();
                            let id = generation;
                            let request = chat_generation;
                            update(&state, &repaint, |s| {
                                s.selected_guild = Some(guild);
                                s.channels.clear();
                                s.status = "Loading voice channels…".into();
                            });
                            tokio::spawn(async move {
                                let _ = tx
                                    .send(ResultEvent::Channels(id, guild, c.channels(guild).await))
                                    .await;
                            });
                            let c = client.as_ref().unwrap().clone();
                            let tx = results_tx.clone();
                            chat_task = Some(tokio::spawn(async move {
                                let result = c.text_channels(guild).await;
                                let _ = tx
                                    .send(ResultEvent::TextChannels(id, request, guild, result))
                                    .await;
                            }));
                        }
                    }
                    Command::SelectTextChannel(channel) => {
                        let s = snapshot(&state);
                        if s.chat_sending
                            || !s
                                .text_channels
                                .iter()
                                .any(|c| c.id == channel && Some(c.guild_id) == s.selected_guild)
                        {
                            continue;
                        }
                        if let Some(c) = client.clone() {
                            if let Some(task) = chat_task.take() {
                                task.abort();
                            }
                            chat_generation = chat_generation.wrapping_add(1);
                            let (id, request, tx) =
                                (generation, chat_generation, results_tx.clone());
                            update(&state, &repaint, |s| {
                                s.selected_text_channel = Some(channel);
                                s.messages.clear();
                                s.chat_busy = true;
                                s.chat_status = "Loading message history…".into();
                            });
                            chat_task = Some(tokio::spawn(async move {
                                let result = c.messages(channel).await;
                                let _ = tx
                                    .send(ResultEvent::Messages(id, request, channel, result))
                                    .await;
                            }));
                        }
                    }
                    Command::RefreshMessages => {
                        let s = snapshot(&state);
                        if s.chat_busy {
                            continue;
                        }
                        if let (Some(c), Some(channel)) = (client.clone(), s.selected_text_channel)
                        {
                            chat_generation = chat_generation.wrapping_add(1);
                            let (id, request, tx) =
                                (generation, chat_generation, results_tx.clone());
                            update(&state, &repaint, |s| {
                                s.chat_busy = true;
                                s.chat_status = "Refreshing history…".into();
                            });
                            chat_task = Some(tokio::spawn(async move {
                                let result = c.messages(channel).await;
                                let _ = tx
                                    .send(ResultEvent::Messages(id, request, channel, result))
                                    .await;
                            }));
                        }
                    }
                    Command::SendMessage {
                        channel_id,
                        content,
                    } => {
                        let s = snapshot(&state);
                        if s.chat_busy
                            || s.selected_text_channel != Some(channel_id)
                            || !s
                                .text_channels
                                .iter()
                                .any(|c| c.id == channel_id && Some(c.guild_id) == s.selected_guild)
                        {
                            continue;
                        }
                        if let Err(error) = crate::messaging::send_payload(&content) {
                            update(&state, &repaint, |s| s.chat_status = error.to_string());
                            continue;
                        }
                        if let Some(c) = client.clone() {
                            chat_generation = chat_generation.wrapping_add(1);
                            let (id, request, tx) =
                                (generation, chat_generation, results_tx.clone());
                            update(&state, &repaint, |s| {
                                s.chat_busy = true;
                                s.chat_sending = true;
                                s.chat_status = "Sending once…".into();
                            });
                            chat_task = Some(tokio::spawn(async move {
                                let result = c.send_message(channel_id, &content).await;
                                let _ = tx
                                    .send(ResultEvent::Sent(id, request, channel_id, result))
                                    .await;
                            }));
                        }
                    }
                    Command::Join {
                        guild_id,
                        channel_id,
                    } => {
                        let s = snapshot(&state);
                        if gateway.is_none()
                            || s.account.is_none()
                            || !s
                                .channels
                                .iter()
                                .any(|c| c.guild_id == guild_id && c.id == channel_id)
                        {
                            continue;
                        }
                        join_generation = join_generation.wrapping_add(1);
                        gate.begin_session();
                        gate.set_suppressed(true);
                        gate.set_ptt_pressed(None);
                        transport.take();
                        if let Some(task) = voice_task.take() {
                            task.abort();
                        }
                        server_suppressed = false;
                        pending = Some(PendingVoice {
                            guild: guild_id,
                            channel: channel_id,
                            allow_initial_connect: true,
                            ..Default::default()
                        });
                        update(&state, &repaint, |s| {
                            s.phase = Phase::Joining;
                            s.status = "Joining encrypted voice…".into();
                            s.selected_channel = Some(channel_id);
                            s.participants = roster
                                .iter()
                                .filter(|((g, _), (c, _))| *g == guild_id && *c == channel_id)
                                .map(|(_, (_, p))| p.clone())
                                .take(250)
                                .collect();
                        });
                        if let Some(g) = &gateway {
                            let _ = g
                                .commands
                                .send(GatewayCommand::Voice {
                                    guild_id,
                                    channel_id: Some(channel_id),
                                    muted: s.muted,
                                    deafened: s.deafened,
                                })
                                .await;
                        }
                    }
                    Command::Leave => {
                        join_generation = join_generation.wrapping_add(1);
                        gate.begin_session();
                        gate.set_suppressed(true);
                        gate.set_ptt_pressed(None);
                        transport.take();
                        if let Some(task) = voice_task.take() {
                            task.abort();
                        }
                        if let (Some(g), Some(p)) = (&gateway, pending.take()) {
                            let _ = g
                                .commands
                                .send(GatewayCommand::Voice {
                                    guild_id: p.guild,
                                    channel_id: None,
                                    muted: true,
                                    deafened: false,
                                })
                                .await;
                        }
                        update(&state, &repaint, |s| {
                            s.phase = if s.account.is_some() {
                                Phase::SignalingReady
                            } else {
                                Phase::Offline
                            };
                            s.status = "Left voice. Microphone released.".into();
                            s.selected_channel = None;
                            s.participants.clear();
                            s.input_level = 0.0;
                        });
                    }
                    Command::SetMuted(value) => {
                        gate.set_muted(value);
                        if let Some(t) = &mut transport {
                            t.mute(value);
                        }
                        update(&state, &repaint, |s| s.muted = value);
                        if may_send_voice_flags(
                            signal_ready,
                            transport.is_some(),
                            recovery.authorized(pending.as_ref()),
                        ) {
                            send_voice_flags(&gateway, &pending, &state).await;
                        }
                    }
                    Command::SetDeafened(value) => {
                        gate.set_deafened(value);
                        if let Some(t) = &mut transport {
                            t.deafen(value);
                        }
                        update(&state, &repaint, |s| s.deafened = value);
                        if may_send_voice_flags(
                            signal_ready,
                            transport.is_some(),
                            recovery.authorized(pending.as_ref()),
                        ) {
                            send_voice_flags(&gateway, &pending, &state).await;
                        }
                    }
                    Command::SetPtt(value) => {
                        gate.set_ptt_enabled(value);
                        gate.set_ptt_pressed(None);
                        update(&state, &repaint, |s| s.ptt_enabled = value);
                    }
                    Command::SetOutputVolume(value) => {
                        let value = value.clamp(0.0, 2.0);
                        if let Some(t) = &mut transport {
                            t.set_output_volume(value);
                        }
                        update(&state, &repaint, |s| s.output_volume = value);
                    }
                    Command::RefreshDevices => match audio::enumerate_devices() {
                        Ok(devices) => update(&state, &repaint, |s| {
                            s.input_devices = devices
                                .iter()
                                .filter(|d| d.is_input)
                                .map(|d| DeviceChoice {
                                    id: d.id.clone(),
                                    name: d.name.clone(),
                                    is_default: d.is_default_input,
                                })
                                .collect();
                            s.output_devices = devices
                                .iter()
                                .filter(|d| d.is_output)
                                .map(|d| DeviceChoice {
                                    id: d.id.clone(),
                                    name: d.name.clone(),
                                    is_default: d.is_default_output,
                                })
                                .collect();
                        }),
                        Err(_) => update(&state, &repaint, |s| {
                            s.status="Audio devices unavailable. Check operating-system permissions and devices.".into()
                        }),
                    },
                    Command::SetDevices { input, output } => {
                        update(&state, &repaint, |s| {
                            s.selected_input = input;
                            s.selected_output = output;
                            s.status="Device selection saved for the next Join. Leave and rejoin to switch safely.".into();
                        });
                    }
                    Command::Quit => break,
                }
            }
            Incoming::Result(event) => match event {
                Some(ResultEvent::TextChannels(id, request, guild, result))
                    if id == generation
                        && request == chat_generation
                        && snapshot(&state).selected_guild == Some(guild) =>
                {
                    chat_task.take();
                    update(&state, &repaint, |s| match result {
                        Ok(channels) => s.text_channels = channels,
                        Err(error) => s.chat_status = error.to_string(),
                    });
                }
                Some(ResultEvent::Messages(id, request, channel, result))
                    if id == generation
                        && request == chat_generation
                        && snapshot(&state).selected_text_channel == Some(channel) =>
                {
                    chat_task.take();
                    update(&state, &repaint, |s| {
                        s.chat_busy = false;
                        match result {
                            Ok(messages) => {
                                s.messages = messages;
                                s.chat_status =
                                    "Latest 50 messages · Refresh to check for updates.".into();
                            }
                            Err(error) => s.chat_status = error.to_string(),
                        }
                    });
                }
                Some(ResultEvent::Sent(id, request, channel, result))
                    if id == generation
                        && request == chat_generation
                        && snapshot(&state).selected_text_channel == Some(channel) =>
                {
                    chat_task.take();
                    update(&state, &repaint, |s| {
                        s.chat_busy = false;
                        s.chat_sending = false;
                        match result {
                            Ok(message) => {
                                s.messages.retain(|m| m.id != message.id);
                                s.messages.push(message);
                                s.messages.sort_by_key(|m| m.id);
                                if s.messages.len() > crate::messaging::MAX_HISTORY {
                                    s.messages.remove(0);
                                }
                                s.sent_revision = s.sent_revision.wrapping_add(1);
                                s.chat_status = "Message sent.".into();
                            }
                            Err(error) => {
                                s.chat_status = format!(
                                    "{error} Check history before retrying; no automatic retry was made."
                                )
                            }
                        }
                    });
                }
                Some(ResultEvent::Account(id, result, saved)) if id == generation => match result {
                    Ok((c, a, guilds)) => {
                        if let Some(secret) = saved
                            && let Err(error) = crate::credential::store(&secret)
                        {
                            update(&state, &repaint, |s| {
                                s.phase = Phase::Failed;
                                s.status = error.to_string();
                            });
                            continue;
                        }
                        signal_ready = false;
                        gateway = Some(c.gateway());
                        client = Some(Arc::new(c));
                        update(&state, &repaint, |s| {
                            s.account = Some(a);
                            s.guilds = guilds;
                            s.status = "Connecting Discord signaling…".into();
                        });
                    }
                    Err(error) => update(&state, &repaint, |s| {
                        s.phase = Phase::Failed;
                        s.status = error.to_string();
                    }),
                },
                Some(ResultEvent::Channels(id, guild, result))
                    if id == generation && snapshot(&state).selected_guild == Some(guild) =>
                {
                    match result {
                        Ok(channels) => {
                            let selected = snapshot(&state).selected_guild;
                            if channels
                                .first()
                                .is_none_or(|c| Some(c.guild_id) == selected)
                            {
                                update(&state, &repaint, |s| {
                                    s.channels = channels;
                                    s.status="Choose a voice channel, then Join. Joining opens your audio devices.".into();
                                });
                            }
                        }
                        Err(_) => update(&state, &repaint, |s| {
                            s.status =
                                "Could not load channels. Check access or retry later.".into()
                        }),
                    }
                }
                Some(ResultEvent::Transport(id, result)) if id == join_generation => match result {
                    Ok(t) => {
                        recovery.config = Some(t.device_config());
                        recovery.retry = None;
                        recovery.budget.healthy(Instant::now());
                        if let (Some(p), Some(info)) = (&mut pending, &recovery.info) {
                            p.session = Some(info.session_id.clone());
                        }
                        let intended = snapshot(&state);
                        gate.set_muted(intended.muted);
                        gate.set_deafened(intended.deafened);
                        gate.set_ptt_enabled(intended.ptt_enabled);
                        gate.set_ptt_pressed(None);
                        gate.set_suppressed(server_suppressed);
                        transport = Some(t);
                        update(&state, &repaint, |s| {
                            s.phase = Phase::VoiceReady;
                            s.status = "Encrypted voice connected · headphones recommended".into();
                        });
                    }
                    Err(error) => {
                        retire_voice_attempt(id, &mut join_generation, &gate);
                        if let Some(task) = voice_task.take() {
                            task.abort();
                        }
                        let retrying = retryable_connect_error(&error, recovery.device_failure)
                            && signal_ready
                            && recovery.schedule(join_generation, pending.as_ref());
                        if !retrying {
                            recovery.revoke();
                        }
                        update(&state, &repaint, |s| {
                            s.phase = if retrying {
                                Phase::Reconnecting
                            } else {
                                Phase::Failed
                            };
                            s.status = if retrying {
                                format!(
                                    "Retrying the same voice session and devices (attempt {}/5).",
                                    recovery.budget.attempts()
                                )
                            } else {
                                error.to_string()
                            };
                        });
                    }
                },
                _ => {}
            },
            Incoming::Gateway(event) => match event {
                Some(GatewayEvent::Ready { resumed }) => {
                    if !resumed && signal_resume.is_some() {
                        recovery.revoke();
                        pending = None;
                    }
                    signal_ready = true;
                    signal_retry = None;
                    signal_resume = None;
                    signal_deadline = None;
                    signal_budget.healthy(Instant::now());
                    let restoring = resumed && recovery.schedule(join_generation, pending.as_ref());
                    update(&state, &repaint, |s| {
                        s.phase = if restoring {
                            Phase::Reconnecting
                        } else {
                            Phase::SignalingReady
                        };
                        s.status = if restoring {
                            "Session resumed; restoring the same authorized voice connection."
                        } else {
                            "Account connected. Choose a server."
                        }
                        .into();
                    });
                }
                Some(GatewayEvent::Closed {
                    message,
                    retryable,
                    resume,
                }) => {
                    signal_ready = false;
                    signal_deadline = None;
                    gate.set_suppressed(true);
                    gate.set_ptt_pressed(None);
                    gate.begin_session();
                    join_generation = join_generation.wrapping_add(1);
                    if let Some(task) = voice_task.take() {
                        task.abort();
                    }
                    transport.take();
                    gateway = None;
                    recovery.retry = None;
                    signal_resume = if retryable { resume } else { None };
                    signal_retry = if signal_resume.is_some() && client.is_some() {
                        signal_budget.schedule(generation, Instant::now())
                    } else {
                        None
                    };
                    if signal_retry.is_none() {
                        recovery.revoke();
                        pending = None;
                    }
                    update(&state, &repaint, |s| {
                        s.phase = if signal_retry.is_some() {
                            Phase::Reconnecting
                        } else {
                            Phase::Failed
                        };
                        s.status = if signal_retry.is_some() {
                            format!("{message} Retry {}/5.", signal_budget.attempts())
                        } else {
                            format!("{message} Use Reconnect and explicitly Join again.")
                        };
                        s.participants.clear();
                    });
                }
                Some(GatewayEvent::Dispatch { kind, data }) => {
                    if kind == "VOICE_SERVER_UPDATE"
                        && let Some(info) = &mut recovery.info
                        && account::snowflake(&data["guild_id"]) == Some(info.guild_id)
                    {
                        if let (Some(endpoint), Some(token)) =
                            (data["endpoint"].as_str(), data["token"].as_str())
                        {
                            info.endpoint = endpoint.into();
                            info.token = token.into();
                        } else {
                            recovery.revoke();
                        }
                    }
                    if kind == "VOICE_STATE_UPDATE" {
                        update_roster(&mut roster, &data);
                    }
                    if kind == "VOICE_SERVER_UPDATE"
                        && let Some(p) = &mut pending
                        && account::snowflake(&data["guild_id"]) == Some(p.guild)
                    {
                        if let (Some(endpoint), Some(token)) =
                            (data["endpoint"].as_str(), data["token"].as_str())
                        {
                            p.server = Some((endpoint.to_owned(), token.to_owned()));
                        } else {
                            recovery.revoke();
                            cancel_voice(
                                &gate,
                                &mut join_generation,
                                &mut transport,
                                &mut pending,
                                &mut voice_task,
                            );
                            update(&state, &repaint, |s| {
                                s.phase = Phase::Failed;
                                s.status =
                                    "Voice endpoint was removed. Audio stopped; rejoin explicitly."
                                        .into();
                            });
                        }
                    }
                    if kind == "VOICE_STATE_UPDATE"
                        && let Some(p) = &mut pending
                        && account::snowflake(&data["guild_id"]) == Some(p.guild)
                    {
                        let user = account::snowflake(&data["user_id"]);
                        let channel = account::snowflake(&data["channel_id"]);
                        let own = snapshot(&state).account.as_ref().map(|a| a.id) == user;
                        if own {
                            if channel != Some(p.channel) {
                                recovery.revoke();
                                gate.set_suppressed(true);
                                transport.take();
                                join_generation = join_generation.wrapping_add(1);
                                gate.begin_session();
                                if let Some(task) = voice_task.take() {
                                    task.abort();
                                }
                                update(&state, &repaint, |s| {
                                    s.phase = Phase::SignalingReady;
                                    s.selected_channel = channel;
                                    s.status="You were moved or disconnected. Audio stopped; select and join a channel to continue.".into();
                                    s.participants.clear();
                                });
                                pending = None;
                            } else {
                                let incoming_session = data["session_id"].as_str();
                                if recovery.info.as_ref().is_some_and(|info| {
                                    incoming_session != Some(info.session_id.as_str())
                                }) {
                                    recovery.revoke();
                                    cancel_voice(
                                        &gate,
                                        &mut join_generation,
                                        &mut transport,
                                        &mut pending,
                                        &mut voice_task,
                                    );
                                    update(&state, &repaint, |s| {
                                        s.phase = Phase::Failed;
                                        s.status="Voice ownership changed. Audio stopped; explicitly Join again.".into();
                                    });
                                    continue;
                                }
                                p.session = incoming_session.map(str::to_owned);
                                let admin_muted = data["mute"].as_bool().unwrap_or(false)
                                    || data["deaf"].as_bool().unwrap_or(false)
                                    || data["suppress"].as_bool().unwrap_or(false);
                                server_suppressed = admin_muted;
                                gate.set_suppressed(admin_muted || transport.is_none());
                                if admin_muted {
                                    update(&state, &repaint, |s| {
                                        s.status = "Server has muted or suppressed you.".into()
                                    });
                                }
                            }
                        }
                        if let Some(id) = user {
                            update(&state, &repaint, |s| {
                                s.participants.retain(|v| v.id != id);
                                if channel.is_some()
                                    && channel == s.selected_channel
                                    && s.participants.len() < 250
                                {
                                    s.participants.push(Participant {
                                        id,
                                        name: roster
                                            .get(&(
                                                account::snowflake(&data["guild_id"]).unwrap_or(0),
                                                id,
                                            ))
                                            .map(|(_, p)| p.name.clone())
                                            .unwrap_or_else(|| format!("User {id}")),
                                        speaking: false,
                                        muted: data["mute"].as_bool().unwrap_or(false)
                                            || data["self_mute"].as_bool().unwrap_or(false),
                                        deafened: data["deaf"].as_bool().unwrap_or(false)
                                            || data["self_deaf"].as_bool().unwrap_or(false),
                                    });
                                }
                            });
                        }
                    }
                    if permission_event_affects_call(
                        &kind,
                        &data,
                        pending.as_ref(),
                        snapshot(&state).account.as_ref().map(|a| a.id),
                    ) && pending.is_some()
                    {
                        recovery.revoke();
                        gate.set_suppressed(true);
                        transport.take();
                        join_generation = join_generation.wrapping_add(1);
                        gate.begin_session();
                        if let Some(task) = voice_task.take() {
                            task.abort();
                        }
                        update(&state, &repaint, |s| {
                            s.phase = Phase::SignalingReady;
                            s.status="Server permissions or channel configuration changed. Audio stopped; rejoin after checking access.".into();
                        });
                        pending = None;
                    }
                    if signal_ready
                        && recovery.retry.is_none()
                        && recovery.config.is_none()
                        && transport.is_none()
                        && voice_task.as_ref().is_none_or(|t| t.is_finished())
                        && let Some(p) = &mut pending
                        && p.allow_initial_connect
                        && let Some((session_id, endpoint, token)) = p.take_tokens()
                        && let Some(user) = snapshot(&state).account
                    {
                        let info = VoiceConnection {
                            guild_id: p.guild,
                            channel_id: p.channel,
                            user_id: user.id,
                            session_id,
                            endpoint,
                            token,
                        };
                        let s = snapshot(&state);
                        let config = AudioConfig {
                            input_device: s.selected_input,
                            output_device: s.selected_output,
                            output_volume: s.output_volume,
                        };
                        p.allow_initial_connect = false;
                        p.session = Some(info.session_id.clone());
                        recovery.info = Some(info.clone());
                        voice_task = Some(launch_transport(
                            info,
                            config,
                            gate.clone(),
                            join_generation,
                            results_tx.clone(),
                            voice_tx.clone(),
                        ));
                    }
                }
                None => {
                    signal_ready = false;
                    signal_retry = None;
                    signal_deadline = None;
                    recovery.revoke();
                    gate.set_suppressed(true);
                    gate.begin_session();
                    transport.take();
                    join_generation = join_generation.wrapping_add(1);
                    if let Some(task) = voice_task.take() {
                        task.abort();
                    }
                    pending = None;
                    gateway = None;
                    update(&state, &repaint, |s| {
                        s.phase = Phase::Failed;
                        s.status = "Signaling stopped. Reconnect your account to continue.".into();
                        s.participants.clear();
                    });
                }
            },
            Incoming::Voice(event) => {
                let Some((id, event)) = event else {
                    continue;
                };
                if id != join_generation {
                    continue;
                }
                match Some(event) {
                    Some(TransportEvent::Ready) => {}
                    Some(TransportEvent::Speaking { user_id, speaking }) => {
                        update(&state, &repaint, |s| {
                            if let Some(p) = s.participants.iter_mut().find(|p| p.id == user_id) {
                                p.speaking = speaking;
                            }
                        })
                    }
                    Some(TransportEvent::Disconnected { message, retryable }) => {
                        retire_voice_attempt(id, &mut join_generation, &gate);
                        transport.take();
                        if let Some(task) = voice_task.take() {
                            task.abort();
                        }
                        recovery.device_failure = false;
                        let retrying = retryable
                            && signal_ready
                            && recovery.schedule(join_generation, pending.as_ref());
                        if !retrying {
                            recovery.revoke();
                        }
                        update(&state, &repaint, |s| {
                            s.phase = if retrying {
                                Phase::Reconnecting
                            } else {
                                Phase::Failed
                            };
                            s.status = if retrying {
                                "Transient voice failure; retrying the same authorized session."
                                    .into()
                            } else {
                                message
                            };
                            s.input_level = 0.0;
                        });
                    }
                    Some(TransportEvent::DeviceFailure(message)) => {
                        retire_voice_attempt(id, &mut join_generation, &gate);
                        transport.take();
                        if let Some(task) = voice_task.take() {
                            task.abort();
                        }
                        recovery.device_failure = true;
                        let retrying =
                            signal_ready && recovery.schedule(join_generation, pending.as_ref());
                        if !retrying {
                            recovery.revoke();
                        }
                        update(&state, &repaint, |s| {
                            s.phase = if retrying {
                                Phase::Reconnecting
                            } else {
                                Phase::Failed
                            };
                            s.status = if retrying {
                                "Audio device interrupted; retrying only the same microphone and speaker.".into()
                            } else {
                                message
                            };
                            s.input_level = 0.0;
                        });
                    }
                    None => {}
                }
            }
            Incoming::Recovery => {
                let now = Instant::now();
                if signal_deadline.is_some_and(|deadline| now >= deadline) {
                    gateway = None;
                    signal_ready = false;
                    signal_deadline = None;
                    signal_retry = if signal_resume.is_some() {
                        signal_budget.schedule(generation, now)
                    } else {
                        None
                    };
                    if signal_retry.is_none() {
                        recovery.revoke();
                        pending = None;
                        update(&state, &repaint, |s| {
                            s.phase = Phase::Failed;
                            s.status =
                                "Session recovery exhausted. Reconnect and explicitly Join again."
                                    .into();
                        });
                    }
                }
                if signal_retry.is_some_and(|retry| retry.is_due(now, generation)) {
                    signal_retry = None;
                    if let (Some(c), Some(resume)) = (&client, signal_resume.clone()) {
                        gateway = Some(c.resume_gateway(resume));
                        signal_deadline = Some(now + AUTHORITY_TIMEOUT);
                    }
                }
                if recovery
                    .retry
                    .is_some_and(|retry| retry.is_due(now, join_generation))
                {
                    recovery.retry = None;
                    if signal_ready
                        && recovery.authorized(pending.as_ref())
                        && let (Some(info), Some(mut config)) =
                            (recovery.info.clone(), recovery.config.clone())
                    {
                        config.output_volume = snapshot(&state).output_volume;
                        voice_task = Some(launch_transport(
                            info,
                            config,
                            gate.clone(),
                            join_generation,
                            results_tx.clone(),
                            voice_tx.clone(),
                        ));
                    }
                }
            }
            Incoming::Meter => {
                if let Some(t) = &transport
                    && let Ok(mut s) = state.lock()
                {
                    s.input_level = t.meter();
                    s.audio_diagnostics = t.diagnostics();
                }
            }
        }
    }
    gate.fail_closed();
    if let Some(t) = chat_task {
        t.abort();
    }
    if let Some(t) = connect_task {
        t.abort();
    }
    if let Some(t) = voice_task {
        t.abort();
    }
    drop(transport);
    drop(gateway);
}
fn retire_voice_attempt(expected: u64, current: &mut u64, gate: &TxGate) -> bool {
    if expected != *current {
        return false;
    }
    gate.set_suppressed(true);
    gate.set_ptt_pressed(None);
    gate.begin_session();
    *current = current.wrapping_add(1);
    true
}
fn retryable_connect_error(error: &anyhow::Error, device_episode: bool) -> bool {
    error.downcast_ref::<TransientVoiceFailure>().is_some()
        || (device_episode && error.downcast_ref::<AudioOpenFailure>().is_some())
}
fn may_send_voice_flags(
    signaling_ready: bool,
    voice_active: bool,
    authority_current: bool,
) -> bool {
    signaling_ready && voice_active && authority_current
}
async fn send_voice_flags(
    gateway: &Option<Gateway>,
    pending: &Option<PendingVoice>,
    state: &Shared,
) {
    if let (Some(g), Some(p)) = (gateway, pending) {
        let s = snapshot(state);
        let _ = g
            .commands
            .send(GatewayCommand::Voice {
                guild_id: p.guild,
                channel_id: Some(p.channel),
                muted: s.muted || s.deafened,
                deafened: s.deafened,
            })
            .await;
    }
}

fn update_roster(roster: &mut HashMap<(u64, u64), (u64, Participant)>, data: &serde_json::Value) {
    let (Some(guild), Some(user)) = (
        account::snowflake(&data["guild_id"]),
        account::snowflake(&data["user_id"]),
    ) else {
        return;
    };
    let previous = roster.remove(&(guild, user));
    let Some(channel) = account::snowflake(&data["channel_id"]) else {
        return;
    };
    if roster.len() >= 2048 {
        return;
    }
    let name = if data["member"]["user"].is_object() {
        account::display_name(&data["member"]["user"])
    } else {
        previous
            .map(|(_, p)| p.name)
            .unwrap_or_else(|| format!("User {user}"))
    };
    roster.insert(
        (guild, user),
        (
            channel,
            Participant {
                id: user,
                name,
                speaking: false,
                muted: data["mute"].as_bool().unwrap_or(false)
                    || data["self_mute"].as_bool().unwrap_or(false),
                deafened: data["deaf"].as_bool().unwrap_or(false)
                    || data["self_deaf"].as_bool().unwrap_or(false),
            },
        ),
    );
}
fn permission_event_affects_call(
    kind: &str,
    data: &serde_json::Value,
    pending: Option<&PendingVoice>,
    own_user: Option<u64>,
) -> bool {
    let Some(p) = pending else {
        return false;
    };
    match kind {
        "GUILD_DELETE" | "GUILD_UPDATE" => account::snowflake(&data["id"]) == Some(p.guild),
        "CHANNEL_DELETE" | "CHANNEL_UPDATE" => account::snowflake(&data["id"]) == Some(p.channel),
        "GUILD_MEMBER_UPDATE" => {
            account::snowflake(&data["guild_id"]) == Some(p.guild)
                && account::snowflake(&data["user"]["id"]) == own_user
        }
        "GUILD_ROLE_UPDATE" | "GUILD_ROLE_DELETE" => {
            account::snowflake(&data["guild_id"]) == Some(p.guild)
        }
        _ => false,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn unrelated_permission_events_do_not_interrupt() {
        let p = PendingVoice {
            guild: 1,
            channel: 2,
            ..Default::default()
        };
        assert!(!permission_event_affects_call(
            "CHANNEL_UPDATE",
            &json!({"id":"3"}),
            Some(&p),
            Some(4)
        ));
        assert!(!permission_event_affects_call(
            "GUILD_MEMBER_UPDATE",
            &json!({"guild_id":"1","user":{"id":"5"}}),
            Some(&p),
            Some(4)
        ));
        assert!(permission_event_affects_call(
            "GUILD_MEMBER_UPDATE",
            &json!({"guild_id":"1","user":{"id":"4"}}),
            Some(&p),
            Some(4)
        ));
        assert!(permission_event_affects_call(
            "CHANNEL_DELETE",
            &json!({"id":"2"}),
            Some(&p),
            Some(4)
        ));
    }
    #[test]
    fn snapshot_poison_fails_to_safe_defaults() {
        let state = Arc::new(Mutex::new(UiState::default()));
        let worker = state.clone();
        let _ = std::thread::spawn(move || {
            let _lock = worker.lock().unwrap();
            panic!("test poison");
        })
        .join();
        let s = snapshot(&state);
        assert!(s.muted);
        assert_eq!(s.phase, Phase::Offline);
    }
    #[test]
    fn initial_state_is_muted_disconnected() {
        let s = UiState::default();
        assert!(s.muted);
        assert!(s.account.is_none());
        assert!(s.participants.is_empty());
        assert_eq!(s.phase, Phase::Offline);
    }
    #[test]
    fn voice_tokens_wait_for_both_events_without_loss() {
        let mut p = PendingVoice {
            session: Some("session".into()),
            ..Default::default()
        };
        assert!(p.take_tokens().is_none());
        assert_eq!(p.session.as_deref(), Some("session"));
        p.server = Some(("endpoint".into(), "secret".into()));
        assert!(p.take_tokens().is_some());
        assert!(p.take_tokens().is_none());
        p.server = Some(("endpoint".into(), "secret".into()));
        assert!(p.take_tokens().is_none());
        assert!(p.server.is_some());
    }
    #[tokio::test]
    async fn removed_endpoint_invalidates_inflight_join_and_gate() {
        let gate = TxGate::default();
        gate.set_suppressed(false);
        gate.set_muted(false);
        let session = gate.session();
        let mut generation = 5;
        let mut transport = None;
        let mut pending = Some(PendingVoice::default());
        let worker = tokio::spawn(std::future::pending());
        let abort = worker.abort_handle();
        let mut task = Some(worker);
        cancel_voice(
            &gate,
            &mut generation,
            &mut transport,
            &mut pending,
            &mut task,
        );
        tokio::task::yield_now().await;
        assert_eq!(generation, 6);
        assert_ne!(session, gate.session());
        assert!(!gate.transmit_allowed());
        assert!(task.is_none());
        assert!(pending.is_none());
        assert!(abort.is_finished());
    }
    #[test]
    fn roster_keeps_names_across_partial_updates_and_removes_leavers() {
        let mut roster = HashMap::new();
        update_roster(
            &mut roster,
            &json!({"guild_id":"1","user_id":"2","channel_id":"3","member":{"user":{"username":"Alex"}}}),
        );
        update_roster(
            &mut roster,
            &json!({"guild_id":"1","user_id":"2","channel_id":"3","self_mute":true}),
        );
        assert_eq!(roster[&(1, 2)].1.name, "Alex");
        assert!(roster[&(1, 2)].1.muted);
        update_roster(
            &mut roster,
            &json!({"guild_id":"1","user_id":"2","channel_id":null}),
        );
        assert!(roster.is_empty());
    }
    fn authorized_recovery() -> (CallRecovery, PendingVoice) {
        let info = VoiceConnection {
            guild_id: 1,
            channel_id: 2,
            user_id: 3,
            session_id: "test-session".into(),
            endpoint: "voice.discord.gg".into(),
            token: "test-not-a-credential".into(),
        };
        (
            CallRecovery {
                info: Some(info),
                config: Some(AudioConfig {
                    input_device: Some("specific-mic".into()),
                    output_device: Some("specific-speaker".into()),
                    output_volume: 0.5,
                }),
                ..Default::default()
            },
            PendingVoice {
                guild: 1,
                channel: 2,
                session: Some("test-session".into()),
                server: None,
                allow_initial_connect: false,
            },
        )
    }
    #[test]
    fn revoked_call_cannot_restore_after_resume() {
        let (mut r, p) = authorized_recovery();
        assert!(r.schedule(1, Some(&p)));
        r.revoke();
        assert!(!r.schedule(1, Some(&p)));
        assert!(r.retry.is_none());
    }
    #[test]
    fn changed_channel_or_session_never_retries() {
        let (mut r, mut p) = authorized_recovery();
        p.channel = 9;
        assert!(!r.schedule(1, Some(&p)));
        p.channel = 2;
        p.session = Some("new-owner".into());
        assert!(!r.schedule(1, Some(&p)));
    }
    #[test]
    fn recovery_never_invents_default_devices() {
        let (mut r, p) = authorized_recovery();
        assert!(r.schedule(1, Some(&p)));
        let c = r.config.as_ref().unwrap();
        assert_eq!(c.input_device.as_deref(), Some("specific-mic"));
        assert_eq!(c.output_device.as_deref(), Some("specific-speaker"));
        r.config = None;
        assert!(!r.schedule(1, Some(&p)));
    }
    #[test]
    fn cancelled_generation_cannot_fire_retry() {
        let (mut r, p) = authorized_recovery();
        assert!(r.schedule(7, Some(&p)));
        let retry = r.retry.unwrap();
        assert!(retry.is_due(retry.at, 7));
        assert!(!retry.is_due(retry.at, 8));
    }
    #[test]
    fn repeated_device_recovery_is_bounded() {
        let (mut r, p) = authorized_recovery();
        for _ in 0..5 {
            assert!(r.schedule(1, Some(&p)));
        }
        assert!(!r.schedule(1, Some(&p)));
    }
    #[test]
    fn mute_controls_never_send_channel_updates_during_replay_or_recovery() {
        assert!(!may_send_voice_flags(false, false, true));
        assert!(!may_send_voice_flags(false, true, true));
        assert!(!may_send_voice_flags(true, false, true));
        assert!(!may_send_voice_flags(true, true, false));
        assert!(may_send_voice_flags(true, true, true));
    }
    #[test]
    fn terminal_failure_revokes_future_resumed_authority() {
        let (mut r, p) = authorized_recovery();
        r.revoke();
        assert!(!r.authorized(Some(&p)));
        assert!(!r.schedule(9, Some(&p)));
        assert!(!p.allow_initial_connect);
    }
    #[test]
    fn recovery_requires_both_actual_device_ids() {
        let (mut r, p) = authorized_recovery();
        r.config.as_mut().unwrap().input_device = None;
        assert!(!r.authorized(Some(&p)));
        let (mut r, p) = authorized_recovery();
        r.config.as_mut().unwrap().output_device = None;
        assert!(!r.authorized(Some(&p)));
    }
    #[test]
    fn retry_decision_uses_typed_result_not_event_arrival_order() {
        assert!(retryable_connect_error(
            &TransientVoiceFailure.into(),
            false
        ));
        assert!(!retryable_connect_error(&AudioOpenFailure.into(), false));
        assert!(retryable_connect_error(&AudioOpenFailure.into(), true));
        assert!(!retryable_connect_error(
            &anyhow::anyhow!("fatal protocol failure"),
            true
        ));
    }
    #[test]
    fn result_and_disconnect_orders_charge_one_retry() {
        for order in [["result", "event"], ["event", "result"]] {
            let gate = TxGate::default();
            let mut generation = 7;
            let (mut recovery, p) = authorized_recovery();
            for _source in order {
                if retire_voice_attempt(7, &mut generation, &gate) {
                    assert!(recovery.schedule(generation, Some(&p)));
                }
            }
            assert_eq!(generation, 8);
            assert_eq!(recovery.budget.attempts(), 1);
        }
    }
    // Account/channel changes must release pending history and erase displayed
    // text, without changing the independent call's microphone controls.
    #[tokio::test]
    async fn leaving_text_scope_erases_history_and_cancels_pending_read() {
        let state = Arc::new(Mutex::new(UiState {
            selected_text_channel: Some(10),
            messages: vec![crate::messaging::ChatMessage {
                id: 1,
                author_id: 2,
                author_name: "Tester".into(),
                content: "Private test text".into(),
            }],
            chat_busy: true,
            muted: false,
            ..Default::default()
        }));
        let mut request = 7;
        let mut task = Some(tokio::spawn(std::future::pending::<()>()));
        let abort = task.as_ref().unwrap().abort_handle();
        reset_chat(&state, &None, &mut request, &mut task);
        tokio::task::yield_now().await;
        let s = snapshot(&state);
        assert!(s.messages.is_empty());
        assert_eq!(s.selected_text_channel, None);
        assert!(!s.chat_busy);
        assert!(!s.muted);
        assert_ne!(request, 7);
        assert!(abort.is_finished());
    }
}
