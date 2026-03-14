//! Distortion kernel — ADAA waveshaping with tone shaping and drive control.
//!
//! `DistortionKernel` owns DSP state (filters, ADAA processors). Parameters
//! are received via `&DistortionParams` each sample. Deployed via
//! [`KernelAdapter`](sonido_core::KernelAdapter) for desktop/plugin, or called
//! directly on embedded targets.
//!
//! # Signal Flow
//!
//! ```text
//! Input → Drive (gain) → ADAA Waveshaper → Tone EQ → Mix → Soft Limit → Output Level
//! ```
//!
//! ADAA waveshaping minimizes aliasing from the nonlinear clipping stages.
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! let adapter = KernelAdapter::new(DistortionKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing, ADCs are hardware-filtered)
//! let mut kernel = DistortionKernel::new(48000.0);
//! let params = DistortionParams::from_knobs(adc_drive, adc_tone, adc_output, adc_mix);
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::math::soft_limit;
use sonido_core::{
    Adaa1, Biquad, ParamDescriptor, ParamFlags, ParamId, ParamUnit, asymmetric_clip,
    asymmetric_clip_ad, fast_db_to_linear, foldback, hard_clip, hard_clip_ad,
    peaking_eq_coefficients, soft_clip, soft_clip_ad, wet_dry_mix_stereo,
};

// ── ADAA function-pointer aliases ──

fn hard_clip_unit(x: f32) -> f32 {
    hard_clip(x, 1.0)
}

fn hard_clip_ad_unit(x: f32) -> f32 {
    hard_clip_ad(x, 1.0)
}

type AdaaProc = Adaa1<fn(f32) -> f32, fn(f32) -> f32>;

/// Center frequency for the tone peaking EQ.
const TONE_CENTER_HZ: f32 = 1000.0;

/// Q factor for the tone peaking EQ (moderately broad).
const TONE_Q: f32 = 0.7;

/// Default foldback threshold.
const FOLDBACK_THRESHOLD: f32 = 0.8;

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`DistortionKernel`].
///
/// All values in **user-facing units** — the same units shown in GUIs and
/// stored in presets. The kernel converts internally as needed.
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `drive_db` | dB | 0–40 | 8.0 |
/// | 1 | `tone_db` | dB | −12–12 | 0.0 |
/// | 2 | `output_db` | dB | −20–20 | 0.0 |
/// | 3 | `shape` | index | 0–3 | 0 (SoftClip) |
/// | 4 | `mix_pct` | % | 0–100 | 100.0 |
#[derive(Debug, Clone, Copy)]
pub struct DistortionParams {
    /// Input gain before waveshaping, in decibels.
    pub drive_db: f32,
    /// Peaking EQ gain at 1 kHz, in decibels.
    pub tone_db: f32,
    /// Output level, in decibels.
    pub output_db: f32,
    /// Waveshaping algorithm: 0=SoftClip, 1=HardClip, 2=Foldback, 3=Asymmetric.
    pub shape: f32,
    /// Wet/dry mix, in percent (0=fully dry, 100=fully wet).
    pub mix_pct: f32,
}

impl Default for DistortionParams {
    fn default() -> Self {
        Self {
            drive_db: 8.0,
            tone_db: 0.0,
            output_db: 0.0,
            shape: 0.0,
            mix_pct: 100.0,
        }
    }
}

impl DistortionParams {
    /// Creates parameters from normalized 0–1 knob readings.
    ///
    /// Curves (logarithmic for frequency/time, linear for percentage) are
    /// derived from [`ParamDescriptor`] — same mapping as GUI and plugin hosts.
    pub fn from_knobs(drive: f32, tone: f32, output: f32, shape: f32, mix: f32) -> Self {
        Self::from_normalized(&[drive, tone, output, shape, mix])
    }
}

