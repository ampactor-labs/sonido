//! Preset management for saving and loading effect configurations.
//!
//! This module uses sonido_config::Preset for storage (TOML format) with
//! [`ParamBridge`] for real-time atomic parameter access in the audio thread.
//! Parameter mapping is fully generic — iterating bridge slots and descriptors
//! instead of hand-mapping individual fields.

#[cfg(not(target_arch = "wasm32"))]
use sonido_config::paths::{ensure_user_presets_dir, list_user_presets, user_presets_dir};
use sonido_config::{EffectConfig, Preset, factory_presets};
use sonido_gui_core::{ParamBridge, ParamIndex, SlotIndex};
use std::path::PathBuf;

/// Convert bridge parameters to a sonido_config::Preset.
///
/// Creates a preset that captures the current state of all parameters
/// by iterating over all slots and their descriptors in the bridge.
/// Parameter names are normalized to snake_case for the preset file format.
pub fn params_to_preset(name: &str, description: Option<&str>, bridge: &dyn ParamBridge) -> Preset {
    let mut preset = Preset::new(name);

    if let Some(desc) = description {
        preset = preset.with_description(desc);
    }

    for slot_raw in 0..bridge.slot_count() {
        let slot = SlotIndex(slot_raw);
        let effect_id = bridge.effect_id(slot);
        let mut config = EffectConfig::new(effect_id).with_bypass(bridge.is_bypassed(slot));

        for p_raw in 0..bridge.param_count(slot) {
            let p = ParamIndex(p_raw);
            if let Some(desc) = bridge.param_descriptor(slot, p) {
                config =
                    config.with_param(to_snake_case(desc.name), format!("{}", bridge.get(slot, p)));
            }
        }

        preset = preset.with_effect(config);
    }

    preset
}

/// Apply a sonido_config::Preset to a ParamBridge.
///
/// Matches effects by type and parameters by name. For each slot in the
/// bridge, finds the corresponding effect config in the preset and applies
/// its values. Missing effects or parameters are silently skipped.
///
/// Name matching is flexible: "Room Size", "room_size", and "roomsize" all
/// match the same parameter. Legacy aliases (intensity→Depth, warmth→Saturation)
/// are tried when a direct match fails.
pub fn preset_to_params(preset: &Preset, bridge: &dyn ParamBridge) {
    for slot_raw in 0..bridge.slot_count() {
        let slot = SlotIndex(slot_raw);
        let effect_id = bridge.effect_id(slot);
        let config = preset
            .effects
            .iter()
            .find(|e| effect_type_matches(&e.effect_type, effect_id));

        if let Some(config) = config {
            bridge.set_bypassed(slot, config.bypassed);

            for p_raw in 0..bridge.param_count(slot) {
                let p = ParamIndex(p_raw);
                if let Some(desc) = bridge.param_descriptor(slot, p)
                    && let Some(v) = find_param_in_config(config, desc.name)
                {
                    bridge.set(slot, p, v);
                }
            }
        }
    }
}

/// Look up a parameter value in the config by descriptor name.
///
/// Tries normalized match first, then falls back to legacy aliases.
fn find_param_in_config(config: &EffectConfig, descriptor_name: &str) -> Option<f32> {
    find_by_normalized_key(config, descriptor_name).or_else(|| {
        let alias = param_alias(descriptor_name);
        if alias.is_empty() {
            None
        } else {
            find_by_normalized_key(config, alias)
        }
    })
}

/// Find a parameter value using normalized key matching.
///
/// Strips case and separators so "Pre-Delay", "pre_delay", and "predelay"
/// all match the same entry in the config HashMap.
fn find_by_normalized_key(config: &EffectConfig, name: &str) -> Option<f32> {
    let target = normalize_key(name);
    config
        .params
        .iter()
        .find(|(k, _)| normalize_key(k) == target)
        .and_then(|(k, _)| config.parse_param(k))
}

/// Normalize a parameter name for matching: lowercase, strip separators.
fn normalize_key(name: &str) -> String {
    name.to_lowercase().replace([' ', '-', '_'], "")
}

/// Convert a descriptor name to snake_case for preset serialization.
///
/// "Room Size" → "room_size", "Pre-Delay" → "pre_delay", "Drive" → "drive"
fn to_snake_case(name: &str) -> String {
    name.to_lowercase().replace([' ', '-'], "_")
}

/// Map a canonical ParameterInfo name to its legacy preset alias.
///
/// Some parameters were renamed in the ParameterInfo definitions but
/// old preset files still use the original names.
fn param_alias(descriptor_name: &str) -> &str {
    match descriptor_name {
        "Depth" => "intensity",   // vibrato renamed
        "Saturation" => "warmth", // tape renamed
        _ => "",
    }
}

