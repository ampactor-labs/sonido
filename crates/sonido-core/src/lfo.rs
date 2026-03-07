//! Low Frequency Oscillator for modulation effects.
//!
//! Provides smooth, periodic modulation signals used in chorus, flanger,
//! tremolo, vibrato, and other time-based effects.

use crate::fast_math::fast_sin_turns;

use crate::tempo::NoteDivision;

/// LFO waveform type
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LfoWaveform {
    /// Sine waveform — smooth, natural-sounding modulation.
    #[default]
    Sine,
    /// Triangle waveform — linear ramps with harder corners than sine.
    Triangle,
    /// Sawtooth waveform — linear ramp up with sharp reset.
    Saw,
    /// Square waveform — abrupt on/off switching.
    Square,
    /// Sample-and-hold — random stepped values at the LFO rate.
    SampleAndHold,
}

/// Low Frequency Oscillator for generating modulation signals.
///
/// Generates periodic waveforms at sub-audio frequencies (typically 0.1-20 Hz).
/// Uses phase accumulation for efficient, alias-free oscillation.
///
/// # Waveforms
///
/// - **Sine**: Smooth, natural modulation
/// - **Triangle**: Linear ramps, harder corners than sine
/// - **Saw**: Rising ramp, abrupt reset
/// - **Square**: Binary on/off modulation
/// - **SampleAndHold**: Random stepped values
///
/// # Example
///
/// ```rust
/// use sonido_core::{Lfo, LfoWaveform};
///
/// let mut lfo = Lfo::new(44100.0, 2.0); // 2 Hz
/// lfo.set_waveform(LfoWaveform::Triangle);
///
/// // Generate modulation values in [-1.0, 1.0]
/// let value = lfo.advance();
/// ```
#[derive(Debug, Clone)]
pub struct Lfo {
    /// Current phase position [0.0, 1.0)
    phase: f32,
    /// Phase increment per sample
    phase_inc: f32,
    /// Sample rate in Hz
    sample_rate: f32,
    /// Waveform type
    waveform: LfoWaveform,
    /// For Sample & Hold: current held value
    sh_value: f32,
    /// Previous phase (for detecting wrap)
    prev_phase: f32,
    /// LCG pseudo-random state for Sample & Hold waveform.
    rng_state: u32,
}

impl Default for Lfo {
    fn default() -> Self {
        Self::new(48000.0, 1.0)
    }
}

impl Lfo {
    /// Create new LFO with given sample rate and frequency
    pub fn new(sample_rate: f32, freq_hz: f32) -> Self {
        Self {
            phase: 0.0,
            phase_inc: freq_hz / sample_rate,
            sample_rate,
            waveform: LfoWaveform::Sine,
            sh_value: 0.0,
            prev_phase: 0.0,
            rng_state: 0x1234_5678,
        }
    }

    /// Set frequency in Hz
    pub fn set_frequency(&mut self, freq_hz: f32) {
        self.phase_inc = freq_hz / self.sample_rate;
    }

    /// Get current frequency in Hz
    pub fn frequency(&self) -> f32 {
        self.phase_inc * self.sample_rate
    }

    /// Set waveform
    pub fn set_waveform(&mut self, waveform: LfoWaveform) {
        self.waveform = waveform;
    }

    /// Get current waveform
    pub fn waveform(&self) -> LfoWaveform {
        self.waveform
    }

    /// Reset phase to 0
    pub fn reset(&mut self) {
        self.phase = 0.0;
        self.prev_phase = 0.0;
        self.rng_state = 0x1234_5678;
    }

    /// Sync phase to a specific value (0.0 - 1.0)
    ///
    /// Useful for phase-offset LFOs in multi-voice effects.
    /// 0.0 = 0°, 0.25 = 90°, 0.5 = 180°, 0.75 = 270°
    pub fn set_phase(&mut self, phase: f32) {
        self.phase = phase.clamp(0.0, 1.0);
        self.prev_phase = self.phase;
    }

    /// Get current phase (0.0 - 1.0)
    pub fn phase(&self) -> f32 {
        self.phase
    }

    /// Compute the waveform value at the current phase without advancing.
    ///
    /// Returns a bipolar value in \[-1.0, 1.0\] based on the current phase
    /// and waveform. For `SampleAndHold`, returns the currently held value.
    ///
    /// This is useful for reading the LFO state without side effects
    /// (e.g., for `ModulationSource::mod_value()`).
    #[inline]
    pub fn value_at_phase(&self) -> f32 {
        match self.waveform {
            LfoWaveform::Sine => fast_sin_turns(self.phase),

            LfoWaveform::Triangle => {
                if self.phase < 0.5 {
                    4.0 * self.phase - 1.0
                } else {
                    3.0 - 4.0 * self.phase
                }
            }

            LfoWaveform::Saw => 2.0 * self.phase - 1.0,

            LfoWaveform::Square => {
                if self.phase < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }

            LfoWaveform::SampleAndHold => self.sh_value,
        }
    }

