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

### Parameter names and values

A parameter key is the parameter's name, short name or string id, ignoring
case, spaces, dashes and underscores. For example, the keys `room_size`,
`"Room Size"`, `room` and `rev_room_size` all select the reverb's Room Size.
`sonido effects <name>` lists an effect's parameters with their ranges.

Values are in the parameter's own units, never normalized 0-1:

| Unit | Example | Notes |
|------|---------|-------|
| dB | `drive = "12"`, `tone = "-2 dB"` | Distortion Tone is a +/-12 dB peak/cut centred on 1 kHz |
| Hz | `cutoff = "6000"`, `cutoff = "6kHz"` | |
| ms | `time = "375"`, `time = "1.5s"` | seconds convert to ms |
| % | `mix = "30"`, `mix = "30%"` | 0-100 (some go further: chorus Feedback tops at 70, vibrato Depth at 400, stereo Width at 200) |
| ratio | `ratio = "4"`, `ratio = "4:1"`; filter `resonance` is a Q | |
| step | `waveshape = "Hard Clip"` or `waveshape = "1"` | a step label (any case) or its index |

Unknown keys and out-of-range values are reported (out-of-range values are
clamped); `cargo test -p sonido-config` checks that every preset here loads
without errors.

## Available Effects

All 36 effects registered in the `EffectRegistry` (`sonido effects` lists them
with their parameters):

| Effect | Description |
|--------|------------|
| `distortion` | Anti-aliased waveshaping distortion with multiple algorithms |
| `compressor` | Dynamics compressor with program-dependent release |
| `chorus` | Multi-voice modulated delay chorus with feedback and tempo sync |
| `flanger` | Classic flanger with through-zero mode, bipolar feedback, and tempo sync |
| `phaser` | Multi-stage allpass phaser with LFO and tempo sync |
| `delay` | Feedback delay with filtering, diffusion, and tempo sync |
| `filter` | Multimode biquad filter (LPF/HPF/BPF/Notch) |
| `vibrato` | 6-unit tape wow/flutter simulation |
| `tape` | Full tape-machine model with wow/flutter, hysteresis, head bump, and self-erasure |
| `preamp` | High-headroom gain stage |
| `reverb` | 8-line Hadamard FDN reverb on Freeverb delay tunings |
| `tremolo` | Amplitude modulation with multiple waveforms and tempo sync |
| `gate` | Noise gate with threshold, hold, hysteresis, and sidechain HPF |
| `wah` | Auto-wah and manual wah with envelope follower |
| `eq` | 3-band parametric equalizer with frequency, gain, and Q |
| `limiter` | Brickwall lookahead peak limiter with ceiling control |
| `bitcrusher` | Lo-fi bit depth and sample rate reduction with jitter |
| `ringmod` | Ring modulation with sine, triangle, and square carriers |
| `stage` | Signal conditioning: gain, phase, width, balance, bass mono, Haas delay |
| `looper` | Stereo looper with record, play, and overdub |
| `amp` | Dual gain stage + interactive tone stack + sag amp simulator |
| `cabinet` | Cabinet IR simulator with direct convolution and 3 factory impulse responses |
| `deesser` | Wideband sibilance reduction via sidechain HPF |
| `drone` | Sympathetic resonance generator with harmonically-related sustaining tones |
| `glitch` | Buffer manipulation effect: stutter, tape-stop, reverse, shuffle |
| `multiband_comp` | Three-band dynamics with Linkwitz-Riley crossovers |
| `pitch_shift` | Granular pitch shifter with overlapping Hann-windowed grain crossfade |
| `plate_reverb` | Dattorro-inspired plate algorithm with input diffusion and modulated tank |
| `shelving_eq` | Low shelf + high shelf with output gain |
| `spring_reverb` | Allpass dispersion chain modeling the boingy character of a spring reverb unit |
| `stereo_widener` | M/S width control, Haas delay, and bass mono |
| `texture` | Granular ambient pad from input with random grain positions and Hann-window envelopes |
| `time_stretch` | Independent pitch and time control via granular synthesis |
| `transient_shaper` | Independent attack/sustain envelope control |
| `tuner` | Chromatic tuner by YIN pitch detection, guitar and bass range (23 Hz up) |
| `harmonic_habitat` | Pitch-aware modal reverb tank that retunes its tail around the player's harmonic center |

## Included Presets

- **guitar_crunch.toml**: Classic soft-clip overdrive (preamp into distortion)
- **tape_delay.toml**: Warm tape-style echo with saturation
- **subtle_chorus.toml**: Light stereo widening effect
- **clean_boost.toml**: Transparent gain boost
- **full_chain.toml**: Complete signal chain example

## Creating Custom Presets

1. Copy an existing preset as a starting point
2. Modify effect types and parameters
3. Test with `sonido process` or `sonido realtime`
4. Save with a descriptive name
