//! Transfer function measurement

use crate::fft::{Fft, Window};
use rustfft::num_complex::Complex;

/// Transfer function measurement result
pub struct TransferFunction {
    /// Frequency bins (Hz)
    pub frequencies: Vec<f32>,
    /// Magnitude response (dB)
    pub magnitude_db: Vec<f32>,
    /// Phase response (radians)
    pub phase_rad: Vec<f32>,
    /// Coherence (0-1, measure of linearity)
    pub coherence: Vec<f32>,
}

impl TransferFunction {
    /// Measure transfer function using cross-spectral method
    ///
    /// # Arguments
    /// * `input` - Input signal (reference)
    /// * `output` - Output signal (system response)
    /// * `sample_rate` - Sample rate in Hz
    /// * `fft_size` - FFT size (determines frequency resolution)
    /// * `overlap` - Overlap ratio (0.0 to 0.9, typical 0.5)
    pub fn measure(
        input: &[f32],
        output: &[f32],
        sample_rate: f32,
        fft_size: usize,
        overlap: f32,
    ) -> Self {
        let hop_size = ((1.0 - overlap) * fft_size as f32) as usize;
        let num_frames = (input.len().min(output.len()) - fft_size) / hop_size + 1;

        let fft = Fft::new(fft_size);
        let window = Window::Hann.coefficients(fft_size);

        // Accumulate cross-spectral density and power spectral densities
        let spectrum_size = fft_size / 2 + 1;
        let mut pxx = vec![0.0f32; spectrum_size]; // Input PSD
        let mut pyy = vec![0.0f32; spectrum_size]; // Output PSD
        let mut pxy = vec![Complex::new(0.0, 0.0); spectrum_size]; // Cross PSD

        for frame_idx in 0..num_frames {
            let start = frame_idx * hop_size;

            // Window and FFT input
            let mut x_windowed: Vec<Complex<f32>> = input[start..start + fft_size]
                .iter()
                .zip(window.iter())
                .map(|(&s, &w)| Complex::new(s * w, 0.0))
                .collect();
            fft.forward_complex(&mut x_windowed);

            // Window and FFT output
            let mut y_windowed: Vec<Complex<f32>> = output[start..start + fft_size]
                .iter()
                .zip(window.iter())
                .map(|(&s, &w)| Complex::new(s * w, 0.0))
                .collect();
            fft.forward_complex(&mut y_windowed);

            // Accumulate spectra
            for i in 0..spectrum_size {
                let x = x_windowed[i];
                let y = y_windowed[i];

                pxx[i] += x.norm_sqr();
                pyy[i] += y.norm_sqr();
                pxy[i] += y * x.conj();
            }
        }

        // Compute transfer function H = Pxy / Pxx
        let mut magnitude_db = Vec::with_capacity(spectrum_size);
        let mut phase_rad = Vec::with_capacity(spectrum_size);
        let mut coherence = Vec::with_capacity(spectrum_size);
        let mut frequencies = Vec::with_capacity(spectrum_size);

        let freq_resolution = sample_rate / fft_size as f32;

        for i in 0..spectrum_size {
            frequencies.push(i as f32 * freq_resolution);

            if pxx[i] > 1e-10 {
                let h = pxy[i] / pxx[i];
                magnitude_db.push(20.0 * h.norm().log10());
                phase_rad.push(h.arg());

                // Coherence = |Pxy|^2 / (Pxx * Pyy)
                let coh = pxy[i].norm_sqr() / (pxx[i] * pyy[i]).max(1e-10);
                coherence.push(coh.min(1.0));
            } else {
                magnitude_db.push(-120.0);
                phase_rad.push(0.0);
                coherence.push(0.0);
            }
        }

        Self {
            frequencies,
            magnitude_db,
            phase_rad,
            coherence,
        }
    }

    /// Get magnitude at a specific frequency (interpolated)
    pub fn magnitude_at(&self, freq_hz: f32) -> f32 {
        interpolate(&self.frequencies, &self.magnitude_db, freq_hz)
    }

    /// Get phase at a specific frequency (interpolated)
    pub fn phase_at(&self, freq_hz: f32) -> f32 {
        interpolate(&self.frequencies, &self.phase_rad, freq_hz)
    }

