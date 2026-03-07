//! A/B morph state management.
//!
//! Captures parameter snapshots at two positions (A and B) and interpolates
//! between them using a crossfade parameter `t`. Mirrors the interpolation
//! logic from [`KernelParams::lerp()`](sonido_core::kernel::KernelParams::lerp):
//! continuous parameters use linear interpolation while
//! [`STEPPED`](sonido_core::ParamFlags::STEPPED) parameters snap at `t = 0.5`.

use sonido_core::ParamFlags;
use sonido_gui_core::{ParamBridge, ParamIndex, SlotIndex};

/// Captured parameter state for a single effect slot.
#[derive(Clone, Debug)]
pub struct SlotSnapshot {
    /// Registry effect ID (e.g., `"distortion"`, `"reverb"`).
    pub effect_id: String,
    /// Parameter values in index order.
    pub values: Vec<f32>,
    /// Whether the slot was bypassed at capture time.
    pub bypassed: bool,
}

/// Complete chain state snapshot for morphing.
#[derive(Clone, Debug)]
pub struct MorphSnapshot {
    /// One entry per effect slot in chain order.
    pub slots: Vec<SlotSnapshot>,
}

impl MorphSnapshot {
    /// Capture all slot parameters from a bridge into a snapshot.
    pub fn capture(bridge: &dyn ParamBridge) -> Self {
        let count = bridge.slot_count();
        let mut slots = Vec::with_capacity(count);
        for i in 0..count {
            let slot = SlotIndex(i);
            let param_count = bridge.param_count(slot);
            let mut values = Vec::with_capacity(param_count);
            for p in 0..param_count {
                values.push(bridge.get(slot, ParamIndex(p)));
            }
            slots.push(SlotSnapshot {
                effect_id: bridge.effect_id(slot).to_owned(),
                values,
                bypassed: bridge.is_bypassed(slot),
            });
        }
        Self { slots }
    }

    /// Apply linearly interpolated values from snapshots A and B onto a bridge.
    ///
    /// Mirrors [`KernelParams::lerp()`](sonido_core::kernel::KernelParams::lerp):
    /// - Continuous parameters: `va + (vb - va) * t`
    /// - [`STEPPED`](sonido_core::ParamFlags::STEPPED) parameters: snap at `t = 0.5`
    /// - Bypass state: snap at `t = 0.5`
    /// - Locked slots (where `locked[i]` is `true`) are skipped entirely.
    /// - Slots where the effect ID differs between A and B are skipped (morphing
    ///   between different effect types is not meaningful).
    pub fn apply_lerped(a: &Self, b: &Self, t: f32, locked: &[bool], bridge: &dyn ParamBridge) {
        let t = t.clamp(0.0, 1.0);
        let slot_count = a.slots.len().min(b.slots.len()).min(bridge.slot_count());

        for i in 0..slot_count {
            // Skip locked slots
            if locked.get(i).copied().unwrap_or(false) {
                continue;
            }

            let sa = &a.slots[i];
            let sb = &b.slots[i];

            // Skip if effect types differ — interpolation is not meaningful
            if sa.effect_id != sb.effect_id {
                continue;
            }

            let slot = SlotIndex(i);

            // Bypass: snap at midpoint
            let bypassed = if t < 0.5 { sa.bypassed } else { sb.bypassed };
            bridge.set_bypassed(slot, bypassed);

            // Parameters: lerp continuous, snap stepped
            let param_count = sa
                .values
                .len()
                .min(sb.values.len())
                .min(bridge.param_count(slot));
            for p in 0..param_count {
                let va = sa.values[p];
                let vb = sb.values[p];
                let pidx = ParamIndex(p);

                let value = if bridge
                    .param_descriptor(slot, pidx)
                    .is_some_and(|d| d.flags.contains(ParamFlags::STEPPED))
                {
                    if t < 0.5 { va } else { vb }
                } else {
                    va + (vb - va) * t
                };

                bridge.set(slot, pidx, value);
            }
        }
    }
}

/// GUI-side morph controller managing A/B snapshots and crossfade position.
pub struct MorphState {
    /// Snapshot A (typically the "clean" or starting preset).
    pub a: Option<MorphSnapshot>,
    /// Snapshot B (typically the "target" preset).
    pub b: Option<MorphSnapshot>,
    /// Crossfade position: 0.0 = full A, 1.0 = full B.
    pub t: f32,
    /// Whether morphing is currently active.
    pub active: bool,
    /// Per-slot lock flags — locked slots are excluded from morphing.
    pub locked_slots: Vec<bool>,
}

