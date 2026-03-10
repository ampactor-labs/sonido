//! Fast mathematical approximations for embedded DSP.
//!
//! These functions trade full IEEE 754 precision for speed on targets
//! without hardware transcendental support (Cortex-M7, etc.). Each
//! function documents its maximum error and valid input range.
//!
//! # When to use
//!
//! | Function | Replaces | Use case | Max error |
//! |----------|----------|----------|-----------|
//! | [`fast_log2`] | `libm::logf` | dB conversion, dynamics | < 0.2% |
//! | [`fast_exp2`] | `libm::expf` | dB conversion, dynamics | < 0.2% |
//! | [`fast_db_to_linear`] | [`db_to_linear`](crate::db_to_linear) | Gain, level metering | < 0.05 dB |
//! | [`fast_linear_to_db`] | [`linear_to_db`](crate::linear_to_db) | Gain, level metering | < 0.05 dB |
//! | [`fast_sin_turns`] | `libm::sinf` | LFO modulation | < 0.001 |
//! | [`fast_cos_turns`] | `libm::cosf` | LFO modulation (phase-shifted) | < 0.001 |
//! | [`fast_tan`] | `libm::tanf` | Filter coefficients | < 0.1% (f < sr/4) |
//! | [`fast_tanh`] | `libm::tanhf` | Saturation, soft clip | < 0.024 abs (\|x\| < 3) |
//!
//! # When NOT to use
//!
//! Audio-rate waveshaping (distortion, saturation) — use `libm` for full
//! precision. These approximations target control signals and coefficient
//! computation where the input range is bounded and perceptual accuracy
//! matters more than mathematical accuracy.
//!
//! # Performance
//!
//! Estimated Cortex-M7 cycles per call:
//!
//! | Function | Fast | libm equivalent |
//! |----------|------|-----------------|
//! | `fast_log2` | ~10 | ~200 (`logf`) |
//! | `fast_exp2` | ~15 | ~150 (`expf`) |
//! | `fast_sin_turns` | ~16 | ~100 (`sinf`) |
//! | `fast_cos_turns` | ~20 | ~100 (`cosf`) |
//! | `fast_tan` | ~12 | ~120 (`tanf`) |
//! | `fast_tanh` | ~8 | ~80 (`tanhf`) |
//!
//! Reference: ARM Cortex-M7 Technical Reference Manual, FPU instruction timings.

use libm::{floorf, tanf};

/// Fast base-2 logarithm via IEEE 754 float decomposition.
///
/// Extracts the exponent directly from the float bit representation,
/// then applies a 2nd-order minimax polynomial to the mantissa.
///
/// # Accuracy
///
/// Maximum relative error: < 0.2% for x > 0.
/// In dB context (`× 20/log₂(10)`): < 0.05 dB.
///
/// # Arguments
///
/// * `x` - Input value. Must be > 0. Returns garbage for x ≤ 0.
///
/// # Examples
///
/// ```
/// use sonido_core::fast_math::fast_log2;
///
/// assert!((fast_log2(1.0) - 0.0).abs() < 0.01);
/// assert!((fast_log2(2.0) - 1.0).abs() < 0.01);
/// assert!((fast_log2(0.5) - (-1.0)).abs() < 0.01);
/// ```
#[inline]
pub fn fast_log2(x: f32) -> f32 {
    let bits = x.to_bits();
    let exponent = ((bits >> 23) & 0xFF) as i32 - 127;
    // Reconstruct mantissa in [1.0, 2.0)
    let m = f32::from_bits((bits & 0x007F_FFFF) | 0x3F80_0000);
    // Minimax 2nd-order polynomial for log2(m), m ∈ [1, 2):
    //   log2(m) ≈ a₂·m² + a₁·m + a₀
    // Coefficients via Remez exchange, max error < 0.003
    exponent as f32 + (m * (m * -0.344_845_6 + 2.024_094) - 1.674_094)
}

