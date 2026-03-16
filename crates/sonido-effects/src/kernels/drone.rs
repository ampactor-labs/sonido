//! Drone kernel — sympathetic resonance generator.
//!
//! `DroneKernel` detects input amplitude via an envelope follower and generates
//! harmonically-related sine tones (root, perfect fifth, octave). A zero-crossing
//! detector provides a coarse frequency estimate. The tones sustain independently
//! after the input stops, controlled by the `decay` parameter.
//!
//! # Signal Flow
//!
//! ```text
//! input → [envelope follower] → gate tones on/off
//!       → [zero-crossing detector] → coarse frequency estimate
//!                                   → [3 sine oscillators: root, +7st, +12st]
//!                                   → detune wobble applied per oscillator
//!                                   → [decay envelope] → add to dry
//! ```
//!
//! # Algorithm
//!
//! - **Frequency tracking**: counts samples between rising zero-crossings. Clamped
//!   to 50–2000 Hz to stay in musical range.
//! - **Oscillators**: `sin(2π × f × t)` with per-sample phase increment. The fifth
//!   is at `f × 2^(7/12)` and the octave at `2f`.
//! - **Detune**: a slow sinusoidal wobble at ~0.5 Hz adds ±detune_cents to each
//!   oscillator's pitch independently (different LFO phases per voice).
//! - **Decay**: one-pole envelope follower; when signal falls below threshold the
//!   output gate follows an exponential decay with time constant `params.decay`.
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! let adapter = Adapter::new(DroneKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing)
//! let mut kernel = DroneKernel::new(48000.0);
//! let params = DroneParams::default();
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

extern crate alloc;

use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::{ParamDescriptor, ParamId, ParamUnit, fast_db_to_linear};

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`DroneKernel`].
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `root_mix` | % | 0–100 | 30.0 |
/// | 1 | `fifth_mix` | % | 0–100 | 20.0 |
/// | 2 | `octave_mix` | % | 0–100 | 25.0 |
/// | 3 | `detune` | cents | 0–50 | 5.0 |
/// | 4 | `decay` | s | 0.1–10 | 3.0 |
/// | 5 | `output_db` | dB | −60–+6 | 0.0 |
#[derive(Debug, Clone, Copy)]
pub struct DroneParams {
    /// Root tone mix level (0–100 %).
    pub root_mix: f32,
    /// Perfect fifth (+7 semitones) mix level (0–100 %).
    pub fifth_mix: f32,
    /// Octave (+12 semitones) mix level (0–100 %).
    pub octave_mix: f32,
    /// Detune wobble depth in cents (0–50). Adds slow pitch variation per voice.
    pub detune: f32,
    /// Sustain decay time in seconds (0.1–10) after input stops.
    pub decay: f32,
    /// Output level in decibels (−60–+6 dB).
    pub output_db: f32,
}

impl Default for DroneParams {
    fn default() -> Self {
        Self {
            root_mix: 30.0,
            fifth_mix: 20.0,
            octave_mix: 25.0,
            detune: 5.0,
            decay: 3.0,
            output_db: 0.0,
        }
    }
}

