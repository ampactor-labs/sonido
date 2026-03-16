//! Integration tests for `sonido-registry`.
//!
//! These tests exercise registry-wide invariants — cross-effect consistency,
//! completeness contracts, and API surface correctness — that are complementary
//! to the unit tests in `src/lib.rs`.
//!
//! Note: Effect-name alias resolution (e.g. `"tape_saturation"` → `"tape"`) is
//! handled by `sonido_graph_dsl::effects::resolve_effect_name`, not the registry
//! itself. Those aliases are tested in the graph-dsl crate.

use sonido_registry::{EffectCategory, EffectDescriptor, EffectRegistry};

// ── 1. All effects creatable ──────────────────────────────────────────────────

/// Every descriptor returned by `all_effects()` must be creatable at 48 kHz.
/// Verifies factory closure is wired correctly for every registered entry.
#[test]
fn all_effects_creatable_at_48k() {
    let registry = EffectRegistry::new();
    for desc in registry.all_effects() {
        let effect = registry.create(desc.id, 48_000.0);
        assert!(
            effect.is_some(),
            "registry.create({:?}, 48000.0) returned None — factory not wired",
            desc.id
        );
    }
}

/// Effects must also be creatable at non-standard sample rates.
/// 44.1 kHz and 96 kHz are common deployment targets.
#[test]
fn all_effects_creatable_at_various_sample_rates() {
    let registry = EffectRegistry::new();
    for &sr in &[22_050.0_f32, 44_100.0, 88_200.0, 96_000.0, 192_000.0] {
        for desc in registry.all_effects() {
            let effect = registry.create(desc.id, sr);
            assert!(
                effect.is_some(),
                "registry.create({:?}, {sr}) returned None",
                desc.id
            );
            // Verify the created instance produces finite output at this rate.
            let mut effect = effect.unwrap();
            let out = effect.process(0.5);
            assert!(
                out.is_finite(),
                "effect {:?} at {sr} Hz produced non-finite output: {out}",
                desc.id
            );
        }
    }
}

// ── 2. Unknown effect returns None ────────────────────────────────────────────

#[test]
fn unknown_effect_id_returns_none_from_create() {
    let registry = EffectRegistry::new();
    assert!(registry.create("nonexistent", 48_000.0).is_none());
}

#[test]
fn empty_string_id_returns_none_from_create() {
    let registry = EffectRegistry::new();
    assert!(registry.create("", 48_000.0).is_none());
}

#[test]
fn create_is_case_sensitive() {
    // Registry IDs are lowercase canonical strings; "Distortion" is not a valid ID.
    let registry = EffectRegistry::new();
    assert!(registry.create("Distortion", 48_000.0).is_none());
    assert!(registry.create("REVERB", 48_000.0).is_none());
}

// ── 3. Category filtering ──────────────────────────────────────────────────────

/// Every `EffectCategory` variant must have at least one registered effect.
#[test]
fn every_category_has_at_least_one_effect() {
    let registry = EffectRegistry::new();
    for category in [
        EffectCategory::Dynamics,
        EffectCategory::Distortion,
        EffectCategory::Modulation,
        EffectCategory::TimeBased,
        EffectCategory::Filter,
        EffectCategory::Utility,
    ] {
        let effects = registry.effects_in_category(category);
        assert!(
            !effects.is_empty(),
            "category {:?} ({}) has no registered effects",
            category,
            category.name()
        );
    }
}

/// The sum of per-category counts must equal the total registry length,
/// confirming every effect is assigned exactly one category.
#[test]
fn category_counts_sum_to_total() {
    let registry = EffectRegistry::new();
    let total = registry.len();

    let sum: usize = [
        EffectCategory::Dynamics,
        EffectCategory::Distortion,
        EffectCategory::Modulation,
        EffectCategory::TimeBased,
        EffectCategory::Filter,
        EffectCategory::Utility,
    ]
    .iter()
    .map(|&cat| registry.effects_in_category(cat).len())
    .sum();

    assert_eq!(
        sum, total,
        "sum of per-category counts ({sum}) != registry.len() ({total}); \
         an effect may belong to multiple categories or none"
    );
}

/// Every effect returned by `effects_in_category(C)` must report category `C`.
#[test]
fn effects_in_category_only_return_matching_category() {
    let registry = EffectRegistry::new();
    for category in [
        EffectCategory::Dynamics,
        EffectCategory::Distortion,
        EffectCategory::Modulation,
        EffectCategory::TimeBased,
        EffectCategory::Filter,
        EffectCategory::Utility,
    ] {
        for desc in registry.effects_in_category(category) {
            assert_eq!(
                desc.category, category,
                "effect {:?} is in category {:?} but was returned for {:?}",
                desc.id, desc.category, category
            );
        }
    }
}

