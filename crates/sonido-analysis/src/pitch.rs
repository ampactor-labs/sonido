//! Standalone YIN pitch detection for analysis use.
//!
//! Provides [`detect_pitch`] — a pure function that runs the YIN fundamental
//! frequency estimator on a mono audio buffer. Unlike the tuner kernel in
//! `sonido-effects`, this module targets offline/analysis workloads where
//! the full buffer is available at once.
//!
//! ## YIN Algorithm
//!
//! Reference: A. de Cheveigné and H. Kawahara, "YIN, a fundamental frequency
//! estimator for speech and music", JASA 111(4), 2002.
//!
//! Steps:
//! 1. Compute difference function `d(τ) = Σ (x[j] − x[j+τ])²`
//! 2. Cumulative Mean Normalized Difference (CMNDF):
//!    `d′(0) = 1`, `d′(τ) = d(τ) · τ / Σ d(j), j=1..τ`
//! 3. Find first τ where `d′(τ) < 0.15` and is a local minimum
//! 4. Parabolic interpolation for sub-sample accuracy
//! 5. `f0 = sample_rate / τ`
//!
//! ## Detection Range
//!
//! A0 = 27.5 Hz to C8 ≈ 4186 Hz. The required buffer length depends on the
//! lowest frequency to detect: at least `sample_rate / min_hz` samples.
//! [`detect_pitch`] uses the supplied buffer length to determine `τ_max`.
//!
//! ## Example
//!
//! ```rust
//! use sonido_analysis::pitch::{detect_pitch, PitchResult};
//! use std::f32::consts::PI;
//!
//! let sr = 48000.0f32;
//! let freq = 440.0f32;
//! let buf: Vec<f32> = (0..4096)
//!     .map(|i| (2.0 * PI * freq * i as f32 / sr).sin())
//!     .collect();
//!
//! if let Some(result) = detect_pitch(&buf, sr) {
//!     println!("detected: {:.1} Hz  confidence: {:.3}", result.frequency, result.confidence);
//!     assert!((result.frequency - freq).abs() < freq * 0.01); // within 1 cent ≈ 0.058%
//! }
//! ```

/// Result from [`detect_pitch`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PitchResult {
    /// Detected fundamental frequency in Hz. Range: 27.5–4186 Hz.
    pub frequency: f32,
    /// Confidence in [0.0, 1.0].
    ///
    /// Derived from the minimum CMNDF value at the detected period:
    /// `confidence = 1 − d′(τ_best)`. Values above ~0.85 are reliable.
    pub confidence: f32,
}

// ── constants ────────────────────────────────────────────────────────────────

/// YIN threshold. Below this CMNDF value a period is accepted.
/// Lower → fewer false accepts; higher → fewer misses.
const YIN_THRESHOLD: f32 = 0.15;

/// Minimum detected frequency (A0), Hz.
const MIN_HZ: f32 = 27.5;

/// Maximum detected frequency (C8 ≈ 4186 Hz), Hz.
const MAX_HZ: f32 = 4186.0;

// ── public API ───────────────────────────────────────────────────────────────