impl KernelParams for DroneParams {
    const COUNT: usize = 6;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(
                ParamDescriptor {
                    name: "Root Mix",
                    short_name: "Root",
                    default: 30.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(3300), "drone_root"),
            ),
            1 => Some(
                ParamDescriptor {
                    name: "Fifth Mix",
                    short_name: "Fifth",
                    default: 20.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(3301), "drone_fifth"),
            ),
            2 => Some(
                ParamDescriptor {
                    name: "Octave Mix",
                    short_name: "Octave",
                    default: 25.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(3302), "drone_octave"),
            ),
            3 => Some(
                ParamDescriptor::custom("Detune", "Detune", 0.0, 50.0, 5.0)
                    .with_unit(ParamUnit::None)
                    .with_id(ParamId(3303), "drone_detune"),
            ),
            4 => Some(
                // Decay stored internally in seconds; display as plain value (no Seconds unit)
                ParamDescriptor::custom("Decay", "Decay", 0.1, 10.0, 3.0)
                    .with_unit(ParamUnit::None)
                    .with_id(ParamId(3304), "drone_decay"),
            ),
            5 => Some(
                ParamDescriptor::gain_db("Output", "Out", -60.0, 6.0, 0.0)
                    .with_id(ParamId(3305), "drone_output"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Standard, // root_mix — 10 ms
            1 => SmoothingStyle::Standard, // fifth_mix — 10 ms
            2 => SmoothingStyle::Standard, // octave_mix — 10 ms
            3 => SmoothingStyle::Slow,     // detune — 20 ms
            4 => SmoothingStyle::Slow,     // decay — 20 ms
            5 => SmoothingStyle::Fast,     // output_db — 5 ms
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.root_mix,
            1 => self.fifth_mix,
            2 => self.octave_mix,
            3 => self.detune,
            4 => self.decay,
            5 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.root_mix = value,
            1 => self.fifth_mix = value,
            2 => self.octave_mix = value,
            3 => self.detune = value,
            4 => self.decay = value,
            5 => self.output_db = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Amplitude threshold below which input is considered silent.
const GATE_THRESHOLD: f32 = 0.001;

/// Detune LFO rate in Hz — slow wobble for organic feel.
const DETUNE_LFO_HZ: f32 = 0.5;

/// Envelope follower attack time in seconds.
const ENV_ATTACK_S: f32 = 0.005;

/// Pure DSP sympathetic drone generator kernel.
///
/// # Invariants
///
/// - `osc_phase[i]` stays in `[0, 1)` via modular wrap.
/// - `detune_phase[i]` stays in `[0, 1)`.
/// - `detected_freq_hz` is clamped to `[50.0, 2000.0]`.
/// - `env_gain` is non-negative and bounded by 1.0.
pub struct DroneKernel {
    /// Audio sample rate in Hz.
    sample_rate: f32,
    /// Normalized phase for each of the 3 oscillators: [root, fifth, octave].
    osc_phase: [f32; 3],
    /// Detune LFO phase per oscillator (staggered for independence).
    detune_phase: [f32; 3],
    /// Current envelope gain controlling tone output (0.0–1.0).
    env_gain: f32,
    /// Detected root frequency in Hz.
    detected_freq_hz: f32,
    /// Sample count since last rising zero-crossing.
    zc_counter: u32,
    /// Previous input sample for zero-crossing detection.
    prev_sample: f32,
    /// One-pole envelope follower output.
    env_follower: f32,
}

impl DroneKernel {
    /// Create a new drone kernel at the given sample rate.
    ///
    /// Oscillator phases are staggered by 1/3 cycle so output is non-zero
    /// immediately (before the envelope opens fully).
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            osc_phase: [0.0, 0.333, 0.667],
            detune_phase: [0.0, 0.333, 0.667],
            env_gain: 0.0,
            detected_freq_hz: 220.0,
            zc_counter: 0,
            prev_sample: 0.0,
            env_follower: 0.0,
        }
    }

    /// Pitch ratio for `semitones` interval. `ratio = 2^(semitones/12)`.
    #[inline]
    fn semitone_ratio(semitones: f32) -> f32 {
        libm::powf(2.0, semitones / 12.0)
    }

    /// Sine from normalized phase [0, 1).
    #[inline]
    fn sine(phase: f32) -> f32 {
        libm::sinf(2.0 * core::f32::consts::PI * phase)
    }
}

impl DspKernel for DroneKernel {
    type Params = DroneParams;

