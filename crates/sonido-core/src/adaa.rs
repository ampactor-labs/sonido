//! Anti-Derivative Anti-Aliasing (ADAA) for static waveshapers.
//!
//! ADAA reduces aliasing in nonlinear waveshaping by reformulating the
//! waveshaper as a continuous-time convolution, then discretizing.  Instead
//! of evaluating `f(x)` directly, first-order ADAA computes:
//!
//! ```text
//! y[n] = (F(x[n]) − F(x[n−1])) / (x[n] − x[n−1])
//! ```
//!
//! where `F` is the first antiderivative of the waveshaping function `f`.
//! When consecutive inputs are nearly identical
//! (`|x[n] − x[n−1]| ≤ ε·(1 + max(|x[n]|, |x[n−1]|))`), the midpoint fallback
//! `f((x[n] + x[n−1]) / 2)` is used to avoid numerical instability from
//! division by near-zero.
//!
//! This is equivalent to applying a rectangular-window anti-aliasing filter
//! in the continuous-time domain, significantly reducing aliased harmonics
//! at minimal computational cost (one extra function evaluation per sample).
//!
//! # Theory
//!
//! A memoryless waveshaper `y = f(x)` applied to a discrete signal creates
//! new harmonic content that can exceed the Nyquist frequency, folding back
//! as aliasing.  Oversampling reduces this but requires 4–8× the processing.
//!
//! ADAA exploits the fact that the *average* of a continuous function over
//! an interval `[x₀, x₁]` equals `(F(x₁) − F(x₀)) / (x₁ − x₀)`, where
//! `F` is the antiderivative.  This implicitly performs a box-filter
//! anti-aliasing on the waveshaper output, suppressing aliased harmonics by
//! approximately 6 dB/octave (first-order ADAA).
//!
//! First-order ADAA adds negligible latency (conceptually half a sample)
//! and requires only the antiderivative evaluation plus one division per
//! sample, making it far cheaper than oversampling for equivalent alias
//! rejection.
//!
//! # Reference
//!
//! Parker et al., "Reducing the Aliasing of Nonlinear Waveshaping Using
//! Continuous-Time Convolution", Proceedings of the 19th International
//! Conference on Digital Audio Effects (DAFx-2016), Brno, Czech Republic.
//!
//! # Example
//!
//! ```rust
//! use sonido_core::adaa::Adaa1;
//! use sonido_core::math::{soft_clip, soft_clip_ad};
//!
//! let mut adaa = Adaa1::new(soft_clip, soft_clip_ad);
//! let output = adaa.process(0.5);
//! assert!(output.abs() < 1.0);
//! ```

/// First-order ADAA processor for static waveshapers.
///
/// Wraps a waveshaping function `f` and its first antiderivative `F` to
/// produce anti-aliased output.  The generic parameters allow zero-cost
/// inlining when used with function pointers or non-capturing closures.
///
/// # Type Parameters
///
/// * `F` — The waveshaping function `f(x) → y`
/// * `AF` — Its first antiderivative `F(x)` such that `F'(x) = f(x)`
///
/// # Epsilon and Midpoint Fallback
///
/// When `|x[n] − x[n−1]| ≤ 1e−3 · (1 + max(|x[n]|, |x[n−1]|))`, the
/// finite-difference quotient is dominated by `F`'s rounding error. The
/// processor falls back to evaluating the waveshaper at the midpoint
/// `(x[n] + x[n−1]) / 2`, which is the L'Hôpital limit of the ADAA formula
/// as `x[n] → x[n−1]` and differs from it by about `Δ²·f''/24`.
///
/// # Parameterized Waveshapers
///
/// For waveshapers with parameters (e.g., [`hard_clip`](crate::hard_clip)
/// with threshold), pass a closure that captures the parameter:
///
/// ```rust
/// use sonido_core::adaa::Adaa1;
/// use sonido_core::math::{hard_clip, hard_clip_ad};
///
/// let threshold = 0.8;
/// let mut adaa = Adaa1::new(
///     move |x| hard_clip(x, threshold),
///     move |x| hard_clip_ad(x, threshold),
/// );
/// let output = adaa.process(0.5);
/// ```
pub struct Adaa1<F, AF>
where
    F: Fn(f32) -> f32,
    AF: Fn(f32) -> f32,
{
    /// The waveshaping function.
    waveshaper: F,
    /// First antiderivative of the waveshaper.
    antiderivative: AF,
    /// Previous input sample.
    prev_x: f32,
    /// Antiderivative evaluated at previous input.
    prev_ad: f32,
}

