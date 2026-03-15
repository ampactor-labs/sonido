//! Graph topology DSL for Sonido.
//!
//! Provides a text-based DSL for defining audio effect topologies, from
//! simple linear chains to complex parallel DAGs with split/merge nodes.
//!
//! # Architecture
//!
//! Three-layer design:
//! - **Parser** ([`parser`]): pure text → IR transformation, no audio dependencies
//! - **Builder** ([`builder`]): IR → `ProcessingGraph`
//!   with effect manifest for `GraphEngine::new_dag()`
//! - **Effects** ([`effects`]): effect factory with name/parameter alias resolution
//! - **Serialize** ([`serialize`]): `GraphSpec` and `GraphSnapshot` ↔ DSL text
//!
//! # Usage
//!
//! ```rust,ignore
//! use sonido_graph_dsl::{parse_graph_dsl, validate_spec, build_graph};
//! use sonido_core::graph::GraphEngine;
//!
//! let spec = parse_graph_dsl("split(distortion:drive=20; -) | reverb:mix=0.3")?;
//! validate_spec(&spec)?;
//! let (graph, manifest) = build_graph(&spec, 48000.0, 256)?;
//! let engine = GraphEngine::new_dag(graph, manifest);
//! ```

pub mod builder;
pub mod effects;
pub mod parser;
pub mod serialize;

// Re-export primary API
pub use builder::{ManifestEntry, build_graph, build_graph_only};
pub use effects::{
    EffectError, create_effect_with_params, parse_chain, parse_effect_spec, resolve_effect_name,
};
pub use parser::{
    DslParseError, GraphNode, GraphSpec, build_graph_slug, count_nodes, parse_graph_dsl,
    validate_spec,
};
pub use serialize::{graph_to_dsl, snapshot_from_dsl, snapshot_to_dsl};

/// Unified error type for DSL operations.
#[derive(Debug, thiserror::Error)]
pub enum DslError {
    /// Parse error from the DSL parser.
    #[error(transparent)]
    Parse(#[from] DslParseError),

    /// Effect creation error (unknown effect, bad param value, etc.).
    #[error(transparent)]
    Effect(#[from] EffectError),

    /// Graph construction error (cycle, invalid connection, etc.).
    #[error(transparent)]
    Graph(#[from] sonido_core::graph::GraphError),
}
