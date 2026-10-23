# Architecture

## Overview

Sonido is a production-grade DSP library designed for multi-target deployment:
- **Desktop**: CLI and GUI applications (the GUI also builds for wasm32)
- **Embedded**: Electrosmith Daisy / Hothouse hardware
- **Plugins**: one CLAP bundle via clack (`sonido.clap`: all 36 effects plus the graph player), and one VST3 bundle (`sonido.vst3`, a clap-wrapper shim that loads `sonido.clap`; `scripts/bundle-vst3.sh`)

The library is built with stereo-first processing and no_std compatibility at its core.

## Crate Diagram

Arrows point from a crate to the sonido crates it depends on (normal
dependencies only; dev-dependencies omitted). `[no_std]` marks crates that
build without `std` (with `alloc`).

```
Applications
  sonido-cli ──────▶ core, registry, analysis, io, config, synth, graph-dsl, patch, rigs
  sonido-gui ──────▶ core, effects, gui-core, registry, graph-dsl, patch, platform, rigs, io
  sonido-plugin ───▶ core, effects, registry, config, patch, rigs, gui-core   (CLAP/VST3 bundle)
  sonido-daisy ────▶ core, effects, registry, patch, platform*     (firmware, no_std;
                     build script: rigs                             excluded from the workspace)
Libraries
  sonido-gui-core ─▶ core, registry              widgets, Cabinet, ParamBridge
  sonido-graph-dsl ▶ core, registry, config      text DSL → ProcessingGraph
  sonido-rigs ─────▶ core, registry, patch,     the factory rigs and       [std]
                     platform                    the pedal guide
  sonido-config ───▶ core, registry, patch       presets + paths            [std]
  sonido-io ───────▶ core                        WAV + cpal audio I/O       [std]
  sonido-analysis ─▶ core                        FFT, spectral analysis     [std]
  sonido-registry ─▶ core, effects               effect factory             [no_std]
  sonido-patch ────▶ core, platform              canonical Patch format     [no_std]
  sonido-synth ────▶ core                        synthesis                  [no_std]
  sonido-platform ─▶ core                        controls + knob mapping    [no_std]
  sonido-effects ──▶ core                        36 effect kernels          [no_std]
  sonido-core                                    DSP primitives, graph      [no_std]

* optional (`platform` feature)
```

## Crate Responsibilities

### sonido-core

The foundation crate providing DSP primitives. Designed for `no_std` environments.

**Why a separate core crate?** Every DSP system needs the same building blocks: delay lines, filters, LFOs, parameter smoothing. By isolating these into `sonido-core` with `no_std` support, the same primitives compile for desktop (x86_64) and embedded (ARM Cortex-M7) targets without conditional compilation in the effects. This mirrors the layered architecture used in commercial DSP frameworks like JUCE's `dsp` module and Faust's core library, but with Rust's zero-cost abstractions ensuring no runtime overhead from the abstraction boundary.

**Key components:**
- `Effect` trait: Object-safe interface all effects implement (`snap_params()` jumps every smoothed parameter to its target, for a freshly built rig)
- `level`: the level contract — `Level` (trim + wet/dry correlation), `blend()` (correlation-normalized constant-power mix), `NOMINAL_PEAK_DBFS`
- `SmoothedParam`: Zipper-free parameter changes with exponential/linear smoothing
- `InterpolatedDelay` / `FixedDelayLine`: Delay buffers with fractional interpolation
- `Biquad`: IIR filter building block for EQ, lowpass, highpass, etc.
- `StateVariableFilter`: Multi-output filter (LP, HP, BP simultaneously)
- `CombFilter`: Comb filter with damping for reverb algorithms
- `AllpassFilter`: true Schroeder allpass (unity gain at every frequency) for diffusion
- `Lfo`: Low-frequency oscillator for modulation effects (5 waveforms)
- `EnvelopeFollower`: Amplitude envelope detection for dynamics
- `Oversampled`: Generic 2x/4x/8x oversampling wrapper for anti-aliasing
- `ModulationSource` trait: Unified interface for LFOs, envelopes, followers
- `TempoManager`: Tempo tracking with musical timing utilities
- `NoteDivision`: Musical note divisions (whole, half, quarter, dotted, triplet)
- `graph/`: `ProcessingGraph` (DAG routing), `GraphEngine`, `StereoSamples`
- `kernel/`: `DspKernel`, `KernelParams`, `Adapter`, plus `MorphSpace` / `curve_lerp` / `ChainMorph` for A/B morphing
- `MacroMap`: Six-knob macro mappings (`MacroTarget` = slot param or `GlobalParam`)

### sonido-effects

Audio effect implementations built on sonido-core. All `no_std` compatible with full stereo support.

**Why separate effects from core?** The `Effect` trait and DSP primitives change rarely; effect implementations change often as new algorithms are added or refined. Separating them means adding a new effect never risks breaking the core infrastructure. It also means `sonido-core` can be used independently for custom DSP work without pulling in all 36 effect implementations.

**36 effects** (registry IDs, grouped by `EffectCategory`; full parameter
reference in `docs/EFFECTS_REFERENCE.md`):

| Category | Effects |
|----------|---------|
| Dynamics | `compressor`, `gate`, `limiter`, `deesser`, `multiband_comp`, `transient_shaper` |
| Distortion | `distortion`, `tape`, `bitcrusher`, `amp`, `cabinet`, `glitch` |
| Modulation | `chorus`, `flanger`, `phaser`, `vibrato`, `tremolo`, `ringmod`, `pitch_shift`, `texture`, `time_stretch` |
| Time-based | `delay`, `reverb`, `looper`, `drone`, `plate_reverb`, `spring_reverb`, `harmonic_habitat` |
| Filter | `filter`, `wah`, `eq`, `shelving_eq` |
| Utility | `preamp`, `stage`, `stereo_widener`, `tuner` |

The reverb is an 8-line Hadamard FDN on Freeverb delay tunings
(`kernels/reverb.rs`); the chorus is multi-voice with feedback and tempo sync.

### Kernel Architecture

All 36 effects have kernel-architecture implementations in `crates/sonido-effects/src/kernels/` that separate pure DSP from parameter ownership. This enables two deployment modes from the same algorithm:

**Three-layer pattern:**

1. **`KernelParams`** (`XxxParams`) — typed parameter struct in user-facing units (dB, %, Hz). Provides indexed access (`get`/`set`), descriptor metadata, smoothing hints per parameter, preset morphing (`lerp`), and normalized ↔ user-unit conversion.

