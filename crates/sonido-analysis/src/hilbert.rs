//! Hilbert transform for computing analytic signals.
//!
//! The Hilbert transform is fundamental for extracting instantaneous phase and
//! amplitude (envelope) from signals, which is essential for phase-amplitude
//! coupling analysis in biosignal research.
//!
//! # Algorithm
//!
//! The Hilbert transform is computed using the FFT method:
//! 1. Compute FFT of the real signal
//! 2. Zero out negative frequencies (bins N/2+1 to N-1)
//! 3. Double positive frequencies (bins 1 to N/2-1)
//! 4. Keep DC and Nyquist unchanged
//! 5. Inverse FFT gives the analytic signal
//!
//! From the analytic signal:
//! - Instantaneous amplitude = |analytic|
//! - Instantaneous phase = arg(analytic)
//!
//! # Example
//!
//! ```rust
//! use sonido_analysis::hilbert::HilbertTransform;
//! use std::f32::consts::PI;
//!
//! let hilbert = HilbertTransform::new(1024);
//!
//! // Create a sine wave
//! let signal: Vec<f32> = (0..1024)
//!     .map(|i| (2.0 * PI * 10.0 * i as f32 / 1024.0).sin())
//!     .collect();
//!
//! // Get the analytic signal
//! let analytic = hilbert.analytic_signal(&signal);
//!
//! // Amplitude should be ~1.0, phase should increase linearly
//! let amplitude = hilbert.instantaneous_amplitude(&signal);
//! let phase = hilbert.instantaneous_phase(&signal);
//! ```

use crate::fft::Fft;
use rustfft::num_complex::Complex;
use std::f32::consts::PI;

/// Hilbert transform processor for computing analytic signals.
///
/// Uses FFT-based Hilbert transform to compute the analytic signal,
/// from which instantaneous phase and amplitude can be extracted.
pub struct HilbertTransform {
    fft: Fft,
    fft_size: usize,
}

impl HilbertTransform {
    /// Create a new Hilbert transform processor.
    ///
    /// # Arguments
    ///
    /// * `fft_size` - Size of the FFT. Input signals will be zero-padded or truncated
    ///   to this size. Should be a power of 2 for efficiency.
    pub fn new(fft_size: usize) -> Self {
        Self {
            fft: Fft::new(fft_size),
            fft_size,
        }
    }

    /// Get the FFT size.
    pub fn fft_size(&self) -> usize {
        self.fft_size
    }

    /// Compute the analytic signal using the Hilbert transform.
    ///
    /// The analytic signal z(t) = x(t) + i*H{x(t)} where H{} is the Hilbert transform.
    /// This is computed efficiently using the FFT method.
    ///
    /// # Arguments
    ///
    /// * `signal` - Input real signal (will be zero-padded or truncated to FFT size)
    ///
    /// # Returns
    ///
    /// Complex analytic signal of length equal to input signal length (up to FFT size)
    pub fn analytic_signal(&self, signal: &[f32]) -> Vec<Complex<f32>> {
        let n = signal.len().min(self.fft_size);

        // Create complex buffer from input signal
        let mut buffer: Vec<Complex<f32>> =
            signal[..n].iter().map(|&x| Complex::new(x, 0.0)).collect();

        // Zero-pad to FFT size
        buffer.resize(self.fft_size, Complex::new(0.0, 0.0));

        // Forward FFT
        self.fft.forward_complex(&mut buffer);

        // Apply Hilbert transform in frequency domain:
        // - DC (bin 0): unchanged
        // - Positive frequencies (bins 1 to N/2-1): multiply by 2
        // - Nyquist (bin N/2): unchanged (if N is even)
        // - Negative frequencies (bins N/2+1 to N-1): set to zero
        let half = self.fft_size / 2;

        // Double positive frequencies
        for sample in buffer.iter_mut().take(half).skip(1) {
            *sample *= 2.0;
        }

        // Zero negative frequencies
        for sample in buffer.iter_mut().take(self.fft_size).skip(half + 1) {
            *sample = Complex::new(0.0, 0.0);
        }

        // Inverse FFT
        self.fft.inverse_complex(&mut buffer);

        // Return only the original signal length
        buffer.truncate(n);
        buffer
    }

