//! Build a runnable [`GraphEngine`] from a [`Patch`].
//!
//! This is the one place that turns the canonical patch into a live DAG, shared
//! by the firmware patch player and the graph-player plugin. Effect construction
//! is injected as a closure, so the firmware can link only its curated subset
//! (keeping the 480 KB binary small) while the plugin uses the full registry —
//! `build_graph_from_patch` itself pulls in neither `sonido-registry` nor the
//! effect set.

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, vec::Vec};

use sonido_core::graph::{GraphError, NodeId, ProcessingGraph};
use sonido_core::{EffectWithParams, GraphEngine};

use crate::{Patch, PatchEndpoint};

/// Why a patch could not be turned into a graph.
///
/// Not `PartialEq`/`Clone`: the `Graph` variant wraps `GraphError`, which is
/// neither. Match on the variant instead.
#[derive(Debug)]
#[non_exhaustive]
pub enum PatchBuildError {
    /// `make_effect` returned `None` for this effect UID (not linked / unknown).
    UnknownEffect(u16),
    /// An edge referenced a node index that does not exist.
    BadEndpoint,
    /// The graph engine rejected a connection or failed to compile.
    Graph(GraphError),
}

impl From<GraphError> for PatchBuildError {
    fn from(e: GraphError) -> Self {
        PatchBuildError::Graph(e)
    }
}

