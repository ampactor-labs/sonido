//! A/B morph across a whole effect chain — the one engine the GUI, plugin, and
//! pedal share.
//!
//! [`ChainMorph`] holds a per-slot [`MorphSpace`] (1-D, corners A and B) plus a
//! lock and A/B bypass state. Capture and apply are closure-based, so the same
//! struct drives a GUI `ParamBridge`, a plugin `GraphEngine`, or raw firmware
//! parameter writes — and because every consumer interpolates through
//! [`curve_lerp`](super::morph::curve_lerp), they all agree value-for-value.
//!
//! This replaces the three previously independent morph implementations (GUI
//! linear-only `apply_lerped`, the pedal's `interpolate_and_apply`, and ad-hoc
//! plugin code), and in doing so gives the GUI and pedal the per-parameter
//! [`Logarithmic`](super::morph::MorphCurve::Logarithmic) and
//! [`Snap`](super::morph::MorphCurve::Snap) curves they lacked: morphing a
//! cutoff 100 Hz ↔ 10 kHz now passes through 1 kHz at `t = 0.5`, not 5.05 kHz.

#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

use super::morph::{MorphCurve, MorphSpace};
use crate::ParamDescriptor;

/// A/B morph state for one chain slot.
struct SlotMorph {
    space: MorphSpace,
    locked: bool,
    bypass_a: bool,
    bypass_b: bool,
}

/// A/B morph across an effect chain.
pub struct ChainMorph {
    slots: Vec<SlotMorph>,
}

impl ChainMorph {
    /// Build a morph for a chain whose slots have the given parameter counts.
    ///
    /// All corners start at zero with [`MorphCurve::Linear`] curves; fill them
    /// with [`set_corner`](Self::set_corner) / [`capture_corner`](Self::capture_corner).
    pub fn new(param_counts: &[usize]) -> Self {
        Self {
            slots: param_counts
                .iter()
                .map(|&pc| SlotMorph {
                    space: MorphSpace::new_1d(pc),
                    locked: false,
                    bypass_a: false,
                    bypass_b: false,
                })
                .collect(),
        }
    }

    /// Number of slots.
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Parameter count of a slot (0 if out of range).
    pub fn param_count(&self, slot: usize) -> usize {
        self.slots.get(slot).map_or(0, |s| s.space.param_count())
    }

    /// Lock or unlock a slot (locked slots are skipped by [`apply`](Self::apply)).
    pub fn set_locked(&mut self, slot: usize, locked: bool) {
        if let Some(s) = self.slots.get_mut(slot) {
            s.locked = locked;
        }
    }

    /// Whether a slot is locked.
    pub fn is_locked(&self, slot: usize) -> bool {
        self.slots.get(slot).is_some_and(|s| s.locked)
    }

    /// Set a slot's parameter values for corner `A` (0) or `B` (1).
    ///
    /// `values.len()` must equal the slot's parameter count.
    pub fn set_corner(&mut self, slot: usize, corner: usize, values: &[f32]) {
        if let Some(s) = self.slots.get_mut(slot) {
            s.space.set_snapshot(corner, values);
        }
    }

    /// Set a slot's bypass state for corner `A` (0) or `B` (1).
    pub fn set_bypass(&mut self, slot: usize, corner: usize, bypassed: bool) {
        if let Some(s) = self.slots.get_mut(slot) {
            match corner {
                0 => s.bypass_a = bypassed,
                _ => s.bypass_b = bypassed,
            }
        }
    }

    /// Capture corner `A` (0) or `B` (1) for every slot from a getter.
    ///
    /// `get(slot, param)` returns the current value of that parameter.
    pub fn capture_corner(&mut self, corner: usize, mut get: impl FnMut(usize, usize) -> f32) {
        for (i, slot) in self.slots.iter_mut().enumerate() {
            let pc = slot.space.param_count();
            for p in 0..pc {
                slot.space.set_snapshot_param(corner, p, get(i, p));
            }
        }
    }

    /// Set the morph curve for one parameter of one slot.
    pub fn set_curve(&mut self, slot: usize, param: usize, curve: MorphCurve) {
        if let Some(s) = self.slots.get_mut(slot) {
            s.space.set_curve(param, curve);
        }
    }

