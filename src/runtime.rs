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
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};
use tokio::sync::mpsc as async_mpsc;
type Shared = Arc<Mutex<UiState>>;
type Repaint = Option<Arc<dyn Fn() + Send + Sync>>;
fn update(state: &Shared, repaint: &Repaint, f: impl FnOnce(&mut UiState)) {
    if let Ok(mut s) = state.lock() {
        f(&mut s);
        crate::calls::refresh(&mut s);
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
        s.selected_dm = None;
        s.social_busy = false;
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
        bool,
    ),
    Social(
        u64,
        u64,
        Result<Vec<crate::social::Friend>>,
        Result<crate::social::DirectChannelsSnapshot>,
    ),
    OpenedDm(u64, u64, Result<crate::social::DirectChannel>),
    Ring(u64, Result<()>),
    StoppedRing(u64, u64, bool),
    Channels(u64, u64, Result<Vec<Channel>>),
    TextChannels(u64, u64, u64, Result<Vec<crate::messaging::TextChannel>>),
    Messages(u64, u64, u64, Result<Vec<crate::messaging::ChatMessage>>),
    Sent(u64, u64, u64, Result<crate::messaging::ChatMessage>),
    Transport(u64, Result<Transport>),
}
#[derive(Default)]
struct PendingVoice {
    guild: u64,
    private: bool,
    ring_pending: bool,
    channel: u64,
    session: Option<String>,
    server: Option<(String, String)>,
    allow_initial_connect: bool,
    roster_complete: bool,
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
        matches!((&self.info,&self.config,pending),(Some(info),Some(config),Some(p)) if config.input_device.is_some()&&config.output_device.is_some()&&info.guild_id==(!p.private).then_some(p.guild) && info.channel_id==p.channel && p.session.as_deref()==Some(info.session_id.as_str()))
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
    handshake: Arc<songbird::DaveHandshake>,
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
            Transport::connect(info, gate, config, event_tx, expected_session, handshake),
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
    let mut complete_rosters = HashSet::new();
    let mut ui_visible = true;
    let mut repaint: Repaint = None;
    let mut client: Option<Arc<PersonalAccount>> = None;
    let mut remembered_session = false;
    let mut gateway: Option<Gateway> = None;
    let mut transport: Option<Transport> = None;
    let (mut generation, mut join_generation) = (0_u64, 0_u64);
    let mut server_suppressed = false;
    let mut pending: Option<PendingVoice> = None;
    let mut connect_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut voice_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut ring_task: Option<tokio::task::JoinHandle<()>> = None;
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
                        | Command::CallDm(_)
                        | Command::Leave
                        | Command::Reconnect
                        | Command::Quit
                ) {
                    let may_have_rung = ring_task.is_some();
                    if let Some(task) = ring_task.take() {
                        task.abort();
                    }
                    let old = snapshot(&state);
                    if let (Some(c), Some(call)) = (client.clone(), old.current_call.as_ref())
                        && call.private
                        && (may_have_rung || call.ring_requested || call.ringing)
                        && let Some(dm) = old
                            .direct_channels
                            .iter()
                            .find(|d| d.id == call.channel && d.recipients.len() == 1)
                    {
                        let channel = call.channel;
                        let recipients = dm.recipients.iter().map(|u| u.id).collect::<Vec<_>>();
                        let tx = results_tx.clone();
                        let account_scope = generation;
                        tokio::spawn(async move {
                            let outcome = tokio::time::timeout(
                                Duration::from_secs(2),
                                c.stop_ringing(channel, &recipients),
                            )
                            .await;
                            let failed = !matches!(outcome, Ok(Ok(())));
                            let _ = tx
                                .send(ResultEvent::StoppedRing(account_scope, channel, failed))
                                .await;
                        });
                    }
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
                if matches!(&command, Command::RefreshDevices) {
                    update(&state, &repaint, |s| {
                        s.microphone_permission = crate::microphone::status()
                    });
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
                        remembered_session = false;
                        update(&state, &repaint, |s| {
                            s.login_storage_status = if remember {
                                "Checking login; existing remembered access is preserved until this login succeeds.".into()
                            } else {
                                "Session-only login will disable automatic login after successful authentication.".into()
                            };
                        });
                        reset_chat(&state, &repaint, &mut chat_generation, &mut chat_task);
                        generation = generation.wrapping_add(1);
                        join_generation = join_generation.wrapping_add(1);
                        gate.begin_session();
                        gate.fail_closed();
                        transport.take();
                        gateway.take();
                        client.take();
                        roster.clear();
                        complete_rosters.clear();
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
                            s.profiles.clear();
                            s.current_call = None;
                            s.friends.clear();
                            s.direct_channels.clear();
                            s.social_status.clear();
                            s.selected_call_dm = None;
                            s.selected_call_guild = None;
                            s.selected_channel = None;
                            s.selected_guild = None;
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
                            let _ = tx
                                .send(ResultEvent::Account(id, result, saved, false))
                                .await;
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
                                complete_rosters.clear();
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
                                    let _ =
                                        tx.send(ResultEvent::Account(id, result, None, true)).await;
                                }));
                                update(&state, &repaint, |s| {
                                    s.phase = Phase::Connecting;
                                    s.status = "Connecting with saved credential…".into();
                                });
                            }
                            Err(error) => update(&state, &repaint, |s| {
                                s.status = error.to_string();
                                s.login_storage_status = error.to_string();
                            }),
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
                        let forgotten = crate::credential::sign_out();
                        remembered_session = false;
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
                        complete_rosters.clear();
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
                    Command::SelectHome => {
                        if snapshot(&state).chat_sending {
                            continue;
                        }
                        if let Some(c) = client.clone() {
                            reset_chat(&state, &repaint, &mut chat_generation, &mut chat_task);
                            let (id, request, tx) =
                                (generation, chat_generation, results_tx.clone());
                            update(&state, &repaint, |s| {
                                s.selected_guild = None;
                                s.channels.clear();
                                s.social_busy = true;
                                s.social_status = "Loading friends and conversations…".into();
                            });
                            chat_task = Some(tokio::spawn(async move {
                                let (friends, channels) =
                                    tokio::join!(c.friends(), c.direct_channels());
                                let _ = tx
                                    .send(ResultEvent::Social(id, request, friends, channels))
                                    .await;
                            }));
                        }
                    }
                    Command::OpenDm(user) => {
                        let s = snapshot(&state);
                        if s.chat_sending
                            || s.social_busy
                            || s.selected_guild.is_some()
                            || !s.friends.iter().any(|f| f.id == user)
                        {
                            continue;
                        }
                        if let Some(c) = client.clone() {
                            reset_chat(&state, &repaint, &mut chat_generation, &mut chat_task);
                            let (id, request, tx) =
                                (generation, chat_generation, results_tx.clone());
                            update(&state, &repaint, |s| {
                                s.social_busy = true;
                                s.social_status = "Opening conversation…".into();
                            });
                            chat_task = Some(tokio::spawn(async move {
                                let result = c.open_dm(user).await;
                                let _ = tx.send(ResultEvent::OpenedDm(id, request, result)).await;
                            }));
                        }
                    }
                    Command::SelectDm(channel) => {
                        let s = snapshot(&state);
                        if s.social_busy
                            || s.chat_sending
                            || s.selected_guild.is_some()
                            || !s.direct_channels.iter().any(|c| c.id == channel)
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
                                s.selected_dm = Some(channel);
                                s.selected_text_channel = Some(channel);
                                s.messages.clear();
                                s.chat_busy = true;
                                s.chat_status = "Loading messages…".into();
                            });
                            chat_task = Some(tokio::spawn(async move {
                                let result = c.messages(channel).await;
                                let _ = tx
                                    .send(ResultEvent::Messages(id, request, channel, result))
                                    .await;
                            }));
                        }
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
                                s.selected_dm = None;
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
                            || !chat_channel_allowed(&s, channel_id)
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
                        gate.set_suppressed(true);
                        if let (Some(g), Some(p)) = (&gateway, &pending) {
                            let _ = g.commands.send(voice_command(p, None, true, false)).await;
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
                        gate.set_remote_muted(true);
                        gate.set_deafened(true);
                        pending = Some(PendingVoice {
                            guild: guild_id,
                            roster_complete: complete_rosters.contains(&guild_id),
                            private: false,
                            channel: channel_id,
                            allow_initial_connect: true,
                            ..Default::default()
                        });
                        update(&state, &repaint, |s| {
                            s.phase = Phase::Joining;
                            s.microphone_permission = crate::microphone::status();
                            s.confirmed_muted = None;
                            s.confirmed_deafened = None;
                            s.server_deafened = false;
                            s.server_suppressed = false;
                            let target = s
                                .channels
                                .iter()
                                .find(|c| c.id == channel_id)
                                .map(|c| c.name.clone())
                                .unwrap_or_else(|| "Voice channel".into());
                            crate::calls::start(s, channel_id, false, target);
                            s.status = "Joining encrypted voice…".into();
                            s.selected_channel = Some(channel_id);
                            s.selected_call_dm = None;
                            s.selected_call_guild = Some(guild_id);
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
                                    muted: true,
                                    deafened: s.deafened,
                                })
                                .await;
                        }
                    }
                    Command::CallDm(channel_id) => {
                        let s = snapshot(&state);
                        if !signal_ready
                            || gateway.is_none()
                            || s.account.is_none()
                            || matches!(s.phase, Phase::Joining | Phase::Reconnecting)
                            || !s
                                .direct_channels
                                .iter()
                                .any(|c| c.id == channel_id && c.recipients.len() == 1)
                        {
                            continue;
                        }
                        gate.set_suppressed(true);
                        if let (Some(g), Some(p)) = (&gateway, &pending) {
                            let _ = g.commands.send(voice_command(p, None, true, false)).await;
                        }
                        cancel_voice(
                            &gate,
                            &mut join_generation,
                            &mut transport,
                            &mut pending,
                            &mut voice_task,
                        );
                        server_suppressed = false;
                        gate.set_remote_muted(true);
                        gate.set_deafened(true);
                        pending = Some(PendingVoice {
                            guild: channel_id,
                            private: true,
                            ring_pending: true,
                            channel: channel_id,
                            allow_initial_connect: true,
                            ..Default::default()
                        });
                        update(&state, &repaint, |s| {
                            s.phase = Phase::Joining;
                            s.microphone_permission = crate::microphone::status();
                            s.confirmed_muted = None;
                            s.confirmed_deafened = None;
                            s.server_deafened = false;
                            s.server_suppressed = false;
                            let target = s
                                .direct_channels
                                .iter()
                                .find(|c| c.id == channel_id)
                                .map(|c| c.name.clone())
                                .unwrap_or_else(|| "Private call".into());
                            crate::calls::start(s, channel_id, true, target);
                            s.status = "Connecting call…".into();
                            s.selected_channel = Some(channel_id);
                            s.selected_call_dm = Some(channel_id);
                            s.selected_call_guild = None;
                            s.participants.clear();
                        });
                        if let Some(g) = &gateway {
                            let _ = g
                                .commands
                                .send(GatewayCommand::RequestCall { channel_id })
                                .await;
                            let _ = g
                                .commands
                                .send(GatewayCommand::PrivateVoice {
                                    channel_id: Some(channel_id),
                                    muted: true,
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
                            let _ = g.commands.send(voice_command(&p, None, true, false)).await;
                        }
                        update(&state, &repaint, |s| {
                            s.phase = if s.account.is_some() {
                                Phase::SignalingReady
                            } else {
                                Phase::Offline
                            };
                            s.status = "Left voice. Microphone released.".into();
                            s.selected_channel = None;
                            s.selected_call_dm = None;
                            s.selected_call_guild = None;
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
                            voice_flags_authorized(&recovery, pending.as_ref()),
                        ) {
                            send_voice_flags(&gateway, &pending, &state).await;
                        }
                    }
                    Command::SetDeafened(value) => {
                        let s = snapshot(&state);
                        let effective = value
                            || s.server_deafened
                            || (pending.is_some() && s.confirmed_deafened != Some(false));
                        gate.set_deafened(effective);
                        if let Some(t) = &mut transport {
                            t.deafen(effective);
                        }
                        update(&state, &repaint, |s| s.deafened = value);
                        if may_send_voice_flags(
                            signal_ready,
                            transport.is_some(),
                            voice_flags_authorized(&recovery, pending.as_ref()),
                        ) {
                            send_voice_flags(&gateway, &pending, &state).await;
                        }
                    }
                    Command::SetCallSounds(value) => {
                        update(&state, &repaint, |s| s.call_sounds = value)
                    }
                    Command::SetSoundVolume(value) => {
                        update(&state, &repaint, |s| s.sound_volume = value.clamp(0.0, 1.0))
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
                Some(ResultEvent::Social(id, request, friends, channels))
                    if id == generation
                        && request == chat_generation
                        && snapshot(&state).selected_guild.is_none() =>
                {
                    chat_task.take();
                    update(&state, &repaint, |s| {
                        apply_social_snapshot(s, friends, channels)
                    });
                }
                Some(ResultEvent::OpenedDm(id, request, result))
                    if id == generation
                        && request == chat_generation
                        && snapshot(&state).selected_guild.is_none() =>
                {
                    chat_task.take();
                    match result {
                        Ok(channel) => {
                            let channel_id = channel.id;
                            update(&state, &repaint, |s| {
                                s.social_busy = false;
                                s.social_status.clear();
                                s.direct_channels.retain(|c| c.id != channel_id);
                                for f in &channel.recipients {
                                    if s.profiles.len() < 2048 || s.profiles.contains_key(&f.id) {
                                        let p = s.profiles.entry(f.id).or_default();
                                        if f.avatar.is_some() {
                                            p.avatar = f.avatar.clone();
                                        }
                                    }
                                }
                                s.direct_channels.insert(0, channel);
                                crate::social::sort_by_activity(&mut s.friends, &s.direct_channels);
                                s.selected_dm = Some(channel_id);
                                s.selected_text_channel = Some(channel_id);
                                s.chat_busy = true;
                            });
                            if let Some(c) = client.clone() {
                                let tx = results_tx.clone();
                                chat_task = Some(tokio::spawn(async move {
                                    let result = c.messages(channel_id).await;
                                    let _ = tx
                                        .send(ResultEvent::Messages(
                                            id, request, channel_id, result,
                                        ))
                                        .await;
                                }));
                            }
                        }
                        Err(error) => update(&state, &repaint, |s| {
                            s.social_busy = false;
                            s.social_status = error.to_string();
                        }),
                    }
                }
                Some(ResultEvent::StoppedRing(id, channel, true))
                    if id == generation && snapshot(&state).account.is_some() =>
                {
                    update(&state, &repaint, |s| {
                        if s.current_call
                            .as_ref()
                            .is_some_and(|call| call.channel == channel)
                        {
                            s.status = "Remote ring cancellation was not confirmed".into();
                        }
                    });
                }
                Some(ResultEvent::Ring(id, result)) if id == join_generation => {
                    ring_task.take();
                    update(&state, &repaint, |s| match result {
                        Ok(()) => {
                            if let Some(call) = &mut s.current_call {
                                call.ring_requested = true;
                            }
                        }
                        Err(error) => {
                            s.status =
                                format!("Call transport connected, but ringing failed: {error}");
                            if let Some(call) = &mut s.current_call {
                                call.ringing = false;
                            }
                        }
                    });
                }
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
                                if let Some(last) = messages.iter().map(|m| m.id).max() {
                                    crate::social::observe_message(
                                        &mut s.direct_channels,
                                        channel,
                                        last,
                                    );
                                    crate::social::sort_by_activity(
                                        &mut s.friends,
                                        &s.direct_channels,
                                    );
                                }
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
                                crate::social::observe_message(
                                    &mut s.direct_channels,
                                    channel,
                                    message.id,
                                );
                                crate::social::sort_by_activity(&mut s.friends, &s.direct_channels);
                                crate::messaging::insert_message(&mut s.messages, message);
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
                Some(ResultEvent::Account(id, result, saved, from_saved)) if id == generation => {
                    match result {
                        Ok((c, a, guilds)) => {
                            let save_requested = saved.is_some();
                            if let Some(secret) = saved {
                                let outcome = crate::credential::remember(&secret);
                                remembered_session = outcome.is_ok();
                                update(&state, &repaint, |s| {
                                    s.login_storage_status = if outcome.is_ok() {
                                        "Remembered in macOS Keychain; reconnects on launch. Quit preserves login.".into()
                                    } else {
                                        format!(
                                            "Connected for this launch, but Remember me was not enabled: {}",
                                            outcome
                                                .err()
                                                .map_or_else(String::new, |e| e.to_string())
                                        )
                                    }
                                });
                            } else if from_saved {
                                remembered_session = true;
                                let enabled = crate::credential::enable_startup();
                                update(&state, &repaint, |s| {
                                    s.login_storage_status = if enabled.is_ok() {
                                        "Connected using macOS Keychain. Remembered for future launches; Quit preserves login.".into()
                                    } else {
                                        "Connected using Keychain, but startup preference could not be saved. Use Connect from Keychain on next launch.".into()
                                    };
                                });
                            }
                            if !remembered_session && !from_saved && !save_requested {
                                let disabled = crate::credential::disable_startup();
                                update(&state, &repaint, |s| {
                                    s.login_storage_status = if disabled.is_ok() {
                                        "Connected for this launch only; automatic login disabled."
                                            .into()
                                    } else {
                                        "Connected for this launch only, but automatic login preference could not be disabled. Use Log out & forget credential.".into()
                                    }
                                });
                            }
                            signal_ready = false;
                            gateway = Some(c.gateway());
                            client = Some(Arc::new(c));
                            update(&state, &repaint, |s| {
                                s.profiles.clear();
                                s.profiles.insert(
                                    a.id,
                                    crate::profiles::Profile {
                                        avatar: a.avatar.clone(),
                                        ..Default::default()
                                    },
                                );
                                s.account = Some(a);
                                s.guilds = guilds;
                                s.status = "Connecting Discord signaling…".into();
                            });
                        }
                        Err(error) => {
                            let rejected = saved_login_rejected(from_saved, &error);
                            let removed = if rejected {
                                crate::credential::sign_out().is_ok()
                            } else {
                                false
                            };
                            update(&state, &repaint, |s| {
                                s.phase = Phase::Failed;
                                s.status = error.to_string();
                                if rejected {
                                    s.login_storage_status = if removed {
                                        "Expired saved login removed. Sign in again.".into()
                                    } else {
                                        "Saved login rejected. Automatic login disabled if possible; remove fastdistord.personal-account in Keychain Access if prompted.".into()
                                    };
                                }
                            });
                        }
                    }
                }
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
                        let ready = t.media_ready();
                        recovery.config = Some(t.device_config());
                        recovery.retry = None;
                        recovery.budget.healthy(Instant::now());
                        if let (Some(p), Some(info)) = (&mut pending, &recovery.info) {
                            p.session = Some(info.session_id.clone());
                        }
                        let intended = snapshot(&state);
                        gate.set_muted(intended.muted);
                        gate.set_deafened(
                            intended.deafened
                                || intended.server_deafened
                                || intended.confirmed_deafened != Some(false),
                        );
                        gate.set_ptt_enabled(intended.ptt_enabled);
                        gate.set_ptt_pressed(None);
                        gate.set_suppressed(server_suppressed);
                        transport = Some(t);
                        if let Some(p) = &mut pending
                            && p.private
                            && p.ring_pending
                        {
                            p.ring_pending = false;
                            if let Some(c) = client.clone() {
                                let channel = p.channel;
                                let recipients = snapshot(&state)
                                    .direct_channels
                                    .iter()
                                    .find(|d| d.id == channel)
                                    .map(|d| d.recipients.iter().map(|u| u.id).collect::<Vec<_>>())
                                    .unwrap_or_default();
                                let tx = results_tx.clone();
                                ring_task = Some(tokio::spawn(async move {
                                    let result = c.ring(channel, &recipients).await;
                                    let _ = tx.send(ResultEvent::Ring(id, result)).await;
                                }));
                            }
                        }
                        update(&state, &repaint, |s| {
                            s.microphone_permission = crate::microphone::status();
                            s.phase = if ready {
                                Phase::VoiceReady
                            } else {
                                Phase::VoiceWaiting
                            };
                            s.status = if ready {
                                "Encrypted voice connected · headphones recommended".into()
                            } else {
                                "Joined · waiting for encrypted voice; microphone closed".into()
                            };
                        });
                        send_voice_flags(&gateway, &pending, &state).await;
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
                        for p in s.profiles.values_mut() {
                            p.presence = crate::profiles::Presence::Unknown;
                        }
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
                    auth_rejected,
                    resume,
                }) => {
                    if auth_rejected {
                        let removed = !remembered_session || crate::credential::sign_out().is_ok();
                        remembered_session = false;
                        client.take();
                        roster.clear();
                        complete_rosters.clear();
                        reset_chat(&state, &repaint, &mut chat_generation, &mut chat_task);
                        update(&state, &repaint, |s| {
                            s.account = None;
                            s.selected_channel = None;
                            s.selected_guild = None;
                            s.selected_call_dm = None;
                            s.selected_call_guild = None;
                            s.profiles.clear();
                            s.friends.clear();
                            s.direct_channels.clear();
                            s.guilds.clear();
                            s.channels.clear();
                            s.current_call = None;
                            s.login_storage_status = if removed {
                                "Discord rejected this login. Sign in again; no automatic retry."
                                    .into()
                            } else {
                                "Discord rejected this login. Keychain removal failed; remove fastdistord.personal-account in Keychain Access.".into()
                            };
                        });
                    }

                    if let Some(task) = ring_task.take() {
                        task.abort();
                    }
                    update(&state, &repaint, |s| {
                        for p in s.profiles.values_mut() {
                            p.presence = crate::profiles::Presence::Unknown;
                        }
                    });
                    signal_ready = false;
                    signal_deadline = None;
                    gate.set_remote_muted(true);
                    update(&state, &repaint, |s| {
                        s.confirmed_muted = None;
                        s.confirmed_deafened = None;
                    });
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
                Some(GatewayEvent::SelfPresence(presence)) => {
                    update(&state, &repaint, |s| {
                        if let Some(a) = &s.account {
                            s.profiles.entry(a.id).or_default().presence = presence;
                        }
                    });
                }
                Some(GatewayEvent::RosterBegin(guild)) => {
                    complete_rosters.remove(&guild);
                    roster.retain(|(g, _), _| *g != guild);
                    if let Some(p) = &mut pending
                        && !p.private
                        && p.guild == guild
                    {
                        p.roster_complete = false;
                    }
                    publish_roster_count(&state, pending.as_ref(), &roster);
                }
                Some(GatewayEvent::RosterComplete(guild, complete)) => {
                    if complete {
                        complete_rosters.insert(guild);
                    }
                    if let Some(p) = &mut pending
                        && !p.private
                        && p.guild == guild
                    {
                        p.roster_complete = complete;
                    }
                    publish_roster_count(&state, pending.as_ref(), &roster);
                }
                Some(GatewayEvent::Dispatch { kind, data }) => {
                    if matches!(
                        kind.as_str(),
                        "PRESENCE_UPDATE" | "USER_UPDATE" | "MESSAGE_CREATE" | "VOICE_STATE_UPDATE"
                    ) {
                        update(&state, &repaint, |s| match kind.as_str() {
                            "PRESENCE_UPDATE" => crate::profiles::presence(&mut s.profiles, &data),
                            "USER_UPDATE" => crate::profiles::observe(&mut s.profiles, &data),
                            "MESSAGE_CREATE" => {
                                crate::profiles::observe(&mut s.profiles, &data["author"])
                            }
                            _ => crate::profiles::observe(&mut s.profiles, &data["member"]["user"]),
                        });
                    }
                    if kind == "MESSAGE_CREATE" {
                        if let (Some(channel), Some(message)) = (
                            account::snowflake(&data["channel_id"]),
                            account::snowflake(&data["id"]),
                        ) {
                            update(&state, &repaint, |s| {
                                crate::social::observe_message(
                                    &mut s.direct_channels,
                                    channel,
                                    message,
                                );
                                crate::social::sort_by_activity(&mut s.friends, &s.direct_channels);
                            });
                        }
                        let s = snapshot(&state);
                        if let Some(channel) = s.selected_text_channel
                            && chat_channel_allowed(&s, channel)
                            && let Ok(message) = crate::messaging::parse_message(&data, channel)
                        {
                            update(&state, &repaint, |s| {
                                crate::messaging::insert_message(&mut s.messages, message);
                            });
                        }
                    }
                    if kind == "MESSAGE_UPDATE" {
                        let selected = snapshot(&state);
                        if let Some(channel) = selected.selected_text_channel
                            && chat_channel_allowed(&selected, channel)
                            && account::snowflake(&data["channel_id"]) == Some(channel)
                        {
                            update(&state, &repaint, |s| {
                                let _ = crate::messaging::update_message(
                                    &mut s.messages,
                                    &data,
                                    channel,
                                );
                            });
                        }
                    }
                    if kind == "CALL_CREATE"
                        && let Some(p) = &mut pending
                        && p.private
                        && account::snowflake(&data["channel_id"]) == Some(p.channel)
                    {
                        let own = snapshot(&state).account.as_ref().map(|a| a.id);
                        if let Some(states) = data["voice_states"].as_array() {
                            p.roster_complete = states.len() <= 250
                                && states.iter().all(|v| {
                                    account::snowflake(&v["user_id"]).is_some()
                                        && account::snowflake(&v["channel_id"]) == Some(p.channel)
                                });
                            let channel = p.channel;
                            update(&state, &repaint, |s| {
                                s.participants = states
                                    .iter()
                                    .filter(|v| {
                                        account::snowflake(&v["channel_id"]) == Some(channel)
                                    })
                                    .filter_map(|v| {
                                        let id = account::snowflake(&v["user_id"])?;
                                        Some(Participant {
                                            id,
                                            name: s
                                                .direct_channels
                                                .iter()
                                                .find(|c| c.id == channel)
                                                .and_then(|c| {
                                                    c.recipients.iter().find(|u| u.id == id)
                                                })
                                                .map(|u| u.name.clone())
                                                .or_else(|| {
                                                    s.account
                                                        .as_ref()
                                                        .filter(|a| a.id == id)
                                                        .map(|a| a.name.clone())
                                                })
                                                .unwrap_or_else(|| "Participant".into()),
                                            speaking: false,
                                            muted: v["self_mute"].as_bool().unwrap_or(false),
                                            deafened: v["self_deaf"].as_bool().unwrap_or(false),
                                        })
                                    })
                                    .take(250)
                                    .collect();
                            });
                            for voice in states {
                                if p.allow_initial_connect
                                    && account::snowflake(&voice["user_id"]) == own
                                    && account::snowflake(&voice["channel_id"]) == Some(p.channel)
                                {
                                    p.session = voice["session_id"].as_str().map(str::to_owned);
                                    server_suppressed = apply_confirmed_voice_state(
                                        &state,
                                        &repaint,
                                        &gate,
                                        voice,
                                        transport.is_some(),
                                    );
                                }
                            }
                        }
                    }
                    if matches!(kind.as_str(), "CALL_CREATE" | "CALL_UPDATE")
                        && let Some(channel) = account::snowflake(&data["channel_id"])
                    {
                        let own = snapshot(&state).account.as_ref().map(|a| a.id);
                        let ringing =
                            data["ringing"]
                                .as_array()
                                .filter(|v| v.len() <= 25)
                                .map(|v| {
                                    v.iter()
                                        .filter_map(account::snowflake)
                                        .any(|id| Some(id) != own)
                                });
                        update(&state, &repaint, |s| {
                            crate::calls::server_call(s, channel, ringing)
                        });
                        if snapshot(&state)
                            .current_call
                            .as_ref()
                            .is_some_and(|call| call.channel == channel && call.peer_joined)
                        {
                            if let Some(p) = &mut pending {
                                p.ring_pending = false;
                            }
                            if let Some(task) = ring_task.take() {
                                task.abort();
                            }
                        }
                    }
                    if kind == "CALL_DELETE"
                        && pending.as_ref().is_some_and(|p| {
                            p.private && account::snowflake(&data["channel_id"]) == Some(p.channel)
                        })
                    {
                        if let Some(task) = ring_task.take() {
                            task.abort();
                        }
                        recovery.revoke();
                        cancel_voice(
                            &gate,
                            &mut join_generation,
                            &mut transport,
                            &mut pending,
                            &mut voice_task,
                        );
                        update(&state, &repaint, |s| {
                            s.phase = Phase::SignalingReady;
                            s.selected_channel = None;
                            s.selected_call_dm = None;
                            s.selected_call_guild = None;
                            s.participants.clear();
                            s.status = "Call ended.".into();
                        });
                    }
                    if kind == "VOICE_SERVER_UPDATE"
                        && let Some(info) = &mut recovery.info
                        && account::snowflake(&data["guild_id"]) == info.guild_id
                        && (info.guild_id.is_some()
                            || account::snowflake(&data["channel_id"]) == Some(info.channel_id))
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
                        && voice_event_matches(&data, p)
                        && (!p.private
                            || account::snowflake(&data["channel_id"]) == Some(p.channel))
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
                        && voice_state_matches(
                            &data,
                            p,
                            snapshot(&state).account.as_ref().map(|a| a.id),
                        )
                    {
                        let user = account::snowflake(&data["user_id"]);
                        let channel = account::snowflake(&data["channel_id"]);
                        let own = snapshot(&state).account.as_ref().map(|a| a.id) == user;
                        if own {
                            if channel != Some(p.channel) {
                                if let Some(task) = ring_task.take() {
                                    task.abort();
                                }
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
                                let admin_muted = apply_confirmed_voice_state(
                                    &state,
                                    &repaint,
                                    &gate,
                                    &data,
                                    transport.is_some(),
                                );
                                server_suppressed = admin_muted;
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
                                        name: s
                                            .direct_channels
                                            .iter()
                                            .find(|c| Some(c.id) == s.selected_call_dm)
                                            .and_then(|c| c.recipients.iter().find(|u| u.id == id))
                                            .map(|u| u.name.clone())
                                            .or_else(|| {
                                                roster
                                                    .get(&(
                                                        account::snowflake(&data["guild_id"])
                                                            .unwrap_or(0),
                                                        id,
                                                    ))
                                                    .map(|(_, p)| p.name.clone())
                                            })
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
                    if kind == "VOICE_STATE_UPDATE"
                        && let Some(p) = pending.as_ref().filter(|p| p.private)
                    {
                        let channel = p.channel;
                        update(&state, &repaint, |s| {
                            if s.current_call.as_ref().is_some_and(|c| c.confirmed) {
                                crate::calls::server_call(s, channel, None);
                            }
                        });
                        if snapshot(&state)
                            .current_call
                            .as_ref()
                            .is_some_and(|call| call.peer_joined)
                        {
                            if let Some(p) = &mut pending {
                                p.ring_pending = false;
                            }
                            if let Some(task) = ring_task.take() {
                                task.abort();
                            }
                        }
                    }
                    publish_roster_count(&state, pending.as_ref(), &roster);
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
                            guild_id: (!p.private).then_some(p.guild),
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
                            snapshot(&state).voice_handshake,
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
                    Some(TransportEvent::Ready) => {
                        if let Some(t) = &transport {
                            recovery.config = Some(t.device_config());
                            gate.set_ptt_pressed(None);
                            update(&state, &repaint, |s| {
                                s.microphone_permission = crate::microphone::status();
                                s.phase = Phase::VoiceReady;
                                s.status =
                                    "Encrypted voice connected · headphones recommended".into();
                            });
                            send_voice_flags(&gateway, &pending, &state).await;
                        }
                    }
                    Some(TransportEvent::Waiting) => {
                        if transport.is_some() {
                            update(&state, &repaint, |s| {
                                s.phase = Phase::VoiceWaiting;
                                s.status = "Joined · encryption pending; microphone closed".into();
                            });
                            send_voice_flags(&gateway, &pending, &state).await;
                        }
                    }
                    Some(TransportEvent::Speaking { user_id, speaking }) => {
                        update(&state, &repaint, |s| {
                            if let Some(p) = s.participants.iter_mut().find(|p| p.id == user_id) {
                                p.speaking = speaking;
                            }
                        })
                    }
                    Some(TransportEvent::Disconnected { message, retryable }) => {
                        update(&state, &repaint, |s| {
                            s.microphone_permission = crate::microphone::status()
                        });
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
                        update(&state, &repaint, |s| {
                            s.microphone_permission = crate::microphone::status()
                        });
                        retire_voice_attempt(id, &mut join_generation, &gate);
                        transport.take();
                        if let Some(task) = voice_task.take() {
                            task.abort();
                        }
                        recovery.device_failure = true;
                        let retrying = signal_ready
                            && crate::microphone::status().usable()
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
                    if let Some(task) = ring_task.take() {
                        task.abort();
                    }
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
                            snapshot(&state).voice_handshake,
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
fn saved_login_rejected(from_saved: bool, error: &anyhow::Error) -> bool {
    from_saved
        && error
            .downcast_ref::<crate::account::AuthenticationRejected>()
            .is_some()
}
fn voice_flags_authorized(recovery: &CallRecovery, pending: Option<&PendingVoice>) -> bool {
    matches!((&recovery.info, pending), (Some(info), Some(p)) if info.guild_id == (!p.private).then_some(p.guild) && info.channel_id == p.channel && p.session.as_deref() == Some(info.session_id.as_str()))
}
fn may_send_voice_flags(
    signaling_ready: bool,
    voice_active: bool,
    authority_current: bool,
) -> bool {
    signaling_ready && voice_active && authority_current
}
fn apply_confirmed_voice_state(
    state: &Shared,
    repaint: &Repaint,
    gate: &TxGate,
    data: &serde_json::Value,
    transport_present: bool,
) -> bool {
    let confirmed_mute = data["self_mute"].as_bool();
    let confirmed_deaf = data["self_deaf"].as_bool();
    let admin_deaf = data["deaf"].as_bool().unwrap_or(false);
    let suppressed = data["mute"].as_bool().unwrap_or(false)
        || admin_deaf
        || data["suppress"].as_bool().unwrap_or(false);
    gate.set_remote_muted(confirmed_mute != Some(false) || confirmed_deaf != Some(false));
    gate.set_deafened(snapshot(state).deafened || confirmed_deaf != Some(false) || admin_deaf);
    gate.set_suppressed(suppressed || !transport_present);
    update(state, repaint, |s| {
        s.confirmed_muted = confirmed_mute;
        s.confirmed_deafened = confirmed_deaf;
        s.server_suppressed = suppressed;
        s.server_deafened = admin_deaf;
    });
    suppressed
}
fn publish_roster_count(
    state: &Shared,
    pending: Option<&PendingVoice>,
    roster: &HashMap<(u64, u64), (u64, Participant)>,
) {
    let s = snapshot(state);
    let count = pending.filter(|p| p.roster_complete).and_then(|p| {
        let own = s.account.as_ref()?.id;
        if p.private {
            s.participants
                .iter()
                .any(|u| u.id == own)
                .then_some(s.participants.len())
        } else {
            (roster
                .get(&(p.guild, own))
                .is_some_and(|(channel, _)| *channel == p.channel))
            .then(|| {
                roster
                    .iter()
                    .filter(|((g, _), (c, _))| *g == p.guild && *c == p.channel)
                    .count()
            })
        }
    });
    s.voice_handshake.set_roster_count(count);
}
fn apply_social_snapshot(
    state: &mut UiState,
    friends: Result<Vec<crate::social::Friend>>,
    channels: Result<crate::social::DirectChannelsSnapshot>,
) {
    state.social_busy = false;
    let mut notices = Vec::new();
    match friends {
        Ok(friends) => state.friends = friends,
        Err(error) => {
            state.friends.clear();
            notices.push(format!("Friends could not load: {error}"));
        }
    }
    match channels {
        Ok(snapshot) => {
            state.direct_channels = snapshot.channels;
            if snapshot.skipped > 0 {
                notices.push(format!(
                    "{} unsupported conversations hidden.",
                    snapshot.skipped
                ));
            }
        }
        Err(error) => {
            state.direct_channels.clear();
            notices.push(format!("Conversations could not load: {error}"));
        }
    }
    for f in state.friends.iter().chain(
        state
            .direct_channels
            .iter()
            .flat_map(|c| c.recipients.iter()),
    ) {
        if state.profiles.len() < 2048 || state.profiles.contains_key(&f.id) {
            let p = state.profiles.entry(f.id).or_default();
            if f.avatar.is_some() {
                p.avatar = f.avatar.clone();
            }
        }
    }
    crate::social::sort_by_activity(&mut state.friends, &state.direct_channels);
    state.social_status = notices.join(" ");
}
fn chat_channel_allowed(s: &UiState, channel: u64) -> bool {
    if s.selected_guild.is_none() {
        s.selected_dm == Some(channel) && s.direct_channels.iter().any(|c| c.id == channel)
    } else {
        s.text_channels
            .iter()
            .any(|c| c.id == channel && Some(c.guild_id) == s.selected_guild)
    }
}
fn voice_command(
    p: &PendingVoice,
    channel_id: Option<u64>,
    muted: bool,
    deafened: bool,
) -> GatewayCommand {
    if p.private {
        GatewayCommand::PrivateVoice {
            channel_id,
            muted,
            deafened,
        }
    } else {
        GatewayCommand::Voice {
            guild_id: p.guild,
            channel_id,
            muted,
            deafened,
        }
    }
}
fn voice_event_matches(data: &serde_json::Value, p: &PendingVoice) -> bool {
    if p.private {
        data["guild_id"].is_null()
            && (data["channel_id"].is_null()
                || account::snowflake(&data["channel_id"]) == Some(p.channel))
    } else {
        account::snowflake(&data["guild_id"]) == Some(p.guild)
    }
}
fn voice_state_matches(data: &serde_json::Value, p: &PendingVoice, own: Option<u64>) -> bool {
    voice_event_matches(data, p)
        || own.is_some_and(|id| account::snowflake(&data["user_id"]) == Some(id))
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
            .send(voice_command(
                p,
                Some(p.channel),
                crate::calls::gateway_mute(&s),
                s.deafened,
            ))
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
    if p.private {
        return kind == "CHANNEL_DELETE" && account::snowflake(&data["id"]) == Some(p.channel);
    }
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
    fn only_saved_auth_rejection_revokes_persistence_without_secret_diagnostics() {
        let rejected: anyhow::Error = crate::account::AuthenticationRejected.into();
        assert!(saved_login_rejected(true, &rejected));
        assert!(!saved_login_rejected(false, &rejected));
        assert!(!saved_login_rejected(
            true,
            &anyhow::anyhow!("Network unavailable")
        ));
        assert!(!saved_login_rejected(
            true,
            &anyhow::anyhow!("Channel access denied")
        ));
        assert_eq!(
            rejected.to_string(),
            "Discord rejected this login. Sign in again."
        );
        assert_eq!(
            format!("{:?}", crate::account::AuthenticationRejected),
            "AuthenticationRejected"
        );
    }
    #[test]
    fn own_confirmed_mute_and_admin_deafen_close_capture_independently_of_intent() {
        let state = Arc::new(Mutex::new(UiState {
            phase: Phase::VoiceReady,
            muted: false,
            ..Default::default()
        }));
        let gate = TxGate::default();
        gate.set_muted(false);
        gate.set_suppressed(false);
        apply_confirmed_voice_state(
            &state,
            &None,
            &gate,
            &json!({"self_mute":true,"self_deaf":false}),
            true,
        );
        assert!(!gate.transmit_allowed());
        assert!(!snapshot(&state).muted);
        apply_confirmed_voice_state(
            &state,
            &None,
            &gate,
            &json!({"self_mute":false,"self_deaf":false,"deaf":true}),
            true,
        );
        assert!(!gate.transmit_allowed());
        assert!(gate.is_deafened());
        assert!(snapshot(&state).server_suppressed);
        apply_confirmed_voice_state(
            &state,
            &None,
            &gate,
            &json!({"self_mute":false,"self_deaf":false}),
            true,
        );
        assert!(gate.transmit_allowed());
        assert!(!gate.is_deafened());
        apply_confirmed_voice_state(&state, &None, &gate, &json!({}), true);
        assert!(!gate.transmit_allowed());
    }
    #[test]
    fn initial_mute_sync_needs_owned_session_not_recovery_device_pins() {
        let (mut recovery, mut pending) = authorized_recovery();
        recovery.config = None;
        assert!(voice_flags_authorized(&recovery, Some(&pending)));
        assert!(!recovery.authorized(Some(&pending)));
        pending.session = Some("another-session".into());
        assert!(!voice_flags_authorized(&recovery, Some(&pending)));
    }
    #[test]
    fn idle_voice_requires_complete_roster_and_own_matching_channel() {
        let state = Arc::new(Mutex::new(UiState {
            account: Some(Account {
                id: 1,
                name: "Self".into(),
                avatar: None,
            }),
            ..Default::default()
        }));
        let mut pending = PendingVoice {
            guild: 10,
            channel: 20,
            ..Default::default()
        };
        let person = |id| Participant {
            id,
            name: "Member".into(),
            speaking: false,
            muted: true,
            deafened: false,
        };
        let mut roster = HashMap::from([((10, 1), (20, person(1)))]);
        publish_roster_count(&state, Some(&pending), &roster);
        assert!(!snapshot(&state).voice_handshake.idle_without_peer());
        pending.roster_complete = true;
        publish_roster_count(&state, Some(&pending), &roster);
        assert!(snapshot(&state).voice_handshake.idle_without_peer());
        roster.insert((10, 2), (20, person(2)));
        publish_roster_count(&state, Some(&pending), &roster);
        assert!(!snapshot(&state).voice_handshake.idle_without_peer());
        roster.remove(&(10, 2));
        roster.insert((10, 1), (21, person(1)));
        publish_roster_count(&state, Some(&pending), &roster);
        assert!(!snapshot(&state).voice_handshake.idle_without_peer());
    }
    #[test]
    fn private_call_signaling_has_no_guild_and_rejects_other_channel() {
        let pending = PendingVoice {
            private: true,
            guild: 20,
            channel: 20,
            ..Default::default()
        };
        assert!(matches!(
            voice_command(&pending, Some(20), true, false),
            GatewayCommand::PrivateVoice {
                channel_id: Some(20),
                ..
            }
        ));
        assert!(voice_event_matches(
            &json!({"guild_id":null,"channel_id":"20"}),
            &pending
        ));
        assert!(!voice_event_matches(
            &json!({"guild_id":"20","channel_id":"20"}),
            &pending
        ));
        assert!(!voice_event_matches(
            &json!({"guild_id":null,"channel_id":"21"}),
            &pending
        ));
    }
    #[test]
    fn own_voice_move_is_observed_even_outside_private_call_scope() {
        let pending = PendingVoice {
            guild: 20,
            private: true,
            channel: 20,
            ..Default::default()
        };
        let moved = json!({"guild_id":"99","channel_id":"21","user_id":"42"});
        assert!(voice_state_matches(&moved, &pending, Some(42)));
        assert!(!voice_state_matches(&moved, &pending, Some(43)));
        assert!(!voice_state_matches(&moved, &pending, None));
    }
    #[test]
    fn failed_conversation_fetch_preserves_independently_loaded_friends() {
        let mut state = UiState {
            social_busy: true,
            ..Default::default()
        };
        apply_social_snapshot(
            &mut state,
            Ok(vec![crate::social::Friend {
                id: 20,
                avatar: None,
                name: "Friend".into(),
            }]),
            Err(anyhow::anyhow!("Unavailable")),
        );
        assert_eq!(state.friends[0].id, 20);
        assert!(state.direct_channels.is_empty());
        assert!(!state.social_busy);
        assert!(state.social_status.contains("Conversations could not load"));
        apply_social_snapshot(
            &mut state,
            Err(anyhow::anyhow!("Unavailable")),
            Ok(crate::social::DirectChannelsSnapshot {
                channels: vec![crate::social::DirectChannel {
                    id: 30,
                    last_message_id: None,
                    name: "DM".into(),
                    recipients: vec![crate::social::Friend {
                        id: 40,
                        avatar: None,
                        name: "Recipient".into(),
                    }],
                }],
                skipped: 1,
            }),
        );
        assert!(state.friends.is_empty());
        assert_eq!(state.direct_channels[0].id, 30);
        assert!(
            state
                .social_status
                .contains("1 unsupported conversations hidden")
        );
    }
    #[test]
    fn dm_sends_require_selected_known_private_channel() {
        let mut state = UiState {
            selected_dm: Some(20),
            direct_channels: vec![crate::social::DirectChannel {
                id: 20,
                last_message_id: None,
                name: "Friend".into(),
                recipients: vec![],
            }],
            ..Default::default()
        };
        assert!(chat_channel_allowed(&state, 20));
        assert!(!chat_channel_allowed(&state, 21));
        state.selected_guild = Some(20);
        assert!(!chat_channel_allowed(&state, 20));
        state.selected_guild = None;
        state.selected_dm = None;
        assert!(!chat_channel_allowed(&state, 20));
    }
    #[test]
    fn unrelated_permission_events_do_not_interrupt() {
        let p = PendingVoice {
            guild: 1,
            private: false,
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
            guild_id: Some(1),
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
                private: false,
                channel: 2,
                session: Some("test-session".into()),
                server: None,
                allow_initial_connect: false,
                ring_pending: false,
                roster_complete: false,
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
                message_type: 0,
                call: None,
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