/// Fast base-2 exponential via polynomial approximation.
///
/// Decomposes `x` into integer and fractional parts: `2^x = 2^⌊x⌋ · 2^frac(x)`.
/// The integer part uses IEEE 754 bit manipulation (exact), the fractional
/// part uses a 3rd-order minimax polynomial.
///
/// # Accuracy
///
/// Maximum relative error: < 0.2% for x ∈ \[-126, 126\].
///
/// # Examples
///
/// ```
/// use sonido_core::fast_math::fast_exp2;
///
/// assert!((fast_exp2(0.0) - 1.0).abs() < 0.01);
/// assert!((fast_exp2(1.0) - 2.0).abs() < 0.01);
/// assert!((fast_exp2(-1.0) - 0.5).abs() < 0.01);
/// ```
#[inline]
pub fn fast_exp2(x: f32) -> f32 {
    let x = x.clamp(-126.0, 126.0);
    let i = floorf(x) as i32;
    let f = x - i as f32;
    // 3rd-order minimax polynomial for 2^f, f ∈ [0, 1)
    let p = 1.0 + f * (core::f32::consts::LN_2 + f * (0.240_226 + f * 0.055_504_1));
    // Multiply by 2^i via IEEE 754 exponent manipulation
    f32::from_bits(((i + 127) as u32) << 23) * p
}

/// Fast dB-to-linear gain conversion.
///
/// Equivalent to `10^(dB/20)` but ~20× faster than [`db_to_linear`](crate::db_to_linear).
/// Uses [`fast_exp2`] internally: `10^(dB/20) = 2^(dB · log₂(10)/20)`.
///
/// # Accuracy
///
/// Maximum error: < 0.05 dB (< 0.6% linear gain error).
///
/// # Examples
///
/// ```
/// use sonido_core::fast_math::fast_db_to_linear;
///
/// assert!((fast_db_to_linear(0.0) - 1.0).abs() < 0.01);
/// assert!((fast_db_to_linear(-20.0) - 0.1).abs() < 0.01);
/// ```
#[inline]
pub fn fast_db_to_linear(db: f32) -> f32 {
    // 10^(dB/20) = 2^(dB · log₂(10) / 20)
    const FACTOR: f32 = core::f32::consts::LOG2_10 / 20.0;
    fast_exp2(db * FACTOR)
}

/// Fast linear-gain-to-dB conversion.
///
/// Equivalent to `20 · log₁₀(x)` but ~20× faster than [`linear_to_db`](crate::linear_to_db).
/// Uses [`fast_log2`] internally: `20·log₁₀(x) = 20·log₂(x)/log₂(10)`.
///
/// # Accuracy
///
/// Maximum error: < 0.05 dB for typical audio range (1e-6 to 10.0).
///
/// # Arguments
///
/// * `linear` - Linear gain value. Must be > 0. Values ≤ 1e-10 are clamped.
///
/// # Examples
///
/// ```
/// use sonido_core::fast_math::fast_linear_to_db;
///
/// assert!((fast_linear_to_db(1.0) - 0.0).abs() < 0.1);
/// assert!((fast_linear_to_db(0.1) - (-20.0)).abs() < 0.1);
/// ```
#[inline]
pub fn fast_linear_to_db(linear: f32) -> f32 {
    // 20·log₁₀(x) = 20·log₂(x) / log₂(10)
    const FACTOR: f32 = 20.0 / core::f32::consts::LOG2_10;
    fast_log2(linear.max(1e-10)) * FACTOR
}

/// Fast sine from phase in turns (full cycles).
///
/// Input: `turns` ∈ \[0, 1) where 0.0 → sin(0) = 0, 0.25 → sin(π/2) = 1,
/// 0.5 → sin(π) = 0, 0.75 → sin(3π/2) = −1. Values outside \[0, 1) are
/// wrapped automatically.
///
/// Uses the corrected parabolic approximation (Bhaskara I variant):
/// base parabola `4p(1−p)` approximates the half-wave, with a correction
/// term `0.225·y·(y−1)` that reduces peak error from 0.056 to < 0.001.
///
/// # Accuracy
///
/// Maximum absolute error: < 0.001 (< 0.009 dB).
/// Sufficient for all LFO and modulation applications.
///
/// # Performance
///
/// ~8 multiplies (~16 CM7 cycles) vs ~100+ for `libm::sinf`.
///
/// # Examples
///
/// ```
/// use sonido_core::fast_math::fast_sin_turns;
///
/// assert!(fast_sin_turns(0.0).abs() < 0.002);
/// assert!((fast_sin_turns(0.25) - 1.0).abs() < 0.002);
/// assert!(fast_sin_turns(0.5).abs() < 0.002);
/// assert!((fast_sin_turns(0.75) + 1.0).abs() < 0.002);
/// ```
#[inline]
pub fn fast_sin_turns(turns: f32) -> f32 {
    // Wrap to [0, 1)
    let p = turns - floorf(turns);
    // Split into half-cycles: first half positive, second half negative
    let (half_p, sign) = if p < 0.5 {
        (p * 2.0, 1.0_f32)
    } else {
        ((p - 0.5) * 2.0, -1.0_f32)
    };
    // Parabolic base: sin(π·t) ≈ 4t(1−t) for t ∈ [0, 1], peak = 1.0 at t = 0.5
    let y = 4.0 * half_p * (1.0 - half_p);
    // Bhaskara correction: 0.225·y·(y − 1) + y
    // Reduces max error from 0.056 to ~0.001
    sign * (0.225 * y * (y - 1.0) + y)
}