impl MorphState {
    /// Create a new inactive morph state.
    pub fn new() -> Self {
        Self {
            a: None,
            b: None,
            t: 0.0,
            active: false,
            locked_slots: Vec::new(),
        }
    }

    /// Returns `true` if both A and B snapshots have been captured.
    pub fn is_ready(&self) -> bool {
        self.a.is_some() && self.b.is_some()
    }

    /// Capture the current bridge state as snapshot A.
    pub fn capture_a(&mut self, bridge: &dyn ParamBridge) {
        self.a = Some(MorphSnapshot::capture(bridge));
        self.ensure_lock_slots(bridge.slot_count());
    }

    /// Capture the current bridge state as snapshot B.
    pub fn capture_b(&mut self, bridge: &dyn ParamBridge) {
        self.b = Some(MorphSnapshot::capture(bridge));
        self.ensure_lock_slots(bridge.slot_count());
    }

    /// Recall snapshot A: set `t = 0.0` and apply.
    pub fn recall_a(&mut self, bridge: &dyn ParamBridge) {
        self.t = 0.0;
        if let Some(a) = &self.a {
            MorphSnapshot::apply_lerped(a, a, 0.0, &self.locked_slots, bridge);
        }
    }

    /// Recall snapshot B: set `t = 1.0` and apply.
    pub fn recall_b(&mut self, bridge: &dyn ParamBridge) {
        self.t = 1.0;
        if let Some(b) = &self.b {
            MorphSnapshot::apply_lerped(b, b, 1.0, &self.locked_slots, bridge);
        }
    }

    /// Apply the current morph position to the bridge.
    ///
    /// Only applies if morphing is active and both snapshots are captured.
    pub fn apply(&self, bridge: &dyn ParamBridge) {
        if self.active
            && let (Some(a), Some(b)) = (&self.a, &self.b)
        {
            MorphSnapshot::apply_lerped(a, b, self.t, &self.locked_slots, bridge);
        }
    }

    /// Ensure `locked_slots` is at least `count` elements long.
    fn ensure_lock_slots(&mut self, count: usize) {
        if self.locked_slots.len() < count {
            self.locked_slots.resize(count, false);
        }
    }
}

