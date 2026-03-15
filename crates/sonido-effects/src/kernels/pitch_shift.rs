//! Granular pitch shifter kernel — overlapping Hann-windowed grain crossfade.
//!
//! `PitchShiftKernel` implements pitch shifting via two overlapping grains that
//! read from a circular delay buffer at a modified speed. The pitch ratio
//! (`2^(semitones/12 + cents/1200)`) controls read speed relative to write.
//! A Hann window crossfade between grains avoids clicks at grain boundaries.
//! Parameters are received via `&PitchShiftParams` each sample. Deployed via
//! [`KernelAdapter`](sonido_core::KernelAdapter) for desktop/plugin, or called
//! directly on embedded targets.
//!
//! # Signal Flow
//!
//! ```text
//! Input → [write to circular buffer]
//!       → [grain 0: Hann read at read_pos × ratio] \
//!       → [grain 1: Hann read at offset position  ]  → sum → wet/dry mix → output gain
//! ```
//!
//! # Algorithm
//!
//! Two grains run in phase quadrature (offset by `grain_size/2`). Each grain:
//! 1. Reads from the delay buffer at `read_pos` (fractional linear interpolation)
//! 2. Multiplies by `Hann(phase) = 0.5 * (1 - cos(2π * phase))`
//! 3. Advances `read_pos` by `ratio` (shift) and `phase` by `1/grain_size_samples`
//! 4. When `phase >= 1.0`, wraps to 0 and repositions at write cursor
//!
//! The two grains overlap continuously: when grain 0 is at phase 0.5 (peak),
//! grain 1 is just starting (phase 0), ensuring smooth crossfade with no holes.
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! let adapter = KernelAdapter::new(PitchShiftKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing)
//! let mut kernel = PitchShiftKernel::new(48000.0);
//! let params = PitchShiftParams::default();
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

extern crate alloc;

use alloc::vec::Vec;
use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::{
    ParamDescriptor, ParamFlags, ParamId, ParamUnit, fast_db_to_linear, wet_dry_mix,
};

/// Circular delay buffer length: ~100 ms at 48 kHz (4800 samples, power of 2 for fast modulo).
const BUF_LEN: usize = 8192; // ~170 ms at 48 kHz, power of 2

/// Number of grains. Quality=0 uses 2, Quality=1 uses 4.
const MAX_GRAINS: usize = 4;

/// Minimum grain size in samples.
const GRAIN_MIN_SAMPLES: usize = 480; // 10 ms at 48 kHz

/// Maximum grain size in samples.
const GRAIN_MAX_SAMPLES: usize = 2400; // 50 ms at 48 kHz

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`PitchShiftKernel`].
///
/// All values in **user-facing units** — the same units shown in GUIs and
/// stored in presets.
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `semitones` | semitones | −24–+24 | 0.0 |
/// | 1 | `cents` | cents | −50–+50 | 0.0 |
/// | 2 | `grain_ms` | ms | 10–50 | 20.0 |
/// | 3 | `mix_pct` | % | 0–100 | 100.0 |
/// | 4 | `quality` | index | 0–1 | 0 (2 grains) |
/// | 5 | `output_db` | dB | −60–+6 | 0.0 |
#[derive(Debug, Clone, Copy)]
pub struct PitchShiftParams {
    /// Pitch shift in semitones. Range: −24.0 to +24.0.
    pub semitones: f32,
    /// Fine pitch adjustment in cents. Range: −50.0 to +50.0.
    pub cents: f32,
    /// Grain size in milliseconds.
    ///
    /// Range: 10–50 ms. Larger grains give smoother output but more latency.
    /// Smaller grains follow transients better but can produce phasiness on sustained tones.
    pub grain_ms: f32,
    /// Wet/dry mix in percent.
    ///
    /// Range: 0.0 to 100.0 %. At 100 % the output is fully pitch-shifted.
    pub mix_pct: f32,
    /// Quality mode: 0 = 2 grains (lower CPU), 1 = 4 grains (smoother).
    pub quality: f32,
    /// Output level in decibels. Range: −60.0 to +6.0 dB.
    pub output_db: f32,
}

