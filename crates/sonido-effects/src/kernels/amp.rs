//! Amp simulator kernel — dual gain stage + interactive tone stack + sag.
//!
//! `AmpKernel` models a valve guitar amplifier signal chain using the
//! [`GainStage`] and [`ToneStack`] DSP sub-chains from `sonido-core`.
//!
//! # Signal Flow
//!
//! ```text
//! Input
//!   │
//!   ├─[× gain_linear]─────────────────────────────────────────────────┐
//!   │                                                                  ▼
//!   └─────────────────────────────────── ① Preamp GainStage (soft clip ADAA)
//!                                        │
//!                              ② Optional bright_filter (high shelf +3dB @ 2kHz)
//!                                        │
//!                              ③ ToneStack (bass / mid / treble)
//!                                        │
//!                              ④ Presence shelf (high shelf @ 4kHz)
//!                                        │
//!                              ⑤ Sag envelope → modulate power_stage pre-gain
//!                                        │
//!                              ⑥ Power GainStage (asymmetric clip ADAA)
//!                                        │
//!                              ⑦ × master_linear × output_linear
//!                                        │
//!                                      Output (stereo / dual-mono)
//! ```
//!
//! # Deployment
//!
//! ```rust,ignore
//! use sonido_core::kernel::KernelAdapter;
//! use sonido_effects::kernels::{AmpKernel, AmpParams};
//!
//! // Desktop / Plugin (via adapter — handles parameter smoothing)
//! let adapter = KernelAdapter::new(AmpKernel::new(48000.0), 48000.0);
//!
//! // Embedded / Daisy Seed (direct — no smoothing)
//! let mut kernel = AmpKernel::new(48000.0);
//! let params = AmpParams::from_knobs(gain, bass, mid, treble, presence, sag, bright, master, output);
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```
//!
//! # DSP Theory
//!
//! ## Preamp Stage
//!
//! A soft-clip ADAA stage models the input triode. `params.gain_pct` (0–100%)
//! maps to 0–40 dB drive. ADAA minimizes aliasing at the cost of one extra
//! antiderivative evaluation per sample.
//!
//! Reference: Parker et al., DAFx-2016; Zölzer, "DAFX" Chapter 5.
//!
//! ## Tone Stack
//!
//! Three cascaded peaking EQ biquads (100 Hz / 800 Hz / 3200 Hz) model the
//! passive RC tone network of classic British and American amplifiers.
//!
//! Reference: Bristow-Johnson, "Audio EQ Cookbook".
//!
//! ## Sag
//!
//! Power-supply sag is modelled by an envelope follower on the preamp output.
//! As signal level increases, the envelope rises → power stage pre-gain is
//! reduced, emulating the dynamic compression of an under-spec PSU.
//!
//! `sag_pct = 0` → no sag (solid-state character), `sag_pct = 100` → heavy sag.
//!
//! ## Power Stage
//!
//! An asymmetric-clip ADAA stage models the output pentodes. Asymmetric
//! clipping produces even harmonics (warm, tube-like character). The pre-gain
//! is modulated by the sag envelope.
//!
//! ## Presence / Bright
//!
//! The presence shelf (high shelf at 4 kHz) emulates negative feedback loop
//! frequency response. The bright switch adds a +3 dB high shelf at 2 kHz to
//! emulate the capacitor bypass bright cap of vintage amps.

use core::f32::consts::PI;
use libm::{cosf, powf, sinf, sqrtf};
use sonido_core::biquad::Biquad;
use sonido_core::dsp::{GainStage, ToneStack};
use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::math::{
    asymmetric_clip, asymmetric_clip_ad, db_to_linear, soft_clip, soft_clip_ad,
};
use sonido_core::{Cached, EnvelopeFollower, ParamDescriptor, ParamFlags, ParamId, ParamUnit};

// ── ADAA function-pointer aliases ──────────────────────────────────────────

fn soft_clip_fn(x: f32) -> f32 {
    soft_clip(x)
}

fn soft_clip_ad_fn(x: f32) -> f32 {
    soft_clip_ad(x)
}

fn asym_clip_fn(x: f32) -> f32 {
    asymmetric_clip(x)
}

fn asym_clip_ad_fn(x: f32) -> f32 {
    asymmetric_clip_ad(x)
}

type AmpAdaa = GainStage<fn(f32) -> f32, fn(f32) -> f32>;

// ── High-shelf biquad coefficient calculation ──────────────────────────────

/// Compute high-shelf biquad coefficients using the RBJ "Audio EQ Cookbook"
/// high-shelf formula.
///
/// # Arguments
///
/// * `frequency` — Shelf transition (-3 dB) frequency in Hz
/// * `gain_db` — Shelf gain in decibels (positive = boost, negative = cut)
/// * `slope` — Shelf slope (1.0 = maximally steep, <1.0 = gentler)
/// * `sample_rate` — Sample rate in Hz
///
/// # Returns
///
/// `(b0, b1, b2, a0, a1, a2)` — unnormalized biquad coefficients
///
/// # Reference
///
/// Bristow-Johnson, "Audio EQ Cookbook" — high shelf filter.
fn high_shelf_coefficients(
    frequency: f32,
    gain_db: f32,
    slope: f32,
    sample_rate: f32,
) -> (f32, f32, f32, f32, f32, f32) {
    let a = powf(10.0, gain_db / 40.0); // sqrt(10^(dB/20))
    let omega = 2.0 * PI * frequency / sample_rate;
    let cos_w = cosf(omega);
    let sin_w = sinf(omega);
    // alpha = sin(w)/2 * sqrt((A + 1/A)*(1/S - 1) + 2)
    let alpha = (sin_w / 2.0) * sqrtf((a + 1.0 / a) * (1.0 / slope - 1.0) + 2.0);

    let b0 = a * ((a + 1.0) + (a - 1.0) * cos_w + 2.0 * sqrtf(a) * alpha);
    let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w);
    let b2 = a * ((a + 1.0) + (a - 1.0) * cos_w - 2.0 * sqrtf(a) * alpha);
    let a0 = (a + 1.0) - (a - 1.0) * cos_w + 2.0 * sqrtf(a) * alpha;
    let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w);
    let a2 = (a + 1.0) - (a - 1.0) * cos_w - 2.0 * sqrtf(a) * alpha;

    (b0, b1, b2, a0, a1, a2)
}

// ── Constants ───────────────────────────────────────────────────────────────

/// Maximum preamp drive in dB (maps from gain_pct = 100%).
const MAX_PREAMP_DRIVE_DB: f32 = 40.0;
/// Minimum preamp drive in dB (maps from gain_pct = 0%).
const MIN_PREAMP_DRIVE_DB: f32 = 0.0;

/// Frequency of the presence high-shelf filter in Hz.
const PRESENCE_HZ: f32 = 4000.0;
/// Maximum presence shelf gain in dB (maps from presence_pct = 100%).
const PRESENCE_MAX_DB: f32 = 12.0;

/// Frequency of the bright-switch high-shelf filter in Hz.
const BRIGHT_HZ: f32 = 2000.0;
/// Bright switch gain in dB.
const BRIGHT_GAIN_DB: f32 = 3.0;

/// Shelf slope parameter for both presence and bright filters.
const SHELF_SLOPE: f32 = 1.0;

/// Maximum power-stage pre-gain reduction from sag (fraction, 0.0–1.0).
///
/// At `sag_pct = 100%` and full envelope, pre-gain is reduced by up to this
/// fraction, emulating a 30% PSU sag.
const SAG_MAX_GAIN_REDUCTION: f32 = 0.3;

/// Power stage pre-gain in dB (nominal, before sag modulation).
const POWER_STAGE_NOMINAL_DB: f32 = 12.0;

/// Power stage post-gain in dB (compensating output).
const POWER_STAGE_POST_DB: f32 = -12.0;

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`AmpKernel`].
///
/// All values in **user-facing units**. The kernel converts internally.
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `gain_pct` | % | 0–100 | 50.0 |
/// | 1 | `bass_pct` | % | 0–100 | 50.0 |
/// | 2 | `mid_pct` | % | 0–100 | 50.0 |
/// | 3 | `treble_pct` | % | 0–100 | 50.0 |
/// | 4 | `presence_pct` | % | 0–100 | 50.0 |
/// | 5 | `sag_pct` | % | 0–100 | 30.0 |
/// | 6 | `bright` | bool | 0–1 (STEPPED) | 0.0 |
/// | 7 | `master_db` | dB | −60–0 | −6.0 |
/// | 8 | `output_db` | dB | −60–6 | 0.0 |
#[derive(Debug, Clone, Copy)]
pub struct AmpParams {
    /// Input gain percentage (0–100%). Maps to 0–40 dB preamp drive.
    pub gain_pct: f32,
    /// Bass tone control (0–100%). 50% = flat, 0% = max cut, 100% = max boost.
    pub bass_pct: f32,
    /// Mid tone control (0–100%).
    pub mid_pct: f32,
    /// Treble tone control (0–100%).
    pub treble_pct: f32,
    /// Presence (high-shelf at 4 kHz) percentage (0–100%). 50% = flat.
    pub presence_pct: f32,
    /// Power-supply sag amount (0–100%). 0% = no sag, 100% = heavy sag.
    pub sag_pct: f32,
    /// Bright switch: 0.0 = off, 1.0 = on (+3 dB high shelf at 2 kHz).
    pub bright: f32,
    /// Master volume in dB (range: −60–0 dB).
    pub master_db: f32,
    /// Output level trim in dB (range: −60–+6 dB).
    pub output_db: f32,
}

impl Default for AmpParams {
    fn default() -> Self {
        Self {
            gain_pct: 50.0,
            bass_pct: 50.0,
            mid_pct: 50.0,
            treble_pct: 50.0,
            presence_pct: 50.0,
            sag_pct: 30.0,
            bright: 0.0,
            master_db: -6.0,
            output_db: 0.0,
        }
    }
}

impl AmpParams {
    /// Creates parameters from normalized 0–1 knob readings.
    ///
    /// Curves are derived from [`ParamDescriptor`] — same mapping as GUI and
    /// plugin hosts.
    #[allow(clippy::too_many_arguments)]
    pub fn from_knobs(
        gain: f32,
        bass: f32,
        mid: f32,
        treble: f32,
        presence: f32,
        sag: f32,
        bright: f32,
        master: f32,
        output: f32,
    ) -> Self {
        Self::from_normalized(&[
            gain, bass, mid, treble, presence, sag, bright, master, output,
        ])
    }
}

impl KernelParams for AmpParams {
    const COUNT: usize = 9;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(
                ParamDescriptor::custom("Gain", "Gain", 0.0, 100.0, 50.0)
                    .with_unit(ParamUnit::Percent)
                    .with_step(1.0)
                    .with_id(ParamId(2100), "amp_gain"),
            ),
            1 => Some(
                ParamDescriptor::custom("Bass", "Bass", 0.0, 100.0, 50.0)
                    .with_unit(ParamUnit::Percent)
                    .with_step(1.0)
                    .with_id(ParamId(2101), "amp_bass"),
            ),
            2 => Some(
                ParamDescriptor::custom("Mid", "Mid", 0.0, 100.0, 50.0)
                    .with_unit(ParamUnit::Percent)
                    .with_step(1.0)
                    .with_id(ParamId(2102), "amp_mid"),
            ),
            3 => Some(
                ParamDescriptor::custom("Treble", "Treble", 0.0, 100.0, 50.0)
                    .with_unit(ParamUnit::Percent)
                    .with_step(1.0)
                    .with_id(ParamId(2103), "amp_treble"),
            ),
            4 => Some(
                ParamDescriptor::custom("Presence", "Pres", 0.0, 100.0, 50.0)
                    .with_unit(ParamUnit::Percent)
                    .with_step(1.0)
                    .with_id(ParamId(2104), "amp_presence"),
            ),
            5 => Some(
                ParamDescriptor::custom("Sag", "Sag", 0.0, 100.0, 30.0)
                    .with_unit(ParamUnit::Percent)
                    .with_step(1.0)
                    .with_id(ParamId(2105), "amp_sag"),
            ),
            6 => Some(
                ParamDescriptor::custom("Bright", "Bright", 0.0, 1.0, 0.0)
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step(1.0)
                    .with_step_labels(&["Off", "On"])
                    .with_id(ParamId(2106), "amp_bright"),
            ),
            7 => Some(
                ParamDescriptor::gain_db("Master", "Master", -60.0, 0.0, -6.0)
                    .with_id(ParamId(2107), "amp_master"),
            ),
            8 => Some(
                sonido_core::gain::output_param_descriptor().with_id(ParamId(2108), "amp_output"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Fast,     // gain — fast response for pick feel
            1 => SmoothingStyle::Slow,     // bass — filter coefficient
            2 => SmoothingStyle::Slow,     // mid — filter coefficient
            3 => SmoothingStyle::Slow,     // treble — filter coefficient
            4 => SmoothingStyle::Slow,     // presence — filter coefficient
            5 => SmoothingStyle::Standard, // sag
            6 => SmoothingStyle::None,     // bright — discrete switch, snap
            7 => SmoothingStyle::Fast,     // master
            8 => SmoothingStyle::Fast,     // output
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.gain_pct,
            1 => self.bass_pct,
            2 => self.mid_pct,
            3 => self.treble_pct,
            4 => self.presence_pct,
            5 => self.sag_pct,
            6 => self.bright,
            7 => self.master_db,
            8 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.gain_pct = value,
            1 => self.bass_pct = value,
            2 => self.mid_pct = value,
            3 => self.treble_pct = value,
            4 => self.presence_pct = value,
            5 => self.sag_pct = value,
            6 => self.bright = value,
            7 => self.master_db = value,
            8 => self.output_db = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP amp simulator kernel.
///
/// Contains ONLY the mutable DSP state:
/// - Two gain stages (preamp + power amp) via [`GainStage`]
/// - [`ToneStack`] (bass/mid/treble)
/// - Presence high-shelf biquad (L/R)
/// - Bright-switch high-shelf biquad (L/R)
/// - [`EnvelopeFollower`] for power-supply sag simulation
/// - Coefficient caches for presence and bright filters
///
/// No `SmoothedParam`, no `AtomicU32`, no platform awareness.
pub struct AmpKernel {
    sample_rate: f32,

    /// Preamp gain stage — soft_clip ADAA, models input triode.
    preamp_stage_l: AmpAdaa,
    /// Preamp gain stage — right channel (dual-mono).
    preamp_stage_r: AmpAdaa,

    /// 3-band interactive tone stack (shared coefficient state, L processed first).
    tone_stack_l: ToneStack,
    /// 3-band interactive tone stack for right channel.
    tone_stack_r: ToneStack,

    /// Presence high-shelf biquad — left channel.
    presence_l: Biquad,
    /// Presence high-shelf biquad — right channel.
    presence_r: Biquad,

    /// Power stage gain — asymmetric_clip ADAA, models output pentode.
    power_stage_l: AmpAdaa,
    /// Power stage — right channel.
    power_stage_r: AmpAdaa,

    /// Envelope follower for sag modulation (peak detection, 10 ms attack, 150 ms release).
    sag_envelope: EnvelopeFollower,

    /// Bright-switch high-shelf biquad — left channel.
    bright_filter_l: Biquad,
    /// Bright-switch high-shelf biquad — right channel.
    bright_filter_r: Biquad,

    /// Cached presence biquad coefficients, keyed on `presence_pct`.
    presence_cache: Cached<[f32; 6]>,

    /// Cached bright biquad coefficients — fixed at construction (only recalculates on
    /// sample-rate change via `set_sample_rate` → invalidate).
    bright_cache: Cached<[f32; 6]>,
}

impl AmpKernel {
    /// Create a new amp simulator kernel initialized for `sample_rate`.
    pub fn new(sample_rate: f32) -> Self {
        let preamp_stage_l: AmpAdaa = GainStage::new(
            soft_clip_fn as fn(f32) -> f32,
            soft_clip_ad_fn as fn(f32) -> f32,
        );
        let preamp_stage_r: AmpAdaa = GainStage::new(
            soft_clip_fn as fn(f32) -> f32,
            soft_clip_ad_fn as fn(f32) -> f32,
        );

        let mut power_stage_l: AmpAdaa = GainStage::new(
            asym_clip_fn as fn(f32) -> f32,
            asym_clip_ad_fn as fn(f32) -> f32,
        );
        let mut power_stage_r: AmpAdaa = GainStage::new(
            asym_clip_fn as fn(f32) -> f32,
            asym_clip_ad_fn as fn(f32) -> f32,
        );
        power_stage_l.set_pre_gain_db(POWER_STAGE_NOMINAL_DB);
        power_stage_l.set_post_gain_db(POWER_STAGE_POST_DB);
        power_stage_r.set_pre_gain_db(POWER_STAGE_NOMINAL_DB);
        power_stage_r.set_post_gain_db(POWER_STAGE_POST_DB);

        // Presence filter (flat at 50% = 0 dB)
        let pres_db = 0.0;
        let pres_coeff = {
            let (b0, b1, b2, a0, a1, a2) =
                high_shelf_coefficients(PRESENCE_HZ, pres_db, SHELF_SLOPE, sample_rate);
            [b0, b1, b2, a0, a1, a2]
        };
        let mut presence_l = Biquad::new();
        let mut presence_r = Biquad::new();
        presence_l.set_coefficients(
            pres_coeff[0],
            pres_coeff[1],
            pres_coeff[2],
            pres_coeff[3],
            pres_coeff[4],
            pres_coeff[5],
        );
        presence_r.set_coefficients(
            pres_coeff[0],
            pres_coeff[1],
            pres_coeff[2],
            pres_coeff[3],
            pres_coeff[4],
            pres_coeff[5],
        );
        let mut presence_cache = Cached::new(pres_coeff, 1);
        presence_cache.update(&[pres_db], 1e-4, |_| pres_coeff);

        // Bright filter (fixed gain)
        let bright_coeff = {
            let (b0, b1, b2, a0, a1, a2) =
                high_shelf_coefficients(BRIGHT_HZ, BRIGHT_GAIN_DB, SHELF_SLOPE, sample_rate);
            [b0, b1, b2, a0, a1, a2]
        };
        let mut bright_filter_l = Biquad::new();
        let mut bright_filter_r = Biquad::new();
        bright_filter_l.set_coefficients(
            bright_coeff[0],
            bright_coeff[1],
            bright_coeff[2],
            bright_coeff[3],
            bright_coeff[4],
            bright_coeff[5],
        );
        bright_filter_r.set_coefficients(
            bright_coeff[0],
            bright_coeff[1],
            bright_coeff[2],
            bright_coeff[3],
            bright_coeff[4],
            bright_coeff[5],
        );
        let mut bright_cache = Cached::new(bright_coeff, 1);
        bright_cache.update(&[BRIGHT_GAIN_DB], 1e-4, |_| bright_coeff);

        let mut sag_envelope = EnvelopeFollower::new(sample_rate);
        sag_envelope.set_attack_ms(10.0);
        sag_envelope.set_release_ms(150.0);

        Self {
            sample_rate,
            preamp_stage_l,
            preamp_stage_r,
            tone_stack_l: ToneStack::new(sample_rate),
            tone_stack_r: ToneStack::new(sample_rate),
            presence_l,
            presence_r,
            power_stage_l,
            power_stage_r,
            sag_envelope,
            bright_filter_l,
            bright_filter_r,
            presence_cache,
            bright_cache,
        }
    }

    /// Apply one channel of the full amp signal chain.
    ///
    /// Shared mutable state (sag_envelope) must be updated before calling this.
    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn process_channel(
        input: f32,
        preamp: &mut AmpAdaa,
        bright_filter: &mut Biquad,
        tone_stack: &mut ToneStack,
        presence: &mut Biquad,
        power_stage: &mut AmpAdaa,
        bright_on: bool,
        gain_linear: f32,
        master_linear: f32,
        output_linear: f32,
    ) -> f32 {
        // ① Preamp gain stage (soft clip ADAA)
        preamp.set_pre_gain_linear(gain_linear);
        let after_preamp = preamp.process(input);

        // ② Bright switch (optional high shelf)
        let after_bright = if bright_on {
            bright_filter.process(after_preamp)
        } else {
            after_preamp
        };

        // ③ Tone stack
        let after_tone = tone_stack.process(after_bright);

        // ④ Presence shelf
        let after_presence = presence.process(after_tone);

        // ⑥ Power stage (pre-gain is set externally by caller with sag)
        let after_power = power_stage.process(after_presence);

        // ⑦ Master × output
        after_power * master_linear * output_linear
    }
}

impl DspKernel for AmpKernel {
    type Params = AmpParams;

    fn process_stereo(&mut self, left: f32, right: f32, params: &AmpParams) -> (f32, f32) {
        // ── Coefficient update: presence shelf ──────────────────────────────
        let presence_db = (params.presence_pct / 100.0 * 2.0 - 1.0) * PRESENCE_MAX_DB;
        let sr = self.sample_rate;

        let pres_coeff = *self.presence_cache.update(&[presence_db], 1e-4, |inputs| {
            let (b0, b1, b2, a0, a1, a2) =
                high_shelf_coefficients(PRESENCE_HZ, inputs[0], SHELF_SLOPE, sr);
            [b0, b1, b2, a0, a1, a2]
        });
        self.presence_l.set_coefficients(
            pres_coeff[0],
            pres_coeff[1],
            pres_coeff[2],
            pres_coeff[3],
            pres_coeff[4],
            pres_coeff[5],
        );
        self.presence_r.set_coefficients(
            pres_coeff[0],
            pres_coeff[1],
            pres_coeff[2],
            pres_coeff[3],
            pres_coeff[4],
            pres_coeff[5],
        );

        // ── Unit conversion ──────────────────────────────────────────────────
        let gain_db = params.gain_pct / 100.0 * (MAX_PREAMP_DRIVE_DB - MIN_PREAMP_DRIVE_DB)
            + MIN_PREAMP_DRIVE_DB;
        let gain_linear = db_to_linear(gain_db);
        let master_linear = db_to_linear(params.master_db);
        let output_linear = db_to_linear(params.output_db);
        let bright_on = params.bright >= 0.5;

        // ── Tone stack controls (same for L and R) ───────────────────────────
        let bass = params.bass_pct / 100.0;
        let mid = params.mid_pct / 100.0;
        let treble = params.treble_pct / 100.0;
        self.tone_stack_l.set_controls(bass, mid, treble);
        self.tone_stack_r.set_controls(bass, mid, treble);

        // ── Sag: compute envelope from L signal ──────────────────────────────
        // ⑤ Sag: compute pre-preamp level estimate using the input signal
        let sag_env = self.sag_envelope.process(left.abs().max(right.abs()));
        let sag = params.sag_pct / 100.0 * sag_env * SAG_MAX_GAIN_REDUCTION;
        // Power stage pre-gain is reduced by sag amount
        let power_pre_db = POWER_STAGE_NOMINAL_DB * (1.0 - sag);
        self.power_stage_l.set_pre_gain_db(power_pre_db);
        self.power_stage_r.set_pre_gain_db(power_pre_db);

        // ── Process left channel ──────────────────────────────────────────────
        let out_l = Self::process_channel(
            left,
            &mut self.preamp_stage_l,
            &mut self.bright_filter_l,
            &mut self.tone_stack_l,
            &mut self.presence_l,
            &mut self.power_stage_l,
            bright_on,
            gain_linear,
            master_linear,
            output_linear,
        );

        // ── Process right channel ─────────────────────────────────────────────
        let out_r = Self::process_channel(
            right,
            &mut self.preamp_stage_r,
            &mut self.bright_filter_r,
            &mut self.tone_stack_r,
            &mut self.presence_r,
            &mut self.power_stage_r,
            bright_on,
            gain_linear,
            master_linear,
            output_linear,
        );

        (out_l, out_r)
    }

    fn reset(&mut self) {
        self.preamp_stage_l.reset();
        self.preamp_stage_r.reset();
        self.tone_stack_l.reset();
        self.tone_stack_r.reset();
        self.presence_l.clear();
        self.presence_r.clear();
        self.power_stage_l.reset();
        self.power_stage_r.reset();
        self.sag_envelope.reset();
        self.bright_filter_l.clear();
        self.bright_filter_r.clear();
        self.presence_cache.invalidate();
        self.bright_cache.invalidate();
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.tone_stack_l.set_sample_rate(sample_rate);
        self.tone_stack_r.set_sample_rate(sample_rate);
        self.sag_envelope.set_sample_rate(sample_rate);
        self.presence_cache.invalidate();
        self.bright_cache.invalidate();
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

    #[test]
    fn default_params_are_correct() {
        let p = AmpParams::default();
        assert_eq!(p.gain_pct, 50.0);
        assert_eq!(p.bass_pct, 50.0);
        assert_eq!(p.mid_pct, 50.0);
        assert_eq!(p.treble_pct, 50.0);
        assert_eq!(p.presence_pct, 50.0);
        assert_eq!(p.sag_pct, 30.0);
        assert_eq!(p.bright, 0.0);
        assert_eq!(p.master_db, -6.0);
        assert_eq!(p.output_db, 0.0);
    }

    #[test]
    fn finite_output_default() {
        let mut kernel = AmpKernel::new(48000.0);
        let params = AmpParams::default();
        let (l, r) = kernel.process_stereo(0.5, 0.5, &params);
        assert!(l.is_finite(), "left output must be finite: {l}");
        assert!(r.is_finite(), "right output must be finite: {r}");
    }

    #[test]
    fn silence_in_silence_out() {
        let mut kernel = AmpKernel::new(48000.0);
        let params = AmpParams::default();
        let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
        assert!(l.abs() < 1e-6, "expected silence, got {l}");
        assert!(r.abs() < 1e-6, "expected silence, got {r}");
    }

    #[test]
    fn output_finite_over_sweep() {
        let mut kernel = AmpKernel::new(48000.0);
        let params = AmpParams {
            gain_pct: 80.0,
            sag_pct: 60.0,
            bright: 1.0,
            ..AmpParams::default()
        };
        for i in 0..512 {
            let x = libm::sinf(i as f32 * 0.1) * 0.7;
            let (l, r) = kernel.process_stereo(x, x, &params);
            assert!(l.is_finite(), "L NaN/Inf at sample {i}: {l}");
            assert!(r.is_finite(), "R NaN/Inf at sample {i}: {r}");
        }
    }

    #[test]
    fn bright_switch_changes_output() {
        let sr = 48000.0_f32;
        // Use 8 kHz — well above bright shelf at 2 kHz — for a strong measurable difference.
        let freq = 8000.0_f32;

        let mut k_off = AmpKernel::new(sr);
        let mut k_on = AmpKernel::new(sr);

        let p_off = AmpParams {
            gain_pct: 20.0, // moderate gain so signal stays in linear region
            bright: 0.0,
            master_db: 0.0,
            output_db: 0.0,
            ..AmpParams::default()
        };
        let p_on = AmpParams {
            gain_pct: 20.0,
            bright: 1.0,
            master_db: 0.0,
            output_db: 0.0,
            ..AmpParams::default()
        };

        // Measure RMS over 1000 samples (after 200 warmup) to average out per-sample noise
        let warmup = 200;
        let measure = 1000;

        for i in 0..(warmup + measure) {
            let x = libm::sinf(2.0 * core::f32::consts::PI * freq * i as f32 / sr) * 0.1;
            k_off.process_stereo(x, x, &p_off);
            k_on.process_stereo(x, x, &p_on);
        }

        // Re-run fresh to collect measurement samples
        let mut k_off2 = AmpKernel::new(sr);
        let mut k_on2 = AmpKernel::new(sr);
        let mut rms_off = 0.0f32;
        let mut rms_on = 0.0f32;

        for i in 0..(warmup + measure) {
            let x = libm::sinf(2.0 * core::f32::consts::PI * freq * i as f32 / sr) * 0.1;
            let (l_off, _) = k_off2.process_stereo(x, x, &p_off);
            let (l_on, _) = k_on2.process_stereo(x, x, &p_on);
            if i >= warmup {
                rms_off += l_off * l_off;
                rms_on += l_on * l_on;
            }
        }
        rms_off = libm::sqrtf(rms_off / measure as f32);
        rms_on = libm::sqrtf(rms_on / measure as f32);

        // Bright switch boosts above 2 kHz — at 8 kHz the bright filter should produce
        // noticeably higher RMS (at least 2% relative difference)
        let rel_diff = (rms_on - rms_off).abs() / rms_off.max(1e-6);
        assert!(
            rel_diff > 0.01,
            "bright switch should change RMS at 8kHz by >1%: off={rms_off:.5}, on={rms_on:.5}, rel_diff={rel_diff:.4}"
        );
    }

    #[test]
    fn gain_increases_drive() {
        let input = 0.05; // quiet signal
        let mut k_low = AmpKernel::new(48000.0);
        let mut k_high = AmpKernel::new(48000.0);

        let p_low = AmpParams {
            gain_pct: 10.0,
            master_db: 0.0,
            output_db: 0.0,
            ..AmpParams::default()
        };
        let p_high = AmpParams {
            gain_pct: 90.0,
            master_db: 0.0,
            output_db: 0.0,
            ..AmpParams::default()
        };

        let (l_low, _) = k_low.process_stereo(input, input, &p_low);
        let (l_high, _) = k_high.process_stereo(input, input, &p_high);

        assert!(
            l_high.abs() > l_low.abs(),
            "higher gain should produce more output: low={l_low}, high={l_high}"
        );
    }

    #[test]
    fn reset_clears_all_state() {
        let mut kernel = AmpKernel::new(48000.0);
        let params = AmpParams {
            gain_pct: 80.0,
            ..AmpParams::default()
        };
        for _ in 0..200 {
            kernel.process_stereo(0.5, 0.5, &params);
        }
        kernel.reset();
        let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
        assert!(l.abs() < 1e-5, "after reset, silence in → silence out: {l}");
        assert!(r.abs() < 1e-5, "after reset, silence in → silence out: {r}");
    }

    #[test]
    fn param_count_and_ids() {
        assert_eq!(AmpParams::COUNT, 9);
        assert_eq!(AmpParams::descriptor(0).unwrap().id, ParamId(2100));
        assert_eq!(AmpParams::descriptor(8).unwrap().id, ParamId(2108));
        assert!(AmpParams::descriptor(9).is_none());

        let bright_desc = AmpParams::descriptor(6).unwrap();
        assert!(bright_desc.flags.contains(ParamFlags::STEPPED));
    }

    #[test]
    fn adapter_wraps_kernel() {
        let kernel = AmpKernel::new(48000.0);
        let mut adapter = KernelAdapter::new(kernel, 48000.0);
        adapter.reset();
        let out = adapter.process(0.3);
        assert!(out.is_finite(), "adapter output must be finite: {out}");
    }

    #[test]
    fn adapter_param_count() {
        let kernel = AmpKernel::new(48000.0);
        let adapter = KernelAdapter::new(kernel, 48000.0);
        assert_eq!(adapter.param_count(), 9);
        assert_eq!(adapter.param_info(0).unwrap().name, "Gain");
        assert_eq!(adapter.param_info(7).unwrap().name, "Master");
        assert_eq!(adapter.param_info(8).unwrap().name, "Output");
        assert!(adapter.param_info(9).is_none());
    }
}
