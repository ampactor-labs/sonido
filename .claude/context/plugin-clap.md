# CLAP Plugin Context

On-demand reference for CLAP plugin work in sonido-plugin.

## Architecture Overview

The plugin crate adapts sonido's `GraphEngine` + `KernelAdapter` to the CLAP host interface via clack. The `chain/` submodule handles the multi-effect chain plugin variant:

| File | Purpose |
|------|---------|
| `lib.rs` | Plugin adapter, CLAP entry point |
| `audio.rs` | Audio thread processing -- calls `GraphEngine::process_block()` |
| `gui.rs` | egui integration via `egui-baseview`. `#![allow(unsafe_code)]` scoped for `ParentWindow` `HasRawWindowHandle` impl |
| `main_thread.rs` | Main thread operations -- state save/load, parameter layout |
| `shared.rs` | Cross-thread shared state (`Arc`-based) |
| `egui_bridge/` | Bridges egui rendering into baseview window |
| `chain/mod.rs` | Chain plugin entry, multi-effect variant |
| `chain/shared.rs` | Chain-specific shared state |
| `chain/audio.rs` | Chain audio thread |
| `chain/main_thread.rs` | Chain main thread |
| `chain/param_bridge.rs` | `ChainParamBridge` -- plugin-only param bridge with `ChainMutator` |
| `chain/gui.rs` | Chain GUI integration |

## CLAP Threading Model

- **Audio thread**: `process()` called by host. Only lock-free operations. Reads params via atomics, processes audio through `GraphEngine`.
- **Main thread**: State save/load, parameter layout queries, GUI creation/destruction. May allocate.
- **GUI thread**: egui rendering via baseview. Communicates with audio via `AtomicParamBridge` (standalone) or `ChainParamBridge` (plugin).

## State Save/Load

Uses `GraphSnapshot` / `SnapshotEntry` from sonido-core:

```rust
// Save: serialize current engine state
let snap = engine.snapshot(); // GraphSnapshot { entries: Vec<SnapshotEntry> }
// Each SnapshotEntry: effect_id, param_values, bypassed

// Load: restore from snapshot
engine.restore(&snap);
```

## GUI Bridge

Two bridge patterns exist:

| Bridge | Context | Location |
|--------|---------|----------|
| `AtomicParamBridge` | Standalone GUI | `crates/sonido-gui/src/atomic_param_bridge.rs` |
| `ChainParamBridge` | Plugin GUI | `crates/sonido-plugin/src/chain/param_bridge.rs` |

- `AtomicParamBridge`: Uses ArcSwap slots for parameter exchange. `ChainMutator` removed (kept only in plugin's `ChainParamBridge`).
- `GraphCommand` channel: `Add` / `Remove` / `ReplaceTopology` messages from GUI to audio thread.
- `PendingResize`: Atomic channel for GUI resize requests. 320x240 min, 1920x1080 max.

## Plugin Entry Points

- 15 plugins total, one per effect + chain variant
- Per-effect entries generated via clack macros
- Build and install: `make plugins` -> installs to `~/.clap/`
- Each plugin wraps `KernelAdapter<XxxKernel>` with CLAP parameter mapping

## Pinned Versions

| Dependency | Pinned To |
|------------|-----------|
| egui-baseview | `ec70c3fe` |
| baseview | `9a0b42c0` |
| raw-window-handle | 0.5 |
| clack | rev `57e89b3` |

## Key Files

`crates/sonido-plugin/src/` -- lib.rs (adapter), audio.rs, gui.rs, main_thread.rs, shared.rs, egui_bridge/, chain/ (mod.rs, shared.rs, audio.rs, main_thread.rs, param_bridge.rs, gui.rs)

See ADR-024/026 in `docs/DESIGN_DECISIONS.md` for architectural rationale.
