//! FM (Frequency Modulation) synthesis — 2-operator and 4-operator engines.
//!
//! Implements the classic Chowning/Yamaha FM model where one sine oscillator
//! (modulator) drives the phase of another (carrier). The modulation index `I`
//! controls the depth of sidebands produced:
//!
//! ```text
//! carrier_out(t) = sin(2π·fc·t + I·sin(2π·fm·t))
//! ```
//!
//! ## 2-Operator Engine ([`Fm2Op`])
//!
//! A single carrier + modulator pair. Covers simple FM bell, piano, and bass sounds.
//!
//! ## 4-Operator Engine ([`Fm4Op`])
//!
//! Four operators (A, B, C, D) with DX7-inspired algorithm selection:
//!
//! | Algorithm | Routing | Description |
//! |-----------|---------|-------------|
//! | 0 | D→C→B→A | Serial chain, maximum harmonic complexity |
//! | 1 | (C+D)→B→A | Two modulators to one, then serial |
//! | 2 | (B+C+D)→A | Three modulators drive one carrier |
//! | 3 | D→C→A, D→B→A | Stacked from shared modulator |
//! | 4 | D→C, D→B, then (B+C)→A | Two parallel stacks into carrier |
//! | 5 | All parallel (A+B+C+D out) | Four independent carriers, additive |
//!
//! ## Reference
//!
//! Chowning, J. (1973). "The Synthesis of Complex Audio Spectra by Means of
//! Frequency Modulation." *Journal of the Audio Engineering Society*, 21(7).
//! Yamaha DX7 Technical Manual (1983).

use core::f32::consts::PI;

/// A single FM operator: a sine oscillator with a frequency ratio and
/// modulation index.
///
/// When used as a **carrier**, the operator converts a phase modulation
/// signal (from a modulator) into audio output.
/// When used as a **modulator**, its output is the phase modulation signal
/// fed to the carrier.
///
/// ## Parameters
///
/// - `ratio`: Frequency multiplier relative to the base frequency.
///   Range: 0.25 to 16.0. Default: 1.0 (unison).
/// - `mod_index`: Modulation depth. Range: 0.0 to 20.0.
///   Controls sideband amplitude; 0 = pure sine, higher = richer timbre.
/// - `output_level`: Output amplitude. Range: 0.0 to 1.0. Default: 1.0.
#[derive(Debug, Clone)]
pub struct FmOperator {
    /// Normalized phase in [0.0, 1.0).
    phase: f32,
    /// Phase increment per sample (ratio * base_freq / sample_rate).
    phase_inc: f32,
    /// Frequency ratio relative to base pitch.
    ratio: f32,
    /// Modulation index — controls sideband depth.
    mod_index: f32,
    /// Output amplitude (0.0 to 1.0).
    output_level: f32,
    /// Cached sample rate.
    sample_rate: f32,
    /// Cached base frequency.
    base_freq_hz: f32,
}

impl Default for FmOperator {
    fn default() -> Self {
        Self::new(48000.0)
    }
}

impl FmOperator {
    /// Create a new FM operator at the given sample rate.
    pub fn new(sample_rate: f32) -> Self {
        let base = 440.0_f32;
        Self {
            phase: 0.0,
            phase_inc: base / sample_rate,
            ratio: 1.0,
            mod_index: 1.0,
            output_level: 1.0,
            sample_rate,
            base_freq_hz: base,
        }
    }

    /// Set the base frequency (from the playing note).
    ///
    /// Range: 0.0 to Nyquist Hz.
    pub fn set_base_frequency(&mut self, freq_hz: f32) {
        self.base_freq_hz = freq_hz.max(0.0);
        self.recalc_phase_inc();
    }

    /// Set frequency ratio (harmonic multiplier of the base frequency).
    ///
    /// Range: 0.25 to 16.0. Values outside this range are clamped.
    pub fn set_ratio(&mut self, ratio: f32) {
        self.ratio = ratio.clamp(0.25, 16.0);
        self.recalc_phase_inc();
    }

    /// Get frequency ratio.
    pub fn ratio(&self) -> f32 {
        self.ratio
    }

    /// Set modulation index.
    ///
    /// Range: 0.0 to 20.0. 0 = no modulation (pure sine).
    pub fn set_mod_index(&mut self, index: f32) {
        self.mod_index = index.clamp(0.0, 20.0);
    }