    /// Find -3dB cutoff frequency
    pub fn cutoff_frequency(&self, reference_db: f32) -> Option<f32> {
        let target = reference_db - 3.0;

        for i in 1..self.magnitude_db.len() {
            if self.magnitude_db[i] < target && self.magnitude_db[i - 1] >= target {
                // Linear interpolation
                let t = (target - self.magnitude_db[i - 1])
                    / (self.magnitude_db[i] - self.magnitude_db[i - 1]);
                return Some(
                    self.frequencies[i - 1] + t * (self.frequencies[i] - self.frequencies[i - 1]),
                );
            }
        }
        None
    }
}

impl TransferFunction {
    /// Compute group delay from phase response
    ///
    /// Group delay is the negative derivative of phase with respect to angular frequency.
    /// Returns delay in samples at each frequency bin.
    pub fn group_delay(&self) -> Vec<f32> {
        let unwrapped = unwrap_phase(&self.phase_rad);
        let mut group_delay = Vec::with_capacity(self.frequencies.len());

        if self.frequencies.len() < 2 {
            return vec![0.0; self.frequencies.len()];
        }

        // First point: forward difference
        let df = self.frequencies[1] - self.frequencies[0];
        if df > 0.0 {
            let d_phase = unwrapped[1] - unwrapped[0];
            let d_omega = 2.0 * std::f32::consts::PI * df;
            group_delay.push(-d_phase / d_omega);
        } else {
            group_delay.push(0.0);
        }

        // Middle points: central difference
        for i in 1..self.frequencies.len() - 1 {
            let df = self.frequencies[i + 1] - self.frequencies[i - 1];
            if df > 0.0 {
                let d_phase = unwrapped[i + 1] - unwrapped[i - 1];
                let d_omega = 2.0 * std::f32::consts::PI * df;
                group_delay.push(-d_phase / d_omega);
            } else {
                group_delay.push(0.0);
            }
        }

        // Last point: backward difference
        let n = self.frequencies.len();
        let df = self.frequencies[n - 1] - self.frequencies[n - 2];
        if df > 0.0 {
            let d_phase = unwrapped[n - 1] - unwrapped[n - 2];
            let d_omega = 2.0 * std::f32::consts::PI * df;
            group_delay.push(-d_phase / d_omega);
        } else {
            group_delay.push(0.0);
        }

        group_delay
    }

    /// Smooth the magnitude response using a moving average
    ///
    /// # Arguments
    /// * `window_size` - Number of bins to average (must be odd, will be made odd if even)
    pub fn smooth(&self, window_size: usize) -> TransferFunction {
        let window = if window_size.is_multiple_of(2) {
            window_size + 1
        } else {
            window_size
        };

        let smoothed_mag = smooth_data(&self.magnitude_db, window);
        let smoothed_phase = smooth_data(&self.phase_rad, window);

        TransferFunction {
            frequencies: self.frequencies.clone(),
            magnitude_db: smoothed_mag,
            phase_rad: smoothed_phase,
            coherence: self.coherence.clone(),
        }
    }