// ── 4. Effect descriptors complete ────────────────────────────────────────────

/// Every descriptor must have a non-empty `id`, `name`, `short_name`, and `description`.
#[test]
fn all_descriptors_have_non_empty_string_fields() {
    let registry = EffectRegistry::new();
    for desc in registry.all_effects() {
        assert!(!desc.id.is_empty(), "descriptor has empty id");
        assert!(
            !desc.name.is_empty(),
            "descriptor {:?} has empty name",
            desc.id
        );
        assert!(
            !desc.short_name.is_empty(),
            "descriptor {:?} has empty short_name",
            desc.id
        );
        assert!(
            !desc.description.is_empty(),
            "descriptor {:?} has empty description",
            desc.id
        );
    }
}

/// `id` must be lowercase and contain no whitespace, matching registry lookup convention.
#[test]
fn all_descriptor_ids_are_lowercase_no_whitespace() {
    let registry = EffectRegistry::new();
    for desc in registry.all_effects() {
        assert_eq!(
            desc.id,
            desc.id.to_lowercase(),
            "descriptor id {:?} is not lowercase",
            desc.id
        );
        assert!(
            !desc.id.contains(' '),
            "descriptor id {:?} contains whitespace",
            desc.id
        );
    }
}

/// No two effects may share the same `id`.
#[test]
fn all_descriptor_ids_are_unique() {
    let registry = EffectRegistry::new();
    let effects = registry.all_effects();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for desc in &effects {
        assert!(
            seen.insert(desc.id),
            "duplicate effect id {:?} in registry",
            desc.id
        );
    }
}

/// `param_count` in the descriptor must exactly match the `ParameterInfo` implementation
/// of the live instance. A mismatch means the descriptor was not updated when parameters
/// were added or removed.
#[test]
fn descriptor_param_count_matches_live_instance() {
    let registry = EffectRegistry::new();
    for desc in registry.all_effects() {
        let effect = registry.create(desc.id, 48_000.0).unwrap();
        assert_eq!(
            desc.param_count,
            effect.effect_param_count(),
            "EffectDescriptor.param_count ({}) != live param_count ({}) for {:?}",
            desc.param_count,
            effect.effect_param_count(),
            desc.id
        );
    }
}

// ── 5. Per-parameter descriptor completeness ──────────────────────────────────

/// Every parameter descriptor on every effect must have:
/// - non-empty `name` and `short_name`
/// - `min` <= `default` <= `max` (valid range with default in-bounds)
#[test]
fn all_param_descriptors_have_valid_fields() {
    let registry = EffectRegistry::new();
    for effect_desc in registry.all_effects() {
        let effect = registry.create(effect_desc.id, 48_000.0).unwrap();
        let count = effect.effect_param_count();
        for i in 0..count {
            let param = effect.effect_param_info(i).unwrap_or_else(|| {
                panic!("effect {:?}: param_info({i}) returned None", effect_desc.id)
            });

            assert!(
                !param.name.is_empty(),
                "effect {:?} param[{i}] has empty name",
                effect_desc.id
            );
            assert!(
                !param.short_name.is_empty(),
                "effect {:?} param[{i}] ({:?}) has empty short_name",
                effect_desc.id,
                param.name
            );
            assert!(
                param.min <= param.max,
                "effect {:?} param[{i}] ({:?}): min ({}) > max ({})",
                effect_desc.id,
                param.name,
                param.min,
                param.max
            );
            assert!(
                param.default >= param.min && param.default <= param.max,
                "effect {:?} param[{i}] ({:?}): default ({}) out of range [{}, {}]",
                effect_desc.id,
                param.name,
                param.default,
                param.min,
                param.max
            );
        }
    }
}

// ── 6. `get()` / `descriptor()` consistency ───────────────────────────────────

/// `get(id)` and `descriptor(id)` must return the same data (they are aliases).
#[test]
fn get_and_descriptor_are_consistent() {
    let registry = EffectRegistry::new();
    for desc in registry.all_effects() {
        let via_get = registry
            .get(desc.id)
            .expect("get() returned None for registered id");
        let via_descriptor = registry
            .descriptor(desc.id)
            .expect("descriptor() returned None for registered id");

        // Both return references into the same backing entry, so pointer equality holds.
        assert_eq!(
            via_get as *const EffectDescriptor, via_descriptor as *const EffectDescriptor,
            "get() and descriptor() return different pointers for {:?}",
            desc.id
        );
    }
}

