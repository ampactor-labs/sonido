# Testing Guide

This guide covers testing practices, patterns, and commands for the Sonido project.

## Quick Start

```bash
# Run all tests
cargo test --workspace

# Run tests for a specific crate
cargo test -p sonido-core
cargo test -p sonido-effects

# Run tests whose name contains a filter string
cargo test -p sonido-effects reverb

# Run tests with output
cargo test -- --nocapture
```

## A note on "ignored" counts

`cargo test` reports a number of **ignored** tests. These are not disabled unit tests:
the codebase contains no `#[ignore]` attributes. Every "ignored"
entry is a documentation example whose code fence is marked ```` ```ignore ```` or
```` ```no_run ```` (illustrative snippets, embedded/hardware code, or examples
that allocate a real audio device). Those fences are still **compile-checked** by
`cargo test --doc`; they are only skipped at *run* time. There are no silently
skipped functional tests.

```bash
# Confirm there are no #[ignore] unit tests:
grep -rn '#\[ignore' crates/ --include='*.rs'   # -> no matches
```

## Test Organization

Tests in Sonido follow Rust conventions with inline unit tests and separate integration tests.

### Unit Tests

Each module contains a `#[cfg(test)]` block with unit tests:

```rust
// In crates/sonido-effects/src/kernels/reverb.rs (abridged)
#[cfg(test)]
mod tests {
    use super::*;
    use sonido_core::kernel::Adapter;
    use sonido_core::{Effect, ParameterInfo};

    /// The kernel must wrap into an `Adapter` and function as an `Effect`.
    #[test]
    fn adapter_wraps_as_effect() {
        let mut adapter = Adapter::new(ReverbKernel::new(48000.0), 48000.0);
        adapter.reset();
        let output = adapter.process(0.3);
        assert!(
            output.is_finite(),
            "Adapter output must be finite, got {output}"
        );
    }
}
```

### Integration Tests

Integration tests are in `tests/` directories within crates. Every crate except
sonido-daisy, sonido-gui and sonido-gui-core has a `tests/integration.rs` or equivalent;
the larger suites are:

| File | Covers |
|------|--------|
| `crates/sonido-effects/tests/regression.rs` | Golden-file output comparison (below) |
| `crates/sonido-effects/tests/quality_standard.rs` | [DSP Quality Standard](DSP_QUALITY_STANDARD.md) rules for every registered effect |
| `crates/sonido-effects/tests/compliance.rs` | Finite output, DC offset, reset safety, sample-rate changes, parameter clamping |
| `crates/sonido-effects/tests/extreme_params.rs` | Min/max parameters and 8 kHz / 192 kHz sample rates |
| `crates/sonido-effects/tests/param_contracts.rs` | Full parameter range reachable (within 1% of range), scale conventions, monotonic curves, macro exclusion only on mix and output |
| `crates/sonido-effects/tests/level_contract.rs` | The [level contract](#level-reference): with the reference guitar (-20 dBFS peaks, integrated LUFS), every effect fully wet (or at its defaults, without a mix control) within ±0.5 dB of the dry loudness, and at 0/25/50/75% mix and its default mix within ±1.5 dB. Skips the looper and tuner. Fails when `calibration.rs` is stale |
| `crates/sonido-effects/tests/reverb_decay.rs` | Plate (Decay 1/4/9 s) and spring (1/2.5/4.5 s) reverbs reach the RT60 their Decay names, within 25% (Schroeder energy-decay curve, T30 doubled); reverb, plate, spring and Harmonic Habitat fully wet within ±1.5 dB of the dry loudness |
| `crates/sonido-effects/tests/default_parity.rs` | `XxxParams::default()` equals every descriptor default, for every kernel |
| `crates/sonido-effects/tests/properties.rs` | Property-based (proptest) checks over all effects |
| `crates/sonido-effects/tests/aliasing.rs` | ADAA nonlinearities vs naive clipping |
| `crates/sonido-effects/tests/noon_mapping.rs` | The pedal's knob mapping (`sonido_platform::param_map::adc_to_param_biased`) over every writable parameter of every registered effect: knob 0/1 reach min/max and noon selects the default (stepped: the default step), both within 1% of range; the mapping never reverses |
| `crates/sonido-effects/tests/dag_rebuild_stress.rs` | Host-side stress of the firmware's DAG-rebuild path with a tracking allocator: finite, bounded output, bounded spillover, no heap growth |
| `crates/sonido-platform/tests/knob_calibration.rs` | Each jam effect (`JAM_EFFECT_IDS`, mapped by `knob_to_param`) with every knob at noon within ±1.5 dB of the dry guitar (integrated LUFS); each knob alone at an end within -15..+8 dB (K3, the level knob: -30..+13 dB), output finite |
| `crates/sonido-rigs/tests/factory.rs` | Every factory rig round-trips the binary format with all six macros mapped and named, passes the pedal's CPU and memory rule (`validate_for_daisy`, over `sonido_platform::cycle_table`) with no errors, and with every knob at noon plays the reference guitar within ±0.5 dB of the dry loudness halfway through its morph and within ±3 dB at A and B. Fails when `trims.rs` is stale |
| `crates/sonido-core/tests/properties.rs` | Property-based checks of core primitives |
| `crates/sonido-patch/tests/roundtrip.rs` | Patch encode/decode round trips |
| `crates/sonido-plugin/tests/plugin_inventory.rs` | Every registered effect ships in the plugin bundle, and vice versa |
| `crates/sonido-plugin/tests/state_roundtrip.rs`, `buffer_sizes.rs` | Plugin state save/restore and buffer-size robustness |

## Test Categories

### Audio Processing Tests

Every effect should verify:

1. **Basic processing**: Output is finite and not NaN
2. **Zero input**: Processing silence produces valid output
3. **Bypass behavior**: Bypassed effects pass audio unchanged
4. **Sample rate changes**: Effects work at multiple sample rates
5. **Block processing**: `process_block()` matches sample-by-sample results

Example pattern:

```rust
// Adapter's set_param comes from the ParameterInfo trait:
// use sonido_core::{Effect, ParameterInfo};

#[test]
fn test_distortion_basic() {
    let mut effect = Adapter::new(DistortionKernel::new(48000.0), 48000.0);
    effect.set_param(0, 20.0);  // drive_db

    // Test single sample
    let output = effect.process(0.5);
    assert!(output.is_finite());
    assert!(output.abs() <= 1.0);
}

#[test]
fn test_distortion_block() {
    let mut effect = Adapter::new(DistortionKernel::new(48000.0), 48000.0);
    let input = vec![0.5; 512];
    let mut output = vec![0.0; 512];

    effect.process_block(&input, &mut output);

    assert!(output.iter().all(|&s| s.is_finite()));
}
```

### Parameter Tests

Verify parameter behavior:

```rust
#[test]
fn test_compressor_parameters() {
    let registry = EffectRegistry::new();
    let mut comp = registry.create("compressor", 48000.0).unwrap();

    // Test parameter ranges via ParameterInfo
    comp.effect_set_param(0, -40.0);  // threshold
    assert_eq!(comp.effect_get_param(0), -40.0);

    comp.effect_set_param(1, 10.0);  // ratio
    let ratio = comp.effect_get_param(1);
    assert!(ratio >= 1.0);
    assert!(ratio <= 20.0);
}
```

### ParameterInfo Tests

Effects implementing `ParameterInfo` should verify introspection:

```rust
#[test]
fn test_reverb_parameter_info() {
    let registry = EffectRegistry::new();
    let reverb = registry.create("reverb", 48000.0).unwrap();

    assert!(reverb.effect_param_count() > 0);

    let info = reverb.effect_param_info(0).unwrap();
    assert!(!info.name.is_empty());
    assert!(info.min <= info.default);
    assert!(info.default <= info.max);
}
```

### Stereo Processing Tests

True stereo effects need additional tests:

```rust
#[test]
fn test_reverb_stereo_decorrelation() {
    let mut reverb = Adapter::new(ReverbKernel::new(48000.0), 48000.0);
    // Find and set the mix parameter to 1.0 (full wet)
    reverb.set_param(4, 100.0);  // mix = 100%

    // Feed identical signal to both channels
    for _ in 0..1000 {
        reverb.process_stereo(0.5, 0.5);
    }

    // After warmup, L and R should be different (decorrelated)
    let (l, r) = reverb.process_stereo(0.5, 0.5);
    assert!((l - r).abs() > 0.001, "stereo reverb should decorrelate L/R");
}
```

## Golden File Regression Tests

The `crates/sonido-effects/tests/regression.rs` test compares effect output against reference outputs stored in `crates/sonido-effects/tests/golden/` (75 `.golden` text files, one sample per line). This catches unintended DSP changes.

Three metrics must pass for each effect:
- **MSE < 1e-6** (sample-level accuracy)
- **SNR > 60 dB** (signal quality)
- **Spectral correlation > 0.9999** (frequency content preserved)

```bash
# Run golden file tests
cargo test --test regression -p sonido-effects

# Regenerate golden files after intentional DSP changes
REGENERATE_GOLDEN=1 cargo test --test regression -p sonido-effects
```

After regenerating, verify the new output sounds correct before committing the updated golden files.

## Generated Level Tables

Two tables are generated from measurements rather than edited by hand, and a test fails
when either is out of date:

| Table | Regenerate with | Checked by |
|-------|-----------------|------------|
| `crates/sonido-effects/src/calibration.rs` (each kernel's `Level`: trim and wet/dry correlation) | `cargo run -p sonido-effects --example calibrate --release` | `crates/sonido-effects/tests/level_contract.rs` |
| `crates/sonido-rigs/src/trims.rs` (factory rig master trims, dB) | `cargo run -p sonido-rigs --example calibrate --release` | `crates/sonido-rigs/tests/factory.rs` |

When `level_contract` or the rigs' `factory` test fails after a DSP or default change,
re-run the generator rather than adjusting a gain by hand. Order matters: calibrate the
effects first, then the rigs (rig trims are measured on calibrated effects), and
regenerate the golden files after the effects' calibration, since the trims are part of
the output they capture. Both generators print the measured gains as they run.

## no_std Compatibility Testing

Six crates must work without the standard library: sonido-core, sonido-effects,
sonido-synth, sonido-registry, sonido-platform and sonido-patch (`no_std + alloc`).

### Running no_std Tests

```bash
make test-nostd   # core, effects, synth, registry, platform
cargo test --no-default-features -p sonido-patch   # also run by the manual CI check job
```

### no_std Test Requirements

When writing tests for no_std crates:

1. Import `Vec` from `alloc`:
   ```rust
   #[cfg(test)]
   mod tests {
       extern crate alloc;
       use alloc::vec;
       use alloc::vec::Vec;
   }
   ```

2. Avoid `std`-only features in tests
3. Use `libm` for math functions instead of `std::f32`

## Test Data and Fixtures

### Generating Test Signals

Use the CLI to generate test signals:

```bash
# Generate test tone
sonido generate tone test_440hz.wav --freq 440 --duration 1.0

# Generate sweep for IR capture
sonido generate sweep sweep.wav --start 20 --end 20000 --duration 3.0

# Generate noise
sonido generate noise noise.wav --duration 1.0
```

### In-Code Test Signal Generation

```rust
fn generate_test_signal(size: usize, sample_rate: f32) -> Vec<f32> {
    (0..size)
        .map(|i| {
            let t = i as f32 / sample_rate;
            (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5
        })
        .collect()
}
```

## Continuous Integration

`.github/workflows/ci.yml` runs on every push and pull request to `main`.

### CI Test Matrix

| Job | Platform | What runs |
|-----|----------|-----------|
| `lint` | Linux (ubuntu-latest) | `cargo fmt --all --check`, `cargo clippy --workspace --all-targets` (warnings denied via `RUSTFLAGS`) |
| `test` | Linux, macOS, Windows | `cargo test --workspace` |
| `wasm` | Linux | `cargo build --target wasm32-unknown-unknown -p sonido-gui` |

no_std tests, docs with `-D warnings`, cargo-deny, benchmarks, coverage and CLAP
validation run in `ci-manual.yml` on manual dispatch
(`gh workflow run ci-manual.yml -f job=<check|bench|coverage|plugin|all>`).
See [CONTRIBUTING.md](CONTRIBUTING.md#continuous-integration).

### Running CI Checks Locally

```bash
# Same commands as the manual `check` job (minus wasm and cargo-deny)
make ci
make test-nostd
cargo test --no-default-features -p sonido-patch
```

## Common Test Patterns

### Effect Chain Testing

```rust
#[test]
fn test_effect_chain() {
    let registry = EffectRegistry::new();
    let mut preamp = registry.create("preamp", 48000.0).unwrap();
    let mut distortion = registry.create("distortion", 48000.0).unwrap();

    let mid = preamp.process(0.5);
    let output = distortion.process(mid);
    assert!(output.is_finite());
}
```

### Preset Testing

```rust
#[test]
fn test_factory_presets() {
    let presets = factory_presets();

    for preset in presets {
        let chain = EffectChain::from_preset(&preset, 48000.0);
        assert!(chain.is_ok(), "preset '{}' should be valid", preset.name);

        let mut chain = chain.unwrap();
        let output = chain.process(0.5);
        assert!(output.is_finite());
    }
}
```

### Reset Behavior Testing

```rust
#[test]
fn test_delay_reset() {
    let registry = EffectRegistry::new();
    let mut delay = registry.create("delay", 48000.0).unwrap();
    delay.effect_set_param(0, 100.0);  // time = 100 ms
    delay.effect_set_param(1, 50.0);   // feedback = 50 %

    // Fill the delay buffer
    for _ in 0..10000 {
        delay.process(0.5);
    }

    // Reset should clear the buffer
    delay.reset();

    let output = delay.process(0.0);
    assert!(output.abs() < 0.01, "reset should clear delay buffer");
}
```

## Debugging Failed Tests

### Verbose Output

```bash
# Show println! output from tests
cargo test -- --nocapture

# Run a single test with backtrace
RUST_BACKTRACE=1 cargo test -p sonido-effects adapter_wraps_as_effect -- --nocapture
```

### Test Isolation

Run tests in a single thread to isolate issues:

```bash
cargo test -- --test-threads=1
```

### Stale Generated Tables

`level_contract` (sonido-effects) or `factory` (sonido-rigs) failing after a DSP or
default change usually means a generated level table is out of date, and the failure
message names the generator to run; see [Generated Level Tables](#generated-level-tables).
A `regression` failure after an intentional change means the golden files need
regenerating (above).

## Documentation Tests

Rustdoc examples are also tested:

```bash
# Run documentation tests
cargo test --doc

# Test docs for a specific crate
cargo test --doc -p sonido-core
```

## Test Coverage

You can measure test coverage using `cargo-llvm-cov` (matches CI):

```bash
# Install cargo-llvm-cov
cargo install cargo-llvm-cov

# Run coverage (generates lcov.info)
cargo llvm-cov --workspace --lcov --output-path lcov.info

# Generate HTML report
cargo llvm-cov --workspace --html
```

CI runs coverage on manual dispatch (`gh workflow run ci-manual.yml -f job=coverage`) and uploads `lcov.info` as an artifact.

## Adding New Tests

When adding a new effect or feature:

1. Add unit tests in the module's `#[cfg(test)]` block
2. Verify basic processing, parameters, and edge cases
3. Test no_std compatibility if applicable
4. Add integration tests if the feature spans multiple modules
5. Ensure documentation examples are testable

### Test Checklist

- [ ] Basic processing produces finite output
- [ ] Zero/silence input handled correctly
- [ ] Parameter ranges validated
- [ ] Sample rate changes work
- [ ] Block processing matches sample-by-sample
- [ ] Reset clears internal state
- [ ] New effect calibrated (`calibrate` example) and `level_contract` passes
- [ ] no_std compatible (for core crates)

## Library Consumer Testing

Testing guide for library consumers integrating sonido crates.

For CLI and GUI manual testing, see [CLI_TESTING.md](CLI_TESTING.md).

### Dependency Profiles

| Profile | Crates | Feature Flags | Target |
|---------|--------|--------------|--------|
| Embedded (no_std) | core, effects, synth, registry, platform, patch | `default-features = false` | Daisy Seed, Hothouse |
| Desktop Library | core, effects, synth, registry, config | default (std) | Plugin hosts, DAWs |
| Full Desktop | all crates | default (std) | Standalone apps |

### Tier 1: Compilation Verification

#### no_std Compilation (Embedded Profile)

Verify the six no_std crates compile without the standard library:

```bash
cargo check --no-default-features -p sonido-core
cargo check --no-default-features -p sonido-effects
cargo check --no-default-features -p sonido-synth
cargo check --no-default-features -p sonido-registry
cargo check --no-default-features -p sonido-platform
cargo check --no-default-features -p sonido-patch
```

#### Full Workspace

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo test --workspace --doc
```

- [ ] Zero clippy warnings
- [ ] Formatting clean
- [ ] Docs build without warnings
- [ ] Doc tests pass

### Tier 2: Integration Patterns

#### Registry Create-by-Name

```rust
use sonido_registry::EffectRegistry;

let registry = EffectRegistry::new();
let mut effect = registry.create("distortion", 48000.0).unwrap();

// Use via EffectWithParams trait
effect.process(0.5);
effect.effect_set_param(0, 20.0);

// Lookup param by name
let idx = registry.param_index_by_name("distortion", "drive");
assert!(idx.is_some());
```

- [ ] All 36 effects create successfully
- [ ] Param lookup by name works for every effect

#### WAV File Processing

```rust
use sonido_io::read_wav;
use sonido_core::Effect;
use sonido_registry::EffectRegistry;

// read_wav mixes multi-channel files down to mono; read_wav_stereo keeps L/R
let (samples, spec) = read_wav("input.wav").unwrap();

let registry = EffectRegistry::new();
let mut chorus = registry.create("chorus", spec.sample_rate as f32).unwrap();
let output: Vec<f32> = samples.iter().map(|&s| chorus.process(s)).collect();
```

- [ ] WAV round-trip preserves format (sample rate, channels, bit depth)
- [ ] Effect output is audible and correct

#### Analysis Pipeline

```rust
use sonido_analysis::{Window, analyze_dynamics, magnitude_spectrum};
use sonido_analysis::spectrum::find_peaks;

let samples = vec![0.0_f32; 48000]; // 1 second at 48kHz
let mag = magnitude_spectrum(&samples[..4096], 4096, Window::Hann);
let peaks = find_peaks(&mag, 48000.0, -60.0, 50.0); // threshold dB, min spacing Hz
let stats = analyze_dynamics(&samples, 1024, -60.0); // window, silence threshold dB
```

- [ ] Spectrum analysis produces valid frequency peaks
- [ ] Dynamics analysis reports RMS, peak, crest factor

### Tier 3: Behavioral Guarantees

#### Default Parameter Sanity

Every effect created at 48 kHz with default parameters must produce finite output for
normalized input. Output is not guaranteed to stay within [-1, 1]: levels are matched in
loudness, not peak (see the [level reference](#level-reference) below).

```rust
use sonido_registry::EffectRegistry;
use sonido_core::Effect;

let registry = EffectRegistry::new();
for desc in registry.all_effects() {
    let mut effect = registry.create(desc.id, 48000.0).unwrap();

    for i in 0..48000 {
        let input = (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48000.0).sin();
        let output = effect.process(input);
        assert!(output.is_finite(), "{}: produced NaN/Inf", desc.id);
    }
}
```

- [ ] No effect produces NaN or Inf with default parameters

#### Boundary and Extreme Values

Extreme parameter values must not cause panics, NaN, or Inf:

```rust
use sonido_registry::EffectRegistry;
use sonido_core::Effect;

let registry = EffectRegistry::new();
for desc in registry.all_effects() {
    let mut effect = registry.create(desc.id, 48000.0).unwrap();

    // Set all params to minimum
    for i in 0..desc.param_count {
        let info = effect.effect_param_info(i).unwrap();
        effect.effect_set_param(i, info.min);
    }
    let out_min = effect.process(0.5);
    assert!(out_min.is_finite(), "{}: NaN at min params", desc.id);

    // Set all params to maximum
    let mut effect = registry.create(desc.id, 48000.0).unwrap();
    for i in 0..desc.param_count {
        let info = effect.effect_param_info(i).unwrap();
        effect.effect_set_param(i, info.max);
    }
    let out_max = effect.process(0.5);
    assert!(out_max.is_finite(), "{}: NaN at max params", desc.id);
}
```

- [ ] No panics at min or max parameter values
- [ ] No NaN/Inf at any extreme

#### Silence In, Silence Out

Effects with no feedback (distortion, compressor, eq, filter, gate, preamp, wah, tremolo) must produce silence when given silence:

```rust
// After processing 48000 samples of silence...
assert!(output.abs() < 1e-6, "non-silent output from silent input");
```

- [ ] Non-feedback effects produce silence from silence
- [ ] Feedback effects (delay, reverb, chorus, flanger, phaser) decay to silence

### Tier 4: Performance

#### Faster-than-Realtime

Every effect must process audio faster than realtime at 48kHz. Benchmark target:
process 1M samples in less than 1M/48000 = 20.8 s (equivalently, under 20.8 µs per
sample; a 256-sample block under 5.3 ms).

- [ ] All effects process faster than realtime
- [ ] Block sizes 64/128/256/512/1024 all pass

#### No Allocation in Process Path

The no_std crates link `alloc` (sonido-core, sonido-effects and sonido-synth declare
`extern crate alloc`) so kernels can allocate delay lines and buffers at construction.
The `process()` and `process_stereo()` methods must not allocate; no_std compilation
alone does not prove this, so review hot paths for `Vec`/`Box` creation.

- [ ] No allocation inside `process()` / `process_stereo()`

### Level Reference

Every effect except the looper and tuner is held to one level contract
(`sonido_core::level`, [ADR-031](DESIGN_DECISIONS.md#adr-031-one-measured-level-contract)):
at its defaults it plays at the loudness of its input, and its mix control does not
change that loudness. The reference is `sonido_analysis::signals::reference_guitar`, a
strummed guitar peaking at -20 dBFS (`NOMINAL_PEAK_DBFS`), measured as integrated LUFS
(ITU-R BS.1770-4). Levels are matched in loudness, not peak, and only for that
reference. Other material and other input levels measure differently, especially into
a saturating effect.

Measured 2026-10-22 with
`cargo test --release -p sonido-effects --test level_contract -- --nocapture`, which
prints the full table: all 34 calibrated effects are within 0.05 dB of the dry loudness
fully wet, or at their defaults when they have no mix control. Across the mix control,
every effect stays within ±0.1 dB except these (dB against the dry loudness):

| Effect (registry id) | 25% mix | 50% mix | 75% mix | Default mix |
|----------------------|---------|---------|---------|-------------|
| Harmonic Habitat (`harmonic_habitat`) | +0.5 | +0.7 | +0.5 | +0.7 |
| Plate Reverb (`plate_reverb`) | +0.3 | +0.4 | +0.3 | +0.4 |
| Phaser (`phaser`) | +0.2 | +0.3 | +0.2 | +0.3 |
| Reverb (`reverb`) | -0.2 | -0.3 | -0.2 | -0.3 |
| Delay (`delay`) | -0.1 | -0.2 | -0.3 | -0.1 |
| Flanger (`flanger`) | +0.2 | +0.2 | +0.2 | +0.2 |

Two more tables print with `--nocapture`: each jam effect's gain with every knob at noon
and with each knob at its ends
(`cargo test --release -p sonido-platform --test knob_calibration -- --nocapture`), and
each factory rig's gain at morph A, halfway and B
(`cargo test --release -p sonido-rigs --test factory -- --nocapture`).

---

## See Also

- [CLI Testing Protocol](CLI_TESTING.md) - Manual testing for CLI and GUI
- [Effects Reference](EFFECTS_REFERENCE.md) - All 36 effects with parameters and DSP theory
- [DSP Fundamentals](DSP_FUNDAMENTALS.md) - Theory behind the implementations
- [Contributing](CONTRIBUTING.md) - Development guidelines
- [Benchmarks](BENCHMARKS.md) - Performance testing
- [Architecture](ARCHITECTURE.md) - System design
