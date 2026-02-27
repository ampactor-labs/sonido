# CLI Testing Protocol

Structured walkthrough for testing every aspect of the Sonido CLI and GUI.
Work through each phase in order. Every step has a checkbox and pass/fail criteria.

---

## Phase 0: Setup

### 0.1 Build

```bash
cargo build
```

- [ ] Workspace compiles without errors

### 0.2 Install CLI

```bash
make dev-install
```

- [ ] `sonido --version` prints version string

> All commands below use the `sonido` shorthand. If you skip dev-install, substitute
> `cargo run -p sonido-cli --` for `sonido` throughout.

### 0.3 WAV Inspection Tool

Sonido includes a built-in `info` command for WAV inspection. No external tools required.

```bash
sonido info demos/sine_440.wav
```

- [ ] Shows sample rate, channels, bit depth, duration, sample count

> `soxi` (from the `sox` package) is an optional alternative for stereo verification,
> but `sonido info` covers most inspection needs.

### 0.4 Generate Test Audio

```bash
mkdir -p demos
sonido generate tone demos/sine_440.wav --freq 440 --duration 2.0
sonido generate sweep demos/sweep.wav --duration 3.0
sonido generate noise demos/noise.wav --duration 2.0 --amplitude 0.3
```

- [ ] `demos/sine_440.wav` exists (96000 samples at 48kHz)
- [ ] `demos/sweep.wav` exists
- [ ] `demos/noise.wav` exists

### 0.5 Verify Audio Devices

```bash
sonido devices list
sonido devices info
```

- [ ] At least one input and one output device listed
- [ ] Default device info displayed

---

## Phase 1: CLI — Effect Discovery

### 1.1 Help and Version

```bash
sonido --help
sonido --version
```

- [ ] Help shows all 10 commands: process, realtime, generate, analyze, compare, devices, effects, presets, info, help

### 1.2 List Effects

```bash
sonido effects
```

- [ ] Lists 19 effects with descriptions

### 1.3 Effect Detail

```bash
sonido effects distortion
sonido effects reverb
```

- [ ] Shows parameters with names, ranges, defaults, and units

### 1.4 Effect Examples

```bash
sonido effects --examples
```

- [ ] Prints example CLI commands

### 1.5 File Info

```bash
sonido info demos/sine_440.wav
```

- [ ] Reports: format, sample rate (48000), channels (1 or 2), bit depth, duration (~2.0s), sample count

---

## Phase 2: CLI — Effect Processing

### 2.1 Default Params (All 19 Effects)

Every effect must produce audible output with zero `--param` flags. This validates
the default-alignment work. Play each output and confirm the effect is clearly present.

Pass = exits OK + auto-named output exists + audible effect when played.

| # | Command | Expected Output | Pass |
|---|---------|-----------------|------|
| 1 | `sonido process demos/sine_440.wav --effect preamp` | `demos/sine_440_preamp.wav` | [ ] |
| 2 | `sonido process demos/sine_440.wav --effect distortion` | `demos/sine_440_distortion.wav` | [ ] |
| 3 | `sonido process demos/noise.wav --effect compressor` | `demos/noise_compressor.wav` | [ ] |
| 4 | `sonido process demos/noise.wav --effect gate` | `demos/noise_gate.wav` | [ ] |
| 5 | `sonido process demos/sine_440.wav --effect eq` | `demos/sine_440_eq.wav` | [ ] |
| 6 | `sonido process demos/noise.wav --effect filter` | `demos/noise_filter.wav` | [ ] |
| 7 | `sonido process demos/sine_440.wav --effect wah` | `demos/sine_440_wah.wav` | [ ] |
| 8 | `sonido process demos/sine_440.wav --effect tremolo` | `demos/sine_440_tremolo.wav` | [ ] |
| 9 | `sonido process demos/sine_440.wav --effect chorus` | `demos/sine_440_chorus.wav` | [ ] |
| 10 | `sonido process demos/sine_440.wav --effect flanger` | `demos/sine_440_flanger.wav` | [ ] |
| 11 | `sonido process demos/sine_440.wav --effect phaser` | `demos/sine_440_phaser.wav` | [ ] |
| 12 | `sonido process demos/sine_440.wav --effect multivibrato` | `demos/sine_440_multivibrato.wav` | [ ] |
| 13 | `sonido process demos/sine_440.wav --effect delay` | `demos/sine_440_delay.wav` | [ ] |
| 14 | `sonido process demos/sine_440.wav --effect tape` | `demos/sine_440_tape.wav` | [ ] |
| 15 | `sonido process demos/sine_440.wav --effect reverb` | `demos/sine_440_reverb.wav` | [ ] |
| 16 | `sonido process demos/sine_440.wav --effect limiter` | `demos/sine_440_limiter.wav` | [ ] |
| 17 | `sonido process demos/sine_440.wav --effect bitcrusher` | `demos/sine_440_bitcrusher.wav` | [ ] |
| 18 | `sonido process demos/sine_440.wav --effect ringmod` | `demos/sine_440_ringmod.wav` | [ ] |
| 19 | `sonido process demos/sine_440.wav --effect stage` | `demos/sine_440_stage.wav` | [ ] |

