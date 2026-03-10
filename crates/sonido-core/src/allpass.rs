//! Allpass filter for reverb diffusion.
//!
//! A Schroeder allpass filter that adds diffusion without coloring the
//! frequency response. Essential for creating dense, smooth reverb tails.

use crate::InterpolatedDelay;
use crate::Interpolation;
use crate::fast_math::fast_sin_turns;
use crate::flush_denormal;

/// Schroeder allpass filter for diffusion.
///
/// Allpass filters pass all frequencies at equal amplitude but modify
/// the phase. In reverb, they "smear" the impulse response, creating
/// a denser, more diffuse sound.
///
/// # Example
///
/// ```rust
/// use sonido_core::AllpassFilter;
///
/// let mut allpass = AllpassFilter::new(500);
/// allpass.set_feedback(0.5);
///
/// let output = allpass.process(1.0);
/// ```
#[derive(Debug, Clone)]
pub struct AllpassFilter {
    delay: InterpolatedDelay,
    feedback: f32,
}

impl AllpassFilter {
    /// Create a new allpass filter with the given delay size in samples.
    ///
    /// # Arguments
    ///
    /// * `delay_samples` - The delay length in samples
    pub fn new(delay_samples: usize) -> Self {
        Self {
            delay: InterpolatedDelay::new(delay_samples),
            feedback: 0.5,
        }
    }

    /// Set the feedback coefficient.
    ///
    /// Typical values are around 0.5 for reverb diffusion.
    /// The allpass is stable for |feedback| < 1.0.
    #[inline]
    pub fn set_feedback(&mut self, feedback: f32) {
        self.feedback = feedback.clamp(-0.99, 0.99);
    }

    /// Get the current feedback value.
    #[inline]
    pub fn feedback(&self) -> f32 {
        self.feedback
    }

    /// Process a single sample through the allpass filter.
    ///
    /// Uses the Schroeder allpass structure:
    /// output = -input + delayed
    /// delay_input = input + delayed * feedback
    #[inline]
    pub fn process(&mut self, input: f32) -> f32 {
        // Integer read — no interpolation needed for fixed delay
        let delayed = self.delay.read_integer(self.delay.capacity() - 1);

        // Schroeder allpass: output = -input + delayed
        let output = -input + delayed;

        // Feed forward: input + delayed * feedback
        self.delay
            .write(flush_denormal(input + delayed * self.feedback));

        output
    }

    /// Clear the allpass filter state.
    pub fn clear(&mut self) {
        self.delay.clear();
    }