/// Check if a preset effect type matches a bridge effect id.
///
/// Handles legacy effect type names for backward compatibility.
fn effect_type_matches(preset_type: &str, bridge_id: &str) -> bool {
    preset_type == bridge_id
        || matches!(
            (preset_type, bridge_id),
            ("parametriceq", "eq") | ("multivibrato", "vibrato")
        )
}

/// Preset entry for the manager.
#[derive(Debug, Clone)]
pub struct PresetEntry {
    /// The preset data.
    pub preset: Preset,
    /// Source: "factory", "user", or file path.
    pub source: PresetSource,
}

/// Where a preset came from.
#[derive(Debug, Clone, PartialEq)]
pub enum PresetSource {
    /// Built-in factory preset.
    Factory,
    /// User preset loaded from disk.
    User(PathBuf),
    /// Unsaved preset (created but not yet saved).
    Unsaved,
}

impl PresetEntry {
    /// Create a factory preset entry.
    pub fn factory(preset: Preset) -> Self {
        Self {
            preset,
            source: PresetSource::Factory,
        }
    }

    /// Create a user preset entry.
    pub fn user(preset: Preset, path: PathBuf) -> Self {
        Self {
            preset,
            source: PresetSource::User(path),
        }
    }

    /// Create an unsaved preset entry.
    pub fn unsaved(preset: Preset) -> Self {
        Self {
            preset,
            source: PresetSource::Unsaved,
        }
    }

    /// Check if this is a factory preset.
    pub fn is_factory(&self) -> bool {
        matches!(self.source, PresetSource::Factory)
    }

    /// Check if this is a user preset.
    pub fn is_user(&self) -> bool {
        matches!(self.source, PresetSource::User(_))
    }

    /// Get the file path if this is a user preset.
    pub fn path(&self) -> Option<&PathBuf> {
        match &self.source {
            PresetSource::User(p) => Some(p),
            _ => None,
        }
    }
}

/// Manager for loading and saving presets.
///
/// Uses sonido_config::Preset for storage (TOML format) while maintaining
/// compatibility with the GUI's [`ParamBridge`] for real-time parameter access.
pub struct PresetManager {
    /// All available presets (factory + user).
    presets: Vec<PresetEntry>,
    /// Index of the currently selected preset.
    current_preset: usize,
    /// Whether the current preset has been modified.
    modified: bool,
}

impl PresetManager {
    /// Create a new preset manager.
    ///
    /// Loads factory presets and any user presets from the user presets directory.
    pub fn new() -> Self {
        let mut manager = Self {
            presets: Vec::new(),
            current_preset: 0,
            modified: false,
        };

        manager.load_factory_presets();
        manager.load_user_presets();

        // Ensure we have at least one preset
        if manager.presets.is_empty() {
            let init = Preset::new("Init").with_description("Clean signal path");
            manager.presets.push(PresetEntry::unsaved(init));
        }

        manager
    }

    /// Load factory presets from sonido_config.
    fn load_factory_presets(&mut self) {
        for preset in factory_presets() {
            self.presets.push(PresetEntry::factory(preset));
        }
    }

    /// Load user presets from the user presets directory (native only).
    #[cfg(not(target_arch = "wasm32"))]
    fn load_user_presets(&mut self) {
        for path in list_user_presets() {
            match Preset::load(&path) {
                Ok(preset) => {
                    self.presets.push(PresetEntry::user(preset, path));
                }
                Err(e) => {
                    tracing::warn!(path = ?path, error = %e, "failed to load preset");
                }
            }
        }
    }

    /// No user presets on wasm (no filesystem).
    #[cfg(target_arch = "wasm32")]
    fn load_user_presets(&mut self) {}

    /// Get all presets.
    pub fn presets(&self) -> &[PresetEntry] {
        &self.presets
    }

    /// Get the current preset index.
    pub fn current_preset(&self) -> usize {
        self.current_preset
    }

    /// Get the current preset.
    pub fn current(&self) -> Option<&PresetEntry> {
        self.presets.get(self.current_preset)
    }

    /// Select a preset by index and apply it to the parameters.
    pub fn select(&mut self, index: usize, bridge: &dyn ParamBridge) {
        if index < self.presets.len() {
            self.current_preset = index;
            preset_to_params(&self.presets[index].preset, bridge);
            self.modified = false;
        }
    }

