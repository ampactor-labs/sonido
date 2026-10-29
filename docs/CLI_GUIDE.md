# CLI Guide

Complete reference for the `sonido` command-line tool.

## Installation

```bash
cargo install --path crates/sonido-cli
```

> During development, use `cargo run -p sonido-cli -- <args>` instead of a bare `sonido` command.
> It builds from source every time, so you never run a stale binary. Add `--release` for
> real-time audio or batch processing where DSP speed matters.
>
> Examples in this guide show `sonido <cmd>` for brevity; when you run them, substitute `cargo run -p sonido-cli --`.

## Commands Overview

| Command | Description |
|---------|-------------|
| `process` | Process audio files through effects |
| `realtime` | Real-time audio processing |
| `play` | Play WAV files through effects |
| `generate` | Generate test signals and synthesis |
| `analyze` | Spectral and audio analysis |
| `compare` | A/B audio comparison |
| `info` | Display WAV file metadata |
| `devices` | List audio devices |
| `effects` | List available effects |
| `presets` | Manage effect presets |
| `patch` | Rig files: export a preset or DSL chain as a rig, inspect a rig |
| `pedal` | The pedal over USB: list, send, read and play rigs; update its firmware |

---

## process

Process an audio file through one or more effects.

### Basic Usage

```bash
sonido process <INPUT> [OUTPUT] [OPTIONS]
```

When OUTPUT is omitted, the output is written next to the input file, with a name
generated from the input stem and the effect specification. For example,
`sonido process input.wav --effect reverb` produces `input_reverb.wav`. User-supplied
parameters are part of the name (`--effect distortion --param drive=15` gives
`input_distortion_drive=15.wav`), chain effects are joined with `+`, graph splits are
written `S[path1+path2]`, and a preset contributes its name. Names longer than 200
characters are truncated.

### Options

| Option | Description |
|--------|-------------|
| `-e, --effect <NAME>` | Single effect to apply |
| `-c, --chain <SPEC>` | Effect chain specification |
| `-g, --graph <SPEC>` | Graph topology specification with split/merge |
| `-p, --preset <NAME>` | Preset name or path (factory, user, or TOML file) |
| `--param <KEY=VALUE>` | Effect parameter (can repeat, used with `--effect`) |
| `--block-size <N>` | Processing block size (default: 512) |
| `--bit-depth <N>` | Output bit depth: 16, 24, or 32 (default: 32) |
| `--mono` | Force mono output (default is always stereo, even for mono input) |

If several of `--preset`, `--chain` and `--effect` are given, the first in that order
takes precedence. `--chain` and `--graph` cannot be used together, and `--graph` cannot
be combined with `--effect`, `--chain` or `--preset`.

### Parameter Values

Parameter keys match a parameter's full name or short name, case-insensitively, with
`_` standing for a space (`room_size` → "Room Size"); a few aliases such as
`time` (delay) and `bits` (bitcrusher) are also accepted. Values are in the parameter's
display unit: dB, Hz, ms, and percent as 0 to 100 (so `mix=50`, not `mix=0.5`).
Enum parameters take their label (`waveshape=foldback`, `mode=manual`) and switches
take `on`/`off`. Out-of-range values are clamped. `sonido effects <name>` lists every
parameter with its range and default.

### Examples

```bash
# Single effect with auto-generated output filename
sonido process input.wav --effect distortion
# -> produces input_distortion.wav

# Single effect with explicit output and custom parameters
sonido process input.wav output.wav --effect distortion --param drive=15 --param tone=4

# Effect chain with auto-naming
sonido process input.wav \
    --chain "preamp:gain=6|distortion:drive=12|delay:time=300,feedback=40"
# -> produces input_preamp_gain=6+distortion_drive=12+delay_time=300_feedback=40.wav

# Graph topology with parallel compression
sonido process input.wav --graph "split(compressor:ratio=4; -) | limiter"

# Nested splits
sonido process input.wav --graph "split(split(distortion; chorus); reverb:mix=100) | limiter"

# Using a preset file
sonido process input.wav output.wav --preset presets/guitar_crunch.toml

# Output as 16-bit WAV
sonido process input.wav output.wav --effect compressor --bit-depth 16

# Force mono output (default is stereo)
sonido process input.wav --effect reverb --mono
```

### Chain Syntax

```
effect1:param1=value1,param2=value2|effect2:param=value|effect3
```

- Effects are separated by `|`
- Parameters are separated by `,`
- Parameter names and values are separated by `=`
- Effects with no parameters can omit the colon
- Whitespace around `|` separators is trimmed (ignored)

**Edge Cases:**

```bash
# Effects without parameters can omit the colon entirely
sonido process input.wav --chain "distortion|delay:time=300"

# Whitespace around pipes is ignored
sonido process input.wav --chain "preamp:gain=6 | distortion | delay:time=300"

# Empty segments between pipes are skipped
sonido process input.wav --chain "preamp:gain=6||distortion"
```

### Graph Syntax

The `--graph` flag extends chain syntax with `split()` for parallel routing (DAG topologies).
It cannot be combined with `--effect`, `--chain`, or `--preset`.

