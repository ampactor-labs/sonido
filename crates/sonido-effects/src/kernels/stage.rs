//! Stage kernel — pure DSP with separated parameter ownership.
//!
//! Implements the Stage effect using the kernel architecture.
//! The DSP math is identical; the difference is structural:
//!
//! - **Classic `Stage`**: owns `SmoothedParam` for gain/width/balance/haas/output,
//!   manages smoothing internally, implements `Effect` + `ParameterInfo` via `impl_params!`.
//!
//! - **`StageKernel`**: owns ONLY DSP state (DC blockers, LR4 biquads, Haas delay line).
//!   Parameters are received via `&StageParams` on each processing call.
//!   Deployed via [`KernelAdapter`](sonido_core::KernelAdapter) for desktop/plugin,
//!   or called directly on embedded targets.
//!
//! # Signal Flow
//!
//! ```text
//! Input L/R
//!   → Phase invert  (±1 per channel, immediate)
//!   → Channel mode  (Normal / Swap / Mono-L / Mono-R, stepped)
//!   → Input gain    (params.gain_db → linear, smoothed by adapter)
//!   → DC block      (optional, DcBlocker × 2)
//!   → M/S width     (mid = (L+R)×0.5, side = (L−R)×0.5×width, decode)
//!   → Balance       (linear attenuation per channel)
//!   → Bass mono     (LR4 crossover → mono-sum lows, keep highs stereo)
//!   → Haas delay    (0–30 ms on selected channel, InterpolatedDelay)
//!   → Output level  (params.output_db → linear, smoothed by adapter)
//!   → Output L/R
//! ```
//!
//! # M/S Width Algorithm
//!
//! ```text
//! mid  = (L + R) × 0.5
//! side = (L − R) × 0.5 × width        // width = params.width_pct / 100
//! out_L = mid + side
//! out_R = mid − side
//! ```
//!
//! At width = 1.0 (100 %) this is the identity transform. At 0.0 (0 %) both
//! channels become the mono mid. At 2.0 (200 %) the side content is doubled,
//! widening the stereo image beyond the original mix.
//!
//! # LR4 Bass Mono Crossover (Linkwitz-Riley 4th-order)
//!
//! Two cascaded Butterworth LP biquads (Q = 1/√2 ≈ 0.707) per channel form the
//! low band. Two cascaded Butterworth HP biquads at the same Q form the high band.
//! The low bands from L and R are mono-summed; the high bands remain stereo and are
//! recombined with the mono low.
//!
//! Reference: Linkwitz, "Active Crossover Networks for Noncoincident Drivers",
//! JAES 1976.
//!
//! # Haas Effect
//!
//! A short delay (0–30 ms) on one channel creates a spatial impression without
//! panning. Uses [`InterpolatedDelay`] with linear interpolation so that delay
//! time changes from the adapter's smoothed value are artefact-free.
//!
//! Reference: Haas, H. (1951). "The influence of a single echo on the
//! audibility of speech", Acustica.
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! use sonido_core::KernelAdapter;
//! let adapter = KernelAdapter::new(StageKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing, ADCs are hardware-filtered)
//! let mut kernel = StageKernel::new(48000.0);
//! let params = StageParams::from_knobs(
//!     gain, width, balance, phase_l, phase_r, channel,
//!     dc_block, bass_mono, bass_freq, haas, haas_side, output,
//! );
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

use sonido_core::biquad::{highpass_coefficients, lowpass_coefficients};
use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::math::ms_to_samples;
use sonido_core::{
    Biquad, DcBlocker, InterpolatedDelay, ParamDescriptor, ParamFlags, ParamId, ParamScale,
    ParamUnit, fast_db_to_linear,
};

// ── Unit conversion ───────────────────────────────────────────────────────────

/// Fast dB-to-linear conversion for per-sample use.
///
/// Uses `sonido_core::fast_db_to_linear` — polynomial approximation
/// (~0.1 dB accuracy, ~4× faster than `10^(db/20)`).
#[inline]
fn db_to_gain(db: f32) -> f32 {
    fast_db_to_linear(db)
}

// ── Constants ─────────────────────────────────────────────────────────────────

/// Butterworth Q for Linkwitz-Riley 4th-order crossover (two cascaded 2nd-order filters).
///
/// Each 2nd-order Butterworth stage uses Q = 1/√2. Two cascaded stages produce a
/// Linkwitz-Riley 4th-order alignment with −6 dB at the crossover frequency and
/// perfect power summation of the split bands.
const BUTTERWORTH_Q: f32 = core::f32::consts::FRAC_1_SQRT_2; // 1/√2 ≈ 0.7071

