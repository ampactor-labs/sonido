//! Graph-aware control-to-parameter mapping for multi-effect DAG routing.
//!
//! [`GraphMapper`] extends the single-effect [`ControlMapper`](crate::ControlMapper) pattern
//! to graph topologies. Each mapping routes a physical control to a `(NodeId, param_index)`
//! pair, enabling one knob to target parameters across multiple effects in the graph.
//!
//! # Design
//!
//! Const-generic `N` sets the maximum number of mappings (fixed-size array, zero heap).
//! Same pattern as [`ControlMapper<N>`](crate::ControlMapper) but graph-aware.
//!
//! # Example
//!
//! ```rust,ignore
//! use sonido_platform::{GraphMapper, ControlId};
//! use sonido_core::graph::NodeId;
//!
//! let mut mapper = GraphMapper::<16>::new();
//!
//! // Map hardware knob 0 to effect A's param 2
//! mapper.map(ControlId::hardware(0), effect_a_id, 2);
//!
//! // Apply the control value to the graph
//! let updated = mapper.apply(ControlId::hardware(0), 0.75, &mut graph);
//! assert_eq!(updated, 1);
//! ```

use crate::ControlId;
use sonido_core::graph::{NodeId, ProcessingGraph};

/// A mapping entry from a control to a specific parameter on a graph node.
#[derive(Debug, Clone, Copy, PartialEq)]
struct GraphMappingEntry {
    /// The control ID this mapping is for.
    control_id: ControlId,
    /// The target node in the graph.
    node_id: NodeId,
    /// The parameter index on the target node's effect.
    param_index: usize,
}

/// Graph-aware control-to-parameter mapper.
///
/// Routes physical controls to `(NodeId, param_index)` pairs across multiple
/// effects in a [`ProcessingGraph`]. Const-generic `N` sets maximum mapping
/// capacity (fixed-size array, no heap allocation).
///
/// A single control can map to multiple parameters (one-to-many). Use
/// [`map()`](Self::map) to add entries and [`apply()`](Self::apply) to push
/// a control value to all its targets.
///
/// # Type Parameter
///
/// - `N`: Maximum number of mapping entries (compile-time constant for no_std)
#[derive(Debug, Clone)]
pub struct GraphMapper<const N: usize> {
    mappings: [Option<GraphMappingEntry>; N],
    count: usize,
}

impl<const N: usize> GraphMapper<N> {
    /// Creates a new empty graph mapper.
    pub const fn new() -> Self {
        Self {
            mappings: [None; N],
            count: 0,
        }
    }

    /// Returns the number of active mappings.
    #[inline]
    pub const fn len(&self) -> usize {
        self.count
    }

    /// Returns true if there are no mappings.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Returns the maximum number of mappings.
    #[inline]
    pub const fn capacity(&self) -> usize {
        N
    }

    /// Maps a control to a specific effect parameter in the graph.
    ///
    /// If an identical mapping (same control, same node, same param) already
    /// exists, this is a no-op returning `true`. Multiple mappings from the
    /// same control to different targets are allowed (one-to-many).
    ///
    /// Returns `false` if at capacity.
    pub fn map(&mut self, control_id: ControlId, node_id: NodeId, param_index: usize) -> bool {
        // Check for duplicate.
        for entry in self.mappings.iter().flatten() {
            if entry.control_id == control_id
                && entry.node_id == node_id
                && entry.param_index == param_index
            {
                return true;
            }
        }

        // Find empty slot.
        for slot in self.mappings.iter_mut() {
            if slot.is_none() {
                *slot = Some(GraphMappingEntry {
                    control_id,
                    node_id,
                    param_index,
                });
                self.count += 1;
                return true;
            }
        }

        false
    }

    /// Removes all mappings for a control.
    ///
    /// Returns the number of mappings removed.
    pub fn unmap_all(&mut self, control_id: ControlId) -> usize {
        let mut removed = 0;
        for slot in self.mappings.iter_mut() {
            if let Some(entry) = slot
                && entry.control_id == control_id
            {
                *slot = None;
                self.count -= 1;
                removed += 1;
            }
        }
        removed
    }

    /// Removes a specific mapping.
    ///
    /// Returns `true` if the mapping was found and removed.
    pub fn unmap(&mut self, control_id: ControlId, node_id: NodeId, param_index: usize) -> bool {
        for slot in self.mappings.iter_mut() {
            if let Some(entry) = slot
                && entry.control_id == control_id
                && entry.node_id == node_id
                && entry.param_index == param_index
            {
                *slot = None;
                self.count -= 1;
                return true;
            }
        }
        false
    }

    /// Applies a normalized control value to all mapped parameters in the graph.
    ///
    /// For each mapping matching `control_id`, reads the parameter descriptor
    /// from the graph node, denormalizes the value, and sets it.
    ///
    /// Returns the number of parameters updated.
    pub fn apply(
        &self,
        control_id: ControlId,
        normalized_value: f32,
        graph: &mut ProcessingGraph,
    ) -> usize {
        let mut updated = 0;
        for entry in self.mappings.iter().flatten() {
            if entry.control_id == control_id
                && let Some(ewp) = graph.effect_with_params_mut(entry.node_id)
                && let Some(desc) = ewp.effect_param_info(entry.param_index)
            {
                let value = desc.denormalize(normalized_value);
                ewp.effect_set_param(entry.param_index, value);
                updated += 1;
            }
        }
        updated
    }

    /// Clears all mappings.
    pub fn clear(&mut self) {
        for slot in self.mappings.iter_mut() {
            *slot = None;
        }
        self.count = 0;
    }
}

