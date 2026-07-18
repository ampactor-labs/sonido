//! A/B morph: one global position, per-effect endpoints.
//!
//! Every effect owns two parameter poses — A and B — keyed to its stable graph
//! identity (the node id), not its slot position. The global morph position `t`
//! crossfades the whole rig between them, per-parameter and curve-aware.
//!
//! Because endpoints ride the effect instance and are seeded to the current
//! sound the first time an effect appears, changing the chain never desyncs the
//! morph: a new effect joins immediately, sitting still (`a == b`) until you give
//! it a distinct B, and reordering or removing effects just moves or drops the
//! endpoints that travel with them. This replaces the old model — two frozen
//! whole-rig snapshots that went stale the moment you added an effect.
//!
//! Authoring is park-and-edit. Focus A (or B) to park the rig at that pose and
//! record every knob you turn into it; leave focus to perform. [`grab_edit`] is
//! the shortcut for "make the sound I have right now be A" — the old capture.
//!
//! The struct is generic over the identity key `K` so the core logic unit-tests
//! against plain integers; the app instantiates it with the graph's `NodeId`.
//!
//! [`grab_edit`]: MorphState::grab_edit

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use sonido_core::{MorphCurve, curve_lerp};
use sonido_gui_core::{ParamBridge, ParamIndex, SlotIndex};

/// Which pose a morph edit writes to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MorphEnd {
    /// The A pose (position `t = 0.0`).
    A,
    /// The B pose (position `t = 1.0`).
    B,
}

impl MorphEnd {
    /// The morph position at which this pose is shown untouched.
    fn position(self) -> f32 {
        match self {
            MorphEnd::A => 0.0,
            MorphEnd::B => 1.0,
        }
    }
}

/// One effect's A and B endpoints: a value per parameter, plus bypass per pose.
#[derive(Clone, Debug, Default, PartialEq)]
struct Endpoints {
    effect_id: String,
    a: Vec<f32>,
    b: Vec<f32>,
    bypass_a: bool,
    bypass_b: bool,
}

/// A single effect's pose in slot order — the persistence DTO for one endpoint.
///
/// The runtime keeps endpoints keyed by identity; sessions store them in slot
/// order as plain value arrays, so save/load converts through this.
#[derive(Clone, Debug, PartialEq)]
pub struct SlotSnapshot {
    /// Registry effect id (e.g. `"distortion"`), for a sanity check on load.
    pub effect_id: String,
    /// Parameter values in index order.
    pub values: Vec<f32>,
    /// Bypass state for this pose.
    pub bypassed: bool,
}

/// A whole-rig pose in slot order (all of A, or all of B) — persistence DTO.
#[derive(Clone, Debug, PartialEq)]
pub struct MorphSnapshot {
    /// One entry per effect slot, in chain order.
    pub slots: Vec<SlotSnapshot>,
}

/// Per-effect A/B morph, swept by one global position.
pub struct MorphState<K: Copy + Eq + Hash> {
    /// Endpoints keyed by stable effect identity.
    endpoints: HashMap<K, Endpoints>,
    /// Effects excluded from the morph (held at their live values).
    locked: HashSet<K>,
    /// Global crossfade position: 0.0 = full A, 1.0 = full B.
    pub t: f32,
    /// The pose being sculpted (`Some` ⇒ parked and recording knob edits into
    /// it); `None` ⇒ performing.
    edit: Option<MorphEnd>,
}

impl<K: Copy + Eq + Hash> MorphState<K> {
    /// A new, empty morph: no effects tracked, position 0, performing.
    pub fn new() -> Self {
        Self {
            endpoints: HashMap::new(),
            locked: HashSet::new(),
            t: 0.0,
            edit: None,
        }
    }

    /// The pose currently focused for editing, if any.
    pub fn edit(&self) -> Option<MorphEnd> {
        self.edit
    }