/// Maximum Haas delay in seconds.
///
/// Sets the size of the [`InterpolatedDelay`] buffer. The 30 ms limit keeps
/// the effect within the "precedence effect" window where the delayed channel
/// is perceived as spatial widening rather than a discrete echo.
const MAX_HAAS_SECONDS: f32 = 0.03;

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`StageKernel`].
///
/// All values in **user-facing units** — the same units shown in GUIs and
/// stored in presets. The kernel converts internally as needed.
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `gain_db` | dB | −40–12 | 0.0 |
/// | 1 | `width_pct` | % | 0–200 | 100.0 |
/// | 2 | `balance_pct` | % | −100–100 | 0.0 |
/// | 3 | `phase_l` | index | 0–1 | 0 (Off) |
/// | 4 | `phase_r` | index | 0–1 | 0 (Off) |
/// | 5 | `channel` | index | 0–3 | 0 (Normal) |
/// | 6 | `dc_block` | index | 0–1 | 0 (Off) |
/// | 7 | `bass_mono` | index | 0–1 | 0 (Off) |
/// | 8 | `bass_freq_hz` | Hz | 20–500 | 120.0 |
/// | 9 | `haas_ms` | ms | 0–30 | 0.0 |
/// | 10 | `haas_side` | index | 0–1 | 1 (Right) |
/// | 11 | `output_db` | dB | −20–20 | 0.0 |
#[derive(Clone, Copy)]
pub struct StageParams {
    /// Input trim in decibels.
    ///
    /// Range: −40.0 to +12.0 dB. Default 0.0 (unity gain).
    pub gain_db: f32,

    /// Stereo width as a percentage.
    ///
    /// Range: 0–200 %. 0 = mono, 100 = normal stereo, 200 = exaggerated side.
    /// Internally divided by 100 before the M/S matrix.
    pub width_pct: f32,

    /// Stereo balance as a percentage.
    ///
    /// Range: −100 to +100 %. −100 = full left, 0 = center, +100 = full right.
    /// Attenuates the opposite channel with `min(1.0, 1.0 ± balance)`.
    pub balance_pct: f32,

    /// Left channel phase inversion.
    ///
    /// 0.0 = Off (normal polarity), 1.0 = On (inverted). Stepped.
    pub phase_l: f32,

    /// Right channel phase inversion.
    ///
    /// 0.0 = Off (normal polarity), 1.0 = On (inverted). Stepped.
    pub phase_r: f32,

    /// Channel routing mode.
    ///
    /// 0.0 = Normal, 1.0 = Swap L↔R, 2.0 = Mono-L (both = left), 3.0 = Mono-R. Stepped.
    pub channel: f32,

    /// DC blocking filter enable.
    ///
    /// 0.0 = Off, 1.0 = On. Stepped. When On, a first-order high-pass at ~5 Hz
    /// removes any DC offset before further processing.
    pub dc_block: f32,

    /// Bass mono crossover enable.
    ///
    /// 0.0 = Off, 1.0 = On. Stepped. When On, bass content below
    /// [`bass_freq_hz`](Self::bass_freq_hz) is mono-summed.
    pub bass_mono: f32,

    /// Bass mono crossover frequency in Hz.
    ///
    /// Range: 20 to 500 Hz. Default 120 Hz. Uses a Linkwitz-Riley 4th-order crossover.
    /// Logarithmic scale.
    pub bass_freq_hz: f32,

    /// Haas delay time in milliseconds.
    ///
    /// Range: 0.0 to 30.0 ms. Default 0.0 (disabled). Applies to the channel
    /// selected by [`haas_side`](Self::haas_side).
    pub haas_ms: f32,

    /// Which channel receives the Haas delay.
    ///
    /// 0.0 = Left, 1.0 = Right. Default 1.0 (Right). Stepped.
    pub haas_side: f32,

    /// Output level in decibels.
    ///
    /// Range: −20.0 to +20.0 dB. Default 0.0 (unity gain).
    pub output_db: f32,
}

impl Default for StageParams {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            width_pct: 100.0,
            balance_pct: 0.0,
            phase_l: 0.0,
            phase_r: 0.0,
            channel: 0.0,
            dc_block: 0.0,
            bass_mono: 0.0,
            bass_freq_hz: 120.0,
            haas_ms: 0.0,
            haas_side: 1.0,
            output_db: 0.0,
        }
    }
}

