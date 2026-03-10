//! Comb filter for reverb algorithms.
//!
//! A comb filter with feedback and damping (one-pole lowpass in feedback path).
//! Essential building block for Schroeder and Freeverb-style reverbs.

use crate::InterpolatedDelay;
use crate::Interpolation;
use crate::OnePole;
use crate::fast_math::fast_sin_turns;
use crate::flush_denormal;

/// Comb filter with feedback and damping.
///
/// The feedback path includes a one-pole lowpass filter for high-frequency
/// damping, simulating the absorption of high frequencies in real acoustic spaces.
///
/// # Example
///
/// ```rust
/// use sonido_core::CombFilter;
///
/// let mut comb = CombFilter::new(1000);
/// comb.set_feedback(0.8);
/// comb.set_damp(0.3);
///
/// let output = comb.process(1.0);
/// ```
#[derive(Debug, Clone)]
pub struct CombFilter {
    delay: InterpolatedDelay,
    feedback: f32,
    damp1: f32,
    damp2: f32,
    filterstore: f32,
}

impl CombFilter {
    /// Create a new comb filter with the given delay size in samples.
    ///
    /// # Arguments
    ///
    /// * `delay_samples` - The delay length in samples
    pub fn new(delay_samples: usize) -> Self {
        Self {
            delay: InterpolatedDelay::new(delay_samples),
            feedback: 0.5,
            damp1: 0.5,
            damp2: 0.5,
            filterstore: 0.0,
        }
    }

    /// Set the feedback amount (0.0 to ~0.98).
    ///
    /// Higher values create longer decay times.
    /// Values above 0.98 may cause instability.
    #[inline]
    pub fn set_feedback(&mut self, feedback: f32) {
        self.feedback = feedback.clamp(0.0, 0.99);
    }

    /// Get the current feedback value.
    #[inline]
    pub fn feedback(&self) -> f32 {
        self.feedback
    }

    /// Set the damping amount (0.0 to 1.0).
    ///
    /// - 0.0 = no damping (bright)
    /// - 1.0 = full damping (dark/muffled)
    #[inline]
    pub fn set_damp(&mut self, damp: f32) {
        self.damp1 = damp.clamp(0.0, 1.0);
        self.damp2 = 1.0 - self.damp1;
    }

    /// Get the current damping value.
    #[inline]
    pub fn damp(&self) -> f32 {
        self.damp1
    }

    /// Process a single sample through the comb filter.
    ///
    /// The output is the delayed signal, which is then fed back through
    /// a one-pole lowpass filter and into the delay line.
    #[inline]
    pub fn process(&mut self, input: f32) -> f32 {
        // Read from the end of the delay line (integer position, no interpolation needed)
        let output = self.delay.read_integer(self.delay.capacity() - 1);

        // One-pole lowpass in feedback path (damping)
        // filterstore = output * (1 - damp) + filterstore * damp
        self.filterstore = flush_denormal(output * self.damp2 + self.filterstore * self.damp1);

        // Write input + filtered feedback to delay line
        self.delay.write(input + self.filterstore * self.feedback);

        output
    }

