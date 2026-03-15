//! Compositional DSP algebra — combinators for building [`ProcessingGraph`] topologies.
//!
//! This module provides a fluent builder API that lets you express effect
//! graphs as Rust expressions rather than sequences of `add_*` / `connect`
//! calls.  Each combinator returns a [`GraphBuilder`] that records the intent;
//! calling [`GraphBuilder::build`] materialises the topology into a
//! [`ProcessingGraph`] and compiles it ready for audio processing.
//!
//! # Overview
//!
//! | Combinator | Topology produced |
//! |------------|------------------|
//! | [`seq`] | A → B (serial chain) |
//! | [`par`] | A and B in parallel, summed at a Merge node |
//! | [`feedback`] | A with its output fed back (1-block delay) at gain `g` |
//!
//! # Example
//!
//! ```rust,ignore
//! use sonido_core::compose::{seq, par, feedback};
//! use sonido_core::ProcessingGraph;
//!
//! // dist → reverb (serial)
//! let graph = seq(dist, reverb).build(48000.0, 256).unwrap();
//!
//! // (dist | reverb) mixed in parallel
//! let graph = par(dist, reverb).build(48000.0, 256).unwrap();
//!
//! // dist with feedback at 0.5
//! let graph = feedback(dist, 0.5).build(48000.0, 256).unwrap();
//! ```
//!
//! # no_std
//!
//! This module is no_std-compatible and relies on `alloc` for `Vec` and `Box`.

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, vec, vec::Vec};

use crate::effect_with_params::EffectWithParams;
use crate::graph::node::NodeId;
use crate::graph::{GraphError, ProcessingGraph};

// ─── Leaf type ───────────────────────────────────────────────────────────────

/// A single boxed effect, the atomic unit of DSP algebra.
pub struct EffectNode(Box<dyn EffectWithParams + Send>);

impl EffectNode {
    /// Wrap a concrete effect in a leaf node.
    pub fn new(effect: impl EffectWithParams + Send + 'static) -> Self {
        Self(Box::new(effect))
    }

    /// Wrap an already-boxed effect.
    pub fn boxed(effect: Box<dyn EffectWithParams + Send>) -> Self {
        Self(effect)
    }
}

// ─── Builder nodes ───────────────────────────────────────────────────────────

/// A node in the builder tree — either a leaf effect or a composed sub-graph.
///
/// This enum is the recursive representation of the DSP algebra before it is
/// lowered into a [`ProcessingGraph`].
pub enum GraphBuilder {
    /// Leaf: a single effect.
    Leaf(Box<dyn EffectWithParams + Send>),
    /// Serial chain: `left` feeds `right`.
    Serial {
        /// First (upstream) sub-graph.
        left: Box<GraphBuilder>,
        /// Second (downstream) sub-graph.
        right: Box<GraphBuilder>,
    },
    /// Parallel split-merge: `left` and `right` process the same input
    /// independently and their outputs are averaged at a Merge node.
    Parallel {
        /// First parallel branch.
        left: Box<GraphBuilder>,
        /// Second parallel branch.
        right: Box<GraphBuilder>,
    },
    /// Feedback loop: the effect's output is mixed back to its input with
    /// the given gain via a 1-block (`block_size` samples) compensation delay.
    Feedback {
        /// The effect that sits inside the feedback loop.
        inner: Box<GraphBuilder>,
        /// Feedback gain (0.0 = no feedback, 1.0 = self-oscillation).
        ///
        /// Range: 0.0 – < 1.0 for stable operation.
        gain: f32,
    },
}

impl GraphBuilder {
    // ── Combinators (method form) ────────────────────────────────────────────

    /// Chain `self` → `next` in series.
    ///
    /// The output of `self` feeds the input of `next`.
    pub fn seq(self, next: impl Into<GraphBuilder>) -> GraphBuilder {
        GraphBuilder::Serial {
            left: Box::new(self),
            right: Box::new(next.into()),
        }
    }

    /// Place `self` and `other` in parallel and mix their outputs.
    ///
    /// Both branches receive the same input.  Outputs are averaged (each
    /// multiplied by 0.5) at the Merge node, so unity-gain signals remain
    /// unity-gain.
    pub fn par(self, other: impl Into<GraphBuilder>) -> GraphBuilder {
        GraphBuilder::Parallel {
            left: Box::new(self),
            right: Box::new(other.into()),
        }
    }

    /// Wrap `self` in a feedback loop with the given `gain`.
    ///
    /// The output is fed back to the input via a 1-block delay (inserted
    /// automatically during graph compilation).  `gain = 0.5` means half of
    /// the output is re-injected.  Keep `gain < 1.0` to avoid instability.
    pub fn feedback(self, gain: f32) -> GraphBuilder {
        GraphBuilder::Feedback {
            inner: Box::new(self),
            gain,
        }
    }

    // ── Materialisation ──────────────────────────────────────────────────────

