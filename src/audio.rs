//! Local, full-duplex audio. Devices are opened only by `AudioEngine::start`.
//!
//! CPAL callbacks never allocate, block, log, or acquire a mutex. Bounded SPSC
//! queues separate them from a dedicated DSP worker and Songbird's decoder.
//! Rubato provides band-limited rate conversion. Optional noise suppression and
//! digital gain run on the worker. Echo cancellation is unavailable.

use std::{
    collections::VecDeque,
    io::{self, Read, Seek, SeekFrom},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use cpal::{
    FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig, SupportedStreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use rtrb::{Consumer, Producer, RingBuffer};
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use symphonia_core::io::MediaSource;
use zeroize::Zeroize;

use crate::audio_processing::{ProcessingOptions, Processor};

pub const VOICE_SAMPLE_RATE: u32 = 48_000;
const BLOCK_FRAMES: usize = 480; // 10 ms of Discord PCM.
const MAX_USERS: usize = 32;
const MAX_USER_VOLUMES: usize = 128;
const USER_QUEUE_FRAMES: usize = 4_800; // 100 ms per speaker.
const CAPTURE_QUEUE_FRAMES: usize = 4_800;
const MAX_PACKET_FRAMES: usize = 5_760; // Opus's maximum 120 ms packet.
const MAX_CHANNELS: usize = 32;
const MUTED: u64 = 1;
const DEAFENED: u64 = 2;
const SUPPRESSED: u64 = 4;
const PTT_ENABLED: u64 = 8;
const PTT_DOWN: u64 = 16;
const PTT_KNOWN: u64 = 32;
const ENCRYPTION_PENDING: u64 = 64;
const REMOTE_MUTED: u64 = 128;
const FLAGS_MASK: u64 = 0xff;
const TX_GENERATION_MASK: u64 = ((1_u64 << 28) - 1) << 8;
const RX_GENERATION_MASK: u64 = ((1_u64 << 28) - 1) << 36;

/// The transmit decision has one atomic linearization point, independent of UI
/// rendering. Unknown PTT state fails closed. Every change invalidates queued
/// capture, including a mute/unmute cycle between two audio callbacks.
#[derive(Debug)]
pub struct TxGate {
    state: AtomicU64,
    session_generation: AtomicU64,
}

impl Default for TxGate {
    fn default() -> Self {
        Self {
            state: AtomicU64::new(MUTED | SUPPRESSED),
            session_generation: AtomicU64::new(0),
        }
    }
}

#[derive(Clone, Copy)]
struct GateSnapshot(u64);

impl GateSnapshot {
    fn allowed(self) -> bool {
        self.0 & (MUTED | DEAFENED | SUPPRESSED | ENCRYPTION_PENDING | REMOTE_MUTED) == 0
            && (self.0 & PTT_ENABLED == 0
                || self.0 & (PTT_DOWN | PTT_KNOWN) == PTT_DOWN | PTT_KNOWN)
    }

    fn receive_epoch(self) -> u64 {
        self.0 & RX_GENERATION_MASK
    }
    fn deafened(self) -> bool {
        self.0 & DEAFENED != 0
    }
}

impl TxGate {
    fn snapshot(&self) -> GateSnapshot {
        GateSnapshot(self.state.load(Ordering::Acquire))
    }

    fn change(&self, f: impl Fn(u64) -> u64) {
        let mut old = self.state.load(Ordering::Acquire);
        loop {
            let flags = f(old & FLAGS_MASK) & FLAGS_MASK;
            if old & FLAGS_MASK == flags {
                return;
            }
            let tx = old.wrapping_add(1 << 8) & TX_GENERATION_MASK;
            let rx = if (old ^ flags) & DEAFENED != 0 {
                old.wrapping_add(1 << 36) & RX_GENERATION_MASK
            } else {
                old & RX_GENERATION_MASK
            };
            match self.state.compare_exchange_weak(
                old,
                tx | rx | flags,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(current) => old = current,
            }
        }
    }

    fn flag(&self, mask: u64, enabled: bool) {
        self.change(|flags| if enabled { flags | mask } else { flags & !mask });
    }

    pub fn set_muted(&self, value: bool) {
        self.flag(MUTED, value);
    }
    pub fn set_deafened(&self, value: bool) {
        self.flag(DEAFENED, value);
    }
    pub fn set_remote_muted(&self, value: bool) {
        self.change(|flags| {
            if value {
                (flags | REMOTE_MUTED) & !(PTT_DOWN | PTT_KNOWN)
            } else {
                flags & !REMOTE_MUTED
            }
        });
    }
    pub fn set_suppressed(&self, value: bool) {
        self.flag(SUPPRESSED, value);
    }
    /// Separate from server suppression: a DAVE transition invalidates capture
    /// and held PTT synchronously without clearing an administrator's gate.
    pub fn set_encryption_pending(&self, pending: bool) {
        self.change(|flags| {
            if pending {
                (flags | ENCRYPTION_PENDING) & !(PTT_DOWN | PTT_KNOWN)
            } else {
                flags & !ENCRYPTION_PENDING
            }
        });
    }
    pub fn encryption_pending(&self) -> bool {
        self.snapshot().0 & ENCRYPTION_PENDING != 0
    }
    pub fn is_muted(&self) -> bool {
        self.snapshot().0 & MUTED != 0
    }
    pub fn is_deafened(&self) -> bool {
        self.snapshot().deafened()
    }
    pub fn is_suppressed(&self) -> bool {
        self.snapshot().0 & SUPPRESSED != 0
    }
    pub fn transmit_allowed(&self) -> bool {
        self.snapshot().allowed()
    }
    /// Opaque generation token. Compare for equality, never order it.
    pub fn epoch(&self) -> u64 {
        self.snapshot().0
    }

    /// Coordinator-only ownership token. Advance when a join attempt or call is
    /// invalidated. Old transports must compare this before touching the gate.
    pub fn begin_session(&self) -> u64 {
        self.session_generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
    }

    pub fn session(&self) -> u64 {
        self.session_generation.load(Ordering::Acquire)
    }

    pub fn set_ptt_enabled(&self, enabled: bool) {
        self.change(|flags| {
            if (flags & PTT_ENABLED != 0) == enabled {
                return flags;
            }
            let flags = flags & !(PTT_DOWN | PTT_KNOWN);
            if enabled {
                flags | PTT_ENABLED
            } else {
                flags & !PTT_ENABLED
            }
        });
    }

    /// `None` is required when the input hook disconnects, loses focus without
    /// reliable global key state, or cannot establish whether the key is held.
    pub fn set_ptt_pressed(&self, pressed: Option<bool>) {
        self.change(|flags| match pressed {
            Some(true) => flags | PTT_DOWN | PTT_KNOWN,
            Some(false) => (flags | PTT_KNOWN) & !PTT_DOWN,
            None => flags & !(PTT_DOWN | PTT_KNOWN),
        });
    }

    /// Publish a shortcut edge only if no privacy gate changed since the
    /// shortcut owner observed it. Never retry against a newer epoch: a held
    /// key must not reopen capture after encryption or remote-mute revocation.
    pub fn set_ptt_pressed_at_epoch(
        &self,
        pressed: Option<bool>,
        expected_epoch: u64,
    ) -> Option<u64> {
        let flags = expected_epoch & FLAGS_MASK;
        let next_flags = match pressed {
            Some(true) => flags | PTT_DOWN | PTT_KNOWN,
            Some(false) => (flags | PTT_KNOWN) & !PTT_DOWN,
            None => flags & !(PTT_DOWN | PTT_KNOWN),
        };
        let next = if flags == next_flags {
            expected_epoch
        } else {
            (expected_epoch.wrapping_add(1 << 8) & TX_GENERATION_MASK)
                | (expected_epoch & RX_GENERATION_MASK)
                | next_flags
        };
        self.state
            .compare_exchange(expected_epoch, next, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| next)
    }

    pub fn fail_closed(&self) {
        self.change(|flags| (flags | MUTED | SUPPRESSED) & !(PTT_DOWN | PTT_KNOWN));
    }
}

#[derive(Clone, Debug)]
pub struct AudioConfig {
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub output_volume: f32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            input_device: None,
            output_device: None,
            output_volume: 1.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub is_input: bool,
    pub is_output: bool,
    pub is_default_input: bool,
    pub is_default_output: bool,
}

/// Enumerates device metadata; does not build or start a recording stream.
pub fn enumerate_devices() -> Result<Vec<DeviceInfo>> {
    let host = cpal::default_host();
    let default_in = host.default_input_device();
    let default_out = host.default_output_device();
    let mut result = Vec::new();
    for device in host.devices().context("Cannot enumerate audio devices")? {
        result.push(DeviceInfo {
            id: device
                .id()
                .context("Cannot identify audio device")?
                .to_string(),
            name: device.to_string(),
            is_input: device.supports_input(),
            is_output: device.supports_output(),
            is_default_input: default_in.as_ref() == Some(&device),
            is_default_output: default_out.as_ref() == Some(&device),
        });
    }
    Ok(result)
}

pub(crate) fn select_device(
    host: &cpal::Host,
    selector: Option<&str>,
    input: bool,
) -> Result<cpal::Device> {
    let kind = if input { "input" } else { "output" };
    let Some(selector) = selector else {
        return (if input {
            host.default_input_device()
        } else {
            host.default_output_device()
        })
        .with_context(|| format!("No default {kind} audio device is available"));
    };
    for device in host.devices()? {
        if !(if input {
            device.supports_input()
        } else {
            device.supports_output()
        }) {
            continue;
        }
        if device.id().is_ok_and(|id| id.to_string() == selector) {
            return Ok(device);
        }
    }
    bail!("Selected {kind} audio device ID is unavailable: {selector}")
}

fn supported_format(format: SampleFormat) -> bool {
    matches!(
        format,
        SampleFormat::I8
            | SampleFormat::I16
            | SampleFormat::I24
            | SampleFormat::I32
            | SampleFormat::I64
            | SampleFormat::U8
            | SampleFormat::U16
            | SampleFormat::U24
            | SampleFormat::U32
            | SampleFormat::U64
            | SampleFormat::F32
            | SampleFormat::F64
    )
}

pub(crate) fn select_config(device: &cpal::Device, input: bool) -> Result<SupportedStreamConfig> {
    // Prefer the OS default for route/channel semantics. A rate conversion is
    // cheaper than accidentally selecting a surround/loopback route.
    let default = if input {
        device.default_input_config()
    } else {
        device.default_output_config()
    };
    if let Some(config) = default
        .ok()
        .filter(|c| supported_format(c.sample_format()) && valid_config(&c.config()))
    {
        return Ok(config);
    }
    let configs: Vec<_> = if input {
        device.supported_input_configs()?.collect()
    } else {
        device.supported_output_configs()?.collect()
    };
    configs
        .into_iter()
        .filter(|c| {
            supported_format(c.sample_format())
                && c.channels() > 0
                && usize::from(c.channels()) <= MAX_CHANNELS
        })
        .filter_map(|c| {
            let min = c.min_sample_rate().max(8_000);
            let max = c.max_sample_rate().min(192_000);
            (min <= max).then(|| c.with_sample_rate(VOICE_SAMPLE_RATE.clamp(min, max)))
        })
        .min_by_key(|c| {
            (
                c.sample_rate().abs_diff(VOICE_SAMPLE_RATE),
                c.channels().abs_diff(if input { 1 } else { 2 }),
            )
        })
        .context("No supported audio configuration (8–192 kHz, 1–32 channels)")
}

fn valid_config(config: &StreamConfig) -> bool {
    (8_000..=192_000).contains(&config.sample_rate)
        && (1..=MAX_CHANNELS).contains(&usize::from(config.channels))
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AudioStats {
    pub capture_overruns: u64,
    pub playback_overruns: u64,
    pub playback_underruns: u64,
}

struct Shared {
    gate: Arc<TxGate>,
    stopped: AtomicBool,
    failed: AtomicBool,
    meter: AtomicU32,
    volume: AtomicU32,
    processing: AtomicU32,
    capture_overruns: AtomicU64,
    playback_overruns: AtomicU64,
    playback_underruns: AtomicU64,
}

impl Shared {
    fn new(gate: Arc<TxGate>, volume: f32) -> Self {
        Self {
            gate,
            stopped: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            meter: AtomicU32::new(0),
            volume: AtomicU32::new(safe_volume(volume).to_bits()),
            processing: AtomicU32::new(0),
            capture_overruns: AtomicU64::new(0),
            playback_overruns: AtomicU64::new(0),
            playback_underruns: AtomicU64::new(0),
        }
    }
    fn fail(&self) {
        // Session-local only. A late callback from an old call must never
        // mutate the global UI gate belonging to a newly connected call.
        self.meter.store(0, Ordering::Relaxed);
        self.failed.store(true, Ordering::Release);
        self.stopped.store(true, Ordering::Release);
    }
}

fn safe_volume(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 2.0)
    } else {
        0.0
    }
}
fn finite_sample(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

#[derive(Clone, Copy, Default)]
struct CaptureSample {
    sample: f32,
    epoch: u64,
}
#[derive(Clone, Copy, Default)]
struct Stereo {
    left: f32,
    right: f32,
}
#[derive(Clone, Copy, Default)]
struct OutputFrame {
    sample: Stereo,
    epoch: u64,
}

fn drain<T: Copy>(consumer: &mut Consumer<T>) {
    let count = consumer.slots();
    if let Ok(chunk) = consumer.read_chunk(count) {
        chunk.commit_all();
    }
}

/// Rubato's buffers and kernels are allocated before any CPAL stream is played.
pub(crate) struct RateConverter {
    inner: Option<SincFixedIn<f32>>,
    pub(crate) input: Vec<Vec<f32>>,
    pub(crate) output: Vec<Vec<f32>>,
    pub(crate) chunk: usize,
}

impl RateConverter {
    pub(crate) fn new(from: u32, to: u32, channels: usize, chunk: usize) -> Result<Self> {
        if from == to {
            return Ok(Self {
                inner: None,
                input: vec![vec![0.0; chunk]; channels],
                output: vec![vec![0.0; chunk]; channels],
                chunk,
            });
        }
        let inner = SincFixedIn::new(
            to as f64 / from as f64,
            1.0,
            SincInterpolationParameters {
                sinc_len: 128,
                f_cutoff: 0.95,
                interpolation: SincInterpolationType::Cubic,
                oversampling_factor: 128,
                window: WindowFunction::BlackmanHarris2,
            },
            chunk,
            channels,
        )
        .context("Cannot initialize sample-rate converter")?;
        let input = inner.input_buffer_allocate(true);
        let output = inner.output_buffer_allocate(true);
        Ok(Self {
            inner: Some(inner),
            input,
            output,
            chunk,
        })
    }

    pub(crate) fn process(&mut self) -> Result<usize> {
        if let Some(inner) = &mut self.inner {
            let (_, written) = inner.process_into_buffer(&self.input, &mut self.output, None)?;
            Ok(written)
        } else {
            for (input, output) in self.input.iter().zip(&mut self.output) {
                output[..self.chunk].copy_from_slice(&input[..self.chunk]);
            }
            Ok(self.chunk)
        }
    }
    fn reset(&mut self) {
        if let Some(inner) = &mut self.inner {
            inner.reset();
        }
        for channel in &mut self.input {
            channel.fill(0.0);
        }
        for channel in &mut self.output {
            channel.fill(0.0);
        }
    }
    fn output_max(&self) -> usize {
        self.output[0].len()
    }
}

impl Drop for RateConverter {
    fn drop(&mut self) {
        // Owned scratch audio is scrubbed. Rubato's private delay history is
        // disposed by its own Drop and has no public zeroization interface.
        for channel in &mut self.input {
            channel.zeroize();
        }
        for channel in &mut self.output {
            channel.zeroize();
        }
    }
}

struct UserBuffer {
    id: Option<u64>,
    samples: VecDeque<Stereo>,
    last_seen: Instant,
}

struct Mixer {
    users: Vec<UserBuffer>,
    volumes: Vec<(u64, f32)>,
    epoch: u64,
}

impl Mixer {
    fn new(epoch: u64) -> Self {
        Self {
            users: (0..MAX_USERS)
                .map(|_| UserBuffer {
                    id: None,
                    samples: VecDeque::with_capacity(USER_QUEUE_FRAMES),
                    last_seen: Instant::now(),
                })
                .collect(),
            epoch,
            volumes: Vec::with_capacity(MAX_USER_VOLUMES),
        }
    }
    fn set_participant_volume(&mut self, user_id: u64, volume: f32) {
        if user_id == 0 {
            return;
        }
        let volume = if volume.is_finite() {
            volume.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if let Some(index) = self.volumes.iter().position(|(id, _)| *id == user_id) {
            if volume == 1.0 {
                self.volumes.swap_remove(index);
            } else {
                self.volumes[index].1 = volume;
            }
        } else if volume != 1.0 && self.volumes.len() < MAX_USER_VOLUMES {
            self.volumes.push((user_id, volume));
        }
    }
    fn clear_for_epoch(&mut self, epoch: u64) {
        if self.epoch == epoch {
            return;
        }
        for user in &mut self.users {
            user.samples.clear();
        }
        self.epoch = epoch;
    }
    fn push(&mut self, user_id: u64, samples: &[i16], channels: usize) -> usize {
        if !(1..=2).contains(&channels) || !samples.len().is_multiple_of(channels) {
            return 0;
        }
        let now = Instant::now();
        let Some(index) = self
            .users
            .iter()
            .position(|u| u.id == Some(user_id))
            .or_else(|| self.users.iter().position(|u| u.id.is_none()))
            .or_else(|| {
                self.users.iter().position(|u| {
                    u.samples.is_empty() && now.duration_since(u.last_seen) > Duration::from_secs(2)
                })
            })
        else {
            return 0;
        };
        let user = &mut self.users[index];
        user.id = Some(user_id);
        user.last_seen = now;
        let count = (samples.len() / channels)
            .min(MAX_PACKET_FRAMES)
            .min(USER_QUEUE_FRAMES - user.samples.len());
        for frame in samples.chunks_exact(channels).take(count) {
            let left = f32::from(frame[0]) / 32_768.0;
            let right = f32::from(frame[channels - 1]) / 32_768.0;
            user.samples.push_back(Stereo { left, right });
        }
        count
    }
    fn mix_into(&mut self, output: &mut [Vec<f32>], frames: usize) {
        for channel in output.iter_mut() {
            channel[..frames].fill(0.0);
        }
        for user in &mut self.users {
            let volume = self
                .volumes
                .iter()
                .find(|(id, _)| Some(*id) == user.id)
                .map_or(1.0, |(_, v)| *v);
            let (left, right) = output.split_at_mut(1);
            for (left, right) in left[0].iter_mut().zip(&mut right[0]).take(frames) {
                let Some(sample) = user.samples.pop_front() else {
                    break;
                };
                *left += sample.left * volume;
                *right += sample.right * volume;
            }
        }
        // Saturate once after summing, avoiding order-dependent clipping.
        for channel in output {
            for sample in &mut channel[..frames] {
                *sample = finite_sample(*sample);
            }
        }
    }
}

/// Songbird's VoiceTick decoded PCM enters here (48 kHz, one or two channels).
/// The lock is confined to network/DSP workers, never the hardware callback.
#[derive(Clone)]
pub struct PlaybackSink {
    mixer: Arc<Mutex<Mixer>>,
    shared: Arc<Shared>,
}

impl PlaybackSink {
    /// Stable Discord user ID, independent of SSRC and mixer slot reuse. Applied
    /// before summation on the DSP worker; hardware callbacks never take this lock.
    pub fn set_participant_volume(&self, user_id: u64, volume: f32) {
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.set_participant_volume(user_id, volume);
        } else {
            self.shared.fail();
        }
    }
    /// Returns accepted *frames*. Excess, invalid formats and new users beyond
    /// the fixed 32-speaker budget are dropped rather than growing memory.
    pub fn push_pcm(&self, user_id: u64, samples: &[i16], channels: usize) -> usize {
        if !(1..=2).contains(&channels) || !samples.len().is_multiple_of(channels) {
            return 0;
        }
        let snapshot = self.shared.gate.snapshot();
        if snapshot.deafened() || self.shared.stopped.load(Ordering::Acquire) {
            return 0;
        }
        let Ok(mut mixer) = self.mixer.lock() else {
            self.shared.fail();
            return 0;
        };
        mixer.clear_for_epoch(snapshot.receive_epoch());
        let accepted = mixer.push(user_id, samples, channels);
        self.shared.playback_overruns.fetch_add(
            (samples.len() / channels - accepted) as u64,
            Ordering::Relaxed,
        );
        accepted
    }

    pub fn set_volume(&self, volume: f32) {
        self.shared
            .volume
            .store(safe_volume(volume).to_bits(), Ordering::Release);
    }
}

struct ReaderShared {
    consumer: Mutex<Consumer<CaptureSample>>,
    leased: AtomicBool,
    shared: Arc<Shared>,
}

/// A nonblocking, non-seekable 48 kHz mono f32-LE stream for Songbird RawAdapter.
/// Reads stop at each 10 ms boundary; underflow becomes silence. Songbird's mixer clock
/// paces consumption. Sleeping here would block its synchronous packet decoder.
/// This does not retract samples already buffered inside Songbird: the transport
/// must gate outgoing packets and recreate its source on gate epoch changes.
pub struct InputReader {
    inner: Arc<ReaderShared>,
    pending: [u8; 4],
    pending_at: usize,
    pending_epoch: u64,
    position: u64,
}

impl Read for InputReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if self.inner.shared.stopped.load(Ordering::Acquire) {
            return Ok(0);
        }
        // A decoder ring-wrap can request only part of a capture block. Finish
        // that block on the next read instead of letting read-ahead cross into
        // the next capture period and permanently buffer its missing audio as zeros.
        let block_bytes = BLOCK_FRAMES * size_of::<f32>();
        let remaining = block_bytes - (self.position % block_bytes as u64) as usize;
        let count = output.len().min(remaining);
        let snapshot = self.inner.shared.gate.snapshot();
        let mut consumer = self
            .inner
            .consumer
            .lock()
            .map_err(|_| io::Error::other("Capture queue poisoned"))?;
        if !snapshot.allowed() {
            drain(&mut consumer);
        }
        // Drop stale epochs before consuming this call's real-time sample budget.
        while consumer.peek().is_ok_and(|s| s.epoch != snapshot.0) {
            let _ = consumer.pop();
        }
        let mut written = 0;
        while written < count {
            if self.pending_at == 4 {
                let sample = if snapshot.allowed() {
                    consumer
                        .pop()
                        .ok()
                        .filter(|s| s.epoch == snapshot.0)
                        .map_or(0.0, |s| s.sample)
                } else {
                    0.0
                };
                self.pending = finite_sample(sample).to_le_bytes();
                self.pending_at = 0;
                self.pending_epoch = snapshot.0;
            }
            if self.pending_epoch != snapshot.0 || !snapshot.allowed() {
                self.pending.fill(0);
            }
            let amount = (4 - self.pending_at).min(count - written);
            output[written..written + amount]
                .copy_from_slice(&self.pending[self.pending_at..self.pending_at + amount]);
            written += amount;
            self.pending_at += amount;
        }
        if self.inner.shared.gate.epoch() != snapshot.0
            || self.inner.shared.stopped.load(Ordering::Acquire)
        {
            output[..count].fill(0);
            self.pending.fill(0);
        }
        drop(consumer);
        self.position += count as u64;
        Ok(count)
    }
}
impl Seek for InputReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        if from == SeekFrom::Current(0) {
            Ok(self.position)
        } else {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Live microphone input is not seekable",
            ))
        }
    }
}
impl MediaSource for InputReader {
    fn is_seekable(&self) -> bool {
        false
    }
    fn byte_len(&self) -> Option<u64> {
        None
    }
}
impl Drop for InputReader {
    fn drop(&mut self) {
        self.inner.leased.store(false, Ordering::Release);
    }
}