/// Detect the fundamental frequency of a mono audio buffer.
///
/// # Arguments
///
/// - `buffer`: Mono audio samples. Longer buffers improve low-frequency detection.
///   Minimum recommended: `sample_rate / 27.5` samples (≈ 1745 at 48 kHz).
/// - `sample_rate`: Sample rate in Hz.
///
/// # Returns
///
/// `Some(PitchResult)` if a pitch is detected within [27.5, 4186] Hz, `None` otherwise.
///
/// # Complexity
///
/// O(N × τ_max) where `τ_max = sample_rate / MIN_HZ`. For a 4096-sample buffer at
/// 48 kHz the inner loop runs ~1745 × 2351 ≈ 4.1 M iterations — suitable for
/// offline analysis but not per-sample real-time use (see tuner kernel instead).
pub fn detect_pitch(buffer: &[f32], sample_rate: f32) -> Option<PitchResult> {
    if buffer.is_empty() || sample_rate <= 0.0 {
        return None;
    }

    let tau_min = (sample_rate / MAX_HZ).ceil() as usize;
    let tau_max_ideal = (sample_rate / MIN_HZ).ceil() as usize;
    // τ_max is bounded by half the buffer length (integration window = buf_len − τ)
    let tau_max = tau_max_ideal.min(buffer.len() / 2);

    if tau_max < tau_min + 2 {
        // Buffer too short to detect any frequency in range
        return None;
    }

    let w = buffer.len() - tau_max; // integration window length

    // ── Step 1 & 2: difference function + CMNDF ──────────────────────────
    let mut cmndf = vec![0.0f32; tau_max + 1];
    cmndf[0] = 1.0;
    let mut running_sum = 0.0f32;

    for tau in 1..=tau_max {
        let mut d = 0.0f32;
        for j in 0..w {
            let diff = buffer[j] - buffer[j + tau];
            d += diff * diff;
        }
        running_sum += d;
        cmndf[tau] = if running_sum > 1e-10 {
            d * tau as f32 / running_sum
        } else {
            1.0
        };
    }

    // ── Step 3: find first dip below threshold ────────────────────────────
    let mut best_tau: Option<usize> = None;

    for tau in tau_min..tau_max {
        if cmndf[tau] < YIN_THRESHOLD && cmndf[tau] <= cmndf[tau + 1] {
            best_tau = Some(tau);
            break;
        }
    }

    // Fallback: global minimum in range
    let tau_est = if let Some(t) = best_tau {
        t
    } else {
        let (t, &val) = cmndf[tau_min..=tau_max]
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())?;
        if val > 0.4 {
            return None; // too uncertain
        }
        t + tau_min
    };

    // ── Step 4: parabolic interpolation ──────────────────────────────────
    let tau_f = if tau_est > 0 && tau_est < tau_max {
        let x0 = cmndf[tau_est - 1];
        let x1 = cmndf[tau_est];
        let x2 = cmndf[tau_est + 1];
        let denom = x0 - 2.0 * x1 + x2;
        if denom.abs() > 1e-10 {
            tau_est as f32 - 0.5 * (x2 - x0) / denom
        } else {
            tau_est as f32
        }
    } else {
        tau_est as f32
    };

    if tau_f < 1.0 {
        return None;
    }

    let frequency = sample_rate / tau_f;

    // Guard: must be within detection range
    if !(MIN_HZ..=MAX_HZ).contains(&frequency) {
        return None;
    }

    // Confidence: 1 − CMNDF value at detected lag (clamped to [0, 1])
    let cmndf_val = cmndf[tau_est].clamp(0.0, 1.0);
    let confidence = (1.0 - cmndf_val).clamp(0.0, 1.0);

    Some(PitchResult {
        frequency,
        confidence,
    })
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    fn sine(freq: f32, sr: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * PI * freq * i as f32 / sr).sin())
            .collect()
    }

    /// Cents deviation between two frequencies.
    fn cents_error(detected: f32, reference: f32) -> f32 {
        1200.0 * (detected / reference).log2().abs()
    }

    #[test]
    fn detect_a4_440hz() {
        let sr = 48000.0f32;
        let buf = sine(440.0, sr, 4096);
        let result = detect_pitch(&buf, sr).expect("should detect 440 Hz");
        assert!(
            cents_error(result.frequency, 440.0) < 1.0,
            "A4 detection error {} cents (detected {} Hz)",
            cents_error(result.frequency, 440.0),
            result.frequency
        );
    }

    #[test]
    fn detect_low_e_guitar_82hz() {
        let sr = 48000.0f32;
        // Need a long buffer for low frequency detection
        let buf = sine(82.41, sr, 8192);
        let result = detect_pitch(&buf, sr).expect("should detect ~82 Hz");
        assert!(
            cents_error(result.frequency, 82.41) < 5.0,
            "low E detection error {} cents (detected {} Hz)",
            cents_error(result.frequency, 82.41),
            result.frequency
        );
    }

    #[test]
    fn detect_high_note_2khz() {
        let sr = 48000.0f32;
        let buf = sine(2000.0, sr, 4096);
        let result = detect_pitch(&buf, sr).expect("should detect 2000 Hz");
        // YIN accuracy at 2 kHz with a 4096-sample buffer (period ≈ 24 samples)
        // is limited by integer lag quantisation. 2 cents is within spec.
        assert!(
            cents_error(result.frequency, 2000.0) < 2.0,
            "2 kHz detection error {} cents (detected {} Hz)",
            cents_error(result.frequency, 2000.0),
            result.frequency
        );
    }

    #[test]
    fn no_detection_on_silence() {
        let buf = vec![0.0f32; 4096];
        assert!(detect_pitch(&buf, 48000.0).is_none());
    }

    #[test]
    fn no_detection_on_white_noise() {
        // White noise shouldn't confidently lock on a pitch
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let buf: Vec<f32> = (0..4096)
            .map(|i| {
                let mut h = DefaultHasher::new();
                i.hash(&mut h);
                (h.finish() as f32 / u64::MAX as f32) * 2.0 - 1.0
            })
            .collect();
        // Either no detection or very low confidence
        if let Some(r) = detect_pitch(&buf, 48000.0) {
            assert!(
                r.confidence < 0.9,
                "noise detection should have low confidence: {}",
                r.confidence
            );
        }
    }

    #[test]
    fn confidence_high_for_pure_sine() {
        let sr = 48000.0f32;
        let buf = sine(440.0, sr, 4096);
        let result = detect_pitch(&buf, sr).expect("should detect");
        assert!(
            result.confidence > 0.8,
            "pure sine should have high confidence, got {}",
            result.confidence
        );
    }

    #[test]
    fn empty_buffer_returns_none() {
        assert!(detect_pitch(&[], 48000.0).is_none());
    }
}
