//! Parameter macro mapping — N exposed knobs → M internal parameters.
//!
//! A [`MacroMap`] exposes a small number of high-level "macro" knobs (the six
//! knobs on the pedal, six automatable params in the plugin) and maps each one
//! to one or more [`MacroTarget`]s — effect-slot parameters *or* graph-level
//! globals. Every mapping has its own min/max range and [`MorphCurve`], so a
//! macro can scale, invert, log-sweep, or range-limit any underlying control.
//!
//! # Design
//!
//! ```text
//!  Macro knob 0  ─────┬──►  Slot{0, 2}            (0.0 – 1.0, linear)
//!                     └──►  Slot{1, 4}            (0.5 – 2.0, log)      ← different range/curve
//!  Macro knob 1  ─────────►  Global(MasterVolume) (0.0 – -40 dB)       ← inverted, global
//! ```
//!
//! The const generic `N` sets the number of exposed macro knobs at compile
//! time. Mappings are added at runtime (heap, no fixed cap).
//!
//! # Application is decoupled from the engine
//!
//! [`MacroMap::apply_all`] feeds resolved `(target, value)` pairs to a sink
//! closure, so the same map drives a GUI `ParamBridge`, a plugin `GraphEngine`,
//! or raw firmware writes. [`MacroMap::apply_to_engine`] is the convenience
//! wrapper for the common `GraphEngine` + globals case.
//!
//! # no_std
//!
//! Compatible with no_std + alloc.

#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

use crate::graph::engine::GraphEngine;
use crate::kernel::morph::{MorphCurve, curve_lerp};

// ─── Macro targets ────────────────────────────────────────────────────────────

/// A graph-level control that lives *outside* an effect slot.
///
/// Macros (and the A/B morph) can drive these in addition to per-slot
/// parameters, so they need an address that the slot/param pair cannot express.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum GlobalParam {
    /// Pre-graph input gain (dB).
    InputGain,
    /// Post-graph master volume (dB).
    MasterVolume,
    /// A/B morph position in `[0.0, 1.0]`.
    MorphPosition,
    /// A/B morph ramp speed.
    MorphSpeed,
}

/// The destination a macro (or morph) writes to.
///
/// `Slot` addresses a parameter inside a [`GraphEngine`] chain slot; `Global`
/// addresses a graph-level control ([`GlobalParam`]). The same enum is used by
/// the runtime [`MacroMapping`] and by the persisted `sonido-patch` format, so
/// authored mappings survive serialization unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MacroTarget {
    /// Parameter `param` of effect at chain `slot`.
    Slot {
        /// Slot index in the engine's linear chain.
        slot: u8,
        /// Parameter index within that slot's effect.
        param: u8,
    },
    /// A graph-level control.
    Global(GlobalParam),
}

// ─── MacroMapping ─────────────────────────────────────────────────────────────

/// A single macro-to-target mapping entry.
///
/// Maps one macro knob position (0.0 – 1.0) to a [`MacroTarget`], remapped
/// through `[min, max]` along `curve`.
///
/// # Ranges
///
/// * Setting `max < min` inverts the control (0.0 → max, 1.0 → min).
/// * `curve` selects the sweep shape: [`MorphCurve::Linear`] for most params,
///   [`MorphCurve::Logarithmic`] for frequencies, [`MorphCurve::Snap`] for
///   stepped/enum params. No clamping to the descriptor range is done here — the
///   consumer (e.g. `GraphEngine::set_param_at`) clamps.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MacroMapping {
    /// Index of the macro knob (0 – N-1) that drives this mapping.
    pub macro_index: usize,
    /// Where this mapping writes.
    pub target: MacroTarget,
    /// Target value when the macro knob is at 0.0.
    pub min: f32,
    /// Target value when the macro knob is at 1.0.
    pub max: f32,
    /// Interpolation curve across the knob's travel.
    pub curve: MorphCurve,
}

impl MacroMapping {
    /// A linear mapping over `[min, max]` for the given target.
    pub fn linear(macro_index: usize, target: MacroTarget, min: f32, max: f32) -> Self {
        Self {
            macro_index,
            target,
            min,
            max,
            curve: MorphCurve::Linear,
        }
    }

