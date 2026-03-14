//! Tape saturation kernel — ADAA hysteresis, wow/flutter, head bump, and HF rolloff.
//!
//! `TapeKernel` owns DSP state (ADAA processors, LFOs, delay lines, hysteresis
//! registers, biquad filters, envelope followers, one-pole filters). Parameters
//! are received via `&TapeParams` each sample. Deployed via
//! [`KernelAdapter`](sonido_core::KernelAdapter) for desktop/plugin, or called
//! directly on embedded targets.
//!
//! # Signal Flow
//!
//! ```text
//! Input -> Wow/Flutter Delay -> Hysteresis Feedback -> Drive + Bias -> ADAA Waveshaper
//!       -> Blend (clean/saturated) -> Head Bump EQ -> Level-Dependent HF Rolloff
//!       -> Static HF Rolloff -> Soft Limit -> Output Level
//! ```
//!
//! ## Tape Waveshaping (Asymmetric Exponential)
//!
//! Transfer function:
//!
//! ```text
//! f(x) = 1 - exp(-2x)    for x >= 0
//! f(x) = -1 + exp(1.8x)  for x < 0
//! ```
//!
//! The asymmetry between positive and negative branches produces even harmonics,
//! characteristic of magnetic tape saturation.
//!
//! ## Wow / Flutter
//!
//! Transport speed instability is modelled with two independent LFOs:
//! - **Wow**: sine wave at ~1.5 Hz, modulates a 5-10 ms delay by up to 2 ms.
//! - **Flutter**: triangle wave at ~10 Hz, modulates the delay by up to 0.2 ms.
//!
//! ## Hysteresis Feedback
//!
//! A fraction of the previous output is added to the current input before
//! waveshaping. This creates frequency-dependent saturation behaviour analogous
//! to magnetic domain lag in real tape formulations.
//!
//! ## Head Bump
//!
//! A peaking EQ centred between 40-200 Hz (default 80 Hz) models the low-end
//! resonance produced by the record head gap, adding warmth at low frequencies.
//!
//! ## Level-Dependent HF Rolloff (Self-Erasure)
//!
//! At high drive and signal levels, the high-frequency cutoff is pushed lower.
//! This models magnetic self-erasure: loud, strongly driven signals saturate
//! the tape oxide and lose high-frequency content progressively.
//!
//! ## References
//!
//! - Jatin Chowdhury, "Real-time Physical Modelling for Analog Tape Machines",
//!   DAFx 2019.
//! - Valimaki & Smith, tape transport flutter modelling.
//! - Magnetic recording self-erasure: HF loss proportional to record level.
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter)
//! let adapter = KernelAdapter::new(TapeKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct)
//! let mut kernel = TapeKernel::new(48000.0);
//! let params = TapeParams::from_knobs(0.25, 0.3, 0.8, 0.0, 0.3, 0.2, 0.15, 0.3, 0.5, 0.3);
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::math::soft_limit;
use sonido_core::{
    Adaa1, Biquad, EnvelopeFollower, InterpolatedDelay, Lfo, LfoWaveform, OnePole, ParamDescriptor,
    ParamId, ParamScale, ParamUnit, db_to_linear, lowpass_coefficients, peaking_eq_coefficients,
    tape_sat_ad,
};

/// ADAA processor type alias for function-pointer waveshapers.
///
/// Using function pointers (rather than closures or generics) ensures the
/// kernel type remains `Send` and avoids captured state.
type FnAdaa = Adaa1<fn(f32) -> f32, fn(f32) -> f32>;

/// Base delay for wow/flutter modulation in seconds (5 ms).
///
/// This represents the quiescent (zero-LFO) tape transport delay. All
/// wow and flutter modulation is applied relative to this centre point.
const WOW_BASE_DELAY_S: f32 = 0.005;

/// Maximum delay for the wow/flutter buffer in seconds (10 ms).
///
/// Sets the upper bound for buffer allocation. At maximum wow and flutter
/// depth the combined excursion remains within this limit.
const WOW_MAX_DELAY_S: f32 = 0.010;

/// Maximum HF cutoff for level-dependent rolloff in Hz.
///
/// At zero drive or zero input level the HF filter sits at this frequency,
/// having minimal effect on the signal.
const HF_MAX: f32 = 15000.0;

/// Minimum HF cutoff at full self-erasure in Hz.
///
/// At maximum drive combined with loud input the HF filter bottoms out
/// at this frequency, aggressively removing HF content.
const HF_MIN: f32 = 3000.0;

/// Tape saturation waveshaping function (asymmetric exponential saturation).
///
/// ## Transfer Function
///
/// ```text
/// f(x) = 1 - exp(-2x)    for x >= 0
/// f(x) = -1 + exp(1.8x)  for x < 0
/// ```
///
/// The asymmetry between the positive exponential decay (compression factor 2)
/// and the negative exponential (factor 1.8) creates even harmonics, mimicking
/// the soft compression characteristic of magnetic tape.
///
/// Companion antiderivative: [`sonido_core::tape_sat_ad`], used by [`Adaa1`]
/// to compute the band-limited version free of aliasing artefacts.
#[inline]
fn tape_waveshape(x: f32) -> f32 {
    if x >= 0.0 {
        1.0 - libm::expf(-2.0 * x)
    } else {
        -1.0 + libm::expf(1.8 * x)
    }
}

// ============================================================================
//  Parameters
// ============================================================================

