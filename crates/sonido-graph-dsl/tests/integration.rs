//! Integration tests for sonido-graph-dsl.
//!
//! These tests exercise the full public API across module boundaries:
//! parse → validate → build → process, alias resolution end-to-end,
//! and `GraphSnapshot` serialization round-trips.

use sonido_graph_dsl::{
    DslError, GraphNode, build_graph, count_nodes, graph_to_dsl, parse_chain, parse_graph_dsl,
    resolve_effect_name, snapshot_from_dsl, snapshot_to_dsl, validate_spec,
};
use sonido_registry::EffectRegistry;

// ── DSL parsing ──────────────────────────────────────────────────────────────

/// Simple two-effect chain parses to exactly two Effect nodes.
#[test]
fn parse_simple_chain() {
    let spec = parse_graph_dsl("distortion|reverb").unwrap();
    assert_eq!(spec.len(), 2);
    assert!(matches!(&spec[0], GraphNode::Effect { name, .. } if name == "distortion"));
    assert!(matches!(&spec[1], GraphNode::Effect { name, .. } if name == "reverb"));
}

/// Whitespace around `|` is optional.
#[test]
fn parse_chain_whitespace_variants() {
    let compact = parse_graph_dsl("distortion|reverb").unwrap();
    let spaced = parse_graph_dsl("distortion | reverb").unwrap();
    assert_eq!(compact, spaced);
}

/// A single effect with multiple parameters is parsed correctly.
#[test]
fn parse_chain_with_params() {
    let spec = parse_graph_dsl("distortion:drive=20|reverb:mix=50").unwrap();
    assert_eq!(spec.len(), 2);

    if let GraphNode::Effect { name, params } = &spec[0] {
        assert_eq!(name, "distortion");
        assert_eq!(params.get("drive").map(String::as_str), Some("20"));
    } else {
        panic!("expected Effect node at index 0");
    }

    if let GraphNode::Effect { name, params } = &spec[1] {
        assert_eq!(name, "reverb");
        assert_eq!(params.get("mix").map(String::as_str), Some("50"));
    } else {
        panic!("expected Effect node at index 1");
    }
}

/// Multiple parameters on one effect, comma-separated.
#[test]
fn parse_effect_with_multiple_params() {
    let spec = parse_graph_dsl("reverb:decay=2.5,mix=30,room=0.8").unwrap();
    assert_eq!(spec.len(), 1);
    if let GraphNode::Effect { params, .. } = &spec[0] {
        assert_eq!(params.len(), 3);
        assert_eq!(params.get("decay").map(String::as_str), Some("2.5"));
        assert_eq!(params.get("mix").map(String::as_str), Some("30"));
        assert_eq!(params.get("room").map(String::as_str), Some("0.8"));
    } else {
        panic!("expected Effect node");
    }
}

/// A longer chain produces the right node count.
#[test]
fn parse_long_chain() {
    let spec =
        parse_graph_dsl("preamp:gain=6|distortion:drive=15|eq|reverb:mix=30|limiter").unwrap();
    assert_eq!(spec.len(), 5);
    assert_eq!(count_nodes(&spec), 5);
}

/// `split(...)` with a dry path is parsed into a Split node with two paths.
#[test]
fn parse_split_with_dry() {
    let spec = parse_graph_dsl("split(chorus; -)").unwrap();
    assert_eq!(spec.len(), 1);
    if let GraphNode::Split { paths } = &spec[0] {
        assert_eq!(paths.len(), 2);
        assert!(matches!(&paths[0][0], GraphNode::Effect { name, .. } if name == "chorus"));
        assert_eq!(paths[1][0], GraphNode::Dry);
    } else {
        panic!("expected Split node");
    }
}

// ── Parse errors ─────────────────────────────────────────────────────────────

/// Empty string is a hard error.
#[test]
fn parse_empty_string_is_error() {
    let err = parse_graph_dsl("").unwrap_err();
    // The parser emits ParamError for empty specs.
    assert!(
        matches!(err, sonido_graph_dsl::DslParseError::ParamError { .. }),
        "expected ParamError, got {err:?}"
    );
}

