//! Explicit, local-only microphone test. No transport, transmit gate, files, or
//! network APIs are involved. Capture is bounded to five seconds and playback
//! requires a second command. Stop/Drop joins the owner and erases owned samples.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use cpal::{
    FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig, SupportedStreamConfig,
    traits::{DeviceTrait, StreamTrait},
};
use zeroize::Zeroizing;

use crate::{
    audio::{AudioConfig, RateConverter, VOICE_SAMPLE_RATE, select_config, select_device},
    audio_processing::{ProcessingOptions, Processor},
};

const TEST_SECONDS: usize = 5;
const READY_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TestPhase {
    #[default]
    Idle,
    Recording,
    Ready,
    Playing,
    Failed,
}

#[derive(Clone, Debug)]
pub struct TestStatus {
    pub phase: TestPhase,
    pub input_name: String,
    pub elapsed_seconds: f32,
    pub duration_seconds: f32,
    pub error: Option<String>,
}

impl Default for TestStatus {
    fn default() -> Self {
        Self {
            phase: TestPhase::Idle,
            input_name: String::new(),
            elapsed_seconds: 0.0,
            duration_seconds: TEST_SECONDS as f32,
            error: None,
        }
    }
}

#[derive(Default)]
pub struct MicTestController {
    owner: Option<TestOwner>,
    status: Arc<Mutex<TestStatus>>,
}

struct TestOwner {
    stopped: Arc<AtomicBool>,
    play: SyncSender<()>,
    thread: JoinHandle<()>,
}

impl MicTestController {
    /// Only call after an explicit test command, outside any active call. Device
    /// access and processing occur on a dedicated owner, never the UI thread.
    pub fn start(&mut self, config: AudioConfig, options: ProcessingOptions) -> Result<()> {
        self.stop();
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = TestStatus {
            phase: TestPhase::Recording,
            input_name: "Opening selected input…".into(),
            ..Default::default()
        };
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stopped);
        let status = Arc::clone(&self.status);
        let (play, commands) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("fastdistord-mic-test".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_test(config, options, &worker_stop, &commands, &status)
                }));
                let mut state = status.lock().unwrap_or_else(|e| e.into_inner());
                if worker_stop.load(Ordering::Acquire) || matches!(result, Ok(Ok(()))) {
                    *state = TestStatus::default();
                } else {
                    state.phase = TestPhase::Failed;
                    state.error = Some(match result {
                        Ok(Err(error)) => error.to_string(),
                        _ => "Microphone test worker stopped unexpectedly".into(),
                    });
                }
            });
        match worker {
            Ok(thread) => {
                self.owner = Some(TestOwner {
                    stopped,
                    play,
                    thread,
                });
                Ok(())
            }
            Err(error) => {
                *self.status.lock().unwrap_or_else(|e| e.into_inner()) = TestStatus::default();
                Err(error).context("Cannot start microphone test worker")
            }
        }
    }

    /// Plays once through the selected output, then disposes the recording.
    pub fn play(&mut self) -> Result<()> {
        if self.status().phase != TestPhase::Ready {
            bail!("Microphone test is not ready to play");
        }
        let Some(owner) = &self.owner else {
            bail!("Microphone test has stopped");
        };
        owner
            .play
            .try_send(())
            .context("Microphone test playback already requested")
    }

    pub fn status(&self) -> TestStatus {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Stops callbacks before erasing buffers. Also required on hide, Settings
    /// close, call start, device change, logout and quit; callers own those events.
    pub fn stop(&mut self) {
        if let Some(owner) = self.owner.take() {
            owner.stopped.store(true, Ordering::Release);
            let _ = owner.thread.join();
        }
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = TestStatus::default();
    }
}

