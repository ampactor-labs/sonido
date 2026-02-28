//! Graph-based audio processing engine.
//!
//! [`GraphEngine`] wraps [`ProcessingGraph`] to provide a higher-level API for
//! audio processing with DAG-based routing. It manages linear chain topology
//! (add/remove/reorder effects) and provides block/file processing methods.
//!
//! # Linear Chain Management
//!
//! For linear chains (the common case), `GraphEngine` maintains an ordered
//! `chain_order` list of effect [`NodeId`]s. Use [`add_effect()`](GraphEngine::add_effect),
//! [`remove_effect()`](GraphEngine::remove_effect), and
//! [`reorder()`](GraphEngine::reorder) to mutate the chain.
//!
//! ```rust,ignore
//! use sonido_core::graph::GraphEngine;
//! use sonido_effects::{Distortion, Reverb};
//!
//! let mut engine = GraphEngine::new_linear(48000.0, 256);
//! let dist = engine.add_effect(Box::new(Distortion::new(48000.0)));
//! let reverb = engine.add_effect(Box::new(Reverb::new(48000.0)));
//!
//! engine.process_block_stereo(&left_in, &right_in, &mut left_out, &mut right_out);
//! ```

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::String, vec, vec::Vec};

use crate::EffectWithParams;
use crate::param_info::ParamDescriptor;
use crate::tempo::TempoContext;

use super::stereo_samples::StereoSamples;
use super::{GraphError, NodeId, ProcessingGraph};

/// Graph-based processing engine for DAG audio routing.
///
/// Wraps [`ProcessingGraph`] with a convenient API for common operations.
/// For linear chains, maintains an ordered list of effect node IDs enabling
/// add/remove/reorder operations with automatic graph recompilation.
pub struct GraphEngine {
    graph: ProcessingGraph,
    /// Ordered list of effect `NodeId`s in the linear chain.
    chain_order: Vec<NodeId>,
    /// Registry effect IDs, parallel to `chain_order`.
    effect_ids: Vec<&'static str>,
    /// The Input node (always present).
    input_node: NodeId,
    /// The Output node (always present).
    output_node: NodeId,
    /// Pre-allocated scratch buffer (left channel) for in-place processing.
    scratch_left: Vec<f32>,
    /// Pre-allocated scratch buffer (right channel) for in-place processing.
    scratch_right: Vec<f32>,
}

impl GraphEngine {
    /// Creates an empty linear engine: Input → Output, no effects.
    ///
    /// Ready for [`add_effect()`](Self::add_effect) calls.
    pub fn new_linear(sample_rate: f32, block_size: usize) -> Self {
        let mut graph = ProcessingGraph::new(sample_rate, block_size);
        let input_node = graph.add_input();
        let output_node = graph.add_output();
        graph.connect(input_node, output_node).unwrap();
        graph.compile().unwrap();

        Self {
            graph,
            chain_order: Vec::new(),
            effect_ids: Vec::new(),
            input_node,
            output_node,
            scratch_left: vec![0.0; block_size],
            scratch_right: vec![0.0; block_size],
        }
    }

    /// Creates a `GraphEngine` from an already-configured [`ProcessingGraph`].
    ///
    /// The graph must already be compiled (via [`ProcessingGraph::compile()`]).
    /// Note: chain management methods (`add_effect`, `remove_effect`, `reorder`)
    /// won't work correctly unless `chain_order`, `input_node`, and `output_node`
    /// are properly set. Prefer [`new_linear()`](Self::new_linear) or
    /// [`from_chain()`](Self::from_chain) for linear chains.
    pub fn new(graph: ProcessingGraph) -> Self {
        let block_size = graph.block_size();
        Self {
            graph,
            chain_order: Vec::new(),
            effect_ids: Vec::new(),
            input_node: NodeId::sentinel(),
            output_node: NodeId::sentinel(),
            scratch_left: vec![0.0; block_size],
            scratch_right: vec![0.0; block_size],
        }
    }

    /// Creates a linear effect chain: Input → E1 → E2 → ... → En → Output.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError`] if graph construction or compilation fails.
    pub fn from_chain(
        effects: Vec<Box<dyn EffectWithParams + Send>>,
        sample_rate: f32,
        block_size: usize,
    ) -> Result<Self, GraphError> {
        let mut graph = ProcessingGraph::new(sample_rate, block_size);
        let input_node = graph.add_input();

        let count = effects.len();
        let mut chain_order = Vec::with_capacity(count);
        let mut prev = input_node;
        for effect in effects {
            let node = graph.add_effect(effect);
            graph.connect(prev, node)?;
            chain_order.push(node);
            prev = node;
        }

        let output_node = graph.add_output();
        graph.connect(prev, output_node)?;
        graph.compile()?;

        Ok(Self {
            graph,
            chain_order,
            effect_ids: vec![""; count],
            input_node,
            output_node,
            scratch_left: vec![0.0; block_size],
            scratch_right: vec![0.0; block_size],
        })
    }

