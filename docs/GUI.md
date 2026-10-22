# Sonido GUI

`sonido-gui` is the standalone node-graph editor: build an effect graph from the 36 registry effects, play a test signal or WAV file through it, perform it with six macros and an A/B morph, and export the result to the CLAP graph-player plugin or the Daisy pedal. It is built on [egui](https://github.com/emilk/egui)/eframe and runs natively and in the browser (wasm).

![The node-graph editor with a Distortion, Delay and Reverb chain wired between the input and output nodes, six macro knobs, and the A/B morph strip](img/editor.png)

## Design System

The editor, the plugins and the web build share one design system: tokens, effect families, the Sonido Vector display face, the knob, meter and selector components, and the plugin Cabinet layout. It is specified in [DESIGN_SYSTEM.md](DESIGN_SYSTEM.md); the values live in `crates/sonido-gui-core/src/theme.rs`.

## Installation

### Native (from source)

```bash
cargo run -p sonido-gui

# Or install the binary
cargo install --path crates/sonido-gui
sonido-gui
```

Linux build dependencies: the ALSA headers for cpal's ALSA backend (`alsa-lib-devel` on Fedora, `libasound2-dev` on Debian/Ubuntu; PipeWire systems serve it through `pipewire-alsa`), plus the X11/Wayland/GL development libraries eframe links against. CI's Debian list is `SYSTEM_DEPS` in [`.github/workflows/ci.yml`](../.github/workflows/ci.yml).

### Web (wasm)

Requires [Trunk](https://trunkrs.dev/) (`cargo install trunk`).

```bash
# Compile check (fast gate for cfg issues, no runtime)
cargo check --target wasm32-unknown-unknown -p sonido-gui

# Dev server with hot reload, at http://127.0.0.1:8080
cd crates/sonido-gui
trunk serve
```

Browsers keep audio suspended until a user gesture: the first click, key press, or PLAY resumes it. Hard-refresh (Ctrl+Shift+R) after a rebuild to bypass the cache. There is no headless wasm test runner; the browser is the runtime test. `cargo check` for wasm catches missing `#[cfg]` gates and std-only APIs that the native build doesn't.

The web build differs from native only where a browser tab can't reach: Save/Load use the browser's download and file picker, and Export offers patch JSON and the pedal `.bin` (no CLAP-folder write, no USB flash).

## Command-Line Options

```
sonido-gui [OPTIONS]

    --output <OUTPUT>                    Output device: index, exact name, or unique part of a name
    --sample-rate <SAMPLE_RATE>          Fallback sample rate in Hz (default: 48000)
    --buffer-size <BUFFER_SIZE>          Buffer size in samples (default: 2048)
    --effect <EFFECT>                    Single-effect mode with this registry ID
    --screenshot <SCREENSHOT>            Render the editor to a PNG, then exit
    --screenshot-size <SCREENSHOT_SIZE>  Window size for --screenshot, as WxH
    -h, --help                           Print help
    -V, --version                        Print version
```

- **Audio device.** `--output` picks the output by index, exact name or a unique part of its name (the same rule as the CLI's `--output`; `sonido devices` lists them). Without it the editor uses the system default. On Linux the device list is ALSA's, which PipeWire serves through `pipewire-alsa`.
- **Sample rate.** The device's own default rate is used; `--sample-rate` applies only if that config can't be read. The status bar shows the rate actually in use.
- **Input.** There is no microphone or line input. The source is the built-in signal generator or a WAV file (see [Source](#source)).
- **Single-effect mode.** `--effect reverb` shows one effect's panel full-width with no graph, info band, or morph band. An unknown ID exits with the list of valid IDs. The IDs are in [EFFECTS_REFERENCE.md](EFFECTS_REFERENCE.md).
- **Screenshots.** `--screenshot out.png` boots the full app offscreen-sized, renders a few frames, writes the PNG and exits. Without `--effect` it first loads a demo chain (Input → Distortion → Delay → Reverb → Output, Distortion selected). The default size is 1600×1000; `--screenshot-size 390x844` previews the phone layout. This is how `docs/img/editor.png` is produced.

```bash
sonido-gui --buffer-size 512          # lower latency
sonido-gui --effect distortion        # one effect, no graph
sonido-gui --screenshot editor.png --screenshot-size 1440x900
```

## Window Layout

```
+---------------------------------------------------------------------------+
| SONIDO PLAY FOCUS UNDO REDO +EFFECT BYPASS Save Load Export GEN FILE  ... |
|   ... MACROS Auto (K1)..(K6)                               [errors] (o)   |
+---------------------------------------------------------------------------+
|IN|                       GRAPH CANVAS                                |OUT|
|  |   o──[Distortion]──[Delay]──[Reverb]──o                           |   |
|  |                                                                   |   |
|GAIN                                                                  |VOL|
+---------------------------------------------------------------------------+
| SELECTED EFFECT PANEL  (knobs, selectors, ACTIVE/BYPASSED, LOCK)          |
+---------------------------------------------------------------------------+
| INFO: name · description · live values · shortcuts                        |
+---------------------------------------------------------------------------+
| MORPH  status  RAMP  A⇄B   (A) ▮▮▮▮▮▮▯▯▯▯▯▯▯▯▯▯▯▯ (B)                       |
+---------------------------------------------------------------------------+
| 48000Hz | 42.7ms | CPU: 2.3% ▁▂▃ | Pedal: 3/8 · CPU 64% | > Sine 440 Hz   |
+---------------------------------------------------------------------------+
```

The layout adapts to width. **Desktop** (≥ 900 px) is shown above; on a wide window the header is a single row (drawn as two here), and it wraps when space runs out. **Tablet** (600–900 px) wraps the header and status bar and enlarges touch targets. **Phone** (< 600 px, web only, since the native window has an 800×550 minimum) drops the FOCUS and + EFFECT buttons and the info band, moves the macros and morph into a **RIG** drawer toggled from the header, and adds a floating **+** button over the canvas.

### Header

- **SONIDO**: the vector wordmark.
- **PLAY / PAUSE**: one transport for both sources (Space).
- **FOCUS**: hides the info band and morph band for a full-height canvas (F). Desktop/Tablet.
- **UNDO / REDO**: structural edits (Ctrl+Z, Ctrl+Shift+Z / Ctrl+Y), 64 steps deep.
- **+ EFFECT**: opens the ADD EFFECT palette (Desktop/Tablet).
- **BYPASS**: bypasses the whole chain with a 5 ms crossfade to the dry signal.
- **Save / Load**: the session as JSON (see [Sessions](#sessions)).
- **Export**: see [Export](#export).
- **GEN / FILE**: source select.
- **MACROS**: **Auto** plus the six macro knobs K1–K6 (in the RIG drawer on Phone).
- **Right side**: audio status LED (green running, red stopped), plus, when present, `errors: N` (audio stream errors such as underruns), the last compile error, the last export result, and an audio start error with a **Retry** button.

### I/O wall bars

The canvas is flanked by two full-height level meters, **IN** (left) and **OUT** (right). Each shows RMS with the peak headroom above it, a falling peak-hold line, and the highest peak in dBFS at the top, red past 0 dBFS. Click a meter to reset its peak. The **GAIN** knob at the foot of IN trims the source from −20 to +20 dB; the **VOL** knob at the foot of OUT sets the master from −40 to +6 dB. Both default to 0 dB. The Input and Output node pins are welded to the bars' inner edges.

## Graph Editor

The canvas is an [egui-snarl](https://crates.io/crates/egui-snarl) node graph. It opens with Input wired straight to Output (plain passthrough).

- **Input / Output**: fixed, one each; they have no menu and can't be selected.
- **Effect nodes**: any registry effect. The header shows the effect name and the body shows its count of visible parameters. Node, header and wire colors come from the effect's family (`Family::from_category(..).accent()`):

| Category | Family | Color |
|----------|--------|-------|
| Distortion | HEAT | orange |
| Dynamics | VOLT | gold |
| Modulation | FLUX | magenta |
| Time-based | ECHO | violet |
| Filter | WAVE | cyan |
| Utility | LINE | green |

Each effect's category and parameters are listed in [EFFECTS_REFERENCE.md](EFFECTS_REFERENCE.md).

**Interactions:**

- **+ EFFECT**: opens a searchable palette of all effects (type to filter). The pick is appended as the last node before Output and selected.
- **Right-click canvas**: a search field plus category submenus. The new node is spliced into the wire nearest the click.
- **Click a node**: selects it and opens its panel. Click empty canvas to deselect.
- **Selected node**: a floating **Duplicate / Remove** bar appears at the canvas's top-left, and Delete or Backspace removes the node.
- **Right-click a node**: Remove or Duplicate.
- **Drag pin to pin**: wire an output to an input. One output may feed several inputs (fan-out), and an input may take several wires, which are summed (fan-in).
- **Ctrl+Scroll / pinch**: zoom, clamped to 0.5×–1.5×.

After every structural edit the nodes re-flow left to right by depth. If the last node feeding Output is removed, the nearest remaining node is wired to Output so the graph never goes silent.

**Compilation.** Every structural edit recompiles automatically. The compiler walks the snarl topology, inserts Split nodes for fan-out and Merge nodes for fan-in (so you never place them yourself), builds a `ProcessingGraph`, creates the effects through the registry, and sends a `GraphCommand::ReplaceTopology` that the audio thread swaps in at the next buffer. Explicit Split/Merge nodes in older sessions still load and compile.

## Effect Panel

Selecting an effect node opens its panel below the canvas, framed in the effect's family color (cyan or amber instead while sculpting morph pose A or B). The panel is the plugin cabinet's deck:

- **Title row**: the effect name, an **ACTIVE / BYPASSED** toggle (a click-free per-effect bypass), and, once the morph is active, a **LOCK** toggle that holds this effect still while the morph sweeps the rest of the rig.
- **Stepped parameters**: `cabinet::selector`, which draws segmented buttons for a few short options and otherwise a ◀ value ▶ stepper (click the value to advance, scroll to step).
- **Continuous parameters**: LED-ring knobs in the family color (`bridged_knob_accent`), with the formatted value and the label under each ring. Ranges that span zero fill from twelve o'clock. When an effect has distinct A/B poses, cyan and amber ghost ticks mark each knob's A and B values.
- **Every** visible parameter is shown, so the node's "N params" count matches the panel. Read-only and hidden parameters are skipped.
- **The looper** has its own transport panel instead.

**Knob controls:** drag vertically (Shift for fine), scroll the mouse wheel over a knob (Shift for fine), use Up/Down on a focused knob (1 % per press, Shift 0.2 %), use Left/Right to move focus between knobs, and double-click to reset to default.

**Right-click any knob or selector** to get **Map to Macro 1–6** or **Clear macro mapping**.

With nothing selected, the area shows a one-line prompt.

### Info band

Docked under the panel on Desktop/Tablet (hidden in Focus mode). It shows the selected effect's name and description from the registry, every parameter's live formatted value, and the relevant shortcuts. With no selection it shows the general shortcuts.

## Macros

Six performance knobs, **K1–K6**, sit in the header. Each macro sweeps any number of parameters at once.

- **Map**: right-click a parameter in an effect panel and choose **Map to Macro N**.
- **Auto**: seeds one macro per effect in chain order (the first six), each bound to that effect's first continuous, musical parameter over its full range and curve. The knob position is set to reproduce the current value, so assigning never jumps the sound.
- **Edit**: right-click a macro knob to open its mapping window. There you can rename it, set each target's min/max, invert (⇄), remove a target (✕), or **Clear all**.
- **Hover** a macro knob for its name and target count. A small cyan dot under the knob marks a mapped macro.

## A/B Morph

The MORPH band crossfades the whole rig between two poses, A and B. Every effect owns its own A and B values, keyed to its node, so adding, removing or reordering effects never desyncs the morph. A new effect joins flat (A = B) until you give it a distinct pose.

- **Left-click A or B**: park the rig at that pose and *sculpt* it; every knob you turn is recorded into it. The readout says `editing A` / `editing B`.
- **Right-click (or double-click) A or B**: grab the current sound into that pose.
- **Drag the LED bar**: perform; the readout shows `A→B nn%`.
- **RAMP / MOM / LATCH**: the mode for the **A⇄B** trigger, saved in the patch (the pedal ignores it: with its A/B toggle in the middle, an FS2 tap glides to the other side at the patch's morph speed, and a tap mid-glide turns back). Ramp glides, Momentary holds B while pressed, and Latch toggles. **A⇄B** is enabled once some effect has distinct poses.

Interpolation is per parameter and curve-aware (`curve_lerp`). Linear parameters interpolate linearly, logarithmic ones (frequencies) geometrically, and snap/stepped ones switch at the midpoint. Per-effect bypass switches at `t = 0.5`. Locked effects are left alone.

## Source

The audio source is either the built-in signal generator (**GEN**, the default) or a WAV file (**FILE**), switched in the header. The app opens on GEN, paused; press Space or PLAY to start.

**Generator signals** (status-bar drop-down):

| Signal | Description |
|--------|-------------|
| Sine | Pure tone (440 Hz) |
| Sweep | Logarithmic frequency sweep |
| White Noise | Flat spectrum, decorrelated L/R |
| Pink Noise | About −3 dB/octave (Voss-McCartney) |
| Impulse | Single-sample click train |
| Saw Chord | Root + major 3rd + perfect 5th, naive saw (440 Hz root) |

The frequency is shown next to the selector for Sine and Saw Chord. It isn't adjustable from the UI.

**File mode:** **OPEN** picks a WAV file. On native you can also drop a `.wav` onto the window. The status bar then shows play/pause, the file name, and position/duration.

## Status Bar

- **Sample rate**: the rate in use (the device's).
- **Latency**: one buffer, `buffer_size / sample_rate`, in ms.
- **CPU**: audio-callback time as a percentage of the buffer duration, with a 60-sample sparkline. Green is ≤ 80 %, yellow is 80–100 %, and red is > 100 %. Debug builds append `(debug)`.
- **Pedal: N/8 · CPU x%**: the pedal's rule, the same one Export ▸ Pedal checks (`sonido_patch::validate::daisy_load`): the graph's effect count against the pedal's 8-node limit, and its estimated share of the pedal's CPU from the measured kernel costs (`sonido_platform::cycle_table`). Green fits; yellow is over 70 % (it runs, near the limit); red is over 80 %, more than 8 effects, an effect the pedal doesn't carry, or past its SDRAM. Hover for the reasons. For example, distortion → delay → reverb is red at 86 %: reverb alone is 62 %.
- **Transport**: generator play/pause, signal type and frequency; in FILE mode, the file controls.

The buffer size is fixed at launch by `--buffer-size`; there is no in-app selector.

## Sessions

**Save / Load** write and read a JSON session: topology and node positions, every effect's parameters and bypass (A and B poses), the six macros with names, mappings and knob positions, the morph mode, locks and position, the input gain and master volume, and the rig's name (`Session::name`, "My Rig" for a new rig or an older session). Older session versions load; missing fields take defaults. Loading a session clears undo history. Native uses OS file dialogs; the web build downloads `sonido-rig.json` and uses the browser file picker.

## Export

The **Export** menu projects the rig to a canonical `sonido_patch::Patch` and offers:

- **Name**, a field at the top of the menu: the rig's name, which the pedal and the graph player show. It is saved in the session, and sending to the pedal again under the same name replaces the rig there.
- **Export as CLAP patch** (native): writes `<name>.sonidopatch.json` (the rig's name, made file-safe) to the rig library, `sonido_config::paths::rig_library_dir()`. The graph-player plugin's RIG selector lists these alongside its built-in factory rigs. The directory is created on first export.

| Platform | Rig library |
|----------|-------------|
| Linux | `$XDG_DATA_HOME/sonido/patches` (or `~/.local/share/sonido/patches`) |
| macOS | `~/Library/Application Support/sonido/patches` |
| Windows | `%APPDATA%\sonido\patches` |

- **Save patch JSON…**: the same patch, anywhere.
- **Pedal (Daisy)**: the rig is validated against the pedal's supported effects and its CPU-cycle and SDRAM budgets. The menu shows `✓ fits pedal budget` or `✗ exceeds pedal limits` with each finding. The check is `sonido_patch::validate::validate_for_daisy`, the one the CLI uses. When it fits, you can choose **Save pedal binary (.bin)…** (a 4 KB patch sector) or **Send to pedal** (native only). Send to pedal talks to the playing pedal over its USB port (`sonido_patch::client`, the first pedal found): the rig replaces the bank rig with the same name, or takes the first empty bank slot, and the pedal checks it against the same rule and plays it at once; the result names the bank slot ([EMBEDDED.md](EMBEDDED.md#usb-rig-link), [PATCH_PLATFORM.md](PATCH_PLATFORM.md)).

The result appears in the header.

## Keyboard Shortcuts

Global shortcuts are inactive while the ADD EFFECT palette or a macro mapping window is open.

| Key | Action |
|-----|--------|
| Space | Play / pause the source |
| F | Toggle Focus mode (Desktop/Tablet) |
| Ctrl+Z (⌘Z) | Undo structural edit |
| Ctrl+Shift+Z or Ctrl+Y (⌘⇧Z / ⌘Y) | Redo |
| Delete / Backspace | Remove the selected effect node |
| Up / Down | Adjust the focused knob (Shift = fine) |
| Left / Right | Move focus between knobs |
| Ctrl+Scroll | Zoom the graph |

## Real-Time Architecture

```
[Generator | WAV file] -> GAIN -> [GraphEngine] -> chain BYPASS crossfade -> VOL -> safety clamp -> [default output device]
                            |                                                                  |
                            +--------------------- MeteringData (in/out peak+RMS, CPU) --------+--> GUI
```

Audio runs in a single cpal **output** callback that owns an `AudioProcessor` (`audio_processor.rs`). Each buffer it:

1. Drains transport commands (play/pause, source, signal type, file load).
2. Drains `GraphCommand`s (`ReplaceTopology` swaps in a freshly compiled `GraphEngine` and rebuilds the parameter bridge's slots).
3. Pushes every parameter and per-effect bypass from the bridge into the graph.
4. Generates or reads the source, applies input gain, and meters it.
5. Runs the graph. If the block goes non-finite (a runaway feedback loop), it resets the graph and outputs silence for that block.
6. Crossfades to dry for chain BYPASS, applies master volume, flushes NaN/Inf and clamps to ±4.0 (about +12 dBFS).
7. Meters the output, measures CPU, and sends `MeteringData`.

The callback reuses its scratch buffers, so it doesn't allocate.

**GUI → audio communication:**

- **Effect parameters** live in `AtomicParamBridge` (`atomic_param_bridge.rs`): one `AtomicU32` per parameter (bit-cast `f32`) in per-slot state held behind an `ArcSwap`. GUI writes and audio reads are wait-free loads. Structural changes (`rebuild_from_manifest` on `ReplaceTopology`) publish a new slot list via RCU, so readers never block.
- **Input gain and master volume** are `AtomicParam`s (`audio_bridge.rs`).
- **Topology and transport commands** travel over unbounded `crossbeam_channel`s. The editor sends only `ReplaceTopology`; the `Add`/`Remove` variants exist for linear-chain mutation.
- **Metering** comes back over a bounded channel (capacity 4) with `try_send`, so the audio thread never blocks on a slow GUI.

All parameter access from UI code goes through the `ParamBridge` trait (`sonido-gui-core`), whose `begin_set`/`end_set` gesture brackets map to host automation gestures in the CLAP plugins.

### Shared GUI crate

`sonido-gui-core` holds everything the editor shares with the plugins (`sonido-plugin`): the theme and families, the cabinet layout and selector, the widgets (`Knob`, `LevelMeter`, `morph_bar`, macro menu, vector font, and others), the `ParamBridge` trait, and the effect panels (`GenericPanel` and `LooperPanel`).

## Troubleshooting

**No audio.**
1. Check the header's status LED and any audio error. Click **Retry** after fixing the device.
2. The editor plays to the system default output device. Change it in your sound settings.
3. The source starts paused. Press Space or PLAY. In FILE mode, open a file first.
4. On the web, click or press a key once to resume the AudioContext.
5. A new graph is a passthrough, and the chain **BYPASS** (red LED) or an effect's **BYPASSED** toggle silences processing, not the source.

**Crackles, dropouts, `errors: N` in the header.** The callback is missing its deadline. Watch the CPU readout: sustained readings near or above 100 % mean overruns.
- Raise the buffer: `sonido-gui --buffer-size 4096`.
- Use a release build (`cargo run --release -p sonido-gui`). Debug builds are much slower, as the `(debug)` tag warns.
- Remove or bypass heavy effects, and close other CPU-heavy applications.

**Too much latency.** Lower the buffer (`--buffer-size 512` or `256`), at the cost of more CPU headroom per buffer.

**Won't start.** Run with logs:

```bash
RUST_LOG=debug sonido-gui
```

On Linux, a build failure mentioning `alsa` means the ALSA development package is missing (see [Installation](#installation)).

## See Also

- [Getting Started](GETTING_STARTED.md): quick start
- [Effects Reference](EFFECTS_REFERENCE.md): every effect and parameter
- [Design System](DESIGN_SYSTEM.md): tokens, families, components
- [Patch Platform](PATCH_PLATFORM.md): the patch format behind sessions and export
- [CLI Guide](CLI_GUIDE.md): the `sonido` command-line tool
- [Architecture](ARCHITECTURE.md): system overview
