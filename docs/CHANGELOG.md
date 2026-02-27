# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] — In Progress

### Added

#### Multi-Effect Chain CLAP Plugin (sonido-plugin)
- `ChainPlugin` — single CLAP plugin exposing a full effect chain with runtime add/remove/reorder
- Pre-allocated parameter space: 16 slots × 32 params = 512 CLAP parameter IDs, stable across sessions
- `ClapParamId` validated newtype for safe flat parameter indexing
- `ChainShared` — thread-safe shared state with `AtomicU32` params, `ArcSwap` slot metadata, `Mutex` command queue
- `ChainAudioProcessor` — owns effects directly, drains structural commands, diffs param cache for CLAP host events
- `ChainMainThread` — full CLAP params/state/GUI extension support, JSON state save/load
- `ChainParamBridge` — implements `ParamBridge` + `ChainMutator` for plugin GUI
- `ChainEditor` — egui-based chain strip with click-to-select, context menu, effect add popup
- `ChainMutator` trait in sonido-gui-core — abstracts chain reorder for standalone and plugin GUIs
- ADR-026 accepted (docs/DESIGN_DECISIONS.md)

#### DAG Routing Engine (sonido-core, sonido-io)
- `ProcessingGraph` — compiled DAG for audio effect routing, replaces linear chain limitations
- Node types: Input, Output, Effect (with per-node bypass crossfade), Split (fan-out), Merge (fan-in)
- Kahn's topological sort at mutation time, cycle detection at `connect()` time
- Buffer liveness analysis (register allocation): 20-node chain = 2 buffers, diamond = 3
- Latency compensation: auto-inserts `CompensationDelay` on shorter parallel paths
- Click-free schedule swap via `SmoothedParam` crossfade (~5ms) when graph topology changes
- `no_std` compatible (with `alloc`) — embeddable on Daisy Seed / Hothouse
- `GraphEngine` in sonido-io wraps `ProcessingGraph` with chain management (add/remove/reorder) and `from_chain()` for linear chains
- All consumers (CLI, GUI, plugin) migrated to `GraphEngine` / `ProcessingGraph` — `ProcessingEngine` and `ChainManager` removed
- `EffectWithParams` trait moved from sonido-registry to sonido-core; `ProcessingGraph` now stores `Box<dyn EffectWithParams + Send>` for runtime parameter access
- 46 graph-related tests (30 core + 16 engine unit + integration)
- ADR-025 accepted (docs/DESIGN_DECISIONS.md)

#### Structured Observability (tracing)
- Migrated std crates (`sonido-gui`, `sonido-cli`, `sonido-io`, `sonido-plugin`) from `log` facade to `tracing` 0.1
- Added `tracing-subscriber` with `EnvFilter` for runtime log-level control via `RUST_LOG`
- Added `tracing-log` bridge to capture eframe/egui internal `log::` output
- ~32 trace points at lifecycle and error boundaries (stream start/stop, chain mutations, preset load/save, errors)
- Native: `tracing-subscriber` fmt subscriber; Wasm: `eframe::WebLogger`
- No per-sample tracing — lifecycle/error boundaries only

#### Coverage Reporting in CI
- Added `cargo-llvm-cov` CI job generating `lcov.info` artifact per main-branch build
- Coverage artifact uploaded for download; no threshold gate (baseline establishment only)
- Known baseline: DSP core ~100% unit tested, CLI 43 tests, GUI-core 22 tests

## [Unreleased]

### Added — GUI Usability Improvements

#### sonido-gui
- **Preset loading**: Selecting a preset now performs a "hard reset" — stops audio, rebuilds the entire effect chain to match the preset (adding/removing effects as needed), applies all parameters, and restarts audio. This ensures exact preset reconstruction.
- **Input gain safety**: Input gain defaults to 0 dB (unity gain). Knob range -20 to +20 dB.
- **Level meter**: Updated to Ableton-style combined Peak/RMS display. RMS bar shows average level with gradient coloring; Peak bar overlays in brighter colors (green→yellow→red at >0.7→>0.95); thin white line indicates peak hold.
- **Drag-and-drop reordering**: Effects can now be dragged to any slot in a single motion. A translucent drop target indicator appears when hovering over a slot, and the effect moves directly to the drop position on mouse release. This replaces the previous incremental arrow-step reordering.
- **NaN guard**: Audio input samples are checked for finiteness; non-finite values are replaced with 0.0 to prevent signal corruption.