```
graph       ::= path
path        ::= segment ( '|' segment )*
segment     ::= '-' | 'split(' path ( ';' path )+ ')' | effect_spec
effect_spec ::= name ( ':' key '=' value ( ',' key '=' value )* )?
```

**Structural characters:**

| Character | Meaning |
|-----------|---------|
| `\|` | Chain: connects segments in series |
| `;` | Path separator inside `split()` |
| `()` | Split delimiters |
| `:` | Separates effect name from parameters |
| `,` | Separates parameters |
| `=` | Separates parameter key from value |
| `-` | Dry passthrough (only inside `split()`) |

**Split semantics:** All split paths are summed at the merge point. This doubles amplitude
for a wet/dry split, so use a limiter after the merge or reduce effect output levels.

**Constraints:** `split()` requires 2 to 8 paths (`MAX_SPLIT_TARGETS`). Empty paths are rejected. `-` is only valid
inside a split path. Splits can be nested.

```bash
# Parallel compression (wet + dry summed)
sonido process input.wav --graph "split(compressor:ratio=4,attack=5; -)"

# Wet/dry reverb blend with post-limiter
sonido process input.wav --graph "split(reverb:decay=90,mix=100; -) | limiter"

# Diamond delay (two different delay times merged)
sonido process input.wav --graph "split(delay:time=250; delay:time=500) | reverb:mix=30"

# Chains inside split paths
sonido process input.wav --graph "preamp | split(distortion:drive=20 | chorus; phaser | flanger) | reverb"

# Three-way split
sonido process input.wav --graph "split(distortion:drive=30; chorus:depth=60; reverb:mix=100) | limiter"

# Nested splits
sonido process input.wav --graph "split(split(distortion; chorus); reverb:mix=100) | limiter"

# Linear chains work too (backward compatible with --chain)
sonido process input.wav --graph "preamp:gain=8 | distortion:drive=25 | reverb:decay=80"
```

---

## realtime

Process live audio through effects in real time.

### Basic Usage

```bash
sonido realtime [OPTIONS]
```

### Options

| Option | Description |
|--------|-------------|
| `-e, --effect <NAME>` | Single effect to apply |
| `-c, --chain <SPEC>` | Effect chain specification |
| `-p, --preset <NAME>` | Preset name or path (factory, user, or file) |
| `--param <KEY=VALUE>` | Effect parameter (can repeat, used with `--effect`) |
| `-i, --input <DEVICE>` | Input device (index, name, or partial name); alias `--input-device` |
| `-o, --output <DEVICE>` | Output device (index, name, or partial name); alias `--output-device` |
| `--sample-rate <N>` | Sample rate (default: 48000) |
| `--buffer-size <N>` | Buffer size in samples (default: 256) |
| `--mono` | Force mono processing |

### Device Selection

Devices can be selected by:
- **Index**: Use the number shown in `sonido devices list` (e.g., `--input 0`)
- **Exact name**: Full device name (e.g., `--input "USB Audio Interface"`)
- **Partial name**: Case-insensitive substring match (e.g., `--input "USB"`)

### Examples

```bash
# Simple chorus effect
sonido realtime --effect chorus --param rate=2 --param depth=60

# Select devices by index
sonido realtime --input 0 --output 0 --effect reverb

# Select devices by partial name match
sonido realtime --input "USB" --output "USB" --effect distortion

# Chain with custom devices
sonido realtime \
    --chain "preamp:gain=6|distortion:drive=12" \
    --input "USB Audio" \
    --output "Built-in Output"

# Using a preset
sonido realtime --preset tape_warmth

# Lower latency with smaller buffer
sonido realtime --effect delay --buffer-size 128
```

Press `Ctrl+C` to stop real-time processing.

---

## play

Play a WAV file through optional effects with real-time output.

### Basic Usage

```bash
sonido play <FILE> [OPTIONS]
```

### Options

| Option | Description |
|--------|-------------|
| `-e, --effect <NAME>` | Single effect to apply during playback |
| `-c, --chain <SPEC>` | Effect chain specification |
| `-p, --preset <NAME>` | Preset name or path (factory, user, or file) |
| `--param <KEY=VALUE>` | Effect parameter (used with `--effect`) |
| `-o, --output <DEVICE>` | Output device (index, name, or partial name) |
| `-l, --loop` | Loop playback continuously (alias `--repeat`) |
| `--mono` | Force mono output |
| `--buffer-size <N>` | Buffer size in frames (default: 1024) |

### Examples

```bash
# Play a file with no effects
sonido play test.wav

# Play through a reverb effect
sonido play vocals.wav --effect reverb --param mix=40

# Play through a chain of effects, looping
sonido play drums.wav --chain "compressor:ratio=4|delay:time=300" --loop

# Play with a preset on a specific output device
sonido play guitar.wav --preset tape_warmth --output "USB Audio"

# Force mono output
sonido play stereo_mix.wav --mono
```

Press `Ctrl+C` to stop playback.

---

## generate

Generate test signals for analysis and testing.

### Subcommands

#### sweep

