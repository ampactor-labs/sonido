//! Tremolo kernel — pure DSP with separated parameter ownership.
//!
//! This is the kernel-architecture equivalent of the classic `Tremolo` effect.
//! The DSP math is identical; the difference is structural:
//!
//! - **Classic `Tremolo`**: owns `SmoothedParam` for rate/depth/output, manages smoothing
//!   internally, implements `Effect` + `ParameterInfo` directly.
//!
//! - **`TremoloKernel`**: owns ONLY DSP state (two `Lfo` instances and a `TempoManager`).
//!   Parameters are received via `&TremoloParams` on each processing call.
//!   Deployed via [`KernelAdapter`](sonido_core::KernelAdapter) for desktop/plugin,
//!   or called directly on embedded targets.
//!
//! # Signal Flow
//!
//! ```text
//! Input ──→ Gain Modulation ──→ Output Level
//!               ↑
//!           LFO (L/R with phase spread)
//! ```
//!
//! Amplitude modulation formula:
//!
//! ```text
//! gain = 1.0 - (depth * (1.0 - lfo_unipolar))
//! output = input * gain * output_level
//! ```
//!
//! When `lfo_unipolar = 1.0`, gain = 1.0 (no attenuation).
//! When `lfo_unipolar = 0.0`, gain = 1.0 - depth (maximum attenuation).
//!
//! # Stereo Spread
//!
//! The right-channel LFO is phase-offset by `stereo_spread_pct / 100.0 * 0.5`
//! (i.e., 0–50% of the cycle period = 0°–180° phase difference). At full spread
//! (100%), when L is at maximum gain, R is at minimum gain, producing auto-pan.
//!
//! # Tempo Sync
//!
//! When `sync > 0.5`, the LFO rate is derived from the current BPM and the
//! selected note division. `set_tempo_context()` updates the `TempoManager`
//! and recalculates the synced rate.
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! let adapter = KernelAdapter::new(TremoloKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing, ADCs are hardware-filtered)
//! let mut kernel = TremoloKernel::new(48000.0);
//! let params = TremoloParams::from_knobs(0.3, 0.7, 0.0, 0.0, 0.0, 0.0, 0.5);
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

use sonido_core::{
    DIVISION_LABELS, Lfo, LfoWaveform, ParamDescriptor, ParamFlags, ParamId, ParamUnit,
    TempoManager, index_to_division,
    kernel::{DspKernel, KernelParams, SmoothingStyle},
};

// ── Unit conversion (inlined, no_std safe) ──

/// Fast dB-to-linear conversion for per-sample use.
///
/// Uses `sonido_core::fast_db_to_linear` which is a polynomial approximation
/// (~0.1 dB accuracy, ~4x faster than `10^(db/20)`).
#[inline]
fn db_to_gain(db: f32) -> f32 {
    sonido_core::fast_db_to_linear(db)
}

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`TremoloKernel`].
///
/// All values are in **user-facing units** — the same units shown in GUIs and
/// stored in presets. The kernel converts internally as needed.
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `rate` | Hz | 0.5–20.0 | 5.0 |
/// | 1 | `depth_pct` | % | 0–100 | 50.0 |
/// | 2 | `waveform` | index | 0–3 | 0 (Sine) |
/// | 3 | `stereo_spread_pct` | % | 0–100 | 0.0 |
/// | 4 | `sync` | index | 0–1 | 0 (Off) |
/// | 5 | `division` | index | 0–11 | 3 (Eighth) |
/// | 6 | `output_db` | dB | −20–20 | 0.0 |
#[derive(Debug, Clone, Copy)]
pub struct TremoloParams {
    /// LFO rate in Hz (0.5–20.0 Hz).
    pub rate: f32,
    /// Modulation depth in percent (0–100).
    pub depth_pct: f32,
    /// Waveform index: 0=Sine, 1=Triangle, 2=Square, 3=SampleHold.
    pub waveform: f32,
    /// Stereo spread in percent (0=dual-mono, 100=full auto-pan / 180° offset).
    pub stereo_spread_pct: f32,
    /// Tempo sync: 0=Off, 1=On.
    pub sync: f32,
    /// Note division index for tempo sync (0–11, see `DIVISION_LABELS`).
    pub division: f32,
    /// Output level in decibels (−20–20 dB).
    pub output_db: f32,
}