#### Buffer Management
- **Configurable buffer size**: Interactive buffer size selector in the status bar with presets: Low Latency (256), Very Low (512), Balanced (1024), Stable (2048), Maximum (4096). Buffer changes restart audio automatically with validation against hardware limits.
- **CPU usage monitoring**: CPU sparkline graph displays last 60 frames of audio thread CPU usage for trend visualization. Color-coded based on load: green (<80%), yellow (80-100%), red (>100%).

#### sonido-plugin
- Plugin GUI remains fixed-size (240×320 minimum) per CLAP specification. Resizable plugin windows may be considered in a future release.

#### Documentation
- `docs/GUI.md`: Updated with details on preset loading, input gain, meter visuals, and drag-drop behavior.
- `README.md`: Revised feature list for clarity and professionalism; removed speculative claims.

### Added — Pluggable Audio Backend Abstraction

#### sonido-io
- `AudioBackend` trait in `backend.rs`: Platform-agnostic interface for device enumeration and stream construction
- `CpalBackend` in `cpal_backend.rs`: Default backend wrapping cpal (ALSA, CoreAudio, WASAPI, WebAudio)
- `StreamHandle`: Type-erased RAII stream handle — dropping stops playback
- `BackendStreamConfig`: Backend-agnostic stream configuration (sample rate, buffer size, channels, device name)

#### Workspace Dependencies
- Centralized `egui`, `eframe`, `egui_extras`, `egui_glow` in workspace dependencies (was declared independently in 3 crates)
- Centralized `proptest` and `tempfile` dev-dependencies (3 different version strings unified)
- Fixed `sonido-ui` using path reference for `sonido-gl` instead of workspace
- Fixed `sonido-gui` wasm target using bare `hound` dep instead of workspace

### Added — Tempo Sync for Modulation Effects

#### sonido-core
- Shared division helpers in `tempo.rs`: `DIVISION_LABELS`, `index_to_division()`, `division_to_index()`
- Exported via `sonido_core::{DIVISION_LABELS, index_to_division, division_to_index}`

#### sonido-effects
- **Chorus**: Tempo sync via Sync (ParamId 707) and Division (ParamId 708) parameters — 9 total params
- **Flanger**: Tempo sync via Sync (ParamId 806) and Division (ParamId 807) parameters — 8 total params
- **Phaser**: Tempo sync via Sync (ParamId 908) and Division (ParamId 909) parameters — 10 total params
- **Tremolo**: Tempo sync via Sync (ParamId 1005) and Division (ParamId 1006) parameters — 7 total params
- All four effects implement `set_tempo_context()` for host BPM synchronization

#### sonido-gui-core
- Updated Chorus, Flanger, Phaser, Tremolo UIs with Sync/Division controls
- Updated Delay UI to expose all 10 parameters (was showing only 3)

