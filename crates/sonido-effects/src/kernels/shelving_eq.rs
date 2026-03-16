//! Shelving EQ kernel — low shelf + high shelf with output gain.
//!
//! `ShelvingEqKernel` owns DSP state (four biquad filters, sample rate,
//! coefficient caches). Parameters are received via `&ShelvingEqParams` each
//! sample. Deployed via [`Adapter`](sonido_core::kernel::Adapter) for
//! desktop/plugin, or called directly on embedded targets.
//!
//! # Signal Flow
//!
//! ```text
//! Input → Low Shelf Biquad → High Shelf Biquad → Output Level
//! ```
//!
//! # DSP Theory
//!
//! Both bands are shelving EQ biquads from the RBJ Audio EQ Cookbook
//! (Bristow-Johnson, 1994).
//!
//! **Low shelf** (boosts/cuts below `low_freq_hz`):
//!
//! ```text
//! A  = 10^(dBgain/40)
//! ω₀ = 2π·f₀/fs
//! S  = 1.0  (shelf slope = 1)
//! α  = sin(ω₀)/2 · √((A + 1/A)·(1/S − 1) + 2)
//!
//! b0 =  A·[ (A+1) − (A−1)·cos(ω₀) + 2·√A·α ]
//! b1 = 2A·[ (A−1) − (A+1)·cos(ω₀)           ]
//! b2 =  A·[ (A+1) − (A−1)·cos(ω₀) − 2·√A·α ]
//! a0 =    [ (A+1) + (A−1)·cos(ω₀) + 2·√A·α ]
//! a1 = −2·[ (A−1) + (A+1)·cos(ω₀)           ]
//! a2 =    [ (A+1) + (A−1)·cos(ω₀) − 2·√A·α ]
//! ```
//!
//! **High shelf** (boosts/cuts above `high_freq_hz`) uses the symmetric
//! complementary formula (same α, b/a indices transposed).
//!
//! Coefficients are recomputed at most every `COEFF_UPDATE_INTERVAL` samples
//! when parameter values have changed, matching the strategy used by
//! [`EqKernel`](super::eq::EqKernel).
//!
//! Reference: Robert Bristow-Johnson, "Cookbook formulae for audio EQ biquad
//! filter coefficients", 1994.
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! let adapter = Adapter::new(ShelvingEqKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing, ADCs are hardware-filtered)
//! let mut kernel = ShelvingEqKernel::new(48000.0);
//! let params = ShelvingEqParams::from_knobs(
//!     adc_low_freq, adc_low_gain, adc_high_freq, adc_high_gain, adc_output,
//! );
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::{
    Biquad, Cached, ParamDescriptor, ParamId, ParamScale, ParamUnit, fast_db_to_linear,
};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Samples between biquad coefficient recalculations during parameter sweeps.
///
/// 32 samples = 0.67 ms at 48 kHz — well below the audible threshold for
/// zipper artifacts while avoiding per-sample transcendental math.
const COEFF_UPDATE_INTERVAL: u32 = 32;

/// Threshold for triggering a coefficient recalculation.
const CHANGE_EPSILON: f32 = 0.001;

// ═══════════════════════════════════════════════════════════════════════════
//  Shelf coefficient helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Compute RBJ low-shelf biquad coefficients (shelf slope S = 1).
///
/// Returns `(b0, b1, b2, a0, a1, a2)`.
///
/// With S = 1 the shelf is maximally smooth; the slope term simplifies to:
///
/// ```text
/// α = sin(ω₀)/2 · √(2)   [S=1 specialisation of the general form]
/// ```
///
/// Reference: Bristow-Johnson, "Audio EQ Cookbook", 1994.
fn low_shelf_coefficients(
    frequency: f32,
    gain_db: f32,
    sample_rate: f32,
) -> (f32, f32, f32, f32, f32, f32) {
    use libm::{cosf, powf, sinf, sqrtf};

    // A = sqrt(10^(gain/20)) = 10^(gain/40)
    let a = powf(10.0, gain_db / 40.0);
    let omega = 2.0 * core::f32::consts::PI * frequency / sample_rate;
    let cos_w = cosf(omega);
    let sin_w = sinf(omega);
    // S = 1 shelf slope: α = sin(ω₀)/2 · √((A+1/A)·(1/S−1)+2) simplifies to
    // α = sin(ω₀)/2 · √2 = sin(ω₀)/√2
    let alpha = sin_w / core::f32::consts::SQRT_2;
    let sqrt_a = sqrtf(a);
    let two_sqrt_a_alpha = 2.0 * sqrt_a * alpha;

    let b0 = a * ((a + 1.0) - (a - 1.0) * cos_w + two_sqrt_a_alpha);
    let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w);
    let b2 = a * ((a + 1.0) - (a - 1.0) * cos_w - two_sqrt_a_alpha);
    let a0 = (a + 1.0) + (a - 1.0) * cos_w + two_sqrt_a_alpha;
    let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cos_w);
    let a2 = (a + 1.0) + (a - 1.0) * cos_w - two_sqrt_a_alpha;

    (b0, b1, b2, a0, a1, a2)
}

