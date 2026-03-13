//! Scale-aware ADC-to-parameter conversion for embedded hardware.
//!
//! Maps a normalized knob reading (0.0–1.0) to a parameter's native range
//! using the scale from its [`ParamDescriptor`](sonido_core::ParamDescriptor). This gives `from_knobs()`-quality
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
/// use sonido_daisy::adc_to_param;
/// use sonido_core::ParamDescriptor;
///
/// let desc = ParamDescriptor::frequency("Cutoff", "Cutoff", 20.0, 20000.0, 1000.0);
/// let val = adc_to_param(&desc, 0.5); // ~632 Hz (geometric mean of 20–20000)
/// ```
#[inline]
pub fn adc_to_param(desc: &ParamDescriptor, normalized: f32) -> f32 {
    let val = match desc.scale {
        ParamScale::Linear => desc.min + normalized * (desc.max - desc.min),
        ParamScale::Logarithmic => {
            // Logarithmic: geometric interpolation between min and max.
            // Clamp min to avoid log2(0).
            let log_min = libm::log2f(if desc.min > 1e-6 { desc.min } else { 1e-6 });
            let log_max = libm::log2f(desc.max);
            libm::exp2f(log_min + normalized * (log_max - log_min))
        }
        ParamScale::Power(exp) => {
            desc.min + libm::powf(normalized, exp) * (desc.max - desc.min)
        }
    };
    if desc.flags.contains(ParamFlags::STEPPED) {
        libm::roundf(val)
    } else {
        val
    }
}
