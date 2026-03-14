//! Ring modulator kernel — carrier oscillator multiplication with mix control.
//!
//! `RingModKernel` owns DSP state (carrier oscillator phase, sample rate).
//! Parameters are received via `&RingModParams` each sample. Deployed via
//! [`KernelAdapter`](sonido_core::KernelAdapter) for desktop/plugin, or called
//! directly on embedded targets.
//!
//! # Signal Flow
//!
//! ```text
//! Input × (1 - depth + depth × carrier) → Wet/Dry Mix → Output Level
//! ```
//!
//! At `depth = 1.0`: full ring mod — `output = input × carrier`
//! At `depth = 0.0`: bypass — `output = input`
//!
//! # Ring Modulation Theory
//!
//! Ring modulation is amplitude modulation with a bipolar carrier oscillator.
//! For a sinusoidal input `A·sin(2π·f_in·t)` and carrier `sin(2π·f_c·t)`:
//!
//! ```text
//! output = A/2 · [cos(2π(f_in - f_c)t) - cos(2π(f_in + f_c)t)]
//! ```
//!
//! The result is two sidebands with no original frequency components, producing
//! the classic "robot voice" or metallic timbre.
//!
//! Reference: Zölzer, "DAFX: Digital Audio Effects" (2011), Ch. 2
//! (Amplitude Modulation and Ring Modulation).
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! let adapter = KernelAdapter::new(RingModKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing, ADCs are hardware-filtered)
//! let mut kernel = RingModKernel::new(48000.0);
//! let params = RingModParams::from_knobs(adc_freq, adc_depth, adc_wave, adc_mix, adc_out);
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

use libm::fabsf;
use sonido_core::fast_math::fast_sin_turns;
use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::{
    ParamDescriptor, ParamFlags, ParamId, ParamScale, ParamUnit, fast_db_to_linear,
    wet_dry_mix_stereo,
};

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`RingModKernel`].
///
/// All values in **user-facing units** — the same units shown in GUIs and
/// stored in presets. The kernel converts internally as needed.
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `freq_hz` | Hz | 20–2000 | 220.0 |
/// | 1 | `depth_pct` | % | 0–100 | 100.0 |
/// | 2 | `waveform` | index | 0–2 (Sine/Triangle/Square) | 0.0 |
/// | 3 | `mix_pct` | % | 0–100 | 50.0 |
/// | 4 | `output_db` | dB | −20–20 | 0.0 |
#[derive(Debug, Clone, Copy)]
pub struct RingModParams {
    /// Carrier oscillator frequency in Hz.
    pub freq_hz: f32,
    /// Modulation depth in percent: 0 = bypass (dry), 100 = full ring mod.
    pub depth_pct: f32,
    /// Carrier waveform index: 0.0 = Sine, 1.0 = Triangle, 2.0 = Square.
    pub waveform: f32,
    /// Wet/dry mix in percent: 0 = fully dry, 100 = fully wet.
    pub mix_pct: f32,
    /// Output level in decibels.
    pub output_db: f32,
}

impl Default for RingModParams {
    fn default() -> Self {
        Self {
            freq_hz: 220.0,
            depth_pct: 100.0,
            waveform: 0.0,
            mix_pct: 50.0,
            output_db: 0.0,
        }
    }
}

impl RingModParams {
    /// Build params directly from hardware knob readings (0.0–1.0 normalized).
    ///
    /// Convenience constructor for embedded targets where ADC values map
    /// linearly across each parameter's range.
    ///
    /// # Parameters
    ///
    /// - `freq`: ADC reading → 20–2000 Hz (logarithmic mapping via linear input)
    /// - `depth`: ADC reading → 0–100 %
    /// - `waveform`: ADC reading → 0, 1, or 2 (Sine / Triangle / Square)
    /// - `mix`: ADC reading → 0–100 %
    /// - `output`: ADC reading → −20–20 dB
    ///
    /// All inputs are expected in [0.0, 1.0].
    /// Creates parameters from normalized 0–1 knob readings.
    ///
    /// Curves (logarithmic for frequency/time, linear for percentage) are
    /// derived from [`ParamDescriptor`] — same mapping as GUI and plugin hosts.
    ///
    /// # Parameters
    ///
    /// - `freq`: ADC reading → 20–2000 Hz (logarithmic)
    /// - `depth`: ADC reading → 0–100 %
    /// - `waveform`: ADC reading → 0, 1, or 2 (Sine / Triangle / Square, stepped)
    /// - `mix`: ADC reading → 0–100 %
    /// - `output`: ADC reading → −20–+6 dB
    ///
    /// All inputs are expected in [0.0, 1.0].
    pub fn from_knobs(freq: f32, depth: f32, waveform: f32, mix: f32, output: f32) -> Self {
        Self::from_normalized(&[freq, depth, waveform, mix, output])
    }
}