    /// Find resonance peaks in the magnitude response
    ///
    /// # Arguments
    /// * `min_prominence_db` - Minimum prominence (height above neighbors) in dB
    /// * `min_freq` - Minimum frequency to search
    /// * `max_freq` - Maximum frequency to search
    ///
    /// # Returns
    /// Vector of (frequency_hz, magnitude_db, q_factor) tuples
    pub fn find_resonances(
        &self,
        min_prominence_db: f32,
        min_freq: f32,
        max_freq: f32,
    ) -> Vec<Resonance> {
        let mut resonances = Vec::new();

        if self.magnitude_db.len() < 3 {
            return resonances;
        }

        // Find peaks
        for i in 1..self.magnitude_db.len() - 1 {
            let freq = self.frequencies[i];
            if freq < min_freq || freq > max_freq {
                continue;
            }

            let mag = self.magnitude_db[i];
            let prev = self.magnitude_db[i - 1];
            let next = self.magnitude_db[i + 1];

            // Check if this is a local maximum
            if mag > prev && mag > next {
                // Calculate prominence (height above the higher of the two neighbors)
                let baseline = prev.max(next);
                let prominence = mag - baseline;

                if prominence >= min_prominence_db {
                    // Estimate Q factor from -3dB bandwidth
                    let target_3db = mag - 3.0;
                    let mut lower_freq = freq;
                    let mut upper_freq = freq;

                    // Find lower -3dB point
                    for j in (0..i).rev() {
                        if self.magnitude_db[j] < target_3db {
                            // Interpolate
                            let t = (target_3db - self.magnitude_db[j])
                                / (self.magnitude_db[j + 1] - self.magnitude_db[j]);
                            lower_freq = self.frequencies[j]
                                + t * (self.frequencies[j + 1] - self.frequencies[j]);
                            break;
                        }
                    }

                    // Find upper -3dB point
                    for j in i + 1..self.magnitude_db.len() {
                        if self.magnitude_db[j] < target_3db {
                            // Interpolate
                            let t = (target_3db - self.magnitude_db[j - 1])
                                / (self.magnitude_db[j] - self.magnitude_db[j - 1]);
                            upper_freq = self.frequencies[j - 1]
                                + t * (self.frequencies[j] - self.frequencies[j - 1]);
                            break;
                        }
                    }

                    let bandwidth = upper_freq - lower_freq;
                    let q_factor = if bandwidth > 0.0 {
                        freq / bandwidth
                    } else {
                        f32::INFINITY
                    };

                    resonances.push(Resonance {
                        frequency_hz: freq,
                        magnitude_db: mag,
                        q_factor,
                        bandwidth_hz: bandwidth,
                    });
                }
            }
        }

        resonances
    }
}

/// Resonance peak information
#[derive(Debug, Clone, Copy)]
pub struct Resonance {
    /// Center frequency in Hz
    pub frequency_hz: f32,
    /// Peak magnitude in dB
    pub magnitude_db: f32,
    /// Q factor (center_freq / bandwidth)
    pub q_factor: f32,
    /// -3dB bandwidth in Hz
    pub bandwidth_hz: f32,
}

/// Unwrap phase to remove discontinuities.
///
/// Delegates to [`crate::phase::unwrap_phase`]. Kept for backwards compatibility.
pub fn unwrap_phase(phase: &[f32]) -> Vec<f32> {
    crate::phase::unwrap_phase(phase)
}

/// Smooth data using a moving average filter
fn smooth_data(data: &[f32], window_size: usize) -> Vec<f32> {
    if data.is_empty() || window_size == 0 {
        return data.to_vec();
    }

    let half_window = window_size / 2;
    let mut smoothed = Vec::with_capacity(data.len());

    for i in 0..data.len() {
        let start = i.saturating_sub(half_window);
        let end = (i + half_window + 1).min(data.len());
        let sum: f32 = data[start..end].iter().sum();
        smoothed.push(sum / (end - start) as f32);
    }

    smoothed
}

