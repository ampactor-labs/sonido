//! The preset files shipped in `presets/` stay in step with the effects:
//! every one loads, converts to a patch, and every parameter it sets
//! resolves to a real parameter with a readable, in-range value.

use std::path::PathBuf;

use sonido_config::{EffectRegistry, Preset, preset_to_patch};

fn preset_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../presets")
}

#[test]
fn every_preset_file_converts_cleanly() {
    let registry = EffectRegistry::new();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(preset_dir())
        .expect("presets/ directory")
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no preset files in {:?}", preset_dir());

    let mut failures = Vec::new();
    for path in &paths {
        let preset = Preset::load(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let converted = preset_to_patch(&preset, &preset.name, None, &registry)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        if !converted.warnings.is_empty() {
            failures.push(format!("{}: {:?}", path.display(), converted.warnings));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
