//! Configuration and preset management for sonido audio effects.
//!
//! This crate provides a unified configuration system for the sonido DSP framework,
//! including preset loading/saving, effect chain configuration, and parameter validation.
//!
//! # Features
//!
//! - **Preset System**: Load and save effect presets from TOML files
//! - **Effect Chains**: Build chains of effects with parameter configuration
//! - **Validation**: Validate effect types and parameter ranges
//! - **Paths**: Platform-specific preset and config directories
//! - **Factory Presets**: Built-in presets for common use cases
//!
//! # Example
//!
//! ```rust,no_run
//! use sonido_config::{Preset, EffectConfig, user_presets_dir};
//!
//! // Load a preset from file
//! let preset = Preset::load("my_preset.toml").unwrap();
//!
//! // Create a preset programmatically. Parameter names match a descriptor's
//! // name or short name (case-insensitive); values are in the parameter's
//! // own units (dB, %, Hz, ...), not normalized.
//! let mut preset = Preset::new("My Preset");
//! preset.effects = vec![
//!     EffectConfig::new("distortion")
//!         .with_param("drive", "18")   // dB
//!         .with_param("tone", "-3"),   // dB
//!     EffectConfig::new("reverb")
//!         .with_param("room", "80")    // %
//!         .with_param("damping", "30"), // %
//! ];
//!
//! // Save to user presets directory
//! let path = user_presets_dir().join("my_preset.toml");
//! preset.save(&path).unwrap();
//! ```

mod chain;
mod effect_config;
mod error;
mod preset;

/// Platform-specific paths for presets and configuration.
pub mod patch;
pub mod paths;

/// Effect and preset validation.
pub mod validation;

/// Factory presets bundled with the library.
pub mod factory_presets;

pub use chain::EffectChain;
pub use effect_config::{EffectConfig, parse_param_value};
pub use error::ConfigError;
pub use factory_presets::{
    FACTORY_PRESET_NAMES, factory_preset_names, factory_presets, get_factory_preset,
    is_factory_preset,
};
pub use patch::{PatchConversion, PresetPatchError, preset_to_patch};
pub use paths::{
    ensure_user_config_dir, ensure_user_presets_dir, find_preset, list_all_presets,
    list_system_presets, list_user_presets, preset_name_from_path, system_presets_dir,
    user_config_dir, user_presets_dir,
};
pub use preset::{PRESET_VERSION, Preset, migrate_state, topology_byte};
pub use validation::{
    EffectValidator, ParamValidationInfo, ValidationError, ValidationResult, validate_effect,
    validate_effect_config, validate_effect_param, validate_preset,
};

/// Re-export commonly used types from sonido-registry
pub use sonido_registry::{EffectCategory, EffectDescriptor, EffectRegistry, EffectWithParams};
