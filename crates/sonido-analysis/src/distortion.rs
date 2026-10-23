//! Distortion analysis tools (THD, THD+N, IMD)
//!
//! Provides tools for measuring harmonic distortion in audio signals,
//! useful for characterizing nonlinear effects like distortion pedals.

use crate::fft::{Fft, Window};
use crate::spectrum::find_peaks;
use std::f32::consts::PI;

/// Result of THD analysis
#[derive(Debug, Clone)]
pub struct ThdResult {
    /// Fundamental frequency detected (Hz)
    pub fundamental_freq: f32,
    /// Fundamental amplitude (linear)
    pub fundamental_amplitude: f32,
    /// THD as a ratio (0.0 to 1.0+)
    pub thd_ratio: f32,
    /// THD in dB
    pub thd_db: f32,
    /// THD+N as a ratio
    pub thd_n_ratio: f32,
    /// THD+N in dB
    pub thd_n_db: f32,
    /// Individual harmonic amplitudes (fundamental, 2nd, 3rd, ...)
    pub harmonics: Vec<f32>,
    /// Noise floor estimate (linear RMS)
    pub noise_floor: f32,
}

/// THD analyzer for measuring harmonic distortion
pub struct ThdAnalyzer {
    sample_rate: f32,
    fft_size: usize,
    window: Window,
    max_harmonics: usize,
}

impl ThdAnalyzer {
    /// Create a new THD analyzer
    ///
    /// # Arguments
    /// * `sample_rate` - Sample rate in Hz
    /// * `fft_size` - FFT size (power of 2 recommended)
    pub fn new(sample_rate: f32, fft_size: usize) -> Self {
        Self {
            sample_rate,
            fft_size,
            window: Window::BlackmanHarris, // Best for THD measurement
            max_harmonics: 10,
        }
    }

    /// Set the window function
    pub fn with_window(mut self, window: Window) -> Self {
        self.window = window;
        self
    }

    /// Set maximum number of harmonics to analyze
    pub fn with_max_harmonics(mut self, max: usize) -> Self {
        self.max_harmonics = max;
        self
    }

    /// Analyze THD of a signal with known fundamental frequency
    ///
    /// # Arguments
    /// * `signal` - Input signal (should be at least `fft_size` samples)
    /// * `fundamental_freq` - Known fundamental frequency in Hz
    pub fn analyze(&self, signal: &[f32], fundamental_freq: f32) -> ThdResult {
        let fft = Fft::new(self.fft_size);
        let bin_width = self.sample_rate / self.fft_size as f32;

        // Window and FFT
        let mut windowed = signal.to_vec();
        windowed.resize(self.fft_size, 0.0);
        self.window.apply(&mut windowed);

        let spectrum = fft.forward(&windowed);
        let magnitudes: Vec<f32> = spectrum.iter().map(|c| c.norm()).collect();

        // Find harmonic amplitudes using parabolic interpolation
        let mut harmonics = Vec::with_capacity(self.max_harmonics);
        let nyquist = self.sample_rate / 2.0;

        for h in 1..=self.max_harmonics {
            let harmonic_freq = fundamental_freq * h as f32;
            if harmonic_freq >= nyquist {
                break;
            }

            let amplitude = self.measure_harmonic(&magnitudes, harmonic_freq, bin_width);
            harmonics.push(amplitude);
        }

        if harmonics.is_empty() {
            return ThdResult {
                fundamental_freq,
                fundamental_amplitude: 0.0,
                thd_ratio: 0.0,
                thd_db: f32::NEG_INFINITY,
                thd_n_ratio: 0.0,
                thd_n_db: f32::NEG_INFINITY,
                harmonics: vec![],
                noise_floor: 0.0,
            };
        }

        let fundamental_amplitude = harmonics[0];

        // Calculate THD (harmonics only)
        let harmonic_power: f32 = harmonics[1..].iter().map(|h| h * h).sum();
        let thd_ratio = harmonic_power.sqrt() / fundamental_amplitude.max(1e-10);
        let thd_db = 20.0 * thd_ratio.max(1e-10).log10();

        // Estimate noise floor (exclude harmonic bins)
        let noise_floor = self.estimate_noise_floor(&magnitudes, fundamental_freq, bin_width);

        // Calculate THD+N
        let total_power: f32 = magnitudes.iter().map(|m| m * m).sum();
        let signal_power = fundamental_amplitude * fundamental_amplitude;
        let noise_plus_distortion = (total_power - signal_power).max(0.0).sqrt();
        let thd_n_ratio = noise_plus_distortion / fundamental_amplitude.max(1e-10);
        let thd_n_db = 20.0 * thd_n_ratio.max(1e-10).log10();

        ThdResult {
            fundamental_freq,
            fundamental_amplitude,
            thd_ratio,
            thd_db,
            thd_n_ratio,
            thd_n_db,
            harmonics,
            noise_floor,
        }
    }