### 2.2 Custom Params

Explicit `--param` flags. Validates param parsing and non-default behavior.

| # | Command | Expected Output | Pass |
|---|---------|-----------------|------|
| 1 | `sonido process demos/sine_440.wav --effect preamp --param gain=6` | `demos/sine_440_preamp_gain=6.wav` | [ ] |
| 2 | `sonido process demos/sine_440.wav --effect distortion --param drive=15` | `demos/sine_440_distortion_drive=15.wav` | [ ] |
| 3 | `sonido process demos/noise.wav --effect compressor --param threshold=-20` | `demos/noise_compressor_threshold=-20.wav` | [ ] |
| 4 | `sonido process demos/noise.wav --effect gate --param threshold=-40` | `demos/noise_gate_threshold=-40.wav` | [ ] |
| 5 | `sonido process demos/sine_440.wav --effect eq --param low_gain=6` | `demos/sine_440_eq_low_gain=6.wav` | [ ] |
| 6 | `sonido process demos/noise.wav --effect filter --param cutoff=2000` | `demos/noise_filter_cutoff=2000.wav` | [ ] |
| 7 | `sonido process demos/sine_440.wav --effect wah --param sensitivity=0.8` | `demos/sine_440_wah_sensitivity=0.8.wav` | [ ] |
| 8 | `sonido process demos/sine_440.wav --effect tremolo --param rate=5 --param depth=0.8` | `demos/sine_440_tremolo_rate=5_depth=0.8.wav` | [ ] |
| 9 | `sonido process demos/sine_440.wav --effect chorus --param rate=2 --param depth=0.5` | `demos/sine_440_chorus_rate=2_depth=0.5.wav` | [ ] |
| 10 | `sonido process demos/sine_440.wav --effect flanger --param rate=0.5 --param depth=0.7` | `demos/sine_440_flanger_rate=0.5_depth=0.7.wav` | [ ] |
| 11 | `sonido process demos/sine_440.wav --effect phaser --param rate=0.3 --param depth=0.8` | `demos/sine_440_phaser_rate=0.3_depth=0.8.wav` | [ ] |
| 12 | `sonido process demos/sine_440.wav --effect multivibrato --param depth=0.5` | `demos/sine_440_multivibrato_depth=0.5.wav` | [ ] |
| 13 | `sonido process demos/sine_440.wav --effect delay --param time=300 --param feedback=0.4` | `demos/sine_440_delay_time=300_feedback=0.4.wav` | [ ] |
| 14 | `sonido process demos/sine_440.wav --effect tape --param drive=12` | `demos/sine_440_tape_drive=12.wav` | [ ] |
| 15 | `sonido process demos/sine_440.wav --effect reverb --param mix=0.5 --param room_size=0.8` | `demos/sine_440_reverb_mix=0.5_room_size=0.8.wav` | [ ] |
| 16 | `sonido process demos/sine_440.wav --effect limiter --param threshold=-12 --param ceiling=-0.5` | `demos/sine_440_limiter_threshold=-12_ceiling=-0.5.wav` | [ ] |
| 17 | `sonido process demos/sine_440.wav --effect bitcrusher --param bit_depth=6 --param downsample=4` | `demos/sine_440_bitcrusher_bit_depth=6_downsample=4.wav` | [ ] |
| 18 | `sonido process demos/sine_440.wav --effect ringmod --param frequency=220 --param depth=1` | `demos/sine_440_ringmod_frequency=220_depth=1.wav` | [ ] |
| 19 | `sonido process demos/sine_440.wav --effect stage --param width=160 --param haas_delay=12` | `demos/sine_440_stage_width=160_haas_delay=12.wav` | [ ] |

