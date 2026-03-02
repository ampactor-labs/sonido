# Sonido Library Testing Guide

Testing guide for library consumers integrating sonido crates.

For CLI and GUI manual testing, see [CLI_TESTING.md](CLI_TESTING.md).

---

## Dependency Profiles

| Profile | Crates | Feature Flags | Target |
|---------|--------|--------------|--------|
| Embedded (no_std) | core, effects, synth, registry, platform | `default-features = false` | Daisy Seed, Hothouse |
| Desktop Library | core, effects, synth, registry, config | default (std) | Plugin hosts, DAWs |
| Full Desktop | all crates | default (std) | Standalone apps |

---

## Tier 1: Compilation Verification

### no_std Compilation (Embedded Profile)

Verify the five no_std crates compile without the standard library:

```bash
cargo check --no-default-features -p sonido-core
cargo check --no-default-features -p sonido-effects
cargo check --no-default-features -p sonido-synth
cargo check --no-default-features -p sonido-registry
cargo check --no-default-features -p sonido-platform
```

- [ ] All 5 crates compile without `std`

### no_std Tests

```bash
cargo test --no-default-features -p sonido-core
cargo test --no-default-features -p sonido-effects
cargo test --no-default-features -p sonido-synth
cargo test --no-default-features -p sonido-registry
cargo test --no-default-features -p sonido-platform
```

- [ ] All no_std tests pass

### Full Workspace

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo doc --workspace --no-deps --all-features
cargo test --workspace --doc
```

- [ ] Zero clippy warnings
- [ ] Formatting clean
- [ ] Docs build without warnings
- [ ] Doc tests pass

---

## Tier 2: Integration Patterns

### Effect Chain

Process audio through a chain of effects:

```rust
use sonido_core::Effect;
use sonido_registry::EffectRegistry;

let sample_rate = 48000.0;
let registry = EffectRegistry::new();
let mut dist = registry.create("distortion", sample_rate).unwrap();
let mut delay = registry.create("delay", sample_rate).unwrap();
let mut reverb = registry.create("reverb", sample_rate).unwrap();

// Process a block of samples through the chain
let input = vec![0.5_f32; 1024];
let mut buffer = input.clone();
for sample in &mut buffer {
    *sample = dist.process(*sample);
    *sample = delay.process(*sample);
    *sample = reverb.process(*sample);
}
```

- [ ] Chain produces non-zero output
- [ ] No panics or NaN values

### Registry Create-by-Name

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

- [ ] All 19 effects create successfully
- [ ] Param lookup by name works for every effect

### WAV File Processing

```rust
use sonido_io::{WavReader, WavWriter};
use sonido_core::Effect;
use sonido_registry::EffectRegistry;

let reader = WavReader::open("input.wav").unwrap();
let spec = reader.spec();
let samples: Vec<f32> = reader.into_samples().map(|s| s.unwrap()).collect();

let registry = EffectRegistry::new();
let mut chorus = registry.create("chorus", spec.sample_rate as f32).unwrap();
let output: Vec<f32> = samples.iter().map(|&s| chorus.process(s)).collect();

let writer = WavWriter::create("output.wav", spec).unwrap();
// write output...
```

- [ ] WAV round-trip preserves format (sample rate, channels, bit depth)
- [ ] Effect output is audible and correct

### Analysis Pipeline

```rust
use sonido_analysis::{spectrum, dynamics};

let samples = vec![0.0_f32; 48000]; // 1 second at 48kHz
let peaks = spectrum::find_peaks(&samples, 48000.0, 5);
let stats = dynamics::analyze(&samples);
```

- [ ] Spectrum analysis produces valid frequency peaks
- [ ] Dynamics analysis reports RMS, peak, crest factor

---

## Tier 3: Behavioral Guarantees

### Default Parameter Sanity

Every effect at `::new(48000.0)` with default parameters must produce output within [-1, 1] for normalized input.

```rust
use sonido_registry::EffectRegistry;
use sonido_core::Effect;