impl StageParams {
    /// Build params directly from hardware knob readings (0.0–1.0 normalized).
    ///
    /// Convenience constructor for embedded targets where ADC values map to
    /// parameter ranges. Stepped parameters are threshold-rounded (≥ 0.5 = On).
    ///
    /// # Arguments (all 0.0–1.0)
    ///
    /// - `gain`:      Input gain knob → −40 to +12 dB
    /// - `width`:     Width knob → 0 to 200 %
    /// - `balance`:   Balance knob → −100 to +100 %
    /// - `phase_l`:   Phase-L toggle → 0 or 1 (stepped at 0.5)
    /// - `phase_r`:   Phase-R toggle → 0 or 1 (stepped at 0.5)
    /// - `channel`:   Channel mode → 0–3 (scaled, rounded)
    /// - `dc_block`:  DC block toggle → 0 or 1
    /// - `bass_mono`: Bass mono toggle → 0 or 1
    /// - `bass_freq`: Bass frequency → 20–500 Hz (logarithmic)
    /// - `haas`:      Haas delay → 0–30 ms
    /// - `haas_side`: Haas side toggle → 0 or 1
    /// - `output`:    Output level → −20 to +20 dB
    #[allow(clippy::too_many_arguments)]
    pub fn from_knobs(
        gain: f32,
        width: f32,
        balance: f32,
        phase_l: f32,
        phase_r: f32,
        channel: f32,
        dc_block: f32,
        bass_mono: f32,
        bass_freq: f32,
        haas: f32,
        haas_side: f32,
        output: f32,
    ) -> Self {
        // Logarithmic mapping for bass_freq: 20 * (500/20)^t = 20 * 25^t
        let bass_freq_hz = 20.0 * libm::powf(25.0, bass_freq.clamp(0.0, 1.0));
        Self {
            gain_db: gain.clamp(0.0, 1.0) * 52.0 - 40.0, // −40 to +12 dB
            width_pct: width.clamp(0.0, 1.0) * 200.0,    // 0–200 %
            balance_pct: balance.clamp(0.0, 1.0) * 200.0 - 100.0, // −100 to +100 %
            phase_l: if phase_l >= 0.5 { 1.0 } else { 0.0 },
            phase_r: if phase_r >= 0.5 { 1.0 } else { 0.0 },
            channel: libm::roundf(channel.clamp(0.0, 1.0) * 3.0).clamp(0.0, 3.0),
            dc_block: if dc_block >= 0.5 { 1.0 } else { 0.0 },
            bass_mono: if bass_mono >= 0.5 { 1.0 } else { 0.0 },
            bass_freq_hz: bass_freq_hz.clamp(20.0, 500.0),
            haas_ms: haas.clamp(0.0, 1.0) * 30.0, // 0–30 ms
            haas_side: if haas_side >= 0.5 { 1.0 } else { 0.0 },
            output_db: output.clamp(0.0, 1.0) * 40.0 - 20.0, // −20 to +20 dB
        }
    }
}

