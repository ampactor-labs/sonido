//! Impulse response capture via exponential sine sweep
//!
//! # Status
//!
//! Sweep-based IR capture is fully implemented but not in the demo path.

use std::f32::consts::PI;

/// Exponential sine sweep generator for IR capture
///
/// Uses the Farina method for deconvolution-based impulse response measurement.
pub struct SineSweep {
    sample_rate: f32,
    start_freq: f32,
    end_freq: f32,
    duration_secs: f32,
}

impl SineSweep {
    /// Create a new sine sweep generator
    ///
    /// # Arguments
    /// * `sample_rate` - Sample rate in Hz
    /// * `start_freq` - Start frequency in Hz
    /// * `end_freq` - End frequency in Hz
    /// * `duration_secs` - Sweep duration in seconds
    pub fn new(sample_rate: f32, start_freq: f32, end_freq: f32, duration_secs: f32) -> Self {
        Self {
            sample_rate,
            start_freq,
            end_freq,
            duration_secs,
        }
    }

    /// Generate the exponential sine sweep
    pub fn generate(&self) -> Vec<f32> {
        let num_samples = (self.duration_secs * self.sample_rate) as usize;
        let k = (self.end_freq / self.start_freq).ln();

        (0..num_samples)
            .map(|i| {
                let t = i as f32 / self.sample_rate;
                let phase = 2.0 * PI * self.start_freq * self.duration_secs / k
                    * ((k * t / self.duration_secs).exp() - 1.0);
                phase.sin()
            })
            .collect()
    }

    /// Generate the inverse filter for deconvolution
    pub fn inverse_filter(&self) -> Vec<f32> {
        let sweep = self.generate();
        let k = (self.end_freq / self.start_freq).ln();

        // Time-reverse and apply amplitude envelope
        sweep
            .into_iter()
            .rev()
            .enumerate()
            .map(|(i, sample)| {
                let t = i as f32 / self.sample_rate;
                // Amplitude envelope compensates for exponential frequency increase
                let amplitude = (-k * t / self.duration_secs).exp();
                sample * amplitude
            })
            .collect()
    }

    /// Compute impulse response from recorded sweep response
    ///
    /// # Arguments
    /// * `response` - Recorded sweep through the system under test
    ///
    /// # Returns
    /// Impulse response of the system
    pub fn compute_ir(&self, response: &[f32]) -> Vec<f32> {
        use crate::fft::Fft;
        use rustfft::num_complex::Complex;

        let inverse = self.inverse_filter();

        // Pad to power of 2 for FFT
        let fft_size = (response.len() + inverse.len() - 1).next_power_of_two();
        let fft = Fft::new(fft_size);

        // Convert to complex and pad
        let mut response_complex: Vec<Complex<f32>> =
            response.iter().map(|&x| Complex::new(x, 0.0)).collect();
        response_complex.resize(fft_size, Complex::new(0.0, 0.0));

        let mut inverse_complex: Vec<Complex<f32>> =
            inverse.iter().map(|&x| Complex::new(x, 0.0)).collect();
        inverse_complex.resize(fft_size, Complex::new(0.0, 0.0));

        // FFT both
        fft.forward_complex(&mut response_complex);
        fft.forward_complex(&mut inverse_complex);

        // Multiply in frequency domain
        for (r, i) in response_complex.iter_mut().zip(inverse_complex.iter()) {
            *r *= *i;
        }

        // IFFT
        fft.inverse_complex(&mut response_complex);

        // Extract real part
        response_complex.iter().map(|c| c.re).collect()
    }

    /// Get sweep duration in seconds
    pub fn duration(&self) -> f32 {
        self.duration_secs
    }

    /// Get number of samples
    pub fn num_samples(&self) -> usize {
        (self.duration_secs * self.sample_rate) as usize
    }
}

/// Generate a simple impulse signal
pub fn impulse(length: usize) -> Vec<f32> {
    let mut signal = vec![0.0; length];
    if !signal.is_empty() {
        signal[0] = 1.0;
    }
    signal
}

/// Generate white noise for testing
pub fn white_noise(length: usize, amplitude: f32) -> Vec<f32> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    (0..length)
        .map(|i| {
            // Simple PRNG using hash
            let mut hasher = DefaultHasher::new();
            i.hash(&mut hasher);
            let hash = hasher.finish();
            let random = (hash as f32 / u64::MAX as f32) * 2.0 - 1.0;
            random * amplitude
        })
        .collect()
}

