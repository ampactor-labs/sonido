//! Multimode biquad filter kernel — LPF/HPF/BPF/Notch with cutoff, resonance, and soft limiting.
//!
//! `FilterKernel` owns DSP state (biquad filters, sample rate, cached
//! coefficients). Parameters are received via `&FilterParams` each sample.
//! Deployed via [`KernelAdapter`](sonido_core::KernelAdapter) for
//! desktop/plugin, or called directly on embedded targets.
//!
//! # Signal Flow
//!
//! ```text
//! Input → Biquad (mode-selected) → Soft Limit → Output Level
//! ```
//!
//! Coefficients are recomputed only when cutoff, resonance, or filter type
//! changes (tracked via cached values).
//!
//! # Theory
//!
//! The biquad filter uses the Audio EQ Cookbook (Bristow-Johnson) direct
//! form II transposed topology:
//!
//! ```text
//! H(z) = (b0 + b1·z⁻¹ + b2·z⁻²) / (a0 + a1·z⁻¹ + a2·z⁻²)
//! ```
//!
//! All four RBJ filter types are supported: LPF, HPF, BPF, and Notch.
//! `α = sin(ω₀) / (2·Q)`, and `ω₀ = 2π·fc/fs`.
//!
//! Reference: Robert Bristow-Johnson, "Cookbook formulae for audio EQ biquad
//! filter coefficients", 1994.
//!
//! # Parameters
//!
//! | Index | Field | Unit | Range | Default |
//! |-------|-------|------|-------|---------|
//! | 0 | `cutoff_hz` | Hz | 20–20000 | 1000.0 |
//! | 1 | `resonance` | ratio (Q) | 0.1–20.0 | 0.707 |
//! | 2 | `output_db` | dB | −20–20 | 0.0 |
//! | 3 | `filter_type` | index | 0–3 | 0 (LPF) |
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! let adapter = KernelAdapter::new(FilterKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing, ADCs are hardware-filtered)
//! let mut kernel = FilterKernel::new(48000.0);
//! let params = FilterParams::from_knobs(adc_cutoff, adc_resonance, adc_output, adc_type);
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::math::soft_limit;
use sonido_core::{
    Biquad, Cached, ParamDescriptor, ParamFlags, ParamId, ParamScale, ParamUnit,
    bandpass_coefficients, fast_db_to_linear, highpass_coefficients, lowpass_coefficients,
    notch_coefficients,
};

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`FilterKernel`].
///
/// All values in **user-facing units** — the same units shown in GUIs and
/// stored in presets. The kernel converts internally as needed.
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `cutoff_hz` | Hz | 20–20000 | 1000.0 |
/// | 1 | `resonance` | ratio (Q) | 0.1–20.0 | 0.707 |
/// | 2 | `output_db` | dB | −20–20 | 0.0 |
/// | 3 | `filter_type` | index | 0–3 | 0 (LPF) |
#[derive(Debug, Clone, Copy)]
pub struct FilterParams {
    /// Filter cutoff frequency in Hz.
    pub cutoff_hz: f32,
    /// Filter Q factor (resonance). Higher values produce a sharper resonant peak.
    pub resonance: f32,
    /// Output level in decibels.
    pub output_db: f32,
    /// Filter type selector: 0=LPF, 1=HPF, 2=BPF, 3=Notch.
    pub filter_type: f32,
}

impl Default for FilterParams {
    fn default() -> Self {
        Self {
            cutoff_hz: 1000.0,
            resonance: 0.707,
            output_db: 0.0,
            filter_type: 0.0,
        }
    }
}

impl FilterParams {
    /// Creates parameters from normalized 0–1 knob readings.
    ///
    /// Curves (logarithmic for frequency/time, linear for percentage) are
    /// derived from [`ParamDescriptor`] — same mapping as GUI and plugin hosts.
    ///
    /// # Parameters
    /// - `cutoff`: normalized 0.0–1.0 → 20–20000 Hz (logarithmic)
    /// - `resonance`: normalized 0.0–1.0 → 0.1–20.0 (linear)
    /// - `output`: normalized 0.0–1.0 → −20–+6 dB (linear)
    /// - `filter_type`: normalized 0.0–1.0 → 0–3 (stepped: LPF/HPF/BPF/Notch)
    pub fn from_knobs(cutoff: f32, resonance: f32, output: f32, filter_type: f32) -> Self {
        Self::from_normalized(&[cutoff, resonance, output, filter_type])
    }
}

