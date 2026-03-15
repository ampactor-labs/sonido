//! Integration tests for sonido-cli.
//!
//! Tests cover the CLI binary invocation, effect creation from the registry,
//! and end-to-end file processing workflows.

use std::process::Command;

/// Helper to get the path to the `sonido` binary built by cargo.
fn sonido_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_sonido"))
}

// ---------------------------------------------------------------------------
// CLI binary tests -- `sonido effects`
// ---------------------------------------------------------------------------

#[test]
fn cli_effects_lists_all_effects() {
    let output = sonido_bin()
        .arg("effects")
        .output()
        .expect("failed to run sonido effects");

    assert!(output.status.success(), "sonido effects failed");

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Verify the header is present
    assert!(
        stdout.contains("Available Effects"),
        "should show 'Available Effects' header"
    );

    // Verify all 15 CLI-exposed effects are listed
    let expected_effects = [
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
    ];

    for effect in &expected_effects {
        assert!(
            stdout.contains(effect),
            "effects listing should contain '{effect}'"
        );
    }
}

#[test]
fn cli_effects_detail_shows_parameters() {
    let output = sonido_bin()
        .args(["effects", "distortion"])
        .output()
        .expect("failed to run sonido effects distortion");

    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should show the effect name as a header
    assert!(stdout.contains("distortion"));

    // Should show parameter table
    assert!(stdout.contains("Parameters"));
    assert!(stdout.contains("drive"));
    assert!(stdout.contains("tone"));
}

#[test]
fn cli_effects_unknown_effect_fails() {
    let output = sonido_bin()
        .args(["effects", "nonexistent_effect_xyz"])
        .output()
        .expect("failed to run sonido");

    assert!(!output.status.success(), "should fail for unknown effect");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Unknown effect") || stderr.contains("nonexistent_effect_xyz"),
        "error should mention unknown effect, got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// CLI binary tests -- `sonido --help`
// ---------------------------------------------------------------------------

#[test]
fn cli_help_works() {
    let output = sonido_bin()
        .arg("--help")
        .output()
        .expect("failed to run sonido --help");

    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Sonido DSP Framework CLI"));
    assert!(stdout.contains("process"));
    assert!(stdout.contains("effects"));
    assert!(stdout.contains("generate"));
}

#[test]
fn cli_version_works() {
    let output = sonido_bin()
        .arg("--version")
        .output()
        .expect("failed to run sonido --version");

    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("sonido"),
        "version output should contain 'sonido'"
    );
}

// ---------------------------------------------------------------------------
// CLI binary tests -- `sonido process` (end-to-end file processing)
// ---------------------------------------------------------------------------

#[test]
fn cli_process_single_effect() {
    use sonido_io::{WavSpec, write_wav};
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let input_path = dir.path().join("input.wav");
    let output_path = dir.path().join("output.wav");

    // Create a test WAV file
    let sr = 48000;
    let samples: Vec<f32> = (0..sr)
        .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / sr as f32).sin())
        .collect();

    let spec = WavSpec {
        channels: 1,
        sample_rate: sr as u32,
        bits_per_sample: 32,
    };
    write_wav(&input_path, &samples, spec).unwrap();

    // Run sonido process
    let output = sonido_bin()
        .args([
            "process",
            input_path.to_str().unwrap(),
            output_path.to_str().unwrap(),
            "--effect",
            "distortion",
            "--param",
            "drive=12",
        ])
        .output()
        .expect("failed to run sonido process");

    assert!(
        output.status.success(),
        "sonido process failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify output file exists and is a valid WAV
    assert!(output_path.exists(), "output WAV should exist");

    let (loaded, loaded_spec) = sonido_io::read_wav(&output_path).unwrap();
    assert_eq!(loaded_spec.sample_rate, sr as u32);
    assert!(!loaded.is_empty());
}

#[test]
fn cli_process_chain() {
    use sonido_io::{WavSpec, write_wav};
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let input_path = dir.path().join("input.wav");
    let output_path = dir.path().join("output.wav");

    // Create a test WAV file
    let sr = 48000;
    let samples: Vec<f32> = (0..sr)
        .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / sr as f32).sin())
        .collect();

    let spec = WavSpec {
        channels: 1,
        sample_rate: sr as u32,
        bits_per_sample: 32,
    };
    write_wav(&input_path, &samples, spec).unwrap();

    // Run with chain
    let output = sonido_bin()
        .args([
            "process",
            input_path.to_str().unwrap(),
            output_path.to_str().unwrap(),
            "--chain",
            "preamp:gain=6|compressor:threshold=-18",
        ])
        .output()
        .expect("failed to run sonido process with chain");

    assert!(
        output.status.success(),
        "sonido process --chain failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(output_path.exists());
    let (loaded, _) = sonido_io::read_wav(&output_path).unwrap();
    assert!(!loaded.is_empty());
}

