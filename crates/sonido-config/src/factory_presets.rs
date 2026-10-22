//! Factory presets bundled with the sonido library.
//!
//! This module provides built-in presets that are always available without
//! requiring external files. These presets demonstrate common effect configurations
//! and serve as starting points for users.

use crate::Preset;

/// Array of factory preset names for external access.
pub static FACTORY_PRESET_NAMES: &[&str] = &[
    "init",
    "crunch",
    "high_gain",
    "ambient",
    "tape_warmth",
    "clean_studio",
    "80s_chorus",
    "slapback",
];

/// TOML content for factory presets.
///
/// These are embedded at compile time and always available.
static FACTORY_PRESETS_TOML: &[(&str, &str)] = &[
    ("init", INIT_PRESET),
    ("crunch", CRUNCH_PRESET),
    ("high_gain", HIGH_GAIN_PRESET),
    ("ambient", AMBIENT_PRESET),
    ("tape_warmth", TAPE_WARMTH_PRESET),
    ("clean_studio", CLEAN_STUDIO_PRESET),
    ("80s_chorus", EIGHTIES_CHORUS_PRESET),
    ("slapback", SLAPBACK_PRESET),
];

/// Initialization preset - clean signal path.
const INIT_PRESET: &str = r#"
name = "Init"
description = "Clean signal path - all effects bypassed"
sample_rate = 48000

[[effects]]
type = "preamp"
bypassed = true
[effects.params]
gain = "0"

[[effects]]
type = "distortion"
bypassed = true
[effects.params]
drive = "15"
tone = "0"
output = "-6"

[[effects]]
type = "compressor"
bypassed = true
[effects.params]
threshold = "-20"
ratio = "4"
attack = "10"
release = "100"
makeup = "0"

[[effects]]
type = "chorus"
bypassed = true
[effects.params]
rate = "1"
depth = "50"
mix = "50"

[[effects]]
type = "delay"
bypassed = true
[effects.params]
time = "300"
feedback = "40"
mix = "50"

[[effects]]
type = "reverb"
bypassed = true
[effects.params]
room_size = "50"
decay = "50"
damping = "50"
predelay = "10"
mix = "30"
"#;

/// Crunch preset - light overdrive.
const CRUNCH_PRESET: &str = r#"
name = "Crunch"
description = "Light overdrive - great for blues and rock rhythm"
sample_rate = 48000

[[effects]]
type = "preamp"
[effects.params]
gain = "3"

[[effects]]
type = "distortion"
[effects.params]
drive = "12"
tone = "2"
output = "-3"

[[effects]]
type = "compressor"
bypassed = true
[effects.params]
threshold = "-20"
ratio = "4"
attack = "10"
release = "100"
makeup = "0"
"#;

/// High gain preset - heavy distortion.
const HIGH_GAIN_PRESET: &str = r#"
name = "High Gain"
description = "Heavy distortion with compression - metal and hard rock"
sample_rate = 48000

[[effects]]
type = "preamp"
[effects.params]
gain = "6"

[[effects]]
type = "distortion"
[effects.params]
drive = "30"
tone = "-2"
output = "-6"

[[effects]]
type = "compressor"
[effects.params]
threshold = "-15"
ratio = "6"
attack = "5"
release = "80"
makeup = "3"

[[effects]]
type = "gate"
[effects.params]
threshold = "-40"
attack = "1"
hold = "50"
release = "50"

[[effects]]
type = "eq"
[effects.params]
low_gain = "2"
mid_freq = "800"
mid_gain = "-2"
high_gain = "3"
"#;

/// Ambient preset - spacious delay and reverb.
const AMBIENT_PRESET: &str = r#"
name = "Ambient"
description = "Lush atmospheric sounds with delay, reverb, and chorus"
sample_rate = 48000

[[effects]]
type = "compressor"
[effects.params]
threshold = "-25"
ratio = "3"
attack = "20"
release = "200"
makeup = "2"

[[effects]]
type = "chorus"
[effects.params]
rate = "0.5"
depth = "30"
mix = "30"

[[effects]]
type = "delay"
[effects.params]
time = "500"
feedback = "50"
mix = "40"

[[effects]]
type = "reverb"
[effects.params]
room_size = "80"
decay = "70"
damping = "30"
predelay = "20"
mix = "50"
"#;

/// Tape warmth preset - analog saturation.
const TAPE_WARMTH_PRESET: &str = r#"
name = "Tape Warmth"
description = "Warm analog saturation and subtle compression"
sample_rate = 48000

