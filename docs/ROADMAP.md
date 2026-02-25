# Sonido Roadmap

This document describes the current state of the project, near-term hardening work, and the capability horizons being built toward. Items are grouped by release milestone. Within each milestone, items are roughly prioritized by impact vs. scope.

---

## Current State (v0.1)

Sonido is a production-grade DSP framework in Rust. The following capabilities are complete and in production.

### Crate Architecture

12-crate workspace with clear dependency discipline. 5 crates are fully `no_std` (core, effects, synth, registry, platform), targeting both desktop and embedded hardware from the same implementation.

| Tier | Crates |
|------|--------|
| no_std DSP core | sonido-core, sonido-effects, sonido-synth, sonido-registry, sonido-platform |
| std library | sonido-config, sonido-analysis, sonido-io |
| Applications | sonido-cli, sonido-gui-core, sonido-gui |
| Plugin adapter | sonido-plugin (optional build) |

### Effects Library

19 production effects, all implementing `Effect` + `ParameterInfo`:

- **Dynamics**: Compressor (11 params, sidechain-capable), Limiter, Gate
- **Gain/Saturation**: Preamp, Distortion (4 waveshaper modes, ADAA), Tape Saturation (10 params, hysteresis + wow/flutter + head bump)
- **Modulation**: Chorus, Flanger, Phaser, Tremolo, MultiVibrato
- **Time**: Delay (ping-pong, diffusion, tempo sync), Reverb (Freeverb topology, stereo tanks)
- **Filter**: Filter (SVF-based), Wah, Parametric EQ (3-band)
- **Special**: Bitcrusher, Ring Modulator, Stage (4-in-1 processor)

Key DSP quality features active across all effects:
- SmoothedParam on every automatable parameter (5–50ms, click-free)
- Topology-aware feedback compensation (no uncontrolled gain at high feedback)
- `soft_limit(1.0)` before output stage in saturation effects
- Oversampling wrapper (2x/4x/8x const-generic, usable on any effect)
- ADAA anti-aliasing (first-order, available in distortion hot path)

### Plugin Integration

19 CLAP plugins (one per effect) via the `sonido_effect_entry!` macro. Plugin features:
- Full `ParameterInfo` automation (stable numeric IDs, CLAP flags, text display)
- Per-gesture `begin_set/end_set` protocol for DAW automation recording
- egui GUI shared with standalone GUI via `sonido-gui-core`
- State save/restore, bypass, host notification on parameter change
- Built as cargo examples, installed to `~/.clap/` via `make plugins`

### GUI

egui-based GUI compiling to both native desktop and `wasm32-unknown-unknown`:
- Dynamic effect chain: add, remove, reorder effects at runtime
- Per-effect parameter panels with knobs, toggles, and meters
- Preset save/load via `sonido-config`
- Global bypass with 5ms click-free crossfade
- WAV file playback through effect chain
- CPU meter with color-coded load indicator
- Real-time audio via cpal (native) or WebAudio (wasm)

### CLI

10 commands: `process`, `realtime`, `effects`, `devices`, `generate`, `analyze`, `info`, `presets`, `play`, `compare`.

### Synthesis Engine

Full polyphonic synthesis pipeline:
- PolyBLEP anti-aliased oscillators (sine, saw, square, triangle)
- ADSR envelopes with configurable curve shapes
- Voice manager with configurable stealing policy
- Modulation matrix with source/destination routing

### Quality Infrastructure

- 48 golden regression baselines (WAV files, MSE < 1e-6, SNR > 60 dB, spectral correlation > 0.9999)
- Property tests via proptest: bounded output, reset state clearing, parameter roundtrip
- 7 CI workflows: test, clippy, doc, no_std, fmt, plugin, pages
- Workspace lints: `unsafe_code = deny`, `clippy::pedantic = warn`
- Edition 2024, resolver 3

### Platform Targets

| Target | Status | Notes |
|--------|--------|-------|
| Linux x86_64 | Production | Primary development target |
| macOS | Production | CI-verified |
| Windows | Production | CI-verified |
| wasm32-unknown-unknown | Production | Deployed to GitHub Pages |
| Cortex-M7 (Daisy Seed) | Stable | no_std core verified, hardware integration manual |

---

## Foundation Hardening (v0.2)

Near-term work to harden the operational foundation before expanding capability scope.

### Structured Observability (tracing)

**Status:** Complete

Replace `log` facade with `tracing` throughout std crates. `tracing-log` bridge captures eframe/egui internal log output. Light instrumentation at lifecycle boundaries: stream start/stop, effect chain mutations, preset load/save, errors.

- `sonido-gui`, `sonido-cli`, `sonido-io`, `sonido-plugin` migrated to `tracing`
- Native: `tracing-subscriber` with `EnvFilter` (controlled via `RUST_LOG`)
- Wasm: `tracing-wasm` for browser console output
- No per-sample tracing — wrong tool for DSP profiling