    /// Auto-select curves for a slot from its parameter descriptors
    /// (frequency → logarithmic, stepped → snap, else linear).
    pub fn auto_curves(&mut self, slot: usize, descriptors: &[Option<ParamDescriptor>]) {
        if let Some(s) = self.slots.get_mut(slot) {
            s.space.auto_curves(descriptors);
        }
    }

    /// Apply the morph at position `t ∈ [0, 1]`.
    ///
    /// For every unlocked slot: each parameter is interpolated through its curve
    /// and handed to `set_param(slot, param, value)`; bypass snaps at the
    /// midpoint and is handed to `set_bypass(slot, bypassed)`. No allocation.
    pub fn apply(
        &self,
        t: f32,
        mut set_param: impl FnMut(usize, usize, f32),
        mut set_bypass: impl FnMut(usize, bool),
    ) {
        let t = t.clamp(0.0, 1.0);
        let pos = [t];
        for (i, slot) in self.slots.iter().enumerate() {
            if slot.locked {
                continue;
            }
            set_bypass(
                i,
                if t < 0.5 {
                    slot.bypass_a
                } else {
                    slot.bypass_b
                },
            );
            for p in 0..slot.space.param_count() {
                set_param(i, p, slot.space.interpolate_param(p, &pos));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::*;
    use alloc::vec;

    #[test]
    fn endpoints_recall_corners() {
        let mut cm = ChainMorph::new(&[2]);
        cm.set_corner(0, 0, &[0.0, 100.0]);
        cm.set_corner(0, 1, &[10.0, 200.0]);
        let mut out = vec![0.0f32; 2];
        cm.apply(0.0, |_, p, v| out[p] = v, |_, _| {});
        assert_eq!(out, vec![0.0, 100.0]);
        cm.apply(1.0, |_, p, v| out[p] = v, |_, _| {});
        assert_eq!(out, vec![10.0, 200.0]);
    }

    #[test]
    fn linear_midpoint() {
        let mut cm = ChainMorph::new(&[1]);
        cm.set_corner(0, 0, &[0.0]);
        cm.set_corner(0, 1, &[10.0]);
        let mut v = 0.0;
        cm.apply(0.5, |_, _, x| v = x, |_, _| {});
        assert!((v - 5.0).abs() < 1e-6);
    }

    #[test]
    fn logarithmic_midpoint_is_geometric_mean() {
        // The fidelity fix: a frequency param morphs through its geometric mean.
        let mut cm = ChainMorph::new(&[1]);
        cm.set_corner(0, 0, &[100.0]);
        cm.set_corner(0, 1, &[10_000.0]);
        cm.set_curve(0, 0, MorphCurve::Logarithmic);
        let mut v = 0.0;
        cm.apply(0.5, |_, _, x| v = x, |_, _| {});
        assert!((v - 1000.0).abs() < 1.0, "got {v}, expected ~1000");
    }

    #[test]
    fn locked_slot_is_skipped() {
        let mut cm = ChainMorph::new(&[1, 1]);
        cm.set_corner(0, 0, &[0.0]);
        cm.set_corner(0, 1, &[10.0]);
        cm.set_corner(1, 0, &[0.0]);
        cm.set_corner(1, 1, &[10.0]);
        cm.set_locked(1, true);
        let mut writes: Vec<usize> = Vec::new();
        cm.apply(0.5, |slot, _, _| writes.push(slot), |_, _| {});
        assert!(writes.contains(&0));
        assert!(!writes.contains(&1), "locked slot 1 must be skipped");
    }

    #[test]
    fn bypass_snaps_at_midpoint() {
        let mut cm = ChainMorph::new(&[1]);
        cm.set_bypass(0, 0, false); // A active
        cm.set_bypass(0, 1, true); // B bypassed
        let mut bypassed = None;
        cm.apply(0.49, |_, _, _| {}, |_, b| bypassed = Some(b));
        assert_eq!(bypassed, Some(false));
        cm.apply(0.51, |_, _, _| {}, |_, b| bypassed = Some(b));
        assert_eq!(bypassed, Some(true));
    }

    #[test]
    fn capture_from_getter_fills_corner() {
        let mut cm = ChainMorph::new(&[3]);
        cm.capture_corner(0, |_slot, p| (p as f32) * 2.0);
        let mut out = vec![0.0f32; 3];
        cm.apply(0.0, |_, p, v| out[p] = v, |_, _| {});
        assert_eq!(out, vec![0.0, 2.0, 4.0]);
    }
}
