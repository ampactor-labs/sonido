# Sonido Roadmap

This roadmap covers Sonido's current state, the near-term hardening work, and the capabilities the project is working toward. Items are grouped by release milestone, and within each milestone they are roughly prioritized by impact relative to scope.

---

## Current State (v0.1)

Sonido is a production-grade DSP framework in Rust. The following capabilities are complete and in production.

### Crate Architecture

The workspace has 15 crates with clearly layered dependencies. 7 crates are fully `no_std` (core, effects, synth, registry, platform, patch, daisy), so the same implementation targets both desktop and embedded hardware.

| Tier | Crates |
|------|--------|
| no_std DSP core | sonido-core, sonido-effects, sonido-synth, sonido-registry, sonido-platform, sonido-patch |
| no_std firmware | sonido-daisy (Daisy Seed, optional build) |
| std library | sonido-config, sonido-analysis, sonido-io, sonido-graph-dsl |
| Applications | sonido-cli, sonido-gui-core, sonido-gui |
| Plugin adapter | sonido-plugin (optional build) |

### Effects Library

36 production effects using the kernel architecture (`DspKernel` + `KernelParams` + `Adapter`), grouped by registry category:

- **Distortion (6)**: Distortion (4 waveshaper modes, ADAA, envelope-driven dynamics), Tape Saturation (hysteresis + wow/flutter + head bump), Bitcrusher, Amp (dual gain stage + interactive tone stack + sag), Cabinet (direct-convolution IR, 3 factory responses), Glitch
- **Dynamics (6)**: Compressor (program-dependent release, sidechain-capable), Limiter (lookahead brickwall), Gate, Multiband Compressor (Linkwitz-Riley crossovers), De-esser, Transient Shaper
- **Modulation (9)**: Chorus, Flanger (through-zero), Phaser, Tremolo, Vibrato, Ring Modulator, Pitch Shift (granular), Time Stretch, Texture (granular pads)
- **Filter (4)**: Filter (multimode biquad), Wah, Parametric EQ (3-band), Shelving EQ
- **Time-Based (7)**: Delay (ping-pong, diffusion, tempo sync), Reverb (8-line Hadamard FDN with early reflections), Plate Reverb (Dattorro-inspired), Spring Reverb, Harmonic Habitat (pitch-aware modal tank), Drone, Looper
- **Utility (4)**: Preamp, Stage (signal conditioning), Stereo Widener, Tuner (YIN pitch detection)

DSP quality features active across all effects:
- Kernel architecture: pure DSP separated from parameter ownership (ADR-028)
- `Adapter<K, SmoothedPolicy>` manages per-parameter smoothing (5 to 50 ms, click-free) via `SmoothingStyle`
- Kernels callable directly on embedded targets without smoothing overhead
- Preset morphing via `KernelParams::lerp()` between any two parameter snapshots
- Topology-aware feedback compensation (no uncontrolled gain at high feedback)
- `soft_limit(1.0)` before output stage in saturation effects
- Oversampling wrapper (2x/4x/8x const-generic, usable on any effect)
- ADAA anti-aliasing (first-order, available in distortion hot path)

### Plugin Integration

All plugins ship in one bundle: `crates/sonido-plugin/examples/sonido.rs` exports `SonidoEntry` (`crates/sonido-plugin/src/bundle.rs`), whose CLAP factory lists all 36 effects (`bundle::CATALOG`, IDs `com.sonido.<effect-id>` with `_` written as `-`) plus the graph player (`com.sonido.graph-player`). Plugin features:
- Full `ParameterInfo` automation (stable numeric IDs, CLAP flags, text display)
- Per-gesture `begin_set/end_set` protocol for DAW automation recording
- Every effect plugin draws the design system's Cabinet (`sonido-gui-core`, see `docs/DESIGN_SYSTEM.md`)
- State save/restore, bypass, host notification on parameter change, tail length reported from the live effect
- `scripts/bundle-clap.sh` builds `dist-clap/sonido.clap`; `scripts/bundle-vst3.sh` builds a `sonido.vst3` shim (clap-wrapper) that loads it. `make plugins` installs `~/.clap/sonido.clap`; `make plugins-vst3` also installs `~/.vst3/sonido.vst3`

### GUI

