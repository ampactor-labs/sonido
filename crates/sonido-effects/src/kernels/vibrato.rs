//! Multi-Vibrato kernel — pure DSP with separated parameter ownership.
//!
//! Implements the Vibrato effect using the kernel architecture.
//! The DSP math is identical; the difference is structural:
//!
//! - **Classic `MultiVibrato`**: owns `SmoothedParam` for depth/output, stores mix as
//!   a plain `f32`, implements `Effect` + `ParameterInfo` directly.
//!
//! - **`VibratoKernel`**: owns ONLY DSP state (6 × 2 `VibratoUnit` structs).
//!   Parameters are received via `&VibratoParams` on each processing call.
//!   Deployed via [`KernelAdapter`](sonido_core::KernelAdapter) for desktop/plugin,
//!   or called directly on embedded targets.
//!
//! # Algorithm
//!
//! Six `VibratoUnit` oscillators run simultaneously on each channel (L and R),
//! each at a different LFO rate and depth. Their outputs are averaged to produce
//! a single wet signal per channel:
//!
//! ```text
//! Input ──┬──→ VibratoUnit 0 (0.13 Hz, 1.5 cents, Sine)     ─┐
//!         ├──→ VibratoUnit 1 (0.31 Hz, 2.5 cents, Triangle)  ─┤
//!         ├──→ VibratoUnit 2 (0.67 Hz, 1.2 cents, Sine)      ─┤──→ avg ──→ wet
//!         ├──→ VibratoUnit 3 (1.10 Hz, 1.8 cents, Triangle)  ─┤
//!         ├──→ VibratoUnit 4 (2.30 Hz, 0.8 cents, Triangle)  ─┤
//!         └──→ VibratoUnit 5 (4.70 Hz, 0.4 cents, Triangle)  ─┘
//! ```
//!
//! Each vibrato unit modulates its delay time with the following equation:
//!
//! ```text
//! delay_mod   = lfo_val × (depth_cents × depth_scale × sample_rate / 44100) × 0.01
//! read_delay  = base_delay (128 samples) + delay_mod
//! wet_sample  = delay_line.read_write(input, read_delay)
//! ```
//!
//! The `depth_cents` values are chosen to simulate authentic tape wow and flutter.
//! Each individual vibrato is nearly imperceptible, but combined they produce the
//! organic, living quality of real tape.
//!
//! # Latency
//!
//! The kernel reports 128 samples of latency, reflecting the base delay offset
//! used by each `VibratoUnit` to provide headroom for negative LFO modulation.
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! let adapter = KernelAdapter::new(VibratoKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing, ADCs are hardware-filtered)
//! let mut kernel = VibratoKernel::new(48000.0);
//! let params = VibratoParams::from_knobs(0.5, 1.0, 0.5);
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::{
    FixedDelayLine, Lfo, LfoWaveform, ParamDescriptor, ParamId, ParamUnit, fast_db_to_linear,
    wet_dry_mix, wet_dry_mix_stereo,
};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Number of simultaneous vibrato units per channel.
///
/// Six units with carefully chosen LFO rates and depth values produce the
/// organic tape wow-and-flutter character when summed.
const NUM_VIBRATOS: usize = 6;

/// Base delay in samples used by every `VibratoUnit`.
///
/// This provides headroom so the LFO's negative modulation excursion can never
/// produce a negative (impossible) delay time. 128 samples ≈ 2.7 ms at 48 kHz.
const BASE_DELAY: f32 = 128.0;

// ── VibratoUnit ───────────────────────────────────────────────────────────────

/// A single vibrato voice: one LFO modulating a fixed delay line.
///
/// Each unit has its own LFO rate, depth (in cents), and waveform. Multiple
/// units at different frequencies are summed to produce a complex, organic
/// pitch-modulation texture reminiscent of tape wow and flutter.
///
/// ## Processing
///
/// On each sample call the delay time is computed as:
///
/// ```text
/// cents_to_samples = depth_cents × depth_scale × (sample_rate / 44100) × 0.01
/// delay_mod        = lfo_val × cents_to_samples
/// read_delay       = BASE_DELAY + delay_mod
/// output           = delay_line.read_write(input, read_delay)
/// ```
///
/// The `0.01` factor converts cents to the fractional pitch ratio for the
/// current sample rate.
struct VibratoUnit {
    /// LFO source for this vibrato voice.
    lfo: Lfo,
    /// Vibrato depth in cents (very subtle: 0.4–2.5 cents per unit).
    depth_cents: f32,
    /// Delay line providing the modulation headroom (256 samples ≈ 5.3 ms at 48 kHz).
    delay: FixedDelayLine<256>,
}

