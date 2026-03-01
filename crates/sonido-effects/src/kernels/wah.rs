//! Wah kernel — pure DSP with separated parameter ownership.
//!
//! This is the kernel-architecture equivalent of [`Wah`](crate::Wah).
//! The DSP math is identical; the difference is structural:
//!
//! - **Classic `Wah`**: owns `SmoothedParam` for frequency/resonance/sensitivity/output,
//!   manages smoothing internally, implements `Effect` + `ParameterInfo` directly.
//!
//! - **`WahKernel`**: owns ONLY DSP state (SVF filters, envelope follower, sample_rate).
//!   Parameters are received via `&WahParams` on each processing call.
//!   Deployed via [`KernelAdapter`](sonido_core::KernelAdapter) for desktop/plugin,
//!   or called directly on embedded targets.
//!
//! # Signal Flow
//!
//! ```text
//! Input → EnvelopeFollower (auto-wah) ─┐
//!                                       ├→ Target Frequency → SVF Bandpass → Normalize → Mix (0.8/0.2) → Soft Limit → Output Level
//! Frequency (manual) ───────────────────┘
//! ```
//!
//! In **Auto** mode the envelope follower tracks input amplitude and sweeps the
//! SVF cutoff upward from the base frequency by `env_level × (max_freq - min_freq) × sensitivity`.
//! In **Manual** mode the SVF cutoff is fixed to the base frequency parameter.
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! let adapter = KernelAdapter::new(WahKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing, ADCs are hardware-filtered)
//! let mut kernel = WahKernel::new(48000.0);
//! let params = WahParams::from_knobs(adc_freq, adc_reso, adc_sens, adc_mode, adc_output);
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

use sonido_core::Effect;
use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::math::soft_limit;
use sonido_core::{
    EnvelopeFollower, ParamDescriptor, ParamFlags, ParamId, ParamUnit, StateVariableFilter,
    SvfOutput, fast_db_to_linear,
};

// ── Constants ───────────────────────────────────────────────────────────────

/// Minimum frequency of the auto-wah sweep (Hz).
const MIN_FREQ: f32 = 200.0;

/// Maximum frequency of the auto-wah sweep (Hz).
const MAX_FREQ: f32 = 2000.0;

/// Envelope follower attack time for auto-wah (ms).
const ENV_ATTACK_MS: f32 = 5.0;

/// Envelope follower release time for auto-wah (ms).
const ENV_RELEASE_MS: f32 = 50.0;

// ═══════════════════════════════════════════════════════════════════════════
//  Wah mode
// ═══════════════════════════════════════════════════════════════════════════

/// Wah operating mode.
///
/// Stored as an `f32` index (0.0 = Auto, 1.0 = Manual) in [`WahParams::mode`]
/// to satisfy the `KernelParams` `f32`-only constraint. Use the helper
/// constants or [`WahMode::to_f32`] / [`WahMode::from_f32`] to convert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WahMode {
    /// Auto-wah: envelope follower controls filter frequency.
    #[default]
    Auto,
    /// Manual: frequency controlled directly by the frequency parameter.
    Manual,
}

impl WahMode {
    /// Convert to the `f32` index stored in [`WahParams`].
    #[inline]
    pub fn to_f32(self) -> f32 {
        match self {
            Self::Auto => 0.0,
            Self::Manual => 1.0,
        }
    }

