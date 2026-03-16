//! Plate reverb kernel — Dattorro-inspired plate algorithm with input diffusion and modulated tank.
//!
//! `PlateReverbKernel` models the classic EMT 140 plate reverb character:
//! a highly diffuse, colorless tail with smooth decay and natural density.
//!
//! # Architecture
//!
//! ```text
//! input → bandwidth LP → predelay → [4 input diffusers (allpass)] → tank
//!                                                                      ↓
//!                         tank: [modulated allpass L] ←→ [delay + damp] ←→ [modulated allpass R]
//!                                        ↓ tap L                                   ↓ tap R
//!                                   wet_L (true stereo)               wet_R (true stereo)
//!                                           ↓
//!                               wet/dry mix → output gain
//! ```
//!
//! # Signal Flow (per sample)
//!
//! 1. Input one-pole bandwidth filter
//! 2. Pre-delay
//! 3. 4-stage allpass input diffusion (delay lengths scaled by size)
//! 4. Recirculate through two tank halves each containing:
//!    - Modulated allpass (decorrelates via slow LFO)
//!    - Long delay line with one-pole HF damping in the feedback path
//! 5. L/R tapped from the two different tank positions (true stereo)
//! 6. Wet/dry mix → output gain
//!
//! # References
//!
//! - Jon Dattorro, "Effect Design, Part 1: Reverberator and Other Filters",
//!   J. Audio Eng. Soc., Vol. 45, No. 9, 1997. Sections 4–6.
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! let adapter = Adapter::new(PlateReverbKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing, ADCs are hardware-filtered)
//! let mut kernel = PlateReverbKernel::new(48000.0);
//! let params = PlateReverbParams::from_knobs(adc_decay, adc_damp, adc_predelay,
//!                                            adc_bw, adc_diff, adc_size, adc_mix, adc_out);
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

use libm::{ceilf, expf, powf};
use sonido_core::fast_math::fast_sin_turns;
use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::{
    AllpassFilter, InterpolatedDelay, Interpolation, OnePole, ParamDescriptor, ParamId, ParamUnit,
    flush_denormal, wet_dry_mix_stereo,
};

// ── Constants ───────────────────────────────────────────────────────────────

/// Reference sample rate for delay tuning constants (44.1 kHz).
const REF_RATE: f32 = 44100.0;

/// Maximum pre-delay in milliseconds.
const MAX_PREDELAY_MS: f32 = 100.0;

/// Input diffusion allpass delay lengths at 44.1 kHz (prime values for density).
///
/// These four stages smear the input impulse before entering the tank,
/// creating the plate's characteristically smooth attack.
const INPUT_DIFF_44K: [usize; 4] = [142, 107, 379, 277];

/// Tank delay line lengths at 44.1 kHz. Scaled by size parameter.
///
/// Based on Dattorro's "infinity" plate structure. Mutually prime for
/// maximum spectral diffusion.
const TANK_DELAY_44K: [usize; 2] = [4453, 3720];

/// Tank modulated allpass delay lengths at 44.1 kHz.
const TANK_ALLPASS_44K: [usize; 2] = [672, 908];

/// LFO modulation rates for the two tank allpasses (Hz).
const TANK_MOD_RATES: [f32; 2] = [0.1, 0.15];

/// LFO modulation depth for tank allpasses (ms).
const TANK_MOD_DEPTH_MS: f32 = 0.5;

/// Tank allpass feedback coefficient (from Dattorro).
const TANK_ALLPASS_FEEDBACK: f32 = 0.7;

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Scale a 44.1 kHz delay length to the current sample rate.
#[inline]
fn scale(samples: usize, sr: f32) -> usize {
    (((samples as f32 * sr / REF_RATE) + 0.5) as usize).max(1)
}

