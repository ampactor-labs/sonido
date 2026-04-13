//! Hothouse 6-knob mapping — which physical knob controls which param index.
//!
//! Maps are keyed by effect registry ID (e.g., `"distortion"`, `"reverb"`).
//! Used by `EffectSlot` in sonido-daisy and host-side test harnesses.

use crate::noon;
use crate::param_map::adc_to_param_biased;
use sonido_core::ParamDescriptor;

/// Sentinel for unmapped knob positions.
pub const NULL_KNOB: u8 = 0xFF;

/// Returns the 6-knob-to-param-index mapping for a named effect.
///
/// Returns `None` if the effect has no hardware mapping.
///
/// Consistent knob roles for muscle memory:
/// - K1: Primary (rate, cutoff, drive, threshold, frequency)
/// - K2: Secondary (depth, feedback, resonance, tone, ratio)
/// - K3: Color (damping, HF rolloff, stages, jitter, waveform)
/// - K4: Character (mode/shape, often STEPPED)
/// - K5: Mix (wet/dry blend)
/// - K6: Level (output/makeup gain) — morph mode: morph speed
pub fn knob_map(effect_id: &str) -> Option<[u8; 6]> {
    match effect_id {
        "filter" => Some([0, 1, 3, NULL_KNOB, NULL_KNOB, 2]),
        "tremolo" => Some([0, 1, 2, 3, NULL_KNOB, 6]),
        "vibrato" => Some([0, NULL_KNOB, NULL_KNOB, NULL_KNOB, 1, 2]),
        "chorus" => Some([0, 1, 4, 3, 2, 8]),
        "phaser" => Some([0, 1, 2, 3, 4, 9]),
        "flanger" => Some([0, 1, 2, 4, 3, 7]),
        "delay" => Some([0, 1, 4, 3, 2, 9]),
        "reverb" => Some([0, 1, 2, 3, 4, 7]),
        "tape" => Some([0, 1, 2, 4, 5, 9]),
        "compressor" => Some([0, 1, 2, 3, 10, 4]),
        "wah" => Some([0, 1, 2, 3, NULL_KNOB, 4]),
        "distortion" => Some([0, 1, 3, 5, 4, 2]),
        "bitcrusher" => Some([0, 1, 2, NULL_KNOB, 3, 4]),
        "ringmod" => Some([0, 1, 2, NULL_KNOB, 3, 4]),
        "looper" => Some([0, 1, 2, 3, 4, 5]),
        _ => None,
    }
}

/// Combined noon + biased ADC conversion: normalized knob → native param value.
///
/// Looks up the noon preset for `effect_id` at `param_idx`, falls back to the
/// descriptor's default if no noon preset is defined.
///
/// # Parameters
///
/// - `effect_id`: Registry ID (e.g., `"distortion"`, `"delay"`).
/// - `param_idx`: Parameter index within the effect.
/// - `desc`: Parameter descriptor with scale, min, max, and flags.
/// - `normalized`: ADC reading normalized to 0.0–1.0.
pub fn knob_to_param(
    effect_id: &str,
    param_idx: usize,
    desc: &ParamDescriptor,
    normalized: f32,
) -> f32 {
    let noon = noon::noon_value(effect_id, param_idx).unwrap_or(desc.default);
    adc_to_param_biased(desc, noon, normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_pedal_effects_have_knob_map() {
        let ids = [
            "chorus",
            "phaser",
            "distortion",
            "bitcrusher",
            "delay",
            "reverb",
            "ringmod",
            "filter",
        ];
        for id in &ids {
            assert!(knob_map(id).is_some(), "missing knob_map for {id}");
        }
    }

    #[test]
    fn unknown_effect_returns_none() {
        assert!(knob_map("nonexistent").is_none());
    }

    #[test]
    fn knob_to_param_uses_noon() {
        let desc = ParamDescriptor::gain_db("Drive", "Drive", 0.0, 40.0, 8.0);
        // At knob center (0.5), should return the noon value for distortion drive (15.0)
        let val = knob_to_param("distortion", 0, &desc, 0.5);
        assert!((val - 15.0).abs() < 0.1, "expected ~15.0, got {val}");
    }

    #[test]
    fn knob_to_param_falls_back_to_default() {
        let desc = ParamDescriptor::custom("P", "P", 0.0, 100.0, 42.0);
        // Unknown effect → falls back to desc.default as noon
        let val = knob_to_param("nonexistent", 0, &desc, 0.5);
        assert!((val - 42.0).abs() < 0.1, "expected ~42.0, got {val}");
    }
}
