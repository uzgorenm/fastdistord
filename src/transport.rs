//! Discord voice transport. All encryption/MLS/Opus is delegated to Songbird and davey.
//! The small vendored Songbird patch is mandatory: upstream's DriverConnect does not
//! imply DAVE readiness, and upstream permits transport-only audio during negotiation.
use crate::audio::{AudioConfig, AudioEngine, PlaybackSink, TxGate};
use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use songbird::{
    Config, ConnectionInfo, CoreEvent, Driver, Event, EventContext, EventHandler,
    driver::{Channels, DecodeConfig, DecodeMode, MixMode, SampleRate},
    input::{AudioStream, Input, LiveInput, RawAdapter},
};
use std::{
    collections::{HashMap, HashSet},
    fmt,
    num::NonZeroU64,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::mpsc::UnboundedSender,
    task::JoinHandle,
    time::{Instant, interval, timeout},
};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
const DAVE_TIMEOUT: Duration = Duration::from_secs(15);
const DISARMED_EPOCH: u64 = u64::MAX;

/// The endpoint/token/session must come from this user's current Gateway session.
#[derive(Clone)]
pub struct VoiceConnection {
    pub guild_id: u64,
    pub channel_id: u64,
    pub user_id: u64,
    pub session_id: String,
    pub endpoint: String,
    pub token: String,
}

impl fmt::Debug for VoiceConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VoiceConnection")
            .field("guild_id", &self.guild_id)
            .field("channel_id", &self.channel_id)
            .field("user_id", &self.user_id)
            .field("session_id", &"[redacted]")
            .field("endpoint", &"[redacted]")
            .field("token", &"[redacted]")
            .finish()
    }
}

#[derive(Clone, Debug)]
pub enum TransportEvent {
    /// Both the voice connection and negotiated DAVE session are ready.
    Ready,
    Disconnected {
        message: String,
        retryable: bool,
    },
    Speaking {
        user_id: u64,
        speaking: bool,
    },
    DeviceFailure(String),
}

/// Distinct from protocol/authentication errors; only a previously authorized,
/// pinned-device recovery episode may retry this failure.
#[derive(Debug)]
pub struct AudioOpenFailure;
impl fmt::Display for AudioOpenFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Could not reopen the selected microphone or speaker")
    }
}
impl std::error::Error for AudioOpenFailure {}

#[derive(Debug)]
pub struct TransientVoiceFailure;
impl fmt::Display for TransientVoiceFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Transient voice connection failure")
    }
}
impl std::error::Error for TransientVoiceFailure {}

/// Owns a single call. Dropping it closes privacy gates before stopping its workers.
pub struct Transport {
    driver: Driver,
    audio: Arc<AudioEngine>,
    gate: Arc<TxGate>,
    alive: Arc<AtomicBool>,
    monitor: Option<JoinHandle<()>>,
}

