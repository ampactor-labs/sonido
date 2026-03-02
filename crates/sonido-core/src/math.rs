//! Mathematical utility functions for DSP.
//!
//! Provides common DSP math operations optimized for real-time audio processing.
//! All functions are designed to be allocation-free and suitable for `no_std`.
//!
//! # Level Conversions
//!
//! - [`db_to_linear`] / [`linear_to_db`] - Convert between dB and linear gain
//!
//! # Waveshaping / Clipping
//!
//! Different clipping functions produce different harmonic characteristics:
//!
//! | Function | Character | Harmonics | Use Case |
//! |----------|-----------|-----------|----------|
//! | [`soft_clip`] | Smooth, warm | Odd | Tube amp simulation |
//! | [`hard_clip`] | Harsh, buzzy | Odd (many) | Transistor fuzz |
//! | [`foldback`] | Complex, synthy | Even + Odd | Synth distortion |
//! | [`asymmetric_clip`] | Warm, tube-like | Even + Odd | Vintage amps |
//!
//! # ADAA Antiderivatives
//!
//! First antiderivatives for use with [`Adaa1`](crate::adaa::Adaa1):
//!
//! | Waveshaper | Antiderivative | Notes |
//! |------------|----------------|-------|
//! | [`soft_clip`] (tanh) | [`soft_clip_ad`] | Numerically stable `ln(2·cosh)` |
//! | [`hard_clip`] | [`hard_clip_ad`] | Piecewise quadratic/linear |
//! | tape positive | [`tape_sat_pos_ad`] | Exponential saturation |
//! | tape negative | [`tape_sat_neg_ad`] | Asymmetric exponential |
//! | tape combined | [`tape_sat_ad`] | Piecewise with continuity correction |
//! | [`asymmetric_clip`] | [`asymmetric_clip_ad`] | Piecewise `soft_clip_ad` |
//!
//! # Utilities
//!
//! - [`lerp`] - Linear interpolation
//! - [`clamp`] - Value limiting
//! - [`hz_to_omega`] - Frequency to angular frequency
//! - [`ms_to_samples`] / [`samples_to_ms`] - Time conversions

use libm::{expf, logf, tanhf};

/// Convert decibels to linear gain.
///
/// # Arguments
/// * `db` - Value in decibels
///
/// # Returns
/// Linear gain value (e.g., 0 dB → 1.0, -6 dB → 0.5, +6 dB → 2.0)
///
/// # Example
/// ```rust
/// use sonido_core::db_to_linear;
///
/// assert!((db_to_linear(0.0) - 1.0).abs() < 0.001);
/// assert!((db_to_linear(-6.02) - 0.5).abs() < 0.01);
/// ```
#[inline]
pub fn db_to_linear(db: f32) -> f32 {
    // 10^(dB/20) = e^(dB * ln(10)/20)
    const FACTOR: f32 = core::f32::consts::LN_10 / 20.0;
    expf(db * FACTOR)
}

/// Convert linear gain to decibels.
///
/// # Arguments
/// * `linear` - Linear gain value (must be > 0)
///
/// # Returns
/// Value in decibels
///
/// # Example
/// ```rust
/// use sonido_core::linear_to_db;
///
/// assert!((linear_to_db(1.0) - 0.0).abs() < 0.001);
/// assert!((linear_to_db(0.5) - (-6.02)).abs() < 0.01);
/// ```
#[inline]
pub fn linear_to_db(linear: f32) -> f32 {
    // 20 * log10(linear) = 20 * ln(linear) / ln(10)
    const FACTOR: f32 = 20.0 / core::f32::consts::LN_10;
    logf(linear.max(1e-10)) * FACTOR
}

/// Fast hyperbolic tangent approximation.
///
/// Uses the actual tanh function from libm for accuracy.
/// This is suitable for soft clipping and saturation effects.
///
/// # Arguments
/// * `x` - Input value
///
/// # Returns
/// tanh(x), in range (-1, 1)
#[inline]
pub fn fast_tanh(x: f32) -> f32 {
    tanhf(x)
}

