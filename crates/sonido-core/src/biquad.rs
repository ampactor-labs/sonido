//! Biquad (bi-quadratic) filter structure.
//!
//! Provides a generic second-order IIR filter that can be configured
//! for various filter types (low-pass, high-pass, band-pass, notch).
//!
//! Coefficient calculation uses the RBJ Audio EQ Cookbook formulas.

use crate::flush_denormal;
use core::f32::consts::PI;
use libm::{cosf, sinf};

/// Generic biquad filter coefficients and state.
///
/// Implements the Direct Form I biquad structure:
/// ```text
/// y[n] = b0*x[n] + b1*x[n-1] + b2*x[n-2]
///                - a1*y[n-1] - a2*y[n-2]
/// ```
///
/// This is a building block for creating specific filter types.
#[derive(Debug, Clone)]
pub struct Biquad {
    /// Feedforward coefficients
    b0: f32,
    b1: f32,
    b2: f32,

    /// Feedback coefficients (stored as negated for efficiency)
    a1: f32,
    a2: f32,

    /// Input delay line: x[n-1], x[n-2]
    x1: f32,
    x2: f32,

    /// Output delay line: y[n-1], y[n-2]
    y1: f32,
    y2: f32,
}

impl Biquad {
    /// Creates a new biquad with passthrough coefficients.
    ///
    /// Initial state: `y[n] = x[n]` (no filtering)
    pub fn new() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    /// Sets the biquad coefficients.
    ///
    /// # Arguments
    ///
    /// * `b0, b1, b2` - Feedforward coefficients
    /// * `a0, a1, a2` - Feedback coefficients (a0 is typically 1.0)
    ///
    /// Note: This function normalizes by a0 internally.
    pub fn set_coefficients(&mut self, b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) {
        // Normalize by a0
        let a0_inv = 1.0 / a0;
        self.b0 = b0 * a0_inv;
        self.b1 = b1 * a0_inv;
        self.b2 = b2 * a0_inv;
        self.a1 = a1 * a0_inv;
        self.a2 = a2 * a0_inv;
    }

    /// Processes a single sample through the biquad filter.
    ///
    /// Uses Direct Form I structure for numerical stability.
    #[inline]
    pub fn process(&mut self, input: f32) -> f32 {
        // Calculate output: y[n] = b0*x[n] + b1*x[n-1] + b2*x[n-2]
        //                                   - a1*y[n-1] - a2*y[n-2]
        let output = self.b0 * input + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;

        // Update delay lines (flush denormals to prevent CPU slowdown in feedback)
        self.x2 = self.x1;
        self.x1 = flush_denormal(input);
        self.y2 = self.y1;
        self.y1 = flush_denormal(output);

        output
    }

    /// Clears the filter state (delay lines).
    ///
    /// Useful for resetting the filter without changing coefficients.
    pub fn clear(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }
}

impl Default for Biquad {
    fn default() -> Self {
        Self::new()
    }
}

/// Calculates low-pass filter coefficients using the RBJ cookbook formula.
///
/// # Arguments
///
/// * `frequency` - Cutoff frequency in Hz
/// * `q` - Q factor (typically 0.707 for Butterworth response)
/// * `sample_rate` - Sample rate in Hz
///
/// # Returns
///
/// (b0, b1, b2, a0, a1, a2) coefficients
pub fn lowpass_coefficients(
    frequency: f32,
    q: f32,
    sample_rate: f32,
) -> (f32, f32, f32, f32, f32, f32) {
    let omega = 2.0 * PI * frequency / sample_rate;
    let cos_omega = cosf(omega);
    let sin_omega = sinf(omega);
    let alpha = sin_omega / (2.0 * q);

    let b0 = (1.0 - cos_omega) / 2.0;
    let b1 = 1.0 - cos_omega;
    let b2 = (1.0 - cos_omega) / 2.0;
    let a0 = 1.0 + alpha;
    let a1 = -2.0 * cos_omega;
    let a2 = 1.0 - alpha;

    (b0, b1, b2, a0, a1, a2)
}

