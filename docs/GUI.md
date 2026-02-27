# Sonido GUI

Sonido GUI is a professional real-time DSP effect processor built on the Sonido framework using [egui](https://github.com/emilk/egui).

## Installation

### Native (from source)

```bash
# Build and run
cargo run -p sonido-gui

# Or install globally
cargo install --path crates/sonido-gui
sonido-gui
```

### Web (wasm)

Requires [Trunk](https://trunkrs.dev/) (`cargo install trunk`).

```bash
# Compile check (fast gate for cfg issues, no runtime)
cargo check --target wasm32-unknown-unknown -p sonido-gui

# Dev server with hot reload
cd crates/sonido-gui
trunk serve
# Opens at http://127.0.0.1:8080
```

On first load, click anywhere to resume the browser's `AudioContext` (autoplay policy).
Hard-refresh with Ctrl+Shift+R after rebuilds to bypass cache.

### Full Verification Flow

From a clean state (after `cargo clean`):

```bash
# 1. Native build + tests
cargo build --workspace
cargo test --workspace

# 2. Wasm compile check
cargo check --target wasm32-unknown-unknown -p sonido-gui

# 3. Wasm dev server (from crates/sonido-gui/)
cd crates/sonido-gui
trunk serve

# 4. Browser: http://127.0.0.1:8080 (Ctrl+Shift+R to hard-refresh)
#    - Three columns visible: INPUT | EFFECT CHAIN | OUTPUT
#    - Click effect in chain → parameter panel renders below
#    - Resize window → center column flexes, I/O columns stay fixed
#    - Click anywhere to resume audio (browser autoplay policy)
```

Step 2 catches missing `#[cfg]` gates and std-only API usage that native compilation misses.
Step 4 is the actual runtime test — there is no headless wasm test runner.

## Command-Line Options

```
sonido-gui [OPTIONS]

OPTIONS:
    --input <NAME>         Input audio device name (uses default if not specified)
    --output <NAME>        Output audio device name (uses default if not specified)
    --sample-rate <N>      Sample rate in Hz (default: 48000)
    --buffer-size <N>      Buffer size in samples (default: 512)
    --effect <NAME>        Launch in single-effect mode (e.g., distortion, reverb)
    -h, --help             Print help information
    -V, --version          Print version information
```

### Examples

```bash
# Use default devices
sonido-gui

# Lower latency (smaller buffer)
sonido-gui --buffer-size 128

# Specify devices
sonido-gui --input "USB Audio" --output "Built-in Output"

# Higher sample rate
sonido-gui --sample-rate 96000

# Single-effect mode (simplified UI with one effect, no chain strip)
sonido-gui --effect distortion
sonido-gui --effect reverb
```

## User Interface

### Window Layout

```
+------------------------------------------------------------------+
| SONIDO         [Preset Selector v] [Save]        [Audio Status]  |
+------------------------------------------------------------------+
|                                                                  |
| INPUT  |  EFFECT CHAIN                              |  OUTPUT    |
| [Meter]|  [PRE]->[DIST]->[COMP]->[CHO]->[DLY]->... |  [Meter]   |
| [Gain] |  [FLT]->[VIB]->[TAPE]->[REV]              |  [Master]  |
|        |                                            |            |
|        +--------------------------------------------+            |
|        | SELECTED EFFECT PANEL                      |            |
|        | (Parameters for currently selected effect) |            |
|        +--------------------------------------------+            |
+------------------------------------------------------------------+
| 48000 Hz | [Buffer ▼] | 10.7 ms | CPU: 2.3% ▁▂▃▄▅▆▇▆▅▄▃▁        |
+------------------------------------------------------------------+
```

### Header Bar

- **SONIDO**: Application title
- **Preset Selector**: Drop-down to choose presets (asterisk * indicates unsaved changes)
- **Save**: Save current settings as a preset
- **Audio Status**: Green dot = audio running, red dot = audio error

### Input/Output Sections

- **Input Meter**: Real-time peak and RMS level display
- **Input Gain**: -20 to +20 dB gain control
- **Output Meter**: Real-time peak and RMS level display
- **Master Volume**: -40 to +6 dB master output control

### Effect Chain Strip

The chain strip shows all 15 effects in processing order:

| Effect | Short Name | Description |
|--------|------------|-------------|
| Preamp | PRE | Clean gain stage |
| Distortion | DIST | Waveshaping distortion |
| Compressor | COMP | Dynamics compressor |
| Gate | GATE | Noise gate |
| Parametric EQ | EQ | 3-band parametric equalizer |
| Filter | FLT | Resonant lowpass filter |
| Wah | WAH | Auto-wah / manual wah |
| Tremolo | TREM | Amplitude modulation |
| Chorus | CHO | Modulated delay chorus |
| Flanger | FLG | Classic flanger effect |
| Phaser | PHAS | Multi-stage phaser |
| MultiVibrato | VIB | 6-unit tape wow/flutter |
| Delay | DLY | Tape-style feedback delay |
| Tape | TAPE | Tape saturation |
| Reverb | REV | Freeverb-style reverb |

The chain strip is wrapped in a horizontal `ScrollArea` that activates when the chain overflows the available width. When the chain fits, it is centered.

**Interactions:**
- **Click**: Select effect to show its parameter panel below
- **Double-click**: Toggle effect bypass (LED indicator: green = active, dim = bypassed)
- **Drag**: Reorder effects in the chain (drag left/right)
- **Right-click**: Context menu with "Remove Effect" and "Bypass"/"Enable" toggle
- **Hover**: Tooltip showing effect short name (from registry descriptors)
- **"+" button**: Opens a popup menu listing all registered effects for dynamic addition

### Effect Panels

When an effect is selected, its parameter panel appears below the chain. Each panel includes:

- Effect name header
- Rotary knobs for continuous parameters
- Toggle buttons for discrete options
- Value readouts below knobs

**Common Controls:**
- **Knob drag**: Vertical drag to adjust value
- **Knob double-click**: Reset to default value

### Status Bar

- **Sample Rate**: Current sample rate (e.g., 48000 Hz)
- **Buffer Size**: Drop-down selector for audio buffer size (click to change)
  - Presets: Low Latency (256), Very Low (512), Balanced (1024), Stable (2048), Maximum (4096)
  - Changes require audio restart and are validated against hardware limits
- **Latency**: Round-trip latency in milliseconds (buffer size / sample rate)
- **CPU**: Audio thread CPU usage percentage with real-time sparkline graph
  - Graph shows last 60 frames of CPU usage trend
  - Color-coded: green (<80%), yellow (80-100%), red (>100%)
- **Buffer Overrun Warning**: Appears when buffer overruns are detected
  - Yellow: warning status (processing time approaching buffer limit)
  - Red: critical status (audio glitches possible)
  - Shows overrun count to indicate severity

## Effects Reference

### Preamp
- **Gain**: -20 to +20 dB
- **Output**: -20 to +20 dB output level
- **Headroom**: 6 to 40 dB clipping ceiling

### Distortion
- **Drive**: 0 to 40 dB (default 12)
- **Tone**: 500 to 10000 Hz lowpass filter (default 4000)
- **Level**: -20 to 0 dB output level (default -6)
- **Waveshape**: SoftClip, HardClip, Foldback, Asymmetric

### Compressor
- **Threshold**: -60 to 0 dB (default -18)
- **Ratio**: 1:1 to 20:1
- **Attack**: 0.1 to 100 ms
- **Release**: 10 to 1000 ms
- **Makeup**: 0 to 24 dB
- **Knee**: 0 to 12 dB knee width

### Chorus
- **Rate**: 0.1 to 10 Hz LFO rate
- **Depth**: 0 to 1 modulation depth
- **Mix**: 0 to 1 wet/dry mix

### Delay
- **Time**: 1 to 2000 ms delay time (default 300)
- **Feedback**: 0 to 0.95 feedback amount (default 0.4)
- **Mix**: 0 to 1 wet/dry mix
- **Ping-Pong**: On/Off stereo ping-pong mode

### Filter (Lowpass)
- **Cutoff**: 20 to 20000 Hz
- **Resonance**: 0.1 to 10 Q factor

### MultiVibrato
- **Depth**: 0 to 1 overall modulation depth
- **Mix**: 0 to 100% wet/dry mix

### Tape Saturation
- **Drive**: 0 to 24 dB (default 6)
- **Saturation**: 0 to 1
- **Output**: -12 to +12 dB output level
- **HF Rolloff**: 1000 to 20000 Hz high-frequency rolloff
- **Bias**: -0.2 to 0.2 tape bias offset

### Reverb
- **Room Size**: 0 to 1 (default 0.5)
- **Decay**: 0 to 1
- **Damping**: 0 to 1 (0 = bright, 1 = dark)
- **Pre-delay**: 0 to 100 ms
- **Mix**: 0 to 1 wet/dry mix
- **Stereo Width**: 0 to 100% stereo decorrelation
- **Type**: Room, Hall

### Tremolo
- **Rate**: 0.5 to 20 Hz LFO rate
- **Depth**: 0 to 100% modulation depth
- **Wave**: Sine, Triangle, Square, Sample & Hold

### Flanger
- **Rate**: 0.05 to 5 Hz LFO rate
- **Depth**: 0 to 100% modulation depth
- **Feedback**: 0 to 95% feedback amount
- **Mix**: 0 to 100% wet/dry mix

### Phaser
- **Rate**: 0.05 to 5 Hz LFO rate
- **Depth**: 0 to 100% modulation depth
- **Feedback**: 0 to 95% feedback/resonance
- **Mix**: 0 to 100% wet/dry mix
- **Stages**: 2, 4, 6, 8, 10, or 12 allpass stages

### Gate
- **Threshold**: -80 to 0 dB open threshold
- **Attack**: 0.1 to 50 ms attack time
- **Release**: 10 to 1000 ms release time
- **Hold**: 0 to 500 ms hold time

### Wah
- **Frequency**: 200 to 2000 Hz center frequency
- **Resonance**: 1 to 10 filter Q
- **Sensitivity**: 0 to 100% (auto-wah mode)
- **Mode**: Auto (envelope follower), Manual

### Parametric EQ
Three independent bands with identical controls:

**Low Band**
- **Frequency**: 20 to 500 Hz
- **Gain**: -12 to +12 dB
- **Q**: 0.5 to 5.0

**Mid Band**
- **Frequency**: 200 to 5000 Hz
- **Gain**: -12 to +12 dB
- **Q**: 0.5 to 5.0

**High Band**
- **Frequency**: 1000 to 15000 Hz
- **Gain**: -12 to +12 dB
- **Q**: 0.5 to 5.0

## Preset Management

### Factory Presets

Built-in presets are available immediately:

- **Init**: Clean signal, all effects bypassed
- **Crunch**: Moderate distortion
- **High Gain**: Heavy distortion with compression
- **Ambient**: Delay, reverb, and chorus combination
- **Tape Warmth**: Subtle tape saturation

### Saving Presets

1. Adjust effect parameters to your liking
2. Click the **Save** button in the header
3. Enter a preset name and category
4. Click **Save** to store the preset

### Preset Storage Location

User presets are saved as JSON files in:

| Platform | Directory |
|----------|-----------|
| Linux | `~/.config/sonido/presets/` |
| macOS | `~/Library/Application Support/sonido/presets/` |
| Windows | `%APPDATA%\sonido\presets\` |

### Preset File Format

Presets are stored as JSON files containing all effect parameters, bypass states, and effect order:

```json
{
  "name": "My Preset",
  "category": "User",
  "input_gain": 0.0,
  "master_volume": -3.0,
  "distortion_bypass": false,
  "dist_drive": 18.0,
  ...
}
```

## Real-Time Parameter Changes

### AtomicParamBridge with Dirty Flags

The `AtomicParamBridge` provides per-slot parameter sharing between GUI and audio threads. Each slot has a `dirty` flag (`AtomicBool`) that is set when the GUI writes any parameter. The audio thread checks this flag once per buffer and only pushes changed params to the effect chain when dirty, avoiding unnecessary iteration over all parameters on every buffer.

Individual parameter values use `AtomicU32` bit-cast (`f32::to_bits` / `f32::from_bits`), which is lock-free and wait-free. The slot collection uses `parking_lot::RwLock<Vec<SlotState>>`, which is acceptable because slot mutations (add/remove) are rare compared to per-buffer param reads.

### Bypass Crossfade

Bypass toggling uses `SmoothedParam::fast` (5ms exponential smoothing) per slot for click-free transitions. The crossfade level (`bypass_fade`) ranges from 0.0 (fully bypassed) to 1.0 (fully active). During the transition, the audio thread mixes dry and wet signals proportionally:

- `fade < 1e-6`: Fully bypassed, skip processing entirely (optimization)
- `fade > 1.0 - 1e-6`: Fully active, no crossfade math needed
- Otherwise: `output = dry * (1 - fade) + wet * fade`

When un-bypassing, all effect parameters are re-set to their current values so internal `SmoothedParam`s settle before audio reaches the effect.

### Effect Order Caching

The `EffectOrder` (shared between GUI and audio thread via `parking_lot::RwLock<Vec<usize>>`) has a dirty flag. The audio thread only reads and caches the order when the flag is set, using `clone_from` for heap reuse (avoids reallocation when the Vec capacity is sufficient). This prevents the audio thread from acquiring the RwLock on every buffer.

## Audio Thread Architecture

```
[Input Device] -> [Input Gain] -> [Effect Chain] -> [Master Volume] -> [Output Device]
                                        |
                                        v
                              [Metering Data] -> [GUI Thread]
```

### AudioProcessor

The audio output callback delegates to `AudioProcessor`, a testable struct extracted from the monolithic closure. It encapsulates:

- `process_commands()` — drains transport + chain commands
- `process_buffer(data)` — parameter sync, effect processing, gain staging, metering

The `AudioProcessor` owns the `GraphEngine`, `AtomicParamBridge`, effect order cache, file playback state, and metering sender. This extraction enables unit testing of the audio path without a live audio stream.

### Dynamic Effect Chain

Effects are added and removed at runtime via `ChainCommand` messages sent through a `crossbeam_channel`. The audio thread drains commands and applies them to the `GraphEngine`:

- `ChainCommand::Add { id, effect, descriptors }` — adds the effect to the graph, registers parameter descriptors in the bridge
- `ChainCommand::Remove { slot }` — removes the effect from the graph, deregisters from the bridge

The `GraphEngine` handles topology mutations (add/remove/reorder) with automatic recompilation. The `AtomicParamBridge` maintains slot-level parameter storage in lock-free atomics.

### Widget and Effect UI Consolidation

All shared GUI components live in `sonido-gui-core`:
- **Widgets**: `Knob`, `LevelMeter`, `BypassToggle`, `FootswitchToggle` — re-exported by `sonido-gui::widgets`
- **Effect UIs**: 15 effect parameter panels + `EffectPanel` dispatcher — used by both standalone and future plugin UIs
- **Theme**: Dark theme shared across all targets
- **ParamBridge**: Trait with `begin_set`/`end_set` gesture protocol for CLAP/VST3 host integration

This architecture means a future `sonido-plugin` crate only depends on `sonido-gui-core`, not the full standalone application.

## Troubleshooting

### No Audio

1. Check the audio status indicator (should be green)
2. Verify input/output devices are connected
3. Run `sonido devices` in CLI to list available devices
4. Try specifying devices explicitly: `sonido-gui --input "Device Name"`

### Audio Glitches/Crackling

1. Increase buffer size: `sonido-gui --buffer-size 512` or `--buffer-size 1024`
2. Close other audio applications
3. Check CPU usage in the status bar

### High Latency

1. Decrease buffer size: `sonido-gui --buffer-size 128` or `--buffer-size 256`
2. Note: smaller buffers require more CPU and may cause glitches on slower systems

### Buffer Overruns (Audio Glitches)

Buffer overruns occur when audio processing takes longer than the buffer duration, causing dropouts.

**Symptoms:**
- Crackling or popping sounds
- "OVERRUN" warning in status bar (yellow or red)
- CPU usage consistently above 80-100%

**Solutions:**
1. Increase buffer size (reduces CPU load per buffer)
2. Reduce CPU usage: bypass unused effects, use simpler chains
3. Close other CPU-intensive applications
4. Check power management settings (disable CPU throttling)

### Buffer Underruns (Stuttering)

Similar to overruns but caused by insufficient audio data. Usually resolves automatically when CPU load decreases.

### Monitoring Buffer Health

- **CPU Sparkline**: Watch the real-time CPU graph in the status bar. Spikes above 80% indicate potential buffer issues.
- **Buffer Status**: The OVERRUN indicator shows cumulative overrun count. If it appears frequently, increase buffer size.
- **Latency Display**: Balance latency needs vs. stability—lower buffer = lower latency but higher CPU.

### Device Selection Issues

List available devices first:
```bash
sonido devices
```

Then specify the exact device name:
```bash
sonido-gui --input "USB Audio Interface" --output "USB Audio Interface"
```

### Application Won't Start

Check logs for errors:
```bash
RUST_LOG=debug sonido-gui
```

Common issues:
- Missing audio drivers (install ALSA on Linux, check CoreAudio on macOS)
- Permission issues with audio devices
- Conflicting sample rates (try `--sample-rate 44100`)

## Keyboard Shortcuts

Currently, the GUI is primarily mouse-driven. Keyboard shortcuts may be added in future versions.

## Recent Changes

### Preset Loading Improvements
Selecting a preset now performs a "hard reset": the application stops audio, rebuilds the entire effect chain to exactly match the preset (adding or removing effects as needed), applies all parameters, and restarts audio. This ensures preset chains are reconstructed precisely rather than just modifying parameters of existing effects.

### Input Gain Safety
The input gain defaults to 0 dB (unity gain). The knob range spans from -20 dB to +20 dB.

### Level Meter Evolution
The level meters now display a combined Peak and RMS visualization:
- RMS bar: shows average signal level (gradient green→yellow→red)
- Peak bar: overlays RMS in brighter colors (green → yellow → red when >0.7 → >0.95)
- Peak hold: thin semi-transparent white line indicates peak level

This design provides clearer visual feedback for level monitoring, inspired by professional DAW meters.

### Drag-and-Drop Reordering
Effects in the chain can now be dragged to any slot in a single motion. A translucent drop target indicator appears when hovering over a slot. The effect moves directly to the drop location when the mouse is released, making chain rearrangement faster and more intuitive.

## See Also

- [Getting Started](GETTING_STARTED.md) - Quick start guide
- [CLI Guide](CLI_GUIDE.md) - Command-line interface documentation
- [Effects Reference](EFFECTS_REFERENCE.md) - Detailed effect documentation
- [Architecture](ARCHITECTURE.md) - System architecture overview