/// Whitespace-only string is a hard error.
#[test]
fn parse_whitespace_only_is_error() {
    let err = parse_graph_dsl("   ").unwrap_err();
    assert!(matches!(
        err,
        sonido_graph_dsl::DslParseError::ParamError { .. }
    ));
}

/// A colon without `=` in the parameter list is malformed.
#[test]
fn parse_malformed_params_missing_equals() {
    // "distortion:drive20" has a colon but no `=` — ParamError expected.
    let err = parse_graph_dsl("distortion:drive20").unwrap_err();
    assert!(
        matches!(err, sonido_graph_dsl::DslParseError::ParamError { .. }),
        "expected ParamError for missing '=', got {err:?}"
    );
}

/// Unclosed `split(` returns `UnclosedSplit`.
#[test]
fn parse_unclosed_split_is_error() {
    let err = parse_graph_dsl("split(distortion; reverb").unwrap_err();
    assert!(matches!(
        err,
        sonido_graph_dsl::DslParseError::UnclosedSplit { .. }
    ));
}

/// `split(single_path)` has only one arm — `SplitTooFewPaths` expected.
#[test]
fn parse_split_single_path_is_error() {
    let err = parse_graph_dsl("split(distortion)").unwrap_err();
    assert!(matches!(
        err,
        sonido_graph_dsl::DslParseError::SplitTooFewPaths { count: 1 }
    ));
}

/// Empty arm inside split: `split(distortion; ; reverb)`.
#[test]
fn parse_empty_split_arm_is_error() {
    let err = parse_graph_dsl("split(distortion; ; reverb)").unwrap_err();
    assert!(matches!(
        err,
        sonido_graph_dsl::DslParseError::EmptySplitPath { .. }
    ));
}

/// Trailing junk after a valid spec triggers `UnexpectedChar`.
#[test]
fn parse_trailing_garbage_is_error() {
    // Extra `)` after a valid spec is unexpected.
    let err = parse_graph_dsl("reverb)").unwrap_err();
    assert!(matches!(
        err,
        sonido_graph_dsl::DslParseError::UnexpectedChar { ch: ')', .. }
    ));
}

// ── Validation ───────────────────────────────────────────────────────────────

/// Dry passthrough at the top level of a chain must be rejected.
#[test]
fn validate_rejects_dry_at_top_level() {
    let spec = parse_graph_dsl("-").unwrap(); // parser accepts it; validator rejects
    assert!(matches!(
        validate_spec(&spec),
        Err(sonido_graph_dsl::DslParseError::DryAtTopLevel)
    ));
}

/// Dry passthrough is allowed inside a split arm.
#[test]
fn validate_allows_dry_inside_split() {
    let spec = parse_graph_dsl("split(reverb; -)").unwrap();
    assert!(validate_spec(&spec).is_ok());
}

// ── Alias resolution ─────────────────────────────────────────────────────────

/// All documented aliases resolve to the correct canonical registry IDs.
#[test]
fn alias_resolution_table() {
    let cases = [
        ("tape_saturation", "tape"),
        ("tapesaturation", "tape"),
        ("tape", "tape"),
        ("parametric_eq", "eq"),
        ("parametriceq", "eq"),
        ("peq", "eq"),
        ("eq", "eq"),
        ("multivibrato", "vibrato"),
        ("multi_vibrato", "vibrato"),
        ("vibrato", "vibrato"),
        ("lowpass", "filter"),
        ("filter", "filter"),
        ("noisegate", "gate"),
        ("noise_gate", "gate"),
        ("gate", "gate"),
        ("autowah", "wah"),
        ("auto_wah", "wah"),
        ("wah", "wah"),
        ("cleanpreamp", "preamp"),
        ("clean_preamp", "preamp"),
        ("preamp", "preamp"),
        ("bitcrusher", "bitcrusher"),
        ("crusher", "bitcrusher"),
        ("ring", "ringmod"),
        ("ring_mod", "ringmod"),
        ("ringmod", "ringmod"),
    ];
    for (alias, expected) in cases {
        assert_eq!(
            resolve_effect_name(alias),
            expected,
            "alias '{alias}' should resolve to '{expected}'"
        );
    }
}

