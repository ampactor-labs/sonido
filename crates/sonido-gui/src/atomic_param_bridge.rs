//! Standalone `ParamBridge` implementation backed by atomic floats.
//!
//! [`AtomicParamBridge`] stores parameter values in lock-free atomics,
//! mirroring the [`ParameterInfo`](sonido_core::ParameterInfo) index
//! convention. The GUI thread calls `set()`, the audio thread calls `get()`.
//!
//! All hot-path reads go through [`ArcSwap::load`], which is wait-free.
//! Structural mutations (add/remove slot, reorder) use RCU via
//! [`ArcSwap::rcu`] — readers never block.

use arc_swap::ArcSwap;
use sonido_core::ParamDescriptor;
use sonido_gui_core::{ParamBridge, ParamIndex, SlotIndex};
use sonido_registry::EffectRegistry;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Per-slot parameter storage.
///
/// Each slot is `Arc`-wrapped inside [`SharedAudioState`] so it survives
/// across snapshot swaps without cloning atomic values.
pub(crate) struct SlotState {
    effect_id: &'static str,
    /// Atomic f32 values, one per ParameterInfo parameter.
    values: Vec<AtomicU32>,
    /// Cached descriptors for display and validation.
    descriptors: Vec<ParamDescriptor>,
    /// Bypass state.
    bypassed: AtomicBool,
    /// Set on the GUI thread when any parameter changes; cleared after
    /// `sync_to_chain` pushes the values to the effect. Avoids iterating
    /// all params every buffer for slots that haven't changed.
    dirty: AtomicBool,
}

/// Immutable snapshot shared between GUI and audio threads via [`ArcSwap`].
///
/// Slots are `Arc`-wrapped so state survives across snapshot swaps without
/// cloning atomic values. The `order` vector tracks processing order.
pub(crate) struct SharedAudioState {
    slots: Vec<Arc<SlotState>>,
    order: Vec<usize>,
}

/// Thread-safe parameter bridge for the standalone dashboard.
///
/// Each slot corresponds to one effect in the processing chain, with
/// parameters indexed to match [`ParameterInfo`](sonido_core::ParameterInfo).
/// Values are stored in ParameterInfo units (dB, Hz, ms, 0–100 for percent).
///
/// Hot-path reads (`get`, `set`, `sync_to_chain`) go through
/// [`ArcSwap::load`] — wait-free, no locks. Structural mutations
/// (`add_slot`, `remove_slot`) use RCU — readers never block.
pub struct AtomicParamBridge {
    state: ArcSwap<SharedAudioState>,
}

