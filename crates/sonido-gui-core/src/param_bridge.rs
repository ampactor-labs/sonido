//! Parameter bridge trait for decoupled GUI-to-audio parameter communication.
//!
//! [`ParamBridge`] abstracts over the parameter storage mechanism, enabling
//! the same GUI widgets to work in both the standalone dashboard (backed by
//! atomic floats) and CLAP/VST3 plugins (backed by clack host params).
//!
//! # Architecture
//!
//! The bridge models a fixed number of **slots**, each representing one effect
//! in the processing chain. Each slot exposes parameters via index, matching
//! the [`ParameterInfo`](sonido_core::ParameterInfo) convention from sonido-core.
//!
//! ```text
//! GUI widgets ──► begin_set(slot, param)
//!                 set(slot, param, value)   ← may be called multiple times (drag)
//!                 end_set(slot, param)
//!                         │
//!                    ┌────┴────┐
//!                    │ Atomic  │  (standalone — begin/end are no-ops)
//!                    │ Clack   │  (CLAP plugin — begin/end map to gesture events)
//!                    └────┬────┘
//!                         │
//! Audio thread ◄── ParamBridge::get(slot, param)
//! ```
//!
//! # Gesture Protocol
//!
//! Plugin hosts (CLAP, VST3) require explicit gesture begin/end notifications
//! around parameter changes for proper undo grouping and automation recording.
//! Call [`begin_set`](ParamBridge::begin_set) before the first `set` in a drag
//! gesture, and [`end_set`](ParamBridge::end_set) after the last. For standalone
//! use, both are no-ops by default.

use core::fmt;
use sonido_core::ParamDescriptor;

/// Type-safe index into the effect chain slot array.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SlotIndex(pub usize);

impl fmt::Display for SlotIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<usize> for SlotIndex {
    fn from(v: usize) -> Self {
        Self(v)
    }
}

/// Type-safe index into a slot's parameter array.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ParamIndex(pub usize);

impl fmt::Display for ParamIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<usize> for ParamIndex {
    fn from(v: usize) -> Self {
        Self(v)
    }
}

/// Trait for structural chain mutations (reorder, add, remove).
///
/// Decouples [`ChainView`](crate) rendering from the underlying storage
/// mechanism. In standalone mode, backed by `AtomicParamBridge` (RCU).
/// In plugin mode, backed by `ChainShared` (command queue to audio thread).
pub trait ChainMutator: Send + Sync {
    /// Returns the current processing order as slot indices.
    fn get_order(&self) -> Vec<usize>;

    /// Move an effect from one position to another in the processing order.
    fn move_effect(&self, from: usize, to: usize);
}

/// Trait for bridging parameter values between GUI and audio threads.
///
/// Implementations must be thread-safe — `get` and `set` may be called
/// from different threads simultaneously. Index-based access mirrors
/// [`ParameterInfo`](sonido_core::ParameterInfo) for zero-cost mapping.
pub trait ParamBridge: Send + Sync {
    /// Number of effect slots in the chain.
    fn slot_count(&self) -> usize;

    /// Effect identifier for the given slot (e.g., `"distortion"`, `"reverb"`).
    ///
    /// Returns `""` if the slot index is out of range.
    fn effect_id(&self, slot: SlotIndex) -> &str;

    /// Number of parameters for the effect in the given slot.
    ///
    /// Returns `0` if the slot index is out of range.
    fn param_count(&self, slot: SlotIndex) -> usize;

    /// Parameter descriptor for display and validation.
    ///
    /// Returns `None` if slot or param index is out of range.
    fn param_descriptor(&self, slot: SlotIndex, param: ParamIndex) -> Option<ParamDescriptor>;

    /// Read the current value of a parameter.
    ///
    /// Returns the parameter's default value (or `0.0`) if indices are out of range.
    fn get(&self, slot: SlotIndex, param: ParamIndex) -> f32;

    /// Write a new value for a parameter.
    ///
    /// Out-of-range indices are silently ignored. Values are clamped to the
    /// parameter's valid range by the implementation.
    fn set(&self, slot: SlotIndex, param: ParamIndex, value: f32);

    /// Whether the effect in the given slot is bypassed.
    ///
    /// Returns `false` if the slot index is out of range.
    fn is_bypassed(&self, slot: SlotIndex) -> bool;

    /// Set the bypass state for the effect in the given slot.
    ///
    /// Out-of-range indices are silently ignored.
    fn set_bypassed(&self, slot: SlotIndex, bypassed: bool);

    /// Signal the start of a parameter adjustment gesture (e.g., mouse down on a knob).
    ///
    /// Plugin hosts use this for undo grouping and automation recording.
    /// Standalone implementations should leave the default no-op.
    /// Must be paired with a matching [`end_set`](Self::end_set) call.
    fn begin_set(&self, _slot: SlotIndex, _param: ParamIndex) {}