    /// Compute the target value for the given macro position.
    ///
    /// `position` is clamped to `[0.0, 1.0]` before interpolation.
    #[inline]
    pub fn evaluate(&self, position: f32) -> f32 {
        curve_lerp(self.min, self.max, position.clamp(0.0, 1.0), self.curve)
    }
}

// ─── MacroMap ─────────────────────────────────────────────────────────────────

/// Maps `N` macro knobs to an arbitrary number of targets.
///
/// `N` is the number of exposed macro knobs (compile-time constant). Mappings
/// are dynamic (heap-allocated) so any number of destinations can be registered.
///
/// # Invariants
///
/// * Macro indices must be in the range `[0, N)`.
/// * Each macro's current position is stored and re-applied on demand.
/// * All current positions start at 0.0.
pub struct MacroMap<const N: usize> {
    /// Current knob positions, one per macro, in `[0.0, 1.0]`.
    positions: [f32; N],
    /// All registered mappings, searched by `macro_index`.
    mappings: Vec<MacroMapping>,
}

impl<const N: usize> MacroMap<N> {
    /// Create a new `MacroMap` with all knobs at position 0.0 and no mappings.
    pub fn new() -> Self {
        Self {
            positions: [0.0; N],
            mappings: Vec::new(),
        }
    }

    /// Register a new macro-to-target mapping.
    ///
    /// Multiple mappings for the same macro index are allowed — all are applied
    /// together.
    ///
    /// # Panics
    ///
    /// Panics if `mapping.macro_index >= N`.
    pub fn add_mapping(&mut self, mapping: MacroMapping) {
        assert!(
            mapping.macro_index < N,
            "macro_index {} out of range [0, {})",
            mapping.macro_index,
            N
        );
        self.mappings.push(mapping);
    }

    /// Remove all mappings for a given macro index.
    pub fn clear_macro(&mut self, macro_index: usize) {
        self.mappings.retain(|m| m.macro_index != macro_index);
    }

    /// Remove all mappings.
    pub fn clear_all(&mut self) {
        self.mappings.clear();
    }

    /// All current mappings.
    pub fn mappings(&self) -> &[MacroMapping] {
        &self.mappings
    }

    /// Current position of macro `index` in `[0.0, 1.0]`.
    ///
    /// # Panics
    ///
    /// Panics if `index >= N`.
    pub fn position(&self, index: usize) -> f32 {
        self.positions[index]
    }

    /// Store macro `index`'s position without applying it (clamped to `[0,1]`).
    ///
    /// # Panics
    ///
    /// Panics if `index >= N`.
    pub fn set_position(&mut self, index: usize, position: f32) {
        self.positions[index] = position.clamp(0.0, 1.0);
    }

    /// Apply macro `index`'s mappings at its stored position to `sink`.
    ///
    /// `sink(target, value)` receives each resolved write. Decoupled from any
    /// engine so the same map can drive a bridge, an engine, or firmware.
    pub fn apply(&self, index: usize, mut sink: impl FnMut(MacroTarget, f32)) {
        let pos = self.positions[index];
        for m in self.mappings.iter().filter(|m| m.macro_index == index) {
            sink(m.target, m.evaluate(pos));
        }
    }

    /// Apply every macro's mappings at their stored positions to `sink`.
    pub fn apply_all(&self, mut sink: impl FnMut(MacroTarget, f32)) {
        for m in &self.mappings {
            sink(m.target, m.evaluate(self.positions[m.macro_index]));
        }
    }

    /// Convenience: apply every mapping, routing slot targets to `engine` and
    /// global targets to `globals`.
    ///
    /// This is what the standalone audio thread, the plugin, and the firmware
    /// call once per control tick after knob positions change.
    pub fn apply_to_engine(
        &self,
        engine: &mut GraphEngine,
        mut globals: impl FnMut(GlobalParam, f32),
    ) {
        self.apply_all(|target, value| match target {
            MacroTarget::Slot { slot, param } => {
                engine.set_param_at(slot as usize, param as usize, value);
            }
            MacroTarget::Global(g) => globals(g, value),
        });
    }

    /// Number of registered mappings.
    pub fn mapping_count(&self) -> usize {
        self.mappings.len()
    }