Generate an exponential (logarithmic) sine sweep (chirp).

```bash
sonido generate sweep <OUTPUT> [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--start <HZ>` | Start frequency (default: 20) |
| `--end <HZ>` | End frequency (default: 20000) |
| `--duration <SEC>` | Duration in seconds (default: 2.0) |
| `--sample-rate <N>` | Sample rate (default: 48000) |
| `--amplitude <N>` | Amplitude 0-1 (default: 0.8) |

```bash
sonido generate sweep sweep.wav --start 20 --end 20000 --duration 3.0
```

#### tone

Generate a sine tone.

```bash
sonido generate tone <OUTPUT> [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--freq <HZ>` | Frequency (default: 440) |
| `--duration <SEC>` | Duration in seconds (default: 1.0) |
| `--sample-rate <N>` | Sample rate (default: 48000) |
| `--amplitude <N>` | Amplitude 0-1 (default: 0.8) |

```bash
sonido generate tone a440.wav --freq 440 --duration 2.0
```

#### noise

Generate white noise.

```bash
sonido generate noise <OUTPUT> [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--duration <SEC>` | Duration in seconds (default: 1.0) |
| `--sample-rate <N>` | Sample rate (default: 48000) |
| `--amplitude <N>` | Amplitude 0-1 (default: 0.5) |

```bash
sonido generate noise noise.wav --duration 1.0 --amplitude 0.3
```

#### impulse

Generate a single impulse (Dirac delta).

```bash
sonido generate impulse <OUTPUT> [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--length <N>` | Length in samples (default: 48000) |
| `--sample-rate <N>` | Sample rate (default: 48000) |
| `--amplitude <N>` | Impulse amplitude (default: 1.0) |

> **Note:** Unlike other generate commands, `--length` is specified in samples, not seconds.
> For a 1-second impulse response at 48kHz, use `--length 48000`.

```bash
# 1 second impulse at 48kHz
sonido generate impulse impulse.wav --length 48000

# 2 second impulse at 44.1kHz
sonido generate impulse impulse.wav --length 88200 --sample-rate 44100
```

#### silence

Generate silence.

```bash
sonido generate silence <OUTPUT> [OPTIONS]
```

```bash
sonido generate silence silence.wav --duration 2.0
```

#### osc

Generate an oscillator waveform using PolyBLEP anti-aliasing, which gives cleaner waveforms than simple mathematical generation.

```bash
sonido generate osc <OUTPUT> [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--freq <HZ>` | Frequency (default: 440) |
| `--waveform <TYPE>` | Waveform: sine, triangle, saw, square, noise (default: sine) |
| `--duration <SEC>` | Duration in seconds (default: 1.0) |
| `--sample-rate <N>` | Sample rate (default: 48000) |
| `--amplitude <N>` | Amplitude 0-1 (default: 0.8) |
| `--pulse-width <N>` | Pulse width 0-1; with `--waveform square`, a value other than 0.5 gives a pulse wave (default: 0.5) |

```bash
# Generate a saw wave
sonido generate osc saw.wav --freq 220 --waveform saw --duration 2.0

# Generate a square wave with custom pulse width
sonido generate osc pulse.wav --freq 440 --waveform square --pulse-width 0.25
```

#### chord

Generate a chord using the polyphonic synthesizer with ADSR envelope.

```bash
sonido generate chord <OUTPUT> [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--notes <MIDI>` | Comma-separated MIDI notes, required (e.g., "60,64,67" for C major) |
| `--duration <SEC>` | Duration in seconds (default: 2.0) |
| `--sample-rate <N>` | Sample rate (default: 48000) |
| `--amplitude <N>` | Amplitude 0-1 (default: 0.5) |
| `--waveform <TYPE>` | Waveform: sine, triangle, saw, square, noise (default: saw) |
| `--filter-cutoff <HZ>` | Filter cutoff frequency (default: 2000) |
| `--attack <MS>` | Envelope attack time (default: 10) |
| `--release <MS>` | Envelope release time (default: 500) |

```bash
# Generate C major chord
sonido generate chord cmajor.wav --notes "60,64,67" --duration 3.0

# Generate Am7 with custom envelope
sonido generate chord am7.wav --notes "57,60,64,67" --waveform saw --attack 50 --release 1000

# Generate power chord with filtered saw
sonido generate chord power.wav --notes "40,47" --filter-cutoff 1500 --duration 2.0
```

Common MIDI note values:
- C4 = 60, D4 = 62, E4 = 64, F4 = 65, G4 = 67, A4 = 69, B4 = 71
- C3 = 48, C5 = 72

#### adsr

Generate a test tone with configurable ADSR envelope for visualizing envelope shapes.

```bash
sonido generate adsr <OUTPUT> [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--attack <MS>` | Attack time in milliseconds (default: 50) |
| `--decay <MS>` | Decay time in milliseconds (default: 100) |
| `--sustain <N>` | Sustain level 0-1 (default: 0.7) |
| `--release <MS>` | Release time in milliseconds (default: 200) |
| `--freq <HZ>` | Test tone frequency (default: 440) |
| `--gate-duration <SEC>` | Gate on duration before release (default: 1.0) |
| `--sample-rate <N>` | Sample rate (default: 48000) |
| `--amplitude <N>` | Amplitude 0-1 (default: 0.8) |

