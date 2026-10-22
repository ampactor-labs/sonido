//! Preset → [`Patch`]: one conversion for every tool that turns a preset
//! chain into a rig.
//!
//! The CLI's `sonido patch export` and the graph-player plugin's factory rigs
//! both go through [`preset_to_patch`], so a preset resolves its parameters
//! the same way everywhere: by name, short name or string id
//! ([`ParamDescriptor::matches_key`](sonido_core::ParamDescriptor::matches_key)),
//! with values in the parameter's own units
//! ([`ParamDescriptor::parse_value`](sonido_core::ParamDescriptor::parse_value)),
//! clamped to range, over each effect's defaults.
//!
//! [`Patch`]: sonido_patch::Patch

use sonido_patch::{MAX_NODES, Patch, PatchEdge, PatchEndpoint, PatchNode};
use sonido_registry::{EffectRegistry, effect_uid};

use crate::Preset;

/// Why a preset can't become a patch.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PresetPatchError {
    /// More effects than a patch holds.
    #[error("a patch holds at most {MAX_NODES} effects; '{preset}' has {count}")]
    TooManyEffects {
        /// The preset's name.
        preset: String,
        /// Its effect count.
        count: usize,
    },
    /// An effect the registry doesn't know (or that has no stable UID).
    #[error("unknown effect '{0}'")]
    UnknownEffect(String),
    /// A topology other than `linear` or `parallel`.
    #[error(
        "unknown topology '{0}': use 'linear' or 'parallel' (other graphs are authored in the editor)"
    )]
    UnknownTopology(String),
}

/// A converted patch, plus what didn't carry over.
#[derive(Debug, Clone, PartialEq)]
pub struct PatchConversion {
    /// The patch.
    pub patch: Patch,
    /// One line per preset parameter that didn't carry over as written
    /// (unknown name, unparsable value, or out of range and clamped), for the
    /// caller to show or test against.
    pub warnings: Vec<String>,
}

/// Convert `preset` into a patch named `name`.
///
/// `topology` overrides the preset's own (`None` uses the preset's, which
/// defaults to linear). Each node starts from its effect's descriptor
/// defaults; the preset's values overlay them.
pub fn preset_to_patch(
    preset: &Preset,
    name: &str,
    topology: Option<&str>,
    registry: &EffectRegistry,
) -> Result<PatchConversion, PresetPatchError> {
    if preset.effects.len() > MAX_NODES {
        return Err(PresetPatchError::TooManyEffects {
            preset: preset.name.clone(),
            count: preset.effects.len(),
        });
    }

    let mut warnings = Vec::new();
    let mut nodes = Vec::with_capacity(preset.effects.len());
    for cfg in &preset.effects {
        let id = cfg.canonical_type();
        let uid = effect_uid(id).ok_or_else(|| PresetPatchError::UnknownEffect(id.to_owned()))?;
        let effect = registry
            .create(id, 48_000.0)
            .ok_or_else(|| PresetPatchError::UnknownEffect(id.to_owned()))?;

        let descs: Vec<_> = (0..effect.effect_param_count())
            .filter_map(|i| effect.effect_param_info(i))
            .collect();
        let mut params: Vec<f32> = descs.iter().map(|d| d.default).collect();
        for (key, text) in &cfg.params {
            match descs.iter().position(|d| d.matches_key(key)) {
                Some(i) => match descs[i].parse_value(text) {
                    Some(v) => {
                        let (lo, hi) = (descs[i].min, descs[i].max);
                        if !(lo..=hi).contains(&v) {
                            warnings.push(format!(
                                "{id}: '{key} = {text}' is outside {lo}..={hi}; clamped"
                            ));
                        }
                        params[i] = v.clamp(lo, hi);
                    }
                    None => warnings.push(format!("{id}: can't read '{key} = {text}'")),
                },
                None => warnings.push(format!("{id}: no parameter '{key}'")),
            }
        }
        let mut node = PatchNode::new(uid, params);
        node.bypassed = cfg.bypassed;
        nodes.push(node);
    }

    let topology = topology.or(preset.topology.as_deref()).unwrap_or("linear");
    let mut patch = Patch::new(name);
    patch.edges = build_edges(topology, nodes.len())?;
    patch.nodes = nodes;
    Ok(PatchConversion { patch, warnings })
}

