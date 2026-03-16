//! Transient Shaper kernel — independent attack/sustain envelope control.
//!
//! `TransientShaperKernel` owns DSP state (two parallel envelope followers).
//! Parameters are received via `&TransientShaperParams` each sample. Deployed via
//! [`Adapter`](sonido_core::kernel::Adapter) for desktop/plugin, or called
//! directly on embedded targets.
//!
//! # Algorithm
//!
//! Two envelope followers run in parallel on the (sensitivity-scaled) input:
//!
//! - **Fast** (1 ms attack, 10 ms release): tracks transients.
//! - **Slow** (50 ms attack, 200 ms release): tracks sustained body.
//!
//! The difference `fast_env − slow_env` is positive during attack portions and
//! near zero during steady-state sustain. Both signals are normalized to \[0, 1\]
//! and used to build a multiplicative gain modifier:
//!
//! ```text
//! gain = 1.0 + (attack_pct/100) × transient_norm + (sustain_pct/100) × sustain_norm
//! ```
//!
//! Clamping `gain ≥ 0.0` prevents sign inversion. The shaped signal is then
//! blended with the dry signal via `mix_pct` and scaled by the output level.
//!
//! # Signal Flow
//!
//! ```text
//! Input ──┬──────────────────────────────────────────── × gain ──┬── wet/dry mix ── × output
//!         │                                              ▲        │
//!         └─► × sensitivity ─► Fast EF ─► transient_norm         └── (dry path)
//!                           └─► Slow EF ─► sustain_norm
//! ```
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (adapter handles smoothing automatically)
//! let adapter = Adapter::new(TransientShaperKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing, ADCs are hardware-filtered)
//! let mut kernel = TransientShaperKernel::new(48000.0);
//! let params = TransientShaperParams::from_knobs(attack, sustain, sensitivity, mix, output);
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::{
    EnvelopeFollower, ParamDescriptor, ParamId, ParamUnit,
    gain::output_param_descriptor,
    math::{db_to_linear, flush_denormal, wet_dry_mix},
};

// ═══════════════════════════════════════════════════════════════════════════
//  Constants
// ═══════════════════════════════════════════════════════════════════════════

/// Fast envelope follower attack time (ms) — tracks transient onset.
const FAST_ATTACK_MS: f32 = 1.0;
/// Fast envelope follower release time (ms) — decays quickly after transient.
const FAST_RELEASE_MS: f32 = 10.0;

/// Slow envelope follower attack time (ms) — tracks sustained body.
const SLOW_ATTACK_MS: f32 = 50.0;
/// Slow envelope follower release time (ms) — holds through sustain tail.
const SLOW_RELEASE_MS: f32 = 200.0;

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`TransientShaperKernel`].
///
/// All values are in **user-facing units** — the same units shown in GUIs and
/// stored in presets.
///
/// ## Parameter Table
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `attack_pct` | % | −100–+100 | 0.0 |
/// | 1 | `sustain_pct` | % | −100–+100 | 0.0 |
/// | 2 | `sensitivity_pct` | % | 0–100 | 50.0 |
/// | 3 | `mix_pct` | % | 0–100 | 100.0 |
/// | 4 | `output_db` | dB | −60–+6 | 0.0 |
///
/// ## Smoothing Notes
///
/// - `attack_pct`, `sustain_pct`: Fast (5 ms) — shaping params, quick response.
/// - `sensitivity_pct`, `mix_pct`: Standard (10 ms) — blend controls.
/// - `output_db`: Fast (5 ms) — level trim.
#[derive(Debug, Clone, Copy)]
pub struct TransientShaperParams {
    /// Transient (attack) boost/cut in percent.
    ///
    /// Range: −100.0–+100.0 % (default 0.0).
    /// +100% doubles the transient level; −100% removes transients entirely.
    pub attack_pct: f32,

    /// Sustain boost/cut in percent.
    ///
    /// Range: −100.0–+100.0 % (default 0.0).
    /// +100% doubles the sustain level; −100% removes the sustained body entirely.
    pub sustain_pct: f32,

    /// Sensitivity of the envelope detection in percent.
    ///
    /// Range: 0.0–100.0 % (default 50.0).
    /// Scales the input to the envelope followers as a pre-gain. Higher values
    /// make the detection more responsive to quieter signals.
    pub sensitivity_pct: f32,

    /// Wet/dry blend in percent.
    ///
    /// Range: 0.0–100.0 % (default 100.0).
    /// At 0% the dry signal passes unchanged; at 100% only the shaped signal
    /// is output.
    pub mix_pct: f32,