2. **`DspKernel`** (`XxxKernel`) — owns ONLY DSP state (filters, delay buffers, ADAA processors, LFO phases). Receives `&Params` by reference each sample. No `SmoothedParam`, no platform types.

3. **`Adapter<K, SmoothedPolicy>`** — bridges kernel to `Effect + ParameterInfo`. Owns per-parameter `SmoothedParam` instances configured from `KernelParams::smoothing()`. Advances smoothers per sample, writes into a params snapshot, and passes to `kernel.process_stereo()`. (Each kernel applies its own level trim, in `Self::blend` or `Self::level` / `Self::level_stereo`.)

**Every target that plays rigs** uses `Adapter<K, SmoothedPolicy>`: the registry creates `Adapter<XxxKernel, SmoothedPolicy>` for all 36 effects, and the plugins, the editor, the CLI and the pedal firmware all build effects through it. The pedal (`crates/sonido-daisy/examples/sonido_pedal.rs`) plays every rig through `sonido_patch::PatchPlayer` with registry-built effects (`patch_loader::make_effect`) on an SDRAM heap, so a rig sounds the same as in the graph-player plugin. `Adapter<K, DirectPolicy>` (`Adapter::new_direct`) — the same `Effect + ParameterInfo` interface with no smoothing — remains for firmware examples that drive one kernel (`single_effect.rs`, `graph_audio_test.rs`); `single_delay.rs` and `single_reverb.rs` call the kernel directly with `XxxParams::from_knobs()`.

**Level contract.** Every kernel declares `const LEVEL: Level` (`crates/sonido-core/src/level.rs`), taken from the generated table `crates/sonido-effects/src/calibration.rs`: a trim and, for an effect with a mix control, the measured correlation of its wet signal with the dry input. Kernels mix with `Self::blend` / `Self::blend_stereo`, a sine/cosine constant-power law normalized for that correlation, so at its defaults every effect plays at the loudness of its input and its mix control does not change that loudness. The table is generated by `cargo run -p sonido-effects --example calibrate --release` (a strummed reference guitar at −20 dBFS peaks, integrated LUFS) and held by `tests/level_contract.rs`. See `docs/KERNEL_ARCHITECTURE.md`.

`SmoothingStyle` tiers: `None` (snap), `Fast` (5ms), `Standard` (10ms), `Slow` (20ms), `Interpolated` (50ms), `Custom(ms)`.

See ADR-028 in `docs/DESIGN_DECISIONS.md` for full rationale.

### sonido-analysis

Spectral analysis tools for reverse engineering hardware and signal analysis. Requires `std` for FFT.

**Why include advanced analysis?** The same FFT and filtering tools used for audio can analyze any 1D signal. The `sonido-analysis` crate requires `std` because FFT computation uses heap allocation for twiddle factors and scratch buffers, which is acceptable since analysis runs offline.

**Components:**
- `Fft`: FFT wrapper around rustfft
- `Window`: Window functions (Rectangular, Hann, Hamming, Blackman, Blackman-Harris)
- `TransferFunction`: Measure frequency response between two signals
- `SineSweep`: Generate logarithmic sine sweeps for IR capture; `estimate_rt60` / `energy_decay_curve` for decay analysis
- `ThdAnalyzer` / `ImdAnalyzer`: Harmonic and intermodulation distortion measurement
- `ConstantQTransform` / `Chromagram`, `MelSpectrogram`, `StftAnalyzer`: Time-frequency analysis
- `LufsMeter`, `StereoCorrelation`, `detect_pitch`: Loudness, stereo and pitch measurement
- `loudness::effect_gain_db` and `signals` (`reference_guitar`, `strummed_guitar`): the measurement behind the level contract — the loudness change an effect makes to a synthetic strummed guitar peaking at −20 dBFS, in integrated LUFS

**Advanced Spectral Analysis:**
- `FilterBank`: Multi-band bandpass filter bank with 4th-order Butterworth filters
- `FrequencyBand`: Frequency band specification with standard bands (delta, theta, alpha, beta, gamma)
- `HilbertTransform`: FFT-based Hilbert transform for instantaneous phase/amplitude
- `PacAnalyzer`: Phase-Amplitude Coupling analyzer (Mean Vector Length, Kullback-Leibler)
- `PacResult`: PAC analysis results (modulation index, preferred phase, phase histogram)
- `Comodulogram`: Multi-frequency PAC analysis for visualizing coupling patterns

**DSP Primitives:**
- `LmsFilter` / `NlmsFilter`: Adaptive filters for noise/echo cancellation and system identification
- `xcorr_direct` / `xcorr_fft` / `xcorr_normalized`: Cross-correlation with `peak_lag` for delay estimation
- `Ddc`: Digital down-conversion (NCO + FIR lowpass + decimation) for baseband shifting
- `unwrap_phase` / `unwrap_phase_tol` / `unwrap_phase_quality` / `PhaseTracker`: Phase unwrapping (batch, quality-guided, streaming)
- `decimate` / `interpolate` / `resample` / `design_lowpass`: Rational resampling via polyphase filter decomposition

**Export Formats:**
- `export_frd` / `import_frd`: FRD format (frequency response data, compatible with REW)
- `export_spectrogram_csv` / `export_spectrogram_pgm`: Spectrogram export as CSV or PGM image
- `export_distortion_json`: THD analysis results as JSON

### sonido-io

Audio I/O layer with pluggable backend abstraction, WAV file I/O, and full stereo support.

**Audio Backend Architecture (ADR-023):**
- `AudioBackend` trait: Platform-agnostic interface for device enumeration and stream construction
- `CpalBackend`: Default backend wrapping cpal (ALSA, CoreAudio, WASAPI, WebAudio)
- `StreamHandle`: Type-erased RAII stream handle — dropping stops playback
- `BackendStreamConfig`: Backend-agnostic stream configuration

**Components:**
- `read_wav` / `write_wav`: Interleaved WAV file I/O (returns/takes a `WavSpec`)
- `read_wav_stereo` / `write_wav_stereo`: Stereo WAV file I/O (mono files are duplicated to both channels)
- `AudioStream`: Real-time audio streaming (`run`, `run_stereo`, `run_output`) — the cpal path the CLI uses

Note: `StereoSamples` and `GraphEngine` live in `sonido-core` (in `src/graph/`). `sonido-io` re-exports both for backwards compatibility.

**Stereo I/O:**
```rust
use sonido_io::{read_wav_stereo, write_wav_stereo, StereoSamples};

let (samples, spec) = read_wav_stereo("input.wav")?;
// samples.left, samples.right, samples.to_interleaved(), etc.

write_wav_stereo("output.wav", &processed, spec)?;
```