    /// Analyze THD with automatic fundamental detection
    ///
    /// Uses peak detection to find the fundamental frequency
    pub fn analyze_auto(&self, signal: &[f32]) -> ThdResult {
        let fft = Fft::new(self.fft_size);

        // Window and FFT
        let mut windowed = signal.to_vec();
        windowed.resize(self.fft_size, 0.0);
        self.window.apply(&mut windowed);

        let spectrum = fft.forward(&windowed);
        let magnitudes: Vec<f32> = spectrum.iter().map(|c| c.norm()).collect();

        // Find strongest peak as fundamental
        let peaks = find_peaks(&magnitudes, self.sample_rate, -60.0, 20.0);

        if peaks.is_empty() {
            return ThdResult {
                fundamental_freq: 0.0,
                fundamental_amplitude: 0.0,
                thd_ratio: 0.0,
                thd_db: f32::NEG_INFINITY,
                thd_n_ratio: 0.0,
                thd_n_db: f32::NEG_INFINITY,
                harmonics: vec![],
                noise_floor: 0.0,
            };
        }

        let fundamental_freq = peaks[0].0;
        self.analyze(signal, fundamental_freq)
    }

    /// Measure amplitude at a specific frequency using parabolic interpolation
    fn measure_harmonic(&self, magnitudes: &[f32], freq: f32, bin_width: f32) -> f32 {
        let bin = freq / bin_width;
        let bin_idx = bin.round() as usize;

        if bin_idx == 0 || bin_idx >= magnitudes.len() - 1 {
            return magnitudes.get(bin_idx).copied().unwrap_or(0.0);
        }

        // Parabolic interpolation for better frequency resolution
        let alpha = magnitudes[bin_idx - 1];
        let beta = magnitudes[bin_idx];
        let gamma = magnitudes[bin_idx + 1];

        // Peak offset (between -0.5 and 0.5)
        let denom = alpha - 2.0 * beta + gamma;
        if denom.abs() < 1e-10 {
            return beta;
        }

        let _p = 0.5 * (alpha - gamma) / denom;

        // Interpolated peak magnitude
        beta - 0.25 * (alpha - gamma) * _p
    }

    /// Estimate noise floor excluding harmonic bins
    fn estimate_noise_floor(&self, magnitudes: &[f32], fundamental: f32, bin_width: f32) -> f32 {
        let mut noise_bins = Vec::new();
        let exclusion_width = 3; // Bins to exclude around each harmonic

        for (i, &mag) in magnitudes.iter().enumerate() {
            let freq = i as f32 * bin_width;
            let mut is_harmonic = false;

            // Check if this bin is near any harmonic
            for h in 1..=self.max_harmonics {
                let harmonic_freq = fundamental * h as f32;
                let harmonic_bin = (harmonic_freq / bin_width).round() as i32;
                if (i as i32 - harmonic_bin).abs() <= exclusion_width {
                    is_harmonic = true;
                    break;
                }
            }

            if !is_harmonic && freq > 20.0 {
                // Exclude DC region
                noise_bins.push(mag);
            }
        }

        if noise_bins.is_empty() {
            return 0.0;
        }

        // RMS of noise bins

        (noise_bins.iter().map(|m| m * m).sum::<f32>() / noise_bins.len() as f32).sqrt()
    }
}

/// Generate a test tone for THD measurement
///
/// # Arguments
/// * `sample_rate` - Sample rate in Hz
/// * `frequency` - Tone frequency in Hz
/// * `duration_secs` - Duration in seconds
/// * `amplitude` - Peak amplitude (0.0 to 1.0)
pub fn generate_test_tone(
    sample_rate: f32,
    frequency: f32,
    duration_secs: f32,
    amplitude: f32,
) -> Vec<f32> {
    let num_samples = (duration_secs * sample_rate) as usize;
    (0..num_samples)
        .map(|i| {
            let t = i as f32 / sample_rate;
            amplitude * (2.0 * PI * frequency * t).sin()
        })
        .collect()
}

/// Intermodulation distortion (IMD) analyzer
///
/// Uses two-tone test to measure IMD products
pub struct ImdAnalyzer {
    sample_rate: f32,
    fft_size: usize,
    window: Window,
}