impl AtomicParamBridge {
    /// Build a bridge from the registry, creating one slot per effect ID.
    ///
    /// Parameters are initialized to their `ParamDescriptor::default` values.
    /// All effects start active (not bypassed). The initial processing order
    /// matches insertion order (`0..n`).
    pub fn new(registry: &EffectRegistry, effect_ids: &[&'static str], sample_rate: f32) -> Self {
        let slots: Vec<Arc<SlotState>> = effect_ids
            .iter()
            .map(|&id| {
                let (values, descriptors) = if let Some(effect) = registry.create(id, sample_rate) {
                    let count = effect.effect_param_count();
                    let mut vals = Vec::with_capacity(count);
                    let mut descs = Vec::with_capacity(count);
                    for i in 0..count {
                        if let Some(desc) = effect.effect_param_info(i) {
                            vals.push(AtomicU32::new(desc.default.to_bits()));
                            descs.push(desc);
                        }
                    }
                    (vals, descs)
                } else {
                    (Vec::new(), Vec::new())
                };

                Arc::new(SlotState {
                    effect_id: id,
                    values,
                    descriptors,
                    bypassed: AtomicBool::new(false),
                    dirty: AtomicBool::new(true),
                })
            })
            .collect();

        let order: Vec<usize> = (0..slots.len()).collect();

        Self {
            state: ArcSwap::from_pointee(SharedAudioState { slots, order }),
        }
    }

    /// Set the default bypass state for a slot.
    ///
    /// Called after construction to configure which effects start bypassed.
    pub fn set_default_bypass(&self, slot: SlotIndex, bypassed: bool) {
        let snap = self.state.load();
        if let Some(s) = snap.slots.get(slot.0) {
            s.bypassed.store(bypassed, Ordering::Relaxed);
        }
    }

    /// Appends a new slot, returning its index.
    ///
    /// Called on the audio thread when processing a `GraphCommand::Add`.
    /// Uses RCU — existing readers see the old snapshot until they reload.
    /// Parameter values are initialized to each descriptor's default.
    pub fn add_slot(&self, id: &'static str, descriptors: Vec<ParamDescriptor>) -> SlotIndex {
        let values = descriptors
            .iter()
            .map(|d| AtomicU32::new(d.default.to_bits()))
            .collect();
        let new_slot = Arc::new(SlotState {
            effect_id: id,
            values,
            descriptors,
            bypassed: AtomicBool::new(false),
            dirty: AtomicBool::new(true),
        });

        self.state.rcu(|old| {
            let mut slots = old.slots.clone();
            slots.push(Arc::clone(&new_slot));
            let mut order = old.order.clone();
            order.push(slots.len() - 1);
            Arc::new(SharedAudioState { slots, order })
        });
        let idx = self.state.load().slots.len() - 1;

        SlotIndex(idx)
    }

    /// Removes a slot at the given position, shifting remaining slots left.
    ///
    /// Called on the audio thread when processing a `GraphCommand::Remove`.
    /// Uses RCU. The order vector is updated to remove the slot and decrement
    /// all indices greater than the removed slot.
    pub(crate) fn remove_slot(&self, slot: SlotIndex) {
        self.state.rcu(|old| {
            if slot.0 >= old.slots.len() {
                return Arc::clone(old);
            }
            let mut slots = old.slots.clone();
            slots.remove(slot.0);

            // Remove `slot` from order and decrement indices > slot.0
            let mut order: Vec<usize> =
                old.order.iter().copied().filter(|&i| i != slot.0).collect();
            for idx in &mut order {
                if *idx > slot.0 {
                    *idx -= 1;
                }
            }
            Arc::new(SharedAudioState { slots, order })
        });
    }

    /// Rebuilds all slots from a topology manifest.
    ///
    /// Called on the audio thread when processing a `GraphCommand::ReplaceTopology`.
    /// Replaces the entire slot list atomically. Readers see the old snapshot
    /// until they reload.
    pub(crate) fn rebuild_from_manifest(
        &self,
        effect_ids: &[&'static str],
        slot_descriptors: &[Vec<ParamDescriptor>],
    ) {
        let slots: Vec<Arc<SlotState>> = effect_ids
            .iter()
            .zip(slot_descriptors.iter())
            .map(|(&id, descs)| {
                let values = descs
                    .iter()
                    .map(|d| AtomicU32::new(d.default.to_bits()))
                    .collect();
                Arc::new(SlotState {
                    effect_id: id,
                    values,
                    descriptors: descs.clone(),
                    bypassed: AtomicBool::new(false),
                    dirty: AtomicBool::new(true),
                })
            })
            .collect();
        let order: Vec<usize> = (0..slots.len()).collect();
        self.state
            .store(Arc::new(SharedAudioState { slots, order }));
    }

    // ── Order operations ──────────────────────────────────────────────────

    /// Returns the current processing order as slot indices.
    pub fn get_order(&self) -> Vec<usize> {
        self.state.load().order.clone()
    }

    /// Returns the effect IDs in their current processing order.
    pub fn ordered_static_ids(&self) -> Vec<&'static str> {
        let snap = self.state.load();
        snap.order
            .iter()
            .map(|&i| snap.slots[i].effect_id)
            .collect()
    }

    /// Returns the bypass states in their current processing order.
    pub fn ordered_bypass_states(&self) -> Vec<bool> {
        let snap = self.state.load();
        snap.order
            .iter()
            .map(|&i| snap.slots[i].bypassed.load(Ordering::Acquire))
            .collect()
    }
}

impl ParamBridge for AtomicParamBridge {
    fn slot_count(&self) -> usize {
        self.state.load().slots.len()
    }

