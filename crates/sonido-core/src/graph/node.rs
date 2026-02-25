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

use crate::effect::Effect;
use crate::param::SmoothedParam;

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
}

/// The role of a node in the processing graph.
pub enum NodeKind {
    /// Receives external audio input. Exactly one per graph.
    Input,
    /// Produces final audio output. Exactly one per graph.
    Output,
    /// Wraps a DSP effect implementing the [`Effect`] trait.
    Effect(Box<dyn Effect + Send>),
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
    pub bypass_fade: SmoothedParam,
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
        }
    }
}