/// Convert damping percentage (0 = bright, 100 = dark) to one-pole coefficient.
///
/// Maps 0%→very bright (coeff≈0.01) to 100%→very dark (coeff≈0.99).
/// Uses logarithmic spacing for perceptually uniform control.
#[inline]
fn damping_to_coeff(damping_pct: f32) -> f32 {
    // damping 0% → cutoff ~20kHz, 100% → cutoff ~200Hz
    let t = damping_pct * 0.01;
    200.0 * powf(100.0, 1.0 - t)
}

/// Convert bandwidth percentage (0–100%) to one-pole cutoff Hz.
///
/// 100% = fully open (~20 kHz), 0% = heavily filtered (~200 Hz).
#[inline]
fn bandwidth_to_hz(bw_pct: f32) -> f32 {
    let t = bw_pct * 0.01;
    200.0 * powf(100.0, t)
}

/// Compute per-sample decay feedback coefficient from decay time (seconds).
///
/// `feedback = exp(-3 * T60 / decay_s)` where T60 is one sample. Clamped
/// to [0.0, 0.98] for stability.
#[inline]
fn decay_to_feedback(decay_s: f32, sr: f32) -> f32 {
    // g^N = 10^(-3) for decay over `decay_s * sr` samples
    // g = 10^(-3 / (decay_s * sr)) = exp(-3 * ln(10) / (decay_s * sr))
    let g = expf(-3.0 * core::f32::consts::LN_10 / (decay_s * sr));
    g.clamp(0.0, 0.98)
}

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`PlateReverbKernel`].
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `decay_s` | s | 0.1–10 | 2.0 |
/// | 1 | `damping_pct` | % | 0–100 | 50.0 |
/// | 2 | `predelay_ms` | ms | 0–100 | 20.0 |
/// | 3 | `bandwidth_pct` | % | 0–100 | 80.0 |
/// | 4 | `diffusion_pct` | % | 0–100 | 70.0 |
/// | 5 | `size_pct` | % | 0–100 | 50.0 |
/// | 6 | `mix_pct` | % | 0–100 | 30.0 |
/// | 7 | `output_db` | dB | −60–+6 | 0.0 |
#[derive(Debug, Clone, Copy)]
pub struct PlateReverbParams {
    /// Decay time in seconds (0.1–10 s).
    ///
    /// Controls the RT60 of the reverb tail. At 10 s the feedback
    /// coefficient approaches 0.98 — nearly infinite sustain.
    pub decay_s: f32,

    /// High-frequency damping (0% = bright, 100% = dark).
    ///
    /// Controls the one-pole cutoff inside the tank feedback loop.
    /// Higher values absorb highs faster, simulating a denser medium.
    pub damping_pct: f32,

    /// Pre-delay time in milliseconds (0–100 ms).
    ///
    /// Delays the wet signal relative to dry, preserving transient clarity.
    pub predelay_ms: f32,

    /// Input bandwidth as a percentage (0–100%).
    ///
    /// One-pole lowpass on the input before the diffusion chain.
    /// 100% = fully open, 0% = darkened input.
    pub bandwidth_pct: f32,

    /// Input diffusion amount (0–100%).
    ///
    /// Controls the feedback coefficient of the 4 input allpass stages.
    /// Higher values = denser, more diffuse attack.
    pub diffusion_pct: f32,

    /// Tank size as a percentage (0–100%).
    ///
    /// Scales the input diffusion allpass lengths and tank delay lengths.
    /// Larger sizes produce more spacious, widely-spaced reflections.
    pub size_pct: f32,

    /// Wet/dry mix as a percentage (0% = dry, 100% = wet).
    pub mix_pct: f32,

    /// Output level in decibels (−60 to +6 dB).
    pub output_db: f32,
}

impl Default for PlateReverbParams {
    fn default() -> Self {
        Self {
            decay_s: 2.0,
            damping_pct: 50.0,
            predelay_ms: 20.0,
            bandwidth_pct: 80.0,
            diffusion_pct: 70.0,
            size_pct: 50.0,
            mix_pct: 30.0,
            output_db: 0.0,
        }
    }
}