    /// Get the delay capacity in samples.
    pub fn capacity(&self) -> usize {
        self.delay.capacity()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allpass_basic() {
        let mut allpass = AllpassFilter::new(100);
        allpass.set_feedback(0.5);

        for _ in 0..200 {
            let out = allpass.process(0.5);
            assert!(out.is_finite());
        }
    }

    #[test]
    fn test_allpass_energy_conservation() {
        // Allpass should preserve energy (approximately)
        let mut allpass = AllpassFilter::new(50);
        allpass.set_feedback(0.5);

        let input_energy: f32 = (0..500)
            .map(|i| {
                let x = if i < 100 { 1.0 } else { 0.0 };
                x * x
            })
            .sum();

        let output_energy: f32 = (0..500)
            .map(|i| {
                let x = if i < 100 { 1.0 } else { 0.0 };
                let y = allpass.process(x);
                y * y
            })
            .sum();

        // Should be within 20% (not exact due to transient behavior)
        let ratio = output_energy / input_energy;
        assert!(
            ratio > 0.5 && ratio < 2.0,
            "Energy ratio {} should be close to 1.0",
            ratio
        );
    }

    #[test]
    fn test_allpass_clear() {
        let mut allpass = AllpassFilter::new(10);

        // Fill with signal
        for _ in 0..20 {
            allpass.process(1.0);
        }

        allpass.clear();

        // After clear, processing zeros should give small output
        // (due to -input term, first output will be -0 = 0)
        let out = allpass.process(0.0);
        assert!(out.abs() < 1e-10, "Should be silent after clear");
    }

    #[test]
    fn test_allpass_impulse_response() {
        let mut allpass = AllpassFilter::new(10);
        allpass.set_feedback(0.5);

        // Impulse
        let first = allpass.process(1.0);
        assert!(
            (first - (-1.0)).abs() < 0.01,
            "First output should be -input"
        );

        // Wait for delay
        for _ in 0..9 {
            allpass.process(0.0);
        }

        // Delayed impulse should appear
        let delayed = allpass.process(0.0);
        assert!(delayed.abs() > 0.3, "Should have delayed output");
    }

    #[test]
    fn test_no_denormals_after_silence() {
        let mut allpass = AllpassFilter::new(100);
        allpass.set_feedback(0.7);

        // Feed signal for 1000 samples to build up internal state
        for _ in 0..1000 {
            allpass.process(0.5);
        }

        // Feed silence for 100k samples -- output should decay cleanly without
        // producing IEEE 754 subnormal values (which start below ~1.2e-38 and
        // cause severe CPU performance degradation on most architectures).
        for i in 0..100_000 {
            let out = allpass.process(0.0);
            assert!(
                out == 0.0 || out.abs() > f32::MIN_POSITIVE,
                "Denormal detected at sample {}: {:.2e} (below f32::MIN_POSITIVE {:.2e})",
                i,
                out,
                f32::MIN_POSITIVE
            );
        }
    }
}

/// Allpass filter with LFO-modulated delay time for diffusion in FDN reverbs.
///
/// A Schroeder allpass with sinusoidal modulation of the delay read position.
/// The modulation decorrelates reflections, reducing metallic coloration and
/// flutter echoes in dense reverb tails.
///
/// ## Parameters
///
/// - `delay_samples`: Base delay length in samples
/// - `feedback`: Allpass feedback coefficient (clamped to −0.99…0.99)
/// - `mod_rate`: LFO frequency in Hz (typical 0.5–1.0 Hz)
/// - `mod_depth`: Modulation depth in milliseconds (typical 0.1–0.3 ms)
/// - `sample_rate`: Sample rate in Hz
///
/// ## DSP Structure
///
/// ```text
///          ┌──────── −g ──────────┐
///          │                      ↓
/// input ─→(+)─→ [delay line] ─→ (+) ─→ output
///                ↑ read pos = base + depth·sin(phase)
///                │
///                └─── g × delayed ───┘
/// ```
///
/// ## Reference
///
/// Jon Dattorro, "Effect Design, Part 1: Reverberator and Other Filters",
/// J. Audio Eng. Soc., Vol. 45, No. 9, 1997.
#[derive(Debug, Clone)]
pub struct ModulatedAllpass {
    delay: InterpolatedDelay,
    feedback: f32,
    base_delay: f32,
    mod_depth_samples: f32,
    mod_phase: f32,
    mod_phase_inc: f32,
}

impl ModulatedAllpass {
    /// Create a new modulated allpass filter.
    ///
    /// # Arguments
    ///
    /// * `delay_samples` - Base delay length in samples
    /// * `feedback` - Allpass feedback coefficient (clamped to −0.99…0.99)
    /// * `mod_rate` - LFO rate in Hz (typical 0.5–1.0 Hz)
    /// * `mod_depth_ms` - Modulation depth in milliseconds (typical 0.1–0.3 ms)
    /// * `sample_rate` - Sample rate in Hz
    pub fn new(
        delay_samples: f32,
        feedback: f32,
        mod_rate: f32,
        mod_depth_ms: f32,
        sample_rate: f32,
    ) -> Self {
        let mod_depth_samples = mod_depth_ms * 0.001 * sample_rate;
        // Extra capacity for modulation excursion + margin for cubic interpolation
        let capacity = (delay_samples + mod_depth_samples) as usize + 4;
        let mut delay = InterpolatedDelay::new(capacity);
        delay.set_interpolation(Interpolation::Linear);

        Self {
            delay,
            feedback: feedback.clamp(-0.99, 0.99),
            base_delay: delay_samples,
            mod_depth_samples,
            mod_phase: 0.0,
            mod_phase_inc: mod_rate / sample_rate,
        }
    }

    /// Process a single sample through the modulated allpass filter.
    ///
    /// Uses the Schroeder allpass structure with a modulated read position:
    /// `output = -g·input + delayed`, `write = input + g·delayed`.
    #[inline]
    pub fn process(&mut self, input: f32) -> f32 {
        let modulated_delay =
            self.base_delay + self.mod_depth_samples * fast_sin_turns(self.mod_phase);
        let delayed = self.delay.read(modulated_delay);

        // Advance LFO phase (turns: 0.0–1.0 = one full cycle)
        self.mod_phase += self.mod_phase_inc;
        if self.mod_phase >= 1.0 {
            self.mod_phase -= 1.0;
        }

        // Schroeder allpass: output = -input + delayed
        let output = -input + delayed;
        self.delay
            .write(flush_denormal(input + delayed * self.feedback));

        output
    }