impl<const N: usize> Default for GraphMapper<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::*;
    use alloc::boxed::Box;
    use sonido_core::effect::Effect;
    use sonido_core::param_info::{ParamDescriptor, ParameterInfo};

    struct TestGain {
        gain_db: f32,
    }

    impl TestGain {
        fn new() -> Self {
            Self { gain_db: 0.0 }
        }
    }

    impl Effect for TestGain {
        fn process(&mut self, input: f32) -> f32 {
            input
        }
        fn set_sample_rate(&mut self, _: f32) {}
        fn reset(&mut self) {}
    }

    impl ParameterInfo for TestGain {
        fn param_count(&self) -> usize {
            1
        }
        fn param_info(&self, index: usize) -> Option<ParamDescriptor> {
            match index {
                0 => Some(ParamDescriptor::gain_db("Gain", "Gain", -60.0, 12.0, 0.0)),
                _ => None,
            }
        }
        fn get_param(&self, index: usize) -> f32 {
            match index {
                0 => self.gain_db,
                _ => 0.0,
            }
        }
        fn set_param(&mut self, index: usize, value: f32) {
            if index == 0 {
                self.gain_db = value.clamp(-60.0, 12.0);
            }
        }
    }

    #[test]
    fn test_graph_mapper_new() {
        let mapper = GraphMapper::<8>::new();
        assert!(mapper.is_empty());
        assert_eq!(mapper.len(), 0);
        assert_eq!(mapper.capacity(), 8);
    }

    #[test]
    fn test_graph_mapper_map_and_apply() {
        let mut graph = ProcessingGraph::new(48000.0, 64);
        let input = graph.add_input();
        let effect = graph.add_effect(Box::new(TestGain::new()));
        let output = graph.add_output();
        graph.connect(input, effect).unwrap();
        graph.connect(effect, output).unwrap();
        graph.compile().unwrap();

        let mut mapper = GraphMapper::<8>::new();
        assert!(mapper.map(ControlId::hardware(0), effect, 0));
        assert_eq!(mapper.len(), 1);

        // Apply midpoint → denormalize to -24 dB.
        let updated = mapper.apply(ControlId::hardware(0), 0.5, &mut graph);
        assert_eq!(updated, 1);

        let ewp = graph.effect_with_params_ref(effect).unwrap();
        let gain = ewp.effect_get_param(0);
        assert!((gain - (-24.0)).abs() < 0.01, "expected -24.0, got {gain}");
    }

    #[test]
    fn test_graph_mapper_one_to_many() {
        let mut graph = ProcessingGraph::new(48000.0, 64);
        let input = graph.add_input();
        let split = graph.add_split();
        let a = graph.add_effect(Box::new(TestGain::new()));
        let b = graph.add_effect(Box::new(TestGain::new()));
        let merge = graph.add_merge();
        let output = graph.add_output();

        graph.connect(input, split).unwrap();
        graph.connect(split, a).unwrap();
        graph.connect(split, b).unwrap();
        graph.connect(a, merge).unwrap();
        graph.connect(b, merge).unwrap();
        graph.connect(merge, output).unwrap();
        graph.compile().unwrap();

        let mut mapper = GraphMapper::<8>::new();
        mapper.map(ControlId::hardware(0), a, 0);
        mapper.map(ControlId::hardware(0), b, 0);
        assert_eq!(mapper.len(), 2);

        let updated = mapper.apply(ControlId::hardware(0), 1.0, &mut graph);
        assert_eq!(updated, 2);

        // Both effects should have gain = 12.0 dB (max).
        let gain_a = graph.effect_with_params_ref(a).unwrap().effect_get_param(0);
        let gain_b = graph.effect_with_params_ref(b).unwrap().effect_get_param(0);
        assert!((gain_a - 12.0).abs() < 0.01);
        assert!((gain_b - 12.0).abs() < 0.01);
    }

    #[test]
    fn test_graph_mapper_unmap_all() {
        let mut mapper = GraphMapper::<8>::new();
        let id = ControlId::hardware(0);
        let node = NodeId::sentinel();

        mapper.map(id, node, 0);
        mapper.map(id, node, 1);
        assert_eq!(mapper.len(), 2);

        let removed = mapper.unmap_all(id);
        assert_eq!(removed, 2);
        assert!(mapper.is_empty());
    }

    #[test]
    fn test_graph_mapper_unmap_specific() {
        let mut mapper = GraphMapper::<8>::new();
        let id = ControlId::hardware(0);
        let node = NodeId::sentinel();

        mapper.map(id, node, 0);
        mapper.map(id, node, 1);

        assert!(mapper.unmap(id, node, 0));
        assert_eq!(mapper.len(), 1);
        assert!(!mapper.unmap(id, node, 0)); // Already removed.
    }

    #[test]
    fn test_graph_mapper_capacity() {
        let mut mapper = GraphMapper::<2>::new();
        let node = NodeId::sentinel();

        assert!(mapper.map(ControlId::hardware(0), node, 0));
        assert!(mapper.map(ControlId::hardware(1), node, 1));
        assert!(!mapper.map(ControlId::hardware(2), node, 2));
    }

    #[test]
    fn test_graph_mapper_duplicate_ignored() {
        let mut mapper = GraphMapper::<8>::new();
        let node = NodeId::sentinel();

        mapper.map(ControlId::hardware(0), node, 0);
        mapper.map(ControlId::hardware(0), node, 0); // Duplicate.
        assert_eq!(mapper.len(), 1);
    }

    #[test]
    fn test_graph_mapper_clear() {
        let mut mapper = GraphMapper::<8>::new();
        let node = NodeId::sentinel();

        mapper.map(ControlId::hardware(0), node, 0);
        mapper.map(ControlId::hardware(1), node, 1);
        mapper.clear();

        assert!(mapper.is_empty());
    }
}