    /// Get modulation index.
    pub fn mod_index(&self) -> f32 {
        self.mod_index
    }

    /// Set output level.
    ///
    /// Range: 0.0 to 1.0.
    pub fn set_output_level(&mut self, level: f32) {
        self.output_level = level.clamp(0.0, 1.0);
    }

    /// Get output level.
    pub fn output_level(&self) -> f32 {
        self.output_level
    }

    /// Set sample rate.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.recalc_phase_inc();
    }

    /// Reset phase to 0.
    pub fn reset(&mut self) {
        self.phase = 0.0;
    }

    /// Advance and return the operator's sine output scaled by output_level,
    /// with optional incoming phase modulation (in radians).
    ///
    /// When this operator is a **modulator**, the returned value should be
    /// multiplied by `mod_index` of the downstream carrier before being passed
    /// as `phase_mod` to the carrier's `advance_pm` call.
    #[inline]
    pub fn advance(&mut self, phase_mod: f32) -> f32 {
        let modulated = self.phase * 2.0 * PI + phase_mod;
        let out = libm::sinf(modulated) * self.output_level;
        self.phase += self.phase_inc;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        out
    }

    fn recalc_phase_inc(&mut self) {
        self.phase_inc = self.base_freq_hz * self.ratio / self.sample_rate;
    }
}

/// 2-operator FM synthesizer: one carrier driven by one modulator.
///
/// ## Signal Flow
///
/// ```text
/// modulator ──(×mod_index)──▶ carrier phase_mod ──▶ audio out
/// ```
///
/// # Example
///
/// ```rust
/// use sonido_synth::fm::Fm2Op;
///
/// let mut fm = Fm2Op::new(48000.0);
/// fm.set_base_frequency(440.0);
/// fm.carrier.set_ratio(1.0);
/// fm.modulator.set_ratio(2.0);
/// fm.modulator.set_mod_index(3.0);
///
/// let sample = fm.advance();
/// assert!(sample.is_finite());
/// ```
#[derive(Debug, Clone)]
pub struct Fm2Op {
    /// The carrier operator — produces the audible output.
    pub carrier: FmOperator,
    /// The modulator operator — drives the carrier's phase.
    pub modulator: FmOperator,
}

impl Fm2Op {
    /// Create a 2-operator FM engine.
    pub fn new(sample_rate: f32) -> Self {
        Self {
            carrier: FmOperator::new(sample_rate),
            modulator: FmOperator::new(sample_rate),
        }
    }

    /// Set the base note frequency for both operators.
    ///
    /// Range: 0.0 Hz to Nyquist.
    pub fn set_base_frequency(&mut self, freq_hz: f32) {
        self.carrier.set_base_frequency(freq_hz);
        self.modulator.set_base_frequency(freq_hz);
    }

    /// Set sample rate for both operators.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.carrier.set_sample_rate(sample_rate);
        self.modulator.set_sample_rate(sample_rate);
    }

    /// Reset both operators.
    pub fn reset(&mut self) {
        self.carrier.reset();
        self.modulator.reset();
    }

    /// Generate one output sample.
    ///
    /// The modulator's output is scaled by its own `mod_index` to produce
    /// the phase deviation for the carrier.
    #[inline]
    pub fn advance(&mut self) -> f32 {
        // Modulator runs with no incoming modulation
        let mod_signal = self.modulator.advance(0.0);
        // Scale modulator output by mod_index to get phase deviation (radians)
        let phase_dev = mod_signal * self.modulator.mod_index;
        // Carrier's phase is deviated by the modulator
        self.carrier.advance(phase_dev)
    }
}

/// 4-operator FM algorithm type.
///
/// Each variant defines a specific routing topology for operators A (output),
/// B, C, and D (deepest modulator). Inspired by Yamaha DX7 algorithms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fm4Algorithm {
    /// D→C→B→A — serial chain, densest sidebands.
    #[default]
    Serial,
    /// (C+D)→B→A — two modulators into B, then serial.
    Parallel2,
    /// (B+C+D)→A — three modulators drive carrier A.
    Parallel3,
    /// D→(C→A + B→A) — D drives both C and B which each modulate A.
    DoubleStack,
    /// (D→C)+(D→B) → A — D feeds two serial pairs that combine into A.
    DualSerial,
    /// A+B+C+D all as carriers (additive). No modulation, pure sine sum.
    Additive,
}

