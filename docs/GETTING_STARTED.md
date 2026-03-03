# Getting Started

This guide walks you through creating your first audio effect with Sonido.

## Prerequisites

- Rust 1.85 or later (Edition 2024)
- For real-time audio: platform audio drivers (ALSA on Linux, CoreAudio on macOS, WASAPI on Windows)

## Installation

### As a Library

Add the crates you need to your `Cargo.toml`:

```toml
[dependencies]
sonido-core = "0.1"      # Core traits and primitives
sonido-effects = "0.1"   # Effect implementations
```

### CLI Tool

Build and install the CLI:

```bash
git clone https://github.com/ampactor-labs/sonido
cd sonido
cargo install --path crates/sonido-cli
```

For development, use `cargo run -p sonido-cli -- <args>` to build and run from source directly.

## Quick Start Options

### GUI (Graphical Interface)

For a visual, interactive experience:

```bash
# Run the GUI application
cargo run -p sonido-gui

# Or install and run
cargo install --path crates/sonido-gui
sonido-gui
```

See [GUI.md](GUI.md) for detailed GUI documentation.

### Walkthrough (recommended)

Run the full guided tour -- builds, generates signals, processes all 19 effects,
runs analysis, and prints listening guidance:

```bash
./scripts/walkthrough.sh           # Full walkthrough with narration
./scripts/walkthrough.sh --verify  # Verify existing demos work
./scripts/walkthrough.sh --clean   # Remove generated files

# Or via make:
make walkthrough
make verify
```

### Generate Demos

Generate all demo WAV files (5 source signals + 22 effect demos):

```bash
./scripts/generate_demos.sh
# Or: make demo
```

### Example Code

Run the chain demo example:

```bash
cargo run -p sonido-effects --example chain_demo
```

---

## Your First Effect

Let's create a simple distortion effect using the registry:

```rust
use sonido_core::Effect;
use sonido_registry::EffectRegistry;

fn main() {
    // Create effect via the registry at 48kHz sample rate
    let registry = EffectRegistry::new();
    let mut distortion = registry.create("distortion", 48000.0).unwrap();

    // Configure parameters by index (0=drive, 1=tone)
    distortion.effect_set_param(0, 12.0);   // 12dB of gain into the waveshaper
    distortion.effect_set_param(1, 3.0);     // +3dB peaking EQ at 1kHz

    // Generate a test signal (sine wave)
    let sample_rate = 48000.0;
    let frequency = 440.0;  // A4
    let duration_samples = 48000;  // 1 second

    let input: Vec<f32> = (0..duration_samples)
        .map(|i| {
            let t = i as f32 / sample_rate;
            (2.0 * std::f32::consts::PI * frequency * t).sin() * 0.5
        })
        .collect();

    // Process the audio
    let mut output = vec![0.0; input.len()];
    distortion.process_block(&input, &mut output);

    println!("Processed {} samples", output.len());
}
```

## Effect Chaining

Chain multiple effects together using the registry:

```rust
use sonido_core::Effect;
use sonido_registry::EffectRegistry;

fn main() {
    let sample_rate = 48000.0;
    let registry = EffectRegistry::new();

    // Create effects via registry
    let mut preamp = registry.create("preamp", sample_rate).unwrap();
    preamp.effect_set_param(0, 6.0);  // gain = 6 dB

    let mut distortion = registry.create("distortion", sample_rate).unwrap();
    distortion.effect_set_param(0, 15.0);  // drive = 15 dB

    let mut delay = registry.create("delay", sample_rate).unwrap();
    delay.effect_set_param(0, 300.0);  // time = 300 ms
    delay.effect_set_param(1, 40.0);   // feedback = 40%
    delay.effect_set_param(2, 30.0);   // mix = 30%

    // Process through the chain
    let input = vec![0.5f32; 1024];
    let mut buffer = vec![0.0; 1024];
    let mut output = vec![0.0; 1024];

    preamp.process_block(&input, &mut buffer);
    distortion.process_block(&buffer, &mut output);
    buffer.copy_from_slice(&output);
    delay.process_block(&buffer, &mut output);

    // 'output' now contains the processed audio
}
```

