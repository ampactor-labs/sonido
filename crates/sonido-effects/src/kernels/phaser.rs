//! Phaser kernel — pure DSP with separated parameter ownership.
//!
//! This is the kernel-architecture equivalent of [`Phaser`](crate::Phaser).
//! The DSP math is identical; the difference is structural:
//!
//! - **Classic `Phaser`**: owns `SmoothedParam` for rate/depth/feedback/mix/output,
//!   manages smoothing internally, implements `Effect` + `ParameterInfo` directly.
//!
//! - **`PhaserKernel`**: owns ONLY DSP state (allpass arrays, LFOs, feedback samples,
//!   tempo manager, coefficient decimation counter).
//!   Parameters are received via `&PhaserParams` on each processing call.
//!   Deployed via [`KernelAdapter`](sonido_core::KernelAdapter) for desktop/plugin,
//!   or called directly on embedded targets.
//!
//! # Signal Flow
//!
//! ```text
//! Input → (+ feedback) → Allpass Cascade (N stages, LFO-swept) → feedback store
//!                                                               ↓
//!                                     Feedback Compensation → Wet/Dry Mix → Output Level
//! ```
//!
//! Left and right channels use independent allpass arrays with 90-degree offset LFOs,
//! producing true stereo phase decorrelation.
//!
//! # Phaser DSP Theory
//!
//! A phaser creates notches in the frequency spectrum by mixing the direct signal
//! with a phase-shifted version. Each first-order allpass filter contributes a 180°
//! phase shift at its centre frequency. Cascading N stages creates N/2 notches.
//! The allpass centre frequencies are modulated by an LFO, causing the notches to
//! sweep up and down — the characteristic "swoosh" sound.
//!
//! The first-order allpass transfer function is:
//!
//! ```text
//! H(z) = (a + z⁻¹) / (1 + a·z⁻¹),   where  a = (tan(π·fc/fs) − 1) / (tan(π·fc/fs) + 1)
//! ```
//!
//! References:
//! - Zölzer, "DAFX: Digital Audio Effects" (2011), Ch. 7 (Modulators and Demodulators).
//! - Steiglitz, "A Digital Signal Processing Primer" (1996), allpass sections.
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! let adapter = KernelAdapter::new(PhaserKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing, ADCs are hardware-filtered)
//! let mut kernel = PhaserKernel::new(48000.0);
//! let params = PhaserParams::from_knobs(adc_rate, adc_depth, adc_stg, adc_fb, adc_mix);
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

use core::f32::consts::PI;
use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::{
    DIVISION_LABELS, Lfo, ParamDescriptor, ParamFlags, ParamId, ParamScale, ParamUnit,
    TempoContext, TempoManager, fast_exp2, fast_log2, fast_tan, flush_denormal, index_to_division,
    wet_dry_mix,
};

// ─── Constants ───────────────────────────────────────────────────────────────

/// Maximum number of allpass stages supported.
const MAX_STAGES: usize = 12;

/// Allpass coefficient update decimation interval (in samples).
///
/// At 48 kHz this gives ~0.67 ms between updates — fast enough that the sweep
/// sounds continuous while saving 31/32 of the `fast_tan` evaluations.
const COEFF_UPDATE_INTERVAL: u32 = 32;

// ─── Unit conversion ─────────────────────────────────────────────────────────

/// Fast dB-to-linear conversion for per-sample use.
///
/// Uses `sonido_core::fast_db_to_linear` which is a polynomial approximation
/// (~0.1 dB accuracy, ~4× faster than `10^(db/20)`).
#[inline]
fn db_to_gain(db: f32) -> f32 {
    sonido_core::fast_db_to_linear(db)
}

// ═══════════════════════════════════════════════════════════════════════════
//  First-order allpass filter (local — kernel-private)
// ═══════════════════════════════════════════════════════════════════════════

/// First-order allpass filter for phaser stages.
///
/// Implements the difference equation:
///
/// ```text
/// y[n] = a · x[n] + x[n−1] − a · y[n−1]
/// ```
///
/// where `a = (tan(π·fc/fs) − 1) / (tan(π·fc/fs) + 1)`.
///
/// At `fc` the allpass contributes exactly 90° of phase shift.
/// Cascading N such stages creates notches in the frequency response when
/// mixed with the direct signal.
///
/// Reference: Zölzer, "DAFX" (2011), Ch. 7.
#[derive(Debug, Clone, Copy, Default)]
struct FirstOrderAllpass {
    /// Allpass coefficient — determines the centre frequency.
    a: f32,
    /// Previous input sample (x[n−1]).
    x1: f32,
    /// Previous output sample (y[n−1]).
    y1: f32,
}

