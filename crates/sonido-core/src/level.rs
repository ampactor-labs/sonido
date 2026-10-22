//! One level contract for every Sonido effect.
//!
//! At its defaults every effect plays at the loudness of its input, and its
//! mix control does not change that loudness. One declaration per kernel makes
//! it so, [`DspKernel::LEVEL`](crate::kernel::DspKernel::LEVEL), and it is
//! *measured*, never chosen by hand: `sonido-effects` generates every kernel's
//! [`Level`] with its `calibrate` example and holds all of them to the
//! contract in its `level_contract` test.
//!
//! - [`Level::trim`] is a gain on the wet signal (or, for an effect without a
//!   mix control, on its sound before its output knob and safety limiter)
//!   that brings it to the input's loudness.
//! - [`Level::mix_correlation`] is the measured correlation `ρ` between the
//!   wet signal and the dry input. Two signals with correlation `ρ` at gains
//!   `a` and `b` sum to power `a² + b² + 2ρab`, so [`blend`] normalizes the
//!   constant-power (sine/cosine) gains `cos(πm/2)`, `sin(πm/2)` by the
//!   blend's power, `1/√(1 + ρ·sin(πm))`: constant loudness at any mix for that
//!   correlation. `ρ = 0` (reverbs, echoes) is the plain constant-power
//!   crossfade, `ρ = 1` (saturation) keeps amplitude like a linear one, and
//!   partly cancelling wets (a vibrato's comb, `ρ < 0`) get the boost they
//!   need. The sine law (not `√m`) has a finite slope at both ends, so the
//!   first turn of a mix knob fades the wet in instead of jumping to -20 dB.
//!
//! [`mix_gains`] runs per sample in every effect, so it uses no
//! transcendentals (libm's `sqrtf` is a software routine on the Cortex-M7):
//! a polynomial sine exact at the ends, and a bit-trick inverse square root.
//!
//! The reference is a strummed guitar (`sonido_analysis::signals`) peaking at
//! [`NOMINAL_PEAK_DBFS`], loudness measured as integrated LUFS (ITU-R
//! BS.1770-4).

/// The instrument level effects are calibrated at: peaks of -20 dBFS, the
/// median measured from a humbucker guitar into a Hothouse's line-level
/// converter, and close to a DI guitar recorded in a DAW.
pub const NOMINAL_PEAK_DBFS: f32 = -20.0;

/// An effect's measured level calibration (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Level {
    /// Gain on the wet signal, or on the output without a mix control.
    pub trim: f32,
    /// Correlation of the wet signal with the dry input, or `None` when the
    /// effect has no mix control.
    pub mix_correlation: Option<f32>,
}

impl Level {
    /// No calibration: unity trim, no mix control.
    pub const UNITY: Self = Self {
        trim: 1.0,
        mix_correlation: None,
    };
}

/// Dry and wet gains for `mix` (0–1) at wet/dry correlation `rho`, keeping
/// the blend's loudness constant.
#[inline]
pub fn mix_gains(mix: f32, rho: f32) -> (f32, f32) {
    let mix = mix.clamp(0.0, 1.0);
    if mix <= 0.0 {
        return (1.0, 0.0);
    }
    if mix >= 1.0 {
        return (0.0, 1.0);
    }
    let (dry, wet) = (quarter_sine(1.0 - mix), quarter_sine(mix));
    // Normalize by the blend's actual power, so the polynomial's own
    // deviation from constant power cancels too.
    let k = inv_sqrt((dry * dry + wet * wet + 2.0 * rho * dry * wet).max(1e-3));
    (dry * k, wet * k)
}

/// `sin(πx/2)` for `x` in 0–1: a fifth-order odd polynomial (Taylor, with the
/// last coefficient fitted so `x = 1` gives exactly 1). Monotonic, within
/// 6e-4 of the sine.
#[inline]
fn quarter_sine(x: f32) -> f32 {
    const A: f32 = core::f32::consts::FRAC_PI_2;
    const B: f32 = -0.645_964_1; // -(π/2)³/6
    const C: f32 = 1.0 - A - B;
    let x2 = x * x;
    x * (A + x2 * (B + x2 * C))
}

/// `1/√x` for positive `x`: the bit-trick estimate and two Newton steps,
/// within 5 ppm.
#[inline]
fn inv_sqrt(x: f32) -> f32 {
    let mut y = f32::from_bits(0x5f37_5a86 - (x.to_bits() >> 1));
    for _ in 0..2 {
        y *= 1.5 - 0.5 * x * y * y;
    }
    y
}

/// Blend `dry` and `wet` by `mix` (0–1) under `level`: the wet is trimmed,
/// then the two are combined at constant loudness for the measured
/// correlation.
#[inline]
pub fn blend(dry: f32, wet: f32, mix: f32, level: &Level) -> f32 {
    let (gd, gw) = mix_gains(mix, level.mix_correlation.unwrap_or(0.0));
    dry * gd + wet * level.trim * gw
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_are_dry_and_trimmed_wet() {
        let level = Level {
            trim: 2.0,
            mix_correlation: Some(0.3),
        };
        assert_eq!(blend(0.3, 0.5, 0.0, &level), 0.3);
        assert_eq!(blend(0.3, 0.5, 1.0, &level), 1.0);
    }

    #[test]
    fn blend_power_is_constant_for_its_correlation() {
        // Unit-power signals with correlation rho: a² + b² + 2ρab = 1.
        for rho in [-0.4f32, 0.0, 0.5, 1.0] {
            for i in 1..100 {
                let m = i as f32 / 100.0;
                let (a, b) = mix_gains(m, rho);
                let power = a * a + b * b + 2.0 * rho * a * b;
                assert!((power - 1.0).abs() < 2e-5, "rho {rho} mix {m}: {power}");
            }
        }
    }

    #[test]
    fn mix_fades_in_from_zero() {
        // A smoother's residue near 0 leaks proportionally, not as its root.
        let (dry, wet) = mix_gains(1e-6, 0.0);
        assert!(wet < 2e-6 && (dry - 1.0).abs() < 1e-5, "{dry} {wet}");
        let mut prev = 0.0;
        for i in 1..=100 {
            let (_, wet) = mix_gains(i as f32 / 100.0, 0.0);
            assert!(wet > prev, "wet gain must rise with mix");
            prev = wet;
        }
    }

    #[test]
    fn helpers_match_libm() {
        for i in 0..=100 {
            let x = i as f32 / 100.0;
            let exact = libm::sinf(core::f32::consts::FRAC_PI_2 * x);
            assert!((quarter_sine(x) - exact).abs() < 6e-4, "sin at {x}");
            let y = 0.05 + 2.0 * x;
            assert!(
                (inv_sqrt(y) * libm::sqrtf(y) - 1.0).abs() < 1e-5,
                "1/sqrt at {y}"
            );
        }
        assert_eq!(quarter_sine(1.0), 1.0);
    }
}