    /// Get next LFO value (-1.0 to 1.0)
    #[inline]
    pub fn advance(&mut self) -> f32 {
        // S&H: generate new random value on phase wrap (before reading)
        if self.waveform == LfoWaveform::SampleAndHold && self.phase < self.prev_phase {
            // LCG PRNG (Numerical Recipes constants) — ~3 cycles vs ~100 for sinf
            self.rng_state = self
                .rng_state
                .wrapping_mul(1_664_525)
                .wrapping_add(1_013_904_223);
            self.sh_value = (self.rng_state as f32) / (u32::MAX as f32) * 2.0 - 1.0;
        }

        let output = self.value_at_phase();

        self.prev_phase = self.phase;
        self.phase += self.phase_inc;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }

        output
    }

    /// Get next value scaled to range (0.0 to 1.0 for unipolar)
    pub fn advance_unipolar(&mut self) -> f32 {
        (self.advance() + 1.0) * 0.5
    }

    /// Set sample rate
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        let freq = self.phase_inc * self.sample_rate;
        self.sample_rate = sample_rate;
        self.set_frequency(freq);
    }

    /// Sync LFO frequency to tempo.
    ///
    /// Sets the LFO frequency to match a musical note division at the given BPM.
    ///
    /// # Example
    ///
    /// ```rust
    /// use sonido_core::{Lfo, NoteDivision};
    ///
    /// let mut lfo = Lfo::new(48000.0, 1.0);
    ///
    /// // Sync to eighth notes at 120 BPM (4 Hz)
    /// lfo.sync_to_tempo(120.0, NoteDivision::Eighth);
    /// assert!((lfo.frequency() - 4.0).abs() < 0.001);
    /// ```
    pub fn sync_to_tempo(&mut self, bpm: f32, division: NoteDivision) {
        let freq = division.to_hz(bpm);
        self.set_frequency(freq);
    }
}

#[cfg(test)]
#[allow(clippy::manual_range_contains)] // Manual range checks are more readable in assertions
mod tests {
    use super::*;

    #[test]
    fn test_lfo_phase_accumulation() {
        let mut lfo = Lfo::new(44100.0, 1.0); // 1 Hz = one cycle per second

        // After 44100 samples (1 second), should complete one cycle
        for _ in 0..44100 {
            lfo.advance();
        }

        // Phase should be very close to 0 or 1 (wrapped around)
        let phase_error = lfo.phase.min((lfo.phase - 1.0).abs());
        assert!(phase_error < 0.01);
    }

    #[test]
    fn test_lfo_output_range() {
        let mut lfo = Lfo::new(44100.0, 5.0);

        // Check all waveforms stay in [-1.0, 1.0]
        for waveform in [
            LfoWaveform::Sine,
            LfoWaveform::Triangle,
            LfoWaveform::Saw,
            LfoWaveform::Square,
        ] {
            lfo.set_waveform(waveform);
            lfo.reset();

            for _ in 0..1000 {
                let value = lfo.advance();
                assert!(
                    value >= -1.0 && value <= 1.0,
                    "Waveform {:?} out of range: {}",
                    waveform,
                    value
                );
            }
        }
    }

    #[test]
    fn test_lfo_phase_offset() {
        let mut lfo1 = Lfo::new(44100.0, 2.0);
        let mut lfo2 = Lfo::new(44100.0, 2.0);

        lfo2.set_phase(0.5); // 180° offset

        let val1 = lfo1.advance();
        let val2 = lfo2.advance();

        // Should be approximately opposite for sine
        assert!(
            (val1 + val2).abs() < 0.01,
            "Expected opposite values, got {} and {}",
            val1,
            val2
        );
    }

    #[test]
    fn test_lfo_sample_rate_change() {
        let mut lfo = Lfo::new(44100.0, 440.0);

        let phase_inc_44k = lfo.phase_inc;

        lfo.set_sample_rate(48000.0);
        let phase_inc_48k = lfo.phase_inc;

        // Phase increment should scale inversely with sample rate
        let ratio = 48000.0 / 44100.0;
        assert!((phase_inc_44k / phase_inc_48k - ratio).abs() < 0.0001);
    }

    #[test]
    fn test_value_at_phase_matches_advance() {
        // For non-S&H waveforms, value_at_phase() should return the same
        // value that advance() is about to return (before phase increment).
        for waveform in [
            LfoWaveform::Sine,
            LfoWaveform::Triangle,
            LfoWaveform::Saw,
            LfoWaveform::Square,
        ] {
            let mut lfo = Lfo::new(48000.0, 3.0);
            lfo.set_waveform(waveform);

            for _ in 0..500 {
                let peek = lfo.value_at_phase();
                let advanced = lfo.advance();
                assert!(
                    (peek - advanced).abs() < 1e-7,
                    "Waveform {:?}: value_at_phase()={peek} != advance()={advanced}",
                    waveform,
                );
            }
        }
    }

    #[test]
    fn test_lfo_unipolar() {
        let mut lfo = Lfo::new(44100.0, 5.0);

        for _ in 0..1000 {
            let value = lfo.advance_unipolar();
            assert!(
                value >= 0.0 && value <= 1.0,
                "Unipolar value out of range: {}",
                value
            );
        }
    }
}