```bash
# Standard ADSR test
sonido generate adsr envelope.wav --attack 50 --decay 100 --sustain 0.7 --release 200

# Pad-style long envelope
sonido generate adsr pad.wav --attack 500 --decay 200 --sustain 0.8 --release 2000 --gate-duration 3.0

# Pluck-style fast envelope
sonido generate adsr pluck.wav --attack 1 --decay 50 --sustain 0.0 --release 100
```

---

## analyze

Spectral analysis tools.

### Subcommands

#### spectrum

Compute and display the frequency spectrum.

```bash
sonido analyze spectrum <INPUT> [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--fft-size <N>` | FFT size (default: 4096) |
| `--window <TYPE>` | Window function: hamming, blackman, hann, rectangular (`rect`/`none`) (default: blackman; unknown names fall back to blackman) |
| `-o, --output <FILE>` | Output CSV file |
| `--peaks <N>` | Show top N peaks (default: 10) |
| `--method <METHOD>` | `fft` (single frame) or `welch` (averaged periodogram) (default: fft); `--welch` is a hidden legacy alias for `--method welch` |
| `--overlap <N>` | Overlap ratio for Welch's method, 0.0-1.0 (default: 0.5) |

```bash
# Analyze and show top peaks
sonido analyze spectrum recording.wav --peaks 20

# Export to CSV
sonido analyze spectrum recording.wav --output spectrum.csv
```

#### transfer

Compute transfer function between input and output recordings.

```bash
sonido analyze transfer <INPUT> <OUTPUT_FILE> [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--fft-size <N>` | FFT size (default: 4096) |
| `-o, --output <FILE>` | Output JSON or FRD file (format detected from extension) |
| `--group-delay` | Include group delay in output |
| `--smooth <N>` | Apply 1/N octave smoothing (e.g., 3 for 1/3 octave) |

```bash
# Measure pedal response
sonido analyze transfer dry.wav wet.wav --output response.json

# Export as FRD for REW/Room EQ Wizard
sonido analyze transfer dry.wav wet.wav --output response.frd

# With group delay and smoothing
sonido analyze transfer dry.wav wet.wav --group-delay --smooth 3 --output response.json
```

#### ir

Extract impulse response using deconvolution.

```bash
sonido analyze ir <SWEEP> <RESPONSE> -o <OUTPUT>
```

| Option | Description |
|--------|-------------|
| `-o, --output <FILE>` | Output IR file (required) |
| `--rt60` | Estimate and display RT60 (T30), T20 and EDT |
| `--format <FMT>` | `wav` (default) or `csv` (`sample_index,amplitude`, written to the `-o` path) |
| `--window-size <N>` | Override the deconvolution FFT size (default: next power of two) |

```bash
# 1. Generate sweep
sonido generate sweep sweep.wav --duration 3.0

# 2. Play through system and record
# (external step)

# 3. Extract IR
sonido analyze ir sweep.wav recorded.wav -o impulse_response.wav

# With RT60 estimation
sonido analyze ir sweep.wav recorded.wav -o impulse_response.wav --rt60
```

#### distortion

Analyze harmonic distortion (THD, THD+N).

```bash
sonido analyze distortion <INPUT> [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--fundamental <HZ>` | Fundamental frequency (auto-detected if omitted) |
| `--fft-size <N>` | FFT size (default: 8192) |
| `-o, --output <FILE>` | Output JSON file |
| `--harmonics <N>` | Number of harmonics to display, including the fundamental (default: 6) |

```bash
# Analyze distortion of a test tone
sonido analyze distortion test_tone.wav --fft-size 16384

# With known fundamental
sonido analyze distortion 1khz_through_amp.wav --fundamental 1000
```

#### spectrogram

Generate time-frequency spectrogram.

```bash
sonido analyze spectrogram <INPUT> -o <OUTPUT.csv> [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--fft-size <N>` | FFT size (default: 2048) |
| `--hop <N>` | Hop size (default: fft_size / 4) |
| `-o, --output <FILE>` | Output CSV file (required) |

Frames use a Hann window.

```bash
sonido analyze spectrogram recording.wav -o spectrogram.csv --fft-size 4096 --hop 512
```

#### dynamics

Analyze dynamics (RMS, crest factor, dynamic range).

```bash
sonido analyze dynamics <INPUT>
```

```bash
sonido analyze dynamics master.wav
```

Output includes:
- Peak level (dBFS)
- RMS level (dBFS)
- Crest factor (dB)
- Dynamic range (dB)
- Headroom (dB)

#### pac

Analyze Phase-Amplitude Coupling between frequency bands.

```bash
sonido analyze pac <INPUT> [OPTIONS]
```

