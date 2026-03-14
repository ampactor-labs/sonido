//! Bitcrusher kernel — bit-depth reduction and sample-rate decimation with jitter.
//!
//! `BitcrusherKernel` owns DSP state (held samples, counter, RNG state).
//! Parameters are received via `&BitcrusherParams` each sample. Deployed via
//! [`KernelAdapter`](sonido_core::KernelAdapter) for desktop/plugin, or called
//! directly on embedded targets.
//!
//! # Signal Flow
//!
//! ```text
//! Input → ZOH Sample Rate Reducer → Bit Depth Quantizer → Mix → Output Level
//! ```
//!
//! Two independent degradation mechanisms:
//! 1. **Zero-order hold (ZOH)** — repeats each input sample for N output samples,
//!    creating aliasing and staircase distortion (`downsample` > 1).
//! 2. **Uniform mid-tread quantization** — rounds amplitude to 2^B discrete levels,
//!    introducing broadband quantization noise (`bits` < 16).
//!
//! An optional **jitter** parameter adds random variation to the ZOH hold threshold,
//! simulating analog clock instability for more organic character.
//!
//! # References
//!
//! Zolzer, "DAFX: Digital Audio Effects" 2nd ed., Chapter 7 (Quantization and
//! Sample Rate Conversion).
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! let adapter = KernelAdapter::new(BitcrusherKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing, ADCs are hardware-filtered)
//! let mut kernel = BitcrusherKernel::new(48000.0);
//! let params = BitcrusherParams::from_knobs(adc_bits, adc_rate, adc_jitter, adc_mix, adc_output);
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

use libm::{floorf, powf};
use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::{
    ParamDescriptor, ParamFlags, ParamId, ParamUnit, fast_db_to_linear, wet_dry_mix_stereo,
};

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`BitcrusherKernel`].
///
/// All values in **user-facing units** — the same units shown in GUIs and
/// stored in presets. The kernel converts internally as needed.
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `bits` | steps | 2–16 | 8.0 |
/// | 1 | `rate` | steps | 1–64 | 1.0 |
/// | 2 | `jitter_pct` | % | 0–100 | 0.0 |
/// | 3 | `mix_pct` | % | 0–100 | 100.0 |
/// | 4 | `output_db` | dB | −20–+20 | 0.0 |
#[derive(Debug, Clone, Copy)]
pub struct BitcrusherParams {
    /// Bit depth (quantization resolution). Integer steps mapped to 2.0–16.0.
    pub bits: f32,
    /// Downsample factor. Integer steps mapped to 1.0–64.0; 1 = no rate reduction.
    pub rate: f32,
    /// Jitter amount in percent (0.0 = no jitter, 100.0 = full random variation).
    pub jitter_pct: f32,
    /// Wet/dry mix in percent (0.0 = dry, 100.0 = fully crushed).
    pub mix_pct: f32,
    /// Output level in decibels.
    pub output_db: f32,
}

impl Default for BitcrusherParams {
    fn default() -> Self {
        Self {
            bits: 8.0,
            rate: 1.0,
            jitter_pct: 0.0,
            mix_pct: 100.0,
            output_db: 0.0,
        }
    }
}

impl BitcrusherParams {
    /// Creates parameters from normalized 0–1 knob readings.
    ///
    /// Curves (logarithmic for frequency/time, linear for percentage) are
    /// derived from [`ParamDescriptor`] — same mapping as GUI and plugin hosts.
    ///
    /// - `bits`: 0.0 → 2 bits, 1.0 → 16 bits (stepped)
    /// - `rate`: 0.0 → 1× downsample, 1.0 → 64× downsample (stepped)
    /// - `jitter`: 0.0 → no jitter, 1.0 → 100% jitter
    /// - `mix`: 0.0 → dry, 1.0 → fully wet
    /// - `output`: 0.0 → −20 dB, 1.0 → +6 dB
    pub fn from_knobs(bits: f32, rate: f32, jitter: f32, mix: f32, output: f32) -> Self {
        Self::from_normalized(&[bits, rate, jitter, mix, output])
    }
}

