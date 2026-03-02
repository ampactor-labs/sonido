//! 3-band parametric EQ kernel — pure DSP with separated parameter ownership.
//!
//! Implements the EQ effect using the kernel architecture.
//! The DSP math is identical; the difference is structural:
//!
//! - **Classic `ParametricEq`**: owns `SmoothedParam` for all 10 parameters,
//!   manages smoothing internally, implements `Effect` + `ParameterInfo` via
//!   `impl_params!`.
//!
//! - **`EqKernel`**: owns ONLY DSP state (six biquad filters, sample
//!   rate, coefficient caches, decimation counter). Parameters are received via
//!   `&EqParams` on each processing call. Deployed via
//!   [`KernelAdapter`](sonido_core::KernelAdapter) for desktop/plugin, or called
//!   directly on embedded targets.
//!
//! # Signal Flow
//!
//! ```text
//! Input → Low Band Peaking EQ → Mid Band Peaking EQ → High Band Peaking EQ
//!                                                              ↓
//!                                                        Soft Limit (1.0)
//!                                                              ↓
//!                                                        Output Level
//! ```
//!
//! # DSP Theory
//!
//! Each band is a peaking EQ biquad filter (Audio EQ Cookbook, Bristow-Johnson):
//!
//! ```text
//! H(z) = (b0 + b1·z⁻¹ + b2·z⁻²) / (a0 + a1·z⁻¹ + a2·z⁻²)
//!
//! A  = 10^(dBgain/40)
//! ω₀ = 2π·f₀/fs
//! α  = sin(ω₀)/(2·Q)
//!
//! b0 =  1 + α·A
//! b1 = −2·cos(ω₀)
//! b2 =  1 − α·A
//! a0 =  1 + α/A
//! a1 = −2·cos(ω₀)
//! a2 =  1 − α/A
//! ```
//!
//! Coefficients are recomputed at most every `COEFF_UPDATE_INTERVAL` samples
//! (32 samples = 0.67 ms at 48 kHz) when parameter values have changed, matching
//! the coefficient decimation strategy of the classic effect.
//!
//! The three bands cascade in series: Low → Mid → High. A soft limiter at
//! threshold 1.0 prevents clipping from accumulated band boosts before the
//! final output level stage.
//!
//! Reference: Robert Bristow-Johnson, "Cookbook formulae for audio EQ biquad
//! filter coefficients", 1994.
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! let adapter = KernelAdapter::new(EqKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing, ADCs are hardware-filtered)
//! let mut kernel = EqKernel::new(48000.0);
//! let params = EqParams::from_knobs(
//!     adc_low_f, adc_low_g, adc_low_q,
//!     adc_mid_f, adc_mid_g, adc_mid_q,
//!     adc_high_f, adc_high_g, adc_high_q,
//!     adc_output,
//! );
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::math::soft_limit;
use sonido_core::{
    Biquad, ParamDescriptor, ParamId, ParamScale, ParamUnit, fast_db_to_linear,
    peaking_eq_coefficients,
};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Samples between biquad coefficient recalculations during parameter sweeps.
///
/// 32 samples = 0.67 ms at 48 kHz — well below the audible threshold for
/// filter sweep zipper artifacts while avoiding per-sample transcendental math.
const COEFF_UPDATE_INTERVAL: u32 = 32;

/// Threshold for triggering a coefficient recalculation.
///
/// When any cached frequency, gain, or Q value deviates from the current
/// smoothed value by more than this epsilon, the band's biquad coefficients
/// are recomputed at the next decimation boundary.
const CHANGE_EPSILON: f32 = 0.001;

// ── Unit conversion ───────────────────────────────────────────────────────────

/// Fast dB-to-linear conversion for per-sample use.
///
/// Uses `sonido_core::fast_db_to_linear`, a polynomial approximation with
/// ~0.1 dB accuracy — approximately 4× faster than `10^(db/20)`.
#[inline]
fn db_to_gain(db: f32) -> f32 {
    fast_db_to_linear(db)
}

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`EqKernel`].
///
/// All values are in **user-facing units** — the same units shown in GUIs and
/// stored in presets. The kernel converts internally as needed.
///
/// ## Parameter Table
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `low_freq_hz` | Hz | 20–500 | 100.0 |
/// | 1 | `low_gain_db` | dB | −12–12 | 0.0 |
/// | 2 | `low_q` | ratio | 0.5–5.0 | 1.0 |
/// | 3 | `mid_freq_hz` | Hz | 200–5000 | 1000.0 |
/// | 4 | `mid_gain_db` | dB | −12–12 | 0.0 |
/// | 5 | `mid_q` | ratio | 0.5–5.0 | 1.0 |
/// | 6 | `high_freq_hz` | Hz | 1000–15000 | 5000.0 |
/// | 7 | `high_gain_db` | dB | −12–12 | 0.0 |
/// | 8 | `high_q` | ratio | 0.5–5.0 | 1.0 |
/// | 9 | `output_db` | dB | −20–20 | 0.0 |
#[derive(Debug, Clone, Copy)]
pub struct EqParams {
    /// Low band center frequency in Hz.
    ///
    /// Range: 20.0 to 500.0 Hz, default 100.0.
    pub low_freq_hz: f32,