impl KernelParams for RingModParams {
    const COUNT: usize = 5;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(
                ParamDescriptor::custom("Frequency", "Freq", 20.0, 2000.0, 220.0)
                    .with_unit(ParamUnit::Hertz)
                    .with_scale(ParamScale::Logarithmic)
                    .with_id(ParamId(1800), "ring_freq"),
            ),
            1 => Some(
                // Matches classic: ParamDescriptor::depth() with id 1801.
                // depth() factory: custom "Depth"/"Depth", 0–100 %, default 100.0
                ParamDescriptor::depth().with_id(ParamId(1801), "ring_depth"),
            ),
            2 => Some(
                ParamDescriptor::custom("Waveform", "Wave", 0.0, 2.0, 0.0)
                    .with_step(1.0)
                    .with_id(ParamId(1802), "ring_wave")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(&["Sine", "Triangle", "Square"]),
            ),
            3 => Some(ParamDescriptor::mix().with_id(ParamId(1803), "ring_mix")),
            4 => Some(
                sonido_core::gain::output_param_descriptor().with_id(ParamId(1804), "ring_output"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Fast,     // frequency — fast for carrier pitch feel
            1 => SmoothingStyle::Standard, // depth — standard AM depth transitions
            2 => SmoothingStyle::None,     // waveform — stepped/discrete, snap immediately
            3 => SmoothingStyle::Standard, // mix
            4 => SmoothingStyle::Standard, // output level
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.freq_hz,
            1 => self.depth_pct,
            2 => self.waveform,
            3 => self.mix_pct,
            4 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.freq_hz = value,
            1 => self.depth_pct = value,
            2 => self.waveform = value,
            3 => self.mix_pct = value,
            4 => self.output_db = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP ring modulator kernel.
///
/// Contains ONLY the mutable state required for audio processing:
/// - Carrier oscillator phase accumulator
/// - Sample rate (for phase increment calculation)
///
/// No `SmoothedParam`, no `AtomicU32`, no platform awareness.
///
/// ## DSP State
///
/// The ring modulator has minimal internal state: a single phase accumulator
/// in `[0.0, 1.0)` that advances by `freq_hz / sample_rate` each sample.
/// The carrier waveform is synthesized from this phase using `fast_sin_turns`.
pub struct RingModKernel {
    /// Sample rate in Hz. Needed to compute the phase increment from `freq_hz`.
    sample_rate: f32,
    /// Carrier phase accumulator in [0.0, 1.0).
    ///
    /// Shared across L/R channels (dual-mono: same modulation applied to both).
    phase: f32,
}

impl RingModKernel {
    /// Create a new ring modulator kernel at the given sample rate.
    ///
    /// Phase is initialized to 0.0.
    ///
    /// # Parameters
    ///
    /// - `sample_rate`: Audio sample rate in Hz (e.g. 44100.0, 48000.0, 96000.0).
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            phase: 0.0,
        }
    }

    /// Compute the carrier oscillator value at the current phase.
    ///
    /// Returns a bipolar value in [-1.0, 1.0] according to the waveform index.
    ///
    /// Waveform index mapping:
    /// - `0`: Sine — `fast_sin_turns(phase)` (phase already in turns [0, 1))
    /// - `1`: Triangle — bipolar triangle from phase: `4·|phase - 0.5| - 1`
    /// - `2` (or higher): Square — `+1` for phase < 0.5, `-1` otherwise
    ///
    /// Reference: Zölzer, "DAFX" (2011), Ch. 2 — AM/Ring Modulation.
    #[inline]
    fn carrier_value(&self, waveform: u8) -> f32 {
        match waveform {
            0 => fast_sin_turns(self.phase),
            // Bipolar triangle wave: amplitude 1 at phase=0.25, -1 at phase=0.75, 0 at 0 and 0.5.
            // Formula: 4·|phase - 0.5| - 1  maps [0,1) phase uniformly to [-1,1] bipolar triangle.
            1 => 4.0 * fabsf(self.phase - 0.5) - 1.0,
            // Square wave: +1 for first half-cycle, -1 for second
            _ => {
                if self.phase < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
        }
    }

    /// Advance the phase accumulator by one sample at `freq_hz`.
    ///
    /// Phase wraps at 1.0 to keep the accumulator in [0.0, 1.0).
    #[inline]
    fn advance_phase(&mut self, freq_hz: f32) {
        self.phase += freq_hz / self.sample_rate;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
    }
}

impl DspKernel for RingModKernel {
    type Params = RingModParams;

    fn process_stereo(&mut self, left: f32, right: f32, params: &RingModParams) -> (f32, f32) {
        // ── Unit conversion (user-facing → internal) ──
        let depth = params.depth_pct / 100.0;
        let mix = params.mix_pct / 100.0;
        let output = fast_db_to_linear(params.output_db);
        let waveform = params.waveform as u8;

        // ── Carrier: compute once, shared for both channels (dual-mono) ──
        // Same carrier is applied to both channels — no stereo decorrelation.
        let carrier = self.carrier_value(waveform);
        self.advance_phase(params.freq_hz);

        // ── Ring modulation: input × (1 - depth + depth × carrier) ──
        // At depth=0: coefficient = 1.0 → passthrough
        // At depth=1: coefficient = carrier → full ring mod
        let coeff = 1.0 - depth + depth * carrier;
        let mod_l = left * coeff;
        let mod_r = right * coeff;

        // ── Wet/dry mix → output level ──
        let (mixed_l, mixed_r) = wet_dry_mix_stereo(left, right, mod_l, mod_r, mix);
        (mixed_l * output, mixed_r * output)
    }

    fn reset(&mut self) {
        self.phase = 0.0;
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
    }

    /// Ring modulator has zero latency.
    fn latency_samples(&self) -> usize {
        0
    }

    /// Dual-mono: L and R receive identical carrier modulation (shared phase).
    fn is_true_stereo(&self) -> bool {
        false
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
    /// Any finite input × any carrier = 0 when input is 0.
    #[test]
    fn silence_in_silence_out() {
        let mut kernel = RingModKernel::new(48000.0);
        let params = RingModParams::default();

        for _ in 0..10 {
            let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
            assert!(l.abs() < 1e-10, "Expected silence L, got {l}");
            assert!(r.abs() < 1e-10, "Expected silence R, got {r}");
        }
    }

    /// No sample should ever produce NaN or infinity.
    #[test]
    fn no_nan_or_inf() {
        let mut kernel = RingModKernel::new(48000.0);
        let params = RingModParams::default();

        let test_inputs = [-1.0f32, -0.5, 0.0, 0.5, 1.0];
        for &input in &test_inputs {
            for _ in 0..100 {
                let (l, r) = kernel.process_stereo(input, -input, &params);
                assert!(!l.is_nan(), "NaN in left output for input {input}");
                assert!(!r.is_nan(), "NaN in right output for input {input}");
                assert!(l.is_finite(), "Inf in left output for input {input}");
                assert!(r.is_finite(), "Inf in right output for input {input}");
            }
        }
    }

    /// Descriptor count and param COUNT must be consistent.
    #[test]
    fn params_descriptor_count() {
        assert_eq!(RingModParams::COUNT, 5);

        for i in 0..5 {
            let desc = RingModParams::descriptor(i);
            assert!(
                desc.is_some(),
                "descriptor({i}) should return Some, got None"
            );
        }

        // Index beyond COUNT must return None
        assert!(
            RingModParams::descriptor(5).is_none(),
            "descriptor(5) should be None"
        );
    }

    /// With full depth and wet mix, output should differ from a dry passthrough.
    ///
    /// Verifies that the ring modulator is actually doing something to the signal —
    /// not just passing it through unchanged.
    #[test]
    fn ring_mod_modulates_signal() {
        let mut kernel = RingModKernel::new(48000.0);
        let params = RingModParams {
            freq_hz: 220.0,
            depth_pct: 100.0,
            waveform: 0.0,
            mix_pct: 100.0,
            output_db: 0.0,
        };

        let input = 1.0f32;
        let mut outputs = [0.0f32; 1000];
        for out in &mut outputs {
            let (l, _) = kernel.process_stereo(input, input, &params);
            *out = l;
        }

        // With full ring mod, DC input through a sine carrier should oscillate.
        // Output should not be constant (the carrier is cycling).
        let mean = outputs.iter().copied().sum::<f32>() / outputs.len() as f32;
        let variance =
            outputs.iter().map(|&v| (v - mean).powi(2)).sum::<f32>() / outputs.len() as f32;

        assert!(
            variance > 0.001,
            "Ring mod output should vary over time (carrier is cycling), variance={variance}"
        );
    }

    /// Different waveforms must produce different output for the same input.
    #[test]
    fn different_waveforms_produce_different_output() {
        let sr = 48000.0;
        let input = 0.7f32;

        // Collect 2000 samples from each waveform and compare means/variance
        let collect_rms = |wave: f32| -> f32 {
            let mut kernel = RingModKernel::new(sr);
            let params = RingModParams {
                freq_hz: 220.0,
                depth_pct: 100.0,
                waveform: wave,
                mix_pct: 100.0,
                output_db: 0.0,
            };
            // Settle a few cycles first (220 Hz → ~218 samples/cycle)
            for _ in 0..500 {
                kernel.process_stereo(input, input, &params);
            }
            // Collect RMS over one second of audio
            let sum_sq: f32 = (0..sr as usize)
                .map(|_| {
                    let (l, _) = kernel.process_stereo(input, input, &params);
                    l * l
                })
                .sum();
            (sum_sq / sr).sqrt()
        };

        let sine_rms = collect_rms(0.0);
        let tri_rms = collect_rms(1.0);
        let sq_rms = collect_rms(2.0);

        // Triangle wave has lower RMS than square (1/√3 ≈ 0.577 vs 1.0)
        // Sine also has lower RMS than square (1/√2 ≈ 0.707 vs 1.0)
        // Waveforms should produce measurably different RMS levels at full depth + wet
        assert!(
            (sine_rms - tri_rms).abs() > 0.01,
            "Sine and Triangle RMS should differ: sine={sine_rms}, tri={tri_rms}"
        );
        assert!(
            (sine_rms - sq_rms).abs() > 0.01,
            "Sine and Square RMS should differ: sine={sine_rms}, sq={sq_rms}"
        );
    }

    /// At 0% mix, output should equal the dry input (scaled only by output level).
    ///
    /// When mix = 0, wet_dry_mix returns only the dry signal unchanged.
    #[test]
    fn dry_mix_passes_input() {
        let mut kernel = RingModKernel::new(48000.0);
        let params = RingModParams {
            freq_hz: 440.0,
            depth_pct: 100.0, // full modulation depth, but…
            waveform: 0.0,
            mix_pct: 0.0, // …mix = 0% → dry only
            output_db: 0.0,
        };

        let input = 0.6f32;
        // Run several samples; since mix=0, output should always equal input.
        for _ in 0..1000 {
            let (l, r) = kernel.process_stereo(input, input, &params);
            assert!(
                (l - input).abs() < 1e-5,
                "Dry mix: expected {input}, got L={l}"
            );
            assert!(
                (r - input).abs() < 1e-5,
                "Dry mix: expected {input}, got R={r}"
            );
        }
    }

    // ── Adapter integration tests ─────────────────────────────────────────

    /// Kernel wrapped in KernelAdapter should function as a standard Effect.
    #[test]
    fn adapter_wraps_as_effect() {
        let kernel = RingModKernel::new(48000.0);
        let mut adapter = KernelAdapter::new(kernel, 48000.0);

        adapter.reset();
        let output = adapter.process(0.5);
        assert!(!output.is_nan(), "Adapter output is NaN");
        assert!(output.is_finite(), "Adapter output is infinite");
    }

    /// KernelAdapter ParameterInfo must expose the same 5 params with matching IDs.
    #[test]
    fn adapter_param_info_matches() {
        let kernel = RingModKernel::new(48000.0);
        let adapter = KernelAdapter::new(kernel, 48000.0);

        assert_eq!(adapter.param_count(), 5, "Should expose exactly 5 params");

        // Frequency (index 0)
        let freq_desc = adapter.param_info(0).unwrap();
        assert_eq!(freq_desc.name, "Frequency");
        assert_eq!(freq_desc.min, 20.0);
        assert_eq!(freq_desc.max, 2000.0);
        assert_eq!(freq_desc.default, 220.0);
        assert_eq!(freq_desc.id, ParamId(1800));

        // Depth (index 1)
        let depth_desc = adapter.param_info(1).unwrap();
        assert_eq!(depth_desc.id, ParamId(1801));

        // Waveform (index 2) — must be STEPPED
        let wave_desc = adapter.param_info(2).unwrap();
        assert_eq!(wave_desc.id, ParamId(1802));
        assert!(
            wave_desc.flags.contains(ParamFlags::STEPPED),
            "Waveform must be STEPPED"
        );

        // Mix (index 3)
        let mix_desc = adapter.param_info(3).unwrap();
        assert_eq!(mix_desc.id, ParamId(1803));

        // Output (index 4)
        let out_desc = adapter.param_info(4).unwrap();
        assert_eq!(out_desc.id, ParamId(1804));

        // Out-of-range
        assert!(adapter.param_info(5).is_none());
    }

    // ── Behavioral / DSP correctness tests ───────────────────────────────

    /// `lerp()` between two param states must always produce finite output.
    ///
    /// Verifies that no parameter combination in the morph path can produce
    /// NaN or infinity.
    #[test]
    fn morph_produces_valid_output() {
        let mut kernel = RingModKernel::new(48000.0);

        let a = RingModParams {
            freq_hz: 20.0,
            depth_pct: 0.0,
            waveform: 0.0,
            mix_pct: 0.0,
            output_db: -20.0,
        };
        let b = RingModParams {
            freq_hz: 2000.0,
            depth_pct: 100.0,
            waveform: 2.0,
            mix_pct: 100.0,
            output_db: 20.0,
        };

        for i in 0..=20 {
            let t = i as f32 / 20.0;
            let morphed = RingModParams::lerp(&a, &b, t);
            for _ in 0..10 {
                let (l, r) = kernel.process_stereo(0.5, -0.5, &morphed);
                assert!(l.is_finite(), "Morph at t={t} produced non-finite L: {l}");
                assert!(r.is_finite(), "Morph at t={t} produced non-finite R: {r}");
            }
            kernel.reset();
        }
    }

    // ── from_knobs helper ─────────────────────────────────────────────────

    /// `from_knobs` should produce values within valid param ranges.
    #[test]
    fn from_knobs_produces_valid_ranges() {
        let params = RingModParams::from_knobs(0.5, 0.5, 0.5, 0.5, 0.5);

        // Frequency should be in the middle of the log-mapped range
        assert!(params.freq_hz >= 20.0 && params.freq_hz <= 2000.0);

        // Depth should be 50%
        assert!((params.depth_pct - 50.0).abs() < 0.5);

        // Waveform index should be 0, 1, or 2
        assert!(params.waveform >= 0.0 && params.waveform <= 2.0);

        // Mix should be 50%
        assert!((params.mix_pct - 50.0).abs() < 0.5);

        // Output at 0.5 should be -7 dB (-20 + 0.5 * 26 = -7)
        assert!((params.output_db - (-7.0)).abs() < 0.5);
    }

    /// Extremes of `from_knobs` (0.0 and 1.0) should stay within param bounds.
    #[test]
    fn from_knobs_extremes_within_bounds() {
        let low = RingModParams::from_knobs(0.0, 0.0, 0.0, 0.0, 0.0);
        assert!((low.freq_hz - 20.0).abs() < 0.5);
        assert!((low.depth_pct - 0.0).abs() < 0.5);
        assert_eq!(low.waveform, 0.0);
        assert!((low.mix_pct - 0.0).abs() < 0.5);
        assert!((low.output_db - (-20.0)).abs() < 0.5);

        let high = RingModParams::from_knobs(1.0, 1.0, 1.0, 1.0, 1.0);
        assert!((high.freq_hz - 2000.0).abs() < 0.5);
        assert!((high.depth_pct - 100.0).abs() < 0.5);
        assert_eq!(high.waveform, 2.0);
        assert!((high.mix_pct - 100.0).abs() < 0.5);
        assert!((high.output_db - 6.0).abs() < 0.5);
    }
}