impl ImdAnalyzer {
    /// Create a new IMD analyzer
    pub fn new(sample_rate: f32, fft_size: usize) -> Self {
        Self {
            sample_rate,
            fft_size,
            window: Window::BlackmanHarris,
        }
    }

    /// Generate a two-tone test signal
    ///
    /// # Arguments
    /// * `freq1` - First tone frequency (Hz)
    /// * `freq2` - Second tone frequency (Hz)
    /// * `duration_secs` - Duration in seconds
    /// * `amplitude` - Peak amplitude per tone
    pub fn generate_two_tone(
        &self,
        freq1: f32,
        freq2: f32,
        duration_secs: f32,
        amplitude: f32,
    ) -> Vec<f32> {
        let num_samples = (duration_secs * self.sample_rate) as usize;
        (0..num_samples)
            .map(|i| {
                let t = i as f32 / self.sample_rate;
                amplitude * (2.0 * PI * freq1 * t).sin() + amplitude * (2.0 * PI * freq2 * t).sin()
            })
            .collect()
    }

    /// Analyze IMD from a two-tone response
    ///
    /// Returns IMD ratio and individual product levels
    pub fn analyze(&self, signal: &[f32], freq1: f32, freq2: f32) -> ImdResult {
        let fft = Fft::new(self.fft_size);
        let bin_width = self.sample_rate / self.fft_size as f32;

        let mut windowed = signal.to_vec();
        windowed.resize(self.fft_size, 0.0);
        self.window.apply(&mut windowed);

        let spectrum = fft.forward(&windowed);
        let magnitudes: Vec<f32> = spectrum.iter().map(|c| c.norm()).collect();

        // Measure fundamental tones
        let amp1 = self.measure_at_freq(&magnitudes, freq1, bin_width);
        let amp2 = self.measure_at_freq(&magnitudes, freq2, bin_width);

        // Calculate IMD product frequencies
        let diff = (freq2 - freq1).abs();
        let sum = freq1 + freq2;

        // Second-order products: f2-f1, f1+f2
        let imd2_diff = self.measure_at_freq(&magnitudes, diff, bin_width);
        let imd2_sum = self.measure_at_freq(&magnitudes, sum, bin_width);

        // Third-order products: 2f1-f2, 2f2-f1
        let imd3_low = self.measure_at_freq(&magnitudes, 2.0 * freq1 - freq2, bin_width);
        let imd3_high = self.measure_at_freq(&magnitudes, 2.0 * freq2 - freq1, bin_width);

        // Calculate IMD ratio (SMPTE method: ratio of sum of products to fundamentals)
        let fundamental_power = amp1 * amp1 + amp2 * amp2;
        let imd_power = imd2_diff * imd2_diff
            + imd2_sum * imd2_sum
            + imd3_low * imd3_low
            + imd3_high * imd3_high;

        let imd_ratio = imd_power.sqrt() / fundamental_power.sqrt().max(1e-10);
        let imd_db = 20.0 * imd_ratio.max(1e-10).log10();

        ImdResult {
            freq1,
            freq2,
            amp1,
            amp2,
            imd2_diff,
            imd2_sum,
            imd3_low,
            imd3_high,
            imd_ratio,
            imd_db,
        }
    }

    /// Analyze IMD with automatic tone detection.
    ///
    /// Finds the two strongest spectral peaks and measures IMD products
    /// between them. Returns `None` if fewer than two peaks are found.
    pub fn analyze_auto(&self, signal: &[f32]) -> Option<ImdResult> {
        let fft = Fft::new(self.fft_size);

        let mut windowed = signal.to_vec();
        windowed.resize(self.fft_size, 0.0);
        self.window.apply(&mut windowed);

        let spectrum = fft.forward(&windowed);
        let magnitudes: Vec<f32> = spectrum.iter().map(|c| c.norm()).collect();

        // find_peaks returns Vec<(freq_hz, magnitude_db)> sorted by magnitude descending
        let peaks = find_peaks(&magnitudes, self.sample_rate, -60.0, 20.0);

        if peaks.len() < 2 {
            return None;
        }

        let (f1, f2) = if peaks[0].0 < peaks[1].0 {
            (peaks[0].0, peaks[1].0)
        } else {
            (peaks[1].0, peaks[0].0)
        };

        Some(self.analyze(signal, f1, f2))
    }

    fn measure_at_freq(&self, magnitudes: &[f32], freq: f32, bin_width: f32) -> f32 {
        if freq <= 0.0 || freq >= self.sample_rate / 2.0 {
            return 0.0;
        }

        let bin_idx = (freq / bin_width).round() as usize;
        if bin_idx >= magnitudes.len() {
            return 0.0;
        }

        magnitudes[bin_idx]
    }
}