impl FirstOrderAllpass {
    /// Create a zeroed first-order allpass filter.
    fn new() -> Self {
        Self {
            a: 0.0,
            x1: 0.0,
            y1: 0.0,
        }
    }

    /// Set the allpass centre frequency.
    ///
    /// Computes `a = (tan(π·freq/sample_rate) − 1) / (tan(π·freq/sample_rate) + 1)`.
    /// Frequency is clamped to [10 Hz, 0.4 × sample_rate] to keep `a` in (−1, 1).
    ///
    /// Uses `fast_tan` (polynomial approximation, ~3× faster than `libm::tanf`).
    ///
    /// Range: `freq` — 10 Hz to Nyquist/2 (clamped).
    #[inline]
    fn set_frequency(&mut self, freq: f32, sample_rate: f32) {
        let freq = freq.clamp(10.0, sample_rate * 0.4);
        let tan_val = fast_tan(PI * freq / sample_rate);
        self.a = (tan_val - 1.0) / (tan_val + 1.0);
    }

    /// Process a single sample through the allpass.
    ///
    /// Returns `a·x[n] + x[n−1] − a·y[n−1]` and updates history.
    #[inline]
    fn process(&mut self, input: f32) -> f32 {
        let output = self.a * input + self.x1 - self.a * self.y1;
        self.x1 = input;
        self.y1 = output;
        output
    }