/// Soft clip using hyperbolic tangent.
///
/// Smooth saturation that approaches ±1 asymptotically.
/// Produces primarily odd harmonics, similar to tube amplifiers.
///
/// # Arguments
/// * `x` - Input sample (any range)
///
/// # Returns
/// Soft-clipped output in range (-1, 1)
#[inline]
pub fn soft_clip(x: f32) -> f32 {
    tanhf(x)
}

/// Hard clip to ±threshold range.
///
/// Abrupt limiting that creates flat tops on waveforms.
/// Produces harsh odd harmonics.
///
/// # Arguments
/// * `x` - Input sample
/// * `threshold` - Clipping threshold (default 1.0)
///
/// # Returns
/// Hard-clipped output in range [-threshold, threshold]
#[inline]
pub fn hard_clip(x: f32, threshold: f32) -> f32 {
    x.clamp(-threshold, threshold)
}

/// Foldback distortion (closed-form triangle-wave reflection).
///
/// When |x| exceeds threshold, the signal "folds" back instead of clipping.
/// Creates rich harmonic content, popular in synthesizers.
///
/// Uses a closed-form computation based on modular arithmetic to map any
/// input magnitude into `[-threshold, threshold]` in constant time,
/// replacing the previous iterative approach.
///
/// The folding pattern is equivalent to a triangle wave with period `2·threshold`:
/// normalize into half-periods, take the fractional part, then flip the sign
/// on odd half-periods.
///
/// # Arguments
/// * `input` - Input sample
/// * `threshold` - Folding threshold (must be > 0; returns 0.0 if ≤ 0)
///
/// # Returns
/// Foldback-distorted output in `[-threshold, threshold]`
#[inline]
pub fn foldback(input: f32, threshold: f32) -> f32 {
    use libm::floorf;

    if threshold <= 0.0 {
        return 0.0;
    }
    if input.abs() <= threshold {
        return input;
    }
    let t2 = 2.0 * threshold;
    let normalized = (input + threshold) / t2;
    let folded = (normalized - floorf(normalized)) * t2 - threshold;
    // Flip sign on odd half-periods to produce triangle-wave reflection.
    let period = floorf(normalized) as i32;
    if period % 2 == 0 { folded } else { -folded }
}

/// Asymmetric soft clipping.
///
/// Positive and negative halves clip differently, producing
/// both even and odd harmonics (warmer, tube-like character).
///
/// # Arguments
/// * `x` - Input sample
///
/// # Returns
/// Asymmetrically clipped output
#[inline]
pub fn asymmetric_clip(x: f32) -> f32 {
    if x >= 0.0 {
        // Positive: gentler clipping
        tanhf(x)
    } else {
        // Negative: harder clipping (reaches limit faster)
        tanhf(x * 1.5) / 1.5 * 1.2
    }
}

// ---------------------------------------------------------------------------
// Antiderivative companion functions for ADAA
// ---------------------------------------------------------------------------
//
// Each `_ad` function is the first antiderivative of the corresponding
// waveshaper above.  ADAA processors use F(x₁) − F(x₀) to approximate the
// continuous-time convolution, so constant offsets are irrelevant.
//
// Reference: Parker et al., "Reducing the Aliasing of Nonlinear Waveshaping
// Using Continuous-Time Convolution", DAFx-2016.

/// First antiderivative of [`soft_clip`] (`tanh`).
///
/// Computes `ln(2·cosh(x))` using the numerically stable identity:
///
/// ```text
/// ln(2·cosh(x)) = |x| + ln(1 + exp(−2|x|))
/// ```
///
/// The direct formula `ln(cosh(x))` overflows for `|x| > ~89` because
/// `cosh(x)` exceeds `f32::MAX`. This identity keeps the `exp` argument
/// always ≤ 0, preventing overflow entirely.
///
/// Differs from the true `ln(cosh(x))` by a constant `ln(2)`, which
/// cancels in ADAA's difference computation `F(x₁) − F(x₀)`.
///
/// # Reference
///
/// Parker et al., DAFx-2016, Section 4.1.
#[inline]
pub fn soft_clip_ad(x: f32) -> f32 {
    let abs_x = x.abs();
    abs_x + logf(1.0 + expf(-2.0 * abs_x))
}