/// Parameter values for [`TapeKernel`].
///
/// All values in **user-facing units** (dB, %, Hz, normalized 0-1).
/// The kernel converts internally as needed.
///
/// | Index | Field | Unit | Range | Default | ParamId | string_id |
/// |-------|-------|------|-------|---------|---------|-----------|
/// | 0 | `drive_db` | dB | 0-24 | 6.0 | 1400 | tape_drive |
/// | 1 | `saturation_pct` | % | 0-100 | 30.0 | 1401 | tape_saturation |
/// | 2 | `hf_rolloff_hz` | Hz | 1000-20000 | 12000.0 | 1403 | tape_hf_rolloff |
/// | 3 | `bias` | none | -0.2-0.2 | 0.0 | 1404 | tape_bias |
/// | 4 | `wow` | none | 0-1 | 0.3 | 1405 | tape_wow |
/// | 5 | `flutter` | none | 0-1 | 0.2 | 1406 | tape_flutter |
/// | 6 | `hysteresis` | none | 0-0.5 | 0.15 | 1407 | tape_hysteresis |
/// | 7 | `head_bump` | none | 0-1 | 0.3 | 1408 | tape_head_bump |
/// | 8 | `bump_freq_hz` | Hz | 40-200 | 80.0 | 1409 | tape_bump_freq |
/// | 9 | `output_db` | dB | -12-12 | -6.0 | 1402 | tape_output |
#[derive(Debug, Clone, Copy)]
pub struct TapeParams {
    /// Input drive in decibels.
    ///
    /// Range: 0.0 to 24.0 dB. Higher values push the signal harder into the
    /// waveshaper, producing more harmonic distortion. Default 6.0 dB.
    pub drive_db: f32,

    /// Saturation blend in percent.
    ///
    /// Range: 0.0 to 100.0 %. At 0 % the output is hard-clipped linear.
    /// At 100 % the ADAA waveshaper output is used exclusively. Default 30.0 %.
    pub saturation_pct: f32,

    /// Static high-frequency rolloff cutoff in Hz.
    ///
    /// Range: 1000.0 to 20000.0 Hz. Applied via a one-pole lowpass after
    /// waveshaping. Models tape formulation bandwidth limits. Default 12000.0 Hz.
    pub hf_rolloff_hz: f32,

    /// Tape bias offset added to the driven signal before waveshaping.
    ///
    /// Range: -0.2 to 0.2. Non-zero bias shifts the operating point of the
    /// waveshaper, changing the harmonic content. Default 0.0.
    pub bias: f32,

    /// Wow depth (low-frequency tape transport instability).
    ///
    /// Range: 0.0 to 1.0. At 1.0, a 1.5 Hz sine LFO modulates the delay by
    /// up to 2 ms. Default 0.3.
    pub wow: f32,

    /// Flutter depth (high-frequency tape transport instability).
    ///
    /// Range: 0.0 to 1.0. At 1.0, a 10 Hz triangle LFO modulates the delay
    /// by up to 0.2 ms. Default 0.2.
    pub flutter: f32,

    /// Hysteresis feedback amount.
    ///
    /// Range: 0.0 to 0.5. A fraction of the previous output fed back before
    /// waveshaping, simulating magnetic domain lag. Default 0.15.
    pub hysteresis: f32,

    /// Head bump level (low-frequency resonance from record head gap).
    ///
    /// Range: 0.0 to 1.0. Scales a peaking EQ at `bump_freq_hz` from 0 to
    /// +6 dB. Default 0.3.
    pub head_bump: f32,

    /// Head bump centre frequency in Hz.
    ///
    /// Range: 40.0 to 200.0 Hz. Default 80.0 Hz.
    pub bump_freq_hz: f32,

    /// Output level in decibels.
    ///
    /// Range: -12.0 to 12.0 dB. Applied after all processing. Default -6.0 dB.
    pub output_db: f32,
}

impl Default for TapeParams {
    fn default() -> Self {
        Self {
            drive_db: 6.0,
            saturation_pct: 30.0,
            hf_rolloff_hz: 12000.0,
            bias: 0.0,
            wow: 0.3,
            flutter: 0.2,
            hysteresis: 0.15,
            head_bump: 0.3,
            bump_freq_hz: 80.0,
            output_db: -6.0,
        }
    }
}

impl TapeParams {
    /// Creates parameters from normalized 0–1 knob readings.
    ///
    /// Curves (logarithmic for frequency/time, linear for percentage) are
    /// derived from [`ParamDescriptor`] — same mapping as GUI and plugin hosts.
    ///
    /// # Parameters
    ///
    /// - `drive`: Drive knob (0–1) → 0–24 dB
    /// - `saturation`: Saturation knob (0–1) → 0–100 %
    /// - `hf_rolloff`: HF rolloff knob (0–1) → 1000–20000 Hz (logarithmic)
    /// - `bias`: Bias knob (0–1) → -0.2 to 0.2 (centre = 0.0)
    /// - `wow`: Wow depth knob (0–1) → 0–1
    /// - `flutter`: Flutter depth knob (0–1) → 0–1
    /// - `hysteresis`: Hysteresis knob (0–1) → 0–0.5
    /// - `head_bump`: Head bump level knob (0–1) → 0–1
    /// - `bump_freq`: Bump freq knob (0–1) → 40–200 Hz (logarithmic)
    /// - `output`: Output knob (0–1) → -12 to +12 dB
    #[allow(clippy::too_many_arguments)]
    pub fn from_knobs(
        drive: f32,
        saturation: f32,
        hf_rolloff: f32,
        bias: f32,
        wow: f32,
        flutter: f32,
        hysteresis: f32,
        head_bump: f32,
        bump_freq: f32,
        output: f32,
    ) -> Self {
        Self::from_normalized(&[
            drive, saturation, hf_rolloff, bias, wow, flutter, hysteresis, head_bump, bump_freq,
            output,
        ])
    }
}