| Option | Description | Default |
|--------|-------------|---------|
| `--phase-low <HZ>` | Phase band lower frequency | 4.0 |
| `--phase-high <HZ>` | Phase band upper frequency | 8.0 |
| `--amp-low <HZ>` | Amplitude band lower frequency | 30.0 |
| `--amp-high <HZ>` | Amplitude band upper frequency | 100.0 |
| `--method <METHOD>` | `mvl` (Mean Vector Length) or `kl` (Kullback-Leibler) | mvl |
| `--surrogates <N>` | Number of surrogate iterations for significance testing | 0 |
| `-o, --output <FILE>` | Output JSON file | - |

```bash
# Analyze phase-amplitude coupling between frequency bands
sonido analyze pac input.wav \
    --phase-low 4 --phase-high 8 \
    --amp-low 30 --amp-high 80 \
    --method mvl

# With surrogate significance testing
sonido analyze pac input.wav \
    --phase-low 4 --phase-high 8 \
    --amp-low 30 --amp-high 100 \
    --surrogates 200 \
    --output pac_results.json
```

Output includes:
- Modulation Index (0-1 coupling strength)
- Preferred phase (radians and degrees)
- Amplitude distribution by phase bin
- p-value (if surrogates > 0)

See [CFC Analysis](reference/cfc-analysis.md) for detailed PAC analysis documentation.

#### comodulogram

Compute coupling across multiple frequency pairs.

```bash
sonido analyze comodulogram <INPUT> -o <OUTPUT.csv> [OPTIONS]
```

| Option | Description | Default |
|--------|-------------|---------|
| `--phase-range <LOW-HIGH>` | Phase frequency range | 2-20 |
| `--amp-range <LOW-HIGH>` | Amplitude frequency range | 20-200 |
| `--phase-step <HZ>` | Phase frequency step | 2.0 |
| `--amp-step <HZ>` | Amplitude frequency step | 10.0 |
| `--bandwidth <RATIO>` | Bandwidth as fraction of center frequency | 0.5 |
| `-o, --output <FILE>` | Output CSV file (required) | - |

```bash
# Full comodulogram
sonido analyze comodulogram recording.wav \
    --phase-range 2-20 \
    --amp-range 20-200 \
    --phase-step 2 \
    --amp-step 10 \
    --output comodulogram.csv
```

The output CSV can be visualized as a heatmap showing coupling strength across frequency pairs.

#### bandpass

Extract a frequency band using bandpass filtering.

```bash
sonido analyze bandpass <INPUT> -o <OUTPUT.wav> [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--low <HZ>` | Lower cutoff frequency (required) |
| `--high <HZ>` | Upper cutoff frequency (required) |
| `--order <N>` | Filter order: 2, 4, or 6 (default: 4); the band filter is applied `order / 2` times |
| `-o, --output <FILE>` | Output WAV file (required) |

```bash
# Extract theta band (4-8 Hz)
sonido analyze bandpass eeg.wav --low 4 --high 8 -o theta_band.wav

# Extract with higher-order filter for sharper cutoff
sonido analyze bandpass eeg.wav --low 30 --high 80 --order 6 -o gamma_band.wav
```

#### hilbert

Extract instantaneous phase and amplitude using Hilbert transform.

```bash
sonido analyze hilbert <INPUT> [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--phase-output <FILE>` | Output WAV file for phase (alias `--phase`) |
| `--amp-output <FILE>` | Output WAV file for amplitude envelope (alias `--envelope`) |
| `--bandpass <LOW-HIGH>` | Optional bandpass filter before transform |

```bash
# Extract amplitude envelope
sonido analyze hilbert recording.wav --amp-output envelope.wav

# Extract phase after bandpass filtering
sonido analyze hilbert eeg.wav \
    --bandpass 4-8 \
    --phase-output theta_phase.wav \
    --amp-output theta_amplitude.wav
```

The phase output is normalized to [-1, 1] (representing [-pi, pi] radians).
The amplitude output is normalized to [0, 1]. Without either output flag, only the
envelope statistics (mean/min/max) are printed.

#### imd

Analyze Intermodulation Distortion (IMD) using a two-tone test signal.

```bash
sonido analyze imd <INPUT> [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--freq1 <HZ>` | First tone frequency (auto-detected if omitted) |
| `--freq2 <HZ>` | Second tone frequency (auto-detected if omitted) |
| `--fft-size <N>` | FFT size (default: 8192) |
| `-o, --output <FILE>` | Output JSON file |

IMD analysis measures distortion products created when two tones interact in a nonlinear system:

- **Second-order products**: f1+f2, f2-f1
- **Third-order products**: 2f1-f2, 2f2-f1

```bash
# Generate two-tone test signal first
sonido generate tone tone1.wav --freq 1000 --duration 2.0
sonido generate tone tone2.wav --freq 1200 --duration 2.0
# (mix them externally or use a stereo file)

# Analyze IMD
sonido analyze imd two_tone_output.wav --freq1 1000 --freq2 1200 --output imd.json
```

Output includes:
- Fundamental frequencies and levels (dB)
- Second-order product levels (dB)
- Third-order product levels (dB)
- Overall IMD ratio (percent and dB)

#### cqt

Constant-Q Transform analysis with logarithmic frequency resolution. It suits musical pitch analysis, where you need equal resolution per octave.