### sonido-synth

Full synthesis engine for building synthesizers. `no_std` compatible.

**Why a separate synth crate?** Synthesis has different concerns from effects processing. Effects transform existing audio; synthesizers generate it. The voice management, note allocation, and modulation matrix concepts have no analog in effect processing. Keeping synthesis separate also means an embedded effects pedal can depend on `sonido-core` + `sonido-effects` without pulling in oscillator, envelope, and voice code it will never use.

**Oscillators:**
- `Oscillator`: Audio-rate oscillator with PolyBLEP anti-aliasing
- `OscillatorWaveform`: Sine, Triangle, Saw, Square, Pulse(width), Noise
- `WavetableOscillator` / `Wavetable`: Wavetable playback (`wavetable` module)
- `FmOperator`, `Fm2Op`, `Fm4Op` (+ `Fm4Algorithm`): FM synthesis (`fm` module)

**Envelopes:**
- `AdsrEnvelope`: Attack-Decay-Sustain-Release envelope generator
- `EnvelopeState`: Envelope stage tracking (Idle, Attack, Hold, Decay, Sustain, Release)

**Voice Management:**
- `Voice`: Single synthesizer voice (oscillators + filter + envelopes)
- `VoiceManager`: Polyphonic voice allocation with stealing strategies
- `VoiceAllocationMode`: RoundRobin, OldestNote, LowestNote, HighestNote
- `SubVoice`: Unison sub-voice (up to `MAX_UNISON`)

**Modulation:**
- `ModulationMatrix`: Flexible routing of modulation sources to destinations
- `ModulationRoute`: Single modulation routing (source, destination, amount, bipolar)
- `AudioModSource`: Use audio input as modulation source
- `AudioGate`: Convert audio amplitude to gate signal

**Complete Synths:**
- `MonophonicSynth`: Single-voice synth with portamento/glide
- `PolyphonicSynth<N>`: N-voice polyphonic synth
- `SynthNode`: Synth wrapped as a graph source node

### sonido-registry

Central registry for discovering and instantiating effects. Provides a unified API for the CLI, GUI, plugins, and firmware (the registry builds `no_std`).

**Why a registry pattern?** The CLI, GUI, and potential plugin hosts all need to create effects by name or category. Without a registry, each application would duplicate the mapping from strings to constructors. The registry centralizes this with `EffectDescriptor` metadata, enabling features like "list all modulation effects" or "create effect by name from a preset file" without the application knowing about individual effect types. This is the Factory pattern adapted for DSP -- common in plugin frameworks like VST3 (which uses a similar `IPluginFactory` interface).

**Key components:**
- `EffectRegistry`: Factory for creating effects by name
- `EffectDescriptor`: Metadata (id, name, short_name, description, category, param_count)
- `EffectCategory`: Effect categorization (Dynamics, Distortion, Modulation, etc.)
- `EffectWithParams`: Helper trait for accessing ParameterInfo through boxed effects (defined in `sonido-core`, re-exported)
- `EFFECT_UIDS` / `effect_uid` / `effect_by_uid`: Stable numeric effect UIDs used by the patch format
- `JAM_EFFECT_IDS`: the eight effects the pedal's jam offers, in the order of its pick knobs' zones (the jam is saved by UID, so the order is free to change)

**Usage:**
```rust
use sonido_registry::EffectRegistry;

let registry = EffectRegistry::new();
let mut effect = registry.create("distortion", 48000.0).unwrap();
```

### sonido-config

CLI-first configuration and preset management. Requires `std`.

**Key components:**
- `Preset`: Effect chain preset with metadata and effect configurations
- `EffectConfig`: Single effect configuration with parameters
- `EffectChain`: Runtime effect chain builder
- `validation`: Effect type and parameter validation
- `paths`: Platform-specific preset directories (user, system)
- `factory_presets`: Built-in presets for common use cases (`sonido presets`; the graph player's built-in rigs come from `sonido-rigs`, not from these)
- `patch`: `preset_to_patch` — convert a preset into a `sonido-patch` rig

**Usage:**
```rust
use sonido_config::{Preset, EffectConfig, user_presets_dir};

// Load a preset
let preset = Preset::load("my_preset.toml")?;

// Create programmatically (values are in each parameter's own units)
let mut preset = Preset::new("My Preset").with_description("Custom effect chain");
preset.effects = vec![
    EffectConfig::new("distortion").with_param("drive", "20"), // dB
    EffectConfig::new("reverb").with_param("room", "80"),      // % (name or short name)
];

preset.save(&user_presets_dir().join("my_preset.toml"))?;
```

### sonido-patch

The canonical rig format, `Patch`: graph topology, per-node A/B parameters, six macros, the rig's two toggles (T1, T3), morph configuration and globals, by stable effect UID. Builds `no_std` (with `alloc`) for the firmware; the default `std` feature adds JSON (serde) and the host tools.

**Key modules:**
- `binary`: one `Patch` ↔ one 4 KB QSPI sector (`encode`, `decode`); the toggle section is written only for a rig that uses its toggles (header flag bit 0)
- `build`, `runtime`: `build_graph_from_patch` and `PatchPlayer`, which runs a rig in the graph-player plugin and on the pedal
- `validate`: structural and pedal-fit checks; `validate_for_daisy` is the one pedal check the editor, the CLI and the factory-rig tests use (`sonido_platform::cycle_table`), and `daisy_load` the same CPU and memory estimate as numbers (the editor's status bar, the firmware's `check_rig`). A `Finding` is data (`Problem`) formatted only when shown, so the firmware carries no message text
- `controls`: what every footswitch press, toggle (`toggle_role`), knob page, LED and power-on gesture does; the firmware dispatches through it and `sonido_rigs::guide` draws it
- `pedal`: the jam (`jam_patch`, `JamSettings`) and the pedal's saved state (`PedalState`)
- `link`: the USB serial protocol between the pedal and the host (frames, requests incl. `Bootloader`, replies)
- `client` (`std`): the host side of the link. `LinkClient`, generic over `Read + Write`; `bank` reads the bank's rig names; `send_rig` replaces the bank rig with the same name, otherwise takes the first empty slot (`choose_send_slot`, shared with the browser editor's Web Serial path along with `reply`, `slot_rig`, `slot_sector` and the timeouts); `enter_bootloader`; with the `serial` feature, `open_pedal` / `pedal_ports` find the pedal by USB id `1209:0001`
- `dfu` (`std`, not wasm): firmware updates through `dfu-util`, to the Daisy bootloader's QSPI region only (`0x90000000`, firmware at `0x90040000`); rigs never go this way

