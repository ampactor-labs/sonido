//! Graph edge types for the DAG routing engine.
//!
//! An `Edge` connects two nodes, representing audio signal flow from a source
//! node to a destination node.  During schedule compilation, edges are mapped to
//! virtual buffer IDs via liveness analysis; the edge itself does not store
//! buffer assignments.

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
    /// Whether this edge is a feedback edge.
    ///
    /// Feedback edges are exempt from cycle detection.  During schedule
    /// compilation a [`FeedbackDelay`](super::schedule::ProcessStep::FeedbackDelay)
    /// step is inserted on the feedback buffer, making the loop causal.
    /// Typical use: Karplus-Strong, analog filter resonance, tape echo.
    pub is_feedback: bool,
}