## Dynamic Effect Chains

For runtime-configurable chains, use the registry with `Box<dyn EffectWithParams + Send>`:

```rust
use sonido_core::Effect;
use sonido_core::EffectWithParams;
use sonido_registry::EffectRegistry;

fn main() {
    let sample_rate = 48000.0;
    let registry = EffectRegistry::new();

    // Build a dynamic chain via registry
    let mut chain: Vec<Box<dyn EffectWithParams + Send>> = vec![
        registry.create("distortion", sample_rate).unwrap(),
        registry.create("chorus", sample_rate).unwrap(),
        registry.create("delay", sample_rate).unwrap(),
    ];

    // Process through the chain
    let input = vec![0.5f32; 1024];
    let mut buffer1 = vec![0.0; 1024];
    let mut buffer2 = vec![0.0; 1024];

    buffer1.copy_from_slice(&input);

    for effect in &mut chain {
        effect.process_block(&buffer1, &mut buffer2);
        std::mem::swap(&mut buffer1, &mut buffer2);
    }

    // Result is in buffer1
}
```

## Using the CLI

Process a file through effects. Output filename is optional -- when omitted, it is
auto-generated from the input stem and effect specification:

```bash
# Simple distortion (auto-generates input_distortion.wav)
sonido process input.wav --effect distortion --param drive=15

# Explicit output filename
sonido process input.wav output.wav --effect distortion --param drive=15

# Effect chain
sonido process input.wav \
    --chain "preamp:gain=6|distortion:drive=12|delay:time=300,feedback=0.4"
```

Real-time processing:

```bash
# List available audio devices
sonido devices

# Process live audio
sonido realtime --effect chorus --param rate=2 --param depth=0.6
```

## Working with WAV Files

```rust
use sonido_io::{read_wav, write_wav, WavSpec};
use sonido_registry::EffectRegistry;
use sonido_core::Effect;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Read input file
    let (samples, spec) = read_wav("input.wav")?;
    let sample_rate = spec.sample_rate as f32;

    // Create and configure effect via registry
    let registry = EffectRegistry::new();
    let mut compressor = registry.create("compressor", sample_rate).unwrap();
    compressor.effect_set_param(0, -18.0);  // threshold
    compressor.effect_set_param(1, 4.0);    // ratio
    compressor.effect_set_param(2, 10.0);   // attack
    compressor.effect_set_param(3, 100.0);  // release

    // Process
    let mut output = vec![0.0; samples.len()];
    compressor.process_block(&samples, &mut output);

    // Write output file
    let out_spec = WavSpec {
        channels: 1,
        sample_rate: spec.sample_rate,
        bits_per_sample: 32,
    };
    write_wav("output.wav", &output, out_spec)?;

    Ok(())
}
```

## Embedded/no_std Usage

For embedded systems without standard library, use kernels directly (no adapter overhead):

```toml
[dependencies]
sonido-core = { version = "0.1", default-features = false }
sonido-effects = { version = "0.1", default-features = false }
```

```rust
#![no_std]

use sonido_effects::kernels::{FilterKernel, FilterParams};
use sonido_core::kernel::DspKernel;

fn audio_callback(kernel: &mut FilterKernel, adc_cutoff: f32, adc_resonance: f32) {
    // Map ADC readings (0.0-1.0) directly to parameter ranges
    let params = FilterParams::from_knobs(adc_cutoff, adc_resonance, 0.5, 0.0, 1.0);

    // Process sample-by-sample — no allocation, no smoothing
    let (left, right) = kernel.process_stereo(input_l, input_r, &params);
}
```

## Basic Synthesis

Sonido includes a synthesis engine with anti-aliased oscillators (PolyBLEP), ADSR envelopes, and FM synthesis support.