/// The edge set for a named topology over `n` effect nodes.
pub fn build_edges(topology: &str, n: usize) -> Result<Vec<PatchEdge>, PresetPatchError> {
    match topology {
        "linear" => Ok(Patch::linear_chain(
            "",
            (0..n)
                .map(|i| PatchNode::new((i + 1) as u16, Vec::new()))
                .collect(),
        )
        .edges),
        "parallel" if n == 0 => Ok(vec![PatchEdge::new(
            PatchEndpoint::Input,
            PatchEndpoint::Output,
        )]),
        "parallel" => {
            let mut edges = vec![PatchEdge::new(
                PatchEndpoint::Input,
                PatchEndpoint::Split(0),
            )];
            for i in 0..n {
                edges.push(PatchEdge::new(
                    PatchEndpoint::Split(0),
                    PatchEndpoint::Node(i as u8),
                ));
                edges.push(PatchEdge::new(
                    PatchEndpoint::Node(i as u8),
                    PatchEndpoint::Merge(0),
                ));
            }
            edges.push(PatchEdge::new(
                PatchEndpoint::Merge(0),
                PatchEndpoint::Output,
            ));
            Ok(edges)
        }
        other => Err(PresetPatchError::UnknownTopology(other.to_owned())),
    }
}

/// Every factory preset as a patch (linear unless the preset says
/// otherwise), each named after its preset. The graph player ships these as
/// its built-in rigs.
pub fn factory_patches(registry: &EffectRegistry) -> Vec<PatchConversion> {
    crate::factory_presets()
        .iter()
        .filter_map(|p| preset_to_patch(p, &p.name, None, registry).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EffectConfig;

    #[test]
    fn overlays_named_values_in_parameter_units_onto_defaults() {
        let registry = EffectRegistry::new();
        let mut preset = Preset::new("t");
        preset.effects.push(
            EffectConfig::new("reverb")
                .with_param("room_size", "80")
                .with_param("Mix", "25%")
                .with_param("predelay", "nonsense")
                .with_param("bogus", "1"),
        );
        let out = preset_to_patch(&preset, "t", None, &registry).unwrap();
        let reverb = registry.create("reverb", 48_000.0).unwrap();
        let room = registry.param_index_by_name("reverb", "room_size").unwrap();
        let mix = registry.param_index_by_name("reverb", "mix").unwrap();
        assert_eq!(out.patch.nodes[0].params_a[room], 80.0);
        assert_eq!(out.patch.nodes[0].params_a[mix], 25.0);
        let untouched = (0..reverb.effect_param_count())
            .find(|&i| i != room && i != mix)
            .unwrap();
        assert_eq!(
            out.patch.nodes[0].params_a[untouched],
            reverb.effect_param_info(untouched).unwrap().default
        );
        assert_eq!(out.warnings.len(), 2, "{:?}", out.warnings);
    }

    #[test]
    fn topologies() {
        assert_eq!(build_edges("parallel", 2).unwrap().len(), 6);
        assert!(build_edges("linear", 3).is_ok());
        assert_eq!(
            build_edges("spiral", 1),
            Err(PresetPatchError::UnknownTopology("spiral".into()))
        );
    }

    /// Every factory preset converts, and every one of its parameters
    /// resolves and parses: nothing in a shipped preset is silently dropped.
    #[test]
    fn every_factory_preset_converts_cleanly() {
        let registry = EffectRegistry::new();
        let presets = crate::factory_presets();
        // factory_presets() skips TOML that fails to parse; none may.
        assert_eq!(
            presets.len(),
            crate::factory_preset_names().len(),
            "a factory preset failed to parse"
        );
        let converted = factory_patches(&registry);
        assert_eq!(converted.len(), presets.len());
        for c in &converted {
            assert!(c.warnings.is_empty(), "{}: {:?}", c.patch.name, c.warnings);
        }
    }
}