    // --- Chain management ---

    /// Appends an effect to the end of the linear chain.
    ///
    /// Returns the new effect's [`NodeId`]. The graph is recompiled automatically.
    pub fn add_effect(&mut self, effect: Box<dyn EffectWithParams + Send>) -> NodeId {
        let node = self.graph.add_effect(effect);

        // Find the current last node before Output.
        let prev = self.chain_order.last().copied().unwrap_or(self.input_node);

        // Disconnect prev → Output.
        self.disconnect_between(prev, self.output_node);

        // Connect prev → new → Output.
        self.graph.connect(prev, node).unwrap();
        self.graph.connect(node, self.output_node).unwrap();
        self.chain_order.push(node);
        self.effect_ids.push("");

        self.graph.compile().unwrap();
        node
    }

    /// Appends a named effect to the end of the linear chain.
    ///
    /// Returns the slot index (0-based position). The `id` is a registry effect ID
    /// (e.g., `"distortion"`, `"reverb"`) stored for snapshot/restore workflows.
    pub fn add_effect_named(
        &mut self,
        effect: Box<dyn EffectWithParams + Send>,
        id: &'static str,
    ) -> usize {
        let node = self.graph.add_effect(effect);

        let prev = self.chain_order.last().copied().unwrap_or(self.input_node);
        self.disconnect_between(prev, self.output_node);

        self.graph.connect(prev, node).unwrap();
        self.graph.connect(node, self.output_node).unwrap();
        self.chain_order.push(node);
        self.effect_ids.push(id);

        self.graph.compile().unwrap();
        self.chain_order.len() - 1
    }

    /// Removes an effect from the chain and returns it.
    ///
    /// Returns `None` if the node doesn't exist or isn't in the chain.
    /// The graph is recompiled automatically.
    pub fn remove_effect(&mut self, id: NodeId) -> Option<Box<dyn EffectWithParams + Send>> {
        let pos = self.chain_order.iter().position(|&n| n == id)?;

        // Determine predecessor and successor in the chain.
        let pred = if pos == 0 {
            self.input_node
        } else {
            self.chain_order[pos - 1]
        };
        let succ = if pos == self.chain_order.len() - 1 {
            self.output_node
        } else {
            self.chain_order[pos + 1]
        };

        // Take the effect out.
        let effect = self.graph.take_effect(id)?;

        // Remove the node (cleans up its edges).
        self.graph.remove_node(id).ok()?;
        self.chain_order.remove(pos);
        self.effect_ids.remove(pos);

        // Reconnect pred → succ.
        self.graph.connect(pred, succ).unwrap();
        self.graph.compile().unwrap();

        Some(effect)
    }

    /// Removes an effect by slot index and returns it.
    ///
    /// Elements after the removed slot shift left (preserves ordering).
    /// Returns `None` if `slot` is out of bounds.
    pub fn remove_at(&mut self, slot: usize) -> Option<Box<dyn EffectWithParams + Send>> {
        if slot >= self.chain_order.len() {
            return None;
        }
        let node_id = self.chain_order[slot];
        self.remove_effect(node_id)
    }