    /// Whether the given pose is the one currently focused.
    pub fn is_editing(&self, end: MorphEnd) -> bool {
        self.edit == Some(end)
    }

    /// Reconcile the endpoint set with the live chain.
    ///
    /// `order[slot]` is the identity of the effect in that chain slot. New
    /// effects are seeded flat (`a == b ==` their current values), so they join
    /// the morph without moving; effects no longer present are dropped. An
    /// effect whose slot now holds a different effect type (id or parameter
    /// count changed) is reseeded. Call once per frame before applying.
    pub fn sync(&mut self, order: &[K], bridge: &dyn ParamBridge) {
        self.endpoints.retain(|k, _| order.contains(k));
        self.locked.retain(|k| order.contains(k));

        for (slot, &key) in order.iter().enumerate() {
            let s = SlotIndex(slot);
            let id = bridge.effect_id(s);
            let pc = bridge.param_count(s);
            let needs_seed = match self.endpoints.get(&key) {
                None => true,
                Some(e) => e.effect_id.as_str() != id || e.a.len() != pc || e.b.len() != pc,
            };
            if needs_seed {
                let cur: Vec<f32> = (0..pc).map(|p| bridge.get(s, ParamIndex(p))).collect();
                let byp = bridge.is_bypassed(s);
                self.endpoints.insert(
                    key,
                    Endpoints {
                        effect_id: id.to_owned(),
                        a: cur.clone(),
                        b: cur,
                        bypass_a: byp,
                        bypass_b: byp,
                    },
                );
            }
        }
    }

    /// Write the interpolated pose to the bridge: every parameter becomes
    /// `curve_lerp(a, b, t)`, bypass snaps at the midpoint. Locked effects are
    /// left untouched. Curve is per-parameter, from the descriptor — the same
    /// rule the firmware uses, so GUI and pedal agree value-for-value.
    pub fn apply(&self, order: &[K], bridge: &dyn ParamBridge) {
        for (slot, key) in order.iter().enumerate() {
            if self.locked.contains(key) {
                continue;
            }
            let Some(e) = self.endpoints.get(key) else {
                continue;
            };
            let s = SlotIndex(slot);
            let n = e.a.len().min(e.b.len()).min(bridge.param_count(s));
            for p in 0..n {
                let pidx = ParamIndex(p);
                let curve = bridge
                    .param_descriptor(s, pidx)
                    .map_or(MorphCurve::Linear, |d| MorphCurve::from_descriptor(&d));
                bridge.set(s, pidx, curve_lerp(e.a[p], e.b[p], self.t, curve));
            }
            let bypassed = if self.t < 0.5 { e.bypass_a } else { e.bypass_b };
            bridge.set_bypassed(s, bypassed);
        }
    }

    /// Focus a pose: park `t` at it and apply, so the rig shows that pose and
    /// subsequent knob edits (folded in via [`record`](Self::record)) land in it.
    pub fn park_edit(&mut self, end: MorphEnd, order: &[K], bridge: &dyn ParamBridge) {
        self.edit = Some(end);
        self.t = end.position();
        self.apply(order, bridge);
    }

    /// Grab the current live rig into a pose, then focus it — "make the sound I
    /// have right now be A (or B)." The endpoint takes the live values and the
    /// rig does not jump, since it already sounds like the new pose.
    pub fn grab_edit(&mut self, end: MorphEnd, order: &[K], bridge: &dyn ParamBridge) {
        self.write_pose(end, order, bridge);
        self.edit = Some(end);
        self.t = end.position();
    }

    /// Leave edit mode and perform at position `t`.
    pub fn perform(&mut self, t: f32, order: &[K], bridge: &dyn ParamBridge) {
        self.edit = None;
        self.t = t.clamp(0.0, 1.0);
        self.apply(order, bridge);
    }

    /// While a pose is focused, fold the live rig into it — call once per frame
    /// after the effect panel renders, so knob turns are recorded. A no-op while
    /// performing, so performed interpolations never overwrite the endpoints.
    pub fn record(&mut self, order: &[K], bridge: &dyn ParamBridge) {
        if let Some(end) = self.edit {
            self.write_pose(end, order, bridge);
        }
    }