### sonido-rigs

The factory rigs: six `sonido_patch::Patch`es built from registry effects (`factory_rigs()`: Shimmer Cathedral, Fuzz Lab, Space Echo, Granular Drift, Harmonic Habitat, Amp Stack). One set serves the pedal (its `build.rs` encodes them into the firmware image) and the graph-player plugin's rig library. Every rig maps its macros by the pedal's knob rule: K1 input gain, K2 morph time, K3 master volume around a level-matching trim (generated into `src/trims.rs` by `cargo run -p sonido-rigs --example calibrate --release`), and K4–K6 the rig's own controls; `tests/factory.rs` holds every rig at the dry guitar's loudness, to the pedal's CPU and memory rule (`validate_for_daisy`), and to a binary round trip. `guide` draws how to play the pedal from `sonido_patch::controls`, the knob maps (`sonido_platform::knob_mapping`) and these rigs: `html_guide()` is a self-contained infographic (an annotated drawing of the pedal, the knobs' pages by mode, the effect-pick dial, every jam effect and rig), `text_card()`/`effect_card()` the same as a terminal card; `sonido pedal controls` shows them. Requires `std`.

### sonido-platform

Hardware abstraction layer for multi-target deployment. Provides `no_std` compatible traits for physical controls and parameter mapping.

**Why abstract hardware controls?** A physical knob on a Hothouse pedal, a GUI slider, a MIDI CC message, and a DAW automation lane all represent the same concept: a continuous control mapped to an effect parameter. The `PlatformController` trait and `ControlMapper` let effect code remain hardware-agnostic. The `ControlId` namespacing (`0x00XX` = hardware, `0x01XX` = GUI, `0x02XX` = MIDI) means the same mapping code handles all input sources without conflicts.

**Key components:**
- `PlatformController`: Trait abstracting hardware I/O (knobs, toggles, footswitches, LEDs)
- `ControlMapper<N>`: Fixed-capacity table mapping `ControlId`s to effect parameter indices; denormalizes 0–1 control values through each parameter's `ParamDescriptor`
- `ControlId`: Namespaced control identifiers (hardware, GUI, MIDI, automation)
- `ControlType`: Control kinds (Knob, Toggle3Way, Toggle2Way, Footswitch, Led)
- `ControlState`: Control value with change tracking
- `knob_mapping` (`knob_map`, `knob_to_param`): Which Hothouse knob drives which parameter, per effect, plus per-effect knob floors
- `param_map` (`adc_to_param`, `adc_to_param_biased`): Scale-aware ADC-to-parameter conversion; the biased form puts the parameter's default (its voiced sweet spot) at noon
- `cycle_table`: per-effect CPU (rig-path worst block, measured on a Daisy Seed; regenerated by `scripts/update_cycle_table.py`) and SDRAM cost, and the one rule (`chain_fits`) the editor's export check and the pedal both apply

**Control ID Namespaces:**
- `0x00XX`: Hardware controls (knobs, switches)
- `0x01XX`: GUI controls
- `0x02XX`: MIDI CC
- `0x03XX`: Automation parameters

**Example:**
```rust
use sonido_platform::{ControlId, ControlMapper};

let mut mapper = ControlMapper::<16>::new();
mapper.map(ControlId::hardware(0), 0); // Knob 1    -> param 0
mapper.map(ControlId::midi(74), 2);    // MIDI CC 74 -> param 2
assert_eq!(mapper.get_param_index(ControlId::hardware(0)), Some(0));
// mapper.apply(id, normalized, &mut effect) denormalizes and sets the param.
```

### sonido-cli

Command-line interface tying everything together.

**Commands:**
- `process`: File-based effect processing (`--effect`, `--chain`, `--graph` DSL, or `--preset`)
- `realtime`: Live audio processing
- `play`: Play an audio file through effects
- `generate`: Test signal generation
- `analyze`: Spectral analysis (spectrum, transfer, ir, distortion, spectrogram, dynamics, imd, cqt, compare)
- `compare`: A/B audio comparison
- `info`: WAV file information
- `devices`: Audio device management
- `effects`: List available effects
- `presets`: Preset management (list, show, save, delete)
- `patch`: Rig files: export a preset, DSL chain or rig as `.json` or a `.bin` sector, inspect a rig
- `pedal`: The pedal over USB (`sonido_patch::client`): info, list, push, pull, erase, play, update

**Additional analyze subcommands:**
- `pac`: Phase-Amplitude Coupling analysis with surrogate testing
- `comodulogram`: Multi-frequency PAC matrix for coupling visualization
- `bandpass`: Extract frequency band with configurable filter order
- `hilbert`: Compute instantaneous phase and amplitude

### sonido-gui-core

Shared GUI infrastructure for both standalone and plugin UIs. Contains everything needed to render effect parameter panels in any egui host. The visual identity is the Sonido design system — a vector-display arcade cabinet (phosphor glow, one accent color per effect family); `docs/DESIGN_SYSTEM.md` is the spec and `theme.rs` holds the values.

**Key modules:**
- `param_bridge.rs`: `ParamBridge` trait — the abstraction boundary between GUI and audio thread. Includes `begin_set`/`end_set` gesture protocol for CLAP/VST3 undo grouping and automation recording.
- `cabinet.rs`: `Cabinet` — the single-effect plugin layout (marquee, screen with live scope, knob deck, footer) every effect plugin draws
- `effects_ui/`: `EffectPanel` trait + `create_panel`; every effect renders through one `GenericPanel` (all params as knobs) except the looper's custom panel
- `widgets/`: Knob, Fader, `bridged_*` helpers (bridge-aware knob/fader/combo with gesture protocol), LedDisplay (7-segment), LevelMeter, GainReductionMeter, BypassToggle, FootswitchToggle, MorphBar, Spectrum/Waveform displays, macro panel, param→macro menu, `vector_font` (the stroke display face)
- `widgets/glow.rs`: Phosphor bloom rendering primitives (`glow_circle`, `glow_line`, `glow_arc`, `glow_rect`, `neon_line`, `scanlines`)
- `crt.rs`: Handshake with a host's GPU CRT post-process pass (painter-drawn scanlines as fallback)
- `telemetry.rs`: `SignalTap` — lock-free audio→GUI peak meters and scope ring
- `theme.rs`: `SonidoTheme` struct — single source of truth for the design-system tokens (colors, sizing, glow config, scanline config). Installed into `egui::Context::data()`, retrieved via `SonidoTheme::get(ctx)`. Includes `reduced_fx` flag for WASM performance.

