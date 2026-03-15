//! Stereo correlation measurement.
//!
//! [`StereoCorrelation`] measures the Pearson correlation coefficient between
//! left and right audio channels over a sliding window. The result ranges from
//! −1 (perfectly out-of-phase / maximum width) to +1 (mono / identical channels).
//!
//! This is the standard "correlation meter" found on mixing consoles and mastering
//! tools. Values below 0 indicate phase issues that may cause cancellation on mono
//! playback.
//!
//! ## Formula
//!
//! ```text
//! r = Σ(L · R) / √(Σ(L²) · Σ(R²))
//! ```
//!
//! where the sums run over the current sliding window.
//!
//! ## Example
//!
//! ```rust
//! use sonido_analysis::stereo::StereoCorrelation;
//!
//! let mut meter = StereoCorrelation::new(4096);
//!
//! // Mono signal → correlation = +1
//! let mono = vec![0.5f32; 512];
//! meter.push_samples(&mono, &mono);
//! let r = meter.correlation();
//! assert!((r - 1.0).abs() < 0.01);
//! ```

use std::collections::VecDeque;

/// Stereo correlation meter with a sliding window.
///
/// Maintains three running accumulators over a circular sample window:
/// - `sum_lr`: Σ L·R
/// - `sum_l2`: Σ L²
/// - `sum_r2`: Σ R²
///
/// # Invariants
///
/// - Window size must be at least 1 (enforced by constructor).
/// - `correlation()` returns 0.0 when both channels are silent (denominator = 0).
#[derive(Debug, Clone)]
pub struct StereoCorrelation {
    window_size: usize,
    /// Ring buffer of (L·R, L², R²) tuples for the sliding window.
    ring: VecDeque<(f32, f32, f32)>,
    /// Running sum of L·R.
    sum_lr: f64,
    /// Running sum of L².
    sum_l2: f64,
    /// Running sum of R².
    sum_r2: f64,
}

impl StereoCorrelation {
    /// Create a new correlation meter with the given sliding-window size in samples.
    ///
    /// # Arguments
    ///
    /// - `window_size`: Number of samples in the sliding window. Typical values:
    ///   4096 (~85 ms at 48 kHz) for responsive metering; larger for smoother display.
    ///   Must be ≥ 1.
    ///
    /// # Panics
    ///
    /// Panics if `window_size == 0`.
    pub fn new(window_size: usize) -> Self {
        assert!(window_size > 0, "window_size must be at least 1");
        Self {
            window_size,
            ring: VecDeque::with_capacity(window_size + 1),
            sum_lr: 0.0,
            sum_l2: 0.0,
            sum_r2: 0.0,
        }
    }

    /// Feed stereo samples into the meter.
    ///
    /// # Arguments
    ///
    /// - `left`: Left channel samples.
    /// - `right`: Right channel samples. Must have the same length as `left`.
    ///
    /// # Panics
    ///
    /// Panics in debug builds if `left.len() != right.len()`.
    pub fn push_samples(&mut self, left: &[f32], right: &[f32]) {
        debug_assert_eq!(left.len(), right.len());

        for (&l, &r) in left.iter().zip(right.iter()) {
            let lr = l * r;
            let l2 = l * l;
            let r2 = r * r;

            // Evict oldest sample if window is full
            if self.ring.len() >= self.window_size
                && let Some((old_lr, old_l2, old_r2)) = self.ring.pop_front()
            {
                self.sum_lr -= old_lr as f64;
                self.sum_l2 -= old_l2 as f64;
                self.sum_r2 -= old_r2 as f64;
            }

            self.ring.push_back((lr, l2, r2));
            self.sum_lr += lr as f64;
            self.sum_l2 += l2 as f64;
            self.sum_r2 += r2 as f64;
        }
    }