impl core::fmt::Display for PatchBuildError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownEffect(uid) => write!(f, "effect UID {uid} is not available"),
            Self::BadEndpoint => write!(f, "edge references a non-existent node"),
            Self::Graph(e) => write!(f, "graph error: {e:?}"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for PatchBuildError {}

/// Lazily fetch (or create) a split/merge junction addressed by `id`.
fn junction(
    g: &mut ProcessingGraph,
    slots: &mut Vec<Option<NodeId>>,
    id: usize,
    is_split: bool,
) -> NodeId {
    if slots.len() <= id {
        slots.resize(id + 1, None);
    }
    if let Some(nid) = slots[id] {
        nid
    } else {
        let nid = if is_split {
            g.add_split()
        } else {
            g.add_merge()
        };
        slots[id] = Some(nid);
        nid
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve(
    g: &mut ProcessingGraph,
    node_ids: &[NodeId],
    input: NodeId,
    output: NodeId,
    splits: &mut Vec<Option<NodeId>>,
    merges: &mut Vec<Option<NodeId>>,
    ep: PatchEndpoint,
) -> Result<NodeId, PatchBuildError> {
    Ok(match ep {
        PatchEndpoint::Input => input,
        PatchEndpoint::Output => output,
        PatchEndpoint::Node(i) => *node_ids
            .get(i as usize)
            .ok_or(PatchBuildError::BadEndpoint)?,
        PatchEndpoint::Split(id) => junction(g, splits, id as usize, true),
        PatchEndpoint::Merge(id) => junction(g, merges, id as usize, false),
    })
}

/// Build and compile a [`GraphEngine`] from `patch`.
///
/// `make_effect(uid, sample_rate)` constructs the effect for a node and returns
/// it alongside its registry id (used as the engine's slot name). Slot `i` of
/// the returned engine corresponds to `patch.nodes[i]`, so macro and morph
/// targets address it directly via [`GraphEngine::set_param_at`].
///
/// Per-node bypass and the A snapshot are applied here; B snapshots and live
/// morph are driven afterward by the caller's [`ChainMorph`](sonido_core::ChainMorph).
///
/// # Errors
///
/// [`UnknownEffect`](PatchBuildError::UnknownEffect) if a UID can't be built,
/// [`BadEndpoint`](PatchBuildError::BadEndpoint) for a dangling edge, or
/// [`Graph`](PatchBuildError::Graph) if connection/compilation fails.
pub fn build_graph_from_patch(
    patch: &Patch,
    sample_rate: f32,
    block_size: usize,
    mut make_effect: impl FnMut(u16, f32) -> Option<(Box<dyn EffectWithParams + Send>, &'static str)>,
) -> Result<GraphEngine, PatchBuildError> {
    let mut g = ProcessingGraph::new(sample_rate, block_size);
    let input = g.add_input();
    let output = g.add_output();

    let mut node_ids: Vec<NodeId> = Vec::with_capacity(patch.nodes.len());
    let mut manifest: Vec<(NodeId, &'static str)> = Vec::with_capacity(patch.nodes.len());
    for node in &patch.nodes {
        let (effect, name) = make_effect(node.effect_uid, sample_rate)
            .ok_or(PatchBuildError::UnknownEffect(node.effect_uid))?;
        let nid = g.add_effect(effect);
        node_ids.push(nid);
        manifest.push((nid, name));
    }

    let mut splits: Vec<Option<NodeId>> = Vec::new();
    let mut merges: Vec<Option<NodeId>> = Vec::new();
    for edge in &patch.edges {
        let from = resolve(
            &mut g,
            &node_ids,
            input,
            output,
            &mut splits,
            &mut merges,
            edge.from,
        )?;
        let to = resolve(
            &mut g,
            &node_ids,
            input,
            output,
            &mut splits,
            &mut merges,
            edge.to,
        )?;
        g.connect(from, to)?;
    }

    g.compile()?;

    let mut engine = GraphEngine::new_dag(g, manifest);

    // Apply per-node bypass and the A parameter snapshot (slot i == node i).
    for (slot, node) in patch.nodes.iter().enumerate() {
        engine.set_bypass_at(slot, node.bypassed);
        for (param, &value) in node.params_a.iter().enumerate() {
            engine.set_param_at(slot, param, value);
        }
    }

    Ok(engine)
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use crate::{PatchEdge, PatchEndpoint, PatchNode};
    use sonido_registry::{EffectRegistry, effect_by_uid, effect_uid};

    /// A make_effect backed by the full registry (the plugin's path).
    fn registry_factory(
        registry: &EffectRegistry,
    ) -> impl FnMut(u16, f32) -> Option<(Box<dyn EffectWithParams + Send>, &'static str)> + '_ {
        move |uid, sr| {
            let id = effect_by_uid(uid)?;
            let effect = registry.create(id, sr)?;
            Some((effect, id))
        }
    }

    fn process_is_finite(engine: &mut GraphEngine) -> bool {
        let mut left = [0.1f32; 32];
        let mut right = [0.1f32; 32];
        engine.process_block_stereo_inplace(&mut left, &mut right);
        left.iter().chain(right.iter()).all(|v| v.is_finite())
    }

    #[test]
    fn builds_linear_chain_and_runs() {
        let registry = EffectRegistry::new();
        let patch = Patch::linear_chain(
            "chain",
            vec![
                PatchNode::new(effect_uid("distortion").unwrap(), vec![20.0]),
                PatchNode::new(effect_uid("delay").unwrap(), vec![]),
            ],
        );
        let mut engine =
            build_graph_from_patch(&patch, 48_000.0, 32, registry_factory(&registry)).unwrap();
        assert_eq!(engine.slot_count(), 2);
        assert!(process_is_finite(&mut engine));
    }

    #[test]
    fn builds_parallel_split_merge_and_runs() {
        let registry = EffectRegistry::new();
        let mut patch = Patch::new("parallel");
        patch.nodes = vec![
            PatchNode::new(effect_uid("delay").unwrap(), vec![]),
            PatchNode::new(effect_uid("reverb").unwrap(), vec![]),
        ];
        patch.edges = vec![
            PatchEdge::new(PatchEndpoint::Input, PatchEndpoint::Split(0)),
            PatchEdge::new(PatchEndpoint::Split(0), PatchEndpoint::Node(0)),
            PatchEdge::new(PatchEndpoint::Split(0), PatchEndpoint::Node(1)),
            PatchEdge::new(PatchEndpoint::Node(0), PatchEndpoint::Merge(0)),
            PatchEdge::new(PatchEndpoint::Node(1), PatchEndpoint::Merge(0)),
            PatchEdge::new(PatchEndpoint::Merge(0), PatchEndpoint::Output),
        ];
        let mut engine =
            build_graph_from_patch(&patch, 48_000.0, 32, registry_factory(&registry)).unwrap();
        assert_eq!(engine.slot_count(), 2);
        assert!(process_is_finite(&mut engine));
    }

    #[test]
    fn unknown_effect_is_reported() {
        let registry = EffectRegistry::new();
        let mut patch = Patch::new("bad");
        patch.nodes = vec![PatchNode::new(60000, vec![])]; // no such UID
        patch.edges = vec![PatchEdge::new(PatchEndpoint::Input, PatchEndpoint::Output)];
        let result = build_graph_from_patch(&patch, 48_000.0, 32, registry_factory(&registry));
        assert!(matches!(result, Err(PatchBuildError::UnknownEffect(60000))));
    }

    #[test]
    fn dangling_edge_is_reported() {
        let registry = EffectRegistry::new();
        let mut patch = Patch::new("dangle");
        patch.nodes = vec![PatchNode::new(effect_uid("delay").unwrap(), vec![])];
        patch.edges = vec![PatchEdge::new(PatchEndpoint::Input, PatchEndpoint::Node(5))];
        let result = build_graph_from_patch(&patch, 48_000.0, 32, registry_factory(&registry));
        assert!(matches!(result, Err(PatchBuildError::BadEndpoint)));
    }
}
