# Doc-to-Code Mapping Table

When you modify a source file, you **must** update every documentation target listed in the same row.

| Source File(s) | Documentation Target(s) | What to Update |
|---|---|---|
| `crates/sonido-core/src/effect.rs` | `docs/ARCHITECTURE.md` (Effect Trait), `docs/DESIGN_DECISIONS.md` ADR-001 | Trait signature, method docs, stereo/mono classification, `tail_samples`, `snap_params` |
| `crates/sonido-core/src/level.rs` | `docs/KERNEL_ARCHITECTURE.md`, `docs/EFFECT_SDK.md`, `docs/DSP_QUALITY_STANDARD.md`, `docs/DSP_FUNDAMENTALS.md` (wet/dry mix), `docs/EFFECTS_REFERENCE.md` (Output Level Convention), `docs/ARCHITECTURE.md` (Kernel Architecture) | `Level`, `blend`/`mix_gains` law, `NOMINAL_PEAK_DBFS` |
| `crates/sonido-core/src/param.rs` | `docs/DSP_FUNDAMENTALS.md` (Parameter Smoothing), `docs/ARCHITECTURE.md` (Parameter Smoothing) | Smoothing config, advance() usage, default timing |
| `crates/sonido-core/src/param_info.rs` | `docs/ARCHITECTURE.md` (ParameterInfo Trait) | Trait methods, ParamDescriptor fields, ParamFlags |
| `crates/sonido-core/src/modulation.rs` | `docs/DSP_FUNDAMENTALS.md` (Modulation Effects) | Trait interface, bipolar/unipolar ranges |
| `crates/sonido-core/src/tempo.rs` | `docs/DSP_FUNDAMENTALS.md` (Tempo Synchronization) | NoteDivision variants, BPM conversion, TempoContext fields |
| `crates/sonido-core/src/biquad.rs` | `docs/DSP_FUNDAMENTALS.md` (Biquad), `docs/DESIGN_DECISIONS.md` ADR-007 | Filter types, coefficient formulas, Direct Form choice |
| `crates/sonido-core/src/svf.rs` | `docs/DSP_FUNDAMENTALS.md` (SVF), `docs/DESIGN_DECISIONS.md` ADR-008 | SVF topology, modulation stability notes |
| `crates/sonido-core/src/comb.rs`, `allpass.rs`, `cached.rs` (`needs_update`) | `docs/DSP_FUNDAMENTALS.md` (Reverb: Feedback Delay Network — Allpass Diffusion), `docs/EFFECTS_REFERENCE.md` (reverb, spring_reverb, plate_reverb) | Delay lengths, feedback structure |
| `crates/sonido-core/src/delay.rs` | `docs/DSP_FUNDAMENTALS.md` (Delay Lines and Interpolation), `docs/DESIGN_DECISIONS.md` ADR-013 | Interpolation methods, buffer sizing |
| `crates/sonido-core/src/oversample.rs` | `docs/DSP_FUNDAMENTALS.md` (Oversampling), `docs/DESIGN_DECISIONS.md` ADR-003 | Const generic design, filter coefficients |
| `crates/sonido-core/src/lfo.rs` | `docs/DSP_FUNDAMENTALS.md` (Modulation Effects) | Waveform shapes, phase accumulator |
| `crates/sonido-core/src/fast_math.rs` | `docs/DSP_FUNDAMENTALS.md` (Fast Math Approximations), `docs/DESIGN_DECISIONS.md` ADR-020 | Function signatures, error bounds, use cases, cycle counts |
| `crates/sonido-effects/src/kernels/reverb.rs` | `docs/EFFECTS_REFERENCE.md` (reverb), `docs/DSP_FUNDAMENTALS.md` (Reverb: Feedback Delay Network), `docs/DESIGN_DECISIONS.md` ADR-009 note | FDN delay tunings, Hadamard mixing, early reflections, stereo width, parameters |
| `crates/sonido-effects/src/kernels/distortion.rs` | `docs/EFFECTS_REFERENCE.md` (distortion), `docs/DSP_FUNDAMENTALS.md` (Waveshaping and Distortion) | Clipping modes, waveshaper transfer functions |
| `crates/sonido-effects/src/kernels/compressor.rs` | `docs/EFFECTS_REFERENCE.md` (compressor), `docs/DSP_FUNDAMENTALS.md` (Dynamics Processing) | Attack/release, knee, ratio, makeup gain |
| `crates/sonido-effects/src/kernels/chorus.rs` | `docs/EFFECTS_REFERENCE.md` (chorus), `docs/DSP_FUNDAMENTALS.md` (Modulation Effects) | Delay modulation, voice count, feedback, tempo sync |
| `crates/sonido-effects/src/kernels/delay.rs` | `docs/EFFECTS_REFERENCE.md` (delay) | Feedback, ping-pong, diffusion, tempo sync |
| `crates/sonido-effects/src/kernels/phaser.rs` | `docs/EFFECTS_REFERENCE.md` (phaser), `docs/DSP_FUNDAMENTALS.md` (Modulation Effects) | Allpass stage count, sweep range, tempo sync |
| `crates/sonido-effects/src/kernels/flanger.rs` | `docs/EFFECTS_REFERENCE.md` (flanger), `docs/DSP_FUNDAMENTALS.md` (Modulation Effects) | Feedback polarity, TZF, delay range, tempo sync |
| `crates/sonido-effects/src/kernels/tremolo.rs` | `docs/EFFECTS_REFERENCE.md` (tremolo), `docs/DSP_FUNDAMENTALS.md` (Modulation Effects) | Waveform types, stereo spread, tempo sync |
| `crates/sonido-effects/src/kernels/tuner.rs` | `docs/EFFECTS_REFERENCE.md` (tuner) | Decimation rate, frame/hop size, detection range |
| `crates/sonido-effects/src/kernels/*.rs` (any new effect) | `docs/EFFECTS_REFERENCE.md`, `README.md` (features + count), `docs/ARCHITECTURE.md` (effect table), `crates/sonido-plugin/src/bundle.rs` `CATALOG` (enforced by `tests/plugin_inventory.rs`) | Full effect entry with parameters, DSP theory, example; plugin catalog entry |
| `crates/sonido-core/src/math.rs` | `docs/DSP_FUNDAMENTALS.md` (Gain Staging, Soft Limiting) | Mix/gain helpers, safety limiters, dB conversions |
| `crates/sonido-effects/src/kernels/*.rs`, `crates/sonido-core/src/gain.rs` | `docs/DSP_QUALITY_STANDARD.md` | Quality rules, compliance table, measurement protocol |
| `crates/sonido-effects/examples/dsp_report.rs` | `docs/DSP_MEASUREMENTS.md` (regenerate: `cargo run --release --example dsp_report -p sonido-effects`) | Generated measurement tables — never hand-edit |
| `crates/sonido-effects/src/calibration.rs`, `examples/calibrate.rs`, `tests/level_contract.rs` | `docs/EFFECT_SDK.md`, `docs/KERNEL_ARCHITECTURE.md`, `docs/TESTING.md`, `docs/EFFECTS_REFERENCE.md` (Output Level Convention) | Generated level table (regenerate: `cargo run -p sonido-effects --example calibrate --release`, never hand-edit), contract tolerances |
| `crates/sonido-effects/tests/reverb_decay.rs` | `docs/TESTING.md`, `docs/EFFECTS_REFERENCE.md` (plate_reverb, spring_reverb) | Decay → RT60 checks |
| `crates/sonido-analysis/src/signals.rs`, `loudness.rs` (`effect_gain_db`) | `docs/ARCHITECTURE.md` (sonido-analysis), `docs/TESTING.md` | Reference guitar, nominal level, the loudness measurement behind the level contract |
| `crates/sonido-synth/src/oscillator.rs` | `docs/SYNTHESIS.md`, `docs/DSP_FUNDAMENTALS.md` (PolyBLEP), `docs/DESIGN_DECISIONS.md` ADR-014 | Waveforms, poly_blep correction, anti-aliasing |
| `crates/sonido-synth/src/envelope.rs` | `docs/SYNTHESIS.md` | ADSR stages, curve shapes, gate behavior |
| `crates/sonido-synth/src/voice.rs` | `docs/SYNTHESIS.md` | Voice allocation, stealing policy |
| `crates/sonido-synth/src/mod_matrix.rs` | `docs/SYNTHESIS.md` | Routing, source/destination IDs, depth scaling |
| `crates/sonido-registry/src/lib.rs` | `docs/ARCHITECTURE.md` (sonido-registry), `docs/DESIGN_DECISIONS.md` ADR-011, `docs/EMBEDDED.md` (Jam Effect List and Knob Mapping) | Registration API, create-by-name pattern, `EFFECT_UIDS`, `JAM_EFFECT_IDS` (the jam's pick order) |
| `crates/sonido-analysis/src/cfc.rs` | `docs/reference/biosignal.md`, `docs/reference/cfc-analysis.md` | PAC algorithm, comodulogram, surrogate stats |
| `crates/sonido-analysis/src/filterbank.rs` | `docs/reference/biosignal.md` | EEG band definitions, filter bank design |
| `crates/sonido-analysis/src/hilbert.rs` | `docs/reference/biosignal.md` | Analytic signal, instantaneous phase/amplitude |
| `crates/sonido-analysis/src/*.rs` (incl. `lms.rs`, `xcorr.rs`, `ddc.rs`, `phase.rs`, `resample.rs`) | `docs/ARCHITECTURE.md` (sonido-analysis section) | Public API names |
| `crates/sonido-platform/src/*.rs` | `docs/EMBEDDED.md`, `docs/ARCHITECTURE.md` (sonido-platform), `docs/DESIGN_DECISIONS.md` ADR-012 | PlatformController trait, ControlId namespaces, ControlMapper, knob_mapping, param_map |
| `crates/sonido-platform/src/cycle_table.rs`, `scripts/update_cycle_table.py` | `docs/BENCHMARKS.md` (Cortex-M7, Updating the cycle table), `docs/EMBEDDED.md` (Chain Configurations), `docs/PATCH_PLATFORM.md`, `README.md` (Benchmarks) | Measured worst blocks (regenerate from a `bench_kernels` log, never hand-edit `EFFECT_CYCLES`), `EFFECT_SDRAM`, `CHAIN_FRACTION`, `NODE_OVERHEAD_CYCLES`, `chain_fits` |
| `crates/sonido-platform/tests/knob_calibration.rs` | `docs/EMBEDDED.md` (Knob Mapping Verification), `docs/TESTING.md` | Noon and knob-extreme loudness tolerances |
| `crates/sonido-io/src/*.rs` | `docs/ARCHITECTURE.md` (sonido-io section), `docs/DESIGN_DECISIONS.md` ADR-023 | AudioBackend trait, CpalBackend, StreamHandle, BackendStreamConfig, WAV API |
| `crates/sonido-cli/src/main.rs`, `commands/*.rs` | `docs/CLI_GUIDE.md`, `docs/CLI_TESTING.md`, `docs/ARCHITECTURE.md` (sonido-cli) | Command syntax, flags, examples |
| `crates/sonido-cli/src/commands/pedal.rs`, `commands/patch.rs` | `docs/CLI_GUIDE.md` (pedal, Rig Export), `docs/EMBEDDED.md` (USB Rig Link, Bypass and Bootloader), `docs/PATCH_PLATFORM.md`, `README.md` | `sonido pedal` subcommands and `--port`, `patch export` inputs/outputs and `--pedal` |
| `crates/sonido-graph-dsl/src/*.rs` | `docs/CLI_GUIDE.md` (graph syntax) | DSL grammar, split/merge semantics, topology examples, effect alias resolution |
| `crates/sonido-patch/src/*.rs` | `docs/PATCH_PLATFORM.md` | Patch model, binary codec, `PatchPlayer`, `build_graph_from_patch` |
| `crates/sonido-patch/src/pedal.rs`, `link.rs` | `docs/EMBEDDED.md` (The Pedal: The Jam, Flash, USB Rig Link), `docs/PATCH_PLATFORM.md` | `PedalState` sector layout, jam patch, `JamSettings`, `RigRef` bytes, link frame/requests (incl. `Bootloader`)/error codes |
| `crates/sonido-patch/src/client.rs`, `dfu.rs` (moved from `sonido-gui`), `validate.rs` (`validate_for_daisy`) | `docs/EMBEDDED.md` (USB Rig Link, Bypass and Bootloader), `docs/PATCH_PLATFORM.md`, `docs/ARCHITECTURE.md` (sonido-patch), `docs/CLI_GUIDE.md` (pedal), `docs/GUI.md` (Export) | `LinkClient`, `send_rig` slot rule, `open_pedal`/`pedal_ports` (`serial` feature, USB id), firmware DFU (Daisy bootloader only, `FIRMWARE_ADDR`), the one pedal check |
| `crates/sonido-rigs/src/*.rs`, `examples/calibrate.rs`, `tests/factory.rs` | `docs/EMBEDDED.md` (Factory Rigs), `docs/ARCHITECTURE.md` (sonido-rigs), `docs/PATCH_PLATFORM.md`, `README.md` | Rig list, signal paths, K1–K6 macros, generated trims (regenerate: `cargo run -p sonido-rigs --example calibrate --release`), test tolerances |
| `crates/sonido-gui/src/graph_view.rs` | `docs/GUI.md`, `docs/ARCHITECTURE.md` | Visual node-graph editor, Snarl topology, compile_to_engine |
| `crates/sonido-gui/src/morph_state.rs` | `docs/GUI.md`, `docs/PATCH_PLATFORM.md` | A/B morph poses, per-effect locks, `curve_lerp` interpolation |
| `crates/sonido-gui/src/app.rs` | `docs/GUI.md`, `docs/PATCH_PLATFORM.md` (macros, morph, export) | GUI features, layout, controls, the Export menu (rig Name, Send to pedal) |
| `crates/sonido-gui/src/session.rs`, `export.rs` | `docs/PATCH_PLATFORM.md`, `docs/GUI.md` (Sessions, Export) | Session version and fields (`name`), patch projection, export destinations |
| `crates/sonido-gui/src/signal_generator.rs` | `docs/GUI.md` (Signal Generator section) | Signal types, SourceMode, generator controls |
| `crates/sonido-gui-core/src/effects_ui/*.rs` | `docs/GUI.md` (Effects Reference, Generic Effect Panels) | `GenericPanel`, `LooperPanel`, `create_panel()` dispatch |
| `crates/sonido-gui-core/src/theme.rs`, `cabinet.rs`, `widgets/*.rs` | `docs/DESIGN_SYSTEM.md`, `docs/ARCHITECTURE.md` (sonido-gui-core) | Tokens, Cabinet layout, widget list |
| `crates/sonido-plugin/src/*.rs`, `examples/sonido.rs` | `docs/ARCHITECTURE.md` (sonido-plugin), `docs/DESIGN_DECISIONS.md` ADR-024 note, `docs/ROADMAP.md` (Plugin Integration) | Bundle catalog, plugin IDs, GUI bridge, shared state, gesture protocol, extensions |
| `scripts/bundle-clap.sh`, `scripts/bundle-vst3.sh`, `Makefile` (`plugins`, `plugins-vst3`) | `docs/GETTING_STARTED.md`, `docs/ARCHITECTURE.md` (sonido-plugin) | Bundle paths, install locations, build deps |
| `crates/sonido-config/src/*.rs` | `docs/CLI_GUIDE.md` (presets, Preset Files), `docs/ARCHITECTURE.md` (sonido-config) | Preset format, config paths, validation, factory presets, `preset_to_patch` |
| Any new crate | `docs/ARCHITECTURE.md` (crate diagram + responsibilities) | Crate purpose, no_std status, dependency position |
| `crates/sonido-core/src/kernel/traits.rs`, `adapter.rs` | `docs/ARCHITECTURE.md` (Kernel Architecture), `docs/KERNEL_ARCHITECTURE.md`, `docs/DESIGN_DECISIONS.md` ADR-028 | DspKernel (`LEVEL`, `blend`, `blend_stereo`, `level`, `level_stereo`), KernelParams, SmoothingStyle, Adapter (SmoothedPolicy / DirectPolicy) |
| `crates/sonido-core/src/kernel/morph.rs`, `chain_morph.rs`, `macro_map.rs` | `docs/KERNEL_ARCHITECTURE.md`, `docs/PATCH_PLATFORM.md` | MorphCurve, curve_lerp, ChainMorph, MacroMap |
| `crates/sonido-effects/src/kernels/*.rs` | `docs/EFFECTS_REFERENCE.md` (Kernel Architecture), `docs/KERNEL_ARCHITECTURE.md` | Kernel implementations, parameter tables, from_knobs() |
| `crates/sonido-core/src/compose.rs` | `docs/KERNEL_ARCHITECTURE.md` | `seq`, `par`, `feedback` combinators, `GraphBuilder` API |
| `crates/sonido-core/src/graph/` | `docs/ARCHITECTURE.md` (DAG Processing), `docs/DESIGN_DECISIONS.md` ADR-025 | Node types, buffer pool, topological sort, `ProcessingGraph` API |
| `crates/sonido-core/src/graph/engine.rs` | `docs/ARCHITECTURE.md` (DAG Processing), `docs/DESIGN_DECISIONS.md` ADR-027 | `GraphEngine` API and consumers |
| `crates/sonido-gui/src/chain_manager.rs`, `audio_processor.rs`, `atomic_param_bridge.rs` | `docs/ARCHITECTURE.md` (Parameter Threading Model), `docs/GUI.md` | `GraphCommand` enum (Add/Remove/ReplaceTopology), param sync, memory ordering |
| `crates/sonido-platform/src/param_map.rs`, `knob_mapping.rs` | `docs/EMBEDDED.md` (Biased Knob Mapping, Knob Mapping Verification, Jam Effect List and Knob Mapping), `docs/DESIGN_DECISIONS.md` ADR-030 and ADR-032, `crates/sonido-effects/tests/noon_mapping.rs` | Biased mapping algorithm (noon = descriptor default), knob floors, every `knob_map` (top row levels with K3 the output, bottom row the sound) |
| `crates/sonido-daisy/src/*.rs` | `docs/EMBEDDED.md` | Firmware lib, DWT helpers, audio constants, `heartbeat` task, SDRAM init, clock profiles, knob hysteresis (`controls.rs`) |
| `crates/sonido-daisy/src/qspi_flash.rs`, `patch_loader.rs`, `rigs.rs`, `build.rs` | `docs/EMBEDDED.md` (The Pedal: Loading a Rig, Real-Time Hardening, Flash), `docs/PATCH_PLATFORM.md` | Rig bank layout, page-chunked reads, `check_rig`, blink codes, factory rigs encoded at build time |
| `crates/sonido-daisy/examples/sonido_pedal.rs` | `docs/EMBEDDED.md` (The Pedal (Rig Player)), `docs/ARCHITECTURE.md` (Hardware Targets), `docs/DESIGN_DECISIONS.md` ADR-032, `README.md` | Rig list, controls tables (the control rule), jam setup page and picks, bootloader gestures, loading, state save, USB link, LEDs, load meter |
| `crates/sonido-daisy/examples/*.rs` | `docs/EMBEDDED.md` (Tier System, Getting Started, Diagnostics), `docs/PATCH_PLATFORM.md` (firmware integration) | bench_kernels, bench_mini, blinky, blinky_bare, convert_passthrough, graph_audio_test, heap_test, hothouse_diag, passthrough, passthrough_blink, qspi_test, silence, simple_gain, single_delay, single_effect, single_reverb, sonido_pedal, square_out, tone_out |
| `LICENSE` (AGPL-3.0), `LICENSING.md` | `README.md` (License section) | License text, AGPL-3.0 + commercial dual-license references |
| `docs/reference/signature-sounds.md` | `docs/ROADMAP.md` (cross-reference) | Brainstorming candidates, interaction patterns |
