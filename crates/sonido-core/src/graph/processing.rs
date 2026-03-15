//! Processing graph — mutation API, cycle detection, and audio execution.
//!
//! [`ProcessingGraph`] is the main entry point for the DAG routing engine. It owns
//! the graph topology (nodes and edges), provides mutation methods (add, remove,
//! connect, disconnect), compiles the graph into a [`CompiledSchedule`], and
//! executes that schedule per audio block.
//!
//! The graph is mutated on the main/GUI thread and compiled into an immutable
//! snapshot that the audio thread executes. Schedule swaps use a ~5ms crossfade
//! of cached output for click-free transitions — no double-execution of effects.

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, format, string::String, sync::Arc, vec, vec::Vec};
#[cfg(feature = "std")]
use std::sync::Arc;

use crate::effect::Effect;
use crate::effect_with_params::EffectWithParams;
use crate::math::wet_dry_mix;
use crate::param::SmoothedParam;
use crate::tempo::TempoContext;

use super::buffer::{BufferPool, CompensationDelay};
use super::edge::{Edge, EdgeId};
use super::node::{NodeData, NodeId, NodeKind, NodeRate};
use super::schedule::{CompiledSchedule, MAX_SPLIT_TARGETS, ProcessStep};

#[cfg(all(debug_assertions, feature = "debug-alloc"))]
use assert_no_alloc::assert_no_alloc;

/// An effect removed from the graph that's still producing decaying output.
///
/// Fed silence each block; output mixed into the final output with exponential
/// fade-out. Dropped when remaining samples reach zero or fade reaches silence.
struct SpilloverTail {
    /// The removed effect, still processing its tail (reverb decay, delay feedback).
    effect: Box<dyn EffectWithParams + Send>,
    /// Exponential fade-out envelope (starts at 1.0, targets 0.0).
    fade: SmoothedParam,
    /// Samples remaining in the tail.
    remaining: usize,
    /// Scratch left buffer for processing (sized to block_size).
    left_buf: Vec<f32>,
    /// Scratch right buffer for processing (sized to block_size).
    right_buf: Vec<f32>,
}

/// Formats a compiled `ProcessStep` into a human-readable description.
#[cfg(feature = "tracing")]
fn format_step(step: &ProcessStep) -> String {
    match step {
        ProcessStep::WriteInput { buffer_idx } => {
            format!("WriteInput → buf[{buffer_idx}]")
        }
        ProcessStep::ProcessEffect {
            node_idx,
            input_buf,
            output_buf,
            sidechain_buf,
            ..
        } => {
            if let Some(sc) = sidechain_buf {
                format!(
                    "ProcessEffect node[{node_idx}] buf[{input_buf}] → buf[{output_buf}] SC=buf[{sc}]"
                )
            } else {
                format!("ProcessEffect node[{node_idx}] buf[{input_buf}] → buf[{output_buf}]")
            }
        }
        ProcessStep::SplitCopy {
            source_buf,
            dest_bufs,
            dest_count,
        } => {
            let dests: Vec<String> = dest_bufs[..*dest_count]
                .iter()
                .map(|b| format!("buf[{b}]"))
                .collect();
            format!("SplitCopy buf[{source_buf}] → [{}]", dests.join(", "))
        }
        ProcessStep::ClearBuffer { buffer_idx } => {
            format!("ClearBuffer buf[{buffer_idx}]")
        }
        ProcessStep::AccumulateBuffer {
            source_buf,
            dest_buf,
            gain,
        } => {
            format!("AccumulateBuffer buf[{source_buf}] → buf[{dest_buf}] (gain={gain:.2})")
        }
        ProcessStep::DelayCompensate {
            buffer_idx,
            delay_line_idx,
        } => {
            format!("DelayCompensate buf[{buffer_idx}] delay_line[{delay_line_idx}]")
        }
        ProcessStep::ReadOutput { buffer_idx } => {
            format!("ReadOutput ← buf[{buffer_idx}]")
        }
        ProcessStep::FeedbackDelay {
            buffer_idx,
            delay_line_idx,
        } => {
            format!("FeedbackDelay buf[{buffer_idx}] delay_line[{delay_line_idx}]")
        }
        ProcessStep::SampleAndHold {
            buffer_idx,
            block_len,
        } => {
            format!("SampleAndHold buf[{buffer_idx}] len={block_len}")
        }
        ProcessStep::ProcessSubGraph {
            node_idx,
            input_buf,
            output_buf,
        } => {
            format!("ProcessSubGraph node[{node_idx}] buf[{input_buf}] → buf[{output_buf}]")
        }
    }
}

/// Errors that can occur during graph operations.
#[derive(Debug)]
pub enum GraphError {
    /// The specified node was not found in the graph.
    NodeNotFound(NodeId),
    /// The specified edge was not found in the graph.
    EdgeNotFound(EdgeId),
    /// Adding this edge would create a cycle.
    CycleDetected,
    /// The graph must have exactly one Input node.
    InvalidInputCount(usize),
    /// The graph must have exactly one Output node.
    InvalidOutputCount(usize),
    /// A node has an invalid connection (e.g., Input with incoming edges).
    InvalidConnection(String),
    /// The graph is empty or has no processable nodes.
    EmptyGraph,
    /// A duplicate edge already exists between these nodes.
    DuplicateEdge(NodeId, NodeId),
}

#[cfg(feature = "std")]
impl std::fmt::Display for GraphError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NodeNotFound(id) => write!(f, "node {id:?} not found"),
            Self::EdgeNotFound(id) => write!(f, "edge {id:?} not found"),
            Self::CycleDetected => write!(f, "adding this edge would create a cycle"),
            Self::InvalidInputCount(n) => write!(f, "expected 1 Input node, found {n}"),
            Self::InvalidOutputCount(n) => write!(f, "expected 1 Output node, found {n}"),
            Self::InvalidConnection(msg) => write!(f, "invalid connection: {msg}"),
            Self::EmptyGraph => write!(f, "graph has no processable nodes"),
            Self::DuplicateEdge(a, b) => write!(f, "edge from {a:?} to {b:?} already exists"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for GraphError {}

impl core::fmt::Display for NodeId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "NodeId({})", self.0)
    }
}

impl core::fmt::Display for EdgeId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "EdgeId({})", self.0)
    }
}

/// Directed acyclic graph (DAG) for audio effect routing.
///
/// The graph holds nodes (effects, splits, merges, I/O) and edges (audio
/// connections). Mutations happen on the main thread; compilation produces an
/// immutable [`CompiledSchedule`] that the audio thread executes.
///
/// # Usage
///
/// 1. Create a graph with [`new()`](Self::new)
/// 2. Add nodes: [`add_input()`](Self::add_input), [`add_output()`](Self::add_output),
///    [`add_effect()`](Self::add_effect), [`add_split()`](Self::add_split),
///    [`add_merge()`](Self::add_merge)
/// 3. Connect nodes: [`connect()`](Self::connect)
/// 4. Compile: [`compile()`](Self::compile)
/// 5. Process: [`process_block()`](Self::process_block)
pub struct ProcessingGraph {
    nodes: Vec<Option<NodeData>>,
    edges: Vec<Option<Edge>>,
    compiled: Option<Arc<CompiledSchedule>>,
    sample_rate: f32,
    block_size: usize,
    next_node_slot: u32,
    next_edge_slot: u32,
    /// Crossfade envelope for click-free schedule swaps.
    swap_fade: SmoothedParam,
    /// Previous schedule, kept alive during crossfade.
    prev_compiled: Option<Arc<CompiledSchedule>>,
    /// Pre-allocated crossfade buffers for RT-safe schedule transitions.
    /// Cache the last output before a schedule swap; blended toward new output.
    crossfade_left: Vec<f32>,
    crossfade_right: Vec<f32>,
    /// Pre-allocated sidechain copy buffers (RT-safe, zero allocation per block).
    ///
    /// When a `ProcessEffect` step has a `sidechain_buf`, the executor copies the
    /// SC content here before processing so that aliasing across pool slots is safe.
    sc_tmp_left: Vec<f32>,
    sc_tmp_right: Vec<f32>,
    /// Pre-allocated audio buffer pool. Sized at compile(), reused every block.
    audio_pool: BufferPool,
    /// Persistent compensation delay lines. Rebuilt at compile(), state persists across blocks.
    audio_delay_lines: Vec<CompensationDelay>,
    /// Persistent feedback delay lines (1-block each). Rebuilt at compile().
    feedback_delay_lines: Vec<CompensationDelay>,
    /// Active spillover tails from removed effects.
    spillover_tails: Vec<SpilloverTail>,
    /// Whether spillover is enabled (default: true).
    spillover_enabled: bool,
}