```rust
use sonido_synth::{Oscillator, OscillatorWaveform, AdsrEnvelope};

fn main() {
    let sample_rate = 48000.0;

    // Create an oscillator
    let mut osc = Oscillator::new(sample_rate);
    osc.set_frequency(440.0);
    osc.set_waveform(OscillatorWaveform::Saw);

    // Shape it with an ADSR envelope
    let mut env = AdsrEnvelope::new(sample_rate);
    env.set_attack_ms(10.0);
    env.set_decay_ms(100.0);
    env.set_sustain(0.7);
    env.set_release_ms(200.0);

    // Play a note
    env.gate_on();
    let mut output = Vec::new();
    for _ in 0..48000 {
        let level = env.advance();
        let sample = osc.advance();
        output.push(sample * level);
    }
    env.gate_off();

    // FM synthesis: modulator controls carrier phase
    let mut carrier = Oscillator::new(sample_rate);
    carrier.set_frequency(440.0);
    carrier.set_waveform(OscillatorWaveform::Sine);

    let mut modulator = Oscillator::new(sample_rate);
    modulator.set_frequency(880.0); // 2:1 ratio
    modulator.set_waveform(OscillatorWaveform::Sine);

    let mod_index = 2.0; // modulation depth in radians
    let fm_sample = carrier.advance_with_pm(modulator.advance() * mod_index);
}
```

See `crates/sonido-synth/examples/synthesis_demo.rs` for a full demo:

```bash
cargo run -p sonido-synth --example synthesis_demo
```

## Tempo-Synced Effects

The `TempoManager` converts BPM and note divisions into delay times, LFO rates, and sample counts:

```rust
use sonido_core::{TempoManager, NoteDivision, Lfo, LfoWaveform};

fn main() {
    let sample_rate = 48000.0;
    let mut tempo = TempoManager::new(sample_rate, 120.0);
    tempo.play();

    // Get delay time for a dotted eighth note
    let delay_ms = tempo.division_to_ms(NoteDivision::DottedEighth); // 375ms at 120 BPM

    // Get LFO rate for quarter notes
    let lfo_hz = tempo.division_to_hz(NoteDivision::Quarter); // 2 Hz at 120 BPM

    // Sync an LFO to tempo
    let mut lfo = Lfo::new(sample_rate, 1.0);
    lfo.set_waveform(LfoWaveform::Sine);
    lfo.sync_to_tempo(120.0, NoteDivision::Eighth); // 4 Hz at 120 BPM

    // Change BPM and all timing values update
    tempo.set_bpm(140.0);
    let new_delay = tempo.division_to_ms(NoteDivision::DottedEighth); // 321.4ms at 140 BPM
}
```

See `crates/sonido-core/examples/tempo_sync_demo.rs` for a full demo:

```bash
cargo run -p sonido-core --example tempo_sync_demo
```

## Analysis Toolkit

The analysis crate provides FFT, spectral analysis, dynamics measurement, and filter banks:

```rust
use sonido_analysis::{
    Fft, Window, magnitude_spectrum, spectral_centroid,
    rms, rms_db, peak, crest_factor,
};
use sonido_analysis::filterbank::{FilterBank, FrequencyBand};
use std::f32::consts::PI;

fn main() {
    let sample_rate = 48000.0;
    let fft_size = 4096;

    // Generate a 1 kHz sine wave
    let signal: Vec<f32> = (0..fft_size)
        .map(|i| (2.0 * PI * 1000.0 * i as f32 / sample_rate).sin())
        .collect();

    // FFT spectrum with Hann window
    let mag = magnitude_spectrum(&signal, fft_size, Window::Hann);
    let centroid = spectral_centroid(&mag, sample_rate);
    println!("Spectral centroid: {:.1} Hz", centroid);

    // Dynamics
    println!("RMS: {:.1} dB, Peak: {:.4}", rms_db(&signal), peak(&signal));
    println!("Crest factor: {:.2}", crest_factor(&signal));

    // Filter bank for audio frequency band extraction
    let sub_bass = FrequencyBand::new(20.0, 60.0);
    let bass = FrequencyBand::new(60.0, 250.0);
    let mid = FrequencyBand::new(250.0, 4000.0);
    let presence = FrequencyBand::new(4000.0, 8000.0);
    let bands = [sub_bass, bass, mid, presence];
    let mut bank = FilterBank::new(sample_rate, &bands);
    let extracted = bank.extract(&signal);
}
```

See `crates/sonido-analysis/examples/analysis_demo.rs` for a full demo:

```bash
cargo run -p sonido-analysis --example analysis_demo
```

## Examples

All runnable examples with their `cargo run` commands:

| Example | Crate | Command |
|---------|-------|---------|
| Synthesis (oscillators, envelopes, FM) | sonido-synth | `cargo run -p sonido-synth --example synthesis_demo` |
| Modulation (mod matrix, LFO routing) | sonido-synth | `cargo run -p sonido-synth --example modulation_demo` |
| Tempo sync (BPM, note divisions) | sonido-core | `cargo run -p sonido-core --example tempo_sync_demo` |
| Presets and config (factory presets, chains) | sonido-config | `cargo run -p sonido-config --example preset_demo` |
| Analysis (FFT, dynamics, filter banks) | sonido-analysis | `cargo run -p sonido-analysis --example analysis_demo` |

## WebAssembly Build

Sonido's GUI compiles to `wasm32-unknown-unknown` for browser deployment. The same crate produces both native and wasm binaries without feature flags — cfg gates handle the differences at the source level.

### Prerequisites

```bash
# Install trunk (wasm build tool)
cargo install trunk

# Add the wasm target
rustup target add wasm32-unknown-unknown
```

### Development Server

```bash
cd crates/sonido-gui && trunk serve
```

Opens at `http://localhost:8080`. Hot-reloads on source changes.

### Production Build

```bash
cd crates/sonido-gui && trunk build --release
```

Output in `crates/sonido-gui/dist/`. The `dist/` directory is self-contained and can be served from any static host.

### Compile Check (without full trunk build)

```bash
cargo check --target wasm32-unknown-unknown -p sonido-gui
```

This is faster than `trunk build` and sufficient to verify the wasm target compiles.

### What Works on Wasm

- Full effect chain with all 19 effects
- Real-time audio via cpal's wasm backend (WebAudio API)
- WAV file loading via browser file picker (`rfd::AsyncFileDialog`)
- Parameter automation and preset management
- All effect parameter panels, knobs, and meters
- Effect chain add/remove/reorder

### What Doesn't Work on Wasm

- **File system access**: Presets cannot be saved to the local filesystem from the browser. Preset management operates in memory only.
- **Audio device selection**: The browser manages the audio output device. Device selection UI is hidden on wasm.
- **CLAP plugin loading**: Plugins are native-only shared libraries. The wasm target has no plugin host.
- **Synchronous file dialogs**: `rfd::FileDialog` (blocking) is replaced by `rfd::AsyncFileDialog` on wasm.

### Architecture Notes

The same source file compiles to both targets using cfg gates:

```rust
// Native-only code (threads, filesystem, blocking I/O):
#[cfg(not(target_arch = "wasm32"))]
fn spawn_audio_thread() { ... }

// Wasm alternative:
#[cfg(target_arch = "wasm32")]
fn start_audio_worklet() { ... }
```

Key per-target differences:

| Concern | Native | Wasm |
|---------|--------|------|
| Time | `std::time::Instant` | `web_time::Instant` |
| File dialogs | `rfd::FileDialog` (sync) | `rfd::AsyncFileDialog` |
| WAV parsing | `hound` from filesystem | `hound` from byte slice |
| Audio | cpal with OS backend | cpal wasm backend (WebAudio) |
| Threads | `std::thread::spawn` | Not used (wasm is single-threaded) |
| Logging | `tracing-subscriber` fmt | `tracing-wasm` → browser console |

### CI Deployment

The wasm build can be deployed to GitHub Pages via manual workflow dispatch. The workflow is in `.github/workflows/pages.yml` (triggered with `gh workflow run pages.yml`).

---

## Next Steps

- See [GUI.md](GUI.md) for graphical interface documentation
- See [Effects Reference](EFFECTS_REFERENCE.md) for all effects and their parameters
- See [CLI Guide](CLI_GUIDE.md) for detailed CLI usage
- See [Architecture](ARCHITECTURE.md) for understanding the codebase
- See [DSP Fundamentals](DSP_FUNDAMENTALS.md) for signal processing theory
- See [Synthesis](SYNTHESIS.md) for oscillator and envelope details