    /// Low band peaking gain in dB.
    ///
    /// Range: −12.0 to 12.0 dB, default 0.0.
    pub low_gain_db: f32,

    /// Low band Q factor (bandwidth control).
    ///
    /// Range: 0.5 to 5.0, default 1.0. Higher Q = narrower boost/cut.
    pub low_q: f32,

    /// Mid band center frequency in Hz.
    ///
    /// Range: 200.0 to 5000.0 Hz, default 1000.0.
    pub mid_freq_hz: f32,

    /// Mid band peaking gain in dB.
    ///
    /// Range: −12.0 to 12.0 dB, default 0.0.
    pub mid_gain_db: f32,

    /// Mid band Q factor (bandwidth control).
    ///
    /// Range: 0.5 to 5.0, default 1.0. Higher Q = narrower boost/cut.
    pub mid_q: f32,

    /// High band center frequency in Hz.
    ///
    /// Range: 1000.0 to 15000.0 Hz, default 5000.0.
    pub high_freq_hz: f32,

    /// High band peaking gain in dB.
    ///
    /// Range: −12.0 to 12.0 dB, default 0.0.
    pub high_gain_db: f32,

    /// High band Q factor (bandwidth control).
    ///
    /// Range: 0.5 to 5.0, default 1.0. Higher Q = narrower boost/cut.
    pub high_q: f32,

    /// Output level in decibels.
    ///
    /// Range: −20.0 to 20.0 dB, default 0.0.
    pub output_db: f32,
}

impl Default for EqParams {
    fn default() -> Self {
        Self {
            low_freq_hz: 100.0,
            low_gain_db: 0.0,
            low_q: 1.0,
            mid_freq_hz: 1000.0,
            mid_gain_db: 0.0,
            mid_q: 1.0,
            high_freq_hz: 5000.0,
            high_gain_db: 0.0,
            high_q: 1.0,
            output_db: 0.0,
        }
    }
}

impl EqParams {
    /// Build params directly from hardware knob readings (0.0–1.0 normalized).
    ///
    /// Convenience for embedded targets where ADC values map to parameter
    /// ranges. Frequency controls use logarithmic mapping to match auditory
    /// perception; gain and Q use linear mapping.
    ///
    /// # Parameters
    /// - `low_f`: normalized 0.0–1.0 → 20–500 Hz (logarithmic)
    /// - `low_g`: normalized 0.0–1.0 → −12–12 dB (linear)
    /// - `low_q`: normalized 0.0–1.0 → 0.5–5.0 (linear)
    /// - `mid_f`: normalized 0.0–1.0 → 200–5000 Hz (logarithmic)
    /// - `mid_g`: normalized 0.0–1.0 → −12–12 dB (linear)
    /// - `mid_q`: normalized 0.0–1.0 → 0.5–5.0 (linear)
    /// - `high_f`: normalized 0.0–1.0 → 1000–15000 Hz (logarithmic)
    /// - `high_g`: normalized 0.0–1.0 → −12–12 dB (linear)
    /// - `high_q`: normalized 0.0–1.0 → 0.5–5.0 (linear)
    /// - `output`: normalized 0.0–1.0 → −20–20 dB (linear)
    #[allow(clippy::too_many_arguments)]
    pub fn from_knobs(
        low_f: f32,
        low_g: f32,
        low_q: f32,
        mid_f: f32,
        mid_g: f32,
        mid_q: f32,
        high_f: f32,
        high_g: f32,
        high_q: f32,
        output: f32,
    ) -> Self {
        // Logarithmic frequency mapping: min * (max/min)^t
        // Low: 20 * (500/20)^t = 20 * 25^t
        let low_freq_hz = 20.0 * libm::powf(25.0, low_f.clamp(0.0, 1.0));
        // Mid: 200 * (5000/200)^t = 200 * 25^t
        let mid_freq_hz = 200.0 * libm::powf(25.0, mid_f.clamp(0.0, 1.0));
        // High: 1000 * (15000/1000)^t = 1000 * 15^t
        let high_freq_hz = 1000.0 * libm::powf(15.0, high_f.clamp(0.0, 1.0));

        Self {
            low_freq_hz,
            low_gain_db: low_g.clamp(0.0, 1.0) * 24.0 - 12.0, // −12–12 dB
            low_q: 0.5 + low_q.clamp(0.0, 1.0) * 4.5,         // 0.5–5.0
            mid_freq_hz,
            mid_gain_db: mid_g.clamp(0.0, 1.0) * 24.0 - 12.0, // −12–12 dB
            mid_q: 0.5 + mid_q.clamp(0.0, 1.0) * 4.5,         // 0.5–5.0
            high_freq_hz,
            high_gain_db: high_g.clamp(0.0, 1.0) * 24.0 - 12.0, // −12–12 dB
            high_q: 0.5 + high_q.clamp(0.0, 1.0) * 4.5,         // 0.5–5.0
            output_db: output.clamp(0.0, 1.0) * 40.0 - 20.0,    // −20–20 dB
        }
    }
}

