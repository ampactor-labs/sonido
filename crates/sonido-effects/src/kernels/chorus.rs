//! Chorus kernel — multi-voice delay modulation with stereo panning.
//!
//! `ChorusKernel` owns DSP state (delay lines, LFOs, feedback state).
//! Parameters are received via `&ChorusParams` each sample. Deployed via
//! [`KernelAdapter`](sonido_core::KernelAdapter) for desktop/plugin, or
//! called directly on embedded targets.
//!
//! # Algorithm
//!
//! Each voice runs an independent delay line modulated by its own LFO, following
//! the Dimension D approach of using irrational LFO rate ratios to prevent phase
//! alignment and produce a rich, evolving ensemble texture:
//!
//! - Voice 1: sine LFO at base rate, 0° phase
//! - Voice 2: sine LFO at base rate, 90° phase offset
//! - Voice 3: sine LFO at rate × 0.73 (irrational ratio)
//! - Voice 4: triangle LFO at rate × 1.17 (irrational ratio)
//!
//! # Signal Flow
//!
//! ```text
//! L ──→ Voice 1 ───────────────────────────────────────┐
//!                                                        ├─ Pan matrix ─→ Wet L
//! R ──→ Voice 2 ───────────────────────────────────────┤
//!                                                        ├─ Pan matrix ─→ Wet R
//! Mid ─→ Voice 3/4 (when voices > 2) ─────────────────┘
//!
//! (Wet L, Wet R) ─→ Wet/Dry Mix ─→ Soft Limit ─→ Output Level ─→ Out
//! ```
//!
//! Stereo panning matrix per voice count:
//! - 2 voices: V1 80% L / 20% R, V2 20% L / 80% R
//! - 3 voices: V1 left, V2 right, V3 center
//! - 4 voices: V1 hard-left, V2 hard-right, V3 centre-left, V4 centre-right
//!
//! # References
//!
//! - Boss Dimension D chorus approach: irrational LFO ratios for decorrelation
//! - Välimäki, "Effect Design Part 2: Delay-Line Modulation and Chorus", JAES 2000
//! - Zölzer, "DAFX: Digital Audio Effects" (2011), Ch. 4 (Modulation Effects)
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! let adapter = KernelAdapter::new(ChorusKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing, ADCs are hardware-filtered)
//! let mut kernel = ChorusKernel::new(48000.0);
//! let params = ChorusParams::from_knobs(adc_rate, adc_depth, adc_mix, adc_feedback, adc_output);
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

use libm::ceilf;
use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::math::soft_limit;
use sonido_core::{
    DIVISION_LABELS, InterpolatedDelay, Lfo, LfoWaveform, ParamDescriptor, ParamFlags, ParamId,
    ParamUnit, TempoManager, fast_db_to_linear, index_to_division, wet_dry_mix_stereo,
};

// ── Constants ────────────────────────────────────────────────────────────────

/// Maximum LFO modulation depth in milliseconds.
///
/// Sets the amplitude of delay time modulation. Together with `base_delay_ms`
/// this determines the total delay range: `base_delay_ms ± (depth × MAX_MOD_MS)`.
const MAX_MOD_MS: f32 = 5.0;

/// LFO rate multiplier for voice 3 (irrational ratio prevents phase locking).
///
/// Dimension D approach: voice 3 runs at 73% of the base rate, ensuring
/// it never re-aligns with voices 1 and 2 over any musical time span.
const VOICE3_RATE_RATIO: f32 = 0.73;

/// LFO rate multiplier for voice 4 (irrational ratio prevents phase locking).
///
/// Voice 4 runs at 117% of the base rate with a triangle waveform for
/// additional timbral variety compared to voices 1–3 (sine).
const VOICE4_RATE_RATIO: f32 = 1.17;

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`ChorusKernel`].
///
/// All values in **user-facing units** — the same units shown in GUIs and
/// stored in presets. The kernel converts internally as needed.
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `rate` | Hz | 0.1–10.0 | 1.0 |
/// | 1 | `depth_pct` | % | 0–100 | 50.0 |
/// | 2 | `mix_pct` | % | 0–100 | 50.0 |
/// | 3 | `voices` | index (2–4) | 2–4 | 2 |
/// | 4 | `feedback_pct` | % | 0–70 | 0.0 |
/// | 5 | `base_delay_ms` | ms | 5–25 | 15.0 |
/// | 6 | `sync` | index (0=Off, 1=On) | 0–1 | 0 |
/// | 7 | `division` | index (0–11) | 0–11 | 3 |
/// | 8 | `output_db` | dB | −20–20 | 0.0 |
#[derive(Debug, Clone, Copy)]
pub struct ChorusParams {
    /// LFO rate in Hz. Voices 1–2 use this directly; voice 3 uses rate × 0.73,
    /// voice 4 uses rate × 1.17 (Dimension D irrational ratios).
    pub rate: f32,