    /// Clear filter history (x[n−1] and y[n−1] → 0).
    fn clear(&mut self) {
        self.x1 = 0.0;
        self.y1 = 0.0;
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`PhaserKernel`].
///
/// All values in **user-facing units** — the same units shown in GUIs and
/// stored in presets. The kernel converts internally as needed.
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `rate` | Hz | 0.05–5.0 | 0.3 |
/// | 1 | `depth_pct` | % | 0–100 | 50.0 |
/// | 2 | `stages` | count | 2–12 (step 2) | 6.0 |
/// | 3 | `feedback_pct` | % | 0–95 | 50.0 |
/// | 4 | `mix_pct` | % | 0–100 | 50.0 |
/// | 5 | `min_freq` | Hz | 20–2000 | 200.0 |
/// | 6 | `max_freq` | Hz | 200–20000 | 4000.0 |
/// | 7 | `sync` | index | 0–1 (Off/On) | 0.0 |
/// | 8 | `division` | index | 0–11 (note division) | 3.0 |
/// | 9 | `output_db` | dB | −20–20 | 0.0 |
#[derive(Debug, Clone, Copy)]
pub struct PhaserParams {
    /// LFO rate in Hz.
    ///
    /// Range: 0.05 to 5.0 Hz. Ignored when `sync` is non-zero.
    pub rate: f32,
    /// Modulation depth in percent.
    ///
    /// Range: 0–100 %. At 0 % all allpasses are pinned to `min_freq`;
    /// at 100 % they sweep the full `min_freq`–`max_freq` range.
    pub depth_pct: f32,
    /// Number of active allpass stages.
    ///
    /// Range: 2–12 (integer-valued float, step 2).
    /// More stages = more notches = denser phasing character.
    pub stages: f32,
    /// Feedback amount in percent.
    ///
    /// Range: 0–95 %. Higher values create a more resonant, pronounced sweep.
    pub feedback_pct: f32,
    /// Wet/dry mix in percent.
    ///
    /// Range: 0–100 %. 0 % = fully dry (passthrough), 100 % = fully wet (phased only).
    pub mix_pct: f32,
    /// Minimum allpass sweep frequency in Hz.
    ///
    /// Range: 20–2000 Hz (Logarithmic scale). Sets the low end of the LFO sweep.
    pub min_freq: f32,
    /// Maximum allpass sweep frequency in Hz.
    ///
    /// Range: 200–20000 Hz (Logarithmic scale). Sets the high end of the LFO sweep.
    pub max_freq: f32,
    /// Tempo sync enable: 0.0 = Off, 1.0 = On.
    ///
    /// When set to 1.0 the LFO rate is derived from BPM + `division` instead of `rate`.
    pub sync: f32,
    /// Note division index for tempo sync.
    ///
    /// Range: 0–11. Only used when `sync` is non-zero.
    /// Index mapping follows `DIVISION_LABELS` / `index_to_division()`.
    pub division: f32,
    /// Output level in decibels.
    ///
    /// Range: −20 to +20 dB. Applied after wet/dry mix.
    pub output_db: f32,
}

impl Default for PhaserParams {
    fn default() -> Self {
        Self {
            rate: 0.3,
            depth_pct: 50.0,
            stages: 6.0,
            feedback_pct: 50.0,
            mix_pct: 50.0,
            min_freq: 200.0,
            max_freq: 4000.0,
            sync: 0.0,
            division: 3.0,
            output_db: 0.0,
        }
    }
}

impl PhaserParams {
    /// Build params directly from hardware knob readings (0.0–1.0 normalized).
    ///
    /// Maps ADC readings to user-facing parameter ranges for embedded deployment.
    /// All inputs are expected in `[0.0, 1.0]`.
    ///
    /// # Parameter mapping
    ///
    /// - `rate`     → 0.05–5.0 Hz  (linear)
    /// - `depth`    → 0–100 %      (linear)
    /// - `stages`   → 2–12         (maps to even integer: 2 4 6 8 10 12)
    /// - `feedback` → 0–95 %       (linear)
    /// - `mix`      → 0–100 %      (linear)
    /// - `min_freq` → 20–2000 Hz   (linear; log feels more natural but hardware ADC is linear)
    /// - `max_freq` → 200–20000 Hz (linear)
    ///
    /// Sync and division are fixed at Off / Quarter for embedded use
    /// (no BPM source available on standalone hardware).
    pub fn from_knobs(
        rate: f32,
        depth: f32,
        stages: f32,
        feedback: f32,
        mix: f32,
        min_freq: f32,
        max_freq: f32,
    ) -> Self {
        // Stages: 6 even steps across [0,1] → 2, 4, 6, 8, 10, 12
        let stages_raw = libm::floorf(stages * 5.99) as u32;
        let stages_val = ((stages_raw + 1) * 2) as f32; // 2,4,6,8,10,12
        Self {
            rate: 0.05 + rate * (5.0 - 0.05), // 0.05–5.0 Hz
            depth_pct: depth * 100.0,         // 0–100 %
            stages: stages_val.clamp(2.0, 12.0),
            feedback_pct: feedback * 95.0,        // 0–95 %
            mix_pct: mix * 100.0,                 // 0–100 %
            min_freq: 20.0 + min_freq * 1980.0,   // 20–2000 Hz
            max_freq: 200.0 + max_freq * 19800.0, // 200–20000 Hz
            sync: 0.0,
            division: 3.0, // Quarter note — sensible embedded default
            output_db: 0.0,
        }
    }
}

impl KernelParams for PhaserParams {
    const COUNT: usize = 10;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            // Index 0: Rate — must match classic Phaser impl_params! ParamId(900)
            0 => Some(ParamDescriptor::rate_hz(0.05, 5.0, 0.3).with_id(ParamId(900), "phsr_rate")),
            // Index 1: Depth — ParamId(901)
            1 => Some(ParamDescriptor::depth().with_id(ParamId(901), "phsr_depth")),
            // Index 2: Stages — ParamId(902), STEPPED, step=2
            2 => Some(
                ParamDescriptor {
                    name: "Stages",
                    short_name: "Stg",
                    unit: ParamUnit::None,
                    min: 2.0,
                    max: 12.0,
                    default: 6.0,
                    step: 2.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(902), "phsr_stages")
                .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED)),
            ),
            // Index 3: Feedback — ParamId(903)
            3 => Some(ParamDescriptor::feedback().with_id(ParamId(903), "phsr_feedback")),
            // Index 4: Mix — ParamId(904)
            4 => Some(ParamDescriptor::mix().with_id(ParamId(904), "phsr_mix")),
            // Index 5: Min Freq — ParamId(906) (note: output is 905, min/max come before it)
            5 => Some(
                ParamDescriptor::custom("Min Freq", "MinF", 20.0, 2000.0, 200.0)
                    .with_id(ParamId(906), "phsr_min_freq")
                    .with_unit(ParamUnit::Hertz)
                    .with_scale(ParamScale::Logarithmic),
            ),
            // Index 6: Max Freq — ParamId(907)
            6 => Some(
                ParamDescriptor::custom("Max Freq", "MaxF", 200.0, 20000.0, 4000.0)
                    .with_id(ParamId(907), "phsr_max_freq")
                    .with_unit(ParamUnit::Hertz)
                    .with_scale(ParamScale::Logarithmic),
            ),
            // Index 7: Sync — ParamId(908), STEPPED
            7 => Some(
                ParamDescriptor::custom("Sync", "Sync", 0.0, 1.0, 0.0)
                    .with_step(1.0)
                    .with_id(ParamId(908), "phsr_sync")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(&["Off", "On"]),
            ),
            // Index 8: Division — ParamId(909), STEPPED
            8 => Some(
                ParamDescriptor::custom("Division", "Div", 0.0, 11.0, 3.0)
                    .with_step(1.0)
                    .with_id(ParamId(909), "phsr_division")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(DIVISION_LABELS),
            ),
            // Index 9: Output — ParamId(905)
            9 => Some(
                sonido_core::gain::output_param_descriptor().with_id(ParamId(905), "phsr_output"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Standard, // rate — LFO frequency, gradual feel
            1 => SmoothingStyle::Standard, // depth
            2 => SmoothingStyle::None,     // stages — discrete/stepped, snap
            3 => SmoothingStyle::Standard, // feedback
            4 => SmoothingStyle::Standard, // mix
            5 => SmoothingStyle::Slow,     // min_freq — filter coefficient, avoid zipper
            6 => SmoothingStyle::Slow,     // max_freq — filter coefficient, avoid zipper
            7 => SmoothingStyle::None,     // sync — on/off toggle, snap
            8 => SmoothingStyle::None,     // division — stepped, snap
            9 => SmoothingStyle::Standard, // output level
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.rate,
            1 => self.depth_pct,
            2 => self.stages,
            3 => self.feedback_pct,
            4 => self.mix_pct,
            5 => self.min_freq,
            6 => self.max_freq,
            7 => self.sync,
            8 => self.division,
            9 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.rate = value,
            1 => self.depth_pct = value,
            2 => self.stages = value,
            3 => self.feedback_pct = value,
            4 => self.mix_pct = value,
            5 => self.min_freq = value,
            6 => self.max_freq = value,
            7 => self.sync = value,
            8 => self.division = value,
            9 => self.output_db = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP phaser kernel.
///
/// Contains ONLY the mutable state required for audio processing:
/// - Two `MAX_STAGES`-element allpass arrays (L and R channels)
/// - Two LFOs with 90° stereo spread (L and R)
/// - Feedback sample memories for L and R
/// - Coefficient decimation counter (updates allpass `a` every 32 samples)
/// - `TempoManager` for tempo-synced LFO rates
/// - Sample rate
///
/// No `SmoothedParam`, no `AtomicU32`, no platform awareness.
pub struct PhaserKernel {
    /// Allpass stages for the left channel.
    allpass: [FirstOrderAllpass; MAX_STAGES],
    /// Allpass stages for the right channel.
    allpass_r: [FirstOrderAllpass; MAX_STAGES],
    /// LFO for the left channel.
    lfo: Lfo,
    /// LFO for the right channel (initialized with 0.25 phase — 90° offset).
    lfo_r: Lfo,
    /// Last output of the left allpass cascade (for feedback).
    feedback_sample: f32,
    /// Last output of the right allpass cascade (for feedback).
    feedback_sample_r: f32,
    /// Fixed stereo LFO phase spread (0.25 = 90°).
    stereo_spread: f32,
    /// Down-counter for allpass coefficient decimation.
    ///
    /// Starts at 1 so the first `wrapping_sub(1)` → 0 triggers an immediate update.
    coeff_update_counter: u32,
    /// Tempo manager for synced LFO rates.
    tempo: TempoManager,
    /// Audio sample rate in Hz.
    sample_rate: f32,
}

impl PhaserKernel {
    /// Create a new phaser kernel at the given sample rate.
    ///
    /// The right-channel LFO is initialised with a 0.25 phase offset
    /// (90° — matching the classic `Phaser::new()` behaviour).
    ///
    /// # Parameters
    ///
    /// - `sample_rate`: Audio sample rate in Hz (e.g. 44100.0, 48000.0, 96000.0).
    pub fn new(sample_rate: f32) -> Self {
        let mut lfo_r = Lfo::new(sample_rate, 0.3);
        lfo_r.set_phase(0.25); // 90° stereo spread

        Self {
            allpass: [FirstOrderAllpass::new(); MAX_STAGES],
            allpass_r: [FirstOrderAllpass::new(); MAX_STAGES],
            lfo: Lfo::new(sample_rate, 0.3),
            lfo_r,
            feedback_sample: 0.0,
            feedback_sample_r: 0.0,
            stereo_spread: 0.25,
            coeff_update_counter: 1, // triggers immediate coefficient update on first sample
            tempo: TempoManager::new(sample_rate, 120.0),
            sample_rate,
        }
    }
}

impl DspKernel for PhaserKernel {
    type Params = PhaserParams;

    /// Process a stereo sample pair.
    ///
    /// # Algorithm
    ///
    /// 1. **Tempo sync** — if `params.sync > 0.5`, derive LFO rate from the
    ///    `TempoManager` and `params.division`; otherwise use `params.rate` directly.
    /// 2. **LFO advance** — both LFOs advance every sample to keep phase correct.
    /// 3. **Coefficient decimation** — allpass `a` coefficients are recomputed
    ///    every `COEFF_UPDATE_INTERVAL` samples to save CPU on the expensive
    ///    `fast_tan` calls. Provides ~0.67 ms update granularity at 48 kHz.
    /// 4. **Frequency mapping** — centre freq uses exponential mapping:
    ///    `f = min_freq · 2^(log2(max/min) · lfo · depth)`.
    ///    Each stage is slightly offset by `1 + i·0.1` for a richer sound.
    /// 5. **Feedback + allpass cascade** — input mixed with `feedback_pct`% of the
    ///    previous cascade output, then processed through N active stages.
    /// 6. **Mix + output** — `feedback_wet_compensation` + `wet_dry_mix` + output gain.
    fn process_stereo(&mut self, left: f32, right: f32, params: &PhaserParams) -> (f32, f32) {
        // ── Unit conversion (user-facing → internal) ──
        let depth = params.depth_pct / 100.0;
        let feedback = params.feedback_pct / 100.0;
        let mix = params.mix_pct / 100.0;
        let output_gain = db_to_gain(params.output_db);
        let stages = (params.stages as usize).clamp(2, MAX_STAGES);

        // ── LFO rate: either free-running or tempo-synced ──
        let rate = if params.sync > 0.5 {
            let div = index_to_division(params.division as u8);
            self.tempo.division_to_hz(div).clamp(0.05, 5.0)
        } else {
            params.rate.clamp(0.05, 5.0)
        };

        // ── LFO advance (must happen every sample to keep phase correct) ──
        self.lfo.set_frequency(rate);
        self.lfo_r.set_frequency(rate);
        let lfo_l = self.lfo.advance_unipolar();
        let lfo_r = self.lfo_r.advance_unipolar();

        // ── Allpass coefficient update (decimated) ──
        self.coeff_update_counter = self.coeff_update_counter.wrapping_sub(1);
        if self.coeff_update_counter == 0 {
            self.coeff_update_counter = COEFF_UPDATE_INTERVAL;

            let freq_ratio = params.max_freq / params.min_freq.max(1.0);
            let log_ratio = fast_log2(freq_ratio);

            // Exponential LFO mapping: perceptually natural frequency sweep.
            // f = min_freq · 2^(log2(max/min) · lfo · depth)
            let center_freq_l = params.min_freq * fast_exp2(log_ratio * lfo_l * depth);
            let center_freq_r = params.min_freq * fast_exp2(log_ratio * lfo_r * depth);

            for i in 0..stages {
                // Slight per-stage offset creates richer, more complex notch pattern.
                let stage_offset = 1.0 + (i as f32 * 0.1);
                self.allpass[i].set_frequency(center_freq_l * stage_offset, self.sample_rate);
                self.allpass_r[i].set_frequency(center_freq_r * stage_offset, self.sample_rate);
            }
        }

        // ── Left channel: feedback → allpass cascade ──
        let input_l = left + self.feedback_sample * feedback;
        let mut wet_l = input_l;
        for i in 0..stages {
            wet_l = self.allpass[i].process(wet_l);
        }
        self.feedback_sample = flush_denormal(wet_l);

        // ── Right channel: feedback → allpass cascade ──
        let input_r = right + self.feedback_sample_r * feedback;
        let mut wet_r = input_r;
        for i in 0..stages {
            wet_r = self.allpass_r[i].process(wet_r);
        }
        self.feedback_sample_r = flush_denormal(wet_r);

        // ── Feedback compensation + wet/dry mix + output level ──
        let comp = sonido_core::gain::feedback_wet_compensation(feedback);
        let out_l = wet_dry_mix(left, wet_l * comp, mix) * output_gain;
        let out_r = wet_dry_mix(right, wet_r * comp, mix) * output_gain;

        (out_l, out_r)
    }

    fn reset(&mut self) {
        // Clear all allpass filter state.
        for i in 0..MAX_STAGES {
            self.allpass[i].clear();
            self.allpass_r[i].clear();
        }

        // Reset LFOs; restore the fixed stereo spread offset on the right LFO.
        self.lfo.reset();
        self.lfo_r.reset();
        self.lfo_r.set_phase(self.stereo_spread);

        // Clear feedback memory.
        self.feedback_sample = 0.0;
        self.feedback_sample_r = 0.0;

        // Reset decimation counter so the first sample triggers an immediate
        // coefficient update (wrapping_sub(1) from 1 → 0).
        self.coeff_update_counter = 1;
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.lfo.set_sample_rate(sample_rate);
        self.lfo_r.set_sample_rate(sample_rate);
        self.tempo.set_sample_rate(sample_rate);
    }

    /// Phaser has zero lookahead latency.
    fn latency_samples(&self) -> usize {
        0
    }

    /// True stereo: left and right LFOs are phase-offset, producing decorrelated
    /// allpass sweeps on each channel.
    fn is_true_stereo(&self) -> bool {
        true
    }

    /// Forward tempo context to the internal `TempoManager`.
    ///
    /// Called once per block by the adapter (or host) when BPM changes.
    /// The updated rate is applied on the next `process_stereo()` call when
    /// `params.sync > 0.5`.
    fn set_tempo_context(&mut self, ctx: &TempoContext) {
        self.tempo.set_bpm(ctx.bpm);
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

    // ── Kernel unit tests ─────────────────────────────────────────────────

    /// Silence in must produce silence out.
    ///
    /// With zero input and zero initial feedback the allpass cascade and mix
    /// produce zero output regardless of parameter values.
    #[test]
    fn silence_in_silence_out() {
        let mut kernel = PhaserKernel::new(48000.0);
        let params = PhaserParams::default();

        for _ in 0..100 {
            let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
            assert!(l.abs() < 1e-9, "Expected silence L, got {l}");
            assert!(r.abs() < 1e-9, "Expected silence R, got {r}");
        }
    }

    /// No sample should ever produce NaN or infinity.
    #[test]
    fn no_nan_or_inf() {
        let mut kernel = PhaserKernel::new(48000.0);
        let params = PhaserParams::default();

        for _ in 0..1000 {
            let (l, r) = kernel.process_stereo(0.5, -0.3, &params);
            assert!(!l.is_nan(), "NaN in left output");
            assert!(!r.is_nan(), "NaN in right output");
            assert!(l.is_finite(), "Inf in left output");
            assert!(r.is_finite(), "Inf in right output");
        }
    }

    /// `COUNT` must be 10 and every index 0–9 must have a descriptor.
    #[test]
    fn params_descriptor_count() {
        assert_eq!(PhaserParams::COUNT, 10);

        for i in 0..10 {
            assert!(
                PhaserParams::descriptor(i).is_some(),
                "descriptor({i}) should return Some, got None"
            );
        }

        // Index beyond COUNT must return None.
        assert!(
            PhaserParams::descriptor(10).is_none(),
            "descriptor(10) should be None"
        );
    }

    /// High feedback with maximum stages must remain stable over thousands of samples.
    ///
    /// The phaser uses a clamped feedback coefficient (≤ 0.95) and
    /// `flush_denormal` to prevent unbounded growth.
    #[test]
    fn feedback_stability() {
        let mut kernel = PhaserKernel::new(48000.0);
        let params = PhaserParams {
            feedback_pct: 95.0,
            stages: 12.0,
            mix_pct: 100.0,
            ..PhaserParams::default()
        };

        for _ in 0..20_000 {
            let (l, r) = kernel.process_stereo(0.1, 0.1, &params);
            assert!(
                l.is_finite(),
                "Left output became non-finite under high feedback"
            );
            assert!(
                r.is_finite(),
                "Right output became non-finite under high feedback"
            );
            assert!(
                l.abs() < 10.0,
                "Left output exceeded bounds under high feedback: {l}"
            );
            assert!(
                r.abs() < 10.0,
                "Right output exceeded bounds under high feedback: {r}"
            );
        }
    }

    /// With full wet mix the phaser must alter the signal from the raw input.
    ///
    /// Checks that the allpass cascade actually modifies the signal over time.
    #[test]
    fn phaser_modifies_signal() {
        let mut kernel = PhaserKernel::new(48000.0);
        let params = PhaserParams {
            mix_pct: 100.0,
            depth_pct: 100.0,
            ..PhaserParams::default()
        };

        let input = 0.5_f32;
        let mut any_different = false;

        for _ in 0..1000 {
            let (l, _) = kernel.process_stereo(input, input, &params);
            if (l - input).abs() > 0.001 {
                any_different = true;
                break;
            }
        }

        assert!(
            any_different,
            "Phaser at 100% wet should modify the signal from the dry input"
        );
    }

    /// Different stage counts must produce different output.
    ///
    /// More stages = more allpass delays = different spectral character.
    #[test]
    fn different_stage_counts_differ() {
        let input = 0.4_f32;

        let collect_rms = |stages: f32| -> f32 {
            let mut kernel = PhaserKernel::new(48000.0);
            let params = PhaserParams {
                stages,
                mix_pct: 100.0,
                depth_pct: 80.0,
                rate: 2.0,
                ..PhaserParams::default()
            };
            // Settle a few hundred samples first
            for _ in 0..500 {
                kernel.process_stereo(input, input, &params);
            }
            let sum_sq: f32 = (0..2048)
                .map(|_| {
                    let (l, _) = kernel.process_stereo(input, input, &params);
                    l * l
                })
                .sum();
            (sum_sq / 2048.0).sqrt()
        };

        let rms_2 = collect_rms(2.0);
        let rms_12 = collect_rms(12.0);

        assert!(
            (rms_2 - rms_12).abs() > 1e-4,
            "2-stage and 12-stage phaser should produce different RMS: rms_2={rms_2}, rms_12={rms_12}"
        );
    }

    // ── Adapter integration tests ─────────────────────────────────────────

    /// Kernel wrapped in `KernelAdapter` must function as a standard `Effect`.
    #[test]
    fn adapter_wraps_as_effect() {
        let kernel = PhaserKernel::new(48000.0);
        let mut adapter = KernelAdapter::new(kernel, 48000.0);

        adapter.reset();
        let output = adapter.process(0.5);
        assert!(!output.is_nan(), "Adapter output is NaN");
        assert!(output.is_finite(), "Adapter output is infinite");
    }

    /// `KernelAdapter` must expose the same 10 parameters with matching `ParamId`s.
    #[test]
    fn adapter_param_info_matches() {
        let kernel = PhaserKernel::new(48000.0);
        let adapter = KernelAdapter::new(kernel, 48000.0);

        assert_eq!(adapter.param_count(), 10, "Should expose exactly 10 params");

        // Rate (index 0)
        let rate_desc = adapter.param_info(0).unwrap();
        assert_eq!(rate_desc.name, "Rate");
        assert_eq!(rate_desc.id, ParamId(900));
        assert!((rate_desc.min - 0.05).abs() < 0.001);
        assert!((rate_desc.max - 5.0).abs() < 0.001);

        // Depth (index 1)
        let depth_desc = adapter.param_info(1).unwrap();
        assert_eq!(depth_desc.id, ParamId(901));

        // Stages (index 2) — must be STEPPED
        let stages_desc = adapter.param_info(2).unwrap();
        assert_eq!(stages_desc.id, ParamId(902));
        assert!(
            stages_desc.flags.contains(ParamFlags::STEPPED),
            "Stages must be STEPPED"
        );

        // Feedback (index 3)
        let fb_desc = adapter.param_info(3).unwrap();
        assert_eq!(fb_desc.id, ParamId(903));

        // Mix (index 4)
        let mix_desc = adapter.param_info(4).unwrap();
        assert_eq!(mix_desc.id, ParamId(904));

        // Min Freq (index 5)
        let min_f_desc = adapter.param_info(5).unwrap();
        assert_eq!(min_f_desc.id, ParamId(906));
        assert_eq!(min_f_desc.scale, ParamScale::Logarithmic);

        // Max Freq (index 6)
        let max_f_desc = adapter.param_info(6).unwrap();
        assert_eq!(max_f_desc.id, ParamId(907));

        // Sync (index 7) — must be STEPPED
        let sync_desc = adapter.param_info(7).unwrap();
        assert_eq!(sync_desc.id, ParamId(908));
        assert!(
            sync_desc.flags.contains(ParamFlags::STEPPED),
            "Sync must be STEPPED"
        );

        // Division (index 8) — must be STEPPED
        let div_desc = adapter.param_info(8).unwrap();
        assert_eq!(div_desc.id, ParamId(909));
        assert!(
            div_desc.flags.contains(ParamFlags::STEPPED),
            "Division must be STEPPED"
        );

        // Output (index 9)
        let out_desc = adapter.param_info(9).unwrap();
        assert_eq!(out_desc.id, ParamId(905));

        // Beyond COUNT
        assert!(adapter.param_info(10).is_none());
    }

    // ── Behavioral / DSP correctness tests ───────────────────────────────

    /// `lerp()` between two param extremes must always produce finite output.
    ///
    /// Verifies that no parameter combination in the morph path can produce
    /// NaN or infinity.
    #[test]
    fn morph_produces_valid_output() {
        let mut kernel = PhaserKernel::new(48000.0);

        let a = PhaserParams {
            rate: 0.05,
            depth_pct: 0.0,
            stages: 2.0,
            feedback_pct: 0.0,
            mix_pct: 0.0,
            min_freq: 20.0,
            max_freq: 200.0,
            sync: 0.0,
            division: 0.0,
            output_db: -20.0,
        };
        let b = PhaserParams {
            rate: 5.0,
            depth_pct: 100.0,
            stages: 12.0,
            feedback_pct: 95.0,
            mix_pct: 100.0,
            min_freq: 2000.0,
            max_freq: 20000.0,
            sync: 0.0, // keep sync Off to avoid bpm dependency in test
            division: 11.0,
            output_db: 20.0,
        };

        for i in 0..=20 {
            let t = i as f32 / 20.0;
            let morphed = PhaserParams::lerp(&a, &b, t);
            for _ in 0..10 {
                let (l, r) = kernel.process_stereo(0.3, -0.3, &morphed);
                assert!(l.is_finite(), "Morph at t={t} produced non-finite L: {l}");
                assert!(r.is_finite(), "Morph at t={t} produced non-finite R: {r}");
            }
            kernel.reset();
        }
    }

    // ── from_knobs helper ─────────────────────────────────────────────────

    /// `from_knobs` must map 0.0 and 1.0 to the edges of each parameter's range.
    #[test]
    fn from_knobs_maps_ranges() {
        let low = PhaserParams::from_knobs(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert!((low.rate - 0.05).abs() < 0.01, "rate low: {}", low.rate);
        assert!(
            (low.depth_pct - 0.0).abs() < 0.5,
            "depth low: {}",
            low.depth_pct
        );
        assert!(
            (low.feedback_pct - 0.0).abs() < 0.5,
            "feedback low: {}",
            low.feedback_pct
        );
        assert!((low.mix_pct - 0.0).abs() < 0.5, "mix low: {}", low.mix_pct);
        assert!(
            (low.min_freq - 20.0).abs() < 1.0,
            "min_freq low: {}",
            low.min_freq
        );
        assert!(
            (low.max_freq - 200.0).abs() < 1.0,
            "max_freq low: {}",
            low.max_freq
        );

        let high = PhaserParams::from_knobs(1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0);
        assert!((high.rate - 5.0).abs() < 0.01, "rate high: {}", high.rate);
        assert!(
            (high.depth_pct - 100.0).abs() < 0.5,
            "depth high: {}",
            high.depth_pct
        );
        assert!(
            (high.feedback_pct - 95.0).abs() < 0.5,
            "feedback high: {}",
            high.feedback_pct
        );
        assert!(
            (high.mix_pct - 100.0).abs() < 0.5,
            "mix high: {}",
            high.mix_pct
        );
        assert!(
            (high.min_freq - 2000.0).abs() < 1.0,
            "min_freq high: {}",
            high.min_freq
        );
        assert!(
            (high.max_freq - 20000.0).abs() < 1.0,
            "max_freq high: {}",
            high.max_freq
        );

        // Midpoint checks
        let mid = PhaserParams::from_knobs(0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5);
        assert!(
            (mid.depth_pct - 50.0).abs() < 1.0,
            "depth mid: {}",
            mid.depth_pct
        );
        assert!((mid.mix_pct - 50.0).abs() < 1.0, "mix mid: {}", mid.mix_pct);
        assert!(
            mid.output_db.abs() < 0.01,
            "output_db should be 0 (fixed in from_knobs): {}",
            mid.output_db
        );
        // sync should be Off (0.0) from from_knobs
        assert_eq!(mid.sync, 0.0);
    }
}