/// First antiderivative of [`hard_clip`].
///
/// Piecewise quadratic/linear antiderivative of the hard clipper:
///
/// ```text
/// F(x) = x²/2           for |x| ≤ threshold
/// F(x) = t·|x| − t²/2   for |x| > threshold
/// ```
///
/// For use with [`Adaa1`](crate::adaa::Adaa1), capture the threshold
/// in a closure: `|x| hard_clip_ad(x, threshold)`.
///
/// # Reference
///
/// Parker et al., DAFx-2016, Section 4.2.
#[inline]
pub fn hard_clip_ad(x: f32, threshold: f32) -> f32 {
    let abs_x = x.abs();
    if abs_x <= threshold {
        x * x * 0.5
    } else {
        threshold * abs_x - threshold * threshold * 0.5
    }
}

/// First antiderivative of the positive tape-saturation branch `1 − exp(−2x)`.
///
/// ```text
/// ∫(1 − exp(−2x)) dx = x + exp(−2x) / 2
/// ```
///
/// This is the positive-input branch of the asymmetric tape saturation
/// transfer function used by `Tape`.
///
/// # Reference
///
/// Parker et al., DAFx-2016 (applied to exponential saturation curves).
#[inline]
pub fn tape_sat_pos_ad(x: f32) -> f32 {
    x + expf(-2.0 * x) * 0.5
}

/// First antiderivative of the negative tape-saturation branch `−1 + exp(1.8x)`.
///
/// ```text
/// ∫(−1 + exp(1.8x)) dx = −x + exp(1.8x) / 1.8
/// ```
///
/// This is the negative-input branch of the asymmetric tape saturation
/// transfer function used by `Tape`.
///
/// # Reference
///
/// Parker et al., DAFx-2016 (applied to exponential saturation curves).
#[inline]
pub fn tape_sat_neg_ad(x: f32) -> f32 {
    -x + expf(1.8 * x) / 1.8
}

/// First antiderivative of the combined tape-saturation waveshaper.
///
/// Piecewise continuous antiderivative of the asymmetric tape transfer
/// function:
///
/// ```text
/// f(x) = 1 − exp(−2x)    for x ≥ 0
/// f(x) = −1 + exp(1.8x)   for x < 0
/// ```
///
/// A continuity correction of `1/2 − 1/1.8` is applied to the negative
/// branch so that `F(0⁻) = F(0⁺)`.
#[inline]
pub fn tape_sat_ad(x: f32) -> f32 {
    if x >= 0.0 {
        tape_sat_pos_ad(x)
    } else {
        // Continuity: pos at 0 = 0 + 0.5 = 0.5
        //             neg at 0 = 0 + 1/1.8 = 0.5556
        //             correction = 0.5 - 1/1.8 = -1/18
        tape_sat_neg_ad(x) + (0.5 - 1.0 / 1.8)
    }
}

/// First antiderivative of [`asymmetric_clip`].
///
/// Piecewise antiderivative matching the asymmetric clipping function:
///
/// ```text
/// F(x) = ln(2·cosh(x))                        for x ≥ 0
/// F(x) = (8/15)·ln(2·cosh(1.5x)) + (7/15)·ln2 for x < 0
/// ```
///
/// The negative branch uses `(8/15)` because `asymmetric_clip` applies
/// `tanh(1.5x) · 1.2 / 1.5 = 0.8 · tanh(1.5x)`, and the chain rule
/// gives `0.8 / 1.5 = 8/15` as the antiderivative coefficient.
/// The `(7/15)·ln2` term ensures continuity at `x = 0`.
#[inline]
pub fn asymmetric_clip_ad(x: f32) -> f32 {
    if x >= 0.0 {
        soft_clip_ad(x)
    } else {
        (8.0 / 15.0) * soft_clip_ad(1.5 * x) + core::f32::consts::LN_2 * 7.0 / 15.0
    }
}