The GUI is an egui node-graph editor that compiles to both native desktop and `wasm32-unknown-unknown` (the live browser demo):
- DAG rig editing: searchable add palette, wire splicing, auto-arrange by signal-flow depth
- Parameter-scale-aware knobs with real-time metering and per-node activity
- Six performance macros (K1-K6) with per-mapping range, curve, and invert
- A/B morph across the whole rig, curve-aware per parameter, with per-effect locks
- Versioned JSON sessions (topology, params, snapshots, macros, morph) with undo/redo
- Export: CLAP patch preset, portable JSON patch, Daisy pedal binary sector, or Send to pedal over USB (by the rig's name)
- Real-time audio via cpal (native) or WebAudio (wasm) over a lock-free atomic parameter bridge

### CLI

12 commands: `process`, `realtime`, `effects`, `devices`, `generate`, `analyze`, `info`, `presets`, `play`, `compare`, `patch`, `pedal`. `pedal` talks to the pedal over its USB rig link (info, list, push, pull, erase, play, update).

### Synthesis Engine

Full polyphonic synthesis pipeline:
- PolyBLEP anti-aliased oscillators (sine, saw, square, triangle)
- ADSR envelopes with configurable curve shapes
- Voice manager with configurable stealing policy
- Modulation matrix with source/destination routing

### Analysis Toolkit

`sonido-analysis` provides offline signal analysis tools (requires `std` for FFT):
- **Spectral**: FFT, STFT spectrogram, CQT, Welch PSD, coherence, spectral centroid
- **Distortion**: THD/THD+N, IMD, harmonic analysis
- **Transfer functions**: Frequency response measurement, impulse response capture, resonance detection
- **Biosignal**: Hilbert transform, filter banks (EEG bands), Phase-Amplitude Coupling (PAC/CFC)
- **DSP primitives**: LMS/NLMS adaptive filters, cross-correlation (direct + FFT), digital down-conversion, phase unwrapping (batch/quality-guided/streaming), rational resampling (polyphase)
- **Export**: FRD (REW-compatible), CSV, PGM

### Quality Infrastructure

- 75 golden regression baselines (MSE < 1e-6, SNR > 60 dB, spectral correlation > 0.9999)
- Property tests via proptest: bounded output, reset state clearing, parameter roundtrip
- 4 CI workflow files (ci.yml, ci-manual.yml, pages.yml, release.yml) with 11 jobs total
- Workspace lints: `unsafe_code = deny`, `clippy::pedantic = warn`
- Edition 2024, resolver 3

### Platform Targets

| Target | Status | Notes |
|--------|--------|-------|
| Linux x86_64 | Production | Primary development target, CI-gated |
| macOS | Builds | Release workflow builds on macOS runners (x64, arm64); not in per-push CI |
| Windows | Builds | Release workflow builds on a Windows runner; not in per-push CI |
| wasm32-unknown-unknown | Production | CI-gated build, deployed to GitHub Pages |
| Cortex-M7 (Daisy Seed) | Stable | no_std core verified, hardware integration manual |

---

## Foundation Hardening (v0.2)

Near-term work to harden what already exists before adding new capabilities.

### Structured Observability (tracing)

**Status:** Complete

The std crates use `tracing` in place of the `log` facade, and a `tracing-log` bridge captures eframe/egui internal log output. Instrumentation is light and sits at lifecycle boundaries: stream start/stop, effect chain mutations, preset load/save, and errors.

- `sonido-gui`, `sonido-cli`, `sonido-io`, `sonido-plugin` migrated to `tracing`
- Native: `tracing-subscriber` with `EnvFilter` (controlled via `RUST_LOG`)
- Wasm: `eframe::WebLogger` for browser console output
- No per-sample tracing, because tracing is the wrong tool for DSP profiling

### Coverage Reporting in CI

**Status:** Complete

A `cargo-llvm-cov` job in `ci-manual.yml` runs on demand via `gh workflow run ci-manual.yml -f job=coverage` and generates an `lcov.info` artifact. There is no threshold gate in v0.2: this release establishes the baseline before any minimum coverage is enforced.

Current known coverage state: DSP core ~100% (unit tested per module), CLI 45 unit tests, GUI-core 71 unit tests, GUI/IO lower (visual/hardware paths untested by unit tests).

### Plugin GUI Resize

**Status:** Complete

Effect plugins support host-negotiated window resize via an atomic `PendingResize` channel (a packed `AtomicU64`). The host calls `set_size()`, and the baseview handler applies it in `on_frame()` via `window.resize()`. Size constraints: 560×380 minimum, 1920×1080 maximum; the window opens at the Cabinet's preferred size for the effect's parameters. `can_resize()` returns true, `get_resize_hints()` reports free resize (no aspect ratio), and `adjust_size()` clamps to bounds.

### Multi-Effect CLAP Plugin

**Status:** Headless core and rig-browser editor complete

The graph player (`crates/sonido-plugin/src/graph_player.rs`, `com.sonido.graph-player`) runs a whole exported `Patch` as one plugin via `PatchPlayer`, the runtime the pedal firmware also plays rigs through. Its parameters are six macros (at noon by default, where every rig is voiced), the rig's T1 and T3, morph position/speed, input and master gain, and bypass. The deck shows the macros and toggles under the rig's own names and labels. Its state is the patch JSON the GUI exports ("Export as CLAP patch"). Its editor (`graph_editor.rs`) is a RIG cabinet with a rig selector over the factory rigs (`sonido_rigs::factory_rigs`, the same six the pedal ships) and the rig library folder the GUI exports to. The rig itself is edited in the GUI, not inside the plugin.

### Benchmark Baseline Tracking in CI

**Status:** Complete

Benchmarks run on demand via `gh workflow run ci-manual.yml -f job=bench` across all 4 crates (core, effects, synth, analysis). Criterion JSON is stored as CI artifacts (90-day retention) and cached via `actions/cache` so that runs can be compared. `critcmp` compares the cached baseline against the current run and produces a `bench-comparison.txt` artifact. There is no regression gate; the comparison is a report for people to review.

### Kernel Architecture Migration

**Status:** Complete

All 36 effects use the `DspKernel` + `KernelParams` pattern, which separates pure DSP from parameter ownership. `Adapter<K, SmoothedPolicy>` bridges kernels to `Effect + ParameterInfo` with per-parameter smoothing. The registry creates `Adapter<XxxKernel, SmoothedPolicy>` for all effects, so the change is transparent to all consumers.

Capabilities this makes possible:
- Kernels callable directly on embedded targets without smoothing overhead or heap allocation
- Preset morphing via `KernelParams::lerp()` between any two parameter snapshots
- `from_knobs()` maps normalized ADC readings to parameter ranges (embedded convenience)
- `from_normalized()` / `to_normalized()` for CLAP/MIDI host bridge
- `SmoothingStyle` per parameter: None, Fast (5ms), Standard (10ms), Slow (20ms), Interpolated (50ms)
- 340+ kernel-specific tests across all implementations

See ADR-028 in `docs/DESIGN_DECISIONS.md`.

---

## Near-Term Priorities (v0.3)

These priorities come from an analysis of DigiTech's design philosophy in the Tom Cram era: musicality over technical correctness, dynamic response to playing dynamics ("volume knob cleanup"), and platform-appropriate design. They address the difference between correct DSP and an instrument that players find inspiring. See [Signature Sounds](reference/signature-sounds.md) for the creative brainstorming behind these priorities.

### Dynamic Waveshaper Response

**What it is:** Input-level-aware distortion that changes character with playing dynamics. The input level changes the waveshaping transfer function itself, as well as the gain. Roll back the guitar volume, and the clipping mode softens from hard clip to gentle saturation. Play harder, and harmonic content shifts from even to odd harmonics. This is the "volume knob cleanup" behavior that great analog distortion circuits are known for.

**Why it matters:** Real tube amps and classic transistor circuits have a continuous, level-dependent response where the transfer function shape changes with input amplitude. It is the most often cited difference in quality between digital and analog distortion. In Tom Cram's DigiTech designs (Whammy, DOD reissues), this dynamic feel took priority over all other DSP qualities.

**Implementation approach:**
- Envelope follower modulates waveshaper blend coefficient in real time
- Low input → soft clip (smooth tanh), high input → harder asymmetric clip
- Configurable response curve and sensitivity
- Builds on existing `EnvelopeFollower` + distortion kernel

**Estimated scope:** ~200 to 400 LOC of changes to `DistortionKernel`, plus a new `DynamicWaveshaper` utility in sonido-core.

**Status:** Partially complete. The distortion kernel's envelope follower already modulates drive (`dynamics_pct`); the remaining gap is level-dependent blending of the transfer function itself.

---

### Pitch Shifting / Harmonizer

**What it is:** Real-time chromatic pitch shifting with harmonizer modes. It covers single-voice pitch shift (clean octave up/down, fixed intervals) and a multi-voice harmonizer (intelligent harmony based on key/scale).

**Why it matters:** Pitch shifting is the most commercially successful guitar DSP category (DigiTech Whammy alone sustained the company for decades). It is also a prerequisite for Space Station 2.0. Sonido ships a granular pitch shifter (`pitch_shift`) and time stretch today; the harmonizer and formant-preserving phase-vocoder path remain open.

**Implementation approach:**
- Phase vocoder in sonido-core (overlap-add FFT, ~800 LOC)
- Fixed interval modes: octave, 5th, 4th, 3rd, minor 3rd
- Whammy-style expression pedal sweep (continuous pitch bend)
- Chromatic quantization option (snap to nearest semitone)
- Polyphonic tracking as a stretch goal

**Dependencies:** FFT infrastructure exists in sonido-analysis but needs a bridge to the real-time path.

**Estimated scope:** ~1,200 to 1,800 LOC for the phase vocoder and the pitch shift effect kernel.

**Status:** Partially complete. `pitch_shift` (granular, Hann-windowed grain crossfade) and `time_stretch` ship; the phase vocoder, intelligent harmony, and expression-pedal sweep remain.

---

### Amp & Cabinet Simulation

**What it is:** Algorithmic amp modeling (preamp stages, tone stack, power amp compression) paired with cabinet impulse response (IR) convolution. It does not use neural capture.

**Why it matters:** Any guitar processor used without a physical amplifier needs amp and cabinet simulation. Musicians expect at least a few amp voicings (clean, crunch, high gain) with matching cabinet responses.

**Implementation approach:**
- Amp model: cascaded gain stages with tone stack (TMB) using existing SVF/biquad
- 3 to 5 amp voicings as parameter presets (clean Fender-ish, crunch Marshall-ish, high-gain Mesa-ish)
- Cabinet IR: uniform-partition convolution reverb (~1,500 LOC, also enables convolution reverb effect)
- Ship with 5 to 10 Creative Commons cabinet IRs
- IR loader for user-supplied WAV files

**Dependencies:** Partitioned convolution engine (new; the cabinet's direct convolution covers short IRs only). Amp modeling builds on existing filter primitives.

**Estimated scope:** ~1,500 LOC partitioned convolution engine + IR loader.

**Status:** Largely complete. `amp` (dual gain stage, interactive tone stack, sag) and `cabinet` (direct convolution, 3 factory IRs) ship. Remaining: user-supplied IR loading, uniform-partition convolution for long IRs, more voicings.

---

### MIDI CC Mapping & Expression Pedal

**What it is:** Runtime MIDI CC-to-parameter routing with learn mode, plus expression pedal support with configurable response curves.

**Why it matters:** External control lets a player perform live with a processor that would otherwise be a studio tool. Scene morphing, wah, and Whammy-style pitch bending all depend on expression pedal control. MIDI CC mapping lets Sonido work with DAWs, controllers, and pedalboard switchers.

**Implementation approach:**
- MIDI CC routing table: `HashMap<(u8, u8), (SlotIndex, ParamIndex)>` (channel, CC → effect param)
- Learn mode: next CC received maps to selected parameter
- Expression pedal: special CC (typically CC11 or CC100) with per-mapping response curves
- Response curves: linear, logarithmic, S-curve, custom LUT (`MacroMap` curves in sonido-core are a starting point)
- GUI: MIDI mapping overlay showing active assignments

**Dependencies:** MIDI input, which is new for both the GUI and the CLI (neither reads MIDI today; `sonido-daisy` has a USB-MIDI packet parser in `midi.rs`). Expression pedal is a special case of CC mapping.

**Estimated scope:** ~600 LOC MIDI routing + ~300 LOC GUI overlay.

**Status:** Not started. `sonido-platform` reserves a MIDI namespace in `ControlId` (`ControlId::midi`, 0x02XX), and `sonido-daisy` has `ExpressionInput` (`expression.rs`); neither is wired to a CC routing table.

---

### Curated Preset Library

**What it is:** A library of musically useful preset chains, each a complete signal path designed for a specific musical context (blues rhythm, shoegaze wash, funk clean, etc.).

**Why it matters:** DigiTech's most successful products (Whammy, Space Station, RP series) shipped with curated presets that demonstrated the hardware's capability and gave players immediate musical results. Sonido has a preset system but ships with only factory defaults per effect. A curated library would make Sonido an instrument as well as a DSP framework.

**Implementation approach:**
- 20 to 30 preset chains in `presets/curated/` as TOML files
- Categories: clean, crunch, high gain, ambient, experimental, utility
- Each preset includes a description and suggested use case
- GUI preset browser with category filtering
- Presets reference effects by registry ID + parameter values

**Dependencies:** None. The preset system already works.

**Estimated scope:** Mostly creative work (designing the presets), plus ~200 LOC for the category browser UI.

**Status:** Started. 5 chains ship in `presets/` (clean boost, guitar crunch, tape delay, subtle chorus, full chain) and 8 factory presets in `crates/sonido-config/src/factory_presets.rs`; six factory rigs (`crates/sonido-rigs`) ship on the pedal and in the graph player. The target of 20 to 30 chains and the category browser remain.

---

## Expanded Capabilities (v0.3+)

New capabilities that require new crates or significant architectural work.

### Wave Digital Filter Library (sonido-wdf)

**What it is:** A pure Rust implementation of Wave Digital Filter theory for component-level circuit modeling. WDF models electronic circuits as trees of port adaptors (resistors, capacitors, inductors, diode junctions, op-amp models) rather than as difference equations. Each component is a port element with a reflection function; the tree structure encodes circuit topology.

**Why it matters:** Algorithmic effects are approximations, while WDF models the actual circuit physics. A DOD 250 Overdrive built from WDF components will have the circuit's exact frequency-dependent clipping behavior, its asymmetric diode clipping characteristic and its tone-stack interaction, all derived from first principles rather than by curve fitting. Commercial VA synthesis (Antelope, UAD) and the WDF research community (CCRMA, DAFx) take this approach.

**Capabilities planned:**
- Series and parallel port adaptors
- R, C, L linear one-port elements
- Diode pair junction (1N914 model, Newton-Raphson solver)
- Ideal op-amp model (LM741 approximation)
- Voltage source and resistive source ports
- Tree assembly API: connect ports into a circuit tree
- Compile-time topology validation where possible

**First target circuit:** DOD 250 Overdrive (LM741 op-amp, 1N914 diode clipping pair, RC tone network). It is historically significant as the first mass-market distortion pedal, and its simple structure makes it a good first circuit for WDF.

**Dependencies:** None. It is pure math, `no_std` compatible, with no allocations in the processing path.

**Estimated scope:** ~500 to 800 LOC for the core WDF library and ~300 to 500 LOC per circuit model.

**Status:** Not started. No Rust WDF library exists, so this would be the first in the ecosystem.

**References:**
- Alfred Fettweis, "Wave Digital Filters: Theory and Practice" (1986)
- Kurt Werner et al., "Wave Digital Filter Adaptors for Arbitrary Topologies" (DAFx-2015)
- chowdsp_wdf (C++ reference implementation, CCRMA Stanford)
- Julius O. Smith, "Physical Audio Signal Processing" (WDF chapter)

---

### Neural Capture Inference (sonido-neural)

**What it is:** A pure Rust GRU/LSTM forward-pass engine for audio-rate neural amp modeling. It has no ML framework dependency: a hand-rolled matrix multiply covers the specific GRU cell dimensions used in neural amp capture. It loads trained model weights (from PyTorch or the NAM/AIDA-X training pipelines) and implements the `Effect` trait, so it can go into any slot of a Sonido effect chain.

**Why it matters:** Algorithmic preamp models cannot fully capture the interaction between vacuum tube nonlinearity, output transformer saturation, and speaker cabinet loading. Neural capture, which trains a GRU on input/output pairs from the real hardware, converges on these behaviors without modeling each component explicitly. Neural Amp Modeler (NAM), ToneX, and AIDA-X use this approach.

**Architecture:**
- GRU cell: hidden state `h`, input `x`, 3 gate activations (sigmoid × 2, tanh × 1)
- Hidden size 16 to 40 for guitar amp capture (tunable)
- At 48 kHz with hidden size 32: about 6,700 FLOPs per sample (three gates of 32 × 33 multiply-adds, plus the output layer), or about 0.32 GFLOPS, which is within Cortex-M7 capability
- Model format: JSON header (architecture) + binary weights (f32 LE), compatible with the NAM format or custom training

**Training pipeline:** External Python (PyTorch). The Rust crate is inference-only. Capture workflow: record DI → train on NAM/custom trainer → export weights → load in Sonido.

**Dependencies:** None for inference. Training is an external process.

**Estimated scope:** ~400 to 600 LOC for the inference engine and Effect wrapper.

**Status:** Not started.

**References:**
- Neural Amp Modeler (NAM) by Steven Atkinson: open source training pipeline
- AIDA-X: open source NAM-compatible format
- RTNeural: C++ neural inference for audio (demonstrates Cortex-M feasibility)
- Wright et al., "Real-Time Guitar Amplifier Emulation with Deep Learning" (2019)

---

### DAG Routing Engine

**Status:** Complete

A directed acyclic graph replaces the linear `Vec<usize>` effect chain. Nodes are effects; edges are audio connections. Topological sort determines processing order; intermediate buffers accumulate branch outputs at merge nodes.

**Architecture highlights:**
- **Two-object split**: `ProcessingGraph` (mutation thread) / `CompiledSchedule` (audio thread, immutable via `Arc`)
- **Kahn's topological sort** at graph mutation time: O(V+E), paid once per edit rather than per block
- **Buffer liveness analysis** (register allocation): 20-node linear chain = 2 buffers (ping-pong), diamond = 3
- **Latency compensation**: auto-inserted `CompensationDelay` ring buffers on shorter parallel paths
- **Click-free schedule swap**: a `SmoothedParam` crossfade (~5ms) on recompile, so the swap causes no audio glitches
- **Node types**: Input, Output, Effect (per-node bypass crossfade), Split (fan-out), Merge (fan-in sum)
- **`ProcessStep` instruction set**: WriteInput, ProcessEffect, ProcessSubGraph, SplitCopy, ClearBuffer, AccumulateBuffer, DelayCompensate, FeedbackDelay, SampleAndHold, ReadOutput
- **`linear()` convenience**: constructs Input→E1→...→En→Output chains
- **no_std with alloc**: embeddable on Daisy Seed

**Unblocked:** Multi-effect CLAP plugin (complete), synth-effects hybrid (Space Station 2.0), spectral parallel processing.

See ADR-025 in `docs/DESIGN_DECISIONS.md` for the full architectural rationale.

---

### Synth-Effects Hybrid (Space Station 2.0)

**What it is:** Guitar input fed into a pitch detector, which drives PolyBLEP oscillators in the synthesis engine. The synthesized signal runs through the effect chain independently from (or mixed with) the dry signal. This recreates the core concept of the DigiTech Space Station pedal (real-time pitch tracking that drives a polyphonic synth) with Sonido's full effect chain.

**Why it matters:** The Space Station is one of the most distinctive guitar effects ever made. It has been discontinued for decades, and nothing has replaced it. A clean-room implementation using modern DSP (PolyBLEP anti-aliasing, full modulation matrix, effect chain routing) would be musically useful, and it would demonstrate Sonido's synthesis and effects working together.

**Planned architecture:**
1. **Pitch detector**: YIN algorithm (~300 LOC) or autocorrelation-based. Input: audio buffer. Output: fundamental frequency estimate + confidence
2. **Pitch-to-note mapping**: frequency → MIDI note + fine tune cents
3. **Voice assignment**: detected notes trigger synth voices in `VoiceManager`
4. **Parallel routing**: dry guitar path + synth path (requires DAG routing)
5. **Effect chain on synth path**: reverb, chorus, etc. applied to synthesized signal

**Dependencies:** None remaining. Every prerequisite now ships: YIN pitch detection (in the tuner effect), DAG routing (parallel dry/synth paths), and the synthesis engine (`sonido-synth`).

**Estimated scope:** ~300 to 500 LOC for the pitch-to-voice bridge. The rest combines existing components.

**Status:** Not started as a product, but nothing blocks it.

**References:**
- Alain de Cheveigné, Hideki Kawahara, "YIN, a fundamental frequency estimator for speech and music" (JASA 2002)
- DigiTech Space Station XP-300: original commercial implementation (1996, discontinued)

---

### Spectral Processing Effects

**What it is:** FFT-domain effects using the existing `sonido-analysis` FFT infrastructure, moved into the real-time processing path. The phase vocoder is the core primitive, and spectral freeze, pitch shifting, and convolution reverb are the first target effects.

**Why it matters:** Time-domain processing cannot produce certain effects. Spectral freeze (holding a single FFT frame in sustain indefinitely), clean pitch shifting without formant distortion, and convolution reverb with real room impulse responses all require FFT-domain processing.

**Planned effects:**
- **Spectral freeze** (~500 LOC): hold an FFT frame on gate and blend the frozen spectrum with the live spectrum, for the classic "infinite reverb" / sustainer sound.
- **Convolution reverb** (~1,500 LOC): uniform-partition overlap-add, IR file loading, stereo support. It simulates real acoustic spaces.
- **Phase vocoder** (~800 LOC): magnitude/phase decomposition, frequency stretching, pitch shifting with formant preservation.
- **Spectral gate** (~300 LOC): suppress bins below an amplitude threshold, for noise reduction or drone removal.

**Architecture notes:**
- Real-time FFT processing uses overlapping windows (typical: 50% overlap, Hann window)
- `sonido-analysis` provides the FFT implementation, so no new FFT code is needed
- Buffer management (overlap-add) is the main new component
- Latency is inherent (one FFT window): 23ms at 48kHz for 1024-point FFT

**Dependencies:** FFT processing has to move from the offline analysis path to the real-time audio path. No new dependencies are needed.

**Status:** Not started.

---

### Pedal Firmware (Rig Player)

**Status:** Complete, with its host tools (`sonido_patch::client`, `sonido pedal`, the editor's Send to pedal), including assignable T1/T3 toggles in rigs.

The pedal firmware on Hothouse/Daisy Seed (`crates/sonido-daisy/examples/sonido_pedal.rs`) is a rig player: every sound is a `Patch` played by `PatchPlayer` with registry-built effects, as in the graph-player plugin. The rig list is the jam (three slots, each one of 8 effects or empty; T1 selects the slot, T3 the topology: linear 1→2→3, parallel split, fan 1→[2,3]), up to 15 rigs in the QSPI rig bank, and the six factory rigs (`crates/sonido-rigs`); a footswitch held 0.4 s steps through it (FS1 back, FS2 forward) and it powers on into the last rig played. The controls follow one rule (ADR-032): FS1 tap is on/off, FS2 tap is tap tempo (with T2 in the middle, a glide between A and B); the top row of knobs is levels, K3 always a volume, and the bottom row the sound. T2 is A / Morph / B for any rig; on a rig the knobs are its six macros, in the jam they edit the selected slot's A or B, and with T2 in the middle they are the jam's setup page (K1 input, K2 morph time, K3 master; K4 to K6 pick the effects for slots 1 to 3). The jam, its settings, the active rig and the tapped tempo are saved to a flash state sector; the USB rig link (`sonido_patch::link`) reads, writes and activates bank rigs while it plays, and restarts the pedal into its bootloader for a firmware update. LED 2 blinks the rig's position or a refusal's error code, shows the morph position as PWM with T2 in the middle and the output level otherwise. See `docs/EMBEDDED.md`.

---

### Performance Optimization

**Status:** Not started

CPU budget targets for the most expensive effects, informed by embedded real-time constraints.

**Targets:**
- Reverb: the 8-line FDN tank must stay under 40% of the Cortex-M7 CPU budget at 48 kHz/128 samples
- Tape: hysteresis + wow/flutter + head bump + HF rolloff within 35% of the M7 CPU budget
- Graph overhead: schedule compilation + buffer allocation overhead < 5% per block

**Approach:**
- Profile with `cargo bench` (CI-only, manual dispatch)
- SIMD-friendly inner loops (aligned buffers, batch processing)
- Minimize branch misprediction in per-sample hot paths
- Cache-line-aware data layout for delay buffers

---

### Embedded Hardening

**Status:** Groundwork only. `sonido-daisy` has `ExpressionInput` (calibration, deadzone, slew limiting; `expression.rs`) and a USB-MIDI packet parser (`midi.rs`), which the shipping pedal firmware does not use yet; it does use tap tempo (`tap_tempo.rs`) and knob hysteresis (`controls.rs`). Everything else below is still open.

Hardware interface features required for production pedal deployment on Hothouse/Daisy Seed.

**Control types:**
- `ControlType::Expression`: TRS expression pedal input, with range mapping (heel/toe calibration), a response curve (linear/logarithmic/S-curve/custom LUT), and polarity detection
- `ControlType::CvInput`: 0-5V unipolar / +/-5V bipolar CV input for Eurorack crossover, with voltage-to-parameter scaling and configurable range and offset

**MIDI routing:**
- MIDI CC-to-parameter routing via `ControlId::midi(0x02XX)` namespace
- Learn mode: hold footswitch + move CC → auto-assigns mapping
- Program Change → preset recall from toggle-defined bank
- Clock sync → TempoManager for delay/LFO tempo lock

**Pot calibration:**
- Real pots read 0.003 to 0.991, not 0.0 to 1.0. Per-pot min/max calibration is stored in flash
- Dead zone configuration: ignore movement below threshold (prevents jitter near boundaries)
- Hysteresis: done. `ControlBuffer` holds a knob's value until the reading leaves a 0.15%-of-travel band (`KNOB_HYSTERESIS`)

**Control curves:**
- Per-control response shaping: linear, logarithmic, reverse logarithmic, S-curve, custom lookup table
- Curves stored alongside calibration data in flash
- Applied in `ControlMapper` before parameter dispatch

**Parameter pages:**
- Footswitch long-press cycles through parameter pages
- LED blink pattern indicates current page (1 blink = page 1, 2 blinks = page 2, etc.)
- Each page maps the 6 knobs to different parameters, which extends 6-knob hardware to 18-36 parameters
- Since ADR-032 a footswitch hold changes rig, so pages would need another control; the jam's setup page (T2 in the middle) is the one page that exists

**Debounce utility:**
- Software debounce for toggles and footswitches (configurable, default 30ms)
- In `sonido-platform` as a reusable utility struct
- Supports both edge-triggered (footswitch) and level-triggered (toggle) modes

---

## Ecosystem (v0.4+)

Longer-term items that depend on v0.3 capabilities or require external coordination.

### Hybrid Neural + Algorithmic Architecture

Combine neural capture for amp/preamp stages (where algorithmic models fall short) with algorithmic effects for modulation, delay, and reverb (where algorithmic models are already excellent). The hybrid architecture treats neural and algorithmic effects as interchangeable `Effect` nodes in the DAG, which allows mixed signal chains such as:

```
DI → Neural Amp Capture → Algorithmic Reverb → Cabinet IR (convolution)
```

**Dependencies:** Neural capture crate (v0.3). DAG routing is complete.

**Status:** Not started. Once the neural capture crate exists, combining the two is simple.

---

### Embedded Neural on Cortex-M

Run neural amp capture on the Daisy Seed ($30, Cortex-M7 at 480MHz). RTNeural has demonstrated GRU hidden=24 at 48kHz on ARM Cortex-M7. The pure Rust version targets the same hardware via the existing no_std + embedded HAL path.

Together, this gives full neural capture + algorithmic effects on $30 hardware: a complete guitar processor that brings modern ML-based amp modeling to the Hothouse DIY pedal platform.

**Dependencies:** Neural capture crate (v0.3), Daisy Seed HAL integration (existing, in `sonido-daisy`).

**Status:** Not started.

---

### Per-Note Expression (CLAP + MPE)

CLAP note expression support for per-note modulation depth, pitch bend, pressure, and timbre per active note. MPE-aware synthesis via the `VoiceManager` gives each voice its own expression lane.

Plugin-side: extend the CLAP plugin adapter to handle `CLAP_EXT_NOTE_EXPRESSION` alongside the existing parameter and note handling.
Synth-side: route per-note expression values into the modulation matrix per-voice.

**Dependencies:** Plugin crate CLAP extension support (extend `sonido-plugin`).

**Status:** Not started. No external dependencies beyond existing crates.

---

### DOD Circuit Library (sonido-wdf circuits)

A collection of WDF circuit models for the DOD back catalog, built on top of the `sonido-wdf` library:

| Circuit | Key components | Musical character |
|---------|---------------|-------------------|
| DOD 250 Overdrive | LM741, 1N914 diode pair, RC tone | Smooth clipping, mid-forward |
| DOD FX25 Envelope Filter | LM13600 OTA, envelope follower | Classic synth-bass sweep |
| DOD FX56 American Metal | Multiple op-amps, asymmetric clipping | High-gain American voicing |
| DOD FX69 Grunge | Dual-stage clipping, mid-scoop EQ | Early 90s grunge character |
| DOD Rubberneck Delay | BBD emulation + analog warmth | Warm bucket-brigade delay |

Each circuit is ~300 to 500 LOC once the WDF library exists. Common building blocks (op-amp, diode, RC network) are shared across circuits.

**Dependencies:** WDF library (v0.3).

**Status:** Not started.

---

### Neural Model Morphing (Research)

Interpolation between two trained neural captures to blend tonal characteristics. Weight-space interpolation works for identical GRU architectures: `weights = (1-α) * model_A + α * model_B`. This produces a range of tones between the two captured amps.

A more powerful approach is a conditioned network: a single GRU takes a style parameter as an additional input and is trained on pairs of captures, so one model can produce any blend between them. This is an active research area, and no production solution exists anywhere in the industry.

**Why it's research-stage:** Weight interpolation produces perceptually smooth blends for some pairs but artifacts for others. The conditions under which interpolation fails are not fully characterized. The conditioned network approach requires new training infrastructure beyond what NAM/AIDA-X currently provide.

**Dependencies:** Neural capture crate (v0.3), ML research investment (external).

**Status:** Research stage. It is worth tracking but is not a near-term deliverable.

---

## Design Philosophy

These principles come from an analysis of DigiTech's R&D approach under Tom Cram and of the broader pedal industry:

**Musicality over technical correctness.** A waveshaper that responds to playing dynamics is worth more than one with lower THD. Optimize for how an effect feels to play through rather than for how it measures on a bench. Golden file tests ensure correctness; the creative work is making correct DSP that players also find inspiring.

**Dynamic response to playing.** The best analog circuits change character with input level. Their volume changes, and so do their tonal quality, harmonic content, and feel. Digital effects should use this too, with envelope followers that modulate waveshaper curves, input dynamics that drive effect parameters, and compression that follows the player's dynamics.

**Platform-appropriate design.** Some effects are better done in analog (simple fuzz, germanium character). Some are only possible in digital (pitch shifting, spectral freeze, topology morphing). Focus Sonido's effort on effects where digital has a genuine advantage, and model analog circuits (via WDF) where their character matters.

**Expression as a first-class feature.** An effect with expression pedal control is a different instrument than the same effect with fixed parameters. Scene morphing, Whammy-style pitch bending, and real-time topology changes are the capabilities that make listeners ask what they are hearing. The design of every new effect should include its expression mapping.

## Development Principles

These principles apply to all roadmap items:

**no_std by default for DSP.** New crates with DSP processing code target `no_std` unless std is unavoidable. Cortex-M is a primary target.

**Effect trait compatibility.** New processing nodes (WDF circuits, neural capture, spectral effects) all implement `Effect` + `ParameterInfo`. They slot into chains and DAG routing without special handling.

**Golden file coverage.** Every new effect gets golden regression baselines before merging, held to MSE < 1e-6 and SNR > 60 dB. After an intentional algorithm change, regenerate the baselines with `REGENERATE_GOLDEN=1`.

**One crate per capability domain.** `sonido-wdf`, `sonido-neural`, and future additions are separate crates with clearly scoped dependencies. The workspace keeps each domain's code and dependencies separate from the others.

**Benchmark before optimizing.** Add benchmarks before claiming performance properties. `cargo bench` runs in CI on manual dispatch (`ci-manual.yml`, `job=bench`). Claims about real-time viability on embedded targets require measured cycle counts.

**Stable parameter IDs.** Once an effect ships, parameter indices and `ParamId` values are public API. New parameters are added at the end, and existing ones are never reordered.