    /// Reorders effects in the linear chain.
    ///
    /// `order` must contain exactly the same `NodeId`s as the current chain,
    /// in the desired new order. The graph is recompiled automatically.
    ///
    /// # Panics
    ///
    /// Panics if `order` doesn't match the current chain's node IDs.
    pub fn reorder(&mut self, order: &[NodeId]) {
        assert_eq!(
            order.len(),
            self.chain_order.len(),
            "reorder: length mismatch"
        );

        // Build NodeId → old effect_id mapping before mutation.
        let id_map: Vec<(NodeId, &'static str)> = self
            .chain_order
            .iter()
            .zip(self.effect_ids.iter())
            .map(|(&n, &id)| (n, id))
            .collect();

        // Disconnect all effect-to-effect and input/output-to-effect edges.
        // First: input → first effect.
        let first_old = self
            .chain_order
            .first()
            .copied()
            .unwrap_or(self.output_node);
        self.disconnect_between(self.input_node, first_old);

        // Effect-to-effect edges.
        for i in 0..self.chain_order.len() {
            let current = self.chain_order[i];
            let next = if i + 1 < self.chain_order.len() {
                self.chain_order[i + 1]
            } else {
                self.output_node
            };
            self.disconnect_between(current, next);
        }

        // Reconnect in new order.
        let mut prev = self.input_node;
        for &node_id in order {
            self.graph.connect(prev, node_id).unwrap();
            prev = node_id;
        }
        self.graph.connect(prev, self.output_node).unwrap();

        // Rebuild effect_ids in new order.
        self.effect_ids = order
            .iter()
            .map(|node| {
                id_map
                    .iter()
                    .find(|(n, _)| n == node)
                    .map_or("", |(_, id)| id)
            })
            .collect();

        self.chain_order = order.to_vec();
        self.graph.compile().unwrap();
    }

    /// Reorders effects by slot indices.
    ///
    /// `order` must contain exactly `slot_count()` indices, each in `0..slot_count()`.
    /// Translates slot indices to `NodeId`s and delegates to [`reorder()`](Self::reorder).
    ///
    /// # Panics
    ///
    /// Panics if `order` length doesn't match slot count or indices are out of bounds.
    pub fn reorder_slots(&mut self, order: &[usize]) {
        let node_ids: Vec<NodeId> = order.iter().map(|&slot| self.chain_order[slot]).collect();
        self.reorder(&node_ids);
    }

    /// Removes all effects, leaving an empty Input → Output passthrough.
    ///
    /// Preserves the current sample rate and block size.
    pub fn clear(&mut self) {
        let sr = self.graph.sample_rate();
        let bs = self.graph.block_size();
        *self = Self::new_linear(sr, bs);
    }

    /// Returns the number of effects in the chain.
    pub fn effect_count(&self) -> usize {
        self.chain_order.len()
    }

    /// Returns true if the chain has no effects.
    pub fn is_empty(&self) -> bool {
        self.chain_order.is_empty()
    }

    /// Returns the ordered list of effect `NodeId`s in the chain.
    pub fn chain_order(&self) -> &[NodeId] {
        &self.chain_order
    }

    /// Returns the number of effect slots (alias for [`effect_count()`](Self::effect_count)).
    pub fn slot_count(&self) -> usize {
        self.chain_order.len()
    }

    /// Returns the registry effect IDs for all slots.
    pub fn effect_ids(&self) -> &[&'static str] {
        &self.effect_ids
    }

    /// Returns the registry effect ID at a slot, or `None` if out of bounds.
    pub fn effect_id_at(&self, slot: usize) -> Option<&'static str> {
        self.effect_ids.get(slot).copied()
    }

    // --- Slot-indexed parameter access ---

    /// Sets a parameter value on an effect by slot and parameter index.
    ///
    /// Returns `true` if the parameter was set, `false` if slot or param is out of bounds.
    pub fn set_param_at(&mut self, slot: usize, param: usize, value: f32) -> bool {
        if let Some(&node_id) = self.chain_order.get(slot)
            && let Some(ewp) = self.graph.effect_with_params_mut(node_id)
            && param < ewp.effect_param_count()
        {
            ewp.effect_set_param(param, value);
            return true;
        }
        false
    }

    /// Gets a parameter value from an effect by slot and parameter index.
    pub fn get_param_at(&self, slot: usize, param: usize) -> Option<f32> {
        let &node_id = self.chain_order.get(slot)?;
        let ewp = self.graph.effect_with_params_ref(node_id)?;
        if param < ewp.effect_param_count() {
            Some(ewp.effect_get_param(param))
        } else {
            None
        }
    }

    /// Returns the number of parameters for the effect at a slot.
    ///
    /// Returns 0 if the slot is out of bounds.
    pub fn param_count_at(&self, slot: usize) -> usize {
        self.chain_order
            .get(slot)
            .and_then(|&id| self.graph.effect_with_params_ref(id))
            .map_or(0, |ewp| ewp.effect_param_count())
    }

    /// Returns a parameter descriptor by slot and parameter index.
    pub fn param_descriptor_at(&self, slot: usize, param: usize) -> Option<ParamDescriptor> {
        let &node_id = self.chain_order.get(slot)?;
        let ewp = self.graph.effect_with_params_ref(node_id)?;
        ewp.effect_param_info(param)
    }

    /// Returns a reference to the effect at a slot.
    pub fn effect_at(&self, slot: usize) -> Option<&(dyn EffectWithParams + Send)> {
        let &node_id = self.chain_order.get(slot)?;
        self.graph.effect_with_params_ref(node_id)
    }

    /// Returns a mutable reference to the effect at a slot.
    pub fn effect_at_mut(&mut self, slot: usize) -> Option<&mut (dyn EffectWithParams + Send)> {
        let &node_id = self.chain_order.get(slot)?;
        self.graph.effect_with_params_mut(node_id)
    }

    /// Sets the bypass state for an effect at a slot.
    pub fn set_bypass_at(&mut self, slot: usize, bypassed: bool) {
        if let Some(&node_id) = self.chain_order.get(slot) {
            self.graph.set_bypass(node_id, bypassed);
        }
    }

    /// Returns whether the effect at a slot is bypassed.
    ///
    /// Returns `false` if the slot is out of bounds.
    pub fn is_bypassed_at(&self, slot: usize) -> bool {
        self.chain_order
            .get(slot)
            .is_some_and(|&id| self.graph.is_bypassed(id))
    }