### 2.3 New Exposed Params

These params were added in the CLI UX overhaul. Each must be accepted without error.

| # | Command | What It Tests | Pass |
|---|---------|---------------|------|
| 1 | `sonido process demos/noise.wav --effect compressor --param knee=0` | Hard knee (0 dB) | [ ] |
| 2 | `sonido process demos/sine_440.wav --effect delay --param ping_pong=1` | Ping-pong stereo mode | [ ] |
| 3 | `sonido process demos/sine_440.wav --effect multivibrato --param mix=0.5` | Vibrato wet/dry mix | [ ] |
| 4 | `sonido process demos/sine_440.wav --effect tape --param output=-6 --param hf_rolloff=4000 --param bias=0.1` | Tape output, HF rolloff, bias | [ ] |
| 5 | `sonido process demos/sine_440.wav --effect preamp --param output=-3 --param headroom=10` | Preamp output level, headroom | [ ] |
| 6 | `sonido process demos/sine_440.wav --effect reverb --param stereo_width=0 --param type=hall` | Stereo width, reverb type | [ ] |
| 7 | `sonido process demos/sine_440.wav --effect chorus --param output=-3` | Universal output param | [ ] |
| 8 | `sonido process demos/sine_440.wav --effect distortion --param waveshape=foldback` | Foldback waveshape mode | [ ] |
| 9 | `sonido process demos/sine_440.wav --effect distortion --param waveshape=asymmetric` | Asymmetric waveshape mode | [ ] |
| 10 | `sonido process demos/sine_440.wav --effect distortion --param foldback_threshold=0.5` | Foldback threshold | [ ] |
| 11 | `sonido process demos/sine_440.wav --effect phaser --param stereo_spread=80` | Phaser stereo spread | [ ] |
| 12 | `sonido process demos/sine_440.wav --effect tremolo --param waveform=square` | Tremolo square waveform | [ ] |
| 13 | `sonido process demos/sine_440.wav --effect tremolo --param waveform=triangle` | Tremolo triangle waveform | [ ] |
| 14 | `sonido process demos/sine_440.wav --effect wah --param mode=manual --param frequency=800` | Wah manual mode | [ ] |
| 15 | `sonido process demos/noise.wav --effect filter --param resonance=90` | Filter high-Q resonance | [ ] |
| 16 | `sonido process demos/sine_440.wav --effect eq --param low_gain=6 --param mid_gain=-3 --param high_gain=3` | EQ multi-band | [ ] |

### 2.4 Stereo Output Verification

All outputs from 2.1 should be 2-channel (always-stereo default). Use `sonido info` or `soxi`.

```bash
for f in demos/sine_440_*.wav demos/noise_*.wav; do
  sonido info "$f" 2>&1 | grep -i channel
done
```

- [ ] Every file shows 2 channels

Verify `--mono` forces 1-channel:

```bash
sonido process demos/sine_440.wav /tmp/mono_test.wav --effect reverb --mono
sonido info /tmp/mono_test.wav
```

- [ ] Shows 1 channel

### 2.5 Auto-Naming Verification

```bash
# Single effect → {stem}_{effect}.wav
ls demos/sine_440_distortion.wav

# Effect + params → {stem}_{effect}_{k=v}.wav
sonido process demos/sine_440.wav --effect distortion --param drive=15
ls demos/sine_440_distortion_drive=15.wav

# Chain → {stem}_{e1}+{e2}.wav
sonido process demos/sine_440.wav --chain "preamp|distortion"
ls demos/sine_440_preamp+distortion.wav

# Chain + params → {stem}_{e1_k=v}+{e2_k=v}.wav
sonido process demos/sine_440.wav --chain "preamp:gain=6|distortion:drive=12"
ls demos/sine_440_preamp_gain=6+distortion_drive=12.wav

# Preset → {stem}_{preset}.wav
sonido process demos/sine_440.wav --preset crunch
ls demos/sine_440_crunch.wav

# Explicit output path still works
sonido process demos/sine_440.wav /tmp/explicit.wav --effect reverb
ls /tmp/explicit.wav
```