#### sonido-plugin
- Added Stage effect plugin example (`sonido-stage.rs`, 19 total plugins)
- Fixed 12 pre-existing compilation errors in `egui_bridge`:
  - Enabled baseview `"opengl"` feature
  - Fixed `GlConfig` import path (`baseview::gl::GlConfig`)
  - Wrapped `make_current()` in unsafe block (baseview API change)
  - Renamed `paint_and_check_errors` → `paint_and_update_textures` (egui_glow 0.31)
  - Updated `WheelScrolled` from tuple to struct variant (baseview API change)
  - Downgraded `keyboard-types` from 0.7 to 0.6 (matching baseview's dependency)

#### sonido-registry
- Updated param counts: Chorus 7→9, Flanger 6→8, Phaser 8→10, Tremolo 5→7

### Changed — Custom egui Bridge

#### sonido-plugin
- Replaced external `egui-baseview` git dependency with a custom `egui_bridge` module (~350 lines)
- Plugin GUI rendering pipeline is now fully owned by sonido (baseview + egui_glow)
- Removed dependencies: `egui-baseview`, `copypasta`, `open`
- Added direct dependencies: `egui`, `egui_glow`, `glow`, `keyboard-types`
- One fewer external git dependency in the plugin build

#### Workspace
- Added `default-members` to workspace Cargo.toml — `cargo build`/`cargo test` skip `sonido-plugin` by default (its git dependencies may not be fetchable in all environments)

#### docs
- Updated `ARCHITECTURE.md` with sonido-plugin section, corrected effect count to 19
- Updated `CLAUDE.md` Key Files table with plugin crate entries

### Added — Effects Expansion + Plugin Stabilization

#### sonido-effects
- **Limiter**: Brickwall lookahead peak limiter with ceiling control (ParamId 1600-1604)
- **Bitcrusher**: Lo-fi bit depth and sample rate reduction with jitter (ParamId 1700-1704)
- **Ring Modulator**: Carrier oscillator × input with sine/triangle/square waveforms (ParamId 1800-1804)
- **Stage**: Signal conditioning and stereo utility — gain, phase, width, balance, bass mono, Haas delay (ParamId 1900-1911)
- Golden regression files for all 4 new effects (8 tests)

#### sonido-registry
- 19 registered effects (was 15): added limiter, bitcrusher, ringmod, stage

#### sonido-gui-core
- UI panels for all 4 new effects (LimiterPanel, BitcrusherPanel, RingModPanel, StagePanel)

#### sonido-plugin
- CLAP plugin binaries for all 3 new effects (18 total plugins)
- Clack dependency pinned to rev 57e89b3

#### Build & CI
- `make plugins` target: builds release plugin binaries, installs to ~/.clap/
- Plugin CI job: tests + release build + binary verification for all 18 plugins

### Fixed — DSP Safety (d7126ff)

#### sonido-core
- `soft_limit()` / `soft_limit_stereo()` added to `math.rs` — knee-based safety limiter using tanh compression (90% knee, ceiling-bounded output)

#### sonido-effects
- Applied `soft_limit(1.0)` before output level stage in 5 effects: `Preamp`, `Compressor`, `ParametricEq`, `LowPassFilter`, `Wah`
- These effects could exceed 0 dBFS under extreme parameter combinations; now hard-bounded at ceiling
- Golden regression files regenerated for all affected effects

### Changed — Core Hardening (8dbcda3)

#### `impl_params!` Macro (sonido-core)
- `impl_params!` declarative macro replaces ~600 lines of hand-written `ParameterInfo` impls across all effects
- Auto-clamping: `set_param()` clamps to descriptor min/max bounds automatically
- `ParamDescriptor::custom()` factory with neutral defaults for non-standard parameters
- `with_unit()` and `with_step()` const builders on `ParamDescriptor`

#### DSP Safety (sonido-core)
- `foldback()`: iterative implementation with 16-iteration bound (was recursive — stack overflow risk on embedded)
- `InterpolatedDelay`: configurable interpolation (None/Linear/Cubic), default Linear. Cubic uses Lagrange 3rd-order, matching `FixedDelayLine`
- `Lfo::value_at_phase()`: deduplicates waveform computation between `advance()` and `ModulationSource::mod_value()`

#### Modulation Cleanup (sonido-core)
- Removed `ModulationSource` impl from `EnvelopeFollower` — trait is for autonomous generators only; `EnvelopeFollower` requires audio input via `process()`

#### Effects Polish (sonido-effects)
- Reverb `latency_samples()` returns 0 — predelay is musical, not processing latency (per CLAP/VST3 spec)
- Distortion waveshape `set_param` clamps to valid range before casting

### Added — Foundation + GUI Architecture Restructure

#### Parameter System Hardening (sonido-core)
- `ParamScale` enum: Linear, Logarithmic, Power(exp) — non-linear parameter normalization for plugin hosts
- `ParamId(u32)`: Stable numeric parameter identifiers for CLAP/VST3 automation and preset persistence
- `ParamFlags`: Bitflags (AUTOMATABLE, STEPPED, HIDDEN, READ_ONLY) for plugin host communication
- `ParamDescriptor` new fields: `id`, `string_id`, `scale`, `flags`, `group`
- Builder methods: `.with_id()`, `.with_scale()`, `.with_flags()`, `.with_group()`
- `ParameterInfo` trait: `param_id()` and `param_index_by_id()` default methods
- Stable IDs assigned to all 15 effects (base IDs: Preamp=100, Distortion=200, ..., Reverb=1500)

#### ParamBridge Gesture Protocol (sonido-gui-core)
- `begin_set`/`end_set` methods on `ParamBridge` trait for CLAP/VST3 undo grouping
- Updated docs to reference clack for CLAP plugins (replacing nih-plug references)

#### GUI Restructure
- Effect UIs (15 panels + dispatcher) moved from sonido-gui to sonido-gui-core
- Widget dedup: knob, meter, theme canonical in gui-core, re-exported by gui
- ComboBox ID collision fixes (5 effects) for multi-instance safety
- `--effect <name>` standalone single-effect mode for sonido-gui

### Added — Architecture Tightening (Phases 1-5)

#### Phase 5: Integration & Property Tests (c46871b)
- 81 integration tests across sonido-core, sonido-synth, and sonido-analysis
- 7 property-based tests (proptest) for effect output finiteness and bounded gain
- Oversampler upgraded: polyphase Blackman-Harris windowed sinc upsampling (8 taps/phase), 48-tap Kaiser-windowed sinc FIR downsampling (beta=8.0), stopband attenuation >80 dB (was ~40 dB with linear interpolation + 16-tap FIR)

#### Phase 4: Block Processing (bb7710b)
- `process_block_stereo` overrides on 5 effects: Distortion, Compressor, Chorus, Delay, Reverb
- Distortion: monomorphized waveshaper dispatch — match once per block, not per sample
- Reverb: structural deduplication — shared comb/allpass processing eliminates stereo copy-paste

#### Phase 3: Audio Thread Optimization (392da0d)
- Bypass crossfade using `SmoothedParam::fast` (5ms) for click-free bypass toggling
- Dirty-flag parameter sync: `AtomicParamBridge` only pushes changed params per buffer
- Effect order caching with dirty flag and `clone_from` for heap reuse

#### Phase 2: GUI Architecture Cleanup (1caea8d)
- `AudioProcessor` struct extracted from monolithic audio output callback (~160 lines)
- `EffectType` enum deleted — single `EffectRegistry` is the only source of effect identity
- Input/output audio streams unified under shared `AudioProcessor`
- Transactional slot operations: `add_transactional`, `remove_transactional` on `ChainManager`
- Dynamic add/remove effects via `ChainCommand` channel

#### Phase 1: GUI Bug Fixes (a6e1f36)
- Repaint cap to prevent excessive redraws
- Toggle widget fix (gui-core is canonical source, gui re-exports)
- Selection cleared on effect removal
- Fixed `db_to_linear` import (use sonido-core, not local copy)

### Removed

- `docs/TECHNICAL_DEBT.md` — all items addressed by Phases 1-5

---

### Added

#### sonido-core
- `gain::feedback_wet_compensation()`: Exact `(1-fb)` comb-filter peak-gain cancellation for wet-signal compensation

### Changed

#### sonido-core
- `feedback_wet_compensation()` formula changed from `sqrt(1-fb)` to `(1-fb)` — exact compensation for single comb topologies (delay, flanger, phaser)

#### sonido-effects
- Delay, Flanger, Phaser: wet-signal compensation now uses exact `(1-fb)` formula, bringing all three within -1 dBFS peak ceiling
- Reverb: uses topology-specific `sqrt(1-fb)` inline (parallel comb averaging makes exact formula too aggressive)

### Added

#### sonido-core
- `gain.rs`: Universal output level helpers — `output_level_param()`, `set_output_level_db()`, `output_param_descriptor()`
- `one_pole.rs`: Reusable one-pole (6 dB/oct) lowpass filter for tone controls and HF rolloff
- `math.rs`: `wet_dry_mix()`, `wet_dry_mix_stereo()`, `mono_sum()` crossfade helpers
- `SmoothedParam` preset constructors: `fast()` (5 ms), `standard()` (10 ms), `slow()` (20 ms), `interpolated()` (50 ms)
- `ParamDescriptor` factory methods: `mix()`, `depth()`, `feedback()`, `time_ms()`, `gain_db()`
- `ParameterInfo::find_param_by_name()` default method for case-insensitive parameter lookup

#### sonido-effects
- Universal `output` parameter (±20 dB) on 11 effects: Chorus, Delay, Flanger, Phaser, Tremolo, MultiVibrato, Gate, Wah, LowPassFilter, ParametricEq, Reverb
- Default-parameter golden regression tests for all 15 effects

#### sonido-cli
- `info` command for WAV file metadata (format, channels, sample rate, duration, file size)
- `foldback_threshold` and `stereo_spread` parameters wired in CLI effect processing
- Auto-generated output filename when OUTPUT argument is omitted — derives name from input stem + effect specification
- `make dev-install` target for fast debug-build iteration (symlinks to `~/.local/bin`)

#### sonido-effects
- Compressor: exposed `knee` parameter (0-12 dB, default 6)
- Delay: exposed `ping_pong` parameter (0=off, 1=on, default off)
- MultiVibrato: exposed `mix` parameter (0-100%, default 100)
- TapeSaturation: exposed `output` (-12 to 12 dB), `hf_rolloff` (1000-20000 Hz), `bias` (-0.2 to 0.2) parameters
- CleanPreamp: exposed `output` (-20 to 20 dB), `headroom` (6-40 dB) parameters
- Reverb: exposed `stereo_width` (0-100%), `reverb_type` (0=room, 1=hall) parameters

### Fixed

#### sonido-effects
- **Wah gain staging**: Normalized SVF bandpass output by Q for unity peak gain at any resonance setting (was +12.5 dB at Q=5)
- **TapeSaturation gain staging**: Default output level set to -6 dB to compensate for drive gain (was 0 dB, producing +7.1 dB at defaults)
- **Chorus/Phaser/Flanger reset**: Added missing `snap_to_target()` calls on all SmoothedParams in `reset()`, preventing parameter smoothing artifacts after reset
- **Compressor/Gate**: Deleted local `db_to_linear`/`linear_to_db` functions, now use `sonido_core` imports
- **Factory preset**: Fixed `tape_warmth` preset referencing nonexistent `warmth` parameter (now `saturation`)
- Fixed stereo cross-contamination in LowPassFilter, ParametricEq, Wah, TapeSaturation, MultiVibrato — each channel now has independent filter state
- Aligned `new()` defaults with ParameterInfo for Distortion (drive=12, level=-6, tone=4000), Compressor (threshold=-18), Delay (time=300, feedback=0.4), TapeSaturation (drive=6), Reverb (room_size=0.5)

### Changed

#### sonido-effects
- All 15 effects migrated to shared DSP vocabulary: `SmoothedParam` presets, `ParamDescriptor` factories, `wet_dry_mix()` helpers
- Distortion and TapeSaturation tone filters replaced with `OnePole` struct from sonido-core
- TapeSaturation output default changed from 0 dB to -6 dB (compensates for drive gain)

#### sonido-cli
- Process command always outputs stereo WAV — mono input is duplicated to stereo before processing
- `--mono` flag now means "force mono output" (was "preserve mono input")

#### sonido-core
- `DcBlocker` high-pass filter for removing DC offset from audio signals
- `flush_denormal()` utility in `math.rs` for clearing denormalized floats

#### Build & QA
- Overnight QA script for full test/lint/doc/bench pipeline
- Audio demos and test audio files
- Walkthrough script and expanded demo coverage

### Removed

#### sonido-cli
- `tui` command and ratatui/crossterm dependencies — replaced by CLI for scriptable workflows and GUI for interactive editing

### Changed

#### Production Hardening
- **Denormal guards**: Added `flush_denormal()` calls in feedback paths of `CombFilter`, `AllpassFilter`, `Delay`, `Flanger`, and `Chorus` to prevent CPU spikes from denormalized float arithmetic
- **Distortion stereo fix**: Separated tone filter state per channel (`tone_filter_state_r`) for correct dual-mono behavior in stereo mode
- **DC blocker in reverb**: Added `DcBlocker` to reverb output path to eliminate DC offset accumulation in long reverb tails
- **Benchmark lint**: Suppressed `missing_docs` lint in benchmark harnesses where criterion macro-generated functions cannot carry doc comments

### Improved

- Missing rustdoc coverage for public items across workspace
- Code formatting applied consistently (`cargo fmt`)
- README updated with performance benchmarks and demo instructions

---

#### sonido-synth (NEW CRATE)

Full synthesis engine for building synthesizers with `no_std` support.

**Oscillators:**
- `Oscillator` with PolyBLEP anti-aliasing for alias-free waveforms
- `OscillatorWaveform`: Sine, Triangle, Saw, Square, Pulse (with width), Noise
- Frequency and phase modulation support

**Envelopes:**
- `AdsrEnvelope` with Attack, Decay, Sustain, Release stages
- Configurable curves and times in milliseconds
- Gate-based triggering

**Voice Management:**
- `Voice` combining oscillators, filter, and envelopes
- `VoiceManager` for polyphonic voice allocation
- `VoiceAllocationMode`: Oldest, Newest, Quietest, HighestNote, LowestNote stealing strategies

**Modulation:**
- `ModulationMatrix` for flexible modulation routing
- `ModulationRoute` with depth, curve, and inversion
- `ModSourceId` and `ModDestination` for source/dest identification
- `AudioModSource` to use audio input as modulation
- `AudioGate` to convert audio amplitude to gate signals

**Complete Synths:**
- `MonophonicSynth` with portamento/glide
- `PolyphonicSynth<N>` with configurable voice count
- Dual oscillators with detune, filter with envelope, amp envelope

#### sonido-config (NEW CRATE)

CLI-first configuration and preset management.

- `Preset` struct for loading/saving effect chain presets from TOML
- `EffectConfig` for individual effect configuration with parameters
- `EffectChain` runtime builder for effect chains
- Parameter validation against registry schemas
- Platform-specific paths (`user_presets_dir()`, `system_presets_dir()`)
- Factory presets bundled with the library
- `find_preset()` for searching preset directories

#### sonido-core

**Tempo System:**
- `TempoManager` for tempo tracking and musical timing
- `NoteDivision` enum: Whole, Half, Quarter, Eighth, Sixteenth, ThirtySecond
- Dotted notes: DottedHalf, DottedQuarter, DottedEighth
- Triplets: TripletQuarter, TripletEighth, TripletSixteenth
- `division_to_hz()`, `division_to_ms()`, `division_to_samples()` conversions
- `TransportState` (Playing, Stopped) with position tracking
- Beat/bar position and phase methods

**Modulation System:**
- `ModulationSource` trait for unified modulation interface
- Implemented for `Lfo` (bipolar) and `EnvelopeFollower` (unipolar)
- `mod_advance()`, `mod_value()`, `mod_reset()` methods
- `mod_advance_unipolar()` and `mod_advance_bipolar()` conversions
- `ModulationAmount` struct for depth and inversion control

#### sonido-analysis

**Cross-Frequency Coupling (CFC) Analysis:**
- `FilterBank` for multi-band frequency extraction with 4th-order Butterworth filters
- `FrequencyBand` specification with center frequency and bandwidth
- `eeg_bands` module with standard EEG bands (Delta, Theta, Alpha, Beta, Low Gamma, High Gamma)
- `HilbertTransform` using FFT method for analytic signal computation
- Instantaneous phase, amplitude, and frequency extraction
- Phase unwrapping and frequency estimation

**Phase-Amplitude Coupling:**
- `PacAnalyzer` for analyzing coupling between frequency bands
- `PacMethod`: Mean Vector Length (Canolty et al., 2006) and Kullback-Leibler (Tort et al., 2010)
- `PacResult` with modulation index, preferred phase, and phase-amplitude histogram
- 18-bin phase histogram (20 degrees per bin)
- Significance threshold checking

**Comodulogram:**
- `Comodulogram::compute()` for multi-frequency PAC analysis
- Configurable phase and amplitude frequency ranges with step sizes
- `peak_coupling()` to find strongest coupling pair
- `to_csv()` export for visualization
- `get_coupling()` for specific frequency pair lookup

#### sonido-cli

**New Analyze Subcommands:**
- `analyze pac` - Phase-Amplitude Coupling analysis
  - Configurable phase and amplitude bands
  - MVL and KL methods
  - Surrogate shuffling for significance testing with p-value
  - JSON output with phase histogram
- `analyze comodulogram` - Multi-frequency PAC matrix
  - Configurable frequency ranges and steps
  - Bandwidth ratio control
  - CSV output for heatmap visualization
- `analyze bandpass` - Frequency band extraction
  - Configurable low/high cutoff frequencies
  - Filter order selection (2, 4, 6)
  - WAV output
- `analyze hilbert` - Hilbert transform analysis
  - Instantaneous phase output (normalized -1 to 1)
  - Amplitude envelope output (normalized 0 to 1)
  - Optional pre-filtering with bandpass

**New Generate Subcommands (Synthesis):**
- `generate osc` - Oscillator waveform generation with PolyBLEP anti-aliasing
  - Waveforms: sine, triangle, saw, square, noise
  - Configurable pulse width for pulse waves
- `generate chord` - Polyphonic chord generation
  - MIDI note input (e.g., "60,64,67" for C major)
  - Configurable waveform, filter cutoff, attack/release
  - Uses `PolyphonicSynth` from sonido-synth
- `generate adsr` - ADSR envelope test tone generation
  - Configurable attack, decay, sustain, release
  - Visualize envelope shapes with audio output

**New Analyze Subcommands:**
- `analyze imd` - Intermodulation Distortion analysis
  - Two-tone test analysis
  - Second-order products (f1+f2, f2-f1)
  - Third-order products (2f1-f2, 2f2-f1)
  - JSON output with IMD percentages
- `analyze cqt` - Constant-Q Transform analysis
  - Logarithmic frequency resolution (equal bins per octave)
  - MIDI note and musical note name output
  - Optional chromagram (pitch class profile)
  - Configurable bins per octave (12=semitone, 24=quarter-tone)

**Other CLI Additions:**
- `presets` command for preset management

---

#### Build Infrastructure

- GitHub Actions CI workflow (`.github/workflows/ci.yml`)
  - Multi-platform testing (Linux, macOS, Windows)
  - `no_std` compatibility checks for core crates
  - Clippy linting and rustfmt checking
  - Cargo caching for faster builds

- GitHub Actions release workflow (`.github/workflows/release.yml`)
  - Triggered on version tags (`v*`)
  - Builds for Linux x64, macOS x64, macOS ARM64, Windows x64
  - Packages CLI and GUI binaries with factory presets
  - Creates GitHub releases with artifacts

#### CLI Audio Device UX

- Device selection by index: `sonido realtime --input 0 --output 0`
- Fuzzy device name matching: `sonido realtime --input "USB" --output "USB"`
- Device list now shows indices for easy reference
- Loopback device detection with `--include-virtual` flag
- Platform-specific guidance for virtual audio setup (VB-Audio, BlackHole, PulseAudio)

#### CLI Preset Management

- `sonido presets export-factory <DIR>` - Export all factory presets as TOML files

#### sonido-io

- `find_device_fuzzy()` - Find devices by partial name match
- `find_device_by_index()` - Find devices by numeric index

### Changed

- `sonido realtime` device options renamed from `--input-device`/`--output-device` to `-i/--input` and `-o/--output` (old names still work as aliases)

---

#### Phase 2: New Guitar Effects

Six new effects expanding the modulation, dynamics, and filter categories:

**Modulation Effects**
- `Tremolo`: Amplitude modulation with sine, triangle, square, and sample-hold waveforms (rate 0.5-20 Hz, depth 0-100%)
- `Flanger`: Classic flanger with modulated short delay (1-10ms range), feedback control, and LFO modulation
- `Phaser`: Multi-stage allpass phaser with 2-12 configurable stages, LFO modulation, and resonance control

**Dynamics Effects**
- `Gate`: Noise gate with threshold, attack, release, and hold time parameters for clean signal gating

**Filter Effects**
- `Wah`: Auto-wah (envelope follower) and manual wah modes using StateVariableFilter in bandpass mode with high Q for classic wah tone
- `ParametricEq`: 3-band parametric equalizer with independent frequency, gain, and Q controls per band using RBJ cookbook peaking EQ coefficients

#### sonido-core
- `peaking_eq_coefficients()`: RBJ Audio EQ Cookbook peaking EQ filter coefficients for parametric equalizers

#### sonido-registry
- Registered all 6 new effects with descriptors and factory functions
- Updated effect count to 15 total effects

#### sonido-cli
- CLI support for all 6 new effects with full parameter parsing
- Wah mode parsing (auto/manual)
- Tremolo waveform parsing (sine, triangle, square, samplehold)
- Short parameter aliases for EQ (lf/lg/lq, mf/mg/mq, hf/hg/hq)

#### Documentation
- Updated EFFECTS_REFERENCE.md with parameter tables for all 6 new effects
- Added example effect chains using new Phase 2 effects

---

#### sonido-registry (NEW CRATE)
- Central effect registry for discovering and instantiating effects by name
- `EffectRegistry` with factory pattern for runtime effect creation
- `EffectDescriptor` with metadata (id, name, description, category, param_count)
- `EffectCategory` enum for organizing effects (Dynamics, Distortion, Modulation, TimeBased, Filter, Utility)
- `EffectWithParams` helper trait for accessing ParameterInfo through boxed effects
- `no_std` compatible with optional `std` feature

#### sonido-core
- `ParameterInfo` trait for runtime parameter introspection
- `ParamDescriptor` struct with name, short_name, unit, min, max, default, step
- `ParamUnit` enum (Decibels, Hertz, Milliseconds, Percent, Ratio, None)
- Helper methods: `clamp()`, `normalize()`, `denormalize()` on ParamDescriptor

#### sonido-effects
- All 9 effects now implement `ParameterInfo` trait
- `CleanPreamp`: Added SmoothedParam for gain and output parameters
- `TapeSaturation`: Added SmoothedParam for drive, saturation, and output_gain
- `MultiVibrato`: Added SmoothedParam for depth_scale parameter

#### sonido-cli
- Shared `preset` module consolidating preset handling across commands

### Changed
- `CleanPreamp::new()` now requires sample_rate parameter
- CLI effects.rs updated to use new effect constructors

### Fixed
- no_std compatibility: replaced `f32::ceil()`, `f32::round()`, `f32::ln()` with libm functions in chorus, delay, reverb, and tape_saturation
- Test modules in gate.rs and wah.rs now correctly import Vec from alloc in no_std mode

### Previously Added
- Root README.md with project overview
- Documentation in `docs/` directory
- Preset files in `presets/` directory
- Makefile for common tasks
- Demo script at `scripts/demo.sh`
- LICENSE-MIT and LICENSE-APACHE files

#### sonido-gui
- Professional egui-based DSP effect processor GUI
- Real-time waveform visualization (input/output)
- Effect chain management with drag-and-drop
- Preset system with save/load functionality
- Audio device selection with hot-swap support
- CPU usage monitoring and performance metrics

**New Effect Panels (Phase 2):**
- Tremolo panel with rate, depth, and waveform selector (sine, triangle, square, S&H)
- Flanger panel with rate, depth, feedback, and mix controls
- Phaser panel with rate, depth, feedback, mix, and stage selector (2-12 stages)
- Gate panel with threshold, attack, release, and hold time
- Wah panel with frequency, resonance, sensitivity, and mode selector (auto/manual)
- Parametric EQ panel with 3-band controls (frequency, gain, Q per band)

#### sonido-effects
- `Reverb` effect: Freeverb-style algorithmic reverb with 8 parallel comb filters and 4 series allpass filters
- `CombFilter` primitive for building reverbs and delays
- `AllpassFilter` primitive for diffusion networks

#### sonido-core
- `CombFilter` delay-based comb filter
- `AllpassFilter` for reverb diffusion

### Changed
- Renamed `next()` to `advance()` in SmoothedParam, LinearSmoothedParam, and Lfo to avoid clippy warnings about iterator naming

### Fixed
- Removed unused imports and dead code warnings

## [0.1.0] - 2024-XX-XX

Initial release.

### Added

#### sonido-core
- `Effect` trait for all audio effects
- `SmoothedParam` for zipper-free parameter changes
- `LinearSmoothedParam` for linear interpolation
- `DelayLine` with fractional delay support
- `Biquad` filter for EQ and filtering
- `Lfo` with sine, triangle, saw, square, sample-and-hold waveforms
- `Oversampling` for 2x/4x oversampling
- Full `no_std` support

#### sonido-effects
- `Distortion` with soft clip, hard clip, foldback, asymmetric modes
- `Compressor` with soft knee and makeup gain
- `Chorus` dual-voice modulated delay
- `Delay` tape-style feedback delay
- `LowPassFilter` resonant 2-pole filter
- `MultiVibrato` 10-unit tape wow/flutter
- `TapeSaturation` with HF rolloff
- `CleanPreamp` gain stage
- Full `no_std` support

#### sonido-analysis
- `Fft` wrapper around rustfft
- `Window` functions (Hamming, Blackman, Hann, Rectangular)
- `TransferFunction` measurement
- `SineSweep` generation

#### sonido-io
- WAV file reading and writing via hound
- Real-time audio streaming via cpal
- `GraphEngine` for DAG-based effect chain processing

#### sonido-cli
- `process` command for file processing
- `realtime` command for live audio
- `generate` command for test signals
- `analyze` command for spectral analysis
- `compare` command for A/B comparison
- `devices` command for device listing
- `effects` command for effect listing
- TOML preset file support