impl ProcessingGraph {
    /// Creates a new empty processing graph.
    ///
    /// # Arguments
    ///
    /// * `sample_rate` - Sample rate in Hz (e.g., 48000.0)
    /// * `block_size` - Number of samples per processing block (e.g., 256)
    pub fn new(sample_rate: f32, block_size: usize) -> Self {
        let mut swap_fade = SmoothedParam::fast(1.0, sample_rate);
        swap_fade.snap_to_target();
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            compiled: None,
            sample_rate,
            block_size,
            next_node_slot: 0,
            next_edge_slot: 0,
            swap_fade,
            prev_compiled: None,
            crossfade_left: vec![0.0; block_size],
            crossfade_right: vec![0.0; block_size],
            sc_tmp_left: vec![0.0; block_size],
            sc_tmp_right: vec![0.0; block_size],
            audio_pool: BufferPool::new(0, block_size),
            audio_delay_lines: Vec::new(),
            feedback_delay_lines: Vec::new(),
            spillover_tails: Vec::new(),
            spillover_enabled: true,
        }
    }

    // --- Node mutations ---

    /// Adds an audio input node. Returns the new node's ID.
    ///
    /// A graph must have exactly one Input node to compile successfully.
    pub fn add_input(&mut self) -> NodeId {
        self.add_node(NodeKind::Input)
    }

    /// Adds an audio output node. Returns the new node's ID.
    ///
    /// A graph must have exactly one Output node to compile successfully.
    pub fn add_output(&mut self) -> NodeId {
        self.add_node(NodeKind::Output)
    }

    /// Adds an effect processing node wrapping the given [`EffectWithParams`].
    ///
    /// The effect's sample rate is set to the graph's sample rate.
    pub fn add_effect(&mut self, mut effect: Box<dyn EffectWithParams + Send>) -> NodeId {
        effect.set_sample_rate(self.sample_rate);
        let id = self.add_node(NodeKind::Effect(effect));
        // Pre-allocate bypass buffer for this node.
        if let Some(Some(node)) = self.nodes.get_mut(id.0 as usize) {
            node.bypass_buf.resize(self.block_size);
        }
        #[cfg(feature = "tracing")]
        tracing::debug!("graph_add: effect node {id}");
        id
    }

    /// Adds a split (fan-out) node. Returns the new node's ID.
    ///
    /// A Split node copies its single input to all connected outputs.
    pub fn add_split(&mut self) -> NodeId {
        let id = self.add_node(NodeKind::Split);
        #[cfg(feature = "tracing")]
        tracing::debug!("graph_add: split node {id}");
        id
    }

    /// Adds a merge (fan-in) node. Returns the new node's ID.
    ///
    /// A Merge node sums all connected inputs into a single output.
    pub fn add_merge(&mut self) -> NodeId {
        let id = self.add_node(NodeKind::Merge);
        #[cfg(feature = "tracing")]
        tracing::debug!("graph_add: merge node {id}");
        id
    }

    /// Removes a node and all its connected edges.
    ///
    /// Returns an error if the node doesn't exist.
    pub fn remove_node(&mut self, id: NodeId) -> Result<(), GraphError> {
        let idx = id.0 as usize;
        let node = self
            .nodes
            .get(idx)
            .and_then(|n| n.as_ref())
            .ok_or(GraphError::NodeNotFound(id))?;

        // Collect edge IDs to remove (avoid borrow conflict).
        let edge_ids: Vec<EdgeId> = node
            .incoming
            .iter()
            .chain(node.outgoing.iter())
            .copied()
            .collect();

        for edge_id in edge_ids {
            self.disconnect_internal(edge_id);
        }

        // Take the node out of the slot.
        if let Some(node_data) = self.nodes[idx].take() {
            // If this is an Effect node with a non-zero tail and spillover is enabled,
            // preserve the effect so it can ring out instead of cutting abruptly.
            if let NodeKind::Effect(effect) = node_data.kind {
                let tail = effect.tail_samples();
                if tail > 0 && self.spillover_enabled {
                    let fade_ms = (tail as f32 / self.sample_rate * 1000.0).min(10000.0);
                    let mut fade = SmoothedParam::with_config(1.0, self.sample_rate, fade_ms);
                    fade.set_target(0.0);
                    let buf_size = self.block_size.max(1);
                    self.spillover_tails.push(SpilloverTail {
                        effect,
                        fade,
                        remaining: tail,
                        left_buf: vec![0.0; buf_size],
                        right_buf: vec![0.0; buf_size],
                    });
                }
                // If no tail or spillover disabled, effect is dropped here.
            }
            // Non-effect nodes (Input, Output, Split, Merge) are dropped normally.
        }
        #[cfg(feature = "tracing")]
        tracing::debug!("graph_remove: node {id}");
        Ok(())
    }

    /// Connects two nodes with a directed edge.
    ///
    /// Returns the new edge's ID, or an error if:
    /// - Either node doesn't exist
    /// - The edge would create a cycle
    /// - A duplicate edge already exists
    /// - The connection is structurally invalid (e.g., edge into Input)
    pub fn connect(&mut self, from: NodeId, to: NodeId) -> Result<EdgeId, GraphError> {
        // Validate both nodes exist.
        self.get_node(from)?;
        self.get_node(to)?;

        // Validate structural constraints.
        self.validate_connection(from, to)?;

        // Check for duplicate edges.
        if self.has_edge(from, to) {
            return Err(GraphError::DuplicateEdge(from, to));
        }

        // Cycle detection: would adding from→to create a cycle?
        // A cycle exists if `to` can already reach `from` via existing edges.
        if self.can_reach(to, from) {
            return Err(GraphError::CycleDetected);
        }

        let edge_id = EdgeId(self.next_edge_slot);
        self.next_edge_slot += 1;

        let edge = Edge {
            from,
            to,
            is_feedback: false,
        };

        let edge_idx = edge_id.0 as usize;
        if edge_idx >= self.edges.len() {
            self.edges.resize_with(edge_idx + 1, || None);
        }
        self.edges[edge_idx] = Some(edge);

        // Update adjacency lists.
        self.nodes[from.0 as usize]
            .as_mut()
            .unwrap()
            .outgoing
            .push(edge_id);
        self.nodes[to.0 as usize]
            .as_mut()
            .unwrap()
            .incoming
            .push(edge_id);

        #[cfg(feature = "tracing")]
        tracing::debug!("graph_connect: {from} → {to}");
        Ok(edge_id)
    }

    /// Disconnects an edge.
    ///
    /// Returns an error if the edge doesn't exist.
    pub fn disconnect(&mut self, id: EdgeId) -> Result<(), GraphError> {
        if self
            .edges
            .get(id.0 as usize)
            .and_then(|e| e.as_ref())
            .is_none()
        {
            return Err(GraphError::EdgeNotFound(id));
        }
        self.disconnect_internal(id);
        #[cfg(feature = "tracing")]
        tracing::debug!("graph_disconnect: edge {id}");
        Ok(())
    }

    /// Connects a sidechain source node to a sidechain-capable effect node.
    ///
    /// The sidechain source node's output buffer will be routed to `to` as an
    /// external detection signal on the next compiled schedule. The `from` node
    /// must have a regular audio path through the graph (i.e., it must be an
    /// ancestor of `to` in the topological order) so that its output is available
    /// when `to` is processed.
    ///
    /// Effects that support sidechain detection (compressor, gate, de-esser)
    /// override [`DspKernel::process_stereo_with_sidechain`] to use the external
    /// signal. All other effects ignore it silently.
    ///
    /// After calling this method, recompile the graph with [`compile()`](Self::compile).
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::NodeNotFound`] if either node does not exist.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Route a kick-drum channel (split_kick) as sidechain to the bass compressor.
    /// graph.connect_sidechain(split_kick, compressor_node)?;
    /// graph.compile()?;
    /// ```
    pub fn connect_sidechain(&mut self, from: NodeId, to: NodeId) -> Result<(), GraphError> {
        self.get_node(from)?;
        self.get_node(to)?;
        if let Some(Some(node)) = self.nodes.get_mut(to.0 as usize) {
            node.sidechain_source = Some(from);
        }
        #[cfg(feature = "tracing")]
        tracing::debug!("graph_sidechain: {from} ─SC─► {to}");
        Ok(())
    }

    /// Removes the sidechain connection from a node, if any.
    ///
    /// After calling this method, recompile the graph with [`compile()`](Self::compile).
    ///
    /// Returns `true` if a sidechain connection existed and was removed.
    pub fn disconnect_sidechain(&mut self, id: NodeId) -> bool {
        if let Some(Some(node)) = self.nodes.get_mut(id.0 as usize)
            && node.sidechain_source.is_some()
        {
            node.sidechain_source = None;
            #[cfg(feature = "tracing")]
            tracing::debug!("graph_sidechain_remove: node {id}");
            return true;
        }
        false
    }

    /// Connects two nodes with a feedback edge (exempt from cycle detection).
    ///
    /// A feedback edge models the previous block's output feeding back as input,
    /// making cycles causal.  The schedule compiler inserts a 1-block
    /// [`FeedbackDelay`](super::schedule::ProcessStep::FeedbackDelay) step on the
    /// feedback buffer automatically, so no manual delay is needed.
    ///
    /// Typical uses: Karplus-Strong synthesis, analog filter resonance, tape echo.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::NodeNotFound`] if either node doesn't exist, or
    /// [`GraphError::DuplicateEdge`] if an edge already exists between the nodes.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Feedback loop: split → gain → merge (1-block delay inserted automatically)
    /// graph.connect_feedback(gain_node, merge_node)?;
    /// ```
    pub fn connect_feedback(&mut self, from: NodeId, to: NodeId) -> Result<EdgeId, GraphError> {
        self.get_node(from)?;
        self.get_node(to)?;

        if self.has_edge(from, to) {
            return Err(GraphError::DuplicateEdge(from, to));
        }

        let edge_id = EdgeId(self.next_edge_slot);
        self.next_edge_slot += 1;

        let edge = Edge {
            from,
            to,
            is_feedback: true,
        };

        let edge_idx = edge_id.0 as usize;
        if edge_idx >= self.edges.len() {
            self.edges.resize_with(edge_idx + 1, || None);
        }
        self.edges[edge_idx] = Some(edge);

        self.nodes[from.0 as usize]
            .as_mut()
            .unwrap()
            .outgoing
            .push(edge_id);
        self.nodes[to.0 as usize]
            .as_mut()
            .unwrap()
            .incoming
            .push(edge_id);

        #[cfg(feature = "tracing")]
        tracing::debug!("graph_feedback: {from} ──FB──► {to}");
        Ok(edge_id)
    }

    /// Sets the processing rate for a node.
    ///
    /// - [`NodeRate::Audio`] (default): processes every sample in the block.
    /// - [`NodeRate::Control`]: processes once per block; the single output sample
    ///   is held (replicated) across the whole block via a
    ///   [`SampleAndHold`](super::schedule::ProcessStep::SampleAndHold) step.
    ///
    /// Only meaningful for `Effect` nodes.  Has no observable effect on Input,
    /// Output, Split, Merge, or SubGraph nodes.
    ///
    /// After calling this method, recompile the graph with [`compile()`](Self::compile).
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::NodeNotFound`] if the node does not exist.
    pub fn set_node_rate(&mut self, id: NodeId, rate: NodeRate) -> Result<(), GraphError> {
        let node = self
            .nodes
            .get_mut(id.0 as usize)
            .and_then(|n| n.as_mut())
            .ok_or(GraphError::NodeNotFound(id))?;
        node.node_rate = rate;
        Ok(())
    }

    /// Adds a sub-graph node wrapping the given [`ProcessingGraph`].
    ///
    /// The sub-graph is processed as a single opaque step in the outer schedule:
    /// its internal Input and Output nodes are wired to the outer buffer slots,
    /// and its own compiled schedule runs in full.
    ///
    /// The sub-graph must be compiled (via [`ProcessingGraph::compile`]) before the
    /// outer graph is compiled, otherwise `process_block` will panic.
    pub fn add_sub_graph(&mut self, inner: ProcessingGraph) -> NodeId {
        let id = self.add_node(NodeKind::SubGraph(Box::new(inner)));
        #[cfg(feature = "tracing")]
        tracing::debug!("graph_add: sub-graph node {id}");
        id
    }

    /// Returns a mutable reference to the effect inside a node (as `dyn Effect`).
    ///
    /// For parameter access, use [`effect_with_params_mut()`](Self::effect_with_params_mut).
    /// Returns `None` if the node doesn't exist or isn't an Effect node.
    pub fn effect_mut(&mut self, id: NodeId) -> Option<&mut (dyn Effect + Send)> {
        let node = self.nodes.get_mut(id.0 as usize)?.as_mut()?;
        match &mut node.kind {
            NodeKind::Effect(effect) => Some(effect.as_mut() as &mut (dyn Effect + Send)),
            _ => None,
        }
    }

    /// Returns a reference to the effect inside a node (as `dyn Effect`).
    ///
    /// For parameter access, use [`effect_with_params_ref()`](Self::effect_with_params_ref).
    /// Returns `None` if the node doesn't exist or isn't an Effect node.
    pub fn effect_ref(&self, id: NodeId) -> Option<&(dyn Effect + Send)> {
        let node = self.nodes.get(id.0 as usize)?.as_ref()?;
        match &node.kind {
            NodeKind::Effect(effect) => Some(effect.as_ref() as &(dyn Effect + Send)),
            _ => None,
        }
    }

    /// Returns a mutable reference to the effect with parameter access.
    ///
    /// Returns `None` if the node doesn't exist or isn't an Effect node.
    pub fn effect_with_params_mut(
        &mut self,
        id: NodeId,
    ) -> Option<&mut (dyn EffectWithParams + Send)> {
        let node = self.nodes.get_mut(id.0 as usize)?.as_mut()?;
        match &mut node.kind {
            NodeKind::Effect(effect) => Some(effect.as_mut()),
            _ => None,
        }
    }

    /// Returns a reference to the effect with parameter access.
    ///
    /// Returns `None` if the node doesn't exist or isn't an Effect node.
    pub fn effect_with_params_ref(&self, id: NodeId) -> Option<&(dyn EffectWithParams + Send)> {
        let node = self.nodes.get(id.0 as usize)?.as_ref()?;
        match &node.kind {
            NodeKind::Effect(effect) => Some(effect.as_ref()),
            _ => None,
        }
    }

    /// Extracts the effect from a node, replacing it with a passthrough.
    ///
    /// Returns `None` if the node doesn't exist or isn't an Effect node.
    /// The node remains in the graph as a no-op passthrough (the node kind
    /// becomes `Split` — a single-input/single-output passthrough).
    pub fn take_effect(&mut self, id: NodeId) -> Option<Box<dyn EffectWithParams + Send>> {
        let node = self.nodes.get_mut(id.0 as usize)?.as_mut()?;
        match &mut node.kind {
            NodeKind::Effect(_) => {
                // Swap out the effect, replacing with Split (acts as passthrough).
                let mut replacement = NodeKind::Split;
                core::mem::swap(&mut node.kind, &mut replacement);
                match replacement {
                    NodeKind::Effect(effect) => Some(effect),
                    _ => unreachable!(),
                }
            }
            _ => None,
        }
    }

    /// Sets the bypass state of an effect node.
    ///
    /// When bypassed, audio passes through unchanged with a click-free crossfade.
    /// Has no effect on non-Effect nodes.
    pub fn set_bypass(&mut self, id: NodeId, bypassed: bool) {
        if let Some(Some(node)) = self.nodes.get_mut(id.0 as usize)
            && matches!(node.kind, NodeKind::Effect(_))
        {
            node.bypassed = bypassed;
            node.bypass_fade
                .set_target(if bypassed { 0.0 } else { 1.0 });
        }
    }

    /// Returns whether the node is bypassed.
    pub fn is_bypassed(&self, id: NodeId) -> bool {
        self.nodes
            .get(id.0 as usize)
            .and_then(|n| n.as_ref())
            .is_some_and(|n| n.bypassed)
    }

    /// Enables or disables output tapping for an effect node.
    ///
    /// When enabled, the node's post-processing output buffer is copied to
    /// `tap_buf` each block and can be read with [`read_tap()`](Self::read_tap).
    /// When disabled, `tap_buf` is freed to release memory.
    ///
    /// Has no effect on non-Effect nodes.
    ///
    /// # Arguments
    ///
    /// * `id` — target node
    /// * `enabled` — `true` to start tapping, `false` to stop
    pub fn set_tap(&mut self, id: NodeId, enabled: bool) {
        if let Some(Some(node)) = self.nodes.get_mut(id.0 as usize) {
            node.tapped = enabled;
            if enabled {
                node.tap_buf.resize(self.block_size);
            } else {
                node.tap_buf.resize(0);
            }
        }
    }

    /// Returns the tapped output for a node, if tapping is active.
    ///
    /// The slices are valid for the most recently processed block. Returns
    /// `None` if the node is not tapped, does not exist, or has no data yet
    /// (i.e. `tap_buf` is zero-length).
    ///
    /// Returns `(left_channel, right_channel)`.
    pub fn read_tap(&self, id: NodeId) -> Option<(&[f32], &[f32])> {
        let node = self.nodes.get(id.0 as usize)?.as_ref()?;
        if !node.tapped || node.tap_buf.is_empty() {
            return None;
        }
        Some((&node.tap_buf.left, &node.tap_buf.right))
    }

    /// Returns the per-block peak output level `(left, right)` for an effect node.
    ///
    /// Peak is the maximum absolute sample value observed in the output buffer
    /// during the most recently processed block. Returns `None` if the node
    /// does not exist.
    pub fn node_peak(&self, id: NodeId) -> Option<(f32, f32)> {
        let node = self.nodes.get(id.0 as usize)?.as_ref()?;
        Some(node.peak_out)
    }

    /// Returns the number of active (non-removed) nodes.
    pub fn node_count(&self) -> usize {
        self.nodes.iter().filter(|n| n.is_some()).count()
    }

    /// Returns the number of active edges.
    pub fn edge_count(&self) -> usize {
        self.edges.iter().filter(|e| e.is_some()).count()
    }

    /// Returns the sample rate.
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Returns the block size.
    pub fn block_size(&self) -> usize {
        self.block_size
    }

    /// Returns the current compiled schedule, if any.
    pub fn compiled(&self) -> Option<&Arc<CompiledSchedule>> {
        self.compiled.as_ref()
    }

    // --- Compilation (Phase 2) ---

    /// Compiles the graph into a [`CompiledSchedule`].
    ///
    /// Performs topological sort (Kahn's algorithm), buffer liveness analysis,
    /// and latency compensation. The resulting schedule is an immutable snapshot
    /// that can be executed by the audio thread.
    ///
    /// Also sizes the persistent [`BufferPool`] and [`CompensationDelay`] lines
    /// for zero-allocation audio processing.
    ///
    /// If a previous schedule exists, sets up a crossfade for click-free
    /// transition (~5ms via [`SmoothedParam`]).
    ///
    /// # Errors
    ///
    /// Returns [`GraphError`] if:
    /// - The graph doesn't have exactly 1 Input and 1 Output node
    /// - The graph contains a cycle (defensive — primary check is at `connect()` time)
    /// - The graph is empty
    pub fn compile(&mut self) -> Result<Arc<CompiledSchedule>, GraphError> {
        // Validate: exactly 1 Input, 1 Output.
        let (input_count, output_count) = self.count_io_nodes();
        if input_count != 1 {
            return Err(GraphError::InvalidInputCount(input_count));
        }
        if output_count != 1 {
            return Err(GraphError::InvalidOutputCount(output_count));
        }

        // Kahn's topological sort.
        let sorted = self.kahn_sort()?;
        #[cfg(feature = "tracing")]
        tracing::debug!("graph_sort: {} nodes in topo order", sorted.len());

        // Find input and output node indices.
        let input_node_idx = self.find_node_by_kind_is_input();
        let output_node_idx = self.find_node_by_kind_is_output();

        // Compute per-node cumulative latency (longest path from input).
        let node_latency = self.compute_node_latencies(&sorted);

        // Emit raw schedule steps (with inline delay compensation).
        let (
            raw_steps,
            edge_first_write,
            edge_last_read,
            delay_sample_counts,
            _emit_feedback_count,
        ) = self.emit_raw_schedule(&sorted, input_node_idx, output_node_idx, &node_latency);

        // Buffer liveness analysis: assign buffer slots.
        let (final_steps, buffer_count) =
            Self::assign_buffers(raw_steps, &edge_first_write, &edge_last_read);

        // Total latency is the max latency at the Output node.
        let total_latency = sorted
            .iter()
            .filter(|&&idx| {
                self.nodes[idx]
                    .as_ref()
                    .is_some_and(|n| matches!(n.kind, NodeKind::Output))
            })
            .map(|&idx| node_latency[idx])
            .max()
            .unwrap_or(0);

        #[cfg(feature = "tracing")]
        tracing::debug!(
            "graph_buffers: {} steps, {} physical buffers",
            final_steps.len(),
            buffer_count
        );
        #[cfg(feature = "tracing")]
        tracing::debug!(
            "graph_latency: {} samples, {} compensation delays",
            total_latency,
            delay_sample_counts.len()
        );

        // Log the compiled schedule step-by-step.
        #[cfg(feature = "tracing")]
        for (i, step) in final_steps.iter().enumerate() {
            tracing::debug!("  step[{i}]: {}", format_step(step));
        }

        // Count FeedbackDelay steps to size the feedback delay line array.
        let feedback_delay_count = final_steps
            .iter()
            .filter(|s| matches!(s, ProcessStep::FeedbackDelay { .. }))
            .count();

        // Build the compiled schedule (lightweight — no pool or delay lines).
        let schedule = Arc::new(CompiledSchedule {
            steps: final_steps,
            buffer_count,
            delay_sample_counts: delay_sample_counts.clone(),
            total_latency,
            block_size: self.block_size,
            feedback_delay_count,
        });

        // Pre-allocate audio pool for RT-safe execution.
        if self.audio_pool.count() < buffer_count {
            self.audio_pool = BufferPool::new(buffer_count, self.block_size);
        } else {
            self.audio_pool.resize_all(self.block_size);
        }

        // Build persistent compensation delay lines (state persists across blocks).
        self.audio_delay_lines = delay_sample_counts
            .into_iter()
            .map(CompensationDelay::new)
            .collect();

        // Build persistent feedback delay lines (1 block each, state persists).
        self.feedback_delay_lines = (0..feedback_delay_count)
            .map(|_| CompensationDelay::new(self.block_size))
            .collect();

        // Ensure crossfade buffers match current block size.
        self.crossfade_left.resize(self.block_size, 0.0);
        self.crossfade_right.resize(self.block_size, 0.0);

        // Click-free swap: keep old schedule for crossfade.
        if self.compiled.is_some() {
            self.prev_compiled = self.compiled.take();
            self.swap_fade = SmoothedParam::fast(0.0, self.sample_rate);
            self.swap_fade.set_target(1.0);
            #[cfg(feature = "tracing")]
            tracing::debug!("graph_swap: crossfade from previous schedule");
        }

        self.compiled = Some(Arc::clone(&schedule));
        Ok(schedule)
    }

    // --- Kahn's topological sort ---

    /// Performs Kahn's algorithm for topological sorting.
    ///
    /// Returns the sorted node indices, or `CycleDetected` if the graph has a cycle.
    fn kahn_sort(&self) -> Result<Vec<usize>, GraphError> {
        let n = self.nodes.len();
        let mut in_degree = vec![0u32; n];
        let mut active_count = 0usize;

        // Compute in-degrees for active nodes.
        // Feedback edges are excluded: they model the previous block's output
        // feeding back as input, so they don't participate in the topological
        // ordering of the current block's processing.
        for (i, node_opt) in self.nodes.iter().enumerate() {
            if node_opt.is_some() {
                active_count += 1;
                for edge_id in &node_opt.as_ref().unwrap().incoming {
                    if let Some(edge) = self.edges[edge_id.0 as usize].as_ref()
                        && !edge.is_feedback
                    {
                        in_degree[i] += 1;
                    }
                }
            }
        }

        if active_count == 0 {
            return Err(GraphError::EmptyGraph);
        }

        // Seed queue with zero in-degree nodes.
        let mut queue: Vec<usize> = (0..n)
            .filter(|&i| self.nodes[i].is_some() && in_degree[i] == 0)
            .collect();

        let mut sorted = Vec::with_capacity(active_count);

        while let Some(idx) = queue.pop() {
            sorted.push(idx);
            let node = self.nodes[idx].as_ref().unwrap();
            for edge_id in &node.outgoing {
                if let Some(edge) = &self.edges[edge_id.0 as usize] {
                    // Feedback edges are excluded from ordering (they are causal).
                    if edge.is_feedback {
                        continue;
                    }
                    let to_idx = edge.to.0 as usize;
                    in_degree[to_idx] -= 1;
                    if in_degree[to_idx] == 0 {
                        queue.push(to_idx);
                    }
                }
            }
        }

        if sorted.len() != active_count {
            return Err(GraphError::CycleDetected);
        }

        Ok(sorted)
    }

    // --- Raw schedule emission ---

    /// Emits raw `ProcessStep`s from the topological order.
    ///
    /// Returns the steps, per-edge first-write/last-read step indices for liveness
    /// analysis, delay sample counts for latency compensation, and a count of
    /// feedback delay lines needed. Delay insertion happens here (in virtual-buffer
    /// space) to avoid aliasing bugs that would occur if inserted after physical
    /// buffer assignment.
    #[allow(clippy::type_complexity)]
    fn emit_raw_schedule(
        &self,
        sorted: &[usize],
        _input_node_idx: usize,
        _output_node_idx: usize,
        node_latency: &[usize],
    ) -> (
        Vec<RawStep>,
        Vec<(usize, usize)>,
        Vec<(usize, usize)>,
        Vec<usize>,
        usize, // feedback_delay_count
    ) {
        // Map each edge to a temporary "virtual buffer" ID (1:1 with edge index for now).
        // Liveness analysis will collapse these into physical buffer slots.

        let mut steps = Vec::new();
        let mut delay_sample_counts: Vec<usize> = Vec::new();
        let mut feedback_delay_count = 0usize;

        // Build edge → virtual buffer mapping.
        let mut edge_to_vbuf: Vec<Option<usize>> = vec![None; self.edges.len()];
        let mut vbuf_count = 0usize;
        for (i, edge_opt) in self.edges.iter().enumerate() {
            if edge_opt.is_some() {
                edge_to_vbuf[i] = Some(vbuf_count);
                vbuf_count += 1;
            }
        }

        // Initialize first_write / last_read tracking.
        // Indexed by virtual buffer ID.
        let mut vbuf_first_write = vec![usize::MAX; vbuf_count];
        let mut vbuf_last_read = vec![0usize; vbuf_count];

        // Track each node's primary output vbuf for sidechain consumers to look up.
        let mut node_output_vbuf: Vec<Option<usize>> = vec![None; self.nodes.len()];

        for &node_idx in sorted {
            let node = self.nodes[node_idx].as_ref().unwrap();
            let _step_idx = steps.len();

            match &node.kind {
                NodeKind::Input => {
                    // Write external input into each outgoing edge's buffer.
                    for edge_id in &node.outgoing {
                        if let Some(vbuf) = edge_to_vbuf[edge_id.0 as usize] {
                            steps.push(RawStep::WriteInput { vbuf });
                            if vbuf_first_write[vbuf] == usize::MAX {
                                vbuf_first_write[vbuf] = steps.len() - 1;
                            }
                            // Record first output vbuf for potential sidechain references.
                            if node_output_vbuf[node_idx].is_none() {
                                node_output_vbuf[node_idx] = Some(vbuf);
                            }
                        }
                    }
                }

                NodeKind::Output => {
                    // Read from the incoming edge's buffer.
                    if let Some(edge_id) = node.incoming.first()
                        && let Some(vbuf) = edge_to_vbuf[edge_id.0 as usize]
                    {
                        steps.push(RawStep::ReadOutput { vbuf });
                        vbuf_last_read[vbuf] = vbuf_last_read[vbuf].max(steps.len() - 1);
                    }
                }

                NodeKind::Effect(_) => {
                    // Input: first non-feedback incoming edge. Output: first outgoing edge.
                    let in_vbuf = node
                        .incoming
                        .iter()
                        .find(|eid| {
                            self.edges[eid.0 as usize]
                                .as_ref()
                                .is_some_and(|e| !e.is_feedback)
                        })
                        .and_then(|eid| edge_to_vbuf[eid.0 as usize]);
                    let out_vbuf = node
                        .outgoing
                        .first()
                        .and_then(|eid| edge_to_vbuf[eid.0 as usize]);

                    // Process feedback incoming edges: emit FeedbackDelay steps.
                    for eid in &node.incoming {
                        if let Some(edge) = self.edges[eid.0 as usize].as_ref()
                            && edge.is_feedback
                            && let Some(fv) = edge_to_vbuf[eid.0 as usize]
                        {
                            let delay_line_idx = feedback_delay_count;
                            feedback_delay_count += 1;
                            steps.push(RawStep::FeedbackDelay {
                                vbuf: fv,
                                delay_line_idx,
                            });
                            let s = steps.len() - 1;
                            if vbuf_first_write[fv] == usize::MAX {
                                vbuf_first_write[fv] = s;
                            }
                            vbuf_last_read[fv] = vbuf_last_read[fv].max(s);
                        }
                    }

                    // Sidechain: look up the output vbuf of the sidechain source node.
                    let sc_vbuf = node.sidechain_source.and_then(|sc_id| {
                        node_output_vbuf.get(sc_id.0 as usize).copied().flatten()
                    });

                    let is_control_rate = matches!(node.node_rate, NodeRate::Control(_));

                    if let (Some(iv), Some(ov)) = (in_vbuf, out_vbuf) {
                        steps.push(RawStep::ProcessEffect {
                            node_idx,
                            input_vbuf: iv,
                            output_vbuf: ov,
                            sidechain_vbuf: sc_vbuf,
                            is_control_rate,
                        });
                        let s = steps.len() - 1;
                        vbuf_last_read[iv] = vbuf_last_read[iv].max(s);
                        if vbuf_first_write[ov] == usize::MAX {
                            vbuf_first_write[ov] = s;
                        }
                        // Extend liveness of the sidechain buffer to this step.
                        if let Some(sv) = sc_vbuf
                            && sv < vbuf_last_read.len()
                        {
                            vbuf_last_read[sv] = vbuf_last_read[sv].max(s);
                        }
                        // Record output vbuf for downstream sidechain consumers.
                        node_output_vbuf[node_idx] = Some(ov);

                        // Control-rate nodes: replicate their single output sample
                        // across the entire block so downstream audio-rate nodes
                        // see a full-length buffer.
                        if is_control_rate {
                            steps.push(RawStep::SampleAndHold {
                                vbuf: ov,
                                block_len: self.block_size,
                            });
                            let s2 = steps.len() - 1;
                            vbuf_last_read[ov] = vbuf_last_read[ov].max(s2);
                        }
                    }
                }

                NodeKind::Split => {
                    // Copy from the single input to each output.
                    let in_vbuf = node
                        .incoming
                        .first()
                        .and_then(|eid| edge_to_vbuf[eid.0 as usize]);
                    let out_vbufs: Vec<usize> = node
                        .outgoing
                        .iter()
                        .filter_map(|eid| edge_to_vbuf[eid.0 as usize])
                        .collect();

                    if let Some(iv) = in_vbuf
                        && !out_vbufs.is_empty()
                    {
                        debug_assert!(
                            out_vbufs.len() <= MAX_SPLIT_TARGETS,
                            "split fan-out {} exceeds MAX_SPLIT_TARGETS {}",
                            out_vbufs.len(),
                            MAX_SPLIT_TARGETS
                        );
                        steps.push(RawStep::SplitCopy {
                            source_vbuf: iv,
                            dest_vbufs: out_vbufs.clone(),
                        });
                        let s = steps.len() - 1;
                        vbuf_last_read[iv] = vbuf_last_read[iv].max(s);
                        for &ov in &out_vbufs {
                            if vbuf_first_write[ov] == usize::MAX {
                                vbuf_first_write[ov] = s;
                            }
                        }
                    }
                }

                NodeKind::Merge => {
                    // Sum all incoming into the single output with latency compensation.
                    let out_vbuf = node
                        .outgoing
                        .first()
                        .and_then(|eid| edge_to_vbuf[eid.0 as usize]);
                    // Collect (from_node_idx, vbuf, is_feedback) tuples.
                    let incoming_with_nodes: Vec<(usize, usize, bool)> = node
                        .incoming
                        .iter()
                        .filter_map(|eid| {
                            let edge = self.edges[eid.0 as usize].as_ref()?;
                            let vbuf = edge_to_vbuf[eid.0 as usize]?;
                            Some((edge.from.0 as usize, vbuf, edge.is_feedback))
                        })
                        .collect();

                    if let Some(ov) = out_vbuf {
                        // First, emit FeedbackDelay for any feedback inputs so the
                        // ring buffer swap happens before the samples are accumulated.
                        for &(_, iv, is_fb) in &incoming_with_nodes {
                            if is_fb {
                                let delay_line_idx = feedback_delay_count;
                                feedback_delay_count += 1;
                                steps.push(RawStep::FeedbackDelay {
                                    vbuf: iv,
                                    delay_line_idx,
                                });
                                let s = steps.len() - 1;
                                if vbuf_first_write[iv] == usize::MAX {
                                    vbuf_first_write[iv] = s;
                                }
                                vbuf_last_read[iv] = vbuf_last_read[iv].max(s);
                            }
                        }

                        steps.push(RawStep::ClearBuffer { vbuf: ov });
                        let s = steps.len() - 1;
                        if vbuf_first_write[ov] == usize::MAX {
                            vbuf_first_write[ov] = s;
                        }

                        // Only non-feedback paths participate in latency computation.
                        let max_lat = incoming_with_nodes
                            .iter()
                            .filter(|&&(_, _, is_fb)| !is_fb)
                            .map(|&(from_idx, _, _)| node_latency[from_idx])
                            .max()
                            .unwrap_or(0);

                        let path_count = incoming_with_nodes.len();
                        let gain = 1.0 / path_count as f32;
                        #[cfg(feature = "tracing")]
                        tracing::debug!("  merge_gain: {path_count} paths, gain={gain:.3}");

                        for &(from_idx, iv, is_fb) in &incoming_with_nodes {
                            // Feedback paths already have their delay from FeedbackDelay above.
                            let delay = if is_fb {
                                0
                            } else {
                                max_lat.saturating_sub(node_latency[from_idx])
                            };
                            if delay > 0 {
                                let delay_line_idx = delay_sample_counts.len();
                                delay_sample_counts.push(delay);
                                steps.push(RawStep::DelayCompensate {
                                    vbuf: iv,
                                    delay_line_idx,
                                });
                                let s = steps.len() - 1;
                                vbuf_last_read[iv] = vbuf_last_read[iv].max(s);
                                #[cfg(feature = "tracing")]
                                tracing::debug!(
                                    "  merge_delay: node {from_idx} needs {delay} sample delay on vbuf[{iv}]"
                                );
                            }
                            steps.push(RawStep::AccumulateBuffer {
                                source_vbuf: iv,
                                dest_vbuf: ov,
                                gain,
                            });
                            let s = steps.len() - 1;
                            vbuf_last_read[iv] = vbuf_last_read[iv].max(s);
                        }
                    }
                }

                NodeKind::SubGraph(_) => {
                    // Sub-graph: route like a single Effect node — one input vbuf, one output vbuf.
                    let in_vbuf = node
                        .incoming
                        .first()
                        .and_then(|eid| edge_to_vbuf[eid.0 as usize]);
                    let out_vbuf = node
                        .outgoing
                        .first()
                        .and_then(|eid| edge_to_vbuf[eid.0 as usize]);

                    if let (Some(iv), Some(ov)) = (in_vbuf, out_vbuf) {
                        steps.push(RawStep::ProcessSubGraph {
                            node_idx,
                            input_vbuf: iv,
                            output_vbuf: ov,
                        });
                        let s = steps.len() - 1;
                        vbuf_last_read[iv] = vbuf_last_read[iv].max(s);
                        if vbuf_first_write[ov] == usize::MAX {
                            vbuf_first_write[ov] = s;
                        }
                        node_output_vbuf[node_idx] = Some(ov);
                    }
                }
            }
        }

        // Build per-vbuf first_write/last_read.
        let edge_first_write: Vec<(usize, usize)> =
            vbuf_first_write.into_iter().enumerate().collect();
        let edge_last_read: Vec<(usize, usize)> = vbuf_last_read.into_iter().enumerate().collect();

        (
            steps,
            edge_first_write,
            edge_last_read,
            delay_sample_counts,
            feedback_delay_count,
        )
    }

    // --- Buffer liveness analysis ---

    /// Assigns physical buffer slots to virtual buffers using liveness analysis.
    ///
    /// This is register allocation for audio buffers: a physical slot is "live"
    /// from first write to last read. After the last reader, the slot returns to
    /// the free list. Result: a 20-node linear chain needs only 2 physical buffers.
    fn assign_buffers(
        raw_steps: Vec<RawStep>,
        first_write: &[(usize, usize)],
        last_read: &[(usize, usize)],
    ) -> (Vec<ProcessStep>, usize) {
        let vbuf_count = first_write.len();

        // Build liveness intervals: (first_write_step, last_read_step) per vbuf.
        let mut intervals: Vec<(usize, usize)> = Vec::with_capacity(vbuf_count);
        for i in 0..vbuf_count {
            intervals.push((first_write[i].1, last_read[i].1));
        }

        // Greedy assignment: walk steps in order, allocate physical slots.
        let mut vbuf_to_phys: Vec<Option<usize>> = vec![None; vbuf_count];
        let mut phys_count = 0usize;
        // Track which physical slots are free and at which step they become free.
        let mut free_at: Vec<(usize, usize)> = Vec::new(); // (step_when_free, phys_slot)

        // Sort vbufs by first_write step for greedy allocation.
        let mut vbuf_order: Vec<usize> = (0..vbuf_count).collect();
        vbuf_order.sort_by_key(|&v| intervals[v].0);

        for &vbuf in &vbuf_order {
            let (fw, lr) = intervals[vbuf];
            if fw == usize::MAX {
                // Never written — skip (disconnected edge remnant).
                continue;
            }

            // Try to reuse a physical slot that's free before this vbuf's first write.
            let mut assigned = None;
            for (i, &(free_step, phys)) in free_at.iter().enumerate() {
                if free_step <= fw {
                    assigned = Some((i, phys));
                    break;
                }
            }

            if let Some((free_idx, phys)) = assigned {
                vbuf_to_phys[vbuf] = Some(phys);
                free_at.remove(free_idx);
                // This slot is now busy until lr.
                free_at.push((lr + 1, phys));
            } else {
                // Allocate a new physical slot.
                let phys = phys_count;
                phys_count += 1;
                vbuf_to_phys[vbuf] = Some(phys);
                free_at.push((lr + 1, phys));
            }
        }

        // At minimum we need 1 buffer (degenerate case: Input → Output).
        let buffer_count = phys_count.max(1);

        // Convert RawSteps to ProcessSteps using physical buffer indices.
        let steps = raw_steps
            .into_iter()
            .map(|raw| match raw {
                RawStep::WriteInput { vbuf } => ProcessStep::WriteInput {
                    buffer_idx: vbuf_to_phys[vbuf].unwrap_or(0),
                },
                RawStep::ProcessEffect {
                    node_idx,
                    input_vbuf,
                    output_vbuf,
                    sidechain_vbuf,
                    is_control_rate,
                } => ProcessStep::ProcessEffect {
                    node_idx,
                    input_buf: vbuf_to_phys[input_vbuf].unwrap_or(0),
                    output_buf: vbuf_to_phys[output_vbuf].unwrap_or(0),
                    sidechain_buf: sidechain_vbuf
                        .and_then(|sv| vbuf_to_phys.get(sv).copied().flatten()),
                    is_control_rate,
                },
                RawStep::SplitCopy {
                    source_vbuf,
                    dest_vbufs,
                } => {
                    let mut dest_arr = [0usize; MAX_SPLIT_TARGETS];
                    let count = dest_vbufs.len().min(MAX_SPLIT_TARGETS);
                    for (i, v) in dest_vbufs.into_iter().enumerate().take(count) {
                        dest_arr[i] = vbuf_to_phys[v].unwrap_or(0);
                    }
                    ProcessStep::SplitCopy {
                        source_buf: vbuf_to_phys[source_vbuf].unwrap_or(0),
                        dest_bufs: dest_arr,
                        dest_count: count,
                    }
                }
                RawStep::ClearBuffer { vbuf } => ProcessStep::ClearBuffer {
                    buffer_idx: vbuf_to_phys[vbuf].unwrap_or(0),
                },
                RawStep::AccumulateBuffer {
                    source_vbuf,
                    dest_vbuf,
                    gain,
                } => ProcessStep::AccumulateBuffer {
                    source_buf: vbuf_to_phys[source_vbuf].unwrap_or(0),
                    dest_buf: vbuf_to_phys[dest_vbuf].unwrap_or(0),
                    gain,
                },
                RawStep::ReadOutput { vbuf } => ProcessStep::ReadOutput {
                    buffer_idx: vbuf_to_phys[vbuf].unwrap_or(0),
                },
                RawStep::DelayCompensate {
                    vbuf,
                    delay_line_idx,
                } => ProcessStep::DelayCompensate {
                    buffer_idx: vbuf_to_phys[vbuf].unwrap_or(0),
                    delay_line_idx,
                },
                RawStep::ProcessSubGraph {
                    node_idx,
                    input_vbuf,
                    output_vbuf,
                } => ProcessStep::ProcessSubGraph {
                    node_idx,
                    input_buf: vbuf_to_phys[input_vbuf].unwrap_or(0),
                    output_buf: vbuf_to_phys[output_vbuf].unwrap_or(0),
                },
                RawStep::FeedbackDelay {
                    vbuf,
                    delay_line_idx,
                } => ProcessStep::FeedbackDelay {
                    buffer_idx: vbuf_to_phys[vbuf].unwrap_or(0),
                    delay_line_idx,
                },
                RawStep::SampleAndHold { vbuf, block_len } => ProcessStep::SampleAndHold {
                    buffer_idx: vbuf_to_phys[vbuf].unwrap_or(0),
                    block_len,
                },
            })
            .collect();

        (steps, buffer_count)
    }

    /// Computes cumulative latency to each node (longest path from Input).
    ///
    /// For each node in topological order, its latency is the maximum incoming
    /// latency plus its own latency contribution.
    fn compute_node_latencies(&self, sorted: &[usize]) -> Vec<usize> {
        let n = self.nodes.len();
        let mut node_latency = vec![0usize; n];

        for &node_idx in sorted {
            let node = self.nodes[node_idx].as_ref().unwrap();

            let own_latency = match &node.kind {
                NodeKind::Effect(effect) => effect.latency_samples(),
                _ => 0,
            };

            let max_incoming = node
                .incoming
                .iter()
                .filter_map(|eid| {
                    self.edges[eid.0 as usize]
                        .as_ref()
                        .map(|e| node_latency[e.from.0 as usize])
                })
                .max()
                .unwrap_or(0);

            node_latency[node_idx] = max_incoming + own_latency;
        }

        node_latency
    }

    // --- Audio execution (Phase 3) ---

    /// Processes one block of stereo audio through the compiled graph.
    ///
    /// If the graph was recently recompiled, crossfades from cached previous
    /// output toward the new schedule's output over ~5ms. Only the new schedule
    /// executes — no double-processing of effects.
    ///
    /// # Panics
    ///
    /// Panics if `compile()` has not been called, or if buffer lengths don't
    /// match the graph's block size.
    pub fn process_block(
        &mut self,
        left_in: &[f32],
        right_in: &[f32],
        left_out: &mut [f32],
        right_out: &mut [f32],
    ) {
        let len = left_in.len();
        debug_assert_eq!(left_in.len(), right_in.len());
        debug_assert!(left_out.len() >= len);
        debug_assert!(right_out.len() >= len);

        let schedule = self
            .compiled
            .as_ref()
            .expect("process_block called before compile()")
            .clone();

        let is_crossfading = !self.swap_fade.is_settled();

        // Execute the current schedule (always — even during crossfade, only the new one runs).
        Self::run_schedule(
            &mut self.nodes,
            &schedule,
            &mut self.audio_pool,
            &mut self.audio_delay_lines,
            &mut self.feedback_delay_lines,
            &mut self.sc_tmp_left,
            &mut self.sc_tmp_right,
            left_in,
            right_in,
            left_out,
            right_out,
        );

        if is_crossfading {
            // Blend from cached previous output toward new output.
            for i in 0..len {
                let fade = self.swap_fade.advance();
                left_out[i] = self.crossfade_left.get(i).copied().unwrap_or(0.0) * (1.0 - fade)
                    + left_out[i] * fade;
                right_out[i] = self.crossfade_right.get(i).copied().unwrap_or(0.0) * (1.0 - fade)
                    + right_out[i] * fade;
            }

            // If crossfade is done, drop the old schedule reference.
            if self.swap_fade.is_settled() {
                self.prev_compiled = None;
            }
        }

        // Process spillover tails and mix into output.
        self.spillover_tails.retain_mut(|tail| {
            if tail.remaining == 0 {
                return false;
            }
            let buf_len = len.min(tail.left_buf.len());
            // Fill scratch buffers with silence — the effect's internal state
            // (reverb feedback, delay lines) produces the ring-out as output.
            tail.left_buf[..buf_len].fill(0.0);
            tail.right_buf[..buf_len].fill(0.0);
            // Process: silence in, tail decay out (in-place).
            tail.effect.process_block_stereo_inplace(
                &mut tail.left_buf[..buf_len],
                &mut tail.right_buf[..buf_len],
            );
            // Mix faded tail into the final output.
            for i in 0..buf_len {
                let fade = tail.fade.advance();
                left_out[i] += tail.left_buf[i] * fade;
                right_out[i] += tail.right_buf[i] * fade;
            }
            tail.remaining = tail.remaining.saturating_sub(buf_len);
            tail.remaining > 0 && !tail.fade.is_settled()
        });

        // Cache output for potential future crossfade.
        let cache_len = len.min(self.crossfade_left.len());
        self.crossfade_left[..cache_len].copy_from_slice(&left_out[..cache_len]);
        self.crossfade_right[..cache_len].copy_from_slice(&right_out[..cache_len]);
    }

    /// Executes a compiled schedule against the given node state.
    ///
    /// Static method to enable disjoint field borrows — callers can pass
    /// `&mut self.nodes` alongside other `self` fields (e.g., crossfade buffers)
    /// without conflicting with the borrow checker.
    ///
    /// **RT-safety**: Zero heap allocations. Pool and delay lines are persistent
    /// fields passed in by reference. All split-borrow workarounds use
    /// `BufferPool::get_ref_and_mut` instead of temporary `Vec`s.
    #[allow(clippy::too_many_arguments)]
    fn run_schedule(
        nodes: &mut [Option<NodeData>],
        schedule: &CompiledSchedule,
        pool: &mut BufferPool,
        delay_lines: &mut [CompensationDelay],
        feedback_delay_lines: &mut [CompensationDelay],
        sc_tmp_left: &mut [f32],
        sc_tmp_right: &mut [f32],
        left_in: &[f32],
        right_in: &[f32],
        left_out: &mut [f32],
        right_out: &mut [f32],
    ) {
        let len = left_in.len();
        pool.clear_all();

        let mut execute =
            |nodes: &mut [Option<NodeData>],
             pool: &mut BufferPool,
             delay_lines: &mut [CompensationDelay],
             feedback_delay_lines: &mut [CompensationDelay]| {
                for step in &schedule.steps {
                    match step {
                        ProcessStep::WriteInput { buffer_idx } => {
                            let buf = pool.get_mut(*buffer_idx);
                            buf.left[..len].copy_from_slice(&left_in[..len]);
                            buf.right[..len].copy_from_slice(&right_in[..len]);
                        }

                        ProcessStep::ProcessEffect {
                            node_idx,
                            input_buf,
                            output_buf,
                            sidechain_buf,
                            is_control_rate,
                        } => {
                            if let Some(Some(node)) = nodes.get_mut(*node_idx) {
                                // Control-rate nodes: process only 1 sample (sample-and-hold
                                // is a separate step inserted by the compiler).
                                let proc_len = if *is_control_rate { 1 } else { len };
                                // Peak input: max abs over the input buffer before processing.
                                {
                                    let inp = pool.get(*input_buf);
                                    let peak_l = inp.left[..len]
                                        .iter()
                                        .fold(0.0f32, |acc, &s| acc.max(s.abs()));
                                    let peak_r = inp.right[..len]
                                        .iter()
                                        .fold(0.0f32, |acc, &s| acc.max(s.abs()));
                                    node.peak_in = (peak_l, peak_r);
                                }

                                let bypass_settled = node.bypassed && node.bypass_fade.is_settled();
                                let bypass_fading = !bypass_settled
                                    && (node.bypassed || !node.bypass_fade.is_settled());

                                if bypass_settled {
                                    // Fully bypassed and fade complete — copy input to output,
                                    // skip effect processing entirely (massive CPU savings).
                                    if *input_buf != *output_buf {
                                        let (inp, out) =
                                            pool.get_ref_and_mut(*input_buf, *output_buf);
                                        out.left[..len].copy_from_slice(&inp.left[..len]);
                                        out.right[..len].copy_from_slice(&inp.right[..len]);
                                    }
                                    // If input_buf == output_buf, data is already in place.
                                } else {
                                    // Phase 1: Save dry signal before effect processing.
                                    if bypass_fading {
                                        let src = pool.get(*input_buf);
                                        node.bypass_buf.left[..len]
                                            .copy_from_slice(&src.left[..len]);
                                        node.bypass_buf.right[..len]
                                            .copy_from_slice(&src.right[..len]);
                                    }

                                    // Phase 2: Process through effect.
                                    if let NodeKind::Effect(ref mut effect) = node.kind {
                                        if let Some(sc_buf) = sidechain_buf {
                                            // Copy SC data before borrowing input/output buffers.
                                            // RT-safe: sc_tmp_* are pre-allocated at compile time.
                                            {
                                                let sc = pool.get(*sc_buf);
                                                sc_tmp_left[..proc_len]
                                                    .copy_from_slice(&sc.left[..proc_len]);
                                                sc_tmp_right[..proc_len]
                                                    .copy_from_slice(&sc.right[..proc_len]);
                                            }
                                            let sc_l = &sc_tmp_left[..proc_len];
                                            let sc_r = &sc_tmp_right[..proc_len];

                                            if *input_buf == *output_buf {
                                                let buf = pool.get_mut(*input_buf);
                                                effect.process_block_stereo_inplace_with_sidechain(
                                                    &mut buf.left[..proc_len],
                                                    &mut buf.right[..proc_len],
                                                    sc_l,
                                                    sc_r,
                                                );
                                            } else {
                                                let (inp, out) =
                                                    pool.get_ref_and_mut(*input_buf, *output_buf);
                                                effect.process_block_stereo_with_sidechain(
                                                    &inp.left[..proc_len],
                                                    &inp.right[..proc_len],
                                                    &mut out.left[..proc_len],
                                                    &mut out.right[..proc_len],
                                                    sc_l,
                                                    sc_r,
                                                );
                                            }
                                        } else if *input_buf == *output_buf {
                                            let buf = pool.get_mut(*input_buf);
                                            effect.process_block_stereo_inplace(
                                                &mut buf.left[..proc_len],
                                                &mut buf.right[..proc_len],
                                            );
                                        } else {
                                            let (inp, out) =
                                                pool.get_ref_and_mut(*input_buf, *output_buf);
                                            effect.process_block_stereo(
                                                &inp.left[..proc_len],
                                                &inp.right[..proc_len],
                                                &mut out.left[..proc_len],
                                                &mut out.right[..proc_len],
                                            );
                                        }
                                    }

                                    // Phase 3: Crossfade between dry and wet during fade.
                                    if bypass_fading {
                                        let out = pool.get_mut(*output_buf);
                                        for i in 0..len {
                                            let fade = node.bypass_fade.advance();
                                            out.left[i] = wet_dry_mix(
                                                node.bypass_buf.left[i],
                                                out.left[i],
                                                fade,
                                            );
                                            out.right[i] = wet_dry_mix(
                                                node.bypass_buf.right[i],
                                                out.right[i],
                                                fade,
                                            );
                                        }
                                    }
                                }

                                // Peak output: max abs over the output buffer after processing.
                                {
                                    let out = pool.get(*output_buf);
                                    let peak_l = out.left[..len]
                                        .iter()
                                        .fold(0.0f32, |acc, &s| acc.max(s.abs()));
                                    let peak_r = out.right[..len]
                                        .iter()
                                        .fold(0.0f32, |acc, &s| acc.max(s.abs()));
                                    node.peak_out = (peak_l, peak_r);
                                }

                                // Tap: copy output buffer to tap_buf if tapping is enabled.
                                if node.tapped && !node.tap_buf.is_empty() {
                                    let out = pool.get(*output_buf);
                                    node.tap_buf.left[..len].copy_from_slice(&out.left[..len]);
                                    node.tap_buf.right[..len].copy_from_slice(&out.right[..len]);
                                }
                            }
                        }

                        ProcessStep::SplitCopy {
                            source_buf,
                            dest_bufs,
                            dest_count,
                        } => {
                            for &dest in &dest_bufs[..*dest_count] {
                                if dest != *source_buf {
                                    let (src, dst) = pool.get_ref_and_mut(*source_buf, dest);
                                    dst.left[..len].copy_from_slice(&src.left[..len]);
                                    dst.right[..len].copy_from_slice(&src.right[..len]);
                                }
                            }
                        }

                        ProcessStep::ClearBuffer { buffer_idx } => {
                            pool.get_mut(*buffer_idx).clear();
                        }

                        ProcessStep::AccumulateBuffer {
                            source_buf,
                            dest_buf,
                            gain,
                        } => {
                            if source_buf != dest_buf {
                                let (src, dst) = pool.get_ref_and_mut(*source_buf, *dest_buf);
                                // Use f64 intermediates to avoid precision loss when summing
                                // multiple paths (merge nodes with many inputs).
                                let gain64 = *gain as f64;
                                for i in 0..len {
                                    dst.left[i] =
                                        (dst.left[i] as f64 + src.left[i] as f64 * gain64) as f32;
                                    dst.right[i] =
                                        (dst.right[i] as f64 + src.right[i] as f64 * gain64) as f32;
                                }
                            }
                        }

                        ProcessStep::DelayCompensate {
                            buffer_idx,
                            delay_line_idx,
                        } => {
                            if let Some(dl) = delay_lines.get_mut(*delay_line_idx) {
                                let buf = pool.get_mut(*buffer_idx);
                                dl.process_block_inplace(
                                    &mut buf.left[..len],
                                    &mut buf.right[..len],
                                );
                            }
                        }

                        ProcessStep::ReadOutput { buffer_idx } => {
                            let buf = pool.get(*buffer_idx);
                            left_out[..len].copy_from_slice(&buf.left[..len]);
                            right_out[..len].copy_from_slice(&buf.right[..len]);
                        }

                        ProcessStep::FeedbackDelay {
                            buffer_idx,
                            delay_line_idx,
                        } => {
                            // The feedback delay ring-buffers the previous block's samples.
                            // `process_block_inplace` outputs the stored samples and then
                            // stores the incoming samples for the next block — exactly a
                            // 1-block delay, making the feedback loop causal.
                            if let Some(dl) = feedback_delay_lines.get_mut(*delay_line_idx) {
                                let buf = pool.get_mut(*buffer_idx);
                                dl.process_block_inplace(
                                    &mut buf.left[..len],
                                    &mut buf.right[..len],
                                );
                            }
                        }

                        ProcessStep::SampleAndHold {
                            buffer_idx,
                            block_len: _,
                        } => {
                            // Replicate the first sample across the whole block.
                            // Control-rate nodes write exactly 1 sample; this fills the rest
                            // so audio-rate downstream nodes see a full-length buffer.
                            let buf = pool.get_mut(*buffer_idx);
                            let l0 = buf.left[0];
                            let r0 = buf.right[0];
                            buf.left[1..len].fill(l0);
                            buf.right[1..len].fill(r0);
                        }

                        ProcessStep::ProcessSubGraph {
                            node_idx,
                            input_buf,
                            output_buf,
                        } => {
                            // Run the inner graph as an opaque block.
                            // We need to copy from the pool into the inner graph's input,
                            // run it, and copy back out.  Sub-graphs own their own BufferPool,
                            // so we use a temporary slice approach.
                            if let Some(Some(node)) = nodes.get_mut(*node_idx)
                                && let NodeKind::SubGraph(ref mut inner) = node.kind
                            {
                                let inp = pool.get(*input_buf);
                                // Borrow checker: copy input data to avoid aliasing.
                                let left_in_sg: Vec<f32> = inp.left[..len].to_vec();
                                let right_in_sg: Vec<f32> = inp.right[..len].to_vec();

                                let out = pool.get_mut(*output_buf);
                                inner.process_block(
                                    &left_in_sg,
                                    &right_in_sg,
                                    &mut out.left[..len],
                                    &mut out.right[..len],
                                );
                            }
                        }
                    }
                }
            };

        #[cfg(all(debug_assertions, feature = "debug-alloc"))]
        assert_no_alloc(|| execute(nodes, pool, delay_lines, feedback_delay_lines));
        #[cfg(not(all(debug_assertions, feature = "debug-alloc")))]
        execute(nodes, pool, delay_lines, feedback_delay_lines);
    }

    // --- Control methods ---

    /// Sets the sample rate for all effect nodes.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.swap_fade.set_sample_rate(sample_rate);
        for node in self.nodes.iter_mut().flatten() {
            if let NodeKind::Effect(ref mut effect) = node.kind {
                effect.set_sample_rate(sample_rate);
            }
            node.bypass_fade.set_sample_rate(sample_rate);
        }
        for tail in &mut self.spillover_tails {
            tail.effect.set_sample_rate(sample_rate);
            tail.fade.set_sample_rate(sample_rate);
        }
    }

    /// Sets the block size. Requires recompilation.
    ///
    /// Also resizes pre-allocated buffers to match the new block size.
    pub fn set_block_size(&mut self, block_size: usize) {
        self.block_size = block_size;
        self.crossfade_left.resize(block_size, 0.0);
        self.crossfade_right.resize(block_size, 0.0);
        self.audio_pool.resize_all(block_size);
        for node in self.nodes.iter_mut().flatten() {
            node.bypass_buf.resize(block_size);
        }
    }

    /// Resets all effect nodes and clears delay lines.
    ///
    /// Also clears all active spillover tails.
    pub fn reset(&mut self) {
        for node in self.nodes.iter_mut().flatten() {
            if let NodeKind::Effect(ref mut effect) = node.kind {
                effect.reset();
            }
            node.bypass_fade.snap_to_target();
        }
        self.swap_fade.snap_to_target();
        self.prev_compiled = None;
        for dl in &mut self.audio_delay_lines {
            dl.clear();
        }
        self.spillover_tails.clear();
    }

    /// Broadcasts a tempo context to all effect nodes.
    pub fn set_tempo_context(&mut self, ctx: &TempoContext) {
        for node_opt in &mut self.nodes {
            if let Some(node) = node_opt
                && let NodeKind::Effect(ref mut effect) = node.kind
            {
                effect.set_tempo_context(ctx);
            }
        }
    }

    /// Returns the total graph latency in samples (longest path).
    ///
    /// Returns 0 if the graph hasn't been compiled.
    pub fn latency_samples(&self) -> usize {
        self.compiled.as_ref().map(|s| s.total_latency).unwrap_or(0)
    }

    /// Enable or disable spillover for removed effects.
    ///
    /// When enabled (default), effects with non-zero [`Effect::tail_samples()`] are
    /// kept alive after [`remove_node()`](Self::remove_node) and fed silence until
    /// their tail decays to zero. When disabled, removed effects are dropped immediately.
    pub fn set_spillover(&mut self, enabled: bool) {
        self.spillover_enabled = enabled;
        if !enabled {
            self.spillover_tails.clear();
        }
    }

    /// Number of active spillover tails from previously removed effects.
    pub fn spillover_count(&self) -> usize {
        self.spillover_tails.len()
    }

    /// Convenience constructor: builds a linear chain Input → E1 → E2 → ... → Output.
    ///
    /// Compiles the graph before returning.
    pub fn linear(
        effects: Vec<Box<dyn EffectWithParams + Send>>,
        sample_rate: f32,
        block_size: usize,
    ) -> Result<Self, GraphError> {
        let mut graph = Self::new(sample_rate, block_size);
        let input = graph.add_input();

        let mut prev = input;
        for effect in effects {
            let node = graph.add_effect(effect);
            graph.connect(prev, node)?;
            prev = node;
        }

        let output = graph.add_output();
        graph.connect(prev, output)?;
        graph.compile()?;

        Ok(graph)
    }

    // --- Internal helpers ---

    fn add_node(&mut self, kind: NodeKind) -> NodeId {
        let id = NodeId(self.next_node_slot);
        self.next_node_slot += 1;
        let node = NodeData::new(id, kind, self.sample_rate);

        let idx = id.0 as usize;
        if idx >= self.nodes.len() {
            self.nodes.resize_with(idx + 1, || None);
        }
        self.nodes[idx] = Some(node);
        id
    }

    fn get_node(&self, id: NodeId) -> Result<&NodeData, GraphError> {
        self.nodes
            .get(id.0 as usize)
            .and_then(|n| n.as_ref())
            .ok_or(GraphError::NodeNotFound(id))
    }

    /// DFS reachability check: can `from` reach `to` via existing edges?
    fn can_reach(&self, from: NodeId, to: NodeId) -> bool {
        let mut visited = vec![false; self.nodes.len()];
        let mut stack = vec![from];

        while let Some(current) = stack.pop() {
            if current == to {
                return true;
            }
            let idx = current.0 as usize;
            if idx >= visited.len() || visited[idx] {
                continue;
            }
            visited[idx] = true;

            if let Some(Some(node)) = self.nodes.get(idx) {
                for edge_id in &node.outgoing {
                    if let Some(edge) = &self.edges[edge_id.0 as usize] {
                        stack.push(edge.to);
                    }
                }
            }
        }
        false
    }

    /// Checks if an edge already exists between two nodes.
    fn has_edge(&self, from: NodeId, to: NodeId) -> bool {
        self.find_edge(from, to).is_some()
    }

    /// Finds the edge ID connecting `from` to `to`, if one exists.
    pub fn find_edge(&self, from: NodeId, to: NodeId) -> Option<EdgeId> {
        let node = self.nodes.get(from.0 as usize)?.as_ref()?;
        for &edge_id in &node.outgoing {
            if let Some(edge) = &self.edges[edge_id.0 as usize]
                && edge.to == to
            {
                return Some(edge_id);
            }
        }
        None
    }

    /// Validates structural constraints for a connection.
    fn validate_connection(&self, from: NodeId, to: NodeId) -> Result<(), GraphError> {
        let from_node = self.get_node(from)?;
        let to_node = self.get_node(to)?;

        // Input nodes cannot have incoming edges.
        if matches!(to_node.kind, NodeKind::Input) {
            return Err(GraphError::InvalidConnection(format!(
                "cannot connect into Input node {from}→{to}"
            )));
        }

        // Output nodes cannot have outgoing edges.
        if matches!(from_node.kind, NodeKind::Output) {
            return Err(GraphError::InvalidConnection(format!(
                "cannot connect from Output node {from}→{to}"
            )));
        }

        // Effect nodes should have at most 1 incoming and 1 outgoing.
        if matches!(to_node.kind, NodeKind::Effect(_)) && !to_node.incoming.is_empty() {
            return Err(GraphError::InvalidConnection(format!(
                "Effect node {to} already has an incoming edge"
            )));
        }
        if matches!(from_node.kind, NodeKind::Effect(_)) && !from_node.outgoing.is_empty() {
            return Err(GraphError::InvalidConnection(format!(
                "Effect node {from} already has an outgoing edge"
            )));
        }

        // Split: exactly 1 incoming.
        if matches!(to_node.kind, NodeKind::Split) && !to_node.incoming.is_empty() {
            return Err(GraphError::InvalidConnection(format!(
                "Split node {to} already has an incoming edge"
            )));
        }

        // Merge: exactly 1 outgoing.
        if matches!(from_node.kind, NodeKind::Merge) && !from_node.outgoing.is_empty() {
            return Err(GraphError::InvalidConnection(format!(
                "Merge node {from} already has an outgoing edge"
            )));
        }

        Ok(())
    }

    /// Disconnects an edge without error checking (caller must verify existence).
    fn disconnect_internal(&mut self, id: EdgeId) {
        let idx = id.0 as usize;
        if let Some(edge) = self.edges[idx].take() {
            // Remove from source's outgoing.
            if let Some(Some(node)) = self.nodes.get_mut(edge.from.0 as usize) {
                node.outgoing.retain(|e| *e != id);
            }
            // Remove from dest's incoming.
            if let Some(Some(node)) = self.nodes.get_mut(edge.to.0 as usize) {
                node.incoming.retain(|e| *e != id);
            }
        }
    }

    /// Counts Input and Output nodes.
    fn count_io_nodes(&self) -> (usize, usize) {
        let mut inputs = 0;
        let mut outputs = 0;
        for node in self.nodes.iter().flatten() {
            match node.kind {
                NodeKind::Input => inputs += 1,
                NodeKind::Output => outputs += 1,
                _ => {}
            }
        }
        (inputs, outputs)
    }

    /// Finds the index of the Input node.
    fn find_node_by_kind_is_input(&self) -> usize {
        self.nodes
            .iter()
            .position(|n| {
                n.as_ref()
                    .is_some_and(|nd| matches!(nd.kind, NodeKind::Input))
            })
            .expect("no Input node found (should be validated before calling)")
    }

    /// Finds the index of the Output node.
    fn find_node_by_kind_is_output(&self) -> usize {
        self.nodes
            .iter()
            .position(|n| {
                n.as_ref()
                    .is_some_and(|nd| matches!(nd.kind, NodeKind::Output))
            })
            .expect("no Output node found (should be validated before calling)")
    }
}

/// Intermediate step type used during schedule emission before buffer assignment.
#[derive(Debug)]
enum RawStep {
    WriteInput {
        vbuf: usize,
    },
    ProcessEffect {
        node_idx: usize,
        input_vbuf: usize,
        output_vbuf: usize,
        /// Virtual buffer for the external sidechain signal, if connected.
        sidechain_vbuf: Option<usize>,
        /// Whether this node runs at control rate (once per block).
        is_control_rate: bool,
    },
    SplitCopy {
        source_vbuf: usize,
        dest_vbufs: Vec<usize>,
    },
    ClearBuffer {
        vbuf: usize,
    },
    AccumulateBuffer {
        source_vbuf: usize,
        dest_vbuf: usize,
        gain: f32,
    },
    ReadOutput {
        vbuf: usize,
    },
    DelayCompensate {
        vbuf: usize,
        delay_line_idx: usize,
    },
    FeedbackDelay {
        vbuf: usize,
        delay_line_idx: usize,
    },
    SampleAndHold {
        vbuf: usize,
        block_len: usize,
    },
    ProcessSubGraph {
        node_idx: usize,
        input_vbuf: usize,
        output_vbuf: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::param_info::{ParamDescriptor, ParameterInfo};
    use core::sync::atomic::{AtomicUsize, Ordering};

    // Simple test effect that multiplies by a factor.
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

    // Effect that reports latency.
    struct LatentGain {
        factor: f32,
        latency: usize,
    }

    impl Effect for LatentGain {
        fn process(&mut self, input: f32) -> f32 {
            input * self.factor
        }
        fn set_sample_rate(&mut self, _sample_rate: f32) {}
        fn reset(&mut self) {}
        fn latency_samples(&self) -> usize {
            self.latency
        }
    }

    impl ParameterInfo for LatentGain {
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

    /// Effect that counts how many blocks it has processed (via stereo path).
    struct CountingEffect {
        counter: &'static AtomicUsize,
    }

    impl Effect for CountingEffect {
        fn process(&mut self, input: f32) -> f32 {
            input
        }
        fn process_block_stereo(
            &mut self,
            left_in: &[f32],
            right_in: &[f32],
            left_out: &mut [f32],
            right_out: &mut [f32],
        ) {
            self.counter.fetch_add(1, Ordering::SeqCst);
            left_out[..left_in.len()].copy_from_slice(left_in);
            right_out[..right_in.len()].copy_from_slice(right_in);
        }
        fn set_sample_rate(&mut self, _: f32) {}
        fn reset(&mut self) {}
    }

    impl ParameterInfo for CountingEffect {
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

    // --- Phase 1: Mutation tests ---

    #[test]
    fn test_add_nodes() {
        let mut graph = ProcessingGraph::new(48000.0, 256);
        let input = graph.add_input();
        let output = graph.add_output();
        let effect = graph.add_effect(Box::new(Gain { factor: 2.0 }));

        assert_eq!(graph.node_count(), 3);
        assert!(graph.effect_ref(effect).is_some());
        assert!(graph.effect_ref(input).is_none()); // not an effect node
        assert!(graph.effect_ref(output).is_none());
    }

    #[test]
    fn test_connect_and_edge_count() {
        let mut graph = ProcessingGraph::new(48000.0, 256);
        let input = graph.add_input();
        let effect = graph.add_effect(Box::new(Gain { factor: 2.0 }));
        let output = graph.add_output();

        graph.connect(input, effect).unwrap();
        graph.connect(effect, output).unwrap();

        assert_eq!(graph.edge_count(), 2);
    }

    #[test]
    fn test_cycle_detection_direct() {
        let mut graph = ProcessingGraph::new(48000.0, 256);
        let a = graph.add_split();
        let b = graph.add_merge();

        graph.connect(a, b).unwrap();
        let result = graph.connect(b, a);
        assert!(matches!(result, Err(GraphError::CycleDetected)));
    }

    #[test]
    fn test_cycle_detection_indirect() {
        let mut graph = ProcessingGraph::new(48000.0, 256);
        let a = graph.add_split();
        let b = graph.add_effect(Box::new(Gain { factor: 1.0 }));
        let c = graph.add_merge();

        graph.connect(a, b).unwrap();
        graph.connect(b, c).unwrap();
        let result = graph.connect(c, a);
        assert!(matches!(result, Err(GraphError::CycleDetected)));
    }

    #[test]
    fn test_duplicate_edge_rejected() {
        let mut graph = ProcessingGraph::new(48000.0, 256);
        let a = graph.add_split();
        let b = graph.add_merge();

        graph.connect(a, b).unwrap();
        let result = graph.connect(a, b);
        assert!(matches!(result, Err(GraphError::DuplicateEdge(_, _))));
    }

    #[test]
    fn test_connect_into_input_rejected() {
        let mut graph = ProcessingGraph::new(48000.0, 256);
        let input = graph.add_input();
        let effect = graph.add_effect(Box::new(Gain { factor: 1.0 }));

        let result = graph.connect(effect, input);
        assert!(matches!(result, Err(GraphError::InvalidConnection(_))));
    }

    #[test]
    fn test_connect_from_output_rejected() {
        let mut graph = ProcessingGraph::new(48000.0, 256);
        let output = graph.add_output();
        let effect = graph.add_effect(Box::new(Gain { factor: 1.0 }));

        let result = graph.connect(output, effect);
        assert!(matches!(result, Err(GraphError::InvalidConnection(_))));
    }

    #[test]
    fn test_remove_node() {
        let mut graph = ProcessingGraph::new(48000.0, 256);
        let input = graph.add_input();
        let effect = graph.add_effect(Box::new(Gain { factor: 2.0 }));
        let output = graph.add_output();

        graph.connect(input, effect).unwrap();
        graph.connect(effect, output).unwrap();

        assert_eq!(graph.node_count(), 3);
        assert_eq!(graph.edge_count(), 2);

        graph.remove_node(effect).unwrap();

        assert_eq!(graph.node_count(), 2);
        assert_eq!(graph.edge_count(), 0);
    }

    #[test]
    fn test_remove_nonexistent_node() {
        let mut graph = ProcessingGraph::new(48000.0, 256);
        let result = graph.remove_node(NodeId(999));
        assert!(matches!(result, Err(GraphError::NodeNotFound(_))));
    }

    #[test]
    fn test_disconnect() {
        let mut graph = ProcessingGraph::new(48000.0, 256);
        let input = graph.add_input();
        let effect = graph.add_effect(Box::new(Gain { factor: 2.0 }));

        let edge = graph.connect(input, effect).unwrap();
        assert_eq!(graph.edge_count(), 1);

        graph.disconnect(edge).unwrap();
        assert_eq!(graph.edge_count(), 0);
    }

    #[test]
    fn test_disconnect_nonexistent() {
        let mut graph = ProcessingGraph::new(48000.0, 256);
        let result = graph.disconnect(EdgeId(999));
        assert!(matches!(result, Err(GraphError::EdgeNotFound(_))));
    }

    #[test]
    fn test_bypass() {
        let mut graph = ProcessingGraph::new(48000.0, 256);
        let effect = graph.add_effect(Box::new(Gain { factor: 2.0 }));

        assert!(!graph.is_bypassed(effect));
        graph.set_bypass(effect, true);
        assert!(graph.is_bypassed(effect));
        graph.set_bypass(effect, false);
        assert!(!graph.is_bypassed(effect));
    }

    #[test]
    fn test_split_and_merge() {
        let mut graph = ProcessingGraph::new(48000.0, 256);
        let input = graph.add_input();
        let split = graph.add_split();
        let a = graph.add_effect(Box::new(Gain { factor: 2.0 }));
        let b = graph.add_effect(Box::new(Gain { factor: 3.0 }));
        let merge = graph.add_merge();
        let output = graph.add_output();

        graph.connect(input, split).unwrap();
        graph.connect(split, a).unwrap();
        graph.connect(split, b).unwrap();
        graph.connect(a, merge).unwrap();
        graph.connect(b, merge).unwrap();
        graph.connect(merge, output).unwrap();

        assert_eq!(graph.node_count(), 6);
        assert_eq!(graph.edge_count(), 6);
    }

    #[test]
    fn test_effect_node_rejects_second_incoming() {
        let mut graph2 = ProcessingGraph::new(48000.0, 256);
        let s1 = graph2.add_split();
        let s2 = graph2.add_split();
        let eff = graph2.add_effect(Box::new(Gain { factor: 1.0 }));

        graph2.connect(s1, eff).unwrap();
        let result = graph2.connect(s2, eff);
        assert!(matches!(result, Err(GraphError::InvalidConnection(_))));
    }

    // --- Phase 2: Compilation tests ---

    #[test]
    fn test_compile_empty_graph_fails() {
        let mut graph = ProcessingGraph::new(48000.0, 256);
        let result = graph.compile();
        assert!(matches!(result, Err(GraphError::InvalidInputCount(0))));
    }

    #[test]
    fn test_compile_missing_output_fails() {
        let mut graph = ProcessingGraph::new(48000.0, 256);
        graph.add_input();
        let result = graph.compile();
        assert!(matches!(result, Err(GraphError::InvalidOutputCount(0))));
    }

    #[test]
    fn test_compile_direct_passthrough() {
        let mut graph = ProcessingGraph::new(48000.0, 256);
        let input = graph.add_input();
        let output = graph.add_output();
        graph.connect(input, output).unwrap();

        let schedule = graph.compile().unwrap();
        assert_eq!(schedule.buffer_count(), 1);
        assert!(schedule.step_count() >= 2); // WriteInput + ReadOutput
    }

    #[test]
    fn test_compile_single_effect() {
        let mut graph = ProcessingGraph::new(48000.0, 256);
        let input = graph.add_input();
        let effect = graph.add_effect(Box::new(Gain { factor: 2.0 }));
        let output = graph.add_output();

        graph.connect(input, effect).unwrap();
        graph.connect(effect, output).unwrap();

        let schedule = graph.compile().unwrap();
        assert!(schedule.buffer_count() >= 1);
    }

    #[test]
    fn test_compile_linear_chain_buffer_efficiency() {
        // A 20-node linear chain should use exactly 2 buffers (ping-pong).
        let effects: Vec<Box<dyn EffectWithParams + Send>> = (0..20)
            .map(|_| Box::new(Gain { factor: 1.0 }) as Box<dyn EffectWithParams + Send>)
            .collect();

        let graph = ProcessingGraph::linear(effects, 48000.0, 256).unwrap();
        let schedule = graph.compiled().unwrap();
        assert_eq!(schedule.buffer_count(), 2);
    }

    #[test]
    fn test_compile_diamond() {
        let mut graph = ProcessingGraph::new(48000.0, 256);
        let input = graph.add_input();
        let split = graph.add_split();
        let a = graph.add_effect(Box::new(Gain { factor: 2.0 }));
        let b = graph.add_effect(Box::new(Gain { factor: 3.0 }));
        let merge = graph.add_merge();
        let output = graph.add_output();

        graph.connect(input, split).unwrap();
        graph.connect(split, a).unwrap();
        graph.connect(split, b).unwrap();
        graph.connect(a, merge).unwrap();
        graph.connect(b, merge).unwrap();
        graph.connect(merge, output).unwrap();

        let schedule = graph.compile().unwrap();
        assert!(schedule.buffer_count() <= 5);
    }

    // --- Phase 3: Execution tests ---

    #[test]
    fn test_process_passthrough() {
        let mut graph = ProcessingGraph::new(48000.0, 4);
        let input = graph.add_input();
        let output = graph.add_output();
        graph.connect(input, output).unwrap();
        graph.compile().unwrap();

        let left_in = [1.0, 2.0, 3.0, 4.0];
        let right_in = [0.5, 1.0, 1.5, 2.0];
        let mut left_out = [0.0; 4];
        let mut right_out = [0.0; 4];

        graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);

        assert_eq!(left_out, left_in);
        assert_eq!(right_out, right_in);
    }

    #[test]
    fn test_process_single_effect() {
        let effects: Vec<Box<dyn EffectWithParams + Send>> = vec![Box::new(Gain { factor: 2.0 })];
        let mut graph = ProcessingGraph::linear(effects, 48000.0, 4).unwrap();

        let left_in = [1.0, 2.0, 3.0, 4.0];
        let right_in = [0.5, 1.0, 1.5, 2.0];
        let mut left_out = [0.0; 4];
        let mut right_out = [0.0; 4];

        graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);

        assert_eq!(left_out, [2.0, 4.0, 6.0, 8.0]);
        assert_eq!(right_out, [1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn test_process_chain() {
        let effects: Vec<Box<dyn EffectWithParams + Send>> = vec![
            Box::new(Gain { factor: 2.0 }),
            Box::new(Gain { factor: 3.0 }),
        ];
        let mut graph = ProcessingGraph::linear(effects, 48000.0, 4).unwrap();

        let left_in = [1.0, 0.5, 0.25, 0.125];
        let right_in = [1.0, 0.5, 0.25, 0.125];
        let mut left_out = [0.0; 4];
        let mut right_out = [0.0; 4];

        graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);

        assert_eq!(left_out, [6.0, 3.0, 1.5, 0.75]);
    }

    #[test]
    fn test_process_diamond_sums() {
        let mut graph = ProcessingGraph::new(48000.0, 4);
        let input = graph.add_input();
        let split = graph.add_split();
        let a = graph.add_effect(Box::new(Gain { factor: 2.0 }));
        let b = graph.add_effect(Box::new(Gain { factor: 3.0 }));
        let merge = graph.add_merge();
        let output = graph.add_output();

        graph.connect(input, split).unwrap();
        graph.connect(split, a).unwrap();
        graph.connect(split, b).unwrap();
        graph.connect(a, merge).unwrap();
        graph.connect(b, merge).unwrap();
        graph.connect(merge, output).unwrap();
        graph.compile().unwrap();

        let left_in = [1.0, 1.0, 1.0, 1.0];
        let right_in = [1.0, 1.0, 1.0, 1.0];
        let mut left_out = [0.0; 4];
        let mut right_out = [0.0; 4];

        graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);

        // Merge normalizes by path count: (2.0 + 3.0) / 2 = 2.5
        for &s in &left_out {
            assert!((s - 2.5).abs() < 1e-6, "expected 2.5, got {s}");
        }
    }

    #[test]
    fn test_process_20_effect_chain() {
        let effects: Vec<Box<dyn EffectWithParams + Send>> = (0..20)
            .map(|_| Box::new(Gain { factor: 1.0 }) as Box<dyn EffectWithParams + Send>)
            .collect();
        let mut graph = ProcessingGraph::linear(effects, 48000.0, 4).unwrap();

        let left_in = [0.5; 4];
        let right_in = [0.25; 4];
        let mut left_out = [0.0; 4];
        let mut right_out = [0.0; 4];

        graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);

        for &s in &left_out {
            assert!((s - 0.5).abs() < 1e-6);
        }
        for &s in &right_out {
            assert!((s - 0.25).abs() < 1e-6);
        }
    }

    #[test]
    fn test_linear_convenience() {
        let effects: Vec<Box<dyn EffectWithParams + Send>> = vec![Box::new(Gain { factor: 0.5 })];
        let graph = ProcessingGraph::linear(effects, 48000.0, 256).unwrap();
        assert_eq!(graph.node_count(), 3); // Input + Effect + Output
        assert_eq!(graph.edge_count(), 2);
        assert!(graph.compiled().is_some());
    }

    #[test]
    fn test_set_sample_rate() {
        let mut graph = ProcessingGraph::new(44100.0, 256);
        let _input = graph.add_input();
        let _effect = graph.add_effect(Box::new(Gain { factor: 1.0 }));

        graph.set_sample_rate(96000.0);
        assert_eq!(graph.sample_rate(), 96000.0);
    }

    #[test]
    fn test_schedule_swap_crossfade() {
        // Build initial graph with gain=1.0.
        let effects: Vec<Box<dyn EffectWithParams + Send>> = vec![Box::new(Gain { factor: 1.0 })];
        let mut graph = ProcessingGraph::linear(effects, 48000.0, 256).unwrap();

        // Process a block to establish baseline.
        let left_in = vec![1.0; 256];
        let right_in = vec![1.0; 256];
        let mut left_out = vec![0.0; 256];
        let mut right_out = vec![0.0; 256];

        graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);

        // Now recompile — this should trigger crossfade.
        graph.compile().unwrap();

        // Process during crossfade.
        graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);

        // Verify no discontinuities: since both schedules are identical,
        // output should remain close to 1.0 throughout.
        for &s in &left_out {
            assert!(
                (s - 1.0).abs() < 0.1,
                "crossfade discontinuity: expected ~1.0, got {s}"
            );
        }
    }

    #[test]
    fn test_latency_reporting() {
        let mut graph = ProcessingGraph::new(48000.0, 256);
        let input = graph.add_input();
        let effect = graph.add_effect(Box::new(LatentGain {
            factor: 1.0,
            latency: 128,
        }));
        let output = graph.add_output();

        graph.connect(input, effect).unwrap();
        graph.connect(effect, output).unwrap();
        graph.compile().unwrap();

        assert_eq!(graph.latency_samples(), 128);
    }

    #[test]
    fn test_effect_mut_access() {
        let mut graph = ProcessingGraph::new(48000.0, 256);
        let effect_id = graph.add_effect(Box::new(Gain { factor: 2.0 }));

        let effect = graph.effect_mut(effect_id).unwrap();
        let output = effect.process(1.0);
        assert_eq!(output, 2.0);
    }

    // --- New correctness tests ---

    #[test]
    fn test_bypass_outputs_dry_signal() {
        // Gain(2.0) with bypass should output dry (input), not silence.
        let effects: Vec<Box<dyn EffectWithParams + Send>> = vec![Box::new(Gain { factor: 2.0 })];
        let mut graph = ProcessingGraph::linear(effects, 48000.0, 64).unwrap();

        // Get the effect node ID (it's the second node: input=0, effect=1, output=2).
        let effect_id = NodeId(1);

        // Bypass the effect and snap the fade to 0.0 immediately.
        graph.set_bypass(effect_id, true);
        if let Some(Some(node)) = graph.nodes.get_mut(1) {
            node.bypass_fade.snap_to_target();
        }

        let left_in = vec![0.5; 64];
        let right_in = vec![0.25; 64];
        let mut left_out = vec![0.0; 64];
        let mut right_out = vec![0.0; 64];

        graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);

        // Should output dry signal (0.5), NOT silence (0.0) or wet (1.0).
        for (i, &s) in left_out.iter().enumerate() {
            assert!(
                (s - 0.5).abs() < 1e-6,
                "bypass left[{i}]: expected 0.5 (dry), got {s}"
            );
        }
        for (i, &s) in right_out.iter().enumerate() {
            assert!(
                (s - 0.25).abs() < 1e-6,
                "bypass right[{i}]: expected 0.25 (dry), got {s}"
            );
        }
    }

    #[test]
    fn test_bypass_crossfade_smooth() {
        // Toggle bypass mid-stream. Verify no discontinuities (all values bounded).
        let effects: Vec<Box<dyn EffectWithParams + Send>> = vec![Box::new(Gain { factor: 2.0 })];
        let mut graph = ProcessingGraph::linear(effects, 48000.0, 64).unwrap();

        let effect_id = NodeId(1);
        let left_in = vec![1.0; 64];
        let right_in = vec![1.0; 64];
        let mut left_out = vec![0.0; 64];
        let mut right_out = vec![0.0; 64];

        // Process a block normally (output = 2.0).
        graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);

        // Toggle bypass — fade starts.
        graph.set_bypass(effect_id, true);

        // Process multiple blocks during crossfade.
        for _ in 0..20 {
            graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);

            // All values should be between dry (1.0) and wet (2.0).
            for &s in &left_out {
                assert!(
                    (0.99..=2.01).contains(&s),
                    "bypass crossfade out of range: {s}"
                );
            }
        }
    }

    #[test]
    fn test_settled_bypass_skips_processing() {
        // When bypass_fade is settled at 0.0, effect should NOT be called.
        static COUNTER: AtomicUsize = AtomicUsize::new(0);

        let effects: Vec<Box<dyn EffectWithParams + Send>> =
            vec![Box::new(CountingEffect { counter: &COUNTER })];
        let mut graph = ProcessingGraph::linear(effects, 48000.0, 64).unwrap();

        let effect_id = NodeId(1);
        graph.set_bypass(effect_id, true);

        // Snap bypass fade to 0.0 immediately (simulates settled state).
        if let Some(Some(node)) = graph.nodes.get_mut(1) {
            node.bypass_fade.snap_to_target();
        }

        COUNTER.store(0, Ordering::SeqCst);

        let left_in = vec![0.5; 64];
        let right_in = vec![0.25; 64];
        let mut left_out = vec![0.0; 64];
        let mut right_out = vec![0.0; 64];

        graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);

        assert_eq!(
            COUNTER.load(Ordering::SeqCst),
            0,
            "settled bypass should skip effect processing"
        );

        for (i, &s) in left_out.iter().enumerate() {
            assert!(
                (s - 0.5).abs() < 1e-6,
                "settled bypass left[{i}]: expected 0.5, got {s}"
            );
        }
        for (i, &s) in right_out.iter().enumerate() {
            assert!(
                (s - 0.25).abs() < 1e-6,
                "settled bypass right[{i}]: expected 0.25, got {s}"
            );
        }
    }

    #[test]
    fn test_schedule_swap_single_execution() {
        // Verify that during crossfade, effects are executed only once per block.
        static COUNTER: AtomicUsize = AtomicUsize::new(0);

        let effects: Vec<Box<dyn EffectWithParams + Send>> =
            vec![Box::new(CountingEffect { counter: &COUNTER })];
        let mut graph = ProcessingGraph::linear(effects, 48000.0, 64).unwrap();

        let left_in = vec![1.0; 64];
        let right_in = vec![1.0; 64];
        let mut left_out = vec![0.0; 64];
        let mut right_out = vec![0.0; 64];

        // Baseline block.
        COUNTER.store(0, Ordering::SeqCst);
        graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);
        let baseline = COUNTER.load(Ordering::SeqCst);
        assert!(baseline > 0, "effect should have processed");

        // Recompile to trigger crossfade.
        graph.compile().unwrap();

        // Process during crossfade — should still only call effect once.
        COUNTER.store(0, Ordering::SeqCst);
        graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);
        let during_crossfade = COUNTER.load(Ordering::SeqCst);
        assert_eq!(
            during_crossfade, baseline,
            "effect should be called exactly once during crossfade, \
             got {during_crossfade} (baseline {baseline})"
        );
    }

    #[test]
    fn test_delay_compensation_persists() {
        // Build diamond with latency mismatch. Process multiple blocks.
        // Delay compensation should work across blocks (state persists).
        let mut graph = ProcessingGraph::new(48000.0, 4);
        let input = graph.add_input();
        let split = graph.add_split();
        let a = graph.add_effect(Box::new(LatentGain {
            factor: 1.0,
            latency: 4,
        }));
        let b = graph.add_effect(Box::new(Gain { factor: 1.0 })); // 0 latency
        let merge = graph.add_merge();
        let output = graph.add_output();

        graph.connect(input, split).unwrap();
        graph.connect(split, a).unwrap();
        graph.connect(split, b).unwrap();
        graph.connect(a, merge).unwrap();
        graph.connect(b, merge).unwrap();
        graph.connect(merge, output).unwrap();
        graph.compile().unwrap();

        assert_eq!(graph.latency_samples(), 4);

        // Process multiple blocks — delay line should accumulate state.
        let mut left_out = [0.0; 4];
        let mut right_out = [0.0; 4];

        // Block 1: impulse
        let left_in = [1.0, 0.0, 0.0, 0.0];
        graph.process_block(&left_in, &left_in, &mut left_out, &mut right_out);

        // Block 2: silence — but delay should produce the delayed impulse.
        let silence = [0.0; 4];
        graph.process_block(&silence, &silence, &mut left_out, &mut right_out);

        // The delayed path (b) should have output the impulse shifted by 4 samples.
        // At least some samples in block 2 should be non-zero from the delayed signal.
        let block2_sum: f32 = left_out.iter().sum();
        assert!(
            block2_sum.abs() > 0.01,
            "delay compensation should produce delayed signal in block 2, got sum={block2_sum}"
        );
    }

    #[test]
    fn test_split_max_fanout() {
        // Split to MAX_SPLIT_TARGETS destinations, verify all receive signal.
        let mut graph = ProcessingGraph::new(48000.0, 4);
        let input = graph.add_input();
        let split = graph.add_split();
        graph.connect(input, split).unwrap();

        let merge = graph.add_merge();
        let output = graph.add_output();

        for _ in 0..MAX_SPLIT_TARGETS {
            let effect = graph.add_effect(Box::new(Gain { factor: 1.0 }));
            graph.connect(split, effect).unwrap();
            graph.connect(effect, merge).unwrap();
        }
        graph.connect(merge, output).unwrap();
        graph.compile().unwrap();

        let left_in = [1.0; 4];
        let right_in = [1.0; 4];
        let mut left_out = [0.0; 4];
        let mut right_out = [0.0; 4];

        graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);

        // All 8 paths at gain 1.0, normalized: 8 * 1.0 / 8 = 1.0
        for &s in &left_out {
            assert!(
                (s - 1.0).abs() < 1e-6,
                "expected 1.0 (unity after normalization), got {s}",
            );
        }
    }

    // --- Latency compensation regression tests ---

    /// Two sequential merges: Input→Split→[A(lat=10), B(lat=0)]→Merge→Split→[C(lat=5), D(lat=0)]→Merge→Output.
    /// Each merge must get independent compensation — the old code matched the
    /// wrong ClearBuffer for the second merge.
    #[test]
    fn test_latency_comp_multi_merge() {
        let mut graph = ProcessingGraph::new(48000.0, 64);
        let input = graph.add_input();
        let split1 = graph.add_split();
        let a = graph.add_effect(Box::new(LatentGain {
            factor: 1.0,
            latency: 10,
        }));
        let b = graph.add_effect(Box::new(Gain { factor: 1.0 }));
        let merge1 = graph.add_merge();
        let split2 = graph.add_split();
        let c = graph.add_effect(Box::new(LatentGain {
            factor: 1.0,
            latency: 5,
        }));
        let d = graph.add_effect(Box::new(Gain { factor: 1.0 }));
        let merge2 = graph.add_merge();
        let output = graph.add_output();

        graph.connect(input, split1).unwrap();
        graph.connect(split1, a).unwrap();
        graph.connect(split1, b).unwrap();
        graph.connect(a, merge1).unwrap();
        graph.connect(b, merge1).unwrap();
        graph.connect(merge1, split2).unwrap();
        graph.connect(split2, c).unwrap();
        graph.connect(split2, d).unwrap();
        graph.connect(c, merge2).unwrap();
        graph.connect(d, merge2).unwrap();
        graph.connect(merge2, output).unwrap();

        let schedule = graph.compile().unwrap();

        // Total latency: longest path is Input→A(10)→Merge→C(5) = 15
        assert_eq!(schedule.total_latency(), 15);

        // Two delay lines: one for B at merge1 (10 samples), one for D at merge2 (5 samples).
        assert_eq!(schedule.delay_line_count(), 2);

        let delays = &schedule.delay_sample_counts;
        assert!(
            delays.contains(&10) && delays.contains(&5),
            "expected delays [10, 5], got {delays:?}"
        );
    }

    /// 3 paths with different latencies into one merge.
    /// Verifies all shorter paths get correct delays.
    #[test]
    fn test_latency_comp_many_paths() {
        let mut graph = ProcessingGraph::new(48000.0, 64);
        let input = graph.add_input();
        let split = graph.add_split();
        let a = graph.add_effect(Box::new(LatentGain {
            factor: 1.0,
            latency: 20,
        }));
        let b = graph.add_effect(Box::new(LatentGain {
            factor: 1.0,
            latency: 8,
        }));
        let c = graph.add_effect(Box::new(Gain { factor: 1.0 })); // 0 latency
        let merge = graph.add_merge();
        let output = graph.add_output();

        graph.connect(input, split).unwrap();
        graph.connect(split, a).unwrap();
        graph.connect(split, b).unwrap();
        graph.connect(split, c).unwrap();
        graph.connect(a, merge).unwrap();
        graph.connect(b, merge).unwrap();
        graph.connect(c, merge).unwrap();
        graph.connect(merge, output).unwrap();

        let schedule = graph.compile().unwrap();

        assert_eq!(schedule.total_latency(), 20);

        // Two delay lines: B needs 20-8=12, C needs 20-0=20. A needs 0.
        assert_eq!(schedule.delay_line_count(), 2);

        let delays = &schedule.delay_sample_counts;
        assert!(
            delays.contains(&12) && delays.contains(&20),
            "expected delays [12, 20], got {delays:?}"
        );
    }

    /// Topology large enough that liveness analysis reuses physical buffer slots.
    /// Verifies delays target the correct buffer after aliasing.
    #[test]
    fn test_latency_comp_buffer_reuse() {
        // Linear chain of 10 effects, then split→[A(lat=16), B(lat=0)]→merge→output.
        // The 10-effect chain forces extensive buffer reuse (ping-pong),
        // exercising the aliasing path.
        let mut graph = ProcessingGraph::new(48000.0, 64);
        let input = graph.add_input();

        let mut prev = input;
        for _ in 0..10 {
            let e = graph.add_effect(Box::new(Gain { factor: 1.0 }));
            graph.connect(prev, e).unwrap();
            prev = e;
        }

        let split = graph.add_split();
        graph.connect(prev, split).unwrap();
        let a = graph.add_effect(Box::new(LatentGain {
            factor: 1.0,
            latency: 16,
        }));
        let b = graph.add_effect(Box::new(Gain { factor: 1.0 }));
        let merge = graph.add_merge();
        let output = graph.add_output();

        graph.connect(split, a).unwrap();
        graph.connect(split, b).unwrap();
        graph.connect(a, merge).unwrap();
        graph.connect(b, merge).unwrap();
        graph.connect(merge, output).unwrap();

        let schedule = graph.compile().unwrap();

        assert_eq!(schedule.total_latency(), 16);
        assert_eq!(schedule.delay_line_count(), 1);
        assert_eq!(schedule.delay_sample_counts[0], 16);

        // Verify the schedule executes correctly by processing audio.
        let left_in = vec![1.0; 64];
        let right_in = vec![1.0; 64];
        let mut left_out = vec![0.0; 64];
        let mut right_out = vec![0.0; 64];

        graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);

        // Should produce non-zero output (both paths contribute).
        let sum: f32 = left_out.iter().sum();
        assert!(
            sum.abs() > 0.01,
            "expected non-zero output after buffer-reuse topology, got sum={sum}"
        );
    }

    // --- Phase 2 (observability): Tap, peak, and f64 accumulation tests ---

    #[test]
    fn tap_preserves_node_output() {
        // Build Input → Gain(2.0) → Output. Tap the effect node.
        let effects: Vec<Box<dyn EffectWithParams + Send>> = vec![Box::new(Gain { factor: 2.0 })];
        let mut graph = ProcessingGraph::linear(effects, 48000.0, 4).unwrap();

        // The effect node is NodeId(1) in a linear graph (input=0, effect=1, output=2).
        let effect_id = NodeId(1);
        graph.set_tap(effect_id, true);

        let left_in = [0.5, 0.5, 0.5, 0.5];
        let right_in = [0.25, 0.25, 0.25, 0.25];
        let mut left_out = [0.0; 4];
        let mut right_out = [0.0; 4];

        graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);

        let (tap_l, tap_r) = graph.read_tap(effect_id).expect("tap should be active");

        // Effect multiplies by 2.0 — tap should capture the wet output.
        for (i, &s) in tap_l.iter().enumerate() {
            assert!(
                (s - 1.0).abs() < 1e-6,
                "tap left[{i}]: expected 1.0 (0.5 * 2.0), got {s}"
            );
        }
        for (i, &s) in tap_r.iter().enumerate() {
            assert!(
                (s - 0.5).abs() < 1e-6,
                "tap right[{i}]: expected 0.5 (0.25 * 2.0), got {s}"
            );
        }

        // Disabling tap should return None.
        graph.set_tap(effect_id, false);
        assert!(graph.read_tap(effect_id).is_none());
    }

    #[test]
    fn peak_tracking_reports_correct_levels() {
        // Gain(1.0) with a known DC signal — peak_out should equal that value.
        let effects: Vec<Box<dyn EffectWithParams + Send>> = vec![Box::new(Gain { factor: 1.0 })];
        let mut graph = ProcessingGraph::linear(effects, 48000.0, 4).unwrap();

        let effect_id = NodeId(1);

        let amplitude = 0.75_f32;
        let left_in = [amplitude; 4];
        let right_in = [amplitude * 0.5; 4];
        let mut left_out = [0.0; 4];
        let mut right_out = [0.0; 4];

        graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);

        let (peak_l, peak_r) = graph.node_peak(effect_id).expect("node should exist");

        assert!(
            (peak_l - amplitude).abs() < 1e-6,
            "peak left: expected {amplitude}, got {peak_l}"
        );
        assert!(
            (peak_r - amplitude * 0.5).abs() < 1e-6,
            "peak right: expected {}, got {peak_r}",
            amplitude * 0.5
        );

        // Non-existent node returns None.
        assert!(graph.node_peak(NodeId(999)).is_none());
    }

    #[test]
    fn f64_accumulate_preserves_precision() {
        // Diamond with 4 equal gain paths into one merge.
        // f32 accumulation would suffer from precision loss after repeated
        // addition; f64 intermediates keep the result accurate.
        let mut graph = ProcessingGraph::new(48000.0, 4);
        let input = graph.add_input();
        let split = graph.add_split();
        let merge = graph.add_merge();
        let output = graph.add_output();

        // 4 parallel unity-gain paths.
        for _ in 0..4 {
            let e = graph.add_effect(Box::new(Gain { factor: 1.0 }));
            graph.connect(split, e).unwrap();
            graph.connect(e, merge).unwrap();
        }
        graph.connect(input, split).unwrap();
        graph.connect(merge, output).unwrap();
        graph.compile().unwrap();

        // Use a value that is not exactly representable in f32 to stress precision.
        let val = 1.0_f32 / 3.0_f32;
        let left_in = [val; 4];
        let right_in = [val; 4];
        let mut left_out = [0.0; 4];
        let mut right_out = [0.0; 4];

        graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);

        // Merge normalizes by 4 paths: 4 * val * (1/4) = val.
        for (i, &s) in left_out.iter().enumerate() {
            assert!(
                (s - val).abs() < 1e-6,
                "f64 accumulate left[{i}]: expected {val}, got {s}"
            );
        }
    }

    // --- Spillover tail tests ---

    /// Test effect with a delay-line feedback loop so it actually has a tail.
    struct TailTestEffect {
        /// Reported tail length in samples.
        tail: usize,
        /// Simple feedback delay buffer.
        buffer: [f32; 256],
        write_pos: usize,
    }

    impl Effect for TailTestEffect {
        fn process(&mut self, input: f32) -> f32 {
            let delayed = self.buffer[self.write_pos];
            self.buffer[self.write_pos] = input + delayed * 0.5;
            self.write_pos = (self.write_pos + 1) % 256;
            delayed
        }

        fn tail_samples(&self) -> usize {
            self.tail
        }

        fn set_sample_rate(&mut self, _: f32) {}

        fn reset(&mut self) {
            self.buffer.fill(0.0);
            self.write_pos = 0;
        }
    }

    impl ParameterInfo for TailTestEffect {
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

    #[test]
    fn spillover_preserves_tail() {
        // Input → TailTestEffect → Output. Process noise to fill the delay,
        // then remove the effect and verify output remains non-zero (tail rings out).
        let tail_len = 512usize;
        let block_size = 64usize;

        let mut graph = ProcessingGraph::new(48000.0, block_size);
        let input = graph.add_input();
        let effect_id = graph.add_effect(Box::new(TailTestEffect {
            tail: tail_len,
            buffer: [0.0; 256],
            write_pos: 0,
        }));
        let output = graph.add_output();

        graph.connect(input, effect_id).unwrap();
        graph.connect(effect_id, output).unwrap();
        graph.compile().unwrap();

        // Fill the effect with signal.
        let signal = vec![0.5_f32; block_size];
        let mut left_out = vec![0.0_f32; block_size];
        let mut right_out = vec![0.0_f32; block_size];
        for _ in 0..4 {
            graph.process_block(&signal, &signal, &mut left_out, &mut right_out);
        }

        // Remove the effect — should move to spillover.
        graph.remove_node(effect_id).unwrap();
        assert_eq!(
            graph.spillover_count(),
            1,
            "spillover tail should be active"
        );

        // Reconnect graph without the effect and recompile.
        graph.connect(input, output).unwrap();
        graph.compile().unwrap();

        // Process silence — spillover should produce non-zero output.
        let silence = vec![0.0_f32; block_size];
        graph.process_block(&silence, &silence, &mut left_out, &mut right_out);

        let sum: f32 = left_out.iter().map(|s| s.abs()).sum();
        assert!(
            sum > 0.0,
            "spillover tail should produce non-zero output, got sum={sum}"
        );
    }

    #[test]
    fn no_spillover_for_zero_tail_effect() {
        // Gain (tail = 0) should not create a spillover tail.
        let mut graph = ProcessingGraph::new(48000.0, 64);
        let input = graph.add_input();
        let dist_id = graph.add_effect(Box::new(Gain { factor: 2.0 }));
        let output = graph.add_output();

        graph.connect(input, dist_id).unwrap();
        graph.connect(dist_id, output).unwrap();
        graph.compile().unwrap();

        graph.remove_node(dist_id).unwrap();

        assert_eq!(
            graph.spillover_count(),
            0,
            "zero-tail effect should not create spillover"
        );
    }

    #[test]
    fn spillover_disabled_drops_immediately() {
        // With spillover off, even tailed effects are dropped immediately.
        let tail_len = 4800usize;
        let block_size = 64usize;

        let mut graph = ProcessingGraph::new(48000.0, block_size);
        let input = graph.add_input();
        let effect_id = graph.add_effect(Box::new(TailTestEffect {
            tail: tail_len,
            buffer: [0.0; 256],
            write_pos: 0,
        }));
        let output = graph.add_output();

        graph.connect(input, effect_id).unwrap();
        graph.connect(effect_id, output).unwrap();
        graph.compile().unwrap();

        // Disable spillover before removing.
        graph.set_spillover(false);

        graph.remove_node(effect_id).unwrap();

        assert_eq!(
            graph.spillover_count(),
            0,
            "spillover disabled: effect should be dropped immediately"
        );
    }

    // --- Sidechain tests ---

    /// An effect that counts sidechain processing invocations via a shared atomic.
    ///
    /// When `process_block_stereo_with_sidechain` is called, increments `sc_calls`.
    /// The standard `process_block_stereo` path leaves the counter unchanged.
    struct SidechainProbe {
        pub sc_calls: &'static AtomicUsize,
    }

    impl Effect for SidechainProbe {
        fn process(&mut self, input: f32) -> f32 {
            input
        }
        fn process_block_stereo_with_sidechain(
            &mut self,
            left_in: &[f32],
            right_in: &[f32],
            left_out: &mut [f32],
            right_out: &mut [f32],
            _sc_left: &[f32],
            _sc_right: &[f32],
        ) {
            self.sc_calls.fetch_add(1, Ordering::SeqCst);
            left_out[..left_in.len()].copy_from_slice(left_in);
            right_out[..right_in.len()].copy_from_slice(right_in);
        }
        fn set_sample_rate(&mut self, _: f32) {}
        fn reset(&mut self) {}
    }

    impl ParameterInfo for SidechainProbe {
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

    /// `connect_sidechain` must succeed when both nodes exist and must be
    /// reflected in the compiled schedule without error.
    #[test]
    fn connect_sidechain_compiles() {
        static SC_CALLS: AtomicUsize = AtomicUsize::new(0);
        let mut graph = ProcessingGraph::new(48000.0, 4);
        let input = graph.add_input();
        let effect = graph.add_effect(Box::new(SidechainProbe {
            sc_calls: &SC_CALLS,
        }));
        let output = graph.add_output();

        graph.connect(input, effect).unwrap();
        graph.connect(effect, output).unwrap();
        // Use the input node itself as sidechain source — valid for compilation.
        graph.connect_sidechain(input, effect).unwrap();

        let schedule = graph.compile().unwrap();
        assert!(schedule.step_count() >= 2);
    }

    /// The sidechain routing must invoke the sidechain processing path on the effect.
    ///
    /// The `SidechainProbe` increments a shared atomic counter in its sidechain
    /// override. After `process_block`, the counter must have advanced.
    #[test]
    fn sidechain_audio_reaches_effect() {
        static SC_CALLS: AtomicUsize = AtomicUsize::new(0);

        let block_size = 4;
        let mut graph = ProcessingGraph::new(48000.0, block_size);

        let input = graph.add_input();
        let probe_id = graph.add_effect(Box::new(SidechainProbe {
            sc_calls: &SC_CALLS,
        }));
        let output = graph.add_output();

        graph.connect(input, probe_id).unwrap();
        graph.connect(probe_id, output).unwrap();
        graph.connect_sidechain(input, probe_id).unwrap();
        graph.compile().unwrap();

        let in_buf = [0.5_f32; 4];
        let mut left_out = [0.0_f32; 4];
        let mut right_out = [0.0_f32; 4];

        let before = SC_CALLS.load(Ordering::SeqCst);
        graph.process_block(&in_buf, &in_buf, &mut left_out, &mut right_out);
        let after = SC_CALLS.load(Ordering::SeqCst);

        assert!(
            after > before,
            "Sidechain path was never invoked: counter before={before}, after={after}"
        );
    }

    /// `connect_sidechain` with a nonexistent node must return `NodeNotFound`.
    #[test]
    fn connect_sidechain_bad_node_errors() {
        let mut graph = ProcessingGraph::new(48000.0, 4);
        let input = graph.add_input();
        let effect = graph.add_effect(Box::new(Gain { factor: 1.0 }));

        let result = graph.connect_sidechain(NodeId(999), effect);
        assert!(
            matches!(result, Err(GraphError::NodeNotFound(_))),
            "Expected NodeNotFound, got {result:?}"
        );

        let result2 = graph.connect_sidechain(input, NodeId(999));
        assert!(
            matches!(result2, Err(GraphError::NodeNotFound(_))),
            "Expected NodeNotFound for target, got {result2:?}"
        );
    }

    /// `disconnect_sidechain` must clear the sidechain source on the node.
    #[test]
    fn disconnect_sidechain_clears() {
        let mut graph = ProcessingGraph::new(48000.0, 4);
        // Single input serves both as main audio and (temporarily) sidechain source.
        let input = graph.add_input();
        let effect = graph.add_effect(Box::new(Gain { factor: 1.0 }));
        let output = graph.add_output();

        graph.connect(input, effect).unwrap();
        graph.connect(effect, output).unwrap();
        graph.connect_sidechain(input, effect).unwrap();

        let removed = graph.disconnect_sidechain(effect);
        assert!(
            removed,
            "disconnect_sidechain should return true when connection existed"
        );

        // After disconnecting, compile should still succeed (no sidechain)
        let _schedule = graph.compile().unwrap();
    }
}