impl KernelParams for TapeParams {
    const COUNT: usize = 10;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(
                ParamDescriptor {
                    name: "Drive",
                    short_name: "Drive",
                    unit: ParamUnit::Decibels,
                    min: 0.0,
                    max: 24.0,
                    default: 6.0,
                    step: 0.5,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(1400), "tape_drive"),
            ),
            1 => Some(
                ParamDescriptor {
                    name: "Saturation",
                    short_name: "Sat",
                    unit: ParamUnit::Percent,
                    min: 0.0,
                    max: 100.0,
                    default: 30.0,
                    step: 1.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(1401), "tape_saturation"),
            ),
            2 => Some(
                ParamDescriptor {
                    name: "HF Rolloff",
                    short_name: "HFRoll",
                    unit: ParamUnit::Hertz,
                    min: 1000.0,
                    max: 20000.0,
                    default: 12000.0,
                    step: 100.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(1403), "tape_hf_rolloff")
                .with_scale(ParamScale::Logarithmic),
            ),
            3 => Some(
                ParamDescriptor {
                    name: "Bias",
                    short_name: "Bias",
                    unit: ParamUnit::None,
                    min: -0.2,
                    max: 0.2,
                    default: 0.0,
                    step: 0.01,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(1404), "tape_bias"),
            ),
            4 => Some(
                ParamDescriptor {
                    name: "Wow",
                    short_name: "Wow",
                    unit: ParamUnit::None,
                    min: 0.0,
                    max: 1.0,
                    default: 0.3,
                    step: 0.01,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(1405), "tape_wow"),
            ),
            5 => Some(
                ParamDescriptor {
                    name: "Flutter",
                    short_name: "Flut",
                    unit: ParamUnit::None,
                    min: 0.0,
                    max: 1.0,
                    default: 0.2,
                    step: 0.01,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(1406), "tape_flutter"),
            ),
            6 => Some(
                ParamDescriptor {
                    name: "Hysteresis",
                    short_name: "Hyst",
                    unit: ParamUnit::None,
                    min: 0.0,
                    max: 0.5,
                    default: 0.15,
                    step: 0.01,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(1407), "tape_hysteresis"),
            ),
            7 => Some(
                ParamDescriptor {
                    name: "Head Bump",
                    short_name: "Bump",
                    unit: ParamUnit::None,
                    min: 0.0,
                    max: 1.0,
                    default: 0.3,
                    step: 0.01,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(1408), "tape_head_bump"),
            ),
            8 => Some(
                ParamDescriptor {
                    name: "Bump Freq",
                    short_name: "BmpHz",
                    unit: ParamUnit::Hertz,
                    min: 40.0,
                    max: 200.0,
                    default: 80.0,
                    step: 1.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(1409), "tape_bump_freq")
                .with_scale(ParamScale::Logarithmic),
            ),
            9 => Some(
                ParamDescriptor {
                    name: "Output",
                    short_name: "Output",
                    unit: ParamUnit::Decibels,
                    min: -12.0,
                    max: 12.0,
                    default: -6.0,
                    step: 0.5,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(1402), "tape_output"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Fast,     // drive
            1 => SmoothingStyle::Standard, // saturation
            2 => SmoothingStyle::Slow,     // hf_rolloff (filter coefficient)
            3 => SmoothingStyle::Standard, // bias
            4 => SmoothingStyle::Standard, // wow
            5 => SmoothingStyle::Standard, // flutter
            6 => SmoothingStyle::Standard, // hysteresis
            7 => SmoothingStyle::Standard, // head_bump
            8 => SmoothingStyle::Slow,     // bump_freq (filter coefficient)
            9 => SmoothingStyle::Standard, // output
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.drive_db,
            1 => self.saturation_pct,
            2 => self.hf_rolloff_hz,
            3 => self.bias,
            4 => self.wow,
            5 => self.flutter,
            6 => self.hysteresis,
            7 => self.head_bump,
            8 => self.bump_freq_hz,
            9 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.drive_db = value,
            1 => self.saturation_pct = value,
            2 => self.hf_rolloff_hz = value,
            3 => self.bias = value,
            4 => self.wow = value,
            5 => self.flutter = value,
            6 => self.hysteresis = value,
            7 => self.head_bump = value,
            8 => self.bump_freq_hz = value,
            9 => self.output_db = value,
            _ => {}
        }
    }
}

// ============================================================================
//  Kernel
// ============================================================================

/// Pure DSP tape saturation kernel.
///
/// Contains ONLY the mutable state required for audio processing:
/// - ADAA waveshaper instances (L/R)
/// - Wow/flutter LFOs and modulated delay lines (L/R)
/// - Hysteresis feedback registers (L/R)
/// - Head bump peaking EQ biquads (L/R)
/// - Level-dependent HF rolloff biquads (L/R) and envelope followers (L/R)
/// - Static HF rolloff one-pole filters (L/R)
/// - Cached coefficient guard values for lazy recomputation
///
/// No `SmoothedParam`, no `AtomicU32`, no platform awareness.
pub struct TapeKernel {
    /// Audio sample rate in Hz.
    sample_rate: f32,

    /// Anti-derivative anti-aliased waveshaper, left/mono channel.
    adaa_l: FnAdaa,
    /// Anti-derivative anti-aliased waveshaper, right channel.
    adaa_r: FnAdaa,

    /// Sine LFO for wow (low-frequency transport instability, ~1.5 Hz).
    wow_lfo: Lfo,
    /// Triangle LFO for flutter (high-frequency transport instability, ~10 Hz).
    flutter_lfo: Lfo,
    /// Modulated delay line for wow/flutter, left/mono channel.
    wow_delay_l: InterpolatedDelay,
    /// Modulated delay line for wow/flutter, right channel.
    wow_delay_r: InterpolatedDelay,

    /// Previous output sample, left/mono channel (hysteresis feedback).
    prev_output_l: f32,
    /// Previous output sample, right channel (hysteresis feedback).
    prev_output_r: f32,

    /// Peaking EQ for head bump, left/mono channel.
    bump_filter_l: Biquad,
    /// Peaking EQ for head bump, right channel.
    bump_filter_r: Biquad,

    /// Level-dependent lowpass filter, left/mono channel.
    hf_filter_l: Biquad,
    /// Level-dependent lowpass filter, right channel.
    hf_filter_r: Biquad,
    /// Envelope follower for input level detection, left/mono channel.
    env_l: EnvelopeFollower,
    /// Envelope follower for input level detection, right channel.
    env_r: EnvelopeFollower,

    /// Static one-pole lowpass for tape bandwidth limit, left/mono channel.
    hf_static_l: OnePole,
    /// Static one-pole lowpass for tape bandwidth limit, right channel.
    hf_static_r: OnePole,

    /// Last head_bump value used to compute bump EQ coefficients. NaN forces init.
    last_head_bump: f32,
    /// Last bump_freq_hz value used to compute bump EQ coefficients. NaN forces init.
    last_bump_freq: f32,
    /// Last hf_rolloff_hz value used to update the static HF filter. NaN forces init.
    last_hf_freq: f32,
    /// Cached cutoff for level-dependent HF filter (left). NaN = needs init.
    last_hf_cutoff_l: f32,
    /// Cached cutoff for level-dependent HF filter (right). NaN = needs init.
    last_hf_cutoff_r: f32,
}