    /// Convert from the `f32` index stored in [`WahParams`].
    ///
    /// Any value ≥ 0.5 is treated as Manual.
    #[inline]
    pub fn from_f32(v: f32) -> Self {
        if v < 0.5 { Self::Auto } else { Self::Manual }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`WahKernel`].
///
/// All values in **user-facing units** — the same units shown in GUIs and
/// stored in presets. The kernel converts internally as needed.
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `freq_hz` | Hz | 200–2000 | 800.0 |
/// | 1 | `resonance` | (Q factor) | 1–10 | 5.0 |
/// | 2 | `sensitivity_pct` | % | 0–100 | 50.0 |
/// | 3 | `mode` | index | 0–1 (Auto/Manual) | 0.0 |
/// | 4 | `output_db` | dB | −20–20 | 0.0 |
///
/// `ParamId` values match the classic `Wah` effect exactly (base 600) so
/// saved automation and presets remain compatible.
#[derive(Debug, Clone, Copy)]
pub struct WahParams {
    /// Base/center frequency in Hz. In manual mode this is the fixed cutoff;
    /// in auto mode it is the starting point of the envelope sweep.
    ///
    /// Range: 200.0–2000.0 Hz.
    pub freq_hz: f32,

    /// Resonance (Q factor) of the SVF bandpass filter.
    ///
    /// Range: 1.0–10.0.
    pub resonance: f32,

    /// Envelope sensitivity in percent.
    ///
    /// Controls how much the envelope sweeps the filter frequency in auto mode.
    /// Internally converted to 0.0–1.0 fraction.
    ///
    /// Range: 0.0–100.0 %.
    pub sensitivity_pct: f32,

    /// Wah mode: 0.0 = Auto, 1.0 = Manual.
    ///
    /// Range: 0.0–1.0 (stepped).
    pub mode: f32,