    /// Compile the builder tree into a [`ProcessingGraph`].
    ///
    /// Inserts Input and Output nodes automatically and calls
    /// [`ProcessingGraph::compile`].
    ///
    /// # Errors
    ///
    /// Propagates any [`GraphError`] from the underlying graph operations.
    pub fn build(self, sample_rate: f32, block_size: usize) -> Result<ProcessingGraph, GraphError> {
        let mut graph = ProcessingGraph::new(sample_rate, block_size);
        let input = graph.add_input();
        let output = graph.add_output();

        let (entry, exit) = lower_builder(&mut graph, self)?;

        graph.connect(input, entry)?;
        graph.connect(exit, output)?;
        graph.compile()?;
        Ok(graph)
    }
}

// ─── Free-function constructors ──────────────────────────────────────────────

/// Build a serial chain: `left` → `right`.
///
/// # Example
///
/// ```rust,ignore
/// let graph = seq(distortion, reverb).build(48000.0, 256).unwrap();
/// ```
pub fn seq(left: impl Into<GraphBuilder>, right: impl Into<GraphBuilder>) -> GraphBuilder {
    GraphBuilder::Serial {
        left: Box::new(left.into()),
        right: Box::new(right.into()),
    }
}

/// Build a parallel split-merge: `left` ‖ `right`.
///
/// Both receive the same input; outputs are averaged at a Merge node.
///
/// # Example
///
/// ```rust,ignore
/// let graph = par(clean_path, effect_path).build(48000.0, 256).unwrap();
/// ```
pub fn par(left: impl Into<GraphBuilder>, right: impl Into<GraphBuilder>) -> GraphBuilder {
    GraphBuilder::Parallel {
        left: Box::new(left.into()),
        right: Box::new(right.into()),
    }
}

/// Wrap `inner` in a feedback loop with the given `gain`.
///
/// Uses a 1-block delay to make the loop causal.  Keep `gain < 1.0` for
/// stable operation; `gain >= 1.0` produces self-oscillation.
///
/// # Arguments
///
/// * `inner` — The effect (or sub-graph) inside the loop.
/// * `gain` — Feedback amount. Valid range: 0.0 – < 1.0.
///
/// # Example
///
/// ```rust,ignore
/// // Karplus-Strong: delay line with feedback
/// let graph = feedback(comb_filter, 0.99).build(48000.0, 256).unwrap();
/// ```
pub fn feedback(inner: impl Into<GraphBuilder>, gain: f32) -> GraphBuilder {
    GraphBuilder::Feedback {
        inner: Box::new(inner.into()),
        gain,
    }
}

// ─── From conversions ────────────────────────────────────────────────────────

impl From<EffectNode> for GraphBuilder {
    fn from(node: EffectNode) -> Self {
        GraphBuilder::Leaf(node.0)
    }
}

impl From<Box<dyn EffectWithParams + Send>> for GraphBuilder {
    fn from(effect: Box<dyn EffectWithParams + Send>) -> Self {
        GraphBuilder::Leaf(effect)
    }
}

// ─── Builder lowering ────────────────────────────────────────────────────────

/// Recursively lower a [`GraphBuilder`] tree into `graph`, returning the
/// `(entry_node, exit_node)` pair for the sub-graph so callers can connect it
/// to surrounding topology.
fn lower_builder(
    graph: &mut ProcessingGraph,
    builder: GraphBuilder,
) -> Result<(NodeId, NodeId), GraphError> {
    match builder {
        GraphBuilder::Leaf(effect) => {
            let node = graph.add_effect(effect);
            Ok((node, node))
        }

        GraphBuilder::Serial { left, right } => {
            let (l_entry, l_exit) = lower_builder(graph, *left)?;
            let (r_entry, r_exit) = lower_builder(graph, *right)?;
            graph.connect(l_exit, r_entry)?;
            Ok((l_entry, r_exit))
        }

        GraphBuilder::Parallel { left, right } => {
            let split = graph.add_split();
            let merge = graph.add_merge();

            let (l_entry, l_exit) = lower_builder(graph, *left)?;
            let (r_entry, r_exit) = lower_builder(graph, *right)?;

            graph.connect(split, l_entry)?;
            graph.connect(split, r_entry)?;
            graph.connect(l_exit, merge)?;
            graph.connect(r_exit, merge)?;

            Ok((split, merge))
        }

        GraphBuilder::Feedback { inner, gain } => {
            // Feedback topology:
            //
            //   entry ──[inner]──► exit
            //              ▲          │
            //              └──[gain]──┘  (1-block delay, inserted by `connect_feedback`)
            //
            // We model this as:
            //   1. The main signal path: input → (merge_in) → inner → (split_out)
            //   2. A feedback arm:       split_out → gain_effect → merge_in
            //
            // The merge_in Merge node sums the forward signal and the feedback.
            // The connect_feedback API marks this edge as a feedback edge so the
            // scheduler inserts a 1-block compensation delay automatically.

            let merge_in = graph.add_merge();
            let (inner_entry, inner_exit) = lower_builder(graph, *inner)?;
            let gain_node = graph.add_effect(Box::new(GainEffect::new(gain)));
            let split_out = graph.add_split();

            // Forward path: merge → inner → split
            graph.connect(merge_in, inner_entry)?;
            graph.connect(inner_exit, split_out)?;

            // Feedback arm: split → gain → merge (feedback edge)
            graph.connect(split_out, gain_node)?;
            graph.connect_feedback(gain_node, merge_in)?;

            Ok((merge_in, split_out))
        }
    }
}