impl KernelParams for BitcrusherParams {
    const COUNT: usize = 5;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(
                ParamDescriptor::custom("Bit Depth", "Bits", 2.0, 16.0, 8.0)
                    .with_step(1.0)
                    .with_id(ParamId(1700), "crush_bits")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED)),
            ),
            1 => Some(
                ParamDescriptor::custom("Downsample", "Down", 1.0, 64.0, 1.0)
                    .with_step(1.0)
                    .with_id(ParamId(1701), "crush_down")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED)),
            ),
            2 => Some(
                ParamDescriptor::custom("Jitter", "Jitter", 0.0, 100.0, 0.0)
                    .with_unit(ParamUnit::Percent)
                    .with_id(ParamId(1702), "crush_jitter"),
            ),
            3 => Some(
                // Match the classic effect: mix default is 100%, unit is percent
                ParamDescriptor {
                    default: 100.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(1703), "crush_mix"),
            ),
            4 => Some(
                sonido_core::gain::output_param_descriptor().with_id(ParamId(1704), "crush_output"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Standard, // bit depth — smoothed for automation
            1 => SmoothingStyle::Standard, // downsample — smoothed for automation
            2 => SmoothingStyle::None,     // jitter — stochastic, no smoothing needed
            3 => SmoothingStyle::Standard, // mix
            4 => SmoothingStyle::Standard, // output level
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.bits,
            1 => self.rate,
            2 => self.jitter_pct,
            3 => self.mix_pct,
            4 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.bits = value,
            1 => self.rate = value,
            2 => self.jitter_pct = value,
            3 => self.mix_pct = value,
            4 => self.output_db = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP bitcrusher kernel.
///
/// Contains ONLY the mutable state required for audio processing:
/// - Held left/mono and right sample values (zero-order hold state)
/// - Fractional sample counter for the ZOH boundary
/// - LCG PRNG state for jitter generation
///
/// No `SmoothedParam`, no atomics, no platform awareness.
///
/// ## DSP State
///
/// The ZOH counter (`counter`) increments each sample. When it reaches
/// the downsample threshold (with optional jitter), a new input is latched
/// and quantized. The fractional overshoot is preserved across latch boundaries
/// so the effective rate stays accurate when `downsample` changes mid-cycle.
///
/// ## Jitter Implementation
///
/// A 32-bit LCG PRNG (Numerical Recipes constants: `a = 1664525`, `c = 1013904223`)
/// generates random hold duration variations. The upper 16 bits are extracted to
/// reduce correlation between successive values.
pub struct BitcrusherKernel {
    /// Sample rate for reference (currently informational — ZOH is sample-count based).
    sample_rate: f32,

    /// Currently held left (or mono) sample. Updated on each latch boundary.
    held_l: f32,

    /// Currently held right sample. Updated on each latch boundary, independently from L.
    held_r: f32,

    /// Fractional sample counter for ZOH boundary detection.
    ///
    /// Increments by 1.0 per sample. When `counter >= threshold`, a new input
    /// is latched. The fractional overshoot is preserved for accurate timing.
    counter: f32,

    /// LCG PRNG state for jitter generation.
    ///
    /// Initialized to `0x1234_5678` and reset to that value on [`reset()`](Self).
    /// This ensures deterministic behaviour after reset.
    rng_state: u32,
}

impl BitcrusherKernel {
    /// Create a new bitcrusher kernel at the given sample rate.
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            held_l: 0.0,
            held_r: 0.0,
            counter: 0.0,
            rng_state: 0x1234_5678,
        }
    }

    /// Quantize a single sample to `bits` bit depth via mid-tread uniform quantization.
    ///
    /// Formula:
    /// ```text
    /// levels = 2^bits
    /// quantized = floor(x · levels + 0.5) / levels
    /// ```
    ///
    /// The `+ 0.5` shifts from floor-rounding to nearest-level rounding.
    /// Input range is assumed to be approximately [-1.0, 1.0]; values outside
    /// that range are still quantized correctly but may clip.
    ///
    /// Reference: Zolzer, "DAFX: Digital Audio Effects" 2nd ed., Chapter 7.
    #[inline]
    fn quantize(sample: f32, bits: f32) -> f32 {
        let levels = powf(2.0, bits);
        floorf(sample * levels + 0.5) / levels
    }

    /// Advance the LCG PRNG and return a value in [0.0, 1.0).
    ///
    /// Uses Numerical Recipes LCG constants (`a = 1664525`, `c = 1013904223`)
    /// which have good statistical properties for a 32-bit generator. The upper
    /// 16 bits are used to reduce correlation between successive values.
    #[inline]
    fn next_random(&mut self) -> f32 {
        self.rng_state = self
            .rng_state
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        // Extract upper 16 bits as u16 (max 65535) before widening to f32.
        // Casting u16 → f32 is lossless (u16 fits in 23-bit mantissa exactly).
        let upper = (self.rng_state >> 16) as u16;
        f32::from(upper) / 65_536.0
    }

    /// Compute the ZOH latch threshold for this cycle, with optional jitter.
    ///
    /// When `jitter > 0`, adds a random offset in `[0, jitter · downsample)` so
    /// the hold duration varies each cycle, simulating analog clock instability.
    ///
    /// `jitter` is expected in the range [0.0, 1.0] (fraction, not percent).
    #[inline]
    fn threshold_with_jitter(&mut self, downsample: f32, jitter: f32) -> f32 {
        if jitter > 0.0 {
            downsample + self.next_random() * jitter * downsample
        } else {
            downsample
        }
    }
}