- [ ] All 6 `ls` commands succeed (file exists)

### 2.6 Effect Aliases

Verify aliases resolve to the same effect:

```bash
sonido process demos/noise.wav --effect lowpass --param cutoff=2000
sonido process demos/sine_440.wav --effect vibrato --param depth=0.5
sonido process demos/noise.wav --effect noisegate --param threshold=-40
sonido process demos/sine_440.wav --effect autowah --param sensitivity=0.8
sonido process demos/sine_440.wav --effect parametriceq --param low_gain=6
sonido process demos/sine_440.wav --effect crusher --param bit_depth=8
sonido process demos/sine_440.wav --effect ring --param frequency=300
```

- [ ] All 7 aliases produce output without error

### 2.7 Effect Chains

```bash
# Simple 2-effect chain
sonido process demos/sine_440.wav \
    --chain "preamp:gain=6|distortion:drive=12"

# Complex 5-effect chain
sonido process demos/sine_440.wav \
    --chain "preamp:gain=6|distortion:drive=12|compressor:threshold=-20|delay:time=300|reverb:mix=0.3"

# Chain with whitespace (should be trimmed)
sonido process demos/sine_440.wav \
    --chain "distortion | delay:time=400 | reverb"
```

- [ ] 2-effect chain: output shows "2 effect(s)"
- [ ] 5-effect chain: output shows "5 effect(s)"
- [ ] Whitespace chain: processes without error

### 2.8 Process Options

```bash
# Force mono output (default is always stereo)
sonido process demos/sine_440.wav /tmp/out.wav --effect reverb --mono

# Custom block size
sonido process demos/sine_440.wav /tmp/out.wav --effect reverb --block-size 128

# Custom bit depth (16-bit)
sonido process demos/sine_440.wav /tmp/out.wav --effect reverb --bit-depth 16

# 24-bit output
sonido process demos/sine_440.wav /tmp/out_24.wav --effect reverb --bit-depth 24
```

- [ ] Mono output produces 1-channel WAV (--mono forces mono; without it, output is always 2-channel)
- [ ] Block size 128 processes without error
- [ ] 16-bit output produces valid WAV
- [ ] 24-bit output produces valid WAV (`sonido info /tmp/out_24.wav` shows 24-bit)

---

## Phase 3: CLI — Presets

### 3.1 List Presets

```bash
sonido presets list
```

- [ ] Shows factory presets and user presets (if any)

### 3.2 Show Preset Detail

```bash
sonido presets show crunch
```

- [ ] Displays preset name, effects, and parameter values

### 3.3 Process with Preset

```bash
sonido process demos/sine_440.wav --preset crunch
```

- [ ] Processes with preset-defined effect chain

### 3.4 Preset Paths

```bash
sonido presets paths
```

- [ ] Shows factory and user preset directories

### 3.5 Save and Delete User Preset

```bash
sonido presets save test_preset --chain "distortion:drive=10|delay:time=200"
sonido presets show test_preset
sonido presets delete test_preset --force
```

- [ ] Save succeeds
- [ ] Show displays the saved chain
- [ ] Delete succeeds

### 3.6 Copy Preset

```bash
sonido presets save my_preset --chain "chorus:rate=1.5|delay:time=300"
sonido presets copy my_preset --name my_preset_backup
sonido presets show my_preset_backup
sonido presets delete my_preset --force
sonido presets delete my_preset_backup --force
```

- [ ] Copy succeeds
- [ ] Backup preset shows identical chain
- [ ] Cleanup deletes both

### 3.7 Export Factory Presets

```bash
sonido presets list --factory
```

- [ ] Lists only factory presets (not user presets)

---

## Phase 4: CLI — Signal Generation

### 4.1 Basic Generators

