//! Graph node types for the DAG routing engine.
//!
//! Each node in the processing graph has a [`NodeId`] and a [`NodeKind`] that
//! determines its role: audio input/output, effect processing, signal splitting,
//! signal merging, or a nested sub-graph.  The `NodeData` struct bundles the kind
//! with internal bookkeeping (adjacency lists, per-node bypass state, and rate).

#[cfg(not(feature = "std"))]
use alloc::boxed::Box;
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

use crate::effect_with_params::EffectWithParams;
use crate::param::SmoothedParam;

use super::buffer::StereoBuffer;

/// Unique identifier for a node in the processing graph.
///
/// Node IDs are assigned sequentially and never reused within a graph instance.
/// They remain stable across graph mutations and schedule compilations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeId(pub(crate) u32);

impl NodeId {
    /// Returns the raw numeric identifier.
    #[inline]
    pub fn index(self) -> u32 {
        self.0
    }

    /// Returns a sentinel value used for uninitialized node references.
    #[inline]
    pub fn sentinel() -> Self {
        Self(u32::MAX)
    }
}

/// Processing rate for a graph node.
///
/// Controls how often the node's effect is evaluated within a block:
///
/// - [`Audio`](NodeRate::Audio) nodes process every sample (the default).
/// - [`Control`](NodeRate::Control) nodes process exactly once per block and
///   hold their output for all samples in that block.  This delivers ~480× CPU
///   savings for modulation sources (LFOs, envelopes) at 48 kHz / 100 Hz.
///
/// # Sample-and-hold
///
/// The schedule compiler inserts a [`SampleAndHold`](super::schedule::ProcessStep::SampleAndHold)
/// step after every control-rate `ProcessEffect` step.  This replicates the
/// single control-rate output sample across the full block before any downstream
/// audio-rate node reads it, so kernels require no special casing.
///
/// # Thread Safety
///
/// `NodeRate` is `Copy` and stored on the mutation thread; it is snapshotted into
/// the compiled schedule as a flag on `ProcessEffect`.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum NodeRate {
    /// Process every sample in the block (default for all effects).
    #[default]
    Audio,
    /// Process once per block; hold the single output sample for the entire block.
    ///
    /// `hz` documents the effective update rate (e.g., `100.0` for a 100 Hz
    /// control signal).  Used only for introspection; the scheduler always
    /// executes the node exactly once per block regardless of this value.
    Control(f32),
}

/// The role of a node in the processing graph.
pub enum NodeKind {
    /// Receives external audio input. Exactly one per graph.
    Input,
    /// Produces final audio output. Exactly one per graph.
    Output,
    /// Wraps a DSP effect implementing [`EffectWithParams`].
    Effect(Box<dyn EffectWithParams + Send>),
    /// Fan-out: copies one input to N outputs.
    Split,
    /// Fan-in: sums N inputs into one output.
    Merge,
    /// A nested sub-graph processed as a single opaque block.
    ///
    /// The inner [`ProcessingGraph`] is compiled independently.  During outer
    /// schedule execution, its Input and Output nodes are wired to the outer
    /// buffer slots, making the sub-graph look like a single effect node from
    /// the outer schedule's perspective.
    ///
    /// Sub-graphs enable **reusable effect racks**: define a topology once
    /// (e.g., a parallel distortion + chorus wet path), then embed it anywhere
    /// in larger graphs without duplicating wiring.
    SubGraph(Box<super::processing::ProcessingGraph>),
}

/// Internal bookkeeping for a node in the graph.
pub(crate) struct NodeData {
    /// Node identifier — used for debugging and future graph introspection APIs.
    #[allow(dead_code)]
    pub id: NodeId,
    pub kind: NodeKind,
    /// Indices of edges arriving at this node.
    pub incoming: Vec<super::edge::EdgeId>,
    /// Indices of edges leaving this node.
    pub outgoing: Vec<super::edge::EdgeId>,
    /// Per-node bypass state (only meaningful for Effect nodes).
    pub bypassed: bool,
    /// Crossfade envelope for click-free bypass toggling.
    /// 1.0 = wet (active), 0.0 = dry (bypassed).
    pub bypass_fade: SmoothedParam,
    /// Pre-allocated buffer to save the dry (input) signal before effect processing.
    /// Used during bypass crossfade so the dry signal is available even when
    /// `input_buf == output_buf` (in-place processing).
    pub bypass_buf: StereoBuffer,
    /// Per-block peak input level (left, right). Updated during `run_schedule`.
    pub peak_in: (f32, f32),
    /// Per-block peak output level (left, right). Updated during `run_schedule`.
    pub peak_out: (f32, f32),
    /// Whether this node's output should be preserved for tap reading.
    /// When `true`, the output buffer is copied to `tap_buf` after processing.
    pub tapped: bool,
    /// Persistent buffer for tap output. Only allocated when `tapped` is `true`.
    pub tap_buf: StereoBuffer,
    /// External sidechain source node, if a sidechain connection has been made.
    ///
    /// When `Some(id)`, the schedule compiler routes that node's output buffer
    /// to this effect as a sidechain input. Only meaningful for `Effect` nodes.
    pub sidechain_source: Option<NodeId>,
    /// Processing rate for this node.
    ///
    /// `Audio` (default) — executed every sample in the block.
    /// `Control(hz)` — executed once per block; output replicated for all samples.
    pub node_rate: NodeRate,
    /// Cached single-sample control output, updated each block for control-rate nodes.
    ///
    /// The sample-and-hold step reads `control_output` and replicates it across
    /// the entire output buffer after each control-rate `ProcessEffect` step.
    #[allow(dead_code)]
    pub control_output: (f32, f32),
}

impl NodeData {
    /// Creates a new node with the given ID and kind.
    pub fn new(id: NodeId, kind: NodeKind, sample_rate: f32) -> Self {
        let mut bypass_fade = SmoothedParam::fast(1.0, sample_rate);
        bypass_fade.snap_to_target();
        Self {
            id,
            kind,
            incoming: Vec::new(),
            outgoing: Vec::new(),
            bypassed: false,
            bypass_fade,
            bypass_buf: StereoBuffer::new(0),
            peak_in: (0.0, 0.0),
            peak_out: (0.0, 0.0),
            tapped: false,
            tap_buf: StereoBuffer::new(0),
            sidechain_source: None,
            node_rate: NodeRate::Audio,
            control_output: (0.0, 0.0),
        }
    }
}