/// Calculates high-pass filter coefficients using the RBJ cookbook formula.
///
/// # Arguments
///
/// * `frequency` - Cutoff frequency in Hz
/// * `q` - Q factor (typically 0.707 for Butterworth response)
/// * `sample_rate` - Sample rate in Hz
///
/// # Returns
///
/// (b0, b1, b2, a0, a1, a2) coefficients
pub fn highpass_coefficients(
    frequency: f32,
    q: f32,
    sample_rate: f32,
) -> (f32, f32, f32, f32, f32, f32) {
    let omega = 2.0 * PI * frequency / sample_rate;
    let cos_omega = cosf(omega);
    let sin_omega = sinf(omega);
    let alpha = sin_omega / (2.0 * q);

    let b0 = (1.0 + cos_omega) / 2.0;
    let b1 = -(1.0 + cos_omega);
    let b2 = (1.0 + cos_omega) / 2.0;
    let a0 = 1.0 + alpha;
    let a1 = -2.0 * cos_omega;
    let a2 = 1.0 - alpha;

    (b0, b1, b2, a0, a1, a2)
}

/// Calculates band-pass filter coefficients using the RBJ cookbook formula.
///
/// This version has constant 0dB peak gain.
///
/// # Arguments
///
/// * `frequency` - Center frequency in Hz
/// * `q` - Q factor (bandwidth = frequency / Q)
/// * `sample_rate` - Sample rate in Hz
///
/// # Returns
///
/// (b0, b1, b2, a0, a1, a2) coefficients
pub fn bandpass_coefficients(
    frequency: f32,
    q: f32,
    sample_rate: f32,
) -> (f32, f32, f32, f32, f32, f32) {
    let omega = 2.0 * PI * frequency / sample_rate;
    let cos_omega = cosf(omega);
    let sin_omega = sinf(omega);
    let alpha = sin_omega / (2.0 * q);

    let b0 = alpha;
    let b1 = 0.0;
    let b2 = -alpha;
    let a0 = 1.0 + alpha;
    let a1 = -2.0 * cos_omega;
    let a2 = 1.0 - alpha;

    (b0, b1, b2, a0, a1, a2)
}

/// Calculates notch (band-reject) filter coefficients using the RBJ cookbook formula.
///
/// # Arguments
///
/// * `frequency` - Notch frequency in Hz
/// * `q` - Q factor (notch width = frequency / Q)
/// * `sample_rate` - Sample rate in Hz
///
/// # Returns
///
/// (b0, b1, b2, a0, a1, a2) coefficients
pub fn notch_coefficients(
    frequency: f32,
    q: f32,
    sample_rate: f32,
) -> (f32, f32, f32, f32, f32, f32) {
    let omega = 2.0 * PI * frequency / sample_rate;
    let cos_omega = cosf(omega);
    let sin_omega = sinf(omega);
    let alpha = sin_omega / (2.0 * q);

    let b0 = 1.0;
    let b1 = -2.0 * cos_omega;
    let b2 = 1.0;
    let a0 = 1.0 + alpha;
    let a1 = -2.0 * cos_omega;
    let a2 = 1.0 - alpha;

    (b0, b1, b2, a0, a1, a2)
}