```bash
sonido generate tone /tmp/tone.wav --freq 1000 --duration 1.0
sonido generate sweep /tmp/sweep.wav --duration 2.0
sonido generate noise /tmp/noise.wav --duration 1.0 --amplitude 0.5
sonido generate impulse /tmp/impulse.wav
sonido generate silence /tmp/silence.wav --duration 1.0
```

- [ ] tone: 1kHz sine, 48000 samples
- [ ] sweep: chirp signal
- [ ] noise: white noise at -6dB
- [ ] impulse: single sample
- [ ] silence: all zeros

### 4.2 PolyBLEP Oscillator Waveforms

```bash
sonido generate osc /tmp/osc_sine.wav --freq 440 --waveform sine --duration 1.0
sonido generate osc /tmp/osc_tri.wav --freq 440 --waveform triangle --duration 1.0
sonido generate osc /tmp/osc_saw.wav --freq 220 --waveform saw --duration 1.0
sonido generate osc /tmp/osc_square.wav --freq 440 --waveform square --duration 1.0
sonido generate osc /tmp/osc_noise.wav --waveform noise --duration 1.0
```

- [ ] All 5 waveforms generate without error
- [ ] Saw and square should sound band-limited (no harsh aliasing at 220/440Hz)

### 4.3 Chord Generation (Polyphonic Synth)

```bash
# C major triad
sonido generate chord /tmp/cmajor.wav --notes "60,64,67" --duration 2.0

# A minor 7th with saw wave
sonido generate chord /tmp/am7.wav --notes "57,60,64,67" --waveform saw --duration 2.0

# With filter and custom envelope
sonido generate chord /tmp/pad.wav --notes "60,64,67,72" --attack 200 --release 2000 --duration 4.0
```

- [ ] C major: 3 voices at 261.6, 329.6, 392.0 Hz
- [ ] Am7: 4 voices with saw timbre
- [ ] Pad: slow attack audible, long release tail

### 4.4 ADSR Envelope Tests

```bash
# Standard envelope
sonido generate adsr /tmp/adsr.wav --attack 50 --decay 100 --sustain 0.7 --release 200

# Pluck (fast attack/decay, zero sustain)
sonido generate adsr /tmp/pluck.wav --attack 1 --decay 50 --sustain 0.0 --release 100

# Pad (slow attack)
sonido generate adsr /tmp/pad_env.wav --attack 500 --decay 200 --sustain 0.8 --release 2000 --gate-duration 3.0
```

- [ ] Standard: audible attack-decay-sustain-release shape
- [ ] Pluck: percussive, short
- [ ] Pad: gradual fade-in

### 4.5 Pulse Width

```bash
sonido generate osc /tmp/pulse.wav --waveform square --pulse-width 0.25 --freq 440 --duration 1.0
```

- [ ] Output is a pulse wave with 25% duty cycle (narrower than square)
- [ ] Timbre is distinctly different from 50% square wave

### 4.6 Chord with Filter

```bash
sonido generate chord /tmp/chord_raw.wav --notes "60,64,67" --waveform saw --duration 2.0
sonido process /tmp/chord_raw.wav /tmp/chord_filtered.wav --effect filter --param cutoff=2000
```

- [ ] Raw chord has bright saw harmonics
- [ ] Filtered chord sounds darker (high frequencies attenuated)

---

## Phase 5: CLI — Analysis

### 5.1 Spectrum Analysis

```bash
sonido analyze spectrum demos/sine_440.wav --peaks 5
```

- [ ] Shows peak near 440Hz as highest amplitude
- [ ] Lists 5 frequency peaks with dB levels

### 5.1b Spectrum Welch Method

```bash
sonido analyze spectrum demos/sine_440.wav --method welch --peaks 5
```

- [ ] Welch averaging produces smoother spectrum estimate
- [ ] Peak still near 440Hz

### 5.2 Distortion Analysis (THD)

```bash
sonido analyze distortion demos/sine_440.wav
```

- [ ] Reports THD (should be very low for pure sine)

### 5.2b Distortion with Extended Harmonics

```bash
sonido analyze distortion demos/sine_440.wav --harmonics 10
```