    /// Modulation depth as a percentage (0–100%).
    ///
    /// Controls the amplitude of LFO-driven delay time variation.
    /// Maps internally to `depth_pct / 100.0 × MAX_MOD_MS` samples of
    /// peak modulation per voice.
    pub depth_pct: f32,

    /// Wet/dry mix as a percentage (0% = fully dry, 100% = fully wet).
    pub mix_pct: f32,

    /// Number of active voices (2, 3, or 4). Stored as `f32` index.
    ///
    /// More voices thicken the ensemble character. Voices 3–4 use
    /// irrational rate ratios relative to voices 1–2.
    pub voices: f32,

    /// Feedback amount as a percentage (0–70%).
    ///
    /// Feeds delayed output back into the delay input, adding resonance.
    /// Capped at 70% to prevent instability.
    pub feedback_pct: f32,

    /// Base delay time in milliseconds (5–25 ms).
    ///
    /// The LFO modulates around this centre point. Shorter values (≤ 10 ms)
    /// produce doubling/thickening; longer values (≥ 20 ms) produce a more
    /// pronounced, ensemble-like sweep.
    pub base_delay_ms: f32,

    /// Tempo sync enable: 0.0 = Off, 1.0 = On.
    ///
    /// When On, the LFO rate is derived from the current BPM and the note
    /// division selected by [`division`](Self::division).
    pub sync: f32,

    /// Note division for tempo sync (index 0–11, see `DIVISION_LABELS`).
    ///
    /// Only active when [`sync`](Self::sync) is 1.0. Default index 3
    /// corresponds to `NoteDivision::Quarter` (one cycle per beat at
    /// the current BPM).
    pub division: f32,

    /// Output level in decibels (−20 to +20 dB).
    pub output_db: f32,
}

impl Default for ChorusParams {
    fn default() -> Self {
        Self {
            rate: 1.0,
            depth_pct: 50.0,
            mix_pct: 50.0,
            voices: 2.0,
            feedback_pct: 0.0,
            base_delay_ms: 15.0,
            sync: 0.0,
            division: 3.0,
            output_db: 0.0,
        }
    }
}

impl ChorusParams {
    /// Build params directly from hardware knob readings (0.0–1.0 normalized).
    ///
    /// Convenience constructor for embedded targets where ADC values map
    /// linearly to parameter ranges. The `voices` and `sync` knobs are
    /// quantised to their respective step counts.
    ///
    /// # Arguments
    ///
    /// - `rate`:      Rate knob, 0.0–1.0 → 0.1–10.0 Hz
    /// - `depth`:     Depth knob, 0.0–1.0 → 0–100 %
    /// - `mix`:       Mix knob, 0.0–1.0 → 0–100 %
    /// - `feedback`:  Feedback knob, 0.0–1.0 → 0–70 %
    /// - `output`:    Output level knob, 0.0–1.0 → −20–+20 dB
    pub fn from_knobs(rate: f32, depth: f32, mix: f32, feedback: f32, output: f32) -> Self {
        Self {
            rate: 0.1 + rate * 9.9,          // 0.1–10.0 Hz
            depth_pct: depth * 100.0,        // 0–100 %
            mix_pct: mix * 100.0,            // 0–100 %
            voices: 2.0,                     // fixed at 2 for embedded simplicity
            feedback_pct: feedback * 70.0,   // 0–70 %
            base_delay_ms: 15.0,             // fixed centre point for embedded
            sync: 0.0,                       // no sync on hardware
            division: 3.0,                   // quarter note (unused when sync=Off)
            output_db: output * 40.0 - 20.0, // −20–+20 dB
        }
    }
}

