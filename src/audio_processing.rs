//! Optional native microphone DSP, exclusively on the audio worker.
//! RNNoise suppresses noise; the conservative digital AGC adjusts level. Neither
//! provides acoustic echo cancellation. Use headphones when possible.

use nnnoiseless::DenoiseState;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

pub const FRAME_SAMPLES: usize = 480;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProcessingOptions {
    pub noise_suppression: bool,
    pub automatic_gain: bool,
}

impl ProcessingOptions {
    pub(crate) fn bits(self) -> u32 {
        u32::from(self.noise_suppression) | (u32::from(self.automatic_gain) << 1)
    }
    pub(crate) fn from_bits(bits: u32) -> Self {
        Self {
            noise_suppression: bits & 1 != 0,
            automatic_gain: bits & 2 != 0,
        }
    }
}

pub(crate) struct Processor {
    options: ProcessingOptions,
    denoise: Option<Box<DenoiseState<'static>>>,
    input: [f32; FRAME_SAMPLES],
    output: [f32; FRAME_SAMPLES],
    filled: usize,
    gain: f32,
    first_noise_frame: bool,
}

impl Processor {
    pub(crate) fn new(options: ProcessingOptions) -> Self {
        Self {
            options,
            denoise: options.noise_suppression.then(DenoiseState::new),
            input: [0.0; FRAME_SAMPLES],
            output: [0.0; FRAME_SAMPLES],
            filled: 0,
            gain: 1.0,
            first_noise_frame: true,
        }
    }

    /// Called on gate epoch changes, including a full mute/unmute between ticks.
    /// No partial frame, denoiser history, or learned gain crosses that boundary.
    pub(crate) fn reset(&mut self, options: ProcessingOptions) {
        *self = Self::new(options);
    }

    pub(crate) fn options(&self) -> ProcessingOptions {
        self.options
    }

    pub(crate) fn process(&mut self, samples: &[f32], mut emit: impl FnMut(f32)) {
        if self.options == ProcessingOptions::default() {
            for &sample in samples {
                emit(sanitize(sample));
            }
            return;
        }
        for &sample in samples {
            self.input[self.filled] = sanitize(sample);
            self.filled += 1;
            if self.filled != FRAME_SAMPLES {
                continue;
            }
            if let Some(denoise) = &mut self.denoise {
                // RNNoise expects f32 containing signed 16-bit PCM magnitudes.
                for sample in &mut self.input {
                    *sample = (*sample * 32_768.0).clamp(-32_768.0, 32_767.0);
                }
                denoise.process_frame(&mut self.output, &self.input);
                for sample in &mut self.output {
                    *sample = sanitize(*sample / 32_768.0);
                }
                // The library documents a startup frame with fade-in artifacts.
                if self.first_noise_frame {
                    self.output.fill(0.0);
                    self.first_noise_frame = false;
                }
            } else {
                self.output.copy_from_slice(&self.input);
            }
            if self.options.automatic_gain {
                apply_gain(&mut self.output, &mut self.gain);
            }
            for &sample in &self.output {
                emit(sample);
            }
            self.input.zeroize();
            self.output.zeroize();
            self.filled = 0;
        }
    }
}

impl Drop for Processor {
    fn drop(&mut self) {
        self.input.zeroize();
        self.output.zeroize();
        self.gain.zeroize();
        // The upstream denoiser owns its private history; dropping disposes it.
        // It exposes no zeroization API, so do not claim that state is scrubbed.
    }
}