    /// Output level in decibels.
    ///
    /// Range: −60.0–+6.0 dB (default 0.0). Final output trim.
    pub output_db: f32,
}

impl Default for TransientShaperParams {
    fn default() -> Self {
        Self {
            attack_pct: 0.0,
            sustain_pct: 0.0,
            sensitivity_pct: 50.0,
            mix_pct: 100.0,
            output_db: 0.0,
        }
    }
}

impl TransientShaperParams {
    /// Creates parameters from normalized 0–1 knob readings.
    ///
    /// | Argument | Index | Parameter | Range |
    /// |----------|-------|-----------|-------|
    /// | `attack` | 0 | `attack_pct` | −100–+100 % |
    /// | `sustain` | 1 | `sustain_pct` | −100–+100 % |
    /// | `sensitivity` | 2 | `sensitivity_pct` | 0–100 % |
    /// | `mix` | 3 | `mix_pct` | 0–100 % |
    /// | `output` | 4 | `output_db` | −60–+6 dB |
    pub fn from_knobs(attack: f32, sustain: f32, sensitivity: f32, mix: f32, output: f32) -> Self {
        Self::from_normalized(&[attack, sustain, sensitivity, mix, output])
    }
}

impl KernelParams for TransientShaperParams {
    const COUNT: usize = 5;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            // ── [0] Attack ───────────────────────────────────────────────────
            // ParamId(2900), "ts_attack" — transient boost/cut
            0 => Some(
                ParamDescriptor::custom("Attack", "Attack", -100.0, 100.0, 0.0)
                    .with_unit(ParamUnit::Percent)
                    .with_step(0.1)
                    .with_id(ParamId(2900), "ts_attack"),
            ),
            // ── [1] Sustain ──────────────────────────────────────────────────
            // ParamId(2901), "ts_sustain" — sustained body boost/cut
            1 => Some(
                ParamDescriptor::custom("Sustain", "Sustain", -100.0, 100.0, 0.0)
                    .with_unit(ParamUnit::Percent)
                    .with_step(0.1)
                    .with_id(ParamId(2901), "ts_sustain"),
            ),
            // ── [2] Sensitivity ──────────────────────────────────────────────
            // ParamId(2902), "ts_sensitivity" — detection pre-gain
            2 => Some(
                ParamDescriptor::custom("Sensitivity", "Sens", 0.0, 100.0, 50.0)
                    .with_unit(ParamUnit::Percent)
                    .with_step(0.1)
                    .with_id(ParamId(2902), "ts_sensitivity"),
            ),
            // ── [3] Mix ──────────────────────────────────────────────────────
            // ParamId(2903), "ts_mix" — wet/dry blend
            3 => Some(ParamDescriptor::mix().with_id(ParamId(2903), "ts_mix")),
            // ── [4] Output ───────────────────────────────────────────────────
            // ParamId(2904), "ts_output" — final output trim
            4 => Some(output_param_descriptor().with_id(ParamId(2904), "ts_output")),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Fast, // attack_pct — shaping param, quick response
            1 => SmoothingStyle::Fast, // sustain_pct — shaping param, quick response
            2 => SmoothingStyle::Standard, // sensitivity_pct — detection blend
            3 => SmoothingStyle::Standard, // mix_pct — wet/dry blend
            4 => SmoothingStyle::Fast, // output_db — level trim
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.attack_pct,
            1 => self.sustain_pct,
            2 => self.sensitivity_pct,
            3 => self.mix_pct,
            4 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.attack_pct = value,
            1 => self.sustain_pct = value,
            2 => self.sensitivity_pct = value,
            3 => self.mix_pct = value,
            4 => self.output_db = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP transient shaper kernel.
///
/// Contains ONLY the mutable state required for audio processing. No `SmoothedParam`,
/// no atomics, no platform awareness.
///
/// # DSP State
///
/// - `fast_env` — fast follower (1 ms attack, 10 ms release): tracks transients.
/// - `slow_env` — slow follower (50 ms attack, 200 ms release): tracks sustain body.
///
/// # Gain Modulation
///
/// At each sample:
/// 1. Sensitivity-scaled input drives both envelope followers.
/// 2. `transient_norm = max(fast_env − slow_env, 0.0)` — positive during attacks.
/// 3. `sustain_norm = slow_env` — steady-state body level.
/// 4. `gain = 1.0 + (attack_pct/100) × transient_norm + (sustain_pct/100) × sustain_norm`
/// 5. Gain is clamped to `≥ 0.0` to prevent signal inversion.
/// 6. Applied to the dry signal, then blended via `mix_pct` and output level.
pub struct TransientShaperKernel {
    /// Fast envelope follower — detects transient onsets.
    fast_env: EnvelopeFollower,
    /// Slow envelope follower — tracks sustained body level.
    slow_env: EnvelopeFollower,
}

impl TransientShaperKernel {
    /// Create a new transient shaper kernel with fixed envelope time constants.
    ///
    /// - Fast: 1 ms attack / 10 ms release
    /// - Slow: 50 ms attack / 200 ms release
    pub fn new(sample_rate: f32) -> Self {
        let mut fast_env = EnvelopeFollower::new(sample_rate);
        fast_env.set_attack_ms(FAST_ATTACK_MS);
        fast_env.set_release_ms(FAST_RELEASE_MS);

        let mut slow_env = EnvelopeFollower::new(sample_rate);
        slow_env.set_attack_ms(SLOW_ATTACK_MS);
        slow_env.set_release_ms(SLOW_RELEASE_MS);

        Self { fast_env, slow_env }
    }