#[test]
fn cli_process_no_effect_fails() {
    use sonido_io::{WavSpec, write_wav};
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let input_path = dir.path().join("input.wav");

    // Create a minimal WAV file
    let spec = WavSpec {
        channels: 1,
        sample_rate: 48000,
        bits_per_sample: 32,
    };
    write_wav(&input_path, &[0.0; 100], spec).unwrap();

    let output = sonido_bin()
        .args(["process", input_path.to_str().unwrap()])
        .output()
        .expect("failed to run sonido");

    assert!(
        !output.status.success(),
        "process without effect should fail"
    );
}

#[test]
fn cli_process_nonexistent_input_fails() {
    let output = sonido_bin()
        .args([
            "process",
            "/tmp/nonexistent_sonido_test_file_12345.wav",
            "--effect",
            "distortion",
        ])
        .output()
        .expect("failed to run sonido");

    assert!(
        !output.status.success(),
        "process with nonexistent input should fail"
    );
}

// ---------------------------------------------------------------------------
// CLI binary tests -- `sonido info`
// ---------------------------------------------------------------------------

#[test]
fn cli_info_shows_wav_metadata() {
    use sonido_io::{WavSpec, write_wav};
    use tempfile::NamedTempFile;

    let file = NamedTempFile::with_suffix(".wav").unwrap();

    let sr = 44100u32;
    let samples: Vec<f32> = (0..sr)
        .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / sr as f32).sin())
        .collect();

    let spec = WavSpec {
        channels: 1,
        sample_rate: sr,
        bits_per_sample: 32,
    };
    write_wav(file.path(), &samples, spec).unwrap();

    let output = sonido_bin()
        .args(["info", file.path().to_str().unwrap()])
        .output()
        .expect("failed to run sonido info");

    assert!(
        output.status.success(),
        "sonido info failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("44100") || stdout.contains("44,100"),
        "should show sample rate, got: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// CLI binary tests -- `sonido generate`
// ---------------------------------------------------------------------------

#[test]
fn cli_generate_tone() {
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let output_path = dir.path().join("tone.wav");

    let output = sonido_bin()
        .args([
            "generate",
            "tone",
            output_path.to_str().unwrap(),
            "--freq",
            "440",
            "--duration",
            "0.1",
        ])
        .output()
        .expect("failed to run sonido generate tone");

    assert!(
        output.status.success(),
        "sonido generate tone failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(output_path.exists());

    let (loaded, spec) = sonido_io::read_wav(&output_path).unwrap();
    assert!(!loaded.is_empty());
    // Duration 0.1s at default sample rate should give ~4800 samples
    assert!(loaded.len() > 1000, "tone should have reasonable length");
    assert_eq!(spec.sample_rate, 48000); // default sample rate
}

// ---------------------------------------------------------------------------
// Registry-based effect creation tests (library-level, not binary)
// ---------------------------------------------------------------------------

#[test]
fn registry_creates_all_19_effects() {
    use sonido_registry::EffectRegistry;

    let registry = EffectRegistry::new();

    // The registry should have 20 effects
    assert_eq!(registry.len(), 20, "registry should have 20 effects");

    // Verify we can create each one
    let effect_names = [
        "preamp",
        "distortion",
        "compressor",
        "gate",
        "eq",
        "wah",
        "chorus",
        "flanger",
        "phaser",
        "tremolo",
        "delay",
        "filter",
        "vibrato",
        "tape",
        "reverb",
        "limiter",
        "bitcrusher",
        "ringmod",
        "stage",
        "looper",
    ];

    for name in &effect_names {
        let effect = registry.create(name, 48000.0);
        assert!(
            effect.is_some(),
            "should be able to create effect '{name}' from registry"
        );
    }
}

#[test]
fn registry_effect_processes_audio() {
    use sonido_registry::EffectRegistry;

    let registry = EffectRegistry::new();
    let mut effect = registry.create("distortion", 48000.0).unwrap();

    // Process some audio through the effect
    let input = 0.5_f32;
    let output = effect.process(input);

    // Distortion with default drive should produce some output
    assert!(output.is_finite(), "output should be finite");
}

#[test]
fn registry_unknown_effect_returns_none() {
    use sonido_registry::EffectRegistry;

    let registry = EffectRegistry::new();
    assert!(
        registry.create("nonexistent", 48000.0).is_none(),
        "unknown effect should return None"
    );
}