    /// Mark the current preset as modified.
    pub fn mark_modified(&mut self) {
        self.modified = true;
    }

    /// Check if the current preset has been modified.
    pub fn is_modified(&self) -> bool {
        self.modified
    }

    /// Save the current parameters as a new preset.
    ///
    /// The preset is saved to the user presets directory as a TOML file.
    /// Not available on wasm (no filesystem).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn save_as(
        &mut self,
        name: &str,
        description: Option<&str>,
        bridge: &dyn ParamBridge,
    ) -> Result<(), String> {
        let preset = params_to_preset(name, description, bridge);

        ensure_user_presets_dir()
            .map_err(|e| format!("Failed to create presets directory: {}", e))?;

        let filename = format!("{}.toml", name.to_lowercase().replace(' ', "_"));
        let path = user_presets_dir().join(&filename);

        preset
            .save(&path)
            .map_err(|e| format!("Failed to save preset: {}", e))?;

        tracing::info!(name, "preset saved");
        self.presets.push(PresetEntry::user(preset, path));
        self.current_preset = self.presets.len() - 1;
        self.modified = false;

        Ok(())
    }

    /// Overwrite the current user preset with updated parameters.
    ///
    /// Only works for user presets, not factory presets.
    /// Not available on wasm (no filesystem).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn save_current(&mut self, bridge: &dyn ParamBridge) -> Result<(), String> {
        let entry = self
            .presets
            .get(self.current_preset)
            .ok_or_else(|| "No preset selected".to_string())?;

        let path = match &entry.source {
            PresetSource::User(p) => p.clone(),
            PresetSource::Factory => {
                return Err("Cannot overwrite factory preset. Use 'Save As' instead.".to_string());
            }
            PresetSource::Unsaved => {
                return Err("Preset has not been saved yet. Use 'Save As'.".to_string());
            }
        };

        let preset = params_to_preset(
            &entry.preset.name,
            entry.preset.description.as_deref(),
            bridge,
        );

        preset
            .save(&path)
            .map_err(|e| format!("Failed to save preset: {}", e))?;

        self.presets[self.current_preset] = PresetEntry::user(preset, path);
        self.modified = false;

        Ok(())
    }

    /// Delete a user preset.
    ///
    /// Only works for user presets, not factory presets.
    /// Not available on wasm (no filesystem).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn delete(&mut self, index: usize) -> Result<(), String> {
        if index >= self.presets.len() {
            return Err("Invalid preset index".to_string());
        }

        let entry = &self.presets[index];
        let path = match &entry.source {
            PresetSource::User(p) => p.clone(),
            PresetSource::Factory => {
                return Err("Cannot delete factory preset".to_string());
            }
            PresetSource::Unsaved => {
                self.presets.remove(index);
                if self.current_preset >= self.presets.len() && self.current_preset > 0 {
                    self.current_preset -= 1;
                }
                return Ok(());
            }
        };

        std::fs::remove_file(&path).map_err(|e| format!("Failed to delete preset file: {}", e))?;

        self.presets.remove(index);
        if self.current_preset >= self.presets.len() && self.current_preset > 0 {
            self.current_preset -= 1;
        }

        Ok(())
    }

    /// Reload all presets from disk.
    pub fn reload(&mut self) {
        let current_name = self.current().map(|e| e.preset.name.clone());

        self.presets.clear();
        self.load_factory_presets();
        self.load_user_presets();

        // Try to restore selection by name
        if let Some(name) = current_name {
            if let Some(idx) = self.presets.iter().position(|e| e.preset.name == name) {
                self.current_preset = idx;
            } else {
                self.current_preset = 0;
            }
        } else {
            self.current_preset = 0;
        }

        self.modified = false;
    }

    /// Get the user presets directory path (native only).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn presets_dir() -> PathBuf {
        user_presets_dir()
    }
}