    /// Captures the current chain state as a [`GraphSnapshot`].
    ///
    /// Each entry contains the effect ID, all parameter values, and bypass state.
    pub fn snapshot(&self) -> GraphSnapshot {
        let entries = self
            .chain_order
            .iter()
            .zip(self.effect_ids.iter())
            .map(|(&node_id, &eid)| {
                let (params, bypassed) = self
                    .graph
                    .effect_with_params_ref(node_id)
                    .map(|ewp| {
                        let count = ewp.effect_param_count();
                        let params: Vec<f32> =
                            (0..count).map(|i| ewp.effect_get_param(i)).collect();
                        (params, self.graph.is_bypassed(node_id))
                    })
                    .unwrap_or_default();

                SnapshotEntry {
                    effect_id: String::from(eid),
                    params,
                    bypassed,
                }
            })
            .collect();

        GraphSnapshot { entries }
    }

    // --- Parameter / effect access (NodeId-based) ---

    /// Returns a mutable reference to an effect's [`EffectWithParams`] interface.
    pub fn effect_with_params_mut(
        &mut self,
        id: NodeId,
    ) -> Option<&mut (dyn EffectWithParams + Send)> {
        self.graph.effect_with_params_mut(id)
    }

    /// Returns a reference to an effect's [`EffectWithParams`] interface.
    pub fn effect_with_params_ref(&self, id: NodeId) -> Option<&(dyn EffectWithParams + Send)> {
        self.graph.effect_with_params_ref(id)
    }

    // --- Graph access ---

    /// Returns a mutable reference to the underlying [`ProcessingGraph`].
    ///
    /// Use this for direct graph mutations (bypass, advanced DAG operations).
    /// Chain management methods may not work correctly after arbitrary graph mutations.
    pub fn graph_mut(&mut self) -> &mut ProcessingGraph {
        &mut self.graph
    }

    /// Returns a reference to the underlying [`ProcessingGraph`].
    pub fn graph(&self) -> &ProcessingGraph {
        &self.graph
    }

    // --- Control ---

    /// Returns the sample rate.
    pub fn sample_rate(&self) -> f32 {
        self.graph.sample_rate()
    }

    /// Sets the sample rate for all effect nodes.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.graph.set_sample_rate(sample_rate);
    }

    /// Resets all effect nodes and clears delay lines.
    pub fn reset(&mut self) {
        self.graph.reset();
    }

    /// Returns the total graph latency in samples.
    pub fn latency_samples(&self) -> usize {
        self.graph.latency_samples()
    }

    /// Broadcasts a tempo context to all effect nodes.
    pub fn set_tempo_context(&mut self, ctx: &TempoContext) {
        self.graph.set_tempo_context(ctx);
    }

    // --- Processing ---

    /// Processes a block of stereo audio through the graph.
    ///
    /// Output buffers must be at least as large as input buffers.
    pub fn process_block_stereo(
        &mut self,
        left_in: &[f32],
        right_in: &[f32],
        left_out: &mut [f32],
        right_out: &mut [f32],
    ) {
        self.graph
            .process_block(left_in, right_in, left_out, right_out);
    }

    /// Processes a block of mono audio through the graph.
    ///
    /// Feeds `input` to both L/R channels, returns the left channel.
    pub fn process_block(&mut self, input: &[f32], output: &mut [f32]) {
        let len = input.len();
        debug_assert!(output.len() >= len);
        self.scratch_right.resize(len, 0.0);
        self.graph.process_block(
            input,
            input,
            &mut output[..len],
            &mut self.scratch_right[..len],
        );
    }

    /// Processes a block of mono audio in-place.
    pub fn process_block_inplace(&mut self, buffer: &mut [f32]) {
        let len = buffer.len();
        self.scratch_left.resize(len, 0.0);
        self.scratch_right.resize(len, 0.0);
        self.scratch_left[..len].copy_from_slice(&buffer[..len]);
        self.graph.process_block(
            &self.scratch_left[..len],
            &self.scratch_left[..len],
            buffer,
            &mut self.scratch_right[..len],
        );
    }

    /// Processes a block of stereo audio in-place.
    pub fn process_block_stereo_inplace(&mut self, left: &mut [f32], right: &mut [f32]) {
        let len = left.len();
        self.scratch_left.resize(len, 0.0);
        self.scratch_right.resize(len, 0.0);
        self.scratch_left[..len].copy_from_slice(&left[..len]);
        self.scratch_right[..len].copy_from_slice(&right[..len]);
        self.graph.process_block(
            &self.scratch_left[..len],
            &self.scratch_right[..len],
            &mut left[..len],
            &mut right[..len],
        );
    }

    /// Processes an entire mono file through the graph.
    ///
    /// Feeds mono input to both L/R channels, returns left channel.
    pub fn process_file(&mut self, input: &[f32], block_size: usize) -> Vec<f32> {
        let len = input.len();
        let mut output = vec![0.0; len];
        let mut right_out = vec![0.0; block_size];

        for i in (0..len).step_by(block_size) {
            let chunk_len = block_size.min(len - i);
            let end = i + chunk_len;
            self.graph.process_block(
                &input[i..end],
                &input[i..end],
                &mut output[i..end],
                &mut right_out[..chunk_len],
            );
        }

        output
    }

    /// Processes an entire stereo file through the graph.
    pub fn process_file_stereo(
        &mut self,
        input: &StereoSamples,
        block_size: usize,
    ) -> StereoSamples {
        let len = input.len();
        let mut left_out = vec![0.0; len];
        let mut right_out = vec![0.0; len];

        for i in (0..len).step_by(block_size) {
            let chunk_len = block_size.min(len - i);
            let end = i + chunk_len;

            self.graph.process_block(
                &input.left[i..end],
                &input.right[i..end],
                &mut left_out[i..end],
                &mut right_out[i..end],
            );
        }

        StereoSamples::new(left_out, right_out)
    }

    // --- Internal helpers ---

    /// Disconnects the edge between `from` and `to`, if it exists.
    fn disconnect_between(&mut self, from: NodeId, to: NodeId) {
        if let Some(edge_id) = self.graph.find_edge(from, to) {
            self.graph.disconnect(edge_id).unwrap();
        }
    }
}

