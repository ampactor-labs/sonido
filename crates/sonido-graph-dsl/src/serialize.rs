//! Graph-to-DSL serialization.
//!
//! Converts a [`GraphSpec`] back into the text-based DSL format, and provides
//! [`GraphSnapshot`]-level roundtrip via [`snapshot_to_dsl`] / [`snapshot_from_dsl`]:
//!
//! ```text
//! distortion:drive=20,mix=100 | reverb:decay=2.5,mix=30
//! ```
//!
//! Parameter values are formatted with up to 6 significant digits; stepped
//! (enum) params use their canonical step-label strings.

use crate::DslError;
use crate::effects::create_effect_with_params;
use crate::parser::{GraphNode, GraphSpec};
use sonido_core::graph::{GraphSnapshot, SnapshotEntry};
use sonido_registry::EffectRegistry;

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

// ---------------------------------------------------------------------------
// GraphSnapshot ↔ DSL
// ---------------------------------------------------------------------------

/// Convert a [`GraphSnapshot`] to a DSL string.
///
/// Encodes each effect slot as `effect_id:param=value,...` joined by ` | `.
/// Bypassed effects are prefixed with `!` (e.g., `!reverb:mix=30`).
///
/// Parameter values are formatted using the descriptor's step labels for
/// stepped (enum) params, and compact decimal for continuous params.
/// Only non-default parameter values are included; effects with all-default
/// params are emitted as bare names (e.g., `reverb` not `reverb:decay=2.5,...`).
///
/// # Example
///
/// ```rust,ignore
/// let dsl = snapshot_to_dsl(&snapshot);
/// // "distortion:drive=20 | reverb:decay=2.5,mix=30"
/// ```
pub fn snapshot_to_dsl(snapshot: &GraphSnapshot) -> String {
    let registry = EffectRegistry::new();

    let parts: Vec<String> = snapshot
        .entries
        .iter()
        .map(|entry| snapshot_entry_to_dsl(entry, &registry))
        .collect();

    parts.join(" | ")
}

/// Serialize a single snapshot entry to `[!]effect_id[:param=value,...]`.
fn snapshot_entry_to_dsl(entry: &SnapshotEntry, registry: &EffectRegistry) -> String {
    let prefix = if entry.bypassed { "!" } else { "" };

    // Build a temporary effect to obtain descriptors.
    let Some(effect) = registry.create(&entry.effect_id, 48000.0) else {
        // Unknown effect — emit as bare name (best-effort).
        return format!("{}{}", prefix, entry.effect_id);
    };

    let mut params: Vec<String> = Vec::new();

    for (i, &value) in entry.params.iter().enumerate() {
        let Some(desc) = effect.effect_param_info(i) else {
            continue;
        };

        // Skip default values to keep the DSL compact.
        if (value - desc.default).abs() < f32::EPSILON {
            continue;
        }

        // Use step label for stepped/enum params; decimal otherwise.
        let formatted = if let Some(labels) = desc.step_labels {
            let idx = value.round() as usize;
            labels
                .get(idx)
                .map(|s| s.to_string())
                .unwrap_or_else(|| format_f32(value))
        } else {
            format_f32(value)
        };

        // Use the short_name as DSL key (lower-cased, spaces → underscores).
        let key = desc.short_name.to_lowercase().replace(' ', "_");
        params.push(format!("{key}={formatted}"));
    }

    if params.is_empty() {
        format!("{}{}", prefix, entry.effect_id)
    } else {
        format!("{}{}:{}", prefix, entry.effect_id, params.join(","))
    }
}

/// Format an f32 value compactly (no trailing zeros).
fn format_f32(v: f32) -> String {
    if v.fract() == 0.0 && v.abs() < 1_000_000.0 {
        format!("{}", v as i64)
    } else {
        // Up to 4 decimal places, strip trailing zeros.
        let s = format!("{:.4}", v);
        let s = s.trim_end_matches('0');
        s.trim_end_matches('.').to_string()
    }
}

/// Parse a DSL string into a [`GraphSnapshot`].
///
/// Supports linear chains only (no `split(...)` topology).
/// Bypassed effects may be prefixed with `!`.
///
/// # Errors
///
/// Returns [`DslError`] on syntax errors, unknown effect names, or invalid
/// parameter values.
///
/// # Example
///
/// ```rust,ignore
/// use sonido_graph_dsl::snapshot_from_dsl;
/// use sonido_registry::EffectRegistry;
///
/// let registry = EffectRegistry::new();
/// let snapshot = snapshot_from_dsl("distortion:drive=20 | reverb:mix=30", &registry)?;
/// assert_eq!(snapshot.entries.len(), 2);
/// ```
pub fn snapshot_from_dsl(s: &str, registry: &EffectRegistry) -> Result<GraphSnapshot, DslError> {
    let entries: Vec<SnapshotEntry> = s
        .split('|')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| dsl_part_to_snapshot_entry(part, registry))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(GraphSnapshot { entries })
}