- [ ] Reports THD using up to 10 harmonics
- [ ] Individual harmonic levels listed

### 5.3 Dynamics Analysis

```bash
sonido analyze dynamics demos/noise.wav
```

- [ ] Reports RMS level, peak level, crest factor, dynamic range

### 5.4 Impulse Response Extraction

```bash
sonido generate sweep /tmp/sweep_dry.wav --duration 3.0
sonido process /tmp/sweep_dry.wav /tmp/sweep_wet.wav --effect reverb --param mix=1
sonido analyze ir /tmp/sweep_dry.wav /tmp/sweep_wet.wav -o /tmp/ir.wav --rt60
```

- [ ] Sweep generated
- [ ] Sweep processed through reverb
- [ ] IR extracted, RT60 reported

### 5.5 CQT Analysis

```bash
sonido analyze cqt demos/sine_440.wav --peaks 5
sonido analyze cqt demos/sine_440.wav --chromagram
sonido analyze cqt demos/sine_440.wav -o /tmp/cqt.csv
```

- [ ] Peaks include A4 (440Hz)
- [ ] Chromagram shows energy at note A
- [ ] CQT CSV output generated

### 5.6 Bandpass and Hilbert

```bash
sonido analyze bandpass demos/noise.wav --low 100 --high 500 -o /tmp/bandpassed.wav
sonido analyze hilbert demos/sine_440.wav --amp-output /tmp/envelope.wav
sonido analyze hilbert demos/sine_440.wav --envelope
sonido analyze hilbert demos/sine_440.wav --phase
```

- [ ] Bandpassed output contains only 100-500Hz content
- [ ] Hilbert envelope output exists
- [ ] `--envelope` works as alias for `--amp-output` (prints amplitude envelope data)
- [ ] `--phase` works as alias for `--phase-output` (prints instantaneous phase data)

### 5.7 PAC and Comodulogram

```bash
sonido generate tone /tmp/carrier.wav --freq 50 --duration 5.0
sonido analyze pac /tmp/carrier.wav --phase-low 4 --phase-high 8 --amp-low 30 --amp-high 100
sonido analyze comodulogram demos/noise.wav \
    --phase-range 2-20 --amp-range 20-200 --phase-step 4 --amp-step 20 -o /tmp/comod.csv
```

- [ ] PAC analysis completes with modulation index
- [ ] Comodulogram CSV file generated

### 5.8 A/B Comparison

```bash
sonido compare demos/sine_440.wav demos/sine_440_distortion.wav --detailed
sonido analyze compare demos/sine_440.wav demos/sine_440_reverb.wav
```

- [ ] Reports spectral differences between dry and wet signals
- [ ] `analyze compare` works as alias for `compare` and produces comparison metrics

### 5.9 Spectrogram

```bash
sonido analyze spectrogram demos/sine_440.wav -o /tmp/spec.csv
```

- [ ] Spectrogram CSV generated at `/tmp/spec.csv`
- [ ] 440Hz bin shows consistent energy across time frames

### 5.10 Transfer Function Extras

```bash
sonido generate sweep /tmp/tf_dry.wav --duration 3.0
sonido process /tmp/tf_dry.wav /tmp/tf_wet.wav --effect filter --param cutoff=1000
sonido analyze ir /tmp/tf_dry.wav /tmp/tf_wet.wav -o /tmp/tf.wav --format csv
sonido analyze ir /tmp/tf_dry.wav /tmp/tf_wet.wav -o /tmp/tf2.wav --format csv --window-size 4096
```

- [ ] `--format csv` produces CSV output
- [ ] `--window-size 4096` produces valid results with different FFT resolution

### 5.11 IMD Analysis

```bash
sonido analyze imd demos/sine_440.wav
```

- [ ] Auto-detects test frequencies when `--freq1` and `--freq2` are omitted
- [ ] Intermodulation distortion analysis completes
- [ ] Reports IMD percentage

---

## Phase 6: CLI — Real-time Processing

Requires audio devices. Press Ctrl+C to stop each test.

### 6.1 Single Effect

```bash
sonido realtime --effect reverb --param mix=0.3
```

- [ ] Audio passes through with reverb applied
- [ ] Ctrl+C exits cleanly