/// Soft safety limiter with transparent knee.
///
/// Signals below 90% of `ceiling` pass through unchanged (transparent).
/// Above the knee, tanh compression smoothly limits toward `ceiling`.
/// Output is bounded: `|output| <= ceiling`.
///
/// Designed as a safety backstop before output level stages. Ensures effects
/// produce bounded output even with extreme parameter combinations, without
/// coloring signals at normal operating levels.
///
/// # Arguments
/// * `x` - Input signal
/// * `ceiling` - Maximum output magnitude (e.g., 1.0 for 0 dBFS)
///
/// # Returns
/// Limited signal with `|output| <= ceiling`
///
/// # Reference
/// Knee-based soft limiter using hyperbolic tangent compression.
/// See Giannoulis et al., "Digital Dynamic Range Compressor Design" (2012)
/// for the general knee-based limiting framework.
#[inline]
pub fn soft_limit(x: f32, ceiling: f32) -> f32 {
    let threshold = ceiling * 0.9;
    if x.abs() <= threshold {
        x
    } else {
        let headroom = ceiling - threshold;
        let excess = x.abs() - threshold;
        x.signum() * (threshold + headroom * tanhf(excess / headroom))
    }
}

/// Stereo version of [`soft_limit`].
#[inline]
pub fn soft_limit_stereo(left: f32, right: f32, ceiling: f32) -> (f32, f32) {
    (soft_limit(left, ceiling), soft_limit(right, ceiling))
}

/// Linear interpolation between two values.
///
/// # Arguments
/// * `a` - Start value (at t=0)
/// * `b` - End value (at t=1)
/// * `t` - Interpolation factor (0.0 to 1.0)
///
/// # Returns
/// Interpolated value
#[inline]
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Clamp a value to a range.
///
/// # Arguments
/// * `x` - Input value
/// * `min` - Minimum value
/// * `max` - Maximum value
///
/// # Returns
/// Clamped value
#[inline]
pub fn clamp(x: f32, min: f32, max: f32) -> f32 {
    x.clamp(min, max)
}

/// Convert frequency in Hz to angular frequency (radians/sample).
///
/// # Arguments
/// * `freq_hz` - Frequency in Hz
/// * `sample_rate` - Sample rate in Hz
///
/// # Returns
/// Angular frequency in radians per sample
#[inline]
pub fn hz_to_omega(freq_hz: f32, sample_rate: f32) -> f32 {
    core::f32::consts::TAU * freq_hz / sample_rate
}

/// Convert milliseconds to samples.
///
/// # Arguments
/// * `ms` - Time in milliseconds
/// * `sample_rate` - Sample rate in Hz
///
/// # Returns
/// Time in samples
#[inline]
pub fn ms_to_samples(ms: f32, sample_rate: f32) -> f32 {
    ms * sample_rate / 1000.0
}

/// Convert samples to milliseconds.
///
/// # Arguments
/// * `samples` - Time in samples
/// * `sample_rate` - Sample rate in Hz
///
/// # Returns
/// Time in milliseconds
#[inline]
pub fn samples_to_ms(samples: f32, sample_rate: f32) -> f32 {
    samples * 1000.0 / sample_rate
}

/// Flush subnormal (denormalized) floats to zero.
///
/// Subnormal floats (~1e-38 to 1e-45) cause severe CPU performance
/// degradation on most architectures (up to 100x slowdown). This function
/// replaces values below 1e-20 with zero, providing margin before the
/// IEEE 754 subnormal range begins.
///
/// Use this in feedback loops (comb filters, delay lines, allpass chains)
/// where signal can decay indefinitely toward zero.
///
/// Reference: IEEE 754-2008, Section 3.4 (Subnormal numbers)
#[allow(clippy::inline_always)]
#[inline(always)]
pub fn flush_denormal(x: f32) -> f32 {
    if x.abs() < 1e-20 { 0.0 } else { x }
}