impl Default for TremoloParams {
    /// Defaults match the classic Tremolo effect's descriptor defaults exactly.
    fn default() -> Self {
        Self {
            rate: 5.0,
            depth_pct: 50.0,
            waveform: 0.0,
            stereo_spread_pct: 0.0,
            sync: 0.0,
            division: 3.0,
            output_db: 0.0,
        }
    }
}

impl TremoloParams {
    /// Build params directly from hardware knob readings (0.0–1.0 normalized).
    ///
    /// Convenience for embedded targets where ADC values map linearly (or
    /// near-linearly) to parameter ranges. Stepped parameters (`waveform`,
    /// `sync`, `division`) are quantized to their nearest integer index.
    ///
    /// # Arguments
    ///
    /// - `rate` — 0.0→0.5 Hz, 1.0→20.0 Hz (linear)
    /// - `depth` — 0.0→0%, 1.0→100%
    /// - `waveform` — 0.0→Sine(0), 0.33→Triangle(1), 0.66→Square(2), 1.0→SampleHold(3)
    /// - `stereo_spread` — 0.0→0%, 1.0→100%
    /// - `sync` — below 0.5→Off(0), 0.5+→On(1)
    /// - `division` — 0.0→index 0 (Whole), 1.0→index 11 (TripletSixteenth)
    /// - `output` — 0.0→−20 dB, 0.5→0 dB, 1.0→+20 dB
    pub fn from_knobs(
        rate: f32,
        depth: f32,
        waveform: f32,
        stereo_spread: f32,
        sync: f32,
        division: f32,
        output: f32,
    ) -> Self {
        Self {
            rate: 0.5 + rate * 19.5,                   // 0.5–20.0 Hz
            depth_pct: depth * 100.0,                  // 0–100%
            waveform: libm::floorf(waveform * 3.99),   // 0, 1, 2, 3
            stereo_spread_pct: stereo_spread * 100.0,  // 0–100%
            sync: if sync >= 0.5 { 1.0 } else { 0.0 }, // 0 or 1
            division: libm::floorf(division * 11.99),  // 0–11
            output_db: output * 40.0 - 20.0,           // −20–20 dB
        }
    }
}