    fn effect_id(&self, slot: SlotIndex) -> &str {
        // effect_id is &'static str so the reference outlives the snapshot.
        self.state
            .load()
            .slots
            .get(slot.0)
            .map_or("", |s| s.effect_id)
    }

    fn param_count(&self, slot: SlotIndex) -> usize {
        self.state
            .load()
            .slots
            .get(slot.0)
            .map_or(0, |s| s.descriptors.len())
    }

    fn param_descriptor(&self, slot: SlotIndex, param: ParamIndex) -> Option<ParamDescriptor> {
        self.state
            .load()
            .slots
            .get(slot.0)
            .and_then(|s| s.descriptors.get(param.0))
            .cloned()
    }

    fn get(&self, slot: SlotIndex, param: ParamIndex) -> f32 {
        self.state
            .load()
            .slots
            .get(slot.0)
            .and_then(|s| s.values.get(param.0))
            .map(|v| f32::from_bits(v.load(Ordering::Acquire)))
            .unwrap_or(0.0)
    }

    fn set(&self, slot: SlotIndex, param: ParamIndex, value: f32) {
        let snap = self.state.load();
        if let Some(s) = snap.slots.get(slot.0)
            && let Some((atomic, desc)) = s.values.get(param.0).zip(s.descriptors.get(param.0))
        {
            let clamped = value.clamp(desc.min, desc.max);
            atomic.store(clamped.to_bits(), Ordering::Release);
            s.dirty.store(true, Ordering::Release);
        }
    }

    fn is_bypassed(&self, slot: SlotIndex) -> bool {
        self.state
            .load()
            .slots
            .get(slot.0)
            .is_some_and(|s| s.bypassed.load(Ordering::Acquire))
    }