    /// Mappings registered for a given macro index.
    pub fn mapping_count_for(&self, macro_index: usize) -> usize {
        self.mappings
            .iter()
            .filter(|m| m.macro_index == macro_index)
            .count()
    }
}

impl<const N: usize> Default for MacroMap<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::*;
    use alloc::vec::Vec;

    fn slot(slot: u8, param: u8) -> MacroTarget {
        MacroTarget::Slot { slot, param }
    }

    #[test]
    fn evaluate_linear_endpoints_and_midpoint() {
        let m = MacroMapping::linear(0, slot(0, 0), 100.0, 200.0);
        assert!((m.evaluate(0.0) - 100.0).abs() < 1e-6);
        assert!((m.evaluate(1.0) - 200.0).abs() < 1e-6);
        assert!((m.evaluate(0.5) - 150.0).abs() < 1e-6);
    }

    #[test]
    fn evaluate_inverted() {
        let m = MacroMapping::linear(0, slot(0, 0), 1.0, 0.0);
        assert!((m.evaluate(0.0) - 1.0).abs() < 1e-6);
        assert!((m.evaluate(1.0) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn evaluate_logarithmic_curve() {
        let m = MacroMapping {
            macro_index: 0,
            target: slot(0, 0),
            min: 100.0,
            max: 10_000.0,
            curve: MorphCurve::Logarithmic,
        };
        assert!((m.evaluate(0.5) - 1000.0).abs() < 1.0); // geometric mean
    }

    #[test]
    fn evaluate_clamps_position() {
        let m = MacroMapping::linear(0, slot(0, 0), 0.0, 10.0);
        assert!((m.evaluate(-1.0) - 0.0).abs() < 1e-6);
        assert!((m.evaluate(2.0) - 10.0).abs() < 1e-6);
    }

    #[test]
    fn apply_all_routes_targets_via_sink() {
        let mut map: MacroMap<6> = MacroMap::new();
        map.add_mapping(MacroMapping::linear(0, slot(0, 2), 0.0, 1.0));
        map.add_mapping(MacroMapping::linear(0, slot(1, 4), 0.0, 10.0));
        map.add_mapping(MacroMapping::linear(
            1,
            MacroTarget::Global(GlobalParam::MasterVolume),
            0.0,
            -40.0,
        ));
        map.set_position(0, 0.5);
        map.set_position(1, 1.0);

        let mut writes: Vec<(MacroTarget, f32)> = Vec::new();
        map.apply_all(|t, v| writes.push((t, v)));

        assert_eq!(writes.len(), 3);
        assert!(writes.contains(&(slot(0, 2), 0.5)));
        assert!(writes.contains(&(slot(1, 4), 5.0)));
        assert!(writes.contains(&(MacroTarget::Global(GlobalParam::MasterVolume), -40.0)));
    }

    #[test]
    fn apply_single_macro_only() {
        let mut map: MacroMap<6> = MacroMap::new();
        map.add_mapping(MacroMapping::linear(0, slot(0, 0), 0.0, 1.0));
        map.add_mapping(MacroMapping::linear(1, slot(1, 0), 0.0, 1.0));
        map.set_position(0, 1.0);

        let mut count = 0;
        map.apply(0, |_, _| count += 1);
        assert_eq!(count, 1);
    }

    #[test]
    fn counts_and_clear() {
        let mut map: MacroMap<6> = MacroMap::new();
        map.add_mapping(MacroMapping::linear(0, slot(0, 0), 0.0, 1.0));
        map.add_mapping(MacroMapping::linear(0, slot(1, 2), 0.0, 10.0));
        map.add_mapping(MacroMapping::linear(1, slot(2, 0), 0.5, 2.0));
        assert_eq!(map.mapping_count(), 3);
        assert_eq!(map.mapping_count_for(0), 2);
        map.clear_macro(0);
        assert_eq!(map.mapping_count_for(0), 0);
        assert_eq!(map.mapping_count(), 1);
    }

    #[test]
    #[should_panic]
    fn out_of_range_macro_panics() {
        let mut map: MacroMap<6> = MacroMap::new();
        map.add_mapping(MacroMapping::linear(6, slot(0, 0), 0.0, 1.0));
    }
}