    /// Compute gain modifier for a single detection sample.
    ///
    /// Runs both envelope followers, extracts transient and sustain components,
    /// and computes the multiplicative gain for this sample.
    ///
    /// - `detection`: sensitivity-scaled input signal (absolute value recommended)
    /// - `attack_pct`: transient boost/cut in percent (−100–+100)
    /// - `sustain_pct`: sustain boost/cut in percent (−100–+100)
    ///
    /// Returns a gain factor ≥ 0.0 (0.0 means fully removed).
    #[inline]
    fn compute_gain(&mut self, detection: f32, attack_pct: f32, sustain_pct: f32) -> f32 {
        let fast = self.fast_env.process(detection);
        let slow = self.slow_env.process(detection);

        // Transient component: positive during attack, ~0 during steady sustain
        let transient_norm = if fast > slow { fast - slow } else { 0.0 };
        // Sustain component: the steady-state body level
        let sustain_norm = slow;

        let gain =
            1.0 + (attack_pct / 100.0) * transient_norm + (sustain_pct / 100.0) * sustain_norm;

        // Clamp to ≥ 0.0 — negative gain would invert the signal phase
        if gain < 0.0 { 0.0 } else { gain }
    }
}

impl DspKernel for TransientShaperKernel {
    type Params = TransientShaperParams;

    /// Process a stereo sample pair with linked-mono transient detection.
    ///
    /// Detection uses the mid signal `(|left| + |right|) / 2` so both channels
    /// receive the same gain modifier, preserving the stereo image.
    ///
    /// Signal path:
    /// 1. Mid signal × sensitivity → envelope followers → gain modifier
    /// 2. Gain modifier applied to both L and R
    /// 3. Wet/dry blend (`mix_pct`)
    /// 4. Output level applied
    fn process_stereo(
        &mut self,
        left: f32,
        right: f32,
        params: &TransientShaperParams,
    ) -> (f32, f32) {
        // Mid detection signal (rectified average of both channels)
        let mid = (libm::fabsf(left) + libm::fabsf(right)) * 0.5;

        // Scale by sensitivity (0–100% → 0–2× pre-gain for detection only)
        let sensitivity_gain = params.sensitivity_pct / 50.0;
        let detection = flush_denormal(mid * sensitivity_gain);

        let gain = self.compute_gain(detection, params.attack_pct, params.sustain_pct);

        let wet_l = flush_denormal(left * gain);
        let wet_r = flush_denormal(right * gain);

        let mix = params.mix_pct / 100.0;
        let out_l = wet_dry_mix(left, wet_l, mix);
        let out_r = wet_dry_mix(right, wet_r, mix);

        let output_linear = db_to_linear(params.output_db);
        (
            flush_denormal(out_l * output_linear),
            flush_denormal(out_r * output_linear),
        )
    }

