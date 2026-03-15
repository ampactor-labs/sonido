//! Effect factory and parameter parsing.
//!
//! Re-exports shared logic from [`sonido_graph_dsl`] and provides CLI-specific
//! display types for the `sonido effects` command.

// Re-export shared types from sonido-graph-dsl
pub use sonido_graph_dsl::{EffectError, parse_chain};

use sonido_core::EffectWithParams;
use sonido_registry::EffectRegistry;
use std::collections::HashMap;

/// Create an effect with custom parameters.
///
/// Thin wrapper around [`sonido_graph_dsl::create_effect_with_params`] that
/// discards the resolved ID, preserving the CLI's existing return type.
pub fn create_effect_with_params(
    name: &str,
    sample_rate: f32,
    params: &HashMap<String, String>,
) -> Result<Box<dyn EffectWithParams + Send>, EffectError> {
    sonido_graph_dsl::create_effect_with_params(name, sample_rate, params).map(|(effect, _)| effect)
}

/// Information about an available effect (for display in `sonido effects`).
#[derive(Debug, Clone)]
pub struct EffectInfo {
    /// Effect name (registry ID).
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Parameter information.
    pub parameters: Vec<ParameterInfo>,
}

/// Information about an effect parameter (for display).
#[derive(Debug, Clone)]
pub struct ParameterInfo {
    /// Parameter short name.
    pub name: String,
    /// Human-readable description with range.
    pub description: String,
    /// Formatted default value.
    pub default: String,
    /// Formatted range string.
    pub range: String,
}

/// Get information about all available effects from the registry.
pub fn available_effects() -> Vec<EffectInfo> {
    let registry = EffectRegistry::new();
    registry
        .all_effects()
        .iter()
        .map(|desc| {
            let effect = registry.create(desc.id, 48000.0).unwrap();
            let params = (0..effect.effect_param_count())
                .filter_map(|i| {
                    let d = effect.effect_param_info(i)?;
                    Some(ParameterInfo {
                        name: d.short_name.to_lowercase(),
                        description: format!(
                            "{} ({} to {})",
                            d.name,
                            d.format_value(d.min),
                            d.format_value(d.max),
                        ),
                        default: d.format_value(d.default),
                        range: format!("{} .. {}", d.format_value(d.min), d.format_value(d.max)),
                    })
                })
                .collect();
            EffectInfo {
                name: desc.id.to_string(),
                description: desc.description.to_string(),
                parameters: params,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_create_effect_with_params() {
        let params = HashMap::new();
        let result = create_effect_with_params("distortion", 48000.0, &params);
        assert!(result.is_ok());

        let result = create_effect_with_params("unknown", 48000.0, &params);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_chain() {
        let chain = parse_chain("preamp:gain=6|distortion:drive=12|delay:time=300", 48000.0);
        assert!(chain.is_ok());
        assert_eq!(chain.unwrap().len(), 3);
    }

    #[test]
    fn available_effects_count() {
        assert_eq!(available_effects().len(), 20);
    }

    #[test]
    fn available_effects_names_match_registry() {
        let effects = available_effects();
        let names: Vec<&str> = effects.iter().map(|e| e.name.as_str()).collect();
        let expected = [
            "distortion",
            "compressor",
            "chorus",
            "delay",
            "flanger",
            "phaser",
            "filter",
            "vibrato",
            "tape",
            "preamp",
            "reverb",
            "tremolo",
            "gate",
            "wah",
            "eq",
            "limiter",
            "bitcrusher",
            "ringmod",
            "stage",
        ];
        for name in &expected {
            assert!(names.contains(name), "missing effect: {name}");
        }
    }

    #[test]
    fn effect_name_aliases_resolve() {
        let aliases = vec![
            ("lowpass", "filter"),
            ("vibrato", "vibrato"),
            ("multivibrato", "vibrato"),
            ("tapesaturation", "tape"),
            ("noisegate", "gate"),
            ("autowah", "wah"),
            ("cleanpreamp", "preamp"),
            ("parametriceq", "eq"),
            ("peq", "eq"),
            ("crusher", "bitcrusher"),
            ("ring", "ringmod"),
        ];
        for (alias, expected) in aliases {
            let params = HashMap::new();
            let result = create_effect_with_params(alias, 48000.0, &params);
            assert!(
                result.is_ok(),
                "alias '{}' should resolve to '{}'",
                alias,
                expected
            );
        }
    }

    #[test]
    fn all_effects_creatable_with_no_params() {
        let registry = EffectRegistry::new();
        for desc in registry.all_effects() {
            let params = HashMap::new();
            let result = create_effect_with_params(desc.id, 48000.0, &params);
            assert!(
                result.is_ok(),
                "effect '{}' should be creatable with no params",
                desc.id
            );
        }
    }
}
