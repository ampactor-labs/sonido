//! Scale-aware ADC-to-parameter conversion for embedded hardware.
//!
//! Maps a normalized knob reading (0.0–1.0) to a parameter's native range
//! using the scale from its [`ParamDescriptor`]. This gives `from_knobs()`-quality
//! response curves: logarithmic for frequency knobs, linear for dB/mix, power
//! curves for custom parameters.
//!
//! STEPPED parameters (discrete values like waveform shape or mode select) are
//! rounded to the nearest integer after scaling.
//!
//! # Why not `ParamDescriptor::denormalize()`?
//!
//! `denormalize()` handles Linear/Logarithmic/Power scales but intentionally
//! doesn't round — plugin hosts need fractional values for smooth automation.
//! On embedded hardware, knob jitter near step boundaries produces rapid
//! oscillation between adjacent values. Rounding eliminates this.

use sonido_core::{ParamDescriptor, ParamFlags, ParamScale};

/// Scale-aware interpolation between two values.
fn interpolate_scaled(lo: f32, hi: f32, t: f32, scale: ParamScale) -> f32 {
    match scale {
        ParamScale::Linear => lo + t * (hi - lo),
        ParamScale::Logarithmic => {
            let log_lo = libm::log2f(if lo > 1e-6 { lo } else { 1e-6 });
            let log_hi = libm::log2f(if hi > 1e-6 { hi } else { 1e-6 });
            libm::exp2f(log_lo + t * (log_hi - log_lo))
        }
        ParamScale::Power(exp) => lo + libm::powf(t, exp) * (hi - lo),
        _ => lo + t * (hi - lo),
    }
}

/// Converts a normalized ADC reading (0.0–1.0) to a parameter's native value.
///
/// Applies the parameter's scale (Linear, Logarithmic, Power) and rounds
/// STEPPED parameters to the nearest integer.
///
/// # Parameters
///
/// - `desc`: Parameter descriptor with scale, min, max, and flags.
/// - `normalized`: ADC reading normalized to 0.0–1.0.
///
/// # Returns
///
/// The parameter value in its native range (e.g., 20.0–20000.0 Hz for a
/// logarithmic frequency parameter).
///
/// # Example
///
/// ```ignore
/// use sonido_platform::adc_to_param;
/// use sonido_core::ParamDescriptor;
///
/// let desc = ParamDescriptor::frequency("Cutoff", "Cutoff", 20.0, 20000.0, 1000.0);
/// let val = adc_to_param(&desc, 0.5); // ~632 Hz (geometric mean of 20–20000)
/// ```
#[inline]
pub fn adc_to_param(desc: &ParamDescriptor, normalized: f32) -> f32 {
    let val = interpolate_scaled(desc.min, desc.max, normalized, desc.scale);
    if desc.flags.contains(ParamFlags::STEPPED) {
        libm::roundf(val)
    } else {
        val
    }
}