    /// Signal the end of a parameter adjustment gesture (e.g., mouse up on a knob).
    ///
    /// Must be preceded by a matching [`begin_set`](Self::begin_set) call.
    /// Standalone implementations should leave the default no-op.
    fn end_set(&self, _slot: SlotIndex, _param: ParamIndex) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct MockParamBridge {
        values: Mutex<Vec<Vec<f32>>>,
        bypassed: Mutex<Vec<bool>>,
        effect_ids: Vec<String>,
    }

    impl MockParamBridge {
        fn new(slots: &[(&str, &[f32])]) -> Self {
            Self {
                values: Mutex::new(slots.iter().map(|(_, v)| v.to_vec()).collect()),
                bypassed: Mutex::new(vec![false; slots.len()]),
                effect_ids: slots.iter().map(|(id, _)| (*id).to_owned()).collect(),
            }
        }
    }

    impl ParamBridge for MockParamBridge {
        fn slot_count(&self) -> usize {
            self.effect_ids.len()
        }

        fn effect_id(&self, slot: SlotIndex) -> &str {
            self.effect_ids.get(slot.0).map_or("", String::as_str)
        }

        fn param_count(&self, slot: SlotIndex) -> usize {
            self.values.lock().unwrap().get(slot.0).map_or(0, Vec::len)
        }

        fn param_descriptor(
            &self,
            _slot: SlotIndex,
            _param: ParamIndex,
        ) -> Option<ParamDescriptor> {
            None
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
            if let Some(slot_vals) = self.values.lock().unwrap().get_mut(slot.0)
                && let Some(v) = slot_vals.get_mut(param.0)
            {
                *v = value;
            }
        }

        fn is_bypassed(&self, slot: SlotIndex) -> bool {
            self.bypassed
                .lock()
                .unwrap()
                .get(slot.0)
                .copied()
                .unwrap_or(false)
        }

        fn set_bypassed(&self, slot: SlotIndex, bypassed: bool) {
            if let Some(b) = self.bypassed.lock().unwrap().get_mut(slot.0) {
                *b = bypassed;
            }
        }
    }

    #[test]
    fn mock_get_set_roundtrip() {
        let bridge = MockParamBridge::new(&[("distortion", &[0.5, 1.0, 0.7])]);
        assert_eq!(bridge.get(SlotIndex(0), ParamIndex(0)), 0.5);

        bridge.set(SlotIndex(0), ParamIndex(0), 0.9);
        assert_eq!(bridge.get(SlotIndex(0), ParamIndex(0)), 0.9);
    }

    #[test]
    fn mock_slot_metadata() {
        let bridge = MockParamBridge::new(&[("reverb", &[0.5, 0.3]), ("delay", &[100.0])]);
        assert_eq!(bridge.slot_count(), 2);
        assert_eq!(bridge.effect_id(SlotIndex(0)), "reverb");
        assert_eq!(bridge.effect_id(SlotIndex(1)), "delay");
        assert_eq!(bridge.param_count(SlotIndex(0)), 2);
        assert_eq!(bridge.param_count(SlotIndex(1)), 1);
    }

    #[test]
    fn out_of_range_returns_defaults() {
        let bridge = MockParamBridge::new(&[("eq", &[1.0])]);
        assert_eq!(bridge.effect_id(SlotIndex(99)), "");
        assert_eq!(bridge.param_count(SlotIndex(99)), 0);
        assert_eq!(bridge.get(SlotIndex(99), ParamIndex(0)), 0.0);
        assert_eq!(bridge.get(SlotIndex(0), ParamIndex(99)), 0.0);
        assert!(!bridge.is_bypassed(SlotIndex(99)));
    }

    #[test]
    fn bypass_roundtrip() {
        let bridge = MockParamBridge::new(&[("chorus", &[0.5])]);
        assert!(!bridge.is_bypassed(SlotIndex(0)));
        bridge.set_bypassed(SlotIndex(0), true);
        assert!(bridge.is_bypassed(SlotIndex(0)));
        bridge.set_bypassed(SlotIndex(0), false);
        assert!(!bridge.is_bypassed(SlotIndex(0)));
    }

    #[test]
    fn begin_end_set_default_are_noops() {
        let bridge = MockParamBridge::new(&[("filter", &[500.0])]);
        // Default impls — just verify they don't panic.
        bridge.begin_set(SlotIndex(0), ParamIndex(0));
        bridge.set(SlotIndex(0), ParamIndex(0), 800.0);
        bridge.end_set(SlotIndex(0), ParamIndex(0));
        assert_eq!(bridge.get(SlotIndex(0), ParamIndex(0)), 800.0);
    }

    #[test]
    fn slot_index_display_and_from() {
        let s = SlotIndex::from(3usize);
        assert_eq!(s.0, 3);
        assert_eq!(format!("{s}"), "3");
    }

    #[test]
    fn param_index_display_and_from() {
        let p = ParamIndex::from(7usize);
        assert_eq!(p.0, 7);
        assert_eq!(format!("{p}"), "7");
    }
}