[[effects]]
type = "preamp"
[effects.params]
gain = "2"

[[effects]]
type = "tape"
[effects.params]
drive = "9"
saturation = "70"

[[effects]]
type = "compressor"
[effects.params]
threshold = "-18"
ratio = "2.5"
attack = "30"
release = "150"
makeup = "1"

[[effects]]
type = "eq"
[effects.params]
low_gain = "1"
mid_gain = "0"
high_gain = "-2"
"#;

/// Clean studio preset - professional clean tone.
const CLEAN_STUDIO_PRESET: &str = r#"
name = "Clean Studio"
description = "Professional clean tone with gentle compression and EQ"
sample_rate = 48000

[[effects]]
type = "preamp"
[effects.params]
gain = "0"

[[effects]]
type = "compressor"
[effects.params]
threshold = "-20"
ratio = "2"
attack = "15"
release = "150"
makeup = "2"

[[effects]]
type = "eq"
[effects.params]
low_freq = "80"
low_gain = "-2"
mid_freq = "2500"
mid_gain = "1"
high_freq = "8000"
high_gain = "2"

[[effects]]
type = "reverb"
[effects.params]
room_size = "30"
decay = "40"
damping = "60"
predelay = "5"
mix = "15"
"#;

/// 80s chorus preset - classic chorus sound.
const EIGHTIES_CHORUS_PRESET: &str = r#"
name = "80s Chorus"
description = "Classic 80s chorus sound - clean and shimmering"
sample_rate = 48000

[[effects]]
type = "compressor"
[effects.params]
threshold = "-15"
ratio = "3"
attack = "10"
release = "100"
makeup = "2"

[[effects]]
type = "chorus"
[effects.params]
rate = "1.2"
depth = "60"
mix = "50"

[[effects]]
type = "delay"
[effects.params]
time = "350"
feedback = "25"
mix = "20"

[[effects]]
type = "reverb"
[effects.params]
room_size = "40"
decay = "50"
damping = "50"
predelay = "10"
mix = "25"
"#;

/// Slapback preset - classic rockabilly delay.
const SLAPBACK_PRESET: &str = r#"
name = "Slapback"
description = "Classic slapback delay - rockabilly and vintage rock"
sample_rate = 48000

[[effects]]
type = "preamp"
[effects.params]
gain = "3"

[[effects]]
type = "distortion"
[effects.params]
drive = "8"
tone = "3"
output = "-2"

[[effects]]
type = "delay"
[effects.params]
time = "120"
feedback = "15"
mix = "40"

[[effects]]
type = "reverb"
bypassed = true
[effects.params]
room_size = "20"
decay = "30"
mix = "15"
"#;

/// Get all factory presets.
///
/// Returns a vector of all built-in presets that ship with the library.
///
/// # Example
///
/// ```rust
/// use sonido_config::factory_presets;
///
/// let presets = factory_presets();
/// println!("Available factory presets:");
/// for preset in &presets {
///     println!("  - {}: {}", preset.name, preset.description.as_deref().unwrap_or(""));
/// }
/// ```
pub fn factory_presets() -> Vec<Preset> {
    FACTORY_PRESETS_TOML
        .iter()
        .filter_map(|(_, toml)| Preset::from_toml(toml).ok())
        .collect()
}

/// Get a factory preset by name.
///
/// Returns `Some(Preset)` if a factory preset with the given name exists,
/// `None` otherwise. The name match is case-insensitive.
///
/// # Example
///
/// ```rust
/// use sonido_config::get_factory_preset;
///
/// if let Some(preset) = get_factory_preset("crunch") {
///     println!("Found preset: {}", preset.name);
/// }
/// ```
pub fn get_factory_preset(name: &str) -> Option<Preset> {
    let name_lower = name.to_lowercase();

    for (preset_name, toml) in FACTORY_PRESETS_TOML {
        if preset_name.to_lowercase() == name_lower {
            return Preset::from_toml(toml).ok();
        }
    }

    // Also try matching against the preset's actual name field
    for (_, toml) in FACTORY_PRESETS_TOML {
        if let Ok(preset) = Preset::from_toml(toml)
            && preset.name.to_lowercase() == name_lower
        {
            return Some(preset);
        }
    }

    None
}

/// Get the names of all factory presets.
///
/// Returns the internal identifiers used for factory presets.
///
/// # Example
///
/// ```rust
/// use sonido_config::factory_presets::factory_preset_names;
///
/// let names = factory_preset_names();
/// assert!(names.contains(&"crunch"));
/// ```
pub fn factory_preset_names() -> Vec<&'static str> {
    FACTORY_PRESETS_TOML.iter().map(|(name, _)| *name).collect()
}

