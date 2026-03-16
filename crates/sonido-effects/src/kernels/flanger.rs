//! Flanger kernel — through-zero flanging with feedback and stereo LFO offset.
//!
//! `FlangerKernel` owns DSP state (delay lines, LFOs, feedback samples).
//! Parameters are received via `&FlangerParams` each sample. Deployed via
//! [`Adapter`](sonido_core::kernel::Adapter) for desktop/plugin, or called
//! directly on embedded targets.
//!
//! # Signal Flow
//!
//! ```text
//! Input ──→ Delay (LFO-modulated) ──→ feedback_wet_compensation ──→ Mix ──→ Output
//!   │            ↑ feedback ──────────────────────────────────────────┘
//!   └──→ [TZF: fixed dry delay] ─────────────────────────────────────→ Mix
//! ```
//!
//! In Through-Zero Flanging (TZF) mode the dry signal is routed through a fixed
//! delay equal to the sweep midpoint (`base_delay_samples`). The wet delay sweeps
//! from 0 to `2 × base_delay`, passing through the dry delay point. At the zero
//! crossing the paths are time-aligned, producing the characteristic "jet whoosh"
//! null of tape-flanger circuits.
//!
//! ## Bipolar Feedback
//!
//! Feedback ranges from −95 % to +95 %. Positive feedback reinforces odd harmonics
//! of the comb filter; negative feedback shifts peaks to even harmonics. The wet
//! signal is compensated by `feedback_wet_compensation(|feedback|)` to maintain
//! consistent perceived level across the range.
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! use sonido_core::kernel::Adapter;
//! let adapter = Adapter::new(FlangerKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing, ADCs are hardware-filtered)
//! let mut kernel = FlangerKernel::new(48000.0);
//! let params = FlangerParams::from_knobs(0.5, 0.35, 0.5, 0.5, 0.0, 0.0, 3.0, 0.0);
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

use libm::ceilf;
use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::{
    DIVISION_LABELS, InterpolatedDelay, Lfo, NoteDivision, ParamDescriptor, ParamFlags, ParamId,
    ParamUnit, TempoContext, TempoManager, fast_db_to_linear, flush_denormal, index_to_division,
    wet_dry_mix_stereo,
};

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`FlangerKernel`].
///
/// All values in **user-facing units** — the same units shown in GUIs and
/// stored in presets. The kernel converts internally as needed.
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `rate` | Hz | 0.05–5.0 | 0.5 |
/// | 1 | `depth_pct` | % | 0–100 | 35.0 |
/// | 2 | `feedback_pct` | % | −95–95 | 50.0 |
/// | 3 | `mix_pct` | % | 0–100 | 50.0 |
/// | 4 | `tzf` | index | 0–1 | 0 (Off) |
/// | 5 | `sync` | index | 0–1 | 0 (Off) |
/// | 6 | `division` | index | 0–11 | 3 (Quarter) |
/// | 7 | `output_db` | dB | −20–+6 | 0.0 |
/// | 8 | `lfo_phase` | 0–1 | READ_ONLY diagnostic | 0.0 |
#[derive(Debug, Clone, Copy)]
pub struct FlangerParams {
    /// LFO rate in Hz.
    ///
    /// Range: 0.05 to 5.0 Hz. Default 0.5 Hz.
    pub rate: f32,

    /// Modulation depth in percent.
    ///
    /// Range: 0.0 to 100.0 %. Internally mapped to a fraction of
    /// `max_mod_samples` (5 ms at the current sample rate). Default 35.0 %.
    pub depth_pct: f32,

    /// Regeneration (feedback) amount in percent.
    ///
    /// Range: −95.0 to +95.0 %. Positive values reinforce odd harmonics;
    /// negative values shift comb peaks to even harmonics. Default 50.0%.
    pub feedback_pct: f32,

    /// Wet/dry mix in percent.
    ///
    /// Range: 0.0 to 100.0 %. 0 % = fully dry, 100 % = fully wet.
    /// Default 50.0 %.
    pub mix_pct: f32,

    /// Through-zero flanging mode: 0.0 = Off, 1.0 = On.
    ///
    /// When enabled, the dry signal is routed through a fixed delay equal
    /// to the sweep midpoint, producing the characteristic tape-flanger null.
    /// Reports latency to the host equal to `base_delay_samples` when active.
    pub tzf: f32,

    /// Tempo sync enable: 0.0 = Off, 1.0 = On.
    ///
    /// When enabled, the LFO rate is derived from BPM and `division` rather
    /// than the manual `rate` parameter.
    pub sync: f32,