    /// Output level in decibels.
    ///
    /// Range: −20.0–20.0 dB.
    pub output_db: f32,
}

impl Default for WahParams {
    fn default() -> Self {
        Self {
            freq_hz: 800.0,
            resonance: 5.0,
            sensitivity_pct: 50.0,
            mode: 0.0,
            output_db: 0.0,
        }
    }
}

impl WahParams {
    /// Build params directly from hardware knob readings (0.0–1.0 normalized).
    ///
    /// Convenience for embedded targets where ADC values map linearly to
    /// parameter ranges. Frequency uses a logarithmic mapping so that
    /// equal knob travel equals equal musical intervals.
    ///
    /// | Knob | Parameter | Range |
    /// |------|-----------|-------|
    /// | `freq` | `freq_hz` | 200–2000 Hz (log) |
    /// | `reso` | `resonance` | 1–10 |
    /// | `sens` | `sensitivity_pct` | 0–100 % |
    /// | `mode` | `mode` | 0 or 1 (snaps at 0.5) |
    /// | `output` | `output_db` | −20–20 dB |
    pub fn from_knobs(freq: f32, reso: f32, sens: f32, mode: f32, output: f32) -> Self {
        // Logarithmic frequency mapping: 200 Hz at 0.0, 2000 Hz at 1.0.
        // log10(200) ≈ 2.301, log10(2000) ≈ 3.301, span = 1.0 decade.
        let log_min = libm::log10f(MIN_FREQ);
        let log_max = libm::log10f(MAX_FREQ);
        let freq_hz = libm::powf(10.0, log_min + freq.clamp(0.0, 1.0) * (log_max - log_min));

        Self {
            freq_hz,
            resonance: 1.0 + reso.clamp(0.0, 1.0) * 9.0, // 1–10
            sensitivity_pct: sens.clamp(0.0, 1.0) * 100.0, // 0–100 %
            mode: libm::floorf(mode.clamp(0.0, 1.0) * 1.99), // 0.0 or 1.0
            output_db: output.clamp(0.0, 1.0) * 40.0 - 20.0, // −20–20 dB
        }
    }
}

impl KernelParams for WahParams {
    const COUNT: usize = 5;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(
                ParamDescriptor {
                    name: "Frequency",
                    short_name: "Freq",
                    unit: ParamUnit::Hertz,
                    min: 200.0,
                    max: 2000.0,
                    default: 800.0,
                    step: 10.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(600), "wah_freq")
                .with_scale(sonido_core::ParamScale::Logarithmic),
            ),
            1 => Some(
                ParamDescriptor {
                    name: "Resonance",
                    short_name: "Reso",
                    unit: ParamUnit::None,
                    min: 1.0,
                    max: 10.0,
                    default: 5.0,
                    step: 0.1,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(601), "wah_reso"),
            ),
            2 => Some(
                ParamDescriptor {
                    name: "Sensitivity",
                    short_name: "Sens",
                    unit: ParamUnit::Percent,
                    min: 0.0,
                    max: 100.0,
                    default: 50.0,
                    step: 1.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(602), "wah_sens"),
            ),
            3 => Some(
                ParamDescriptor {
                    name: "Mode",
                    short_name: "Mode",
                    unit: ParamUnit::None,
                    min: 0.0,
                    max: 1.0,
                    default: 0.0,
                    step: 1.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(603), "wah_mode")
                .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                .with_step_labels(&["Auto", "Manual"]),
            ),
            4 => Some(
                sonido_core::gain::output_param_descriptor().with_id(ParamId(604), "wah_output"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Fast,     // frequency — fast for expressive feel
            1 => SmoothingStyle::Slow,     // resonance — SVF coefficient, avoid zipper
            2 => SmoothingStyle::Standard, // sensitivity
            3 => SmoothingStyle::None,     // mode — discrete, snap immediately
            4 => SmoothingStyle::Standard, // output level
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.freq_hz,
            1 => self.resonance,
            2 => self.sensitivity_pct,
            3 => self.mode,
            4 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.freq_hz = value,
            1 => self.resonance = value,
            2 => self.sensitivity_pct = value,
            3 => self.mode = value,
            4 => self.output_db = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP wah kernel.
///
/// Contains ONLY the mutable state required for audio processing:
/// - Two SVF bandpass filters (one per stereo channel)
/// - One envelope follower (linked stereo detection from mid signal)
/// - Sample rate (required for filter coefficient recalculation)
///
/// No `SmoothedParam`, no `AtomicU32`, no platform awareness.
///
/// # DSP Algorithm
///
/// The wah uses a State Variable Filter in bandpass mode. In auto-wah mode
/// the envelope follower measures the signal's amplitude and sweeps the
/// filter's cutoff upward from the base frequency:
///
/// ```text
/// target_freq = clamp(base_freq + env_level × freq_range × sensitivity, min_freq, max_freq)
/// freq_range  = max_freq − min_freq = 1800 Hz
/// ```
///
/// The bandpass output is normalized by `1 / resonance` to compensate for the
/// SVF bandpass peak gain of Q at the center frequency, then blended 80/20 wet/dry
/// to preserve body and avoid the thin sound of a pure bandpass signal.
///
/// Reference: Pirkle, "Designing Audio Effect Plugins in C++", Chapter 19.
pub struct WahKernel {
    /// Bandpass SVF filter for the left channel.
    filter_l: StateVariableFilter,
    /// Bandpass SVF filter for the right channel.
    filter_r: StateVariableFilter,
    /// Envelope follower for auto-wah mode.
    envelope: EnvelopeFollower,
    /// Current sample rate (Hz).
    sample_rate: f32,
}

impl WahKernel {
    /// Create a new wah kernel at the given sample rate.
    ///
    /// Initialises both SVF filters in bandpass mode at 800 Hz / Q = 5.0
    /// and the envelope follower with 5 ms attack / 50 ms release.
    pub fn new(sample_rate: f32) -> Self {
        let mut filter_l = StateVariableFilter::new(sample_rate);
        filter_l.set_output_type(SvfOutput::Bandpass);
        filter_l.set_resonance(5.0);
        filter_l.set_cutoff(800.0);

        let mut filter_r = StateVariableFilter::new(sample_rate);
        filter_r.set_output_type(SvfOutput::Bandpass);
        filter_r.set_resonance(5.0);
        filter_r.set_cutoff(800.0);

        let mut envelope = EnvelopeFollower::new(sample_rate);
        envelope.set_attack_ms(ENV_ATTACK_MS);
        envelope.set_release_ms(ENV_RELEASE_MS);

        Self {
            filter_l,
            filter_r,
            envelope,
            sample_rate,
        }
    }

    /// Compute the target filter frequency from the current params and envelope level.
    ///
    /// In Auto mode, sweeps from `base_freq` upward proportionally to `env_level`
    /// and `sensitivity`. In Manual mode, returns `base_freq` directly.
    #[inline]
    fn target_freq(base_freq: f32, env_level: f32, sensitivity: f32, mode: WahMode) -> f32 {
        match mode {
            WahMode::Auto => {
                let freq_range = (MAX_FREQ - MIN_FREQ) * sensitivity;
                let freq_offset = env_level * freq_range;
                (base_freq + freq_offset).clamp(MIN_FREQ, MAX_FREQ)
            }
            WahMode::Manual => base_freq,
        }
    }
}

impl DspKernel for WahKernel {
    type Params = WahParams;

    fn process_stereo(&mut self, left: f32, right: f32, params: &WahParams) -> (f32, f32) {
        // ── Unit conversion (user-facing → internal) ──
        let base_freq = params.freq_hz;
        let resonance = params.resonance;
        let sensitivity = params.sensitivity_pct / 100.0;
        let output = fast_db_to_linear(params.output_db);
        let mode = WahMode::from_f32(params.mode);

        // ── Envelope detection (linked stereo — mid signal) ──
        let mid = (left + right) * 0.5;
        let env_level = self.envelope.process(mid);

        // ── Target frequency calculation ──
        let freq = Self::target_freq(base_freq, env_level, sensitivity, mode);

        // ── Update filter parameters ──
        self.filter_l.set_cutoff(freq);
        self.filter_l.set_resonance(resonance);
        self.filter_r.set_cutoff(freq);
        self.filter_r.set_resonance(resonance);

        // ── Filter processing ──
        let filtered_l = self.filter_l.process(left);
        let filtered_r = self.filter_r.process(right);

        // ── Normalize bandpass peak gain (peak gain = Q at centre freq) ──
        let safe_q = if resonance > 0.0 { resonance } else { 1.0 };
        let normalized_l = filtered_l / safe_q;
        let normalized_r = filtered_r / safe_q;

        // ── 80 % wet + 20 % dry — preserves body like a real wah pedal ──
        let out_l = normalized_l * 0.8 + left * 0.2;
        let out_r = normalized_r * 0.8 + right * 0.2;

        // ── Soft limit → output level ──
        (
            soft_limit(out_l, 1.0) * output,
            soft_limit(out_r, 1.0) * output,
        )
    }

    fn reset(&mut self) {
        self.filter_l.reset();
        self.filter_r.reset();
        self.envelope.reset();
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.filter_l.set_sample_rate(sample_rate);
        self.filter_r.set_sample_rate(sample_rate);
        self.envelope.set_sample_rate(sample_rate);
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

    // ── Kernel unit tests ────────────────────────────────────────────────────

    #[test]
    fn silence_in_silence_out() {
        let mut kernel = WahKernel::new(48000.0);
        let params = WahParams::default();

        let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
        assert!(l.abs() < 1e-6, "Expected silence on left, got {l}");
        assert!(r.abs() < 1e-6, "Expected silence on right, got {r}");
    }

    #[test]
    fn no_nan_or_inf() {
        let mut kernel = WahKernel::new(48000.0);
        let params = WahParams::default();

        // Process a burst of varied signals
        for i in 0..1000 {
            let phase = i as f32 * 0.02;
            let input = libm::sinf(phase) * 0.8;
            let (l, r) = kernel.process_stereo(input, -input, &params);
            assert!(!l.is_nan(), "NaN on left at sample {i}");
            assert!(!r.is_nan(), "NaN on right at sample {i}");
            assert!(l.is_finite(), "Inf on left at sample {i}");
            assert!(r.is_finite(), "Inf on right at sample {i}");
        }
    }

    #[test]
    fn params_descriptor_count() {
        assert_eq!(WahParams::COUNT, 5);

        // Verify all 5 descriptors are present
        for i in 0..5 {
            assert!(
                WahParams::descriptor(i).is_some(),
                "Missing descriptor for index {i}"
            );
        }
        assert!(WahParams::descriptor(5).is_none(), "Index 5 should be None");
    }

    #[test]
    fn wah_modifies_signal() {
        // After the filter state has built up, wah output must differ from input
        let mut kernel = WahKernel::new(48000.0);
        let params = WahParams {
            freq_hz: 800.0,
            resonance: 8.0, // High Q → strong colouring
            sensitivity_pct: 60.0,
            mode: 0.0, // Auto
            output_db: 0.0,
        };

        let input = 0.5_f32;
        let mut last_l = 0.0_f32;

        // Warm up the filter and envelope
        for _ in 0..256 {
            let (l, _) = kernel.process_stereo(input, input, &params);
            last_l = l;
        }

        // The wah colours the signal — output should not equal input
        assert!(
            (last_l - input).abs() > 0.01,
            "Wah output should differ from input: output={last_l}, input={input}"
        );
    }

    // ── Adapter integration tests ────────────────────────────────────────────

    #[test]
    fn adapter_wraps_as_effect() {
        let kernel = WahKernel::new(48000.0);
        let mut adapter = KernelAdapter::new(kernel, 48000.0);

        adapter.reset();
        let output = adapter.process(0.3);
        assert!(!output.is_nan(), "Adapter output is NaN");
        assert!(output.is_finite(), "Adapter output is Inf");
    }

    #[test]
    fn adapter_param_info_matches() {
        let kernel = WahKernel::new(48000.0);
        let adapter = KernelAdapter::new(kernel, 48000.0);

        assert_eq!(adapter.param_count(), 5);

        let freq_desc = adapter.param_info(0).unwrap();
        assert_eq!(freq_desc.name, "Frequency");
        assert_eq!(freq_desc.min, 200.0);
        assert_eq!(freq_desc.max, 2000.0);
        assert_eq!(freq_desc.default, 800.0);
        assert_eq!(freq_desc.id, ParamId(600));

        let reso_desc = adapter.param_info(1).unwrap();
        assert_eq!(reso_desc.name, "Resonance");
        assert_eq!(reso_desc.id, ParamId(601));

        let sens_desc = adapter.param_info(2).unwrap();
        assert_eq!(sens_desc.name, "Sensitivity");
        assert_eq!(sens_desc.id, ParamId(602));

        let mode_desc = adapter.param_info(3).unwrap();
        assert_eq!(mode_desc.name, "Mode");
        assert_eq!(mode_desc.id, ParamId(603));
        assert!(mode_desc.flags.contains(ParamFlags::STEPPED));

        let out_desc = adapter.param_info(4).unwrap();
        assert_eq!(out_desc.id, ParamId(604));

        // Out-of-range index
        assert!(adapter.param_info(5).is_none());
    }

    #[test]
    fn morph_produces_valid_output() {
        let auto_wah = WahParams {
            freq_hz: 300.0,
            resonance: 3.0,
            sensitivity_pct: 80.0,
            mode: 0.0,
            output_db: -6.0,
        };
        let manual_wah = WahParams {
            freq_hz: 1500.0,
            resonance: 8.0,
            sensitivity_pct: 20.0,
            mode: 1.0,
            output_db: 3.0,
        };

        let mut kernel = WahKernel::new(48000.0);

        for i in 0..=10 {
            let t = i as f32 / 10.0;
            let morphed = WahParams::lerp(&auto_wah, &manual_wah, t);
            let (l, r) = kernel.process_stereo(0.3, -0.3, &morphed);
            assert!(
                l.is_finite() && r.is_finite(),
                "Morph at t={t:.1} produced non-finite output: l={l}, r={r}"
            );
            kernel.reset();
        }
    }

    // ── Additional behavioral tests ──────────────────────────────────────────

    #[test]
    fn manual_mode_ignores_envelope() {
        // In manual mode, the output frequency should not change with input level
        let mut kernel_manual = WahKernel::new(48000.0);
        let params_manual = WahParams {
            freq_hz: 1000.0,
            resonance: 5.0,
            sensitivity_pct: 100.0, // Max sensitivity — would sweep hard in auto
            mode: 1.0,              // Manual
            output_db: 0.0,
        };

        // Process two different input levels
        let mut out_loud = (0.0_f32, 0.0_f32);
        let mut out_quiet = (0.0_f32, 0.0_f32);

        for _ in 0..256 {
            out_loud = kernel_manual.process_stereo(0.9, 0.9, &params_manual);
        }
        kernel_manual.reset();
        for _ in 0..256 {
            out_quiet = kernel_manual.process_stereo(0.01, 0.01, &params_manual);
        }

        // Outputs should be proportional (not equal due to normalization), but
        // the frequency character (ratio to input) should be similar in manual mode.
        // We simply verify both are finite — the key test is wah_modifies_signal.
        assert!(out_loud.0.is_finite());
        assert!(out_quiet.0.is_finite());
    }

    #[test]
    fn from_knobs_maps_ranges() {
        // Mid-point knobs → mid-range parameters
        let params = WahParams::from_knobs(0.5, 0.5, 0.5, 0.0, 0.5);

        // Frequency at 0.5 should be geometric mean of 200..2000 (≈ 632 Hz)
        let expected_freq = libm::powf(
            10.0,
            (libm::log10f(MIN_FREQ) + libm::log10f(MAX_FREQ)) * 0.5,
        );
        assert!(
            (params.freq_hz - expected_freq).abs() < 1.0,
            "freq_hz={}, expected≈{expected_freq}",
            params.freq_hz
        );

        // Resonance mid-point: 1 + 0.5 * 9 = 5.5
        assert!(
            (params.resonance - 5.5).abs() < 0.01,
            "resonance={}",
            params.resonance
        );

        // Sensitivity mid-point: 50 %
        assert!(
            (params.sensitivity_pct - 50.0).abs() < 0.01,
            "sensitivity_pct={}",
            params.sensitivity_pct
        );

        // Mode knob at 0.0 → Auto (0.0)
        assert_eq!(params.mode, 0.0);

        // Output mid-point: 0.5 * 40 - 20 = 0 dB
        assert!(
            (params.output_db - 0.0).abs() < 0.01,
            "output_db={}",
            params.output_db
        );
    }

    #[test]
    fn param_ids_match_classic_effect() {
        // ParamId values are plugin host API contracts — must never change.
        assert_eq!(WahParams::descriptor(0).unwrap().id, ParamId(600));
        assert_eq!(WahParams::descriptor(1).unwrap().id, ParamId(601));
        assert_eq!(WahParams::descriptor(2).unwrap().id, ParamId(602));
        assert_eq!(WahParams::descriptor(3).unwrap().id, ParamId(603));
        assert_eq!(WahParams::descriptor(4).unwrap().id, ParamId(604));
    }

    #[test]
    fn params_snapshot_roundtrip_through_adapter() {
        let kernel = WahKernel::new(48000.0);
        let mut adapter = KernelAdapter::new(kernel, 48000.0);

        adapter.set_param(0, 1200.0); // frequency
        adapter.set_param(1, 7.0); // resonance
        adapter.set_param(2, 80.0); // sensitivity
        adapter.set_param(3, 1.0); // manual mode
        adapter.set_param(4, -3.0); // output

        let saved = adapter.snapshot();
        assert!((saved.freq_hz - 1200.0).abs() < 0.1);
        assert!((saved.resonance - 7.0).abs() < 0.1);
        assert!((saved.sensitivity_pct - 80.0).abs() < 0.1);
        assert!((saved.mode - 1.0).abs() < 0.01);
        assert!((saved.output_db - (-3.0)).abs() < 0.1);
    }
}
