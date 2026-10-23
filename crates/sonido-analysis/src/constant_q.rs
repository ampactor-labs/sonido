//! Constant-Q Transform (CQT) for pitch-based frequency analysis
//!
//! The CQT provides logarithmically-spaced frequency bins, making it ideal for
//! musical analysis where each octave has the same number of bins.

use crate::fft::Fft;
use rustfft::num_complex::Complex;
use std::f32::consts::PI;

/// Constant-Q Transform analyzer
///
/// Provides logarithmically-spaced frequency analysis, ideal for music
/// where each octave should have equal resolution.
pub struct ConstantQTransform {
    /// Sample rate
    sample_rate: f32,
    /// Minimum frequency (Hz)
    min_freq: f32,
    /// Maximum frequency (Hz)
    max_freq: f32,
    /// Bins per octave
    bins_per_octave: usize,
    /// Total number of frequency bins
    num_bins: usize,
    /// Center frequencies for each bin
    center_freqs: Vec<f32>,
    /// Kernel for each bin (sparse representation)
    kernels: Vec<CqKernel>,
    /// FFT processor
    fft: Fft,
    /// FFT size (smallest power of 2 that fits longest kernel)
    fft_size: usize,
}

/// Sparse kernel for a single CQ bin
struct CqKernel {
    /// Complex coefficients
    coefficients: Vec<Complex<f32>>,
    /// Length of kernel
    length: usize,
}

/// CQT result
#[derive(Debug, Clone)]
pub struct CqtResult {
    /// Magnitude for each frequency bin
    pub magnitudes: Vec<f32>,
    /// Phase for each frequency bin (radians)
    pub phases: Vec<f32>,
    /// Center frequencies (Hz)
    pub frequencies: Vec<f32>,
    /// Bins per octave
    pub bins_per_octave: usize,
}

impl CqtResult {
    /// Get magnitude in dB
    pub fn magnitude_db(&self) -> Vec<f32> {
        self.magnitudes
            .iter()
            .map(|&m| 20.0 * m.max(1e-10).log10())
            .collect()
    }

    /// Get MIDI note number for each bin (A4 = 69)
    pub fn midi_notes(&self) -> Vec<f32> {
        self.frequencies
            .iter()
            .map(|&f| 69.0 + 12.0 * (f / 440.0).log2())
            .collect()
    }

    /// Find the bin with maximum magnitude
    pub fn peak_bin(&self) -> Option<usize> {
        self.magnitudes
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
            .map(|(i, _)| i)
    }

    /// Get the peak frequency
    pub fn peak_frequency(&self) -> Option<f32> {
        self.peak_bin().map(|i| self.frequencies[i])
    }
}

impl ConstantQTransform {
    /// Create a new CQT analyzer
    ///
    /// # Arguments
    /// * `sample_rate` - Sample rate in Hz
    /// * `min_freq` - Minimum frequency (Hz), typically ~32 Hz (C1)
    /// * `max_freq` - Maximum frequency (Hz), typically < Nyquist
    /// * `bins_per_octave` - Bins per octave (12 for semitone resolution, 24 for quarter-tone)
    pub fn new(sample_rate: f32, min_freq: f32, max_freq: f32, bins_per_octave: usize) -> Self {
        // Calculate number of bins
        let num_octaves = (max_freq / min_freq).log2();
        let num_bins = (num_octaves * bins_per_octave as f32).ceil() as usize;

        // Q factor (quality factor)
        let q = 1.0 / (2.0_f32.powf(1.0 / bins_per_octave as f32) - 1.0);

        // Calculate center frequencies
        let center_freqs: Vec<f32> = (0..num_bins)
            .map(|k| min_freq * 2.0_f32.powf(k as f32 / bins_per_octave as f32))
            .collect();

        // Calculate kernel lengths for each bin
        let kernel_lengths: Vec<usize> = center_freqs
            .iter()
            .map(|&f| ((q * sample_rate / f).ceil() as usize).max(1))
            .collect();

        // FFT size is smallest power of 2 that fits longest kernel
        let max_kernel_len = kernel_lengths.iter().max().copied().unwrap_or(1);
        let fft_size = max_kernel_len.next_power_of_two();
        let fft = Fft::new(fft_size);

        // Pre-compute kernels (windowed complex exponentials)
        let kernels: Vec<CqKernel> = center_freqs
            .iter()
            .zip(kernel_lengths.iter())
            .map(|(&freq, &length)| {
                let mut coefficients = Vec::with_capacity(length);

                for n in 0..length {
                    // Hann window
                    let window = 0.5 * (1.0 - (2.0 * PI * n as f32 / length as f32).cos());
                    // Complex exponential at center frequency
                    let phase = 2.0 * PI * freq * n as f32 / sample_rate;
                    let coeff =
                        Complex::new(window * phase.cos(), window * (-phase).sin()) / length as f32;
                    coefficients.push(coeff);
                }

                CqKernel {
                    coefficients,
                    length,
                }
            })
            .collect();

        Self {
            sample_rate,
            min_freq,
            max_freq,
            bins_per_octave,
            num_bins,
            center_freqs,
            kernels,
            fft,
            fft_size,
        }
    }