// ─── Internal gain effect for feedback paths ─────────────────────────────────

/// Minimal passthrough gain effect used in feedback paths.
///
/// Multiplies both channels by a fixed `gain` coefficient each sample.
struct GainEffect {
    gain: f32,
}

impl GainEffect {
    fn new(gain: f32) -> Self {
        Self { gain }
    }
}

impl crate::effect::Effect for GainEffect {
    fn process(&mut self, input: f32) -> f32 {
        input * self.gain
    }

    fn set_sample_rate(&mut self, _sample_rate: f32) {}

    fn reset(&mut self) {}
}

impl crate::param_info::ParameterInfo for GainEffect {
    fn param_count(&self) -> usize {
        0
    }

    fn param_info(&self, _index: usize) -> Option<crate::param_info::ParamDescriptor> {
        None
    }

    fn get_param(&self, _index: usize) -> f32 {
        0.0
    }

    fn set_param(&mut self, _index: usize, _value: f32) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::Effect;
    use crate::param_info::{ParamDescriptor, ParameterInfo};

    struct PassThrough;

    impl Effect for PassThrough {
        fn process(&mut self, input: f32) -> f32 {
            input
        }
        fn set_sample_rate(&mut self, _: f32) {}
        fn reset(&mut self) {}
    }

    impl ParameterInfo for PassThrough {
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

    fn passthrough() -> GraphBuilder {
        GraphBuilder::Leaf(Box::new(PassThrough))
    }

    #[test]
    fn test_seq_builds_linear_graph() {
        let graph = seq(passthrough(), passthrough())
            .build(48000.0, 64)
            .unwrap();
        // Input, 2 effects, Output
        assert_eq!(graph.node_count(), 4);
    }

    #[test]
    fn test_par_builds_split_merge_graph() {
        let graph = par(passthrough(), passthrough())
            .build(48000.0, 64)
            .unwrap();
        // Input, Split, 2 effects, Merge, Output
        assert_eq!(graph.node_count(), 6);
    }

    #[test]
    fn test_seq_processes_audio() {
        let mut graph = seq(passthrough(), passthrough())
            .build(48000.0, 64)
            .unwrap();
        let left_in = [0.5f32; 64];
        let right_in = [0.25f32; 64];
        let mut left_out = [0.0f32; 64];
        let mut right_out = [0.0f32; 64];
        graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);
        assert!((left_out[0] - 0.5).abs() < 1e-6);
        assert!((right_out[0] - 0.25).abs() < 1e-6);
    }

    #[test]
    fn test_par_processes_audio_unity() {
        let mut graph = par(passthrough(), passthrough())
            .build(48000.0, 64)
            .unwrap();
        let left_in = [0.5f32; 64];
        let right_in = [0.25f32; 64];
        let mut left_out = [0.0f32; 64];
        let mut right_out = [0.0f32; 64];
        graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);
        // Two identical passthrough paths averaged → 0.5 * (input + input) = input
        assert!((left_out[0] - 0.5).abs() < 1e-4);
        assert!((right_out[0] - 0.25).abs() < 1e-4);
    }

    #[test]
    fn test_method_syntax_seq() {
        let graph = passthrough()
            .seq(passthrough())
            .seq(passthrough())
            .build(48000.0, 64)
            .unwrap();
        // Input, 3 effects, Output
        assert_eq!(graph.node_count(), 5);
    }

    #[test]
    fn test_builder_from_boxed_effect() {
        let boxed: Box<dyn EffectWithParams + Send> = Box::new(PassThrough);
        let _graph: GraphBuilder = boxed.into();
    }

    #[test]
    fn test_feedback_builds_and_compiles() {
        // feedback wraps a passthrough in a loop. Should compile without error.
        let graph = feedback(passthrough(), 0.5).build(48000.0, 64).unwrap();
        // Feedback topology: Input, Merge, PassThrough, Split, GainEffect, Output = 6 nodes
        assert_eq!(graph.node_count(), 6);
    }

    #[test]
    fn test_feedback_processes_audio() {
        let mut graph = feedback(passthrough(), 0.5).build(48000.0, 64).unwrap();
        let left_in = [0.5f32; 64];
        let right_in = [0.25f32; 64];
        let mut left_out = [0.0f32; 64];
        let mut right_out = [0.0f32; 64];
        // First block: feedback path is zeroed (FeedbackDelay outputs silence first block).
        graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);
        // Output should be non-zero (signal passes through).
        assert!(left_out.iter().any(|&s| s != 0.0));
    }
}
