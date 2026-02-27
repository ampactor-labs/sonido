//! [`ParamBridge`] implementation backed by [`ChainShared`].
//!
//! Maps the slot-based `ParamBridge` interface to the flat atomic param array
//! in `ChainShared`, using [`ClapParamId`] for validated index arithmetic.
//! Also implements [`ChainMutator`] so the plugin GUI's `ChainView` can
//! reorder the chain via the same shared state.

use sonido_core::ParamDescriptor;
use sonido_gui_core::{ChainMutator, ParamBridge, ParamIndex, SlotIndex};

use super::ClapParamId;
use super::shared::{ChainCommand, ChainShared};

/// [`ParamBridge`] backed by [`ChainShared`] lock-free atomics.
///
/// Maps slot+param indices to the flat `[AtomicU32; 512]` array using
/// [`ClapParamId`]. Gesture tracking sets atomic flags consumed by the
/// audio thread to emit CLAP `ParamGestureBegin/EndEvent`.
pub struct ChainParamBridge {
    shared: ChainShared,
}

impl ChainParamBridge {
    /// Creates a new bridge backed by the given shared state.
    pub fn new(shared: ChainShared) -> Self {
        Self { shared }
    }
}

impl ParamBridge for ChainParamBridge {
    fn slot_count(&self) -> usize {
        let slots = self.shared.load_slots();
        slots.iter().filter(|s| s.active).count()
    }

    fn effect_id(&self, slot: SlotIndex) -> &str {
        let slots = self.shared.load_slots();
        if let Some(snap) = slots.get(slot.0)
            && snap.active
        {
            // SlotSnapshot::effect_id is &'static str, so it outlives the Guard.
            snap.effect_id
        } else {
            ""
        }
    }

    fn param_count(&self, slot: SlotIndex) -> usize {
        let slots = self.shared.load_slots();
        slots
            .get(slot.0)
            .filter(|s| s.active)
            .map_or(0, |s| s.descriptors.len())
    }

    fn param_descriptor(&self, slot: SlotIndex, param: ParamIndex) -> Option<ParamDescriptor> {
        let slots = self.shared.load_slots();
        slots
            .get(slot.0)
            .filter(|s| s.active)
            .and_then(|s| s.descriptors.get(param.0))
            .cloned()
    }

    fn get(&self, slot: SlotIndex, param: ParamIndex) -> f32 {
        ClapParamId::new(slot.0, param.0)
            .map(|id| self.shared.get_value(id))
            .unwrap_or(0.0)
    }

    fn set(&self, slot: SlotIndex, param: ParamIndex, value: f32) {
        if let Some(id) = ClapParamId::new(slot.0, param.0) {
            self.shared.set_value(id, value);
            self.shared.request_process();
        }
    }

    fn is_bypassed(&self, slot: SlotIndex) -> bool {
        self.shared.is_bypassed(slot.0)
    }

    fn set_bypassed(&self, slot: SlotIndex, bypassed: bool) {
        self.shared.set_bypassed(slot.0, bypassed);
    }

    fn begin_set(&self, slot: SlotIndex, param: ParamIndex) {
        if let Some(id) = ClapParamId::new(slot.0, param.0) {
            self.shared.gesture_begin(id);
            self.shared.request_process();
        }
    }

    fn end_set(&self, slot: SlotIndex, param: ParamIndex) {
        if let Some(id) = ClapParamId::new(slot.0, param.0) {
            self.shared.gesture_end(id);
            self.shared.request_process();
        }
    }
}

impl ChainMutator for ChainParamBridge {
    fn get_order(&self) -> Vec<usize> {
        self.shared.load_order().as_ref().clone()
    }

    fn move_effect(&self, from: usize, to: usize) {
        let current = self.shared.load_order();
        if from >= current.len() || to >= current.len() || from == to {
            return;
        }
        let mut order = current.as_ref().clone();
        let effect = order.remove(from);
        order.insert(to, effect);
        self.shared
            .push_command(ChainCommand::Reorder { new_order: order });
    }
}