/// Minimum input difference for the finite-difference quotient, relative to
/// `1 + max(|x₁|, |x₀|)`.
///
/// `F`'s rounding error grows with `|x|`, and the quotient divides it by
/// `x₁ − x₀`, so the cutoff scales with the input. Below it the processor
/// uses the midpoint `f((x₁ + x₀) / 2)`. At 1e−3 the soft clipper stays at
/// least 93 dB above its own rounding noise from −60 to −6 dBFS and
/// 82–880 Hz; the old absolute 1e−5 left about 55 dB.
const ADAA_EPSILON: f32 = 1e-3;

impl<F, AF> Adaa1<F, AF>
where
    F: Fn(f32) -> f32,
    AF: Fn(f32) -> f32,
{
    /// Create a new first-order ADAA processor.
    ///
    /// # Arguments
    ///
    /// * `waveshaper` — The nonlinear function `f(x)`
    /// * `antiderivative` — Its first antiderivative `F(x)` where `F'(x) = f(x)`
    ///
    /// The processor starts with zero state (previous input = 0).
    pub fn new(waveshaper: F, antiderivative: AF) -> Self {
        let prev_ad = antiderivative(0.0);
        Self {
            waveshaper,
            antiderivative,
            prev_x: 0.0,
            prev_ad,
        }
    }

    /// Process a single sample through the anti-aliased waveshaper.
    ///
    /// Computes the first-order ADAA output:
    ///
    /// ```text
    /// y = (F(x) − F(x_prev)) / (x − x_prev)     if |x − x_prev| > ε·(1 + max(|x|, |x_prev|))
    /// y = f((x + x_prev) / 2)                     otherwise (midpoint fallback)
    /// ```
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let ad = (self.antiderivative)(x);
        let diff = x - self.prev_x;
        let tol = ADAA_EPSILON * (1.0 + x.abs().max(self.prev_x.abs()));

        let result = if diff.abs() > tol {
            // Normal ADAA: finite-difference of antiderivative
            (ad - self.prev_ad) / diff
        } else {
            // Midpoint fallback (L'Hôpital limit)
            (self.waveshaper)(0.5 * (x + self.prev_x))
        };

        self.prev_x = x;
        self.prev_ad = ad;
        result
    }

    /// Process a block of samples in-place.
    ///
    /// Equivalent to calling [`process`](Self::process) on each sample
    /// sequentially.
    #[inline]
    pub fn process_block(&mut self, buffer: &mut [f32]) {
        for sample in buffer.iter_mut() {
            *sample = self.process(*sample);
        }
    }

    /// Reset internal state to zero.
    ///
    /// Call this when the audio stream is interrupted (e.g., preset change,
    /// transport stop) to prevent the first sample of the new segment from
    /// computing a stale difference against the last sample of the old one.
    pub fn reset(&mut self) {
        self.prev_x = 0.0;
        self.prev_ad = (self.antiderivative)(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::{
        asymmetric_clip, asymmetric_clip_ad, hard_clip, hard_clip_ad, soft_clip, soft_clip_ad,
        tape_sat_ad,
    };

    extern crate alloc;
    use alloc::{vec, vec::Vec};

    #[test]
    fn test_adaa_soft_clip_smoother_than_raw() {
        // A step input creates maximum aliasing in raw waveshaping.
        // ADAA should produce a smoother transition.
        let mut adaa = Adaa1::new(soft_clip, soft_clip_ad);
        let mut raw_out = Vec::new();
        let mut adaa_out = Vec::new();

        // Generate step: 64 samples at 0, then 64 at 0.8
        let input: Vec<f32> = (0..128).map(|i| if i < 64 { 0.0 } else { 0.8 }).collect();

        for &x in &input {
            raw_out.push(soft_clip(x));
            adaa_out.push(adaa.process(x));
        }

        // Measure high-frequency energy via first-difference sum
        let raw_hf: f32 = raw_out.windows(2).map(|w| (w[1] - w[0]).powi(2)).sum();
        let adaa_hf: f32 = adaa_out.windows(2).map(|w| (w[1] - w[0]).powi(2)).sum();

        // ADAA should have less high-frequency energy at the transition
        assert!(
            adaa_hf <= raw_hf,
            "ADAA should be smoother: adaa_hf={adaa_hf}, raw_hf={raw_hf}"
        );
    }

    #[test]
    fn test_adaa_hard_clip_with_closure() {
        let threshold = 0.6;
        let mut adaa = Adaa1::new(
            move |x| hard_clip(x, threshold),
            move |x| hard_clip_ad(x, threshold),
        );

        // Below threshold: ADAA output ≈ input (linear region, derivative = 1)
        adaa.reset();
        // Feed a few ramp samples to get past initial state
        for i in 0..10 {
            let x = i as f32 * 0.05;
            let y = adaa.process(x);
            // In linear region, ADAA of identity f(x)=x gives (x²/2 - prev²/2)/(x - prev)
            // = (x + prev)/2, which lags by half a sample. Check boundedness.
            assert!(y.abs() <= 0.6 + 0.01, "output {y} exceeds threshold");
        }
    }

    #[test]
    fn test_adaa_tape_sat() {
        let tape_ws = |x: f32| {
            if x >= 0.0 {
                1.0 - libm::expf(-2.0 * x)
            } else {
                -1.0 + libm::expf(1.8 * x)
            }
        };
        let mut adaa = Adaa1::new(tape_ws, tape_sat_ad);

        // Process a sine sweep and verify bounded output
        for i in 0..256 {
            let x = libm::sinf(i as f32 * 0.1) * 2.0;
            let y = adaa.process(x);
            assert!(
                y.is_finite() && y.abs() < 2.0,
                "tape ADAA out of bounds at sample {i}: {y}"
            );
        }
    }

    #[test]
    fn test_adaa_asymmetric_clip() {
        let mut adaa = Adaa1::new(asymmetric_clip, asymmetric_clip_ad);

        // Verify output is bounded and finite
        for i in 0..256 {
            let x = libm::sinf(i as f32 * 0.05) * 3.0;
            let y = adaa.process(x);
            assert!(
                y.is_finite() && y.abs() < 2.0,
                "asymmetric ADAA out of bounds at sample {i}: {y}"
            );
        }
    }

    #[test]
    fn test_adaa_midpoint_fallback() {
        // Feed identical samples to trigger the epsilon fallback
        let mut adaa = Adaa1::new(soft_clip, soft_clip_ad);

        // Prime with a value
        let _ = adaa.process(0.5);
        // Same value again — should use midpoint fallback
        let y = adaa.process(0.5);
        // Midpoint of (0.5, 0.5) = 0.5, so f(0.5) = tanh(0.5) ≈ 0.4621
        let expected = soft_clip(0.5);
        assert!(
            (y - expected).abs() < 1e-5,
            "midpoint fallback: got {y}, expected {expected}"
        );
    }

    #[test]
    fn test_adaa_reset() {
        let mut adaa = Adaa1::new(soft_clip, soft_clip_ad);

        // Process some samples
        let _ = adaa.process(1.0);
        let _ = adaa.process(2.0);

        // Reset and verify state is cleared
        adaa.reset();
        // After reset, processing 0.0 should use midpoint of (0, 0) = f(0) = 0
        let y = adaa.process(0.0);
        assert!(
            y.abs() < 1e-6,
            "after reset, process(0) should be ~0, got {y}"
        );
    }

    #[test]
    fn test_adaa_process_block() {
        let mut adaa = Adaa1::new(soft_clip, soft_clip_ad);
        let mut block = vec![0.0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7];
        adaa.process_block(&mut block);

        // All outputs should be bounded by tanh range
        for (i, &y) in block.iter().enumerate() {
            assert!(
                y.is_finite() && y.abs() < 1.0,
                "block output out of bounds at {i}: {y}"
            );
        }
    }

    #[test]
    fn test_adaa_dc_preservation() {
        // Constant input → output should converge to f(dc)
        let mut adaa = Adaa1::new(soft_clip, soft_clip_ad);
        let dc = 0.7;
        let expected = soft_clip(dc);

        // After initial transient, output should equal f(dc)
        let mut last = 0.0;
        for _ in 0..100 {
            last = adaa.process(dc);
        }
        assert!(
            (last - expected).abs() < 1e-5,
            "DC preservation: got {last}, expected {expected}"
        );
    }

    /// SNR of `Adaa1` against exact f64 ADAA of the same f32 inputs, for a
    /// sine of `amp` at `freq` Hz (48 kHz, one second).
    fn adaa_snr_db<F, AF>(
        mut adaa: Adaa1<F, AF>,
        exact_ad: impl Fn(f64) -> f64,
        exact_f: impl Fn(f64) -> f64,
        amp: f64,
        freq: f64,
    ) -> f64
    where
        F: Fn(f32) -> f32,
        AF: Fn(f32) -> f32,
    {
        let input = |n: i32| {
            (amp * libm::sin(2.0 * core::f64::consts::PI * freq * n as f64 / 48_000.0)) as f32
                as f64
        };
        let (mut signal, mut noise) = (0.0, 0.0);
        for n in 0..48_000 {
            let (x0, x1) = (input(n - 1), input(n));
            let y = f64::from(adaa.process(x1 as f32));
            let exact = if x1 != x0 {
                (exact_ad(x1) - exact_ad(x0)) / (x1 - x0)
            } else {
                exact_f(x1)
            };
            if n > 0 {
                signal += exact * exact;
                noise += (y - exact) * (y - exact);
            }
        }
        10.0 * libm::log10(signal / noise.max(1e-300))
    }

    #[test]
    fn test_adaa_quiet_input_stays_clean() {
        // Regression: soft_clip_ad was ln(2·cosh x), which sits near ln 2 for
        // quiet input, so F(x₁) − F(x₀) lost its low bits to the constant.
        // A −40 dBFS guitar-range sine came out ~16–27 dB above its own
        // rounding noise: audible grit on every soft-clipping kernel.
        let ln_cosh = |x: f64| libm::log(libm::cosh(x));
        for amp in [0.001, 0.01, 0.1, 0.5] {
            for freq in [82.0, 220.0, 880.0] {
                let snr = adaa_snr_db(
                    Adaa1::new(soft_clip, soft_clip_ad),
                    ln_cosh,
                    libm::tanh,
                    amp,
                    freq,
                );
                assert!(
                    snr > 85.0,
                    "soft clip at {amp} × {freq} Hz: SNR {snr:.1} dB"
                );
            }
        }
        // Tape: 1 − e^(−2x) above zero, −1 + e^(1.8x) below.
        let tape_ad = |x: f64| {
            if x >= 0.0 {
                x + (libm::exp(-2.0 * x) - 1.0) / 2.0
            } else {
                -x + (libm::exp(1.8 * x) - 1.0) / 1.8
            }
        };
        let tape_f = |x: f64| {
            if x >= 0.0 {
                1.0 - libm::exp(-2.0 * x)
            } else {
                -1.0 + libm::exp(1.8 * x)
            }
        };
        let tape_ws = |x: f32| {
            if x >= 0.0 {
                1.0 - libm::expf(-2.0 * x)
            } else {
                -1.0 + libm::expf(1.8 * x)
            }
        };
        for amp in [0.001, 0.01, 0.1, 0.5] {
            let snr = adaa_snr_db(
                Adaa1::new(tape_ws, tape_sat_ad),
                tape_ad,
                tape_f,
                amp,
                220.0,
            );
            assert!(snr > 85.0, "tape at {amp}: SNR {snr:.1} dB");
        }
    }
}
