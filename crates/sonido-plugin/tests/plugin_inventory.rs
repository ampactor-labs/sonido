//! Every registered effect ships as a plugin in the bundle, and every plugin
//! in the bundle wraps a registered effect.
//!
//! The plugin list is `bundle::CATALOG`. This test keeps it in lockstep with
//! `EffectRegistry`, so a kernel added to the registry cannot silently miss
//! its CLAP/VST3 plugin, and a plugin ID can never be reused or reshaped.

use std::collections::BTreeSet;

use sonido_plugin::bundle::CATALOG;
use sonido_registry::EffectRegistry;

#[test]
fn every_registered_effect_has_a_plugin() {
    let registry = EffectRegistry::new();
    let shipped: BTreeSet<&str> = CATALOG.iter().map(|p| p.effect_id).collect();
    let missing: Vec<&str> = registry
        .all_effects()
        .iter()
        .map(|d| d.id)
        .filter(|id| !shipped.contains(id))
        .collect();
    assert!(
        missing.is_empty(),
        "registered effects with no plugin: {missing:?}"
    );
    assert_eq!(
        CATALOG.len(),
        registry.all_effects().len(),
        "duplicate catalog entry"
    );
}

#[test]
fn every_plugin_wraps_a_registered_effect() {
    let registry = EffectRegistry::new();
    for p in CATALOG {
        assert!(
            registry.create(p.effect_id, 48_000.0).is_some(),
            "{} wraps unknown effect id {:?}",
            p.clap_id,
            p.effect_id
        );
    }
}

#[test]
fn plugin_ids_are_unique_and_stable_in_shape() {
    let mut seen = BTreeSet::new();
    for p in CATALOG {
        assert!(seen.insert(p.clap_id), "duplicate CLAP id {}", p.clap_id);
        // Hosts store these IDs in projects: com.sonido.<effect id, dashed>.
        assert_eq!(
            p.clap_id,
            format!("com.sonido.{}", p.effect_id.replace('_', "-")),
        );
        assert!(p.name.starts_with("Sonido "), "{} name", p.clap_id);
        assert!(!p.features.is_empty(), "{} has no CLAP features", p.clap_id);
    }
}