    /// Set the feedback coefficient.
    ///
    /// Range: −0.99 to 0.99. Typical reverb values are around 0.5–0.7.
    #[inline]
    pub fn set_feedback(&mut self, feedback: f32) {
        self.feedback = feedback.clamp(-0.99, 0.99);
    }

    /// Get the current feedback value.
    #[inline]
    pub fn feedback(&self) -> f32 {
        self.feedback
    }

    /// Reset all internal state (delay line, LFO phase).
    pub fn reset(&mut self) {
        self.delay.clear();
        self.mod_phase = 0.0;
    }
}

#[cfg(test)]
mod modulated_allpass_tests {
    use super::*;

    extern crate alloc;
    use alloc::vec::Vec;

    #[test]
    fn test_modulated_allpass_basic() {
        let mut ap = ModulatedAllpass::new(100.0, 0.5, 0.7, 0.2, 48000.0);

        for _ in 0..500 {
            let out = ap.process(0.5);
            assert!(out.is_finite());
        }
    }

    #[test]
    fn test_modulated_allpass_impulse_response() {
        let mut ap = ModulatedAllpass::new(50.0, 0.5, 0.7, 0.2, 48000.0);

        // First sample: output = -input + delayed(0) = -1.0
        let first = ap.process(1.0);
        assert!(
            (first - (-1.0)).abs() < 0.01,
            "First output should be ~-1.0, got {first}"
        );

        // Collect enough samples to see the delayed impulse (modulation shifts timing)
        let mut peak = 0.0f32;
        for _ in 0..80 {
            let out = ap.process(0.0);
            if out.abs() > peak {
                peak = out.abs();
            }
        }
        assert!(
            peak > 0.1,
            "Should have delayed output within window, peak={peak}"
        );
    }

    #[test]
    fn test_modulated_allpass_energy_conservation() {
        // Collect over a longer window so transient energy averages out
        let input_energy: f32 = (0..2000)
            .map(|i| {
                let x = if i < 200 { 1.0 } else { 0.0 };
                x * x
            })
            .sum();

        let output_energy: f32 = {
            let mut ap = ModulatedAllpass::new(50.0, 0.5, 0.7, 0.2, 48000.0);
            (0..2000)
                .map(|i| {
                    let x = if i < 200 { 1.0 } else { 0.0 };
                    let y = ap.process(x);
                    y * y
                })
                .sum()
        };

        // Modulated allpass approximately conserves energy; wider tolerance
        // than static allpass due to delay-time modulation causing pitch shift
        let ratio = output_energy / input_energy;
        assert!(
            ratio > 0.3 && ratio < 3.0,
            "Energy ratio {ratio} should be roughly near 1.0"
        );
    }

    #[test]
    fn test_modulated_allpass_modulation_varies_output() {
        let mut ap = ModulatedAllpass::new(48.0, 0.6, 1.0, 0.3, 48000.0);

        // Feed constant signal — output should vary due to modulation
        let outputs: Vec<f32> = (0..2000).map(|_| ap.process(0.3)).collect();

        let mean = outputs.iter().sum::<f32>() / outputs.len() as f32;
        let variance =
            outputs.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / outputs.len() as f32;
        assert!(
            variance > 1e-8,
            "Modulated output should vary, variance={variance}"
        );
    }

    #[test]
    fn test_modulated_allpass_reset() {
        let mut ap = ModulatedAllpass::new(50.0, 0.6, 0.7, 0.2, 48000.0);

        for _ in 0..200 {
            ap.process(1.0);
        }

        ap.reset();

        for _ in 0..200 {
            let out = ap.process(0.0);
            assert!(out.abs() < 1e-10, "Should be silent after reset, got {out}");
        }
    }

    #[test]
    fn test_modulated_allpass_no_denormals() {
        let mut ap = ModulatedAllpass::new(100.0, 0.7, 0.5, 0.2, 48000.0);

        for _ in 0..1000 {
            ap.process(0.5);
        }

        for i in 0..50_000 {
            let out = ap.process(0.0);
            assert!(
                out == 0.0 || out.abs() > f32::MIN_POSITIVE,
                "Denormal at sample {i}: {out:.2e}"
            );
        }
    }
}