    /// Copy the live bridge values (and bypass) into `end` for every effect.
    fn write_pose(&mut self, end: MorphEnd, order: &[K], bridge: &dyn ParamBridge) {
        for (slot, key) in order.iter().enumerate() {
            let s = SlotIndex(slot);
            let Some(e) = self.endpoints.get_mut(key) else {
                continue;
            };
            let arr = match end {
                MorphEnd::A => &mut e.a,
                MorphEnd::B => &mut e.b,
            };
            let n = arr.len().min(bridge.param_count(s));
            for (p, slot_val) in arr.iter_mut().enumerate().take(n) {
                *slot_val = bridge.get(s, ParamIndex(p));
            }
            match end {
                MorphEnd::A => e.bypass_a = bridge.is_bypassed(s),
                MorphEnd::B => e.bypass_b = bridge.is_bypassed(s),
            }
        }
    }

    /// The `(a, b)` endpoint pair for one parameter, for drawing knob markers.
    ///
    /// `None` when the effect is flat there (`a == b`) — nothing to show — or
    /// when the identity/parameter is unknown.
    pub fn markers(&self, key: &K, param: usize) -> Option<(f32, f32)> {
        let e = self.endpoints.get(key)?;
        let a = *e.a.get(param)?;
        let b = *e.b.get(param)?;
        if (a - b).abs() < f32::EPSILON {
            None
        } else {
            Some((a, b))
        }
    }

    /// Whether any effect has a distinct A and B — i.e. the morph does something.
    pub fn is_active(&self) -> bool {
        self.endpoints
            .values()
            .any(|e| e.a != e.b || e.bypass_a != e.bypass_b)
    }

    /// Whether any effect is being tracked at all (the chain is non-empty).
    pub fn has_effects(&self) -> bool {
        !self.endpoints.is_empty()
    }

    /// Exclude / include an effect. Locked effects are held at their live values
    /// while the rest of the rig morphs.
    pub fn set_locked(&mut self, key: K, locked: bool) {
        if locked {
            self.locked.insert(key);
        } else {
            self.locked.remove(&key);
        }
    }

    /// Whether an effect is excluded from the morph.
    pub fn is_locked(&self, key: &K) -> bool {
        self.locked.contains(key)
    }

    /// Export one pose in slot order for a session save.
    ///
    /// Effects with no tracked endpoint (should not happen after [`sync`](Self::sync))
    /// fall back to their live bridge values.
    pub fn snapshot(&self, end: MorphEnd, order: &[K], bridge: &dyn ParamBridge) -> MorphSnapshot {
        let slots = order
            .iter()
            .enumerate()
            .map(|(slot, key)| {
                let s = SlotIndex(slot);
                let (values, bypassed) = match self.endpoints.get(key) {
                    Some(e) => {
                        let arr = match end {
                            MorphEnd::A => &e.a,
                            MorphEnd::B => &e.b,
                        };
                        let byp = match end {
                            MorphEnd::A => e.bypass_a,
                            MorphEnd::B => e.bypass_b,
                        };
                        (arr.clone(), byp)
                    }
                    None => (
                        (0..bridge.param_count(s))
                            .map(|p| bridge.get(s, ParamIndex(p)))
                            .collect(),
                        bridge.is_bypassed(s),
                    ),
                };
                SlotSnapshot {
                    effect_id: bridge.effect_id(s).to_owned(),
                    values,
                    bypassed,
                }
            })
            .collect();
        MorphSnapshot { slots }
    }