impl KernelParams for StageParams {
    const COUNT: usize = 12;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(
                ParamDescriptor::gain_db("Gain", "Gain", -40.0, 12.0, 0.0)
                    .with_id(ParamId(1900), "stage_gain"),
            ),
            1 => Some(
                ParamDescriptor::custom("Width", "Width", 0.0, 200.0, 100.0)
                    .with_unit(ParamUnit::Percent)
                    .with_step(1.0)
                    .with_id(ParamId(1901), "stage_width"),
            ),
            2 => Some(
                ParamDescriptor::custom("Balance", "Bal", -100.0, 100.0, 0.0)
                    .with_unit(ParamUnit::Percent)
                    .with_step(1.0)
                    .with_id(ParamId(1902), "stage_balance"),
            ),
            3 => Some(
                ParamDescriptor::custom("Phase L", "PhL", 0.0, 1.0, 0.0)
                    .with_step(1.0)
                    .with_id(ParamId(1903), "stage_phase_l")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(&["Off", "On"]),
            ),
            4 => Some(
                ParamDescriptor::custom("Phase R", "PhR", 0.0, 1.0, 0.0)
                    .with_step(1.0)
                    .with_id(ParamId(1904), "stage_phase_r")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(&["Off", "On"]),
            ),
            5 => Some(
                ParamDescriptor::custom("Channel", "Chan", 0.0, 3.0, 0.0)
                    .with_step(1.0)
                    .with_id(ParamId(1905), "stage_channel")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(&["Normal", "Swap", "Mono L", "Mono R"]),
            ),
            6 => Some(
                ParamDescriptor::custom("DC Block", "DC", 0.0, 1.0, 0.0)
                    .with_step(1.0)
                    .with_id(ParamId(1906), "stage_dc_block")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(&["Off", "On"]),
            ),
            7 => Some(
                ParamDescriptor::custom("Bass Mono", "BMon", 0.0, 1.0, 0.0)
                    .with_step(1.0)
                    .with_id(ParamId(1907), "stage_bass_mono")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(&["Off", "On"]),
            ),
            8 => Some(
                ParamDescriptor::custom("Bass Freq", "BFrq", 20.0, 500.0, 120.0)
                    .with_unit(ParamUnit::Hertz)
                    .with_step(1.0)
                    .with_id(ParamId(1908), "stage_bass_freq")
                    .with_scale(ParamScale::Logarithmic),
            ),
            9 => Some(
                ParamDescriptor::custom("Haas", "Haas", 0.0, 30.0, 0.0)
                    .with_unit(ParamUnit::Milliseconds)
                    .with_step(0.1)
                    .with_id(ParamId(1909), "stage_haas"),
            ),
            10 => Some(
                ParamDescriptor::custom("Haas Side", "HSde", 0.0, 1.0, 1.0)
                    .with_step(1.0)
                    .with_id(ParamId(1910), "stage_haas_side")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(&["L", "R"]),
            ),
            11 => Some(
                sonido_core::gain::output_param_descriptor().with_id(ParamId(1911), "stage_output"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Standard,  // gain_db — 10 ms
            1 => SmoothingStyle::Standard,  // width_pct — 10 ms
            2 => SmoothingStyle::Standard,  // balance_pct — 10 ms
            3 => SmoothingStyle::None,      // phase_l — stepped, snap immediately
            4 => SmoothingStyle::None,      // phase_r — stepped, snap immediately
            5 => SmoothingStyle::None,      // channel — stepped, snap immediately
            6 => SmoothingStyle::None,      // dc_block — stepped, snap immediately
            7 => SmoothingStyle::None,      // bass_mono — stepped, snap immediately
            8 => SmoothingStyle::Slow,      // bass_freq_hz — filter coefficient, 20 ms
            9 => SmoothingStyle::Fast,      // haas_ms — delay time, 5 ms
            10 => SmoothingStyle::None,     // haas_side — stepped, snap immediately
            11 => SmoothingStyle::Standard, // output_db — 10 ms
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.gain_db,
            1 => self.width_pct,
            2 => self.balance_pct,
            3 => self.phase_l,
            4 => self.phase_r,
            5 => self.channel,
            6 => self.dc_block,
            7 => self.bass_mono,
            8 => self.bass_freq_hz,
            9 => self.haas_ms,
            10 => self.haas_side,
            11 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.gain_db = value,
            1 => self.width_pct = value,
            2 => self.balance_pct = value,
            3 => self.phase_l = value,
            4 => self.phase_r = value,
            5 => self.channel = value,
            6 => self.dc_block = value,
            7 => self.bass_mono = value,
            8 => self.bass_freq_hz = value,
            9 => self.haas_ms = value,
            10 => self.haas_side = value,
            11 => self.output_db = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP Stage (signal conditioning / stereo utility) kernel.
///
/// Contains ONLY the mutable state required for audio processing:
///
/// - Two [`DcBlocker`] instances (one per channel)
/// - LR4 crossover biquads: `bass_lp[channel][stage]` (2×2) and `bass_hp[channel][stage]` (2×2)
/// - [`InterpolatedDelay`] for the Haas effect
/// - `sample_rate` and `last_bass_freq` for efficient coefficient caching
///
/// No `SmoothedParam`, no atomics, no platform awareness. The kernel is
/// `Send`-safe because all contained types are `Send`.
///
/// Note: [`DcBlocker`] does not implement `Debug` or `Clone`, so this struct
/// cannot derive those traits.
pub struct StageKernel {
    /// DC blocker for the left channel.
    dc_blocker_l: DcBlocker,
    /// DC blocker for the right channel.
    dc_blocker_r: DcBlocker,

    /// LR4 lowpass biquads for the bass mono crossover.
    ///
    /// Indexed as `[channel][stage]` where channel 0 = left, 1 = right,
    /// and two cascaded stages produce a Linkwitz-Riley 4th-order response.
    bass_lp: [[Biquad; 2]; 2],

    /// LR4 highpass biquads for the bass mono crossover.
    ///
    /// Indexed as `[channel][stage]`. Same topology as `bass_lp` but passes
    /// the high-frequency content that remains stereo after bass-mono summing.
    bass_hp: [[Biquad; 2]; 2],

    /// Delay line for the Haas effect.
    ///
    /// Sized at construction to hold up to [`MAX_HAAS_SECONDS`] of audio.
    /// Receives the selected channel's signal each sample and is read back
    /// at a fractional delay (interpolated).
    haas_delay_line: InterpolatedDelay,

    /// Current sample rate in Hz.
    sample_rate: f32,

    /// Last bass crossover frequency seen by the kernel.
    ///
    /// Coefficients are recomputed only when `params.bass_freq_hz` deviates
    /// from this value by more than 0.01 Hz, keeping the hot path coefficient-free
    /// during steady-state operation.
    last_bass_freq: f32,
}

impl StageKernel {
    /// Create a new Stage kernel at the given sample rate.
    ///
    /// Allocates the Haas delay line and pre-computes bass crossover
    /// coefficients at the default frequency (120 Hz).
    pub fn new(sample_rate: f32) -> Self {
        let mut kernel = Self {
            dc_blocker_l: DcBlocker::new(sample_rate),
            dc_blocker_r: DcBlocker::new(sample_rate),
            bass_lp: core::array::from_fn(|_| core::array::from_fn(|_| Biquad::new())),
            bass_hp: core::array::from_fn(|_| core::array::from_fn(|_| Biquad::new())),
            haas_delay_line: InterpolatedDelay::from_time(sample_rate, MAX_HAAS_SECONDS),
            sample_rate,
            last_bass_freq: f32::NAN, // Force initial coefficient computation
        };
        let defaults = StageParams::default();
        kernel.update_bass_crossover(defaults.bass_freq_hz);
        kernel
    }

    /// Recalculate all 8 LR4 crossover biquad coefficients for the given frequency.
    ///
    /// The Linkwitz-Riley 4th-order design uses two cascaded 2nd-order Butterworth
    /// stages at Q = 1/√2. Both the lowpass and highpass coefficients are updated
    /// for both channels simultaneously.
    fn update_bass_crossover(&mut self, freq_hz: f32) {
        let freq = freq_hz.clamp(20.0, 500.0_f32.min(self.sample_rate * 0.45));

        let (lb0, lb1, lb2, la0, la1, la2) =
            lowpass_coefficients(freq, BUTTERWORTH_Q, self.sample_rate);
        let (hb0, hb1, hb2, ha0, ha1, ha2) =
            highpass_coefficients(freq, BUTTERWORTH_Q, self.sample_rate);

        for ch in 0..2 {
            for stage in 0..2 {
                self.bass_lp[ch][stage].set_coefficients(lb0, lb1, lb2, la0, la1, la2);
                self.bass_hp[ch][stage].set_coefficients(hb0, hb1, hb2, ha0, ha1, ha2);
            }
        }
        self.last_bass_freq = freq;
    }

    /// Process a signal through two cascaded biquad stages (one LR4 half-filter).
    ///
    /// The two-stage cascade produces a 4th-order (24 dB/oct) roll-off.
    #[inline]
    fn process_lr4(biquads: &mut [Biquad; 2], input: f32) -> f32 {
        let mid = biquads[0].process(input);
        biquads[1].process(mid)
    }
}

impl DspKernel for StageKernel {
    type Params = StageParams;

    /// Process a stereo sample pair through the Stage signal chain.
    ///
    /// Processing order (matches the classic `Stage` effect exactly):
    ///
    /// 1. **Phase invert** — negate left if `phase_l ≥ 0.5`, right if `phase_r ≥ 0.5`.
    /// 2. **Channel mode** — Normal (0), Swap (1), Mono-L (2), Mono-R (3).
    /// 3. **Input gain** — `db_to_gain(params.gain_db)` applied to both channels.
    /// 4. **DC block** — if `dc_block ≥ 0.5`, route through per-channel DC blockers.
    /// 5. **M/S width** — encode to mid/side, scale side by `width_pct/100`, decode.
    /// 6. **Balance** — attenuate left or right with `min(1.0, 1.0 ∓ balance)`.
    /// 7. **Bass mono** — if `bass_mono ≥ 0.5`, split at crossover and mono-sum lows.
    /// 8. **Haas delay** — if `haas_ms > 0.01 samples`, delay the selected channel.
    /// 9. **Output level** — `db_to_gain(params.output_db)` applied to both channels.
    fn process_stereo(&mut self, left: f32, right: f32, params: &StageParams) -> (f32, f32) {
        // ── 1. Phase invert ──
        let mut l = if params.phase_l >= 0.5 { -left } else { left };
        let mut r = if params.phase_r >= 0.5 { -right } else { right };

        // ── 2. Channel mode ──
        match params.channel as u8 {
            0 => {}                               // Normal
            1 => core::mem::swap(&mut l, &mut r), // Swap
            2 => r = l,                           // Mono-L: both channels = left input
            _ => l = r,                           // Mono-R: both channels = right input
        }

        // ── 3. Input gain ──
        let input_gain = db_to_gain(params.gain_db);
        l *= input_gain;
        r *= input_gain;

        // ── 4. DC block ──
        if params.dc_block >= 0.5 {
            l = self.dc_blocker_l.process(l);
            r = self.dc_blocker_r.process(r);
        }

        // ── 5. M/S width ──
        let width = params.width_pct / 100.0;
        let mid = (l + r) * 0.5;
        let side = (l - r) * 0.5 * width;
        l = mid + side;
        r = mid - side;

        // ── 6. Balance ──
        let bal = params.balance_pct / 100.0;
        let gain_l = (1.0_f32 - bal).min(1.0);
        let gain_r = (1.0_f32 + bal).min(1.0);
        l *= gain_l;
        r *= gain_r;

        // ── 7. Bass mono (LR4 crossover) ──
        if params.bass_mono >= 0.5 {
            // Recompute crossover coefficients only when frequency changes.
            if (params.bass_freq_hz - self.last_bass_freq).abs() > 0.01 {
                self.update_bass_crossover(params.bass_freq_hz);
            }

            let low_l = Self::process_lr4(&mut self.bass_lp[0], l);
            let low_r = Self::process_lr4(&mut self.bass_lp[1], r);
            let high_l = Self::process_lr4(&mut self.bass_hp[0], l);
            let high_r = Self::process_lr4(&mut self.bass_hp[1], r);
            let mono_low = (low_l + low_r) * 0.5;
            l = mono_low + high_l;
            r = mono_low + high_r;
        }

        // ── 8. Haas delay ──
        let haas_samples = ms_to_samples(params.haas_ms.clamp(0.0, 30.0), self.sample_rate);
        if haas_samples > 0.01 {
            if params.haas_side >= 0.5 {
                // Delay right channel
                let delayed = self.haas_delay_line.read(haas_samples);
                self.haas_delay_line.write(r);
                r = delayed;
            } else {
                // Delay left channel
                let delayed = self.haas_delay_line.read(haas_samples);
                self.haas_delay_line.write(l);
                l = delayed;
            }
        } else {
            // Keep the delay line fed even when inactive to prevent stale reads
            // if haas_ms is later increased.
            if params.haas_side >= 0.5 {
                self.haas_delay_line.write(r);
            } else {
                self.haas_delay_line.write(l);
            }
        }

        // ── 9. Output level ──
        let output_gain = db_to_gain(params.output_db);
        (l * output_gain, r * output_gain)
    }

    fn reset(&mut self) {
        self.dc_blocker_l.reset();
        self.dc_blocker_r.reset();
        self.haas_delay_line.clear();
        for ch in 0..2 {
            for stage in 0..2 {
                self.bass_lp[ch][stage].clear();
                self.bass_hp[ch][stage].clear();
            }
        }
        // Retain last_bass_freq so coefficients are not recomputed unnecessarily
        // on the next call if bass_freq has not changed.
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.dc_blocker_l.set_sample_rate(sample_rate);
        self.dc_blocker_r.set_sample_rate(sample_rate);
        // Reallocate delay line for the new sample rate.
        self.haas_delay_line = InterpolatedDelay::from_time(sample_rate, MAX_HAAS_SECONDS);
        // Force crossover recomputation with the new sample rate.
        let freq = if self.last_bass_freq.is_nan() {
            StageParams::default().bass_freq_hz
        } else {
            self.last_bass_freq
        };
        self.last_bass_freq = f32::NAN; // trigger recompute
        self.update_bass_crossover(freq);
    }

    fn is_true_stereo(&self) -> bool {
        // Stage always performs stereo operations: M/S width, balance, Haas delay.
        // Even at unity settings, the architecture routes L and R through different
        // DSP paths (different DC blockers, different crossover chains) — true stereo.
        true
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

    // ── Basic correctness ─────────────────────────────────────────────────────

    /// Silence in must produce silence out with default parameters.
    ///
    /// With gain = 0 dB, width = 100 %, balance = 0, no DC offset, no bass mono,
    /// and no Haas delay, the kernel is a unity-gain stereo passthrough.
    /// Zero input → zero output.
    #[test]
    fn silence_in_silence_out() {
        let mut kernel = StageKernel::new(48000.0);
        let params = StageParams::default();

        let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
        assert!(l.abs() < 1e-6, "Expected silence on left, got {l}");
        assert!(r.abs() < 1e-6, "Expected silence on right, got {r}");
    }

    /// Processing must never produce NaN or ±Infinity over 1000 samples
    /// with all features active.
    #[test]
    fn no_nan_or_inf() {
        let mut kernel = StageKernel::new(48000.0);
        let params = StageParams {
            gain_db: 6.0,
            width_pct: 150.0,
            balance_pct: 25.0,
            phase_l: 1.0,
            dc_block: 1.0,
            bass_mono: 1.0,
            bass_freq_hz: 120.0,
            haas_ms: 10.0,
            haas_side: 1.0,
            output_db: -3.0,
            ..Default::default()
        };

        for i in 0..1000 {
            let t = i as f32 / 48000.0;
            let input = libm::sinf(2.0 * core::f32::consts::PI * 440.0 * t);
            let (l, r) = kernel.process_stereo(input, input, &params);
            assert!(l.is_finite(), "Left NaN/Inf at sample {i}: {l}");
            assert!(r.is_finite(), "Right NaN/Inf at sample {i}: {r}");
        }
    }

    /// Descriptor count must equal `COUNT` and all indices must be populated.
    #[test]
    fn params_descriptor_count() {
        assert_eq!(StageParams::COUNT, 12, "Expected 12 parameters");

        for i in 0..StageParams::COUNT {
            assert!(
                StageParams::descriptor(i).is_some(),
                "Missing descriptor at index {i}"
            );
        }
        assert!(
            StageParams::descriptor(StageParams::COUNT).is_none(),
            "Descriptor beyond COUNT must be None"
        );
    }

    /// The kernel must wrap into a `KernelAdapter` and function as an `Effect`.
    #[test]
    fn adapter_wraps_as_effect() {
        let kernel = StageKernel::new(48000.0);
        let mut adapter = KernelAdapter::new(kernel, 48000.0);

        adapter.reset();
        let output = adapter.process(0.3);
        assert!(!output.is_nan(), "adapter.process() returned NaN");
        assert!(output.is_finite(), "adapter.process() returned Inf");
    }

    /// The adapter's `ParameterInfo` must expose the correct names and ParamIds.
    #[test]
    fn adapter_param_info_matches() {
        let kernel = StageKernel::new(48000.0);
        let adapter = KernelAdapter::new(kernel, 48000.0);

        assert_eq!(adapter.param_count(), 12);

        // Spot-check ParamIds 1900–1911
        let expected: &[(usize, ParamId, &str)] = &[
            (0, ParamId(1900), "Gain"),
            (1, ParamId(1901), "Width"),
            (2, ParamId(1902), "Balance"),
            (3, ParamId(1903), "Phase L"),
            (4, ParamId(1904), "Phase R"),
            (5, ParamId(1905), "Channel"),
            (6, ParamId(1906), "DC Block"),
            (7, ParamId(1907), "Bass Mono"),
            (8, ParamId(1908), "Bass Freq"),
            (9, ParamId(1909), "Haas"),
            (10, ParamId(1910), "Haas Side"),
            (11, ParamId(1911), "Output"),
        ];

        for &(idx, expected_id, expected_name) in expected {
            let desc = adapter.param_info(idx).unwrap_or_else(|| {
                panic!("param_info({idx}) must return Some");
            });
            assert_eq!(
                desc.id, expected_id,
                "param [{idx}] '{expected_name}' — expected id {expected_id:?}, got {:?}",
                desc.id
            );
            assert_eq!(
                desc.name, expected_name,
                "param [{idx}] — expected name '{expected_name}', got '{}'",
                desc.name
            );
        }

        assert!(adapter.param_info(12).is_none(), "index 12 must be None");
    }

    /// Morphing between two extreme param states must always produce finite output.
    #[test]
    fn morph_produces_valid_output() {
        let mut kernel = StageKernel::new(48000.0);
        let a = StageParams::default();
        let b = StageParams {
            gain_db: 12.0,
            width_pct: 200.0,
            balance_pct: -80.0,
            phase_l: 1.0,
            phase_r: 1.0,
            dc_block: 1.0,
            bass_mono: 1.0,
            bass_freq_hz: 300.0,
            haas_ms: 25.0,
            haas_side: 0.0,
            output_db: -10.0,
            ..Default::default()
        };

        for step in 0..=10 {
            let t = step as f32 / 10.0;
            let morphed = StageParams::lerp(&a, &b, t);
            let (l, r) = kernel.process_stereo(0.3, -0.3, &morphed);
            assert!(
                l.is_finite() && r.is_finite(),
                "Morph at t={t} produced NaN/Inf: l={l}, r={r}"
            );
            kernel.reset();
        }
    }

    /// `from_knobs()` must map 0.0–1.0 inputs to the correct parameter ranges.
    #[test]
    fn from_knobs_maps_ranges() {
        // All knobs at maximum
        let p = StageParams::from_knobs(1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0);
        assert!(
            (p.gain_db - 12.0).abs() < 0.1,
            "Gain at 1.0 should be 12 dB, got {}",
            p.gain_db
        );
        assert!(
            (p.width_pct - 200.0).abs() < 0.1,
            "Width at 1.0 should be 200 %, got {}",
            p.width_pct
        );
        assert!(
            (p.balance_pct - 100.0).abs() < 0.1,
            "Balance at 1.0 should be 100 %, got {}",
            p.balance_pct
        );
        assert!(
            (p.haas_ms - 30.0).abs() < 0.1,
            "Haas at 1.0 should be 30 ms, got {}",
            p.haas_ms
        );
        assert!(
            (p.output_db - 20.0).abs() < 0.1,
            "Output at 1.0 should be 20 dB, got {}",
            p.output_db
        );

        // All knobs at minimum
        let p = StageParams::from_knobs(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert!(
            (p.gain_db - (-40.0)).abs() < 0.1,
            "Gain at 0.0 should be -40 dB, got {}",
            p.gain_db
        );
        assert!(
            p.width_pct.abs() < 0.1,
            "Width at 0.0 should be 0 %, got {}",
            p.width_pct
        );
        assert!(
            (p.balance_pct - (-100.0)).abs() < 0.1,
            "Balance at 0.0 should be -100 %, got {}",
            p.balance_pct
        );
        assert!(
            p.haas_ms.abs() < 0.1,
            "Haas at 0.0 should be 0 ms, got {}",
            p.haas_ms
        );
        assert!(
            (p.output_db - (-20.0)).abs() < 0.1,
            "Output at 0.0 should be -20 dB, got {}",
            p.output_db
        );
    }

    /// Default parameters must pass a sustained signal with unity gain.
    ///
    /// After smoothing settles (1000 samples), default settings produce
    /// an output equal to the input within 1 % tolerance.
    #[test]
    fn default_unity_gain() {
        let mut kernel = StageKernel::new(48000.0);
        let params = StageParams::default();

        let mut l_out = 0.0_f32;
        let mut r_out = 0.0_f32;
        // Process 1000 samples to ensure biquad filters have settled
        for _ in 0..1000 {
            (l_out, r_out) = kernel.process_stereo(0.5, 0.5, &params);
        }

        assert!(
            (l_out - 0.5).abs() < 0.01,
            "Default gain should be unity on left: expected 0.5, got {l_out}"
        );
        assert!(
            (r_out - 0.5).abs() < 0.01,
            "Default gain should be unity on right: expected 0.5, got {r_out}"
        );
    }

    /// Phase inversion must negate the selected channel.
    ///
    /// With phase_l = 1.0 and identical L/R inputs, after the M/S matrix the
    /// inverted left leaks into the side channel. With unequal inputs (0.5, 0.5)
    /// and only left inverted, the width = 100 % (identity M/S) means:
    /// mid = (−0.5 + 0.5)/2 = 0, side = (−0.5 − 0.5)/2 = −0.5
    /// out_l = 0 + (−0.5) = −0.5, out_r = 0 − (−0.5) = +0.5
    #[test]
    fn phase_invert_works() {
        let mut kernel = StageKernel::new(48000.0);
        let params = StageParams {
            phase_l: 1.0, // invert left
            ..Default::default()
        };

        // Settle the filter state first with matching parameters
        for _ in 0..1000 {
            kernel.process_stereo(0.5, 0.5, &params);
        }

        let (l, r) = kernel.process_stereo(0.5, 0.5, &params);
        assert!(l < 0.0, "Phase-inverted left should be negative, got {l}");
        assert!(r > 0.0, "Right (non-inverted) should be positive, got {r}");
    }

    /// Width 0 % collapses both channels to the mono mid signal (L = R).
    ///
    /// With width = 0, side = 0 always:
    ///   mid = (L + R) / 2
    ///   out_l = mid + 0 = mid
    ///   out_r = mid − 0 = mid
    /// Both outputs are identical and equal to the average of L and R.
    #[test]
    fn width_mono() {
        let mut kernel = StageKernel::new(48000.0);
        let params = StageParams {
            width_pct: 0.0,
            ..Default::default()
        };

        for _ in 0..1000 {
            kernel.process_stereo(0.3, 0.7, &params);
        }
        let (l, r) = kernel.process_stereo(0.3, 0.7, &params);

        // Expected: mid = (0.3 + 0.7) / 2 = 0.5
        assert!(
            (l - 0.5).abs() < 0.05,
            "Width 0 % should produce mono mid ~0.5 on left, got {l}"
        );
        assert!(
            (r - 0.5).abs() < 0.05,
            "Width 0 % should produce mono mid ~0.5 on right, got {r}"
        );
        assert!(
            (l - r).abs() < 1e-5,
            "Width 0 % should make L = R, diff = {}",
            (l - r).abs()
        );
    }
}
