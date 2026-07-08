//! Effect and preset validation.
//!
//! This module provides validation for effect types, parameter names, and parameter values.
//! It uses the sonido-registry to verify that effects exist and that parameters are within
//! their valid ranges.
//!
//! # Example
//!
//! ```rust
//! use sonido_config::{validate_effect, EffectValidator};
//!
//! // Validate that an effect type exists
//! validate_effect("distortion").expect("distortion should exist");
//!
//! // Use the validator for more complex validation
//! let validator = EffectValidator::new();
//! validator.validate_effect("reverb").expect("reverb should exist");
//! ```

use sonido_registry::EffectRegistry;
use std::collections::HashMap;
use thiserror::Error;

/// Validation error types.
#[derive(Debug, Error, Clone, PartialEq)]
pub enum ValidationError {
    /// Unknown effect type.
    #[error("unknown effect type: {0}")]
    UnknownEffect(String),

    /// Unknown parameter name.
    #[error("unknown parameter '{param}' for effect '{effect}'")]
    UnknownParameter {
        /// Name of the effect.
        effect: String,
        /// Name of the unrecognized parameter.
        param: String,
    },

    /// Parameter value out of range.
    #[error("parameter '{param}' value {value} out of range [{min}, {max}]")]
    OutOfRange {
        /// Name of the parameter.
        param: String,
        /// The value that was out of range.
        value: f32,
        /// Minimum allowed value.
        min: f32,
        /// Maximum allowed value.
        max: f32,
    },

    /// Invalid parameter format.
    #[error("invalid format for parameter '{param}': {reason}")]
    InvalidFormat {
        /// Name of the parameter.
        param: String,
        /// Description of the format error.
        reason: String,
    },

    /// Multiple validation errors.
    #[error("multiple validation errors: {}", .0.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("; "))]
    Multiple(Vec<ValidationError>),
}

/// Result type for validation operations.
pub type ValidationResult<T> = Result<T, ValidationError>;

/// Effect parameter metadata for validation.
#[derive(Debug, Clone)]
pub struct ParamValidationInfo {
    /// Parameter name (lowercase, used for lookup).
    pub name: String,
    /// Parameter index in the effect.
    pub index: usize,
    /// Minimum value.
    pub min: f32,
    /// Maximum value.
    pub max: f32,
    /// Default value.
    pub default: f32,
}

/// Validator for effects and their parameters.
///
/// The validator caches parameter information from effects to allow
/// efficient repeated validation without creating new effect instances.
pub struct EffectValidator {
    registry: EffectRegistry,
    /// Cached parameter info for each effect type.
    /// Maps effect_id -> `Vec<ParamValidationInfo>`
    param_cache: HashMap<String, Vec<ParamValidationInfo>>,
}

impl Default for EffectValidator {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectValidator {
    /// Create a new effect validator.
    pub fn new() -> Self {
        Self {
            registry: EffectRegistry::new(),
            param_cache: HashMap::new(),
        }
    }

    /// Get or create cached parameter info for an effect.
    ///
    /// Creates an effect instance via the registry and queries its
    /// [`EffectWithParams`] methods to discover parameters dynamically.
    fn get_params(&mut self, effect_id: &str) -> Option<&Vec<ParamValidationInfo>> {
        if !self.param_cache.contains_key(effect_id) {
            let effect = self.registry.create(effect_id, 48000.0)?;
            let count = effect.effect_param_count();
            let mut params = Vec::with_capacity(count);
            for i in 0..count {
                if let Some(desc) = effect.effect_param_info(i) {
                    params.push(ParamValidationInfo {
                        name: normalize_param_name(desc.name),
                        index: i,
                        min: desc.min,
                        max: desc.max,
                        default: desc.default,
                    });
                }
            }
            self.param_cache.insert(effect_id.to_string(), params);
        }
        self.param_cache.get(effect_id)
    }

    /// Validate that an effect type exists.
    pub fn validate_effect(&self, effect_type: &str) -> ValidationResult<()> {
        if self.registry.get(effect_type).is_some() {
            Ok(())
        } else {
            Err(ValidationError::UnknownEffect(effect_type.to_string()))
        }
    }

