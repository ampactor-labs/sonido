//! Multiband compressor kernel — three-band dynamics with Linkwitz-Riley crossovers.
//!
//! `MultibandCompKernel` splits the signal into low, mid, and high bands via
//! cascaded Linkwitz-Riley crossover filters, compresses each band independently,
//! then sums the bands back with per-band makeup gain and a final output level.
//!
//! # Signal Flow
//!
//! ```text
//! Input ──┬── 2× LP @ xover_low ────────────────── low_band ── × low_gain ──┐
//!         │                                                                   │
//!         ├── 2× HP @ xover_low → 2× LP @ xover_high ─── mid_band ── × mid_gain ─┤── Sum → Output
//!         │                                                                   │
//!         └── 2× HP @ xover_high ───────────────── high_band ── × high_gain ─┘
//!
//! Each band: EnvelopeFollower (RMS, 10ms/100ms) → gain computer (thresh + ratio) → apply
//! ```
//!
//! # Linkwitz-Riley Crossovers
//!
//! A Linkwitz-Riley (LR2) crossover is two cascaded 2nd-order Butterworth filters
//! (Q = 1/√2 ≈ 0.707) at the same frequency. This gives −6 dB at the crossover
//! point and sums to a flat allpass response, preserving phase coherence when
//! the bands are recombined.
//!
//! Reference: Linkwitz, "Active Crossover Networks for Noncoincident Drivers",
//! JAES 1976; Zolzer, "DAFX" (2011), Ch. 3.
//!
//! # Gain Computer
//!
//! Hard-knee compressor per band. Given `x = envelope_db − threshold_db`:
//! - `x ≤ 0`: gain reduction = 0
//! - `x > 0`: gain reduction = −x × (1 − 1/ratio)
//!
//! Reference: Giannoulis, Massberg & Reiss, "Digital Dynamic Range Compressor
//! Design — A Tutorial and Analysis", JAES 2012.
//!
//! # Stereo Architecture
//!
//! All crossover biquads are `[Biquad; 2]` arrays (index 0 = L, index 1 = R).
//! The per-band envelope followers use linked-stereo detection: the maximum of
//! `|L|` and `|R|` band levels drives the gain computer, so both channels receive
//! the same gain reduction, preserving the stereo image.
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! let adapter = KernelAdapter::new(MultibandCompKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing)
//! let mut kernel = MultibandCompKernel::new(48000.0);
//! let params = MultibandCompParams::default();
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::{
    Biquad, Cached, EnvelopeFollower, ParamDescriptor, ParamId, ParamScale, ParamUnit,
    fast_db_to_linear, fast_linear_to_db, highpass_coefficients, lowpass_coefficients,
    math::flush_denormal,
};

// ── Constants ────────────────────────────────────────────────────────────────

/// Butterworth Q for Linkwitz-Riley crossovers (1/√2 ≈ 0.707).
const LR_Q: f32 = core::f32::consts::SQRT_2 * 0.5;

/// RMS envelope attack for band compression (ms).
const BAND_ATTACK_MS: f32 = 10.0;

/// RMS envelope release for band compression (ms).
const BAND_RELEASE_MS: f32 = 100.0;

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`MultibandCompKernel`].
///
/// All values are in **user-facing units**.
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `xover_low_hz` | Hz | 100–1000 | 250.0 |
/// | 1 | `xover_high_hz` | Hz | 1000–10000 | 3000.0 |
/// | 2 | `low_thresh_db` | dB | −60–0 | −20.0 |
/// | 3 | `low_ratio` | ratio | 1–20 | 4.0 |
/// | 4 | `mid_thresh_db` | dB | −60–0 | −20.0 |
/// | 5 | `mid_ratio` | ratio | 1–20 | 4.0 |
/// | 6 | `high_thresh_db` | dB | −60–0 | −20.0 |
/// | 7 | `high_ratio` | ratio | 1–20 | 4.0 |
/// | 8 | `low_gain_db` | dB | −12–12 | 0.0 |
/// | 9 | `mid_gain_db` | dB | −12–12 | 0.0 |
/// | 10 | `high_gain_db` | dB | −12–12 | 0.0 |
/// | 11 | `output_db` | dB | −60–+6 | 0.0 |
#[derive(Debug, Clone, Copy)]
pub struct MultibandCompParams {
    /// Low/mid crossover frequency in Hz.
    ///
    /// Range: 100.0–1000.0 Hz (default 250.0). The Linkwitz-Riley crossover
    /// that separates the low band from the mid band.
    pub xover_low_hz: f32,