fn sanitize(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

fn apply_gain(samples: &mut [f32; FRAME_SAMPLES], gain: &mut f32) {
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / FRAME_SAMPLES as f32).sqrt();
    let peak = samples.iter().fold(0.0_f32, |a, s| a.max(s.abs()));
    // Never chase near silence. Boost limited to 12 dB; output has 1 dB headroom.
    let target = if rms < 0.01 {
        1.0
    } else {
        (0.12 / rms).clamp(0.25, 4.0)
    };
    let target = target.min(if peak > 0.0 { 0.89 / peak } else { 1.0 });
    // Reduce immediately for a loud transient; raise slowly (~0.5 s settling).
    *gain = if target < *gain {
        target
    } else {
        *gain + 0.02 * (target - *gain)
    };
    for sample in samples {
        *sample = sanitize(*sample * *gain);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_processing_is_immediate_identity_and_options_default_off() {
        let mut processor = Processor::new(ProcessingOptions::default());
        let input = [0.125, -0.5, 0.0];
        let mut out = Vec::new();
        processor.process(&input, |s| out.push(s));
        assert_eq!(out, input);
        assert_eq!(
            serde_json::from_str::<ProcessingOptions>("{}").unwrap(),
            ProcessingOptions::default()
        );
    }

    #[test]
    fn gain_does_not_chase_silence_and_limits_loud_transients() {
        let mut processor = Processor::new(ProcessingOptions {
            automatic_gain: true,
            ..Default::default()
        });
        let mut peak = 0.0_f32;
        for _ in 0..100 {
            processor.process(&[0.02; FRAME_SAMPLES], |s| peak = peak.max(s.abs()));
        }
        assert!(peak > 0.02 && peak < 0.081);
        processor.process(&[1.0; FRAME_SAMPLES], |s| assert!(s.abs() <= 0.89));
        processor.reset(ProcessingOptions {
            automatic_gain: true,
            ..Default::default()
        });
        for _ in 0..100 {
            processor.process(&[0.0001; FRAME_SAMPLES], |s| assert!(s.abs() <= 0.0001));
        }
    }

    #[test]
    fn gate_reset_discards_partial_frames_and_noise_history() {
        let options = ProcessingOptions {
            noise_suppression: true,
            automatic_gain: true,
        };
        let mut processor = Processor::new(options);
        processor.process(&[0.9; FRAME_SAMPLES - 1], |_| {
            panic!("partial frame emitted")
        });
        processor.reset(options);
        let mut emitted = 0;
        processor.process(&[0.0; FRAME_SAMPLES], |s| {
            assert_eq!(s, 0.0);
            emitted += 1;
        });
        assert_eq!(emitted, FRAME_SAMPLES);
        processor.reset(ProcessingOptions::default());
        processor.process(&[0.25], |s| assert_eq!(s, 0.25));
    }

    #[test]
    fn suppression_matches_native_reference_across_partial_chunks() {
        let mut processor = Processor::new(ProcessingOptions {
            noise_suppression: true,
            automatic_gain: false,
        });
        let mut reference = DenoiseState::new();
        let mut seed = 1_u32;
        let mut input_energy = 0.0_f64;
        let mut output_energy = 0.0_f64;
        for block in 0..200 {
            let mut input = [0.0; FRAME_SAMPLES];
            for sample in &mut input {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *sample = (seed as f32 / u32::MAX as f32 - 0.5) * 0.02;
                if block > 20 {
                    input_energy += f64::from(*sample).powi(2);
                }
            }
            let pcm = input.map(|s| (s * 32_768.0).clamp(-32_768.0, 32_767.0));
            let mut expected = [0.0; FRAME_SAMPLES];
            reference.process_frame(&mut expected, &pcm);
            if block == 0 {
                expected.fill(0.0);
            }
            let mut actual = Vec::new();
            // Rubato output need not align with RNNoise's frame boundaries.
            processor.process(&input[..171], |s| actual.push(s));
            processor.process(&input[171..], |s| actual.push(s));
            assert_eq!(actual.len(), FRAME_SAMPLES);
            for (s, expected) in actual.into_iter().zip(expected) {
                assert!(s.is_finite() && s.abs() <= 1.0);
                assert_eq!(s, sanitize(expected / 32_768.0));
                if block > 20 {
                    output_energy += f64::from(s).powi(2);
                }
            }
        }
        // A synthetic fixture confirms actual suppression, not a voice-quality
        // guarantee or a fixed reduction target (RNNoise is signal-dependent).
        assert!(
            output_energy < input_energy,
            "synthetic noise suppression regressed: {output_energy}/{input_energy}"
        );
    }

    /// Explicit offline benchmark, excluded from ordinary tests. Measures this
    /// CPU/build only; synthetic blocks cannot establish live call performance.
    #[test]
    #[ignore = "run manually in release mode for a synthetic DSP timing sample"]
    fn synthetic_processing_timing() {
        for options in [
            ProcessingOptions::default(),
            ProcessingOptions {
                automatic_gain: true,
                noise_suppression: false,
            },
            ProcessingOptions {
                automatic_gain: true,
                noise_suppression: true,
            },
        ] {
            let mut processor = Processor::new(options);
            let frame: [f32; FRAME_SAMPLES] = std::array::from_fn(|i| {
                (i as f32 * std::f32::consts::TAU * 440.0 / 48_000.0).sin() * 0.05
            });
            let start = std::time::Instant::now();
            let mut sum = 0.0_f32;
            for _ in 0..1_000 {
                processor.process(std::hint::black_box(&frame), |s| sum += s);
            }
            std::hint::black_box(sum);
            eprintln!(
                "DSP {options:?}: 10.0s synthetic mono audio processed in {:.3}ms",
                start.elapsed().as_secs_f64() * 1_000.0
            );
        }
    }
}