impl Transport {
    pub async fn connect(
        info: VoiceConnection,
        gate: Arc<TxGate>,
        config: AudioConfig,
        events: UnboundedSender<TransportEvent>,
        expected_session: u64,
    ) -> Result<Self> {
        if gate.session() != expected_session {
            bail!("Voice connection was superseded");
        }
        let connection = checked_connection(info)?;
        let armed_epoch = Arc::new(AtomicU64::new(DISARMED_EPOCH));
        let dave_ready = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        let audio_health: Arc<OnceLock<Arc<AudioEngine>>> = Arc::new(OnceLock::new());
        let mut voice_config = Config::default()
            .decode_mode(DecodeMode::Decode(DecodeConfig::new(
                Channels::Stereo,
                SampleRate::Hz48000,
            )))
            .mix_mode(MixMode::Mono);
        voice_config.require_dave = true;
        voice_config.dave_ready = dave_ready.clone();
        voice_config.driver_timeout = Some(Duration::from_secs(10).into());
        voice_config.packet_gate = Some({
            let gate = gate.clone();
            let armed_epoch = armed_epoch.clone();
            let alive = alive.clone();
            let audio_health = audio_health.clone();
            Arc::new(move || {
                alive.load(Ordering::Acquire)
                    && audio_health.get().is_some_and(|audio| !audio.has_failed())
                    && epoch_allows_transmit(&gate, &armed_epoch, expected_session)
            })
        });
        let mut pending = PendingDriver {
            driver: Driver::new(voice_config),
            armed: true,
        };
        let transient_failure = Arc::new(AtomicBool::new(false));
        let status = DriverStatus {
            transient_failure: transient_failure.clone(),
            alive: alive.clone(),
            events: events.clone(),
        };
        pending
            .driver
            .add_global_event(Event::Core(CoreEvent::DriverDisconnect), status);

        let playback = Arc::new(OnceLock::new());
        let receiver = VoiceReceiver {
            playback: playback.clone(),
            events: events.clone(),
            state: Arc::new(Mutex::new(ReceiveState::default())),
            dave_ready: dave_ready.clone(),
            alive: alive.clone(),
        };
        // Register before connect: Discord can send SSRC mappings during the handshake.
        for event in [
            CoreEvent::SpeakingStateUpdate,
            CoreEvent::VoiceTick,
            CoreEvent::ClientDisconnect,
        ] {
            pending
                .driver
                .add_global_event(Event::Core(event), receiver.clone());
        }
        match timeout(HANDSHAKE_TIMEOUT, pending.driver.connect(connection)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                let reason = songbird::events::context_data::DisconnectReason::from(&error);
                if matches!(
                    reason,
                    songbird::events::context_data::DisconnectReason::Io
                        | songbird::events::context_data::DisconnectReason::TimedOut
                ) {
                    return Err(TransientVoiceFailure.into());
                }
                bail!("Encrypted Discord voice handshake failed; reconnect to try again");
            }
            Err(_) => return Err(TransientVoiceFailure.into()),
        }
        // DriverConnect only means transport negotiation completed. Never expose it as ready.
        timeout(DAVE_TIMEOUT, async {
            while !dave_ready.load(Ordering::Acquire) {
                if !alive.load(Ordering::Acquire) || gate.session() != expected_session {
                    if gate.session() == expected_session
                        && transient_failure.load(Ordering::Acquire)
                    {
                        return Err(TransientVoiceFailure.into());
                    }
                    bail!("Discord voice disconnected before end-to-end encryption was ready");
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok::<(), anyhow::Error>(())
        })
        .await
        .context("DAVE end-to-end encryption did not become ready; microphone stayed closed")??;

        if gate.session() != expected_session {
            bail!("Voice connection was superseded");
        }
        // No audio device is opened until the MLS session has been verified ready.
        let audio =
            Arc::new(AudioEngine::start(config, gate.clone()).map_err(|_| AudioOpenFailure)?);
        let _ = playback.set(audio.playback());
        let _ = audio_health.set(audio.clone());
        if !dave_ready.load(Ordering::Acquire)
            || !alive.load(Ordering::Acquire)
            || gate.session() != expected_session
        {
            bail!("End-to-end encryption changed during audio startup; reconnect to try again");
        }
        let driver = pending.driver.clone();
        // Disarm the failure guard without creating a leaked Driver reference.
        pending.armed = false;
        drop(pending);
        let mut transport = Self {
            driver,
            audio,
            gate,
            alive,
            monitor: None,
        };
        transport.monitor = Some(spawn_monitor(
            transport.driver.clone(),
            MonitorState {
                expected_session,
                audio: transport.audio.clone(),
                gate: transport.gate.clone(),
                alive: transport.alive.clone(),
                dave_ready,
                armed: armed_epoch,
                events: events.clone(),
            },
        ));
        let _ = events.send(TransportEvent::Ready);
        Ok(transport)
    }

    pub fn mute(&mut self, muted: bool) {
        self.gate.set_muted(muted);
        self.driver.mute(muted || self.gate.is_deafened());
    }

    pub fn deafen(&mut self, deafened: bool) {
        self.gate.set_deafened(deafened);
        self.driver.mute(deafened || self.gate.is_muted());
    }

    pub fn set_output_volume(&mut self, volume: f32) {
        self.audio.set_output_volume(volume);
    }

    pub fn device_config(&self) -> AudioConfig {
        self.audio.device_config()
    }
    pub fn diagnostics(&self) -> String {
        let stats = self.audio.stats();
        format!(
            "Input {} Hz · Output {} Hz · Suppressed {} · Capture overruns {} · Playback overruns {} · Playback underruns {}",
            self.audio.input_sample_rate,
            self.audio.output_sample_rate,
            self.gate.is_suppressed(),
            stats.capture_overruns,
            stats.playback_overruns,
            stats.playback_underruns
        )
    }

    pub fn meter(&self) -> f32 {
        self.audio.meter()
    }

    pub fn shutdown(&mut self) {
        self.alive.store(false, Ordering::Release);
        self.audio.stop();
        self.driver.mute(true);
        self.driver.stop();
        self.driver.leave();
        if let Some(monitor) = self.monitor.take() {
            monitor.abort();
        }
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// A cancelled/failed connection future must not leave background voice tasks connected.
struct PendingDriver {
    driver: Driver,
    armed: bool,
}
impl Drop for PendingDriver {
    fn drop(&mut self) {
        if self.armed {
            self.driver.mute(true);
            self.driver.stop();
            self.driver.leave();
        }
    }
}

fn epoch_allows_transmit(gate: &TxGate, armed: &AtomicU64, expected_session: u64) -> bool {
    let before = gate.epoch();
    gate.session() == expected_session
        && gate.transmit_allowed()
        && before == armed.load(Ordering::Acquire)
        && before == gate.epoch()
        && gate.session() == expected_session
}

struct MonitorState {
    expected_session: u64,
    audio: Arc<AudioEngine>,
    gate: Arc<TxGate>,
    alive: Arc<AtomicBool>,
    dave_ready: Arc<AtomicBool>,
    armed: Arc<AtomicU64>,
    events: UnboundedSender<TransportEvent>,
}

fn spawn_monitor(mut driver: Driver, state: MonitorState) -> JoinHandle<()> {
    let MonitorState {
        expected_session,
        audio,
        gate,
        alive,
        dave_ready,
        armed,
        events,
    } = state;
    tokio::spawn(async move {
        let mut tick = interval(Duration::from_millis(10));
        let mut installed_epoch = DISARMED_EPOCH;
        let mut negotiating_since = None;
        while alive.load(Ordering::Acquire) && gate.session() == expected_session {
            tick.tick().await;
            if audio.has_failed() {
                let _ = events.send(TransportEvent::DeviceFailure(
                    "An audio device stopped; reselect the microphone or speaker and reconnect"
                        .into(),
                ));
                break;
            }
            if !dave_ready.load(Ordering::Acquire) {
                armed.store(DISARMED_EPOCH, Ordering::Release);
                installed_epoch = DISARMED_EPOCH;
                driver.mute(true);
                driver.stop();
                let since = negotiating_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= DAVE_TIMEOUT {
                    let _ = events.send(TransportEvent::Disconnected {
                        message:
                            "DAVE end-to-end encryption could not be renewed; microphone closed"
                                .into(),
                        retryable: false,
                    });
                    break;
                }
                continue;
            }
            negotiating_since = None;
            let epoch = gate.epoch();
            if epoch != installed_epoch {
                // The packet callback has already blocked the old epoch synchronously.
                // Replace the track to discard Symphonia/Opus read-ahead, not just the capture ring.
                armed.store(DISARMED_EPOCH, Ordering::Release);
                driver.mute(true);
                driver.stop();
                installed_epoch = epoch;
                if !gate.transmit_allowed() {
                    continue;
                }
                // Songbird disposes stopped tracks off-thread. Wait for its old
                // reader lease to end before sharing the capture consumer again.
                let reader = timeout(Duration::from_secs(2), async {
                    loop {
                        if let Ok(reader) = audio.input_reader() {
                            break reader;
                        }
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await;
                let input = match reader {
                    Ok(reader) => Input::Live(
                        LiveInput::Raw(AudioStream {
                            input: Box::new(RawAdapter::new(reader, 48_000, 1)),
                        }),
                        None,
                    ),
                    Err(_) => {
                        let _ = events.send(TransportEvent::DeviceFailure(
                            "Microphone input could not be restarted safely".into(),
                        ));
                        break;
                    }
                };
                let track = driver.play_only_input(input);
                let arm = armed.clone();
                // This action runs on the mixer, after the old track has been replaced.
                // It cannot arm an old buffered frame because every epoch owns a fresh track.
                if track
                    .action(move |_| {
                        arm.store(epoch, Ordering::Release);
                        None
                    })
                    .is_err()
                {
                    let _ = events.send(TransportEvent::DeviceFailure(
                        "Microphone encoder stopped; reconnect to try again".into(),
                    ));
                    break;
                }
                driver.mute(false);
            }
        }
        armed.store(DISARMED_EPOCH, Ordering::Release);
        audio.stop();
        alive.store(false, Ordering::Release);
        driver.mute(true);
        driver.stop();
        driver.leave();
    })
}

#[derive(Clone)]
struct DriverStatus {
    transient_failure: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    events: UnboundedSender<TransportEvent>,
}
#[async_trait]
impl EventHandler for DriverStatus {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if let EventContext::DriverDisconnect(data) = ctx
            && self.alive.load(Ordering::Acquire)
        {
            // Preserve terminal/admin ambiguity. Only explicit I/O/timeout errors
            // qualify for coordinator recovery; never stringify secret context.
            let retryable = matches!(
                data.reason,
                Some(
                    songbird::events::context_data::DisconnectReason::Io
                        | songbird::events::context_data::DisconnectReason::TimedOut
                )
            );
            self.transient_failure.store(retryable, Ordering::Release);
            if !self.alive.swap(false, Ordering::AcqRel) {
                return None;
            }
            let _ = self.events.send(TransportEvent::Disconnected {
                message: "Discord voice disconnected.".into(),
                retryable,
            });
        }
        None
    }
}

#[derive(Default)]
struct ReceiveState {
    users: HashMap<u32, u64>,
    speaking: HashSet<u64>,
    last_heard: HashMap<u64, Instant>,
}
#[derive(Clone)]
struct VoiceReceiver {
    playback: Arc<OnceLock<PlaybackSink>>,
    events: UnboundedSender<TransportEvent>,
    state: Arc<Mutex<ReceiveState>>,
    dave_ready: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
}
#[async_trait]
impl EventHandler for VoiceReceiver {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if !self.alive.load(Ordering::Acquire) {
            return None;
        }
        let Ok(mut state) = self.state.lock() else {
            return None;
        };
        match ctx {
            EventContext::SpeakingStateUpdate(update) => {
                if let Some(user) = update.user_id
                    && (state.users.len() < 256 || state.users.contains_key(&update.ssrc))
                {
                    state.users.insert(update.ssrc, user.0);
                }
            }
            EventContext::VoiceTick(tick) => {
                let now = Instant::now();
                if self.dave_ready.load(Ordering::Acquire) {
                    for (ssrc, voice) in &tick.speaking {
                        if let (Some(&user_id), Some(pcm)) =
                            (state.users.get(ssrc), voice.decoded_voice.as_ref())
                            && !pcm.is_empty()
                        {
                            if let Some(playback) = self.playback.get() {
                                playback.push_pcm(user_id, pcm, 2);
                            }
                            if pcm.iter().any(|sample| sample.unsigned_abs() > 80) {
                                state.last_heard.insert(user_id, now);
                            }
                        }
                    }
                    state
                        .last_heard
                        .retain(|_, heard| now.duration_since(*heard) < Duration::from_millis(160));
                } else {
                    state.last_heard.clear();
                }
                let speaking: HashSet<u64> = state.last_heard.keys().copied().collect();
                for &user_id in speaking.difference(&state.speaking) {
                    let _ = self.events.send(TransportEvent::Speaking {
                        user_id,
                        speaking: true,
                    });
                }
                for &user_id in state.speaking.difference(&speaking) {
                    let _ = self.events.send(TransportEvent::Speaking {
                        user_id,
                        speaking: false,
                    });
                }
                state.speaking = speaking;
            }
            EventContext::ClientDisconnect(update) => {
                let user_id = update.user_id.0;
                state.users.retain(|_, id| *id != user_id);
                state.last_heard.remove(&user_id);
                if state.speaking.remove(&user_id) {
                    let _ = self.events.send(TransportEvent::Speaking {
                        user_id,
                        speaking: false,
                    });
                }
            }
            _ => {}
        }
        None
    }
}

fn checked_connection(info: VoiceConnection) -> Result<ConnectionInfo> {
    let nz = |id| {
        NonZeroU64::new(id)
            .ok_or_else(|| anyhow!("Voice connection contains an invalid Discord ID"))
    };
    let endpoint = info
        .endpoint
        .strip_prefix("wss://")
        .unwrap_or(&info.endpoint)
        .trim_end_matches(":443");
    if endpoint.is_empty()
        || endpoint.len() > 253
        || !endpoint
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        || !(endpoint.ends_with(".discord.media") || endpoint.ends_with(".discord.gg"))
    {
        bail!("Gateway supplied an invalid Discord voice endpoint");
    }
    if info.session_id.is_empty() || info.token.is_empty() {
        bail!("Voice credentials are incomplete");
    }
    Ok(ConnectionInfo {
        channel_id: nz(info.channel_id)?.into(),
        guild_id: nz(info.guild_id)?.into(),
        user_id: nz(info.user_id)?.into(),
        endpoint: endpoint.to_owned(),
        session_id: info.session_id,
        token: info.token,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn credentials() -> VoiceConnection {
        VoiceConnection {
            guild_id: 1,
            channel_id: 2,
            user_id: 3,
            session_id: "private-session".into(),
            endpoint: "wss://voice.discord.media:443".into(),
            token: "private-token".into(),
        }
    }
    #[test]
    fn connection_debug_never_exposes_credentials() {
        let text = format!("{:?}", credentials());
        assert!(!text.contains("private-session"));
        assert!(!text.contains("private-token"));
    }
    #[test]
    fn connection_retains_real_channel_id() {
        let info = checked_connection(credentials()).unwrap();
        assert_eq!(info.channel_id.0.get(), 2);
        assert_eq!(info.endpoint, "voice.discord.media");
    }
    #[test]
    fn rejects_credential_forwarding_endpoints_without_echoing_them() {
        for endpoint in [
            "attacker.example",
            "discord.media.attacker.example",
            "private-token@voice.discord.media",
            "voice.discord.media/private-token",
        ] {
            let mut info = credentials();
            info.endpoint = endpoint.into();
            let error = checked_connection(info).unwrap_err().to_string();
            assert!(!error.contains("private-token"));
        }
    }
    #[test]
    fn invalid_ids_fail_before_network_access() {
        let mut info = credentials();
        info.channel_id = 0;
        assert!(checked_connection(info).is_err());
    }
    #[test]
    fn epoch_change_revokes_prebuffered_microphone_frames() {
        let gate = TxGate::default();
        gate.set_muted(false);
        gate.set_suppressed(false);
        gate.set_ptt_enabled(false);
        let armed = AtomicU64::new(gate.epoch());
        assert!(epoch_allows_transmit(&gate, &armed, gate.session()));
        gate.set_muted(true);
        assert!(!epoch_allows_transmit(&gate, &armed, gate.session()));
        gate.set_muted(false);
        assert!(!epoch_allows_transmit(&gate, &armed, gate.session()));
        armed.store(gate.epoch(), Ordering::Release);
        assert!(epoch_allows_transmit(&gate, &armed, gate.session()));
    }
    #[test]
    fn superseded_session_cannot_reuse_an_armed_microphone_epoch() {
        let gate = TxGate::default();
        gate.set_muted(false);
        gate.set_suppressed(false);
        gate.set_ptt_enabled(false);
        let armed = AtomicU64::new(gate.epoch());
        let old_session = gate.session();
        assert!(epoch_allows_transmit(&gate, &armed, old_session));
        gate.begin_session();
        assert!(!epoch_allows_transmit(&gate, &armed, old_session));
    }
}