    /// Mid/high crossover frequency in Hz.
    ///
    /// Range: 1000.0–10000.0 Hz (default 3000.0). The Linkwitz-Riley crossover
    /// that separates the mid band from the high band.
    pub xover_high_hz: f32,

    /// Low band compression threshold in dB.
    ///
    /// Range: −60.0–0.0 dB (default −20.0). Gain reduction begins when the
    /// low band envelope exceeds this level.
    pub low_thresh_db: f32,

    /// Low band compression ratio.
    ///
    /// Range: 1.0–20.0 (default 4.0). At 4.0, a 4 dB overshoot produces 1 dB output.
    pub low_ratio: f32,

    /// Mid band compression threshold in dB.
    ///
    /// Range: −60.0–0.0 dB (default −20.0). Gain reduction begins when the
    /// mid band envelope exceeds this level.
    pub mid_thresh_db: f32,

    /// Mid band compression ratio.
    ///
    /// Range: 1.0–20.0 (default 4.0).
    pub mid_ratio: f32,

    /// High band compression threshold in dB.
    ///
    /// Range: −60.0–0.0 dB (default −20.0). Gain reduction begins when the
    /// high band envelope exceeds this level.
    pub high_thresh_db: f32,

    /// High band compression ratio.
    ///
    /// Range: 1.0–20.0 (default 4.0).
    pub high_ratio: f32,

    /// Low band makeup gain in dB.
    ///
    /// Range: −12.0–12.0 dB (default 0.0). Applied after low band compression.
    pub low_gain_db: f32,

    /// Mid band makeup gain in dB.
    ///
    /// Range: −12.0–12.0 dB (default 0.0). Applied after mid band compression.
    pub mid_gain_db: f32,

    /// High band makeup gain in dB.
    ///
    /// Range: −12.0–12.0 dB (default 0.0). Applied after high band compression.
    pub high_gain_db: f32,

    /// Output level in dB.
    ///
    /// Range: −60.0–+6.0 dB (default 0.0). Final output trim applied after
    /// the band sum.
    pub output_db: f32,
}

impl Default for MultibandCompParams {
    fn default() -> Self {
        Self {
            xover_low_hz: 250.0,
            xover_high_hz: 3000.0,
            low_thresh_db: -20.0,
            low_ratio: 4.0,
            mid_thresh_db: -20.0,
            mid_ratio: 4.0,
            high_thresh_db: -20.0,
            high_ratio: 4.0,
            low_gain_db: 0.0,
            mid_gain_db: 0.0,
            high_gain_db: 0.0,
            output_db: 0.0,
        }
    }
}

