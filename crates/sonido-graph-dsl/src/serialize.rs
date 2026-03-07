//! Graph-to-DSL serialization.
//!
//! Converts a [`GraphSpec`] back into the text-based DSL format,
//! enabling round-trip: `parse_graph_dsl(graph_to_dsl(&spec))` ≈ original.

use crate::parser::{GraphNode, GraphSpec};

/// Serialize a parsed graph specification back to DSL text.
///
/// Linear sequences produce pipe syntax: `distortion:drive=20 | reverb:mix=0.3`
/// Parallel paths produce split syntax: `split(distortion; reverb) | limiter`
/// Dry passthrough renders as `-`.
///
/// Parameters are emitted in alphabetical key order for deterministic output.
/// Only non-empty parameter maps are included.
pub fn graph_to_dsl(spec: &GraphSpec) -> String {
    serialize_path(spec)
}

/// Serialize a path (serial chain of nodes) joined by ` | `.
fn serialize_path(nodes: &[GraphNode]) -> String {
    let parts: Vec<String> = nodes.iter().map(serialize_node).collect();
    parts.join(" | ")
}

/// Serialize a single graph node to DSL text.
fn serialize_node(node: &GraphNode) -> String {
    match node {
        GraphNode::Effect { name, params } => {
            if params.is_empty() {
                name.clone()
            } else {
                let mut sorted: Vec<_> = params.iter().collect();
                sorted.sort_by_key(|(k, _)| k.as_str());
                let param_str: Vec<String> = sorted
                    .into_iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect();
                format!("{name}:{}", param_str.join(","))
            }
        }
        GraphNode::Dry => "-".to_string(),
        GraphNode::Split { paths } => {
            let inner: Vec<String> = paths.iter().map(|p| serialize_path(p)).collect();
            format!("split({})", inner.join("; "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_graph_dsl;

    /// Helper: parse then serialize, verify round-trip.
    fn roundtrip(input: &str) -> String {
        let spec = parse_graph_dsl(input).unwrap();
        graph_to_dsl(&spec)
    }

    #[test]
    fn single_effect() {
        assert_eq!(roundtrip("reverb"), "reverb");
    }

    #[test]
    fn linear_chain() {
        assert_eq!(roundtrip("distortion | reverb"), "distortion | reverb");
    }

    #[test]
    fn effect_with_params() {
        assert_eq!(
            roundtrip("distortion:drive=20,mix=0.8"),
            "distortion:drive=20,mix=0.8"
        );
    }

    #[test]
    fn parallel_split() {
        assert_eq!(
            roundtrip("split(distortion; reverb) | limiter"),
            "split(distortion; reverb) | limiter"
        );
    }

    #[test]
    fn dry_path() {
        assert_eq!(roundtrip("split(distortion; -)"), "split(distortion; -)");
    }

    #[test]
    fn nested_split() {
        let input = "split(split(chorus; flanger); reverb)";
        let result = roundtrip(input);
        // Re-parse to verify structural equivalence
        let spec1 = parse_graph_dsl(input).unwrap();
        let spec2 = parse_graph_dsl(&result).unwrap();
        assert_eq!(spec1, spec2);
    }

    #[test]
    fn whitespace_normalization() {
        assert_eq!(
            roundtrip("  distortion  |  reverb  "),
            "distortion | reverb"
        );
    }

    #[test]
    fn params_sorted_alphabetically() {
        // Parser may not preserve order, but serializer sorts
        let spec = parse_graph_dsl("distortion:mix=0.5,drive=20").unwrap();
        let result = graph_to_dsl(&spec);
        assert_eq!(result, "distortion:drive=20,mix=0.5");
    }

    #[test]
    fn complex_topology() {
        let input = "preamp | split(distortion:drive=15; chorus | flanger) | limiter";
        let result = roundtrip(input);
        let spec1 = parse_graph_dsl(input).unwrap();
        let spec2 = parse_graph_dsl(&result).unwrap();
        assert_eq!(spec1, spec2);
    }

    #[test]
    fn three_way_split() {
        assert_eq!(
            roundtrip("split(distortion; chorus; reverb)"),
            "split(distortion; chorus; reverb)"
        );
    }

    #[test]
    fn negative_param_values() {
        assert_eq!(
            roundtrip("limiter:ceiling=-0.5,threshold=-12"),
            "limiter:ceiling=-0.5,threshold=-12"
        );
    }

    #[test]
    fn split_with_chains_inside() {
        assert_eq!(
            roundtrip("split(distortion | chorus; reverb:mix=1.0)"),
            "split(distortion | chorus; reverb:mix=1.0)"
        );
    }
}