impl KernelParams for DistortionParams {
    const COUNT: usize = 5;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(
                ParamDescriptor::gain_db("Drive", "Drive", 0.0, 40.0, 8.0)
                    .with_id(ParamId(200), "dist_drive"),
            ),
            1 => Some(
                ParamDescriptor::custom("Tone", "Tone", -12.0, 12.0, 0.0)
                    .with_unit(ParamUnit::Decibels)
                    .with_step(0.5)
                    .with_id(ParamId(201), "dist_tone"),
            ),
            2 => Some(
                sonido_core::gain::output_param_descriptor().with_id(ParamId(202), "dist_output"),
            ),
            3 => Some(
                ParamDescriptor::custom("Waveshape", "Shape", 0.0, 3.0, 0.0)
                    .with_step(1.0)
                    .with_id(ParamId(203), "dist_shape")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(&["Soft Clip", "Hard Clip", "Foldback", "Asymmetric"]),
            ),
            4 => Some(
                ParamDescriptor::custom("Mix", "Mix", 0.0, 100.0, 100.0)
                    .with_unit(ParamUnit::Percent)
                    .with_step(1.0)
                    .with_id(ParamId(204), "dist_mix"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Fast,     // drive — fast response for feel
            1 => SmoothingStyle::Slow,     // tone — filter coefficient, avoid zipper
            2 => SmoothingStyle::Standard, // output level
            3 => SmoothingStyle::None,     // waveshape — discrete, snap
            4 => SmoothingStyle::Standard, // mix
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.drive_db,
            1 => self.tone_db,
            2 => self.output_db,
            3 => self.shape,
            4 => self.mix_pct,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.drive_db = value,
            1 => self.tone_db = value,
            2 => self.output_db = value,
            3 => self.shape = value,
            4 => self.mix_pct = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP distortion kernel.
///
/// Contains ONLY the mutable state required for audio processing:
/// - ADAA waveshaper state (L/R × 3 modes)
/// - Tone biquad filters (L/R)
/// - Cached coefficient tracking
///
/// No `SmoothedParam`, no `AtomicU32`, no platform awareness.
pub struct DistortionKernel {
    sample_rate: f32,

    // Tone EQ filters
    tone_filter: Biquad,
    tone_filter_r: Biquad,

    // ADAA processors (L/R pairs per waveshape mode)
    adaa_soft_l: AdaaProc,
    adaa_soft_r: AdaaProc,
    adaa_hard_l: AdaaProc,
    adaa_hard_r: AdaaProc,
    adaa_asym_l: AdaaProc,
    adaa_asym_r: AdaaProc,

    // Coefficient cache — recompute biquad only when tone_db changes
    last_tone_db: f32,
}

impl DistortionKernel {
    /// Create a new distortion kernel.
    pub fn new(sample_rate: f32) -> Self {
        let mut kernel = Self {
            sample_rate,
            tone_filter: Biquad::new(),
            tone_filter_r: Biquad::new(),
            adaa_soft_l: Adaa1::new(soft_clip as fn(f32) -> f32, soft_clip_ad as fn(f32) -> f32),
            adaa_soft_r: Adaa1::new(soft_clip as fn(f32) -> f32, soft_clip_ad as fn(f32) -> f32),
            adaa_hard_l: Adaa1::new(
                hard_clip_unit as fn(f32) -> f32,
                hard_clip_ad_unit as fn(f32) -> f32,
            ),
            adaa_hard_r: Adaa1::new(
                hard_clip_unit as fn(f32) -> f32,
                hard_clip_ad_unit as fn(f32) -> f32,
            ),
            adaa_asym_l: Adaa1::new(
                asymmetric_clip as fn(f32) -> f32,
                asymmetric_clip_ad as fn(f32) -> f32,
            ),
            adaa_asym_r: Adaa1::new(
                asymmetric_clip as fn(f32) -> f32,
                asymmetric_clip_ad as fn(f32) -> f32,
            ),
            last_tone_db: f32::NAN, // Force initial coefficient computation
        };
        // Initialize tone filter with default params
        kernel.update_tone_coefficients(0.0);
        kernel
    }

    /// Recalculate biquad coefficients for the tone EQ.
    fn update_tone_coefficients(&mut self, tone_db: f32) {
        let (b0, b1, b2, a0, a1, a2) =
            peaking_eq_coefficients(TONE_CENTER_HZ, TONE_Q, tone_db, self.sample_rate);
        self.tone_filter.set_coefficients(b0, b1, b2, a0, a1, a2);
        self.tone_filter_r.set_coefficients(b0, b1, b2, a0, a1, a2);
        self.last_tone_db = tone_db;
    }
}

impl DspKernel for DistortionKernel {
    type Params = DistortionParams;

    fn process_stereo(&mut self, left: f32, right: f32, params: &DistortionParams) -> (f32, f32) {
        // ── Coefficient update (only when tone changes) ──
        // Comparison threshold accounts for smoothing granularity.
        if (params.tone_db - self.last_tone_db).abs() > 0.001 {
            self.update_tone_coefficients(params.tone_db);
        }

        // ── Unit conversion (user-facing → internal) ──
        let drive = fast_db_to_linear(params.drive_db);
        let output = fast_db_to_linear(params.output_db);
        let mix = params.mix_pct / 100.0;
        let shape = params.shape as u8;

        // ── Drive stage ──
        let driven_l = left * drive;
        let driven_r = right * drive;

        // ── Waveshaping (with ADAA where available) ──
        let (shaped_l, shaped_r) = match shape {
            0 => (
                self.adaa_soft_l.process(driven_l),
                self.adaa_soft_r.process(driven_r),
            ),
            1 => (
                self.adaa_hard_l.process(driven_l),
                self.adaa_hard_r.process(driven_r),
            ),
            2 => (
                foldback(driven_l, FOLDBACK_THRESHOLD),
                foldback(driven_r, FOLDBACK_THRESHOLD),
            ),
            _ => (
                self.adaa_asym_l.process(driven_l),
                self.adaa_asym_r.process(driven_r),
            ),
        };

        // ── Tone EQ ──
        let toned_l = self.tone_filter.process(shaped_l);
        let toned_r = self.tone_filter_r.process(shaped_r);

        // ── Mix → Soft Limit → Output Level ──
        let (mixed_l, mixed_r) = wet_dry_mix_stereo(left, right, toned_l, toned_r, mix);
        (
            soft_limit(mixed_l, 1.0) * output,
            soft_limit(mixed_r, 1.0) * output,
        )
    }

    fn reset(&mut self) {
        self.tone_filter.clear();
        self.tone_filter_r.clear();
        self.adaa_soft_l.reset();
        self.adaa_soft_r.reset();
        self.adaa_hard_l.reset();
        self.adaa_hard_r.reset();
        self.adaa_asym_l.reset();
        self.adaa_asym_r.reset();
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.update_tone_coefficients(self.last_tone_db);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_core::kernel::KernelAdapter;
    use sonido_core::{Effect, ParameterInfo};

    #[test]
    fn kernel_default_params() {
        let params = DistortionParams::default();
        assert_eq!(params.drive_db, 8.0);
        assert_eq!(params.tone_db, 0.0);
        assert_eq!(params.output_db, 0.0);
        assert_eq!(params.shape, 0.0);
        assert_eq!(params.mix_pct, 100.0);
    }

    #[test]
    fn kernel_processes_without_panic() {
        let mut kernel = DistortionKernel::new(48000.0);
        let params = DistortionParams::default();

        let (left, right) = kernel.process_stereo(0.5, 0.5, &params);
        assert!(!left.is_nan(), "Left output is NaN");
        assert!(!right.is_nan(), "Right output is NaN");
        assert!(left.is_finite(), "Left output is infinite");
        assert!(right.is_finite(), "Right output is infinite");
    }

    #[test]
    fn kernel_silence_in_silence_out() {
        let mut kernel = DistortionKernel::new(48000.0);
        let params = DistortionParams::default();

        let (left, right) = kernel.process_stereo(0.0, 0.0, &params);
        assert!(left.abs() < 1e-6, "Expected silence, got {left}");
        assert!(right.abs() < 1e-6, "Expected silence, got {right}");
    }

    #[test]
    fn kernel_drive_increases_amplitude() {
        let mut kernel = DistortionKernel::new(48000.0);

        let low_drive = DistortionParams {
            drive_db: 0.0,
            ..Default::default()
        };
        let high_drive = DistortionParams {
            drive_db: 30.0,
            ..Default::default()
        };

        // Process a quiet signal through both
        let input = 0.01;
        let (low_out, _) = kernel.process_stereo(input, input, &low_drive);

        kernel.reset();
        let (high_out, _) = kernel.process_stereo(input, input, &high_drive);

        assert!(
            high_out.abs() > low_out.abs(),
            "Higher drive should produce higher amplitude: low={low_out}, high={high_out}"
        );
    }

    #[test]
    fn kernel_waveshape_modes_differ() {
        let input = 0.3;
        let mut outputs = [0.0f32; 4];

        for shape in 0..4 {
            let mut kernel = DistortionKernel::new(48000.0);
            let params = DistortionParams {
                drive_db: 20.0,
                shape: shape as f32,
                ..Default::default()
            };
            let (out, _) = kernel.process_stereo(input, input, &params);
            outputs[shape] = out;
        }

        // At least some modes should produce different outputs
        let all_same = outputs.windows(2).all(|w| (w[0] - w[1]).abs() < 1e-6);
        assert!(
            !all_same,
            "Different waveshapes should produce different outputs"
        );
    }

    #[test]
    fn kernel_mix_dry_is_passthrough() {
        let mut kernel = DistortionKernel::new(48000.0);
        let params = DistortionParams {
            mix_pct: 0.0, // Fully dry
            output_db: 0.0,
            ..Default::default()
        };

        let input = 0.5;
        let (out, _) = kernel.process_stereo(input, input, &params);

        // At 0% mix and 0dB output, output should approximate input
        // (soft_limit at 1.0 ceiling is nearly transparent for signals < 1.0)
        assert!(
            (out - input).abs() < 0.05,
            "Dry mix should approximate passthrough: input={input}, output={out}"
        );
    }

    #[test]
    fn kernel_params_descriptors() {
        assert_eq!(DistortionParams::COUNT, 5);

        let desc = DistortionParams::descriptor(0).unwrap();
        assert_eq!(desc.name, "Drive");
        assert_eq!(desc.min, 0.0);
        assert_eq!(desc.max, 40.0);
        assert_eq!(desc.id, ParamId(200));

        let desc = DistortionParams::descriptor(3).unwrap();
        assert_eq!(desc.name, "Waveshape");
        assert!(desc.flags.contains(ParamFlags::STEPPED));
    }

    #[test]
    fn kernel_params_smoothing_styles() {
        assert_eq!(DistortionParams::smoothing(0), SmoothingStyle::Fast); // drive
        assert_eq!(DistortionParams::smoothing(1), SmoothingStyle::Slow); // tone
        assert_eq!(DistortionParams::smoothing(3), SmoothingStyle::None); // shape
    }

    #[test]
    fn kernel_params_from_knobs() {
        // After rewiring, from_knobs delegates to from_normalized.
        // Verify it matches from_normalized for the same inputs.
        let inputs = [0.5_f32, 0.5, 0.5, 0.25, 1.0];
        let via_knobs =
            DistortionParams::from_knobs(inputs[0], inputs[1], inputs[2], inputs[3], inputs[4]);
        let via_norm = DistortionParams::from_normalized(&inputs);
        assert!((via_knobs.drive_db - via_norm.drive_db).abs() < 1e-5);
        assert!((via_knobs.tone_db - via_norm.tone_db).abs() < 1e-5);
        assert!((via_knobs.output_db - via_norm.output_db).abs() < 1e-5);
        assert!((via_knobs.shape - via_norm.shape).abs() < 1e-5);
        assert!((via_knobs.mix_pct - via_norm.mix_pct).abs() < 1e-5);

        // Boundary checks: 0.0 → min, 1.0 → max
        let p_min = DistortionParams::from_knobs(0.0, 0.0, 0.0, 0.0, 0.0);
        assert!((p_min.drive_db - 0.0).abs() < 0.01, "drive min");
        assert!((p_min.output_db - (-20.0)).abs() < 0.01, "output min");

        let p_max = DistortionParams::from_knobs(1.0, 1.0, 1.0, 1.0, 1.0);
        assert!((p_max.drive_db - 40.0).abs() < 0.01, "drive max");
        assert!(
            (p_max.output_db - 6.0).abs() < 0.01,
            "output max (OUTPUT_MAX_DB=6)"
        );
        assert!((p_max.mix_pct - 100.0).abs() < 0.01, "mix max");
    }

    // ── Adapter integration tests ──

    #[test]
    fn adapter_wraps_kernel_as_effect() {
        let kernel = DistortionKernel::new(48000.0);
        let mut adapter = KernelAdapter::new(kernel, 48000.0);

        // Should work as a standard Effect
        adapter.reset();
        let output = adapter.process(0.3);
        assert!(!output.is_nan());
        assert!(output.is_finite());
    }

    #[test]
    fn adapter_exposes_correct_params() {
        let kernel = DistortionKernel::new(48000.0);
        let adapter = KernelAdapter::new(kernel, 48000.0);

        assert_eq!(adapter.param_count(), 5);
        assert_eq!(adapter.param_info(0).unwrap().name, "Drive");
        assert_eq!(adapter.param_info(4).unwrap().name, "Mix");
        assert!(adapter.param_info(5).is_none());
    }

    #[test]
    fn adapter_set_get_roundtrip() {
        let kernel = DistortionKernel::new(48000.0);
        let mut adapter = KernelAdapter::new(kernel, 48000.0);

        adapter.set_param(0, 25.0); // Drive = 25 dB
        assert!((adapter.get_param(0) - 25.0).abs() < 0.01);

        adapter.set_param(4, 50.0); // Mix = 50%
        assert!((adapter.get_param(4) - 50.0).abs() < 0.01);
    }

    // ── Multi-role params tests (preset, morph, normalized) ──

    #[test]
    fn params_are_presets() {
        // The params struct IS the preset — clone to save, restore to load
        let original = DistortionParams {
            drive_db: 25.0,
            tone_db: 3.0,
            output_db: -6.0,
            shape: 2.0,
            mix_pct: 80.0,
        };

        // "Save" preset
        let saved = original;

        // "Load" into adapter
        let kernel = DistortionKernel::new(48000.0);
        let mut adapter = KernelAdapter::new(kernel, 48000.0);
        adapter.load_snapshot(&saved);
        adapter.reset(); // snap for instant recall

        assert!((adapter.get_param(0) - 25.0).abs() < 0.01);
        assert!((adapter.get_param(4) - 80.0).abs() < 0.01);
    }

    #[test]
    fn params_morph_between_presets() {
        let clean = DistortionParams {
            drive_db: 3.0,
            tone_db: 0.0,
            output_db: 0.0,
            shape: 0.0,
            mix_pct: 30.0,
        };
        let heavy = DistortionParams {
            drive_db: 35.0,
            tone_db: 6.0,
            output_db: -3.0,
            shape: 1.0,
            mix_pct: 100.0,
        };

        // 50% morph
        let mid = DistortionParams::lerp(&clean, &heavy, 0.5);
        assert!((mid.drive_db - 19.0).abs() < 0.1, "drive should be ~19");
        assert!((mid.mix_pct - 65.0).abs() < 0.1, "mix should be ~65");

        // shape is STEPPED — should snap at 0.5
        assert_eq!(mid.shape, 1.0, "stepped param should snap to b at t=0.5");

        // 25% morph — shape should still be 'a' value
        let quarter = DistortionParams::lerp(&clean, &heavy, 0.25);
        assert_eq!(
            quarter.shape, 0.0,
            "stepped param should stay at a when t<0.5"
        );
    }

    #[test]
    fn params_snapshot_roundtrip_through_adapter() {
        let kernel = DistortionKernel::new(48000.0);
        let mut adapter = KernelAdapter::new(kernel, 48000.0);

        adapter.set_param(0, 20.0);
        adapter.set_param(1, -5.0);
        adapter.set_param(4, 75.0);

        let saved = adapter.snapshot();
        assert!((saved.drive_db - 20.0).abs() < 0.01);
        assert!((saved.tone_db - (-5.0)).abs() < 0.01);
        assert!((saved.mix_pct - 75.0).abs() < 0.01);
    }
}