    fn reset(&mut self) {
        self.fast_env.reset();
        self.slow_env.reset();
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        // Rebuild both followers with new sample rate — time constants are fixed
        self.fast_env = EnvelopeFollower::new(sample_rate);
        self.fast_env.set_attack_ms(FAST_ATTACK_MS);
        self.fast_env.set_release_ms(FAST_RELEASE_MS);

        self.slow_env = EnvelopeFollower::new(sample_rate);
        self.slow_env.set_attack_ms(SLOW_ATTACK_MS);
        self.slow_env.set_release_ms(SLOW_RELEASE_MS);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48000.0;

    /// Helper: run a block of samples through the kernel with given params.
    fn process_block(
        kernel: &mut TransientShaperKernel,
        params: &TransientShaperParams,
        block: &[(f32, f32)],
    ) -> Vec<(f32, f32)> {
        block
            .iter()
            .map(|&(l, r)| kernel.process_stereo(l, r, params))
            .collect()
    }

    #[test]
    fn output_is_finite_for_silence() {
        let mut kernel = TransientShaperKernel::new(SR);
        let params = TransientShaperParams::default();
        let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
        assert!(l.is_finite(), "left output must be finite for silence");
        assert!(r.is_finite(), "right output must be finite for silence");
    }

    #[test]
    fn output_is_finite_for_impulse() {
        let mut kernel = TransientShaperKernel::new(SR);
        let params = TransientShaperParams {
            attack_pct: 100.0,
            sustain_pct: 0.0,
            sensitivity_pct: 50.0,
            mix_pct: 100.0,
            output_db: 0.0,
        };
        // Feed an impulse followed by silence
        let (l, r) = kernel.process_stereo(1.0, 1.0, &params);
        assert!(l.is_finite(), "left output must be finite after impulse");
        assert!(r.is_finite(), "right output must be finite after impulse");
        for _ in 0..100 {
            let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
            assert!(l.is_finite());
            assert!(r.is_finite());
        }
    }

    #[test]
    fn attack_boost_increases_peak_on_impulse() {
        // With attack boost, the peak response to an impulse should exceed
        // the unmodified (0% attack) response.
        let mut baseline = TransientShaperKernel::new(SR);
        let params_baseline = TransientShaperParams {
            attack_pct: 0.0,
            sustain_pct: 0.0,
            sensitivity_pct: 50.0,
            mix_pct: 100.0,
            output_db: 0.0,
        };

        let mut boosted = TransientShaperKernel::new(SR);
        let params_boosted = TransientShaperParams {
            attack_pct: 100.0,
            ..params_baseline
        };

        // Apply a short burst then silence
        let input: Vec<(f32, f32)> = (0..10)
            .map(|i| if i == 0 { (1.0, 1.0) } else { (0.0, 0.0) })
            .collect();

        let baseline_out = process_block(&mut baseline, &params_baseline, &input);
        let boosted_out = process_block(&mut boosted, &params_boosted, &input);

        let baseline_peak = baseline_out
            .iter()
            .map(|&(l, _)| l.abs())
            .fold(0.0f32, f32::max);
        let boosted_peak = boosted_out
            .iter()
            .map(|&(l, _)| l.abs())
            .fold(0.0f32, f32::max);

        assert!(
            boosted_peak > baseline_peak,
            "attack boost should increase peak: baseline={baseline_peak}, boosted={boosted_peak}"
        );
    }

    #[test]
    fn sustain_cut_reduces_tail_level() {
        // Sustain cut (−100%) should reduce the output level during the held-note tail
        // compared to no sustain modification.
        let mut baseline = TransientShaperKernel::new(SR);
        let params_baseline = TransientShaperParams {
            attack_pct: 0.0,
            sustain_pct: 0.0,
            sensitivity_pct: 50.0,
            mix_pct: 100.0,
            output_db: 0.0,
        };

        let mut cut = TransientShaperKernel::new(SR);
        let params_cut = TransientShaperParams {
            sustain_pct: -100.0,
            ..params_baseline
        };

        // Fill the slow envelope with sustained signal (500 samples ≈ 10 ms at 48kHz)
        let held_value = 0.5_f32;
        let priming: Vec<(f32, f32)> = vec![(held_value, held_value); 500];
        process_block(&mut baseline, &params_baseline, &priming);
        process_block(&mut cut, &params_cut, &priming);

        // Measure average output level over the tail
        let tail: Vec<(f32, f32)> = vec![(held_value, held_value); 200];
        let baseline_tail = process_block(&mut baseline, &params_baseline, &tail);
        let cut_tail = process_block(&mut cut, &params_cut, &tail);

        let baseline_rms: f32 = {
            let sum: f32 = baseline_tail.iter().map(|&(l, _)| l * l).sum();
            libm::sqrtf(sum / baseline_tail.len() as f32)
        };
        let cut_rms: f32 = {
            let sum: f32 = cut_tail.iter().map(|&(l, _)| l * l).sum();
            libm::sqrtf(sum / cut_tail.len() as f32)
        };

        assert!(
            cut_rms < baseline_rms,
            "sustain cut should reduce tail RMS: baseline={baseline_rms}, cut={cut_rms}"
        );
    }
}