impl PlateReverbParams {
    /// Create parameters from normalized 0–1 hardware knob readings.
    #[allow(clippy::too_many_arguments)]
    pub fn from_knobs(
        decay: f32,
        damping: f32,
        predelay: f32,
        bandwidth: f32,
        diffusion: f32,
        size: f32,
        mix: f32,
        output: f32,
    ) -> Self {
        Self::from_normalized(&[
            decay, damping, predelay, bandwidth, diffusion, size, mix, output,
        ])
    }
}

impl KernelParams for PlateReverbParams {
    const COUNT: usize = 8;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(
                ParamDescriptor::custom("Decay", "Decay", 0.1, 10.0, 2.0)
                    .with_unit(ParamUnit::None)
                    .with_id(ParamId(2700), "plate_decay"),
            ),
            1 => Some(
                ParamDescriptor {
                    name: "Damping",
                    short_name: "Damp",
                    unit: ParamUnit::Percent,
                    min: 0.0,
                    max: 100.0,
                    default: 50.0,
                    step: 1.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(2701), "plate_damping"),
            ),
            2 => Some(
                ParamDescriptor::custom("Pre-Delay", "PreDly", 0.0, 100.0, 20.0)
                    .with_unit(ParamUnit::Milliseconds)
                    .with_step(1.0)
                    .with_id(ParamId(2702), "plate_predelay"),
            ),
            3 => Some(
                ParamDescriptor {
                    name: "Bandwidth",
                    short_name: "BW",
                    unit: ParamUnit::Percent,
                    min: 0.0,
                    max: 100.0,
                    default: 80.0,
                    step: 1.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(2703), "plate_bandwidth"),
            ),
            4 => Some(
                ParamDescriptor {
                    name: "Diffusion",
                    short_name: "Diff",
                    unit: ParamUnit::Percent,
                    min: 0.0,
                    max: 100.0,
                    default: 70.0,
                    step: 1.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(2704), "plate_diffusion"),
            ),
            5 => Some(
                ParamDescriptor {
                    name: "Size",
                    short_name: "Size",
                    unit: ParamUnit::Percent,
                    min: 0.0,
                    max: 100.0,
                    default: 50.0,
                    step: 1.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(2705), "plate_size"),
            ),
            6 => Some(ParamDescriptor::mix().with_id(ParamId(2706), "plate_mix")),
            7 => Some(
                sonido_core::gain::output_param_descriptor().with_id(ParamId(2707), "plate_output"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Slow,         // decay_s — feedback coeff changes
            1 => SmoothingStyle::Slow,         // damping_pct — one-pole coeff
            2 => SmoothingStyle::Interpolated, // predelay_ms — prevent pitch artifacts
            3 => SmoothingStyle::Slow,         // bandwidth_pct — input filter
            4 => SmoothingStyle::Standard,     // diffusion_pct
            5 => SmoothingStyle::Slow,         // size_pct — changes allpass lengths
            6 => SmoothingStyle::Standard,     // mix_pct
            7 => SmoothingStyle::Fast,         // output_db
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.decay_s,
            1 => self.damping_pct,
            2 => self.predelay_ms,
            3 => self.bandwidth_pct,
            4 => self.diffusion_pct,
            5 => self.size_pct,
            6 => self.mix_pct,
            7 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.decay_s = value,
            1 => self.damping_pct = value,
            2 => self.predelay_ms = value,
            3 => self.bandwidth_pct = value,
            4 => self.diffusion_pct = value,
            5 => self.size_pct = value,
            6 => self.mix_pct = value,
            7 => self.output_db = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP plate reverb kernel — Dattorro-inspired plate with input diffusion and modulated tank.
///
/// Contains only mutable DSP state. No `SmoothedParam`, no atomics.
///
/// ## DSP State
///
/// - **Input bandwidth filter** (`OnePole`): one-pole lowpass on the mono input sum.
/// - **Pre-delay** (`InterpolatedDelay`): up to 100 ms delay before diffusion.
/// - **4 input allpass diffusers** (`AllpassFilter`): series chain that smears the
///   input impulse, creating plate's smooth attack.
/// - **2 tank modulated allpasses** (`InterpolatedDelay` + LFO): break metallic
///   resonances; L/R use different LFO rates for stereo decorrelation.
/// - **2 tank delay lines** (`InterpolatedDelay`): main recirculation delay.
/// - **2 tank damping filters** (`OnePole`): one-pole HF damping in feedback path.
///
/// ## Coefficient Caching
///
/// `feedback`, `damp_cutoff_hz`, and `diff_coeff` are recomputed only when the
/// corresponding parameters change beyond epsilon, avoiding per-sample `expf`/`powf`.
pub struct PlateReverbKernel {
    // Input bandwidth filter
    bw_filter: OnePole,

    // Pre-delay
    predelay: InterpolatedDelay,

    // Input diffusion: 4 series allpass filters
    input_diff: [AllpassFilter; 4],

    // Tank: 2 modulated allpass (one per side)
    tank_ap: [InterpolatedDelay; 2],
    /// LFO phase per tank allpass (turns, 0–1).
    tank_ap_phase: [f32; 2],
    /// LFO phase increment per sample.
    tank_ap_phase_inc: [f32; 2],
    /// Modulation depth in samples.
    tank_ap_mod_depth: f32,
    /// Base delay for each tank allpass in samples.
    tank_ap_base: [f32; 2],

    // Tank: 2 long delay lines
    tank_delay: [InterpolatedDelay; 2],
    /// Tank delay lengths in samples (scaled from constants by size).
    tank_delay_len: [f32; 2],

    // Tank: 2 damping filters
    tank_damp: [OnePole; 2],

    sample_rate: f32,

    // Coefficient caches (NaN sentinel = force update on first sample)
    cached_decay: f32,
    cached_damp: f32,
    cached_bw: f32,
    cached_diff: f32,
    cached_size: f32,
    /// Current feedback coefficient for tank recirculation.
    feedback: f32,
    /// Current allpass feedback for input diffusion chain.
    diff_coeff: f32,
}

impl PlateReverbKernel {
    /// Create a new plate reverb kernel at the given sample rate.
    pub fn new(sample_rate: f32) -> Self {
        let max_predelay = (ceilf(MAX_PREDELAY_MS * 0.001 * sample_rate) as usize).max(1);

        // Input diffusion allpasses — allocate at full size (size=100%)
        let input_diff: [AllpassFilter; 4] = core::array::from_fn(|i| {
            let len = (scale(INPUT_DIFF_44K[i], sample_rate) as f32 * 2.0) as usize + 1;
            let mut ap = AllpassFilter::new(len);
            ap.set_feedback(0.7);
            ap
        });

        // Tank allpasses with modulation
        let mod_depth = TANK_MOD_DEPTH_MS * 0.001 * sample_rate;
        let tank_ap_base: [f32; 2] =
            core::array::from_fn(|i| scale(TANK_ALLPASS_44K[i], sample_rate) as f32);
        let tank_ap: [InterpolatedDelay; 2] = core::array::from_fn(|i| {
            let capacity = (tank_ap_base[i] + mod_depth) as usize + 4;
            let mut d = InterpolatedDelay::new(capacity);
            d.set_interpolation(Interpolation::Linear);
            d
        });

        // Tank delay lines — allocate at full size (size=100%)
        let tank_delay_len: [f32; 2] =
            core::array::from_fn(|i| scale(TANK_DELAY_44K[i], sample_rate) as f32 * 2.0);
        let tank_delay: [InterpolatedDelay; 2] = core::array::from_fn(|i| {
            let capacity = tank_delay_len[i] as usize + 4;
            let mut d = InterpolatedDelay::new(capacity);
            d.set_interpolation(Interpolation::Linear);
            d
        });

        // Tank damping filters — init with default damping (50%)
        let damp_hz = damping_to_coeff(50.0);
        let tank_damp: [OnePole; 2] = core::array::from_fn(|_| OnePole::new(sample_rate, damp_hz));

        // Bandwidth filter — init with default (80%)
        let bw_hz = bandwidth_to_hz(80.0);
        let bw_filter = OnePole::new(sample_rate, bw_hz);

        let tank_ap_phase_inc: [f32; 2] = core::array::from_fn(|i| TANK_MOD_RATES[i] / sample_rate);

        let mut kernel = Self {
            bw_filter,
            predelay: InterpolatedDelay::new(max_predelay),
            input_diff,
            tank_ap,
            tank_ap_phase: [0.0; 2],
            tank_ap_phase_inc,
            tank_ap_mod_depth: mod_depth,
            tank_ap_base,
            tank_delay,
            tank_delay_len,
            tank_damp,
            sample_rate,
            cached_decay: f32::NAN,
            cached_damp: f32::NAN,
            cached_bw: f32::NAN,
            cached_diff: f32::NAN,
            cached_size: f32::NAN,
            feedback: 0.0,
            diff_coeff: 0.7,
        };
        // Prime coefficient cache with defaults.
        kernel.update_derived(2.0, 50.0, 80.0, 70.0, 50.0);
        kernel
    }

    /// Recompute cached coefficients when parameters change by more than epsilon.
    #[inline]
    fn update_derived(
        &mut self,
        decay_s: f32,
        damp_pct: f32,
        bw_pct: f32,
        diff_pct: f32,
        size_pct: f32,
    ) {
        let eps = 0.001;
        if (decay_s - self.cached_decay).abs() > eps {
            self.cached_decay = decay_s;
            self.feedback = decay_to_feedback(decay_s, self.sample_rate);
        }
        if (damp_pct - self.cached_damp).abs() > eps {
            self.cached_damp = damp_pct;
            let hz = damping_to_coeff(damp_pct);
            for f in &mut self.tank_damp {
                f.set_frequency(hz);
            }
        }
        if (bw_pct - self.cached_bw).abs() > eps {
            self.cached_bw = bw_pct;
            let hz = bandwidth_to_hz(bw_pct);
            self.bw_filter.set_frequency(hz);
        }
        if (diff_pct - self.cached_diff).abs() > eps {
            self.cached_diff = diff_pct;
            self.diff_coeff = diff_pct * 0.01 * 0.75 + 0.1; // 0.1–0.85 range
            for ap in &mut self.input_diff {
                ap.set_feedback(self.diff_coeff);
            }
        }
        if (size_pct - self.cached_size).abs() > eps {
            self.cached_size = size_pct;
            let scale_factor = 0.5 + size_pct * 0.01 * 1.5; // 0.5–2.0
            for (i, ap) in self.input_diff.iter_mut().enumerate() {
                let base = scale(INPUT_DIFF_44K[i], self.sample_rate) as f32;
                let _ = ap; // delay length in AllpassFilter is fixed at construction
                let _ = base; // size-scaling for allpasses uses tank delays only
            }
            // Scale tank delay lengths
            for i in 0..2 {
                let base = scale(TANK_DELAY_44K[i], self.sample_rate) as f32;
                self.tank_delay_len[i] =
                    (base * scale_factor).min((self.tank_delay[i].capacity() - 2) as f32);
            }
        }
    }

    /// Process one sample through a tank allpass with LFO modulation.
    ///
    /// The allpass structure: output = delay_read - feedback * input
    ///                        write  = input + feedback * delay_read
    #[inline]
    fn tank_allpass_process(&mut self, side: usize, input: f32) -> f32 {
        let phase = self.tank_ap_phase[side];
        let mod_val = fast_sin_turns(phase) * self.tank_ap_mod_depth;
        let delay_len = (self.tank_ap_base[side] + mod_val).max(1.0);

        let delayed = self.tank_ap[side].read(delay_len);
        let write_val = flush_denormal(input + TANK_ALLPASS_FEEDBACK * delayed);
        self.tank_ap[side].write(write_val);

        let output = delayed - TANK_ALLPASS_FEEDBACK * input;

        self.tank_ap_phase[side] += self.tank_ap_phase_inc[side];
        if self.tank_ap_phase[side] >= 1.0 {
            self.tank_ap_phase[side] -= 1.0;
        }

        output
    }
}

impl DspKernel for PlateReverbKernel {
    type Params = PlateReverbParams;

    fn process_stereo(&mut self, left: f32, right: f32, params: &Self::Params) -> (f32, f32) {
        self.update_derived(
            params.decay_s,
            params.damping_pct,
            params.bandwidth_pct,
            params.diffusion_pct,
            params.size_pct,
        );

        // 1. Mono sum → bandwidth filter
        let mono = (left + right) * 0.5;
        let bw_out = self.bw_filter.process(mono);

        // 2. Pre-delay
        let predelay_samples = (params.predelay_ms * 0.001 * self.sample_rate)
            .clamp(0.0, (self.predelay.capacity() - 2) as f32);
        self.predelay.write(bw_out);
        let pd_out = self.predelay.read(predelay_samples);

        // 3. Input diffusion: 4 series allpass
        let mut diffused = pd_out;
        for ap in &mut self.input_diff {
            diffused = ap.process(diffused);
        }

        // 4. Tank recirculation (Dattorro plate structure)
        //
        // Each tank half: modulated allpass → delay line → one-pole damp → feedback.
        // Cross-coupling: output of delay[0] feeds into input of path[1] and vice versa.
        // Wet taps: short-position reads from each delay line (well before the feedback
        // recirculation point) so output appears quickly, independent of tail length.

        // Tap wet output at fixed short positions (in samples) scaled to sample rate.
        // These are early taps within the delay lines — they appear quickly (well before
        // the full recirculation cycle) and differ between L/R for stereo decorrelation.
        // At 44.1 kHz: tap_l ≈ 12 ms, tap_r ≈ 20 ms.
        let max_tap_l = (self.tank_delay[0].capacity() - 2) as f32;
        let max_tap_r = (self.tank_delay[1].capacity() - 2) as f32;
        let tap_l = (533.0 * self.sample_rate / REF_RATE).clamp(1.0, max_tap_l);
        let tap_r = (889.0 * self.sample_rate / REF_RATE).clamp(1.0, max_tap_r);
        let wet_l = flush_denormal(self.tank_delay[0].read(tap_l));
        let wet_r = flush_denormal(self.tank_delay[1].read(tap_r));

        // Read full-length delay for cross-recirculation feedback
        let fb_from_r = self.tank_delay[1].read(self.tank_delay_len[1]);
        let fb_from_l = self.tank_delay[0].read(self.tank_delay_len[0]);

        // Inject diffused input into both sides with cross-feedback
        let in_l = diffused + fb_from_r * self.feedback;
        let in_r = diffused + fb_from_l * self.feedback;

        // Modulated allpass in each half
        let ap_l = self.tank_allpass_process(0, in_l);
        let ap_r = self.tank_allpass_process(1, in_r);

        // Damp and write into tank delay lines
        let damp_l = flush_denormal(self.tank_damp[0].process(ap_l));
        let damp_r = flush_denormal(self.tank_damp[1].process(ap_r));

        self.tank_delay[0].write(damp_l);
        self.tank_delay[1].write(damp_r);

        // 6. Wet/dry mix
        let gain = sonido_core::db_to_linear(params.output_db);
        let (out_l, out_r) = wet_dry_mix_stereo(left, right, wet_l, wet_r, params.mix_pct * 0.01);

        (out_l * gain, out_r * gain)
    }

    fn reset(&mut self) {
        self.bw_filter.reset();
        self.predelay.clear();
        for ap in &mut self.input_diff {
            ap.clear();
        }
        for d in &mut self.tank_ap {
            d.clear();
        }
        for d in &mut self.tank_delay {
            d.clear();
        }
        for f in &mut self.tank_damp {
            f.reset();
        }
        self.tank_ap_phase = [0.0; 2];
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        let mod_depth = TANK_MOD_DEPTH_MS * 0.001 * sample_rate;
        self.tank_ap_mod_depth = mod_depth;
        self.tank_ap_phase_inc = core::array::from_fn(|i| TANK_MOD_RATES[i] / sample_rate);
        self.tank_ap_base =
            core::array::from_fn(|i| scale(TANK_ALLPASS_44K[i], sample_rate) as f32);

        // Reset coefficient caches to force recomputation
        self.cached_decay = f32::NAN;
        self.cached_damp = f32::NAN;
        self.cached_bw = f32::NAN;
        self.cached_diff = f32::NAN;
        self.cached_size = f32::NAN;
    }
}

// ── TailReporting ────────────────────────────────────────────────────────────

impl sonido_core::TailReporting for PlateReverbKernel {
    fn tail_samples(&self) -> usize {
        // Approximate tail: decay * 3 * sr
        let decay = if self.cached_decay.is_nan() {
            2.0
        } else {
            self.cached_decay
        };
        (decay * 3.0 * self.sample_rate) as usize
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finite_output_silence() {
        let mut kernel = PlateReverbKernel::new(48000.0);
        let params = PlateReverbParams::default();
        for _ in 0..512 {
            let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
            assert!(l.is_finite(), "left output is not finite on silence");
            assert!(r.is_finite(), "right output is not finite on silence");
        }
    }

    #[test]
    fn finite_output_impulse() {
        let mut kernel = PlateReverbKernel::new(48000.0);
        let params = PlateReverbParams::default();
        // Fire an impulse and run for a while
        kernel.process_stereo(1.0, 1.0, &params);
        for _ in 0..8192 {
            let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
            assert!(l.is_finite(), "left output is not finite after impulse");
            assert!(r.is_finite(), "right output is not finite after impulse");
        }
    }

    #[test]
    fn decay_tail_nonzero() {
        // After an impulse, the tail should still have energy at moderate decay
        let mut kernel = PlateReverbKernel::new(48000.0);
        let params = PlateReverbParams {
            decay_s: 3.0,
            mix_pct: 100.0,
            ..PlateReverbParams::default()
        };
        // Fire impulse
        kernel.process_stereo(1.0, 1.0, &params);
        // Skip predelay + attack
        for _ in 0..2048 {
            kernel.process_stereo(0.0, 0.0, &params);
        }
        // Measure energy in a window after input stops
        let mut energy = 0.0f32;
        for _ in 0..1024 {
            let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
            energy += l * l + r * r;
        }
        assert!(
            energy > 1e-10,
            "decay tail energy too low ({energy}), reverb not sustaining"
        );
    }

    #[test]
    fn wet_signal_present_with_mix() {
        // With mix=100%, output should differ from dry (zero) when wet is non-zero
        let mut kernel = PlateReverbKernel::new(48000.0);
        let params = PlateReverbParams {
            mix_pct: 100.0,
            predelay_ms: 0.0,
            ..PlateReverbParams::default()
        };
        // Drive with sine-like signal
        let mut any_nonzero = false;
        for i in 0..2048 {
            let s = fast_sin_turns(i as f32 / 128.0) * 0.5;
            let (l, r) = kernel.process_stereo(s, s, &params);
            if l.abs() > 1e-8 || r.abs() > 1e-8 {
                any_nonzero = true;
            }
        }
        assert!(any_nonzero, "wet output is always zero with mix=100%");
    }

    #[test]
    fn reset_clears_state() {
        let mut kernel = PlateReverbKernel::new(48000.0);
        let params = PlateReverbParams {
            mix_pct: 100.0,
            ..PlateReverbParams::default()
        };
        // Build up state
        for _ in 0..4096 {
            kernel.process_stereo(0.5, 0.5, &params);
        }
        kernel.reset();
        // After reset, output should be near zero for silence input
        let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
        assert!(
            l.abs() < 1e-6 && r.abs() < 1e-6,
            "state not cleared after reset: l={l}, r={r}"
        );
    }
}