/// Check if a preset name is a factory preset.
///
/// Returns true if the given name matches any factory preset (case-insensitive).
///
/// # Example
///
/// ```rust
/// use sonido_config::is_factory_preset;
///
/// assert!(is_factory_preset("crunch"));
/// assert!(is_factory_preset("Crunch"));
/// assert!(!is_factory_preset("my_custom_preset"));
/// ```
pub fn is_factory_preset(name: &str) -> bool {
    let name_lower = name.to_lowercase();

    // Check against internal names
    for preset_name in FACTORY_PRESET_NAMES {
        if preset_name.to_lowercase() == name_lower {
            return true;
        }
    }

    // Also check against display names in the presets
    for (_, toml) in FACTORY_PRESETS_TOML {
        if let Ok(preset) = Preset::from_toml(toml)
            && preset.name.to_lowercase() == name_lower
        {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_factory_presets_load() {
        let presets = factory_presets();
        assert!(!presets.is_empty(), "should have factory presets");

        // Check we have the expected presets
        let names: Vec<_> = presets.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"Init"));
        assert!(names.contains(&"Crunch"));
        assert!(names.contains(&"High Gain"));
        assert!(names.contains(&"Ambient"));
    }

    #[test]
    fn test_get_factory_preset() {
        // By internal name
        let preset = get_factory_preset("crunch").expect("crunch should exist");
        assert_eq!(preset.name, "Crunch");

        // By display name
        let preset = get_factory_preset("High Gain").expect("High Gain should exist");
        assert_eq!(preset.name, "High Gain");

        // Case insensitive
        let preset = get_factory_preset("AMBIENT").expect("AMBIENT should exist");
        assert_eq!(preset.name, "Ambient");

        // Non-existent
        assert!(get_factory_preset("nonexistent").is_none());
    }

    #[test]
    fn test_factory_preset_names() {
        let names = factory_preset_names();
        assert!(names.contains(&"init"));
        assert!(names.contains(&"crunch"));
        assert!(names.contains(&"high_gain"));
        assert!(names.contains(&"ambient"));
    }

    #[test]
    fn test_all_factory_presets_valid() {
        for (name, toml) in FACTORY_PRESETS_TOML {
            let result = Preset::from_toml(toml);
            assert!(
                result.is_ok(),
                "factory preset '{}' should parse: {:?}",
                name,
                result
            );

            let preset = result.unwrap();
            assert!(
                !preset.name.is_empty(),
                "preset '{}' should have a name",
                name
            );
            assert!(
                preset.description.is_some(),
                "preset '{}' should have a description",
                name
            );
        }
    }

    #[test]
    fn test_init_preset_has_all_effects_bypassed() {
        let init = get_factory_preset("init").expect("init should exist");

        // All effects in init preset should be bypassed
        for effect in &init.effects {
            assert!(
                effect.bypassed,
                "init preset effect '{}' should be bypassed",
                effect.effect_type
            );
        }
    }

    #[test]
    fn test_presets_have_reasonable_sample_rate() {
        for preset in factory_presets() {
            assert!(
                preset.sample_rate >= 44100 && preset.sample_rate <= 192000,
                "preset '{}' has unusual sample rate: {}",
                preset.name,
                preset.sample_rate
            );
        }
    }

    #[test]
    fn test_crunch_preset_structure() {
        let crunch = get_factory_preset("crunch").expect("crunch should exist");

        assert_eq!(crunch.name, "Crunch");
        assert!(crunch.description.is_some());

        // Should have preamp and distortion active
        let preamp = crunch.effects.iter().find(|e| e.effect_type == "preamp");
        assert!(preamp.is_some());
        assert!(!preamp.unwrap().bypassed);

        let dist = crunch
            .effects
            .iter()
            .find(|e| e.effect_type == "distortion");
        assert!(dist.is_some());
        assert!(!dist.unwrap().bypassed);
    }

    #[test]
    fn test_ambient_preset_has_time_effects() {
        let ambient = get_factory_preset("ambient").expect("ambient should exist");

        // Should have delay and reverb active
        let has_delay = ambient
            .effects
            .iter()
            .any(|e| e.effect_type == "delay" && !e.bypassed);
        let has_reverb = ambient
            .effects
            .iter()
            .any(|e| e.effect_type == "reverb" && !e.bypassed);

        assert!(has_delay, "ambient preset should have active delay");
        assert!(has_reverb, "ambient preset should have active reverb");
    }
}