impl Drop for MicTestController {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Fixed atomic storage lets callbacks write/read without locks or allocation.
/// Stop first drops the stream owner, then clears the samples. Final Drop repeats
/// the scrub even if initialization, device access, or processing failed.
struct Clip {
    samples: Box<[AtomicU32]>,
    frames: AtomicUsize,
    cursor: AtomicUsize,
    cancelled: AtomicBool,
    failed: AtomicBool,
}

impl Clip {
    fn new(frames: usize) -> Self {
        Self {
            samples: (0..frames).map(|_| AtomicU32::new(0)).collect(),
            frames: AtomicUsize::new(0),
            cursor: AtomicUsize::new(0),
            cancelled: AtomicBool::new(false),
            failed: AtomicBool::new(false),
        }
    }
    fn clear(&self) {
        self.cancelled.store(true, Ordering::Release);
        for sample in &self.samples {
            sample.store(0, Ordering::SeqCst);
        }
        self.frames.store(0, Ordering::Release);
        self.cursor.store(0, Ordering::Release);
    }
}
impl Drop for Clip {
    fn drop(&mut self) {
        self.clear();
    }
}

struct LocalStream {
    stream: Option<Stream>,
    clip: Arc<Clip>,
}
impl Drop for LocalStream {
    fn drop(&mut self) {
        self.clip.cancelled.store(true, Ordering::Release);
        drop(self.stream.take());
        // A controller stop cannot leave an Arc-held clip containing samples.
        self.clip.clear();
    }
}

fn run_test(
    config: AudioConfig,
    options: ProcessingOptions,
    stopped: &AtomicBool,
    commands: &Receiver<()>,
    status: &Mutex<TestStatus>,
) -> Result<()> {
    if stopped.load(Ordering::Acquire) {
        return Ok(());
    }
    let host = cpal::default_host();
    let input = select_device(&host, config.input_device.as_deref(), true)?;
    let input_config = select_config(&input, true)?;
    let input_rate = input_config.sample_rate();
    let clip = Arc::new(Clip::new(input_rate as usize * TEST_SECONDS));
    {
        let mut s = status.lock().unwrap_or_else(|e| e.into_inner());
        s.input_name = input.to_string();
    }
    if stopped.load(Ordering::Acquire) {
        return Ok(());
    }
    let capture_stream = build_stream(&input, &input_config, Arc::clone(&clip), true)?;
    let mut capture = LocalStream {
        stream: Some(capture_stream),
        clip: Arc::clone(&clip),
    };
    if stopped.load(Ordering::Acquire) {
        return Ok(());
    }
    capture
        .stream
        .as_ref()
        .expect("initialized stream")
        .play()
        .context("Cannot start test microphone")?;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(TEST_SECONDS as u64)
        && clip.frames.load(Ordering::Acquire) < clip.samples.len()
    {
        if stopped.load(Ordering::Acquire) {
            return Ok(());
        }
        if clip.failed.load(Ordering::Acquire) {
            bail!("Selected microphone failed during test");
        }
        status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .elapsed_seconds = started.elapsed().as_secs_f32();
        thread::sleep(Duration::from_millis(10));
    }
    // Close capture before Ready. Preserve the samples until Play/Stop/timeout.
    clip.cancelled.store(true, Ordering::Release);
    drop(capture.stream.take());
    let count = clip.frames.load(Ordering::Acquire).min(clip.samples.len());
    if count == 0 {
        bail!("Selected microphone delivered no samples");
    }
    {
        let mut s = status.lock().unwrap_or_else(|e| e.into_inner());
        s.phase = TestPhase::Ready;
        s.duration_seconds = count as f32 / input_rate as f32;
        s.elapsed_seconds = s.duration_seconds;
    }
    let ready = Instant::now();
    loop {
        if stopped.load(Ordering::Acquire) || ready.elapsed() >= READY_TIMEOUT {
            return Ok(());
        }
        match commands.try_recv() {
            Ok(()) => break,
            Err(TryRecvError::Disconnected) => return Ok(()),
            Err(TryRecvError::Empty) => thread::sleep(Duration::from_millis(10)),
        }
    }
    let output = select_device(&host, config.output_device.as_deref(), false)?;
    let output_config = select_config(&output, false)?;
    let output_rate = output_config.sample_rate();
    let playback = prepare_playback(&clip, count, input_rate, output_rate, options, stopped)?;
    // Honor the selected output level without allowing a test to amplify above
    // full scale. Test playback is local and independent of the call's gates.
    let volume = if config.output_volume.is_finite() {
        config.output_volume.clamp(0.0, 1.0)
    } else {
        0.0
    };
    for sample in &playback.samples {
        sample.store(
            (f32::from_bits(sample.load(Ordering::Relaxed)) * volume).to_bits(),
            Ordering::Relaxed,
        );
    }
    drop(capture); // Erase original captured samples before opening playback.
    if stopped.load(Ordering::Acquire) {
        return Ok(());
    }
    let output_stream = build_stream(&output, &output_config, Arc::clone(&playback), false)?;
    let player = LocalStream {
        stream: Some(output_stream),
        clip: Arc::clone(&playback),
    };
    if stopped.load(Ordering::Acquire) {
        return Ok(());
    }
    {
        let mut s = status.lock().unwrap_or_else(|e| e.into_inner());
        s.phase = TestPhase::Playing;
        s.elapsed_seconds = 0.0;
    }
    player
        .stream
        .as_ref()
        .expect("initialized stream")
        .play()
        .context("Cannot play microphone test")?;
    let started = Instant::now();
    while playback.cursor.load(Ordering::Acquire) < playback.frames.load(Ordering::Acquire) {
        if stopped.load(Ordering::Acquire) {
            return Ok(());
        }
        if playback.failed.load(Ordering::Acquire) {
            bail!("Selected output failed during test");
        }
        if started.elapsed() > Duration::from_secs(TEST_SECONDS as u64 + 2) {
            bail!("Microphone test output timed out");
        }
        status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .elapsed_seconds = playback.cursor.load(Ordering::Acquire) as f32 / output_rate as f32;
        thread::sleep(Duration::from_millis(10));
    }
    // Allow the last submitted callback block to finish playing. Stop remains
    // responsive during this bounded device-buffer grace period.
    let submitted = Instant::now();
    while submitted.elapsed() < Duration::from_millis(100) && !stopped.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(5));
    }
    drop(player);
    Ok(())
}

