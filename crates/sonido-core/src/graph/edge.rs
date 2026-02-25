//! Graph edge types for the DAG routing engine.
//!
//! An `Edge` connects two nodes, representing audio signal flow from a source
//! node to a destination node. During schedule compilation, each edge is assigned
//! a buffer index from the [`BufferPool`](super::buffer::BufferPool) via liveness
//! analysis.

/// Unique identifier for an edge in the processing graph.
///
/// Edge IDs are assigned sequentially and never reused within a graph instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EdgeId(pub(crate) u32);

impl EdgeId {
    /// Returns the raw numeric identifier.
    #[inline]
    pub fn index(self) -> u32 {
        self.0
    }
}

/// A directed connection between two nodes in the processing graph.
pub(crate) struct Edge {
    /// Source node.
    pub from: super::node::NodeId,
    /// Destination node.
    pub to: super::node::NodeId,
    /// Buffer slot assigned during schedule compilation.
    /// `None` until `compile()` is called.
    /// Currently unused — buffer assignment uses virtual buffer IDs during compilation
    /// rather than annotating edges directly. Reserved for future direct edge→buffer mapping.
    #[allow(dead_code)]
    pub buffer_idx: Option<usize>,
}
