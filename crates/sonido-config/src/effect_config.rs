//! Effect configuration types.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Configuration for a single effect in a preset.
///
/// Each effect has a type identifier and optional parameters. Effects can be
/// bypassed by prefixing the type with `!` (e.g., `!reverb`).
///
/// # Example
///
/// ```rust
/// use sonido_config::EffectConfig;
///
/// let config = EffectConfig::new("distortion")
///     .with_param("drive", "0.7")
///     .with_param("tone", "0.5");
///
/// assert_eq!(config.effect_type, "distortion");
/// assert!(!config.bypassed);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EffectConfig {
    /// Effect type name (e.g., "distortion", "reverb").
    /// Use `!` prefix to bypass (e.g., "!reverb").
    #[serde(rename = "type")]
    pub effect_type: String,

    /// Whether the effect is bypassed.
    #[serde(default)]
    pub bypassed: bool,

    /// Effect parameters as key-value pairs.
    /// Values are strings to support various formats (numbers, percentages, etc.)
    #[serde(default)]
    pub params: HashMap<String, String>,
}

impl EffectConfig {
    /// Create a new effect configuration.
    ///
    /// If the type starts with `!`, the effect will be marked as bypassed.
    pub fn new(effect_type: impl Into<String>) -> Self {
        let type_str = effect_type.into();
        let (effect_type, bypassed) = if let Some(stripped) = type_str.strip_prefix('!') {
            (stripped.to_string(), true)
        } else {
            (type_str, false)
        };

        Self {
            effect_type,
            bypassed,
            params: HashMap::new(),
        }
    }

    /// Create a new bypassed effect configuration.
    pub fn new_bypassed(effect_type: impl Into<String>) -> Self {
        Self {
            effect_type: effect_type.into(),
            bypassed: true,
            params: HashMap::new(),
        }
    }

    /// Add a parameter to the configuration.
    pub fn with_param(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.params.insert(key.into(), value.into());
        self
    }

    /// Set whether the effect is bypassed.
    pub fn with_bypass(mut self, bypassed: bool) -> Self {
        self.bypassed = bypassed;
        self
    }

    /// Get a parameter value.
    pub fn get_param(&self, key: &str) -> Option<&str> {
        self.params.get(key).map(|s| s.as_str())
    }

    /// Set a parameter value.
    pub fn set_param(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.params.insert(key.into(), value.into());
    }

    /// Parse a parameter value as f32, in Sonido's parameter units
    /// (see [`parse_param_value`]): "-6dB" → -6.0, "50%" → 50.0,
    /// "100ms" → 100.0, "1.5s" → 1500.0, "1.2kHz" → 1200.0.
    pub fn parse_param(&self, key: &str) -> Option<f32> {
        let value = self.params.get(key)?;
        parse_param_value(value)
    }

    /// Get the canonical effect type (without bypass prefix).
    pub fn canonical_type(&self) -> &str {
        &self.effect_type
    }

    /// Get the effect type string for display (with ! prefix if bypassed).
    pub fn display_type(&self) -> String {
        if self.bypassed {
            format!("!{}", self.effect_type)
        } else {
            self.effect_type.clone()
        }
    }
}