/// Aliases work case-insensitively.
#[test]
fn alias_resolution_case_insensitive() {
    assert_eq!(resolve_effect_name("TAPE_SATURATION"), "tape");
    assert_eq!(resolve_effect_name("Parametric_EQ"), "eq");
    assert_eq!(resolve_effect_name("MultiVibrato"), "vibrato");
}

/// An alias used directly in a DSL string reaches the correct effect.
#[test]
fn alias_in_dsl_string_resolves_via_parse_chain() {
    // `parse_chain` internally calls `create_effect_with_params` → `resolve_effect_name`.
    let chain = parse_chain("tape_saturation", 48000.0).unwrap();
    assert_eq!(chain.len(), 1);
}

/// `parametric_eq` alias creates an EQ effect end-to-end.
#[test]
fn alias_parametric_eq_creates_effect() {
    let chain = parse_chain("parametric_eq", 48000.0).unwrap();
    assert_eq!(chain.len(), 1);
}

/// Unknown effect names are rejected with `EffectError::UnknownEffect`.
#[test]
fn alias_unknown_effect_is_error() {
    let result = parse_chain("totally_unknown_effect_xyz", 48000.0);
    match result {
        Err(sonido_graph_dsl::EffectError::UnknownEffect(_)) => {} // expected
        Err(other) => panic!("expected UnknownEffect, got {other:?}"),
        Ok(_) => panic!("expected an error for unknown effect name"),
    }
}

// ── parse_chain end-to-end ────────────────────────────────────────────────────

/// `parse_chain` builds real effect objects from a pipe-separated spec.
#[test]
fn parse_chain_produces_correct_count() {
    let chain = parse_chain("preamp:gain=6|distortion:drive=12|reverb:mix=0.3", 48000.0).unwrap();
    assert_eq!(chain.len(), 3);
}

/// `parse_chain` with a single effect and no parameters.
#[test]
fn parse_chain_single_effect_no_params() {
    let chain = parse_chain("reverb", 48000.0).unwrap();
    assert_eq!(chain.len(), 1);
}

/// `parse_chain` honours parameter aliases (e.g. `time` → `delay time`).
#[test]
fn parse_chain_with_param_aliases() {
    // "time" is aliased to "delay time" for the delay effect.
    let chain = parse_chain("delay:time=300", 48000.0).unwrap();
    assert_eq!(chain.len(), 1);
}

// ── Full pipeline: parse → validate → build_graph ───────────────────────────

/// A simple two-effect chain builds into the expected graph structure.
#[test]
fn build_graph_simple_chain() {
    let spec = parse_graph_dsl("distortion:drive=20|reverb:mix=50").unwrap();
    validate_spec(&spec).unwrap();
    let (graph, manifest) = build_graph(&spec, 48000.0, 256).unwrap();

    // Input + distortion + reverb + Output = 4 nodes
    assert_eq!(graph.node_count(), 4);
    assert_eq!(manifest.len(), 2);
    assert_eq!(manifest[0].1, "distortion");
    assert_eq!(manifest[1].1, "reverb");
}

/// A split topology builds and can process audio without panicking.
#[test]
fn build_graph_split_processes_audio() {
    let spec = parse_graph_dsl("split(distortion:drive=15; -) | limiter").unwrap();
    validate_spec(&spec).unwrap();
    let (mut graph, manifest) = build_graph(&spec, 48000.0, 256).unwrap();

    // distortion + limiter in manifest (dry `-` is not an effect node)
    assert_eq!(manifest.len(), 2);
    assert_eq!(manifest[0].1, "distortion");
    assert_eq!(manifest[1].1, "limiter");

    let input = vec![0.05_f32; 256];
    let mut out_l = vec![0.0_f32; 256];
    let mut out_r = vec![0.0_f32; 256];
    graph.process_block(&input, &input, &mut out_l, &mut out_r);

    let energy: f32 = out_l.iter().map(|s| s * s).sum();
    assert!(energy > 0.0, "graph must produce non-zero output");
}

