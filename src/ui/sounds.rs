//! Original synthesized local feedback; no Discord sound assets, capture or recording.
use crate::calls::Cue;
use cpal::{
    FromSample, SizedSample, Stream,
    traits::{DeviceTrait, StreamTrait},
};
use std::sync::{
    Arc,
    atomic::{AtomicU32, AtomicU64, Ordering},
    mpsc,
};
use std::time::Duration;
#[derive(Clone)]
struct Lease {
    authority: Arc<AtomicU64>,
    sequence: u64,
}
struct Message {
    lease: Option<Lease>,
    cue: Cue,
    epoch: u64,
    output: Option<String>,
}
struct Shared {
    epoch: AtomicU64,
    volume: AtomicU32,
    wake: Arc<dyn Fn() + Send + Sync>,
}
pub struct Player {
    tx: Option<mpsc::SyncSender<Message>>,
    shared: Arc<Shared>,
    last: Option<(u64, bool)>,
    blocked_sequence: Option<u64>,
    errors: mpsc::Receiver<&'static str>,
}
impl Player {
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        let (tx, rx) = mpsc::sync_channel::<Message>(16);
        let (errors, received) = mpsc::sync_channel(4);
        let shared = Arc::new(Shared {
            epoch: AtomicU64::new(0),
            volume: AtomicU32::new(0.0_f32.to_bits()),
            wake: Arc::new(wake),
        });
        let worker = shared.clone();
        std::thread::spawn(move || {
            let mut stream: Option<Stream> = None;
            let mut timeout = None;
            loop {
                let incoming = if let Some(timeout) = timeout {
                    rx.recv_timeout(timeout)
                } else {
                    rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected)
                };
                match incoming {
                    Ok(message) => {
                        drop(stream.take());
                        timeout = None;
                        if message.epoch != worker.epoch.load(Ordering::Acquire)
                            || message.cue == Cue::Stop
                        {
                            continue;
                        }
                        match open(&message, worker.clone(), errors.clone()) {
                            Ok(next) => {
                                stream = Some(next);
                                timeout = if message.cue == Cue::Ring {
                                    None
                                } else {
                                    Some(Duration::from_millis(450))
                                };
                            }
                            Err(_) => {
                                let _ = errors
                                    .try_send("Call sounds could not open the selected speaker");
                                (worker.wake)();
                            }
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        drop(stream.take());
                        timeout = None;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        });
        Self {
            tx: Some(tx),
            shared,
            last: None,
            blocked_sequence: None,
            errors: received,
        }
    }
}
impl Player {
    pub fn stop(&mut self) {
        self.blocked_sequence = self.last.map(|(sequence, _)| sequence);
        let epoch = self.shared.epoch.fetch_add(1, Ordering::AcqRel) + 1;
        self.shared
            .volume
            .store(0.0_f32.to_bits(), Ordering::Release);
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(Message {
                lease: None,
                cue: Cue::Stop,
                epoch,
                output: None,
            });
        }
    }
    pub fn sync(&mut self, state: &crate::model::UiState) -> Option<&'static str> {
        let enabled = state.call_sounds
            && !state.deafened
            && !state.server_deafened
            && state.confirmed_deafened != Some(true)
            && state.account.is_some()
            && state.sound_volume > 0.0;
        if self.blocked_sequence == Some(state.sound_sequence) && enabled {
            self.shared
                .volume
                .store(0.0_f32.to_bits(), Ordering::Release);
            return self.errors.try_iter().last();
        }
        self.blocked_sequence = None;
        self.shared.volume.store(
            if enabled {
                state.sound_volume.clamp(0.0, 1.0)
            } else {
                0.0
            }
            .to_bits(),
            Ordering::Release,
        );
        let new_sequence = self.last.is_none_or(|(seq, _)| seq != state.sound_sequence);
        if self.last != Some((state.sound_sequence, enabled)) {
            let epoch = self.shared.epoch.fetch_add(1, Ordering::AcqRel) + 1;
            let cue = if enabled && (new_sequence || state.sound_cue == Cue::Ring) {
                state.sound_cue
            } else {
                Cue::Stop
            };
            if let Some(tx) = &self.tx {
                let _ = tx.try_send(Message {
                    lease: Some(Lease {
                        authority: state.sound_authority.clone(),
                        sequence: state.sound_sequence,
                    }),
                    cue,
                    epoch,
                    output: state.selected_output.clone(),
                });
            }
            self.last = Some((state.sound_sequence, enabled));
        }
        self.errors.try_iter().last()
    }
}
impl Drop for Player {
    fn drop(&mut self) {
        self.stop();
        self.tx.take();
    }
}
fn open(
    message: &Message,
    shared: Arc<Shared>,
    errors: mpsc::SyncSender<&'static str>,
) -> anyhow::Result<Stream> {
    let device =
        crate::audio::select_device(&cpal::default_host(), message.output.as_deref(), false)?;
    let config = crate::audio::select_config(&device, false)?;
    use cpal::SampleFormat;
    let stream = match config.sample_format() {
        SampleFormat::F32 => build::<f32>(
            &device,
            config.config(),
            message.cue,
            message.epoch,
            message.lease.clone(),
            shared,
            errors,
        )?,
        SampleFormat::F64 => build::<f64>(
            &device,
            config.config(),
            message.cue,
            message.epoch,
            message.lease.clone(),
            shared,
            errors,
        )?,
        SampleFormat::I16 => build::<i16>(
            &device,
            config.config(),
            message.cue,
            message.epoch,
            message.lease.clone(),
            shared,
            errors,
        )?,
        SampleFormat::I32 => build::<i32>(
            &device,
            config.config(),
            message.cue,
            message.epoch,
            message.lease.clone(),
            shared,
            errors,
        )?,
        SampleFormat::U16 => build::<u16>(
            &device,
            config.config(),
            message.cue,
            message.epoch,
            message.lease.clone(),
            shared,
            errors,
        )?,
        _ => anyhow::bail!("Unsupported sound sample format"),
    };
    stream.play()?;
    Ok(stream)
}
fn build<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    cue: Cue,
    epoch: u64,
    lease: Option<Lease>,
    shared: Arc<Shared>,
    errors: mpsc::SyncSender<&'static str>,
) -> anyhow::Result<Stream> {
    let rate = config.sample_rate as f64;
    let channels = config.channels as usize;
    let mut frame = 0_u64;
    let failure = shared.clone();
    Ok(device.build_output_stream(
        config,
        move |out: &mut [T], _| {
            let valid = shared.epoch.load(Ordering::Acquire) == epoch
                && lease
                    .as_ref()
                    .is_some_and(|lease| lease.authority.load(Ordering::Acquire) == lease.sequence);
            let volume = f32::from_bits(shared.volume.load(Ordering::Acquire));
            for samples in out.chunks_mut(channels) {
                let value = if valid {
                    sample(cue, frame as f64 / rate) * volume
                } else {
                    0.0
                };
                for output in samples {
                    *output = T::from_sample(value);
                }
                frame = frame.saturating_add(1);
            }
        },
        move |_| {
            let _ = errors.try_send("Call sound speaker stopped");
            (failure.wake)();
        },
        None,
    )?)
}
fn sample(cue: Cue, seconds: f64) -> f32 {
    let (t, duration, a, b) = match cue {
        Cue::Stop => return 0.0,
        Cue::Ring => (seconds % 3.0, 0.7, 340.0, 430.0),
        Cue::Connecting => (seconds, 0.25, 430.0, 510.0),
        Cue::Joined => (
            seconds,
            0.35,
            if seconds < 0.175 { 450.0 } else { 650.0 },
            0.0,
        ),
        Cue::Left => (
            seconds,
            0.35,
            if seconds < 0.175 { 650.0 } else { 450.0 },
            0.0,
        ),
    };
    if t >= duration {
        return 0.0;
    }
    let fade = (t / 0.02).min((duration - t) / 0.02).clamp(0.0, 1.0);
    let wave = (std::f64::consts::TAU * a * t).sin()
        + if b == 0.0 {
            0.0
        } else {
            (std::f64::consts::TAU * b * t).sin()
        };
    (wave * fade * 0.06) as f32
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_cannot_restart_ring_from_a_stale_snapshot() {
        let (tx, rx) = mpsc::sync_channel(16);
        let (_out, errors) = mpsc::channel();
        let shared = Arc::new(Shared {
            epoch: AtomicU64::new(1),
            volume: AtomicU32::new(0.3_f32.to_bits()),
            wake: Arc::new(|| {}),
        });
        let mut player = Player {
            tx: Some(tx),
            shared: shared.clone(),
            last: Some((7, true)),
            blocked_sequence: None,
            errors,
        };
        let mut state = crate::model::UiState {
            account: Some(crate::model::Account {
                id: 1,
                name: "Self".into(),
                avatar: None,
            }),
            sound_sequence: 7,
            sound_cue: Cue::Ring,
            ..Default::default()
        };
        player.stop();
        player.sync(&state);
        assert_eq!(f32::from_bits(shared.volume.load(Ordering::Acquire)), 0.0);
        assert_eq!(rx.try_recv().unwrap().cue, Cue::Stop);
        assert!(rx.try_recv().is_err());
        state.sound_sequence = 8;
        state.sound_cue = Cue::Stop;
        player.sync(&state);
        assert_eq!(rx.try_recv().unwrap().cue, Cue::Stop);
    }
    #[test]
    fn original_tones_are_bounded_and_ring_has_silent_interval() {
        for cue in [Cue::Connecting, Cue::Joined, Cue::Left, Cue::Ring] {
            for i in 0..4000 {
                assert!(sample(cue, i as f64 / 1000.0).abs() <= 0.12);
            }
            assert_eq!(sample(cue, 1.0), 0.0);
        }
        assert_eq!(sample(Cue::Stop, 0.1), 0.0);
    }
}