impl Default for PresetManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atomic_param_bridge::AtomicParamBridge;
    use sonido_registry::EffectRegistry;

    /// Find a parameter index by descriptor name within a bridge slot.
    fn find_param(bridge: &AtomicParamBridge, slot: SlotIndex, name: &str) -> Option<ParamIndex> {
        (0..bridge.param_count(slot)).find_map(|i| {
            let p = ParamIndex(i);
            bridge
                .param_descriptor(slot, p)
                .is_some_and(|d| d.name == name)
                .then_some(p)
        })
    }

    #[test]
    fn test_preset_manager_new() {
        let manager = PresetManager::new();
        assert!(!manager.presets.is_empty());
        assert!(manager.presets[0].is_factory());
    }

    #[test]
    fn test_params_to_preset_roundtrip() {
        let registry = EffectRegistry::new();
        let bridge = AtomicParamBridge::new(&registry, &["distortion", "reverb"], 48000.0);

        let dist_drive = find_param(&bridge, SlotIndex(0), "Drive").unwrap();
        let reverb_decay = find_param(&bridge, SlotIndex(1), "Decay").unwrap();

        bridge.set(SlotIndex(0), dist_drive, 20.0);
        bridge.set(SlotIndex(1), reverb_decay, 0.7);
        bridge.set_bypassed(SlotIndex(1), true);

        // Convert to preset and apply to fresh bridge
        let preset = params_to_preset("Test", Some("Test preset"), &bridge);

        let bridge2 = AtomicParamBridge::new(&registry, &["distortion", "reverb"], 48000.0);
        preset_to_params(&preset, &bridge2);

        assert!((bridge2.get(SlotIndex(0), dist_drive) - 20.0).abs() < 0.01);
        assert!((bridge2.get(SlotIndex(1), reverb_decay) - 0.7).abs() < 0.01);
        assert!(bridge2.is_bypassed(SlotIndex(1)));
        assert!(!bridge2.is_bypassed(SlotIndex(0)));
    }

    #[test]
    fn test_unknown_effects_ignored() {
        let registry = EffectRegistry::new();
        let bridge = AtomicParamBridge::new(&registry, &["distortion"], 48000.0);

        // Use lowercase key as old presets would
        let preset = Preset::new("Test")
            .with_effect(EffectConfig::new("nonexistent").with_param("foo", "1.0"))
            .with_effect(EffectConfig::new("distortion").with_param("drive", "15.0"));

        // Should not panic — unknown effect silently skipped
        preset_to_params(&preset, &bridge);

        let drive_idx = find_param(&bridge, SlotIndex(0), "Drive").unwrap();
        assert!((bridge.get(SlotIndex(0), drive_idx) - 15.0).abs() < 0.01);
    }

    #[test]
    fn test_param_alias_resolution() {
        let registry = EffectRegistry::new();
        let bridge = AtomicParamBridge::new(&registry, &["vibrato"], 48000.0);

        // Old preset uses "intensity" instead of "depth"
        let preset = Preset::new("Legacy")
            .with_effect(EffectConfig::new("vibrato").with_param("intensity", "80"));

        preset_to_params(&preset, &bridge);

        if let Some(idx) = find_param(&bridge, SlotIndex(0), "Depth") {
            assert!((bridge.get(SlotIndex(0), idx) - 80.0).abs() < 0.01);
        }
    }

    #[test]
    fn test_effect_type_alias() {
        let registry = EffectRegistry::new();
        let bridge = AtomicParamBridge::new(&registry, &["eq"], 48000.0);

        // Old preset uses "parametriceq" instead of "eq"
        let preset = Preset::new("Legacy EQ")
            .with_effect(EffectConfig::new("parametriceq").with_bypass(true));

        preset_to_params(&preset, &bridge);
        assert!(bridge.is_bypassed(SlotIndex(0)));
    }

    #[test]
    fn test_old_snake_case_presets_load() {
        let registry = EffectRegistry::new();
        let bridge = AtomicParamBridge::new(&registry, &["reverb"], 48000.0);

        // Old presets used snake_case: "room_size", "predelay"
        let preset = Preset::new("Old Format").with_effect(
            EffectConfig::new("reverb")
                .with_param("room_size", "0.9")
                .with_param("predelay", "25.0"),
        );

        preset_to_params(&preset, &bridge);

        if let Some(idx) = find_param(&bridge, SlotIndex(0), "Room Size") {
            assert!((bridge.get(SlotIndex(0), idx) - 0.9).abs() < 0.01);
        }
        if let Some(idx) = find_param(&bridge, SlotIndex(0), "Pre-Delay") {
            assert!((bridge.get(SlotIndex(0), idx) - 25.0).abs() < 0.01);
        }
    }

    #[test]
    fn test_preset_entry_sources() {
        let preset = Preset::new("Test");

        let factory = PresetEntry::factory(preset.clone());
        assert!(factory.is_factory());
        assert!(!factory.is_user());
        assert!(factory.path().is_none());

        let user = PresetEntry::user(preset.clone(), PathBuf::from("/test.toml"));
        assert!(!user.is_factory());
        assert!(user.is_user());
        assert!(user.path().is_some());

        let unsaved = PresetEntry::unsaved(preset);
        assert!(!unsaved.is_factory());
        assert!(!unsaved.is_user());
    }
}