/// Result of IMD analysis
#[derive(Debug, Clone)]
pub struct ImdResult {
    /// First tone frequency (Hz)
    pub freq1: f32,
    /// Second tone frequency (Hz)
    pub freq2: f32,
    /// First tone amplitude
    pub amp1: f32,
    /// Second tone amplitude
    pub amp2: f32,
    /// Second-order difference product (f2-f1)
    pub imd2_diff: f32,
    /// Second-order sum product (f1+f2)
    pub imd2_sum: f32,
    /// Third-order low product (2f1-f2)
    pub imd3_low: f32,
    /// Third-order high product (2f2-f1)
    pub imd3_high: f32,
    /// IMD ratio
    pub imd_ratio: f32,
    /// IMD in dB
    pub imd_db: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pure_tone_low_thd() {
        let sample_rate = 48000.0;
        let freq = 1000.0;
        let signal = generate_test_tone(sample_rate, freq, 0.5, 0.5);

        let analyzer = ThdAnalyzer::new(sample_rate, 8192);
        let result = analyzer.analyze(&signal, freq);

        // Pure sine should have very low THD
        assert!(
            result.thd_ratio < 0.01,
            "Pure sine THD should be < 1%, got {}%",
            result.thd_ratio * 100.0
        );
    }

    #[test]
    fn test_clipped_signal_high_thd() {
        let sample_rate = 48000.0;
        let freq = 1000.0;

        // Generate and clip signal
        let mut signal = generate_test_tone(sample_rate, freq, 0.5, 1.0);
        for sample in signal.iter_mut() {
            *sample = sample.clamp(-0.5, 0.5); // Hard clip at 50%
        }

        let analyzer = ThdAnalyzer::new(sample_rate, 8192);
        let result = analyzer.analyze(&signal, freq);

        // Clipped signal should have significant THD
        assert!(
            result.thd_ratio > 0.1,
            "Clipped signal should have high THD, got {}%",
            result.thd_ratio * 100.0
        );
    }

    #[test]
    fn test_auto_detect_fundamental() {
        let sample_rate = 48000.0;
        let freq = 440.0;
        let signal = generate_test_tone(sample_rate, freq, 0.5, 0.5);

        let analyzer = ThdAnalyzer::new(sample_rate, 8192);
        let result = analyzer.analyze_auto(&signal);

        // Should detect fundamental within 5 Hz
        assert!(
            (result.fundamental_freq - freq).abs() < 5.0,
            "Detected {} Hz, expected {} Hz",
            result.fundamental_freq,
            freq
        );
    }

    #[test]
    fn test_imd_pure_tones() {
        let sample_rate = 48000.0;
        let analyzer = ImdAnalyzer::new(sample_rate, 8192);

        // Generate two-tone signal (no distortion)
        let signal = analyzer.generate_two_tone(1000.0, 1100.0, 0.5, 0.25);

        let result = analyzer.analyze(&signal, 1000.0, 1100.0);

        // Pure tones should have low IMD
        assert!(
            result.imd_ratio < 0.05,
            "Pure two-tone IMD should be low, got {}%",
            result.imd_ratio * 100.0
        );
    }

    #[test]
    fn test_harmonic_count() {
        let sample_rate = 48000.0;
        let freq = 500.0;
        let signal = generate_test_tone(sample_rate, freq, 0.5, 0.5);

        let analyzer = ThdAnalyzer::new(sample_rate, 8192).with_max_harmonics(5);
        let result = analyzer.analyze(&signal, freq);

        // Should have up to 5 harmonics (fundamental + 4)
        assert!(result.harmonics.len() <= 5);
        assert_ne!(result.harmonics, [] as [f32; 0]);
    }

    #[test]
    fn test_imd_auto_detect() {
        let sample_rate = 48000.0;
        let analyzer = ImdAnalyzer::new(sample_rate, 8192);

        // Generate two-tone signal
        let signal = analyzer.generate_two_tone(1000.0, 1100.0, 0.5, 0.25);

        let result = analyzer.analyze_auto(&signal);
        assert!(result.is_some(), "Should detect two tones");

        let result = result.unwrap();
        // Detected frequencies should be close to the originals
        assert!(
            (result.freq1 - 1000.0).abs() < 10.0 || (result.freq2 - 1000.0).abs() < 10.0,
            "Should detect 1000 Hz tone, got {} and {}",
            result.freq1,
            result.freq2
        );
    }
}
