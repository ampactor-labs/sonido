# Sonido Pedal v2 — Static State Architecture

## Problem

The v1 pedal's audio callback closure captures ~10-15KB of state (preset buffer,
user presets, graph, nodes), inflating the embassy async future to 28KB BSS.
This causes SAI DMA TX overrun because the executor takes too long to poll the
large future between DMA transfers. The single_effect example (1.9KB BSS) works
because its closure is tiny.

## Solution

Move heavy state to statics (same pattern as `HothouseBuffer`). The audio
callback captures only `BypassCrossfade` + local buffers (<1KB). Target BSS <4KB.

## Hardware Target

Electrosmith Daisy Seed (STM32H750, Cortex-M7 480MHz) in Hothouse DIY enclosure.
6 knobs, 3 toggles, 2 footswitches, 2 LEDs.

## Effect List (Phase 1)

3 effects to prove the architecture:

| Index | Effect     | Kernel          |
|-------|------------|-----------------|
| 0     | Chorus     | ChorusKernel    |
| 1     | Distortion | DistortionKernel|
| 2     | Reverb     | ReverbKernel    |

Expand to full 15-effect list once audio is proven.

## Toggle Mapping

| Toggle | UP (0)         | MID (1)        | DOWN (2)             |
|--------|----------------|----------------|----------------------|
| **T1** | Node 1         | Node 2         | Node 3               |
| **T2** | A mode (edit)  | B mode (edit)  | Morph (FS ramp)      |
| **T3** | Linear 1->2->3 | Parallel split | Fan 1->split->[2,3]  |

## Footswitch Mapping

- **FS1 tap (A/B mode):** Scroll previous effect on focused node
- **FS2 tap (A/B mode):** Scroll next effect on focused node
- **FS1 hold (A mode):** Cycle factory preset
- **Both FS release:** Toggle global bypass
- **FS1 held (Morph mode):** Ramp morph toward A
- **FS2 held (Morph mode):** Ramp morph toward B

## Static Shared State

```rust
// Same pattern as HothouseBuffer — static + atomic/cell access
static CONTROLS: HothouseBuffer = HothouseBuffer::new();
static BYPASSED: AtomicBool = AtomicBool::new(false);
```

Heavy state wrapped in `Mutex<RefCell<T>>` for safe interior mutability:

```rust
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use core::cell::RefCell;

type SharedMutex<T> = Mutex<CriticalSectionRawMutex, RefCell<T>>;

static GRAPH: SharedMutex<Option<ProcessingGraph>> = Mutex::new(RefCell::new(None));
static NODES: SharedMutex<[NodeState; 3]> = ...;
```

The Mutex is a critical-section mutex (interrupt-disable), not a blocking mutex.
On a single-core Cortex-M7 with cooperative embassy executor, this is zero-cost —
there's no contention. It just satisfies Rust's safety requirements for mutable
statics.

## Audio Callback Flow

The callback closure captures only:
- `bypass_xfade: BypassCrossfade` (~24 bytes)
- Local audio buffers: `[f32; 32] x 4` (512 bytes)
- `poll_counter: u16`, `needs_rebuild: bool`, misc scalars (~100 bytes)

Total capture: <1KB. Target BSS: <4KB.

```
Every block (0.67ms at 48kHz/32 samples):
  1. BYPASSED check → hard copy input→output + minimal FS poll, return
  2. Deinterleave u24→f32
  3. GRAPH.lock → process_block → unlock
  4. Sanitize + bypass crossfade → f32→u24 output

Every 15th block (~100Hz control poll):
  5. Read toggles → focused_node, ab_mode, topology
  6. Footswitch state machine
  7. Knob reading + noon-biased mapping (A/B modes)
  8. Morph interpolation (Morph mode)
  9. Graph rebuild if needed
  10. LED feedback
```

## Init Sequence

Matches single_effect's proven pattern — no SDRAM, no milestones, no watchdog:

```
1. enable_d2_sram()
2. enable_fpu_ftz()
3. HEAP.init(0x3000_8000, 256KB)  — D2 SRAM heap
4. rcc_config(Performance) + hal::init()
5. Heartbeat task spawn
6. Control task spawn
7. LED1 on
8. Initialize GRAPH and NODES statics (build graph, load factory preset 1)
9. prepare_interface → start_interface → start_callback (SAI starts inside callback)
```

## Graph Rebuild Strategy

Rebuild happens inside the control poll (step 9), within the audio callback.
Single-threaded — no contention. At 480MHz, rebuild_graph takes ~50µs, well
within the 667µs block budget.

`needs_rebuild` is a local bool in the closure. Set by footswitch scroll or
topology change, consumed by the rebuild step. The GRAPH mutex is already held
during process_block, so rebuild just extends that critical section.

## Factory Presets

3 presets (same as v1):

1. "Room to Shimmer" — Reverb with morph from intimate room to infinite shimmer
2. "Slap to Self-Osc" — (uses Delay, not available in Phase 1 — skip or use Chorus)
3. "Clean to Saturated" — Distortion + Reverb

Adapt preset 2 to use available effects for Phase 1.

## File

`crates/sonido-daisy/examples/sonido_pedal_v2.rs`

Old `sonido_pedal.rs` kept for reference. Add `sonido_pedal_v2` to
`Cargo.toml` `[[example]]` with `required-features = ["alloc", "platform"]`.

## Reused Modules (no changes)

- `sonido_daisy::audio` (with start_fresh fix already in place)
- `sonido_daisy::controls::HothouseBuffer`
- `sonido_daisy::hothouse::hothouse_control_task`
- `sonido_daisy::heartbeat`, `led::UserLed`
- `sonido_daisy::tap_tempo::TapTempo`
- `sonido_platform::knob_mapping`
- `sonido_core::graph::ProcessingGraph`
- `sonido_core::kernel::Adapter`
- `sonido_core::TempoManager`
- `sonido_effects::{ChorusKernel, DistortionKernel, ReverbKernel}`

## Deferred (Phase 2+)

- Full 15-effect list
- QSPI preset persistence (as static + background save task)
- Watchdog + boot counter safe mode
- SDRAM heap (debug FMC/MPU init separately)
- Deferred D-cache
- Looper footswitch override
- Tap tempo (FS2 long-hold in B mode)

## Success Criteria

1. Audio passes through with effects processing (no SAI overrun)
2. Heartbeat LED stays alive (executor not starved)
3. Footswitches scroll effects and toggle bypass with LED feedback
4. Toggles switch nodes, A/B mode, and topology
5. Knobs control effect parameters with noon-biased mapping
6. BSS < 4KB (verified with `cargo size`)
