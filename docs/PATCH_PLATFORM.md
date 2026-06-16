# Patch Platform — design & status

The patch platform lets you build an effect rig in the standalone editor, map
effect/global controls to **six macros** (the pedal's six knobs), dial in an
**A/B morph**, and export the result as a **CLAP graph-player patch** or a
**flashable pedal sector** — all from one canonical model.

## One model, three projections

```
                 sonido_patch::Patch
   nodes (stable effect UID) · edges (Input/Output/Split/Merge) ·
   per-node A/B param snapshots · bypass · 6 macros · morph config · globals
      │                      │                         │
      ▼ JSON (serde)         ▼ JSON                     ▼ binary codec (4 KB sector)
 GUI session v2         CLAP plugin state          QSPI patch bank
 (+ node positions)     (graph-player)             firmware decodes; GUI writes via DFU
```

Effects are referenced by **stable UID** (`sonido_registry::EFFECT_UIDS`), never
by list position — positional indices rotted once already (the daisy-export
test drift fixed in Phase 0). The same Rust binary codec compiles into the host
tools (encode+decode) and the firmware (decode only): no second hand-mirrored
struct to drift.

Runtime layering, shared by GUI / plugin / pedal:
- `MacroMap<6>` (`sonido-core`) — knob → `MacroTarget` (slot param or global),
  per-mapping range + curve, engine-decoupled sink apply.
- `ChainMorph` (`sonido-core`) — per-slot `MorphSpace` + locks; one engine for
  all three consumers, so they interpolate identically (incl. logarithmic
  frequency morph and stepped snap).
- Per-control-tick order: **morph first, then macros** (macros win on overlap).

## What's built (committed, verified)

| Area | Where | Status |
|------|-------|--------|
| Phase 0 clean state | effects/cli/daisy | ✅ restored green gate (was red 3 ways) |
| Patch format + codec + validate | `crates/sonido-patch` | ✅ proptest roundtrips; no_std thumbv7em |
| Stable effect UIDs | `sonido-registry::EFFECT_UIDS` | ✅ totality/uniqueness test |
| CLI `sonido patch export\|inspect` | `sonido-cli/commands/patch.rs` | ✅ end-to-end verified |
| GUI session v2 + v1 migration + `to_patch()` | `sonido-gui/session.rs` | ✅ migration test |
| Macro system | `sonido-core/macro_map.rs` | ✅ 8 tests |
| A/B morph unification + GUI log-fidelity fix | `sonido-core/kernel/chain_morph.rs`, `sonido-gui/morph_state.rs` | ✅ |
| `build_graph_from_patch` (arbitrary DAG) | `sonido-patch/build.rs` | ✅ host-tested with real effects |
| Firmware QSPI read + patch loader + factory | `sonido-daisy/{qspi_flash,patch_loader}.rs` | ✅ compiles thumbv7em |
| `qspi_read_test` hardware probe | `sonido-daisy/examples/` | ✅ compiles; **awaiting hardware** |
| Export logic (dfu / cost table / validate) | `sonido-gui/{dfu,export}.rs`, `sonido-platform/cycle_table.rs` | ✅ 11 tests |
| Macro panel + morph crossfader widgets | `sonido-gui-core/widgets/{macro_panel,morph_bar}.rs` | ✅ lib-checked |
| **PatchPlayer** runtime (macros+morph+gains over a graph) | `sonido-patch/runtime.rs` | ✅ 5 tests; no_std thumbv7em |
| **Graph-player CLAP plugin** (headless: params/state/audio) | `sonido-plugin/graph_player.rs` + example | ✅ cdylib builds; 4 tests; in `make verify` |
| Full `make verify` (fmt+clippy+tests+no_std+wasm+doc) | whole workspace | ✅ exits 0, zero warnings |

`PatchPlayer` is the single runtime the plugin and firmware both wrap, so a rig
sounds identical in the DAW and on the pedal.

## Remaining integration (needs a live GUI / hardware)

GL is now installed (plugin builds, full `make verify` green). What's left needs
either a visible GUI session (you, present) or the pedal:

1. **No Seed/pedal access yet** → the QSPI-on-H7 read path is unvalidated (run
   `qspi_read_test` first).
2. **Standalone GUI panels** can be lib-checked but want a visible window to dial
   in the UX (you've said morph must feel right) — best done with you present.

Precise remaining wiring points, by workstream:

- **B-GUI (macros):** place `macro_panel` in `app.rs`; add a "Map to Macro 1–6"
  context menu on parameter knobs (`sonido-gui-core/widgets/bridged_knob.rs`);
  add `GraphCommand::SetMacroMap(MacroMap<6>)` to `chain_manager.rs` + apply on
  the audio thread in `audio_processor.rs` (Box-swap like `ReplaceTopology`);
  6 atomic macro positions in `atomic_param_bridge.rs`.
- **C-GUI (morph):** the `morph_bar` widget exists; wire it to `MorphState`
  (`morph_state.rs`) in `app.rs`, add per-slot lock toggles on the chain strip,
  and expose `GlobalParam::MorphPosition` as a macro target.
- **E (export panel):** an `export_panel.rs` calling the host-tested `export.rs`
  + `dfu.rs` (logic done); bootloader-entry modal; slot picker; full-firmware
  flash button.
- **D (firmware integration):** after `qspi_read_test` confirms QSPI, evolve
  `examples/sonido_pedal.rs` into `sonido_patch_player.rs` — load via
  `patch_loader::load_and_build`, drive `MacroMap<6>` from the knobs (pickup-lock),
  `ChainMorph` from the footswitch (Ramp/Momentary/Latch), wire `tap_tempo.rs`,
  keep the `GRAPH_UPDATING` rebuild + bypass crossfade, LED error codes
  (`patch_loader::error_blink_code`). Introduce `CallbackCell<T>` to retire the
  `#![allow(static_mut_refs)]`.
- **F (graph-player CLAP):** new `sonido-plugin/src/graph_player/*` +
  `examples/sonido_graph_player.rs`; owns a `GraphEngine` (via
  `build_graph_from_patch`), `MacroMap<6>`, `ChainMorph`; state = patch JSON;
  reuses `macro_panel`/`morph_bar` + gui-core effect panels. Blocked until GL.

## Validate it (when you're back)

```sh
# 1) Cost table + budget logic is estimates until measured on-device — regenerate
#    real cycle counts with a bench_kernels DWT probe and update
#    crates/sonido-platform/src/cycle_table.rs.

# 2) QSPI read path (first hardware step):
sonido patch export --from-dsl "distortion:drive=25 | reverb:mix=40" -o p0.bin
dfu-util -a 0 -s 0x907F0000:leave -D p0.bin           # flash patch to slot 0
cd crates/sonido-daisy
cargo objcopy --example qspi_read_test --release --features alloc -- -O binary q.bin
dfu-util -a 0 -s 0x90040000:leave -D q.bin            # flash the probe
# watch RTT: expect JEDEC EF 40 17 and a slot-0 decode summary
```
