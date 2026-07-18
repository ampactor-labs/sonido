//! Auto-assign the six performance macros from the effects in the chain.
//!
//! One macro per effect, in chain order: each macro binds to that effect's
//! *signature* parameter — the first continuous, musical control it exposes.
//! The heuristic skips STEPPED selectors, read-only meters, hidden params, and
//! anything marked [`ParamFlags::MACRO_EXCLUDE`](sonido_core::ParamFlags::MACRO_EXCLUDE)
//! (output/makeup gain, dry/wet mix). Because an effect lists its primary knob
//! first (Drive, Time, Cutoff, Rate…), "first candidate" lands on the right
//! control without a per-effect table.
//!
//! The macro's range and curve come straight from the parameter descriptor, so
//! a frequency sweeps logarithmically and a gain linearly. The range is the
//! parameter's full `[min, max]` — deliberately not noon-aligned, so the knob
//! position that reproduces the *current* value is always representable and
//! auto-assign never jumps the sound. Each macro's position is set to that
//! reproducing value; binding does not move the parameter until the knob does,
//! matching the single-param mapping path in the editor.

use sonido_core::{MacroMap, MacroMapping, MacroTarget, MorphCurve, ParamFlags};
use sonido_gui_core::{ParamBridge, ParamIndex, SlotIndex};

/// Number of macro knobs (K1–K6).
const MACRO_COUNT: usize = 6;

/// A freshly seeded macro map plus a display name per macro.
///
/// Names are empty for macros left unbound (fewer than six mappable effects).
pub struct AutoAssign {
    /// The seeded map: up to six single-target mappings, positions preset to
    /// reproduce each parameter's current value.
    pub map: MacroMap<MACRO_COUNT>,
    /// Per-macro display name (the bound parameter's short name), K1..K6.
    pub names: [String; MACRO_COUNT],
    /// Effects that had a mappable parameter but got no macro because all six
    /// were already spoken for. Zero unless the chain has more than six
    /// mappable effects; surfaced so the caller can tell the user, never a
    /// silent cap.
    pub overflow: usize,
}

/// Build a macro map by binding each effect to its signature parameter.
///
/// See the module docs for the selection rule. Effects with no mappable
/// parameter (e.g. a tuner that is all meters) are skipped without consuming a
/// macro. At most [`MACRO_COUNT`] effects are bound; any beyond that are
/// counted in [`AutoAssign::overflow`].
pub fn auto_assign_macros(bridge: &dyn ParamBridge) -> AutoAssign {
    let mut map = MacroMap::<MACRO_COUNT>::new();
    let mut names: [String; MACRO_COUNT] = std::array::from_fn(|_| String::new());
    let mut next = 0usize;
    let mut overflow = 0usize;

    for s in 0..bridge.slot_count() {
        let slot = SlotIndex(s);
        let Some((p, desc)) = signature_param(bridge, slot) else {
            continue;
        };

        if next >= MACRO_COUNT {
            overflow += 1;
            continue;
        }

        let curve = MorphCurve::from_descriptor(&desc);
        let target = MacroTarget::Slot {
            slot: s as u8,
            param: p as u8,
        };
        map.add_mapping(MacroMapping {
            macro_index: next,
            target,
            min: desc.min,
            max: desc.max,
            curve,
        });
        // Preset the knob so it reproduces the current value — no audible jump.
        let cur = bridge.get(slot, ParamIndex(p));
        map.set_position(next, inverse_position(cur, desc.min, desc.max, curve));
        names[next] = desc.short_name.to_string();
        next += 1;
    }

    AutoAssign {
        map,
        names,
        overflow,
    }
}

/// The signature parameter of a slot: the first continuous, musical control.
///
/// Returns `(param_index, descriptor)` or `None` if the effect exposes nothing
/// mappable (all stepped/read-only/hidden/excluded, or degenerate ranges).
fn signature_param(
    bridge: &dyn ParamBridge,
    slot: SlotIndex,
) -> Option<(usize, sonido_core::ParamDescriptor)> {
    for p in 0..bridge.param_count(slot) {
        let Some(d) = bridge.param_descriptor(slot, ParamIndex(p)) else {
            continue;
        };
        let skip = d.flags.contains(ParamFlags::STEPPED)
            || d.flags.contains(ParamFlags::READ_ONLY)
            || d.flags.contains(ParamFlags::HIDDEN)
            || d.flags.contains(ParamFlags::MACRO_EXCLUDE);
        if skip || d.max <= d.min {
            continue;
        }
        return Some((p, d));
    }
    None
}