    /// Validate a parameter name for an effect.
    pub fn validate_param_name(
        &mut self,
        effect_type: &str,
        param_name: &str,
    ) -> ValidationResult<()> {
        // First check effect exists
        self.validate_effect(effect_type)?;

        let params = self
            .get_params(effect_type)
            .ok_or_else(|| ValidationError::UnknownEffect(effect_type.to_string()))?;

        let normalized_name = normalize_param_name(param_name);
        if params.iter().any(|p| p.name == normalized_name) {
            Ok(())
        } else {
            Err(ValidationError::UnknownParameter {
                effect: effect_type.to_string(),
                param: param_name.to_string(),
            })
        }
    }

    /// Validate a parameter value for an effect.
    pub fn validate_param_value(
        &mut self,
        effect_type: &str,
        param_name: &str,
        value: f32,
    ) -> ValidationResult<()> {
        // First check effect exists
        self.validate_effect(effect_type)?;

        let params = self
            .get_params(effect_type)
            .ok_or_else(|| ValidationError::UnknownEffect(effect_type.to_string()))?;

        let normalized_name = normalize_param_name(param_name);
        let param = params
            .iter()
            .find(|p| p.name == normalized_name)
            .ok_or_else(|| ValidationError::UnknownParameter {
                effect: effect_type.to_string(),
                param: param_name.to_string(),
            })?;

        if value >= param.min && value <= param.max {
            Ok(())
        } else {
            Err(ValidationError::OutOfRange {
                param: param_name.to_string(),
                value,
                min: param.min,
                max: param.max,
            })
        }
    }

    /// Get all valid effect type IDs.
    pub fn effect_ids(&self) -> Vec<&str> {
        self.registry.all_effects().iter().map(|e| e.id).collect()
    }

    /// Get parameter info for an effect type.
    pub fn effect_params(&mut self, effect_type: &str) -> Option<Vec<ParamValidationInfo>> {
        self.get_params(effect_type).cloned()
    }

    /// Find a parameter index by name for an effect type.
    ///
    /// Returns `None` if the effect or parameter is not found.
    pub fn find_param_index(&mut self, effect_type: &str, param_name: &str) -> Option<usize> {
        let params = self.get_params(effect_type)?;
        let normalized_name = normalize_param_name(param_name);
        params
            .iter()
            .find(|p| p.name == normalized_name)
            .map(|p| p.index)
    }

    /// Get parameter info by name for an effect type.
    ///
    /// Returns `None` if the effect or parameter is not found.
    pub fn get_param_info(
        &mut self,
        effect_type: &str,
        param_name: &str,
    ) -> Option<ParamValidationInfo> {
        let params = self.get_params(effect_type)?;
        let normalized_name = normalize_param_name(param_name);
        params.iter().find(|p| p.name == normalized_name).cloned()
    }

