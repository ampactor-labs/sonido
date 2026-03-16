//! Gain staging helpers for the universal output level contract.
//!
//! Every effect in sonido exposes an output level parameter as the *last* index
//! in its [`ParameterInfo`](crate::ParameterInfo) implementation. This module
//! provides the shared vocabulary for that contract: a standard
//! [`SmoothedParam`] constructor, dB get/set helpers,
//! and a canonical [`ParamDescriptor`].
//!
//! # Usage
//!
//! ```rust
//! use sonido_core::gain;
//!
//! let mut output = gain::output_level_param(48000.0);
//! gain::set_output_level_db(&mut output, -3.0);
//! assert!((gain::output_level_db(&output) - (-3.0)).abs() < 0.1);
//! ```
//!
//! # Design
//!
//! The output level operates in linear gain internally (for multiplication in
//! the audio path) but exposes a dB interface for parameter get/set. The range
//! is [`OUTPUT_MIN_DB`] to [`OUTPUT_MAX_DB`] (−6 to +6 dB), clamped on set.

use crate::{ParamDescriptor, SmoothedParam, db_to_linear, linear_to_db};

/// Minimum output level in dB.
///
/// Chosen so that noon (knob at 50%) maps to 0 dB (unity):
/// `(OUTPUT_MIN_DB + OUTPUT_MAX_DB) / 2 = 0`.
pub const OUTPUT_MIN_DB: f32 = -6.0;

/// Maximum output level in dB.
pub const OUTPUT_MAX_DB: f32 = 6.0;

/// Create a [`SmoothedParam`] for the standard output level (0 dB, 10 ms smoothing).
///
/// The param stores linear gain internally. At 0 dB the value is 1.0 (unity).
pub fn output_level_param(sample_rate: f32) -> SmoothedParam {
    SmoothedParam::standard(1.0, sample_rate)
}

/// Set output level from dB, clamped to [`OUTPUT_MIN_DB`]..=[`OUTPUT_MAX_DB`].
#[inline]
pub fn set_output_level_db(param: &mut SmoothedParam, db: f32) {
    param.set_target(db_to_linear(db.clamp(OUTPUT_MIN_DB, OUTPUT_MAX_DB)));
}

/// Read output level target as dB.
#[inline]
pub fn output_level_db(param: &SmoothedParam) -> f32 {
    linear_to_db(param.target())
}

/// Standard [`ParamDescriptor`] for output level.
///
/// Use as the **last** `ParameterInfo` index for every effect.
pub fn output_param_descriptor() -> ParamDescriptor {
    ParamDescriptor::gain_db("Output", "Out", OUTPUT_MIN_DB, OUTPUT_MAX_DB, 0.0)
}

/// Exact wet-signal compensation for feedback comb resonance.
///
/// A feedback comb filter with coefficient `fb` has peak gain `1/(1-fb)`
/// at resonance frequencies. Scaling the wet signal by `1-fb` exactly
/// cancels this, guaranteeing the output never exceeds the input level.
///
/// For parallel comb banks (reverb), where 1/N averaging provides
/// additional headroom, use `sqrt(1-fb)` instead — exact compensation
/// is unnecessarily aggressive.
///
/// Reference: CCRMA PASP, "Feedback Comb Filters" — peak gain = 1/(1-|g|).
///
/// # Examples
///
/// ```rust
/// use sonido_core::gain::feedback_wet_compensation;
///
/// assert_eq!(feedback_wet_compensation(0.0), 1.0);
/// assert_eq!(feedback_wet_compensation(0.5), 0.5);
/// assert!(feedback_wet_compensation(0.95) < 0.06);
/// ```
#[inline]
pub fn feedback_wet_compensation(feedback: f32) -> f32 {
    (1.0 - feedback.clamp(0.0, 0.99)).max(0.01)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_level_roundtrip() {
        let mut param = output_level_param(48000.0);
        set_output_level_db(&mut param, -6.0);
        let db = output_level_db(&param);
        assert!((db - (-6.0)).abs() < 0.01, "Expected -6.0, got {db}");
    }

    #[test]
    fn output_level_clamping() {
        let mut param = output_level_param(48000.0);

        set_output_level_db(&mut param, -50.0);
        assert!(
            (output_level_db(&param) - OUTPUT_MIN_DB).abs() < 0.01,
            "Should clamp to min"
        );

        set_output_level_db(&mut param, 50.0);
        assert!(
            (output_level_db(&param) - OUTPUT_MAX_DB).abs() < 0.01,
            "Should clamp to max"
        );
    }

    #[test]
    fn output_descriptor_defaults() {
        let desc = output_param_descriptor();
        assert_eq!(desc.name, "Output");
        assert_eq!(desc.default, 0.0);
        assert_eq!(desc.unit, crate::ParamUnit::Decibels);
    }

    #[test]
    fn feedback_compensation_zero() {
        assert_eq!(feedback_wet_compensation(0.0), 1.0);
    }

    #[test]
    fn feedback_compensation_half() {
        let comp = feedback_wet_compensation(0.5);
        assert!((comp - 0.5).abs() < 0.01, "Expected ~0.5, got {comp}");
    }

    #[test]
    fn feedback_compensation_high() {
        let comp = feedback_wet_compensation(0.95);
        assert!((comp - 0.05).abs() < 0.01, "Expected ~0.05, got {comp}");
    }

    #[test]
    fn feedback_compensation_monotonic() {
        let mut prev = feedback_wet_compensation(0.0);
        for i in 1..100 {
            let curr = feedback_wet_compensation(i as f32 / 100.0);
            assert!(
                prev >= curr,
                "Must be monotonically decreasing: {prev} < {curr}"
            );
            prev = curr;
        }
    }

    #[test]
    fn feedback_compensation_never_negative() {
        for i in 0..=100 {
            assert!(feedback_wet_compensation(i as f32 / 100.0) > 0.0);
        }
    }
}
