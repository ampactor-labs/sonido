//! Clean preamp kernel — gain staging with tone control and soft clipping.
//!
//! `PreampKernel` owns DSP state (tone biquad filter, sample rate, cached
//! coefficient tracking). Parameters are received via `&PreampParams` each
//! sample. Deployed via [`Adapter`](sonido_core::kernel::Adapter) for
//! desktop/plugin, or called directly on embedded targets.
//!
//! # Signal Flow
//!
//! ```text
//! Input → Gain (linear) → Soft Clip at threshold → Tone EQ → Soft Limit → Output Level
//! ```
//!
//! The soft clip fires only when the signal exceeds the internal headroom threshold
//! (fixed at +20 dB ≈ 10.0 linear). For typical signals at reasonable gain settings
//! the preamp stage is transparent — clean gain with tonal shaping.
//!
//! # DSP Theory
//!
//! ## Gain Stage
//!
//! A linear gain multiplier maps `gain_db` → `10^(gain_db/20)` via
//! [`fast_db_to_linear`]. The `gain_db` range
//! (0–40 dB) intentionally starts at unity; this is a boost-only preamp.
//!
//! ## Soft Clipping
//!
//! When the gained signal exceeds the internal threshold `T` (10.0 linear = +20 dBFS),
//! the output is shaped by:
//!
//! ```text
//! y = T · sign(x) · (1 + tanh(|x|/T − 1))
//! ```
//!
//! This is a smooth, asymptotically bounded waveshaper with a gentle knee at `T`.
//! Below `T` the transfer function is linear (y = x). Above `T` the `tanh` arm
//! adds a progressively stronger compression curve, approaching `2T` asymptotically.
//!
//! Reference: Zölzer, "DAFX: Digital Audio Effects", 2nd ed., Chapter 5 (Clipping).
//!
//! ## Tone EQ
//!
//! A peaking EQ biquad at 3 kHz with Q = 0.8 boosts/cuts the upper-mid frequency
//! range. This models the classic "presence" or "bright" control found on valve
//! preamps where cathode bypass capacitors and transformer resonances shape the
//! frequency response. Positive `tone_db` values add sparkle/bite; negative values
//! warm/darken the sound.
//!
//! Reference: R. Bristow-Johnson, "Audio EQ Cookbook", peaking EQ coefficients.
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! use sonido_core::kernel::Adapter;
//! let adapter = Adapter::new(PreampKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing, ADCs are hardware-filtered)
//! let mut kernel = PreampKernel::new(48000.0);
//! let params = PreampParams::from_knobs(adc_gain, adc_tone, adc_output);
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

use libm::tanhf;

use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::math::soft_limit;
use sonido_core::{
    Biquad, Cached, ParamDescriptor, ParamId, ParamUnit, fast_db_to_linear, peaking_eq_coefficients,
};

// ── Constants ──

/// Internal headroom threshold: +20 dBFS = 10.0 linear.
///
/// Soft clipping engages when the gained signal exceeds this level.
/// Below this threshold the preamp is transparent (y = x).
const CLIP_THRESHOLD: f32 = 10.0; // 10^(20/20)

/// Center frequency for the tone peaking EQ in Hz.
const TONE_CENTER_HZ: f32 = 3000.0;

/// Q factor for the tone peaking EQ.
///
/// 0.8 gives a moderately broad peak suitable for "presence" shaping
/// without introducing a tight, nasal resonance.
const TONE_Q: f32 = 0.8;

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`PreampKernel`].
///
/// All values in **user-facing units** — the same units shown in GUIs and
/// stored in presets. The kernel converts internally as needed.
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `gain_db` | dB | 0–40 | 0.0 |
/// | 1 | `tone_db` | dB | −12–12 | 0.0 |
/// | 2 | `output_db` | dB | −20–20 | 0.0 |
#[derive(Debug, Clone, Copy)]
pub struct PreampParams {
    /// Input gain before the soft-clip stage, in decibels.
    ///
    /// Range: 0.0–40.0 dB. At 0 dB the gain stage is unity.
    pub gain_db: f32,
    /// Peaking EQ gain at `TONE_CENTER_HZ` (3 kHz), in decibels.
    ///
    /// Range: −12.0–12.0 dB. Positive values add presence/brightness;
    /// negative values warm/darken the output.
    pub tone_db: f32,
    /// Output level trim after all processing, in decibels.
    ///
    /// Range: −20.0–20.0 dB. Default 0.0 (unity).
    pub output_db: f32,
}