    /// Rebuild endpoints from two saved poses (slot order), keyed to the current
    /// effects. Call after the chain is rebuilt from a session. Clears any locks
    /// and edit focus; the caller re-applies locks from the session config.
    pub fn restore(&mut self, order: &[K], a: &MorphSnapshot, b: &MorphSnapshot) {
        self.endpoints.clear();
        self.locked.clear();
        self.edit = None;
        for (slot, &key) in order.iter().enumerate() {
            let (Some(sa), Some(sb)) = (a.slots.get(slot), b.slots.get(slot)) else {
                continue;
            };
            self.endpoints.insert(
                key,
                Endpoints {
                    effect_id: sa.effect_id.clone(),
                    a: sa.values.clone(),
                    b: sb.values.clone(),
                    bypass_a: sa.bypassed,
                    bypass_b: sb.bypassed,
                },
            );
        }
    }
}

impl<K: Copy + Eq + Hash> Default for MorphState<K> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_core::{ParamDescriptor, ParamScale};
    use std::sync::Mutex;

    /// One mock slot: (effect id, param values, param descriptors).
    type SlotSpec<'a> = (&'a str, &'a [f32], &'a [Option<ParamDescriptor>]);

    /// Descriptor-aware positional bridge for exercising morph.
    struct MockBridge {
        values: Mutex<Vec<Vec<f32>>>,
        bypassed: Mutex<Vec<bool>>,
        ids: Vec<String>,
        descriptors: Vec<Vec<Option<ParamDescriptor>>>,
    }

    impl MockBridge {
        fn new(slots: &[SlotSpec]) -> Self {
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
            if let Some(sv) = self.values.lock().unwrap().get_mut(slot.0)
                && let Some(v) = sv.get_mut(param.0)
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

    fn none_descs(n: usize) -> Vec<Option<ParamDescriptor>> {
        vec![None; n]
    }

    #[test]
    fn sync_seeds_new_effects_flat() {
        let bridge = MockBridge::new(&[("dist", &[10.0, 0.5], &none_descs(2))]);
        let mut m: MorphState<u32> = MorphState::new();
        m.sync(&[1], &bridge);

        // Freshly seeded → flat, so it is not "active" and shows no markers.
        assert!(!m.is_active());
        assert!(m.markers(&1, 0).is_none());
        // Morphing at any t leaves the value where it is (a == b).
        m.t = 1.0;
        m.apply(&[1], &bridge);
        assert!((bridge.get(SlotIndex(0), ParamIndex(0)) - 10.0).abs() < 1e-6);
    }

    #[test]
    fn added_effect_joins_flat_and_does_not_move() {
        // Two effects; give effect 1 a real A/B, leave effect 2 (added later) flat.
        let bridge = MockBridge::new(&[
            ("dist", &[10.0], &none_descs(1)),
            ("verb", &[0.3], &none_descs(1)),
        ]);
        let order = [1u32, 2u32];
        let mut m: MorphState<u32> = MorphState::new();
        m.sync(&order, &bridge);

        // Author effect 1: A=10, B=20. Effect 2 stays at 0.3 for both.
        m.park_edit(MorphEnd::A, &order, &bridge);
        bridge.set(SlotIndex(0), ParamIndex(0), 10.0);
        m.record(&order, &bridge);
        m.park_edit(MorphEnd::B, &order, &bridge);
        bridge.set(SlotIndex(0), ParamIndex(0), 20.0);
        m.record(&order, &bridge);

        // Perform to the middle: effect 1 → 15, effect 2 stays flat at 0.3.
        m.perform(0.5, &order, &bridge);
        assert!((bridge.get(SlotIndex(0), ParamIndex(0)) - 15.0).abs() < 1e-6);
        assert!((bridge.get(SlotIndex(1), ParamIndex(0)) - 0.3).abs() < 1e-6);
        // Effect 2 is in the morph but flat — no marker.
        assert!(m.markers(&2, 0).is_none());
        assert!(m.markers(&1, 0).is_some());
    }

    #[test]
    fn endpoints_follow_identity_across_reorder() {
        // Author on order [1, 2]; then the chain reorders to [2, 1] (a different
        // bridge layout). Each effect's endpoints must move with its identity.
        let a_layout = MockBridge::new(&[
            ("dist", &[10.0], &none_descs(1)),
            ("verb", &[0.2], &none_descs(1)),
        ]);
        let order1 = [1u32, 2u32];
        let mut m: MorphState<u32> = MorphState::new();
        m.sync(&order1, &a_layout);

        // dist (key 1): B = 20. verb (key 2): B = 0.9.
        m.park_edit(MorphEnd::B, &order1, &a_layout);
        a_layout.set(SlotIndex(0), ParamIndex(0), 20.0);
        a_layout.set(SlotIndex(1), ParamIndex(0), 0.9);
        m.record(&order1, &a_layout);

        // Reordered layout: verb now in slot 0, dist in slot 1.
        let b_layout = MockBridge::new(&[
            ("verb", &[0.0], &none_descs(1)),
            ("dist", &[0.0], &none_descs(1)),
        ]);
        let order2 = [2u32, 1u32];
        m.sync(&order2, &b_layout); // same ids present → no reseed
        m.perform(1.0, &order2, &b_layout);

        // Slot 0 holds verb (key 2) → 0.9; slot 1 holds dist (key 1) → 20.
        assert!((b_layout.get(SlotIndex(0), ParamIndex(0)) - 0.9).abs() < 1e-6);
        assert!((b_layout.get(SlotIndex(1), ParamIndex(0)) - 20.0).abs() < 1e-6);
    }

    #[test]
    fn removed_effect_is_dropped() {
        let bridge = MockBridge::new(&[("dist", &[1.0], &none_descs(1))]);
        let mut m: MorphState<u32> = MorphState::new();
        m.sync(&[1, 2], &bridge); // key 2 has no slot → seeded from empty slot
        assert!(m.markers(&2, 0).is_none());
        // Now only key 1 remains.
        m.sync(&[1], &bridge);
        assert!(m.markers(&2, 0).is_none());
        m.set_locked(2, true);
        m.sync(&[1], &bridge);
        assert!(!m.is_locked(&2)); // lock for the gone effect was pruned
    }

    #[test]
    fn park_edit_records_only_focused_pose() {
        let bridge = MockBridge::new(&[("dist", &[5.0], &none_descs(1))]);
        let order = [1u32];
        let mut m: MorphState<u32> = MorphState::new();
        m.sync(&order, &bridge);

        m.park_edit(MorphEnd::A, &order, &bridge);
        bridge.set(SlotIndex(0), ParamIndex(0), 2.0);
        m.record(&order, &bridge); // A := 2

        m.park_edit(MorphEnd::B, &order, &bridge);
        bridge.set(SlotIndex(0), ParamIndex(0), 8.0);
        m.record(&order, &bridge); // B := 8

        assert_eq!(m.markers(&1, 0), Some((2.0, 8.0)));
        m.perform(0.5, &order, &bridge);
        assert!((bridge.get(SlotIndex(0), ParamIndex(0)) - 5.0).abs() < 1e-6);
    }

    #[test]
    fn perform_does_not_corrupt_endpoints() {
        let bridge = MockBridge::new(&[("dist", &[0.0], &none_descs(1))]);
        let order = [1u32];
        let mut m: MorphState<u32> = MorphState::new();
        m.sync(&order, &bridge);
        m.park_edit(MorphEnd::A, &order, &bridge);
        bridge.set(SlotIndex(0), ParamIndex(0), 0.0);
        m.record(&order, &bridge);
        m.park_edit(MorphEnd::B, &order, &bridge);
        bridge.set(SlotIndex(0), ParamIndex(0), 10.0);
        m.record(&order, &bridge);

        // Perform + record while NOT editing must leave endpoints intact.
        m.perform(0.5, &order, &bridge); // live → 5
        m.record(&order, &bridge); // no-op: edit is None
        assert_eq!(m.markers(&1, 0), Some((0.0, 10.0)));
    }

    #[test]
    fn grab_edit_captures_live_without_jump() {
        let bridge = MockBridge::new(&[("dist", &[7.0], &none_descs(1))]);
        let order = [1u32];
        let mut m: MorphState<u32> = MorphState::new();
        m.sync(&order, &bridge);
        // Live is 7; grab into B.
        m.grab_edit(MorphEnd::B, &order, &bridge);
        // B endpoint is now 7; live unchanged.
        assert!((bridge.get(SlotIndex(0), ParamIndex(0)) - 7.0).abs() < 1e-6);
        // A is still the seeded 7 → flat, so no marker yet.
        assert!(m.markers(&1, 0).is_none());
    }

    #[test]
    fn locked_effect_is_excluded_from_apply() {
        let bridge = MockBridge::new(&[("dist", &[0.0], &none_descs(1))]);
        let order = [1u32];
        let mut m: MorphState<u32> = MorphState::new();
        m.sync(&order, &bridge);
        m.park_edit(MorphEnd::B, &order, &bridge);
        bridge.set(SlotIndex(0), ParamIndex(0), 10.0);
        m.record(&order, &bridge);

        m.set_locked(1, true);
        bridge.set(SlotIndex(0), ParamIndex(0), 3.0);
        m.perform(1.0, &order, &bridge); // would push to 10 if not locked
        assert!((bridge.get(SlotIndex(0), ParamIndex(0)) - 3.0).abs() < 1e-6);
    }

    #[test]
    fn apply_is_curve_aware_for_frequency() {
        let freq = ParamDescriptor::custom("Cutoff", "Cut", 20.0, 20_000.0, 1_000.0)
            .with_scale(ParamScale::Logarithmic);
        let bridge = MockBridge::new(&[("filter", &[100.0], &[Some(freq)])]);
        let order = [1u32];
        let mut m: MorphState<u32> = MorphState::new();
        m.sync(&order, &bridge);
        m.park_edit(MorphEnd::A, &order, &bridge);
        bridge.set(SlotIndex(0), ParamIndex(0), 100.0);
        m.record(&order, &bridge);
        m.park_edit(MorphEnd::B, &order, &bridge);
        bridge.set(SlotIndex(0), ParamIndex(0), 10_000.0);
        m.record(&order, &bridge);

        m.perform(0.5, &order, &bridge);
        // Geometric mean of 100 and 10 000 = 1000 (log curve), not 5050.
        assert!((bridge.get(SlotIndex(0), ParamIndex(0)) - 1_000.0).abs() < 1.0);
    }

    #[test]
    fn snapshot_restore_round_trip() {
        let bridge = MockBridge::new(&[
            ("dist", &[0.0], &none_descs(1)),
            ("verb", &[0.0, 0.0], &none_descs(2)),
        ]);
        let order = [1u32, 2u32];
        let mut m: MorphState<u32> = MorphState::new();
        m.sync(&order, &bridge);
        // Author some A/B.
        m.park_edit(MorphEnd::A, &order, &bridge);
        bridge.set(SlotIndex(0), ParamIndex(0), 1.0);
        bridge.set(SlotIndex(1), ParamIndex(0), 2.0);
        m.record(&order, &bridge);
        m.park_edit(MorphEnd::B, &order, &bridge);
        bridge.set(SlotIndex(0), ParamIndex(0), 3.0);
        bridge.set(SlotIndex(1), ParamIndex(1), 4.0);
        m.record(&order, &bridge);

        let a = m.snapshot(MorphEnd::A, &order, &bridge);
        let b = m.snapshot(MorphEnd::B, &order, &bridge);

        let mut restored: MorphState<u32> = MorphState::new();
        restored.restore(&order, &a, &b);
        assert_eq!(restored.markers(&1, 0), m.markers(&1, 0));
        assert_eq!(restored.markers(&2, 1), m.markers(&2, 1));
        assert!(restored.is_active());
    }
}