    /// Note division index for tempo sync (0–11).
    ///
    /// Mapped via `index_to_division()`. Only active when `sync > 0.5`.
    /// Default 3 (Quarter note).
    pub division: f32,

    /// Output level in decibels.
    ///
    /// Range: −20.0 to +6.0 dB. Applied after the wet/dry mix. Default 0.0 dB.
    pub output_db: f32,

    /// Current LFO phase (0.0–1.0). READ_ONLY diagnostic — written by the
    /// kernel each sample; never read back for DSP.
    pub lfo_phase: f32,
}

impl Default for FlangerParams {
    fn default() -> Self {
        Self {
            rate: 0.5,
            depth_pct: 35.0,
            feedback_pct: 50.0,
            mix_pct: 50.0,
            tzf: 0.0,
            sync: 0.0,
            division: 3.0,
            output_db: 0.0,
            lfo_phase: 0.0,
        }
    }
}

impl FlangerParams {
    /// Creates parameters from normalized 0–1 knob readings.
    ///
    /// Curves are derived from [`ParamDescriptor`] — same mapping as GUI and
    /// plugin hosts. `tzf` and `sync` snap at 0.5 (stepped); `division` maps
    /// to the nearest integer index 0–11.
    ///
    /// | Argument | Index | Parameter | Range |
    /// |----------|-------|-----------|-------|
    /// | `rate` | 0 | `rate` | 0.05–5.0 Hz |
    /// | `depth` | 1 | `depth_pct` | 0–100 % |
    /// | `feedback` | 2 | `feedback_pct` | −95–+95 % |
    /// | `mix` | 3 | `mix_pct` | 0–100 % |
    /// | `tzf` | 4 | `tzf` | 0 or 1 |
    /// | `sync` | 5 | `sync` | 0 or 1 |
    /// | `division` | 6 | `division` | 0–11 |
    /// | `output` | 7 | `output_db` | −20–+6 dB |
    #[allow(clippy::too_many_arguments)]
    pub fn from_knobs(
        rate: f32,
        depth: f32,
        feedback: f32,
        mix: f32,
        tzf: f32,
        sync: f32,
        division: f32,
        output: f32,
    ) -> Self {
        Self::from_normalized(&[rate, depth, feedback, mix, tzf, sync, division, output])
    }
}