**Why a separate gui-core?** Plugin hosts (CLAP via clack) need effect UIs but not cpal audio streams or preset management. By isolating widgets, the Cabinet, and the ParamBridge trait into gui-core, `sonido-plugin` depends on gui-core alone and provides its own `ParamBridge` implementation (`PluginParamBridge`) backed by lock-free atomic host parameters.

### sonido-plugin

CLAP audio plugin adapter via the `clack` safe wrapper. Everything ships as ONE bundle: `examples/sonido.rs` exports `bundle.rs`'s `SonidoEntry`, whose CLAP factory lists all 36 effects (`bundle::CATALOG`, IDs `com.sonido.<effect-id-with-dashes>`) plus the graph player (`com.sonido.graph-player`). `scripts/bundle-clap.sh` packages it as `dist-clap/sonido.clap`; `scripts/bundle-vst3.sh` builds `dist-vst3/sonido.vst3`, a clap-wrapper v0.15.1 shim that loads `sonido.clap`. `make plugins` installs `~/.clap/sonido.clap`; `make plugins-vst3` also installs `~/.vst3/sonido.vst3`.

**Modules:**
- `bundle.rs`: `CATALOG`, the multi-plugin `SonidoFactory`, and `SonidoEntry`
- `lib.rs`: `sonido_effect_entry!` macro — still available to build a standalone single-effect `.clap`
- `audio.rs`: Real-time audio processor — parameter sync, gesture events, stereo block processing
- `gui.rs`: Plugin GUI — `PluginParamBridge` and `SonidoEditor` window lifecycle; every effect draws the gui-core `Cabinet`
- `main_thread.rs`: CLAP params, state save/load (JSON), GUI extension, audio ports
- `shared.rs`: `SonidoShared` — lock-free atomic parameter store shared across threads
- `graph_player.rs`: Graph-player plugin — runs an exported `Patch` (six macros, morph, input/master gain)
- `graph_editor.rs`: the graph player's editor — a RIG selector over the factory rigs (`sonido_rigs::factory_rigs`) and the rig library folder
- `bin/sonido-preview.rs`: Opens one plugin's GUI in a window without a DAW (`--snapshot` for PNGs)