impl Default for MorphState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_core::ParamDescriptor;
    use std::sync::Mutex;

    struct MockBridge {
        values: Mutex<Vec<Vec<f32>>>,
        bypassed: Mutex<Vec<bool>>,
        ids: Vec<String>,
        descriptors: Vec<Vec<Option<ParamDescriptor>>>,
    }

    impl MockBridge {
        #[allow(clippy::type_complexity)]
        fn new(slots: &[(&str, &[f32], &[Option<ParamDescriptor>])]) -> Self {
            Self {
                values: Mutex::new(slots.iter().map(|(_, v, _)| v.to_vec()).collect()),
                bypassed: Mutex::new(vec![false; slots.len()]),
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
    fn capture_roundtrip() {
        let bridge = MockBridge::new(&[("dist", &[10.0, 0.5], &[None, None])]);
        let snap = MorphSnapshot::capture(&bridge);
        assert_eq!(snap.slots.len(), 1);
        assert_eq!(snap.slots[0].effect_id, "dist");
        assert_eq!(snap.slots[0].values, vec![10.0, 0.5]);
        assert!(!snap.slots[0].bypassed);
    }

    #[test]
    fn lerp_continuous_midpoint() {
        let bridge = MockBridge::new(&[("dist", &[0.0, 0.0], &[None, None])]);

        // Set up A
        bridge.set(SlotIndex(0), ParamIndex(0), 10.0);
        bridge.set(SlotIndex(0), ParamIndex(1), 0.0);
        let a = MorphSnapshot::capture(&bridge);

        // Set up B
        bridge.set(SlotIndex(0), ParamIndex(0), 20.0);
        bridge.set(SlotIndex(0), ParamIndex(1), 1.0);
        let b = MorphSnapshot::capture(&bridge);

        // Morph to midpoint
        MorphSnapshot::apply_lerped(&a, &b, 0.5, &[], &bridge);
        assert!((bridge.get(SlotIndex(0), ParamIndex(0)) - 15.0).abs() < 0.001);
        assert!((bridge.get(SlotIndex(0), ParamIndex(1)) - 0.5).abs() < 0.001);
    }

    #[test]
    fn lerp_stepped_snaps_at_midpoint() {
        use sonido_core::ParamFlags;

        let stepped = ParamDescriptor::custom("Mode", "Mode", 0.0, 3.0, 0.0)
            .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED));

        let bridge = MockBridge::new(&[("dist", &[0.0, 0.5], &[Some(stepped), None])]);

        bridge.set(SlotIndex(0), ParamIndex(0), 1.0);
        bridge.set(SlotIndex(0), ParamIndex(1), 0.0);
        let a = MorphSnapshot::capture(&bridge);

        bridge.set(SlotIndex(0), ParamIndex(0), 3.0);
        bridge.set(SlotIndex(0), ParamIndex(1), 1.0);
        let b = MorphSnapshot::capture(&bridge);

        // t=0.3 — stepped param stays at A value
        MorphSnapshot::apply_lerped(&a, &b, 0.3, &[], &bridge);
        assert!((bridge.get(SlotIndex(0), ParamIndex(0)) - 1.0).abs() < 0.001);

        // t=0.7 — stepped param snaps to B value
        MorphSnapshot::apply_lerped(&a, &b, 0.7, &[], &bridge);
        assert!((bridge.get(SlotIndex(0), ParamIndex(0)) - 3.0).abs() < 0.001);
    }

    #[test]
    fn locked_slots_are_skipped() {
        let bridge = MockBridge::new(&[("dist", &[10.0], &[None]), ("rev", &[0.5], &[None])]);

        let a = MorphSnapshot::capture(&bridge);

        bridge.set(SlotIndex(0), ParamIndex(0), 20.0);
        bridge.set(SlotIndex(1), ParamIndex(0), 1.0);
        let b = MorphSnapshot::capture(&bridge);

        // Lock slot 0, morph to B
        let locked = vec![true, false];
        MorphSnapshot::apply_lerped(&a, &b, 1.0, &locked, &bridge);

        // Slot 0 should still be at B's capture value (20.0) — it was skipped
        // The bridge retains whatever value was last set (20.0 from B capture)
        assert!((bridge.get(SlotIndex(0), ParamIndex(0)) - 20.0).abs() < 0.001);
        // Slot 1 should be at B value
        assert!((bridge.get(SlotIndex(1), ParamIndex(0)) - 1.0).abs() < 0.001);
    }

    #[test]
    fn morph_state_lifecycle() {
        let bridge = MockBridge::new(&[("dist", &[10.0], &[None])]);
        let mut state = MorphState::new();

        assert!(!state.is_ready());

        state.capture_a(&bridge);
        assert!(!state.is_ready());

        bridge.set(SlotIndex(0), ParamIndex(0), 20.0);
        state.capture_b(&bridge);
        assert!(state.is_ready());

        state.active = true;
        state.t = 0.5;
        state.apply(&bridge);
        assert!((bridge.get(SlotIndex(0), ParamIndex(0)) - 15.0).abs() < 0.001);
    }

    #[test]
    fn recall_a_resets_to_snapshot() {
        let bridge = MockBridge::new(&[("dist", &[10.0], &[None])]);
        let mut state = MorphState::new();

        state.capture_a(&bridge);
        bridge.set(SlotIndex(0), ParamIndex(0), 99.0);
        state.recall_a(&bridge);

        assert!((bridge.get(SlotIndex(0), ParamIndex(0)) - 10.0).abs() < 0.001);
        assert!((state.t - 0.0).abs() < 0.001);
    }

    #[test]
    fn different_effect_ids_are_skipped() {
        let bridge = MockBridge::new(&[("dist", &[10.0], &[None])]);
        let a = MorphSnapshot::capture(&bridge);

        // Manually create B with different effect ID
        let b = MorphSnapshot {
            slots: vec![SlotSnapshot {
                effect_id: "reverb".to_owned(),
                values: vec![99.0],
                bypassed: false,
            }],
        };

        bridge.set(SlotIndex(0), ParamIndex(0), 50.0);
        MorphSnapshot::apply_lerped(&a, &b, 0.5, &[], &bridge);
        // Should not have changed — effect IDs differ
        assert!((bridge.get(SlotIndex(0), ParamIndex(0)) - 50.0).abs() < 0.001);
    }
}