### 6.2 Effect Chain

```bash
sonido realtime --chain "preamp:gain=6|distortion:drive=10|delay:time=250"
```

- [ ] 3-effect chain processes live audio

### 6.3 Device Selection

```bash
sonido realtime --input 0 --output 0 --effect chorus
```

- [ ] Runs on specified devices

### 6.4 Buffer Size

```bash
sonido realtime --effect delay --buffer-size 128
sonido realtime --effect delay --buffer-size 1024
```

- [ ] 128: lower latency, no glitches
- [ ] 1024: higher latency, stable

### 6.5 Mono Mode

```bash
sonido realtime --effect reverb --mono
```

- [ ] Processes in mono without error

### 6.6 Realtime with Preset

```bash
sonido realtime --preset my_chorus
```

- [ ] Loads preset and processes live audio with preset chain
- [ ] Ctrl+C exits cleanly

### 6.7 Custom Sample Rate

```bash
sonido realtime --sample-rate 96000 --effect reverb
```

- [ ] Runs at 96kHz (if hardware supports it)
- [ ] Falls back gracefully if unsupported

---

## Phase 7: GUI

### 7.1 Launch

```bash
cargo run -p sonido-gui
```

- [ ] Window opens without crash
- [ ] No errors in terminal output

### 7.2 Initial State

- [ ] Audio status indicator visible (green = OK, red = error)
- [ ] Input/output meters present
- [ ] All 19 effect buttons visible in chain strip

### 7.3 Effect Panel Walkthrough

Click each effect button and test its panel:

| Effect | Selects | Panel Renders | Knobs Respond | Bypass Toggles | Pass |
|--------|---------|--------------|---------------|----------------|------|
| Preamp | [ ] | [ ] | [ ] | [ ] | [ ] |
| Distortion | [ ] | [ ] | [ ] | [ ] | [ ] |
| Compressor | [ ] | [ ] | [ ] | [ ] | [ ] |
| Gate | [ ] | [ ] | [ ] | [ ] | [ ] |
| EQ | [ ] | [ ] | [ ] | [ ] | [ ] |
| Filter | [ ] | [ ] | [ ] | [ ] | [ ] |
| Wah | [ ] | [ ] | [ ] | [ ] | [ ] |
| Tremolo | [ ] | [ ] | [ ] | [ ] | [ ] |
| Chorus | [ ] | [ ] | [ ] | [ ] | [ ] |
| Flanger | [ ] | [ ] | [ ] | [ ] | [ ] |
| Phaser | [ ] | [ ] | [ ] | [ ] | [ ] |
| MultiVibrato | [ ] | [ ] | [ ] | [ ] | [ ] |
| Delay | [ ] | [ ] | [ ] | [ ] | [ ] |
| Tape | [ ] | [ ] | [ ] | [ ] | [ ] |
| Reverb | [ ] | [ ] | [ ] | [ ] | [ ] |

### 7.4 Knob Interaction

For any effect panel:

- [ ] Drag knob: value changes smoothly
- [ ] Double-click knob: resets to default value
- [ ] Value display updates in real-time

### 7.5 Bypass

- [ ] Double-click effect button: toggles bypass
- [ ] Bypass LED changes color (green = active, dim = bypassed)
- [ ] Audio is dry when bypassed

### 7.6 Effect-Specific Panel Verification

#### Compressor
- [ ] Gain reduction meter displays (should now work — was a TODO, now fixed)
- [ ] Threshold, ratio, attack, release, makeup knobs present

#### Phaser
- [ ] Stages selector works (2/4/6/8/10/12)

#### Tremolo
- [ ] Wave selector works (Sine/Triangle/Square/S&H)

#### Wah
- [ ] Mode toggle works (Auto/Manual)

#### EQ
- [ ] Three bands (Low/Mid/High) each with Frequency, Gain, Q

### 7.7 Metering

- [ ] Input meter responds to audio input
- [ ] Output meter shows processed signal level
- [ ] Meter ballistics look smooth (not jumpy)

### 7.8 Preset Management

- [ ] Open preset selector
- [ ] Select a factory preset — parameters update
- [ ] Modify a parameter — asterisk (*) appears for unsaved changes
- [ ] Save preset — enter name, confirm save
- [ ] Saved preset appears in list