**Shared infrastructure:**
- `egui_bridge/`: Custom baseview-to-egui bridge (event translation, frame loop, `egui_glow` painter, CRT post-process), replacing `egui-baseview`
- Lock-free parameter sync: `AtomicU32` (f32 bit-cast) per parameter, `AtomicU8` gesture flags
- Host notification: `host.request_process()` for GUI parameter changes
- Extensions: audio-ports, gui, latency, params, state, tail (reports the live effect's `tail_samples`), track-info

### sonido-gui

Standalone real-time audio effects processor and node-graph editor built on egui (eframe) + cpal. Builds natively and for wasm32 (the browser demo).

**Key modules:**
- `app.rs`: Main application state and UI layout
- `audio_processor.rs`: Audio-thread processor and stream construction (owns the `GraphEngine`)
- `audio_bridge.rs`: Lock-free communication between UI and audio thread (metering, transport and graph command channels)
- `atomic_param_bridge.rs`: `ParamBridge` implementation using AtomicU32 per parameter
- `graph_view.rs`: Visual node-graph editor (egui-snarl) with compile-to-engine
- `morph_state.rs`: A/B morph — one global position, per-effect A/B parameter poses
- `macro_autoassign.rs`: Auto-assigns the six performance macros from the chain
- `chain_manager.rs`: `GraphCommand` enum for GUI→audio thread topology mutations
- `session.rs`: Session save/load (graph topology, node parameters, editor state, the rig's name)
- `export.rs`: Project the editor into a `Patch` and export it (`.bin` sector, `.json`, CLAP graph-player preset) under the rig's name; the Export menu's Send to pedal goes over USB through `sonido_patch::client` natively, and over Web Serial (`web_serial.rs`) in the browser
- `pedal_panel.rs`: the PEDAL window: the controls guide (`sonido_rigs::guide`) and, natively, the connected pedal's state and rig list with Play, Erase and Send, on a worker thread
- `file_player.rs`: WAV file playback (native + wasm)
- `signal_generator.rs`: Built-in test/demo signals
- `info_view.rs`: Contextual info strip (selection, live values, shortcuts)
- `screenshot.rs`: `--screenshot out.png` one-shot capture

**Architecture:**
- UI thread: egui rendering
- Audio thread: Real-time processing via cpal
- Communication: `crossbeam-channel` (graph/transport commands, metering) + atomic params for lock-free updates
- Single-effect mode: `--effect <name>` launches simplified UI with one effect

## Data Flow

### File Processing (Stereo)

```
┌─────────────┐    ┌──────────────────┐    ┌─────────────────────┐    ┌──────────────────┐
│ WAV input   │───▶│ read_wav_stereo  │───▶│ GraphEngine         │───▶│ write_wav_stereo │
│ (mono/stereo)│    │  → StereoSamples │    │ process_file_stereo │    └──────────────────┘
└─────────────┘    └──────────────────┘    └─────────────────────┘           │
                                                                              ▼
                                                                        ┌───────────┐
                                                                        │ WAV output│
                                                                        │ (stereo)  │
                                                                        └───────────┘
```

### Real-time Processing (Stereo)

```
┌──────────┐    ┌───────────────┐    ┌──────────────────────────┐    ┌──────────┐
│ Audio    │───▶│ AudioStream   │───▶│ GraphEngine::            │───▶│ Audio    │
│ input L/R│    │ run_stereo()  │    │ process_block_stereo()   │    │ output   │
└──────────┘    │ (cpal stereo) │    │ (per block)              │    └──────────┘
                └───────────────┘    └──────────────────────────┘
```

### DAG Processing (ProcessingGraph)

The `ProcessingGraph` in `sonido-core/src/graph/` replaces linear chains with compiled
directed acyclic graph (DAG) routing. See ADR-025 for full design rationale.

```
Mutation thread (main/GUI):
  ┌──────────────────┐
  │ ProcessingGraph   │  add_effect(), connect(), remove_node()
  │   nodes: Vec<..> │
  │   edges: Vec<..> │───▶ compile()
  └──────────────────┘         │
                               ▼
                    ┌──────────────────────┐
                    │ CompiledSchedule     │  Immutable snapshot
                    │   steps: Vec<Step>   │  (via Arc)
                    │   pool: BufferPool   │
                    │   delay_lines: Vec   │
                    └──────────────────────┘
                               │
                 swap (Arc; old schedule
                  crossfaded out)
                               │
                               ▼
Audio callback:     ┌──────────────────────┐
                    │ for step in steps:   │  Zero-allocation loop
                    │   execute(step,      │
                    │     effects, bufs)   │
                    └──────────────────────┘
```

**Compilation pipeline**: Kahn's topological sort → buffer liveness analysis
(register allocation) → latency compensation → flat `Vec<ProcessStep>`.

**Buffer efficiency**: Liveness analysis reuses buffer slots. A 20-node linear
chain needs only 2 buffers (ping-pong). A diamond (split + 2 parallel + merge)
needs 3.

**Schedule swap**: When `compile()` produces a new schedule, old and new run
simultaneously during a ~5ms crossfade via `SmoothedParam`. No clicks.

**Example topologies**:
```
Linear:     Input → E1 → E2 → E3 → Output         (2 buffers)
Diamond:    Input → Split → [E1] → Merge → Output  (3 buffers)
                          → [E2] →
Fan-out:    Input → Split → [E1] → Merge → Output  (4 buffers)
                          → [E2] →
                          → [E3] →
```

`GraphEngine` in `sonido-core/src/graph/engine.rs` wraps `ProcessingGraph` with
high-level chain management and exposes a dual API (ADR-027):

- **NodeId-based** (`add_effect`, `remove_effect`, `reorder`) — for arbitrary DAG topologies.
- **Slot-indexed** (`add_effect_named`, `remove_at`, `reorder_slots`, `set_param_at`, `set_bypass_at`, `snapshot`) — for linear chains with registry IDs.

`GraphEngine` is the topology owner for the GUI, CLI and graph player: the GUI's
audio processor uses the slot-indexed API (and `new_dag()` for graphs built in the
node editor); the CLI uses `new_linear()` + `add_effect()` for chains and `new()`
for `--graph` DSL topologies; the graph-player plugin's `PatchPlayer`
(`sonido-patch`) owns one, and so does the pedal firmware's, which plays every rig
through the same `PatchPlayer`. The single-effect plugins own one effect directly.
`sonido-io` re-exports `GraphEngine` for backwards compatibility.

### CLI Stereo Output

`process` produces stereo output by default:
- Mono input: duplicates to stereo, processes, outputs 2-channel WAV
- Stereo input: processes stereo, outputs 2-channel WAV
- Use `--mono` flag to force mono (1-channel) output

### GUI Processing

```
┌─────────────────────────────────────────────────────────────────────┐
│                          UI Thread (egui)                           │
│  ┌──────────┐   ┌──────────────┐   ┌─────────────┐                 │
│  │ Knob/    │──▶│ AtomicParam- │──▶│ Session     │                 │
│  │ Controls │   │ Bridge       │   │ (save/load) │                 │
│  └──────────┘   └──────┬───────┘   └─────────────┘                 │
│                        │                                            │
│  ┌──────────┐          │                                            │
│  │ Meters   │◀─────────┼───────────────────────────────────────┐   │
│  └──────────┘          │                                       │   │
└────────────────────────┼───────────────────────────────────────┼───┘
                         │ atomic reads                          │
                         ▼                                       │
┌────────────────────────────────────────────────────────────────┼───┐
│                       Audio Thread                             │   │
│  ┌──────────┐   ┌────────────────┐   ┌──────────┐              │   │
│  │ cpal     │──▶│ GraphEngine    │──▶│ cpal     │──────────────┘   │
│  │ input    │   │ (process)      │   │ output   │  metering data   │
│  └──────────┘   └────────────────┘   └──────────┘                  │
└────────────────────────────────────────────────────────────────────┘
```

## Effect Trait

All effects implement the `Effect` trait with stereo-first design.

**Why stereo-first?** Most real-world audio is stereo. A mono-first API forces every stereo application to manually duplicate processing or wrap effects in stereo adapters. With stereo-first, the common case (stereo processing) requires no boilerplate, while mono processing derives automatically from the stereo path. The `is_true_stereo()` method lets hosts distinguish effects that have cross-channel interaction (reverb, ping-pong delay) from those that process channels independently (distortion, compressor). This metadata enables optimization: a host can skip the stereo path for dual-mono effects when rendering to mono.

**Why object-safe?** The `Effect` trait avoids associated types, const generics, and non-object-safe methods so that `Box<dyn Effect>` works for dynamic effect chains. This is critical for the GUI chain view (drag-and-drop reordering), preset loading (creating effects by name at runtime), and the CLI (parsing effect names from command-line arguments). Static dispatch via `.chain()` and generics remains available for maximum performance in known configurations.

```rust
pub trait Effect {
    // === STEREO (primary interface) ===

    /// Process a stereo frame. This is the primary method.
    fn process_stereo(&mut self, left: f32, right: f32) -> (f32, f32);

    /// Process stereo block (default: calls process_stereo per sample)
    fn process_block_stereo(&mut self, left_in: &[f32], right_in: &[f32],
                            left_out: &mut [f32], right_out: &mut [f32]);

    // === MONO (for convenience) ===

    /// Process a single mono sample (derives from stereo)
    fn process(&mut self, input: f32) -> f32;

    /// Process a block of mono samples
    fn process_block(&mut self, input: &[f32], output: &mut [f32]);

    // === METADATA ===

    /// True if effect has meaningful stereo processing (not dual-mono)
    fn is_true_stereo(&self) -> bool;

    /// Update sample rate (call when rate changes)
    fn set_sample_rate(&mut self, sample_rate: f32);

    /// Reset internal state (call when starting new audio)
    fn reset(&mut self);

    /// Report latency for delay compensation
    fn latency_samples(&self) -> usize;

    /// Samples of output after input stops (reverb/delay tails; CLAP tail extension)
    fn tail_samples(&self) -> usize;

    /// Receive host tempo/transport for tempo-synced params (default: no-op)
    fn set_tempo_context(&mut self, ctx: &TempoContext);
}
```

Most methods have default implementations (only `set_sample_rate`, `reset`, and one of
`process`/`process_stereo` must be written). There are also in-place
(`process_block_inplace`, `process_block_stereo_inplace`) and sidechain
(`process_block_stereo_with_sidechain`) block variants.

### True Stereo vs Dual-Mono

Effects fall into two categories:

**True Stereo** (`is_true_stereo() -> true` at default settings):
`reverb`, `plate_reverb`, `harmonic_habitat`, `chorus`, `flanger`, `phaser`,
`vibrato`, `tremolo`, `compressor`, `limiter`, `stage`, `stereo_widener`.
`delay` reports true stereo only while ping-pong mode is on.

**Dual-Mono** (`is_true_stereo() -> false`): the remaining effects, e.g.
`distortion`, `gate`, `wah`, `eq`, `tape`, `preamp`, `filter`, `bitcrusher`.

Dual-mono effects process each channel independently with the same algorithm.

## ParameterInfo Trait

All effects implement the `ParameterInfo` trait for runtime parameter discovery.

**Why index-based parameters?** Indexed parameters (rather than named string lookups) enable O(1) access in the audio thread and zero-allocation parameter changes. The `ParamDescriptor` provides metadata (name, unit, range, step) for UI generation without embedding strings in the hot path. The `short_name` field (max 8 characters) is for space-constrained displays: hardware/controller LCDs, compact knob labels, and name lookup (`find_param_by_name` matches `name` or `short_name`). This design is modeled after the VST3 parameter model, where parameters are identified by index for performance and by descriptor for presentation.

```rust
pub trait ParameterInfo {
    /// Returns the number of parameters this effect has.
    fn param_count(&self) -> usize;

    /// Returns descriptor for parameter at index.
    fn param_info(&self, index: usize) -> Option<ParamDescriptor>;

    /// Gets current value of parameter at index.
    fn get_param(&self, index: usize) -> f32;

    /// Sets value of parameter at index.
    fn set_param(&mut self, index: usize, value: f32);

    /// Returns stable ParamId for parameter at index (default impl).
    fn param_id(&self, index: usize) -> Option<ParamId>;

    /// Looks up parameter index by stable ParamId (default impl).
    fn param_index_by_id(&self, id: ParamId) -> Option<usize>;
}

pub struct ParamDescriptor {
    pub name: &'static str,       // "Delay Time"
    pub short_name: &'static str, // "Time" (max 8 chars for hardware)
    pub unit: ParamUnit,          // Milliseconds, Decibels, etc.
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub step: f32,                // Encoder increment
    pub id: ParamId,              // Stable numeric ID (for CLAP/VST3)
    pub string_id: &'static str, // Stable string ID (for presets)
    pub scale: ParamScale,        // Linear, Logarithmic, or Power(exp)
    pub flags: ParamFlags,        // AUTOMATABLE, STEPPED, HIDDEN, READ_ONLY, MODULATABLE, MACRO_EXCLUDE
    pub group: &'static str,     // Parameter group (e.g., "filter")
    pub modulation_id: Option<u32>, // CLAP modulation routing ID
    pub step_labels: Option<&'static [&'static str]>, // Labels for enum params
}
```

This enables:
- **Hardware mapping**: Map pedal knobs to parameters by descriptor range and scale
- **Plugin automation**: Expose parameters to DAW hosts
- **Preset systems**: Save/restore parameter state by index
- **Dynamic UIs**: Auto-generate parameter controls

## no_std Compatibility

`sonido-core`, `sonido-effects`, `sonido-registry`, `sonido-synth`, `sonido-platform` and `sonido-patch` support `no_std` (with `alloc`) for embedded use.

**Why `no_std`?** The target hardware (Electrosmith Daisy Seed running on ARM Cortex-M7) has no operating system and hard real-time constraints; the firmware supplies its own heap (on SDRAM) for construction-time allocation. Audio callbacks must complete within the buffer period (e.g., 64 samples / 48 kHz = 1.33 ms) with zero tolerance for missed deadlines. `no_std` builds are checked by `make test-nostd` and the manually dispatched `ci-manual.yml` workflow (`cargo test --no-default-features -p …`); they guarantee no code path can touch the filesystem or OS, and the design rule is that allocation happens at construction, never in the audio path.

```toml
[dependencies]
sonido-core = { version = "0.1", default-features = false }
sonido-effects = { version = "0.1", default-features = false }
```

Key design decisions for `no_std`:
- Use `libm` for math functions instead of `std::f32` (the `libm` crate provides software implementations of `sinf`, `expf`, `tanf`, etc. that work without libc)
- Pre-allocated delay lines (maximum sizes determined at construction time, no dynamic allocation in the audio path)
- Effect state stored in structs (no thread-locals, no statics, no `Rc`/`Arc` in kernels)
- `core::array::from_fn` for fixed-size array initialization (e.g., the reverb's 8 FDN delay lines and damping filters)

## Parameter Smoothing

Parameters use `SmoothedParam` to avoid zipper noise.

**Why per-sample smoothing?** When a user turns a knob, the parameter changes in discrete steps. Without smoothing, gain changes produce audible clicks ("zipper noise") and filter cutoff changes produce crackles. The `SmoothedParam` (`crates/sonido-core/src/param.rs`) implements a one-pole IIR lowpass filter on the parameter value itself: `current = current + coeff * (target - current)`. The coefficient is derived from the desired smoothing time: `coeff = 1 - exp(-1 / (time_ms/1000 * sample_rate))`. After one time constant, the value reaches 63% of the way to the target; after 5 time constants (~99.3%), it is effectively settled. It then snaps to the target exactly, once within one part in a million of the target's magnitude or once a step no longer moves the f32 value (a large value such as a 4 kHz cutoff would otherwise stall a few representable steps short and smooth forever), and a settled parameter's `advance()` returns at once.

**Why exponential rather than linear?** Exponential smoothing (`SmoothedParam`) is the default because it has constant computational cost (one multiply-add per sample) and naturally decelerates as it approaches the target, producing smooth-sounding transitions. Linear smoothing (`LinearSmoothedParam`) is also available for cases like crossfades where a predictable, constant-rate transition is needed.

```rust
// Exponential smoothing (default, natural-sounding)
let mut gain = SmoothedParam::with_config(1.0, 48000.0, 10.0); // 10ms smoothing

// In audio callback
gain.set_target(0.5);  // Will smooth to 0.5 over ~10ms
for sample in buffer {
    *sample *= gain.advance();  // Get smoothed value per sample
}
```

Typical smoothing times:
- Gain/pan: 5-10ms
- Filter cutoff: 20-50ms
- Gradual transitions: 100ms+

## Hardware Targets

### Electrosmith Daisy Seed / Hothouse

The `sonido-platform` crate provides abstractions for hardware deployment:

**Target Hardware:**
- Electrosmith Daisy Seed (ARM Cortex-M7 @ 480 MHz, or 400 MHz in the `Efficient` clock profile; 64 MB SDRAM)
- Cleveland Music Co. Hothouse (pedal enclosure with 6 knobs, 3 toggles, 2 footswitches)

**Control Layout:**
| Control | Type | Values |
|---------|------|--------|
| KNOB_1-6 | 10K pot (ADC) | 0.0 - 1.0 |
| TOGGLE_1-3 | 3-way switch | UP / MIDDLE / DOWN |
| FOOTSWITCH_1-2 | Momentary | Pressed / Released |
| LED_1-2 | Status LED | On / Off |

**The shipping firmware (`sonido_pedal.rs`)** is a rig player: it steps through
the jam, up to 15 rigs in its QSPI flash bank and the six factory rigs
(`sonido-rigs`), and plays each through `sonido_patch::PatchPlayer`. On a rig the
six knobs are its macros; in the jam they edit the selected slot, or with T2 in
the middle set the jam's levels and pick its effects. The footswitches mean the
same everywhere: an FS1 tap is on/off, an FS2 tap is tap tempo (T2 middle: a glide
A↔B), and either held 0.4 s changes rig.
```
TOGGLE_1: Jam slot being edited (1 / 2 / 3)
TOGGLE_2: Mode (A / Morph / B), for every rig
TOGGLE_3: Jam topology (linear 1→2→3 / parallel / fan 1→[2,3])
```
Each of the jam's 3 slots holds one of 8 effects (`JAM_EFFECT_IDS`: distortion,
bitcrusher, filter, ringmod, chorus, phaser, delay, reverb) or nothing.

See `docs/EMBEDDED.md` for detailed pin mappings and design patterns.

## Build Targets

```bash
# Desktop (default)
cargo build
cargo run -p sonido-cli -- process input.wav output.wav --effect reverb
cargo run -p sonido-gui

# Tests including no_std
cargo test
cargo test --no-default-features -p sonido-core
cargo test --no-default-features -p sonido-effects

# Embedded — sonido-daisy is excluded from the workspace; its
# .cargo/config.toml sets the thumbv7em-none-eabihf target
cd crates/sonido-daisy
cargo build --release --example sonido_pedal --features alloc,platform
```

## Parameter Threading Model

This section documents the parameter data flow for GUI and plugin wrapper
developers (CLAP, VST3).

### Ownership Model

`EffectWithParams` is `Send` but not `Sync` — the effect instance lives exclusively
on the audio thread. Cross-thread parameter access goes through a `ParamBridge`
(`AtomicParamBridge` in the standalone GUI, `PluginParamBridge` over
`SonidoShared` in the plugin), never direct shared references to the effect.

### Data Flow (standalone GUI)

```
GUI thread                          Audio thread (AudioProcessor::process_buffer)
    │                                    │
    ├─ ParamBridge::set(slot, idx, val)  │
    │   └─ clamp to descriptor range     │
    │   └─ AtomicU32::store(Release)     │
    │                                    │
    │                     drain GraphCommand channel
    │                     sync_bridge_to_graph():
    │                     │ for each slot, for each param:
    │                     │   val = AtomicU32::load(Acquire)
    │                     │   graph.set_param_at(slot, idx, val)
    │                     │   → Adapter's SmoothedParam::set_target()
    │                                    │
    │                     process block ─┤
    │                     │ SmoothedParam::advance() per sample
```

Parameters are pushed every buffer unconditionally: wait-free atomic loads are
cheaper than tracking per-slot dirty state across the borrow boundary.

### Memory Ordering

| Operation | Ordering | Rationale |
|-----------|----------|-----------|
| GUI write (param value, bypass) | `Release` | Publishes the value to the audio thread |
| Audio read (param value, bypass) | `Acquire` | Pairs with the GUI's `Release` store |
| Plugin gesture flags (`AtomicU8`, `fetch_or`) | `Release` | Read and cleared by the audio thread when it emits host events |

### Gesture Protocol

CLAP and VST3 hosts group parameter edits for undo/redo. The `ParamBridge` trait
exposes `begin_set` / `end_set` to bracket a gesture:

```rust
bridge.begin_set(slot, param);          // "user started dragging this knob"
bridge.set(slot, param, new_value);     // one or more value changes
bridge.end_set(slot, param);            // "user released the knob"
```

In the standalone GUI these are no-ops (the trait default; `AtomicParamBridge`
does not override them). In the plugin, `PluginParamBridge` sets per-parameter
gesture flags in `SonidoShared`; on the next process call `audio.rs` emits CLAP
`ParamGestureBeginEvent` → `ParamValueEvent` → `ParamGestureEndEvent`, which the
clap-wrapper VST3 shim maps to `beginEdit` / `performEdit` / `endEdit`.

### Structural Changes

Adding, removing, or replacing effects in the chain is NOT a parameter operation.
In the standalone GUI these mutations flow through an unbounded `crossbeam-channel`
of `GraphCommand` (`chain_manager.rs`):

- **GUI thread**: sends `GraphCommand::Add`, `Remove`, or `ReplaceTopology`
  (a whole pre-built `GraphEngine` from the node editor)
- **Audio thread**: drains the channel with `try_recv` at the top of each
  process callback and applies the mutations before processing

The send side never blocks the audio thread, and the receive side never waits.

### SmoothedParam Interaction

Both bridges transport raw parameter values (f32 → u32 bit-cast → AtomicU32).
Smoothing happens *inside* the effect after `set_param()`: the kernel's
`Adapter<K, SmoothedPolicy>` calls the parameter's `SmoothedParam::set_target()`,
and `advance()` produces the smoothed value per sample in the process loop
(`DirectPolicy` adapters, used only by single-kernel firmware examples, skip this). The bridge never sees smoothed
values.

### Plugin Wrapper Contract

Plugin wrappers (CLAP, VST3, nih-plug) must observe these rules:

1. **Call `set_param()` from the audio thread only.** Use an atomic
   `ParamBridge` implementation for cross-thread transport.
2. **Use `begin_set`/`end_set` for host gestures.** Omitting these breaks undo
   grouping in DAWs.
3. **Call `effect_format_value()`/`effect_parse_value()` from the main thread.**
   These allocate (`String`) and must not be called on the audio thread.
4. **Map `ParamId` to host parameter IDs 1:1.** The stable numeric ID is the
   canonical identity for automation recording and preset persistence.
5. **Respect `ParamFlags`**: `STEPPED` → integer step count, `HIDDEN` → omit from
   generic UI, `MODULATABLE` → register for CLAP modulation, `READ_ONLY` → no
   host automation.
