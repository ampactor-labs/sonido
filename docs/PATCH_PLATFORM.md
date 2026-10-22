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
 GUI session (v3)       CLAP plugin state          QSPI patch bank
 (+ node positions)     (graph-player)             firmware decodes; GUI writes via DFU
```

Effects are referenced by **stable UID** (`sonido_registry::EFFECT_UIDS`), never
by list position — positional indices rotted once already (the daisy-export
test drift fixed in Phase 0). The same Rust binary codec compiles into the host
tools (encode+decode) and the firmware (decode only): no second hand-mirrored
struct to drift.

Runtime layering — the consumers do **not** share one runtime:
- `MacroMap<6>` (`sonido-core`) — knob → `MacroTarget` (slot param or global),
  per-mapping range + `MorphCurve`, engine-decoupled sink apply. Used by the
  standalone GUI (`app.rs`, writing through the param bridge) and by
  `PatchPlayer` (graph-player plugin).
- `ChainMorph` (`sonido-core`) — per-slot `MorphSpace` + locks. Used only by
  `PatchPlayer`. The GUI's `MorphState` (`morph_state.rs`) is its own A/B
  implementation built on the same `curve_lerp` / `MorphCurve` primitives, so
  both interpolate per-parameter the same way (incl. logarithmic frequency
  morph and stepped snap).
- In `PatchPlayer`, per-control-tick order is **morph first, then macros**
  (macros win on overlap).
- The pedal firmware uses neither `MacroMap` nor `ChainMorph` (see below).

## What's built (committed, verified)

| Area | Where | Status |
|------|-------|--------|
| Phase 0 clean state | effects/cli/daisy | ✅ restored green gate (was red 3 ways) |
| Patch format + codec + validate | `crates/sonido-patch` | ✅ proptest roundtrips; no_std thumbv7em |
| Stable effect UIDs | `sonido-registry::EFFECT_UIDS` | ✅ totality/uniqueness test |
| CLI `sonido patch export\|inspect` | `sonido-cli/commands/patch.rs` | ✅ end-to-end verified |
| GUI session (v3; older files load via `serde(default)`) + `to_patch()` | `sonido-gui/session.rs` | ✅ 6 tests |
| Macro system | `sonido-core/macro_map.rs` | ✅ 10 tests |
| A/B morph unification + GUI log-fidelity fix | `sonido-core/kernel/chain_morph.rs`, `sonido-gui/morph_state.rs` | ✅ |
| `build_graph_from_patch` (arbitrary DAG) | `sonido-patch/build.rs` | ✅ host-tested with real effects |
| Firmware QSPI read + patch loader + factory | `sonido-daisy/{qspi_flash,patch_loader}.rs` | ✅ compiles thumbv7em |
| `qspi_read_test` hardware probe | `sonido-daisy/examples/` | ✅ compiles; **awaiting hardware** |
| Export logic (dfu / cost table / validate) | `sonido-gui/{dfu,export}.rs`, `sonido-platform/cycle_table.rs` | ✅ 14 tests |
| GUI macros: macro strip, right-click param → macro menu, auto-assign, macro editor | `sonido-gui/{app,macro_autoassign}.rs`, `sonido-gui-core/widgets/param_menu.rs` | ✅ wired |
| GUI A/B morph: `morph_bar` crossfader, per-node locks, `GlobalParam::MorphPosition` as macro target | `sonido-gui/{app,morph_state}.rs`, `sonido-gui-core/widgets/morph_bar.rs` | ✅ wired |
| GUI export menu: `.bin` / `.json` / CLAP preset, flash patch to slot 0 over DFU | `sonido-gui/app.rs` (`render_export_menu`) | ✅ wired |
| **PatchPlayer** runtime (macros+morph+gains over a graph) | `sonido-patch/runtime.rs` | ✅ 5 tests; no_std thumbv7em |
| **Graph-player CLAP plugin** (params/state/audio) | `sonido-plugin/graph_player.rs`, exported by the `sonido.clap` bundle as `com.sonido.graph-player` | ✅ 4 tests; in `make verify` |
| **Graph-player editor** (RIG cabinet: rig browser over factory rigs + the rig library, macros, morph, gains) | `sonido-plugin/graph_editor.rs` | ✅ tests; `sonido-preview graph-player` |
| Full `make verify` (fmt+clippy+tests+no_std+wasm+doc) | whole workspace | ✅ exits 0, zero warnings |

The plugin and the firmware share the effect kernels and the `Patch` format: the
firmware's `patch_loader.rs` decodes the 4 KB binary form and builds it with the
same `build_graph_from_patch`. They do not share a runtime. Only the
graph-player plugin wraps `PatchPlayer`; the firmware never uses it. The
shipping pedal (`examples/sonido_pedal.rs`) runs its own `ProcessingGraph` of up
to 3 of the 8 pedal effects through unsmoothed `Adapter::new_direct` wrappers
and does not load patches yet (workstream D below), so a rig is not guaranteed
to sound the same in the DAW and on the pedal.

## Remaining integration (needs hardware or a GUI pass)

1. **The QSPI-on-H7 read path is unvalidated.** A Seed and a Hothouse are on hand
   (2026-10-22) but `qspi_read_test` has not run yet; it is the first step.

Precise remaining wiring points, by workstream:

- **E (export panel, remainder):** the export menu flashes patches to slot 0
  only; a slot picker and a full-firmware flash button (`dfu::flash_firmware`
  exists but no UI calls it) are still to do.
- **D (firmware integration):** after `qspi_read_test` confirms QSPI, evolve
  `examples/sonido_pedal.rs` into `sonido_patch_player.rs` — load via
  `patch_loader::load_and_build`, drive `MacroMap<6>` from the knobs (pickup-lock),
  `ChainMorph` from the footswitch (Ramp/Momentary/Latch), wire `tap_tempo.rs`,
  keep the `GRAPH_UPDATING` rebuild + bypass crossfade, LED error codes
  (`patch_loader::error_blink_code`). Introduce `CallbackCell<T>` to retire the
  `#![allow(static_mut_refs)]`.

## Validate it (when you're back)

```sh
# 1) Cost table: measured on a Daisy Seed (2026-10-22, worst block per pedal
#    kernel). After changing a pedal kernel, re-run bench_kernels
#    (crates/sonido-daisy: cargo run --example bench_kernels --release) and
#    update crates/sonido-platform/src/cycle_table.rs.

# 2) QSPI read path (first hardware step):
sonido patch export --from-dsl "distortion:drive=25 | reverb:mix=40" -o p0.bin
dfu-util -a 0 -s 0x907F0000:leave -D p0.bin           # flash patch to slot 0
cd crates/sonido-daisy
cargo objcopy --example qspi_read_test --release --features alloc -- -O binary q.bin
dfu-util -a 0 -s 0x90040000:leave -D q.bin            # flash the probe
# watch RTT: expect JEDEC EF 40 17 and a slot-0 decode summary
```