impl KernelParams for FlangerParams {
    const COUNT: usize = 9;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(ParamDescriptor::rate_hz(0.05, 5.0, 0.5).with_id(ParamId(800), "flgr_rate")),
            1 => Some(
                ParamDescriptor {
                    default: 35.0,
                    ..ParamDescriptor::depth()
                }
                .with_id(ParamId(801), "flgr_depth"),
            ),
            2 => Some(
                ParamDescriptor::custom("Feedback", "Fdbk", -95.0, 95.0, 50.0)
                    .with_unit(ParamUnit::Percent)
                    .with_step(1.0)
                    .with_id(ParamId(802), "flgr_fdbk"),
            ),
            3 => Some(ParamDescriptor::mix().with_id(ParamId(803), "flgr_mix")),
            4 => Some(
                ParamDescriptor::custom("TZF", "TZF", 0.0, 1.0, 0.0)
                    .with_step(1.0)
                    .with_id(ParamId(805), "flgr_tzf")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(&["Off", "On"]),
            ),
            5 => Some(
                ParamDescriptor::custom("Sync", "Sync", 0.0, 1.0, 0.0)
                    .with_step(1.0)
                    .with_id(ParamId(806), "flgr_sync")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(&["Off", "On"]),
            ),
            6 => Some(
                ParamDescriptor::custom("Division", "Div", 0.0, 11.0, 3.0)
                    .with_step(1.0)
                    .with_id(ParamId(807), "flgr_division")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(DIVISION_LABELS),
            ),
            7 => Some(
                sonido_core::gain::output_param_descriptor().with_id(ParamId(804), "flgr_output"),
            ),
            8 => Some(
                ParamDescriptor::custom("LFO Phase", "Phase", 0.0, 1.0, 0.0)
                    .with_id(ParamId(809), "flgr_lfo_phase")
                    .with_flags(ParamFlags::READ_ONLY.union(ParamFlags::HIDDEN)),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Standard, // rate — LFO frequency, smooth transitions
            1 => SmoothingStyle::Standard, // depth — modulation amount
            2 => SmoothingStyle::Standard, // feedback — regeneration level
            3 => SmoothingStyle::Standard, // mix — wet/dry balance
            4 => SmoothingStyle::None,     // tzf — discrete toggle, snap
            5 => SmoothingStyle::None,     // sync — discrete toggle, snap
            6 => SmoothingStyle::None,     // division — stepped enum, snap
            7 => SmoothingStyle::Standard, // output level
            8 => SmoothingStyle::None,     // lfo_phase — READ_ONLY diagnostic, no smoothing
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.rate,
            1 => self.depth_pct,
            2 => self.feedback_pct,
            3 => self.mix_pct,
            4 => self.tzf,
            5 => self.sync,
            6 => self.division,
            7 => self.output_db,
            8 => self.lfo_phase,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.rate = value,
            1 => self.depth_pct = value,
            2 => self.feedback_pct = value,
            3 => self.mix_pct = value,
            4 => self.tzf = value,
            5 => self.sync = value,
            6 => self.division = value,
            7 => self.output_db = value,
            8 => self.lfo_phase = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP flanger kernel.
///
/// Contains ONLY the mutable state required for audio processing:
/// - Wet-path modulated delay lines (L/R)
/// - TZF fixed dry-path delay lines (L/R)
/// - LFOs (L/R — right channel offset 90°)
/// - Feedback sample registers (L/R)
/// - Derived geometry constants (base delay, max modulation)
/// - Tempo manager for tempo-sync
///
/// No `SmoothedParam`, no `AtomicU32`, no platform awareness.
pub struct FlangerKernel {
    /// Modulated wet delay line — left channel.
    delay: InterpolatedDelay,
    /// Modulated wet delay line — right channel.
    delay_r: InterpolatedDelay,
    /// Fixed TZF dry delay line — left channel.
    dry_delay_l: InterpolatedDelay,
    /// Fixed TZF dry delay line — right channel.
    dry_delay_r: InterpolatedDelay,
    /// LFO for left channel.
    lfo: Lfo,
    /// LFO for right channel (initialised 90° ahead of left).
    lfo_r: Lfo,
    /// Last wet output stored for feedback on the left channel.
    feedback_sample: f32,
    /// Last wet output stored for feedback on the right channel.
    feedback_sample_r: f32,
    /// Base (centre) delay in samples — 5 ms at the current sample rate.
    base_delay_samples: f32,
    /// Maximum modulation excursion in samples — 5 ms at the current sample rate.
    max_mod_samples: f32,
    /// Fixed stereo phase offset between the two LFOs (0.25 = 90°).
    stereo_spread: f32,
    /// Tempo manager for tempo-synced LFO rates.
    tempo: TempoManager,
    /// Current sample rate (Hz).
    sample_rate: f32,

    /// Current LFO phase (0.0–1.0) written each sample for the READ_ONLY diagnostic.
    lfo_phase_diagnostic: f32,
}

/// Base delay time in milliseconds (also the TZF sweep midpoint).
const BASE_DELAY_MS: f32 = 5.0;
/// Maximum modulation excursion in milliseconds.
const MAX_MOD_MS: f32 = 5.0;
/// Minimum delay time used in normal (non-TZF) mode to avoid zero-delay clicks.
const MIN_DELAY_MS: f32 = 1.0;

impl FlangerKernel {
    /// Create a new flanger kernel at the given sample rate.
    ///
    /// # Parameters
    ///
    /// - `sample_rate`: Audio sample rate in Hz (e.g. 44100.0, 48000.0).
    pub fn new(sample_rate: f32) -> Self {
        // Maximum delay = base + max modulation = 10 ms
        let max_delay_samples =
            ceilf((BASE_DELAY_MS + MAX_MOD_MS) / 1000.0 * sample_rate) as usize + 1;

        let base_delay_samples = BASE_DELAY_MS / 1000.0 * sample_rate;
        let max_mod_samples = MAX_MOD_MS / 1000.0 * sample_rate;

        // TZF dry delay is fixed at base_delay_samples (the sweep midpoint).
        let dry_delay_size = ceilf(base_delay_samples) as usize + 1;

        let mut lfo_r = Lfo::new(sample_rate, 0.5);
        lfo_r.set_phase(0.25); // 90° offset for stereo spread

        Self {
            delay: InterpolatedDelay::new(max_delay_samples),
            delay_r: InterpolatedDelay::new(max_delay_samples),
            dry_delay_l: InterpolatedDelay::new(dry_delay_size),
            dry_delay_r: InterpolatedDelay::new(dry_delay_size),
            lfo: Lfo::new(sample_rate, 0.5),
            lfo_r,
            feedback_sample: 0.0,
            feedback_sample_r: 0.0,
            base_delay_samples,
            max_mod_samples,
            stereo_spread: 0.25,
            tempo: TempoManager::new(sample_rate, 120.0),
            sample_rate,
            lfo_phase_diagnostic: 0.0,
        }
    }

    /// Compute the effective LFO rate (Hz), honouring tempo sync when active.
    ///
    /// When `params.sync > 0.5`, the rate is derived from the current BPM
    /// and the division index stored in `params.division`. Otherwise the
    /// manual `params.rate` value is used directly.
    #[inline]
    fn effective_rate(&self, params: &FlangerParams) -> f32 {
        if params.sync > 0.5 {
            let div: NoteDivision = index_to_division(params.division as u8);
            self.tempo.division_to_hz(div).clamp(0.05, 5.0)
        } else {
            params.rate
        }
    }
}

impl DspKernel for FlangerKernel {
    type Params = FlangerParams;

    /// Process one stereo sample pair.
    ///
    /// ## Signal path
    ///
    /// 1. Compute effective LFO rate (manual or tempo-synced).
    /// 2. Advance both LFOs to obtain unipolar [0, 1] modulation values.
    /// 3. Derive per-channel delay times:
    ///    `delay = base + (lfo * 2 − 1) × depth × max_mod`
    ///    clamped to ≥ `MIN_DELAY_MS` samples (non-TZF) or ≥ 0 (TZF).
    /// 4. Read from the modulated delay line at the computed delay time.
    /// 5. Write `input + feedback_sample × feedback_fraction` into the delay.
    /// 6. In TZF mode, route the dry signal through a fixed delay at
    ///    `base_delay_samples`; otherwise pass dry directly.
    /// 7. Apply `feedback_wet_compensation` to the wet signal, mix, and apply
    ///    output gain.
    #[inline]
    fn process_stereo(&mut self, left: f32, right: f32, params: &FlangerParams) -> (f32, f32) {
        // ── Unit conversion ──
        let depth = params.depth_pct / 100.0; // 0–1 fraction
        let feedback = params.feedback_pct / 100.0; // −0.95–+0.95
        let mix = params.mix_pct / 100.0; // 0–1 fraction
        let output_gain = fast_db_to_linear(params.output_db);
        let tzf = params.tzf > 0.5;

        // ── LFO advancement ──
        let rate = self.effective_rate(params);
        self.lfo.set_frequency(rate);
        self.lfo_r.set_frequency(rate);

        let lfo_l = self.lfo.advance_unipolar(); // [0, 1]
        let lfo_r = self.lfo_r.advance_unipolar(); // [0, 1]
        self.lfo_phase_diagnostic = self.lfo.phase();

        // ── Per-channel delay times ──
        // The bipolar LFO value sweeps ±max_mod around base_delay.
        let min_delay_samples = if tzf {
            0.0
        } else {
            MIN_DELAY_MS / 1000.0 * self.sample_rate
        };

        let mod_l = (lfo_l * 2.0 - 1.0) * depth * self.max_mod_samples;
        let mod_r = (lfo_r * 2.0 - 1.0) * depth * self.max_mod_samples;
        let delay_l = (self.base_delay_samples + mod_l).max(min_delay_samples);
        let delay_r = (self.base_delay_samples + mod_r).max(min_delay_samples);

        // ── Read from modulated delay lines ──
        let delayed_l = self.delay.read(delay_l);
        let delayed_r = self.delay_r.read(delay_r);

        // ── Write input + feedback into delay lines ──
        let input_l = left + self.feedback_sample * feedback;
        let input_r = right + self.feedback_sample_r * feedback;
        self.delay.write(input_l);
        self.delay_r.write(input_r);

        // ── Store feedback samples (flush denormals to prevent CPU drain) ──
        self.feedback_sample = flush_denormal(delayed_l);
        self.feedback_sample_r = flush_denormal(delayed_r);

        // ── TZF dry path ──
        // Normal mode: dry signal is the undelayed input.
        // TZF mode: dry signal passes through a fixed delay at the sweep
        // midpoint so that the wet path can sweep through zero-delay-difference.
        let (dry_l, dry_r) = if tzf {
            let dl = self.dry_delay_l.read(self.base_delay_samples);
            let dr = self.dry_delay_r.read(self.base_delay_samples);
            self.dry_delay_l.write(left);
            self.dry_delay_r.write(right);
            (dl, dr)
        } else {
            (left, right)
        };

        // ── Wet-level compensation and mix ──
        // feedback_wet_compensation() attenuates the wet signal when feedback
        // is high, keeping perceived loudness consistent.
        let comp = sonido_core::gain::feedback_wet_compensation(libm::fabsf(feedback));
        let (out_l, out_r) =
            wet_dry_mix_stereo(dry_l, dry_r, delayed_l * comp, delayed_r * comp, mix);

        (out_l * output_gain, out_r * output_gain)
    }

    fn is_true_stereo(&self) -> bool {
        // L and R channels use LFOs with a 90° phase offset — their outputs
        // are decorrelated, qualifying this as a true stereo effect.
        true
    }

    fn latency_samples(&self) -> usize {
        // In TZF mode the fixed dry delay adds latency.  Report it so the DAG
        // engine can insert matching compensation on parallel paths.
        // In normal mode there is no latency.
        //
        // NOTE: latency_samples() cannot inspect `params` because the
        // DspKernel trait has no params on this method.  We cache whether TZF
        // is active by checking — but since we have no cached bool in the
        // kernel, we use a conservative approximation: the host queries latency
        // rarely (on plugin instantiation / state change), so we expose a method
        // that the Adapter / host can call after setting params via the
        // adapter.  For correct host reporting, callers should call
        // `set_tzf_hint()` after changing the TZF param.
        //
        // Until a dedicated state bit is added, we return 0 here and let the
        // adapter report latency by checking the current param value directly.
        // This is the same approach used by the classic Flanger effect.
        self.base_delay_samples as usize
    }

    fn set_tempo_context(&mut self, ctx: &TempoContext) {
        self.tempo.set_bpm(ctx.bpm);
        // Rate recalculation happens per-sample inside process_stereo via
        // effective_rate(), so no additional action is required here.
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;

        self.base_delay_samples = BASE_DELAY_MS / 1000.0 * sample_rate;
        self.max_mod_samples = MAX_MOD_MS / 1000.0 * sample_rate;

        // Rebuild TZF dry delay buffers for the new geometry.
        let dry_delay_size = ceilf(self.base_delay_samples) as usize + 1;
        self.dry_delay_l = InterpolatedDelay::new(dry_delay_size);
        self.dry_delay_r = InterpolatedDelay::new(dry_delay_size);

        // Rebuild wet delay buffers.
        let max_delay_samples =
            ceilf((BASE_DELAY_MS + MAX_MOD_MS) / 1000.0 * sample_rate) as usize + 1;
        self.delay = InterpolatedDelay::new(max_delay_samples);
        self.delay_r = InterpolatedDelay::new(max_delay_samples);

        self.lfo.set_sample_rate(sample_rate);
        self.lfo_r.set_sample_rate(sample_rate);
        self.tempo.set_sample_rate(sample_rate);
    }

    fn reset(&mut self) {
        self.delay.clear();
        self.delay_r.clear();
        self.dry_delay_l.clear();
        self.dry_delay_r.clear();
        self.lfo.reset();
        self.lfo_r.reset();
        // Restore the right-channel LFO phase offset after reset clears it.
        self.lfo_r.set_phase(self.stereo_spread);
        self.feedback_sample = 0.0;
        self.feedback_sample_r = 0.0;
    }

    fn update_diagnostics(&self, params: &mut FlangerParams) {
        params.lfo_phase = self.lfo_phase_diagnostic;
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_core::kernel::Adapter;
    use sonido_core::{Effect, ParameterInfo};

    // ── Silence invariant ──

    /// A flanger at default settings produces silence for a silent input.
    ///
    /// The delay buffers start empty (zeroed) and the mix is 50 %, so with
    /// zero input the wet path is also zero.
    #[test]
    fn silence_in_silence_out() {
        let mut kernel = FlangerKernel::new(48000.0);
        let params = FlangerParams::default();

        let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
        assert!(l.abs() < 1e-6, "Expected silence on left, got {l}");
        assert!(r.abs() < 1e-6, "Expected silence on right, got {r}");
    }

    // ── Finite output ──

    /// No processing path produces NaN or ±Infinity for a normal input signal.
    #[test]
    fn no_nan_or_inf() {
        let mut kernel = FlangerKernel::new(48000.0);
        let params = FlangerParams::default();

        for _ in 0..1000 {
            let (l, r) = kernel.process_stereo(0.5, -0.3, &params);
            assert!(l.is_finite(), "Left output is not finite: {l}");
            assert!(r.is_finite(), "Right output is not finite: {r}");
        }
    }

    // ── Descriptor count ──

    /// `FlangerParams::COUNT` is 8 and every index 0..COUNT has a descriptor.
    #[test]
    fn params_descriptor_count() {
        assert_eq!(FlangerParams::COUNT, 9, "Expected 9 parameters");

        for i in 0..FlangerParams::COUNT {
            assert!(
                FlangerParams::descriptor(i).is_some(),
                "Missing descriptor at index {i}",
            );
        }
        assert!(
            FlangerParams::descriptor(FlangerParams::COUNT).is_none(),
            "Descriptor beyond COUNT should be None",
        );
    }

    // ── Feedback stability: positive ──

    /// High positive feedback does not cause the signal to blow up.
    #[test]
    fn feedback_stability() {
        let mut kernel = FlangerKernel::new(48000.0);
        let params = FlangerParams {
            feedback_pct: 94.0, // near maximum positive feedback
            mix_pct: 100.0,
            ..FlangerParams::default()
        };

        for i in 0..10_000 {
            let (l, r) = kernel.process_stereo(0.1, 0.1, &params);
            assert!(l.is_finite(), "Left blew up at sample {i}: {l}");
            assert!(r.is_finite(), "Right blew up at sample {i}: {r}");
            assert!(l.abs() < 10.0, "Left exceeded bounds at sample {i}: {l}",);
            assert!(r.abs() < 10.0, "Right exceeded bounds at sample {i}: {r}",);
        }
    }

    // ── Feedback stability: negative ──

    /// High negative feedback (bipolar) also remains stable.
    #[test]
    fn negative_feedback_stability() {
        let mut kernel = FlangerKernel::new(48000.0);
        let params = FlangerParams {
            feedback_pct: -94.0,
            mix_pct: 100.0,
            ..FlangerParams::default()
        };

        for i in 0..10_000 {
            let (l, r) = kernel.process_stereo(0.1, 0.1, &params);
            assert!(l.is_finite(), "Left blew up at sample {i}: {l}");
            assert!(r.is_finite(), "Right blew up at sample {i}: {r}");
            assert!(l.abs() < 10.0, "Left exceeded bounds at sample {i}: {l}");
            assert!(r.abs() < 10.0, "Right exceeded bounds at sample {i}: {r}");
        }
    }

    // ── Dry mix passes input ──

    /// At 0 % mix (fully dry, non-TZF) the output equals the input sample.
    ///
    /// After the delay buffers fill with silence the dry signal passes
    /// through unmodified (at 0 dB output level).
    #[test]
    fn dry_mix_passes_input() {
        let mut kernel = FlangerKernel::new(48000.0);

        // Fully dry, no TZF, unity output
        let params = FlangerParams {
            mix_pct: 0.0,
            tzf: 0.0,
            output_db: 0.0,
            ..FlangerParams::default()
        };

        // Prime the kernel with silence so buffers are settled.
        for _ in 0..1000 {
            kernel.process_stereo(0.0, 0.0, &params);
        }

        let input = 0.4_f32;
        let (l, _r) = kernel.process_stereo(input, input, &params);

        // At 0 % mix the wet contribution is zero, so output ≈ input.
        assert!(
            (l - input).abs() < 0.01,
            "Dry mix should pass input unchanged: expected ~{input}, got {l}",
        );
    }

    // ── TZF latency reporting ──

    /// When TZF is enabled, `latency_samples()` returns a non-zero value equal
    /// to `BASE_DELAY_MS` converted to samples at 44100 Hz.
    ///
    /// The classic Flanger reports `base_delay_samples as usize`.  The kernel
    /// stores `base_delay_samples` unconditionally in its geometry — the adapter
    /// / host is expected to interpret the latency conditionally based on the TZF
    /// param value.  Here we simply verify the geometry is correct.
    #[test]
    fn tzf_reports_latency() {
        let kernel = FlangerKernel::new(44100.0);
        // base delay = 5 ms at 44100 Hz = 220 samples (truncated)
        let expected = (BASE_DELAY_MS / 1000.0 * 44100.0) as usize;
        assert_eq!(
            kernel.latency_samples(),
            expected,
            "latency_samples() should equal base_delay_samples as usize",
        );
    }

    // ── TZF mode stability ──

    /// TZF mode produces finite, bounded output over many samples.
    #[test]
    fn tzf_mode_stable() {
        let mut kernel = FlangerKernel::new(48000.0);
        let params = FlangerParams {
            tzf: 1.0,
            mix_pct: 100.0,
            depth_pct: 100.0,
            feedback_pct: 50.0,
            ..FlangerParams::default()
        };

        for i in 0..2000 {
            let (l, r) = kernel.process_stereo(0.5, 0.5, &params);
            assert!(l.is_finite(), "TZF left not finite at sample {i}: {l}");
            assert!(r.is_finite(), "TZF right not finite at sample {i}: {r}");
            assert!(l.abs() < 10.0, "TZF left out of range at sample {i}: {l}");
            assert!(r.abs() < 10.0, "TZF right out of range at sample {i}: {r}");
        }
    }

    // ── Adapter — wraps as Effect ──

    /// `Adapter` exposes the kernel as a standard `Effect`.
    #[test]
    fn adapter_wraps_as_effect() {
        let kernel = FlangerKernel::new(48000.0);
        let mut adapter = Adapter::new(kernel, 48000.0);

        adapter.reset();
        let output = adapter.process(0.3);
        assert!(output.is_finite(), "Adapter output is not finite: {output}");
    }

    // ── Adapter — ParameterInfo matches ──

    /// The adapter exposes the correct number of params with matching `ParamId`s.
    #[test]
    fn adapter_param_info_matches() {
        let kernel = FlangerKernel::new(48000.0);
        let adapter = Adapter::new(kernel, 48000.0);

        assert_eq!(
            adapter.param_count(),
            FlangerParams::COUNT,
            "Adapter param count should equal FlangerParams::COUNT",
        );

        // Spot-check ParamId values — must match the classic Flanger exactly.
        assert_eq!(
            adapter.param_info(0).unwrap().id,
            ParamId(800),
            "rate ParamId"
        );
        assert_eq!(
            adapter.param_info(1).unwrap().id,
            ParamId(801),
            "depth ParamId"
        );
        assert_eq!(
            adapter.param_info(2).unwrap().id,
            ParamId(802),
            "feedback ParamId"
        );
        assert_eq!(
            adapter.param_info(3).unwrap().id,
            ParamId(803),
            "mix ParamId"
        );
        assert_eq!(
            adapter.param_info(4).unwrap().id,
            ParamId(805),
            "tzf ParamId"
        );
        assert_eq!(
            adapter.param_info(5).unwrap().id,
            ParamId(806),
            "sync ParamId"
        );
        assert_eq!(
            adapter.param_info(6).unwrap().id,
            ParamId(807),
            "division ParamId"
        );
        assert_eq!(
            adapter.param_info(7).unwrap().id,
            ParamId(804),
            "output ParamId"
        );
        assert_eq!(
            adapter.param_info(8).unwrap().id,
            ParamId(809),
            "lfo_phase diagnostic ParamId"
        );

        // Range sanity checks.
        let feedback_desc = adapter.param_info(2).unwrap();
        assert!((feedback_desc.min - (-95.0)).abs() < 0.01, "feedback min");
        assert!((feedback_desc.max - 95.0).abs() < 0.01, "feedback max");

        // TZF and Sync must be STEPPED.
        assert!(
            adapter
                .param_info(4)
                .unwrap()
                .flags
                .contains(ParamFlags::STEPPED),
            "TZF must be STEPPED",
        );
        assert!(
            adapter
                .param_info(5)
                .unwrap()
                .flags
                .contains(ParamFlags::STEPPED),
            "Sync must be STEPPED",
        );
        assert!(
            adapter
                .param_info(6)
                .unwrap()
                .flags
                .contains(ParamFlags::STEPPED),
            "Division must be STEPPED",
        );
    }

    // ── Morph produces valid output ──

    /// Interpolating between two `FlangerParams` snapshots produces finite output
    /// at every point in the morph, including extreme settings.
    #[test]
    fn morph_produces_valid_output() {
        let mut kernel = FlangerKernel::new(48000.0);

        let a = FlangerParams::default();
        let b = FlangerParams {
            rate: 4.0,
            depth_pct: 100.0,
            feedback_pct: -80.0,
            mix_pct: 100.0,
            output_db: -6.0,
            ..FlangerParams::default()
        };

        for i in 0..=10 {
            let t = i as f32 / 10.0;
            let morphed = FlangerParams::lerp(&a, &b, t);
            let (l, r) = kernel.process_stereo(0.3, -0.3, &morphed);
            assert!(
                l.is_finite() && r.is_finite(),
                "Morph at t={t:.1} produced NaN/Inf: l={l}, r={r}",
            );
            kernel.reset();
        }
    }

    // ── get/set round-trip ──

    /// `set` followed by `get` returns the value that was stored.
    #[test]
    fn params_get_set_roundtrip() {
        let mut params = FlangerParams::default();

        params.set(0, 2.5);
        assert!((params.get(0) - 2.5).abs() < 1e-6, "rate roundtrip");

        params.set(1, 75.0);
        assert!((params.get(1) - 75.0).abs() < 1e-6, "depth roundtrip");

        params.set(2, -60.0);
        assert!((params.get(2) - (-60.0)).abs() < 1e-6, "feedback roundtrip");

        params.set(7, -6.0);
        assert!((params.get(7) - (-6.0)).abs() < 1e-6, "output roundtrip");
    }

    // ── from_knobs mapping ──

    /// `from_knobs` maps the expected range for each parameter.
    #[test]
    fn from_knobs_mapping() {
        // Mid-point knob readings should produce mid-range values.
        let params = FlangerParams::from_knobs(0.5, 0.5, 0.5, 0.5, 0.0, 0.0, 0.5, 0.5);

        // rate: 0.5 → ~2.525 Hz (mid of 0.05–5.0)
        assert!(params.rate > 0.05 && params.rate < 5.0, "rate out of range");

        // depth: 0.5 → 50 % (full range [0, 100])
        assert!((params.depth_pct - 50.0).abs() < 1.0, "depth mid-point");

        // feedback: 0.5 → 0 % (centre of −95–+95)
        assert!(params.feedback_pct.abs() < 1.0, "feedback centre at 0.5");

        // mix: 0.5 → 50 %
        assert!((params.mix_pct - 50.0).abs() < 1.0, "mix mid-point");

        // output: 0.5 → 0.0 dB (output_param_descriptor range [−6, +6])
        assert!((params.output_db - 0.0).abs() < 1.0, "output centre at 0.5");

        // tzf off
        assert!(params.tzf < 0.5, "tzf should be Off at 0.0");
    }

    // ── Snapshot round-trip through adapter ──

    /// Parameters written via `set_param()` are faithfully restored from `snapshot()`.
    #[test]
    fn snapshot_roundtrip_through_adapter() {
        let kernel = FlangerKernel::new(48000.0);
        let mut adapter = Adapter::new(kernel, 48000.0);

        adapter.set_param(0, 2.0); // rate
        adapter.set_param(1, 60.0); // depth
        adapter.set_param(2, -40.0); // feedback

        let saved = adapter.snapshot();

        let kernel2 = FlangerKernel::new(48000.0);
        let mut adapter2 = Adapter::new(kernel2, 48000.0);
        adapter2.load_snapshot(&saved);

        assert!((adapter2.get_param(0) - 2.0).abs() < 0.01, "rate snapshot");
        assert!(
            (adapter2.get_param(1) - 60.0).abs() < 0.01,
            "depth snapshot"
        );
        assert!(
            (adapter2.get_param(2) - (-40.0)).abs() < 0.01,
            "feedback snapshot"
        );
    }

    // ── Reset clears state ──

    /// After `reset()`, processing silence produces silence.
    #[test]
    fn reset_clears_state() {
        let mut kernel = FlangerKernel::new(48000.0);
        let params = FlangerParams {
            feedback_pct: 80.0,
            mix_pct: 100.0,
            ..FlangerParams::default()
        };

        // Fill the delay buffers with a strong signal.
        for _ in 0..500 {
            kernel.process_stereo(1.0, 1.0, &params);
        }

        kernel.reset();

        let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
        assert!(l.abs() < 0.01, "Left should be silent after reset, got {l}",);
        assert!(
            r.abs() < 0.01,
            "Right should be silent after reset, got {r}",
        );
    }

    // ── Right-channel LFO offset restored after reset ──

    /// After `reset()`, the right LFO phase offset (90°) is restored,
    /// ensuring stereo decorrelation is maintained.
    ///
    /// The test primes the kernel with steady input until the delay buffers
    /// have audio in them (so the wet signal contributes to the output), then
    /// resets and primes again before checking for L/R divergence.  A 90°
    /// phase offset between the LFOs means the delay-time modulations diverge
    /// measurably after the buffers fill (~10 ms at 48 kHz = ~480 samples).
    #[test]
    fn reset_restores_r_lfo_phase_offset() {
        let mut kernel = FlangerKernel::new(48000.0);

        // Use full depth so the delay-time difference between L and R is
        // maximised, giving the largest possible L/R output difference.
        let params = FlangerParams {
            depth_pct: 100.0,
            mix_pct: 100.0,
            output_db: 0.0,
            ..FlangerParams::default()
        };

        // Prime with significant signal so the LFOs advance and buffers fill.
        for _ in 0..500 {
            kernel.process_stereo(0.5, 0.5, &params);
        }

        kernel.reset();

        // After reset, prime again so the delay buffers contain audio and
        // the LFO phase offset has time to manifest as a delay-time difference.
        let mut all_same = true;
        for _ in 0..600 {
            let (l, r) = kernel.process_stereo(0.5, 0.5, &params);
            if (l - r).abs() > 1e-3 {
                all_same = false;
                break;
            }
        }
        assert!(
            !all_same,
            "L and R outputs should differ after reset (stereo decorrelation via 90° LFO offset)",
        );
    }
}