```bash
sonido analyze cqt <INPUT> [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--min-freq <HZ>` | Minimum frequency (default: 32.7, C1) |
| `--max-freq <HZ>` | Maximum frequency (default: the lower of Nyquist and 8000 Hz) |
| `--bins-per-octave <N>` | Bins per octave (default: 12 for semitone resolution) |
| `--peaks <N>` | Show top N peaks (default: 10) |
| `--chromagram` | Also compute pitch class profile |
| `-o, --output <FILE>` | Output CSV file |

```bash
# Analyze pitch content of a recording
sonido analyze cqt recording.wav --peaks 10

# Quarter-tone resolution (24 bins per octave)
sonido analyze cqt microtonal.wav --bins-per-octave 24

# Guitar range analysis with chromagram
sonido analyze cqt guitar.wav --min-freq 82.4 --max-freq 1320 --chromagram

# Export to CSV for visualization
sonido analyze cqt music.wav --output cqt.csv
```

Output includes:
- Frequency peaks with MIDI note numbers and note names
- Magnitude in dB
- Chromagram showing pitch class distribution (with `--chromagram`)

---

## compare

Compare two audio files (A/B comparison for reverse engineering).

```bash
sonido compare <REFERENCE> <IMPLEMENTATION> [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--fft-size <N>` | FFT size for spectral analysis (default: 4096) |
| `-o, --output <FILE>` | Output detailed JSON report |
| `--detailed` | Show per-band frequency analysis |

Both files are trimmed to the shorter length. The command calculates:
- MSE, RMSE and SNR (time domain)
- RMS and peak level of each file, and their difference
- Spectral correlation and average magnitude difference (dB)
- Per-band breakdown, sub bass through brilliance (with `--detailed`)
- A match-quality summary (Excellent / Good / Fair / Poor)

`sonido analyze compare` takes the same arguments.

```bash
# Basic comparison
sonido compare hardware_recording.wav software_output.wav

# Detailed frequency band analysis with JSON export
sonido compare dry.wav wet.wav --detailed --output report.json
```

---

## info

Display WAV file metadata without processing.

### Basic Usage

```bash
sonido info <FILE>
```

### Output

Shows:
- **Format**: PCM or IEEE Float with bit depth
- **Channels**: channel count
- **Sample Rate**: in Hz
- **Duration**: in seconds and total frames
- **File Size**: formatted as B/KB/MB

### Example

```bash
sonido info recording.wav
# File:        recording.wav
# Format:      PCM 24-bit
# Channels:    2
# Sample Rate: 48000 Hz
# Duration:    3.500s (168000 frames)
# File Size:   984.4 KB
```

For basic WAV inspection, you do not need an external tool such as `soxi`.

---

## devices

List and manage audio devices. `sonido devices` with no subcommand runs `list`.

### Subcommands

#### list

List all available audio devices with indices.

```bash
sonido devices list [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--include-virtual` | Show virtual/loopback device info and setup guidance |

Output shows numbered devices that can be used with `--input` and `--output`:

```
Available Audio Devices
=======================

Input Devices:
  [0] Built-in Microphone (48000 Hz)
  [1] USB Audio Interface (44100 Hz) (also output)

Output Devices:
  [0] Built-in Speaker (48000 Hz)
  [1] USB Audio Interface (44100 Hz) (also input)

Total: 2 input(s), 2 output(s)
```

#### info

Show default device information.

```bash
sonido devices info
```

### Virtual Audio / Loopback