    fn set_bypassed(&self, slot: SlotIndex, bypassed: bool) {
        let snap = self.state.load();
        if let Some(s) = snap.slots.get(slot.0) {
            s.bypassed.store(bypassed, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_from_registry() {
        let registry = EffectRegistry::new();
        let bridge = AtomicParamBridge::new(&registry, &["distortion", "reverb"], 48000.0);

        assert_eq!(bridge.slot_count(), 2);
        assert_eq!(bridge.effect_id(SlotIndex(0)), "distortion");
        assert_eq!(bridge.effect_id(SlotIndex(1)), "reverb");
        assert!(bridge.param_count(SlotIndex(0)) > 0);
        assert!(bridge.param_count(SlotIndex(1)) > 0);
    }

    #[test]
    fn get_set_clamps() {
        let registry = EffectRegistry::new();
        let bridge = AtomicParamBridge::new(&registry, &["distortion"], 48000.0);

        // Read default
        let desc = bridge
            .param_descriptor(SlotIndex(0), ParamIndex(0))
            .unwrap();
        assert_eq!(bridge.get(SlotIndex(0), ParamIndex(0)), desc.default);

        // Set within range
        bridge.set(SlotIndex(0), ParamIndex(0), 10.0);
        assert_eq!(bridge.get(SlotIndex(0), ParamIndex(0)), 10.0);

        // Clamp above max
        bridge.set(SlotIndex(0), ParamIndex(0), 999.0);
        assert_eq!(bridge.get(SlotIndex(0), ParamIndex(0)), desc.max);
    }

    #[test]
    fn bypass_states() {
        let registry = EffectRegistry::new();
        let bridge = AtomicParamBridge::new(&registry, &["chorus", "gate"], 48000.0);

        assert!(!bridge.is_bypassed(SlotIndex(0)));
        assert!(!bridge.is_bypassed(SlotIndex(1)));

        bridge.set_bypassed(SlotIndex(1), true);
        assert!(!bridge.is_bypassed(SlotIndex(0)));
        assert!(bridge.is_bypassed(SlotIndex(1)));
    }

    #[test]
    fn out_of_range_safe() {
        let registry = EffectRegistry::new();
        let bridge = AtomicParamBridge::new(&registry, &["distortion"], 48000.0);

        assert_eq!(bridge.get(SlotIndex(99), ParamIndex(0)), 0.0);
        assert_eq!(bridge.param_count(SlotIndex(99)), 0);
        assert_eq!(bridge.effect_id(SlotIndex(99)), "");
        assert!(!bridge.is_bypassed(SlotIndex(99)));

        // These should not panic
        bridge.set(SlotIndex(99), ParamIndex(0), 1.0);
        bridge.set_bypassed(SlotIndex(99), true);
    }

    #[test]
    fn add_slot_appends() {
        let registry = EffectRegistry::new();
        let bridge = AtomicParamBridge::new(&registry, &["distortion"], 48000.0);
        assert_eq!(bridge.slot_count(), 1);

        // Create an effect to extract its descriptors
        let effect = registry.create("reverb", 48000.0).unwrap();
        let count = effect.effect_param_count();
        let descs: Vec<_> = (0..count)
            .filter_map(|i| effect.effect_param_info(i))
            .collect();

        let idx = bridge.add_slot("reverb", descs);
        assert_eq!(idx, SlotIndex(1));
        assert_eq!(bridge.slot_count(), 2);
        assert_eq!(bridge.effect_id(SlotIndex(1)), "reverb");
        assert!(bridge.param_count(SlotIndex(1)) > 0);
    }

    #[test]
    fn remove_slot_positional() {
        let registry = EffectRegistry::new();
        let bridge =
            AtomicParamBridge::new(&registry, &["distortion", "compressor", "reverb"], 48000.0);
        assert_eq!(bridge.slot_count(), 3);

        // Remove slot 0 → remaining slots shift left
        bridge.remove_slot(SlotIndex(0));
        assert_eq!(bridge.slot_count(), 2);
        assert_eq!(bridge.effect_id(SlotIndex(0)), "compressor");
        assert_eq!(bridge.effect_id(SlotIndex(1)), "reverb");
    }

    #[test]
    fn initial_order_is_sequential() {
        let registry = EffectRegistry::new();
        let bridge =
            AtomicParamBridge::new(&registry, &["distortion", "reverb", "chorus"], 48000.0);
        assert_eq!(bridge.get_order(), vec![0, 1, 2]);
    }

    #[test]
    fn add_slot_updates_order() {
        let registry = EffectRegistry::new();
        let bridge = AtomicParamBridge::new(&registry, &["distortion"], 48000.0);
        assert_eq!(bridge.get_order(), vec![0]);

        let effect = registry.create("reverb", 48000.0).unwrap();
        let count = effect.effect_param_count();
        let descs: Vec<_> = (0..count)
            .filter_map(|i| effect.effect_param_info(i))
            .collect();

        bridge.add_slot("reverb", descs);
        assert_eq!(bridge.get_order(), vec![0, 1]);
    }

    #[test]
    fn remove_slot_updates_order() {
        let registry = EffectRegistry::new();
        let bridge =
            AtomicParamBridge::new(&registry, &["distortion", "compressor", "reverb"], 48000.0);
        assert_eq!(bridge.get_order(), vec![0, 1, 2]);

        // Remove slot 0 → indices shift down
        bridge.remove_slot(SlotIndex(0));
        // Original order [0, 1, 2] → remove 0 → [1, 2] → decrement → [0, 1]
        assert_eq!(bridge.get_order(), vec![0, 1]);
    }
}