/// Calculates peaking EQ filter coefficients using the RBJ cookbook formula.
///
/// A peaking EQ boosts or cuts around a center frequency with a specified bandwidth.
/// Used for parametric equalizers.
///
/// # Arguments
///
/// * `frequency` - Center frequency in Hz
/// * `q` - Q factor (bandwidth = frequency / Q)
/// * `gain_db` - Gain in decibels (positive = boost, negative = cut)
/// * `sample_rate` - Sample rate in Hz
///
/// # Returns
///
/// (b0, b1, b2, a0, a1, a2) coefficients
pub fn peaking_eq_coefficients(
    frequency: f32,
    q: f32,
    gain_db: f32,
    sample_rate: f32,
) -> (f32, f32, f32, f32, f32, f32) {
    use libm::powf;

    let a = powf(10.0, gain_db / 40.0); // sqrt(10^(dB/20))
    let omega = 2.0 * PI * frequency / sample_rate;
    let cos_omega = cosf(omega);
    let sin_omega = sinf(omega);
    let alpha = sin_omega / (2.0 * q);

    let b0 = 1.0 + alpha * a;
    let b1 = -2.0 * cos_omega;
    let b2 = 1.0 - alpha * a;
    let a0 = 1.0 + alpha / a;
    let a1 = -2.0 * cos_omega;
    let a2 = 1.0 - alpha / a;

    (b0, b1, b2, a0, a1, a2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_biquad_passthrough() {
        let mut biquad = Biquad::new();

        // Default coefficients should pass signal through
        for i in 0..10 {
            let input = i as f32 * 0.1;
            let output = biquad.process(input);
            assert!((output - input).abs() < 0.0001);
        }
    }

    #[test]
    fn test_biquad_clear() {
        let mut biquad = Biquad::new();

        // Process some samples to fill state
        for _ in 0..10 {
            biquad.process(1.0);
        }

        // Clear state
        biquad.clear();

        // State should be zero
        assert_eq!(biquad.x1, 0.0);
        assert_eq!(biquad.x2, 0.0);
        assert_eq!(biquad.y1, 0.0);
        assert_eq!(biquad.y2, 0.0);
    }

    #[test]
    fn test_lowpass_coefficients() {
        // Test that coefficient calculation doesn't panic
        let (b0, b1, b2, a0, a1, a2) = lowpass_coefficients(1000.0, 0.707, 44100.0);

        // Basic sanity checks
        assert!(b0.is_finite());
        assert!(b1.is_finite());
        assert!(b2.is_finite());
        assert!(a0.is_finite());
        assert!(a1.is_finite());
        assert!(a2.is_finite());

        // a0 should be close to 1.0 after normalization
        assert!(a0 > 0.0);
    }

    #[test]
    fn test_biquad_lowpass_dc_pass() {
        let mut biquad = Biquad::new();

        // Set up low-pass filter
        let (b0, b1, b2, a0, a1, a2) = lowpass_coefficients(1000.0, 0.707, 44100.0);
        biquad.set_coefficients(b0, b1, b2, a0, a1, a2);

        // Process DC signal (0 Hz)
        let mut output = 0.0;
        for _ in 0..1000 {
            output = biquad.process(1.0);
        }

        // DC should pass through a low-pass filter with near-unity gain
        assert!((output - 1.0).abs() < 0.05);
    }

    #[test]
    fn test_highpass_coefficients() {
        let (b0, b1, b2, a0, a1, a2) = highpass_coefficients(1000.0, 0.707, 44100.0);

        assert!(b0.is_finite());
        assert!(b1.is_finite());
        assert!(b2.is_finite());
        assert!(a0.is_finite());
        assert!(a1.is_finite());
        assert!(a2.is_finite());
    }

    #[test]
    fn test_bandpass_coefficients() {
        let (b0, b1, b2, a0, a1, a2) = bandpass_coefficients(1000.0, 1.0, 44100.0);

        assert!(b0.is_finite());
        assert!(b1.is_finite());
        assert!(b2.is_finite());
        assert!(a0.is_finite());
        assert!(a1.is_finite());
        assert!(a2.is_finite());
    }

    #[test]
    fn test_notch_coefficients() {
        let (b0, b1, b2, a0, a1, a2) = notch_coefficients(1000.0, 1.0, 44100.0);

        assert!(b0.is_finite());
        assert!(b1.is_finite());
        assert!(b2.is_finite());
        assert!(a0.is_finite());
        assert!(a1.is_finite());
        assert!(a2.is_finite());
    }

    #[test]
    fn test_peaking_eq_coefficients() {
        // Test boost
        let (b0, b1, b2, a0, a1, a2) = peaking_eq_coefficients(1000.0, 1.0, 6.0, 44100.0);

        assert!(b0.is_finite());
        assert!(b1.is_finite());
        assert!(b2.is_finite());
        assert!(a0.is_finite());
        assert!(a1.is_finite());
        assert!(a2.is_finite());

        // Test cut
        let (b0, b1, b2, a0, a1, a2) = peaking_eq_coefficients(1000.0, 1.0, -6.0, 44100.0);

        assert!(b0.is_finite());
        assert!(b1.is_finite());
        assert!(b2.is_finite());
        assert!(a0.is_finite());
        assert!(a1.is_finite());
        assert!(a2.is_finite());
    }

    #[test]
    fn test_peaking_eq_unity_at_zero_gain() {
        let mut biquad = Biquad::new();
        let (b0, b1, b2, a0, a1, a2) = peaking_eq_coefficients(1000.0, 1.0, 0.0, 44100.0);
        biquad.set_coefficients(b0, b1, b2, a0, a1, a2);

        // At 0dB gain, DC should pass through unchanged
        let mut output = 0.0;
        for _ in 0..1000 {
            output = biquad.process(1.0);
        }

        assert!(
            (output - 1.0).abs() < 0.05,
            "DC should pass at 0dB gain, got {}",
            output
        );
    }
}