To capture system audio (e.g., for recording what's playing), use `--include-virtual` to see loopback device guidance:

```bash
sonido devices list --include-virtual
```

If no loopback devices are detected, platform-specific installation instructions are shown:
- **Windows**: VB-Audio Virtual Cable
- **macOS**: BlackHole
- **Linux**: PulseAudio/PipeWire `module-loopback` or `module-null-sink`

---

## effects

List available effects and their parameters.

```bash
# List all effects
sonido effects

# Show details for a specific effect by registry ID (parameters, ranges, defaults, examples)
sonido effects <EFFECT>

# Show example CLI commands
sonido effects --examples
```

The list shows all 36 registered effects (registry ID and description). The detail
view shows each parameter's short name (usable as a `--param` key), full name, default
and range.

### Effect Aliases

`--effect`, `--chain` and `--graph` accept these aliases (case-insensitive; the
underscore spellings such as `noise_gate` work too). Every other effect is named by its
registry ID, as listed by `sonido effects`.

| Effect | Alias(es) |
|--------|-----------|
| `filter` | `lowpass` |
| `vibrato` | `multivibrato` |
| `tape` | `tapesaturation` |
| `preamp` | `cleanpreamp` |
| `gate` | `noisegate` |
| `wah` | `autowah` |
| `eq` | `parametriceq`, `peq` |
| `bitcrusher` | `crusher` |
| `ringmod` | `ring` |
| `harmonic_habitat` | `habitat`, `harmonic`, `harmonichabitat` |

Examples:
```bash
# These are equivalent
sonido process input.wav --effect filter --param cutoff=2000
sonido process input.wav --effect lowpass --param cutoff=2000

# These are equivalent
sonido process input.wav --effect vibrato --param depth=60
sonido process input.wav --effect multivibrato --param depth=60

# These are equivalent
sonido process input.wav --effect gate --param threshold=-40
sonido process input.wav --effect noisegate --param threshold=-40
```

---

## presets

Manage effect presets (list, show, save, delete, copy, export-factory, paths).
Factory presets are built in: `init`, `crunch`, `high_gain`, `ambient`, `tape_warmth`,
`clean_studio`, `80s_chorus`, `slapback`. Names match case-insensitively, by ID or by
display name.

### Subcommands

#### list

List available presets.

```bash
sonido presets list [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--factory` | Show only factory presets |
| `--user` | Show only user presets |

#### show

Show details of a preset (name, description, sample rate, effects and parameters).

```bash
sonido presets show <NAME>
```

#### save

Save an effect chain as a user preset.

```bash
sonido presets save <NAME> --chain <SPEC> [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `-c, --chain <SPEC>` | Effect chain specification (required) |
| `-d, --description <TEXT>` | Preset description |
| `--force` | Overwrite if preset exists |

```bash
sonido presets save my_tone --chain "preamp:gain=6|distortion:drive=12" \
    --description "My custom crunch tone"
```

#### delete

Delete a user preset.

```bash
sonido presets delete <NAME> [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--force` | Required to confirm the deletion |

#### copy

Copy a factory preset to user presets for customization.

```bash
sonido presets copy <SOURCE> [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `-n, --name <NAME>` | New preset name (default: source name) |

The source must be a factory preset; copying fails if a user preset with the target
name already exists.

```bash
sonido presets copy crunch --name my_crunch
```

#### export-factory

Export all factory presets to a directory as individual TOML files. Existing files are
skipped unless `--force` is given.

```bash
sonido presets export-factory <OUTPUT_DIR> [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--force` | Overwrite existing files |

```bash
sonido presets export-factory ./presets/
```

This is useful for:
- Distributing presets with release builds
- Inspecting factory preset configurations
- Using as templates for custom presets

#### paths

Show the user preset, system preset and config directories.

```bash
sonido presets paths
```

---

## pedal

Talk to a Sonido pedal over USB (`crates/sonido-cli/src/commands/pedal.rs`). While it
plays, the pedal is a USB serial device (`1209:0001`) that communicates over the rig link
(`sonido_patch::link`; see `docs/EMBEDDED.md`, USB Rig Link). These commands list and
play its rigs, send rigs to its bank, read rigs and the jam back as editor JSON, and
update its firmware.

```bash
sonido pedal controls                     # how to play it: an illustrated guide, opened in your browser (no pedal needed)
sonido pedal controls delay               # the guide, opened at one effect's knobs
sonido pedal controls --text              # the same as an 80-column terminal card
sonido pedal controls --text delay        # one effect's knobs, with their travel and noon
sonido pedal controls --html guide.html   # write the guide to a file (to print or share)
sonido pedal info                         # firmware, CPU load, what it plays
sonido pedal list                         # the rig list, in footswitch order
sonido pedal push my_rig.json             # replaces the bank rig with its name, or takes the first empty slot
sonido pedal push --from-dsl "distortion:drive=20 | reverb:mix=30" --name "Dirty Verb"
sonido pedal push my_rig.json --slot 3    # into bank slot 3
sonido pedal pull 3 -o slot3.json         # bank slot 3 as editor JSON
sonido pedal pull jam                     # print the jam
sonido pedal erase 3
sonido pedal play "Space Echo"            # jam, a bank slot 1–15, or a factory rig
sonido pedal update sonido_pedal.bin      # firmware update over USB DFU
```

### Subcommands

| Subcommand | Description |
|------------|-------------|
| `controls [EFFECT] [--text] [--html FILE]` | Open an illustrated guide to playing the pedal in the default browser: a drawing of the pedal with every knob, toggle, LED and footswitch called out (each footswitch press per T2 position, each toggle position), the knobs' three pages colour-coded by mode (jam sound, jam setup, rig), the jam's effect-pick dial, the rig list, every jam effect's and factory rig's K1 to K6 and the power-on gestures. With an effect, the guide opens at that effect. `--text` prints the same facts as an 80-column card instead (ANSI-styled on a terminal unless `NO_COLOR` is set); with an effect, that effect's knobs with each one's travel (fully left … fully right, including the pedal's knob floors) and noon. `--html FILE` writes the page without opening it; the page is self-contained and can be printed. It needs no pedal: the guide is generated from this build's `sonido_patch::controls`, knob maps, effect parameters and factory rigs (`sonido_rigs::guide`), so it matches firmware built from the same source |
| `info` | Firmware and link protocol, what the pedal plays (and whether it is off), its morph position and CPU load (last second's mean and peak), and its bank slots, factory rigs and effect count |
| `list` | The rig list, in the order the footswitches step through it: the jam with its effects, each bank slot, the factory rigs; marks the one playing |
| `push [FILE]` | Send a rig to the bank: it replaces the bank rig with the same name, or takes the first empty slot (`-s, --slot <N>` selects a bank slot from 1 to 15 instead). The rig is checked here with `validate_for_daisy`, then again by the pedal, which plays it at once |
| `pull <WHAT>` | Save a bank rig (`1` to `15`) or the jam (`jam`) as editor JSON (`-o, --output <FILE>`); prints the rig without `-o` |
| `erase <SLOT>` | Empty a bank slot from 1 to 15 (if its rig was playing, the pedal returns to the jam) |
| `play <RIG>` | Play `jam`, a bank slot from `1` to `15`, or a factory rig by name (case-insensitive) |
| `update [FIRMWARE]` | Restart the pedal into its bootloader; with a firmware `.bin`, flash it with `dfu-util` (the pedal then restarts into it); without one, print the `dfu-util` command |

`push` reads its rig like [`patch export`](#rig-export): a `.json` rig from the
editor, a `.bin` sector, a preset `.toml`, or `--from-dsl`, with `-t, --topology` and
`--name`. The global `--port <PORT>` names the serial port; without it the first
Sonido pedal found is used. Bank slots are numbered 1 to 15 here (the link counts from 0).

`update` restarts the running pedal into its bootloader over USB, waits up to 10 s for
the Daisy bootloader to appear, and flashes the image to `0x90040000`; it needs
`dfu-util` installed, and it rejects the STM32 ROM bootloader (BOOT held through
RESET), which cannot reach the pedal's QSPI flash. Download `sonido_pedal.bin` from a
GitHub release (or a CI run's `sonido_pedal` artifact), or build the image as in
`crates/sonido-daisy/examples/sonido_pedal.rs`:

```bash
cd crates/sonido-daisy
cargo objcopy --example sonido_pedal --release --features alloc,platform -- -O binary sonido_pedal.bin
```

---

## Preset Files

Presets are TOML files defining effect chains with an optional topology
(`presets/` in the repository has examples):

```toml
name = "Guitar Crunch"
description = "Classic overdrive sound"
topology = "linear"  # optional: "linear" (default), "parallel", "fan"

[[effects]]
type = "preamp"
[effects.params]
gain = "6"

[[effects]]
type = "distortion"
[effects.params]
drive = "15"
tone = "5"
output = "-6"
```

Parameter values follow the same rules as `--param`. An effect entry with
`bypassed = true` is skipped.

The `topology` field is optional and defaults to linear chaining.
Valid values: `"linear"`, `"parallel"`, `"fan"`. `process`, `realtime` and `play` always
run a preset as a linear chain, and `sonido patch export` and `sonido pedal push` take
the topology from their `--topology` option (default `linear`, or `parallel`), not
from this field.

Use with:

```bash
sonido process input.wav output.wav --preset my_preset.toml
sonido realtime --preset my_preset.toml
```

### Rig Export

`sonido patch` (`crates/sonido-cli/src/commands/patch.rs`) works on rig files: the
canonical `sonido-patch` model, which stores effects by stable UID and carries macros,
morph and globals. `export` reads a rig (a `.json` from the editor or a `.bin`
sector) or builds one from a preset `.toml` or a DSL chain, and writes it as `.json`
(for the editor and the graph player) or as a `.bin` pedal sector, chosen by the output
file's extension:

```bash
sonido patch export my_preset.toml -o rig.json
sonido patch export --from-dsl "distortion:drive=20 | reverb:mix=30" --name "Dirty Verb" -o rig.bin --pedal
sonido patch export rig.json -o rig.bin     # editor JSON → pedal sector
sonido patch inspect rig.bin                # or a .json rig
```

| Option | Description |
|--------|-------------|
| `[FILE]` | A rig (`.json` from the editor, a `.bin` sector) or a preset `.toml` (conflicts with `--from-dsl`) |
| `--from-dsl <SPEC>` | A DSL chain instead of a file |
| `-o, --output <FILE>` | Output file (required): `.json` writes JSON, any other extension the binary sector |
| `-t, --topology <NAME>` | For a preset or DSL chain: `linear` (default) or `parallel` (all effects between one split/merge) |
| `--name <NAME>` | Name the rig (default: the rig's own name, the preset's name, or `dsl`) |
| `--pedal` | Fail unless the rig runs on the pedal: `sonido_patch::validate::validate_for_daisy`, the effect set and the CPU and memory rule the pedal itself enforces (warnings are printed; errors fail) |

Up to 8 effects (`MAX_NODES`). Arbitrary split/merge graphs are authored in the GUI.
The `.bin` is the sector the pedal's rig bank holds. To put a rig on the pedal, send
it with [`sonido pedal push`](#pedal), which checks it the same way; the pedal plays
it at once. `inspect` reads a `.bin` or `.json` rig and prints the name, format version,
globals, morph settings, each node's parameters (A/B where they differ), edges and
active macros.

### DSL Strings

`--from-dsl` takes the [Graph Syntax](#graph-syntax) grammar:

```
distortion:drive=20|reverb:decay=80
split(chorus:depth=60; -)|reverb:mix=50
```