    /// Pearson correlation coefficient between L and R over the current window.
    ///
    /// Returns a value in [−1.0, +1.0]:
    /// - **+1.0**: channels are identical (mono)
    /// - **0.0**: channels are uncorrelated
    /// - **−1.0**: channels are perfectly out of phase
    ///
    /// Returns `0.0` if both channels are silent (denominator ≤ 0).
    pub fn correlation(&self) -> f32 {
        let denom_sq = self.sum_l2 * self.sum_r2;
        if denom_sq <= 0.0 {
            return 0.0;
        }
        let r = self.sum_lr / denom_sq.sqrt();
        r.clamp(-1.0, 1.0) as f32
    }

    /// Reset all accumulated state.
    pub fn reset(&mut self) {
        self.ring.clear();
        self.sum_lr = 0.0;
        self.sum_l2 = 0.0;
        self.sum_r2 = 0.0;
    }

    /// Current number of samples in the sliding window.
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    /// Returns `true` if the window is empty.
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    #[test]
    fn mono_correlation_is_one() {
        let mut meter = StereoCorrelation::new(4096);
        let sig = vec![0.5f32; 1024];
        meter.push_samples(&sig, &sig);
        let r = meter.correlation();
        assert!(
            (r - 1.0).abs() < 1e-4,
            "mono signal should have r ≈ +1, got {}",
            r
        );
    }

    #[test]
    fn out_of_phase_correlation_is_minus_one() {
        let mut meter = StereoCorrelation::new(4096);
        let sr = 48000.0f32;
        let left: Vec<f32> = (0..2048)
            .map(|i| (2.0 * PI * 440.0 * i as f32 / sr).sin())
            .collect();
        let right: Vec<f32> = left.iter().map(|&x| -x).collect();
        meter.push_samples(&left, &right);
        let r = meter.correlation();
        assert!(
            (r + 1.0).abs() < 1e-4,
            "out-of-phase signal should have r ≈ −1, got {}",
            r
        );
    }

    #[test]
    fn decorrelated_channels_near_zero() {
        // Two sine waves at different frequencies are uncorrelated
        let sr = 48000.0f32;
        let n = 4096;
        let left: Vec<f32> = (0..n)
            .map(|i| (2.0 * PI * 100.0 * i as f32 / sr).sin())
            .collect();
        let right: Vec<f32> = (0..n)
            .map(|i| (2.0 * PI * 327.0 * i as f32 / sr).sin())
            .collect();

        let mut meter = StereoCorrelation::new(n);
        meter.push_samples(&left, &right);
        let r = meter.correlation();
        assert!(
            r.abs() < 0.2,
            "decorrelated signals should have |r| near 0, got {}",
            r
        );
    }

    #[test]
    fn silence_returns_zero() {
        let mut meter = StereoCorrelation::new(1024);
        let silence = vec![0.0f32; 512];
        meter.push_samples(&silence, &silence);
        assert_eq!(meter.correlation(), 0.0);
    }

    #[test]
    fn sliding_window_forgets_old_data() {
        let window = 256;
        let mut meter = StereoCorrelation::new(window);

        // Fill with anti-correlated data
        let n = window * 4;
        let sig: Vec<f32> = (0..n).map(|i| (i as f32 / n as f32) * 2.0 - 1.0).collect();
        let neg: Vec<f32> = sig.iter().map(|&x| -x).collect();
        meter.push_samples(&sig, &neg);

        // Now overwrite with mono (correlated) data
        let mono = vec![0.5f32; window * 2];
        meter.push_samples(&mono, &mono);

        let r = meter.correlation();
        assert!(
            r > 0.9,
            "after overwriting with mono data, correlation should be near +1, got {}",
            r
        );
    }

    #[test]
    fn reset_clears_state() {
        let mut meter = StereoCorrelation::new(1024);
        let sig = vec![0.3f32; 512];
        meter.push_samples(&sig, &sig);
        assert!(!meter.is_empty());
        meter.reset();
        assert!(meter.is_empty());
        assert_eq!(meter.correlation(), 0.0);
    }
}
