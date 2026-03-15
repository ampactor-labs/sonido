//! Reverb kernel — 8-line Hadamard FDN with early reflections and allpass diffusion.
//!
//! `ReverbKernel` owns DSP state (FDN delay lines, damping filters, allpass
//! diffusers, predelay lines, ER tapped delay). Parameters are received via
//! `&ReverbParams` each sample. Deployed via
//! [`KernelAdapter`](sonido_core::KernelAdapter) for desktop/plugin, or called
//! directly on embedded targets.
//!
//! # Architecture
//!
//! ```text
//! input → predelay L/R → [early reflections (tapped delay)]  → er_mix
//!                       → [8 FDN delays ↔ Hadamard feedback] → [allpass diffusion] → late_mix
//!                                                                           ↓
//!                                               stereo width → wet/dry → output
//! ```
//!
//! The FDN uses an 8×8 Hadamard matrix (implemented via fast Walsh–Hadamard
//! butterfly) to mix energy between delay lines in the feedback path. Each delay
//! line has sinusoidal LFO modulation (breaking metallic resonances) and a
//! one-pole lowpass for high-frequency damping. A tapped delay provides early
//! reflections with room-size-dependent timing.
//!
//! # Signal Flow (per sample)
//!
//! 1. Stereo predelay
//! 2. Mono sum → ER tapped delay write
//! 3. ER stereo read (even/odd taps → L/R)
//! 4. 8-line Hadamard FDN (modulated read → butterfly → damp → write)
//! 5. Allpass diffusion (separate L/R chains)
//! 6. ER + late reverb combine
//! 7. Stereo width via M/S
//! 8. Wet/dry mix → output gain
//!
//! # References
//!
//! - Jot & Chaigne, "Digital Delay Networks for Designing Artificial
//!   Reverberators", AES Convention Paper 3030, 1991.
//! - Jon Dattorro, "Effect Design, Part 1: Reverberator and Other Filters",
//!   J. Audio Eng. Soc., Vol. 45, No. 9, 1997.
//! - Jezar, Freeverb — delay tunings and comb filter structure.
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! let adapter = KernelAdapter::new(ReverbKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing, ADCs are hardware-filtered)
//! let mut kernel = ReverbKernel::new(48000.0);
//! let params = ReverbParams::from_knobs(adc_room, adc_decay, adc_damp, adc_mix, adc_output);
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

use libm::{ceilf, powf, roundf, sqrtf};
use sonido_core::fast_math::fast_sin_turns;
use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::{
    InterpolatedDelay, Interpolation, ModulatedAllpass, OnePole, ParamDescriptor, ParamId,
    ParamUnit, fast_db_to_linear, flush_denormal, wet_dry_mix_stereo,
};

// ── Constants (identical to classic Reverb) ─────────────────────────────────

/// FDN delay tunings at 44.1 kHz reference (from Freeverb, mutually prime).
const FDN_TUNINGS_44K: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];

/// LFO modulation rates per FDN line (Hz). Different rates prevent correlation.
const FDN_MOD_RATES: [f32; 8] = [0.3, 0.4, 0.5, 0.7, 0.8, 1.0, 1.1, 1.3];

/// Modulation depth for FDN delay lines (ms).
const FDN_MOD_DEPTH_MS: f32 = 0.3;

/// Allpass diffusion delay times at 44.1 kHz reference.
const ALLPASS_TUNINGS_44K: [usize; 4] = [556, 441, 341, 225];

/// Right-channel allpass delay times (offset for stereo decorrelation).
const ALLPASS_TUNINGS_44K_R: [usize; 4] = [579, 464, 364, 248];

/// Early reflection tap positions at 44.1 kHz reference (primes, ~25 ms window).
///
/// Room-size scaling extends these up to ~80 ms for large rooms.
const ER_TAP_POSITIONS_44K: [usize; 14] = [
    131, 197, 263, 337, 401, 463, 541, 613, 677, 751, 811, 877, 941, 1009,
];

/// Number of early reflection taps.
const ER_TAP_COUNT: usize = 14;

/// Reference sample rate for tuning constants.
const REFERENCE_RATE: f32 = 44100.0;

/// Maximum pre-delay in milliseconds.
const MAX_PREDELAY_MS: f32 = 100.0;

/// Maximum early reflection delay in milliseconds.
const MAX_ER_MS: f32 = 80.0;

/// Hadamard scale factor: 1/sqrt(8) for energy preservation.
///
/// The unnormalized 8×8 Hadamard satisfies H·Hᵀ = 8I, so dividing by
/// sqrt(8) makes it orthogonal (energy-preserving).
const HADAMARD_SCALE: f32 = 0.353_553_39;

/// ER output normalization. Keeps per-channel ER level near unity for
/// sustained signals (7 taps/channel with 1/√k gains sum ≈ 3.3).
/// Used to derive [`ER_TAP_GAINS`] LUT values.
#[allow(dead_code)]
const ER_GAIN_SCALE: f32 = 0.3;

/// Allpass diffusion feedback coefficient.
const ALLPASS_FEEDBACK: f32 = 0.6;

/// Allpass modulation depth (ms). Subtle — enough to decorrelate.
const ALLPASS_MOD_DEPTH_MS: f32 = 0.15;

// ── Helper functions ─────────────────────────────────────────────────────────

/// Scale delay samples from 44.1 kHz reference to target rate.
#[inline]
fn scale_to_rate(samples: usize, target_rate: f32) -> usize {
    (roundf(samples as f32 * target_rate / REFERENCE_RATE) as usize).max(1)
}

