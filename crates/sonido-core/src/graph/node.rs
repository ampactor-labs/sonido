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
#[non_exhaustive]
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
#[non_exhaustive]
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
    /// The inner [`super::ProcessingGraph`] is compiled independently.  During outer
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
    /// CPU cycles consumed by the last `ProcessEffect` call for this node.
    ///
    /// Measured using the ARM DWT cycle counter on embedded targets.
    /// Always `0` on non-ARM platforms (desktop, WASM). Used for per-effect
    /// CPU profiling on the Daisy Seed.
    pub last_cycles: u32,
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
            last_cycles: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;

    use super::*;

    // ── NodeId ────────────────────────────────────────────────────────────────

    #[test]
    fn node_id_index_returns_raw_value() {
        let id = NodeId(42);
        assert_eq!(id.index(), 42);
    }

    #[test]
    fn node_id_sentinel_is_u32_max() {
        assert_eq!(NodeId::sentinel().index(), u32::MAX);
    }

    #[test]
    fn node_id_equality() {
        let a = NodeId(7);
        let b = NodeId(7);
        let c = NodeId(8);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn node_id_copy() {
        let id = NodeId(3);
        let copy = id;
        assert_eq!(id, copy);
    }

    // ── NodeRate ──────────────────────────────────────────────────────────────

    #[test]
    fn node_rate_default_is_audio() {
        let rate = NodeRate::default();
        assert_eq!(rate, NodeRate::Audio);
    }

    #[test]
    fn node_rate_control_stores_hz() {
        let rate = NodeRate::Control(100.0);
        match rate {
            NodeRate::Control(hz) => assert!((hz - 100.0).abs() < 1e-6),
            _ => panic!("expected NodeRate::Control"),
        }
    }

    #[test]
    fn node_rate_audio_equals_audio() {
        assert_eq!(NodeRate::Audio, NodeRate::Audio);
    }

    // ── NodeData ──────────────────────────────────────────────────────────────

    #[test]
    fn node_data_new_input_kind() {
        let id = NodeId(0);
        let node = NodeData::new(id, NodeKind::Input, 48000.0);
        assert_eq!(node.id, id);
        assert!(!node.bypassed);
        assert_eq!(node.incoming.len(), 0);
        assert_eq!(node.outgoing.len(), 0);
    }

    #[test]
    fn node_data_new_output_kind() {
        let id = NodeId(1);
        let node = NodeData::new(id, NodeKind::Output, 48000.0);
        assert_eq!(node.id, id);
        assert_eq!(node.incoming.len(), 0);
        assert_eq!(node.outgoing.len(), 0);
    }

    #[test]
    fn node_data_new_split_kind() {
        let node = NodeData::new(NodeId(2), NodeKind::Split, 48000.0);
        assert!(!node.bypassed);
        assert_eq!(node.node_rate, NodeRate::Audio);
    }

    #[test]
    fn node_data_new_merge_kind() {
        let node = NodeData::new(NodeId(3), NodeKind::Merge, 48000.0);
        assert!(!node.bypassed);
        assert_eq!(node.node_rate, NodeRate::Audio);
    }

    #[test]
    fn node_data_initial_peaks_are_zero() {
        let node = NodeData::new(NodeId(0), NodeKind::Input, 48000.0);
        assert_eq!(node.peak_in, (0.0, 0.0));
        assert_eq!(node.peak_out, (0.0, 0.0));
    }

    #[test]
    fn node_data_initial_last_cycles_is_zero() {
        let node = NodeData::new(NodeId(0), NodeKind::Input, 48000.0);
        assert_eq!(node.last_cycles, 0);
    }

    #[test]
    fn node_data_not_tapped_by_default() {
        let node = NodeData::new(NodeId(0), NodeKind::Input, 48000.0);
        assert!(!node.tapped);
    }

    #[test]
    fn node_data_sidechain_source_none_by_default() {
        let node = NodeData::new(NodeId(0), NodeKind::Input, 48000.0);
        assert!(node.sidechain_source.is_none());
    }

    #[test]
    fn node_data_bypass_buf_starts_empty() {
        let node = NodeData::new(NodeId(0), NodeKind::Input, 48000.0);
        assert_eq!(node.bypass_buf.len(), 0);
    }

    #[test]
    fn node_data_tap_buf_starts_empty() {
        let node = NodeData::new(NodeId(0), NodeKind::Input, 48000.0);
        assert_eq!(node.tap_buf.len(), 0);
    }
}
