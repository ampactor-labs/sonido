//! Parameter macro mapping — N exposed knobs → M internal parameters.
//!
//! A [`MacroMap`] exposes a small number of high-level "macro" knobs (e.g., four
//! performance knobs on a hardware controller) and maps each one to one or more
//! target (slot, param) pairs inside a [`GraphEngine`].  Every mapping has its
//! own min/max range so the macro can act as a scaled, inverted, or range-limited
//! control for any underlying parameter.
//!
//! # Design
//!
//! ```text
//!  Macro knob 0  ─────┬──►  slot 0, param 2  (0.0 – 1.0)
//!                     └──►  slot 1, param 4  (0.5 – 2.0)   ← different range
//!  Macro knob 1  ─────────►  slot 2, param 0  (1.0 – 0.0)   ← inverted
//! ```
//!
//! The const generic `N` sets the number of exposed macro knobs at
//! compile time.  Mappings are added at runtime, one per
//! [`MacroMap::add_mapping`] call, with no fixed cap (stored on the heap).
//!
//! # Usage
//!
//! ```rust,ignore
//! use sonido_core::macro_map::{MacroMap, MacroMapping};
//! use sonido_core::GraphEngine;
//!
//! // 4 macro knobs
//! let mut macros: MacroMap<4> = MacroMap::new();
//!
//! // Macro 0 controls slot 0, param 2 over the full 0–1 range
//! macros.add_mapping(MacroMapping { macro_index: 0, target_slot: 0, target_param: 2, min: 0.0, max: 1.0 });
//!
//! // Move macro 0 to 0.75 — applies to all its targets
//! macros.set_macro(&mut engine, 0, 0.75);
//! ```
//!
//! # no_std
//!
//! Compatible with no_std + alloc.

#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

use crate::graph::engine::GraphEngine;

// ─── MacroMapping ─────────────────────────────────────────────────────────────

/// A single macro-to-parameter mapping entry.
///
/// Maps one macro knob position (0.0 – 1.0) to a target parameter in a
/// [`GraphEngine`] slot, remapped through `[min, max]`.
///
/// The final parameter value is:
/// ```text
/// param_value = min + position * (max - min)
/// ```
///
/// # Ranges
///
/// * `min` and `max` must be within the parameter's own valid range (as defined
///   by its [`ParamDescriptor`](crate::param_info::ParamDescriptor)).  No
///   clamping is applied here — the `GraphEngine` clamps at the descriptor level.
/// * Setting `max < min` inverts the control (0.0 → max, 1.0 → min).
#[derive(Clone, Debug)]
pub struct MacroMapping {
    /// Index of the macro knob (0 – N-1) that drives this mapping.
    pub macro_index: usize,
    /// Slot index in the [`GraphEngine`]'s linear chain.
    pub target_slot: usize,
    /// Parameter index within the effect at `target_slot`.
    pub target_param: usize,
    /// Parameter value when the macro knob is at 0.0.
    pub min: f32,
    /// Parameter value when the macro knob is at 1.0.
    pub max: f32,
}

impl MacroMapping {
    /// Compute the target parameter value for the given macro position.
    ///
    /// `position` is clamped to `[0.0, 1.0]` before interpolation.
    #[inline]
    pub fn evaluate(&self, position: f32) -> f32 {
        let t = position.clamp(0.0, 1.0);
        self.min + t * (self.max - self.min)
    }
}

// ─── MacroMap ─────────────────────────────────────────────────────────────────

