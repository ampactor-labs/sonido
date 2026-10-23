//! Effect chain management.
//!
//! This module provides the `EffectChain` type which manages a chain of audio
//! effects that can be processed as a unit. Effects can be created from a preset
//! or built programmatically.
//!
//! # Example
//!
//! ```rust,no_run
//! use sonido_config::{EffectChain, Preset, EffectConfig};
//! use sonido_core::Effect;
//!
//! // Create from a preset
//! let preset = Preset::new("My Chain")
//!     .with_effect(EffectConfig::new("distortion").with_param("drive", "0.7"))
//!     .with_effect(EffectConfig::new("reverb"));
//!
//! let mut chain = EffectChain::from_preset(&preset, 48000.0).unwrap();
//!
//! // Process audio
//! let output = chain.process(0.5);
//! ```

use sonido_core::Effect;
use sonido_registry::{EffectRegistry, EffectWithParams};

use crate::effect_config::EffectConfig;
use crate::error::ConfigError;
use crate::preset::Preset;
use crate::validation::validate_effect;

/// An entry in the effect chain.
struct ChainEntry {
    /// The effect instance (supports both processing and parameter access).
    effect: Box<dyn EffectWithParams + Send>,
    /// Whether the effect is bypassed.
    bypassed: bool,
    /// The effect type name.
    effect_type: String,
}

/// A chain of effects that can be processed as a unit.
///
/// The chain processes audio through each effect in sequence, respecting
/// bypass states. It implements the `Effect` trait so it can be used
/// anywhere a single effect can be used.
pub struct EffectChain {
    /// Effects in the chain.
    entries: Vec<ChainEntry>,
    /// The current sample rate.
    sample_rate: f32,
    /// Registry for creating effects.
    registry: EffectRegistry,
}

impl EffectChain {
    /// Create a new empty effect chain.
    pub fn new(sample_rate: f32) -> Self {
        Self {
            entries: Vec::new(),
            sample_rate,
            registry: EffectRegistry::new(),
        }
    }

    /// Create an effect chain from a preset.
    ///
    /// Creates each effect in the preset and configures its parameters.
    /// Invalid effect types will result in an error.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::UnknownEffect` if an effect type is not recognized.
    pub fn from_preset(preset: &Preset, sample_rate: f32) -> Result<Self, ConfigError> {
        let mut chain = Self::new(sample_rate);

        for effect_config in &preset.effects {
            chain.add_effect_config(effect_config)?;
        }

        Ok(chain)
    }

    /// Create an effect chain from effect type strings.
    ///
    /// Effect types can be prefixed with `!` to bypass them.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use sonido_config::EffectChain;
    ///
    /// // Create a chain with distortion active and reverb bypassed
    /// let chain = EffectChain::from_effect_types(
    ///     &["distortion", "!reverb"],
    ///     48000.0
    /// ).unwrap();
    /// ```
    pub fn from_effect_types(types: &[&str], sample_rate: f32) -> Result<Self, ConfigError> {
        let mut chain = Self::new(sample_rate);

        for effect_type in types {
            let config = EffectConfig::new(*effect_type);
            chain.add_effect_config(&config)?;
        }

        Ok(chain)
    }

    /// Add an effect from a configuration.
    pub fn add_effect_config(&mut self, config: &EffectConfig) -> Result<(), ConfigError> {
        let effect_type = config.canonical_type();

        // Validate the effect type
        validate_effect(effect_type).map_err(ConfigError::Validation)?;

        // Create the effect
        let mut effect = self
            .registry
            .create(effect_type, self.sample_rate)
            .ok_or_else(|| ConfigError::UnknownEffect(effect_type.to_string()))?;

        // Apply parameters
        self.apply_params(&mut effect, config)?;

        self.entries.push(ChainEntry {
            effect,
            bypassed: config.bypassed,
            effect_type: effect_type.to_string(),
        });

        Ok(())
    }

    /// Apply parameters from config to an effect.
    fn apply_params(
        &self,
        effect: &mut Box<dyn EffectWithParams + Send>,
        config: &EffectConfig,
    ) -> Result<(), ConfigError> {
        // Same resolution everywhere (ParamDescriptor::matches_key /
        // parse_value): name, short name or string id; values in the
        // parameter's own units, clamped to its range.
        for (param_name, param_value) in &config.params {
            if let Some(idx) = self
                .registry
                .param_index_by_name(config.canonical_type(), param_name)
                && let Some(desc) = effect.effect_param_info(idx)
                && let Some(value) = desc.parse_value(param_value)
            {
                effect.effect_set_param(idx, value.clamp(desc.min, desc.max));
            }
        }

        Ok(())
    }

    /// Add an effect by type name.
    ///
    /// Use `!` prefix to add as bypassed.
    pub fn add_effect(&mut self, effect_type: &str) -> Result<(), ConfigError> {
        let config = EffectConfig::new(effect_type);
        self.add_effect_config(&config)
    }