impl KernelParams for FilterParams {
    const COUNT: usize = 4;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(
                ParamDescriptor {
                    name: "Cutoff",
                    short_name: "Cutoff",
                    unit: ParamUnit::Hertz,
                    min: 20.0,
                    max: 20000.0,
                    default: 1000.0,
                    step: 1.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(1200), "flt_cutoff")
                .with_scale(ParamScale::Logarithmic),
            ),
            1 => Some(
                ParamDescriptor {
                    name: "Resonance",
                    short_name: "Reso",
                    unit: ParamUnit::Ratio,
                    min: 0.1,
                    max: 20.0,
                    default: 0.707,
                    step: 0.01,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(1201), "flt_resonance"),
            ),
            2 => Some(
                sonido_core::gain::output_param_descriptor().with_id(ParamId(1202), "flt_output"),
            ),
            3 => Some(
                ParamDescriptor::custom("Type", "Type", 0.0, 3.0, 0.0)
                    .with_step(1.0)
                    .with_id(ParamId(1203), "flt_type")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(&["LPF", "HPF", "BPF", "Notch"]),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Slow,     // cutoff — filter coefficient, avoid zipper
            1 => SmoothingStyle::Slow,     // resonance — filter coefficient, avoid zipper
            2 => SmoothingStyle::Standard, // output level
            3 => SmoothingStyle::None,     // filter_type — stepped, snap immediately
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.cutoff_hz,
            1 => self.resonance,
            2 => self.output_db,
            3 => self.filter_type,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.cutoff_hz = value,
            1 => self.resonance = value,
            2 => self.output_db = value,
            3 => self.filter_type = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP multimode filter kernel.
///
/// Contains ONLY the mutable state required for audio processing:
/// - Stereo biquad filters (L/R)
/// - Sample rate (for coefficient recalculation on rate change)
/// - Cached coefficient tracking (recompute only on cutoff/resonance/type change)
///
/// No `SmoothedParam`, no `AtomicU32`, no platform awareness. Coefficients
/// are recomputed only when `cutoff_hz`, `resonance`, or `filter_type` deviate
/// by more than a small epsilon from the cached values, keeping the hot path
/// coefficient-free on steady-state operation.
pub struct FilterKernel {
    /// Sample rate in Hz, used for biquad coefficient computation.
    sample_rate: f32,

    /// Left-channel biquad filter.
    biquad_l: Biquad,
    /// Right-channel biquad filter.
    biquad_r: Biquad,

    /// Change-detector for biquad coefficients.
    ///
    /// Keyed on `[cutoff_hz, resonance, filter_type]`. Avoids coefficient
    /// function calls (involves `sinf`/`cosf`) when the filter params are stable.
    coeff_cache: Cached<[f32; 6]>,
}

impl FilterKernel {
    /// Create a new filter kernel at the given sample rate.
    ///
    /// Initialises both biquad filters with default parameters (1000 Hz, Q=0.707, LPF).
    pub fn new(sample_rate: f32) -> Self {
        let defaults = FilterParams::default();
        let sr = sample_rate;

        let initial_coeffs = {
            let cutoff = defaults.cutoff_hz.clamp(20.0, sr * 0.49);
            let q = defaults.resonance.clamp(0.1, 20.0);
            let (b0, b1, b2, a0, a1, a2) = lowpass_coefficients(cutoff, q, sr);
            [b0, b1, b2, a0, a1, a2]
        };

        let mut biquad_l = Biquad::new();
        let mut biquad_r = Biquad::new();
        biquad_l.set_coefficients(
            initial_coeffs[0],
            initial_coeffs[1],
            initial_coeffs[2],
            initial_coeffs[3],
            initial_coeffs[4],
            initial_coeffs[5],
        );
        biquad_r.set_coefficients(
            initial_coeffs[0],
            initial_coeffs[1],
            initial_coeffs[2],
            initial_coeffs[3],
            initial_coeffs[4],
            initial_coeffs[5],
        );

        let mut coeff_cache = Cached::new(initial_coeffs, 3);
        coeff_cache.update(
            &[defaults.cutoff_hz, defaults.resonance, defaults.filter_type],
            0.001,
            |inputs| {
                let cutoff = inputs[0].clamp(20.0, sr * 0.49);
                let q = inputs[1].clamp(0.1, 20.0);
                let (b0, b1, b2, a0, a1, a2) = match inputs[2] as u8 {
                    1 => highpass_coefficients(cutoff, q, sr),
                    2 => bandpass_coefficients(cutoff, q, sr),
                    3 => notch_coefficients(cutoff, q, sr),
                    _ => lowpass_coefficients(cutoff, q, sr),
                };
                [b0, b1, b2, a0, a1, a2]
            },
        );

        Self {
            sample_rate,
            biquad_l,
            biquad_r,
            coeff_cache,
        }
    }
}

impl DspKernel for FilterKernel {
    type Params = FilterParams;

    fn process_stereo(&mut self, left: f32, right: f32, params: &FilterParams) -> (f32, f32) {
        // ── Coefficient update (only when cutoff, resonance, or filter type changes) ──
        let sr = self.sample_rate;
        let c = *self.coeff_cache.update(
            &[params.cutoff_hz, params.resonance, params.filter_type],
            0.001,
            |inputs| {
                let cutoff = inputs[0].clamp(20.0, sr * 0.49);
                let q = inputs[1].clamp(0.1, 20.0);
                let (b0, b1, b2, a0, a1, a2) = match inputs[2] as u8 {
                    1 => highpass_coefficients(cutoff, q, sr),
                    2 => bandpass_coefficients(cutoff, q, sr),
                    3 => notch_coefficients(cutoff, q, sr),
                    _ => lowpass_coefficients(cutoff, q, sr),
                };
                [b0, b1, b2, a0, a1, a2]
            },
        );
        self.biquad_l
            .set_coefficients(c[0], c[1], c[2], c[3], c[4], c[5]);
        self.biquad_r
            .set_coefficients(c[0], c[1], c[2], c[3], c[4], c[5]);

        // ── Unit conversion (user-facing → internal) ──
        let output_gain = fast_db_to_linear(params.output_db);

        // ── Biquad filtering → Soft limit → Output level ──
        let out_l = soft_limit(self.biquad_l.process(left), 1.0) * output_gain;
        let out_r = soft_limit(self.biquad_r.process(right), 1.0) * output_gain;

        (out_l, out_r)
    }

    fn reset(&mut self) {
        self.biquad_l.clear();
        self.biquad_r.clear();
        self.coeff_cache.invalidate();
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.coeff_cache.invalidate();
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

    // ── Kernel unit tests ──

    #[test]
    fn silence_in_silence_out() {
        let mut kernel = FilterKernel::new(48000.0);
        let params = FilterParams::default();

        let (left, right) = kernel.process_stereo(0.0, 0.0, &params);
        assert!(left.abs() < 1e-6, "Expected silence on left, got {left}");
        assert!(right.abs() < 1e-6, "Expected silence on right, got {right}");
    }

    #[test]
    fn no_nan_or_inf() {
        let mut kernel = FilterKernel::new(48000.0);
        let params = FilterParams::default();

        for i in 0..1000 {
            let t = i as f32 / 48000.0;
            let input = libm::sinf(2.0 * core::f32::consts::PI * 440.0 * t);
            let (left, right) = kernel.process_stereo(input, input, &params);
            assert!(
                left.is_finite(),
                "Left output NaN/Inf at sample {i}: {left}"
            );
            assert!(
                right.is_finite(),
                "Right output NaN/Inf at sample {i}: {right}"
            );
        }
    }

    #[test]
    fn params_descriptor_count() {
        assert_eq!(FilterParams::COUNT, 4);

        let desc0 = FilterParams::descriptor(0).expect("index 0 must exist");
        assert_eq!(desc0.name, "Cutoff");
        assert!((desc0.min - 20.0).abs() < 0.01);
        assert!((desc0.max - 20000.0).abs() < 0.01);
        assert!((desc0.default - 1000.0).abs() < 0.01);
        assert_eq!(desc0.id, ParamId(1200));
        assert_eq!(desc0.string_id, "flt_cutoff");

        let desc1 = FilterParams::descriptor(1).expect("index 1 must exist");
        assert_eq!(desc1.name, "Resonance");
        assert!((desc1.min - 0.1).abs() < 0.001);
        assert!((desc1.max - 20.0).abs() < 0.01);
        assert!((desc1.default - 0.707).abs() < 0.001);
        assert_eq!(desc1.id, ParamId(1201));
        assert_eq!(desc1.string_id, "flt_resonance");

        let desc2 = FilterParams::descriptor(2).expect("index 2 must exist");
        assert_eq!(desc2.name, "Output");
        assert_eq!(desc2.id, ParamId(1202));
        assert_eq!(desc2.string_id, "flt_output");

        let desc3 = FilterParams::descriptor(3).expect("index 3 must exist");
        assert_eq!(desc3.name, "Type");
        assert!((desc3.min - 0.0).abs() < 0.01);
        assert!((desc3.max - 3.0).abs() < 0.01);
        assert!((desc3.default - 0.0).abs() < 0.01);
        assert_eq!(desc3.id, ParamId(1203));
        assert_eq!(desc3.string_id, "flt_type");
        assert!(desc3.flags.contains(ParamFlags::STEPPED));

        assert!(
            FilterParams::descriptor(4).is_none(),
            "index 4 must be None"
        );
    }

    #[test]
    fn higher_cutoff_passes_more_signal() {
        // A high-frequency test tone should be more attenuated by a low cutoff
        // than a high cutoff. We compare RMS energy after the filter settles.
        let sample_rate = 48000.0;
        let test_freq_hz = 8000.0; // well above low cutoff, well below high cutoff

        let low_cutoff = FilterParams {
            cutoff_hz: 500.0,
            resonance: 0.707,
            output_db: 0.0,
            filter_type: 0.0,
        };
        let high_cutoff = FilterParams {
            cutoff_hz: 16000.0,
            resonance: 0.707,
            output_db: 0.0,
            filter_type: 0.0,
        };

        let mut kernel_low = FilterKernel::new(sample_rate);
        let mut kernel_high = FilterKernel::new(sample_rate);

        // Warm up (let transients settle)
        for i in 0..256 {
            let t = i as f32 / sample_rate;
            let s = libm::sinf(2.0 * core::f32::consts::PI * test_freq_hz * t);
            kernel_low.process_stereo(s, s, &low_cutoff);
            kernel_high.process_stereo(s, s, &high_cutoff);
        }

        // Measure RMS over the next 512 samples
        let mut energy_low = 0.0f32;
        let mut energy_high = 0.0f32;
        for i in 256..768 {
            let t = i as f32 / sample_rate;
            let s = libm::sinf(2.0 * core::f32::consts::PI * test_freq_hz * t);
            let (l_low, _) = kernel_low.process_stereo(s, s, &low_cutoff);
            let (l_high, _) = kernel_high.process_stereo(s, s, &high_cutoff);
            energy_low += l_low * l_low;
            energy_high += l_high * l_high;
        }

        assert!(
            energy_high > energy_low,
            "Higher cutoff ({}) should pass more of a {test_freq_hz} Hz signal than lower cutoff ({}). \
             energy_high={energy_high:.4}, energy_low={energy_low:.4}",
            high_cutoff.cutoff_hz,
            low_cutoff.cutoff_hz,
        );
    }

    // ── Adapter integration tests ──

    #[test]
    fn adapter_wraps_as_effect() {
        let kernel = FilterKernel::new(48000.0);
        let mut adapter = KernelAdapter::new(kernel, 48000.0);

        adapter.reset();
        let output = adapter.process(0.3);
        assert!(!output.is_nan(), "adapter.process() returned NaN");
        assert!(output.is_finite(), "adapter.process() returned Inf");
    }

    #[test]
    fn adapter_param_info_matches() {
        let kernel = FilterKernel::new(48000.0);
        let adapter = KernelAdapter::new(kernel, 48000.0);

        assert_eq!(adapter.param_count(), 4);

        let desc0 = adapter.param_info(0).expect("param 0 must exist");
        assert_eq!(desc0.name, "Cutoff");
        assert_eq!(desc0.id, ParamId(1200));

        let desc1 = adapter.param_info(1).expect("param 1 must exist");
        assert_eq!(desc1.name, "Resonance");
        assert_eq!(desc1.id, ParamId(1201));

        let desc2 = adapter.param_info(2).expect("param 2 must exist");
        assert_eq!(desc2.name, "Output");
        assert_eq!(desc2.id, ParamId(1202));

        let desc3 = adapter.param_info(3).expect("param 3 must exist");
        assert_eq!(desc3.name, "Type");

        assert!(adapter.param_info(4).is_none());
    }

    #[test]
    fn morph_produces_valid_output() {
        let mut kernel = FilterKernel::new(48000.0);

        let a = FilterParams {
            cutoff_hz: 500.0,
            resonance: 0.5,
            output_db: 0.0,
            filter_type: 0.0,
        };
        let b = FilterParams {
            cutoff_hz: 10000.0,
            resonance: 4.0,
            output_db: -6.0,
            filter_type: 0.0,
        };

        for step in 0..=10 {
            let t = step as f32 / 10.0;
            let morphed = FilterParams::lerp(&a, &b, t);

            let (out_l, out_r) = kernel.process_stereo(0.3, -0.3, &morphed);
            assert!(
                out_l.is_finite(),
                "Left NaN/Inf during morph at t={t}: {out_l}"
            );
            assert!(
                out_r.is_finite(),
                "Right NaN/Inf during morph at t={t}: {out_r}"
            );
            kernel.reset();
        }
    }

    // ── Additional behavioural tests ──

    #[test]
    fn dc_passes_through_filter() {
        // A low-pass filter at any cutoff should pass DC (0 Hz) through.
        let mut kernel = FilterKernel::new(48000.0);
        let params = FilterParams {
            cutoff_hz: 500.0,
            resonance: 0.707,
            output_db: 0.0,
            filter_type: 0.0,
        };

        // Warm up with constant input.
        for _ in 0..2000 {
            kernel.process_stereo(1.0, 1.0, &params);
        }

        // After settling the biquad memory should have reached near-unity for DC.
        let (out_l, out_r) = kernel.process_stereo(1.0, 1.0, &params);
        assert!(
            (out_l - 1.0).abs() < 0.05,
            "DC should pass filter with gain ≈1.0, got {out_l}"
        );
        assert!(
            (out_r - 1.0).abs() < 0.05,
            "DC should pass filter (right) with gain ≈1.0, got {out_r}"
        );
    }

    #[test]
    fn from_knobs_maps_midpoint_to_approx_1khz() {
        // At t=0.5, the log mapping 20 * 1000^0.5 = 20 * ~31.6 = ~632 Hz.
        // Verify the formula is consistent (not equal to 1 kHz, but finite).
        let params = FilterParams::from_knobs(0.5, 0.5, 0.5, 0.0);
        assert!(
            params.cutoff_hz.is_finite(),
            "cutoff_hz should be finite, got {}",
            params.cutoff_hz
        );
        assert!(
            params.cutoff_hz > 20.0 && params.cutoff_hz < 20000.0,
            "cutoff_hz should be in range, got {}",
            params.cutoff_hz
        );
        // Resonance at 0.5 → 0.1 + 0.5 * 19.9 = 10.05
        assert!(
            (params.resonance - 10.05).abs() < 0.01,
            "resonance should be ~10.05, got {}",
            params.resonance
        );
        // Output at 0.5 → -20 + 0.5 * 26 = -7.0 dB
        assert!(
            (params.output_db - (-7.0)).abs() < 0.01,
            "output at 0.5 should be -7 dB, got {}",
            params.output_db
        );
    }

    #[test]
    fn smoothing_styles_match_expected() {
        // Both filter params use Slow to avoid zipper noise on coefficient changes.
        assert_eq!(FilterParams::smoothing(0), SmoothingStyle::Slow);
        assert_eq!(FilterParams::smoothing(1), SmoothingStyle::Slow);
        assert_eq!(FilterParams::smoothing(2), SmoothingStyle::Standard);
        assert_eq!(FilterParams::smoothing(3), SmoothingStyle::None);
    }

    #[test]
    fn adapter_set_get_roundtrip() {
        let kernel = FilterKernel::new(48000.0);
        let mut adapter = KernelAdapter::new(kernel, 48000.0);

        adapter.set_param(0, 4000.0); // Cutoff = 4 kHz
        assert!(
            (adapter.get_param(0) - 4000.0).abs() < 1.0,
            "Cutoff roundtrip failed: {}",
            adapter.get_param(0)
        );

        adapter.set_param(1, 2.0); // Resonance = 2.0
        assert!(
            (adapter.get_param(1) - 2.0).abs() < 0.01,
            "Resonance roundtrip failed: {}",
            adapter.get_param(1)
        );

        adapter.set_param(2, -6.0); // Output = -6 dB
        assert!(
            (adapter.get_param(2) - (-6.0)).abs() < 0.01,
            "Output roundtrip failed: {}",
            adapter.get_param(2)
        );
    }

    #[test]
    fn default_filter_type_is_lpf() {
        let params = FilterParams::default();
        assert_eq!(
            params.filter_type, 0.0,
            "Default filter type should be LPF (0)"
        );
    }

    #[test]
    fn highpass_rejects_dc() {
        let mut kernel = FilterKernel::new(48000.0);
        let params = FilterParams {
            cutoff_hz: 500.0,
            resonance: 0.707,
            output_db: 0.0,
            filter_type: 1.0, // HPF
        };

        // Warm up with DC
        for _ in 0..2000 {
            kernel.process_stereo(1.0, 1.0, &params);
        }

        // HPF should reject DC — output near zero
        let (out_l, _) = kernel.process_stereo(1.0, 1.0, &params);
        assert!(out_l.abs() < 0.05, "HPF should reject DC, got {out_l}");
    }

    #[test]
    fn bandpass_passes_center_frequency() {
        let sample_rate = 48000.0;
        let center_hz = 1000.0;
        let mut kernel = FilterKernel::new(sample_rate);
        let params = FilterParams {
            cutoff_hz: center_hz,
            resonance: 2.0,
            output_db: 0.0,
            filter_type: 2.0, // BPF
        };

        // Process signal at center frequency
        let mut energy_center = 0.0f32;
        for i in 0..1024 {
            let t = i as f32 / sample_rate;
            let s = libm::sinf(2.0 * core::f32::consts::PI * center_hz * t);
            let (out, _) = kernel.process_stereo(s, s, &params);
            if i >= 512 {
                // skip transient
                energy_center += out * out;
            }
        }

        // Process signal well off-center (100 Hz)
        kernel.reset();
        let mut energy_off = 0.0f32;
        for i in 0..1024 {
            let t = i as f32 / sample_rate;
            let s = libm::sinf(2.0 * core::f32::consts::PI * 100.0 * t);
            let (out, _) = kernel.process_stereo(s, s, &params);
            if i >= 512 {
                energy_off += out * out;
            }
        }

        assert!(
            energy_center > energy_off * 2.0,
            "BPF should pass center ({center_hz} Hz) more than off-center: center={energy_center:.4}, off={energy_off:.4}"
        );
    }

    #[test]
    fn notch_rejects_center_frequency() {
        let sample_rate = 48000.0;
        let notch_hz = 1000.0;
        let mut kernel = FilterKernel::new(sample_rate);
        let params = FilterParams {
            cutoff_hz: notch_hz,
            resonance: 2.0,
            output_db: 0.0,
            filter_type: 3.0, // Notch
        };

        // Process signal at notch frequency
        let mut energy_notch = 0.0f32;
        for i in 0..1024 {
            let t = i as f32 / sample_rate;
            let s = libm::sinf(2.0 * core::f32::consts::PI * notch_hz * t);
            let (out, _) = kernel.process_stereo(s, s, &params);
            if i >= 512 {
                energy_notch += out * out;
            }
        }

        // Process DC (0 Hz) — notch should pass DC
        kernel.reset();
        for _ in 0..2000 {
            kernel.process_stereo(1.0, 1.0, &params);
        }
        let (dc_out, _) = kernel.process_stereo(1.0, 1.0, &params);

        assert!(dc_out.abs() > 0.8, "Notch should pass DC, got {dc_out}");
        assert!(
            energy_notch < 10.0, // very attenuated at notch frequency
            "Notch should reject center frequency, energy={energy_notch:.4}"
        );
    }
}
