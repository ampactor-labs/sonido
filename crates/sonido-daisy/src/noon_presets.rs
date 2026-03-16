//! Per-effect "noon preset" values for the Hothouse 6-knob pedal.
//!
//! Each registered effect defines the parameter values that constitute
//! a "sweet spot" — the tone you get when all knobs are at center (noon).
//! These values are passed to [`adc_to_param_biased`](crate::adc_to_param_biased)
//! so that ADC 0.5 maps to the sweet spot rather than the raw midpoint
//! of the parameter range.
//!
//! The sweet-spot values match the descriptor defaults set by effect
//! authors. This module simply centralizes them for the embedded mapping
//! layer.

/// Returns the "sweet spot" value for a parameter of a named effect.
///
/// `effect_id` is the registry ID (e.g., `"distortion"`, `"delay"`).
/// `param_idx` is the parameter index within the effect.
///
/// Returns `None` if the effect is unknown or param index is out of range,
/// in which case callers should fall back to the descriptor's `default`.
pub fn noon_value(effect_id: &str, param_idx: usize) -> Option<f32> {
    let values: &[f32] = match effect_id {
        "distortion" => &[8.0, 0.0, 0.0, 0.0, 100.0, 0.0],
        "preamp" => &[0.0, 0.0, 0.0],
        "compressor" => &[
            -18.0, 4.0, 10.0, 100.0, 0.0, 6.0, 0.0, 80.0, 0.0, 0.0, 100.0,
        ],
        "gate" => &[-40.0, 1.0, 100.0, 50.0, -80.0, 3.0, 80.0, 0.0],
        "eq" => &[100.0, 0.0, 1.0, 500.0, 0.0, 1.0, 5000.0, 0.0, 1.0, 0.0],
        "wah" => &[800.0, 5.0, 50.0, 0.0, 0.0],
        "chorus" => &[1.0, 50.0, 50.0, 2.0, 0.0, 0.0, 3.0, 0.0],
        "flanger" => &[0.5, 35.0, 50.0, 50.0, 0.0, 0.0, 3.0, 0.0],
        "phaser" => &[0.3, 50.0, 6.0, 50.0, 50.0, 20.0, 4000.0, 0.0, 3.0, 0.0],
        "tremolo" => &[5.0, 50.0, 0.0, 0.0, 0.0, 3.0, 0.0],
        "delay" => &[
            300.0, 40.0, 50.0, 0.0, 20000.0, 20.0, 0.0, 0.0, 2.0, 0.0,
        ],
        "filter" => &[1000.0, 0.707, 0.0, 0.0],
        "vibrato" => &[100.0, 100.0, 0.0],
        "tape" => &[6.0, 30.0, 12000.0, 0.0, 0.3, 0.2, 0.15, 0.3, 80.0, -6.0],
        "reverb" => &[0.7, 0.5, 30.0, 10.0, 50.0, 100.0, 20000.0, 0.0],
        "limiter" => &[-6.0, -0.3, 100.0, 5.0, 0.0],
        "bitcrusher" => &[8.0, 1.0, 0.0, 100.0, 0.0],
        "ringmod" => &[440.0, 50.0, 0.0, 50.0, 0.0],
        "stage" => &[
            0.0, 100.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 120.0, 0.0, 0.0, 0.0,
        ],
        "looper" => &[0.0, 80.0, 0.0, 0.0, 100.0, 0.0],
        _ => return None,
    };
    values.get(param_idx).copied()
}