/// The knob position `t ∈ [0, 1]` whose [`MacroMapping`] output equals `value`.
///
/// Inverse of `curve_lerp(min, max, t, curve)`. Linear and (positive)
/// logarithmic ranges invert exactly; anything else falls back to the linear
/// inverse. The result is clamped, so a value outside `[min, max]` pins to an
/// end rather than driving the knob off its travel.
fn inverse_position(value: f32, min: f32, max: f32, curve: MorphCurve) -> f32 {
    let t = match curve {
        MorphCurve::Logarithmic if min > 0.0 && max > 0.0 && value > 0.0 => {
            (value / min).ln() / (max / min).ln()
        }
        _ => {
            let span = max - min;
            if span.abs() < f32::EPSILON {
                0.0
            } else {
                (value - min) / span
            }
        }
    };
    t.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_core::{ParamDescriptor, ParamFlags, ParamScale};
    use std::sync::Mutex;

    /// One mock slot: (effect id, param values, param descriptors).
    type SlotSpec<'a> = (&'a str, &'a [f32], &'a [Option<ParamDescriptor>]);

    /// Minimal descriptor-aware bridge (mirrors the one in `morph_state`).
    struct MockBridge {
        values: Mutex<Vec<Vec<f32>>>,
        ids: Vec<String>,
        descriptors: Vec<Vec<Option<ParamDescriptor>>>,
    }

    impl MockBridge {
        fn new(slots: &[SlotSpec]) -> Self {
            Self {
                values: Mutex::new(slots.iter().map(|(_, v, _)| v.to_vec()).collect()),
                ids: slots.iter().map(|(id, _, _)| (*id).to_owned()).collect(),
                descriptors: slots.iter().map(|(_, _, d)| d.to_vec()).collect(),
            }
        }
    }

    impl ParamBridge for MockBridge {
        fn slot_count(&self) -> usize {
            self.ids.len()
        }
        fn effect_id(&self, slot: SlotIndex) -> &str {
            self.ids.get(slot.0).map_or("", String::as_str)
        }
        fn param_count(&self, slot: SlotIndex) -> usize {
            self.values.lock().unwrap().get(slot.0).map_or(0, Vec::len)
        }
        fn param_descriptor(&self, slot: SlotIndex, param: ParamIndex) -> Option<ParamDescriptor> {
            self.descriptors
                .get(slot.0)
                .and_then(|s| s.get(param.0))
                .cloned()
                .flatten()
        }
        fn get(&self, slot: SlotIndex, param: ParamIndex) -> f32 {
            self.values
                .lock()
                .unwrap()
                .get(slot.0)
                .and_then(|s| s.get(param.0))
                .copied()
                .unwrap_or(0.0)
        }
        fn set(&self, slot: SlotIndex, param: ParamIndex, value: f32) {
            if let Some(sv) = self.values.lock().unwrap().get_mut(slot.0)
                && let Some(v) = sv.get_mut(param.0)
            {
                *v = value;
            }
        }
        fn is_bypassed(&self, _slot: SlotIndex) -> bool {
            false
        }
        fn set_bypassed(&self, _slot: SlotIndex, _bypassed: bool) {}
    }

    fn drive() -> ParamDescriptor {
        ParamDescriptor::gain_db("Drive", "Drive", 0.0, 40.0, 8.0)
    }
    fn mix() -> ParamDescriptor {
        ParamDescriptor::mix()
    }

    #[test]
    fn binds_first_musical_param_per_effect() {
        let bridge = MockBridge::new(&[
            ("distortion", &[8.0, 50.0], &[Some(drive()), Some(mix())]),
            ("reverb", &[30.0, 50.0], &[Some(mix()), Some(mix())]),
        ]);
        let a = auto_assign_macros(&bridge);

        assert_eq!(a.map.mapping_count(), 2);
        assert_eq!(a.overflow, 0);
        // K1 → distortion drive (param 0), K2 → reverb's first param.
        let m = a.map.mappings();
        assert!(
            m.iter()
                .any(|m| m.macro_index == 0 && m.target == MacroTarget::Slot { slot: 0, param: 0 })
        );
        assert!(
            m.iter()
                .any(|m| m.macro_index == 1 && m.target == MacroTarget::Slot { slot: 1, param: 0 })
        );
        assert_eq!(a.names[0], "Drive");
    }

    #[test]
    fn skips_stepped_and_excluded_to_find_signature() {
        let stepped = ParamDescriptor::custom("Mode", "Mode", 0.0, 3.0, 0.0)
            .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED));
        let excluded = ParamDescriptor::gain_db("Output", "Out", -20.0, 20.0, 0.0)
            .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::MACRO_EXCLUDE));
        // param 0 stepped, param 1 excluded, param 2 is the real signature.
        let bridge = MockBridge::new(&[(
            "amp",
            &[0.0, 0.0, 8.0],
            &[Some(stepped), Some(excluded), Some(drive())],
        )]);
        let a = auto_assign_macros(&bridge);

        assert_eq!(a.map.mapping_count(), 1);
        assert_eq!(
            a.map.mappings()[0].target,
            MacroTarget::Slot { slot: 0, param: 2 }
        );
    }

    #[test]
    fn effect_with_no_mappable_param_is_skipped_without_consuming_a_macro() {
        let stepped = ParamDescriptor::custom("Note", "Note", 0.0, 11.0, 0.0)
            .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED));
        let bridge = MockBridge::new(&[
            ("tuner", &[0.0], &[Some(stepped)]),
            ("distortion", &[8.0], &[Some(drive())]),
        ]);
        let a = auto_assign_macros(&bridge);

        // Only distortion binds, and it takes K1 (not K2) — the tuner didn't
        // burn a macro slot.
        assert_eq!(a.map.mapping_count(), 1);
        assert_eq!(a.map.mappings()[0].macro_index, 0);
        assert_eq!(
            a.map.mappings()[0].target,
            MacroTarget::Slot { slot: 1, param: 0 }
        );
    }

    #[test]
    fn position_reproduces_current_value_no_jump() {
        // Drive at 8 dB over [0, 40] linear → knob at 0.2.
        let bridge = MockBridge::new(&[("distortion", &[8.0], &[Some(drive())])]);
        let a = auto_assign_macros(&bridge);
        let pos = a.map.position(0);
        assert!((pos - 0.2).abs() < 1e-4, "expected 0.2, got {pos}");
        // Evaluating the mapping at that position returns the original value.
        let back = a.map.mappings()[0].evaluate(pos);
        assert!((back - 8.0).abs() < 1e-3, "round-trip gave {back}");
    }

    #[test]
    fn logarithmic_position_inverts_geometrically() {
        // Cutoff 1 kHz over [20, 20k] log → knob near the geometric-mean position.
        let cutoff = ParamDescriptor::custom("Cutoff", "Cut", 20.0, 20_000.0, 1_000.0)
            .with_scale(ParamScale::Logarithmic);
        let bridge = MockBridge::new(&[("filter", &[1_000.0], &[Some(cutoff)])]);
        let a = auto_assign_macros(&bridge);
        let pos = a.map.position(0);
        let back = a.map.mappings()[0].evaluate(pos);
        assert!((back - 1_000.0).abs() < 1.0, "log round-trip gave {back}");
    }

    #[test]
    fn overflow_counts_effects_beyond_six() {
        // Eight distortions: six bind, two overflow. The backing slices are
        // scoped bindings so every tuple can share them (new() copies out).
        let vals = [8.0f32];
        let descs = [Some(drive())];
        let slots: Vec<SlotSpec> = (0..8)
            .map(|_| ("distortion", &vals[..], &descs[..]))
            .collect();
        let bridge = MockBridge::new(&slots);
        let a = auto_assign_macros(&bridge);
        assert_eq!(a.map.mapping_count(), 6);
        assert_eq!(a.overflow, 2);
    }
}