/// Linear interpolation helper
fn interpolate(x: &[f32], y: &[f32], target_x: f32) -> f32 {
    if x.is_empty() {
        return 0.0;
    }

    if target_x <= x[0] {
        return y[0];
    }

    for i in 1..x.len() {
        if target_x <= x[i] {
            let t = (target_x - x[i - 1]) / (x[i] - x[i - 1]);
            return y[i - 1] + t * (y[i] - y[i - 1]);
        }
    }

    *y.last().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    #[test]
    fn test_transfer_function_passthrough() {
        let sample_rate = 48000.0;
        let duration = 1.0;
        let num_samples = (sample_rate * duration) as usize;

        // Generate test signal
        let input: Vec<f32> = (0..num_samples)
            .map(|i| {
                let t = i as f32 / sample_rate;
                (2.0 * PI * 1000.0 * t).sin() + (2.0 * PI * 5000.0 * t).sin() * 0.5
            })
            .collect();

        // Passthrough system
        let output = input.clone();

        let tf = TransferFunction::measure(&input, &output, sample_rate, 4096, 0.5);

        // Magnitude should be ~0 dB across frequencies
        let avg_mag: f32 = tf.magnitude_db.iter().sum::<f32>() / tf.magnitude_db.len() as f32;
        assert!(
            avg_mag.abs() < 6.0,
            "Passthrough should have ~0dB magnitude, got {}",
            avg_mag
        );
    }

    #[test]
    fn test_interpolate() {
        let x = vec![0.0, 1.0, 2.0];
        let y = vec![0.0, 10.0, 20.0];

        assert_eq!(interpolate(&x, &y, 0.5), 5.0);
        assert_eq!(interpolate(&x, &y, 1.5), 15.0);
    }

    #[test]
    fn test_unwrap_phase() {
        // Phase that wraps from PI to -PI
        let wrapped = vec![0.0, PI * 0.5, PI * 0.9, -PI * 0.9, -PI * 0.5, 0.0];
        let unwrapped = unwrap_phase(&wrapped);

        // After unwrapping, phase should be monotonically increasing
        for i in 1..unwrapped.len() {
            assert!(
                unwrapped[i] >= unwrapped[i - 1] - 0.1,
                "Phase should be continuous: {} vs {}",
                unwrapped[i - 1],
                unwrapped[i]
            );
        }
    }

    #[test]
    fn test_unwrap_phase_empty() {
        let empty: Vec<f32> = vec![];
        let unwrapped = unwrap_phase(&empty);
        assert_eq!(unwrapped, [] as [f32; 0]);
    }

    #[test]
    fn test_smooth_data() {
        let data = vec![0.0, 0.0, 10.0, 0.0, 0.0];
        let smoothed = smooth_data(&data, 3);

        // Center should be reduced
        assert!(smoothed[2] < 10.0);
        // Neighbors should be increased
        assert!(smoothed[1] > 0.0);
        assert!(smoothed[3] > 0.0);
    }

    #[test]
    fn test_group_delay_passthrough() {
        let sample_rate = 48000.0;
        let duration = 0.5;
        let num_samples = (sample_rate * duration) as usize;

        // Generate noise-like test signal
        let input: Vec<f32> = (0..num_samples)
            .map(|i| {
                let t = i as f32 / sample_rate;
                (2.0 * PI * 100.0 * t).sin()
                    + (2.0 * PI * 500.0 * t).sin() * 0.7
                    + (2.0 * PI * 2000.0 * t).sin() * 0.5
            })
            .collect();

        // Passthrough (no delay)
        let output = input.clone();
        let tf = TransferFunction::measure(&input, &output, sample_rate, 2048, 0.5);

        let gd = tf.group_delay();
        assert_eq!(gd.len(), tf.frequencies.len());

        // For passthrough, group delay should be near zero (within measurement noise)
        let avg_gd: f32 = gd.iter().sum::<f32>() / gd.len() as f32;
        assert!(
            avg_gd.abs() < 10.0,
            "Passthrough group delay should be near zero, got {}",
            avg_gd
        );
    }

    #[test]
    fn test_find_resonances() {
        // Create a transfer function with a clear resonance
        // Use a narrower peak to get higher prominence
        let tf = TransferFunction {
            frequencies: (0..100).map(|i| i as f32 * 100.0).collect(),
            magnitude_db: (0..100)
                .map(|i| {
                    // Sharp peak at 5000 Hz (index 50)
                    let x = (i as f32 - 50.0) / 2.0; // Narrower peak
                    -20.0 + 30.0 * (-x * x).exp()
                })
                .collect(),
            phase_rad: vec![0.0; 100],
            coherence: vec![1.0; 100],
        };

        // Use lower prominence threshold for this test
        let resonances = tf.find_resonances(1.0, 1000.0, 9000.0);
        assert!(!resonances.is_empty(), "Should find at least one resonance");

        let peak = &resonances[0];
        assert!(
            (peak.frequency_hz - 5000.0).abs() < 200.0,
            "Resonance should be near 5000 Hz, got {}",
            peak.frequency_hz
        );
        assert!(peak.magnitude_db > 0.0, "Peak should be above 0 dB");
    }

    #[test]
    fn test_smooth_transfer_function() {
        let tf = TransferFunction {
            frequencies: vec![100.0, 200.0, 300.0, 400.0, 500.0],
            magnitude_db: vec![0.0, 0.0, 20.0, 0.0, 0.0],
            phase_rad: vec![0.0, 0.0, 1.0, 0.0, 0.0],
            coherence: vec![1.0; 5],
        };

        let smoothed = tf.smooth(3);

        // Smoothed peak should be lower
        assert!(smoothed.magnitude_db[2] < 20.0);
        // Neighbors should be raised
        assert!(smoothed.magnitude_db[1] > 0.0);
    }
}
