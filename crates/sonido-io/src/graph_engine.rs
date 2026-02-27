//! Graph-based audio processing engine.
//!
//! [`GraphEngine`] wraps [`ProcessingGraph`] to provide a higher-level API for
//! audio processing with DAG-based routing. It replaces `ProcessingEngine` for
//! linear chains and supports arbitrary DAG topologies (parallel paths,
//! sidechains, multiband).
//!
//! # Linear Chain Management
//!
//! For linear chains (the common case), `GraphEngine` maintains an ordered
//! `chain_order` list of effect [`NodeId`]s. Use [`add_effect()`](GraphEngine::add_effect),
//! [`remove_effect()`](GraphEngine::remove_effect), and
//! [`reorder()`](GraphEngine::reorder) to mutate the chain.
//!
//! ```rust,ignore
//! use sonido_io::GraphEngine;
//! use sonido_effects::{Distortion, Reverb};
//!
//! let mut engine = GraphEngine::new_linear(48000.0, 256);
//! let dist = engine.add_effect(Box::new(Distortion::new(48000.0)));
//! let reverb = engine.add_effect(Box::new(Reverb::new(48000.0)));
//!
//! engine.process_block_stereo(&left_in, &right_in, &mut left_out, &mut right_out);
//! ```

use crate::StereoSamples;
use sonido_core::EffectWithParams;
use sonido_core::graph::{GraphError, NodeId, ProcessingGraph};
use sonido_core::tempo::TempoContext;

/// Graph-based processing engine for DAG audio routing.
///
/// Wraps [`ProcessingGraph`] with a convenient API for common operations.
/// For linear chains, maintains an ordered list of effect node IDs enabling
/// add/remove/reorder operations with automatic graph recompilation.
pub struct GraphEngine {
    graph: ProcessingGraph,
    /// Ordered list of effect `NodeId`s in the linear chain.
    chain_order: Vec<NodeId>,
    /// The Input node (always present).
    input_node: NodeId,
    /// The Output node (always present).
    output_node: NodeId,
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
            input_node,
            output_node,
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
        Self {
            graph,
            chain_order: Vec::new(),
            input_node: NodeId::sentinel(),
            output_node: NodeId::sentinel(),
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

        let mut chain_order = Vec::with_capacity(effects.len());
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
            input_node,
            output_node,
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

        self.graph.compile().unwrap();
        node
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

        // Reconnect pred → succ.
        self.graph.connect(pred, succ).unwrap();
        self.graph.compile().unwrap();

        Some(effect)
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

        self.chain_order = order.to_vec();
        self.graph.compile().unwrap();
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

    // --- Parameter / effect access ---

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
        let mut right_out = vec![0.0; len];
        self.graph
            .process_block(input, input, &mut output[..len], &mut right_out);
    }

    /// Processes a block of mono audio in-place.
    pub fn process_block_inplace(&mut self, buffer: &mut [f32]) {
        let len = buffer.len();
        let input_copy: Vec<f32> = buffer.to_vec();
        let mut right_out = vec![0.0; len];
        self.graph
            .process_block(&input_copy, &input_copy, buffer, &mut right_out);
    }

    /// Processes a block of stereo audio in-place.
    pub fn process_block_stereo_inplace(&mut self, left: &mut [f32], right: &mut [f32]) {
        let len = left.len();
        let left_in: Vec<f32> = left.to_vec();
        let right_in: Vec<f32> = right.to_vec();
        self.graph
            .process_block(&left_in, &right_in, &mut left[..len], &mut right[..len]);
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
        // Find the edge ID by scanning the graph's edges.
        // We need to find edges from `from` that go to `to`.
        if let Some(edge_id) = self.graph.find_edge(from, to) {
            self.graph.disconnect(edge_id).unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_core::Effect;
    use sonido_core::param_info::{ParamDescriptor, ParameterInfo};

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
            0
        }
        fn param_info(&self, _index: usize) -> Option<ParamDescriptor> {
            None
        }
        fn get_param(&self, _index: usize) -> f32 {
            0.0
        }
        fn set_param(&mut self, _index: usize, _value: f32) {}
    }

    fn gain(factor: f32) -> Box<dyn EffectWithParams + Send> {
        Box::new(Gain { factor })
    }

    /// Process enough blocks to let the crossfade settle.
    /// SmoothedParam::fast = 5ms time constant. Need ~5τ = 25ms = 1200 samples.
    fn settle_crossfade(engine: &mut GraphEngine) {
        let bs = engine.graph().block_size();
        let blocks_needed = (1200 / bs) + 1;
        let mut left = vec![0.0; bs];
        let mut right = vec![0.0; bs];
        let input = vec![0.0; bs];
        for _ in 0..blocks_needed {
            engine.process_block_stereo(&input, &input, &mut left, &mut right);
        }
    }

    // --- Existing tests (updated) ---

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

    // --- New tests for chain management ---

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

        // Settle the crossfade from recompilation.
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
        // 1.0 * 2.0 * 3.0 = 6.0
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

        // Only gain(3.0) remains.
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

        // Should be passthrough.
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

        // Reorder from [a, b] to [b, a].
        engine.reorder(&[b, a]);
        assert_eq!(engine.chain_order(), &[b, a]);

        settle_crossfade(&mut engine);

        // Both orderings yield 1.0 (2.0 * 0.5 commutes), but chain_order is updated.
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

        // Access via EffectWithParams.
        let ewp = engine.effect_with_params_ref(id).unwrap();
        assert_eq!(ewp.effect_param_count(), 0);

        // Mutable access.
        assert!(engine.effect_with_params_mut(id).is_some());
    }
}