pub struct AudioEngine {
    // Control-thread mutex only. Callbacks never touch this owner.
    streams: Mutex<Option<(Stream, Stream)>>,
    worker: Option<JoinHandle<()>>,
    reader: Arc<ReaderShared>,
    playback: PlaybackSink,
    shared: Arc<Shared>,
    pub input_sample_rate: u32,
    pub output_sample_rate: u32,
    device_config: AudioConfig,
}

impl AudioEngine {
    /// The only device-starting entry point. Call solely after an explicit Join
    /// or Call command. The coordinator must close the global gate before
    /// startup; device errors and teardown close this engine's local gates.
    pub fn start(config: AudioConfig, gate: Arc<TxGate>) -> Result<Self> {
        let host = cpal::default_host();
        let input = select_device(&host, config.input_device.as_deref(), true)?;
        let output = select_device(&host, config.output_device.as_deref(), false)?;
        // Pin the actual devices, including defaults, for this authorized call.
        // Recovery must never reinterpret "default" as a different microphone.
        let pinned_config = AudioConfig {
            input_device: Some(input.id()?.to_string()),
            output_device: Some(output.id()?.to_string()),
            output_volume: config.output_volume,
        };
        let input_config = select_config(&input, true)?;
        let output_config = select_config(&output, false)?;
        let input_rate = input_config.sample_rate();
        let output_rate = output_config.sample_rate();
        let capture_chunk = (input_rate as usize / 100).max(1);
        let capture_converter =
            RateConverter::new(input_rate, VOICE_SAMPLE_RATE, 1, capture_chunk)?;
        let playback_converter =
            RateConverter::new(VOICE_SAMPLE_RATE, output_rate, 2, BLOCK_FRAMES)?;
        let shared = Arc::new(Shared::new(gate, config.output_volume));
        let (capture_producer, capture_consumer) =
            RingBuffer::new((input_rate as usize / 10).max(capture_chunk * 2));
        let (transmit_producer, transmit_consumer) = RingBuffer::new(CAPTURE_QUEUE_FRAMES);
        let output_capacity = (output_rate as usize / 10).max(playback_converter.output_max() * 2);
        let (output_producer, output_consumer) = RingBuffer::new(output_capacity);
        let mixer = Arc::new(Mutex::new(Mixer::new(
            shared.gate.snapshot().receive_epoch(),
        )));
        let playback = PlaybackSink {
            mixer: Arc::clone(&mixer),
            shared: Arc::clone(&shared),
        };
        let reader = Arc::new(ReaderShared {
            consumer: Mutex::new(transmit_consumer),
            leased: AtomicBool::new(false),
            shared: Arc::clone(&shared),
        });
        let input_stream =
            build_input(&input, &input_config, capture_producer, Arc::clone(&shared))?;
        let output_stream = build_output(
            &output,
            &output_config,
            output_consumer,
            Arc::clone(&shared),
        )?;
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("fastdistord-audio".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_worker(
                        (capture_consumer, transmit_producer, output_producer),
                        capture_converter,
                        playback_converter,
                        mixer,
                        &worker_shared,
                        output_rate,
                    )
                }));
                if !matches!(result, Ok(Ok(()))) {
                    worker_shared.fail();
                }
            })
            .context("Cannot start audio processing worker")?;
        let engine = Self {
            streams: Mutex::new(Some((input_stream, output_stream))),
            worker: Some(worker),
            reader,
            playback,
            shared,
            input_sample_rate: input_rate,
            output_sample_rate: output_rate,
            device_config: pinned_config,
        };
        // Drop(engine) reliably tears down both devices and worker on play error.
        {
            let streams = engine.streams.lock().expect("new stream owner");
            let (input, output) = streams.as_ref().expect("streams initialized");
            output.play().context("Cannot start speaker output")?;
            input.play().context("Cannot start microphone capture")?;
        }
        Ok(engine)
    }

    /// A fresh reader can be obtained after the previous reader is dropped.
    /// Source replacement is required when transport-side decode-ahead is purged.
    pub fn input_reader(&self) -> Result<InputReader> {
        if self.shared.stopped.load(Ordering::Acquire) {
            bail!("Audio engine has stopped");
        }
        if self
            .reader
            .leased
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            bail!(
                "Previous microphone reader is still active; stop its Songbird track before replacing it"
            );
        }
        let lock = self.reader.consumer.lock();
        match lock {
            Ok(mut consumer) => drain(&mut consumer),
            Err(_) => {
                self.reader.leased.store(false, Ordering::Release);
                bail!("Capture queue poisoned");
            }
        }
        Ok(InputReader {
            inner: Arc::clone(&self.reader),
            pending: [0; 4],
            pending_at: 4,
            pending_epoch: self.shared.gate.epoch(),
            position: 0,
        })
    }
    pub fn playback(&self) -> PlaybackSink {
        self.playback.clone()
    }
    pub fn device_config(&self) -> AudioConfig {
        self.device_config.clone()
    }
    pub fn meter(&self) -> f32 {
        if self.shared.stopped.load(Ordering::Acquire) || !self.shared.gate.transmit_allowed() {
            return 0.0;
        }
        f32::from_bits(self.shared.meter.load(Ordering::Relaxed))
    }
    pub fn has_failed(&self) -> bool {
        self.shared.failed.load(Ordering::Acquire)
    }
    pub fn set_output_volume(&self, volume: f32) {
        self.playback.set_volume(volume);
    }
    pub fn set_participant_volume(&self, user_id: u64, volume: f32) {
        self.playback.set_participant_volume(user_id, volume);
    }
    pub fn set_processing(&self, options: ProcessingOptions) {
        self.shared
            .processing
            .store(options.bits(), Ordering::Release);
    }
    pub fn stats(&self) -> AudioStats {
        AudioStats {
            capture_overruns: self.shared.capture_overruns.load(Ordering::Relaxed),
            playback_overruns: self.shared.playback_overruns.load(Ordering::Relaxed),
            playback_underruns: self.shared.playback_underruns.load(Ordering::Relaxed),
        }
    }

    /// Stops both devices immediately, even while other Arc owners or old
    /// Songbird readers still exist. Idempotent and permanently fail-closed.
    pub fn stop(&self) {
        self.shared.stopped.store(true, Ordering::Release);
        self.shared.meter.store(0, Ordering::Relaxed);
        let streams = self
            .streams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        drop(streams);
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.stop();
        // Dropping CPAL streams stops the hardware. Reader/sink clones cannot
        // keep either device alive; a surviving reader immediately reaches EOF.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run_worker(
    queues: (
        Consumer<CaptureSample>,
        Producer<CaptureSample>,
        Producer<OutputFrame>,
    ),
    mut capture_converter: RateConverter,
    mut playback_converter: RateConverter,
    mixer: Arc<Mutex<Mixer>>,
    shared: &Shared,
    output_rate: u32,
) -> Result<()> {
    let (mut capture, mut transmit, mut output) = queues;
    let mut capture_epoch = shared.gate.epoch();
    let mut receive_epoch = shared.gate.snapshot().receive_epoch();
    let mut filled = 0;
    let mut processor = Processor::new(ProcessingOptions::from_bits(
        shared.processing.load(Ordering::Acquire),
    ));
    let output_capacity = output.buffer().capacity();
    let target_output = (output_rate as usize / 50).max(1); // 20 ms prepared output.
    while !shared.stopped.load(Ordering::Acquire) {
        let snapshot = shared.gate.snapshot();
        let processing = ProcessingOptions::from_bits(shared.processing.load(Ordering::Acquire));
        if capture_epoch != snapshot.0 || processor.options() != processing {
            capture_converter.reset();
            processor.reset(processing);
            filled = 0;
            capture_epoch = snapshot.0;
        }
        if !snapshot.allowed() {
            drain(&mut capture);
        } else {
            // At most one ring's capacity per iteration, even under overload.
            let available = capture.slots();
            for _ in 0..available {
                let Ok(sample) = capture.pop() else {
                    break;
                };
                if sample.epoch != capture_epoch {
                    continue;
                }
                capture_converter.input[0][filled] = sample.sample;
                filled += 1;
                if filled == capture_converter.chunk {
                    let count = capture_converter.process()?;
                    if shared.gate.epoch() == capture_epoch {
                        processor.process(&capture_converter.output[0][..count], |sample| {
                            if transmit
                                .push(CaptureSample {
                                    sample,
                                    epoch: capture_epoch,
                                })
                                .is_err()
                            {
                                shared.capture_overruns.fetch_add(1, Ordering::Relaxed);
                            }
                        });
                    }
                    filled = 0;
                }
            }
        }
        if receive_epoch != snapshot.receive_epoch() {
            receive_epoch = snapshot.receive_epoch();
            playback_converter.reset();
        }
        // Do not consume remote speech while output is backed up. Slow/failed
        // hardware can fill only the fixed per-user budget.
        if output_capacity - output.slots() < target_output
            && output.slots() >= playback_converter.output_max()
        {
            {
                let mut mixer = mixer
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Playback mixer poisoned"))?;
                mixer.clear_for_epoch(receive_epoch);
                if snapshot.deafened() {
                    for user in &mut mixer.users {
                        user.samples.clear();
                    }
                    for channel in &mut playback_converter.input {
                        channel.fill(0.0);
                    }
                } else {
                    mixer.mix_into(&mut playback_converter.input, BLOCK_FRAMES);
                }
            }
            let count = playback_converter.process()?;
            for frame in 0..count {
                let sample = Stereo {
                    left: playback_converter.output[0][frame],
                    right: playback_converter.output[1][frame],
                };
                if output
                    .push(OutputFrame {
                        sample,
                        epoch: receive_epoch,
                    })
                    .is_err()
                {
                    shared.playback_overruns.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

fn capture_callback<T>(
    data: &[T],
    channels: usize,
    producer: &mut Producer<CaptureSample>,
    shared: &Shared,
) where
    T: Sample + Copy,
    f32: FromSample<T>,
{
    let snapshot = shared.gate.snapshot();
    if !snapshot.allowed() || shared.stopped.load(Ordering::Acquire) {
        shared.meter.store(0, Ordering::Relaxed);
        return;
    }
    let mut peak = 0.0_f32;
    let mut dropped = 0;
    for frame in data.chunks_exact(channels) {
        let mono = frame
            .iter()
            .map(|s| finite_sample(f32::from_sample(*s)))
            .sum::<f32>()
            / channels as f32;
        peak = peak.max(mono.abs());
        if producer
            .push(CaptureSample {
                sample: mono,
                epoch: snapshot.0,
            })
            .is_err()
        {
            dropped += 1;
        }
    }
    shared.meter.store(peak.to_bits(), Ordering::Relaxed);
    if dropped > 0 {
        shared
            .capture_overruns
            .fetch_add(dropped, Ordering::Relaxed);
    }
}

fn output_callback<T>(
    data: &mut [T],
    channels: usize,
    consumer: &mut Consumer<OutputFrame>,
    shared: &Shared,
) where
    T: Sample + FromSample<f32> + Copy,
{
    let snapshot = shared.gate.snapshot();
    if snapshot.deafened() || shared.stopped.load(Ordering::Acquire) {
        data.fill(T::from_sample(0.0));
        drain(consumer);
        return;
    }
    while consumer
        .peek()
        .is_ok_and(|f| f.epoch != snapshot.receive_epoch())
    {
        let _ = consumer.pop();
    }
    let volume = f32::from_bits(shared.volume.load(Ordering::Acquire));
    let mut underruns = 0;
    for frame in data.chunks_exact_mut(channels) {
        let sample = match consumer.pop() {
            Ok(f) if f.epoch == snapshot.receive_epoch() => f.sample,
            _ => {
                underruns += 1;
                Stereo::default()
            }
        };
        // Downmix stereo only for mono devices; front L/R for multichannel
        // hardware, with all other channels silent (no accidental LFE feed).
        let left = finite_sample(sample.left * volume);
        let right = finite_sample(sample.right * volume);
        frame.fill(T::from_sample(0.0));
        frame[0] = T::from_sample(if channels == 1 {
            (left + right) * 0.5
        } else {
            left
        });
        if channels > 1 {
            frame[1] = T::from_sample(right);
        }
    }
    // A deafen event racing this callback silences even its prepared output.
    if shared.gate.snapshot().receive_epoch() != snapshot.receive_epoch()
        || shared.stopped.load(Ordering::Acquire)
    {
        data.fill(T::from_sample(0.0));
    }
    if underruns > 0 {
        shared
            .playback_underruns
            .fetch_add(underruns, Ordering::Relaxed);
    }
}

fn input_typed<T>(
    device: &cpal::Device,
    config: StreamConfig,
    mut producer: Producer<CaptureSample>,
    shared: Arc<Shared>,
) -> Result<Stream>
where
    T: SizedSample + Copy,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels);
    let errors = Arc::clone(&shared);
    Ok(device.build_input_stream(
        config,
        move |data: &[T], _| capture_callback(data, channels, &mut producer, &shared),
        move |_| errors.fail(),
        Some(Duration::from_secs(3)),
    )?)
}
fn output_typed<T>(
    device: &cpal::Device,
    config: StreamConfig,
    mut consumer: Consumer<OutputFrame>,
    shared: Arc<Shared>,
) -> Result<Stream>
where
    T: SizedSample + FromSample<f32> + Copy,
{
    let channels = usize::from(config.channels);
    let errors = Arc::clone(&shared);
    Ok(device.build_output_stream(
        config,
        move |data: &mut [T], _| output_callback(data, channels, &mut consumer, &shared),
        move |_| errors.fail(),
        Some(Duration::from_secs(3)),
    )?)
}

macro_rules! dispatch_format {
    ($format:expr, $function:ident, $device:expr, $config:expr, $queue:expr, $shared:expr) => {
        match $format {
            SampleFormat::I8 => $function::<i8>($device, $config, $queue, $shared),
            SampleFormat::I16 => $function::<i16>($device, $config, $queue, $shared),
            SampleFormat::I24 => $function::<cpal::I24>($device, $config, $queue, $shared),
            SampleFormat::I32 => $function::<i32>($device, $config, $queue, $shared),
            SampleFormat::I64 => $function::<i64>($device, $config, $queue, $shared),
            SampleFormat::U8 => $function::<u8>($device, $config, $queue, $shared),
            SampleFormat::U16 => $function::<u16>($device, $config, $queue, $shared),
            SampleFormat::U24 => $function::<cpal::U24>($device, $config, $queue, $shared),
            SampleFormat::U32 => $function::<u32>($device, $config, $queue, $shared),
            SampleFormat::U64 => $function::<u64>($device, $config, $queue, $shared),
            SampleFormat::F32 => $function::<f32>($device, $config, $queue, $shared),
            SampleFormat::F64 => $function::<f64>($device, $config, $queue, $shared),
            _ => bail!("Unsupported audio sample format: {:?}", $format),
        }
    };
}
fn build_input(
    device: &cpal::Device,
    config: &SupportedStreamConfig,
    producer: Producer<CaptureSample>,
    shared: Arc<Shared>,
) -> Result<Stream> {
    dispatch_format!(
        config.sample_format(),
        input_typed,
        device,
        config.config(),
        producer,
        shared
    )
    .context("Cannot open selected microphone")
}
fn build_output(
    device: &cpal::Device,
    config: &SupportedStreamConfig,
    consumer: Consumer<OutputFrame>,
    shared: Arc<Shared>,
) -> Result<Stream> {
    dispatch_format!(
        config.sample_format(),
        output_typed,
        device,
        config.config(),
        consumer,
        shared
    )
    .context("Cannot open selected speaker device")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_gate() -> Arc<TxGate> {
        let gate = Arc::new(TxGate::default());
        gate.set_muted(false);
        gate.set_suppressed(false);
        gate
    }
    fn reader_fixture() -> (Producer<CaptureSample>, InputReader, Arc<TxGate>) {
        let gate = open_gate();
        let shared = Arc::new(Shared::new(Arc::clone(&gate), 1.0));
        let (producer, consumer) = RingBuffer::new(CAPTURE_QUEUE_FRAMES);
        let inner = Arc::new(ReaderShared {
            consumer: Mutex::new(consumer),
            leased: AtomicBool::new(true),
            shared,
        });
        (
            producer,
            InputReader {
                inner,
                pending: [0; 4],
                pending_at: 4,
                pending_epoch: gate.epoch(),
                position: 0,
            },
            gate,
        )
    }

    #[test]
    fn microphone_reader_decodes_with_production_codecs_and_encodes_opus() {
        use songbird::input::{AudioStream, LiveInput, RawAdapter, codecs};
        use symphonia_core::audio::{AudioBufferRef, Signal};

        let (mut producer, reader, gate) = reader_fixture();
        let samples: Vec<f32> = (0..960)
            .map(|i| (i as f32 * std::f32::consts::TAU * 440.0 / 48_000.0).sin() * 0.25)
            .collect();
        for &sample in &samples {
            producer
                .push(CaptureSample {
                    sample,
                    epoch: gate.epoch(),
                })
                .unwrap();
        }
        // Use the runtime registry, not a test-only decoder or enabled test feature.
        let mut input = LiveInput::Raw(AudioStream {
            input: Box::new(RawAdapter::new(reader, 48_000, 1)),
        })
        .promote(codecs::get_codec_registry(), codecs::get_probe())
        .expect("production microphone PCM decoder is missing");
        let parsed = input.parsed_mut().unwrap();
        let packet = parsed.format.next_packet().unwrap();
        let AudioBufferRef::F32(decoded) = parsed.decoder.decode(&packet).unwrap() else {
            panic!("microphone stream must decode as f32 PCM");
        };
        assert_eq!(decoded.spec().rate, 48_000);
        assert_eq!(decoded.spec().channels.count(), 1);
        assert_eq!(decoded.chan(0), samples);
        let mut encoder = songbird::driver::opus::Encoder::new(
            48_000,
            songbird::driver::opus::Channels::Mono,
            songbird::driver::opus::Application::Audio,
        )
        .unwrap();
        let mut opus = [0_u8; 1276];
        let len = encoder.encode_float(decoded.chan(0), &mut opus).unwrap();
        assert!(
            len > 3,
            "synthetic speech must produce a non-silence Opus packet"
        );
        let mut decoder =
            songbird::driver::opus::Decoder::new(48_000, songbird::driver::opus::Channels::Mono)
                .unwrap();
        let mut received = [0_f32; 960];
        assert_eq!(
            decoder
                .decode_float(&opus[..len], &mut received, false)
                .unwrap(),
            960
        );
        assert!(received.iter().any(|sample| sample.abs() > 0.1));
    }

    #[test]
    fn continuous_microphone_packets_do_not_prefetch_future_silence() {
        use songbird::input::{AudioStream, LiveInput, RawAdapter, codecs};
        use symphonia_core::audio::{AudioBufferRef, Signal};

        let (mut producer, reader, gate) = reader_fixture();
        let mut input = LiveInput::Raw(AudioStream {
            input: Box::new(RawAdapter::new(reader, 48_000, 1)),
        })
        .promote(codecs::get_codec_registry(), codecs::get_probe())
        .unwrap();
        let parsed = input.parsed_mut().unwrap();
        // Feed precisely one 20 ms capture period before each mixer read. This
        // crosses Symphonia's ring boundary repeatedly without a device or clock.
        for frame in 0..200 {
            let value = 0.25 + (frame % 4) as f32 * 0.125;
            for _ in 0..960 {
                producer
                    .push(CaptureSample {
                        sample: value,
                        epoch: gate.epoch(),
                    })
                    .unwrap();
            }
            let packet = parsed.format.next_packet().unwrap();
            let AudioBufferRef::F32(decoded) = parsed.decoder.decode(&packet).unwrap() else {
                panic!("microphone must decode to f32");
            };
            assert_eq!(decoded.frames(), 960);
            assert!(
                decoded.chan(0).iter().all(|s| *s == value),
                "capture frame {frame} contains stale samples or synthesized silence"
            );
        }
    }

    #[test]
    fn discord_mute_confirmation_cycle_revokes_queued_pcm_and_held_ptt() {
        let (mut producer, mut reader, gate) = reader_fixture();
        producer
            .push(CaptureSample {
                sample: 0.75,
                epoch: gate.epoch(),
            })
            .unwrap();
        gate.set_remote_muted(true);
        assert!(!gate.transmit_allowed());
        gate.set_remote_muted(false);
        producer
            .push(CaptureSample {
                sample: 0.25,
                epoch: gate.epoch(),
            })
            .unwrap();
        let mut bytes = [0; 4];
        reader.read_exact(&mut bytes).unwrap();
        assert_eq!(f32::from_le_bytes(bytes), 0.25);
        gate.set_ptt_enabled(true);
        gate.set_ptt_pressed(Some(true));
        assert!(gate.transmit_allowed());
        gate.set_remote_muted(true);
        gate.set_remote_muted(false);
        assert!(!gate.transmit_allowed());
    }
    #[test]
    fn starts_muted_and_suppressed() {
        let gate = TxGate::default();
        assert!(gate.is_muted());
        assert!(gate.is_suppressed());
        assert!(!gate.transmit_allowed());
        gate.set_muted(false);
        assert!(!gate.transmit_allowed());
        gate.set_suppressed(false);
        assert!(gate.transmit_allowed());
    }
    #[test]
    fn all_ptt_unknown_and_release_paths_fail_closed() {
        let gate = open_gate();
        gate.set_ptt_pressed(Some(true));
        gate.set_ptt_enabled(true);
        assert!(
            !gate.transmit_allowed(),
            "enabling must invalidate an old key-down"
        );
        gate.set_ptt_pressed(Some(true));
        assert!(gate.transmit_allowed());
        gate.set_ptt_pressed(None);
        assert!(!gate.transmit_allowed());
        gate.set_ptt_pressed(Some(true));
        gate.set_ptt_pressed(Some(false));
        assert!(!gate.transmit_allowed());
        gate.set_ptt_enabled(false);
        assert!(gate.transmit_allowed());
        gate.fail_closed();
        assert!(!gate.transmit_allowed());
    }
    #[test]
    fn deafen_suppression_and_mute_are_independent() {
        let gate = open_gate();
        for flag in [
            TxGate::set_muted,
            TxGate::set_deafened,
            TxGate::set_suppressed,
        ] {
            flag(&gate, true);
            assert!(!gate.transmit_allowed());
            flag(&gate, false);
            assert!(gate.transmit_allowed());
        }
        gate.set_muted(true);
        gate.set_deafened(true);
        gate.set_deafened(false);
        assert!(gate.is_muted());
    }
    #[test]
    fn encryption_transition_discards_capture_and_does_not_clear_admin_or_ptt_gates() {
        let (mut producer, mut reader, gate) = reader_fixture();
        producer
            .push(CaptureSample {
                sample: 0.75,
                epoch: gate.epoch(),
            })
            .unwrap();
        let before = gate.epoch();
        gate.set_encryption_pending(true);
        assert!(!gate.transmit_allowed());
        gate.set_suppressed(true);
        gate.set_encryption_pending(false);
        assert!(!gate.transmit_allowed());
        gate.set_suppressed(false);
        assert_ne!(before, gate.epoch());
        producer
            .push(CaptureSample {
                sample: 0.25,
                epoch: gate.epoch(),
            })
            .unwrap();
        let mut bytes = [0; 8];
        reader.read_exact(&mut bytes).unwrap();
        assert_eq!(f32::from_le_bytes(bytes[0..4].try_into().unwrap()), 0.25);
        gate.set_ptt_enabled(true);
        gate.set_ptt_pressed(Some(true));
        assert!(gate.transmit_allowed());
        gate.set_encryption_pending(true);
        gate.set_encryption_pending(false);
        assert!(!gate.transmit_allowed());
    }
    #[test]
    fn mute_cycle_invalidates_queued_capture() {
        let (mut producer, mut reader, gate) = reader_fixture();
        producer
            .push(CaptureSample {
                sample: 0.75,
                epoch: gate.epoch(),
            })
            .unwrap();
        let old = gate.epoch();
        gate.set_muted(true);
        gate.set_muted(false);
        assert_ne!(old, gate.epoch());
        producer
            .push(CaptureSample {
                sample: 0.25,
                epoch: gate.epoch(),
            })
            .unwrap();
        let mut bytes = [0; 8];
        assert_eq!(reader.read(&mut bytes).unwrap(), 8);
        assert_eq!(f32::from_le_bytes(bytes[0..4].try_into().unwrap()), 0.25);
        assert_eq!(f32::from_le_bytes(bytes[4..8].try_into().unwrap()), 0.0);
    }
    #[test]
    fn receive_epoch_changes_only_on_deafen() {
        let gate = open_gate();
        let epoch = gate.snapshot().receive_epoch();
        gate.set_muted(true);
        gate.set_ptt_enabled(true);
        assert_eq!(epoch, gate.snapshot().receive_epoch());
        gate.set_deafened(true);
        gate.set_deafened(false);
        assert_ne!(epoch, gate.snapshot().receive_epoch());
    }
    #[test]
    fn stale_ptt_edge_cannot_reopen_after_encryption_gate_revokes_held_state() {
        let gate = open_gate();
        gate.set_ptt_enabled(true);
        let epoch = gate.epoch();
        let held = gate.set_ptt_pressed_at_epoch(Some(true), epoch).unwrap();
        assert!(gate.transmit_allowed());
        gate.set_encryption_pending(true);
        gate.set_encryption_pending(false);
        assert_eq!(gate.set_ptt_pressed_at_epoch(Some(true), held), None);
        assert!(!gate.transmit_allowed());
        let released = gate
            .set_ptt_pressed_at_epoch(Some(false), gate.epoch())
            .unwrap();
        assert!(
            gate.set_ptt_pressed_at_epoch(Some(true), released)
                .is_some()
        );
        assert!(gate.transmit_allowed());
    }
    #[test]
    fn arbitrary_byte_reads_and_shutdown() {
        let (mut producer, mut reader, gate) = reader_fixture();
        producer
            .push(CaptureSample {
                sample: 0.5,
                epoch: gate.epoch(),
            })
            .unwrap();
        let mut bytes = [0; 4];
        reader.read_exact(&mut bytes[..1]).unwrap();
        reader.read_exact(&mut bytes[1..]).unwrap();
        assert_eq!(f32::from_le_bytes(bytes), 0.5);
        assert!(!reader.is_seekable());
        assert!(reader.seek(SeekFrom::Start(0)).is_err());
        reader.inner.shared.stopped.store(true, Ordering::Release);
        assert_eq!(reader.read(&mut bytes).unwrap(), 0);
    }
    #[test]
    fn source_and_handles_have_required_thread_traits() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<InputReader>();
        assert_send_sync::<PlaybackSink>();
        assert_send_sync::<TxGate>();
        assert_send_sync::<AudioEngine>();
    }
    #[test]
    fn mixer_preserves_stereo_and_saturates() {
        let mut mixer = Mixer::new(0);
        assert_eq!(mixer.push(1, &[16_384, -16_384, 32_767, -32_768], 2), 2);
        assert_eq!(mixer.push(2, &[16_384, 16_384], 1), 2);
        let mut output = vec![vec![0.0; 3]; 2];
        mixer.mix_into(&mut output, 3);
        assert_eq!(output[0], [1.0, 1.0, 0.0]);
        assert_eq!(output[1], [0.0, -0.5, 0.0]);
    }
    #[test]
    fn participant_attenuation_follows_user_id_and_applies_to_queued_audio() {
        let mut mixer = Mixer::new(0);
        mixer.push(10, &[16_384, -16_384], 2);
        mixer.push(20, &[16_384, 16_384], 2);
        mixer.set_participant_volume(10, 0.0);
        mixer.set_participant_volume(20, 0.5);
        let mut out = vec![vec![0.0; 1]; 2];
        mixer.mix_into(&mut out, 1);
        assert_eq!(out, [vec![0.25], vec![0.25]]);
        // Reuse the first mixer slot for another speaker: old attenuation must
        // not follow the slot/SSRC. A later packet for user10 is still muted.
        mixer.users[0].id = None;
        mixer.push(30, &[16_384], 1);
        mixer.push(10, &[16_384], 1);
        mixer.mix_into(&mut out, 1);
        assert_eq!(out, [vec![0.5], vec![0.5]]);
        mixer.set_participant_volume(10, 8.0);
        mixer.push(10, &[16_384], 1);
        mixer.mix_into(&mut out, 1);
        assert_eq!(out[0][0], 0.5, "participant volume must never amplify");
        mixer.set_participant_volume(10, f32::NAN);
        mixer.push(10, &[16_384], 1);
        mixer.mix_into(&mut out, 1);
        assert_eq!(out[0][0], 0.0);
    }
    #[test]
    fn participant_volume_controls_remain_bounded_and_deafen_still_clears_queues() {
        let mut mixer = Mixer::new(0);
        for id in 1..=1_000 {
            mixer.set_participant_volume(id, 0.5);
        }
        assert_eq!(mixer.volumes.len(), MAX_USER_VOLUMES);
        mixer.push(1, &[16_384], 1);
        mixer.clear_for_epoch(1);
        let mut out = vec![vec![0.0; 1]; 2];
        mixer.mix_into(&mut out, 1);
        assert_eq!(out[0][0], 0.0);
        mixer.push(1, &[16_384], 1);
        mixer.mix_into(&mut out, 1);
        assert_eq!(
            out[0][0], 0.25,
            "deafen must preserve the user's chosen attenuation"
        );
    }
    #[test]
    fn mixer_is_bounded_per_user_and_globally() {
        let mut mixer = Mixer::new(0);
        let incoming = vec![100; USER_QUEUE_FRAMES * 2];
        for user in 0..MAX_USERS {
            assert_eq!(mixer.push(user as u64, &incoming, 1), USER_QUEUE_FRAMES);
            assert_eq!(mixer.push(user as u64, &[100], 1), 0);
        }
        assert_eq!(mixer.push(999, &[100], 1), 0);
        assert_eq!(
            mixer.users.iter().map(|u| u.samples.len()).sum::<usize>(),
            MAX_USERS * USER_QUEUE_FRAMES
        );
        assert_eq!(mixer.push(0, &[1, 2, 3], 2), 0);
        assert_eq!(mixer.push(0, &[1], 0), 0);
        mixer.clear_for_epoch(1);
        assert!(mixer.users.iter().all(|u| u.samples.is_empty()));
    }
    #[test]
    fn callbacks_gate_sanitize_convert_and_bound_without_hardware() {
        let gate = open_gate();
        let shared = Shared::new(Arc::clone(&gate), 1.0);
        let (mut producer, mut consumer) = RingBuffer::new(1);
        capture_callback(&[0.5_f32, 0.25, f32::NAN, 0.5], 2, &mut producer, &shared);
        assert_eq!(consumer.pop().unwrap().sample, 0.375);
        assert_eq!(shared.capture_overruns.load(Ordering::Relaxed), 1);
        gate.set_muted(true);
        capture_callback(&[1.0_f32], 1, &mut producer, &shared);
        assert!(consumer.is_empty());
    }
    #[test]
    fn output_deafen_epoch_volume_and_downmix() {
        let gate = open_gate();
        let shared = Shared::new(Arc::clone(&gate), 0.5);
        let (mut producer, mut consumer) = RingBuffer::new(4);
        let sample = Stereo {
            left: 1.0,
            right: 0.5,
        };
        let epoch = gate.snapshot().receive_epoch();
        producer.push(OutputFrame { sample, epoch }).unwrap();
        let mut out = [0.0_f32; 1];
        output_callback(&mut out, 1, &mut consumer, &shared);
        assert_eq!(out[0], 0.375);
        producer.push(OutputFrame { sample, epoch }).unwrap();
        gate.set_deafened(true);
        gate.set_deafened(false);
        output_callback(&mut out, 1, &mut consumer, &shared);
        assert_eq!(out[0], 0.0, "old audio must not replay after undeafen");
        producer
            .push(OutputFrame {
                sample,
                epoch: gate.snapshot().receive_epoch(),
            })
            .unwrap();
        gate.set_deafened(true);
        output_callback(&mut out, 1, &mut consumer, &shared);
        assert_eq!(out[0], 0.0);
        assert!(consumer.is_empty());
    }
    #[test]
    fn resampling_common_rates_preserves_duration_and_dc() {
        for (from, to) in [
            (44_100, 48_000),
            (96_000, 48_000),
            (48_000, 44_100),
            (48_000, 48_000),
            (8_000, 48_000),
        ] {
            let mut converter = RateConverter::new(from, to, 1, from as usize / 100).unwrap();
            let mut count = 0;
            let mut last = Vec::new();
            for _ in 0..100 {
                converter.input[0].fill(0.25);
                let n = converter.process().unwrap();
                count += n;
                last = converter.output[0][..n].to_vec();
            }
            assert!(
                (count as i64 - to as i64).unsigned_abs() < 1_000,
                "{from}->{to}: {count}"
            );
            assert!(last.iter().all(|sample| (*sample - 0.25).abs() < 0.002));
            converter.reset();
            let n = converter.process().unwrap();
            assert!(converter.output[0][..n].iter().all(|s| s.abs() < 0.00001));
        }
    }
    #[test]
    fn downsampling_rejects_above_nyquist_tone() {
        let mut converter = RateConverter::new(96_000, 48_000, 1, 960).unwrap();
        let mut sum = 0.0_f64;
        let mut count = 0;
        for block in 0..30 {
            for (i, sample) in converter.input[0].iter_mut().enumerate() {
                *sample =
                    (std::f32::consts::TAU * 30_000.0 * (block * 960 + i) as f32 / 96_000.0).sin();
            }
            let n = converter.process().unwrap();
            if block > 5 {
                sum += converter.output[0][..n]
                    .iter()
                    .map(|x| f64::from(*x).powi(2))
                    .sum::<f64>();
                count += n;
            }
        }
        assert!((sum / count as f64).sqrt() < 0.01);
    }

    #[test]
    fn duplex_worker_moves_real_pcm_in_both_directions_without_devices() {
        let gate = open_gate();
        let shared = Arc::new(Shared::new(Arc::clone(&gate), 1.0));
        let (mut capture_in, capture_out) = RingBuffer::new(4_410);
        let (transmit_in, mut transmit_out) = RingBuffer::new(CAPTURE_QUEUE_FRAMES);
        let (output_in, mut output_out) = RingBuffer::new(4_410);
        let mixer = Arc::new(Mutex::new(Mixer::new(gate.snapshot().receive_epoch())));
        let playback = PlaybackSink {
            mixer: Arc::clone(&mixer),
            shared: Arc::clone(&shared),
        };
        assert_eq!(playback.push_pcm(1, &[8_192; 960], 1), 960);
        assert_eq!(playback.push_pcm(2, &[8_192; 960], 1), 960);
        for _ in 0..882 {
            capture_in
                .push(CaptureSample {
                    sample: 0.25,
                    epoch: gate.epoch(),
                })
                .unwrap();
        }
        let worker_shared = Arc::clone(&shared);
        let worker = thread::spawn(move || {
            run_worker(
                (capture_out, transmit_in, output_in),
                RateConverter::new(44_100, 48_000, 1, 441).unwrap(),
                RateConverter::new(48_000, 44_100, 2, BLOCK_FRAMES).unwrap(),
                mixer,
                &worker_shared,
                44_100,
            )
            .unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        while (transmit_out.slots() < 700 || output_out.slots() < 700) && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(2));
        }
        shared.stopped.store(true, Ordering::Release);
        worker.join().unwrap();
        assert!(
            transmit_out.slots() >= 700,
            "capture worker did not produce resampled PCM"
        );
        assert!(
            output_out.slots() >= 700,
            "playback worker did not produce device-rate PCM"
        );
        let mut mic_last = 0.0;
        while let Ok(frame) = transmit_out.pop() {
            assert_eq!(frame.epoch, gate.epoch());
            mic_last = frame.sample;
        }
        assert!((mic_last - 0.25).abs() < 0.002);
        let mut speaker_peak = 0.0_f32;
        while let Ok(frame) = output_out.pop() {
            assert_eq!(frame.epoch, gate.snapshot().receive_epoch());
            speaker_peak = speaker_peak.max(frame.sample.left);
            assert_eq!(frame.sample.left, frame.sample.right);
        }
        assert!((0.49..0.6).contains(&speaker_peak));
    }

    #[test]
    fn old_device_failure_closes_local_capture_without_mutating_new_session_gate() {
        let gate = open_gate();
        let shared = Shared::new(Arc::clone(&gate), 1.0);
        let epoch = gate.epoch();
        shared.fail();
        assert!(gate.transmit_allowed());
        assert_eq!(
            gate.epoch(),
            epoch,
            "old call failure must not mute a new call"
        );
        assert!(shared.failed.load(Ordering::Acquire));
        assert!(shared.stopped.load(Ordering::Acquire));
        let (mut producer, mut consumer) = RingBuffer::new(2);
        capture_callback(&[0.5_f32; 2], 1, &mut producer, &shared);
        assert!(consumer.is_empty());
        // Keep the queue usable to prove failure blocked capture, not allocation.
        producer
            .push(CaptureSample {
                sample: 1.0,
                epoch: gate.epoch(),
            })
            .unwrap();
        assert_eq!(consumer.pop().unwrap().sample, 1.0);
        assert_eq!(safe_volume(f32::NAN), 0.0);
        assert_eq!(safe_volume(5.0), 2.0);
    }

    #[test]
    fn old_engine_stop_and_drop_do_not_change_new_session_gate() {
        let (_, mut reader, gate) = reader_fixture();
        let shared = Arc::clone(&reader.inner.shared);
        let old_engine = AudioEngine {
            streams: Mutex::new(None),
            worker: None,
            reader: Arc::clone(&reader.inner),
            playback: PlaybackSink {
                mixer: Arc::new(Mutex::new(Mixer::new(gate.snapshot().receive_epoch()))),
                shared: Arc::clone(&shared),
            },
            shared,
            input_sample_rate: 48_000,
            output_sample_rate: 48_000,
            device_config: AudioConfig::default(),
        };
        assert_eq!(gate.begin_session(), 1);
        assert_eq!(gate.begin_session(), 2);
        let new_epoch = gate.epoch();
        old_engine.stop();
        old_engine.stop();
        drop(old_engine);
        assert_eq!(gate.session(), 2);
        assert_eq!(gate.epoch(), new_epoch);
        assert!(gate.transmit_allowed());
        assert_eq!(reader.read(&mut [0; 4]).unwrap(), 0);
    }
}