impl TapeKernel {
    /// Create a new tape saturation kernel at the given sample rate.
    ///
    /// Allocates all delay buffers at maximum wow/flutter capacity and
    /// initialises all filters to their default coefficient values.
    ///
    /// # Parameters
    ///
    /// - `sample_rate`: Audio sample rate in Hz (e.g. 44100.0, 48000.0).
    pub fn new(sample_rate: f32) -> Self {
        let max_delay_samples = (WOW_MAX_DELAY_S * sample_rate) as usize + 1;

        let mut wow_lfo = Lfo::new(sample_rate, 1.5);
        wow_lfo.set_waveform(LfoWaveform::Sine);

        let mut flutter_lfo = Lfo::new(sample_rate, 10.0);
        flutter_lfo.set_waveform(LfoWaveform::Triangle);

        // Head bump: peaking EQ at 80 Hz, Q=1.5, 0 dB initially
        let mut bump_filter_l = Biquad::new();
        let mut bump_filter_r = Biquad::new();
        let bump_coeffs = peaking_eq_coefficients(80.0, 1.5, 0.0, sample_rate);
        bump_filter_l.set_coefficients(
            bump_coeffs.0,
            bump_coeffs.1,
            bump_coeffs.2,
            bump_coeffs.3,
            bump_coeffs.4,
            bump_coeffs.5,
        );
        bump_filter_r.set_coefficients(
            bump_coeffs.0,
            bump_coeffs.1,
            bump_coeffs.2,
            bump_coeffs.3,
            bump_coeffs.4,
            bump_coeffs.5,
        );

        // Level-dependent HF lowpass: starts at HF_MAX (minimal effect)
        let mut hf_filter_l = Biquad::new();
        let mut hf_filter_r = Biquad::new();
        let safe_hf_max = HF_MAX.min(sample_rate * 0.45);
        let hf_coeffs = lowpass_coefficients(safe_hf_max, 0.707, sample_rate);
        hf_filter_l.set_coefficients(
            hf_coeffs.0,
            hf_coeffs.1,
            hf_coeffs.2,
            hf_coeffs.3,
            hf_coeffs.4,
            hf_coeffs.5,
        );
        hf_filter_r.set_coefficients(
            hf_coeffs.0,
            hf_coeffs.1,
            hf_coeffs.2,
            hf_coeffs.3,
            hf_coeffs.4,
            hf_coeffs.5,
        );

        Self {
            sample_rate,
            adaa_l: Adaa1::new(
                tape_waveshape as fn(f32) -> f32,
                tape_sat_ad as fn(f32) -> f32,
            ),
            adaa_r: Adaa1::new(
                tape_waveshape as fn(f32) -> f32,
                tape_sat_ad as fn(f32) -> f32,
            ),
            wow_lfo,
            flutter_lfo,
            wow_delay_l: InterpolatedDelay::new(max_delay_samples),
            wow_delay_r: InterpolatedDelay::new(max_delay_samples),
            prev_output_l: 0.0,
            prev_output_r: 0.0,
            bump_filter_l,
            bump_filter_r,
            hf_filter_l,
            hf_filter_r,
            env_l: EnvelopeFollower::with_times(sample_rate, 1.0, 50.0),
            env_r: EnvelopeFollower::with_times(sample_rate, 1.0, 50.0),
            hf_static_l: OnePole::new(sample_rate, 12000.0),
            hf_static_r: OnePole::new(sample_rate, 12000.0),
            last_head_bump: f32::NAN,
            last_bump_freq: f32::NAN,
            last_hf_freq: f32::NAN,
            last_hf_cutoff_l: f32::NAN,
            last_hf_cutoff_r: f32::NAN,
        }
    }

    /// Compute wow/flutter combined delay time in samples and advance both LFOs.
    ///
    /// Returns the clamped delay in samples (1.0 to `WOW_MAX_DELAY_S * sample_rate`).
    /// Both LFOs are advanced as a side effect, keeping their phase consistent
    /// whether the delay path is active or bypassed.
    ///
    /// ## Modulation formula
    ///
    /// ```text
    /// wow_samples    = wow_lfo    * (0.5 + wow_depth    * 1.5) ms * sr * wow_depth
    /// flutter_samples= flutter_lfo* (0.05 + flutter_depth*0.15)ms * sr * flutter_depth
    /// total = base_samples + wow_samples + flutter_samples
    /// ```
    #[inline]
    fn compute_wow_flutter_delay(&mut self, wow_depth: f32, flutter_depth: f32) -> f32 {
        let base_samples = WOW_BASE_DELAY_S * self.sample_rate;

        let wow_mod = self.wow_lfo.advance();
        let wow_depth_ms = 0.5 + wow_depth * 1.5;
        let wow_samples = wow_mod * wow_depth_ms * 0.001 * self.sample_rate * wow_depth;

        let flutter_mod = self.flutter_lfo.advance();
        let flutter_depth_ms = 0.05 + flutter_depth * 0.15;
        let flutter_samples =
            flutter_mod * flutter_depth_ms * 0.001 * self.sample_rate * flutter_depth;

        let total = base_samples + wow_samples + flutter_samples;
        let max = WOW_MAX_DELAY_S * self.sample_rate;
        total.clamp(1.0, max)
    }

    /// Recompute head bump peaking EQ coefficients.
    ///
    /// `head_bump` is mapped to gain_db = head_bump * 6.0 dB (0 to +6 dB).
    /// Q is fixed at 1.5 for a moderately peaked resonance.
    #[inline]
    fn update_bump_coefficients(&mut self, head_bump: f32, bump_freq_hz: f32) {
        let gain_db = head_bump * 6.0;
        let coeffs = peaking_eq_coefficients(bump_freq_hz, 1.5, gain_db, self.sample_rate);
        self.bump_filter_l
            .set_coefficients(coeffs.0, coeffs.1, coeffs.2, coeffs.3, coeffs.4, coeffs.5);
        self.bump_filter_r
            .set_coefficients(coeffs.0, coeffs.1, coeffs.2, coeffs.3, coeffs.4, coeffs.5);
        self.last_head_bump = head_bump;
        self.last_bump_freq = bump_freq_hz;
    }