    /// Parse a parameter value from string.
    ///
    /// Handles special formats like "20dB", "500Hz", "50%", etc.
    pub fn parse_param_value(&self, param_name: &str, value_str: &str) -> ValidationResult<f32> {
        parse_param_value(param_name, value_str)
    }
}

/// Normalize a parameter name for consistent lookup.
///
/// Converts to lowercase and replaces spaces/underscores with underscores.
fn normalize_param_name(name: &str) -> String {
    name.to_lowercase().replace([' ', '-'], "_")
}

/// Parse a parameter value from a string.
///
/// Supports various formats:
/// - Plain numbers: "0.5", "20", "-6"
/// - With units: "20dB", "500Hz", "50%", "100ms"
/// - Percentages are converted to 0-100 range
pub fn parse_param_value(param_name: &str, value_str: &str) -> ValidationResult<f32> {
    let s = value_str.trim();

    // Try to parse with common unit suffixes
    if let Some(v) = s.strip_suffix("dB").or_else(|| s.strip_suffix("db")) {
        v.trim()
            .parse::<f32>()
            .map_err(|_| ValidationError::InvalidFormat {
                param: param_name.to_string(),
                reason: format!("cannot parse '{}' as number", v),
            })
    } else if let Some(v) = s.strip_suffix("Hz").or_else(|| s.strip_suffix("hz")) {
        v.trim()
            .parse::<f32>()
            .map_err(|_| ValidationError::InvalidFormat {
                param: param_name.to_string(),
                reason: format!("cannot parse '{}' as number", v),
            })
    } else if let Some(v) = s.strip_suffix("ms") {
        v.trim()
            .parse::<f32>()
            .map_err(|_| ValidationError::InvalidFormat {
                param: param_name.to_string(),
                reason: format!("cannot parse '{}' as number", v),
            })
    } else if let Some(v) = s.strip_suffix('%') {
        v.trim()
            .parse::<f32>()
            .map_err(|_| ValidationError::InvalidFormat {
                param: param_name.to_string(),
                reason: format!("cannot parse '{}' as number", v),
            })
    } else {
        // Plain number
        s.parse::<f32>()
            .map_err(|_| ValidationError::InvalidFormat {
                param: param_name.to_string(),
                reason: format!("cannot parse '{}' as number", s),
            })
    }
}

// get_effect_params removed — EffectValidator::get_params uses dynamic discovery via
// EffectWithParams trait. See get_params() above.
//

/// Validate an effect type exists in the registry.
///
/// This is a convenience function that creates a validator internally.
/// For repeated validation, use `EffectValidator` directly.
///
/// # Example
///
/// ```rust
/// use sonido_config::validate_effect;
///
/// assert!(validate_effect("distortion").is_ok());
/// assert!(validate_effect("unknown").is_err());
/// ```
pub fn validate_effect(effect_type: &str) -> ValidationResult<()> {
    let validator = EffectValidator::new();
    validator.validate_effect(effect_type)
}

/// Validate a parameter value for an effect.
///
/// This is a convenience function that creates a validator internally.
pub fn validate_effect_param(
    effect_type: &str,
    param_name: &str,
    value: f32,
) -> ValidationResult<()> {
    let mut validator = EffectValidator::new();
    validator.validate_param_value(effect_type, param_name, value)
}

/// Validate a preset's effects and parameters.
///
/// Checks that all effects in the preset exist and all parameters
/// are valid for their respective effects.
///
/// # Example
///
/// ```rust,no_run
/// use sonido_config::{Preset, validate_preset};
///
/// let preset = Preset::load("my_preset.toml").unwrap();
/// validate_preset(&preset).expect("preset should be valid");
/// ```
pub fn validate_preset(preset: &crate::Preset) -> ValidationResult<()> {
    let mut validator = EffectValidator::new();
    let mut errors = Vec::new();

    for effect_config in &preset.effects {
        // Validate effect type
        if let Err(e) = validator.validate_effect(&effect_config.effect_type) {
            errors.push(e);
            continue;
        }

        // Validate each parameter
        for (param_name, param_value) in &effect_config.params {
            // Parse the value
            let value = match validator.parse_param_value(param_name, param_value) {
                Ok(v) => v,
                Err(e) => {
                    errors.push(e);
                    continue;
                }
            };

            // Validate the value
            if let Err(e) =
                validator.validate_param_value(&effect_config.effect_type, param_name, value)
            {
                errors.push(e);
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else if errors.len() == 1 {
        Err(errors.pop().unwrap())
    } else {
        Err(ValidationError::Multiple(errors))
    }
}

/// Validate effect configuration parameters.
///
/// This validates a map of parameter name -> value string pairs
/// against an effect type.
pub fn validate_effect_config(
    effect_type: &str,
    params: &HashMap<String, String>,
) -> ValidationResult<()> {
    let mut validator = EffectValidator::new();
    let mut errors = Vec::new();

    // Validate effect type
    validator.validate_effect(effect_type)?;

    // Validate each parameter
    for (param_name, param_value) in params {
        // Parse the value
        let value = match validator.parse_param_value(param_name, param_value) {
            Ok(v) => v,
            Err(e) => {
                errors.push(e);
                continue;
            }
        };

        // Validate the value
        if let Err(e) = validator.validate_param_value(effect_type, param_name, value) {
            errors.push(e);
        }
    }

    if errors.is_empty() {
        Ok(())
    } else if errors.len() == 1 {
        Err(errors.pop().unwrap())
    } else {
        Err(ValidationError::Multiple(errors))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_known_effects() {
        let validator = EffectValidator::new();

        // All known effects should validate
        for effect_id in [
            "distortion",
            "compressor",
            "chorus",
            "flanger",
            "phaser",
            "delay",
            "filter",
            "vibrato",
            "tape",
            "preamp",
            "reverb",
            "tremolo",
            "gate",
            "wah",
            "eq",
        ] {
            assert!(
                validator.validate_effect(effect_id).is_ok(),
                "effect '{}' should be valid",
                effect_id
            );
        }
    }

    #[test]
    fn test_validate_unknown_effect() {
        let validator = EffectValidator::new();

        let result = validator.validate_effect("unknown_effect");
        assert!(matches!(result, Err(ValidationError::UnknownEffect(_))));
    }

    #[test]
    fn test_validate_param_name() {
        let mut validator = EffectValidator::new();

        // Valid parameter names
        assert!(validator.validate_param_name("distortion", "drive").is_ok());
        assert!(validator.validate_param_name("distortion", "tone").is_ok());
        assert!(
            validator
                .validate_param_name("distortion", "output")
                .is_ok()
        );
        assert!(validator.validate_param_name("reverb", "room_size").is_ok());
        assert!(validator.validate_param_name("reverb", "mix").is_ok());

        // Invalid parameter name
        let result = validator.validate_param_name("distortion", "unknown_param");
        assert!(matches!(
            result,
            Err(ValidationError::UnknownParameter { .. })
        ));
    }

    #[test]
    fn test_validate_param_value_in_range() {
        let mut validator = EffectValidator::new();

        // Valid values
        assert!(
            validator
                .validate_param_value("distortion", "drive", 0.0)
                .is_ok()
        );
        assert!(
            validator
                .validate_param_value("distortion", "drive", 20.0)
                .is_ok()
        );
        assert!(
            validator
                .validate_param_value("distortion", "drive", 40.0)
                .is_ok()
        );

        // Out of range
        let result = validator.validate_param_value("distortion", "drive", -1.0);
        assert!(matches!(result, Err(ValidationError::OutOfRange { .. })));

        let result = validator.validate_param_value("distortion", "drive", 50.0);
        assert!(matches!(result, Err(ValidationError::OutOfRange { .. })));
    }

    #[test]
    fn test_parse_param_value_plain_numbers() {
        assert_eq!(parse_param_value("test", "0.5").unwrap(), 0.5);
        assert_eq!(parse_param_value("test", "20").unwrap(), 20.0);
        assert_eq!(parse_param_value("test", "-6").unwrap(), -6.0);
        assert_eq!(parse_param_value("test", "1.234").unwrap(), 1.234);
    }

    #[test]
    fn test_parse_param_value_with_units() {
        assert_eq!(parse_param_value("test", "20dB").unwrap(), 20.0);
        assert_eq!(parse_param_value("test", "-6dB").unwrap(), -6.0);
        assert_eq!(parse_param_value("test", "500Hz").unwrap(), 500.0);
        assert_eq!(parse_param_value("test", "100ms").unwrap(), 100.0);
        assert_eq!(parse_param_value("test", "50%").unwrap(), 50.0);
    }

    #[test]
    fn test_parse_param_value_invalid() {
        let result = parse_param_value("test", "not_a_number");
        assert!(matches!(result, Err(ValidationError::InvalidFormat { .. })));

        let result = parse_param_value("test", "");
        assert!(matches!(result, Err(ValidationError::InvalidFormat { .. })));
    }

    #[test]
    fn test_normalize_param_name() {
        assert_eq!(normalize_param_name("Drive"), "drive");
        assert_eq!(normalize_param_name("room_size"), "room_size");
        assert_eq!(normalize_param_name("Room Size"), "room_size");
        assert_eq!(normalize_param_name("pre-delay"), "pre_delay");
    }

    #[test]
    fn test_effect_ids() {
        let validator = EffectValidator::new();
        let ids = validator.effect_ids();

        assert!(ids.contains(&"distortion"));
        assert!(ids.contains(&"reverb"));
        assert!(ids.contains(&"harmonic_habitat"));
        assert!(ids.contains(&"compressor"));
        assert_eq!(ids.len(), 36); // 36 effects registered
    }

    #[test]
    fn test_effect_params() {
        let mut validator = EffectValidator::new();

        let params = validator.effect_params("distortion").unwrap();
        assert_eq!(params.len(), 6);
        assert_eq!(params[0].name, "drive");
        assert_eq!(params[0].min, 0.0);
        assert_eq!(params[0].max, 40.0);

        let params = validator.effect_params("reverb").unwrap();
        assert_eq!(params.len(), 8);
    }

    #[test]
    fn test_validate_effect_config_valid() {
        let mut params = HashMap::new();
        params.insert("drive".to_string(), "20".to_string());
        params.insert("tone".to_string(), "5".to_string());
        params.insert("output".to_string(), "-6dB".to_string());

        assert!(validate_effect_config("distortion", &params).is_ok());
    }

    #[test]
    fn test_validate_effect_config_invalid_param() {
        let mut params = HashMap::new();
        params.insert("unknown".to_string(), "20".to_string());

        let result = validate_effect_config("distortion", &params);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_effect_config_out_of_range() {
        let mut params = HashMap::new();
        params.insert("drive".to_string(), "100".to_string()); // max is 40

        let result = validate_effect_config("distortion", &params);
        assert!(matches!(result, Err(ValidationError::OutOfRange { .. })));
    }

    #[test]
    fn test_validation_error_display() {
        let err = ValidationError::UnknownEffect("foo".to_string());
        assert_eq!(err.to_string(), "unknown effect type: foo");

        let err = ValidationError::UnknownParameter {
            effect: "dist".to_string(),
            param: "foo".to_string(),
        };
        assert_eq!(err.to_string(), "unknown parameter 'foo' for effect 'dist'");

        let err = ValidationError::OutOfRange {
            param: "drive".to_string(),
            value: 50.0,
            min: 0.0,
            max: 40.0,
        };
        assert_eq!(
            err.to_string(),
            "parameter 'drive' value 50 out of range [0, 40]"
        );
    }

    #[test]
    fn test_multiple_validation_errors() {
        let errors = vec![
            ValidationError::UnknownEffect("foo".to_string()),
            ValidationError::UnknownParameter {
                effect: "bar".to_string(),
                param: "baz".to_string(),
            },
        ];
        let multi = ValidationError::Multiple(errors);

        let msg = multi.to_string();
        assert!(msg.contains("unknown effect type: foo"));
        assert!(msg.contains("unknown parameter 'baz'"));
    }

    #[test]
    fn test_convenience_functions() {
        // validate_effect
        assert!(validate_effect("distortion").is_ok());
        assert!(validate_effect("unknown").is_err());

        // validate_effect_param
        assert!(validate_effect_param("distortion", "drive", 20.0).is_ok());
        assert!(validate_effect_param("distortion", "drive", 100.0).is_err());
    }

    #[test]
    fn test_find_param_index() {
        let mut validator = EffectValidator::new();

        // Valid lookups
        assert_eq!(validator.find_param_index("distortion", "drive"), Some(0));
        assert_eq!(validator.find_param_index("distortion", "tone"), Some(1));
        assert_eq!(validator.find_param_index("distortion", "output"), Some(2));
        assert_eq!(validator.find_param_index("reverb", "mix"), Some(4));

        // Case-insensitive
        assert_eq!(validator.find_param_index("distortion", "Drive"), Some(0));

        // Unknown param
        assert_eq!(validator.find_param_index("distortion", "unknown"), None);

        // Unknown effect
        assert_eq!(validator.find_param_index("unknown_effect", "drive"), None);
    }

    #[test]
    fn test_get_param_info() {
        let mut validator = EffectValidator::new();

        let info = validator.get_param_info("distortion", "drive").unwrap();
        assert_eq!(info.name, "drive");
        assert_eq!(info.index, 0);
        assert_eq!(info.min, 0.0);
        assert_eq!(info.max, 40.0);

        assert!(validator.get_param_info("distortion", "unknown").is_none());
        assert!(
            validator
                .get_param_info("unknown_effect", "drive")
                .is_none()
        );
    }

    #[test]
    fn test_all_effects_have_params() {
        let mut validator = EffectValidator::new();

        // Collect effect_ids first to avoid borrow issues
        let effect_ids: Vec<String> = validator
            .effect_ids()
            .iter()
            .map(|s| s.to_string())
            .collect();

        for effect_id in &effect_ids {
            let params = validator.effect_params(effect_id);
            assert!(
                params.is_some(),
                "effect '{}' should have params",
                effect_id
            );
            assert!(
                !params.unwrap().is_empty(),
                "effect '{}' should have at least one param",
                effect_id
            );
        }
    }

    #[test]
    fn test_param_ranges_are_sensible() {
        let mut validator = EffectValidator::new();

        // Collect effect_ids first to avoid borrow issues
        let effect_ids: Vec<String> = validator
            .effect_ids()
            .iter()
            .map(|s| s.to_string())
            .collect();

        for effect_id in &effect_ids {
            if let Some(params) = validator.effect_params(effect_id) {
                for param in params {
                    // Min should be less than or equal to max
                    assert!(
                        param.min <= param.max,
                        "effect '{}' param '{}': min {} > max {}",
                        effect_id,
                        param.name,
                        param.min,
                        param.max
                    );

                    // Default should be in range
                    assert!(
                        param.default >= param.min && param.default <= param.max,
                        "effect '{}' param '{}': default {} not in range [{}, {}]",
                        effect_id,
                        param.name,
                        param.default,
                        param.min,
                        param.max
                    );
                }
            }
        }
    }
}