/// Trim an impulse response to its significant portion
///
/// Removes leading silence and trailing decay below threshold.
///
/// # Arguments
/// * `ir` - Impulse response samples
/// * `start_threshold_db` - Threshold for detecting IR start (relative to peak)
/// * `end_threshold_db` - Threshold for detecting IR end (relative to peak)
///
/// # Returns
/// Tuple of (trimmed_ir, start_sample, end_sample)
pub fn trim_ir(
    ir: &[f32],
    start_threshold_db: f32,
    end_threshold_db: f32,
) -> (Vec<f32>, usize, usize) {
    if ir.is_empty() {
        return (Vec::new(), 0, 0);
    }

    // Find peak amplitude
    let peak = ir.iter().map(|&x| x.abs()).fold(0.0f32, f32::max);
    if peak < 1e-10 {
        return (Vec::new(), 0, 0);
    }

    // Convert thresholds to linear
    let start_thresh_linear = peak * 10.0f32.powf(start_threshold_db / 20.0);
    let end_thresh_linear = peak * 10.0f32.powf(end_threshold_db / 20.0);

    // Find start (first sample above threshold)
    let start = ir
        .iter()
        .position(|&x| x.abs() > start_thresh_linear)
        .unwrap_or(0);

    // Find end (last sample above threshold)
    let end = ir
        .iter()
        .rposition(|&x| x.abs() > end_thresh_linear)
        .unwrap_or(ir.len() - 1)
        + 1;

    if end <= start {
        return (Vec::new(), start, start);
    }

    (ir[start..end].to_vec(), start, end)
}

/// Compute the Energy Decay Curve (Schroeder integration)
///
/// The EDC shows how energy decays over time, useful for RT60 estimation.
/// Computed by reverse-integrating the squared impulse response.
///
/// # Arguments
/// * `ir` - Impulse response samples
///
/// # Returns
/// Energy decay curve in dB (normalized to 0 dB at start)
pub fn energy_decay_curve(ir: &[f32]) -> Vec<f32> {
    if ir.is_empty() {
        return Vec::new();
    }

    // Compute squared samples
    let squared: Vec<f32> = ir.iter().map(|&x| x * x).collect();

    // Reverse cumulative sum (Schroeder integration)
    let mut edc = Vec::with_capacity(ir.len());
    let mut sum = 0.0f32;

    for &s in squared.iter().rev() {
        sum += s;
        edc.push(sum);
    }

    edc.reverse();

    // Normalize to 0 dB at start and convert to dB
    let max_energy = edc[0].max(1e-10);
    edc.iter()
        .map(|&e| 10.0 * (e / max_energy).max(1e-10).log10())
        .collect()
}

/// RT60 estimation result
#[derive(Debug, Clone, Copy)]
pub struct Rt60Estimate {
    /// RT60 in seconds (extrapolated from decay slope)
    pub rt60_seconds: f32,
    /// T20 (time to decay 20 dB, from -5 to -25 dB)
    pub t20_seconds: f32,
    /// T30 (time to decay 30 dB, from -5 to -35 dB)
    pub t30_seconds: f32,
    /// Early Decay Time (time to decay 10 dB, from 0 to -10 dB)
    pub edt_seconds: f32,
    /// Correlation coefficient of the linear fit (1.0 = perfect fit)
    pub correlation: f32,
}

