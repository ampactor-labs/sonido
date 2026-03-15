//! Graph node types for the DAG routing engine.
//!
//! Each node in the processing graph has a [`NodeId`] and a [`NodeKind`] that
//! determines its role: audio input/output, effect processing, signal splitting,
//! or signal merging. The `NodeData` struct bundles the kind with internal
//! bookkeeping (adjacency lists, per-node bypass state).

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
        }
    }
}