    /// Update the static HF one-pole filter cutoff frequency.
    #[inline]
    fn update_static_hf(&mut self, hf_rolloff_hz: f32) {
        self.hf_static_l.set_frequency(hf_rolloff_hz);
        self.hf_static_r.set_frequency(hf_rolloff_hz);
        self.last_hf_freq = hf_rolloff_hz;
    }

    /// Apply level-dependent HF lowpass to the left channel.
    ///
    /// The cutoff frequency is computed as:
    ///
    /// ```text
    /// cutoff = HF_MAX - (HF_MAX - HF_MIN) * level * drive_norm
    /// ```
    ///
    /// where `drive_norm` = (drive_linear - 1.0) / 14.85 clamped to [0, 1].
    /// Coefficients are cached and only recomputed when the cutoff changes by
    /// more than 1 Hz, avoiding expensive trig per sample.
    #[inline]
    fn apply_level_hf_l(&mut self, signal: f32, input: f32, drive: f32) -> f32 {
        let nyquist = self.sample_rate * 0.45;
        let hf_max = HF_MAX.min(nyquist);
        let hf_min = HF_MIN.min(nyquist * 0.5);
        let drive_norm = ((drive - 1.0) / 14.85).clamp(0.0, 1.0);
        let level = self.env_l.process(libm::fabsf(input));
        let cutoff = (hf_max - (hf_max - hf_min) * level * drive_norm).clamp(hf_min, hf_max);
        if (cutoff - self.last_hf_cutoff_l).abs() > 1.0 || self.last_hf_cutoff_l.is_nan() {
            let coeffs = lowpass_coefficients(cutoff, 0.707, self.sample_rate);
            self.hf_filter_l
                .set_coefficients(coeffs.0, coeffs.1, coeffs.2, coeffs.3, coeffs.4, coeffs.5);
            self.last_hf_cutoff_l = cutoff;
        }
        self.hf_filter_l.process(signal)
    }

    /// Apply level-dependent HF lowpass to the right channel.
    ///
    /// Identical algorithm to [`apply_level_hf_l`] but uses the right-channel
    /// envelope follower and biquad. Coefficients are cached and only recomputed
    /// when the cutoff changes by more than 1 Hz.
    #[inline]
    fn apply_level_hf_r(&mut self, signal: f32, input: f32, drive: f32) -> f32 {
        let nyquist = self.sample_rate * 0.45;
        let hf_max = HF_MAX.min(nyquist);
        let hf_min = HF_MIN.min(nyquist * 0.5);
        let drive_norm = ((drive - 1.0) / 14.85).clamp(0.0, 1.0);
        let level = self.env_r.process(libm::fabsf(input));
        let cutoff = (hf_max - (hf_max - hf_min) * level * drive_norm).clamp(hf_min, hf_max);
        if (cutoff - self.last_hf_cutoff_r).abs() > 1.0 || self.last_hf_cutoff_r.is_nan() {
            let coeffs = lowpass_coefficients(cutoff, 0.707, self.sample_rate);
            self.hf_filter_r
                .set_coefficients(coeffs.0, coeffs.1, coeffs.2, coeffs.3, coeffs.4, coeffs.5);
            self.last_hf_cutoff_r = cutoff;
        }
        self.hf_filter_r.process(signal)
    }
}

impl DspKernel for TapeKernel {
    type Params = TapeParams;

    /// Process one stereo sample pair through the full tape machine model.
    ///
    /// ## Signal path (per sample)
    ///
    /// 1. Lazy coefficient updates for head bump EQ and static HF filter.
    /// 2. Wow/flutter: compute combined LFO delay, read both delay lines at
    ///    the same time (correlated transport). Bypass when both depths are zero.
    /// 3. Hysteresis: `x_eff = wow_out + hysteresis * prev_output`.
    /// 4. Drive + bias -> ADAA waveshaping -> blend clean/saturated.
    /// 5. Head bump EQ.
    /// 6. Level-dependent HF rolloff (per-channel envelope follower).
    /// 7. Static HF rolloff (one-pole).
    /// 8. Soft limit -> output gain.
    #[inline]
    fn process_stereo(&mut self, left: f32, right: f32, params: &TapeParams) -> (f32, f32) {
        let drive = db_to_linear(params.drive_db);
        let saturation = params.saturation_pct / 100.0;
        let output = db_to_linear(params.output_db);

        // Lazy coefficient updates — NaN sentinel triggers forced init on first call
        if self.last_head_bump.is_nan()
            || self.last_bump_freq.is_nan()
            || (params.head_bump - self.last_head_bump).abs() > 0.001
            || (params.bump_freq_hz - self.last_bump_freq).abs() > 0.5
        {
            self.update_bump_coefficients(params.head_bump, params.bump_freq_hz);
        }
        if self.last_hf_freq.is_nan() || (params.hf_rolloff_hz - self.last_hf_freq).abs() > 10.0 {
            self.update_static_hf(params.hf_rolloff_hz);
        }

        // Wow / flutter delay
        let (wow_l, wow_r) = if params.wow > 0.0 || params.flutter > 0.0 {
            let delay_samples = self.compute_wow_flutter_delay(params.wow, params.flutter);
            (
                self.wow_delay_l.read_write(left, delay_samples),
                self.wow_delay_r.read_write(right, delay_samples),
            )
        } else {
            self.wow_lfo.advance();
            self.flutter_lfo.advance();
            (left, right)
        };

        // Hysteresis feedback
        let x_eff_l = wow_l + params.hysteresis * self.prev_output_l;
        let x_eff_r = wow_r + params.hysteresis * self.prev_output_r;

        // Drive + bias -> ADAA waveshaping -> blend
        let driven_l = x_eff_l * drive + params.bias;
        let driven_r = x_eff_r * drive + params.bias;
        let sat_l = self.adaa_l.process(driven_l);
        let sat_r = self.adaa_r.process(driven_r);
        let clean_l = driven_l.clamp(-1.0, 1.0);
        let clean_r = driven_r.clamp(-1.0, 1.0);
        let blended_l = clean_l * (1.0 - saturation) + sat_l * saturation;
        let blended_r = clean_r * (1.0 - saturation) + sat_r * saturation;

        // Store for next sample's hysteresis feedback
        self.prev_output_l = blended_l;
        self.prev_output_r = blended_r;

        // Head bump EQ
        let bumped_l = self.bump_filter_l.process(blended_l);
        let bumped_r = self.bump_filter_r.process(blended_r);

        // Level-dependent HF rolloff
        let hf_l = self.apply_level_hf_l(bumped_l, left, drive);
        let hf_r = self.apply_level_hf_r(bumped_r, right, drive);

        // Static HF rolloff
        let filt_l = self.hf_static_l.process(hf_l);
        let filt_r = self.hf_static_r.process(hf_r);

        // Soft limit -> output gain
        (
            soft_limit(filt_l, 1.0) * output,
            soft_limit(filt_r, 1.0) * output,
        )
    }