    /// Clear the comb filter state.
    pub fn clear(&mut self) {
        self.delay.clear();
        self.filterstore = 0.0;
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
    fn test_comb_basic() {
        let mut comb = CombFilter::new(100);
        comb.set_feedback(0.5);
        comb.set_damp(0.2);

        // Process impulse
        let first = comb.process(1.0);
        assert_eq!(first, 0.0); // First output is from empty delay

        // Process silence, wait for echo
        for _ in 0..99 {
            comb.process(0.0);
        }

        // Now we should see the delayed impulse
        let echo = comb.process(0.0);
        assert!(echo.abs() > 0.1, "Should have echo, got {}", echo);
    }

    #[test]
    fn test_comb_feedback_decay() {
        let mut comb = CombFilter::new(10);
        comb.set_feedback(0.8);
        comb.set_damp(0.0);

        // Impulse
        comb.process(1.0);

        // Process many samples
        let mut last_peak = 0.0f32;
        for _ in 0..100 {
            let out = comb.process(0.0);
            if out.abs() > 0.01 {
                // Each echo should be smaller than the last
                if last_peak > 0.0 {
                    assert!(out.abs() <= last_peak + 0.01, "Echo should decay");
                }
                last_peak = out.abs();
            }
        }
    }

    #[test]
    fn test_comb_clear() {
        let mut comb = CombFilter::new(10);

        // Fill with signal
        for _ in 0..20 {
            comb.process(1.0);
        }

        comb.clear();

        // Should output zeros
        for _ in 0..20 {
            let out = comb.process(0.0);
            assert!(out.abs() < 1e-10, "Should be silent after clear");
        }
    }

    #[test]
    fn test_comb_damping() {
        // Compare bright vs damped
        let mut bright = CombFilter::new(20);
        bright.set_feedback(0.8);
        bright.set_damp(0.0);

        let mut dark = CombFilter::new(20);
        dark.set_feedback(0.8);
        dark.set_damp(0.8);

        // Impulse
        bright.process(1.0);
        dark.process(1.0);

        // Collect output
        let mut bright_sum = 0.0f32;
        let mut dark_sum = 0.0f32;

        for _ in 0..200 {
            bright_sum += bright.process(0.0).abs();
            dark_sum += dark.process(0.0).abs();
        }

        // Damped should have less total energy (due to HF loss)
        assert!(dark_sum < bright_sum, "Damped should have less energy");
    }

    #[test]
    fn test_no_denormals_after_silence() {
        let mut comb = CombFilter::new(100);
        comb.set_feedback(0.9);
        comb.set_damp(0.3);

        // Feed signal for 1000 samples to fill delay line and build up feedback
        for _ in 0..1000 {
            comb.process(0.5);
        }

        // Feed silence for 100k samples -- signal should decay cleanly without
        // producing IEEE 754 subnormal values (which start below ~1.2e-38 and
        // cause severe CPU performance degradation on most architectures).
        // The flush_denormal() guard in the feedback path uses a 1e-20 threshold,
        // so we check that no output falls into the actual subnormal range.
        for i in 0..100_000 {
            let out = comb.process(0.0);
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

/// Comb filter with LFO-modulated delay time and one-pole damping.
///
/// Designed for FDN reverb topologies where slight pitch modulation breaks up
/// metallic resonances. The delay read position is modulated by an internal
/// sine-wave LFO, and the feedback path includes a [`OnePole`] lowpass for
/// high-frequency damping.
///
/// ## Parameters
///
/// - `delay_samples`: Base delay length in samples (typically 20–100 ms for reverb)
/// - `feedback`: Feedback coefficient (0.0 to 0.99, default 0.5)
/// - `damping`: One-pole lowpass cutoff in Hz for HF absorption (default 8000 Hz)
/// - `mod_rate`: LFO frequency in Hz (typical 0.3–1.3 Hz)
/// - `mod_depth`: Modulation depth in milliseconds (typical 0.2–0.5 ms)
/// - `sample_rate`: Sample rate in Hz
///
/// ## DSP Structure
///
/// ```text
/// input ─→ (+) ─→ [delay line] ─→ output
///            ↑         ↑ read pos = base + depth·sin(phase)
///            │         │
///            └─ feedback × [OnePole LP] ←─┘
/// ```
///
/// ## Reference
///
/// Jon Dattorro, "Effect Design, Part 1: Reverberator and Other Filters",
/// J. Audio Eng. Soc., Vol. 45, No. 9, 1997.
#[derive(Debug, Clone)]
pub struct ModulatedComb {
    delay: InterpolatedDelay,
    damping: OnePole,
    feedback: f32,
    base_delay: f32,
    mod_depth_samples: f32,
    mod_phase: f32,
    mod_phase_inc: f32,
}

impl ModulatedComb {
    /// Create a new modulated comb filter.
    ///
    /// # Arguments
    ///
    /// * `delay_samples` - Base delay length in samples
    /// * `feedback` - Feedback coefficient (clamped to 0.0–0.99)
    /// * `damping_hz` - One-pole lowpass cutoff in Hz for HF damping
    /// * `mod_rate` - LFO rate in Hz (typical 0.3–1.3 Hz)
    /// * `mod_depth_ms` - Modulation depth in milliseconds (typical 0.2–0.5 ms)
    /// * `sample_rate` - Sample rate in Hz
    pub fn new(
        delay_samples: f32,
        feedback: f32,
        damping_hz: f32,
        mod_rate: f32,
        mod_depth_ms: f32,
        sample_rate: f32,
    ) -> Self {
        let mod_depth_samples = mod_depth_ms * 0.001 * sample_rate;
        // Extra capacity for modulation excursion + 2 samples for cubic interpolation
        let capacity = (delay_samples + mod_depth_samples) as usize + 4;
        let mut delay = InterpolatedDelay::new(capacity);
        delay.set_interpolation(Interpolation::Linear);

        Self {
            delay,
            damping: OnePole::new(sample_rate, damping_hz),
            feedback: feedback.clamp(0.0, 0.99),
            base_delay: delay_samples,
            mod_depth_samples,
            mod_phase: 0.0,
            mod_phase_inc: mod_rate / sample_rate,
        }
    }

    /// Process a single sample through the modulated comb filter.
    ///
    /// The read position oscillates around `base_delay` by `mod_depth_samples`
    /// according to an internal sine LFO. The feedback signal is filtered through
    /// a one-pole lowpass before being summed with the input.
    #[inline]
    pub fn process(&mut self, input: f32) -> f32 {
        let modulated_delay =
            self.base_delay + self.mod_depth_samples * fast_sin_turns(self.mod_phase);
        let output = self.delay.read(modulated_delay);

        // Advance LFO phase (turns: 0.0–1.0 = one full cycle)
        self.mod_phase += self.mod_phase_inc;
        if self.mod_phase >= 1.0 {
            self.mod_phase -= 1.0;
        }

        // Damping in feedback path
        let damped = self.damping.process(output);
        self.delay
            .write(flush_denormal(input + damped * self.feedback));

        output
    }

    /// Set the feedback coefficient.
    ///
    /// Range: 0.0 to 0.99. Higher values create longer decay times.
    #[inline]
    pub fn set_feedback(&mut self, feedback: f32) {
        self.feedback = feedback.clamp(0.0, 0.99);
    }

    /// Get the current feedback value.
    #[inline]
    pub fn feedback(&self) -> f32 {
        self.feedback
    }

    /// Set the damping filter cutoff frequency.
    ///
    /// Range: 20.0 to sample_rate/2 Hz. Lower values absorb more high frequencies.
    #[inline]
    pub fn set_damping(&mut self, freq_hz: f32) {
        self.damping.set_frequency(freq_hz);
    }

    /// Reset all internal state (delay line, damping filter, LFO phase).
    pub fn reset(&mut self) {
        self.delay.clear();
        self.damping.reset();
        self.mod_phase = 0.0;
    }
}

#[cfg(test)]
mod modulated_comb_tests {
    use super::*;

    extern crate alloc;
    use alloc::vec::Vec;

    #[test]
    fn test_modulated_comb_basic() {
        let mut comb = ModulatedComb::new(100.0, 0.7, 6000.0, 0.5, 0.3, 48000.0);

        // Impulse response should produce finite output
        let first = comb.process(1.0);
        assert!(first.is_finite());

        for _ in 0..500 {
            let out = comb.process(0.0);
            assert!(out.is_finite());
        }
    }

    #[test]
    fn test_modulated_comb_feedback_decay() {
        let mut comb = ModulatedComb::new(50.0, 0.8, 10000.0, 0.5, 0.3, 48000.0);

        comb.process(1.0);

        let mut energy_first_half = 0.0f32;
        let mut energy_second_half = 0.0f32;

        for i in 0..2000 {
            let out = comb.process(0.0);
            if i < 1000 {
                energy_first_half += out * out;
            } else {
                energy_second_half += out * out;
            }
        }

        assert!(
            energy_second_half < energy_first_half,
            "Signal should decay: first={energy_first_half}, second={energy_second_half}"
        );
    }

    #[test]
    fn test_modulated_comb_modulation_spreads_spectrum() {
        // With modulation, the comb peaks should be slightly smeared
        // compared to a static comb. We verify by checking that output
        // varies from sample to sample (not locked to a rigid pattern).
        let mut comb = ModulatedComb::new(48.0, 0.8, 10000.0, 1.0, 0.5, 48000.0);

        // Feed white noise
        let mut outputs: Vec<f32> = Vec::with_capacity(1000);
        for i in 0..1000 {
            // Simple pseudo-noise
            let input = libm::sinf(i as f32 * 0.73) * 0.5;
            outputs.push(comb.process(input));
        }

        // Check variance is nonzero
        let mean = outputs.iter().sum::<f32>() / outputs.len() as f32;
        let variance =
            outputs.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / outputs.len() as f32;
        assert!(
            variance > 1e-6,
            "Output should have variance, got {variance}"
        );
    }

    #[test]
    fn test_modulated_comb_reset() {
        let mut comb = ModulatedComb::new(50.0, 0.8, 6000.0, 0.5, 0.3, 48000.0);

        for _ in 0..200 {
            comb.process(1.0);
        }

        comb.reset();

        for _ in 0..200 {
            let out = comb.process(0.0);
            assert!(out.abs() < 1e-10, "Should be silent after reset, got {out}");
        }
    }

    #[test]
    fn test_modulated_comb_no_denormals() {
        let mut comb = ModulatedComb::new(100.0, 0.9, 4000.0, 0.5, 0.3, 48000.0);

        for _ in 0..1000 {
            comb.process(0.5);
        }

        for i in 0..50_000 {
            let out = comb.process(0.0);
            assert!(
                out == 0.0 || out.abs() > f32::MIN_POSITIVE,
                "Denormal at sample {i}: {out:.2e}"
            );
        }
    }

    #[test]
    fn test_modulated_comb_damping_reduces_brightness() {
        // Bright (high cutoff) vs dark (low cutoff) — dark should have less energy
        let mut bright = ModulatedComb::new(50.0, 0.8, 18000.0, 0.5, 0.3, 48000.0);
        let mut dark = ModulatedComb::new(50.0, 0.8, 1000.0, 0.5, 0.3, 48000.0);

        bright.process(1.0);
        dark.process(1.0);

        let mut bright_energy = 0.0f32;
        let mut dark_energy = 0.0f32;

        for _ in 0..2000 {
            bright_energy += bright.process(0.0).abs();
            dark_energy += dark.process(0.0).abs();
        }

        assert!(
            dark_energy < bright_energy,
            "Damped should have less energy: dark={dark_energy}, bright={bright_energy}"
        );
    }
}