impl DspKernel for BitcrusherKernel {
    type Params = BitcrusherParams;

    /// Process a stereo sample pair through the bitcrusher.
    ///
    /// L and R channels are crushed independently (separate held samples) but
    /// share the same counter/threshold. This gives dual-mono behaviour: the hold
    /// boundary fires at the same moment for both channels, preserving stereo
    /// imaging while crushing each side independently.
    ///
    /// ## Unit Conversions
    ///
    /// - `params.jitter_pct` (0–100%) → fraction (0.0–1.0) before use in threshold
    /// - `params.mix_pct` (0–100%) → fraction (0.0–1.0) for wet/dry blend
    /// - `params.output_db` (dB) → linear gain via `fast_db_to_linear`
    #[inline]
    fn process_stereo(&mut self, left: f32, right: f32, params: &BitcrusherParams) -> (f32, f32) {
        // Unit conversions: user-facing → kernel-internal
        let jitter = params.jitter_pct / 100.0;
        let mix = params.mix_pct / 100.0;
        let output_gain = fast_db_to_linear(params.output_db);

        self.counter += 1.0;
        let threshold = self.threshold_with_jitter(params.rate, jitter);
        if self.counter >= threshold {
            self.held_l = Self::quantize(left, params.bits);
            self.held_r = Self::quantize(right, params.bits);
            // Keep only the fractional overshoot so transitions are smooth
            // when downsample changes mid-cycle.
            self.counter -= floorf(self.counter);
        }

        let (wet_l, wet_r) = wet_dry_mix_stereo(left, right, self.held_l, self.held_r, mix);
        (wet_l * output_gain, wet_r * output_gain)
    }

    /// Reset all DSP state to initial values.
    ///
    /// Clears held samples, resets the ZOH counter, and re-seeds the PRNG to its
    /// initial value (`0x1234_5678`) for deterministic post-reset behaviour.
    fn reset(&mut self) {
        self.held_l = 0.0;
        self.held_r = 0.0;
        self.counter = 0.0;
        self.rng_state = 0x1234_5678;
    }

