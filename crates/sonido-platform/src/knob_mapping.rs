//! Hothouse 6-knob mapping — which physical knob controls which param index.
//!
//! Maps are keyed by effect registry ID (e.g., `"distortion"`, `"reverb"`).
//! Used by `EffectSlot` in sonido-daisy and host-side test harnesses.

use crate::param_map::adc_to_param_biased;
use sonido_core::ParamDescriptor;

/// Sentinel for unmapped knob positions.
pub const NULL_KNOB: u8 = 0xFF;

/// Returns the 6-knob-to-param-index mapping for a named effect.
///
/// Returns `None` if the effect has no hardware mapping.
///
/// The Hothouse's knobs are two rows: K1 K2 K3 on top, K4 K5 K6 below. One
/// rule on every page of the pedal, for muscle memory:
///
/// - **Top row, levels and extras.** K1 a secondary control (stages,
///   voices, ping-pong, pre-delay, …), K2 the wet/dry mix, and K3 (top right)
///   always a volume: the effect's output level here, the master volume on
///   the jam's setup page and in the factory rigs.
/// - **Bottom row, the sound.** K4 K5 K6 the three controls that shape the
///   effect most (drive · tone · shape, rate · depth · feedback, size · decay
///   · damping, …).
pub fn knob_map(effect_id: &str) -> Option<[u8; 6]> {
    const N: u8 = NULL_KNOB;
    match effect_id {
        //                    K1  K2  K3 | K4  K5  K6
        "distortion" => Some([5, 4, 2, 0, 1, 3]), // dynamics mix out | drive tone shape
        "bitcrusher" => Some([N, 3, 4, 0, 1, 2]), // — mix out | bits downsample jitter
        "filter" => Some([N, N, 2, 0, 1, 3]),     // — — out | cutoff resonance type
        "ringmod" => Some([N, 3, 4, 0, 1, 2]),    // — mix out | frequency depth waveform
        "chorus" => Some([3, 2, 8, 0, 1, 4]),     // voices mix out | rate depth feedback
        "phaser" => Some([2, 4, 9, 0, 1, 3]),     // stages mix out | rate depth feedback
        "delay" => Some([3, 2, 9, 0, 1, 4]),      // ping-pong mix out | time feedback tone
        "reverb" => Some([3, 4, 7, 0, 1, 2]),     // pre-delay mix out | size decay damping
        "flanger" => Some([4, 3, 7, 0, 1, 2]),    // through-zero mix out | rate depth feedback
        "tremolo" => Some([3, N, 6, 0, 1, 2]),    // spread — out | rate depth waveform
        "vibrato" => Some([N, 1, 2, 0, N, N]),    // — mix out | depth — —
        "tape" => Some([4, 5, 9, 0, 1, 2]),       // wow flutter out | drive saturation rolloff
        "compressor" => Some([3, 10, 4, 0, 1, 2]), // release mix makeup | threshold ratio attack
        "wah" => Some([3, N, 4, 0, 1, 2]),        // mode — out | frequency resonance sensitivity
        "looper" => Some([3, 4, 5, 0, 1, 2]),     // reverse mix out | mode feedback half-speed
        _ => None,
    }
}

/// Per-effect knob floor override: tightens the descriptor's `min` for hardware
/// knob travel only. The kernel's full descriptor range is preserved for host /
/// CLI use; this only affects how a 0.0–1.0 ADC reading maps to a native value.
///
/// Returned floor must satisfy `desc.min <= floor <= desc.max`. Returning `None`
/// means "use the descriptor's `min` unchanged."
///
/// Use cases: clamp out musically inaudible regions (e.g. delay times below
/// ~30 ms read as comb filtering, not discrete echo, on guitar) so all of the
/// physical knob travel sits in the useful range.
fn knob_min_override(effect_id: &str, param_idx: usize) -> Option<f32> {
    match (effect_id, param_idx) {
        // Delay time: skip the sub-Haas (1-30 ms) zone where echoes aren't
        // perceived as discrete repeats — that range is for sound design, not
        // pedal performance.
        ("delay", 0) => Some(30.0),
        // Filter cutoff: below ~150 Hz a resonant low-pass on guitar is
        // silence, not a sound.
        ("filter", 0) => Some(150.0),
        // Bit depth: below 5 bits a guitar at instrument level never crosses
        // the first quantization step, and the output is silence.
        ("bitcrusher", 0) => Some(5.0),
        _ => None,
    }
}

/// Biased ADC conversion: normalized knob → native param value, with the
/// knob at noon selecting the parameter's default (the effect's voiced sweet
/// spot, level-matched by `sonido_core::level`). Applies a per-effect
/// knob floor override (see `knob_min_override`) when one is defined.
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
    let noon = desc.default;
    if let Some(floor) = knob_min_override(effect_id, param_idx) {
        let floor = floor.clamp(desc.min, desc.max);
        let mut shifted = *desc;
        shifted.min = floor;
        let noon = noon.clamp(floor, desc.max);
        return adc_to_param_biased(&shifted, noon, normalized);
    }
    adc_to_param_biased(desc, noon, normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_pedal_effects_have_knob_map() {
        for id in jam_ids() {
            assert!(knob_map(id).is_some(), "missing knob_map for {id}");
        }
    }

    /// K3 (top right) is always a level, and every effect's main control is
    /// on the bottom row.
    #[test]
    fn k3_is_the_level_and_the_sound_is_below() {
        for id in jam_ids() {
            let map = knob_map(id).unwrap();
            assert_ne!(map[2], NULL_KNOB, "{id}: K3 must be a level");
            assert_eq!(map[3], 0, "{id}: K4 must be the main control (param 0)");
        }
    }

    fn jam_ids() -> [&'static str; 8] {
        [
            "distortion",
            "bitcrusher",
            "filter",
            "ringmod",
            "chorus",
            "phaser",
            "delay",
            "reverb",
        ]
    }

    #[test]
    fn unknown_effect_returns_none() {
        assert!(knob_map("nonexistent").is_none());
    }

    #[test]
    fn knob_to_param_uses_noon() {
        let desc = ParamDescriptor::gain_db("Drive", "Drive", 0.0, 40.0, 8.0);
        // At knob center (0.5), should return the parameter's default.
        let noon = desc.default;
        let val = knob_to_param("distortion", 0, &desc, 0.5);
        assert!((val - noon).abs() < 0.1, "expected ~{noon}, got {val}");
    }

    #[test]
    fn knob_to_param_falls_back_to_default() {
        let desc = ParamDescriptor::custom("P", "P", 0.0, 100.0, 42.0);
        // Unknown effect → falls back to desc.default as noon
        let val = knob_to_param("nonexistent", 0, &desc, 0.5);
        assert!((val - 42.0).abs() < 0.1, "expected ~42.0, got {val}");
    }
}