    /// Add an already-created effect instance.
    pub fn add_effect_instance(
        &mut self,
        effect: Box<dyn EffectWithParams + Send>,
        effect_type: &str,
        bypassed: bool,
    ) {
        self.entries.push(ChainEntry {
            effect,
            bypassed,
            effect_type: effect_type.to_string(),
        });
    }

    /// Get the sample rate.
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Get the number of effects in the chain.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if the chain is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Get effect types in the chain (with ! prefix for bypassed).
    pub fn effect_types(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|e| {
                if e.bypassed {
                    format!("!{}", e.effect_type)
                } else {
                    e.effect_type.clone()
                }
            })
            .collect()
    }

    /// Check if an effect at the given index is bypassed.
    pub fn is_bypassed(&self, index: usize) -> Option<bool> {
        self.entries.get(index).map(|e| e.bypassed)
    }

    /// Set the bypass state for an effect at the given index.
    pub fn set_bypassed(&mut self, index: usize, bypassed: bool) -> bool {
        if let Some(entry) = self.entries.get_mut(index) {
            entry.bypassed = bypassed;
            true
        } else {
            false
        }
    }

    /// Toggle bypass for an effect at the given index.
    pub fn toggle_bypass(&mut self, index: usize) -> Option<bool> {
        self.entries.get_mut(index).map(|e| {
            e.bypassed = !e.bypassed;
            e.bypassed
        })
    }

    /// Remove an effect at the given index.
    pub fn remove(&mut self, index: usize) -> Option<Box<dyn EffectWithParams + Send>> {
        if index < self.entries.len() {
            Some(self.entries.remove(index).effect)
        } else {
            None
        }
    }

    /// Clear all effects from the chain.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Get the effect type at a given index.
    pub fn get_effect_type(&self, index: usize) -> Option<&str> {
        self.entries.get(index).map(|e| e.effect_type.as_str())
    }

    /// Get an effect reference by index.
    pub fn get_effect(&self, index: usize) -> Option<&(dyn EffectWithParams + Send)> {
        self.entries.get(index).map(|e| e.effect.as_ref())
    }

    /// Get a mutable effect reference by index.
    pub fn get_effect_mut(
        &mut self,
        index: usize,
    ) -> Option<&mut Box<dyn EffectWithParams + Send>> {
        self.entries.get_mut(index).map(|e| &mut e.effect)
    }
}

impl Effect for EffectChain {
    fn process(&mut self, input: f32) -> f32 {
        let mut sample = input;
        for entry in &mut self.entries {
            if !entry.bypassed {
                sample = entry.effect.process(sample);
            }
        }
        sample
    }

    fn process_block(&mut self, input: &[f32], output: &mut [f32]) {
        // Copy input to output first
        output.copy_from_slice(input);

        // Process through each non-bypassed effect
        for entry in &mut self.entries {
            if !entry.bypassed {
                // Use the effect's block processing if available
                // For in-place processing, we use a temporary buffer approach
                for sample in output.iter_mut() {
                    *sample = entry.effect.process(*sample);
                }
            }
        }
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        for entry in &mut self.entries {
            entry.effect.set_sample_rate(sample_rate);
        }
    }

    fn reset(&mut self) {
        for entry in &mut self.entries {
            entry.effect.reset();
        }
    }

    fn snap_params(&mut self) {
        for entry in &mut self.entries {
            entry.effect.snap_params();
        }
    }

    fn latency_samples(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| !e.bypassed)
            .map(|e| e.effect.latency_samples())
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_chain() {
        let chain = EffectChain::new(48000.0);
        assert_eq!(chain.len(), 0);
        assert_eq!(chain.len(), 0);
        assert_eq!(chain.sample_rate(), 48000.0);
    }

    #[test]
    fn test_add_effect() {
        let mut chain = EffectChain::new(48000.0);
        assert!(chain.add_effect("distortion").is_ok());
        assert_eq!(chain.len(), 1);
        assert!(!chain.is_bypassed(0).unwrap());
    }

    #[test]
    fn test_add_bypassed_effect() {
        let mut chain = EffectChain::new(48000.0);
        assert!(chain.add_effect("!reverb").is_ok());
        assert_eq!(chain.len(), 1);
        assert!(chain.is_bypassed(0).unwrap());
    }

    #[test]
    fn test_unknown_effect() {
        let mut chain = EffectChain::new(48000.0);
        let result = chain.add_effect("unknown_effect_xyz");
        assert!(result.is_err());
    }

    #[test]
    fn test_from_effect_types() {
        let chain =
            EffectChain::from_effect_types(&["distortion", "!reverb", "compressor"], 48000.0)
                .unwrap();

        assert_eq!(chain.len(), 3);
        assert!(!chain.is_bypassed(0).unwrap());
        assert!(chain.is_bypassed(1).unwrap());
        assert!(!chain.is_bypassed(2).unwrap());
    }