    /// Update internal state for a new sample rate.
    ///
    /// The bitcrusher ZOH operates in sample counts (not seconds), so no
    /// coefficient recalculation is required. The sample rate is stored for
    /// reference/debugging.
    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
    }

    /// Returns `false` — the bitcrusher is dual-mono (shared counter, independent channels).
    fn is_true_stereo(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_core::kernel::KernelAdapter;
    use sonido_core::{Effect, ParameterInfo};

    // ── Kernel unit tests ──

    #[test]
    fn silence_in_silence_out() {
        let mut kernel = BitcrusherKernel::new(48000.0);
        let params = BitcrusherParams::default();

        let (left, right) = kernel.process_stereo(0.0, 0.0, &params);
        assert!(left.abs() < 1e-6, "Expected silence on left, got {left}");
        assert!(right.abs() < 1e-6, "Expected silence on right, got {right}");
    }

    #[test]
    fn no_nan_or_inf() {
        let mut kernel = BitcrusherKernel::new(48000.0);
        let params = BitcrusherParams {
            bits: 2.0,
            rate: 8.0,
            jitter_pct: 50.0,
            mix_pct: 100.0,
            output_db: 0.0,
        };

        for i in 0..1024 {
            let t = i as f32 * core::f32::consts::PI * 0.01;
            let input = libm::sinf(t);
            let (l, r) = kernel.process_stereo(input, -input, &params);
            assert!(!l.is_nan(), "Left is NaN at sample {i}");
            assert!(!r.is_nan(), "Right is NaN at sample {i}");
            assert!(l.is_finite(), "Left is Inf at sample {i}");
            assert!(r.is_finite(), "Right is Inf at sample {i}");
        }
    }

    #[test]
    fn params_descriptor_count() {
        assert_eq!(BitcrusherParams::COUNT, 5);

        let d0 = BitcrusherParams::descriptor(0).unwrap();
        assert_eq!(d0.name, "Bit Depth");
        assert_eq!(d0.min, 2.0);
        assert_eq!(d0.max, 16.0);
        assert_eq!(d0.default, 8.0);
        assert_eq!(d0.id, ParamId(1700));
        assert!(d0.flags.contains(ParamFlags::STEPPED));

        let d1 = BitcrusherParams::descriptor(1).unwrap();
        assert_eq!(d1.name, "Downsample");
        assert_eq!(d1.min, 1.0);
        assert_eq!(d1.max, 64.0);
        assert_eq!(d1.default, 1.0);
        assert_eq!(d1.id, ParamId(1701));
        assert!(d1.flags.contains(ParamFlags::STEPPED));

        let d2 = BitcrusherParams::descriptor(2).unwrap();
        assert_eq!(d2.name, "Jitter");
        assert_eq!(d2.min, 0.0);
        assert_eq!(d2.max, 100.0);
        assert_eq!(d2.default, 0.0);
        assert_eq!(d2.id, ParamId(1702));

        let d3 = BitcrusherParams::descriptor(3).unwrap();
        assert_eq!(d3.name, "Mix");
        assert_eq!(d3.default, 100.0);
        assert_eq!(d3.id, ParamId(1703));

        let d4 = BitcrusherParams::descriptor(4).unwrap();
        assert_eq!(d4.name, "Output");
        assert_eq!(d4.default, 0.0);
        assert_eq!(d4.id, ParamId(1704));

        // Out-of-range returns None
        assert!(BitcrusherParams::descriptor(5).is_none());
    }

    #[test]
    fn lower_bits_increases_distortion() {
        // With fully wet mix and zero jitter, lower bit depth should produce
        // more quantization noise (greater deviation from input).
        let input = 0.3_f32;

        let high_bits = BitcrusherParams {
            bits: 16.0,
            rate: 1.0,
            jitter_pct: 0.0,
            mix_pct: 100.0,
            output_db: 0.0,
        };
        let low_bits = BitcrusherParams {
            bits: 2.0,
            rate: 1.0,
            jitter_pct: 0.0,
            mix_pct: 100.0,
            output_db: 0.0,
        };

        let mut kernel_high = BitcrusherKernel::new(48000.0);
        let (out_high, _) = kernel_high.process_stereo(input, input, &high_bits);

        let mut kernel_low = BitcrusherKernel::new(48000.0);
        let (out_low, _) = kernel_low.process_stereo(input, input, &low_bits);

        let error_high = (out_high - input).abs();
        let error_low = (out_low - input).abs();

        assert!(
            error_low > error_high,
            "2-bit should produce more error than 16-bit: low={error_low}, high={error_high}"
        );
    }

    #[test]
    fn dry_mix_passes_input() {
        let params = BitcrusherParams {
            bits: 2.0, // heavy crushing
            rate: 8.0, // heavy downsampling
            jitter_pct: 0.0,
            mix_pct: 0.0, // fully dry — input should pass unchanged
            output_db: 0.0,
        };

        let mut kernel = BitcrusherKernel::new(48000.0);
        let input = 0.35_f32;

        // Process several samples; all should pass through unmodified.
        for _ in 0..10 {
            let (l, r) = kernel.process_stereo(input, input, &params);
            assert!(
                (l - input).abs() < 1e-5,
                "Dry mix: left should equal input {input}, got {l}"
            );
            assert!(
                (r - input).abs() < 1e-5,
                "Dry mix: right should equal input {input}, got {r}"
            );
        }
    }

    // ── Adapter integration tests ──

    #[test]
    fn adapter_wraps_as_effect() {
        let kernel = BitcrusherKernel::new(48000.0);
        let mut adapter = KernelAdapter::new(kernel, 48000.0);

        adapter.reset();
        let output = adapter.process(0.3);
        assert!(!output.is_nan(), "Adapter output is NaN");
        assert!(output.is_finite(), "Adapter output is Inf");
    }

    #[test]
    fn adapter_param_info_matches() {
        let kernel = BitcrusherKernel::new(48000.0);
        let adapter = KernelAdapter::new(kernel, 48000.0);

        assert_eq!(adapter.param_count(), 5);

        assert_eq!(adapter.param_info(0).unwrap().name, "Bit Depth");
        assert_eq!(adapter.param_info(1).unwrap().name, "Downsample");
        assert_eq!(adapter.param_info(2).unwrap().name, "Jitter");
        assert_eq!(adapter.param_info(3).unwrap().name, "Mix");
        assert_eq!(adapter.param_info(4).unwrap().name, "Output");
        assert!(adapter.param_info(5).is_none());

        // ParamIds must match classic effect exactly
        assert_eq!(adapter.param_info(0).unwrap().id, ParamId(1700));
        assert_eq!(adapter.param_info(1).unwrap().id, ParamId(1701));
        assert_eq!(adapter.param_info(2).unwrap().id, ParamId(1702));
        assert_eq!(adapter.param_info(3).unwrap().id, ParamId(1703));
        assert_eq!(adapter.param_info(4).unwrap().id, ParamId(1704));
    }

    #[test]
    fn morph_produces_valid_output() {
        let clean = BitcrusherParams {
            bits: 16.0,
            rate: 1.0,
            jitter_pct: 0.0,
            mix_pct: 0.0,
            output_db: 0.0,
        };
        let crushed = BitcrusherParams {
            bits: 2.0,
            rate: 32.0,
            jitter_pct: 100.0,
            mix_pct: 100.0,
            output_db: -6.0,
        };

        let mut kernel = BitcrusherKernel::new(48000.0);

        for i in 0..=10 {
            let t = i as f32 / 10.0;
            let morphed = BitcrusherParams::lerp(&clean, &crushed, t);
            let (l, r) = kernel.process_stereo(0.3, -0.3, &morphed);
            assert!(l.is_finite(), "Left is NaN/Inf at morph t={t:.1}: {l}");
            assert!(r.is_finite(), "Right is NaN/Inf at morph t={t:.1}: {r}");
            kernel.reset();
        }
    }

    // ── Additional behavioral tests ──

    #[test]
    fn default_params_are_descriptor_defaults() {
        let params = BitcrusherParams::from_defaults();
        assert!(
            (params.bits - 8.0).abs() < 1e-6,
            "bits default should be 8.0"
        );
        assert!(
            (params.rate - 1.0).abs() < 1e-6,
            "rate default should be 1.0"
        );
        assert!(
            (params.jitter_pct - 0.0).abs() < 1e-6,
            "jitter default should be 0.0"
        );
        assert!(
            (params.mix_pct - 100.0).abs() < 1e-6,
            "mix default should be 100.0"
        );
        assert!(
            (params.output_db - 0.0).abs() < 1e-6,
            "output default should be 0.0"
        );
    }

    #[test]
    fn from_knobs_maps_full_range() {
        // All-zero knobs → minimum values
        let min_params = BitcrusherParams::from_knobs(0.0, 0.0, 0.0, 0.0, 0.0);
        assert!((min_params.bits - 2.0).abs() < 1.0, "min bits should be ~2");
        assert!((min_params.rate - 1.0).abs() < 1.0, "min rate should be ~1");
        assert!((min_params.jitter_pct - 0.0).abs() < 1e-5);
        assert!((min_params.mix_pct - 0.0).abs() < 1e-5);
        assert!((min_params.output_db - (-20.0)).abs() < 1e-4);

        // All-one knobs → maximum values
        let max_params = BitcrusherParams::from_knobs(1.0, 1.0, 1.0, 1.0, 1.0);
        assert!(
            (max_params.bits - 16.0).abs() < 1.0,
            "max bits should be ~16"
        );
        assert!(
            (max_params.rate - 64.0).abs() < 1.0,
            "max rate should be ~64"
        );
        assert!((max_params.jitter_pct - 100.0).abs() < 1e-4);
        assert!((max_params.mix_pct - 100.0).abs() < 1e-4);
        assert!((max_params.output_db - 6.0).abs() < 1e-4);
    }

    #[test]
    fn kernel_reset_clears_state() {
        let mut kernel = BitcrusherKernel::new(48000.0);
        let params = BitcrusherParams {
            bits: 8.0,
            rate: 8.0,
            jitter_pct: 0.0,
            mix_pct: 1.0,
            output_db: 0.0,
        };

        // Process some audio to fill held samples
        for _ in 0..20 {
            kernel.process_stereo(0.7, -0.7, &params);
        }

        kernel.reset();

        assert!(
            (kernel.held_l).abs() < 1e-10,
            "held_l should be zero after reset"
        );
        assert!(
            (kernel.held_r).abs() < 1e-10,
            "held_r should be zero after reset"
        );
        assert!(
            (kernel.counter).abs() < 1e-10,
            "counter should be zero after reset"
        );
    }

    #[test]
    fn adapter_set_get_roundtrip() {
        let kernel = BitcrusherKernel::new(48000.0);
        let mut adapter = KernelAdapter::new(kernel, 48000.0);

        adapter.set_param(0, 4.0); // bit depth
        assert!((adapter.get_param(0) - 4.0).abs() < 1e-5, "bits roundtrip");

        adapter.set_param(1, 16.0); // downsample
        assert!((adapter.get_param(1) - 16.0).abs() < 1e-5, "rate roundtrip");

        adapter.set_param(2, 50.0); // jitter %
        assert!(
            (adapter.get_param(2) - 50.0).abs() < 1e-5,
            "jitter roundtrip"
        );

        adapter.set_param(3, 75.0); // mix %
        assert!((adapter.get_param(3) - 75.0).abs() < 1e-5, "mix roundtrip");
    }
}