/// Converts a normalized ADC reading to a parameter value, biased so that
/// knob center (0.5) maps to a caller-specified "noon" sweet-spot value.
///
/// The knob travel is split at center:
/// - `[0.0, 0.5]`: interpolates from `desc.min` to `noon`
/// - `[0.5, 1.0]`: interpolates from `noon` to `desc.max`
///
/// Both halves use the descriptor's scale (Linear, Logarithmic, Power) so
/// the response curve is musically consistent across the entire range.
/// STEPPED parameters use the same split, rounded to whole steps, so the
/// noon option is what noon selects.
///
/// # Parameters
///
/// - `desc`: Parameter descriptor defining the full range and scale.
/// - `noon`: The "sweet spot" value at knob center. Clamped to `[min, max]`.
/// - `normalized`: ADC reading normalized to 0.0–1.0.
///
/// # Example
///
/// ```ignore
/// use sonido_platform::adc_to_param_biased;
/// use sonido_core::ParamDescriptor;
///
/// // Drive [0, 40] with sweet spot at 8 dB
/// let desc = ParamDescriptor::gain_db("Drive", "Drive", 0.0, 40.0, 8.0);
/// let val = adc_to_param_biased(&desc, 8.0, 0.5); // → 8.0 (noon = sweet spot)
/// let val = adc_to_param_biased(&desc, 8.0, 0.0); // → 0.0 (full min)
/// let val = adc_to_param_biased(&desc, 8.0, 1.0); // → 40.0 (full max)
/// ```
#[inline]
pub fn adc_to_param_biased(desc: &ParamDescriptor, noon: f32, normalized: f32) -> f32 {
    // STEPPED params: noon selects the noon option; the options below it share
    // the left half of the travel and those above it the right half. A noon
    // on the first option keeps the whole left half there: a mode knob that
    // turns right for the alternatives. (Mapping steps linearly instead put
    // the *middle* option at noon whatever the voicing asked for.)
    if desc.flags.contains(ParamFlags::STEPPED) {
        let noon = libm::roundf(noon.clamp(desc.min, desc.max));
        let value = if normalized <= 0.5 {
            desc.min + normalized * 2.0 * (noon - desc.min)
        } else {
            noon + (normalized - 0.5) * 2.0 * (desc.max - noon)
        };
        return libm::roundf(value);
    }

    let noon = noon.clamp(desc.min, desc.max);
    let range = desc.max - desc.min;

    // Noon always selects `noon`, even at an end of the range: a parameter
    // voiced at its maximum (a distortion's 100% mix) sweeps its whole range
    // in the left half and holds there in the right. Mapping such a knob
    // linearly instead put noon mid-range, off the voicing.
    if range < 1e-6 {
        return noon;
    }

    if normalized <= 0.5 {
        let t = normalized * 2.0; // 0.0→1.0 in bottom half
        interpolate_scaled(desc.min, noon, t, desc.scale)
    } else {
        let t = (normalized - 0.5) * 2.0; // 0.0→1.0 in top half
        interpolate_scaled(noon, desc.max, t, desc.scale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: linear param with given range.
    fn linear(min: f32, max: f32, default: f32) -> ParamDescriptor {
        ParamDescriptor::custom("P", "P", min, max, default)
    }

    /// Helper: STEPPED (discrete) param.
    fn stepped(min: f32, max: f32, default: f32) -> ParamDescriptor {
        ParamDescriptor::custom("P", "P", min, max, default).with_flags(ParamFlags::STEPPED)
    }

    #[test]
    fn biased_stepped_puts_the_noon_option_at_noon() {
        // 4-option selector [0, 3] voiced on option 1.
        let desc = stepped(0.0, 3.0, 1.0);
        assert_eq!(adc_to_param_biased(&desc, 1.0, 0.0), 0.0);
        assert_eq!(adc_to_param_biased(&desc, 1.0, 0.5), 1.0);
        assert_eq!(adc_to_param_biased(&desc, 1.0, 0.75), 2.0);
        assert_eq!(adc_to_param_biased(&desc, 1.0, 1.0), 3.0);
        // Voiced on the first option: the left half stays on it.
        assert_eq!(adc_to_param_biased(&desc, 0.0, 0.2), 0.0);
        assert_eq!(adc_to_param_biased(&desc, 0.0, 0.5), 0.0);
        assert_eq!(adc_to_param_biased(&desc, 0.0, 1.0), 3.0);
    }

    #[test]
    fn noon_at_an_end_still_selects_noon() {
        // Mix [0, 100] voiced at 100: noon is 100, the left half blends.
        let desc = linear(0.0, 100.0, 100.0);
        assert!((adc_to_param_biased(&desc, 100.0, 0.5) - 100.0).abs() < 0.01);
        assert!((adc_to_param_biased(&desc, 100.0, 0.25) - 50.0).abs() < 0.01);
        assert!(adc_to_param_biased(&desc, 100.0, 0.0).abs() < 0.01);
        // Gain [0, 20] voiced at 0: noon is 0, the right half adds gain.
        let desc = linear(0.0, 20.0, 0.0);
        assert!(adc_to_param_biased(&desc, 0.0, 0.5).abs() < 0.01);
        assert!((adc_to_param_biased(&desc, 0.0, 1.0) - 20.0).abs() < 0.01);
    }

    #[test]
    fn biased_full_range() {
        // Drive [0, 40] with noon=8: noon is 20% from min, well above 5%.
        let desc = linear(0.0, 40.0, 8.0);
        let at_zero = adc_to_param_biased(&desc, 8.0, 0.0);
        let at_one = adc_to_param_biased(&desc, 8.0, 1.0);
        assert!((at_zero - 0.0).abs() < 0.01);
        assert!((at_one - 40.0).abs() < 0.01);
    }

    #[test]
    fn biased_noon_at_center() {
        // Drive [0, 40] with noon=8: knob center should return noon.
        let desc = linear(0.0, 40.0, 8.0);
        let at_half = adc_to_param_biased(&desc, 8.0, 0.5);
        assert!((at_half - 8.0).abs() < 0.01);
    }
}