/// Crossfade between dry and wet signals.
///
/// Equivalent to `dry * (1 - mix) + wet * mix` but uses one fewer multiply:
/// `dry + (wet - dry) * mix`.
///
/// # Arguments
///
/// * `dry` - Unprocessed signal
/// * `wet` - Processed signal
/// * `mix` - Blend factor in \[0.0, 1.0\]: 0.0 = all dry, 1.0 = all wet
#[inline]
pub fn wet_dry_mix(dry: f32, wet: f32, mix: f32) -> f32 {
    dry + (wet - dry) * mix
}

/// Stereo crossfade between dry and wet signals.
///
/// Applies [`wet_dry_mix`] independently to left and right channels.
#[inline]
pub fn wet_dry_mix_stereo(dry_l: f32, dry_r: f32, wet_l: f32, wet_r: f32, mix: f32) -> (f32, f32) {
    (
        wet_dry_mix(dry_l, wet_l, mix),
        wet_dry_mix(dry_r, wet_r, mix),
    )
}

/// Sum stereo to mono (equal-power average).
#[inline]
pub fn mono_sum(left: f32, right: f32) -> f32 {
    (left + right) * 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_db_linear_roundtrip() {
        let original = 0.5;
        let db = linear_to_db(original);
        let back = db_to_linear(db);
        assert!(
            (original - back).abs() < 1e-5,
            "Roundtrip failed: {} -> {} -> {}",
            original,
            db,
            back
        );
    }

    #[test]
    fn test_db_known_values() {
        // 0 dB = 1.0 linear
        assert!((db_to_linear(0.0) - 1.0).abs() < 1e-6);
        // -6 dB ≈ 0.5 linear
        assert!((db_to_linear(-6.0206) - 0.5).abs() < 0.001);
        // +6 dB ≈ 2.0 linear
        assert!((db_to_linear(6.0206) - 2.0).abs() < 0.001);
    }

    #[test]
    fn test_soft_clip_bounds() {
        assert!(soft_clip(3.0) < 1.0);
        assert!(soft_clip(3.0) > 0.99);
        assert!(soft_clip(-3.0) > -1.0);
        assert!(soft_clip(-3.0) < -0.99);
    }

    #[test]
    fn test_foldback() {
        let threshold = 0.8;
        // Below threshold: unchanged
        assert!((foldback(0.5, threshold) - 0.5).abs() < 1e-6);
        // At threshold: unchanged
        assert!((foldback(0.8, threshold) - 0.8).abs() < 1e-6);
        // Above threshold: folds back
        let folded = foldback(1.0, threshold);
        assert!((folded - 0.6).abs() < 1e-6, "Expected 0.6, got {}", folded);
    }

    #[test]
    fn test_foldback_no_fold_needed() {
        assert!((foldback(0.5, 1.0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn test_foldback_single_fold() {
        assert!((foldback(1.5, 1.0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn test_foldback_extreme_input() {
        let result = foldback(100.0, 1.0);
        assert!(
            (-1.0..=1.0).contains(&result),
            "Result {} out of bounds",
            result
        );
    }

    #[test]
    fn test_foldback_zero_threshold() {
        assert_eq!(foldback(0.5, 0.0), 0.0);
        assert_eq!(foldback(-3.0, 0.0), 0.0);
        assert_eq!(foldback(0.0, 0.0), 0.0);
    }

    #[test]
    fn test_foldback_closed_form_sweep() {
        // Verify closed-form matches expected triangle-wave foldback
        // for a range of inputs at threshold = 1.0
        let threshold = 1.0;
        let cases: &[(f32, f32)] = &[
            (0.0, 0.0),
            (0.5, 0.5),
            (-0.5, -0.5),
            (1.0, 1.0),
            (-1.0, -1.0),
            (1.5, 0.5),
            (-1.5, -0.5),
            (2.0, 0.0),
            (2.5, -0.5),
            (3.0, -1.0),
            (3.5, -0.5),
            (4.0, 0.0),
            (5.0, 1.0),
            (-5.0, -1.0),
            (10.0, 0.0),
            (-10.0, 0.0),
        ];
        for &(input, expected) in cases {
            let result = foldback(input, threshold);
            assert!(
                (result - expected).abs() < 1e-5,
                "foldback({input}, {threshold}) = {result}, expected {expected}"
            );
        }
    }

    #[test]
    fn test_foldback_nonunit_threshold() {
        // Verify with threshold != 1.0
        let threshold = 0.8;
        // 1.0 exceeds 0.8 by 0.2, folds to 0.8 - 0.2 = 0.6
        assert!(
            (foldback(1.0, threshold) - 0.6).abs() < 1e-5,
            "got {}",
            foldback(1.0, threshold)
        );
        // Below threshold passes through
        assert!((foldback(0.5, threshold) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn test_lerp() {
        assert_eq!(lerp(0.0, 10.0, 0.0), 0.0);
        assert_eq!(lerp(0.0, 10.0, 0.5), 5.0);
        assert_eq!(lerp(0.0, 10.0, 1.0), 10.0);
    }

    #[test]
    fn test_ms_samples_conversion() {
        let sample_rate = 48000.0;
        let ms = 10.0;
        let samples = ms_to_samples(ms, sample_rate);
        assert_eq!(samples, 480.0);
        let back = samples_to_ms(samples, sample_rate);
        assert!((back - ms).abs() < 1e-6);
    }

    #[test]
    fn test_wet_dry_mix() {
        // All dry
        assert_eq!(wet_dry_mix(1.0, 0.5, 0.0), 1.0);
        // All wet
        assert_eq!(wet_dry_mix(1.0, 0.5, 1.0), 0.5);
        // 50/50
        assert!((wet_dry_mix(0.0, 1.0, 0.5) - 0.5).abs() < 1e-6);
        // Equivalent to dry*(1-mix)+wet*mix
        let dry = 0.3;
        let wet = 0.8;
        let mix = 0.7;
        let expected = dry * (1.0 - mix) + wet * mix;
        assert!((wet_dry_mix(dry, wet, mix) - expected).abs() < 1e-6);
    }

    #[test]
    fn test_wet_dry_mix_stereo() {
        let (l, r) = wet_dry_mix_stereo(1.0, 0.5, 0.0, 1.0, 0.5);
        assert!((l - 0.5).abs() < 1e-6);
        assert!((r - 0.75).abs() < 1e-6);
    }

    #[test]
    fn test_mono_sum() {
        assert_eq!(mono_sum(1.0, 1.0), 1.0);
        assert_eq!(mono_sum(1.0, -1.0), 0.0);
        assert_eq!(mono_sum(0.5, 0.3), 0.4);
    }

    #[test]
    fn test_flush_denormal() {
        // Normal values pass through
        assert_eq!(flush_denormal(1.0), 1.0);
        assert_eq!(flush_denormal(-0.5), -0.5);
        assert_eq!(flush_denormal(1e-10), 1e-10);

        // Subnormal-range values are flushed to zero
        assert_eq!(flush_denormal(1e-21), 0.0);
        assert_eq!(flush_denormal(-1e-21), 0.0);
        assert_eq!(flush_denormal(1e-38), 0.0);
        assert_eq!(flush_denormal(0.0), 0.0);
    }

    #[test]
    fn test_soft_limit_below_knee() {
        // Below 90% of ceiling passes through unchanged
        assert_eq!(soft_limit(0.5, 1.0), 0.5);
        assert_eq!(soft_limit(-0.5, 1.0), -0.5);
        assert_eq!(soft_limit(0.0, 1.0), 0.0);
        assert_eq!(soft_limit(0.89, 1.0), 0.89);
        assert_eq!(soft_limit(-0.89, 1.0), -0.89);
    }

    #[test]
    fn test_soft_limit_at_knee() {
        // At exactly 90% of ceiling, should pass through
        let result = soft_limit(0.9, 1.0);
        assert!((result - 0.9).abs() < 1e-6, "at knee: {result}");
    }

    #[test]
    fn test_soft_limit_above_knee() {
        // Above knee, output is compressed but <= ceiling
        let result = soft_limit(2.0, 1.0);
        assert!(result > 0.9, "should be above knee: {result}");
        assert!(result <= 1.0, "should be at or below ceiling: {result}");
    }

    #[test]
    fn test_soft_limit_extreme_input() {
        // Very large input still bounded at ceiling (tanhf saturates to 1.0)
        assert!(soft_limit(100.0, 1.0) <= 1.0);
        assert!(soft_limit(-100.0, 1.0) >= -1.0);
        assert!(soft_limit(1000.0, 1.0) <= 1.0);
    }

    #[test]
    fn test_soft_limit_symmetry() {
        // Negative mirrors positive
        let pos = soft_limit(1.5, 1.0);
        let neg = soft_limit(-1.5, 1.0);
        assert!((pos + neg).abs() < 1e-6, "not symmetric: {pos} vs {neg}");
    }

    #[test]
    fn test_soft_limit_custom_ceiling() {
        // Works with ceiling != 1.0
        assert_eq!(soft_limit(1.0, 2.0), 1.0); // below 90% of 2.0
        assert!(soft_limit(3.0, 2.0) < 2.0);
        assert!(soft_limit(3.0, 2.0) > 1.8);
    }

    #[test]
    fn test_soft_limit_stereo() {
        let (l, r) = soft_limit_stereo(0.5, 2.0, 1.0);
        assert_eq!(l, 0.5); // below knee
        assert!(r <= 1.0); // above knee, limited
        assert!(r > 0.9);
    }

    // --- Antiderivative tests ---
    //
    // Verify each _ad function against trapezoidal numerical integration
    // of the corresponding waveshaper: ∫_a^b f(x)dx ≈ F(b) − F(a).

    /// Trapezoidal integration of `f` over `[a, b]` with `n` subintervals.
    fn trapz(f: impl Fn(f32) -> f32, a: f32, b: f32, n: usize) -> f32 {
        let h = (b - a) / n as f32;
        let mut sum = 0.5 * (f(a) + f(b));
        for i in 1..n {
            sum += f(a + i as f32 * h);
        }
        sum * h
    }

    #[test]
    fn test_soft_clip_ad_vs_numerical() {
        let intervals: &[(f32, f32)] = &[(0.0, 1.0), (-2.0, 2.0), (0.5, 3.0), (-3.0, -0.5)];
        for &(a, b) in intervals {
            let numerical = trapz(soft_clip, a, b, 10_000);
            let analytical = soft_clip_ad(b) - soft_clip_ad(a);
            assert!(
                (numerical - analytical).abs() < 1e-3,
                "soft_clip_ad [{a}, {b}]: numerical={numerical}, analytical={analytical}"
            );
        }
    }

    #[test]
    fn test_soft_clip_ad_no_overflow() {
        // Large inputs that would overflow coshf
        let val = soft_clip_ad(100.0);
        assert!(val.is_finite(), "overflow at x=100: {val}");
        assert!((val - 100.0).abs() < 1e-5, "expected ~100.0, got {val}");

        let val_neg = soft_clip_ad(-100.0);
        assert!(val_neg.is_finite(), "overflow at x=-100: {val_neg}");
        assert!(
            (val_neg - 100.0).abs() < 1e-5,
            "expected ~100.0, got {val_neg}"
        );
    }

    #[test]
    fn test_hard_clip_ad_vs_numerical() {
        let threshold = 0.8;
        let f = |x: f32| hard_clip(x, threshold);
        let intervals: &[(f32, f32)] = &[
            (0.0, 0.5),   // below threshold
            (0.0, 1.5),   // crosses threshold
            (-1.5, 1.5),  // symmetric span
            (-2.0, -0.5), // negative region
        ];
        for &(a, b) in intervals {
            let numerical = trapz(f, a, b, 10_000);
            let analytical = hard_clip_ad(b, threshold) - hard_clip_ad(a, threshold);
            assert!(
                (numerical - analytical).abs() < 1e-3,
                "hard_clip_ad [{a}, {b}]: numerical={numerical}, analytical={analytical}"
            );
        }
    }

    #[test]
    fn test_tape_sat_pos_ad_vs_numerical() {
        let f = |x: f32| 1.0 - expf(-2.0 * x);
        let intervals: &[(f32, f32)] = &[(0.0, 1.0), (0.5, 3.0), (0.0, 5.0)];
        for &(a, b) in intervals {
            let numerical = trapz(f, a, b, 10_000);
            let analytical = tape_sat_pos_ad(b) - tape_sat_pos_ad(a);
            assert!(
                (numerical - analytical).abs() < 1e-3,
                "tape_sat_pos_ad [{a}, {b}]: numerical={numerical}, analytical={analytical}"
            );
        }
    }

    #[test]
    fn test_tape_sat_neg_ad_vs_numerical() {
        let f = |x: f32| -1.0 + expf(1.8 * x);
        let intervals: &[(f32, f32)] = &[(-3.0, 0.0), (-5.0, -1.0), (-2.0, -0.5)];
        for &(a, b) in intervals {
            let numerical = trapz(f, a, b, 10_000);
            let analytical = tape_sat_neg_ad(b) - tape_sat_neg_ad(a);
            assert!(
                (numerical - analytical).abs() < 1e-3,
                "tape_sat_neg_ad [{a}, {b}]: numerical={numerical}, analytical={analytical}"
            );
        }
    }

    #[test]
    fn test_tape_sat_ad_continuity() {
        // F must be continuous at x=0
        let at_zero_pos = tape_sat_ad(0.0);
        let at_zero_neg = tape_sat_ad(-1e-10);
        assert!(
            (at_zero_pos - at_zero_neg).abs() < 1e-4,
            "discontinuity at 0: pos={at_zero_pos}, neg={at_zero_neg}"
        );
    }

    #[test]
    fn test_tape_sat_ad_vs_numerical() {
        // Combined waveshaper
        let f = |x: f32| {
            if x >= 0.0 {
                1.0 - expf(-2.0 * x)
            } else {
                -1.0 + expf(1.8 * x)
            }
        };
        let intervals: &[(f32, f32)] = &[
            (-2.0, 2.0), // crosses zero
            (-1.0, 1.0),
            (0.0, 3.0),
            (-3.0, 0.0),
        ];
        for &(a, b) in intervals {
            let numerical = trapz(f, a, b, 10_000);
            let analytical = tape_sat_ad(b) - tape_sat_ad(a);
            assert!(
                (numerical - analytical).abs() < 1e-3,
                "tape_sat_ad [{a}, {b}]: numerical={numerical}, analytical={analytical}"
            );
        }
    }

    #[test]
    fn test_asymmetric_clip_ad_continuity() {
        let at_zero_pos = asymmetric_clip_ad(0.0);
        let at_zero_neg = asymmetric_clip_ad(-1e-10);
        assert!(
            (at_zero_pos - at_zero_neg).abs() < 1e-4,
            "discontinuity at 0: pos={at_zero_pos}, neg={at_zero_neg}"
        );
    }

    #[test]
    fn test_asymmetric_clip_ad_vs_numerical() {
        let intervals: &[(f32, f32)] = &[(0.0, 2.0), (-2.0, 0.0), (-2.0, 2.0), (-1.0, 1.0)];
        for &(a, b) in intervals {
            let numerical = trapz(asymmetric_clip, a, b, 10_000);
            let analytical = asymmetric_clip_ad(b) - asymmetric_clip_ad(a);
            assert!(
                (numerical - analytical).abs() < 1e-3,
                "asymmetric_clip_ad [{a}, {b}]: numerical={numerical}, analytical={analytical}"
            );
        }
    }
}