### Coverage Reporting in CI

**Status:** Complete

Add `cargo-llvm-cov` job to CI. Generates `lcov.info` artifact per main-branch build. No threshold gate in v0.2 — establishes the baseline before enforcing floors.

Current known coverage state: DSP core ~100% (unit tested per module), CLI 43 unit tests, GUI-core 22 unit tests, GUI/IO lower (visual/hardware paths untested by unit tests).

### Plugin GUI Resize

**Status:** Planned

CLAP GUI extension supports host-negotiated resize via `clap_plugin_gui` callbacks. Current fixed 480×380 will be replaced with resizable panels, minimum size constraints, and host-driven resize events.

Requires: understanding `CLAP_EXT_GUI` `can_resize`, `get_resize_hints`, `adjust_size`, `set_size` callbacks. All 19 plugins share the GUI bridge — one implementation, 19 beneficiaries.

### Multi-Effect CLAP Plugin

**Status:** Planned

A single CLAP plugin exposing the full effect chain. Requires the DAG routing engine (see v0.3) for proper signal routing semantics. Blocked on DAG work.

### Benchmark Baseline Tracking in CI

**Status:** In progress

Benchmarks run on-demand via `gh workflow run ci-manual.yml -f job=bench`.
Criterion JSON and bencher-format text stored as CI artifacts (90-day retention).
Baseline captures: per-effect block processing at 64/128/256/512/1024 samples,
chain processing at 4 effects, oversampled distortion. Next step: `critcmp`
comparison between baselines.

---

## New Capabilities (v0.3+)

Capability expansions that require new crates or significant architectural additions.

### Wave Digital Filter Library (sonido-wdf)

**What it is:** A pure Rust implementation of Wave Digital Filter theory for component-level circuit modeling. WDF models electronic circuits as trees of port adaptors — resistors, capacitors, inductors, diode junctions, op-amp models — rather than as difference equations. Each component is a port element with a reflection function; the tree structure encodes circuit topology.

**Why it matters:** Algorithmic effects are approximations. WDF models the actual circuit physics. A DOD 250 Overdrive built from WDF components will exhibit the exact frequency-dependent clipping behavior, the asymmetric diode clipping characteristic, the tone-stack interaction — all from first principles, not curve-fitting. This is the approach taken by commercial VA synthesis (Antelope, UAD) and the WDF research community (CCRMA, DAFx).

**Capabilities planned:**
- Series and parallel port adaptors
- R, C, L linear one-port elements
- Diode pair junction (1N914 model, Newton-Raphson solver)
- Ideal op-amp model (LM741 approximation)
- Voltage source and resistive source ports
- Tree assembly API: connect ports into a circuit tree
- Compile-time topology validation where possible

**First target circuit:** DOD 250 Overdrive — LM741 op-amp, 1N914 diode clipping pair, RC tone network. Historically significant (first mass-market distortion pedal), structurally straightforward for WDF entry point.

**Dependencies:** None. Pure math, `no_std` compatible. No allocations in the processing path.

**Estimated scope:** ~500–800 LOC for the core WDF library, ~300–500 LOC per circuit model.

**Status:** Not started. No existing Rust WDF library exists — this would be the first in the ecosystem.

**References:**
- Alfred Fettweis, "Wave Digital Filters: Theory and Practice" (1986)
- Kurt Werner et al., "Wave Digital Filter Adaptors for Arbitrary Topologies" (DAFx-2015)
- chowdsp_wdf (C++ reference implementation, CCRMA Stanford)
- Julius O. Smith, "Physical Audio Signal Processing" (WDF chapter)

---

### Neural Capture Inference (sonido-neural)

**What it is:** A pure Rust GRU/LSTM forward-pass engine for audio-rate neural amp modeling. No ML framework dependency — hand-rolled matrix multiply for the specific GRU cell dimensions used in neural amp capture. Loads trained model weights (from PyTorch or the NAM/AIDA-X training pipelines) and implements the `Effect` trait, making it a drop-in slot in any Sonido effect chain.

**Why it matters:** Algorithmic preamp models cannot fully capture the interaction between vacuum tube nonlinearity, output transformer saturation, and speaker cabinet loading. Neural capture — training a GRU on input/output pairs from the real hardware — converges on these behaviors without needing to model each component explicitly. This is the approach used by Neural Amp Modeler (NAM), ToneX, and AIDA-X.

**Architecture:**
- GRU cell: hidden state `h`, input `x`, 3 gate activations (sigmoid × 2, tanh × 1)
- Hidden size 16–40 for guitar amp capture (tunable)
- At 48 kHz with hidden size 32: ~1,500 FLOPs/sample → ~1.15 GFLOPS/sec — within Cortex-M7 capability
- Model format: JSON header (architecture) + binary weights (f32 LE) — compatible with NAM format or custom training