/// Parse one pipe-separated segment into a [`SnapshotEntry`].
fn dsl_part_to_snapshot_entry(
    part: &str,
    registry: &EffectRegistry,
) -> Result<SnapshotEntry, DslError> {
    // Detect and strip bypass prefix `!`.
    let (bypassed, spec) = if let Some(rest) = part.strip_prefix('!') {
        (true, rest)
    } else {
        (false, part)
    };

    // Split into name and optional params string.
    let (name_part, params_str) = if let Some(colon) = spec.find(':') {
        (&spec[..colon], Some(&spec[colon + 1..]))
    } else {
        (spec, None)
    };

    let effect_id_raw = name_part.trim();
    let canonical_id = crate::effects::resolve_effect_name(effect_id_raw);

    // Create a temporary effect to get descriptors and defaults.
    let effect = registry.create(canonical_id, 48000.0).ok_or_else(|| {
        DslError::Effect(crate::effects::EffectError::UnknownEffect(
            effect_id_raw.to_string(),
        ))
    })?;

    let param_count = effect.effect_param_count();
    let mut params: Vec<f32> = (0..param_count)
        .map(|i| effect.effect_param_info(i).map_or(0.0, |d| d.default))
        .collect();

    // Apply overrides from the DSL string.
    if let Some(params_str) = params_str {
        use std::collections::HashMap;
        let raw_params: HashMap<String, String> = params_str
            .split(',')
            .filter_map(|kv| {
                let mut it = kv.splitn(2, '=');
                let k = it.next()?.trim().to_string();
                let v = it.next()?.trim().to_string();
                Some((k, v))
            })
            .collect();

        if !raw_params.is_empty() {
            let (temp_effect, _) = create_effect_with_params(canonical_id, 48000.0, &raw_params)?;

            // Overwrite all param slots with post-override values.
            // `create_effect_with_params` starts from defaults and applies overrides,
            // so unmentioned params stay at their defaults.
            for i in 0..params.len() {
                params[i] = temp_effect.effect_get_param(i);
            }
        }
    }

    Ok(SnapshotEntry {
        effect_id: canonical_id.to_string(),
        params,
        bypassed,
    })
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

    // --- GraphSnapshot roundtrip tests ---

    #[test]
    fn snapshot_from_dsl_single_effect() {
        let registry = EffectRegistry::new();
        let snap = snapshot_from_dsl("distortion", &registry).unwrap();
        assert_eq!(snap.entries.len(), 1);
        assert_eq!(snap.entries[0].effect_id, "distortion");
        assert!(!snap.entries[0].bypassed);
        // All params should be at defaults.
        let effect = registry.create("distortion", 48000.0).unwrap();
        for i in 0..snap.entries[0].params.len() {
            let default = effect.effect_param_info(i).unwrap().default;
            assert_eq!(
                snap.entries[0].params[i], default,
                "param {i} should be default"
            );
        }
    }

    #[test]
    fn snapshot_from_dsl_with_params() {
        let registry = EffectRegistry::new();
        let snap = snapshot_from_dsl("distortion:drive=20", &registry).unwrap();
        assert_eq!(snap.entries[0].effect_id, "distortion");
        // param 0 is drive
        assert_eq!(snap.entries[0].params[0], 20.0);
    }

    #[test]
    fn snapshot_from_dsl_bypassed() {
        let registry = EffectRegistry::new();
        let snap = snapshot_from_dsl("!reverb", &registry).unwrap();
        assert_eq!(snap.entries[0].effect_id, "reverb");
        assert!(snap.entries[0].bypassed);
    }

    #[test]
    fn snapshot_from_dsl_chain() {
        let registry = EffectRegistry::new();
        let snap = snapshot_from_dsl("distortion:drive=20 | !reverb", &registry).unwrap();
        assert_eq!(snap.entries.len(), 2);
        assert_eq!(snap.entries[0].effect_id, "distortion");
        assert!(!snap.entries[0].bypassed);
        assert_eq!(snap.entries[1].effect_id, "reverb");
        assert!(snap.entries[1].bypassed);
    }

    #[test]
    fn snapshot_to_dsl_default_params_omitted() {
        let registry = EffectRegistry::new();
        let effect = registry.create("distortion", 48000.0).unwrap();
        let param_count = effect.effect_param_count();
        let params: Vec<f32> = (0..param_count)
            .map(|i| effect.effect_param_info(i).unwrap().default)
            .collect();
        let snap = GraphSnapshot {
            entries: vec![SnapshotEntry {
                effect_id: "distortion".to_string(),
                params,
                bypassed: false,
            }],
        };
        let dsl = snapshot_to_dsl(&snap);
        assert_eq!(dsl, "distortion");
    }

    #[test]
    fn snapshot_roundtrip() {
        let registry = EffectRegistry::new();
        // Parse DSL → snapshot → DSL → snapshot, check structural equality.
        let original_dsl = "distortion:drive=20 | !reverb";
        let snap1 = snapshot_from_dsl(original_dsl, &registry).unwrap();
        let round_dsl = snapshot_to_dsl(&snap1);
        let snap2 = snapshot_from_dsl(&round_dsl, &registry).unwrap();

        assert_eq!(snap1.entries.len(), snap2.entries.len());
        for (a, b) in snap1.entries.iter().zip(snap2.entries.iter()) {
            assert_eq!(a.effect_id, b.effect_id);
            assert_eq!(a.bypassed, b.bypassed);
            for (i, (va, vb)) in a.params.iter().zip(b.params.iter()).enumerate() {
                assert!((va - vb).abs() < 1e-4, "param {i} diverged: {va} vs {vb}");
            }
        }
    }
}