    #[test]
    fn test_effect_types() {
        let chain = EffectChain::from_effect_types(&["distortion", "!reverb"], 48000.0).unwrap();

        let types = chain.effect_types();
        assert_eq!(types, vec!["distortion", "!reverb"]);
    }

    #[test]
    fn test_toggle_bypass() {
        let mut chain = EffectChain::new(48000.0);
        chain.add_effect("distortion").unwrap();

        assert!(!chain.is_bypassed(0).unwrap());

        let new_state = chain.toggle_bypass(0);
        assert_eq!(new_state, Some(true));
        assert!(chain.is_bypassed(0).unwrap());

        let new_state = chain.toggle_bypass(0);
        assert_eq!(new_state, Some(false));
        assert!(!chain.is_bypassed(0).unwrap());
    }

    #[test]
    fn test_set_bypassed() {
        let mut chain = EffectChain::new(48000.0);
        chain.add_effect("distortion").unwrap();

        assert!(chain.set_bypassed(0, true));
        assert!(chain.is_bypassed(0).unwrap());

        assert!(chain.set_bypassed(0, false));
        assert!(!chain.is_bypassed(0).unwrap());

        // Invalid index
        assert!(!chain.set_bypassed(99, true));
    }

    #[test]
    fn test_process_passthrough() {
        let mut chain = EffectChain::new(48000.0);
        // Empty chain should pass through
        let output = chain.process(0.5);
        assert_eq!(output, 0.5);
    }

    #[test]
    fn test_process_with_effects() {
        let mut chain = EffectChain::new(48000.0);
        chain.add_effect("preamp").unwrap();

        // With preamp at default gain (1.0), should be roughly unity
        let output = chain.process(0.5);
        assert!(output.is_finite());
    }

    #[test]
    fn test_process_bypassed() {
        let mut chain = EffectChain::new(48000.0);
        chain.add_effect("distortion").unwrap();
        chain.set_bypassed(0, true);

        // Bypassed distortion should pass through
        let output = chain.process(0.5);
        assert_eq!(output, 0.5);
    }

    #[test]
    fn test_process_block() {
        let mut chain = EffectChain::new(48000.0);
        chain.add_effect("preamp").unwrap();

        let input = [0.1, 0.2, 0.3, 0.4];
        let mut output = [0.0; 4];

        chain.process_block(&input, &mut output);

        // Output should be finite and processed
        assert!(output.iter().all(|&s| s.is_finite()));
    }

    #[test]
    fn test_reset() {
        let mut chain = EffectChain::new(48000.0);
        chain.add_effect("delay").unwrap();

        // Process some samples
        for _ in 0..100 {
            chain.process(0.5);
        }

        // Reset should clear internal state
        chain.reset();
        // First sample after reset should be silence (no feedback yet)
        // This depends on the delay implementation
    }

    #[test]
    fn test_remove_effect() {
        let mut chain = EffectChain::new(48000.0);
        chain.add_effect("distortion").unwrap();
        chain.add_effect("reverb").unwrap();

        assert_eq!(chain.len(), 2);

        let removed = chain.remove(0);
        assert!(removed.is_some());
        assert_eq!(chain.len(), 1);
        assert_eq!(chain.get_effect_type(0), Some("reverb"));
    }

    #[test]
    fn test_clear() {
        let mut chain = EffectChain::new(48000.0);
        chain.add_effect("distortion").unwrap();
        chain.add_effect("reverb").unwrap();

        chain.clear();
        assert_eq!(chain.len(), 0);
    }

    #[test]
    fn test_latency() {
        let chain = EffectChain::new(48000.0);
        // Empty chain has no latency
        assert_eq!(chain.latency_samples(), 0);
    }

    #[test]
    fn test_set_sample_rate() {
        let mut chain = EffectChain::new(48000.0);
        chain.add_effect("distortion").unwrap();

        chain.set_sample_rate(96000.0);
        assert_eq!(chain.sample_rate(), 96000.0);
    }

    #[test]
    fn test_from_preset() {
        let preset = Preset::new("Test")
            .with_effect(EffectConfig::new("distortion"))
            .with_effect(EffectConfig::new("!reverb"));

        let chain = EffectChain::from_preset(&preset, 48000.0).unwrap();

        assert_eq!(chain.len(), 2);
        assert!(!chain.is_bypassed(0).unwrap());
        assert!(chain.is_bypassed(1).unwrap());
    }

    #[test]
    fn test_get_effect_type() {
        let mut chain = EffectChain::new(48000.0);
        chain.add_effect("distortion").unwrap();
        chain.add_effect("reverb").unwrap();

        assert_eq!(chain.get_effect_type(0), Some("distortion"));
        assert_eq!(chain.get_effect_type(1), Some("reverb"));
        assert_eq!(chain.get_effect_type(2), None);
    }
}