**Training pipeline:** External Python (PyTorch). The Rust crate is inference-only. Capture workflow: record DI → train on NAM/custom trainer → export weights → load in Sonido.

**Dependencies:** None for inference. Training is an external process.

**Estimated scope:** ~400–600 LOC inference engine + Effect wrapper.

**Status:** Not started.

**References:**
- Neural Amp Modeler (NAM) — Steven Atkinson, open source training pipeline
- AIDA-X — open source NAM-compatible format
- RTNeural — C++ neural inference for audio (demonstrates Cortex-M feasibility)
- "Real-Time Guitar Amplifier Emulation with Deep Learning" — Wright et al. (2019)

---

### DAG Routing Engine

**What it is:** A directed acyclic graph (DAG) replacing the current linear `Vec<usize>` effect chain. Nodes are effects; edges are audio connections. Topological sort determines processing order; intermediate buffers accumulate branch outputs at merge nodes.

**Why it matters:** Linear chains cannot express parallel signal paths, sidechains, multiband processing, or wet/dry blend at arbitrary points. Helix, Axe-Fx, and modern modular plugins all route audio as graphs, not chains. Implementing the DAG engine unlocks the full range of these routing patterns without redesigning effects or the `Effect` trait.

**Planned node types:**
- **Effect node**: wraps any `Box<dyn Effect + Send>` — the existing type
- **Split node**: routes one input to N outputs (fan-out, no mixing)
- **Merge node**: sums N inputs to one output (additive mix)
- **Mixer node**: weighted sum of N inputs
- **Bypass node**: passthrough with enable/disable

**Key technical decisions:**
- **Topological sort at graph mutation time** (not per audio buffer) — O(V+E) cost paid once at edit time, not per block
- **Intermediate buffer pool** — pre-allocated f32 buffers assigned to edges, reused across blocks
- **Cycle detection** — reject graphs with cycles at insertion time, not audio time
- **Elastic resource tracking** — per-node estimated CPU cost, total graph budget for load-aware routing

**Dependencies:** None. Hand-rolled is cleaner than adapting `dasp_graph` to the `Effect` trait semantics.

**Estimated scope:** ~1,500–2,500 LOC for graph engine + buffer pool + routing nodes. Larger than any single effect, smaller than the full effects library.

**Status:** Not started. This is the most impactful architectural item in the roadmap. Multi-effect CLAP plugin, synth-effects hybrid, and spectral parallel processing all depend on it.

---

### Synth-Effects Hybrid (Space Station 2.0)

**What it is:** Guitar input fed into a pitch detector, which drives PolyBLEP oscillators in the synthesis engine. The synthesized signal runs through the effect chain independently from (or mixed with) the dry signal. This recreates the core concept of the DigiTech Space Station pedal — real-time pitch tracking driving a polyphonic synth — using Sonido's full effect chain.

**Why it matters:** The Space Station is one of the most distinctive guitar effects ever made, and it has been discontinued and irreplaceable for decades. A clean-room implementation using modern DSP (PolyBLEP anti-aliasing, full modulation matrix, effect chain routing) would be both musically useful and a compelling demonstration of Sonido's cross-domain capability.

**Planned architecture:**
1. **Pitch detector** — YIN algorithm (~300 LOC) or autocorrelation-based. Input: audio buffer. Output: fundamental frequency estimate + confidence
2. **Pitch-to-note mapping** — frequency → MIDI note + fine tune cents
3. **Voice assignment** — detected notes trigger synth voices in `VoiceManager`
4. **Parallel routing** — dry guitar path + synth path (requires DAG routing)
5. **Effect chain on synth path** — reverb, chorus, etc. applied to synthesized signal

**Dependencies:** Pitch detector (new), DAG routing (for parallel dry/synth paths). The synthesis engine (`sonido-synth`) is already complete.

**Estimated scope:** ~500–800 LOC for pitch detector + bridge to synth engine. The rest is composition of existing components.

**Status:** Not started. Depends on DAG routing.

**References:**
- Alain de Cheveigné, Hideki Kawahara, "YIN, a fundamental frequency estimator for speech and music" (JASA 2002)
- DigiTech Space Station XP-300 — original commercial implementation (1996, discontinued)

---

### Spectral Processing Effects

**What it is:** FFT-domain effects using the existing `sonido-analysis` FFT infrastructure, moved into the real-time processing path. Phase vocoder as the core primitive; spectral freeze, pitch shifting, and convolution reverb as initial effect targets.

**Why it matters:** Time-domain processing cannot produce certain effects. Spectral freeze (holding a single FFT frame in sustain indefinitely), clean pitch shifting without formant distortion, and convolution reverb with real room impulse responses all require FFT-domain processing.