impl Default for PreampParams {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            tone_db: 0.0,
            output_db: 0.0,
        }
    }
}

impl PreampParams {
    /// Creates parameters from normalized 0–1 knob readings.
    ///
    /// Curves (logarithmic for frequency/time, linear for percentage) are
    /// derived from [`ParamDescriptor`] — same mapping as GUI and plugin hosts.
    pub fn from_knobs(gain: f32, tone: f32, output: f32) -> Self {
        Self::from_normalized(&[gain, tone, output])
    }
}

impl KernelParams for PreampParams {
    const COUNT: usize = 3;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(
                ParamDescriptor::gain_db("Gain", "Gain", 0.0, 40.0, 0.0)
                    .with_id(ParamId(100), "pre_gain"),
            ),
            1 => Some(
                ParamDescriptor::custom("Tone", "Tone", -12.0, 12.0, 0.0)
                    .with_unit(ParamUnit::Decibels)
                    .with_step(0.5)
                    .with_id(ParamId(101), "pre_tone"),
            ),
            2 => Some(
                sonido_core::gain::output_param_descriptor().with_id(ParamId(102), "pre_output"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Standard, // gain — 10 ms, responsive but click-free
            1 => SmoothingStyle::Slow,     // tone — filter coefficient, avoid zipper
            2 => SmoothingStyle::Standard, // output level
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.gain_db,
            1 => self.tone_db,
            2 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.gain_db = value,
            1 => self.tone_db = value,
            2 => self.output_db = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP clean preamp kernel.
///
/// Contains ONLY the mutable state required for audio processing:
/// - Tone biquad filters (left and right channels)
/// - Cached tone coefficient tracking (avoids per-sample biquad recalculation)
/// - Sample rate
///
/// No `SmoothedParam`, no `AtomicU32`, no platform awareness.
///
/// ## Signal Flow
///
/// ```text
/// L ──► × gain ──► soft_clip ──► tone_biquad_L ──► soft_limit ──► × output ──► L
/// R ──► × gain ──► soft_clip ──► tone_biquad_R ──► soft_limit ──► × output ──► R
/// ```
///
/// This is a dual-mono effect: L and R are processed independently with
/// identical gain/EQ settings. `is_true_stereo()` returns `false`.
pub struct PreampKernel {
    /// Current sample rate — required for biquad coefficient calculation.
    sample_rate: f32,

    /// Tone peaking EQ biquad for the left channel.
    tone_filter_l: Biquad,

    /// Tone peaking EQ biquad for the right channel.
    tone_filter_r: Biquad,

    /// Cached tone biquad coefficients `[b0, b1, b2, a0, a1, a2]`.
    ///
    /// Recomputed only when `tone_db` changes by more than 1e-4 dB,
    /// avoiding unnecessary per-sample trigonometric computation.
    tone_cache: Cached<[f32; 6]>,
}

impl PreampKernel {
    /// Create a new clean preamp kernel initialized for `sample_rate`.
    ///
    /// Both tone filters are initialized with flat (0 dB) coefficients.
    ///
    /// # Parameters
    /// - `sample_rate`: Audio sample rate in Hz (e.g. 44100.0, 48000.0, 96000.0).
    pub fn new(sample_rate: f32) -> Self {
        let (b0, b1, b2, a0, a1, a2) =
            peaking_eq_coefficients(TONE_CENTER_HZ, TONE_Q, 0.0, sample_rate);
        let initial_coeffs = [b0, b1, b2, a0, a1, a2];
        let mut tone_filter_l = Biquad::new();
        let mut tone_filter_r = Biquad::new();
        tone_filter_l.set_coefficients(b0, b1, b2, a0, a1, a2);
        tone_filter_r.set_coefficients(b0, b1, b2, a0, a1, a2);
        let mut tone_cache = Cached::new(initial_coeffs, 1);
        // Prime cache so first process() call matches the already-set coefficients.
        tone_cache.update(&[0.0], 1e-4, |inputs| {
            let (b0, b1, b2, a0, a1, a2) =
                peaking_eq_coefficients(TONE_CENTER_HZ, TONE_Q, inputs[0], sample_rate);
            [b0, b1, b2, a0, a1, a2]
        });
        Self {
            sample_rate,
            tone_filter_l,
            tone_filter_r,
            tone_cache,
        }
    }

    /// Apply the preamp's soft-clip transfer function to a single sample.
    ///
    /// Below [`CLIP_THRESHOLD`] (`T`) the function is linear (transparent).
    /// Above the threshold a `tanh`-based curve provides smooth limiting:
    ///
    /// ```text
    /// y = x                                           if |x| ≤ T
    /// y = T · sign(x) · (1 + tanh(|x|/T − 1))        if |x| > T
    /// ```
    ///
    /// The `tanh` function asymptotically approaches `T · (1 + 1)` = `2T` as
    /// the input level increases, so the output is bounded.
    ///
    /// Reference: Zölzer, "DAFX: Digital Audio Effects", 2nd ed., Chapter 5.
    #[inline]
    fn soft_clip_sample(x: f32) -> f32 {
        if x.abs() > CLIP_THRESHOLD {
            CLIP_THRESHOLD * x.signum() * (1.0 + tanhf(x.abs() / CLIP_THRESHOLD - 1.0))
        } else {
            x
        }
    }
}

impl DspKernel for PreampKernel {
    type Params = PreampParams;

    /// Process a stereo sample pair through the clean preamp.
    ///
    /// Per-sample processing order:
    /// 1. Coefficient update if `tone_db` changed by > 0.001 dB
    /// 2. Unit conversion: dB → linear for gain and output
    /// 3. Drive: multiply by linear gain
    /// 4. Soft clip: smooth saturation at internal headroom threshold
    /// 5. Tone EQ: peaking biquad at 3 kHz (L and R independently)
    /// 6. Safety limiter: `soft_limit` at 1.0 ceiling
    /// 7. Output level scaling
    #[inline]
    fn process_stereo(&mut self, left: f32, right: f32, params: &PreampParams) -> (f32, f32) {
        // ── Coefficient update (only when tone_db changes beyond epsilon) ──
        let coeffs = *self.tone_cache.update(&[params.tone_db], 1e-4, |inputs| {
            let (b0, b1, b2, a0, a1, a2) =
                peaking_eq_coefficients(TONE_CENTER_HZ, TONE_Q, inputs[0], self.sample_rate);
            [b0, b1, b2, a0, a1, a2]
        });
        self.tone_filter_l.set_coefficients(
            coeffs[0], coeffs[1], coeffs[2], coeffs[3], coeffs[4], coeffs[5],
        );
        self.tone_filter_r.set_coefficients(
            coeffs[0], coeffs[1], coeffs[2], coeffs[3], coeffs[4], coeffs[5],
        );

        // ── Unit conversion (user-facing → internal) ──
        let gain = fast_db_to_linear(params.gain_db);
        let output = fast_db_to_linear(params.output_db);

        // ── Gain stage ──
        let driven_l = left * gain;
        let driven_r = right * gain;

        // ── Soft clip at headroom threshold ──
        let clipped_l = Self::soft_clip_sample(driven_l);
        let clipped_r = Self::soft_clip_sample(driven_r);

        // ── Tone EQ (peaking at 3 kHz) ──
        let toned_l = self.tone_filter_l.process(clipped_l);
        let toned_r = self.tone_filter_r.process(clipped_r);

        // ── Safety limiter → Output level ──
        (
            soft_limit(toned_l, 1.0) * output,
            soft_limit(toned_r, 1.0) * output,
        )
    }

    fn reset(&mut self) {
        self.tone_filter_l.clear();
        self.tone_filter_r.clear();
        // Invalidate coefficient cache so it recomputes on the next process() call.
        self.tone_cache.invalidate();
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        // Invalidate so coefficients recompute at new sample rate on next process().
        self.tone_cache.invalidate();
    }

    fn latency_samples(&self) -> usize {
        0 // Zero latency: no look-ahead, no block processing delay
    }

    /// Dual-mono: L and R use the same settings but independent filter state.
    ///
    /// Returns `false` because the two channels are not cross-correlated;
    /// the only stereo "interaction" is that they share coefficient computation.
    fn is_true_stereo(&self) -> bool {
        false
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_core::kernel::Adapter;
    use sonido_core::{Effect, KernelParams, ParameterInfo};

    // Zero-config kernel invariant tests (finite output, reset, morph, descriptors, extreme inputs)
    crate::test_kernel!(PreampKernel, PreampParams);

    // ── Kernel unit tests ──

    #[test]
    fn silence_in_silence_out() {
        let mut kernel = PreampKernel::new(48000.0);
        let params = PreampParams::default();

        let (left, right) = kernel.process_stereo(0.0, 0.0, &params);
        assert!(left.abs() < 1e-6, "Expected silence on left, got {left}");
        assert!(right.abs() < 1e-6, "Expected silence on right, got {right}");
    }

    #[test]
    fn no_nan_or_inf() {
        let mut kernel = PreampKernel::new(48000.0);
        let params = PreampParams {
            gain_db: 20.0,
            tone_db: 6.0,
            output_db: -6.0,
        };

        // Use a sine-like test signal (simple increment for no_std compatibility)
        for i in 0..1000 {
            // Alternating polarity signal covering typical audio range
            let phase = (i as f32) * 0.01;
            // Simple approximation: triangle wave between -0.5 and 0.5
            let sample = if i % 200 < 100 {
                (i % 100) as f32 * 0.01 - 0.5
            } else {
                0.5 - (i % 100) as f32 * 0.01
            };
            let _ = phase; // suppress unused warning

            let (l, r) = kernel.process_stereo(sample, -sample, &params);
            assert!(
                !l.is_nan() && !l.is_infinite(),
                "Left output NaN/Inf at sample {i}: {l}"
            );
            assert!(
                !r.is_nan() && !r.is_infinite(),
                "Right output NaN/Inf at sample {i}: {r}"
            );
        }
    }

    #[test]
    fn params_descriptor_count() {
        assert_eq!(PreampParams::COUNT, 3);

        // Verify all three descriptors are present
        assert!(
            PreampParams::descriptor(0).is_some(),
            "gain descriptor missing"
        );
        assert!(
            PreampParams::descriptor(1).is_some(),
            "tone descriptor missing"
        );
        assert!(
            PreampParams::descriptor(2).is_some(),
            "output descriptor missing"
        );
        assert!(
            PreampParams::descriptor(3).is_none(),
            "index 3 should be None"
        );

        // Verify ParamId values match classic effect
        let gain_desc = PreampParams::descriptor(0).unwrap();
        assert_eq!(gain_desc.id, ParamId(100), "gain must be ParamId(100)");

        let tone_desc = PreampParams::descriptor(1).unwrap();
        assert_eq!(tone_desc.id, ParamId(101), "tone must be ParamId(101)");

        let output_desc = PreampParams::descriptor(2).unwrap();
        assert_eq!(output_desc.id, ParamId(102), "output must be ParamId(102)");

        // Verify ranges
        assert_eq!(gain_desc.min, 0.0);
        assert_eq!(gain_desc.max, 40.0);
        assert_eq!(tone_desc.min, -12.0);
        assert_eq!(tone_desc.max, 12.0);
    }

    #[test]
    fn drive_increases_amplitude() {
        // Higher gain should produce higher output amplitude on a quiet input
        // (where the signal stays below the soft clip threshold).
        let quiet_input = 0.001; // Well below soft-clip threshold at any tested gain

        let low_params = PreampParams {
            gain_db: 0.0,
            tone_db: 0.0,
            output_db: 0.0,
        };
        let high_params = PreampParams {
            gain_db: 20.0,
            tone_db: 0.0,
            output_db: 0.0,
        };

        let mut kernel_low = PreampKernel::new(48000.0);
        let (low_out, _) = kernel_low.process_stereo(quiet_input, quiet_input, &low_params);

        let mut kernel_high = PreampKernel::new(48000.0);
        let (high_out, _) = kernel_high.process_stereo(quiet_input, quiet_input, &high_params);

        assert!(
            high_out.abs() > low_out.abs(),
            "Higher gain_db should produce higher amplitude: low={low_out}, high={high_out}"
        );
    }

    #[test]
    fn adapter_wraps_as_effect() {
        let kernel = PreampKernel::new(48000.0);
        let mut adapter = Adapter::new(kernel, 48000.0);

        // Should behave as a standard Effect
        adapter.reset();
        let output = adapter.process(0.3);
        assert!(!output.is_nan(), "Adapter output is NaN");
        assert!(output.is_finite(), "Adapter output is infinite");
    }

    #[test]
    fn adapter_param_info_matches() {
        let kernel = PreampKernel::new(48000.0);
        let adapter = Adapter::new(kernel, 48000.0);

        assert_eq!(adapter.param_count(), 3);

        let gain_info = adapter.param_info(0).unwrap();
        assert_eq!(gain_info.name, "Gain");
        assert_eq!(gain_info.min, 0.0);
        assert_eq!(gain_info.max, 40.0);

        let tone_info = adapter.param_info(1).unwrap();
        assert_eq!(tone_info.name, "Tone");
        assert_eq!(tone_info.min, -12.0);
        assert_eq!(tone_info.max, 12.0);

        let output_info = adapter.param_info(2).unwrap();
        assert_eq!(output_info.name, "Output");

        // Index 3 should not exist
        assert!(adapter.param_info(3).is_none());
    }

    #[test]
    fn morph_produces_valid_output() {
        let clean = PreampParams {
            gain_db: 0.0,
            tone_db: 0.0,
            output_db: 0.0,
        };
        let driven = PreampParams {
            gain_db: 30.0,
            tone_db: 6.0,
            output_db: -6.0,
        };

        let mut kernel = PreampKernel::new(48000.0);
        for i in 0..=10 {
            let t = i as f32 / 10.0;
            let morphed = PreampParams::lerp(&clean, &driven, t);

            // Process a few samples at each morph point
            for s in 0..5 {
                let sample = if s % 2 == 0 { 0.3_f32 } else { -0.3_f32 };
                let (l, r) = kernel.process_stereo(sample, -sample, &morphed);
                assert!(
                    l.is_finite() && r.is_finite(),
                    "Morph at t={t}: NaN/Inf output (l={l}, r={r})"
                );
            }
            kernel.reset();
        }
    }

    // ── Behavioral tests ──

    #[test]
    fn unity_gain_passes_signal() {
        let mut kernel = PreampKernel::new(48000.0);
        let params = PreampParams {
            gain_db: 0.0,
            tone_db: 0.0,
            output_db: 0.0,
        };

        // Process many samples to let the biquad transient settle
        for _ in 0..100 {
            kernel.process_stereo(0.5, 0.5, &params);
        }
        let (out_l, _) = kernel.process_stereo(0.5, 0.5, &params);
        // At 0 dB gain, 0 dB tone, 0 dB output: output ≈ input (biquad flat)
        assert!(
            (out_l - 0.5).abs() < 0.01,
            "Unity gain should pass signal: expected ~0.5, got {out_l}"
        );
    }

    #[test]
    fn soft_clip_activates_at_high_gain() {
        // With high gain and a normal input, the signal should be soft-clipped
        // and the output should remain finite and bounded.
        let mut kernel = PreampKernel::new(48000.0);
        let params = PreampParams {
            gain_db: 40.0, // 100× linear — will push past threshold
            tone_db: 0.0,
            output_db: 0.0,
        };

        let (out_l, out_r) = kernel.process_stereo(0.5, 0.5, &params);
        assert!(
            out_l.is_finite(),
            "Soft clip should bound output: got {out_l}"
        );
        assert!(
            out_r.is_finite(),
            "Soft clip should bound output: got {out_r}"
        );
        // Output should be substantially less than the raw gained value (50.0)
        assert!(
            out_l < 5.0,
            "Output should be limited by soft clip: {out_l}"
        );
    }

    #[test]
    fn tone_affects_frequency_balance() {
        // High tone boost vs cut should produce different RMS levels on a
        // signal containing energy near the tone center (3 kHz).
        // DC signals converge to the same level through a peaking EQ.
        let params_bright = PreampParams {
            gain_db: 0.0,
            tone_db: 12.0,
            output_db: 0.0,
        };
        let params_dark = PreampParams {
            gain_db: 0.0,
            tone_db: -12.0,
            output_db: 0.0,
        };

        let sr = 48000.0;
        let freq = 3000.0; // near tone center
        let mut kernel_bright = PreampKernel::new(sr);
        let mut kernel_dark = PreampKernel::new(sr);

        let mut rms_bright = 0.0f32;
        let mut rms_dark = 0.0f32;
        let n = 1000;
        for i in 0..n {
            let phase = 2.0 * core::f32::consts::PI * freq * (i as f32) / sr;
            let input = libm::sinf(phase) * 0.3;
            let (bl, _) = kernel_bright.process_stereo(input, input, &params_bright);
            let (dl, _) = kernel_dark.process_stereo(input, input, &params_dark);
            rms_bright += bl * bl;
            rms_dark += dl * dl;
        }
        rms_bright = libm::sqrtf(rms_bright / n as f32);
        rms_dark = libm::sqrtf(rms_dark / n as f32);

        assert!(
            rms_bright > rms_dark * 1.5,
            "Tone boost should be louder than cut at center freq: bright={rms_bright}, dark={rms_dark}"
        );
    }

    #[test]
    fn reset_clears_filter_state() {
        let mut kernel = PreampKernel::new(48000.0);
        let params = PreampParams {
            gain_db: 20.0,
            tone_db: 12.0,
            output_db: 0.0,
        };

        // Prime the filter with non-zero signal
        for _ in 0..100 {
            kernel.process_stereo(0.5, 0.5, &params);
        }

        // Reset should clear filter memories
        kernel.reset();

        // After reset with silence input, output should be near zero
        let (out_l, out_r) = kernel.process_stereo(0.0, 0.0, &params);
        assert!(
            out_l.abs() < 1e-6,
            "After reset, silence should yield silence: got {out_l}"
        );
        assert!(
            out_r.abs() < 1e-6,
            "After reset, silence should yield silence: got {out_r}"
        );
    }

    #[test]
    fn from_knobs_maps_ranges() {
        // Verify range mapping for from_knobs()

        // 0.0 on gain → 0.0 dB
        let p = PreampParams::from_knobs(0.0, 0.5, 0.5);
        assert!((p.gain_db - 0.0).abs() < 0.01, "gain knob 0.0 → 0 dB");

        // 1.0 on gain → 40.0 dB
        let p = PreampParams::from_knobs(1.0, 0.5, 0.5);
        assert!((p.gain_db - 40.0).abs() < 0.01, "gain knob 1.0 → 40 dB");

        // 0.5 on tone → 0.0 dB (flat)
        let p = PreampParams::from_knobs(0.5, 0.5, 0.5);
        assert!((p.tone_db - 0.0).abs() < 0.01, "tone knob 0.5 → 0 dB");

        // 0.0 on tone → -12.0 dB
        let p = PreampParams::from_knobs(0.5, 0.0, 0.5);
        assert!((p.tone_db - (-12.0)).abs() < 0.01, "tone knob 0.0 → -12 dB");

        // 1.0 on tone → 12.0 dB
        let p = PreampParams::from_knobs(0.5, 1.0, 0.5);
        assert!((p.tone_db - 12.0).abs() < 0.01, "tone knob 1.0 → +12 dB");

        // 0.5 on output → -6 + 0.5*12 = 0.0 dB  (output_param_descriptor range is -6..+6)
        let p = PreampParams::from_knobs(0.5, 0.5, 0.5);
        assert!((p.output_db - 0.0).abs() < 0.01, "output knob 0.5 → 0 dB");

        // 0.0 on output → -6.0 dB
        let p = PreampParams::from_knobs(0.5, 0.5, 0.0);
        assert!(
            (p.output_db - (-6.0)).abs() < 0.01,
            "output knob 0.0 → -6 dB"
        );

        // 1.0 on output → +6.0 dB  (OUTPUT_MAX_DB = 6.0)
        let p = PreampParams::from_knobs(0.5, 0.5, 1.0);
        assert!((p.output_db - 6.0).abs() < 0.01, "output knob 1.0 → +6 dB");
    }

    #[test]
    fn params_get_set_roundtrip() {
        let mut params = PreampParams::default();

        params.set(0, 15.0);
        params.set(1, -6.0);
        params.set(2, 3.0);

        assert!(
            (params.get(0) - 15.0).abs() < 0.01,
            "gain get/set roundtrip"
        );
        assert!(
            (params.get(1) - (-6.0)).abs() < 0.01,
            "tone get/set roundtrip"
        );
        assert!(
            (params.get(2) - 3.0).abs() < 0.01,
            "output get/set roundtrip"
        );

        // Out-of-range index should return 0
        assert_eq!(params.get(99), 0.0);
    }

    #[test]
    fn params_lerp_continuous() {
        let a = PreampParams {
            gain_db: 0.0,
            tone_db: -12.0,
            output_db: -20.0,
        };
        let b = PreampParams {
            gain_db: 40.0,
            tone_db: 12.0,
            output_db: 20.0,
        };

        let mid = PreampParams::lerp(&a, &b, 0.5);
        assert!(
            (mid.gain_db - 20.0).abs() < 0.1,
            "gain lerp at 0.5: expected 20, got {}",
            mid.gain_db
        );
        assert!(
            (mid.tone_db - 0.0).abs() < 0.1,
            "tone lerp at 0.5: expected 0, got {}",
            mid.tone_db
        );
        assert!(
            (mid.output_db - 0.0).abs() < 0.1,
            "output lerp at 0.5: expected 0, got {}",
            mid.output_db
        );

        // Boundary cases
        let at_a = PreampParams::lerp(&a, &b, 0.0);
        assert!((at_a.gain_db - 0.0).abs() < 0.01);
        let at_b = PreampParams::lerp(&a, &b, 1.0);
        assert!((at_b.gain_db - 40.0).abs() < 0.01);
    }

    #[test]
    fn adapter_set_get_roundtrip() {
        let kernel = PreampKernel::new(48000.0);
        let mut adapter = Adapter::new(kernel, 48000.0);

        adapter.set_param(0, 20.0); // gain
        assert!(
            (adapter.get_param(0) - 20.0).abs() < 0.01,
            "gain set/get roundtrip"
        );

        adapter.set_param(1, -6.0); // tone
        assert!(
            (adapter.get_param(1) - (-6.0)).abs() < 0.01,
            "tone set/get roundtrip"
        );

        adapter.set_param(2, -3.0); // output
        assert!(
            (adapter.get_param(2) - (-3.0)).abs() < 0.01,
            "output set/get roundtrip"
        );
    }

    #[test]
    fn adapter_snapshot_roundtrip() {
        let kernel = PreampKernel::new(48000.0);
        let mut adapter = Adapter::new(kernel, 48000.0);

        adapter.set_param(0, 18.0);
        adapter.set_param(1, 4.0);
        adapter.set_param(2, -3.0); // within output_param_descriptor range [-6, 6]

        let saved = adapter.snapshot();
        assert!((saved.gain_db - 18.0).abs() < 0.01);
        assert!((saved.tone_db - 4.0).abs() < 0.01);
        assert!((saved.output_db - (-3.0)).abs() < 0.01);

        // Load into a fresh adapter
        let kernel2 = PreampKernel::new(48000.0);
        let mut adapter2 = Adapter::new(kernel2, 48000.0);
        adapter2.load_snapshot(&saved);

        assert!((adapter2.get_param(0) - 18.0).abs() < 0.01);
        assert!((adapter2.get_param(1) - 4.0).abs() < 0.01);
        assert!((adapter2.get_param(2) - (-3.0)).abs() < 0.01);
    }
}