### 7.9 Performance

- [ ] Status bar shows sample rate, buffer size, latency
- [ ] No audio glitches during parameter changes
- [ ] CPU usage reasonable (check status bar)

Test lower buffer:
```bash
cargo run -p sonido-gui -- --buffer-size 128
```

- [ ] Launches and runs at lower latency

---

## Phase 8: Regression Tests

### 8.1 Golden File Tests

```bash
cargo test -p sonido-effects --test regression
```

- [ ] All 16 regression tests pass

### 8.2 Full Test Suite

```bash
cargo test --workspace
```

- [ ] All tests pass, zero failures

### 8.3 no_std Compatibility

```bash
cargo test --no-default-features -p sonido-core
cargo test --no-default-features -p sonido-effects
cargo test --no-default-features -p sonido-synth
cargo test --no-default-features -p sonido-registry
cargo test --no-default-features -p sonido-platform
```

- [ ] All 5 no_std crates pass

### 8.4 Clippy and Formatting

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

- [ ] Zero clippy warnings
- [ ] Formatting clean

### 8.5 Documentation Build

```bash
cargo doc --workspace --no-deps
cargo test --workspace --doc
```

- [ ] Zero doc warnings
- [ ] All doc tests pass

---

## Quick Smoke Test

Minimal pass/fail covering default params, auto-naming, stereo output, and core features:

```bash
# Default params + auto-naming
sonido process demos/sine_440.wav --effect reverb && echo "PASS: default-param process"
ls demos/sine_440_reverb.wav && echo "PASS: auto-naming"
sonido info demos/sine_440_reverb.wav | grep -qi "2 ch\|channels.*2\|stereo" && echo "PASS: stereo output"

# Chain auto-naming
sonido process demos/sine_440.wav --chain "distortion|delay:time=300" && echo "PASS: chain"
ls demos/sine_440_distortion+delay_time=300.wav && echo "PASS: chain auto-naming"

# Preset auto-naming
sonido process demos/sine_440.wav --preset crunch && echo "PASS: preset"
ls demos/sine_440_crunch.wav && echo "PASS: preset auto-naming"

# Core features
sonido generate osc /tmp/smoke_osc.wav --waveform saw --freq 220 && echo "PASS: oscillator"
sonido generate chord /tmp/smoke_chord.wav --notes "60,64,67" && echo "PASS: chord"
sonido analyze spectrum demos/sine_440.wav --peaks 3 && echo "PASS: analysis"
sonido effects && echo "PASS: effects list"
sonido info demos/sine_440.wav && echo "PASS: file info"
cargo test -p sonido-effects --test regression && echo "PASS: regression"
cargo test --workspace && echo "PASS: all tests"
```

- [ ] All 13 lines print PASS

---

## Cleanup

Remove generated test outputs:

```bash
rm -f demos/sine_440_*.wav demos/noise_*.wav /tmp/mono_test.wav /tmp/explicit.wav /tmp/out.wav /tmp/out_24.wav
```

---

## Troubleshooting

### Audio Device Not Found
```bash
sonido devices list
```
Try specifying device by index: `--input 0 --output 0`

### High Latency / Glitches
Increase buffer size: `--buffer-size 512` or `--buffer-size 1024`. Close other audio applications.

### GUI Won't Launch
```bash
RUST_LOG=debug cargo run -p sonido-gui
```
Check for missing display server, GPU driver issues, or Wayland/X11 compatibility.

### Regression Test Fails
If golden files are missing: `REGENERATE_GOLDEN=1 cargo test -p sonido-effects --test regression`
If audio processing changed intentionally, regenerate golden files and verify the diff is expected.

---

## See Also

- [CLI Guide](CLI_GUIDE.md) — Full command reference
- [Effects Reference](EFFECTS_REFERENCE.md) — All 19 effects with parameters and DSP theory
- [GUI Documentation](GUI.md) — GUI user guide
- [Library Testing Guide](LIBRARY_TESTING.md) — Testing guide for library consumers
- [DSP Fundamentals](DSP_FUNDAMENTALS.md) — Theory behind the implementations
