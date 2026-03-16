# Doc-to-Code Mapping Table

When you modify a source file, you **must** update every documentation target listed in the same row.

Rows marked `<!-- PLANNED -->` are pre-reserved for v0.3+ features. Do not update these doc targets until the corresponding source files exist. Remove the marker when implementation begins.

| Source File(s) | Documentation Target(s) | What to Update |
|---|---|---|
| `crates/sonido-core/src/effect.rs` | CLAUDE.md (Effect Trait section), `docs/ARCHITECTURE.md`, `docs/DESIGN_DECISIONS.md` ADR-001 | Trait signature, method docs, stereo/mono classification |
| `crates/sonido-core/src/param.rs` | CLAUDE.md (Key Patterns: SmoothedParam), `docs/DSP_FUNDAMENTALS.md` (Parameter Smoothing) | Smoothing config, advance() usage, default timing |
| `crates/sonido-core/src/param_info.rs` | CLAUDE.md (Key Patterns: ParameterInfo) | Trait methods, ParamDescriptor fields |
| `crates/sonido-core/src/modulation.rs` | CLAUDE.md (Key Patterns: ModulationSource), `docs/DSP_FUNDAMENTALS.md` (Modulation Effects) | Trait interface, bipolar/unipolar ranges |
| `crates/sonido-core/src/tempo.rs` | CLAUDE.md (Key Patterns: TempoManager, TempoContext), `docs/DSP_FUNDAMENTALS.md` (Tempo Sync) | NoteDivision variants, BPM conversion, TempoContext fields |
| `crates/sonido-core/src/biquad.rs` | `docs/DSP_FUNDAMENTALS.md` (Biquad), `docs/DESIGN_DECISIONS.md` ADR-007 | Filter types, coefficient formulas, Direct Form choice |
| `crates/sonido-core/src/svf.rs` | `docs/DSP_FUNDAMENTALS.md` (SVF), `docs/DESIGN_DECISIONS.md` ADR-008 | SVF topology, modulation stability notes |
| `crates/sonido-core/src/comb.rs`, `allpass.rs` | `docs/DSP_FUNDAMENTALS.md` (Reverb: Freeverb), `docs/EFFECTS_REFERENCE.md` (Reverb) | Delay lengths, feedback structure |
| `crates/sonido-core/src/delay.rs` | `docs/DSP_FUNDAMENTALS.md` (Delay Lines), `docs/DESIGN_DECISIONS.md` ADR-013 | Interpolation methods, buffer sizing |
| `crates/sonido-core/src/oversample.rs` | `docs/DSP_FUNDAMENTALS.md` (Oversampling), `docs/DESIGN_DECISIONS.md` ADR-003 | Const generic design, filter coefficients |
| `crates/sonido-core/src/lfo.rs` | `docs/DSP_FUNDAMENTALS.md` (Modulation Effects) | Waveform shapes, phase accumulator |
| `crates/sonido-core/src/fast_math.rs` | `docs/DSP_FUNDAMENTALS.md` (Fast Math Approximations), `docs/DESIGN_DECISIONS.md` ADR-020 | Function signatures, error bounds, use cases, cycle counts |
| `crates/sonido-effects/src/kernels/reverb.rs` | `docs/EFFECTS_REFERENCE.md` (Reverb), `docs/DSP_FUNDAMENTALS.md` (Freeverb), `docs/DESIGN_DECISIONS.md` ADR-009 | Comb tunings, stereo spread, parameters |
| `crates/sonido-effects/src/kernels/distortion.rs` | `docs/EFFECTS_REFERENCE.md` (Distortion), `docs/DSP_FUNDAMENTALS.md` (Waveshaping) | Clipping modes, waveshaper transfer functions |
| `crates/sonido-effects/src/kernels/compressor.rs` | `docs/EFFECTS_REFERENCE.md` (Compressor), `docs/DSP_FUNDAMENTALS.md` (Dynamics) | Attack/release, knee, ratio, makeup gain |
| `crates/sonido-effects/src/kernels/chorus.rs` | `docs/EFFECTS_REFERENCE.md` (Chorus), `docs/DSP_FUNDAMENTALS.md` (Modulation Effects) | Delay modulation, voice count, feedback, tempo sync |
| `crates/sonido-effects/src/kernels/delay.rs` | `docs/EFFECTS_REFERENCE.md` (Delay) | Feedback, ping-pong, diffusion, tempo sync |
| `crates/sonido-effects/src/kernels/phaser.rs` | `docs/EFFECTS_REFERENCE.md` (Phaser), `docs/DSP_FUNDAMENTALS.md` (Modulation Effects) | Allpass stage count, sweep range, tempo sync |
| `crates/sonido-effects/src/kernels/flanger.rs` | `docs/EFFECTS_REFERENCE.md` (Flanger), `docs/DSP_FUNDAMENTALS.md` (Modulation Effects) | Feedback polarity, TZF, delay range, tempo sync |
| `crates/sonido-effects/src/kernels/tremolo.rs` | `docs/EFFECTS_REFERENCE.md` (Tremolo), `docs/DSP_FUNDAMENTALS.md` (Modulation Effects) | Waveform types, stereo spread, tempo sync |
| `crates/sonido-effects/src/kernels/*.rs` (any new effect) | `docs/EFFECTS_REFERENCE.md`, `README.md` (features + count), CLAUDE.md (Key Files) | Full effect entry with parameters, DSP theory, example |
| `crates/sonido-core/src/math.rs` | `docs/DSP_FUNDAMENTALS.md` (Gain Staging, Soft Limiting) | Mix/gain helpers, safety limiters, dB conversions |
| `crates/sonido-effects/src/kernels/*.rs`, `crates/sonido-core/src/gain.rs` | `docs/DSP_QUALITY_STANDARD.md` | Quality rules, compliance table, measurement protocol |
| `crates/sonido-synth/src/oscillator.rs` | `docs/SYNTHESIS.md`, `docs/DSP_FUNDAMENTALS.md` (PolyBLEP), `docs/DESIGN_DECISIONS.md` ADR-014 | Waveforms, poly_blep correction, anti-aliasing |
| `crates/sonido-synth/src/envelope.rs` | `docs/SYNTHESIS.md` | ADSR stages, curve shapes, gate behavior |
| `crates/sonido-synth/src/voice.rs` | `docs/SYNTHESIS.md` | Voice allocation, stealing policy |
| `crates/sonido-synth/src/mod_matrix.rs` | `docs/SYNTHESIS.md` | Routing, source/destination IDs, depth scaling |
| `crates/sonido-registry/src/lib.rs` | CLAUDE.md (Key Patterns: Effect Registry), `docs/DESIGN_DECISIONS.md` ADR-011 | Registration API, create-by-name pattern |
| `crates/sonido-analysis/src/cfc.rs` | `docs/reference/biosignal.md`, `docs/reference/cfc-analysis.md` | PAC algorithm, comodulogram, surrogate stats |
| `crates/sonido-analysis/src/filterbank.rs` | `docs/reference/biosignal.md` | EEG band definitions, filter bank design |
| `crates/sonido-analysis/src/hilbert.rs` | `docs/reference/biosignal.md` | Analytic signal, instantaneous phase/amplitude |
| `crates/sonido-analysis/src/lms.rs`, `xcorr.rs`, `ddc.rs`, `phase.rs`, `resample.rs` | CLAUDE.md (Key Files table), `docs/ARCHITECTURE.md` (sonido-analysis section) | LMS/NLMS API, xcorr functions, DDC struct, phase unwrapping, resampling |
| `crates/sonido-platform/src/*.rs` | `docs/EMBEDDED.md`, `docs/DESIGN_DECISIONS.md` ADR-012 | PlatformController trait, ControlId namespaces, GraphMapper, ControlContext |
| `crates/sonido-io/src/backend.rs`, `cpal_backend.rs` | `docs/ARCHITECTURE.md` (sonido-io section), `docs/DESIGN_DECISIONS.md` ADR-023 | AudioBackend trait, CpalBackend, StreamHandle, BackendStreamConfig |
| `crates/sonido-cli/src/commands/*.rs` | `docs/CLI_GUIDE.md` | Command syntax, flags, examples |
| `crates/sonido-graph-dsl/src/*.rs` | `docs/CLI_GUIDE.md` (Graph Syntax section), CLAUDE.md (Crates table, Key Files) | DSL grammar, split/merge semantics, topology examples, effect alias resolution |
| `crates/sonido-gui/src/graph_view.rs` | `docs/GUI.md`, `docs/ARCHITECTURE.md` | Visual node-graph editor, Snarl topology, compile_to_engine |
| `crates/sonido-gui/src/morph_state.rs` | `docs/GUI.md` | A/B morph snapshot capture, lerp-powered interpolation |
| `crates/sonido-gui/src/app.rs` | `docs/GUI.md` | GUI features, layout, controls |
| `crates/sonido-gui/src/signal_generator.rs` | `docs/GUI.md` (Signal Generator section) | Signal types, SourceMode, generator controls |
| `crates/sonido-plugin/src/lib.rs`, `audio.rs`, `gui.rs`, `main_thread.rs`, `shared.rs` | CLAUDE.md (Crates table, Key Files table), `docs/ARCHITECTURE.md` (plugin section), `docs/DESIGN_DECISIONS.md` ADR-024 | Plugin adapter API, macro interface, GUI bridge, shared state, gesture protocol |
| `crates/sonido-plugin/src/chain/*.rs` | CLAUDE.md (Key Files table), `docs/ARCHITECTURE.md` (plugin section), `docs/DESIGN_DECISIONS.md` ADR-026, `docs/CHANGELOG.md` | ChainPlugin, ClapParamId, ChainShared, ChainAudioProcessor, ChainMainThread, ChainParamBridge, ChainEditor |
| `crates/sonido-config/src/*.rs` | `docs/GETTING_STARTED.md` (presets section) | Preset format, config paths, validation |
| Any new crate | CLAUDE.md (Crates table + Key Files table), `docs/ARCHITECTURE.md` (diagram) | Crate purpose, no_std status, dependency position |
| `crates/sonido-core/src/kernel/traits.rs`, `adapter.rs` | CLAUDE.md (Kernel Architecture section), `docs/ARCHITECTURE.md` (Kernel Architecture), `docs/KERNEL_ARCHITECTURE.md`, `docs/DESIGN_DECISIONS.md` ADR-028 | DspKernel, KernelParams, SmoothingStyle, Adapter (SmoothedPolicy / DirectPolicy) |
| `crates/sonido-effects/src/kernels/*.rs` | `docs/EFFECTS_REFERENCE.md` (Kernel Architecture), `docs/ARCHITECTURE.md` | Kernel implementations, parameter tables, from_knobs() |
| `crates/sonido-core/src/graph/` | `docs/ARCHITECTURE.md` (DAG section), `docs/DESIGN_DECISIONS.md` ADR-025, CLAUDE.md (Crates table, Key Patterns, Key Files) | Node types, buffer pool, topological sort, `ProcessingGraph` API |
| `crates/sonido-core/src/graph/engine.rs` | `docs/ARCHITECTURE.md` (data flow), CLAUDE.md (Key Files) | `GraphEngine` API, `from_chain()` migration path |
| `crates/sonido-core/src/graph/stereo_samples.rs` | CLAUDE.md (Key Files) | `StereoSamples` struct (stereo buffer pair) |
| `crates/sonido-gui/src/chain_manager.rs` | `docs/ARCHITECTURE.md` (data flow), `docs/GUI.md` | `GraphCommand` enum (Add/Remove/ReplaceTopology) |
| `crates/sonido-daisy/src/*.rs` | `docs/EMBEDDED.md`, CLAUDE.md (Crates table, Key Files) | Firmware lib, DWT helpers, audio constants, `heartbeat` task, SDRAM init, clock profiles |
| `crates/sonido-daisy/examples/*.rs` | `docs/EMBEDDED.md` (Tier System, Getting Started, Diagnostics) | bench_kernels, bench_mini, blinky, blinky_bare, heap_test, hothouse_diag, morph_pedal, passthrough, passthrough_blink, silence, single_effect, square_out, tone_out |
| `LICENSE-MIT`, `LICENSE-APACHE` | `README.md` (License section), `docs/LICENSING.md` | License text, dual-license references |
| `docs/reference/signature-sounds.md` | `docs/ROADMAP.md` (cross-reference) | Brainstorming candidates, interaction patterns |