**Planned effects:**
- **Spectral freeze** (~500 LOC) — hold an FFT frame on gate, blend frozen spectrum with live spectrum. Classic "infinite reverb" / sustainer sound.
- **Convolution reverb** (~1,500 LOC) — uniform-partition overlap-add, IR file loading, stereo support. True acoustic space simulation.
- **Phase vocoder** (~800 LOC) — magnitude/phase decomposition, frequency stretching, pitch shifting with formant preservation.
- **Spectral gate** (~300 LOC) — suppress bins below amplitude threshold. Noise reduction / drone removal.

**Architecture notes:**
- Real-time FFT processing uses overlapping windows (typical: 50% overlap, Hann window)
- `sonido-analysis` provides the FFT implementation — no new FFT code
- Buffer management (overlap-add) is the main new component
- Latency is inherent (one FFT window): 23ms at 48kHz for 1024-point FFT

**Dependencies:** Move FFT processing from offline-analysis path to real-time audio path. No new dependencies needed.

**Status:** Not started.

---

## Ecosystem (v0.4+)

Longer-horizon items that depend on v0.3 capabilities or require external coordination.

### Hybrid Neural + Algorithmic Architecture

Combine neural capture for amp/preamp stages (where algorithmic models fall short) with algorithmic effects for modulation, delay, and reverb (where algorithmic models are already excellent). The hybrid architecture treats neural and algorithmic effects as interchangeable `Effect` nodes in the DAG, enabling mixed signal chains like:

```
DI → Neural Amp Capture → Algorithmic Reverb → Cabinet IR (convolution)
```

**Dependencies:** Neural capture crate (v0.3), DAG routing (v0.3).

**Status:** Not started. Trivially composable once the v0.3 primitives exist.

---

### Embedded Neural on Cortex-M

Run neural amp capture on the Daisy Seed ($30, Cortex-M7 at 480MHz). RTNeural has demonstrated GRU hidden=24 at 48kHz on ARM Cortex-M7. The pure Rust version targets the same hardware via the existing no_std + embedded HAL path.

Combined capability: full neural capture + algorithmic effects on $30 hardware. This is the end-to-end guitar processor that closes the loop between the Hothouse DIY pedal platform and modern ML-based amp modeling.

**Dependencies:** Neural capture crate (v0.3), Daisy Seed HAL integration (existing, in `sonido-platform`).

**Status:** Not started.

---

### Per-Note Expression (CLAP + MPE)

CLAP note expression support for per-note modulation depth, pitch bend, pressure, and timbre per active note. MPE-aware synthesis via the `VoiceManager` — each voice receives its own expression lane.

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

Each circuit: ~300–500 LOC once the WDF library exists. Common building blocks (op-amp, diode, RC network) are shared across circuits.

**Dependencies:** WDF library (v0.3).

**Status:** Not started.

---

### Neural Model Morphing (Research)

Interpolation between two trained neural captures to blend tonal characteristics. Weight-space interpolation works for identical GRU architectures: `weights = (1-α) * model_A + α * model_B`. This produces a spectrum of tones between the two captured amps.

More powerful approach: conditioned networks, where a single GRU takes a style parameter as an additional input and is trained on pairs of captures. One model, infinite blends. Active research area — no production solution exists industry-wide.

**Why it's research-stage:** Weight interpolation produces perceptually smooth blends for some pairs but artifacts for others. The conditions under which interpolation fails are not fully characterized. The conditioned network approach requires new training infrastructure beyond what NAM/AIDA-X currently provide.

**Dependencies:** Neural capture crate (v0.3), ML research investment (external).

**Status:** Research stage. Worth tracking; not a near-term deliverable.

---

## Development Principles

These principles apply to all roadmap items:

**no_std by default for DSP.** New crates with DSP processing code target `no_std` unless std is unavoidable. The Cortex-M target is a first-class citizen.

**Effect trait compatibility.** New processing nodes (WDF circuits, neural capture, spectral effects) all implement `Effect` + `ParameterInfo`. They slot into chains and DAG routing without special handling.

**Golden file coverage.** Every new effect gets golden regression baselines before merging. MSE < 1e-6, SNR > 60 dB. Intentional algorithm changes regenerate baselines via `REGENERATE_GOLDEN=1`.

**One crate per capability domain.** `sonido-wdf`, `sonido-neural`, and future additions are separate crates with clearly scoped dependencies. The workspace prevents capability bleeding between domains.

**Benchmark before optimizing.** Add benchmarks before claiming performance properties. `cargo bench` is part of CI. Claims about real-time viability on embedded targets require measured cycle counts.

**Stable parameter IDs.** Once an effect ships, parameter indices and `ParamId` values are public API. New parameters are added at the end. No reordering.