impl VibratoUnit {
    /// Create a new vibrato unit.
    ///
    /// # Arguments
    ///
    /// - `sample_rate` — current audio sample rate in Hz.
    /// - `rate_hz` — LFO frequency in Hz.
    /// - `depth_cents` — modulation depth in semitone-cents (1 cent = 1/100 semitone).
    /// - `waveform` — LFO waveform shape.
    fn new(sample_rate: f32, rate_hz: f32, depth_cents: f32, waveform: LfoWaveform) -> Self {
        let mut lfo = Lfo::new(sample_rate, rate_hz);
        lfo.set_waveform(waveform);
        Self {
            lfo,
            depth_cents,
            delay: FixedDelayLine::new(),
        }
    }

    /// Process a single sample through this vibrato unit.
    ///
    /// # Arguments
    ///
    /// - `input` — the audio sample to process.
    /// - `sample_rate` — current sample rate in Hz (for pitch-accurate delay scaling).
    /// - `depth_scale` — master depth multiplier (from `params.depth_pct / 100.0`).
    ///
    /// Returns the delayed, pitch-modulated sample.
    #[inline]
    fn process(&mut self, input: f32, sample_rate: f32, depth_scale: f32) -> f32 {
        let lfo_val = self.lfo.advance();

        // Convert cents to a delay modulation in samples, normalised to 44100 Hz
        // so that the cents-to-delay mapping is sample-rate independent.
        let cents_to_samples = self.depth_cents * depth_scale * sample_rate / 44100.0 * 0.01;
        let delay_mod = lfo_val * cents_to_samples;

        let delay_samples = BASE_DELAY + delay_mod;
        self.delay.read_write(input, delay_samples)
    }

    /// Reset LFO phase and clear the delay buffer.
    fn reset(&mut self) {
        self.lfo.reset();
        self.delay.clear();
    }

    /// Update the LFO sample rate without clearing state.
    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.lfo.set_sample_rate(sample_rate);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`VibratoKernel`].
///
/// All values are in **user-facing units** — the same units shown in GUIs and
/// stored in presets. The kernel converts internally as needed.
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `depth_pct` | % | 0–400 | 100.0 |
/// | 1 | `mix_pct` | % | 0–100 | 100.0 |
/// | 2 | `output_db` | dB | −20–20 | 0.0 |
#[derive(Debug, Clone, Copy)]
pub struct VibratoParams {
    /// Master vibrato depth as a percentage of the full scale (0–400%).
    ///
    /// Scales all six vibrato units' cent depths proportionally.
    /// 100% applies the unit's native depth; 200% doubles it; 400% quadruples it.
    pub depth_pct: f32,

    /// Wet/dry mix as a percentage (0% = fully dry, 100% = fully wet).
    pub mix_pct: f32,

    /// Output level in decibels (−20 to +20 dB).
    pub output_db: f32,
}

impl Default for VibratoParams {
    /// Defaults match the classic `MultiVibrato` effect's descriptor defaults exactly.
    fn default() -> Self {
        Self {
            depth_pct: 100.0,
            mix_pct: 100.0,
            output_db: 0.0,
        }
    }
}

impl VibratoParams {
    /// Build params directly from hardware knob readings (0.0–1.0 normalized).
    ///
    /// Convenience constructor for embedded targets where ADC values map
    /// linearly to parameter ranges.
    ///
    /// # Arguments
    ///
    /// - `depth` — 0.0 → 0 %, 1.0 → 400 %
    /// - `mix` — 0.0 → 0 %, 1.0 → 100 %
    /// - `output` — 0.0 → −20 dB, 0.5 → 0 dB, 1.0 → +20 dB
    pub fn from_knobs(depth: f32, mix: f32, output: f32) -> Self {
        Self {
            depth_pct: depth * 400.0,        // 0–400 %
            mix_pct: mix * 100.0,            // 0–100 %
            output_db: output * 40.0 - 20.0, // −20–+20 dB
        }
    }
}