/// Parse a parameter value string into an f32, in Sonido's parameter units.
///
/// Every Sonido parameter takes real units (dB, Hz, ms, 0–100 %), so the
/// suffix only says what the number already is; nothing is converted except
/// the scaled spellings:
/// - Plain numbers: "0.5", "20", "-6"
/// - Decibels: "-6dB", "+3 dB" → -6.0, 3.0 (dB params take dB)
/// - Percent: "50%" → 50.0 (percent params run 0–100)
/// - Milliseconds: "100ms" → 100.0; seconds: "1.5s" → 1500.0 (time params take ms)
/// - Frequency: "440Hz" → 440.0; "1.2kHz" → 1200.0
///
/// When the parameter is known, prefer [`ParamDescriptor::parse_value`]
/// (`sonido_core`), which also accepts step labels like `"Hard Clip"`.
///
/// [`ParamDescriptor::parse_value`]: sonido_core::ParamDescriptor::parse_value
pub fn parse_param_value(value: &str) -> Option<f32> {
    // A millisecond-unit descriptor gives "s" its ×1000 meaning.
    let as_ms = sonido_core::ParamDescriptor::custom("", "", f32::MIN, f32::MAX, 0.0)
        .with_unit(sonido_core::ParamUnit::Milliseconds);
    as_ms.parse_value(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_effect_config_new() {
        let config = EffectConfig::new("distortion");
        assert_eq!(config.effect_type, "distortion");
        assert!(!config.bypassed);
        assert_eq!(config.params.len(), 0);
    }

    #[test]
    fn test_effect_config_bypassed_prefix() {
        let config = EffectConfig::new("!reverb");
        assert_eq!(config.effect_type, "reverb");
        assert!(config.bypassed);
    }

    #[test]
    fn test_effect_config_with_params() {
        let config = EffectConfig::new("distortion")
            .with_param("drive", "0.7")
            .with_param("tone", "0.5");

        assert_eq!(config.get_param("drive"), Some("0.7"));
        assert_eq!(config.get_param("tone"), Some("0.5"));
        assert_eq!(config.get_param("missing"), None);
    }

    #[test]
    fn test_effect_config_display_type() {
        let normal = EffectConfig::new("distortion");
        assert_eq!(normal.display_type(), "distortion");

        let bypassed = EffectConfig::new("!reverb");
        assert_eq!(bypassed.display_type(), "!reverb");
    }

    #[test]
    fn test_parse_plain_numbers() {
        assert_eq!(parse_param_value("0.5"), Some(0.5));
        assert_eq!(parse_param_value("1.2"), Some(1.2));
        assert_eq!(parse_param_value("-0.3"), Some(-0.3));
        assert_eq!(parse_param_value("  0.5  "), Some(0.5));
    }

    #[test]
    fn test_parse_percentages() {
        // Percent params run 0–100: the suffix doesn't rescale.
        assert_eq!(parse_param_value("50%"), Some(50.0));
        assert_eq!(parse_param_value("100%"), Some(100.0));
        assert_eq!(parse_param_value("0%"), Some(0.0));
    }

    #[test]
    fn test_parse_decibels() {
        // dB params take dB: the suffix doesn't convert to linear gain.
        assert_eq!(parse_param_value("0dB"), Some(0.0));
        assert_eq!(parse_param_value("-6dB"), Some(-6.0));
        assert_eq!(parse_param_value("+6 dB"), Some(6.0));
        assert_eq!(parse_param_value("-6db"), Some(-6.0));
    }

    #[test]
    fn test_parse_time() {
        // Time params take ms: "ms" stays, "s" scales to ms.
        assert_eq!(parse_param_value("100ms"), Some(100.0));
        assert_eq!(parse_param_value("1000ms"), Some(1000.0));
        assert_eq!(parse_param_value("1.5s"), Some(1500.0));
        assert_eq!(parse_param_value("0.5s"), Some(500.0));
    }

    #[test]
    fn test_parse_frequency() {
        assert_eq!(parse_param_value("440Hz"), Some(440.0));
        assert_eq!(parse_param_value("440hz"), Some(440.0));
        assert_eq!(parse_param_value("1kHz"), Some(1000.0));
        assert_eq!(parse_param_value("1.5kHz"), Some(1500.0));
        assert_eq!(parse_param_value("2.2khz"), Some(2200.0));
    }

    #[test]
    fn test_parse_invalid() {
        assert_eq!(parse_param_value("invalid"), None);
        assert_eq!(parse_param_value("abc%"), None);
    }

    #[test]
    fn test_config_parse_param() {
        let config = EffectConfig::new("test")
            .with_param("gain", "-6dB")
            .with_param("time", "100ms");

        // Values come back in parameter units: dB and ms, not linear/seconds.
        assert_eq!(config.parse_param("gain"), Some(-6.0));
        assert_eq!(config.parse_param("time"), Some(100.0));

        assert!(config.parse_param("missing").is_none());
    }

    #[test]
    fn test_serde_roundtrip() {
        let config = EffectConfig::new("distortion")
            .with_param("drive", "0.7")
            .with_bypass(true);

        let toml_str = toml::to_string(&config).unwrap();
        let parsed: EffectConfig = toml::from_str(&toml_str).unwrap();

        assert_eq!(parsed.effect_type, "distortion");
        assert!(parsed.bypassed);
        assert_eq!(parsed.get_param("drive"), Some("0.7"));
    }
}