/// Compute RBJ high-shelf biquad coefficients (shelf slope S = 1).
///
/// Returns `(b0, b1, b2, a0, a1, a2)`. Same shelf slope and α as the low
/// shelf; b/a numerator terms use the complementary (sign-flipped cosine)
/// arrangement.
///
/// Reference: Bristow-Johnson, "Audio EQ Cookbook", 1994.
fn high_shelf_coefficients(
    frequency: f32,
    gain_db: f32,
    sample_rate: f32,
) -> (f32, f32, f32, f32, f32, f32) {
    use libm::{cosf, powf, sinf, sqrtf};

    let a = powf(10.0, gain_db / 40.0);
    let omega = 2.0 * core::f32::consts::PI * frequency / sample_rate;
    let cos_w = cosf(omega);
    let sin_w = sinf(omega);
    // Same S=1 alpha: sin(ω₀)/√2
    let alpha = sin_w / core::f32::consts::SQRT_2;
    let sqrt_a = sqrtf(a);
    let two_sqrt_a_alpha = 2.0 * sqrt_a * alpha;

    let b0 = a * ((a + 1.0) + (a - 1.0) * cos_w + two_sqrt_a_alpha);
    let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w);
    let b2 = a * ((a + 1.0) + (a - 1.0) * cos_w - two_sqrt_a_alpha);
    let a0 = (a + 1.0) - (a - 1.0) * cos_w + two_sqrt_a_alpha;
    let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w);
    let a2 = (a + 1.0) - (a - 1.0) * cos_w - two_sqrt_a_alpha;

    (b0, b1, b2, a0, a1, a2)
}

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`ShelvingEqKernel`].
///
/// All values are in **user-facing units** — the same units shown in GUIs and
/// stored in presets. The kernel converts internally as needed.
///
/// ## Parameter Table
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `low_freq_hz` | Hz | 20–500 | 100.0 |
/// | 1 | `low_gain_db` | dB | −15–15 | 0.0 |
/// | 2 | `high_freq_hz` | Hz | 1000–20000 | 8000.0 |
/// | 3 | `high_gain_db` | dB | −15–15 | 0.0 |
/// | 4 | `output_db` | dB | −60–6 | 0.0 |
#[derive(Debug, Clone, Copy)]
pub struct ShelvingEqParams {
    /// Low shelf corner frequency in Hz.
    ///
    /// Range: 20.0 to 500.0 Hz, default 100.0. Logarithmic scale.
    pub low_freq_hz: f32,

    /// Low shelf gain in dB.
    ///
    /// Range: −15.0 to +15.0 dB, default 0.0. Positive = boost below corner.
    pub low_gain_db: f32,

    /// High shelf corner frequency in Hz.
    ///
    /// Range: 1000.0 to 20000.0 Hz, default 8000.0. Logarithmic scale.
    pub high_freq_hz: f32,

    /// High shelf gain in dB.
    ///
    /// Range: −15.0 to +15.0 dB, default 0.0. Positive = boost above corner.
    pub high_gain_db: f32,

    /// Output level in decibels.
    ///
    /// Range: −60.0 to +6.0 dB, default 0.0.
    pub output_db: f32,
}

impl Default for ShelvingEqParams {
    fn default() -> Self {
        Self {
            low_freq_hz: 100.0,
            low_gain_db: 0.0,
            high_freq_hz: 8000.0,
            high_gain_db: 0.0,
            output_db: 0.0,
        }
    }
}