    fn reset(&mut self) {
        self.adaa_l.reset();
        self.adaa_r.reset();

        self.wow_lfo.reset();
        self.flutter_lfo.reset();
        self.wow_delay_l.clear();
        self.wow_delay_r.clear();

        // Zero hysteresis feedback registers. If bias + hysteresis create a
        // non-zero DC fixed point, a brief DC convergence loop mirrors the
        // behaviour of the classic TapeSaturation::reset().
        self.prev_output_l = 0.0;
        self.prev_output_r = 0.0;

        self.bump_filter_l.clear();
        self.bump_filter_r.clear();

        self.hf_filter_l.clear();
        self.hf_filter_r.clear();
        self.env_l.reset();
        self.env_r.reset();

        self.hf_static_l.reset();
        self.hf_static_r.reset();

        self.last_hf_cutoff_l = f32::NAN;
        self.last_hf_cutoff_r = f32::NAN;
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;

        let max_delay_samples = (WOW_MAX_DELAY_S * sample_rate) as usize + 1;
        self.wow_delay_l = InterpolatedDelay::new(max_delay_samples);
        self.wow_delay_r = InterpolatedDelay::new(max_delay_samples);

        self.wow_lfo.set_sample_rate(sample_rate);
        self.flutter_lfo.set_sample_rate(sample_rate);

        self.env_l = EnvelopeFollower::with_times(sample_rate, 1.0, 50.0);
        self.env_r = EnvelopeFollower::with_times(sample_rate, 1.0, 50.0);

        self.hf_static_l.set_sample_rate(sample_rate);
        self.hf_static_r.set_sample_rate(sample_rate);

        // Force coefficient recomputation at the new sample rate
        self.last_head_bump = f32::NAN;
        self.last_bump_freq = f32::NAN;
        self.last_hf_freq = f32::NAN;
    }

    fn is_true_stereo(&self) -> bool {
        // L/R share the same wow/flutter delay time (correlated transport)
        // but are otherwise processed independently. Dual-mono, not true stereo.
        false
    }
}