    /// Create CQT with musical defaults (C1 to C8, 12 bins per octave)
    pub fn musical(sample_rate: f32) -> Self {
        // C1 to just below Nyquist
        let min_freq = 32.7; // C1
        let max_freq = (sample_rate / 2.0).min(4186.0); // C8 or Nyquist
        Self::new(sample_rate, min_freq, max_freq, 12)
    }

    /// Create CQT with high resolution (24 bins per octave)
    pub fn high_resolution(sample_rate: f32, min_freq: f32, max_freq: f32) -> Self {
        Self::new(sample_rate, min_freq, max_freq, 24)
    }

    /// Analyze a signal segment
    ///
    /// The signal should be at least as long as the longest kernel
    pub fn analyze(&self, signal: &[f32]) -> CqtResult {
        let mut magnitudes = Vec::with_capacity(self.num_bins);
        let mut phases = Vec::with_capacity(self.num_bins);

        for kernel in &self.kernels {
            // Direct convolution with kernel
            let mut sum = Complex::new(0.0, 0.0);

            if signal.len() >= kernel.length {
                for (n, &coeff) in kernel.coefficients.iter().enumerate() {
                    sum += Complex::new(signal[n], 0.0) * coeff;
                }
            }

            magnitudes.push(sum.norm());
            phases.push(sum.arg());
        }

        CqtResult {
            magnitudes,
            phases,
            frequencies: self.center_freqs.clone(),
            bins_per_octave: self.bins_per_octave,
        }
    }

    /// Analyze using FFT-based method (faster for long signals)
    pub fn analyze_fft(&self, signal: &[f32]) -> CqtResult {
        // Zero-pad signal to FFT size
        let mut padded: Vec<Complex<f32>> = signal
            .iter()
            .take(self.fft_size)
            .map(|&x| Complex::new(x, 0.0))
            .collect();
        padded.resize(self.fft_size, Complex::new(0.0, 0.0));

        // FFT of signal
        self.fft.forward_complex(&mut padded);

        let mut magnitudes = Vec::with_capacity(self.num_bins);
        let mut phases = Vec::with_capacity(self.num_bins);

        for (k, &freq) in self.center_freqs.iter().enumerate() {
            // Get the bin corresponding to this center frequency
            let bin = (freq * self.fft_size as f32 / self.sample_rate).round() as usize;

            if bin < padded.len() {
                // Use nearby bins for smoother result
                let mut sum = Complex::new(0.0, 0.0);
                let bandwidth =
                    (self.fft_size as f32 / self.kernels[k].length as f32).ceil() as usize;
                let half_bw = bandwidth / 2;

                for &sample in padded
                    .iter()
                    .take((bin + half_bw).min(padded.len() - 1) + 1)
                    .skip(bin.saturating_sub(half_bw))
                {
                    sum += sample;
                }

                magnitudes.push(sum.norm() / bandwidth as f32);
                phases.push(sum.arg());
            } else {
                magnitudes.push(0.0);
                phases.push(0.0);
            }
        }

        CqtResult {
            magnitudes,
            phases,
            frequencies: self.center_freqs.clone(),
            bins_per_octave: self.bins_per_octave,
        }
    }

    /// Get number of frequency bins
    pub fn num_bins(&self) -> usize {
        self.num_bins
    }

    /// Get bins per octave
    pub fn bins_per_octave(&self) -> usize {
        self.bins_per_octave
    }

    /// Get center frequencies
    pub fn frequencies(&self) -> &[f32] {
        &self.center_freqs
    }

    /// Get frequency range
    pub fn frequency_range(&self) -> (f32, f32) {
        (self.min_freq, self.max_freq)
    }

    /// Convert frequency to bin index
    pub fn freq_to_bin(&self, freq: f32) -> usize {
        if freq <= self.min_freq {
            return 0;
        }
        let bin = (self.bins_per_octave as f32 * (freq / self.min_freq).log2()).round() as usize;
        bin.min(self.num_bins - 1)
    }

    /// Convert bin index to frequency
    pub fn bin_to_freq(&self, bin: usize) -> f32 {
        self.center_freqs.get(bin).copied().unwrap_or(0.0)
    }

    /// Convert MIDI note to bin index
    pub fn midi_to_bin(&self, midi_note: f32) -> usize {
        let freq = 440.0 * 2.0_f32.powf((midi_note - 69.0) / 12.0);
        self.freq_to_bin(freq)
    }
}