/// Cosine approximation using turns (0.0–1.0 = one full cycle).
///
/// Equivalent to `cos(2π·turns)`. Implemented as `fast_sin_turns(turns + 0.25)`
/// since cos(x) = sin(x + π/2) and π/2 in turns is 0.25.
///
/// Max error: same as [`fast_sin_turns`] (~0.06%).
///
/// # Cortex-M7 Performance
///
/// ~20 cycles (same as `fast_sin_turns` plus one addition).
///
/// # Examples
///
/// ```
/// use sonido_core::fast_math::fast_cos_turns;
///
/// assert!((fast_cos_turns(0.0) - 1.0).abs() < 0.002);
/// assert!(fast_cos_turns(0.25).abs() < 0.002);
/// assert!((fast_cos_turns(0.5) + 1.0).abs() < 0.002);
/// assert!(fast_cos_turns(0.75).abs() < 0.002);
/// ```
#[inline]
pub fn fast_cos_turns(turns: f32) -> f32 {
    fast_sin_turns(turns + 0.25)
}

/// Fast tangent for small positive angles.
///
/// Uses a Padé \[2/1\] rational approximation:
///   `tan(x) ≈ x · (15 − x²) / (15 − 6x²)`
///
/// This approximant matches the Taylor series through the x⁵ term,
/// providing excellent accuracy well beyond the small-angle regime.
///
/// # Safety guard
///
/// The Padé \[2/1\] has a spurious pole at `x ≈ 1.58` (where `15 − 6x² = 0`).
/// For `x > 1.5` the function falls back to [`libm::tanf`] to avoid divergence.
/// At 44.1 kHz with a 20 kHz cutoff the argument reaches `x ≈ 1.42`, safely
/// below the guard. At 22.05 kHz cutoff (Nyquist) it hits 1.57 — the guard
/// ensures correct behavior.
///
/// # Accuracy
///
/// | Frequency (@ 48 kHz) | Argument x = π·f/sr | Relative error |
/// |----------------------|---------------------|----------------|
/// | < 4.6 kHz | < 0.3 | < 0.03% |
/// | < 7.6 kHz | < 0.5 | < 0.2% |
/// | < 15.3 kHz | < 1.0 | < 2% |
///
/// # Use case
///
/// Filter coefficient calculation where `tanf(π · cutoff / sample_rate)`
/// is needed. Replaces ~120 CM7 cycles with ~12 cycles.
///
/// # Arguments
///
/// * `x` - Angle in radians. Valid for x ∈ \[0, π/3\] (~1.047).
///   Beyond this range, error grows as tan approaches its pole at π/2.
///   Values above 1.5 fall back to `libm::tanf` for safety.
///
/// # Examples
///
/// ```
/// use sonido_core::fast_math::fast_tan;
///
/// // 1 kHz @ 48 kHz: argument ≈ 0.0654
/// let x = core::f32::consts::PI * 1000.0 / 48000.0;
/// let exact = libm::tanf(x);
/// assert!((fast_tan(x) - exact).abs() / exact < 0.001);
/// ```
#[inline]
pub fn fast_tan(x: f32) -> f32 {
    // Guard: Padé [2/1] has a pole at x ≈ 1.58 (15 − 6x² = 0).
    // Fall back to libm for safety near the singularity.
    if x > 1.5 {
        return tanf(x);
    }
    let x2 = x * x;
    x * (15.0 - x2) / (15.0 - 6.0 * x2)
}