// ============================================================================
//  Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_core::kernel::KernelAdapter;
    use sonido_core::{Effect, ParameterInfo};

    /// Silence in produces silence out at default settings.
    #[test]
    fn silence_in_silence_out() {
        let mut kernel = TapeKernel::new(48000.0);
        let params = TapeParams::default();
        let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
        assert!(l.abs() < 1e-6, "Expected silence on left, got {l}");
        assert!(r.abs() < 1e-6, "Expected silence on right, got {r}");
    }

    /// No NaN or Infinity for sustained normal input.
    #[test]
    fn no_nan_or_inf() {
        let mut kernel = TapeKernel::new(48000.0);
        let params = TapeParams::default();
        for _ in 0..1000 {
            let (l, r) = kernel.process_stereo(0.5, -0.3, &params);
            assert!(l.is_finite(), "Left output not finite: {l}");
            assert!(r.is_finite(), "Right output not finite: {r}");
        }
    }

    /// COUNT is 10 and every index 0..COUNT has a descriptor.
    #[test]
    fn params_descriptor_count() {
        assert_eq!(TapeParams::COUNT, 10, "Expected 10 parameters");
        for i in 0..TapeParams::COUNT {
            assert!(
                TapeParams::descriptor(i).is_some(),
                "Missing descriptor at index {i}",
            );
        }
        assert!(
            TapeParams::descriptor(TapeParams::COUNT).is_none(),
            "Descriptor beyond COUNT should be None",
        );
    }

    /// KernelAdapter exposes the kernel as a standard Effect.
    #[test]
    fn adapter_wraps_as_effect() {
        let kernel = TapeKernel::new(48000.0);
        let mut adapter = KernelAdapter::new(kernel, 48000.0);
        adapter.reset();
        let output = adapter.process(0.3);
        assert!(output.is_finite(), "Adapter output not finite: {output}");
    }

    /// The adapter exposes 10 params with the correct ParamIds.
    #[test]
    fn adapter_param_info_matches() {
        let kernel = TapeKernel::new(48000.0);
        let adapter = KernelAdapter::new(kernel, 48000.0);

        assert_eq!(adapter.param_count(), TapeParams::COUNT);

        assert_eq!(adapter.param_info(0).unwrap().id, ParamId(1400), "drive");
        assert_eq!(
            adapter.param_info(1).unwrap().id,
            ParamId(1401),
            "saturation"
        );
        assert_eq!(
            adapter.param_info(2).unwrap().id,
            ParamId(1403),
            "hf_rolloff"
        );
        assert_eq!(adapter.param_info(3).unwrap().id, ParamId(1404), "bias");
        assert_eq!(adapter.param_info(4).unwrap().id, ParamId(1405), "wow");
        assert_eq!(adapter.param_info(5).unwrap().id, ParamId(1406), "flutter");
        assert_eq!(
            adapter.param_info(6).unwrap().id,
            ParamId(1407),
            "hysteresis"
        );
        assert_eq!(
            adapter.param_info(7).unwrap().id,
            ParamId(1408),
            "head_bump"
        );
        assert_eq!(
            adapter.param_info(8).unwrap().id,
            ParamId(1409),
            "bump_freq"
        );
        assert_eq!(adapter.param_info(9).unwrap().id, ParamId(1402), "output");

        let drive_desc = adapter.param_info(0).unwrap();
        assert!((drive_desc.min - 0.0).abs() < 0.01, "drive min");
        assert!((drive_desc.max - 24.0).abs() < 0.01, "drive max");

        assert_eq!(
            adapter.param_info(2).unwrap().scale,
            ParamScale::Logarithmic,
            "HF rolloff must be Logarithmic",
        );
        assert_eq!(
            adapter.param_info(8).unwrap().scale,
            ParamScale::Logarithmic,
            "Bump freq must be Logarithmic",
        );
    }

    /// Interpolating between two param snapshots produces finite output at each step.
    #[test]
    fn morph_produces_valid_output() {
        let mut kernel = TapeKernel::new(48000.0);
        let a = TapeParams::default();
        let b = TapeParams {
            drive_db: 20.0,
            saturation_pct: 100.0,
            hf_rolloff_hz: 5000.0,
            wow: 1.0,
            flutter: 1.0,
            hysteresis: 0.4,
            head_bump: 1.0,
            output_db: -12.0,
            ..TapeParams::default()
        };

        for i in 0..=10 {
            let t = i as f32 / 10.0;
            let morphed = TapeParams::lerp(&a, &b, t);
            let (l, r) = kernel.process_stereo(0.3, -0.3, &morphed);
            assert!(
                l.is_finite() && r.is_finite(),
                "Morph at t={t:.1} produced NaN/Inf: l={l}, r={r}",
            );
            kernel.reset();
        }
    }

    /// from_knobs maps all parameters to expected ranges at min, max, and midpoint.
    #[test]
    fn from_knobs_maps_ranges() {
        let p_min = TapeParams::from_knobs(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert!((p_min.drive_db - 0.0).abs() < 0.01, "drive at 0");
        assert!((p_min.saturation_pct - 0.0).abs() < 0.01, "sat at 0");
        assert!((p_min.hf_rolloff_hz - 1000.0).abs() < 1.0, "hf min");
        assert!((p_min.bias - (-0.2)).abs() < 0.01, "bias at 0");
        assert!((p_min.output_db - (-12.0)).abs() < 0.01, "output at 0");

        let p_max = TapeParams::from_knobs(1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0);
        assert!((p_max.drive_db - 24.0).abs() < 0.01, "drive at 1");
        assert!((p_max.saturation_pct - 100.0).abs() < 0.01, "sat at 1");
        assert!((p_max.hf_rolloff_hz - 20000.0).abs() < 1.0, "hf max");
        assert!((p_max.bias - 0.2).abs() < 0.01, "bias at 1");
        assert!((p_max.output_db - 12.0).abs() < 0.01, "output at 1");

        let p_mid = TapeParams::from_knobs(0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5);
        assert!((p_mid.drive_db - 12.0).abs() < 0.1, "drive mid");
        assert!((p_mid.saturation_pct - 50.0).abs() < 0.5, "sat mid");
        assert!(p_mid.bias.abs() < 0.01, "bias mid is zero");
        assert!((p_mid.hysteresis - 0.25).abs() < 0.01, "hysteresis mid");
        assert!(p_mid.output_db.abs() < 0.1, "output mid is zero");
    }

    /// Higher drive produces greater output amplitude for a quiet signal.
    #[test]
    fn drive_increases_amplitude() {
        let low_drive = TapeParams {
            drive_db: 0.0,
            saturation_pct: 0.0,
            wow: 0.0,
            flutter: 0.0,
            hysteresis: 0.0,
            head_bump: 0.0,
            output_db: 0.0,
            ..TapeParams::default()
        };
        let high_drive = TapeParams {
            drive_db: 18.0,
            ..low_drive
        };

        let input = 0.01_f32;
        let mut k_low = TapeKernel::new(48000.0);
        let (low_out, _) = k_low.process_stereo(input, input, &low_drive);

        let mut k_high = TapeKernel::new(48000.0);
        let (high_out, _) = k_high.process_stereo(input, input, &high_drive);

        assert!(
            high_out.abs() > low_out.abs(),
            "Higher drive should increase output: low={low_out}, high={high_out}",
        );
    }

    /// The waveshaper is asymmetric: equal-magnitude +/- inputs give different outputs.
    #[test]
    fn asymmetric_saturation() {
        let params = TapeParams {
            drive_db: 18.0,
            saturation_pct: 100.0,
            wow: 0.0,
            flutter: 0.0,
            hysteresis: 0.0,
            head_bump: 0.0,
            output_db: 0.0,
            ..TapeParams::default()
        };

        let mut kernel_pos = TapeKernel::new(48000.0);
        for _ in 0..200 {
            kernel_pos.process_stereo(0.0, 0.0, &params);
        }
        let (pos, _) = kernel_pos.process_stereo(0.5, 0.5, &params);

        let mut kernel_neg = TapeKernel::new(48000.0);
        for _ in 0..200 {
            kernel_neg.process_stereo(0.0, 0.0, &params);
        }
        let (neg, _) = kernel_neg.process_stereo(-0.5, -0.5, &params);

        assert!(
            (pos.abs() - neg.abs()).abs() > 0.001,
            "Saturation should be asymmetric: pos={pos}, neg={neg}",
        );
    }

    /// Non-zero saturation with high drive changes the signal relative to zero saturation.
    ///
    /// At `saturation_pct=0` the blend is entirely `clean = driven.clamp(-1, 1)`.
    /// At `saturation_pct=100` the blend is entirely the ADAA waveshaper output,
    /// which uses an asymmetric exponential and differs from hard-clip.
    #[test]
    fn saturation_distorts() {
        let mut clean = TapeKernel::new(48000.0);
        let mut saturated = TapeKernel::new(48000.0);

        let params_clean = TapeParams {
            drive_db: 18.0,
            saturation_pct: 0.0,
            wow: 0.0,
            flutter: 0.0,
            hysteresis: 0.0,
            head_bump: 0.0,
            output_db: 0.0,
            ..TapeParams::default()
        };
        let params_sat = TapeParams {
            drive_db: 18.0,
            saturation_pct: 100.0,
            wow: 0.0,
            flutter: 0.0,
            hysteresis: 0.0,
            head_bump: 0.0,
            output_db: 0.0,
            ..TapeParams::default()
        };

        let mut diff_sum = 0.0_f32;
        let mut phase: f32 = 0.0;
        let phase_inc: f32 = 440.0 / 48000.0;

        for _ in 0..500 {
            let input = if phase < 0.5 {
                4.0 * phase - 1.0
            } else {
                3.0 - 4.0 * phase
            };
            phase += phase_inc;
            if phase >= 1.0 {
                phase -= 1.0;
            }
            let (l_clean, _) = clean.process_stereo(input, input, &params_clean);
            let (l_sat, _) = saturated.process_stereo(input, input, &params_sat);
            diff_sum += libm::fabsf(l_clean - l_sat);
        }

        assert!(
            diff_sum > 0.01,
            "Saturation should change signal: diff_sum={diff_sum}"
        );
    }

    /// Head bump at 1.0 boosts 80 Hz energy relative to head_bump=0.0.
    ///
    /// The head bump applies a peaking EQ at `bump_freq_hz` with up to +6 dB of gain.
    /// A sustained 80 Hz sine should accumulate more energy when bump=1.0.
    #[test]
    fn head_bump_boosts_lows() {
        let sr = 48000.0;

        let mut bumped = TapeKernel::new(sr);
        let mut flat = TapeKernel::new(sr);

        let params_bumped = TapeParams {
            head_bump: 1.0,
            bump_freq_hz: 80.0,
            saturation_pct: 0.0,
            wow: 0.0,
            flutter: 0.0,
            hysteresis: 0.0,
            drive_db: 0.0,
            output_db: 0.0,
            ..TapeParams::default()
        };
        let params_flat = TapeParams {
            head_bump: 0.0,
            ..params_bumped
        };

        let mut energy_bumped = 0.0_f32;
        let mut energy_flat = 0.0_f32;

        // Feed 80 Hz sine for 2000 samples; accumulate energy in the second half (filters settled)
        for i in 0..2000 {
            let input = 0.3 * libm::sinf(2.0 * core::f32::consts::PI * 80.0 * i as f32 / sr);
            let (l_b, _) = bumped.process_stereo(input, input, &params_bumped);
            let (l_f, _) = flat.process_stereo(input, input, &params_flat);
            if i >= 1000 {
                energy_bumped += l_b * l_b;
                energy_flat += l_f * l_f;
            }
        }

        assert!(
            energy_bumped > energy_flat * 1.1,
            "Head bump should boost 80 Hz energy: bumped={energy_bumped}, flat={energy_flat}"
        );
    }

    /// get/set round-trip: set() followed by get() returns the stored value.
    #[test]
    fn params_get_set_roundtrip() {
        let mut params = TapeParams::default();
        params.set(0, 18.0);
        assert!((params.get(0) - 18.0).abs() < 1e-6, "drive");
        params.set(1, 75.0);
        assert!((params.get(1) - 75.0).abs() < 1e-6, "saturation");
        params.set(6, 0.3);
        assert!((params.get(6) - 0.3).abs() < 1e-6, "hysteresis");
        params.set(9, -3.0);
        assert!((params.get(9) - (-3.0)).abs() < 1e-6, "output");
    }

    /// ParamId 1402 is output (index 9) and ParamId 1403 is hf_rolloff (index 2).
    #[test]
    fn param_id_ordering_correct() {
        let out_desc = TapeParams::descriptor(9).unwrap();
        assert_eq!(out_desc.id, ParamId(1402), "output ParamId must be 1402");
        let hf_desc = TapeParams::descriptor(2).unwrap();
        assert_eq!(hf_desc.id, ParamId(1403), "hf_rolloff ParamId must be 1403");
    }

    /// Descriptor defaults match the Default impl values.
    #[test]
    fn descriptor_defaults_match_struct() {
        let p = TapeParams::default();
        for i in 0..TapeParams::COUNT {
            let desc = TapeParams::descriptor(i).unwrap();
            let struct_val = p.get(i);
            assert!(
                (struct_val - desc.default).abs() < 0.01,
                "Default mismatch at index {i} ({}): desc={}, struct={}",
                desc.name,
                desc.default,
                struct_val,
            );
        }
    }

    /// With wow=0 and flutter=0 the bypass path produces finite output.
    #[test]
    fn wow_flutter_bypass_is_valid() {
        let mut kernel = TapeKernel::new(48000.0);
        let params = TapeParams {
            wow: 0.0,
            flutter: 0.0,
            ..TapeParams::default()
        };
        for _ in 0..200 {
            let (l, r) = kernel.process_stereo(0.4, -0.4, &params);
            assert!(
                l.is_finite() && r.is_finite(),
                "non-finite in wow/flutter bypass"
            );
        }
    }

    /// Parameters written via set_param() survive a snapshot/restore cycle.
    #[test]
    fn snapshot_roundtrip_through_adapter() {
        let kernel = TapeKernel::new(48000.0);
        let mut adapter = KernelAdapter::new(kernel, 48000.0);
        adapter.set_param(0, 18.0);
        adapter.set_param(1, 80.0);
        adapter.set_param(6, 0.25);

        let saved = adapter.snapshot();

        let kernel2 = TapeKernel::new(48000.0);
        let mut adapter2 = KernelAdapter::new(kernel2, 48000.0);
        adapter2.load_snapshot(&saved);

        assert!(
            (adapter2.get_param(0) - 18.0).abs() < 0.01,
            "drive snapshot"
        );
        assert!(
            (adapter2.get_param(1) - 80.0).abs() < 0.01,
            "saturation snapshot"
        );
        assert!(
            (adapter2.get_param(6) - 0.25).abs() < 0.01,
            "hysteresis snapshot"
        );
    }
}