impl KernelParams for TremoloParams {
    const COUNT: usize = 7;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(ParamDescriptor::rate_hz(0.5, 20.0, 5.0).with_id(ParamId(1000), "trem_rate")),
            1 => Some(ParamDescriptor::depth().with_id(ParamId(1001), "trem_depth")),
            2 => Some(
                ParamDescriptor::custom("Waveform", "Wave", 0.0, 3.0, 0.0)
                    .with_unit(ParamUnit::None)
                    .with_step(1.0)
                    .with_id(ParamId(1002), "trem_waveform")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(&["Sine", "Triangle", "Square", "SampleHold"]),
            ),
            3 => Some(
                ParamDescriptor::custom("Stereo Spread", "Spread", 0.0, 100.0, 0.0)
                    .with_unit(ParamUnit::Percent)
                    .with_id(ParamId(1004), "trem_stereo_spread"),
            ),
            4 => Some(
                ParamDescriptor::custom("Sync", "Sync", 0.0, 1.0, 0.0)
                    .with_step(1.0)
                    .with_id(ParamId(1005), "trem_sync")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(&["Off", "On"]),
            ),
            5 => Some(
                ParamDescriptor::custom("Division", "Div", 0.0, 11.0, 3.0)
                    .with_step(1.0)
                    .with_id(ParamId(1006), "trem_division")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(DIVISION_LABELS),
            ),
            6 => Some(
                sonido_core::gain::output_param_descriptor().with_id(ParamId(1003), "trem_output"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Standard, // rate — 10ms, smooth tempo feel
            1 => SmoothingStyle::Standard, // depth — 10ms, click-free depth changes
            2 => SmoothingStyle::None,     // waveform — discrete, snap immediately
            3 => SmoothingStyle::Standard, // stereo spread — 10ms
            4 => SmoothingStyle::None,     // sync — discrete on/off, snap
            5 => SmoothingStyle::None,     // division — discrete, snap
            6 => SmoothingStyle::Standard, // output level — 10ms
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.rate,
            1 => self.depth_pct,
            2 => self.waveform,
            3 => self.stereo_spread_pct,
            4 => self.sync,
            5 => self.division,
            6 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.rate = value,
            1 => self.depth_pct = value,
            2 => self.waveform = value,
            3 => self.stereo_spread_pct = value,
            4 => self.sync = value,
            5 => self.division = value,
            6 => self.output_db = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP tremolo kernel.
///
/// Contains ONLY the mutable state required for audio processing:
/// - Two `Lfo` instances (left/right channels)
/// - A `TempoManager` for tempo-synced rate calculation
/// - The current sample rate
///
/// No `SmoothedParam`, no `AtomicU32`, no platform awareness.
///
/// ## DSP Algorithm
///
/// Classic amplitude modulation using a unipolar LFO:
///
/// ```text
/// gain = 1.0 - (depth * (1.0 - lfo_unipolar))
/// output = input * gain * output_level
/// ```
///
/// The LFO waveform, rate, and per-channel phase offset (stereo spread) are
/// all computed from `TremoloParams` on each call to `process_stereo`.
pub struct TremoloKernel {
    /// LFO for the left (and mono) channel.
    lfo_l: Lfo,
    /// LFO for the right channel — phase-offset for stereo spread.
    lfo_r: Lfo,
    /// Tempo manager used to derive synced LFO frequencies.
    tempo: TempoManager,
    /// Current sample rate in Hz.
    sample_rate: f32,
}

impl TremoloKernel {
    /// Create a new tremolo kernel at the given sample rate.
    ///
    /// Both LFOs are initialized to the default rate (5.0 Hz, Sine waveform),
    /// and the right-channel LFO starts with zero phase offset (dual-mono).
    /// The tempo manager is initialized to 120 BPM.
    pub fn new(sample_rate: f32) -> Self {
        let mut lfo_l = Lfo::new(sample_rate, 5.0);
        lfo_l.set_waveform(LfoWaveform::Sine);

        let mut lfo_r = Lfo::new(sample_rate, 5.0);
        lfo_r.set_waveform(LfoWaveform::Sine);
        // Phase 0.0 — same as L (spread defaults to 0.0 = dual-mono)

        Self {
            lfo_l,
            lfo_r,
            tempo: TempoManager::new(sample_rate, 120.0),
            sample_rate,
        }
    }

    /// Map a waveform index (0–3) to the corresponding `LfoWaveform` variant.
    #[inline]
    fn waveform_from_index(index: u8) -> LfoWaveform {
        match index {
            0 => LfoWaveform::Sine,
            1 => LfoWaveform::Triangle,
            2 => LfoWaveform::Square,
            _ => LfoWaveform::SampleAndHold,
        }
    }

    /// Compute the effective LFO rate in Hz from the current params.
    ///
    /// When `params.sync > 0.5`, the rate is derived from the BPM and division
    /// stored in the `TempoManager`. Otherwise the manual `params.rate` is used.
    /// The result is clamped to the valid LFO range (0.5–20.0 Hz).
    #[inline]
    fn effective_rate(&self, params: &TremoloParams) -> f32 {
        if params.sync > 0.5 {
            let division = index_to_division(params.division as u8);
            self.tempo.division_to_hz(division).clamp(0.5, 20.0)
        } else {
            params.rate.clamp(0.5, 20.0)
        }
    }
}

impl DspKernel for TremoloKernel {
    type Params = TremoloParams;

    fn process_stereo(&mut self, left: f32, right: f32, params: &TremoloParams) -> (f32, f32) {
        // ── Unit conversion (user-facing → internal) ──
        let depth = params.depth_pct / 100.0; // % → 0.0–1.0
        let output = db_to_gain(params.output_db);
        let waveform = Self::waveform_from_index(params.waveform as u8);
        let rate = self.effective_rate(params);

        // Phase offset for R channel: spread% / 100.0 * 0.5 = 0–50% of cycle = 0°–180°
        let spread_offset = params.stereo_spread_pct / 100.0 * 0.5;

        // ── Update LFO state ──
        self.lfo_l.set_frequency(rate);
        self.lfo_r.set_frequency(rate);
        self.lfo_l.set_waveform(waveform);
        self.lfo_r.set_waveform(waveform);

        // Apply stereo spread: R lags L by spread_offset cycles.
        // We compute the desired R phase as L's current phase + spread_offset,
        // wrapped to [0, 1). This is applied each sample so it tracks smoothly
        // when spread is modulated.
        let r_phase = {
            let p = self.lfo_l.phase() + spread_offset;
            if p >= 1.0 { p - 1.0 } else { p }
        };
        self.lfo_r.set_phase(r_phase);

        // ── Advance LFOs ──
        let lfo_l_uni = self.lfo_l.advance_unipolar();
        let lfo_r_uni = self.lfo_r.advance_unipolar();

        // ── Amplitude modulation ──
        // gain = 1.0 - (depth * (1.0 - lfo_unipolar))
        // At lfo_unipolar=1.0 → gain = 1.0  (full pass-through)
        // At lfo_unipolar=0.0 → gain = 1.0 - depth  (maximum attenuation)
        let gain_l = 1.0 - (depth * (1.0 - lfo_l_uni));
        let gain_r = 1.0 - (depth * (1.0 - lfo_r_uni));

        (left * gain_l * output, right * gain_r * output)
    }

    fn is_true_stereo(&self) -> bool {
        // True stereo when R channel carries a different LFO phase from L.
        // The caller holds the params, so we conservatively return true here —
        // the adapter will report based on the params state. For a pure kernel
        // the convention is: this kernel *can* produce decorrelated L/R, so true.
        true
    }

    fn set_tempo_context(&mut self, ctx: &sonido_core::TempoContext) {
        self.tempo.set_bpm(ctx.bpm);
    }

    fn reset(&mut self) {
        self.lfo_l.reset();
        self.lfo_r.reset();
        // After reset the R phase offset is zero; callers that need the spread
        // offset to be restored must call process_stereo() with the correct params,
        // which re-applies the spread on the first sample.
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.lfo_l.set_sample_rate(sample_rate);
        self.lfo_r.set_sample_rate(sample_rate);
        self.tempo.set_sample_rate(sample_rate);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_core::kernel::KernelAdapter;
    use sonido_core::{Effect, ParameterInfo, TempoContext};

    // ── Kernel unit tests ──

    #[test]
    fn silence_in_silence_out() {
        let mut kernel = TremoloKernel::new(48000.0);
        let params = TremoloParams::default();
        let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
        assert!(l.abs() < 1e-6, "Expected silence on L, got {l}");
        assert!(r.abs() < 1e-6, "Expected silence on R, got {r}");
    }

    #[test]
    fn no_nan_or_inf() {
        let mut kernel = TremoloKernel::new(48000.0);
        let params = TremoloParams::default();
        for _ in 0..1000 {
            let (l, r) = kernel.process_stereo(0.5, -0.3, &params);
            assert!(l.is_finite(), "L output is not finite: {l}");
            assert!(r.is_finite(), "R output is not finite: {r}");
        }
    }

    #[test]
    fn params_descriptor_count() {
        assert_eq!(TremoloParams::COUNT, 7);
        for i in 0..TremoloParams::COUNT {
            assert!(
                TremoloParams::descriptor(i).is_some(),
                "Missing descriptor at index {i}"
            );
        }
        assert!(
            TremoloParams::descriptor(TremoloParams::COUNT).is_none(),
            "Descriptor beyond COUNT should be None"
        );
    }

    #[test]
    fn depth_modulates_signal() {
        // At full depth (100%) and high rate, the gain should reach near-zero
        // at the LFO trough. Process enough samples to cover at least one full cycle.
        let mut kernel = TremoloKernel::new(48000.0);
        let params = TremoloParams {
            depth_pct: 100.0,
            rate: 10.0, // 10 Hz → 4800 samples per cycle at 48kHz
            ..Default::default()
        };

        let mut min_output = f32::MAX;
        for _ in 0..9600 {
            let (l, _) = kernel.process_stereo(1.0, 1.0, &params);
            min_output = min_output.min(l);
        }

        assert!(
            min_output < 0.1,
            "Full depth should produce near-zero gain at trough, got min={min_output}"
        );
    }

    #[test]
    fn zero_depth_passthrough() {
        let mut kernel = TremoloKernel::new(48000.0);
        let params = TremoloParams {
            depth_pct: 0.0,
            output_db: 0.0,
            ..Default::default()
        };

        // With zero depth and 0 dB output, all samples should pass unchanged.
        for _ in 0..500 {
            let (l, r) = kernel.process_stereo(0.5, 0.75, &params);
            assert!(
                (l - 0.5).abs() < 1e-5,
                "Zero depth should be passthrough: expected 0.5, got {l}"
            );
            assert!(
                (r - 0.75).abs() < 1e-5,
                "Zero depth should be passthrough: expected 0.75, got {r}"
            );
        }
    }

    #[test]
    fn different_waveforms_produce_different_output() {
        let input = 0.5_f32;
        let mut outputs = [0.0_f32; 4];

        for (waveform_idx, out) in outputs.iter_mut().enumerate() {
            let mut kernel = TremoloKernel::new(48000.0);
            let params = TremoloParams {
                rate: 10.0,
                depth_pct: 80.0,
                waveform: waveform_idx as f32,
                ..Default::default()
            };
            // Advance enough to get past the initial phase
            for _ in 0..500 {
                kernel.process_stereo(input, input, &params);
            }
            let (l, _) = kernel.process_stereo(input, input, &params);
            *out = l;
        }

        // At least some waveform outputs should differ
        let all_same = outputs.windows(2).all(|w| (w[0] - w[1]).abs() < 1e-4);
        assert!(
            !all_same,
            "Different waveforms should produce different outputs: {outputs:?}"
        );
    }

    #[test]
    fn stereo_spread_decorrelates() {
        // At full spread, L and R should be anti-correlated (phase-opposite gains).
        let mut kernel = TremoloKernel::new(48000.0);
        let params = TremoloParams {
            depth_pct: 100.0,
            rate: 5.0,
            stereo_spread_pct: 100.0, // Full 180° offset
            ..Default::default()
        };

        // Collect L×R product over many samples. Anti-correlated gains → low average product.
        let mut sum_product = 0.0_f32;
        let n = 9600_usize;
        for _ in 0..n {
            let (l, r) = kernel.process_stereo(1.0, 1.0, &params);
            sum_product += l * r;
        }

        let avg = sum_product / n as f32;
        // Identical phase → avg ≈ 0.25 (both centered around 0.5 gain).
        // Anti-phase → avg < 0.1.
        assert!(
            avg < 0.2,
            "Full stereo spread should decorrelate L/R, avg product = {avg}"
        );
    }

    // ── Adapter integration tests ──

    #[test]
    fn adapter_wraps_as_effect() {
        let kernel = TremoloKernel::new(48000.0);
        let mut adapter = KernelAdapter::new(kernel, 48000.0);

        adapter.reset();
        let output = adapter.process(0.4);
        assert!(output.is_finite(), "Adapter output must be finite");
    }

    #[test]
    fn adapter_param_info_matches() {
        let kernel = TremoloKernel::new(48000.0);
        let adapter = KernelAdapter::new(kernel, 48000.0);

        assert_eq!(adapter.param_count(), 7, "Must expose exactly 7 parameters");

        // Verify ParamIds match the classic effect exactly (plugin API contract)
        assert_eq!(
            adapter.param_info(0).unwrap().id,
            ParamId(1000),
            "Rate must be ParamId(1000)"
        );
        assert_eq!(
            adapter.param_info(1).unwrap().id,
            ParamId(1001),
            "Depth must be ParamId(1001)"
        );
        assert_eq!(
            adapter.param_info(2).unwrap().id,
            ParamId(1002),
            "Waveform must be ParamId(1002)"
        );
        assert_eq!(
            adapter.param_info(3).unwrap().id,
            ParamId(1004),
            "Stereo Spread must be ParamId(1004)"
        );
        assert_eq!(
            adapter.param_info(4).unwrap().id,
            ParamId(1005),
            "Sync must be ParamId(1005)"
        );
        assert_eq!(
            adapter.param_info(5).unwrap().id,
            ParamId(1006),
            "Division must be ParamId(1006)"
        );
        assert_eq!(
            adapter.param_info(6).unwrap().id,
            ParamId(1003),
            "Output must be ParamId(1003)"
        );

        // Waveform and Sync should be STEPPED
        let waveform_info = adapter.param_info(2).unwrap();
        assert!(
            waveform_info.flags.contains(ParamFlags::STEPPED),
            "Waveform param must be STEPPED"
        );
        let sync_info = adapter.param_info(4).unwrap();
        assert!(
            sync_info.flags.contains(ParamFlags::STEPPED),
            "Sync param must be STEPPED"
        );
    }

    // ── Behavioral / DSP tests ──

    #[test]
    fn morph_produces_valid_output() {
        let mut kernel = TremoloKernel::new(48000.0);
        let a = TremoloParams::default();
        let b = TremoloParams {
            depth_pct: 90.0,
            rate: 15.0,
            stereo_spread_pct: 100.0,
            ..Default::default()
        };

        for i in 0..=10 {
            let t = i as f32 / 10.0;
            let morphed = TremoloParams::lerp(&a, &b, t);
            let (l, r) = kernel.process_stereo(0.3, -0.3, &morphed);
            assert!(
                l.is_finite() && r.is_finite(),
                "Morph at t={t} produced NaN/Inf"
            );
            kernel.reset();
        }
    }

    #[test]
    fn from_knobs_maps_ranges() {
        // Mid-point of all knobs
        let params = TremoloParams::from_knobs(0.5, 0.5, 0.0, 0.5, 0.0, 0.0, 0.5);

        // rate: 0.5 * 19.5 + 0.5 = 10.25 Hz
        assert!(
            (params.rate - 10.25).abs() < 0.01,
            "Mid rate should be ~10.25 Hz, got {}",
            params.rate
        );

        // depth: 0.5 * 100.0 = 50%
        assert!(
            (params.depth_pct - 50.0).abs() < 0.01,
            "Mid depth should be 50%, got {}",
            params.depth_pct
        );

        // waveform: floor(0.0 * 3.99) = 0
        assert_eq!(params.waveform, 0.0, "Waveform should be 0 (Sine)");

        // stereo_spread: 0.5 * 100.0 = 50%
        assert!(
            (params.stereo_spread_pct - 50.0).abs() < 0.01,
            "Mid spread should be 50%, got {}",
            params.stereo_spread_pct
        );

        // sync: 0.0 < 0.5 → Off (0.0)
        assert_eq!(params.sync, 0.0, "Sync should be Off (0.0)");

        // output: 0.5 * 40.0 - 20.0 = 0.0 dB
        assert!(
            (params.output_db - 0.0).abs() < 0.01,
            "Mid output should be 0 dB, got {}",
            params.output_db
        );

        // Full-scale sync enable
        let synced = TremoloParams::from_knobs(0.5, 0.5, 0.0, 0.0, 1.0, 0.0, 0.5);
        assert_eq!(synced.sync, 1.0, "sync=1.0 should be On");

        // Full depth + max rate
        let full = TremoloParams::from_knobs(1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0);
        assert!(
            (full.rate - 20.0).abs() < 0.01,
            "Max rate should be 20 Hz, got {}",
            full.rate
        );
        assert!(
            (full.depth_pct - 100.0).abs() < 0.01,
            "Max depth should be 100%"
        );
        assert!(
            (full.output_db - 20.0).abs() < 0.01,
            "Max output should be +20 dB"
        );
    }

    #[test]
    fn tempo_sync_uses_bpm_and_division() {
        let mut kernel = TremoloKernel::new(48000.0);

        // Tell the kernel about 120 BPM
        kernel.set_tempo_context(&TempoContext {
            bpm: 120.0,
            ..Default::default()
        });

        // Sync on, division 3 = Eighth note (index 3 → NoteDivision::Eighth)
        // At 120 BPM: Eighth = 0.5 beats → (120/60)/0.5 = 4.0 Hz
        let params = TremoloParams {
            sync: 1.0,
            division: 3.0,
            depth_pct: 50.0,
            ..Default::default()
        };

        let rate = kernel.effective_rate(&params);
        assert!(
            (rate - 4.0).abs() < 0.01,
            "Eighth at 120 BPM should be 4.0 Hz, got {rate}"
        );
    }

    #[test]
    fn reset_clears_lfo_phase() {
        let mut kernel = TremoloKernel::new(48000.0);
        let params = TremoloParams::default();

        // Advance enough to change LFO phase significantly
        for _ in 0..2400 {
            kernel.process_stereo(0.5, 0.5, &params);
        }

        // Phase should not be 0.0 at this point
        let phase_before_reset = kernel.lfo_l.phase();

        kernel.reset();
        let phase_after_reset = kernel.lfo_l.phase();

        assert_eq!(
            phase_after_reset, 0.0,
            "LFO L phase should be 0.0 after reset"
        );
        assert_ne!(
            phase_before_reset, 0.0,
            "Phase before reset should be non-zero (test validity check)"
        );
    }

    #[test]
    fn set_sample_rate_updates_lfo() {
        let mut kernel = TremoloKernel::new(48000.0);
        let params = TremoloParams {
            rate: 10.0,
            ..Default::default()
        };

        // Process at 48kHz
        for _ in 0..100 {
            kernel.process_stereo(0.5, 0.5, &params);
        }

        // Switch to 44.1 kHz — should not panic or produce NaN
        kernel.set_sample_rate(44100.0);
        let (l, r) = kernel.process_stereo(0.5, 0.5, &params);
        assert!(l.is_finite(), "L should be finite after sample rate change");
        assert!(r.is_finite(), "R should be finite after sample rate change");
    }

    #[test]
    fn snapshot_roundtrip_through_adapter() {
        let kernel = TremoloKernel::new(48000.0);
        let mut adapter = KernelAdapter::new(kernel, 48000.0);

        adapter.set_param(0, 12.0); // rate = 12.0 Hz
        adapter.set_param(1, 75.0); // depth = 75%
        adapter.set_param(6, -6.0); // output = -6 dB

        let saved = adapter.snapshot();
        assert!(
            (saved.rate - 12.0).abs() < 0.01,
            "Saved rate should be 12.0"
        );
        assert!(
            (saved.depth_pct - 75.0).abs() < 0.01,
            "Saved depth should be 75.0"
        );
        assert!(
            (saved.output_db - (-6.0)).abs() < 0.01,
            "Saved output should be -6.0"
        );

        // Reload into a fresh adapter
        let kernel2 = TremoloKernel::new(48000.0);
        let mut adapter2 = KernelAdapter::new(kernel2, 48000.0);
        adapter2.load_snapshot(&saved);

        assert!(
            (adapter2.get_param(0) - 12.0).abs() < 0.01,
            "Restored rate should be 12.0"
        );
        assert!(
            (adapter2.get_param(1) - 75.0).abs() < 0.01,
            "Restored depth should be 75.0"
        );
    }
}