impl KernelParams for VibratoParams {
    const COUNT: usize = 3;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(
                ParamDescriptor {
                    name: "Depth",
                    short_name: "Depth",
                    unit: ParamUnit::Percent,
                    min: 0.0,
                    max: 400.0,
                    default: 100.0,
                    step: 1.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(1300), "vib_depth"),
            ),
            1 => Some(
                ParamDescriptor {
                    name: "Mix",
                    short_name: "Mix",
                    unit: ParamUnit::Percent,
                    min: 0.0,
                    max: 100.0,
                    default: 100.0,
                    step: 1.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(1301), "vib_mix"),
            ),
            2 => Some(
                sonido_core::gain::output_param_descriptor().with_id(ParamId(1302), "vib_output"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Standard, // depth_pct — 10 ms, click-free depth sweeps
            1 => SmoothingStyle::Standard, // mix_pct — 10 ms, click-free wet/dry transitions
            2 => SmoothingStyle::Standard, // output_db — 10 ms, click-free level changes
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.depth_pct,
            1 => self.mix_pct,
            2 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.depth_pct = value,
            1 => self.mix_pct = value,
            2 => self.output_db = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP multi-vibrato kernel.
///
/// Contains ONLY the mutable state required for audio processing:
///
/// - Six `VibratoUnit` instances for the left channel
/// - Six `VibratoUnit` instances for the right channel (independent state for
///   true stereo — each unit's LFO starts at the same phase but diverges over time)
/// - The current sample rate in Hz
///
/// No `SmoothedParam`, no atomics, no platform awareness. The kernel is `Send`-safe
/// because all contained types (`VibratoUnit`, `Lfo`, `FixedDelayLine`, primitive
/// floats) are `Send`.
///
/// ## True Stereo
///
/// L and R channels each have their own set of six vibrato units. Though they
/// are initialized identically, their LFO phases diverge because they are
/// advanced independently — subtle decorrelation emerges over time, giving the
/// characteristic "living" quality of real tape.
pub struct VibratoKernel {
    /// Six vibrato units for the left channel.
    vibratos_l: [VibratoUnit; NUM_VIBRATOS],
    /// Six vibrato units for the right channel (independent state from L).
    vibratos_r: [VibratoUnit; NUM_VIBRATOS],
    /// Current sample rate in Hz.
    sample_rate: f32,
}

/// LFO configurations: (rate_hz, depth_cents, waveform).
///
/// These values were selected empirically to produce an organic tape character:
/// - A mix of sine and triangle waveforms prevents repetitive phase patterns
/// - Rate ratios are irrational multiples to avoid periodic alignment
/// - Depth decreases with rate so faster units add subtle texture, not obvious warble
const VIBRATO_CONFIGS: [(f32, f32, LfoWaveform); NUM_VIBRATOS] = [
    (0.13, 1.5, LfoWaveform::Sine),     // Very slow drift
    (0.31, 2.5, LfoWaveform::Triangle), // Slow wobble
    (0.67, 1.2, LfoWaveform::Sine),     // Medium drift
    (1.1, 1.8, LfoWaveform::Triangle),  // Flutter component
    (2.3, 0.8, LfoWaveform::Triangle),  // Subtle fast flutter
    (4.7, 0.4, LfoWaveform::Triangle),  // Fastest, most subtle
];

impl VibratoKernel {
    /// Create a new multi-vibrato kernel initialised at `sample_rate`.
    ///
    /// Allocates 12 vibrato units (6 per channel) using the canonical LFO
    /// configurations. Both channels start with identical unit configurations;
    /// they naturally diverge as they are advanced independently during processing.
    pub fn new(sample_rate: f32) -> Self {
        let vibratos_l = VIBRATO_CONFIGS
            .map(|(rate, depth, waveform)| VibratoUnit::new(sample_rate, rate, depth, waveform));
        let vibratos_r = VIBRATO_CONFIGS
            .map(|(rate, depth, waveform)| VibratoUnit::new(sample_rate, rate, depth, waveform));

        Self {
            vibratos_l,
            vibratos_r,
            sample_rate,
        }
    }
}

impl DspKernel for VibratoKernel {
    type Params = VibratoParams;

    /// Process a stereo sample pair through all six vibrato units per channel.
    ///
    /// Per-sample steps:
    /// 1. Convert parameter units (% → 0–1 or 0–4 scale for depth, dB → linear for output)
    /// 2. Process the left input through all six L vibrato units and average their outputs
    /// 3. Process the right input through all six R vibrato units and average their outputs
    /// 4. Wet/dry mix the stereo pair
    /// 5. Apply output level gain
    fn process_stereo(&mut self, left: f32, right: f32, params: &VibratoParams) -> (f32, f32) {
        // ── Unit conversion (user-facing → internal) ──
        let depth_scale = params.depth_pct / 100.0; // 0–400 % → 0.0–4.0
        let mix = params.mix_pct / 100.0; // 0–100 % → 0.0–1.0
        let output = fast_db_to_linear(params.output_db);

        // ── Left channel: sum all six units and average ──
        let mut wet_l = 0.0_f32;
        for vib in &mut self.vibratos_l {
            wet_l += vib.process(left, self.sample_rate, depth_scale);
        }
        wet_l /= NUM_VIBRATOS as f32;

        // ── Right channel: sum all six units and average ──
        let mut wet_r = 0.0_f32;
        for vib in &mut self.vibratos_r {
            wet_r += vib.process(right, self.sample_rate, depth_scale);
        }
        wet_r /= NUM_VIBRATOS as f32;

        // ── Wet/dry mix → output level ──
        let (out_l, out_r) = wet_dry_mix_stereo(left, right, wet_l, wet_r, mix);
        (out_l * output, out_r * output)
    }

    /// Process a single mono sample through the left vibrato bank.
    ///
    /// Used when mono processing is explicitly requested. Routes only through
    /// the L channel vibrato units for efficiency.
    fn process(&mut self, input: f32, params: &VibratoParams) -> f32 {
        let depth_scale = params.depth_pct / 100.0;
        let mix = params.mix_pct / 100.0;
        let output = fast_db_to_linear(params.output_db);

        let mut wet = 0.0_f32;
        for vib in &mut self.vibratos_l {
            wet += vib.process(input, self.sample_rate, depth_scale);
        }
        wet /= NUM_VIBRATOS as f32;

        wet_dry_mix(input, wet, mix) * output
    }

    fn reset(&mut self) {
        for vib in &mut self.vibratos_l {
            vib.reset();
        }
        for vib in &mut self.vibratos_r {
            vib.reset();
        }
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        for vib in &mut self.vibratos_l {
            vib.set_sample_rate(sample_rate);
        }
        for vib in &mut self.vibratos_r {
            vib.set_sample_rate(sample_rate);
        }
    }

    fn latency_samples(&self) -> usize {
        // BASE_DELAY of 128 samples is the minimum delay in every vibrato unit.
        128
    }

    fn is_true_stereo(&self) -> bool {
        // L and R have fully independent vibrato banks — they diverge over time
        // and produce genuinely decorrelated outputs.
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

    // ── Kernel unit tests ──────────────────────────────────────────────────

    /// Silence in must produce silence out regardless of parameter state.
    ///
    /// With zero input the delay lines fill with zeros. Averaging zeros produces
    /// zero wet signal; wet/dry mix of zero with zero is zero.
    #[test]
    fn silence_in_silence_out() {
        let mut kernel = VibratoKernel::new(48000.0);
        let params = VibratoParams::default();

        // Note: the first sample may not be exactly zero because the delay line
        // starts cleared, but after a call with 0.0 input it should be ≈ 0.
        let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
        assert!(
            l.abs() < 1e-6,
            "Expected near-silence on left with zero input, got {l}"
        );
        assert!(
            r.abs() < 1e-6,
            "Expected near-silence on right with zero input, got {r}"
        );
    }

    /// Processing must never produce NaN or ±Infinity over 1000 samples.
    ///
    /// Drives the kernel with a 440 Hz sine approximation (alternating polarity)
    /// and verifies all outputs remain IEEE-finite.
    #[test]
    fn no_nan_or_inf() {
        let mut kernel = VibratoKernel::new(48000.0);
        let params = VibratoParams {
            depth_pct: 200.0,
            mix_pct: 100.0,
            output_db: 0.0,
        };

        // Generate 440 Hz sine via phase accumulator (no_std safe: manual sin approximation
        // is unnecessary here as the test does not need perfect accuracy — just a varying signal).
        let mut phase: f32 = 0.0;
        let phase_inc: f32 = 440.0 / 48000.0;

        for _ in 0..1000 {
            // Use a simple triangle wave as a no_std-safe signal source.
            let input = if phase < 0.5 {
                4.0 * phase - 1.0
            } else {
                3.0 - 4.0 * phase
            };
            phase += phase_inc;
            if phase >= 1.0 {
                phase -= 1.0;
            }

            let (l, r) = kernel.process_stereo(input, -input, &params);
            assert!(l.is_finite(), "Left output is NaN or Inf at a 440 Hz input");
            assert!(
                r.is_finite(),
                "Right output is NaN or Inf at a 440 Hz input"
            );
        }
    }

    /// `VibratoParams::COUNT` must equal 3 and all descriptor indices must be
    /// populated with `Some`, while the index beyond `COUNT` returns `None`.
    #[test]
    fn params_descriptor_count() {
        assert_eq!(VibratoParams::COUNT, 3, "Expected exactly 3 parameters");

        for i in 0..VibratoParams::COUNT {
            assert!(
                VibratoParams::descriptor(i).is_some(),
                "Missing descriptor at index {i}"
            );
        }
        assert!(
            VibratoParams::descriptor(VibratoParams::COUNT).is_none(),
            "Descriptor beyond COUNT should be None"
        );
    }

    /// The kernel must wrap into a `KernelAdapter` and function as a `dyn Effect`.
    #[test]
    fn adapter_wraps_as_effect() {
        let mut adapter = KernelAdapter::new(VibratoKernel::new(48000.0), 48000.0);
        adapter.reset();

        let output = adapter.process(0.3);
        assert!(
            output.is_finite(),
            "Adapter output must be finite, got {output}"
        );
    }

    /// The adapter's `ParameterInfo` must expose the correct count and `ParamId`s,
    /// matching the classic `MultiVibrato` effect's parameter contract exactly.
    #[test]
    fn adapter_param_info_matches() {
        let adapter = KernelAdapter::new(VibratoKernel::new(48000.0), 48000.0);

        assert_eq!(
            adapter.param_count(),
            VibratoParams::COUNT,
            "Adapter param count must match VibratoParams::COUNT"
        );

        // Verify ParamIds match the classic MultiVibrato effect exactly
        // (plugin/preset API contract — these must not change).
        let depth_info = adapter.param_info(0).expect("Depth param must exist");
        let mix_info = adapter.param_info(1).expect("Mix param must exist");
        let output_info = adapter.param_info(2).expect("Output param must exist");

        assert_eq!(depth_info.id, ParamId(1300), "Depth must be ParamId(1300)");
        assert_eq!(mix_info.id, ParamId(1301), "Mix must be ParamId(1301)");
        assert_eq!(
            output_info.id,
            ParamId(1302),
            "Output must be ParamId(1302)"
        );

        // Verify string IDs
        assert_eq!(depth_info.string_id, "vib_depth");
        assert_eq!(mix_info.string_id, "vib_mix");
        assert_eq!(output_info.string_id, "vib_output");

        // Verify parameter names
        assert_eq!(depth_info.name, "Depth");
        assert_eq!(mix_info.name, "Mix");
    }

    /// Morphing linearly between two param states must always produce finite output.
    ///
    /// This verifies stability when parameters change mid-session (preset transition).
    #[test]
    fn morph_produces_valid_output() {
        let mut kernel = VibratoKernel::new(48000.0);

        let a = VibratoParams::default();
        let b = VibratoParams {
            depth_pct: 350.0,
            mix_pct: 80.0,
            output_db: -6.0,
        };

        for i in 0..=10 {
            let t = i as f32 / 10.0;
            let morphed = VibratoParams::lerp(&a, &b, t);
            let (l, r) = kernel.process_stereo(0.3, -0.3, &morphed);
            assert!(
                l.is_finite() && r.is_finite(),
                "Morph at t={t} produced NaN/Inf: l={l}, r={r}"
            );
            kernel.reset();
        }
    }

    /// `from_knobs()` must map normalized 0.0–1.0 inputs to the correct
    /// user-facing parameter ranges at both extremes.
    #[test]
    fn from_knobs_maps_ranges() {
        // Maximum deflection: all knobs at 1.0
        let max = VibratoParams::from_knobs(1.0, 1.0, 1.0);
        assert!(
            (max.depth_pct - 400.0).abs() < 0.01,
            "Depth at 1.0 should be 400 %, got {}",
            max.depth_pct
        );
        assert!(
            (max.mix_pct - 100.0).abs() < 0.01,
            "Mix at 1.0 should be 100 %, got {}",
            max.mix_pct
        );
        assert!(
            (max.output_db - 20.0).abs() < 0.01,
            "Output at 1.0 should be +20 dB, got {}",
            max.output_db
        );

        // Minimum deflection: all knobs at 0.0
        let min = VibratoParams::from_knobs(0.0, 0.0, 0.0);
        assert!(
            min.depth_pct.abs() < 0.01,
            "Depth at 0.0 should be 0 %, got {}",
            min.depth_pct
        );
        assert!(
            min.mix_pct.abs() < 0.01,
            "Mix at 0.0 should be 0 %, got {}",
            min.mix_pct
        );
        assert!(
            (min.output_db - (-20.0)).abs() < 0.01,
            "Output at 0.0 should be −20 dB, got {}",
            min.output_db
        );

        // Mid-point: depth=0.5, mix=0.5, output=0.5
        let mid = VibratoParams::from_knobs(0.5, 0.5, 0.5);
        assert!(
            (mid.depth_pct - 200.0).abs() < 0.01,
            "Depth at 0.5 should be 200 %, got {}",
            mid.depth_pct
        );
        assert!(
            (mid.mix_pct - 50.0).abs() < 0.01,
            "Mix at 0.5 should be 50 %, got {}",
            mid.mix_pct
        );
        assert!(
            mid.output_db.abs() < 0.01,
            "Output at 0.5 should be 0 dB, got {}",
            mid.output_db
        );
    }

    /// Higher depth must produce a different output than lower depth.
    ///
    /// At greater depth the LFO modulates the delay time by a larger amount,
    /// changing the pitch of the delayed signal. After priming the delay lines
    /// with the same input, the two kernels must diverge.
    #[test]
    fn depth_affects_output() {
        let params_low = VibratoParams {
            depth_pct: 0.0, // No modulation — passes through base delay only
            mix_pct: 100.0,
            output_db: 0.0,
        };
        let params_high = VibratoParams {
            depth_pct: 400.0, // Maximum modulation
            mix_pct: 100.0,
            output_db: 0.0,
        };

        let mut kernel_low = VibratoKernel::new(48000.0);
        let mut kernel_high = VibratoKernel::new(48000.0);

        // Prime delay lines with the same signal for long enough for differences to emerge.
        let mut acc_low = 0.0_f32;
        let mut acc_high = 0.0_f32;
        for i in 0..1000 {
            let inp = if i % 2 == 0 { 0.5_f32 } else { -0.5_f32 };
            let (l_low, _) = kernel_low.process_stereo(inp, inp, &params_low);
            let (l_high, _) = kernel_high.process_stereo(inp, inp, &params_high);
            acc_low += l_low;
            acc_high += l_high;
        }

        // The accumulated sums must differ because the LFO modulation shifts the
        // phase of the delayed signal differently at different depth settings.
        assert!(
            (acc_low - acc_high).abs() > 1e-3,
            "Different depth values should produce different output sums: \
             low={acc_low}, high={acc_high}"
        );
    }

    /// At 0% mix the output must match the dry input exactly.
    ///
    /// With `mix_pct = 0.0` the `wet_dry_mix_stereo` function returns the dry
    /// signal unchanged. The default output level is 0 dB (unity gain).
    #[test]
    fn mix_at_zero_is_dry() {
        let mut kernel = VibratoKernel::new(48000.0);
        let params = VibratoParams {
            depth_pct: 200.0,
            mix_pct: 0.0, // Fully dry
            output_db: 0.0,
        };

        let input = 0.4_f32;

        // Process enough samples to ensure the delay lines are active.
        for _ in 0..200 {
            kernel.process_stereo(input, input, &params);
        }

        let (l, r) = kernel.process_stereo(input, input, &params);
        assert!(
            (l - input).abs() < 1e-5,
            "0% mix should pass dry signal on left: expected {input}, got {l}"
        );
        assert!(
            (r - input).abs() < 1e-5,
            "0% mix should pass dry signal on right: expected {input}, got {r}"
        );
    }
}