/// Pre-computed ER tap gains: `ER_GAIN_SCALE / sqrt(i+1)` for i in 0..14.
///
/// Gains decrease as 1/√(i+1), giving a natural amplitude decay with
/// distance. Scaled by [`ER_GAIN_SCALE`] so 7 taps per channel sum to ≈ 1.
///
/// Replaces the per-sample `sqrtf` calls with a const lookup.
const ER_TAP_GAINS: [f32; ER_TAP_COUNT] = [
    0.300_000, // i=0:  0.3 / sqrt(1)
    0.212_132, // i=1:  0.3 / sqrt(2)
    0.173_205, // i=2:  0.3 / sqrt(3)
    0.150_000, // i=3:  0.3 / sqrt(4)
    0.134_164, // i=4:  0.3 / sqrt(5)
    0.122_474, // i=5:  0.3 / sqrt(6)
    0.113_389, // i=6:  0.3 / sqrt(7)
    0.106_066, // i=7:  0.3 / sqrt(8)
    0.100_000, // i=8:  0.3 / sqrt(9)
    0.094_868, // i=9:  0.3 / sqrt(10)
    0.090_453, // i=10: 0.3 / sqrt(11)
    0.086_603, // i=11: 0.3 / sqrt(12)
    0.083_205, // i=12: 0.3 / sqrt(13)
    0.080_178, // i=13: 0.3 / sqrt(14)
];

/// Convert damping parameter (0 = bright, 1 = dark) to lowpass cutoff Hz.
///
/// Uses a logarithmic mapping: 200 Hz at damping = 1.0, 20 kHz at damping = 0.0.
/// This gives perceptually uniform brightness control.
#[inline]
fn damping_to_hz(damping: f32) -> f32 {
    200.0 * powf(100.0, 1.0 - damping)
}

// ── Hadamard fast Walsh–Hadamard transform ───────────────────────────────────

/// In-place butterfly on array elements at indices `i` and `j`.
///
/// `(buf[i], buf[j]) → (buf[i]+buf[j], buf[i]−buf[j])`.
#[inline]
fn butterfly_at(buf: &mut [f32; 8], i: usize, j: usize) {
    let sum = buf[i] + buf[j];
    let diff = buf[i] - buf[j];
    buf[i] = sum;
    buf[j] = diff;
}

