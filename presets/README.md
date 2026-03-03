# Sonido Presets

This directory contains preset files for the Sonido CLI.

## Usage

```bash
# Process a file with a preset
sonido process input.wav output.wav --preset presets/guitar_crunch.toml

# Real-time processing with a preset
sonido realtime --preset presets/tape_delay.toml
```

## Preset Format

Presets are TOML files with the following structure:

```toml
name = "Preset Name"
description = "Optional description"

[[effects]]
type = "effect_name"
[effects.params]
param1 = "value1"
param2 = "value2"

[[effects]]
type = "another_effect"
[effects.params]
param = "value"
```

## Available Effects

All 19 effects registered in the `EffectRegistry`:

| Effect | Description |
|--------|------------|
| `preamp` | Clean gain stage |
| `distortion` | Waveshaping distortion with ADAA |
| `compressor` | Dynamics compressor with soft knee |
| `gate` | Noise gate |
| `eq` | 3-band parametric EQ |
| `wah` | Auto/manual wah filter |
| `chorus` | Modulated delay chorus |
| `flanger` | Modulated short-delay flanger |
| `phaser` | Multi-stage allpass phaser |
| `tremolo` | Amplitude modulation tremolo |
| `delay` | Feedback delay with ping-pong and diffusion |
| `filter` | Resonant lowpass filter |
| `vibrato` | Multi-unit pitch vibrato |
| `tape` | Tape saturation with hysteresis |
| `reverb` | Freeverb-style algorithmic reverb |
| `limiter` | Brickwall lookahead limiter |
| `bitcrusher` | Bit depth and sample rate reduction |
| `ringmod` | Ring modulator with carrier oscillator |
| `stage` | Signal conditioning and stereo utility |

## Included Presets

- **guitar_crunch.toml** - Classic overdrive sound with subtle compression
- **tape_delay.toml** - Warm tape-style echo with saturation
- **subtle_chorus.toml** - Light stereo widening effect
- **clean_boost.toml** - Transparent gain boost
- **full_chain.toml** - Complete signal chain example

## Creating Custom Presets

1. Copy an existing preset as a starting point
2. Modify effect types and parameters
3. Test with `sonido process` or `sonido realtime`
4. Save with a descriptive name