impl ShelvingEqParams {
    /// Creates parameters from normalized 0–1 knob readings.
    ///
    /// | Argument | Index | Parameter | Range |
    /// |----------|-------|-----------|-------|
    /// | `low_f` | 0 | `low_freq_hz` | 20–500 Hz (log) |
    /// | `low_g` | 1 | `low_gain_db` | −15–+15 dB |
    /// | `high_f` | 2 | `high_freq_hz` | 1000–20000 Hz (log) |
    /// | `high_g` | 3 | `high_gain_db` | −15–+15 dB |
    /// | `output` | 4 | `output_db` | −60–+6 dB |
    pub fn from_knobs(low_f: f32, low_g: f32, high_f: f32, high_g: f32, output: f32) -> Self {
        Self::from_normalized(&[low_f, low_g, high_f, high_g, output])
    }
}

impl KernelParams for ShelvingEqParams {
    const COUNT: usize = 5;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(
                ParamDescriptor {
                    name: "Low Freq",
                    short_name: "LowFrq",
                    unit: ParamUnit::Hertz,
                    min: 20.0,
                    max: 500.0,
                    default: 100.0,
                    step: 1.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(2500), "shelf_low_freq")
                .with_scale(ParamScale::Logarithmic),
            ),
            1 => Some(
                ParamDescriptor::gain_db("Low Gain", "LowGn", -15.0, 15.0, 0.0)
                    .with_id(ParamId(2501), "shelf_low_gain"),
            ),
            2 => Some(
                ParamDescriptor {
                    name: "High Freq",
                    short_name: "HiFrq",
                    unit: ParamUnit::Hertz,
                    min: 1000.0,
                    max: 20000.0,
                    default: 8000.0,
                    step: 100.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(2502), "shelf_high_freq")
                .with_scale(ParamScale::Logarithmic),
            ),
            3 => Some(
                ParamDescriptor::gain_db("High Gain", "HiGn", -15.0, 15.0, 0.0)
                    .with_id(ParamId(2503), "shelf_high_gain"),
            ),
            4 => Some(
                sonido_core::gain::output_param_descriptor().with_id(ParamId(2504), "shelf_output"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Slow, // low_freq_hz — filter coefficient
            1 => SmoothingStyle::Slow, // low_gain_db — shelf gain, avoid zipper
            2 => SmoothingStyle::Slow, // high_freq_hz — filter coefficient
            3 => SmoothingStyle::Slow, // high_gain_db — shelf gain, avoid zipper
            4 => SmoothingStyle::Fast, // output_db — level control, 5 ms
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.low_freq_hz,
            1 => self.low_gain_db,
            2 => self.high_freq_hz,
            3 => self.high_gain_db,
            4 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.low_freq_hz = value,
            1 => self.low_gain_db = value,
            2 => self.high_freq_hz = value,
            3 => self.high_gain_db = value,
            4 => self.output_db = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP shelving EQ kernel: RBJ low shelf + high shelf in series.
///
/// Contains ONLY the mutable state required for audio processing:
/// - Four biquad filters (low shelf L/R, high shelf L/R)
/// - Sample rate for Nyquist clamping and coefficient recalculation
/// - Per-band coefficient caches (recompute only on change)
/// - Coefficient decimation counter (recalculate at most every 32 samples)
///
/// No `SmoothedParam`, no `AtomicU32`, no platform awareness. This is a
/// dual-mono topology — the same coefficients are applied to both channels.
///
/// # Invariants
///
/// `coeff_update_counter` wraps via `wrapping_sub(1)` and fires on zero.
/// Initialized to `1` so the first sample triggers a coefficient computation.
pub struct ShelvingEqKernel {
    /// Sample rate in Hz.
    sample_rate: f32,

    /// Low shelf biquad — left channel.
    low_l: Biquad,
    /// Low shelf biquad — right channel.
    low_r: Biquad,

    /// High shelf biquad — left channel.
    high_l: Biquad,
    /// High shelf biquad — right channel.
    high_r: Biquad,

    /// Change-detector for low shelf biquad coefficients.
    ///
    /// Keyed on `[low_freq_hz, low_gain_db]`. Avoids `sinf`/`cosf`/`powf`
    /// when parameters are stable.
    low_cache: Cached<[f32; 6]>,

    /// Change-detector for high shelf biquad coefficients.
    ///
    /// Keyed on `[high_freq_hz, high_gain_db]`.
    high_cache: Cached<[f32; 6]>,

    /// Down-counter for block-rate coefficient decimation.
    coeff_update_counter: u32,
}

impl ShelvingEqKernel {
    /// Create a new shelving EQ kernel at the given sample rate.
    pub fn new(sample_rate: f32) -> Self {
        let defaults = ShelvingEqParams::default();

        let compute_low = |freq: f32, gain_db: f32, sr: f32| -> [f32; 6] {
            let f = freq.clamp(20.0, sr * 0.475);
            let (b0, b1, b2, a0, a1, a2) = low_shelf_coefficients(f, gain_db, sr);
            [b0, b1, b2, a0, a1, a2]
        };
        let compute_high = |freq: f32, gain_db: f32, sr: f32| -> [f32; 6] {
            let f = freq.clamp(1000.0, sr * 0.475);
            let (b0, b1, b2, a0, a1, a2) = high_shelf_coefficients(f, gain_db, sr);
            [b0, b1, b2, a0, a1, a2]
        };

        let init_low = compute_low(defaults.low_freq_hz, defaults.low_gain_db, sample_rate);
        let init_high = compute_high(defaults.high_freq_hz, defaults.high_gain_db, sample_rate);

        let mut low_l = Biquad::new();
        let mut low_r = Biquad::new();
        low_l.set_coefficients(
            init_low[0],
            init_low[1],
            init_low[2],
            init_low[3],
            init_low[4],
            init_low[5],
        );
        low_r.set_coefficients(
            init_low[0],
            init_low[1],
            init_low[2],
            init_low[3],
            init_low[4],
            init_low[5],
        );

        let mut high_l = Biquad::new();
        let mut high_r = Biquad::new();
        high_l.set_coefficients(
            init_high[0],
            init_high[1],
            init_high[2],
            init_high[3],
            init_high[4],
            init_high[5],
        );
        high_r.set_coefficients(
            init_high[0],
            init_high[1],
            init_high[2],
            init_high[3],
            init_high[4],
            init_high[5],
        );

        let mut low_cache = Cached::new(init_low, 2);
        low_cache.update(
            &[defaults.low_freq_hz, defaults.low_gain_db],
            CHANGE_EPSILON,
            |inputs| compute_low(inputs[0], inputs[1], sample_rate),
        );

        let mut high_cache = Cached::new(init_high, 2);
        high_cache.update(
            &[defaults.high_freq_hz, defaults.high_gain_db],
            CHANGE_EPSILON,
            |inputs| compute_high(inputs[0], inputs[1], sample_rate),
        );

        Self {
            sample_rate,
            low_l,
            low_r,
            high_l,
            high_r,
            low_cache,
            high_cache,
            coeff_update_counter: 1,
        }
    }

    /// Apply a coefficient array to an L/R biquad pair.
    #[inline]
    fn apply_coefficients(filter_l: &mut Biquad, filter_r: &mut Biquad, c: [f32; 6]) {
        filter_l.set_coefficients(c[0], c[1], c[2], c[3], c[4], c[5]);
        filter_r.set_coefficients(c[0], c[1], c[2], c[3], c[4], c[5]);
    }
}

impl DspKernel for ShelvingEqKernel {
    type Params = ShelvingEqParams;

    fn process_stereo(&mut self, left: f32, right: f32, params: &ShelvingEqParams) -> (f32, f32) {
        // ── Coefficient decimation ──────────────────────────────────────────
        self.coeff_update_counter = self.coeff_update_counter.wrapping_sub(1);
        if self.coeff_update_counter == 0 {
            self.coeff_update_counter = COEFF_UPDATE_INTERVAL;

            let sr = self.sample_rate;

            let low_c = *self.low_cache.update(
                &[params.low_freq_hz, params.low_gain_db],
                CHANGE_EPSILON,
                |inputs| {
                    let freq = inputs[0].clamp(20.0, sr * 0.475);
                    let (b0, b1, b2, a0, a1, a2) = low_shelf_coefficients(freq, inputs[1], sr);
                    [b0, b1, b2, a0, a1, a2]
                },
            );
            Self::apply_coefficients(&mut self.low_l, &mut self.low_r, low_c);

            let high_c = *self.high_cache.update(
                &[params.high_freq_hz, params.high_gain_db],
                CHANGE_EPSILON,
                |inputs| {
                    let freq = inputs[0].clamp(1000.0, sr * 0.475);
                    let (b0, b1, b2, a0, a1, a2) = high_shelf_coefficients(freq, inputs[1], sr);
                    [b0, b1, b2, a0, a1, a2]
                },
            );
            Self::apply_coefficients(&mut self.high_l, &mut self.high_r, high_c);
        }

        let output_gain = fast_db_to_linear(params.output_db);

        // ── Signal path: Low shelf → High shelf → Output ────────────────────
        let left_out = self.high_l.process(self.low_l.process(left)) * output_gain;
        let right_out = self.high_r.process(self.low_r.process(right)) * output_gain;

        (left_out, right_out)
    }

    fn reset(&mut self) {
        self.low_l.clear();
        self.low_r.clear();
        self.high_l.clear();
        self.high_r.clear();
        self.low_cache.invalidate();
        self.high_cache.invalidate();
        self.coeff_update_counter = 1;
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.low_cache.invalidate();
        self.high_cache.invalidate();
        self.coeff_update_counter = 1;
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
    fn silence_in_silence_out() {
        let mut kernel = ShelvingEqKernel::new(48000.0);
        let params = ShelvingEqParams::default();
        let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
        assert!(l.abs() < 1e-6, "Expected silence on left, got {l}");
        assert!(r.abs() < 1e-6, "Expected silence on right, got {r}");
    }

    #[test]
    fn no_nan_or_inf() {
        let mut kernel = ShelvingEqKernel::new(48000.0);
        let params = ShelvingEqParams {
            low_gain_db: 15.0,
            high_gain_db: -15.0,
            ..Default::default()
        };
        for i in 0..1000 {
            let t = i as f32 / 48000.0;
            let s = libm::sinf(2.0 * core::f32::consts::PI * 440.0 * t);
            let (l, r) = kernel.process_stereo(s, s, &params);
            assert!(l.is_finite(), "Left NaN/Inf at sample {i}: {l}");
            assert!(r.is_finite(), "Right NaN/Inf at sample {i}: {r}");
        }
    }

    #[test]
    fn params_descriptor_count() {
        assert_eq!(ShelvingEqParams::COUNT, 5);

        let d0 = ShelvingEqParams::descriptor(0).expect("index 0 must exist");
        assert_eq!(d0.name, "Low Freq");
        assert!((d0.min - 20.0).abs() < 0.01);
        assert!((d0.max - 500.0).abs() < 0.01);
        assert!((d0.default - 100.0).abs() < 0.01);
        assert_eq!(d0.id, ParamId(2500));
        assert_eq!(d0.string_id, "shelf_low_freq");

        let d1 = ShelvingEqParams::descriptor(1).expect("index 1 must exist");
        assert_eq!(d1.name, "Low Gain");
        assert_eq!(d1.id, ParamId(2501));
        assert_eq!(d1.string_id, "shelf_low_gain");

        let d2 = ShelvingEqParams::descriptor(2).expect("index 2 must exist");
        assert_eq!(d2.name, "High Freq");
        assert_eq!(d2.id, ParamId(2502));
        assert_eq!(d2.string_id, "shelf_high_freq");

        let d3 = ShelvingEqParams::descriptor(3).expect("index 3 must exist");
        assert_eq!(d3.name, "High Gain");
        assert_eq!(d3.id, ParamId(2503));
        assert_eq!(d3.string_id, "shelf_high_gain");

        let d4 = ShelvingEqParams::descriptor(4).expect("index 4 must exist");
        assert_eq!(d4.name, "Output");
        assert_eq!(d4.id, ParamId(2504));
        assert_eq!(d4.string_id, "shelf_output");

        assert!(ShelvingEqParams::descriptor(5).is_none());
    }

    #[test]
    fn adapter_wraps_as_effect() {
        let kernel = ShelvingEqKernel::new(48000.0);
        let mut adapter = Adapter::new(kernel, 48000.0);
        adapter.reset();
        let output = adapter.process(0.3);
        assert!(!output.is_nan(), "adapter.process() returned NaN");
        assert!(output.is_finite(), "adapter.process() returned Inf");
    }

    /// Low shelf boost should increase energy below the corner frequency.
    ///
    /// A 15 dB low shelf boost at 200 Hz should raise RMS energy from a
    /// 100 Hz sine more than from a flat response.
    #[test]
    fn low_shelf_boost_increases_low_energy() {
        let sr = 48000.0;
        let test_freq = 100.0_f32; // well below 200 Hz shelf corner

        let flat = ShelvingEqParams::default();
        let boosted = ShelvingEqParams {
            low_freq_hz: 200.0,
            low_gain_db: 15.0,
            ..Default::default()
        };

        let mut kernel_flat = ShelvingEqKernel::new(sr);
        let mut kernel_boosted = ShelvingEqKernel::new(sr);

        // Warm up
        for i in 0..256 {
            let t = i as f32 / sr;
            let s = libm::sinf(2.0 * core::f32::consts::PI * test_freq * t);
            kernel_flat.process_stereo(s, s, &flat);
            kernel_boosted.process_stereo(s, s, &boosted);
        }

        // Measure energy
        let mut energy_flat = 0.0_f32;
        let mut energy_boosted = 0.0_f32;
        for i in 256..768 {
            let t = i as f32 / sr;
            let s = libm::sinf(2.0 * core::f32::consts::PI * test_freq * t);
            let (l_f, _) = kernel_flat.process_stereo(s, s, &flat);
            let (l_b, _) = kernel_boosted.process_stereo(s, s, &boosted);
            energy_flat += l_f * l_f;
            energy_boosted += l_b * l_b;
        }

        assert!(
            energy_boosted > energy_flat,
            "Low shelf boost should increase low energy: flat={energy_flat:.4}, boosted={energy_boosted:.4}"
        );
    }

    /// High shelf cut should reduce energy above the corner frequency.
    #[test]
    fn high_shelf_cut_reduces_high_energy() {
        let sr = 48000.0;
        let test_freq = 16000.0_f32; // well above 8000 Hz shelf corner

        let flat = ShelvingEqParams::default();
        let cut = ShelvingEqParams {
            high_freq_hz: 8000.0,
            high_gain_db: -15.0,
            ..Default::default()
        };

        let mut kernel_flat = ShelvingEqKernel::new(sr);
        let mut kernel_cut = ShelvingEqKernel::new(sr);

        for i in 0..256 {
            let t = i as f32 / sr;
            let s = libm::sinf(2.0 * core::f32::consts::PI * test_freq * t);
            kernel_flat.process_stereo(s, s, &flat);
            kernel_cut.process_stereo(s, s, &cut);
        }

        let mut energy_flat = 0.0_f32;
        let mut energy_cut = 0.0_f32;
        for i in 256..768 {
            let t = i as f32 / sr;
            let s = libm::sinf(2.0 * core::f32::consts::PI * test_freq * t);
            let (l_f, _) = kernel_flat.process_stereo(s, s, &flat);
            let (l_c, _) = kernel_cut.process_stereo(s, s, &cut);
            energy_flat += l_f * l_f;
            energy_cut += l_c * l_c;
        }

        assert!(
            energy_cut < energy_flat,
            "High shelf cut should reduce high energy: flat={energy_flat:.4}, cut={energy_cut:.4}"
        );
    }

    /// Both gains at 0 dB should produce unity gain after settling.
    #[test]
    fn flat_response_unity_gain() {
        let mut kernel = ShelvingEqKernel::new(48000.0);
        let params = ShelvingEqParams::default(); // all gains = 0 dB

        for _ in 0..2000 {
            kernel.process_stereo(1.0, 1.0, &params);
        }

        let (l, r) = kernel.process_stereo(1.0, 1.0, &params);
        assert!(
            (l - 1.0).abs() < 0.05,
            "Flat shelving EQ should pass DC at unity, got left={l}"
        );
        assert!(
            (r - 1.0).abs() < 0.05,
            "Flat shelving EQ should pass DC at unity, got right={r}"
        );
    }
}