/// Apply the 8×8 Hadamard transform in place via 3-stage butterfly.
///
/// The result is scaled by 1/√8 for energy preservation. This is equivalent
/// to multiplying by the normalized Hadamard matrix H₈/√8, which is
/// orthogonal (unitary): H₈ H₈ᵀ = 8I.
///
/// Reference: Wikipedia, "Fast Walsh–Hadamard Transform".
#[inline]
fn hadamard8(buf: &mut [f32; 8]) {
    // Stage 1
    butterfly_at(buf, 0, 1);
    butterfly_at(buf, 2, 3);
    butterfly_at(buf, 4, 5);
    butterfly_at(buf, 6, 7);
    // Stage 2
    butterfly_at(buf, 0, 2);
    butterfly_at(buf, 1, 3);
    butterfly_at(buf, 4, 6);
    butterfly_at(buf, 5, 7);
    // Stage 3
    butterfly_at(buf, 0, 4);
    butterfly_at(buf, 1, 5);
    butterfly_at(buf, 2, 6);
    butterfly_at(buf, 3, 7);
    // Energy-preserving scale
    for x in buf.iter_mut() {
        *x *= HADAMARD_SCALE;
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`ReverbKernel`].
///
/// All values in **user-facing units** — the same units shown in GUIs and
/// stored in presets. The kernel converts internally as needed.
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `room_size_pct` | % | 0–100 | 50.0 |
/// | 1 | `decay_pct` | % | 0–100 | 50.0 |
/// | 2 | `damping_pct` | % | 0–100 | 50.0 |
/// | 3 | `predelay_ms` | ms | 0–100 | 10.0 |
/// | 4 | `mix_pct` | % | 0–100 | 50.0 |
/// | 5 | `width_pct` | % | 0–100 | 100.0 |
/// | 6 | `er_level_pct` | % | 0–100 | 50.0 |
/// | 7 | `output_db` | dB | −20–+20 | 0.0 |
#[derive(Debug, Clone, Copy)]
pub struct ReverbParams {
    /// Room size as a percentage (0–100%).
    ///
    /// Controls early reflection timing and FDN feedback density.
    /// Higher values create wider, more spacious reflections.
    pub room_size_pct: f32,

    /// Decay time as a percentage (0–100%).
    ///
    /// Controls how long the reverb tail lasts. Higher values create
    /// longer, more sustained tails.
    pub decay_pct: f32,

    /// Damping amount as a percentage (0–100%).
    ///
    /// - 0% = bright (no HF absorption)
    /// - 100% = dark (heavy HF absorption)
    pub damping_pct: f32,

    /// Pre-delay time in milliseconds (0–100 ms).
    ///
    /// Introduces a gap between the dry signal and the onset of reverb,
    /// which helps preserve transient clarity in dense mixes.
    pub predelay_ms: f32,

    /// Wet/dry mix as a percentage (0% = fully dry, 100% = fully wet).
    pub mix_pct: f32,

    /// Stereo width as a percentage (0% = mono, 100% = full stereo).
    ///
    /// Scales the M/S side component after the FDN and allpass stages.
    /// At 0% both channels output identical mid signal (mono reverb).
    pub width_pct: f32,

    /// Early reflections level as a percentage (0–100%).
    ///
    /// Controls how prominent the early reflections are relative to
    /// the late diffuse reverb tail.
    pub er_level_pct: f32,

    /// Output level in decibels (−20 to +20 dB).
    pub output_db: f32,
}

impl Default for ReverbParams {
    fn default() -> Self {
        Self {
            room_size_pct: 50.0,
            decay_pct: 50.0,
            damping_pct: 50.0,
            predelay_ms: 10.0,
            mix_pct: 50.0,
            width_pct: 100.0,
            er_level_pct: 50.0,
            output_db: 0.0,
        }
    }
}

impl ReverbParams {
    /// Creates parameters from normalized 0–1 knob readings.
    ///
    /// Curves (logarithmic for frequency/time, linear for percentage) are
    /// derived from [`ParamDescriptor`] — same mapping as GUI and plugin hosts.
    #[allow(clippy::too_many_arguments)]
    pub fn from_knobs(
        room: f32,
        decay: f32,
        damp: f32,
        predelay: f32,
        mix: f32,
        width: f32,
        er_level: f32,
        output: f32,
    ) -> Self {
        Self::from_normalized(&[room, decay, damp, predelay, mix, width, er_level, output])
    }
}

impl KernelParams for ReverbParams {
    const COUNT: usize = 8;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(
                ParamDescriptor {
                    name: "Room Size",
                    short_name: "Room",
                    unit: ParamUnit::Percent,
                    min: 0.0,
                    max: 100.0,
                    default: 50.0,
                    step: 1.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(1500), "rev_room_size"),
            ),
            1 => Some(
                ParamDescriptor {
                    name: "Decay",
                    short_name: "Decay",
                    unit: ParamUnit::Percent,
                    min: 0.0,
                    max: 100.0,
                    default: 50.0,
                    step: 1.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(1501), "rev_decay"),
            ),
            2 => Some(
                ParamDescriptor {
                    name: "Damping",
                    short_name: "Damping",
                    unit: ParamUnit::Percent,
                    min: 0.0,
                    max: 100.0,
                    default: 50.0,
                    step: 1.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(1502), "rev_damping"),
            ),
            3 => Some(
                ParamDescriptor::custom("Pre-Delay", "PreDly", 0.0, 100.0, 10.0)
                    .with_unit(ParamUnit::Milliseconds)
                    .with_step(1.0)
                    .with_id(ParamId(1503), "rev_predelay"),
            ),
            4 => Some(ParamDescriptor::mix().with_id(ParamId(1504), "rev_mix")),
            5 => Some(
                ParamDescriptor {
                    name: "Stereo Width",
                    short_name: "Width",
                    unit: ParamUnit::Percent,
                    min: 0.0,
                    max: 100.0,
                    default: 100.0,
                    step: 1.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(1505), "rev_width"),
            ),
            6 => Some(
                ParamDescriptor {
                    name: "ER Level",
                    short_name: "ER Lvl",
                    unit: ParamUnit::Percent,
                    min: 0.0,
                    max: 100.0,
                    default: 50.0,
                    step: 1.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(1508), "rev_er_level"),
            ),
            7 => Some(
                sonido_core::gain::output_param_descriptor().with_id(ParamId(1507), "rev_output"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Slow, // room_size_pct — affects FDN feedback coeffs
            1 => SmoothingStyle::Slow, // decay_pct — affects FDN feedback coeffs
            2 => SmoothingStyle::Slow, // damping_pct — affects one-pole cutoff
            3 => SmoothingStyle::Interpolated, // predelay_ms — prevent pitch-shift artifacts
            4 => SmoothingStyle::Standard, // mix_pct — 10ms crossfade
            5 => SmoothingStyle::Standard, // width_pct — 10ms
            6 => SmoothingStyle::Standard, // er_level_pct — 10ms
            7 => SmoothingStyle::Standard, // output_db — 10ms
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.room_size_pct,
            1 => self.decay_pct,
            2 => self.damping_pct,
            3 => self.predelay_ms,
            4 => self.mix_pct,
            5 => self.width_pct,
            6 => self.er_level_pct,
            7 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.room_size_pct = value,
            1 => self.decay_pct = value,
            2 => self.damping_pct = value,
            3 => self.predelay_ms = value,
            4 => self.mix_pct = value,
            5 => self.width_pct = value,
            6 => self.er_level_pct = value,
            7 => self.output_db = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP reverb kernel — 8×8 Hadamard FDN with early reflections and allpass diffusion.
///
/// Contains ONLY the mutable state required for audio processing. No `SmoothedParam`,
/// no atomics, no platform awareness. The kernel is `Send`-safe because all
/// contained types are `Send`.
///
/// ## DSP State
///
/// - **8 FDN delay lines** (`InterpolatedDelay`, Cubic): the core reverb tank.
///   Each line has a sinusoidal LFO that modulates the read position to break
///   metallic resonances.
/// - **8 one-pole damping filters**: one per FDN line. Applied after Hadamard
///   mixing to simulate frequency-dependent air absorption.
/// - **ER tapped delay** (`InterpolatedDelay`): writes mono input; 14 taps read
///   back at room-size-scaled positions, distributed to L (even) and R (odd).
/// - **4 modulated allpass diffusers per channel** (L/R): smooth the late reverb
///   texture after the FDN by scattering energy across delay.
/// - **Stereo predelay** (two `InterpolatedDelay` lines, L + R): delays the input
///   before entering both ER and FDN.
///
/// ## Coefficient Caching
///
/// `feedback`, `fdn_compensation`, and `er_room_scale` are recomputed only when
/// `room_size_pct`, `decay_pct`, or `damping_pct` change beyond a small epsilon.
/// This avoids redundant `powf` / `sqrtf` calls on every sample during normal play.
pub struct ReverbKernel {
    // ── FDN: 8 delay lines with LFO modulation + feedback damping ────────
    /// Eight delay lines forming the feedback delay network.
    fdn_delays: [InterpolatedDelay; 8],
    /// One-pole lowpass filters applied per FDN line for HF damping.
    fdn_damping: [OnePole; 8],
    /// Base read positions in samples (scaled from 44.1 kHz reference).
    fdn_base_delays: [f32; 8],
    /// Peak modulation depth in samples = `FDN_MOD_DEPTH_MS * 0.001 * sample_rate`.
    fdn_mod_depth: f32,
    /// Current LFO phase per FDN line (turns, 0.0–1.0).
    fdn_phases: [f32; 8],
    /// LFO phase increment per sample per FDN line (rate / sample_rate).
    fdn_phase_incs: [f32; 8],
    /// Hadamard-mixed feedback from the previous sample (one value per FDN line).
    fdn_fb: [f32; 8],

    // ── Early reflections ─────────────────────────────────────────────────
    /// Tapped delay line for early reflections (up to `MAX_ER_MS`).
    er_delay: InterpolatedDelay,
    /// Base tap positions in samples (scaled to current sample rate).
    er_base_taps: [f32; ER_TAP_COUNT],

    // ── Allpass diffusion (stereo, modulated) ─────────────────────────────
    /// Four modulated allpass sections for the left channel.
    allpasses_l: [ModulatedAllpass; 4],
    /// Four modulated allpass sections for the right channel (different delay times).
    allpasses_r: [ModulatedAllpass; 4],

    // ── Predelay (stereo) ─────────────────────────────────────────────────
    /// Left-channel predelay line (up to `MAX_PREDELAY_MS`).
    predelay_l: InterpolatedDelay,
    /// Right-channel predelay line.
    predelay_r: InterpolatedDelay,

    /// Current sample rate in Hz.
    sample_rate: f32,

    // ── Coefficient caches (updated on room/decay/damping change) ─────────
    /// Last room_size value used to compute derived params (0–1 internal).
    cached_room: f32,
    /// Last decay value used to compute derived params (0–1 internal).
    cached_decay: f32,
    /// Last damping value used to compute derived params (0–1 internal).
    cached_damp: f32,
    /// FDN feedback coefficient: `scaled_room + decay × (0.98 − scaled_room)`.
    feedback: f32,
    /// Wet-signal compensation: `sqrt(1 − feedback)` — maintains perceptual level.
    fdn_compensation: f32,
    /// ER tap scaling: `0.5 + room × 1.5` (range 0.5 → 2.0).
    er_room_scale: f32,
}

impl ReverbKernel {
    /// Create a new reverb kernel initialised at `sample_rate`.
    ///
    /// Allocates all delay lines and filters. The FDN delay lines are sized
    /// for their base delay + maximum LFO modulation depth. The ER and predelay
    /// lines are sized for their respective maximum durations. Allpass diffusers
    /// are initialised with different LFO rates (0.7 for L, 0.8 for R) for
    /// natural stereo decorrelation.
    pub fn new(sample_rate: f32) -> Self {
        let mod_depth = FDN_MOD_DEPTH_MS * 0.001 * sample_rate;

        let fdn_delays: [InterpolatedDelay; 8] = core::array::from_fn(|i| {
            let base = scale_to_rate(FDN_TUNINGS_44K[i], sample_rate) as f32;
            let capacity = (base + mod_depth) as usize + 4;
            let mut delay = InterpolatedDelay::new(capacity);
            delay.set_interpolation(Interpolation::Linear);
            delay
        });

        let damping_hz = damping_to_hz(0.5); // default damping = 50%
        let fdn_damping: [OnePole; 8] =
            core::array::from_fn(|_| OnePole::new(sample_rate, damping_hz));

        let fdn_base_delays: [f32; 8] =
            core::array::from_fn(|i| scale_to_rate(FDN_TUNINGS_44K[i], sample_rate) as f32);

        let fdn_phases = [0.0f32; 8];
        let fdn_phase_incs: [f32; 8] = core::array::from_fn(|i| FDN_MOD_RATES[i] / sample_rate);

        // Early reflections tapped delay (~80 ms max)
        let er_max = (ceilf(MAX_ER_MS * 0.001 * sample_rate) as usize).max(1);
        let er_delay = InterpolatedDelay::new(er_max);
        let er_base_taps: [f32; ER_TAP_COUNT] =
            core::array::from_fn(|i| scale_to_rate(ER_TAP_POSITIONS_44K[i], sample_rate) as f32);

        // Allpass diffusion (modulated, stereo)
        let allpasses_l: [ModulatedAllpass; 4] = core::array::from_fn(|i| {
            let base = scale_to_rate(ALLPASS_TUNINGS_44K[i], sample_rate) as f32;
            ModulatedAllpass::new(
                base,
                ALLPASS_FEEDBACK,
                0.7,
                ALLPASS_MOD_DEPTH_MS,
                sample_rate,
            )
        });
        let allpasses_r: [ModulatedAllpass; 4] = core::array::from_fn(|i| {
            let base = scale_to_rate(ALLPASS_TUNINGS_44K_R[i], sample_rate) as f32;
            ModulatedAllpass::new(
                base,
                ALLPASS_FEEDBACK,
                0.8,
                ALLPASS_MOD_DEPTH_MS,
                sample_rate,
            )
        });

        // Predelay (stereo, up to 100 ms)
        let max_predelay = (ceilf(MAX_PREDELAY_MS * 0.001 * sample_rate) as usize).max(1);
        let predelay_l = InterpolatedDelay::new(max_predelay);
        let predelay_r = InterpolatedDelay::new(max_predelay);

        let mut kernel = Self {
            fdn_delays,
            fdn_damping,
            fdn_base_delays,
            fdn_mod_depth: mod_depth,
            fdn_phases,
            fdn_phase_incs,
            fdn_fb: [0.0; 8],
            er_delay,
            er_base_taps,
            allpasses_l,
            allpasses_r,
            predelay_l,
            predelay_r,
            sample_rate,
            cached_room: -1.0,
            cached_decay: -1.0,
            cached_damp: -1.0,
            feedback: 0.0,
            fdn_compensation: 1.0,
            er_room_scale: 1.0,
        };
        // Prime the coefficient cache with the default param values (room=0.5, decay=0.5, damp=0.5).
        kernel.update_derived(0.5, 0.5, 0.5);
        kernel
    }

    // ── Internal processing helpers ───────────────────────────────────────

    /// Recompute `feedback`, `fdn_compensation`, `er_room_scale`, and damping
    /// cutoff when room/decay/damping parameters change by more than 0.001.
    ///
    /// Uses the Freeverb feedback formula:
    /// - `scaled_room = 0.28 + room × 0.7`  (range 0.28..0.98)
    /// - `feedback    = scaled_room + decay × (0.98 − scaled_room)`
    ///
    /// `fdn_compensation = sqrt(1 − feedback)` preserves wet-signal level.
    /// `er_room_scale = 0.5 + room × 1.5` scales ER tap positions (0.5 → 2.0).
    #[inline]
    fn update_derived(&mut self, room: f32, decay: f32, damp: f32) {
        if (room - self.cached_room).abs() < 0.001
            && (decay - self.cached_decay).abs() < 0.001
            && (damp - self.cached_damp).abs() < 0.001
        {
            return;
        }
        self.cached_room = room;
        self.cached_decay = decay;
        self.cached_damp = damp;

        let scaled_room = 0.28 + room * 0.7;
        self.feedback = (scaled_room + decay * (0.98 - scaled_room)).clamp(0.0, 0.99);
        self.fdn_compensation = sqrtf((1.0 - self.feedback).max(0.01));
        self.er_room_scale = 0.5 + room * 1.5;

        let freq = damping_to_hz(damp);
        for filter in &mut self.fdn_damping {
            filter.set_frequency(freq);
        }
    }

    /// Process stereo predelay for one channel.
    ///
    /// When `predelay_samples > 0.5` the input is written to the delay line and
    /// the delayed output is returned. Otherwise the write is a no-op write and
    /// the input passes through directly (zero-latency path).
    #[inline]
    fn apply_predelay(line: &mut InterpolatedDelay, input: f32, predelay: f32) -> f32 {
        if predelay > 0.5 {
            line.read_write(input, predelay)
        } else {
            line.write(input);
            input
        }
    }

    /// Process one sample through the 8×8 Hadamard FDN.
    ///
    /// Steps:
    /// 1. Read modulated outputs from all 8 delay lines.
    /// 2. Apply Hadamard butterfly to mix the raw outputs.
    /// 3. Damp the mixed signals and write them back with the new input.
    /// 4. Advance each LFO phase.
    ///
    /// Returns raw L (even delays averaged) and R (odd delays averaged) before
    /// `fdn_compensation` scaling.
    #[inline]
    fn process_fdn(&mut self, input: f32) -> (f32, f32) {
        let mut raw = [0.0f32; 8];

        // 1. Read modulated outputs
        for i in 0..8 {
            let modulated =
                self.fdn_base_delays[i] + self.fdn_mod_depth * fast_sin_turns(self.fdn_phases[i]);
            raw[i] = self.fdn_delays[i].read(modulated);
        }

        // 2. Hadamard-mix raw outputs for the feedback path
        let mut mixed = raw;
        hadamard8(&mut mixed);

        // 3. Damp, write back with feedback, advance LFO phases
        for i in 0..8 {
            let damped = self.fdn_damping[i].process(mixed[i]);
            self.fdn_delays[i].write(flush_denormal(input + damped * self.feedback));

            self.fdn_phases[i] += self.fdn_phase_incs[i];
            if self.fdn_phases[i] >= 1.0 {
                self.fdn_phases[i] -= 1.0;
            }
        }

        // 4. Stereo output: L = even delays, R = odd delays
        let fdn_l = (raw[0] + raw[2] + raw[4] + raw[6]) * 0.25;
        let fdn_r = (raw[1] + raw[3] + raw[5] + raw[7]) * 0.25;

        (fdn_l * self.fdn_compensation, fdn_r * self.fdn_compensation)
    }

    /// Compute stereo early reflections from the ER tapped delay.
    ///
    /// Even-indexed taps → L, odd-indexed taps → R. Tap positions are
    /// scaled by `er_room_scale` so larger rooms produce wider ER spacing.
    ///
    /// Returns `(er_left, er_right)`.
    #[inline]
    fn compute_er_stereo(&self) -> (f32, f32) {
        let mut er_l = 0.0f32;
        let mut er_r = 0.0f32;
        for i in 0..ER_TAP_COUNT {
            let tap_pos = self.er_base_taps[i] * self.er_room_scale;
            let sample = self.er_delay.read(tap_pos);
            let gain = ER_TAP_GAINS[i];
            if i % 2 == 0 {
                er_l += sample * gain;
            } else {
                er_r += sample * gain;
            }
        }
        (er_l, er_r)
    }
}

impl DspKernel for ReverbKernel {
    type Params = ReverbParams;

    /// Process a stereo sample pair through the reverb.
    ///
    /// ## Signal Flow
    ///
    /// 1. Convert params to internal units (0–1 for room/decay/damp/mix/width/er_lvl,
    ///    samples for predelay, linear for output).
    /// 2. Update derived coefficient caches when room/decay/damping change.
    /// 3. Stereo predelay (separate L + R lines).
    /// 4. Mono sum → ER tapped delay write + stereo ER read.
    /// 5. Hadamard FDN (mono in → stereo out).
    /// 6. Separate L/R allpass diffusion chains.
    /// 7. ER + late reverb combine.
    /// 8. Stereo width (M/S).
    /// 9. Wet/dry mix → output gain.
    #[inline]
    fn process_stereo(&mut self, left: f32, right: f32, params: &ReverbParams) -> (f32, f32) {
        // ── Unit conversion ──
        let room = params.room_size_pct / 100.0;
        let decay = params.decay_pct / 100.0;
        let damp = params.damping_pct / 100.0;
        let predelay_samples = (params.predelay_ms * 0.001 * self.sample_rate)
            .clamp(0.0, MAX_PREDELAY_MS * 0.001 * self.sample_rate);
        let mix = params.mix_pct / 100.0;
        let er_lvl = params.er_level_pct / 100.0;
        let width = params.width_pct / 100.0;
        let output = fast_db_to_linear(params.output_db);

        // ── Update coefficient caches ──
        self.update_derived(room, decay, damp);

        // ── Stereo predelay ──
        let pre_l = Self::apply_predelay(&mut self.predelay_l, left, predelay_samples);
        let pre_r = Self::apply_predelay(&mut self.predelay_r, right, predelay_samples);
        let mono = (pre_l + pre_r) * 0.5;

        // ── Early reflections ──
        self.er_delay.write(mono);
        let (er_l, er_r) = self.compute_er_stereo();

        // ── FDN (mono in → stereo out) ──
        let (fdn_l, fdn_r) = self.process_fdn(mono);

        // ── Allpass diffusion (separate L/R chains) ──
        let mut diff_l = fdn_l;
        for ap in &mut self.allpasses_l {
            diff_l = ap.process(diff_l);
        }
        let mut diff_r = fdn_r;
        for ap in &mut self.allpasses_r {
            diff_r = ap.process(diff_r);
        }

        // ── Combine ER + late reverb ──
        let wet_l = diff_l + er_l * er_lvl;
        let wet_r = diff_r + er_r * er_lvl;

        // ── Stereo width: M/S encode-scale-decode ──
        let mid = (wet_l + wet_r) * 0.5;
        let side = (wet_l - wet_r) * 0.5;
        let final_l = mid + side * width;
        let final_r = mid - side * width;

        // ── Wet/dry mix → output gain ──
        let (out_l, out_r) = wet_dry_mix_stereo(left, right, final_l, final_r, mix);
        (out_l * output, out_r * output)
    }

    /// Reset all DSP state to silence.
    ///
    /// Clears all delay lines, resets one-pole filter histories, zeroes LFO
    /// phases and the FDN feedback buffer, and resets all allpass diffusers.
    /// The coefficient cache is invalidated so it will be recomputed on the
    /// next call to `process_stereo`.
    fn reset(&mut self) {
        for delay in &mut self.fdn_delays {
            delay.clear();
        }
        for filter in &mut self.fdn_damping {
            filter.reset();
        }
        self.fdn_phases = [0.0; 8];
        self.fdn_fb = [0.0; 8];

        self.er_delay.clear();

        for ap in &mut self.allpasses_l {
            ap.reset();
        }
        for ap in &mut self.allpasses_r {
            ap.reset();
        }

        self.predelay_l.clear();
        self.predelay_r.clear();

        // Invalidate cache so derived params recompute on next call.
        self.cached_room = -1.0;
    }

    /// Update internal state for a new sample rate.
    ///
    /// Recreates all delay lines, allpass diffusers, damping filters, and
    /// phase increment tables for the new rate. The coefficient cache is
    /// invalidated so derived params will recompute on the next call.
    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        let mod_depth = FDN_MOD_DEPTH_MS * 0.001 * sample_rate;
        self.fdn_mod_depth = mod_depth;

        // Recreate FDN delay lines
        self.fdn_delays = core::array::from_fn(|i| {
            let base = scale_to_rate(FDN_TUNINGS_44K[i], sample_rate) as f32;
            let capacity = (base + mod_depth) as usize + 4;
            let mut delay = InterpolatedDelay::new(capacity);
            delay.set_interpolation(Interpolation::Linear);
            delay
        });
        self.fdn_base_delays =
            core::array::from_fn(|i| scale_to_rate(FDN_TUNINGS_44K[i], sample_rate) as f32);
        self.fdn_phase_incs = core::array::from_fn(|i| FDN_MOD_RATES[i] / sample_rate);
        for filter in &mut self.fdn_damping {
            filter.set_sample_rate(sample_rate);
        }
        self.fdn_phases = [0.0; 8];
        self.fdn_fb = [0.0; 8];

        // Recreate ER delay
        let er_max = (ceilf(MAX_ER_MS * 0.001 * sample_rate) as usize).max(1);
        self.er_delay = InterpolatedDelay::new(er_max);
        self.er_base_taps =
            core::array::from_fn(|i| scale_to_rate(ER_TAP_POSITIONS_44K[i], sample_rate) as f32);

        // Recreate allpass diffusion
        self.allpasses_l = core::array::from_fn(|i| {
            let base = scale_to_rate(ALLPASS_TUNINGS_44K[i], sample_rate) as f32;
            ModulatedAllpass::new(
                base,
                ALLPASS_FEEDBACK,
                0.7,
                ALLPASS_MOD_DEPTH_MS,
                sample_rate,
            )
        });
        self.allpasses_r = core::array::from_fn(|i| {
            let base = scale_to_rate(ALLPASS_TUNINGS_44K_R[i], sample_rate) as f32;
            ModulatedAllpass::new(
                base,
                ALLPASS_FEEDBACK,
                0.8,
                ALLPASS_MOD_DEPTH_MS,
                sample_rate,
            )
        });

        // Recreate predelay
        let max_predelay = (ceilf(MAX_PREDELAY_MS * 0.001 * sample_rate) as usize).max(1);
        self.predelay_l = InterpolatedDelay::new(max_predelay);
        self.predelay_r = InterpolatedDelay::new(max_predelay);

        // Invalidate cache
        self.cached_room = -1.0;
    }

    /// Returns `true` — L/R are decorrelated by asymmetric FDN tap assignment
    /// and different allpass delay networks per channel.
    fn is_true_stereo(&self) -> bool {
        true
    }

    /// Reverb reports zero processing latency.
    ///
    /// The predelay is a musical effect, not processing overhead, so it is
    /// not reported as latency to the host.
    fn latency_samples(&self) -> usize {
        0
    }

    /// Estimated reverb tail duration in samples.
    ///
    /// Based on the T60 approximation: `decay × 3.0 × sample_rate`.
    /// At `decay = 1.0` (maximum) this is 3 seconds — enough to cover the
    /// longest practical reverb tail. Spillover (Phase 4) uses this to keep
    /// the old graph alive while the tail rings out.
    fn tail_samples(&self) -> usize {
        (self.cached_decay * 3.0 * self.sample_rate) as usize
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

    // ── Basic correctness ────────────────────────────────────────────────────

    /// Silence in must produce silence out regardless of parameter state.
    ///
    /// With zero input and all delay lines cleared, both wet and dry
    /// contributions are zero — the output must be exactly 0.
    #[test]
    fn silence_in_silence_out() {
        let mut kernel = ReverbKernel::new(48000.0);
        let params = ReverbParams::default();

        let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
        assert!(l.abs() < 1e-6, "Expected silence on left, got {l}");
        assert!(r.abs() < 1e-6, "Expected silence on right, got {r}");
    }

    /// Processing must never produce NaN or ±Infinity over 2000 samples.
    #[test]
    fn no_nan_or_inf() {
        let mut kernel = ReverbKernel::new(48000.0);
        let params = ReverbParams {
            room_size_pct: 80.0,
            decay_pct: 90.0,
            damping_pct: 20.0,
            predelay_ms: 30.0,
            mix_pct: 100.0,
            width_pct: 100.0,
            er_level_pct: 80.0,
            output_db: 0.0,
        };

        for i in 0..2000 {
            let t = i as f32 * core::f32::consts::PI * 0.02;
            let inp = libm::sinf(t) * 0.8;
            let (l, r) = kernel.process_stereo(inp, -inp, &params);
            assert!(l.is_finite(), "Left NaN/Inf at sample {i}: {l}");
            assert!(r.is_finite(), "Right NaN/Inf at sample {i}: {r}");
        }
    }

    /// `ReverbParams::COUNT` must equal 8, and all indices must have descriptors.
    #[test]
    fn params_descriptor_count() {
        assert_eq!(ReverbParams::COUNT, 8, "Expected 8 parameters");

        for i in 0..ReverbParams::COUNT {
            assert!(
                ReverbParams::descriptor(i).is_some(),
                "Missing descriptor at index {i}"
            );
        }
        assert!(
            ReverbParams::descriptor(ReverbParams::COUNT).is_none(),
            "Descriptor beyond COUNT should be None"
        );
    }

    /// The kernel must wrap into a `KernelAdapter` and function as an `Effect`.
    #[test]
    fn adapter_wraps_as_effect() {
        let mut adapter = KernelAdapter::new(ReverbKernel::new(48000.0), 48000.0);
        adapter.reset();
        let output = adapter.process(0.3);
        assert!(
            output.is_finite(),
            "Adapter output must be finite, got {output}"
        );
    }

    /// The adapter's `ParameterInfo` must match `ReverbParams::COUNT` and ParamIds.
    #[test]
    fn adapter_param_info_matches() {
        let adapter = KernelAdapter::new(ReverbKernel::new(48000.0), 48000.0);
        assert_eq!(
            adapter.param_count(),
            ReverbParams::COUNT,
            "Adapter param count must match ReverbParams::COUNT"
        );

        // Verify ParamIds match the classic Reverb effect exactly.
        assert_eq!(adapter.param_info(0).unwrap().id, ParamId(1500)); // room_size
        assert_eq!(adapter.param_info(1).unwrap().id, ParamId(1501)); // decay
        assert_eq!(adapter.param_info(2).unwrap().id, ParamId(1502)); // damping
        assert_eq!(adapter.param_info(3).unwrap().id, ParamId(1503)); // predelay
        assert_eq!(adapter.param_info(4).unwrap().id, ParamId(1504)); // mix
        assert_eq!(adapter.param_info(5).unwrap().id, ParamId(1505)); // width
        assert_eq!(adapter.param_info(6).unwrap().id, ParamId(1508)); // er_level (non-sequential!)
        assert_eq!(adapter.param_info(7).unwrap().id, ParamId(1507)); // output (non-sequential!)
    }

    /// Morphing between two param states must always produce finite output.
    #[test]
    fn morph_produces_valid_output() {
        let mut kernel = ReverbKernel::new(48000.0);
        let a = ReverbParams::default();
        let b = ReverbParams {
            room_size_pct: 100.0,
            decay_pct: 95.0,
            damping_pct: 10.0,
            predelay_ms: 80.0,
            mix_pct: 100.0,
            width_pct: 100.0,
            er_level_pct: 100.0,
            output_db: -6.0,
        };

        for i in 0..=10 {
            let t = i as f32 / 10.0;
            let morphed = ReverbParams::lerp(&a, &b, t);
            let (l, r) = kernel.process_stereo(0.3, -0.3, &morphed);
            assert!(
                l.is_finite() && r.is_finite(),
                "Morph at t={t} produced NaN/Inf: l={l}, r={r}"
            );
            kernel.reset();
        }
    }

    /// `from_knobs()` must map 0.0–1.0 inputs to the correct parameter ranges
    /// for all 8 parameters.
    #[test]
    fn from_knobs_maps_ranges() {
        // Maximum deflection: all 8 knobs at 1.0
        let max = ReverbParams::from_knobs(1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0);
        assert!(
            (max.room_size_pct - 100.0).abs() < 0.01,
            "Room at 1.0 should be 100%, got {}",
            max.room_size_pct
        );
        assert!(
            (max.decay_pct - 100.0).abs() < 0.01,
            "Decay at 1.0 should be 100%, got {}",
            max.decay_pct
        );
        assert!(
            (max.damping_pct - 100.0).abs() < 0.01,
            "Damping at 1.0 should be 100%, got {}",
            max.damping_pct
        );
        assert!(
            (max.predelay_ms - 100.0).abs() < 0.01,
            "Predelay at 1.0 should be 100 ms, got {}",
            max.predelay_ms
        );
        assert!(
            (max.mix_pct - 100.0).abs() < 0.01,
            "Mix at 1.0 should be 100%, got {}",
            max.mix_pct
        );
        assert!(
            (max.width_pct - 100.0).abs() < 0.01,
            "Width at 1.0 should be 100%, got {}",
            max.width_pct
        );
        assert!(
            (max.er_level_pct - 100.0).abs() < 0.01,
            "ER level at 1.0 should be 100%, got {}",
            max.er_level_pct
        );
        assert!(
            (max.output_db - 6.0).abs() < 0.01,
            "Output at 1.0 should be +6 dB, got {}",
            max.output_db
        );

        // Minimum deflection: all 8 knobs at 0.0
        let min = ReverbParams::from_knobs(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert!(
            min.room_size_pct.abs() < 0.01,
            "Room at 0.0 should be 0%, got {}",
            min.room_size_pct
        );
        assert!(
            min.decay_pct.abs() < 0.01,
            "Decay at 0.0 should be 0%, got {}",
            min.decay_pct
        );
        assert!(
            min.predelay_ms.abs() < 0.01,
            "Predelay at 0.0 should be 0 ms, got {}",
            min.predelay_ms
        );
        assert!(
            min.mix_pct.abs() < 0.01,
            "Mix at 0.0 should be 0%, got {}",
            min.mix_pct
        );
        assert!(
            min.width_pct.abs() < 0.01,
            "Width at 0.0 should be 0%, got {}",
            min.width_pct
        );
        assert!(
            min.er_level_pct.abs() < 0.01,
            "ER level at 0.0 should be 0%, got {}",
            min.er_level_pct
        );
        assert!(
            (min.output_db - (-20.0)).abs() < 0.01,
            "Output at 0.0 should be -20 dB, got {}",
            min.output_db
        );

        // Mid-point: all 8 knobs at 0.5
        let mid = ReverbParams::from_knobs(0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5);
        assert!(
            (mid.room_size_pct - 50.0).abs() < 0.01,
            "Room at 0.5 should be 50%, got {}",
            mid.room_size_pct
        );
        assert!(
            (mid.predelay_ms - 50.0).abs() < 0.01,
            "Predelay at 0.5 should be 50 ms, got {}",
            mid.predelay_ms
        );
        assert!(
            (mid.output_db - (-7.0)).abs() < 0.1,
            "Output at 0.5 should be -7 dB, got {}",
            mid.output_db
        );
    }

    /// High decay must produce a reverb tail that persists for at least 1 second.
    ///
    /// After a brief impulse (1 sample), feeding silence for 48000 samples
    /// (1 second at 48 kHz) should still leave audible energy in the FDN.
    #[test]
    fn decay_tail_persists() {
        let mut kernel = ReverbKernel::new(48000.0);
        let params = ReverbParams {
            room_size_pct: 80.0,
            decay_pct: 90.0, // high decay → long tail
            damping_pct: 30.0,
            predelay_ms: 0.0,
            mix_pct: 100.0,
            width_pct: 100.0,
            er_level_pct: 50.0,
            output_db: 0.0,
        };

        // Impulse
        kernel.process_stereo(1.0, 1.0, &params);

        // Feed 1 second of silence
        let silent = ReverbParams {
            mix_pct: 100.0,
            ..params
        };
        let mut last_l = 0.0f32;
        for _ in 0..48000 {
            let (l, _r) = kernel.process_stereo(0.0, 0.0, &silent);
            last_l = l;
        }

        assert!(
            last_l.abs() > 1e-6,
            "High-decay reverb tail should persist after 1 s, got {last_l}"
        );
    }

    /// Applying the Hadamard transform twice must return the original vector (involutory).
    ///
    /// The normalized 8×8 Hadamard satisfies (H₈/√8)² = H₈²/8 = 8I/8 = I, meaning
    /// it is its own inverse. This verifies the butterfly implementation is correct.
    #[test]
    fn hadamard_orthogonality() {
        let original = [1.0_f32, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let mut buf = original;
        hadamard8(&mut buf);
        hadamard8(&mut buf);

        for i in 0..8 {
            assert!(
                (buf[i] - original[i]).abs() < 1e-5,
                "Hadamard should be involutory (H² = I): buf[{i}] = {}, expected {}",
                buf[i],
                original[i]
            );
        }
    }

    /// The Hadamard transform must preserve energy (H × H = I for the normalised variant).
    ///
    /// Applying `hadamard8` to a unit impulse vector must leave the total energy
    /// unchanged: ‖H₈/√8 · x‖² = ‖x‖² since the matrix is unitary (orthogonal).
    #[test]
    fn hadamard_energy_preservation() {
        let mut buf = [1.0_f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let energy_before: f32 = buf.iter().map(|x| x * x).sum();
        hadamard8(&mut buf);
        let energy_after: f32 = buf.iter().map(|x| x * x).sum();

        assert!(
            (energy_before - energy_after).abs() < 1e-5,
            "Hadamard should preserve energy: before={energy_before}, after={energy_after}"
        );
    }
}