fn prepare_playback(
    clip: &Clip,
    count: usize,
    input_rate: u32,
    output_rate: u32,
    options: ProcessingOptions,
    stopped: &AtomicBool,
) -> Result<Arc<Clip>> {
    let mut input =
        RateConverter::new(input_rate, VOICE_SAMPLE_RATE, 1, input_rate as usize / 100)?;
    let mut processor = Processor::new(options);
    let mut processed = Zeroizing::new(Vec::with_capacity(
        (TEST_SECONDS + 1) * VOICE_SAMPLE_RATE as usize,
    ));
    for start in (0..count).step_by(input.chunk) {
        if stopped.load(Ordering::Acquire) {
            break;
        }
        input.input[0].fill(0.0);
        for (i, sample) in input.input[0].iter_mut().enumerate().take(count - start) {
            *sample = f32::from_bits(clip.samples[start + i].load(Ordering::Acquire));
        }
        let n = input.process()?;
        processor.process(&input.output[0][..n], |s| processed.push(s));
    }
    let mut output = RateConverter::new(VOICE_SAMPLE_RATE, output_rate, 1, 480)?;
    let playback = Arc::new(Clip::new((TEST_SECONDS + 1) * output_rate as usize));
    let mut written = 0;
    for chunk in processed.chunks(480) {
        if stopped.load(Ordering::Acquire) {
            break;
        }
        output.input[0].fill(0.0);
        output.input[0][..chunk.len()].copy_from_slice(chunk);
        let n = output.process()?;
        for &sample in &output.output[0][..n] {
            if written == playback.samples.len() {
                break;
            }
            playback.samples[written].store(clean(sample).to_bits(), Ordering::Relaxed);
            written += 1;
        }
    }
    playback.frames.store(written, Ordering::Release);
    Ok(playback)
}