impl Default for PitchShiftParams {
    fn default() -> Self {
        Self {
            semitones: 0.0,
            cents: 0.0,
            grain_ms: 20.0,
            mix_pct: 100.0,
            quality: 0.0,
            output_db: 0.0,
        }
    }
}

impl KernelParams for PitchShiftParams {
    const COUNT: usize = 6;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(
                ParamDescriptor::custom("Semitones", "Semi", -24.0, 24.0, 0.0)
                    .with_unit(ParamUnit::None)
                    .with_step(1.0)
                    .with_id(ParamId(2400), "ps_semitones"),
            ),
            1 => Some(
                ParamDescriptor::custom("Cents", "Cents", -50.0, 50.0, 0.0)
                    .with_unit(ParamUnit::None)
                    .with_id(ParamId(2401), "ps_cents"),
            ),
            2 => Some(
                ParamDescriptor::custom("Grain Size", "Grain", 10.0, 50.0, 20.0)
                    .with_unit(ParamUnit::Milliseconds)
                    .with_id(ParamId(2402), "ps_grain_size"),
            ),
            3 => Some(ParamDescriptor::mix().with_id(ParamId(2403), "ps_mix")),
            4 => Some(
                ParamDescriptor::custom("Quality", "Quality", 0.0, 1.0, 0.0)
                    .with_unit(ParamUnit::None)
                    .with_step(1.0)
                    .with_id(ParamId(2404), "ps_quality")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(&["Standard", "High"]),
            ),
            5 => Some(
                sonido_core::gain::output_param_descriptor().with_id(ParamId(2405), "ps_output"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Interpolated, // semitones — 50 ms (avoids pitch jump artifacts)
            1 => SmoothingStyle::Interpolated, // cents — 50 ms
            2 => SmoothingStyle::Slow,         // grain_ms — 20 ms
            3 => SmoothingStyle::Standard,     // mix_pct — 10 ms
            4 => SmoothingStyle::None,         // quality — stepped, snap
            5 => SmoothingStyle::Fast,         // output_db — 5 ms
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.semitones,
            1 => self.cents,
            2 => self.grain_ms,
            3 => self.mix_pct,
            4 => self.quality,
            5 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.semitones = value,
            1 => self.cents = value,
            2 => self.grain_ms = value,
            3 => self.mix_pct = value,
            4 => self.quality = value,
            5 => self.output_db = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Grain
// ═══════════════════════════════════════════════════════════════════════════

/// A single pitch-shifting grain.
///
/// Each grain reads from the delay buffer at a fractional position, applies
/// a Hann window, and contributes to the output sum. When the grain completes
/// (`phase >= 1.0`), it resets to a new position aligned with the write cursor.
#[derive(Clone, Copy, Debug)]
struct Grain {
    /// Fractional read position in the delay buffer.
    read_pos: f32,
    /// Grain progress, 0.0 → 1.0. Advances by `1.0 / grain_size_samples` per sample.
    phase: f32,
    /// Whether this grain is active (should contribute to output).
    active: bool,
}

impl Grain {
    fn new() -> Self {
        Self {
            read_pos: 0.0,
            phase: 0.0,
            active: false,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP granular pitch shifter kernel.
///
/// Contains ONLY the mutable state required for audio processing:
/// - Separate circular delay buffers for left and right channels
/// - Up to 4 grains, each tracking a fractional read position and Hann phase
///
/// No `SmoothedParam`, no atomics, no platform awareness.
///
/// # Invariants
///
/// - `write_pos` is always in `[0, BUF_LEN)`.
/// - Each grain's `read_pos` stays in `[0, BUF_LEN)` via modular arithmetic.
/// - Grain `phase` advances uniformly; wrapping triggers repositioning.
pub struct PitchShiftKernel {
    /// Circular delay buffer for the left channel.
    buf_l: Vec<f32>,
    /// Circular delay buffer for the right channel.
    buf_r: Vec<f32>,
    /// Next write position in the circular buffers.
    write_pos: usize,
    /// Grain state array (up to MAX_GRAINS).
    grains: [Grain; MAX_GRAINS],
    /// Audio sample rate in Hz.
    sample_rate: f32,
}

impl PitchShiftKernel {
    /// Create a new pitch shift kernel at the given sample rate.
    pub fn new(sample_rate: f32) -> Self {
        let mut buf_l = Vec::with_capacity(BUF_LEN);
        buf_l.resize(BUF_LEN, 0.0);
        let mut buf_r = Vec::with_capacity(BUF_LEN);
        buf_r.resize(BUF_LEN, 0.0);

        // All grains start inactive so they are positioned correctly (relative to
        // write_pos and grain_size) on first process call rather than at construction
        // when the buffer is empty and grain_size is unknown.
        let grains = [Grain::new(); MAX_GRAINS];

        Self {
            buf_l,
            buf_r,
            write_pos: 0,
            grains,
            sample_rate,
        }
    }

    /// Compute the pitch ratio from semitones + cents.
    ///
    /// `ratio = 2^(semitones/12 + cents/1200)`
    #[inline]
    fn pitch_ratio(semitones: f32, cents: f32) -> f32 {
        libm::powf(2.0, semitones / 12.0 + cents / 1200.0)
    }

    /// Hann window at phase `t` in [0, 1].
    ///
    /// `w(t) = 0.5 * (1 - cos(2π * t))`
    #[inline]
    fn hann(phase: f32) -> f32 {
        0.5 * (1.0 - libm::cosf(2.0 * core::f32::consts::PI * phase))
    }

    /// Linearly interpolated read from a circular buffer.
    ///
    /// `pos` may be fractional. Wraps within `[0, BUF_LEN)`.
    #[inline]
    fn read_lerp(buf: &[f32], pos: f32) -> f32 {
        let len = BUF_LEN as f32;
        // Ensure pos is in [0, BUF_LEN)
        let pos = ((pos % len) + len) % len;
        let i0 = pos as usize;
        let i1 = (i0 + 1) % BUF_LEN;
        let frac = pos - i0 as f32;
        buf[i0] * (1.0 - frac) + buf[i1] * frac
    }

    /// Grain size in samples, clamped to valid range.
    #[inline]
    fn grain_size_samples(grain_ms: f32, sample_rate: f32) -> f32 {
        let samples = grain_ms / 1000.0 * sample_rate;
        let min = GRAIN_MIN_SAMPLES as f32;
        let max = GRAIN_MAX_SAMPLES as f32;
        if samples < min {
            min
        } else if samples > max {
            max
        } else {
            samples
        }
    }

    /// Number of active grains based on quality setting.
    #[inline]
    fn active_grain_count(quality: f32) -> usize {
        if quality >= 0.5 { 4 } else { 2 }
    }
}

impl DspKernel for PitchShiftKernel {
    type Params = PitchShiftParams;

    fn process_stereo(&mut self, left: f32, right: f32, params: &PitchShiftParams) -> (f32, f32) {
        let ratio = Self::pitch_ratio(params.semitones, params.cents);
        let grain_size = Self::grain_size_samples(params.grain_ms, self.sample_rate);
        let phase_inc = 1.0 / grain_size;
        let n_grains = Self::active_grain_count(params.quality);
        let write_pos_f = self.write_pos as f32;

        // ── Write input to delay buffers ──
        self.buf_l[self.write_pos] = left;
        self.buf_r[self.write_pos] = right;
        self.write_pos = (self.write_pos + 1) % BUF_LEN;

        // ── Accumulate grain outputs ──
        let mut wet_l = 0.0f32;
        let mut wet_r = 0.0f32;

        for i in 0..n_grains {
            let grain = &mut self.grains[i];

            // Position newly active grains on first use
            if !grain.active {
                // Stagger grains evenly: grain i starts at phase i/n_grains
                let phase_offset = i as f32 / n_grains as f32;
                grain.phase = phase_offset;
                // Place read cursor grain_size * (1 - phase_offset) samples behind write head
                // so the grain starts reading at the correct point in its window
                let lookback = grain_size * (1.0 - phase_offset);
                grain.read_pos = (write_pos_f - lookback + BUF_LEN as f32 * 2.0) % BUF_LEN as f32;
                grain.active = true;
            }

            let window = Self::hann(grain.phase);

            // Read from both channels at same position (dual-mono grain)
            let sample_l = Self::read_lerp(&self.buf_l, grain.read_pos) * window;
            let sample_r = Self::read_lerp(&self.buf_r, grain.read_pos) * window;

            wet_l += sample_l;
            wet_r += sample_r;

            // Advance grain read position by pitch ratio
            grain.read_pos = (grain.read_pos + ratio + BUF_LEN as f32) % BUF_LEN as f32;
            grain.phase += phase_inc;

            // Grain completed: wrap phase, reposition read cursor grain_size behind write head.
            // This guarantees the grain always reads from buffered (written) audio regardless
            // of pitch ratio — critical for upward shifts where ratio > 1.
            if grain.phase >= 1.0 {
                grain.phase -= 1.0;
                // Lag write head by one full grain so there's always valid audio to read
                grain.read_pos =
                    (self.write_pos as f32 - grain_size + BUF_LEN as f32 * 2.0) % BUF_LEN as f32;
            }
        }

        // ── Normalize by grain count to prevent amplitude buildup ──
        // With 2 grains, each Hann window sums to ~1.0 at any given phase, so
        // dividing by 1 is correct. With 4 grains the sum is ~2.0 — divide by 2.
        let grain_norm = if n_grains <= 2 { 1.0 } else { 2.0 };
        wet_l /= grain_norm;
        wet_r /= grain_norm;

        // ── Wet/dry mix → output gain ──
        let mix = params.mix_pct / 100.0;
        let output_gain = fast_db_to_linear(params.output_db);

        let out_l = wet_dry_mix(left, wet_l, mix) * output_gain;
        let out_r = wet_dry_mix(right, wet_r, mix) * output_gain;

        (out_l, out_r)
    }

    fn reset(&mut self) {
        for s in self.buf_l.iter_mut() {
            *s = 0.0;
        }
        for s in self.buf_r.iter_mut() {
            *s = 0.0;
        }
        self.write_pos = 0;
        // Mark all grains inactive so they are repositioned correctly on next process call
        for grain in self.grains.iter_mut() {
            grain.phase = 0.0;
            grain.read_pos = 0.0;
            grain.active = false;
        }
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
    use sonido_core::kernel::KernelAdapter;
    use sonido_core::{Effect, ParameterInfo};

    #[test]
    fn finite_output_always() {
        let mut kernel = PitchShiftKernel::new(48000.0);
        let params = PitchShiftParams::default();

        for i in 0..2048_u32 {
            let t = i as f32 / 48000.0;
            let s = libm::sinf(2.0 * core::f32::consts::PI * 440.0 * t) * 0.5;
            let (l, r) = kernel.process_stereo(s, s, &params);
            assert!(l.is_finite(), "L is not finite at sample {i}: {l}");
            assert!(r.is_finite(), "R is not finite at sample {i}: {r}");
        }
    }

    #[test]
    fn unity_ratio_passes_audio() {
        // With semitones=0 and a long warmup, wet output should be non-zero for a sine
        let mut kernel = PitchShiftKernel::new(48000.0);
        let params = PitchShiftParams {
            semitones: 0.0,
            cents: 0.0,
            mix_pct: 100.0,
            output_db: 0.0,
            ..Default::default()
        };

        let sr = 48000.0_f32;
        let freq = 440.0_f32;

        // Warmup: fill the buffer
        let mut energy_out = 0.0f32;
        for i in 0..4096_u32 {
            let t = i as f32 / sr;
            let s = libm::sinf(2.0 * core::f32::consts::PI * freq * t) * 0.5;
            let (l, _) = kernel.process_stereo(s, s, &params);
            if i > 2048 {
                energy_out += l * l;
            }
        }

        // Should have produced some energy
        assert!(
            energy_out > 0.01,
            "Unity pitch shift should produce non-zero output, energy={energy_out}"
        );
    }

    #[test]
    fn octave_up_produces_output() {
        let mut kernel = PitchShiftKernel::new(48000.0);
        let params = PitchShiftParams {
            semitones: 12.0, // octave up
            mix_pct: 100.0,
            output_db: 0.0,
            ..Default::default()
        };

        let sr = 48000.0_f32;
        let mut energy_out = 0.0f32;
        for i in 0..4096_u32 {
            let t = i as f32 / sr;
            let s = libm::sinf(2.0 * core::f32::consts::PI * 220.0 * t) * 0.5;
            let (l, _) = kernel.process_stereo(s, s, &params);
            if i > 2048 {
                energy_out += l * l;
            }
        }

        assert!(
            energy_out > 0.01,
            "Octave-up shift should produce non-zero output, energy={energy_out}"
        );
    }

    #[test]
    fn dry_mix_passes_input() {
        let mut kernel = PitchShiftKernel::new(48000.0);
        let params = PitchShiftParams {
            mix_pct: 0.0,
            output_db: 0.0,
            ..Default::default()
        };

        let (l, r) = kernel.process_stereo(0.7, -0.3, &params);
        assert!((l - 0.7).abs() < 1e-5, "Dry mix should pass L, got {l}");
        assert!((r - (-0.3)).abs() < 1e-5, "Dry mix should pass R, got {r}");
    }

    #[test]
    fn params_descriptor_count() {
        assert_eq!(PitchShiftParams::COUNT, 6);
        for i in 0..PitchShiftParams::COUNT {
            assert!(
                PitchShiftParams::descriptor(i).is_some(),
                "Missing descriptor at {i}"
            );
        }
        assert!(PitchShiftParams::descriptor(PitchShiftParams::COUNT).is_none());
    }

    #[test]
    fn params_ids_correct() {
        assert_eq!(PitchShiftParams::descriptor(0).unwrap().id, ParamId(2400));
        assert_eq!(PitchShiftParams::descriptor(1).unwrap().id, ParamId(2401));
        assert_eq!(PitchShiftParams::descriptor(2).unwrap().id, ParamId(2402));
        assert_eq!(PitchShiftParams::descriptor(3).unwrap().id, ParamId(2403));
        assert_eq!(PitchShiftParams::descriptor(4).unwrap().id, ParamId(2404));
        assert_eq!(PitchShiftParams::descriptor(5).unwrap().id, ParamId(2405));
    }

    #[test]
    fn quality_param_is_stepped() {
        let d = PitchShiftParams::descriptor(4).unwrap();
        assert!(
            d.flags.contains(ParamFlags::STEPPED),
            "quality must be STEPPED"
        );
    }

    #[test]
    fn all_param_combinations_finite() {
        let test_cases = [
            PitchShiftParams {
                semitones: -24.0,
                cents: -50.0,
                grain_ms: 10.0,
                mix_pct: 0.0,
                quality: 0.0,
                output_db: -60.0,
            },
            PitchShiftParams {
                semitones: 0.0,
                cents: 0.0,
                grain_ms: 20.0,
                mix_pct: 50.0,
                quality: 0.0,
                output_db: 0.0,
            },
            PitchShiftParams {
                semitones: 24.0,
                cents: 50.0,
                grain_ms: 50.0,
                mix_pct: 100.0,
                quality: 1.0,
                output_db: 6.0,
            },
            PitchShiftParams {
                semitones: 7.0,
                cents: 0.0,
                grain_ms: 30.0,
                mix_pct: 75.0,
                quality: 1.0,
                output_db: -3.0,
            },
        ];

        for p in &test_cases {
            let mut kernel = PitchShiftKernel::new(48000.0);
            for i in 0..1024_u32 {
                let t = i as f32 / 48000.0;
                let s = libm::sinf(2.0 * core::f32::consts::PI * 330.0 * t) * 0.3;
                let (l, r) = kernel.process_stereo(s, s, p);
                assert!(l.is_finite(), "L not finite for {:?}: {l}", p);
                assert!(r.is_finite(), "R not finite for {:?}: {r}", p);
            }
        }
    }

    #[test]
    fn adapter_wraps_as_effect() {
        let mut adapter = KernelAdapter::new(PitchShiftKernel::new(48000.0), 48000.0);
        adapter.reset();
        let out = adapter.process(0.3);
        assert!(out.is_finite(), "Adapter output must be finite, got {out}");
    }

    #[test]
    fn adapter_param_count() {
        let adapter = KernelAdapter::new(PitchShiftKernel::new(48000.0), 48000.0);
        assert_eq!(adapter.param_count(), 6);
        assert_eq!(adapter.param_info(0).unwrap().name, "Semitones");
        assert_eq!(adapter.param_info(5).unwrap().name, "Output");
    }
}