/// Building with an unknown effect name surfaces a `DslError::Effect`.
#[test]
fn build_graph_unknown_effect_is_error() {
    let spec = parse_graph_dsl("totally_unknown_effect_xyz").unwrap();
    let result = build_graph(&spec, 48000.0, 256);
    match result {
        Err(DslError::Effect(_)) => {} // expected
        Err(other) => panic!("expected DslError::Effect, got {other:?}"),
        Ok(_) => panic!("expected a build error for unknown effect"),
    }
}

// ── graph_to_dsl (GraphSpec serialization) ───────────────────────────────────

/// Serializing a bare effect name produces the same bare name.
#[test]
fn graph_to_dsl_bare_name() {
    let spec = parse_graph_dsl("reverb").unwrap();
    assert_eq!(graph_to_dsl(&spec), "reverb");
}

/// `graph_to_dsl` round-trips a linear chain with parameters.
#[test]
fn graph_to_dsl_roundtrip_chain_with_params() {
    // Parameters are re-emitted in alphabetical order by key.
    let input = "distortion:drive=20,mix=100";
    let spec = parse_graph_dsl(input).unwrap();
    let dsl = graph_to_dsl(&spec);
    // Keys sorted: drive < mix
    assert_eq!(dsl, "distortion:drive=20,mix=100");
}

/// `graph_to_dsl` sorts parameter keys alphabetically.
#[test]
fn graph_to_dsl_sorts_params_alphabetically() {
    // Provide params in non-alphabetical order.
    let spec = parse_graph_dsl("reverb:mix=0.5,decay=2.0,room=0.6").unwrap();
    let dsl = graph_to_dsl(&spec);
    // Sorted: decay < mix < room
    assert_eq!(dsl, "reverb:decay=2.0,mix=0.5,room=0.6");
}

/// `graph_to_dsl` → `parse_graph_dsl` produces structurally equal specs.
#[test]
fn graph_to_dsl_full_roundtrip_structural_equality() {
    let input = "preamp:gain=6 | split(distortion:drive=15; -) | limiter";
    let spec1 = parse_graph_dsl(input).unwrap();
    let serialized = graph_to_dsl(&spec1);
    let spec2 = parse_graph_dsl(&serialized).unwrap();
    assert_eq!(spec1, spec2);
}

// ── snapshot_to_dsl / snapshot_from_dsl round-trips ─────────────────────────

/// A snapshot with a single default-params effect serializes to just the name.
#[test]
fn snapshot_roundtrip_defaults_omitted() {
    let registry = EffectRegistry::new();

    // Build a snapshot with all-default params.
    let effect = registry.create("distortion", 48000.0).unwrap();
    let param_count = effect.effect_param_count();
    let params: Vec<f32> = (0..param_count)
        .map(|i| effect.effect_param_info(i).unwrap().default)
        .collect();

    let snap = sonido_core::graph::GraphSnapshot {
        entries: vec![sonido_core::graph::SnapshotEntry {
            effect_id: "distortion".to_string(),
            params,
            bypassed: false,
        }],
    };

    // Serialize: all-default → bare name
    let dsl = snapshot_to_dsl(&snap);
    assert_eq!(dsl, "distortion");

    // Re-parse back to snapshot and verify param count matches.
    let snap2 = snapshot_from_dsl(&dsl, &registry).unwrap();
    assert_eq!(snap2.entries.len(), 1);
    assert_eq!(snap2.entries[0].effect_id, "distortion");
    assert_eq!(snap2.entries[0].params.len(), snap.entries[0].params.len());
}

/// A non-default parameter value survives the full DSL round-trip.
#[test]
fn snapshot_roundtrip_non_default_param() {
    let registry = EffectRegistry::new();

    // Parse a DSL with an explicit non-default drive value.
    let snap1 = snapshot_from_dsl("distortion:drive=25", &registry).unwrap();
    assert_eq!(snap1.entries[0].effect_id, "distortion");

    // Serialize back to DSL and re-parse.
    let dsl = snapshot_to_dsl(&snap1);
    let snap2 = snapshot_from_dsl(&dsl, &registry).unwrap();

    assert_eq!(snap1.entries.len(), snap2.entries.len());
    for (a, b) in snap1.entries.iter().zip(snap2.entries.iter()) {
        assert_eq!(a.effect_id, b.effect_id);
        assert_eq!(a.bypassed, b.bypassed);
        for (i, (va, vb)) in a.params.iter().zip(b.params.iter()).enumerate() {
            assert!(
                (va - vb).abs() < 1e-4,
                "param {i} diverged after round-trip: {va} vs {vb}"
            );
        }
    }
}