fn clean(sample: f32) -> f32 {
    if sample.is_finite() {
        sample.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

fn capture_callback<T: Sample + Copy>(data: &[T], channels: usize, clip: &Clip)
where
    f32: FromSample<T>,
{
    if clip.cancelled.load(Ordering::Acquire) {
        return;
    }
    let start = clip.frames.load(Ordering::Relaxed);
    let mut at = start;
    for frame in data.chunks_exact(channels) {
        if at == clip.samples.len() {
            break;
        }
        let mono = frame
            .iter()
            .map(|s| clean(f32::from_sample(*s)))
            .sum::<f32>()
            / channels as f32;
        clip.samples[at].store(mono.to_bits(), Ordering::Relaxed);
        at += 1;
    }
    clip.frames.store(at, Ordering::Release);
    // Erase a callback raced by Stop. This only visits this callback's writes.
    if clip.cancelled.load(Ordering::Acquire) {
        for sample in &clip.samples[start..at] {
            sample.store(0, Ordering::SeqCst);
        }
    }
}

fn output_callback<T: Sample + FromSample<f32> + Copy>(
    data: &mut [T],
    channels: usize,
    clip: &Clip,
) {
    data.fill(T::from_sample(0.0));
    if clip.cancelled.load(Ordering::Acquire) {
        return;
    }
    let count = clip.frames.load(Ordering::Acquire);
    let mut at = clip.cursor.load(Ordering::Relaxed);
    for frame in data.chunks_exact_mut(channels) {
        if at >= count {
            break;
        }
        let sample = clean(f32::from_bits(clip.samples[at].swap(0, Ordering::AcqRel)));
        // Mono test through front L/R only; no accidental surround/LFE feed.
        frame[0] = T::from_sample(sample);
        if channels > 1 {
            frame[1] = T::from_sample(sample);
        }
        at += 1;
    }
    clip.cursor.store(at, Ordering::Release);
    if clip.cancelled.load(Ordering::Acquire) {
        data.fill(T::from_sample(0.0));
    }
}

fn input_typed<T: SizedSample + Copy>(
    device: &cpal::Device,
    config: StreamConfig,
    clip: Arc<Clip>,
) -> Result<Stream>
where
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels);
    let errors = Arc::clone(&clip);
    Ok(device.build_input_stream(
        config,
        move |data: &[T], _| capture_callback(data, channels, &clip),
        move |_| errors.failed.store(true, Ordering::Release),
        Some(Duration::from_secs(3)),
    )?)
}

fn output_typed<T: SizedSample + FromSample<f32> + Copy>(
    device: &cpal::Device,
    config: StreamConfig,
    clip: Arc<Clip>,
) -> Result<Stream> {
    let channels = usize::from(config.channels);
    let errors = Arc::clone(&clip);
    Ok(device.build_output_stream(
        config,
        move |data: &mut [T], _| output_callback(data, channels, &clip),
        move |_| errors.failed.store(true, Ordering::Release),
        Some(Duration::from_secs(3)),
    )?)
}