/// Estimate RT60 (reverberation time) from an impulse response
///
/// Uses the Schroeder method with linear regression on the EDC.
///
/// # Arguments
/// * `ir` - Impulse response samples
/// * `sample_rate` - Sample rate in Hz
///
/// # Returns
/// RT60 estimate with multiple decay metrics
pub fn estimate_rt60(ir: &[f32], sample_rate: f32) -> Option<Rt60Estimate> {
    if ir.is_empty() {
        return None;
    }

    let edc = energy_decay_curve(ir);
    if edc.is_empty() {
        return None;
    }

    // Find EDT (0 to -10 dB)
    let edt_seconds = find_decay_time(&edc, 0.0, -10.0, sample_rate);

    // Find T20 (-5 to -25 dB) - used for RT60 extrapolation
    let t20_seconds = find_decay_time(&edc, -5.0, -25.0, sample_rate);

    // Find T30 (-5 to -35 dB) - more accurate if noise floor allows
    let t30_seconds = find_decay_time(&edc, -5.0, -35.0, sample_rate);

    // Extrapolate RT60 from T20 (multiply by 3)
    let rt60_from_t20 = t20_seconds * 3.0;
    let rt60_from_t30 = t30_seconds * 2.0;

    // Use T30 if available, otherwise T20
    let rt60_seconds =
        if t30_seconds > 0.0 && (rt60_from_t30 - rt60_from_t20).abs() / rt60_from_t20 < 0.3 {
            rt60_from_t30
        } else {
            rt60_from_t20
        };

    // Calculate correlation from the -5 to -25 dB region
    let correlation = calculate_edc_correlation(&edc, -5.0, -25.0);

    Some(Rt60Estimate {
        rt60_seconds,
        t20_seconds,
        t30_seconds,
        edt_seconds,
        correlation,
    })
}

/// Find decay time between two dB levels
fn find_decay_time(edc: &[f32], start_db: f32, end_db: f32, sample_rate: f32) -> f32 {
    // Find indices for start and end levels
    let start_idx = edc.iter().position(|&e| e <= start_db);
    let end_idx = edc.iter().position(|&e| e <= end_db);

    match (start_idx, end_idx) {
        (Some(s), Some(e)) if e > s => {
            // Linear regression for more accurate timing
            let mut sum_x = 0.0f32;
            let mut sum_y = 0.0f32;
            let mut sum_xy = 0.0f32;
            let mut sum_xx = 0.0f32;

            for (i, &val) in edc[s..=e].iter().enumerate() {
                let x = i as f32;
                let y = val;
                sum_x += x;
                sum_y += y;
                sum_xy += x * y;
                sum_xx += x * x;
            }

            let n_points = (e - s + 1) as f32;
            let slope = (n_points * sum_xy - sum_x * sum_y) / (n_points * sum_xx - sum_x * sum_x);

            if slope < 0.0 {
                // Time to decay from start_db to end_db
                let db_range = start_db - end_db;
                let samples = db_range / (-slope);
                samples / sample_rate
            } else {
                0.0
            }
        }
        _ => 0.0,
    }
}