/// CQT-based spectrogram (chromagram-like)
#[derive(Debug, Clone)]
pub struct CqtSpectrogram {
    /// CQT data `[time_frame][frequency_bin]`
    pub data: Vec<CqtResult>,
    /// Hop size in samples
    pub hop_size: usize,
    /// Sample rate
    pub sample_rate: f32,
}

impl CqtSpectrogram {
    /// Compute CQT spectrogram from signal
    pub fn from_signal(signal: &[f32], cqt: &ConstantQTransform, hop_size: usize) -> Self {
        let num_frames = if signal.len() >= cqt.fft_size {
            (signal.len() - cqt.fft_size) / hop_size + 1
        } else {
            0
        };

        let mut data = Vec::with_capacity(num_frames);

        for frame_idx in 0..num_frames {
            let start = frame_idx * hop_size;
            let end = (start + cqt.fft_size).min(signal.len());
            let frame = &signal[start..end];

            let result = cqt.analyze(frame);
            data.push(result);
        }

        Self {
            data,
            hop_size,
            sample_rate: cqt.sample_rate,
        }
    }

    /// Get number of time frames
    pub fn num_frames(&self) -> usize {
        self.data.len()
    }

    /// Get time in seconds for a frame
    pub fn frame_to_time(&self, frame: usize) -> f32 {
        frame as f32 * self.hop_size as f32 / self.sample_rate
    }

    /// Get magnitude matrix (for visualization)
    pub fn to_magnitude_matrix(&self) -> Vec<Vec<f32>> {
        self.data.iter().map(|r| r.magnitudes.clone()).collect()
    }

    /// Get magnitude matrix in dB
    pub fn to_db_matrix(&self) -> Vec<Vec<f32>> {
        self.data.iter().map(|r| r.magnitude_db()).collect()
    }
}

/// Chromagram (pitch class profile)
///
/// Folds CQT bins into 12 pitch classes (C, C#, D, ..., B)
pub struct Chromagram {
    /// Chroma vectors [time_frame][pitch_class (0-11)]
    pub data: Vec<[f32; 12]>,
    /// Hop size
    pub hop_size: usize,
    /// Sample rate
    pub sample_rate: f32,
}

impl Chromagram {
    /// Compute chromagram from CQT spectrogram
    pub fn from_cqt_spectrogram(cqt_spec: &CqtSpectrogram, bins_per_octave: usize) -> Self {
        if !bins_per_octave.is_multiple_of(12) {
            // Non-standard bins per octave, use simple folding
            return Self::from_cqt_spectrogram_simple(cqt_spec);
        }

        let bins_per_semitone = bins_per_octave / 12;

        let data: Vec<[f32; 12]> = cqt_spec
            .data
            .iter()
            .map(|cqt| {
                let mut chroma = [0.0f32; 12];

                for (i, &mag) in cqt.magnitudes.iter().enumerate() {
                    // Find pitch class (0-11)
                    let pitch_class = (i / bins_per_semitone) % 12;
                    chroma[pitch_class] += mag * mag; // Energy
                }

                // Convert to amplitude
                for c in &mut chroma {
                    *c = c.sqrt();
                }

                // Normalize
                let max = chroma.iter().fold(0.0f32, |a, &b| a.max(b));
                if max > 1e-10 {
                    for c in &mut chroma {
                        *c /= max;
                    }
                }

                chroma
            })
            .collect();

        Self {
            data,
            hop_size: cqt_spec.hop_size,
            sample_rate: cqt_spec.sample_rate,
        }
    }

    fn from_cqt_spectrogram_simple(cqt_spec: &CqtSpectrogram) -> Self {
        let data: Vec<[f32; 12]> = cqt_spec
            .data
            .iter()
            .map(|cqt| {
                let mut chroma = [0.0f32; 12];

                for (freq, &mag) in cqt.frequencies.iter().zip(cqt.magnitudes.iter()) {
                    // Convert frequency to MIDI note, then to pitch class
                    let midi = 69.0 + 12.0 * (freq / 440.0).log2();
                    let pitch_class = (midi.round() as i32 % 12).rem_euclid(12) as usize;
                    chroma[pitch_class] += mag * mag;
                }

                for c in &mut chroma {
                    *c = c.sqrt();
                }

                let max = chroma.iter().fold(0.0f32, |a, &b| a.max(b));
                if max > 1e-10 {
                    for c in &mut chroma {
                        *c /= max;
                    }
                }

                chroma
            })
            .collect();

        Self {
            data,
            hop_size: cqt_spec.hop_size,
            sample_rate: cqt_spec.sample_rate,
        }
    }

    /// Get pitch class names
    pub fn pitch_class_names() -> [&'static str; 12] {
        [
            "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
        ]
    }