impl KernelParams for MultibandCompParams {
    const COUNT: usize = 12;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            // ── [0] Low/mid crossover ─────────────────────────────────────────
            0 => Some(
                ParamDescriptor::custom("Low Xover", "Lo Xvr", 100.0, 1000.0, 250.0)
                    .with_unit(ParamUnit::Hertz)
                    .with_step(1.0)
                    .with_id(ParamId(2600), "mbc_xover_low")
                    .with_scale(ParamScale::Logarithmic),
            ),
            // ── [1] Mid/high crossover ────────────────────────────────────────
            1 => Some(
                ParamDescriptor::custom("High Xover", "Hi Xvr", 1000.0, 10000.0, 3000.0)
                    .with_unit(ParamUnit::Hertz)
                    .with_step(1.0)
                    .with_id(ParamId(2601), "mbc_xover_high")
                    .with_scale(ParamScale::Logarithmic),
            ),
            // ── [2] Low threshold ─────────────────────────────────────────────
            2 => Some(
                ParamDescriptor::gain_db("Low Thresh", "Lo Thr", -60.0, 0.0, -20.0)
                    .with_id(ParamId(2602), "mbc_low_thresh"),
            ),
            // ── [3] Low ratio ─────────────────────────────────────────────────
            3 => Some(
                ParamDescriptor::custom("Low Ratio", "Lo Rat", 1.0, 20.0, 4.0)
                    .with_unit(ParamUnit::Ratio)
                    .with_step(0.1)
                    .with_id(ParamId(2603), "mbc_low_ratio"),
            ),
            // ── [4] Mid threshold ─────────────────────────────────────────────
            4 => Some(
                ParamDescriptor::gain_db("Mid Thresh", "Md Thr", -60.0, 0.0, -20.0)
                    .with_id(ParamId(2604), "mbc_mid_thresh"),
            ),
            // ── [5] Mid ratio ─────────────────────────────────────────────────
            5 => Some(
                ParamDescriptor::custom("Mid Ratio", "Md Rat", 1.0, 20.0, 4.0)
                    .with_unit(ParamUnit::Ratio)
                    .with_step(0.1)
                    .with_id(ParamId(2605), "mbc_mid_ratio"),
            ),
            // ── [6] High threshold ────────────────────────────────────────────
            6 => Some(
                ParamDescriptor::gain_db("High Thresh", "Hi Thr", -60.0, 0.0, -20.0)
                    .with_id(ParamId(2606), "mbc_high_thresh"),
            ),
            // ── [7] High ratio ────────────────────────────────────────────────
            7 => Some(
                ParamDescriptor::custom("High Ratio", "Hi Rat", 1.0, 20.0, 4.0)
                    .with_unit(ParamUnit::Ratio)
                    .with_step(0.1)
                    .with_id(ParamId(2607), "mbc_high_ratio"),
            ),
            // ── [8] Low makeup gain ───────────────────────────────────────────
            8 => Some(
                ParamDescriptor::gain_db("Low Gain", "Lo Gain", -12.0, 12.0, 0.0)
                    .with_id(ParamId(2608), "mbc_low_gain"),
            ),
            // ── [9] Mid makeup gain ───────────────────────────────────────────
            9 => Some(
                ParamDescriptor::gain_db("Mid Gain", "Md Gain", -12.0, 12.0, 0.0)
                    .with_id(ParamId(2609), "mbc_mid_gain"),
            ),
            // ── [10] High makeup gain ─────────────────────────────────────────
            10 => Some(
                ParamDescriptor::gain_db("High Gain", "Hi Gain", -12.0, 12.0, 0.0)
                    .with_id(ParamId(2610), "mbc_high_gain"),
            ),
            // ── [11] Output level ─────────────────────────────────────────────
            11 => Some(
                sonido_core::gain::output_param_descriptor().with_id(ParamId(2611), "mbc_output"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 | 1 => SmoothingStyle::Slow, // crossover freqs — filter coeff recalc
            2..=7 => SmoothingStyle::Standard, // thresholds + ratios
            8..=10 => SmoothingStyle::Standard, // makeup gains
            11 => SmoothingStyle::Fast,    // output
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.xover_low_hz,
            1 => self.xover_high_hz,
            2 => self.low_thresh_db,
            3 => self.low_ratio,
            4 => self.mid_thresh_db,
            5 => self.mid_ratio,
            6 => self.high_thresh_db,
            7 => self.high_ratio,
            8 => self.low_gain_db,
            9 => self.mid_gain_db,
            10 => self.high_gain_db,
            11 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.xover_low_hz = value,
            1 => self.xover_high_hz = value,
            2 => self.low_thresh_db = value,
            3 => self.low_ratio = value,
            4 => self.mid_thresh_db = value,
            5 => self.mid_ratio = value,
            6 => self.high_thresh_db = value,
            7 => self.high_ratio = value,
            8 => self.low_gain_db = value,
            9 => self.mid_gain_db = value,
            10 => self.high_gain_db = value,
            11 => self.output_db = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Helper: apply biquad coefficients to a [Biquad; 2] stereo pair
// ═══════════════════════════════════════════════════════════════════════════

/// Apply a single set of biquad coefficients to both channels of a stereo pair.
#[inline]
fn set_stereo_biquad(biquads: &mut [Biquad; 2], c: &[f32; 6]) {
    biquads[0].set_coefficients(c[0], c[1], c[2], c[3], c[4], c[5]);
    biquads[1].set_coefficients(c[0], c[1], c[2], c[3], c[4], c[5]);
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP multiband compressor kernel.
///
/// Contains ONLY mutable state required for audio processing. No `SmoothedParam`,
/// no atomics, no platform awareness.
///
/// # DSP State
///
/// - `low_lp` / `low_lp2`: cascaded `[Biquad; 2]` lowpass for low band (LR2, L+R).
/// - `mid_hp` / `mid_hp2`: cascaded highpass biquads at xover_low.
/// - `mid_lp` / `mid_lp2`: cascaded lowpass biquads at xover_high.
/// - `high_hp` / `high_hp2`: cascaded highpass biquads for high band (LR2, L+R).
/// - `low_env` / `mid_env` / `high_env`: per-band envelope followers (linked-stereo).
/// - `xover_low_cache` / `xover_high_cache`: `[f32; 12]` change-detectors for crossover
///   coefficients. Each entry stores `[LP × 6, HP × 6]` to avoid per-sample trig calls.
///
/// # Linkwitz-Riley Crossover Topology
///
/// All four filter pairs use the same topology: two consecutive biquad stages
/// at the same frequency with Butterworth Q (1/√2). This cascades to −12 dB/oct
/// per slope and achieves flat magnitude sum at the crossover frequency.
pub struct MultibandCompKernel {
    sample_rate: f32,

    // ── Low band: LR2 lowpass at xover_low, [L, R] ────────────────────────
    low_lp: [Biquad; 2],
    low_lp2: [Biquad; 2],

    // ── Mid band: HP at xover_low then LP at xover_high, [L, R] ───────────
    mid_hp: [Biquad; 2],
    mid_hp2: [Biquad; 2],
    mid_lp: [Biquad; 2],
    mid_lp2: [Biquad; 2],

    // ── High band: LR2 highpass at xover_high, [L, R] ─────────────────────
    high_hp: [Biquad; 2],
    high_hp2: [Biquad; 2],

    // ── Per-band linked-stereo envelope followers ──────────────────────────
    low_env: EnvelopeFollower,
    mid_env: EnvelopeFollower,
    high_env: EnvelopeFollower,

    // ── Coefficient change-detectors ─────────────────────────────────────────
    /// Cached `[LP_b0,LP_b1,LP_b2,LP_a0,LP_a1,LP_a2, HP_b0,...HP_a2]` (12 values)
    /// for the xover_low crossover. Stores both LP and HP coefficients.
    xover_low_cache: Cached<[f32; 12]>,
    /// Cached `[LP..6, HP..6]` for the xover_high crossover.
    xover_high_cache: Cached<[f32; 12]>,
}

impl MultibandCompKernel {
    /// Create a new multiband compressor kernel at `sample_rate`.
    ///
    /// Initializes crossover filters at their default frequencies (250 Hz and
    /// 3000 Hz). All three bands start with default envelope timing
    /// (10 ms attack, 100 ms release).
    pub fn new(sample_rate: f32) -> Self {
        let defaults = MultibandCompParams::default();

        let lp_lo = Self::make_lp_coeffs(defaults.xover_low_hz, sample_rate);
        let hp_lo = Self::make_hp_coeffs(defaults.xover_low_hz, sample_rate);
        let lp_hi = Self::make_lp_coeffs(defaults.xover_high_hz, sample_rate);
        let hp_hi = Self::make_hp_coeffs(defaults.xover_high_hz, sample_rate);

        let make_biquad_pair = |c: &[f32; 6]| -> [Biquad; 2] {
            core::array::from_fn(|_| {
                let mut b = Biquad::new();
                b.set_coefficients(c[0], c[1], c[2], c[3], c[4], c[5]);
                b
            })
        };

        let make_env = || {
            let mut env = EnvelopeFollower::new(sample_rate);
            env.set_attack_ms(BAND_ATTACK_MS);
            env.set_release_ms(BAND_RELEASE_MS);
            env
        };

        // Pack LP+HP into a single 12-element cache value to avoid redundant trig.
        let lo_both = Self::make_lp_hp(defaults.xover_low_hz, sample_rate);
        let hi_both = Self::make_lp_hp(defaults.xover_high_hz, sample_rate);

        let mut xover_low_cache = Cached::new(lo_both, 1);
        xover_low_cache.update(&[defaults.xover_low_hz], 0.5, |inputs| {
            Self::make_lp_hp(inputs[0].clamp(100.0, 1000.0), sample_rate)
        });
        let mut xover_high_cache = Cached::new(hi_both, 1);
        xover_high_cache.update(&[defaults.xover_high_hz], 0.5, |inputs| {
            Self::make_lp_hp(inputs[0].clamp(1000.0, 10000.0), sample_rate)
        });

        Self {
            sample_rate,
            low_lp: make_biquad_pair(&lp_lo),
            low_lp2: make_biquad_pair(&lp_lo),
            mid_hp: make_biquad_pair(&hp_lo),
            mid_hp2: make_biquad_pair(&hp_lo),
            mid_lp: make_biquad_pair(&lp_hi),
            mid_lp2: make_biquad_pair(&lp_hi),
            high_hp: make_biquad_pair(&hp_hi),
            high_hp2: make_biquad_pair(&hp_hi),
            low_env: make_env(),
            mid_env: make_env(),
            high_env: make_env(),
            xover_low_cache,
            xover_high_cache,
        }
    }

    /// Compute 2nd-order Butterworth lowpass coefficients as `[b0,b1,b2,a0,a1,a2]`.
    #[inline]
    fn make_lp_coeffs(freq_hz: f32, sample_rate: f32) -> [f32; 6] {
        let (b0, b1, b2, a0, a1, a2) = lowpass_coefficients(freq_hz, LR_Q, sample_rate);
        [b0, b1, b2, a0, a1, a2]
    }

    /// Compute 2nd-order Butterworth highpass coefficients as `[b0,b1,b2,a0,a1,a2]`.
    #[inline]
    fn make_hp_coeffs(freq_hz: f32, sample_rate: f32) -> [f32; 6] {
        let (b0, b1, b2, a0, a1, a2) = highpass_coefficients(freq_hz, LR_Q, sample_rate);
        [b0, b1, b2, a0, a1, a2]
    }

    /// Compute both LP and HP coefficients at `freq_hz`, packed as `[LP × 6, HP × 6]`.
    ///
    /// Packing both into one cache entry avoids running `make_hp_coeffs` outside
    /// the cache (which would call `sinf`/`cosf` every sample even when stable).
    #[inline]
    fn make_lp_hp(freq_hz: f32, sample_rate: f32) -> [f32; 12] {
        let lp = Self::make_lp_coeffs(freq_hz, sample_rate);
        let hp = Self::make_hp_coeffs(freq_hz, sample_rate);
        [
            lp[0], lp[1], lp[2], lp[3], lp[4], lp[5], hp[0], hp[1], hp[2], hp[3], hp[4], hp[5],
        ]
    }

    /// Flush cached crossover coefficients into the biquad filter banks.
    ///
    /// Only runs the (expensive) `lowpass_coefficients`/`highpass_coefficients`
    /// trig calls when the crossover frequency has actually changed. Both LP and
    /// HP coefficients are packed in a single `[f32; 12]` cache entry per crossover.
    #[inline]
    fn update_crossovers(&mut self, params: &MultibandCompParams) {
        let sr = self.sample_rate;

        // xover_low: low band LP + mid/high HP
        let lo = *self
            .xover_low_cache
            .update(&[params.xover_low_hz], 0.5, |inputs| {
                Self::make_lp_hp(inputs[0].clamp(100.0, 1000.0), sr)
            });
        let lp_lo = [lo[0], lo[1], lo[2], lo[3], lo[4], lo[5]];
        let hp_lo = [lo[6], lo[7], lo[8], lo[9], lo[10], lo[11]];
        set_stereo_biquad(&mut self.low_lp, &lp_lo);
        set_stereo_biquad(&mut self.low_lp2, &lp_lo);
        set_stereo_biquad(&mut self.mid_hp, &hp_lo);
        set_stereo_biquad(&mut self.mid_hp2, &hp_lo);

        // xover_high: mid band LP + high band HP
        let hi = *self
            .xover_high_cache
            .update(&[params.xover_high_hz], 0.5, |inputs| {
                Self::make_lp_hp(inputs[0].clamp(1000.0, 10000.0), sr)
            });
        let lp_hi = [hi[0], hi[1], hi[2], hi[3], hi[4], hi[5]];
        let hp_hi = [hi[6], hi[7], hi[8], hi[9], hi[10], hi[11]];
        set_stereo_biquad(&mut self.mid_lp, &lp_hi);
        set_stereo_biquad(&mut self.mid_lp2, &lp_hi);
        set_stereo_biquad(&mut self.high_hp, &hp_hi);
        set_stereo_biquad(&mut self.high_hp2, &hp_hi);
    }

    /// Compute hard-knee gain reduction linear multiplier.
    ///
    /// Formula: `gr = db_to_linear(-overshoot * (1 - 1/ratio))` when above threshold,
    /// else 1.0. `envelope_abs` is the pre-computed absolute amplitude of the detection signal.
    ///
    /// Reference: Giannoulis et al. (2012).
    #[inline]
    fn gain_computer(
        env: &mut EnvelopeFollower,
        detection: f32,
        thresh_db: f32,
        ratio: f32,
    ) -> f32 {
        let amplitude = env.process(libm::fabsf(detection));
        let amplitude = flush_denormal(amplitude).max(1e-10);
        let envelope_db = fast_linear_to_db(amplitude);
        let overshoot = envelope_db - thresh_db;
        if overshoot > 0.0 {
            let ratio_safe = ratio.max(1.0);
            let gr_db = -overshoot * (1.0 - 1.0 / ratio_safe);
            fast_db_to_linear(gr_db)
        } else {
            1.0
        }
    }
}

impl DspKernel for MultibandCompKernel {
    type Params = MultibandCompParams;

    /// Process a mono sample through the multiband compressor.
    ///
    /// Routes the input through the three crossover bands, compresses each
    /// independently, applies per-band makeup gain, sums, and scales by output.
    fn process(&mut self, input: f32, params: &MultibandCompParams) -> f32 {
        self.update_crossovers(params);

        // Band splitting (mono — only channel index 0)
        let low = self.low_lp2[0].process(self.low_lp[0].process(input));
        let mid_hp_sig = self.mid_hp2[0].process(self.mid_hp[0].process(input));
        let mid = self.mid_lp2[0].process(self.mid_lp[0].process(mid_hp_sig));
        let high = self.high_hp2[0].process(self.high_hp[0].process(input));

        // Gain computation
        let low_gr = Self::gain_computer(
            &mut self.low_env,
            low,
            params.low_thresh_db,
            params.low_ratio,
        );
        let mid_gr = Self::gain_computer(
            &mut self.mid_env,
            mid,
            params.mid_thresh_db,
            params.mid_ratio,
        );
        let high_gr = Self::gain_computer(
            &mut self.high_env,
            high,
            params.high_thresh_db,
            params.high_ratio,
        );

        let out = low * low_gr * fast_db_to_linear(params.low_gain_db)
            + mid * mid_gr * fast_db_to_linear(params.mid_gain_db)
            + high * high_gr * fast_db_to_linear(params.high_gain_db);

        flush_denormal(out) * fast_db_to_linear(params.output_db)
    }

    /// Process a stereo sample pair through the multiband compressor.
    ///
    /// Linked-stereo detection: each band uses the maximum of `|L|` and `|R|`
    /// band amplitudes so both channels receive the same gain reduction. This
    /// preserves the stereo image under heavy compression.
    fn process_stereo(
        &mut self,
        left: f32,
        right: f32,
        params: &MultibandCompParams,
    ) -> (f32, f32) {
        self.update_crossovers(params);

        // Band splitting — L (index 0) and R (index 1)
        let low_l = self.low_lp2[0].process(self.low_lp[0].process(left));
        let low_r = self.low_lp2[1].process(self.low_lp[1].process(right));

        let mid_hp_l = self.mid_hp2[0].process(self.mid_hp[0].process(left));
        let mid_hp_r = self.mid_hp2[1].process(self.mid_hp[1].process(right));
        let mid_l = self.mid_lp2[0].process(self.mid_lp[0].process(mid_hp_l));
        let mid_r = self.mid_lp2[1].process(self.mid_lp[1].process(mid_hp_r));

        let high_l = self.high_hp2[0].process(self.high_hp[0].process(left));
        let high_r = self.high_hp2[1].process(self.high_hp[1].process(right));

        // Linked-stereo gain: detect on channel with higher amplitude per band
        let detect_low = if libm::fabsf(low_l) >= libm::fabsf(low_r) {
            low_l
        } else {
            low_r
        };
        let detect_mid = if libm::fabsf(mid_l) >= libm::fabsf(mid_r) {
            mid_l
        } else {
            mid_r
        };
        let detect_high = if libm::fabsf(high_l) >= libm::fabsf(high_r) {
            high_l
        } else {
            high_r
        };

        let low_gr = Self::gain_computer(
            &mut self.low_env,
            detect_low,
            params.low_thresh_db,
            params.low_ratio,
        );
        let mid_gr = Self::gain_computer(
            &mut self.mid_env,
            detect_mid,
            params.mid_thresh_db,
            params.mid_ratio,
        );
        let high_gr = Self::gain_computer(
            &mut self.high_env,
            detect_high,
            params.high_thresh_db,
            params.high_ratio,
        );

        let low_gain = fast_db_to_linear(params.low_gain_db);
        let mid_gain = fast_db_to_linear(params.mid_gain_db);
        let high_gain = fast_db_to_linear(params.high_gain_db);
        let output_gain = fast_db_to_linear(params.output_db);

        let out_l = flush_denormal(
            (low_l * low_gr * low_gain + mid_l * mid_gr * mid_gain + high_l * high_gr * high_gain)
                * output_gain,
        );
        let out_r = flush_denormal(
            (low_r * low_gr * low_gain + mid_r * mid_gr * mid_gain + high_r * high_gr * high_gain)
                * output_gain,
        );

        (out_l, out_r)
    }

    fn reset(&mut self) {
        for b in &mut self.low_lp {
            b.clear();
        }
        for b in &mut self.low_lp2 {
            b.clear();
        }
        for b in &mut self.mid_hp {
            b.clear();
        }
        for b in &mut self.mid_hp2 {
            b.clear();
        }
        for b in &mut self.mid_lp {
            b.clear();
        }
        for b in &mut self.mid_lp2 {
            b.clear();
        }
        for b in &mut self.high_hp {
            b.clear();
        }
        for b in &mut self.high_hp2 {
            b.clear();
        }
        self.low_env.reset();
        self.mid_env.reset();
        self.high_env.reset();
        self.xover_low_cache.invalidate();
        self.xover_high_cache.invalidate();
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.low_env.set_sample_rate(sample_rate);
        self.mid_env.set_sample_rate(sample_rate);
        self.high_env.set_sample_rate(sample_rate);
        self.xover_low_cache.invalidate();
        self.xover_high_cache.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_core::ParameterInfo;
    use sonido_core::kernel::KernelAdapter;

    /// All outputs must be finite — no NaN or Inf under any input.
    #[test]
    fn finite_output() {
        let mut kernel = MultibandCompKernel::new(48000.0);
        let params = MultibandCompParams::default();

        for i in 0..4096 {
            let t = i as f32 / 48000.0;
            let s = 0.8 * libm::sinf(2.0 * core::f32::consts::PI * 440.0 * t);
            let (l, r) = kernel.process_stereo(s, -s, &params);
            assert!(l.is_finite(), "Left output non-finite at sample {i}: {l}");
            assert!(r.is_finite(), "Right output non-finite at sample {i}: {r}");
        }
    }

    /// Compression must reduce gain on a loud signal above threshold.
    ///
    /// Feeds a 1 kHz sine at −6 dBFS with threshold at −20 dB and ratio 8:1.
    /// After warm-up, the compressed output must be quieter than the raw input.
    #[test]
    fn compression_reduces_loud_signal() {
        let sr = 48000.0_f32;
        let mut kernel = MultibandCompKernel::new(sr);

        // All bands set tight: -20dB threshold, ratio 8
        let params = MultibandCompParams {
            low_thresh_db: -20.0,
            low_ratio: 8.0,
            mid_thresh_db: -20.0,
            mid_ratio: 8.0,
            high_thresh_db: -20.0,
            high_ratio: 8.0,
            ..MultibandCompParams::default()
        };

        // Input: 1 kHz sine at ~−6 dBFS (amplitude 0.5 ≈ −6 dBFS, 14 dB above threshold)
        let freq = 1000.0_f32;
        let amplitude = 0.5_f32;

        // Warm up for 200 ms so envelopes settle
        for i in 0..(sr as usize / 5) {
            let t = i as f32 / sr;
            let s = amplitude * libm::sinf(2.0 * core::f32::consts::PI * freq * t);
            kernel.process(s, &params);
        }

        // Measure output peak over one cycle
        let cycle = (sr / freq) as usize;
        let mut peak_out = 0.0_f32;
        let base = sr as usize / 5;
        for i in 0..cycle {
            let t = (base + i) as f32 / sr;
            let s = amplitude * libm::sinf(2.0 * core::f32::consts::PI * freq * t);
            let out = kernel.process(s, &params);
            peak_out = peak_out.max(libm::fabsf(out));
        }

        // Output peak must be less than input amplitude (compression applied)
        assert!(
            peak_out < amplitude,
            "Compression had no effect: peak_out={peak_out:.4} >= amplitude={amplitude:.4}"
        );
    }

    /// Silence in must produce silence out.
    #[test]
    fn silence_in_silence_out() {
        let mut kernel = MultibandCompKernel::new(44100.0);
        let params = MultibandCompParams::default();
        for _ in 0..1024 {
            let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
            assert!(l.abs() < 1e-6, "Expected silence, got left={l}");
            assert!(r.abs() < 1e-6, "Expected silence, got right={r}");
        }
    }

    /// KernelAdapter wraps the kernel and exposes the correct parameter count.
    #[test]
    fn adapter_param_count() {
        let kernel = MultibandCompKernel::new(48000.0);
        let adapter = KernelAdapter::new(kernel, 48000.0);
        assert_eq!(adapter.param_count(), 12);
        for i in 0..12 {
            assert!(adapter.param_info(i).is_some(), "Missing param {i}");
        }
        assert!(adapter.param_info(12).is_none());
    }

    /// ParamId base must be 2600.
    #[test]
    fn param_ids_start_at_2600() {
        assert_eq!(
            MultibandCompParams::descriptor(0).unwrap().id,
            ParamId(2600)
        );
        assert_eq!(
            MultibandCompParams::descriptor(11).unwrap().id,
            ParamId(2611)
        );
    }
}