impl KernelParams for ChorusParams {
    const COUNT: usize = 9;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(ParamDescriptor::rate_hz(0.1, 10.0, 1.0).with_id(ParamId(700), "chor_rate")),
            1 => Some(ParamDescriptor::depth().with_id(ParamId(701), "chor_depth")),
            2 => Some(ParamDescriptor::mix().with_id(ParamId(702), "chor_mix")),
            3 => Some(
                ParamDescriptor::custom("Voices", "Voices", 2.0, 4.0, 2.0)
                    .with_step(1.0)
                    .with_id(ParamId(704), "chor_voices")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(&["2", "3", "4"]),
            ),
            4 => Some(
                ParamDescriptor::custom("Feedback", "Fdbk", 0.0, 70.0, 0.0)
                    .with_unit(ParamUnit::Percent)
                    .with_step(1.0)
                    .with_id(ParamId(705), "chor_feedback"),
            ),
            5 => Some(
                ParamDescriptor::custom("Base Delay", "BDly", 5.0, 25.0, 15.0)
                    .with_unit(ParamUnit::Milliseconds)
                    .with_step(0.5)
                    .with_id(ParamId(706), "chor_base_delay"),
            ),
            6 => Some(
                ParamDescriptor::custom("Sync", "Sync", 0.0, 1.0, 0.0)
                    .with_step(1.0)
                    .with_id(ParamId(707), "chor_sync")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(&["Off", "On"]),
            ),
            7 => Some(
                ParamDescriptor::custom("Division", "Div", 0.0, 11.0, 3.0)
                    .with_step(1.0)
                    .with_id(ParamId(708), "chor_division")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(DIVISION_LABELS),
            ),
            8 => Some(
                sonido_core::gain::output_param_descriptor().with_id(ParamId(703), "chor_output"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Standard, // rate — 10ms, avoid abrupt pitch jumps
            1 => SmoothingStyle::Standard, // depth_pct — 10ms
            2 => SmoothingStyle::Standard, // mix_pct — 10ms
            3 => SmoothingStyle::None,     // voices — discrete, snap immediately
            4 => SmoothingStyle::Standard, // feedback_pct — 10ms
            5 => SmoothingStyle::Interpolated, // base_delay_ms — 50ms, prevent pitch artifacts
            6 => SmoothingStyle::None,     // sync — discrete toggle
            7 => SmoothingStyle::None,     // division — discrete
            8 => SmoothingStyle::Standard, // output_db — 10ms
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.rate,
            1 => self.depth_pct,
            2 => self.mix_pct,
            3 => self.voices,
            4 => self.feedback_pct,
            5 => self.base_delay_ms,
            6 => self.sync,
            7 => self.division,
            8 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.rate = value,
            1 => self.depth_pct = value,
            2 => self.mix_pct = value,
            3 => self.voices = value,
            4 => self.feedback_pct = value,
            5 => self.base_delay_ms = value,
            6 => self.sync = value,
            7 => self.division = value,
            8 => self.output_db = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP chorus kernel.
///
/// Contains ONLY the mutable state required for audio processing:
///
/// - Four [`InterpolatedDelay`] lines (one per voice)
/// - Four [`Lfo`] oscillators with different waveforms and rate ratios
/// - Per-voice feedback state (`fb_state`)
/// - Cached sample-domain delay and modulation values
/// - [`TempoManager`] for tempo-sync calculation
/// - `sample_rate`
///
/// No `SmoothedParam`, no atomics, no platform awareness. The kernel is
/// `Send`-safe because all contained types (`InterpolatedDelay`, `Lfo`,
/// `TempoManager`, primitive floats) are `Send`.
pub struct ChorusKernel {
    /// Delay line for voice 1 (fed by the left input channel).
    delay1: InterpolatedDelay,
    /// Delay line for voice 2 (fed by the right input channel).
    delay2: InterpolatedDelay,
    /// Delay line for voice 3 (fed by mid signal; active when voices ≥ 3).
    delay3: InterpolatedDelay,
    /// Delay line for voice 4 (fed by mid signal; active when voices = 4).
    delay4: InterpolatedDelay,

    /// Voice 1 LFO: sine at base rate, 0° initial phase.
    lfo1: Lfo,
    /// Voice 2 LFO: sine at base rate, 90° initial phase offset.
    lfo2: Lfo,
    /// Voice 3 LFO: sine at rate × [`VOICE3_RATE_RATIO`] (0.73).
    lfo3: Lfo,
    /// Voice 4 LFO: triangle at rate × [`VOICE4_RATE_RATIO`] (1.17).
    lfo4: Lfo,

    /// Per-voice delayed output, used to implement feedback.
    ///
    /// `fb_state[i]` holds the previous output sample of voice `i`.
    fb_state: [f32; 4],

    /// Base delay time in samples, derived from `params.base_delay_ms`.
    ///
    /// Cached and updated each sample via the kernel's processing loop
    /// to avoid redundant `ms → samples` conversions.
    base_delay_samples: f32,

    /// Peak LFO modulation in samples, derived from `MAX_MOD_MS` and `sample_rate`.
    ///
    /// LFO output (−1 to +1) is multiplied by `depth × max_mod_samples` to
    /// produce the per-sample delay offset added to `base_delay_samples`.
    max_mod_samples: f32,

    /// Tempo manager for synced LFO rates.
    tempo: TempoManager,

    /// Current sample rate in Hz.
    sample_rate: f32,
}

impl ChorusKernel {
    /// Create a new chorus kernel initialised at `sample_rate`.
    ///
    /// Allocates four delay lines sized to hold the maximum possible delay
    /// (`25 ms base + 5 ms peak modulation = 30 ms`). LFO phase offsets and
    /// waveforms are set per the Dimension D approach.
    pub fn new(sample_rate: f32) -> Self {
        // Buffer must accommodate maximum base delay + maximum modulation depth.
        let max_delay_ms = 25.0 + MAX_MOD_MS;
        let max_delay_samples = ceilf((max_delay_ms / 1000.0) * sample_rate) as usize;
        let base_delay_samples = (15.0_f32 / 1000.0) * sample_rate; // default 15 ms
        let max_mod_samples = (MAX_MOD_MS / 1000.0) * sample_rate;

        // Voice 1: sine at 1 Hz, 0° phase
        let lfo1 = Lfo::new(sample_rate, 1.0);

        // Voice 2: sine at 1 Hz, 90° (0.25 cycle) phase offset — quadrature pair
        let mut lfo2 = Lfo::new(sample_rate, 1.0);
        lfo2.set_phase(0.25);

        // Voice 3: sine at rate × 0.73 — irrational ratio prevents locking
        let lfo3 = Lfo::new(sample_rate, VOICE3_RATE_RATIO);

        // Voice 4: triangle at rate × 1.17 — different waveform + irrational ratio
        let mut lfo4 = Lfo::new(sample_rate, VOICE4_RATE_RATIO);
        lfo4.set_waveform(LfoWaveform::Triangle);

        Self {
            delay1: InterpolatedDelay::new(max_delay_samples),
            delay2: InterpolatedDelay::new(max_delay_samples),
            delay3: InterpolatedDelay::new(max_delay_samples),
            delay4: InterpolatedDelay::new(max_delay_samples),
            lfo1,
            lfo2,
            lfo3,
            lfo4,
            fb_state: [0.0; 4],
            base_delay_samples,
            max_mod_samples,
            tempo: TempoManager::new(sample_rate, 120.0),
            sample_rate,
        }
    }

    /// Process all active voices in stereo with the given per-sample parameters.
    ///
    /// Voice routing:
    /// - Voice 1 is fed by the left input channel
    /// - Voice 2 is fed by the right input channel
    /// - Voices 3–4 are fed by the mid (average L+R) signal
    ///
    /// Returns `(wet_left, wet_right)` before wet/dry mixing.
    #[inline]
    fn process_voices_stereo(
        &mut self,
        left: f32,
        right: f32,
        depth: f32,
        feedback: f32,
        voices: u8,
    ) -> (f32, f32) {
        // Voice 1 — fed by left channel
        let dt1 = self.base_delay_samples + self.lfo1.advance() * depth * self.max_mod_samples;
        let wet1 = self.delay1.read(dt1);
        self.delay1.write(left + self.fb_state[0] * feedback);
        self.fb_state[0] = wet1;

        // Voice 2 — fed by right channel
        let dt2 = self.base_delay_samples + self.lfo2.advance() * depth * self.max_mod_samples;
        let wet2 = self.delay2.read(dt2);
        self.delay2.write(right + self.fb_state[1] * feedback);
        self.fb_state[1] = wet2;

        if voices == 2 {
            // 2 voices: V1 mostly left, V2 mostly right
            let wet_l = wet1 * 0.8 + wet2 * 0.2;
            let wet_r = wet2 * 0.8 + wet1 * 0.2;
            return (wet_l, wet_r);
        }

        // Voice 3 — fed by mid (average of L+R), panned centre
        let mid = (left + right) * 0.5;
        let dt3 = self.base_delay_samples + self.lfo3.advance() * depth * self.max_mod_samples;
        let wet3 = self.delay3.read(dt3);
        self.delay3.write(mid + self.fb_state[2] * feedback);
        self.fb_state[2] = wet3;

        if voices == 3 {
            // 3 voices: V1 left, V2 right, V3 centre — equal contribution
            let scale = 1.0 / 3.0;
            let wet_l = (wet1 * 0.9 + wet2 * 0.1 + wet3 * 0.5) * scale * 2.0;
            let wet_r = (wet2 * 0.9 + wet1 * 0.1 + wet3 * 0.5) * scale * 2.0;
            return (wet_l, wet_r);
        }

        // Voice 4 — fed by mid, panned centre-right
        let dt4 = self.base_delay_samples + self.lfo4.advance() * depth * self.max_mod_samples;
        let wet4 = self.delay4.read(dt4);
        self.delay4.write(mid + self.fb_state[3] * feedback);
        self.fb_state[3] = wet4;

        // 4 voices: V1 hard-left, V2 hard-right, V3 centre-left, V4 centre-right
        let wet_l = (wet1 * 0.9 + wet2 * 0.1 + wet3 * 0.7 + wet4 * 0.3) * 0.5;
        let wet_r = (wet2 * 0.9 + wet1 * 0.1 + wet4 * 0.7 + wet3 * 0.3) * 0.5;
        (wet_l, wet_r)
    }
}

impl DspKernel for ChorusKernel {
    type Params = ChorusParams;

    /// Process a stereo sample pair through the chorus.
    ///
    /// Per-sample steps:
    /// 1. Convert parameter units (%, dB, ms → internal domain)
    /// 2. Update base delay from `params.base_delay_ms` (in samples)
    /// 3. Apply tempo sync if enabled: derive LFO rate from `tempo.division_to_hz()`
    /// 4. Set all four LFO frequencies (voices 3/4 use irrational rate ratios)
    /// 5. Route through stereo voice matrix with feedback and panning
    /// 6. Wet/dry mix → soft limit → output level
    fn process_stereo(&mut self, left: f32, right: f32, params: &ChorusParams) -> (f32, f32) {
        // ── Unit conversion ──
        let depth = params.depth_pct / 100.0; // 0–100 % → 0–1
        let mix = params.mix_pct / 100.0; // 0–100 % → 0–1
        let feedback = params.feedback_pct / 100.0; // 0–70 % → 0–0.7
        let output = fast_db_to_linear(params.output_db);

        // ── Base delay in samples ──
        self.base_delay_samples = (params.base_delay_ms / 1000.0) * self.sample_rate;

        // ── LFO rate (with optional tempo sync) ──
        let rate = if params.sync > 0.5 {
            let div = index_to_division(params.division as u8);
            self.tempo.division_to_hz(div).clamp(0.1, 10.0)
        } else {
            params.rate
        };

        self.lfo1.set_frequency(rate);
        self.lfo2.set_frequency(rate);
        self.lfo3.set_frequency(rate * VOICE3_RATE_RATIO);
        self.lfo4.set_frequency(rate * VOICE4_RATE_RATIO);

        // Clamp voices to valid range; use integer for the match in process_voices_stereo
        let voices = (libm::roundf(params.voices) as u8).clamp(2, 4);

        // ── Voice processing with feedback ──
        let (wet_l, wet_r) = self.process_voices_stereo(left, right, depth, feedback, voices);

        // ── Wet/Dry mix → Soft Limit → Output Level ──
        let (out_l, out_r) = wet_dry_mix_stereo(left, right, wet_l, wet_r, mix);
        (
            soft_limit(out_l, 1.0) * output,
            soft_limit(out_r, 1.0) * output,
        )
    }

    fn reset(&mut self) {
        self.delay1.clear();
        self.delay2.clear();
        self.delay3.clear();
        self.delay4.clear();
        self.lfo1.reset();
        self.lfo2.reset();
        self.lfo3.reset();
        self.lfo4.reset();
        // Restore voice 2 LFO to its initial 90° phase offset after reset.
        self.lfo2.set_phase(0.25);
        self.fb_state = [0.0; 4];
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.max_mod_samples = (MAX_MOD_MS / 1000.0) * sample_rate;
        // base_delay_samples is recalculated per-sample in process_stereo.

        self.lfo1.set_sample_rate(sample_rate);
        self.lfo2.set_sample_rate(sample_rate);
        self.lfo3.set_sample_rate(sample_rate);
        self.lfo4.set_sample_rate(sample_rate);
        self.tempo.set_sample_rate(sample_rate);
    }

    fn is_true_stereo(&self) -> bool {
        // Different voice panning routes left and right outputs differently —
        // this is genuine cross-channel processing.
        true
    }

    fn set_tempo_context(&mut self, ctx: &sonido_core::TempoContext) {
        // Store updated BPM in the tempo manager. The LFO rate is applied
        // next time process_stereo() runs and detects sync = On.
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

    // ── Basic correctness ────────────────────────────────────────────────────

    /// Silence in must produce silence out regardless of parameter state.
    ///
    /// With zero input and zero feedback state, the delay lines are empty,
    /// so the wet signal is also zero. Mix of zero with zero is zero.
    #[test]
    fn silence_in_silence_out() {
        let mut kernel = ChorusKernel::new(48000.0);
        let params = ChorusParams::default();

        let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
        assert!(l.abs() < 1e-6, "Expected silence on left, got {l}");
        assert!(r.abs() < 1e-6, "Expected silence on right, got {r}");
    }

    /// Processing must never produce NaN or ±Infinity over 1000 samples.
    #[test]
    fn no_nan_or_inf() {
        let mut kernel = ChorusKernel::new(48000.0);
        let params = ChorusParams {
            rate: 3.0,
            depth_pct: 80.0,
            mix_pct: 75.0,
            voices: 4.0,
            feedback_pct: 50.0,
            base_delay_ms: 20.0,
            ..Default::default()
        };

        for _ in 0..1000 {
            let (l, r) = kernel.process_stereo(0.5, -0.3, &params);
            assert!(l.is_finite(), "Left output is NaN or Inf");
            assert!(r.is_finite(), "Right output is NaN or Inf");
        }
    }

    /// Descriptor count must match `COUNT` and all indices must be populated.
    #[test]
    fn params_descriptor_count() {
        assert_eq!(ChorusParams::COUNT, 9, "Expected 9 parameters");

        for i in 0..ChorusParams::COUNT {
            assert!(
                ChorusParams::descriptor(i).is_some(),
                "Missing descriptor at index {i}"
            );
        }
        assert!(
            ChorusParams::descriptor(ChorusParams::COUNT).is_none(),
            "Descriptor beyond COUNT should be None"
        );
    }

    /// Kernel must report true stereo (cross-channel voice panning).
    #[test]
    fn is_true_stereo() {
        let kernel = ChorusKernel::new(48000.0);
        assert!(
            kernel.is_true_stereo(),
            "Chorus must be true stereo (cross-channel panning)"
        );
    }

    /// With max feedback and depth the output must stay within bounded limits.
    ///
    /// `soft_limit` at threshold 1.0 ensures the output cannot saturate to ±Inf
    /// even at maximum feedback settings.
    #[test]
    fn feedback_bounded() {
        let mut kernel = ChorusKernel::new(48000.0);
        let params = ChorusParams {
            rate: 2.0,
            depth_pct: 100.0,
            mix_pct: 100.0,
            voices: 4.0,
            feedback_pct: 70.0, // maximum feedback
            base_delay_ms: 15.0,
            ..Default::default()
        };

        // Drive with a sustained signal at max feedback for 10 000 samples.
        for i in 0..10_000 {
            // Alternate sign to exercise both polarities.
            let inp = if i % 2 == 0 { 0.5_f32 } else { -0.5_f32 };
            let (l, r) = kernel.process_stereo(inp, inp, &params);
            assert!(
                l.abs() <= 1.5,
                "Left output exceeded bounds at sample {i}: {l}"
            );
            assert!(
                r.abs() <= 1.5,
                "Right output exceeded bounds at sample {i}: {r}"
            );
        }
    }

    /// 4-voice mode must produce a different output than 2-voice mode.
    ///
    /// Additional active voices add independently modulated delays, changing
    /// the combined output spectrum — they cannot produce identical results.
    /// Both kernels are driven from the same input for 1000 samples before
    /// comparing, ensuring delay lines are well-seeded and the panning
    /// matrix differences have time to manifest.
    #[test]
    fn voices_param_changes_output() {
        let params2 = ChorusParams {
            voices: 2.0,
            mix_pct: 100.0,
            depth_pct: 60.0,
            rate: 2.0,
            ..Default::default()
        };
        let params4 = ChorusParams {
            voices: 4.0,
            mix_pct: 100.0,
            depth_pct: 60.0,
            rate: 2.0,
            ..Default::default()
        };

        let mut kernel2 = ChorusKernel::new(48000.0);
        let mut kernel4 = ChorusKernel::new(48000.0);

        // Run both kernels from the start with their respective voice counts
        // so the panning matrices diverge from sample 1.
        let mut sum2 = 0.0f32;
        let mut sum4 = 0.0f32;
        for _ in 0..1000 {
            let (l2, _) = kernel2.process_stereo(0.3, 0.3, &params2);
            let (l4, _) = kernel4.process_stereo(0.3, 0.3, &params4);
            sum2 += l2;
            sum4 += l4;
        }

        // The accumulated sums will differ because the 2-voice pan matrix
        // produces (wet1 * 0.8 + wet2 * 0.2), while the 4-voice matrix
        // additionally blends voices 3 and 4 with their own delay histories.
        assert!(
            (sum2 - sum4).abs() > 0.1,
            "2-voice and 4-voice accumulated sums should differ: sum2={sum2}, sum4={sum4}"
        );
    }

    /// At 0% mix the output must approximate the dry input.
    ///
    /// With `mix_pct = 0.0` the `wet_dry_mix_stereo` function returns the dry
    /// signal. The subsequent `soft_limit` is transparent for signals below 1.0,
    /// and the default output level is 0 dB (unity gain).
    #[test]
    fn dry_mix_passes_input() {
        let mut kernel = ChorusKernel::new(48000.0);
        let params = ChorusParams {
            mix_pct: 0.0,
            output_db: 0.0,
            ..Default::default()
        };

        let input = 0.4_f32;
        let (l, _r) = kernel.process_stereo(input, input, &params);
        assert!(
            (l - input).abs() < 0.01,
            "0% mix should pass dry signal: expected {input}, got {l}"
        );
    }

    // ── Adapter integration ──────────────────────────────────────────────────

    /// The kernel must wrap into a `KernelAdapter` and function as an `Effect`.
    #[test]
    fn adapter_wraps_as_effect() {
        let mut adapter = KernelAdapter::new(ChorusKernel::new(48000.0), 48000.0);
        adapter.reset();
        let output = adapter.process(0.3);
        assert!(
            output.is_finite(),
            "Adapter output must be finite, got {output}"
        );
    }

    /// The adapter's `ParameterInfo` must match `ChorusParams::COUNT`.
    #[test]
    fn adapter_param_info_matches() {
        let adapter = KernelAdapter::new(ChorusKernel::new(48000.0), 48000.0);
        assert_eq!(
            adapter.param_count(),
            ChorusParams::COUNT,
            "Adapter param count must match ChorusParams::COUNT"
        );

        // Verify ParamIds match the classic Chorus effect exactly.
        assert_eq!(adapter.param_info(0).unwrap().id, ParamId(700)); // rate
        assert_eq!(adapter.param_info(1).unwrap().id, ParamId(701)); // depth
        assert_eq!(adapter.param_info(2).unwrap().id, ParamId(702)); // mix
        assert_eq!(adapter.param_info(3).unwrap().id, ParamId(704)); // voices
        assert_eq!(adapter.param_info(4).unwrap().id, ParamId(705)); // feedback
        assert_eq!(adapter.param_info(5).unwrap().id, ParamId(706)); // base_delay
        assert_eq!(adapter.param_info(6).unwrap().id, ParamId(707)); // sync
        assert_eq!(adapter.param_info(7).unwrap().id, ParamId(708)); // division
        assert_eq!(adapter.param_info(8).unwrap().id, ParamId(703)); // output
    }

    // ── Morphing / preset ────────────────────────────────────────────────────

    /// Morphing between two param states must always produce finite output.
    #[test]
    fn morph_produces_valid_output() {
        let mut kernel = ChorusKernel::new(48000.0);
        let a = ChorusParams::default();
        let b = ChorusParams {
            rate: 8.0,
            depth_pct: 100.0,
            mix_pct: 100.0,
            voices: 4.0,
            feedback_pct: 60.0,
            base_delay_ms: 22.0,
            output_db: -6.0,
            ..Default::default()
        };

        for i in 0..=10 {
            let t = i as f32 / 10.0;
            let morphed = ChorusParams::lerp(&a, &b, t);
            let (l, r) = kernel.process_stereo(0.3, -0.3, &morphed);
            assert!(
                l.is_finite() && r.is_finite(),
                "Morph at t={t} produced NaN/Inf: l={l}, r={r}"
            );
            kernel.reset();
        }
    }

    // ── Knob mapping ─────────────────────────────────────────────────────────

    /// `from_knobs()` must map 0.0–1.0 inputs to the correct parameter ranges.
    #[test]
    fn from_knobs_maps_ranges() {
        // Full-deflection test: all knobs at maximum
        let p = ChorusParams::from_knobs(1.0, 1.0, 1.0, 1.0, 1.0);
        assert!(
            (p.rate - 10.0).abs() < 0.01,
            "Rate at 1.0 should be ~10.0 Hz, got {}",
            p.rate
        );
        assert!(
            (p.depth_pct - 100.0).abs() < 0.01,
            "Depth at 1.0 should be ~100 %, got {}",
            p.depth_pct
        );
        assert!(
            (p.mix_pct - 100.0).abs() < 0.01,
            "Mix at 1.0 should be ~100 %, got {}",
            p.mix_pct
        );
        assert!(
            (p.feedback_pct - 70.0).abs() < 0.01,
            "Feedback at 1.0 should be ~70 %, got {}",
            p.feedback_pct
        );
        assert!(
            (p.output_db - 20.0).abs() < 0.01,
            "Output at 1.0 should be ~+20 dB, got {}",
            p.output_db
        );

        // Zero-deflection test: all knobs at minimum
        let p = ChorusParams::from_knobs(0.0, 0.0, 0.0, 0.0, 0.0);
        assert!(
            (p.rate - 0.1).abs() < 0.01,
            "Rate at 0.0 should be ~0.1 Hz, got {}",
            p.rate
        );
        assert!(
            p.depth_pct.abs() < 0.01,
            "Depth at 0.0 should be ~0 %, got {}",
            p.depth_pct
        );
        assert!(
            p.mix_pct.abs() < 0.01,
            "Mix at 0.0 should be ~0 %, got {}",
            p.mix_pct
        );
        assert!(
            p.feedback_pct.abs() < 0.01,
            "Feedback at 0.0 should be ~0 %, got {}",
            p.feedback_pct
        );
        assert!(
            (p.output_db - (-20.0)).abs() < 0.01,
            "Output at 0.0 should be ~-20 dB, got {}",
            p.output_db
        );
    }

    // ── Additional behavioural ────────────────────────────────────────────────

    /// Resetting the kernel must clear feedback state and delay buffers.
    #[test]
    fn reset_clears_state() {
        let mut kernel = ChorusKernel::new(48000.0);
        let params = ChorusParams {
            feedback_pct: 60.0,
            mix_pct: 100.0,
            ..Default::default()
        };

        // Build up state
        for _ in 0..1000 {
            kernel.process_stereo(0.5, 0.5, &params);
        }

        kernel.reset();
        assert_eq!(kernel.fb_state, [0.0; 4], "reset() must clear fb_state");

        // After reset, silence in should give near-silence out (empty delay lines)
        let silent_params = ChorusParams {
            mix_pct: 100.0,
            ..Default::default()
        };
        let (l, r) = kernel.process_stereo(0.0, 0.0, &silent_params);
        assert!(
            l.abs() < 1e-6,
            "After reset, silence in → silence out (left)"
        );
        assert!(
            r.abs() < 1e-6,
            "After reset, silence in → silence out (right)"
        );
    }

    /// Adapter's `is_true_stereo` must forward to the kernel.
    #[test]
    fn adapter_is_true_stereo() {
        let adapter = KernelAdapter::new(ChorusKernel::new(48000.0), 48000.0);
        assert!(adapter.is_true_stereo());
    }
}