impl KernelParams for EqParams {
    const COUNT: usize = 10;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            // Low band
            0 => Some(
                ParamDescriptor {
                    name: "Low Frequency",
                    short_name: "LowFreq",
                    unit: ParamUnit::Hertz,
                    min: 20.0,
                    max: 500.0,
                    default: 100.0,
                    step: 1.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(500), "eq_low_freq")
                .with_scale(ParamScale::Logarithmic),
            ),
            1 => Some(
                ParamDescriptor::gain_db("Low Gain", "LowGain", -12.0, 12.0, 0.0)
                    .with_id(ParamId(501), "eq_low_gain"),
            ),
            2 => Some(
                ParamDescriptor {
                    name: "Low Q",
                    short_name: "LowQ",
                    unit: ParamUnit::None,
                    min: 0.5,
                    max: 5.0,
                    default: 1.0,
                    step: 0.1,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(502), "eq_low_q"),
            ),
            // Mid band
            3 => Some(
                ParamDescriptor {
                    name: "Mid Frequency",
                    short_name: "MidFreq",
                    unit: ParamUnit::Hertz,
                    min: 200.0,
                    max: 5000.0,
                    default: 1000.0,
                    step: 10.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(503), "eq_mid_freq")
                .with_scale(ParamScale::Logarithmic),
            ),
            4 => Some(
                ParamDescriptor::gain_db("Mid Gain", "MidGain", -12.0, 12.0, 0.0)
                    .with_id(ParamId(504), "eq_mid_gain"),
            ),
            5 => Some(
                ParamDescriptor {
                    name: "Mid Q",
                    short_name: "MidQ",
                    unit: ParamUnit::None,
                    min: 0.5,
                    max: 5.0,
                    default: 1.0,
                    step: 0.1,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(505), "eq_mid_q"),
            ),
            // High band
            6 => Some(
                ParamDescriptor {
                    name: "High Frequency",
                    short_name: "HighFreq",
                    unit: ParamUnit::Hertz,
                    min: 1000.0,
                    max: 15000.0,
                    default: 5000.0,
                    step: 100.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(506), "eq_high_freq")
                .with_scale(ParamScale::Logarithmic),
            ),
            7 => Some(
                ParamDescriptor::gain_db("High Gain", "HighGain", -12.0, 12.0, 0.0)
                    .with_id(ParamId(507), "eq_high_gain"),
            ),
            8 => Some(
                ParamDescriptor {
                    name: "High Q",
                    short_name: "HighQ",
                    unit: ParamUnit::None,
                    min: 0.5,
                    max: 5.0,
                    default: 1.0,
                    step: 0.1,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(508), "eq_high_q"),
            ),
            // Output
            9 => Some(
                sonido_core::gain::output_param_descriptor().with_id(ParamId(509), "eq_output"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Slow, // low_freq_hz — filter coefficient, avoid zipper
            1 => SmoothingStyle::Standard, // low_gain_db — gain, moderate smoothing
            2 => SmoothingStyle::Slow, // low_q — filter coefficient, avoid zipper
            3 => SmoothingStyle::Slow, // mid_freq_hz — filter coefficient
            4 => SmoothingStyle::Standard, // mid_gain_db
            5 => SmoothingStyle::Slow, // mid_q — filter coefficient
            6 => SmoothingStyle::Slow, // high_freq_hz — filter coefficient
            7 => SmoothingStyle::Standard, // high_gain_db
            8 => SmoothingStyle::Slow, // high_q — filter coefficient
            9 => SmoothingStyle::Standard, // output_db — level control
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.low_freq_hz,
            1 => self.low_gain_db,
            2 => self.low_q,
            3 => self.mid_freq_hz,
            4 => self.mid_gain_db,
            5 => self.mid_q,
            6 => self.high_freq_hz,
            7 => self.high_gain_db,
            8 => self.high_q,
            9 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.low_freq_hz = value,
            1 => self.low_gain_db = value,
            2 => self.low_q = value,
            3 => self.mid_freq_hz = value,
            4 => self.mid_gain_db = value,
            5 => self.mid_q = value,
            6 => self.high_freq_hz = value,
            7 => self.high_gain_db = value,
            8 => self.high_q = value,
            9 => self.output_db = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP 3-band parametric equalizer kernel.
///
/// Contains ONLY the mutable state required for audio processing:
/// - Six biquad filters (low/mid/high × L/R)
/// - Sample rate (for Nyquist clamping and coefficient recalculation)
/// - Per-band coefficient caches (nine cached values — recompute only on change)
/// - Coefficient decimation counter (recalculate at most every 32 samples)
///
/// No `SmoothedParam`, no `AtomicU32`, no platform awareness. Coefficients
/// are recomputed at most every `COEFF_UPDATE_INTERVAL` samples when any
/// band's frequency, gain, or Q has changed beyond `CHANGE_EPSILON`.
///
/// The dual L/R biquad pairs implement a dual-mono topology: the same
/// coefficients are applied to both channels (not true stereo decorrelation),
/// so [`DspKernel::process_stereo`] returns `false` for `is_true_stereo`.
pub struct EqKernel {
    /// Sample rate in Hz, used for Nyquist clamping and coefficient computation.
    sample_rate: f32,

    /// Low band biquad filter — left channel.
    low_l: Biquad,
    /// Low band biquad filter — right channel.
    low_r: Biquad,

    /// Mid band biquad filter — left channel.
    mid_l: Biquad,
    /// Mid band biquad filter — right channel.
    mid_r: Biquad,

    /// High band biquad filter — left channel.
    high_l: Biquad,
    /// High band biquad filter — right channel.
    high_r: Biquad,

    // ── Coefficient caches — each tracks the last value used to compute biquad
    //    coefficients. A NaN sentinel forces recalculation on the first call.
    /// Cached low band center frequency (Hz) from the last coefficient update.
    last_low_freq_hz: f32,
    /// Cached low band gain (dB) from the last coefficient update.
    last_low_gain: f32,
    /// Cached low band Q from the last coefficient update.
    last_low_q: f32,

    /// Cached mid band center frequency (Hz) from the last coefficient update.
    last_mid_freq_hz: f32,
    /// Cached mid band gain (dB) from the last coefficient update.
    last_mid_gain: f32,
    /// Cached mid band Q from the last coefficient update.
    last_mid_q: f32,

    /// Cached high band center frequency (Hz) from the last coefficient update.
    last_high_freq_hz: f32,
    /// Cached high band gain (dB) from the last coefficient update.
    last_high_gain: f32,
    /// Cached high band Q from the last coefficient update.
    last_high_q: f32,

    /// Down-counter for block-rate coefficient decimation.
    ///
    /// Decrements each sample. When it reaches zero, pending coefficient
    /// updates are applied and the counter reloads to `COEFF_UPDATE_INTERVAL`.
    coeff_update_counter: u32,
}

impl EqKernel {
    /// Create a new parametric EQ kernel at the given sample rate.
    ///
    /// All six biquad filters are initialised with default parameters
    /// (flat response: 100/1000/5000 Hz, 0 dB gain, Q=1.0).
    pub fn new(sample_rate: f32) -> Self {
        let mut kernel = Self {
            sample_rate,

            low_l: Biquad::new(),
            low_r: Biquad::new(),
            mid_l: Biquad::new(),
            mid_r: Biquad::new(),
            high_l: Biquad::new(),
            high_r: Biquad::new(),

            // NaN sentinels force coefficient computation on the first call.
            last_low_freq_hz: f32::NAN,
            last_low_gain: f32::NAN,
            last_low_q: f32::NAN,
            last_mid_freq_hz: f32::NAN,
            last_mid_gain: f32::NAN,
            last_mid_q: f32::NAN,
            last_high_freq_hz: f32::NAN,
            last_high_gain: f32::NAN,
            last_high_q: f32::NAN,

            coeff_update_counter: 1,
        };

        // Initialise with default params to set real coefficients immediately.
        let defaults = EqParams::default();
        kernel.update_low_coefficients(defaults.low_freq_hz, defaults.low_gain_db, defaults.low_q);
        kernel.update_mid_coefficients(defaults.mid_freq_hz, defaults.mid_gain_db, defaults.mid_q);
        kernel.update_high_coefficients(
            defaults.high_freq_hz,
            defaults.high_gain_db,
            defaults.high_q,
        );
        kernel
    }

    /// Clamp a frequency to 95 % of Nyquist to prevent biquad instability.
    ///
    /// Biquad coefficients become numerically unstable as the pole frequency
    /// approaches Nyquist. Clamping to 95 % (`sample_rate × 0.475`) provides
    /// sufficient headroom at all standard sample rates.
    #[inline]
    fn clamp_to_nyquist(&self, freq: f32) -> f32 {
        let max_freq = self.sample_rate * 0.475;
        if freq > max_freq { max_freq } else { freq }
    }

    /// Recompute biquad coefficients for the low band and apply to both filters.
    ///
    /// Uses the RBJ peaking EQ formula. Both the left and right channel filters
    /// receive identical coefficients (dual-mono topology).
    fn update_low_coefficients(&mut self, freq: f32, gain_db: f32, q: f32) {
        let freq = self.clamp_to_nyquist(freq);
        let (b0, b1, b2, a0, a1, a2) = peaking_eq_coefficients(freq, q, gain_db, self.sample_rate);
        self.low_l.set_coefficients(b0, b1, b2, a0, a1, a2);
        self.low_r.set_coefficients(b0, b1, b2, a0, a1, a2);
        self.last_low_freq_hz = freq;
        self.last_low_gain = gain_db;
        self.last_low_q = q;
    }

    /// Recompute biquad coefficients for the mid band and apply to both filters.
    ///
    /// Uses the RBJ peaking EQ formula. Both the left and right channel filters
    /// receive identical coefficients (dual-mono topology).
    fn update_mid_coefficients(&mut self, freq: f32, gain_db: f32, q: f32) {
        let freq = self.clamp_to_nyquist(freq);
        let (b0, b1, b2, a0, a1, a2) = peaking_eq_coefficients(freq, q, gain_db, self.sample_rate);
        self.mid_l.set_coefficients(b0, b1, b2, a0, a1, a2);
        self.mid_r.set_coefficients(b0, b1, b2, a0, a1, a2);
        self.last_mid_freq_hz = freq;
        self.last_mid_gain = gain_db;
        self.last_mid_q = q;
    }

    /// Recompute biquad coefficients for the high band and apply to both filters.
    ///
    /// Uses the RBJ peaking EQ formula. Both the left and right channel filters
    /// receive identical coefficients (dual-mono topology).
    fn update_high_coefficients(&mut self, freq: f32, gain_db: f32, q: f32) {
        let freq = self.clamp_to_nyquist(freq);
        let (b0, b1, b2, a0, a1, a2) = peaking_eq_coefficients(freq, q, gain_db, self.sample_rate);
        self.high_l.set_coefficients(b0, b1, b2, a0, a1, a2);
        self.high_r.set_coefficients(b0, b1, b2, a0, a1, a2);
        self.last_high_freq_hz = freq;
        self.last_high_gain = gain_db;
        self.last_high_q = q;
    }

    /// Returns `true` if the low band parameters have drifted beyond the update
    /// threshold, indicating that coefficient recalculation is needed.
    #[inline]
    fn low_needs_update(&self, params: &EqParams) -> bool {
        (params.low_freq_hz - self.last_low_freq_hz).abs() > CHANGE_EPSILON
            || (params.low_gain_db - self.last_low_gain).abs() > CHANGE_EPSILON
            || (params.low_q - self.last_low_q).abs() > CHANGE_EPSILON
    }

    /// Returns `true` if the mid band parameters have drifted beyond the update
    /// threshold, indicating that coefficient recalculation is needed.
    #[inline]
    fn mid_needs_update(&self, params: &EqParams) -> bool {
        (params.mid_freq_hz - self.last_mid_freq_hz).abs() > CHANGE_EPSILON
            || (params.mid_gain_db - self.last_mid_gain).abs() > CHANGE_EPSILON
            || (params.mid_q - self.last_mid_q).abs() > CHANGE_EPSILON
    }

    /// Returns `true` if the high band parameters have drifted beyond the update
    /// threshold, indicating that coefficient recalculation is needed.
    #[inline]
    fn high_needs_update(&self, params: &EqParams) -> bool {
        (params.high_freq_hz - self.last_high_freq_hz).abs() > CHANGE_EPSILON
            || (params.high_gain_db - self.last_high_gain).abs() > CHANGE_EPSILON
            || (params.high_q - self.last_high_q).abs() > CHANGE_EPSILON
    }
}

impl DspKernel for EqKernel {
    type Params = EqParams;

    fn process_stereo(&mut self, left: f32, right: f32, params: &EqParams) -> (f32, f32) {
        // ── Coefficient decimation ────────────────────────────────────────────
        // Coefficients are recomputed at most once every COEFF_UPDATE_INTERVAL
        // samples, preventing per-sample transcendental function calls while
        // keeping parameter changes audibly smooth.
        self.coeff_update_counter = self.coeff_update_counter.wrapping_sub(1);
        if self.coeff_update_counter == 0 {
            self.coeff_update_counter = COEFF_UPDATE_INTERVAL;

            if self.low_needs_update(params) {
                self.update_low_coefficients(params.low_freq_hz, params.low_gain_db, params.low_q);
            }
            if self.mid_needs_update(params) {
                self.update_mid_coefficients(params.mid_freq_hz, params.mid_gain_db, params.mid_q);
            }
            if self.high_needs_update(params) {
                self.update_high_coefficients(
                    params.high_freq_hz,
                    params.high_gain_db,
                    params.high_q,
                );
            }
        }

        // ── Unit conversion ───────────────────────────────────────────────────
        let output_gain = db_to_gain(params.output_db);

        // ── Signal path: Low → Mid → High → Soft Limit → Output Level ────────
        // Left channel through L filters
        let left_low = self.low_l.process(left);
        let left_mid = self.mid_l.process(left_low);
        let left_out = soft_limit(self.high_l.process(left_mid), 1.0) * output_gain;

        // Right channel through R filters
        let right_low = self.low_r.process(right);
        let right_mid = self.mid_r.process(right_low);
        let right_out = soft_limit(self.high_r.process(right_mid), 1.0) * output_gain;

        (left_out, right_out)
    }

    fn reset(&mut self) {
        self.low_l.clear();
        self.low_r.clear();
        self.mid_l.clear();
        self.mid_r.clear();
        self.high_l.clear();
        self.high_r.clear();

        // Invalidate all caches — forces coefficient recomputation on the next
        // processing call so filters are correctly initialised after reset.
        self.last_low_freq_hz = f32::NAN;
        self.last_low_gain = f32::NAN;
        self.last_low_q = f32::NAN;
        self.last_mid_freq_hz = f32::NAN;
        self.last_mid_gain = f32::NAN;
        self.last_mid_q = f32::NAN;
        self.last_high_freq_hz = f32::NAN;
        self.last_high_gain = f32::NAN;
        self.last_high_q = f32::NAN;

        self.coeff_update_counter = 1;
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        // Invalidate all caches — rate change invalidates all cached coefficients
        // since they are sample-rate dependent. The next call to process_stereo
        // will trigger a full recomputation via the NaN sentinels.
        self.last_low_freq_hz = f32::NAN;
        self.last_low_gain = f32::NAN;
        self.last_low_q = f32::NAN;
        self.last_mid_freq_hz = f32::NAN;
        self.last_mid_gain = f32::NAN;
        self.last_mid_q = f32::NAN;
        self.last_high_freq_hz = f32::NAN;
        self.last_high_gain = f32::NAN;
        self.last_high_q = f32::NAN;

        self.coeff_update_counter = 1;
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

    // ── Kernel unit tests ──────────────────────────────────────────────────

    #[test]
    fn silence_in_silence_out() {
        let mut kernel = EqKernel::new(48000.0);
        let params = EqParams::default();

        let (left, right) = kernel.process_stereo(0.0, 0.0, &params);
        assert!(left.abs() < 1e-6, "Expected silence on left, got {left}");
        assert!(right.abs() < 1e-6, "Expected silence on right, got {right}");
    }

    #[test]
    fn no_nan_or_inf() {
        let mut kernel = EqKernel::new(48000.0);
        let params = EqParams::default();

        for i in 0..1000 {
            let t = i as f32 / 48000.0;
            let input = libm::sinf(2.0 * core::f32::consts::PI * 440.0 * t);
            let (left, right) = kernel.process_stereo(input, input, &params);
            assert!(left.is_finite(), "Left NaN/Inf at sample {i}: {left}");
            assert!(right.is_finite(), "Right NaN/Inf at sample {i}: {right}");
        }
    }

    #[test]
    fn params_descriptor_count() {
        assert_eq!(EqParams::COUNT, 10);

        // Low band
        let d0 = EqParams::descriptor(0).expect("index 0 must exist");
        assert_eq!(d0.name, "Low Frequency");
        assert!((d0.min - 20.0).abs() < 0.01);
        assert!((d0.max - 500.0).abs() < 0.01);
        assert!((d0.default - 100.0).abs() < 0.01);
        assert_eq!(d0.id, ParamId(500));
        assert_eq!(d0.string_id, "eq_low_freq");

        let d1 = EqParams::descriptor(1).expect("index 1 must exist");
        assert_eq!(d1.name, "Low Gain");
        assert!((d1.min - (-12.0)).abs() < 0.01);
        assert!((d1.max - 12.0).abs() < 0.01);
        assert_eq!(d1.id, ParamId(501));
        assert_eq!(d1.string_id, "eq_low_gain");

        let d2 = EqParams::descriptor(2).expect("index 2 must exist");
        assert_eq!(d2.name, "Low Q");
        assert!((d2.min - 0.5).abs() < 0.01);
        assert!((d2.max - 5.0).abs() < 0.01);
        assert_eq!(d2.id, ParamId(502));
        assert_eq!(d2.string_id, "eq_low_q");

        // Mid band
        let d3 = EqParams::descriptor(3).expect("index 3 must exist");
        assert_eq!(d3.name, "Mid Frequency");
        assert_eq!(d3.id, ParamId(503));
        assert_eq!(d3.string_id, "eq_mid_freq");

        let d4 = EqParams::descriptor(4).expect("index 4 must exist");
        assert_eq!(d4.name, "Mid Gain");
        assert_eq!(d4.id, ParamId(504));
        assert_eq!(d4.string_id, "eq_mid_gain");

        let d5 = EqParams::descriptor(5).expect("index 5 must exist");
        assert_eq!(d5.name, "Mid Q");
        assert_eq!(d5.id, ParamId(505));
        assert_eq!(d5.string_id, "eq_mid_q");

        // High band
        let d6 = EqParams::descriptor(6).expect("index 6 must exist");
        assert_eq!(d6.name, "High Frequency");
        assert!((d6.min - 1000.0).abs() < 0.01);
        assert!((d6.max - 15000.0).abs() < 0.01);
        assert!((d6.default - 5000.0).abs() < 0.01);
        assert_eq!(d6.id, ParamId(506));
        assert_eq!(d6.string_id, "eq_high_freq");

        let d7 = EqParams::descriptor(7).expect("index 7 must exist");
        assert_eq!(d7.name, "High Gain");
        assert_eq!(d7.id, ParamId(507));
        assert_eq!(d7.string_id, "eq_high_gain");

        let d8 = EqParams::descriptor(8).expect("index 8 must exist");
        assert_eq!(d8.name, "High Q");
        assert_eq!(d8.id, ParamId(508));
        assert_eq!(d8.string_id, "eq_high_q");

        // Output
        let d9 = EqParams::descriptor(9).expect("index 9 must exist");
        assert_eq!(d9.name, "Output");
        assert_eq!(d9.id, ParamId(509));
        assert_eq!(d9.string_id, "eq_output");

        assert!(EqParams::descriptor(10).is_none(), "index 10 must be None");
    }

    #[test]
    fn adapter_wraps_as_effect() {
        let kernel = EqKernel::new(48000.0);
        let mut adapter = KernelAdapter::new(kernel, 48000.0);

        adapter.reset();
        let output = adapter.process(0.3);
        assert!(!output.is_nan(), "adapter.process() returned NaN");
        assert!(output.is_finite(), "adapter.process() returned Inf");
    }

    #[test]
    fn adapter_param_info_matches() {
        let kernel = EqKernel::new(48000.0);
        let adapter = KernelAdapter::new(kernel, 48000.0);

        assert_eq!(adapter.param_count(), 10);

        let d0 = adapter.param_info(0).expect("param 0 must exist");
        assert_eq!(d0.name, "Low Frequency");
        assert_eq!(d0.id, ParamId(500));
        assert_eq!(d0.string_id, "eq_low_freq");

        let d3 = adapter.param_info(3).expect("param 3 must exist");
        assert_eq!(d3.name, "Mid Frequency");
        assert_eq!(d3.id, ParamId(503));

        let d6 = adapter.param_info(6).expect("param 6 must exist");
        assert_eq!(d6.name, "High Frequency");
        assert_eq!(d6.id, ParamId(506));

        let d9 = adapter.param_info(9).expect("param 9 must exist");
        assert_eq!(d9.name, "Output");
        assert_eq!(d9.id, ParamId(509));
        assert_eq!(d9.string_id, "eq_output");

        assert!(adapter.param_info(10).is_none());
    }

    #[test]
    fn morph_produces_valid_output() {
        let mut kernel = EqKernel::new(48000.0);

        let flat = EqParams::default();
        let boosted = EqParams {
            low_freq_hz: 200.0,
            low_gain_db: 6.0,
            low_q: 2.0,
            mid_freq_hz: 2000.0,
            mid_gain_db: -4.0,
            mid_q: 1.5,
            high_freq_hz: 8000.0,
            high_gain_db: 3.0,
            high_q: 0.8,
            output_db: -3.0,
        };

        for step in 0..=10 {
            let t = step as f32 / 10.0;
            let morphed = EqParams::lerp(&flat, &boosted, t);

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

    #[test]
    fn from_knobs_maps_ranges() {
        // Mid-point knobs: frequencies should be within range, gains at 0 dB,
        // Q at midpoint, output at 0 dB.
        let params = EqParams::from_knobs(
            0.5, 0.5, 0.5, // low band
            0.5, 0.5, 0.5, // mid band
            0.5, 0.5, 0.5, // high band
            0.5, // output
        );

        assert!(
            params.low_freq_hz >= 20.0 && params.low_freq_hz <= 500.0,
            "low_freq_hz out of range: {}",
            params.low_freq_hz
        );
        assert!(
            params.mid_freq_hz >= 200.0 && params.mid_freq_hz <= 5000.0,
            "mid_freq_hz out of range: {}",
            params.mid_freq_hz
        );
        assert!(
            params.high_freq_hz >= 1000.0 && params.high_freq_hz <= 15000.0,
            "high_freq_hz out of range: {}",
            params.high_freq_hz
        );

        // Mid-point gain (0.5) → (0.5 * 24.0 - 12.0) = 0.0 dB
        assert!(
            params.low_gain_db.abs() < 0.01,
            "low_gain_db should be 0 at mid-point, got {}",
            params.low_gain_db
        );
        assert!(
            params.mid_gain_db.abs() < 0.01,
            "mid_gain_db should be 0 at mid-point, got {}",
            params.mid_gain_db
        );
        assert!(
            params.high_gain_db.abs() < 0.01,
            "high_gain_db should be 0 at mid-point, got {}",
            params.high_gain_db
        );

        // Mid-point output (0.5) → (0.5 * 40 - 20) = 0.0 dB
        assert!(
            params.output_db.abs() < 0.01,
            "output_db should be 0 at mid-point, got {}",
            params.output_db
        );

        // Mid-point Q (0.5) → 0.5 + 0.5 * 4.5 = 2.75
        assert!(
            (params.low_q - 2.75).abs() < 0.01,
            "low_q should be 2.75 at mid-point, got {}",
            params.low_q
        );

        // Min/max boundary checks
        let min_params = EqParams::from_knobs(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert!(
            (min_params.low_freq_hz - 20.0).abs() < 0.1,
            "low_freq_hz at 0 should be ~20 Hz"
        );
        assert!(
            (min_params.low_gain_db - (-12.0)).abs() < 0.01,
            "low_gain at 0 should be -12 dB"
        );
        assert!(
            (min_params.low_q - 0.5).abs() < 0.01,
            "low_q at 0 should be 0.5"
        );
        assert!(
            (min_params.output_db - (-20.0)).abs() < 0.01,
            "output at 0 should be -20 dB"
        );

        let max_params = EqParams::from_knobs(1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0);
        assert!(
            (max_params.low_freq_hz - 500.0).abs() < 1.0,
            "low_freq_hz at 1 should be ~500 Hz"
        );
        assert!(
            (max_params.low_gain_db - 12.0).abs() < 0.01,
            "low_gain at 1 should be 12 dB"
        );
        assert!(
            (max_params.low_q - 5.0).abs() < 0.01,
            "low_q at 1 should be 5.0"
        );
        assert!(
            (max_params.output_db - 20.0).abs() < 0.01,
            "output at 1 should be 20 dB"
        );
    }

    #[test]
    fn flat_eq_passes_dc() {
        // With all gains at 0 dB and 0 dB output, a DC input should pass
        // through with unity gain after the filter settles.
        let mut kernel = EqKernel::new(48000.0);
        let params = EqParams::default(); // all gains = 0 dB

        // Warm up (let biquad state settle)
        for _ in 0..2000 {
            kernel.process_stereo(1.0, 1.0, &params);
        }

        // After settling, DC (1.0) should pass through essentially unchanged.
        let (out_l, out_r) = kernel.process_stereo(1.0, 1.0, &params);
        assert!(
            (out_l - 1.0).abs() < 0.05,
            "Flat EQ should pass DC with gain ≈1.0, got left={out_l}"
        );
        assert!(
            (out_r - 1.0).abs() < 0.05,
            "Flat EQ should pass DC with gain ≈1.0, got right={out_r}"
        );
    }

    #[test]
    fn gain_boost_increases_energy() {
        // Boosting the mid band should increase RMS energy compared to flat EQ.
        let sample_rate = 48000.0;
        let test_freq_hz = 1000.0; // Right at the mid band default center

        let flat = EqParams::default();
        let boosted = EqParams {
            mid_gain_db: 12.0, // +12 dB boost at 1 kHz
            ..EqParams::default()
        };

        let mut kernel_flat = EqKernel::new(sample_rate);
        let mut kernel_boosted = EqKernel::new(sample_rate);

        // Warm up
        for i in 0..256 {
            let t = i as f32 / sample_rate;
            let s = libm::sinf(2.0 * core::f32::consts::PI * test_freq_hz * t);
            kernel_flat.process_stereo(s, s, &flat);
            kernel_boosted.process_stereo(s, s, &boosted);
        }

        // Measure RMS energy over 512 samples
        let mut energy_flat = 0.0f32;
        let mut energy_boosted = 0.0f32;
        for i in 256..768 {
            let t = i as f32 / sample_rate;
            let s = libm::sinf(2.0 * core::f32::consts::PI * test_freq_hz * t);
            let (l_flat, _) = kernel_flat.process_stereo(s, s, &flat);
            let (l_boosted, _) = kernel_boosted.process_stereo(s, s, &boosted);
            energy_flat += l_flat * l_flat;
            energy_boosted += l_boosted * l_boosted;
        }

        assert!(
            energy_boosted > energy_flat,
            "Boosted EQ should produce more energy at {test_freq_hz} Hz: \
             flat={energy_flat:.4}, boosted={energy_boosted:.4}"
        );
    }
}