/// Snapshot of the entire chain state for save/restore workflows.
///
/// Captures effect IDs, parameter values, and bypass states.
/// Owned strings allow deserialization from external sources.
#[derive(Debug, Clone)]
pub struct GraphSnapshot {
    /// One entry per effect slot, in chain order.
    pub entries: Vec<SnapshotEntry>,
}

/// State of a single effect slot in a [`GraphSnapshot`].
#[derive(Debug, Clone)]
pub struct SnapshotEntry {
    /// Registry effect ID (e.g., `"distortion"`, `"reverb"`).
    pub effect_id: String,
    /// All parameter values in index order.
    pub params: Vec<f32>,
    /// Whether the effect is bypassed.
    pub bypassed: bool,
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use alloc::boxed::Box;
    use alloc::vec;
    use alloc::vec::Vec;

    use super::*;
    use crate::Effect;
    use crate::param_info::{ParamDescriptor, ParameterInfo};

    /// Simple test effect that multiplies by a constant.
    struct Gain {
        factor: f32,
    }

    impl Effect for Gain {
        fn process(&mut self, input: f32) -> f32 {
            input * self.factor
        }
        fn set_sample_rate(&mut self, _sample_rate: f32) {}
        fn reset(&mut self) {}
    }

    impl ParameterInfo for Gain {
        fn param_count(&self) -> usize {
            1
        }
        fn param_info(&self, index: usize) -> Option<ParamDescriptor> {
            match index {
                0 => Some(ParamDescriptor::custom("Factor", "Factor", 0.0, 10.0, 1.0)),
                _ => None,
            }
        }
        fn get_param(&self, index: usize) -> f32 {
            match index {
                0 => self.factor,
                _ => 0.0,
            }
        }
        fn set_param(&mut self, index: usize, value: f32) {
            if index == 0 {
                self.factor = value;
            }
        }
    }

    fn gain(factor: f32) -> Box<dyn EffectWithParams + Send> {
        Box::new(Gain { factor })
    }

    /// Process enough blocks to let the crossfade settle.
    fn settle_crossfade(engine: &mut GraphEngine) {
        let bs = engine.graph().block_size();
        let blocks_needed = (2880 / bs) + 1;
        let mut left = vec![0.0; bs];
        let mut right = vec![0.0; bs];
        let input = vec![0.0; bs];
        for _ in 0..blocks_needed {
            engine.process_block_stereo(&input, &input, &mut left, &mut right);
        }
    }