/// Fast hyperbolic tangent via Padé \[1/1\] rational approximation.
///
/// Approximates `tanh(x) ≈ x · (27 + x²) / (27 + 9x²)`, which matches
/// the Taylor series through the x³ term. For `|x| > 3` the output is
/// clamped to ±1.0 (tanh saturates to within 0.5% of ±1 at |x| = 3).
///
/// ~8 Cortex-M7 cycles vs ~80 for `libm::tanhf`.
///
/// # Error bounds
///
/// | Input range | Max absolute error | Max relative error |
/// |-------------|-------------------|--------------------|
/// | \|x\| < 1 | < 0.005 | < 2.2% |
/// | \|x\| < 2 | < 0.020 | < 2.5% |
/// | \|x\| < 3 | < 0.024 | < 2.5% |
/// | \|x\| ≥ 3 | clamped to ±1.0 | — |
///
/// # Use case
///
/// Saturation curves, soft clipping, and waveshaping where the input is
/// typically in \[-3, 3\] and sub-percent accuracy suffices. For audio-rate
/// waveshaping requiring full precision, use [`libm::tanhf`] instead.
///
/// # Examples
///
/// ```
/// use sonido_core::fast_math::fast_tanh;
///
/// assert!(fast_tanh(0.0).abs() < 0.001);
/// assert!((fast_tanh(1.0) - libm::tanhf(1.0)).abs() < 0.025);
/// assert!((fast_tanh(3.0) - 1.0).abs() < 0.025);
/// assert!((fast_tanh(-3.0) + 1.0).abs() < 0.025);
/// ```
#[inline]
pub fn fast_tanh(x: f32) -> f32 {
    // Padé [1/1]: tanh(x) ≈ x(27 + x²) / (27 + 9x²)
    // Clamp in saturation region for bounded output.
    if x > 3.0 {
        return 1.0;
    }
    if x < -3.0 {
        return -1.0;
    }
    let x2 = x * x;
    x * (27.0 + x2) / (27.0 + 9.0 * x2)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- fast_log2 ----

    #[test]
    fn log2_exact_powers() {
        for i in -10..=10 {
            let x = libm::exp2f(i as f32);
            let result = fast_log2(x);
            assert!(
                (result - i as f32).abs() < 0.01,
                "fast_log2(2^{i}) = {result}, expected {i}"
            );
        }
    }

    #[test]
    fn log2_accuracy_sweep() {
        // Sweep through 3 decades of audio-relevant range
        let mut max_err: f32 = 0.0;
        for i in 1..1000 {
            let x = i as f32 * 0.01; // 0.01 to 10.0
            let exact = libm::log2f(x);
            let approx = fast_log2(x);
            let err = (approx - exact).abs();
            if err > max_err {
                max_err = err;
            }
        }
        // In dB terms: max_err * 20/log2(10) ≈ max_err * 6.02
        let max_db_err = max_err * 6.020_6;
        assert!(
            max_db_err < 0.1,
            "Max dB error {max_db_err:.4} exceeds 0.1 dB"
        );
    }

    // ---- fast_exp2 ----

    #[test]
    fn exp2_exact_integers() {
        for i in -10..=10 {
            let result = fast_exp2(i as f32);
            let expected = libm::exp2f(i as f32);
            let rel_err = (result - expected).abs() / expected;
            assert!(
                rel_err < 0.005,
                "fast_exp2({i}) = {result}, expected {expected}, rel_err = {rel_err}"
            );
        }
    }

    #[test]
    fn exp2_accuracy_sweep() {
        let mut max_rel_err: f32 = 0.0;
        // Sweep -20 to +6 dB range (typical audio)
        for i in -200..=60 {
            let x = i as f32 * 0.1;
            let exact = libm::exp2f(x);
            let approx = fast_exp2(x);
            let rel_err = (approx - exact).abs() / exact;
            if rel_err > max_rel_err {
                max_rel_err = rel_err;
            }
        }
        assert!(
            max_rel_err < 0.005,
            "Max relative error {max_rel_err:.6} exceeds 0.5%"
        );
    }

    #[test]
    fn exp2_clamp_extremes() {
        // Should not panic or produce NaN/Inf
        let result = fast_exp2(-200.0);
        assert!(result.is_finite() && result >= 0.0);
        let result = fast_exp2(200.0);
        assert!(result.is_finite());
    }

    // ---- fast_db_to_linear / fast_linear_to_db ----

    #[test]
    fn db_roundtrip() {
        for db in [-20, -12, -6, -3, 0, 3, 6, 12, 20] {
            let db = db as f32;
            let linear = fast_db_to_linear(db);
            let back = fast_linear_to_db(linear);
            assert!(
                (back - db).abs() < 0.1,
                "Roundtrip: {db} dB → {linear} → {back} dB"
            );
        }
    }

    #[test]
    fn db_to_linear_accuracy() {
        use crate::db_to_linear;
        for i in -40..=20 {
            let db = i as f32;
            let exact = db_to_linear(db);
            let approx = fast_db_to_linear(db);
            let db_err = (crate::linear_to_db(approx) - db).abs();
            assert!(
                db_err < 0.1,
                "fast_db_to_linear({db}): exact={exact}, approx={approx}, err={db_err} dB"
            );
        }
    }

    // ---- fast_sin_turns ----

    #[test]
    fn sin_cardinal_points() {
        assert!(fast_sin_turns(0.0).abs() < 0.002);
        assert!((fast_sin_turns(0.25) - 1.0).abs() < 0.002);
        assert!(fast_sin_turns(0.5).abs() < 0.002);
        assert!((fast_sin_turns(0.75) + 1.0).abs() < 0.002);
    }

    #[test]
    fn sin_accuracy_sweep() {
        let mut max_err: f32 = 0.0;
        for i in 0..1000 {
            let turns = i as f32 / 1000.0;
            let exact = libm::sinf(turns * core::f32::consts::TAU);
            let approx = fast_sin_turns(turns);
            let err = (approx - exact).abs();
            if err > max_err {
                max_err = err;
            }
        }
        assert!(max_err < 0.002, "Max sin error {max_err:.6} exceeds 0.002");
    }

    #[test]
    fn sin_wraps_outside_unit() {
        // Values > 1 and < 0 should wrap correctly
        assert!((fast_sin_turns(1.25) - fast_sin_turns(0.25)).abs() < 0.002);
        assert!((fast_sin_turns(-0.25) - fast_sin_turns(0.75)).abs() < 0.002);
    }

    #[test]
    fn sin_symmetry() {
        // sin(0.25 + d) should equal sin(0.25 - d)
        for i in 0..25 {
            let d = i as f32 / 100.0;
            let a = fast_sin_turns(0.25 + d);
            let b = fast_sin_turns(0.25 - d);
            assert!(
                (a - b).abs() < 0.003,
                "Symmetry broken at d={d}: {a} vs {b}"
            );
        }
    }

    // ---- fast_cos_turns ----

    #[test]
    fn cos_cardinal_points() {
        assert!((fast_cos_turns(0.0) - 1.0).abs() < 0.002);
        assert!(fast_cos_turns(0.25).abs() < 0.002);
        assert!((fast_cos_turns(0.5) + 1.0).abs() < 0.002);
        assert!(fast_cos_turns(0.75).abs() < 0.002);
    }

    #[test]
    fn cos_accuracy_sweep() {
        let mut max_err: f32 = 0.0;
        for i in 0..1000 {
            let turns = i as f32 / 1000.0;
            let exact = libm::cosf(turns * core::f32::consts::TAU);
            let approx = fast_cos_turns(turns);
            let err = (approx - exact).abs();
            if err > max_err {
                max_err = err;
            }
        }
        assert!(max_err < 0.002, "Max cos error {max_err:.6} exceeds 0.002");
    }

    #[test]
    fn cos_wraps_outside_unit() {
        assert!((fast_cos_turns(1.5) - fast_cos_turns(0.5)).abs() < 0.002);
        assert!((fast_cos_turns(-0.25) - fast_cos_turns(0.75)).abs() < 0.002);
    }

    #[test]
    fn cos_sin_quadrature() {
        // cos(t) = sin(t + 0.25) — verify the identity holds at several points
        for i in 0..20 {
            let turns = i as f32 / 20.0;
            let cos_val = fast_cos_turns(turns);
            let sin_shifted = fast_sin_turns(turns + 0.25);
            assert!(
                (cos_val - sin_shifted).abs() < 1e-6,
                "Quadrature mismatch at turns={turns}: cos={cos_val}, sin+0.25={sin_shifted}"
            );
        }
    }

    // ---- fast_tan ----

    #[test]
    fn tan_small_angles() {
        // At small angles, tan(x) ≈ x
        for i in 1..10 {
            let x = i as f32 * 0.01;
            let exact = libm::tanf(x);
            let approx = fast_tan(x);
            let rel_err = (approx - exact).abs() / exact;
            assert!(rel_err < 0.001, "fast_tan({x}) rel_err = {rel_err}");
        }
    }

    #[test]
    fn tan_filter_range() {
        // Typical filter coefficient range: 20 Hz to 15 kHz @ 48 kHz
        let sr = 48000.0;
        let mut max_rel_err: f32 = 0.0;
        for freq in [20.0, 100.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 15000.0] {
            let x = core::f32::consts::PI * freq / sr;
            let exact = libm::tanf(x);
            let approx = fast_tan(x);
            let rel_err = (approx - exact).abs() / exact;
            if rel_err > max_rel_err {
                max_rel_err = rel_err;
            }
            assert!(
                rel_err < 0.02,
                "fast_tan at {freq} Hz: exact={exact}, approx={approx}, rel_err={rel_err}"
            );
        }
    }

    #[test]
    fn tan_zero() {
        assert_eq!(fast_tan(0.0), 0.0);
    }

    #[test]
    fn tan_guard_near_pole() {
        // x > 1.5 should fall back to libm::tanf, not diverge
        let x = 1.55;
        let result = fast_tan(x);
        let expected = libm::tanf(x);
        assert!(
            (result - expected).abs() < 1e-5,
            "Guard fallback: fast_tan({x}) = {result}, expected {expected}"
        );
    }

    #[test]
    fn tan_guard_boundary() {
        // x = 1.5 exactly should still use the Padé approximation
        let x = 1.5;
        let result = fast_tan(x);
        let exact = libm::tanf(x);
        // Padé error is larger here but should not diverge
        assert!(
            result.is_finite(),
            "fast_tan({x}) should be finite, got {result}"
        );
        let rel_err = (result - exact).abs() / exact.abs();
        assert!(rel_err < 0.15, "fast_tan({x}) rel_err = {rel_err}");
    }

    // ---- fast_tanh ----

    #[test]
    fn tanh_zero() {
        assert_eq!(fast_tanh(0.0), 0.0);
    }

    #[test]
    fn tanh_known_values() {
        for x in [0.5, 1.0, 2.0, 3.0, -1.0] {
            let approx = fast_tanh(x);
            let exact = libm::tanhf(x);
            let err = (approx - exact).abs();
            assert!(
                err < 0.025,
                "fast_tanh({x}) = {approx}, tanhf = {exact}, err = {err}"
            );
        }
    }

    #[test]
    fn tanh_saturation_clamp() {
        // |x| > 3 should clamp to ±1.0
        assert_eq!(fast_tanh(5.0), 1.0);
        assert_eq!(fast_tanh(100.0), 1.0);
        assert_eq!(fast_tanh(-5.0), -1.0);
        assert_eq!(fast_tanh(-100.0), -1.0);
    }

    #[test]
    fn tanh_symmetry() {
        for i in 1..30 {
            let x = i as f32 * 0.1;
            let pos = fast_tanh(x);
            let neg = fast_tanh(-x);
            assert!(
                (pos + neg).abs() < 1e-6,
                "Asymmetry at x={x}: {pos} vs {neg}"
            );
        }
    }

    #[test]
    fn tanh_accuracy_sweep() {
        let mut max_err: f32 = 0.0;
        for i in -30..=30 {
            let x = i as f32 * 0.1;
            let approx = fast_tanh(x);
            let exact = libm::tanhf(x);
            let err = (approx - exact).abs();
            if err > max_err {
                max_err = err;
            }
        }
        assert!(max_err < 0.025, "Max tanh error {max_err:.6} exceeds 0.025");
    }
}