    fn process_stereo(&mut self, left: f32, right: f32, params: &DroneParams) -> (f32, f32) {
        let mono = (left + right) * 0.5;
        let sr = self.sample_rate;

        // ── Envelope follower (attack fast, decay per params) ──────────────
        let attack_coeff = libm::expf(-1.0 / (ENV_ATTACK_S * sr));
        let abs_in = libm::fabsf(mono);
        if abs_in > self.env_follower {
            self.env_follower = abs_in + attack_coeff * (self.env_follower - abs_in);
        } else {
            let decay_coeff = libm::expf(-1.0 / (params.decay.max(0.001) * sr));
            self.env_follower *= decay_coeff;
        }

        // ── Zero-crossing frequency detector ──────────────────────────────
        self.zc_counter = self.zc_counter.saturating_add(1);
        if self.prev_sample <= 0.0 && mono > 0.0 && self.zc_counter > 10 {
            let freq = sr / self.zc_counter as f32;
            if (50.0..=2000.0).contains(&freq) {
                self.detected_freq_hz = freq;
            }
            self.zc_counter = 0;
        }
        self.prev_sample = mono;

        // ── Envelope gate ─────────────────────────────────────────────────
        let decay_coeff = libm::expf(-1.0 / (params.decay.max(0.001) * sr));
        if self.env_follower > GATE_THRESHOLD {
            self.env_gain = 1.0; // snap open
        } else {
            self.env_gain *= decay_coeff;
        }

        // ── Oscillator frequencies ────────────────────────────────────────
        let root_freq = self.detected_freq_hz;
        let freqs = [
            root_freq,
            root_freq * Self::semitone_ratio(7.0),
            root_freq * Self::semitone_ratio(12.0),
        ];

        // ── Detune LFO advance ────────────────────────────────────────────
        let detune_inc = DETUNE_LFO_HZ / sr;
        for dp in self.detune_phase.iter_mut() {
            *dp = (*dp + detune_inc) % 1.0;
        }

        // detune_cents → fractional ratio deviation for phase increment wobble
        let detune_deviation = libm::powf(2.0, params.detune / 1200.0) - 1.0;

        // ── Synthesize oscillators ────────────────────────────────────────
        let mix_factors = [
            params.root_mix / 100.0,
            params.fifth_mix / 100.0,
            params.octave_mix / 100.0,
        ];
        let mut tone_sum = 0.0f32;
        for i in 0..3 {
            let wobble = Self::sine(self.detune_phase[i]) * detune_deviation;
            let phase_inc = (freqs[i] / sr) * (1.0 + wobble);
            self.osc_phase[i] = (self.osc_phase[i] + phase_inc) % 1.0;
            tone_sum += Self::sine(self.osc_phase[i]) * mix_factors[i];
        }

        // ── Apply envelope and output gain ────────────────────────────────
        let output_gain = fast_db_to_linear(params.output_db);
        let wet = tone_sum * self.env_gain * output_gain;

        // Tones add to dry; each mix param controls individual voice level
        (left + wet, right + wet)
    }

    fn reset(&mut self) {
        self.osc_phase = [0.0, 0.333, 0.667];
        self.detune_phase = [0.0, 0.333, 0.667];
        self.env_gain = 0.0;
        self.detected_freq_hz = 220.0;
        self.zc_counter = 0;
        self.prev_sample = 0.0;
        self.env_follower = 0.0;
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.reset();
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_core::Effect;
    use sonido_core::kernel::Adapter;

    #[test]
    fn finite_output() {
        let mut kernel = DroneKernel::new(48000.0);
        let params = DroneParams::default();
        for i in 0..2048_u32 {
            let t = i as f32 / 48000.0;
            let s = libm::sinf(2.0 * core::f32::consts::PI * 220.0 * t) * 0.5;
            let (l, r) = kernel.process_stereo(s, s, &params);
            assert!(l.is_finite(), "L not finite at {i}: {l}");
            assert!(r.is_finite(), "R not finite at {i}: {r}");
        }
    }

    #[test]
    fn sustains_after_silence() {
        let mut kernel = DroneKernel::new(48000.0);
        let params = DroneParams::default();
        // Prime with signal
        for i in 0..2000_u32 {
            let t = i as f32 / 48000.0;
            let s = libm::sinf(2.0 * core::f32::consts::PI * 440.0 * t) * 0.5;
            kernel.process_stereo(s, s, &params);
        }
        // Sustain: drone tones still finite during decay
        for _ in 0..200 {
            let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
            assert!(l.is_finite(), "L not finite during sustain: {l}");
            assert!(r.is_finite(), "R not finite during sustain: {r}");
        }
    }

    #[test]
    fn params_descriptor_count() {
        assert_eq!(DroneParams::COUNT, 6);
        for i in 0..DroneParams::COUNT {
            assert!(
                DroneParams::descriptor(i).is_some(),
                "Missing descriptor at {i}"
            );
        }
        assert!(DroneParams::descriptor(DroneParams::COUNT).is_none());
    }

    #[test]
    fn adapter_wraps_as_effect() {
        let mut adapter = Adapter::new(DroneKernel::new(48000.0), 48000.0);
        adapter.reset();
        let out = adapter.process(0.3);
        assert!(out.is_finite(), "Adapter output must be finite, got {out}");
    }
}