fn build_stream(
    device: &cpal::Device,
    config: &SupportedStreamConfig,
    clip: Arc<Clip>,
    input: bool,
) -> Result<Stream> {
    macro_rules! build {
        ($t:ty) => {
            if input {
                input_typed::<$t>(device, config.config(), clip)
            } else {
                output_typed::<$t>(device, config.config(), clip)
            }
        };
    }
    match config.sample_format() {
        SampleFormat::I8 => build!(i8),
        SampleFormat::I16 => build!(i16),
        SampleFormat::I24 => build!(cpal::I24),
        SampleFormat::I32 => build!(i32),
        SampleFormat::I64 => build!(i64),
        SampleFormat::U8 => build!(u8),
        SampleFormat::U16 => build!(u16),
        SampleFormat::U24 => build!(cpal::U24),
        SampleFormat::U32 => build!(u32),
        SampleFormat::U64 => build!(u64),
        SampleFormat::F32 => build!(f32),
        SampleFormat::F64 => build!(f64),
        other => bail!("Unsupported test audio format: {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_is_bounded_sanitized_and_cannot_restart_after_disposal() {
        let clip = Clip::new(2);
        capture_callback(&[0.5_f32, -0.5, f32::NAN, 1.0, 1.0, 1.0], 2, &clip);
        assert_eq!(clip.frames.load(Ordering::Acquire), 2);
        assert_eq!(f32::from_bits(clip.samples[0].load(Ordering::Acquire)), 0.0);
        assert_eq!(f32::from_bits(clip.samples[1].load(Ordering::Acquire)), 0.5);
        clip.clear();
        capture_callback(&[1.0_f32; 8], 1, &clip);
        assert_eq!(clip.frames.load(Ordering::Acquire), 0);
        assert!(clip.samples.iter().all(|s| s.load(Ordering::Acquire) == 0));
    }

    #[test]
    fn playback_erases_consumed_samples_and_stop_erases_arc_held_clip() {
        let clip = Arc::new(Clip::new(4));
        capture_callback(&[0.5_f32; 4], 1, &clip);
        let mut out = [1.0_f32; 4];
        output_callback(&mut out, 2, &clip);
        assert_eq!(out, [0.5; 4]);
        assert!(
            clip.samples[..2]
                .iter()
                .all(|s| s.load(Ordering::Acquire) == 0)
        );
        drop(LocalStream {
            stream: None,
            clip: Arc::clone(&clip),
        });
        assert!(clip.samples.iter().all(|s| s.load(Ordering::Acquire) == 0));
        output_callback(&mut out, 2, &clip);
        assert_eq!(out, [0.0; 4]);
    }

    #[test]
    fn default_controller_never_opens_devices_and_play_requires_ready() {
        let mut controller = MicTestController::default();
        assert_eq!(controller.status().phase, TestPhase::Idle);
        assert!(controller.play().is_err());
        controller.stop();
        assert_eq!(controller.status().phase, TestPhase::Idle);
    }

    #[test]
    fn stop_joins_owner_and_erases_retained_samples_before_returning() {
        let retained = Arc::new(Clip::new(32));
        capture_callback(&[0.5_f32; 32], 1, &retained);
        let clip = Arc::clone(&retained);
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stopped);
        let worker = thread::spawn(move || {
            let _owner = LocalStream { stream: None, clip };
            while !worker_stop.load(Ordering::Acquire) {
                thread::yield_now();
            }
        });
        let (play, _commands) = mpsc::sync_channel(1);
        let mut controller = MicTestController {
            owner: Some(TestOwner {
                stopped,
                play,
                thread: worker,
            }),
            status: Arc::new(Mutex::new(TestStatus::default())),
        };
        controller.stop();
        assert!(
            retained
                .samples
                .iter()
                .all(|s| s.load(Ordering::Acquire) == 0)
        );
        assert_eq!(controller.status().phase, TestPhase::Idle);
        assert!(controller.play().is_err());
    }

    #[test]
    fn local_preparation_uses_rate_conversion_processing_and_bounded_storage() {
        let clip = Clip::new(44_100);
        capture_callback(&vec![0.05_f32; 44_100], 1, &clip);
        let out = prepare_playback(
            &clip,
            44_100,
            44_100,
            48_000,
            ProcessingOptions {
                automatic_gain: true,
                noise_suppression: false,
            },
            &AtomicBool::new(false),
        )
        .unwrap();
        let count = out.frames.load(Ordering::Acquire);
        assert!((47_000..=48_500).contains(&count));
        assert!(out.samples.len() <= (TEST_SECONDS + 1) * 48_000);
        assert!(
            out.samples[..count]
                .iter()
                .any(|s| f32::from_bits(s.load(Ordering::Acquire)) > 0.05)
        );
        out.clear();
        assert!(out.samples.iter().all(|s| s.load(Ordering::Acquire) == 0));
    }
}