    /// Compute the instantaneous phase of the signal.
    ///
    /// The instantaneous phase is the argument (angle) of the analytic signal,
    /// ranging from -PI to PI radians.
    ///
    /// # Arguments
    ///
    /// * `signal` - Input real signal
    ///
    /// # Returns
    ///
    /// Instantaneous phase in radians for each sample
    pub fn instantaneous_phase(&self, signal: &[f32]) -> Vec<f32> {
        self.analytic_signal(signal)
            .iter()
            .map(|c| c.arg())
            .collect()
    }

    /// Compute the instantaneous amplitude (envelope) of the signal.
    ///
    /// The instantaneous amplitude is the magnitude of the analytic signal,
    /// representing the envelope of the oscillation.
    ///
    /// # Arguments
    ///
    /// * `signal` - Input real signal
    ///
    /// # Returns
    ///
    /// Instantaneous amplitude for each sample
    pub fn instantaneous_amplitude(&self, signal: &[f32]) -> Vec<f32> {
        self.analytic_signal(signal)
            .iter()
            .map(|c| c.norm())
            .collect()
    }

    /// Compute both phase and amplitude simultaneously.
    ///
    /// More efficient than calling `instantaneous_phase` and `instantaneous_amplitude`
    /// separately as it only computes the Hilbert transform once.
    ///
    /// # Returns
    ///
    /// Tuple of (phase vector, amplitude vector)
    pub fn phase_and_amplitude(&self, signal: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let analytic = self.analytic_signal(signal);

        let phase: Vec<f32> = analytic.iter().map(|c| c.arg()).collect();
        let amplitude: Vec<f32> = analytic.iter().map(|c| c.norm()).collect();

        (phase, amplitude)
    }

    /// Unwrap the phase to remove discontinuities.
    ///
    /// Phase values jump from PI to -PI (or vice versa) at discontinuities.
    /// This function unwraps the phase to create a continuous curve.
    ///
    /// Delegates to [`crate::phase::unwrap_phase`]. Kept for backwards compatibility.
    ///
    /// # Arguments
    ///
    /// * `phase` - Wrapped phase values in radians
    ///
    /// # Returns
    ///
    /// Unwrapped phase values
    pub fn unwrap_phase(phase: &[f32]) -> Vec<f32> {
        crate::phase::unwrap_phase(phase)
    }