    /// Get the dominant pitch class for a frame
    pub fn dominant_pitch_class(&self, frame: usize) -> Option<usize> {
        self.data.get(frame).map(|chroma| {
            chroma
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
                .map(|(i, _)| i)
                .unwrap_or(0)
        })
    }

    /// Get frame time in seconds
    pub fn frame_to_time(&self, frame: usize) -> f32 {
        frame as f32 * self.hop_size as f32 / self.sample_rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generate_sine(sample_rate: f32, freq: f32, duration_secs: f32) -> Vec<f32> {
        let num_samples = (duration_secs * sample_rate) as usize;
        (0..num_samples)
            .map(|i| (2.0 * PI * freq * i as f32 / sample_rate).sin())
            .collect()
    }

    #[test]
    fn test_cqt_pure_tone() {
        let sample_rate = 44100.0;
        let freq = 440.0; // A4
        let signal = generate_sine(sample_rate, freq, 0.5);

        let cqt = ConstantQTransform::musical(sample_rate);
        let result = cqt.analyze(&signal);

        // Peak should be near A4 (440 Hz)
        let peak_freq = result.peak_frequency().unwrap();
        assert!(
            (peak_freq - freq).abs() < 20.0,
            "Peak {} Hz should be near {} Hz",
            peak_freq,
            freq
        );
    }

    #[test]
    fn test_cqt_frequency_range() {
        let sample_rate = 48000.0;
        let cqt = ConstantQTransform::new(sample_rate, 100.0, 5000.0, 12);

        let (min, max) = cqt.frequency_range();
        assert!((min - 100.0).abs() < 1.0);
        assert!(max <= 5000.0);
    }

    #[test]
    fn test_cqt_bins_per_octave() {
        let sample_rate = 44100.0;
        let cqt = ConstantQTransform::new(sample_rate, 100.0, 1600.0, 12);

        // 100 Hz to 1600 Hz = 4 octaves = 48 bins
        assert_eq!(cqt.bins_per_octave(), 12);
        // Should have approximately 4 * 12 = 48 bins
        assert!(cqt.num_bins() >= 48 && cqt.num_bins() <= 50);
    }

    #[test]
    fn test_freq_to_bin_conversion() {
        let sample_rate = 44100.0;
        let cqt = ConstantQTransform::musical(sample_rate);

        // A4 = 440 Hz should map to a specific bin
        let bin = cqt.freq_to_bin(440.0);
        let freq_back = cqt.bin_to_freq(bin);

        // Should be within a semitone
        let ratio = freq_back / 440.0;
        assert!(
            ratio > 0.94 && ratio < 1.06,
            "Frequency {} should be near 440",
            freq_back
        );
    }

    #[test]
    fn test_midi_notes() {
        let sample_rate = 44100.0;
        let freq = 440.0; // A4 = MIDI 69
        let signal = generate_sine(sample_rate, freq, 0.5);

        let cqt = ConstantQTransform::musical(sample_rate);
        let result = cqt.analyze(&signal);
        let midi_notes = result.midi_notes();

        // Find the peak
        let peak_bin = result.peak_bin().unwrap();
        let peak_midi = midi_notes[peak_bin];

        // Should be close to MIDI 69 (A4)
        assert!(
            (peak_midi - 69.0).abs() < 1.0,
            "Peak MIDI note {} should be near 69",
            peak_midi
        );
    }

    #[test]
    fn test_cqt_spectrogram() {
        let sample_rate = 44100.0;
        let signal = generate_sine(sample_rate, 440.0, 1.0);

        let cqt = ConstantQTransform::musical(sample_rate);
        let spec = CqtSpectrogram::from_signal(&signal, &cqt, 2048);

        assert!(spec.num_frames() > 0);
        assert_ne!(spec.data.len(), 0);

        // Check time conversion
        let time_0 = spec.frame_to_time(0);
        assert!((time_0 - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_chromagram() {
        let sample_rate = 44100.0;
        let freq = 440.0; // A4
        let signal = generate_sine(sample_rate, freq, 1.0);

        let cqt = ConstantQTransform::musical(sample_rate);
        let cqt_spec = CqtSpectrogram::from_signal(&signal, &cqt, 2048);
        let chroma = Chromagram::from_cqt_spectrogram(&cqt_spec, 12);

        // Most frames should have A (pitch class 9) as dominant
        let dominant = chroma.dominant_pitch_class(chroma.data.len() / 2).unwrap();
        // A4 = pitch class 9
        assert_eq!(dominant, 9, "Dominant pitch class should be A (9)");
    }

    #[test]
    fn test_chromagram_pitch_names() {
        let names = Chromagram::pitch_class_names();
        assert_eq!(names[0], "C");
        assert_eq!(names[9], "A");
        assert_eq!(names[11], "B");
    }
}