/// Bypass flag (`!`) is preserved through the round-trip.
#[test]
fn snapshot_roundtrip_bypass_flag() {
    let registry = EffectRegistry::new();

    let snap1 = snapshot_from_dsl("!reverb", &registry).unwrap();
    assert!(snap1.entries[0].bypassed);

    let dsl = snapshot_to_dsl(&snap1);
    // The serialized form should include the `!` prefix.
    assert!(
        dsl.starts_with('!'),
        "bypassed effect should serialize with '!'"
    );

    let snap2 = snapshot_from_dsl(&dsl, &registry).unwrap();
    assert!(snap2.entries[0].bypassed);
}

/// A multi-effect snapshot round-trips with correct order and bypass flags.
#[test]
fn snapshot_roundtrip_multi_effect_chain() {
    let registry = EffectRegistry::new();

    let dsl = "distortion:drive=20 | !reverb | limiter";
    let snap1 = snapshot_from_dsl(dsl, &registry).unwrap();

    assert_eq!(snap1.entries.len(), 3);
    assert_eq!(snap1.entries[0].effect_id, "distortion");
    assert!(!snap1.entries[0].bypassed);
    assert_eq!(snap1.entries[1].effect_id, "reverb");
    assert!(snap1.entries[1].bypassed);
    assert_eq!(snap1.entries[2].effect_id, "limiter");
    assert!(!snap1.entries[2].bypassed);

    // Round-trip: serialize → parse → check structural equality.
    let dsl2 = snapshot_to_dsl(&snap1);
    let snap2 = snapshot_from_dsl(&dsl2, &registry).unwrap();

    assert_eq!(snap1.entries.len(), snap2.entries.len());
    for (a, b) in snap1.entries.iter().zip(snap2.entries.iter()) {
        assert_eq!(a.effect_id, b.effect_id);
        assert_eq!(a.bypassed, b.bypassed);
        for (i, (va, vb)) in a.params.iter().zip(b.params.iter()).enumerate() {
            assert!(
                (va - vb).abs() < 1e-4,
                "param {i} of '{}' diverged: {va} vs {vb}",
                a.effect_id
            );
        }
    }
}

/// `snapshot_from_dsl` resolves aliases in the DSL string.
#[test]
fn snapshot_from_dsl_resolves_alias() {
    let registry = EffectRegistry::new();
    let snap = snapshot_from_dsl("tape_saturation", &registry).unwrap();
    assert_eq!(snap.entries[0].effect_id, "tape");
}

/// `snapshot_from_dsl` rejects an unknown effect with a `DslError::Effect`.
#[test]
fn snapshot_from_dsl_unknown_effect_is_error() {
    let registry = EffectRegistry::new();
    let err = snapshot_from_dsl("totally_unknown_effect_xyz", &registry).unwrap_err();
    assert!(
        matches!(err, DslError::Effect(_)),
        "expected DslError::Effect, got {err:?}"
    );
}

/// `snapshot_from_dsl` with negative parameter values round-trips correctly.
#[test]
fn snapshot_roundtrip_negative_param_values() {
    let registry = EffectRegistry::new();
    let snap1 = snapshot_from_dsl("limiter:threshold=-12,ceiling=-0.5", &registry).unwrap();
    let dsl = snapshot_to_dsl(&snap1);
    let snap2 = snapshot_from_dsl(&dsl, &registry).unwrap();

    for (a, b) in snap1.entries.iter().zip(snap2.entries.iter()) {
        for (i, (va, vb)) in a.params.iter().zip(b.params.iter()).enumerate() {
            assert!((va - vb).abs() < 1e-3, "param {i} diverged: {va} vs {vb}");
        }
    }
}