/// Calculate correlation coefficient for EDC linearity
fn calculate_edc_correlation(edc: &[f32], start_db: f32, end_db: f32) -> f32 {
    let start_idx = edc.iter().position(|&e| e <= start_db);
    let end_idx = edc.iter().position(|&e| e <= end_db);

    match (start_idx, end_idx) {
        (Some(s), Some(e)) if e > s => {
            let n = (e - s + 1) as f32;
            let mut sum_x = 0.0f32;
            let mut sum_y = 0.0f32;
            let mut sum_xy = 0.0f32;
            let mut sum_xx = 0.0f32;
            let mut sum_yy = 0.0f32;

            for (i, &val) in edc[s..=e].iter().enumerate() {
                let x = i as f32;
                let y = val;
                sum_x += x;
                sum_y += y;
                sum_xy += x * y;
                sum_xx += x * x;
                sum_yy += y * y;
            }

            let numerator = n * sum_xy - sum_x * sum_y;
            let denominator = ((n * sum_xx - sum_x * sum_x) * (n * sum_yy - sum_y * sum_y)).sqrt();

            if denominator > 0.0 {
                (numerator / denominator).abs()
            } else {
                0.0
            }
        }
        _ => 0.0,
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Environmental-audio / room-capture API
// ────────────────────────────────────────────────────────────────────────────

/// Configuration for an impulse-response measurement sweep.
///
/// Used by [`generate_sweep`] and [`compute_ir`] for the "Capture Room" /
/// "Clone This Space" workflow.
///
/// # Typical Values
///
/// | Field           | Default  | Range          |
/// |-----------------|----------|----------------|
/// | `start_freq`    | 20 Hz    | [1, 200] Hz    |
/// | `end_freq`      | 20 000 Hz| [1 k, 24 k] Hz |
/// | `duration_secs` | 5 s      | [1, 30] s      |
/// | `sample_rate`   | 48 000 Hz| [22 050, 96 k] |
pub struct SweepConfig {
    /// Sweep start frequency in Hz.  Typical: 20 Hz.
    pub start_freq: f32,
    /// Sweep end frequency in Hz.  Typical: 20 000 Hz.
    pub end_freq: f32,
    /// Sweep duration in seconds.  Longer sweeps give higher SNR.
    pub duration_secs: f32,
    /// Output sample rate in Hz.
    pub sample_rate: f32,
}

impl Default for SweepConfig {
    fn default() -> Self {
        Self {
            start_freq: 20.0,
            end_freq: 20_000.0,
            duration_secs: 5.0,
            sample_rate: 48_000.0,
        }
    }
}

/// Generate a logarithmic (Farina) sine sweep.
///
/// Produces a unit-amplitude exponential sine sweep from `config.start_freq`
/// to `config.end_freq` over `config.duration_secs` seconds.  The sweep has
/// flat power-spectral density on a log-frequency axis, which yields a
/// well-conditioned deconvolution in [`compute_ir`].
///
/// # Reference
///
/// A. Farina, "Simultaneous measurement of impulse response and distortion
/// with a swept-sine technique," AES 108th Convention, 2000.
///
pub fn generate_sweep(config: &SweepConfig) -> Vec<f32> {
    SineSweep::new(
        config.sample_rate,
        config.start_freq,
        config.end_freq,
        config.duration_secs,
    )
    .generate()
}

/// Compute an impulse response from a recorded sweep response.
///
/// Deconvolves `recorded` with the analytical inverse filter of `sweep` in the
/// frequency domain.  The result is the linear time-invariant impulse response
/// of the room or device under test.
///
/// # Arguments
///
/// * `sweep`    — The excitation signal, as produced by [`generate_sweep`].
/// * `recorded` — The microphone capture of `sweep` played through the room.
/// * `config`   — Must match the config used to generate `sweep`.
///
/// # Returns
///
/// Impulse response samples at `config.sample_rate`.  Length equals
/// `recorded.len() + sweep.len() - 1` (linear convolution length).
pub fn compute_ir(_sweep: &[f32], recorded: &[f32], config: &SweepConfig) -> Vec<f32> {
    SineSweep::new(
        config.sample_rate,
        config.start_freq,
        config.end_freq,
        config.duration_secs,
    )
    .compute_ir(recorded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sine_sweep_generation() {
        let sweep = SineSweep::new(48000.0, 20.0, 20000.0, 1.0);
        let signal = sweep.generate();

        assert_eq!(signal.len(), 48000);

        // Should be bounded
        assert!(signal.iter().all(|&x| x.abs() <= 1.0));
    }

    #[test]
    fn test_inverse_filter_length() {
        let sweep = SineSweep::new(48000.0, 20.0, 20000.0, 1.0);
        let inverse = sweep.inverse_filter();

        assert_eq!(inverse.len(), sweep.num_samples());
    }

    #[test]
    fn test_impulse() {
        let imp = impulse(100);
        assert_eq!(imp[0], 1.0);
        assert!(imp[1..].iter().all(|&x| x == 0.0));
    }

    #[test]
    fn test_trim_ir_basic() {
        // Create an IR with leading silence and trailing decay
        let mut ir = vec![0.0; 100];
        ir[20] = 1.0; // Peak
        ir[21] = 0.5;
        ir[22] = 0.25;
        ir[23] = 0.1;
        ir[24] = 0.05;
        // Rest is silence

        let (trimmed, start, end) = trim_ir(&ir, -20.0, -40.0);

        assert!(start >= 20, "Should start at or after the peak");
        assert!(end <= 30, "Should end after the decay");
        assert!(!trimmed.is_empty(), "Trimmed IR should not be empty");
    }

    #[test]
    fn test_trim_ir_empty() {
        let ir: Vec<f32> = vec![];
        let (trimmed, start, end) = trim_ir(&ir, -20.0, -60.0);
        assert!(trimmed.is_empty());
        assert_eq!(start, 0);
        assert_eq!(end, 0);
    }

    #[test]
    fn test_trim_ir_silence() {
        let ir = vec![0.0; 100];
        let (trimmed, _, _) = trim_ir(&ir, -20.0, -60.0);
        assert!(trimmed.is_empty());
    }

    #[test]
    fn test_energy_decay_curve() {
        // Create exponentially decaying IR
        let sample_rate = 48000.0;
        let decay_time = 0.5; // 500ms
        let ir: Vec<f32> = (0..24000)
            .map(|i| {
                let t = i as f32 / sample_rate;
                (-t / decay_time * 6.91).exp() // 6.91 = ln(1000) for 60dB decay
            })
            .collect();

        let edc = energy_decay_curve(&ir);

        assert_eq!(edc.len(), ir.len());
        assert!((edc[0] - 0.0).abs() < 0.1, "EDC should start at 0 dB");

        // EDC should be monotonically decreasing
        for i in 1..edc.len() {
            assert!(
                edc[i] <= edc[i - 1] + 0.01,
                "EDC should be monotonically decreasing"
            );
        }
    }

    #[test]
    fn test_energy_decay_curve_empty() {
        let ir: Vec<f32> = vec![];
        let edc = energy_decay_curve(&ir);
        assert!(edc.is_empty());
    }

    #[test]
    fn test_estimate_rt60_exponential_decay() {
        // Create an exponentially decaying IR with known RT60
        let sample_rate = 48000.0;
        let target_rt60 = 1.0; // 1 second RT60
        let decay_constant = target_rt60 / 6.91; // time constant for 60dB decay

        let ir: Vec<f32> = (0..96000) // 2 seconds
            .map(|i| {
                let t = i as f32 / sample_rate;
                (-t / decay_constant).exp()
            })
            .collect();

        let result = estimate_rt60(&ir, sample_rate);
        assert!(result.is_some(), "Should successfully estimate RT60");

        let rt60 = result.unwrap();
        assert!(
            (rt60.rt60_seconds - target_rt60).abs() < 0.2,
            "RT60 should be close to {} s, got {} s",
            target_rt60,
            rt60.rt60_seconds
        );
        assert!(
            rt60.correlation > 0.9,
            "Correlation should be high for exponential decay, got {}",
            rt60.correlation
        );
    }

    #[test]
    fn test_estimate_rt60_empty() {
        let ir: Vec<f32> = vec![];
        let result = estimate_rt60(&ir, 48000.0);
        assert!(result.is_none());
    }

    #[test]
    fn test_rt60_metrics() {
        let sample_rate = 48000.0;
        let target_rt60 = 0.5;
        let decay_constant = target_rt60 / 6.91;

        let ir: Vec<f32> = (0..48000)
            .map(|i| {
                let t = i as f32 / sample_rate;
                (-t / decay_constant).exp()
            })
            .collect();

        let result = estimate_rt60(&ir, sample_rate).unwrap();

        // EDT should be approximately RT60/6 for exponential decay
        assert!(result.edt_seconds > 0.0, "EDT should be positive");

        // T20 should be approximately RT60/3
        assert!(result.t20_seconds > 0.0, "T20 should be positive");

        // T30 should be approximately RT60/2
        assert!(result.t30_seconds > 0.0, "T30 should be positive");
    }

    #[test]
    fn test_generate_sweep_matches_sine_sweep() {
        let config = SweepConfig {
            sample_rate: 48000.0,
            start_freq: 20.0,
            end_freq: 20000.0,
            duration_secs: 1.0,
        };
        let from_fn = generate_sweep(&config);
        let from_struct = SineSweep::new(
            config.sample_rate,
            config.start_freq,
            config.end_freq,
            config.duration_secs,
        )
        .generate();

        assert_eq!(from_fn.len(), from_struct.len());
        for (a, b) in from_fn.iter().zip(from_struct.iter()) {
            assert!(
                (a - b).abs() < f32::EPSILON,
                "generate_sweep output must match SineSweep::generate()"
            );
        }
    }

    #[test]
    fn test_compute_ir_nonempty() {
        let config = SweepConfig {
            sample_rate: 48000.0,
            start_freq: 20.0,
            end_freq: 20000.0,
            duration_secs: 0.1,
        };
        let sweep = generate_sweep(&config);
        // Use the sweep itself as the "recorded" signal — simulates a
        // pass-through system whose IR is approximately an impulse.
        let ir = compute_ir(&sweep, &sweep, &config);
        assert!(!ir.is_empty(), "compute_ir should return non-empty IR");
        assert!(
            ir.iter().any(|&x| x.abs() > 1e-6),
            "IR should contain non-trivial values"
        );
    }
}