/// Maps `N` macro knobs to an arbitrary number of internal parameters.
///
/// `N` is the number of exposed macro knobs (compile-time constant).  Mappings
/// are dynamic (heap-allocated) so any number of parameter destinations can be
/// registered at runtime.
///
/// # Invariants
///
/// * Macro indices must be in the range `[0, N)`.
/// * Each macro's current position is stored and re-applied on `set_macro`.
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

    /// Register a new macro-to-parameter mapping.
    ///
    /// Multiple mappings for the same macro index are allowed — all are applied
    /// together when [`set_macro`](Self::set_macro) is called.
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

    /// Returns a slice of all current mappings.
    pub fn mappings(&self) -> &[MacroMapping] {
        &self.mappings
    }

    /// Returns the current position of macro `index` in `[0.0, 1.0]`.
    ///
    /// # Panics
    ///
    /// Panics if `index >= N`.
    pub fn position(&self, index: usize) -> f32 {
        self.positions[index]
    }

    /// Set macro knob `index` to `position` and apply all associated mappings
    /// to `engine`.
    ///
    /// `position` is clamped to `[0.0, 1.0]`.  Every mapping whose
    /// `macro_index` matches `index` is evaluated and written to the engine via
    /// [`GraphEngine::set_param_at`].
    ///
    /// # Panics
    ///
    /// Panics if `index >= N`.
    pub fn set_macro(&mut self, engine: &mut GraphEngine, index: usize, position: f32) {
        let pos = position.clamp(0.0, 1.0);
        self.positions[index] = pos;
        for mapping in &self.mappings {
            if mapping.macro_index == index {
                let value = mapping.evaluate(pos);
                engine.set_param_at(mapping.target_slot, mapping.target_param, value);
            }
        }
    }

    /// Re-apply all current macro positions to `engine`.
    ///
    /// Useful after an engine topology change (e.g., after loading a preset)
    /// to ensure all macro-driven parameters reflect the current knob positions.
    pub fn apply_all(&mut self, engine: &mut GraphEngine) {
        for i in 0..N {
            let pos = self.positions[i];
            for mapping in &self.mappings {
                if mapping.macro_index == i {
                    let value = mapping.evaluate(pos);
                    engine.set_param_at(mapping.target_slot, mapping.target_param, value);
                }
            }
        }
    }

    /// Returns the number of registered mappings.
    pub fn mapping_count(&self) -> usize {
        self.mappings.len()
    }

    /// Returns how many mappings are registered for a given macro index.
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
    use super::*;

    // We can't easily construct a GraphEngine in a unit test without real effects,
    // so we test MacroMap logic directly where possible.

    #[test]
    fn test_macro_mapping_evaluate() {
        let m = MacroMapping {
            macro_index: 0,
            target_slot: 0,
            target_param: 0,
            min: 100.0,
            max: 200.0,
        };
        assert!((m.evaluate(0.0) - 100.0).abs() < 1e-6);
        assert!((m.evaluate(1.0) - 200.0).abs() < 1e-6);
        assert!((m.evaluate(0.5) - 150.0).abs() < 1e-6);
    }

    #[test]
    fn test_macro_mapping_inverted() {
        let m = MacroMapping {
            macro_index: 0,
            target_slot: 0,
            target_param: 0,
            min: 1.0,
            max: 0.0,
        };
        assert!((m.evaluate(0.0) - 1.0).abs() < 1e-6);
        assert!((m.evaluate(1.0) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn test_macro_mapping_clamping() {
        let m = MacroMapping {
            macro_index: 0,
            target_slot: 0,
            target_param: 0,
            min: 0.0,
            max: 10.0,
        };
        assert!((m.evaluate(-1.0) - 0.0).abs() < 1e-6);
        assert!((m.evaluate(2.0) - 10.0).abs() < 1e-6);
    }

    #[test]
    fn test_macro_map_new() {
        let map: MacroMap<4> = MacroMap::new();
        assert_eq!(map.mapping_count(), 0);
        for i in 0..4 {
            assert_eq!(map.position(i), 0.0);
        }
    }

    #[test]
    fn test_macro_map_add_and_count() {
        let mut map: MacroMap<4> = MacroMap::new();
        map.add_mapping(MacroMapping {
            macro_index: 0,
            target_slot: 0,
            target_param: 0,
            min: 0.0,
            max: 1.0,
        });
        map.add_mapping(MacroMapping {
            macro_index: 0,
            target_slot: 1,
            target_param: 2,
            min: 0.0,
            max: 10.0,
        });
        map.add_mapping(MacroMapping {
            macro_index: 1,
            target_slot: 2,
            target_param: 0,
            min: 0.5,
            max: 2.0,
        });
        assert_eq!(map.mapping_count(), 3);
        assert_eq!(map.mapping_count_for(0), 2);
        assert_eq!(map.mapping_count_for(1), 1);
        assert_eq!(map.mapping_count_for(2), 0);
    }

    #[test]
    fn test_macro_map_clear_macro() {
        let mut map: MacroMap<4> = MacroMap::new();
        map.add_mapping(MacroMapping {
            macro_index: 0,
            target_slot: 0,
            target_param: 0,
            min: 0.0,
            max: 1.0,
        });
        map.add_mapping(MacroMapping {
            macro_index: 1,
            target_slot: 1,
            target_param: 0,
            min: 0.0,
            max: 1.0,
        });
        map.clear_macro(0);
        assert_eq!(map.mapping_count(), 1);
        assert_eq!(map.mapping_count_for(0), 0);
        assert_eq!(map.mapping_count_for(1), 1);
    }

    #[test]
    fn test_macro_map_position_updates() {
        let mut map: MacroMap<4> = MacroMap::new();
        // Positions update even without mappings
        // We can't call set_macro without an engine, but we test position tracking separately
        assert_eq!(map.position(0), 0.0);
        // Manually set via positions array for test purposes
        map.positions[0] = 0.75;
        assert!((map.position(0) - 0.75).abs() < 1e-6);
    }

    #[test]
    #[should_panic]
    fn test_macro_map_out_of_range_panics() {
        let mut map: MacroMap<4> = MacroMap::new();
        map.add_mapping(MacroMapping {
            macro_index: 4, // out of range for N=4
            target_slot: 0,
            target_param: 0,
            min: 0.0,
            max: 1.0,
        });
    }
}