/// 4-operator FM synthesizer.
///
/// Operators are named A (carrier output), B, C, D (deepest modulator).
/// The `algorithm` field selects the routing topology.
///
/// ## Modulation Index Conventions
///
/// The `mod_index` stored on each operator scales the output before it is
/// applied as the phase deviation of the downstream carrier. For algorithms
/// where an operator modulates multiple targets the index is shared.
///
/// # Example
///
/// ```rust
/// use sonido_synth::fm::{Fm4Op, Fm4Algorithm};
///
/// let mut fm = Fm4Op::new(48000.0);
/// fm.set_base_frequency(261.63); // C4
/// fm.set_algorithm(Fm4Algorithm::Serial);
/// fm.ops[3].set_mod_index(5.0); // deep modulation from D
///
/// let sample = fm.advance();
/// assert!(sample.is_finite());
/// ```
#[derive(Debug, Clone)]
pub struct Fm4Op {
    /// Four operators: [A (carrier out), B, C, D (deepest modulator)].
    pub ops: [FmOperator; 4],
    /// Routing topology.
    algorithm: Fm4Algorithm,
}

impl Fm4Op {
    /// Create a 4-operator FM engine with the Serial algorithm.
    pub fn new(sample_rate: f32) -> Self {
        let mut engine = Self {
            ops: core::array::from_fn(|_| FmOperator::new(sample_rate)),
            algorithm: Fm4Algorithm::default(),
        };
        // Sensible default mod indices
        engine.ops[3].set_mod_index(4.0); // D
        engine.ops[2].set_mod_index(3.0); // C
        engine.ops[1].set_mod_index(2.0); // B
        engine.ops[0].set_mod_index(1.0); // A (carrier level)
        engine
    }

    /// Set algorithm.
    pub fn set_algorithm(&mut self, algo: Fm4Algorithm) {
        self.algorithm = algo;
    }

    /// Get current algorithm.
    pub fn algorithm(&self) -> Fm4Algorithm {
        self.algorithm
    }

    /// Set base note frequency for all operators.
    pub fn set_base_frequency(&mut self, freq_hz: f32) {
        for op in &mut self.ops {
            op.set_base_frequency(freq_hz);
        }
    }

