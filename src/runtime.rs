use crate::{
    account::{self, Gateway, GatewayCommand, GatewayEvent, PersonalAccount},
    audio::{self, AudioConfig, TxGate},
    model::*,
    transport::{Transport, TransportEvent, VoiceConnection},
};
use anyhow::Result;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, mpsc},
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
enum ResultEvent {
    Account(
        u64,
        Result<(PersonalAccount, Account, Vec<Guild>)>,
        Option<zeroize::Zeroizing<String>>,
    ),
    Channels(u64, u64, Result<Vec<Channel>>),
    Transport(u64, Result<Transport>),
}
#[derive(Default)]
struct PendingVoice {
    guild: u64,
    channel: u64,
    session: Option<String>,
    server: Option<(String, String)>,
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
    let (results_tx, mut results) = async_mpsc::channel(8);
    let (voice_tx, mut voice_events) = async_mpsc::unbounded_channel();
    let mut meter_tick = tokio::time::interval(std::time::Duration::from_millis(100));
    enum Incoming {
        Command(Option<Command>),
        Result(Option<ResultEvent>),
        Gateway(Option<GatewayEvent>),
        Voice(Option<(u64, TransportEvent)>),
        Meter,
    }
    loop {
        let incoming = tokio::select! {
            command=commands.recv()=>Incoming::Command(command),
            event=results.recv()=>Incoming::Result(event),
            event=async {if let Some(g)=&mut gateway{g.events.recv().await}else{std::future::pending().await}}=>Incoming::Gateway(event),
            event=voice_events.recv()=>Incoming::Voice(event),
            _=meter_tick.tick(),if transport.is_some()&&ui_visible&&!gate.is_muted()&&!gate.is_deafened()=>Incoming::Meter,
        };
        match incoming {
            Incoming::Command(command) => {
                let Some(command) = command else {
                    break;
                };
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
                        if let Some(c) = client.clone() {
                            let tx = results_tx.clone();
                            let id = generation;
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
                        send_voice_flags(&gateway, &pending, &state).await;
                    }
                    Command::SetDeafened(value) => {
                        gate.set_deafened(value);
                        if let Some(t) = &mut transport {
                            t.deafen(value);
                        }
                        update(&state, &repaint, |s| s.deafened = value);
                        send_voice_flags(&gateway, &pending, &state).await;
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
                        gate.set_suppressed(true);
                        update(&state, &repaint, |s| {
                            s.phase = Phase::Failed;
                            s.status = error.to_string();
                        });
                    }
                },
                _ => {}
            },
            Incoming::Gateway(event) => match event {
                Some(GatewayEvent::Ready) => update(&state, &repaint, |s| {
                    s.phase = Phase::SignalingReady;
                    s.status = "Account connected. Choose a server.".into();
                }),
                Some(GatewayEvent::Closed(message)) => {
                    gate.set_suppressed(true);
                    transport.take();
                    join_generation = join_generation.wrapping_add(1);
                    gate.begin_session();
                    if let Some(task) = voice_task.take() {
                        task.abort();
                    }
                    pending = None;
                    gateway = None;
                    update(&state, &repaint, |s| {
                        s.phase = Phase::Failed;
                        s.status = message;
                        s.participants.clear();
                    });
                }
                Some(GatewayEvent::Dispatch { kind, data }) => {
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
                                p.session = data["session_id"].as_str().map(str::to_owned);
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
                    if transport.is_none()
                        && voice_task.as_ref().is_none_or(|t| t.is_finished())
                        && let Some(p) = &mut pending
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
                        let gate = gate.clone();
                        let tx = results_tx.clone();
                        let events = voice_tx.clone();
                        let id = join_generation;
                        let expected_session = gate.session();
                        voice_task = Some(tokio::spawn(async move {
                            let (event_tx, mut event_rx) = async_mpsc::unbounded_channel();
                            tokio::spawn(async move {
                                while let Some(event) = event_rx.recv().await {
                                    if events.send((id, event)).is_err() {
                                        break;
                                    }
                                }
                            });
                            let result =
                                Transport::connect(info, gate, config, event_tx, expected_session)
                                    .await;
                            let _ = tx.send(ResultEvent::Transport(id, result)).await;
                        }));
                    }
                }
                None => {
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
                    Some(TransportEvent::Disconnected(message))
                    | Some(TransportEvent::DeviceFailure(message)) => {
                        gate.set_suppressed(true);
                        transport.take();
                        update(&state, &repaint, |s| {
                            s.phase = Phase::Failed;
                            s.status = message;
                            s.input_level = 0.0;
                        });
                    }
                    None => {}
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
    if let Some(t) = connect_task {
        t.abort();
    }
    if let Some(t) = voice_task {
        t.abort();
    }
    drop(transport);
    drop(gateway);
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
}