    #[test]
    fn test_from_chain_passthrough() {
        let mut engine = GraphEngine::from_chain(vec![gain(1.0)], 48000.0, 256).unwrap();

        let left_in = [1.0, 2.0, 3.0, 4.0];
        let right_in = [0.5, 1.0, 1.5, 2.0];
        let mut left_out = [0.0; 4];
        let mut right_out = [0.0; 4];

        engine.process_block_stereo(&left_in, &right_in, &mut left_out, &mut right_out);

        assert_eq!(left_out, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(right_out, [0.5, 1.0, 1.5, 2.0]);
    }

    #[test]
    fn test_process_file_stereo() {
        let mut engine = GraphEngine::from_chain(vec![gain(0.5)], 48000.0, 64).unwrap();

        let left: Vec<f32> = (0..1000).map(|i| i as f32).collect();
        let right: Vec<f32> = (0..1000).map(|i| (i as f32) * 2.0).collect();
        let input = StereoSamples::new(left.clone(), right.clone());

        let output = engine.process_file_stereo(&input, 64);

        assert_eq!(output.len(), 1000);
        for i in 0..output.len() {
            assert!(
                (output.left[i] - left[i] * 0.5).abs() < 1e-6,
                "left mismatch at {i}"
            );
            assert!(
                (output.right[i] - right[i] * 0.5).abs() < 1e-6,
                "right mismatch at {i}"
            );
        }
    }

    #[test]
    fn test_graph_engine_latency() {
        struct LatentGain {
            factor: f32,
            latency: usize,
        }

        impl Effect for LatentGain {
            fn process(&mut self, input: f32) -> f32 {
                input * self.factor
            }
            fn set_sample_rate(&mut self, _: f32) {}
            fn reset(&mut self) {}
            fn latency_samples(&self) -> usize {
                self.latency
            }
        }

        impl ParameterInfo for LatentGain {
            fn param_count(&self) -> usize {
                0
            }
            fn param_info(&self, _: usize) -> Option<ParamDescriptor> {
                None
            }
            fn get_param(&self, _: usize) -> f32 {
                0.0
            }
            fn set_param(&mut self, _: usize, _: f32) {}
        }

        let effects: Vec<Box<dyn EffectWithParams + Send>> = vec![
            Box::new(LatentGain {
                factor: 1.0,
                latency: 64,
            }),
            Box::new(LatentGain {
                factor: 1.0,
                latency: 128,
            }),
        ];
        let engine = GraphEngine::from_chain(effects, 48000.0, 256).unwrap();
        assert_eq!(engine.latency_samples(), 192);
    }

    #[test]
    fn test_new_linear_empty() {
        let engine = GraphEngine::new_linear(48000.0, 256);
        assert!(engine.is_empty());
        assert_eq!(engine.effect_count(), 0);
    }

    #[test]
    fn test_new_linear_passthrough() {
        let mut engine = GraphEngine::new_linear(48000.0, 4);

        let left_in = [1.0, 2.0, 3.0, 4.0];
        let right_in = [0.5, 1.0, 1.5, 2.0];
        let mut left_out = [0.0; 4];
        let mut right_out = [0.0; 4];

        engine.process_block_stereo(&left_in, &right_in, &mut left_out, &mut right_out);

        assert_eq!(left_out, left_in);
        assert_eq!(right_out, right_in);
    }

    #[test]
    fn test_add_effect() {
        let mut engine = GraphEngine::new_linear(48000.0, 256);

        let id = engine.add_effect(gain(2.0));
        assert_eq!(engine.effect_count(), 1);
        assert_eq!(engine.chain_order(), &[id]);

        settle_crossfade(&mut engine);

        let left_in = vec![1.0; 256];
        let right_in = vec![0.5; 256];
        let mut left_out = vec![0.0; 256];
        let mut right_out = vec![0.0; 256];

        engine.process_block_stereo(&left_in, &right_in, &mut left_out, &mut right_out);
        for &s in &left_out {
            assert!((s - 2.0).abs() < 0.02, "expected 2.0, got {s}");
        }
    }

    #[test]
    fn test_add_multiple_effects() {
        let mut engine = GraphEngine::new_linear(48000.0, 256);

        let a = engine.add_effect(gain(2.0));
        let b = engine.add_effect(gain(3.0));
        assert_eq!(engine.effect_count(), 2);
        assert_eq!(engine.chain_order(), &[a, b]);

        settle_crossfade(&mut engine);

        let left_in = vec![1.0; 256];
        let mut left_out = vec![0.0; 256];
        let mut right_out = vec![0.0; 256];

        engine.process_block_stereo(&left_in, &left_in, &mut left_out, &mut right_out);
        for &s in &left_out {
            assert!((s - 6.0).abs() < 0.02, "expected 6.0, got {s}");
        }
    }

    #[test]
    fn test_remove_effect() {
        let mut engine = GraphEngine::new_linear(48000.0, 256);
        let a = engine.add_effect(gain(2.0));
        let b = engine.add_effect(gain(3.0));

        let removed = engine.remove_effect(a);
        assert!(removed.is_some());
        assert_eq!(engine.effect_count(), 1);
        assert_eq!(engine.chain_order(), &[b]);

        settle_crossfade(&mut engine);

        let left_in = vec![1.0; 256];
        let mut left_out = vec![0.0; 256];
        let mut right_out = vec![0.0; 256];

        engine.process_block_stereo(&left_in, &left_in, &mut left_out, &mut right_out);
        for &s in &left_out {
            assert!((s - 3.0).abs() < 0.02, "expected 3.0, got {s}");
        }
    }

    #[test]
    fn test_remove_last_effect() {
        let mut engine = GraphEngine::new_linear(48000.0, 256);
        let a = engine.add_effect(gain(2.0));

        engine.remove_effect(a);
        assert!(engine.is_empty());

        settle_crossfade(&mut engine);

        let left_in = vec![1.0; 256];
        let mut left_out = vec![0.0; 256];
        let mut right_out = vec![0.0; 256];
        engine.process_block_stereo(&left_in, &left_in, &mut left_out, &mut right_out);
        for &s in &left_out {
            assert!((s - 1.0).abs() < 0.02, "expected 1.0, got {s}");
        }
    }

    #[test]
    fn test_reorder() {
        let mut engine = GraphEngine::new_linear(48000.0, 256);
        let a = engine.add_effect(gain(2.0));
        let b = engine.add_effect(gain(0.5));

        engine.reorder(&[b, a]);
        assert_eq!(engine.chain_order(), &[b, a]);

        settle_crossfade(&mut engine);

        let left_in = vec![1.0; 256];
        let mut left_out = vec![0.0; 256];
        let mut right_out = vec![0.0; 256];
        engine.process_block_stereo(&left_in, &left_in, &mut left_out, &mut right_out);
        for &s in &left_out {
            assert!((s - 1.0).abs() < 0.02, "expected 1.0, got {s}");
        }
    }

    #[test]
    fn test_mono_process_block() {
        let mut engine = GraphEngine::from_chain(vec![gain(2.0)], 48000.0, 4).unwrap();

        let input = [1.0, 2.0, 3.0, 4.0];
        let mut output = [0.0; 4];

        engine.process_block(&input, &mut output);
        assert_eq!(output, [2.0, 4.0, 6.0, 8.0]);
    }

    #[test]
    fn test_mono_process_file() {
        let mut engine = GraphEngine::from_chain(vec![gain(0.5)], 48000.0, 64).unwrap();

        let input: Vec<f32> = (0..1000).map(|i| i as f32).collect();
        let output = engine.process_file(&input, 64);

        assert_eq!(output.len(), 1000);
        for (i, &s) in output.iter().enumerate() {
            assert!(
                (s - i as f32 * 0.5).abs() < 1e-6,
                "mismatch at {i}: expected {}, got {s}",
                i as f32 * 0.5
            );
        }
    }

    #[test]
    fn test_process_block_inplace() {
        let mut engine = GraphEngine::from_chain(vec![gain(2.0)], 48000.0, 4).unwrap();

        let mut buffer = [1.0, 2.0, 3.0, 4.0];
        engine.process_block_inplace(&mut buffer);
        assert_eq!(buffer, [2.0, 4.0, 6.0, 8.0]);
    }

    #[test]
    fn test_process_block_stereo_inplace() {
        let mut engine = GraphEngine::from_chain(vec![gain(2.0)], 48000.0, 4).unwrap();

        let mut left = [1.0, 2.0, 3.0, 4.0];
        let mut right = [0.5, 1.0, 1.5, 2.0];

        engine.process_block_stereo_inplace(&mut left, &mut right);
        assert_eq!(left, [2.0, 4.0, 6.0, 8.0]);
        assert_eq!(right, [1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn test_effect_with_params_access() {
        let mut engine = GraphEngine::new_linear(48000.0, 256);
        let id = engine.add_effect(gain(2.0));

        let ewp = engine.effect_with_params_ref(id).unwrap();
        assert_eq!(ewp.effect_param_count(), 1);

        assert!(engine.effect_with_params_mut(id).is_some());
    }

    // --- Slot-indexed API tests ---

    #[test]
    fn test_add_effect_named() {
        let mut engine = GraphEngine::new_linear(48000.0, 256);
        let slot = engine.add_effect_named(gain(2.0), "distortion");
        assert_eq!(slot, 0);
        assert_eq!(engine.effect_ids(), &["distortion"]);
        assert_eq!(engine.slot_count(), 1);

        let slot2 = engine.add_effect_named(gain(0.5), "reverb");
        assert_eq!(slot2, 1);
        assert_eq!(engine.effect_ids(), &["distortion", "reverb"]);
    }

    #[test]
    fn test_add_effect_anonymous_empty_id() {
        let mut engine = GraphEngine::new_linear(48000.0, 256);
        engine.add_effect(gain(2.0));
        assert_eq!(engine.effect_ids(), &[""]);
    }

    #[test]
    fn test_remove_at() {
        let mut engine = GraphEngine::new_linear(48000.0, 256);
        engine.add_effect_named(gain(2.0), "a");
        engine.add_effect_named(gain(3.0), "b");

        let removed = engine.remove_at(0);
        assert!(removed.is_some());
        assert_eq!(engine.slot_count(), 1);
        assert_eq!(engine.effect_ids(), &["b"]);
    }

    #[test]
    fn test_remove_at_out_of_bounds() {
        let mut engine = GraphEngine::new_linear(48000.0, 256);
        engine.add_effect_named(gain(2.0), "a");
        assert!(engine.remove_at(5).is_none());
        assert_eq!(engine.slot_count(), 1);
    }

    #[test]
    fn test_reorder_slots() {
        let mut engine = GraphEngine::new_linear(48000.0, 256);
        engine.add_effect_named(gain(2.0), "a");
        engine.add_effect_named(gain(3.0), "b");
        engine.add_effect_named(gain(4.0), "c");

        engine.reorder_slots(&[2, 0, 1]);
        assert_eq!(engine.effect_ids(), &["c", "a", "b"]);
    }

    #[test]
    fn test_clear() {
        let mut engine = GraphEngine::new_linear(48000.0, 4);
        engine.add_effect_named(gain(2.0), "a");
        engine.add_effect_named(gain(3.0), "b");
        engine.add_effect_named(gain(4.0), "c");

        engine.clear();
        assert!(engine.is_empty());
        assert_eq!(engine.slot_count(), 0);
        assert!(engine.effect_ids().is_empty());

        // Verify passthrough still works.
        let input = [1.0; 4];
        let mut left_out = [0.0; 4];
        let mut right_out = [0.0; 4];
        engine.process_block_stereo(&input, &input, &mut left_out, &mut right_out);
        assert_eq!(left_out, [1.0; 4]);
    }

    #[test]
    fn test_set_get_param_at() {
        let mut engine = GraphEngine::new_linear(48000.0, 256);
        engine.add_effect_named(gain(2.0), "g");

        // Read initial value.
        assert_eq!(engine.get_param_at(0, 0), Some(2.0));

        // Set and read back.
        assert!(engine.set_param_at(0, 0, 5.0));
        assert_eq!(engine.get_param_at(0, 0), Some(5.0));

        // Out of bounds.
        assert!(!engine.set_param_at(5, 0, 1.0));
        assert_eq!(engine.get_param_at(5, 0), None);
        assert!(!engine.set_param_at(0, 99, 1.0));
        assert_eq!(engine.get_param_at(0, 99), None);
    }

    #[test]
    fn test_param_count_at() {
        let mut engine = GraphEngine::new_linear(48000.0, 256);
        engine.add_effect_named(gain(2.0), "g");

        assert_eq!(engine.param_count_at(0), 1);
        assert_eq!(engine.param_count_at(99), 0);
    }

    #[test]
    fn test_param_descriptor_at() {
        let mut engine = GraphEngine::new_linear(48000.0, 256);
        engine.add_effect_named(gain(2.0), "g");

        let desc = engine.param_descriptor_at(0, 0).unwrap();
        assert_eq!(desc.name, "Factor");
        assert!(engine.param_descriptor_at(0, 99).is_none());
        assert!(engine.param_descriptor_at(99, 0).is_none());
    }

    #[test]
    fn test_bypass_at() {
        let mut engine = GraphEngine::new_linear(48000.0, 256);
        engine.add_effect_named(gain(2.0), "g");

        assert!(!engine.is_bypassed_at(0));
        engine.set_bypass_at(0, true);
        assert!(engine.is_bypassed_at(0));
        engine.set_bypass_at(0, false);
        assert!(!engine.is_bypassed_at(0));

        // Out of bounds is safe.
        assert!(!engine.is_bypassed_at(99));
    }

    #[test]
    fn test_snapshot() {
        let mut engine = GraphEngine::new_linear(48000.0, 256);
        engine.add_effect_named(gain(2.0), "distortion");
        engine.add_effect_named(gain(3.0), "reverb");
        engine.set_bypass_at(1, true);

        let snap = engine.snapshot();
        assert_eq!(snap.entries.len(), 2);

        assert_eq!(snap.entries[0].effect_id, "distortion");
        assert_eq!(snap.entries[0].params, vec![2.0]);
        assert!(!snap.entries[0].bypassed);

        assert_eq!(snap.entries[1].effect_id, "reverb");
        assert_eq!(snap.entries[1].params, vec![3.0]);
        assert!(snap.entries[1].bypassed);
    }

    #[test]
    fn test_effect_ids_across_remove() {
        let mut engine = GraphEngine::new_linear(48000.0, 256);
        engine.add_effect_named(gain(1.0), "a");
        engine.add_effect_named(gain(2.0), "b");
        engine.add_effect_named(gain(3.0), "c");

        engine.remove_at(1); // remove "b"
        assert_eq!(engine.effect_ids(), &["a", "c"]);
        assert_eq!(engine.slot_count(), 2);
    }

    #[test]
    fn test_slot_count_alias() {
        let mut engine = GraphEngine::new_linear(48000.0, 256);
        engine.add_effect(gain(1.0));
        engine.add_effect(gain(2.0));
        assert_eq!(engine.slot_count(), engine.effect_count());
    }

    #[test]
    fn test_effect_at_access() {
        let mut engine = GraphEngine::new_linear(48000.0, 256);
        engine.add_effect_named(gain(2.0), "g");

        assert!(engine.effect_at(0).is_some());
        assert!(engine.effect_at(99).is_none());

        let ewp = engine.effect_at_mut(0).unwrap();
        ewp.effect_set_param(0, 7.0);
        assert_eq!(engine.get_param_at(0, 0), Some(7.0));
    }
}