let registry = EffectRegistry::new();
for desc in registry.list() {
    let mut effect = registry.create(desc.id, 48000.0).unwrap();

    // Process 1 second of unit sine
    for i in 0..48000 {
        let input = (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48000.0).sin();
        let output = effect.process(input);
        assert!(output.is_finite(), "{}: produced NaN/Inf", desc.id);
        // Note: some effects (distortion, preamp with gain) may exceed [-1, 1]
        // but should never produce NaN or Inf
    }
}
```

- [ ] No effect produces NaN or Inf with default parameters
- [ ] Dual-mono effects stay within reasonable bounds (~[-2, 2]) for unit input

### Boundary and Extreme Values

Extreme parameter values must not cause panics, NaN, or Inf:

```rust
use sonido_registry::EffectRegistry;
use sonido_core::Effect;

let registry = EffectRegistry::new();
for desc in registry.list() {
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

### Silence In, Silence Out

Effects with no feedback (distortion, compressor, eq, filter, gate, preamp, wah, tremolo) must produce silence when given silence:

```rust
// After processing 48000 samples of silence...
assert!(output.abs() < 1e-6, "non-silent output from silent input");
```

- [ ] Non-feedback effects produce silence from silence
- [ ] Feedback effects (delay, reverb, chorus, flanger, phaser) decay to silence

### Golden File Regression

```bash
cargo test -p sonido-effects --test regression
```

Three metrics must pass:
- MSE < 1e-6 (sample-level accuracy)
- SNR > 60 dB (signal quality)
- Spectral correlation > 0.9999 (frequency content preserved)

Regenerate after intentional changes:
```bash
REGENERATE_GOLDEN=1 cargo test --test regression -p sonido-effects
```

- [ ] All golden file tests pass

---

## Tier 4: Performance

### Faster-than-Realtime

Every effect must process audio faster than realtime at 48kHz:

```bash
cargo bench
```

Benchmark target: process 1M samples in less than 1M/48000 = 20.8ms.

- [ ] All effects process faster than realtime
- [ ] Block sizes 64/128/256/512/1024 all pass

### No Allocation in Process Path

Effects in no_std mode should not require an allocator for basic single-sample processing. The `process()` and `process_stereo()` methods must not allocate.

```bash
cargo check --no-default-features -p sonido-effects
```

- [ ] no_std compilation proves no hidden allocator dependency in core paths

---

## Gain Staging Reference

Default gain behavior for each effect with unit-amplitude input. Use this table to verify output levels are reasonable when integrating effects.

| Effect | Default Gain | Output Control | Notes |
|--------|-------------|----------------|-------|
| CleanPreamp | 0 dB | `output` param | Transparent at defaults |
| Distortion | +3.7 dB | `level` param | Signal-dependent; clipping adds energy |
| Tape | ~0 dB | `output` param | Gain-compensated after saturation |
| Compressor | -4.5 dB | `makeup` param | Safety-first default; use makeup to restore |
| Gate | 0 dB | — | Pass-through when open, silence when closed |
| Eq | 0 dB | band gain params | Flat at defaults (all gains 0 dB) |
| Filter | 0 dB | — | Unity passband gain |
| Wah | ~0 dB | `output` param | Bandpass normalized by Q |
| Tremolo | -3 dB avg | `depth` param | Amplitude modulation reduces average level |
| Chorus | ~0 dB | `output` param | Wet/dry mix preserves level |
| Flanger | ~0 dB | `output` param | Comb filtering causes signal-dependent variation |
| Phaser | ~0 dB | `output` param | Allpass summing, ~unity average |
| Vibrato | 0 dB | `mix` param | Pitch shift only, no amplitude change |
| Delay | 0-2.5 dB | `output` param | Feedback adds energy; signal-dependent |
| Reverb | ~0 dB | `output` param | Mix control balances dry/wet |
| Limiter | 0 dB | `ceiling` param | Brickwall limiting to ceiling level |
| Bitcrusher | 0 dB | `output` param | Bit reduction + downsampling |
| RingMod | ~0 dB | `output` param | Ring modulation, signal-dependent |
| Stage | 0 dB | `output` param | Stereo imaging, no amplitude change |

---

## See Also

- [CLI Testing Protocol](CLI_TESTING.md) — Manual testing for CLI and GUI
- [Effects Reference](EFFECTS_REFERENCE.md) — All 19 effects with parameters and DSP theory
- [DSP Fundamentals](DSP_FUNDAMENTALS.md) — Theory behind the implementations
- [Architecture](ARCHITECTURE.md) — Crate dependency graph and design overview