    /// Compute the instantaneous frequency from the phase.
    ///
    /// The instantaneous frequency is the derivative of the unwrapped phase
    /// divided by 2*PI, giving frequency in Hz.
    ///
    /// # Arguments
    ///
    /// * `signal` - Input signal
    /// * `sample_rate` - Sample rate in Hz
    ///
    /// # Returns
    ///
    /// Instantaneous frequency in Hz for each sample (length = signal length - 1)
    pub fn instantaneous_frequency(&self, signal: &[f32], sample_rate: f32) -> Vec<f32> {
        let phase = self.instantaneous_phase(signal);
        let unwrapped = Self::unwrap_phase(&phase);

        // Compute derivative (central difference)
        let mut freq = Vec::with_capacity(unwrapped.len().saturating_sub(1));

        for i in 0..unwrapped.len().saturating_sub(1) {
            let dphi = unwrapped[i + 1] - unwrapped[i];
            let f = dphi * sample_rate / (2.0 * PI);
            freq.push(f);
        }

        freq
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate a sine wave at a given frequency.
    fn sine_wave(frequency: f32, sample_rate: f32, num_samples: usize) -> Vec<f32> {
        (0..num_samples)
            .map(|i| (2.0 * PI * frequency * i as f32 / sample_rate).sin())
            .collect()
    }

    /// Generate a cosine wave at a given frequency.
    fn cosine_wave(frequency: f32, sample_rate: f32, num_samples: usize) -> Vec<f32> {
        (0..num_samples)
            .map(|i| (2.0 * PI * frequency * i as f32 / sample_rate).cos())
            .collect()
    }

    #[test]
    fn test_hilbert_sine_to_cosine() {
        // The Hilbert transform of sin(x) should be -cos(x)
        // So the imaginary part of the analytic signal of sin should be -cos
        let sample_rate = 1000.0;
        let frequency = 10.0;
        let num_samples = 512;

        let hilbert = HilbertTransform::new(num_samples);

        let sine = sine_wave(frequency, sample_rate, num_samples);
        let cosine = cosine_wave(frequency, sample_rate, num_samples);

        let analytic = hilbert.analytic_signal(&sine);

        // Check middle portion (avoid edge effects)
        let start = num_samples / 4;
        let end = 3 * num_samples / 4;

        for i in start..end {
            // Real part should be the original sine
            assert!(
                (analytic[i].re - sine[i]).abs() < 0.05,
                "Real part mismatch at {}: {} vs {}",
                i,
                analytic[i].re,
                sine[i]
            );

            // Imaginary part should be -cos (which is sin shifted by +90 degrees)
            // Actually, Hilbert of sin = -cos, so imaginary should be -cos
            assert!(
                (analytic[i].im - (-cosine[i])).abs() < 0.1,
                "Imaginary part mismatch at {}: {} vs {}",
                i,
                analytic[i].im,
                -cosine[i]
            );
        }
    }

    #[test]
    fn test_hilbert_amplitude_sine() {
        // The instantaneous amplitude of a pure sine wave should be constant ~1.0
        let sample_rate = 1000.0;
        let frequency = 10.0;
        let num_samples = 512;

        let hilbert = HilbertTransform::new(num_samples);
        let sine = sine_wave(frequency, sample_rate, num_samples);

        let amplitude = hilbert.instantaneous_amplitude(&sine);

        // Check middle portion (avoid edge effects)
        let start = num_samples / 4;
        let end = 3 * num_samples / 4;

        for (i, &amp) in amplitude.iter().enumerate().take(end).skip(start) {
            assert!(
                (amp - 1.0).abs() < 0.1,
                "Amplitude should be ~1.0, got {} at sample {}",
                amp,
                i
            );
        }
    }

    #[test]
    fn test_hilbert_phase_linear() {
        // The unwrapped phase of a pure sine wave should increase linearly
        let sample_rate = 1000.0;
        let frequency = 10.0;
        let num_samples = 512;

        let hilbert = HilbertTransform::new(num_samples);
        let sine = sine_wave(frequency, sample_rate, num_samples);

        let phase = hilbert.instantaneous_phase(&sine);
        let unwrapped = HilbertTransform::unwrap_phase(&phase);

        // Check that phase increases linearly in the middle portion
        let start = num_samples / 4;
        let end = 3 * num_samples / 4;

        // Expected phase increment per sample
        let expected_delta = 2.0 * PI * frequency / sample_rate;

        for i in (start + 1)..end {
            let delta = unwrapped[i] - unwrapped[i - 1];
            assert!(
                (delta - expected_delta).abs() < 0.1,
                "Phase delta should be ~{}, got {} at sample {}",
                expected_delta,
                delta,
                i
            );
        }
    }

    #[test]
    fn test_instantaneous_frequency() {
        // The instantaneous frequency of a pure sine should be constant
        let sample_rate = 1000.0;
        let frequency = 25.0;
        let num_samples = 512;

        let hilbert = HilbertTransform::new(num_samples);
        let sine = sine_wave(frequency, sample_rate, num_samples);

        let inst_freq = hilbert.instantaneous_frequency(&sine, sample_rate);

        // Check middle portion
        let start = num_samples / 4;
        let end = 3 * num_samples / 4;

        for (i, &freq) in inst_freq
            .iter()
            .enumerate()
            .take(end.min(inst_freq.len()))
            .skip(start)
        {
            assert!(
                (freq - frequency).abs() < 2.0,
                "Instantaneous frequency should be ~{} Hz, got {} at sample {}",
                frequency,
                freq,
                i
            );
        }
    }

    #[test]
    fn test_phase_and_amplitude() {
        let sample_rate = 1000.0;
        let num_samples = 256;

        let hilbert = HilbertTransform::new(num_samples);
        let sine = sine_wave(10.0, sample_rate, num_samples);

        // Both methods should give the same result
        let phase1 = hilbert.instantaneous_phase(&sine);
        let amp1 = hilbert.instantaneous_amplitude(&sine);
        let (phase2, amp2) = hilbert.phase_and_amplitude(&sine);

        for i in 0..num_samples {
            assert!(
                (phase1[i] - phase2[i]).abs() < 1e-6,
                "Phase mismatch at {}",
                i
            );
            assert!(
                (amp1[i] - amp2[i]).abs() < 1e-6,
                "Amplitude mismatch at {}",
                i
            );
        }
    }

    #[test]
    fn test_unwrap_phase() {
        // Create phase with artificial discontinuities
        let mut phase = Vec::new();
        for i in 0..100 {
            let p = (i as f32 * 0.1) % (2.0 * PI) - PI;
            phase.push(p);
        }

        let unwrapped = HilbertTransform::unwrap_phase(&phase);

        // Unwrapped phase should have no large jumps
        for i in 1..unwrapped.len() {
            let delta = (unwrapped[i] - unwrapped[i - 1]).abs();
            assert!(
                delta < PI,
                "Unwrapped phase should have no jumps > PI, got {} at {}",
                delta,
                i
            );
        }
    }

    #[test]
    fn test_amplitude_modulated_signal() {
        // Test on an amplitude-modulated signal
        // Carrier: 50 Hz, modulator: 5 Hz
        let sample_rate = 1000.0;
        let num_samples = 1024;
        let carrier_freq = 50.0;
        let mod_freq = 5.0;

        let signal: Vec<f32> = (0..num_samples)
            .map(|i| {
                let t = i as f32 / sample_rate;
                let modulator = 0.5 + 0.5 * (2.0 * PI * mod_freq * t).cos();
                modulator * (2.0 * PI * carrier_freq * t).sin()
            })
            .collect();

        let hilbert = HilbertTransform::new(num_samples);
        let amplitude = hilbert.instantaneous_amplitude(&signal);

        // The amplitude envelope should vary with the modulator frequency
        // Check that envelope varies over time (not constant)
        let start = num_samples / 4;
        let end = 3 * num_samples / 4;

        let min_amp = amplitude[start..end]
            .iter()
            .copied()
            .fold(f32::INFINITY, f32::min);
        let max_amp = amplitude[start..end]
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);

        // Envelope should vary between ~0.5 and ~1.0 (allowing some error)
        assert!(
            min_amp < 0.7,
            "Min amplitude should be < 0.7, got {}",
            min_amp
        );
        assert!(
            max_amp > 0.8,
            "Max amplitude should be > 0.8, got {}",
            max_amp
        );
    }

    #[test]
    fn test_empty_signal() {
        let hilbert = HilbertTransform::new(256);

        let analytic = hilbert.analytic_signal(&[]);
        assert_eq!(analytic, [] as [rustfft::num_complex::Complex<f32>; 0]);

        let phase = hilbert.instantaneous_phase(&[]);
        assert_eq!(phase, [] as [f32; 0]);

        let amplitude = hilbert.instantaneous_amplitude(&[]);
        assert_eq!(amplitude, [] as [f32; 0]);

        let unwrapped = HilbertTransform::unwrap_phase(&[]);
        assert_eq!(unwrapped, [] as [f32; 0]);
    }
}