#[test]
fn get_returns_none_for_unknown_id() {
    let registry = EffectRegistry::new();
    assert!(registry.get("no_such_effect").is_none());
}

// ── 7. `param_index_by_name()` ────────────────────────────────────────────────

/// `param_index_by_name` must resolve a parameter by its exact `name` field.
#[test]
fn param_index_by_name_resolves_by_exact_name() {
    let registry = EffectRegistry::new();
    // Pick a well-known parameter: distortion "Drive"
    let idx = registry.param_index_by_name("distortion", "Drive");
    assert!(
        idx.is_some(),
        "param_index_by_name(\"distortion\", \"Drive\") returned None"
    );
    // The returned index must be in-bounds for the effect.
    let effect = registry.create("distortion", 48_000.0).unwrap();
    assert!(
        idx.unwrap() < effect.effect_param_count(),
        "param index {} is out of bounds (count={})",
        idx.unwrap(),
        effect.effect_param_count()
    );
}

/// Lookup must be case-insensitive (per the documented contract).
#[test]
fn param_index_by_name_is_case_insensitive() {
    let registry = EffectRegistry::new();
    let lower = registry.param_index_by_name("distortion", "drive");
    let upper = registry.param_index_by_name("distortion", "DRIVE");
    let mixed = registry.param_index_by_name("distortion", "Drive");
    assert!(lower.is_some(), "lowercase 'drive' not found");
    assert_eq!(lower, upper, "case-insensitive mismatch: lower vs upper");
    assert_eq!(lower, mixed, "case-insensitive mismatch: lower vs mixed");
}

/// Unknown parameter name must return `None`.
#[test]
fn param_index_by_name_returns_none_for_unknown_param() {
    let registry = EffectRegistry::new();
    assert!(
        registry
            .param_index_by_name("distortion", "nonexistent_param")
            .is_none()
    );
}

/// Unknown effect ID must return `None` even with a valid-looking param name.
#[test]
fn param_index_by_name_returns_none_for_unknown_effect() {
    let registry = EffectRegistry::new();
    assert!(
        registry
            .param_index_by_name("ghost_effect", "drive")
            .is_none()
    );
}

// ── 8. `default_chain_ids()` validity ────────────────────────────────────────

/// Every ID in `default_chain_ids()` must exist in the registry.
#[test]
fn default_chain_ids_all_exist_in_registry() {
    let registry = EffectRegistry::new();
    for &id in registry.default_chain_ids() {
        assert!(
            registry.get(id).is_some(),
            "default_chain_ids() contains {:?} which is not registered",
            id
        );
    }
}

/// `default_chain_ids()` must be non-empty and contain no duplicates.
#[test]
fn default_chain_ids_non_empty_and_no_duplicates() {
    let registry = EffectRegistry::new();
    let ids = registry.default_chain_ids();
    assert!(!ids.is_empty(), "default_chain_ids() is empty");

    let mut seen = std::collections::HashSet::new();
    for &id in ids {
        assert!(
            seen.insert(id),
            "default_chain_ids() contains duplicate entry {:?}",
            id
        );
    }
}

// ── 9. `len()` / `is_empty()` contract ───────────────────────────────────────

#[test]
fn len_equals_all_effects_count() {
    let registry = EffectRegistry::new();
    assert_eq!(
        registry.len(),
        registry.all_effects().len(),
        "registry.len() disagrees with all_effects().len()"
    );
}

#[test]
fn is_empty_false_for_populated_registry() {
    let registry = EffectRegistry::new();
    assert!(!registry.is_empty(), "registry should not be empty");
}

// ── 10. `Default` impl produces identical registry ───────────────────────────

/// `EffectRegistry::default()` must be equivalent to `EffectRegistry::new()`.
#[test]
fn default_impl_equivalent_to_new() {
    let via_new = EffectRegistry::new();
    let via_default = EffectRegistry::default();

    assert_eq!(
        via_new.len(),
        via_default.len(),
        "Default registry has different len than new()"
    );

    // Verify each ID exists in both and descriptors match structurally.
    for desc in via_new.all_effects() {
        let d2 = via_default
            .get(desc.id)
            .unwrap_or_else(|| panic!("Default registry missing {:?}", desc.id));
        assert_eq!(desc.id, d2.id);
        assert_eq!(desc.name, d2.name);
        assert_eq!(desc.category, d2.category);
        assert_eq!(desc.param_count, d2.param_count);
    }
}