    /// Set sample rate for all operators.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        for op in &mut self.ops {
            op.set_sample_rate(sample_rate);
        }
    }

    /// Reset all operators.
    pub fn reset(&mut self) {
        for op in &mut self.ops {
            op.reset();
        }
    }

    /// Generate one output sample using the selected algorithm.
    #[inline]
    pub fn advance(&mut self) -> f32 {
        let [a, b, c, d] = &mut self.ops;
        match self.algorithm {
            Fm4Algorithm::Serial => {
                // D→C→B→A
                let d_out = d.advance(0.0) * d.mod_index;
                let c_out = c.advance(d_out) * c.mod_index;
                let b_out = b.advance(c_out) * b.mod_index;
                a.advance(b_out)
            }
            Fm4Algorithm::Parallel2 => {
                // (C+D)→B→A
                let d_out = d.advance(0.0) * d.mod_index;
                let c_out = c.advance(0.0) * c.mod_index;
                let b_out = b.advance(d_out + c_out) * b.mod_index;
                a.advance(b_out)
            }
            Fm4Algorithm::Parallel3 => {
                // (B+C+D)→A
                let d_out = d.advance(0.0) * d.mod_index;
                let c_out = c.advance(0.0) * c.mod_index;
                let b_out = b.advance(0.0) * b.mod_index;
                a.advance(d_out + c_out + b_out)
            }
            Fm4Algorithm::DoubleStack => {
                // D→C→A and D→B→A (D is shared modulator)
                let d_raw = d.advance(0.0);
                let d_mod = d_raw * d.mod_index;
                let c_out = c.advance(d_mod) * c.mod_index;
                let b_out = b.advance(d_mod) * b.mod_index;
                a.advance(c_out + b_out)
            }
            Fm4Algorithm::DualSerial => {
                // D→C, D→B independently, then (C+B)→A
                let d_raw = d.advance(0.0);
                let d_mod = d_raw * d.mod_index;
                let c_out = c.advance(d_mod) * c.mod_index;
                let b_out = b.advance(d_mod) * b.mod_index;
                a.advance((c_out + b_out) * 0.5) // average to keep level stable
            }
            Fm4Algorithm::Additive => {
                // All carriers, no modulation — just sum
                let a_out = a.advance(0.0);
                let b_out = b.advance(0.0);
                let c_out = c.advance(0.0);
                let d_out = d.advance(0.0);
                (a_out + b_out + c_out + d_out) * 0.25
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fm_operator_basic() {
        let mut op = FmOperator::new(48000.0);
        op.set_base_frequency(440.0);
        for _ in 0..4800 {
            let s = op.advance(0.0);
            assert!(s.is_finite());
            assert!((-1.1..=1.1).contains(&s));
        }
    }

    #[test]
    fn test_fm_operator_mod_index_zero_is_sine() {
        let mut op = FmOperator::new(48000.0);
        op.set_base_frequency(440.0);
        op.set_mod_index(0.0);
        op.set_ratio(1.0);
        // With no modulation input and output_level=1, should behave as sine
        let s = op.advance(0.0);
        // At phase 0 sin(0) = 0
        assert!(
            s.abs() < 0.01,
            "At phase=0 with no mod, output ≈ 0, got {s}"
        );
    }

    #[test]
    fn test_fm2op_produces_output() {
        let mut fm = Fm2Op::new(48000.0);
        fm.set_base_frequency(440.0);
        fm.modulator.set_mod_index(3.0);
        let mut sum = 0.0f32;
        for _ in 0..4800 {
            sum += fm.advance().abs();
        }
        assert!(sum > 10.0, "2-op FM should produce significant output");
    }

    #[test]
    fn test_fm2op_zero_modulation_is_sine_like() {
        let mut fm = Fm2Op::new(48000.0);
        fm.set_base_frequency(440.0);
        fm.modulator.set_mod_index(0.0);
        fm.modulator.set_output_level(0.0);
        let s = fm.advance();
        // Carrier at phase 0 with no phase mod should be ~0
        assert!(
            s.abs() < 0.01,
            "Unmodulated carrier at phase 0 should be ~0, got {s}"
        );
    }

    #[test]
    fn test_fm4op_all_algorithms_finite() {
        use super::Fm4Algorithm::*;
        for algo in [
            Serial,
            Parallel2,
            Parallel3,
            DoubleStack,
            DualSerial,
            Additive,
        ] {
            let mut fm = Fm4Op::new(48000.0);
            fm.set_algorithm(algo);
            fm.set_base_frequency(440.0);
            for _ in 0..4800 {
                let s = fm.advance();
                assert!(
                    s.is_finite(),
                    "Algorithm {algo:?} produced non-finite sample"
                );
            }
        }
    }

    #[test]
    fn test_fm4op_additive_four_equal_levels() {
        let mut fm = Fm4Op::new(48000.0);
        fm.set_algorithm(Fm4Algorithm::Additive);
        fm.set_base_frequency(440.0);
        // At phase 0 all sines = 0, so first sample should be near 0
        let s = fm.advance();
        assert!(
            s.abs() < 0.01,
            "All phases start at 0, sum should be ~0, got {s}"
        );
    }

    #[test]
    fn test_fm_operator_ratio_affects_frequency() {
        // Two operators: one at ratio 1, one at ratio 2
        let mut op1 = FmOperator::new(48000.0);
        let mut op2 = FmOperator::new(48000.0);
        op1.set_base_frequency(100.0);
        op2.set_base_frequency(100.0);
        op1.set_ratio(1.0);
        op2.set_ratio(2.0);

        // After N samples, op2 should have advanced twice the phase of op1
        let n = 480; // ~100ms
        let mut phase_inc_1 = 0.0f32;
        let mut phase_inc_2 = 0.0f32;
        // Just verify phase increments differ by ratio
        for _ in 0..n {
            phase_inc_1 += op1.phase_inc;
            phase_inc_2 += op2.phase_inc;
        }
        assert!(
            (phase_inc_2 / phase_inc_1 - 2.0).abs() < 0.01,
            "Ratio 2 should double phase speed: {:.3} vs {:.3}",
            phase_inc_2,
            phase_inc_1
        );
    }
}
