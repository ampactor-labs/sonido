//! Morph Pedal v3 — DAG-based effect processor with per-node A/B morphing.
//!
//! Three toggle switches control node focus, A/B/morph mode, and routing
//! topology. Each of the 3 DAG slots has independent A/B parameter snapshots.
//! Dual footswitches scroll effects (A/B modes) or ramp morph position (morph
//! mode). Both footswitches = bypass toggle. FS1 hold in A mode = factory preset.
//!
//! # Toggle Mapping
//!
//! | Toggle | UP (0)         | MID (1)        | DOWN (2)                    |
//! |--------|----------------|----------------|-----------------------------|
//! | **T1** | Node 1         | Node 2         | Node 3                      |
//! | **T2** | A mode (edit)  | B mode (edit)  | Morph (FS1/FS2 ramp)        |
//! | **T3** | Linear 1→2→3   | Parallel split | Fan 1→split→[2,3]→merge     |
//!
//! # Hardware (Hothouse DIY)
//!
//! | Control     | Pin(s)               | Function                    |
//! |-------------|----------------------|-----------------------------|
//! | Knobs 1–6   | PA3,PB1,PA7,PA6,PC1,PC4 | Per-effect curated params |
//! | Toggle 1    | PB4/PB5              | Node select (1/2/3)         |
//! | Toggle 2    | PG10/PG11            | A / B / Morph               |
//! | Toggle 3    | PD2/PC12             | Routing topology            |
//! | Footswitch 1| PA0                  | Prev effect / morph→A       |
//! | Footswitch 2| PD11                 | Next effect / morph→B       |
//! | LED 1       | PA5                  | Active / bypassed           |
//! | LED 2       | PA4                  | A/B/morph feedback          |
//!
//! # Build & Flash
//!
//! ```bash
//! cd crates/sonido-daisy
//! cargo objcopy --example sonido_pedal --release -- -O binary -R .sram1_bss sonido_pedal.bin
//! dfu-util -a 0 -s 0x90040000:leave -D sonido_pedal.bin
//! ```

#![no_std]
#![no_main]
#![allow(clippy::needless_range_loop)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use defmt_rtt as _;
use embassy_stm32 as hal;
use embedded_alloc::LlffHeap as Heap;
use panic_probe as _;

use sonido_core::graph::ProcessingGraph;
use sonido_core::kernel::Adapter;
use sonido_core::{EffectWithParams, ParamFlags, TempoManager};
use sonido_daisy::controls::HothouseBuffer;
use sonido_daisy::expression::ExpressionInput;
use sonido_daisy::hothouse::hothouse_control_task;
use sonido_daisy::midi::{MidiEvent, MidiHandler};
use sonido_daisy::noon_presets;
use sonido_daisy::qspi::{EffectSlotData, MAX_USER_PRESETS, PresetSlot};
use sonido_daisy::tap_tempo::TapTempo;
use sonido_daisy::{
    BLOCK_SIZE, ClockProfile, SAMPLE_RATE, adc_to_param_biased, f32_to_u24, heartbeat,
    led::UserLed, u24_to_f32,
};
use sonido_effects::{
    BitcrusherKernel, ChorusKernel, CompressorKernel, DelayKernel, DistortionKernel, FilterKernel,
    FlangerKernel, LooperKernel, PhaserKernel, ReverbKernel, RingModKernel, TapeKernel,
    TremoloKernel, VibratoKernel, WahKernel,
};

// ── Heap ────────────────────────────────────────────────────────────────────

#[global_allocator]
static HEAP: Heap = Heap::empty();

// ── Shared control buffer ───────────────────────────────────────────────────

static CONTROLS: HothouseBuffer = HothouseBuffer::new();

// ── Constants ───────────────────────────────────────────────────────────────

/// Sentinel value for unmapped knob positions.
const NULL_KNOB: u8 = 0xFF;

/// Maximum parameters per effect slot (largest is Reverb with 10).
const MAX_PARAMS: usize = 16;

/// Number of effect slots.
const NUM_SLOTS: usize = 3;

/// Control poll rate: every 15th block ≈ 100 Hz at 48 kHz / 32 samples.
const POLL_EVERY: u16 = 15;

/// Footswitch tap threshold: 30 polls × ~10ms = 300ms.
const TAP_LIMIT: u16 = 30;

/// Number of effects in the curated list.
const NUM_EFFECTS: usize = 15;

// ── Curated Effect List ─────────────────────────────────────────────────────

/// Knob-to-parameter mapping entry for one effect.
///
/// Each effect has 6 knob slots mapped to specific parameter indices.
/// `NULL_KNOB` (0xFF) means the knob is inactive for this effect.
///
/// Consistent knob roles for muscle memory:
/// - K1: Primary (rate, cutoff, drive, threshold, frequency)
/// - K2: Secondary (depth, feedback, resonance, tone, ratio)
/// - K3: Color (damping, HF rolloff, stages, jitter, waveform)
/// - K4: Character (mode/shape, often STEPPED)
/// - K5: Mix (wet/dry blend)
/// - K6: Level (output/makeup gain) — **morph mode: morph speed**
struct EffectEntry {
    /// Registry ID for logging.
    id: &'static str,
    /// Knob-to-param-index mapping: `knobs[k]` = param index for knob K.
    knobs: [u8; 6],
}

/// 14 curated effects ordered chillest → gnarliest.
///
/// Verified parameter indices from kernel implementations.
/// `--` = `NULL_KNOB`. **(S)** = STEPPED (snaps at morph midpoint).
///
/// | # | Effect     | K1          | K2           | K3           | K4            | K5   | K6      |
/// |---|------------|-------------|--------------|--------------|---------------|------|---------|
/// | 0 | filter     | 0:Cutoff    | 1:Reso       | 3:Type(S)    | --            | --   | 2:Out   |
/// | 1 | tremolo    | 0:Rate      | 1:Depth      | 2:Wave(S)    | 3:Spread      | --   | 6:Out   |
/// | 2 | vibrato    | 0:Depth     | --           | --           | --            | 1:Mix| 2:Out   |
/// | 3 | chorus     | 0:Rate      | 1:Depth      | 4:Feedback   | 3:Voices(S)   | 2:Mix| 8:Out   |
/// | 4 | phaser     | 0:Rate      | 1:Depth      | 2:Stages(S)  | 3:Feedback    | 4:Mix| 9:Out   |
/// | 5 | flanger    | 0:Rate      | 1:Depth      | 2:Feedback   | 4:TZF(S)      | 3:Mix| 7:Out   |
/// | 6 | delay      | 0:Time      | 1:Feedback   | 4:FbLP       | 3:PingPong(S) | 2:Mix| 9:Out   |
/// | 7 | reverb     | 0:Room      | 1:Decay      | 2:Damping    | 3:PreDelay    | 4:Mix| 7:Out   |
/// | 8 | tape       | 0:Drive     | 1:Saturation | 2:HFRolloff  | 4:Wow         | 5:Flutter| 9:Out|
/// | 9 | compressor | 0:Threshold | 1:Ratio      | 2:Attack     | 3:Release     |10:Mix| 4:Makeup|
/// |10 | wah        | 0:Freq      | 1:Reso       | 2:Sensitivity| 3:Mode(S)     | --   | 4:Out   |
/// |11 | distortion | 0:Drive     | 1:Tone       | 3:Shape(S)   | 5:Dyn         | 4:Mix| 2:Out   |
/// |12 | bitcrusher | 0:Bits(S)   | 1:Down(S)    | 2:Jitter     | --            | 3:Mix| 4:Out   |
/// |13 | ringmod    | 0:Freq      | 1:Depth      | 2:Wave(S)    | --            | 3:Mix| 4:Out   |
/// |14 | looper     | 0:Mode(S)   | 1:Feedback   | 2:HalfSpd(S) | 3:Reverse(S)  | 4:Mix| 5:Out   |
const EFFECT_LIST: [EffectEntry; NUM_EFFECTS] = [
    EffectEntry {
        id: "filter",
        knobs: [0, 1, 3, NULL_KNOB, NULL_KNOB, 2],
    },
    EffectEntry {
        id: "tremolo",
        knobs: [0, 1, 2, 3, NULL_KNOB, 6],
    },
    EffectEntry {
        id: "vibrato",
        knobs: [0, NULL_KNOB, NULL_KNOB, NULL_KNOB, 1, 2],
    },
    EffectEntry {
        id: "chorus",
        knobs: [0, 1, 4, 3, 2, 8],
    },
    EffectEntry {
        id: "phaser",
        knobs: [0, 1, 2, 3, 4, 9],
    },
    EffectEntry {
        id: "flanger",
        knobs: [0, 1, 2, 4, 3, 7],
    },
    EffectEntry {
        id: "delay",
        knobs: [0, 1, 4, 3, 2, 9],
    },
    EffectEntry {
        id: "reverb",
        knobs: [0, 1, 2, 3, 4, 7],
    },
    EffectEntry {
        id: "tape",
        knobs: [0, 1, 2, 4, 5, 9],
    },
    EffectEntry {
        id: "compressor",
        knobs: [0, 1, 2, 3, 10, 4],
    },
    EffectEntry {
        id: "wah",
        knobs: [0, 1, 2, 3, NULL_KNOB, 4],
    },
    EffectEntry {
        id: "distortion",
        knobs: [0, 1, 3, 5, 4, 2],
    },
    EffectEntry {
        id: "bitcrusher",
        knobs: [0, 1, 2, NULL_KNOB, 3, 4],
    },
    EffectEntry {
        id: "ringmod",
        knobs: [0, 1, 2, NULL_KNOB, 3, 4],
    },
    EffectEntry {
        id: "looper",
        knobs: [0, 1, 2, 3, 4, 5],
    },
];

// ── Enums ───────────────────────────────────────────────────────────────────

/// A/B/Morph mode, selected by Toggle 2.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AbMode {
    /// Hearing and editing the A-state parameters.
    A,
    /// Hearing and editing the B-state parameters.
    B,
    /// Footswitch-controlled crossfade between A and B.
    Morph,
}

/// Audio routing topology, selected by Toggle 3.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Topology {
    /// Serial: 1 → 2 → 3
    Linear,
    /// Parallel: split → [1,2,3] → merge
    Parallel,
    /// Fan: 1 → split → [2,3] → merge
    Fan,
}

// ── Toggle parsing ─────────────────────────────────────────────────────────

/// Map toggle 1 position to focused node index (0, 1, or 2).
fn toggle_to_node(val: u8) -> usize {
    match val {
        0 => 0,
        2 => 2,
        _ => 1,
    }
}

/// Map toggle 2 position to A/B/Morph mode.
fn toggle_to_ab_mode(val: u8) -> AbMode {
    match val {
        0 => AbMode::A,
        2 => AbMode::Morph,
        _ => AbMode::B,
    }
}

/// Map toggle 3 position to routing topology.
fn toggle_to_topology(val: u8) -> Topology {
    match val {
        0 => Topology::Linear,
        2 => Topology::Fan,
        _ => Topology::Parallel,
    }
}

// ── Effect factory ──────────────────────────────────────────────────────────

/// Node ID type re-exported for convenience.
use sonido_core::graph::NodeId;

/// Create an effect by `EFFECT_LIST` index. Returns `None` for out-of-range.
///
/// Each arm wraps a `DspKernel` in `Adapter<K, DirectPolicy>` for zero-smoothing
/// `Effect + ParameterInfo`. Replaces `EffectRegistry` — we know our 14
/// effects at compile time.
fn create_effect(idx: usize, sr: f32) -> Option<Box<dyn EffectWithParams + Send>> {
    match idx {
        0 => Some(Box::new(Adapter::new_direct(FilterKernel::new(sr), sr))),
        1 => Some(Box::new(Adapter::new_direct(TremoloKernel::new(sr), sr))),
        2 => Some(Box::new(Adapter::new_direct(VibratoKernel::new(sr), sr))),
        3 => Some(Box::new(Adapter::new_direct(ChorusKernel::new(sr), sr))),
        4 => Some(Box::new(Adapter::new_direct(PhaserKernel::new(sr), sr))),
        5 => Some(Box::new(Adapter::new_direct(FlangerKernel::new(sr), sr))),
        6 => Some(Box::new(Adapter::new_direct(DelayKernel::new(sr), sr))),
        7 => Some(Box::new(Adapter::new_direct(ReverbKernel::new(sr), sr))),
        8 => Some(Box::new(Adapter::new_direct(TapeKernel::new(sr), sr))),
        9 => Some(Box::new(Adapter::new_direct(CompressorKernel::new(sr), sr))),
        10 => Some(Box::new(Adapter::new_direct(WahKernel::new(sr), sr))),
        11 => Some(Box::new(Adapter::new_direct(DistortionKernel::new(sr), sr))),
        12 => Some(Box::new(Adapter::new_direct(BitcrusherKernel::new(sr), sr))),
        13 => Some(Box::new(Adapter::new_direct(RingModKernel::new(sr), sr))),
        14 => Some(Box::new(Adapter::new_direct(LooperKernel::new(sr), sr))),
        _ => None,
    }
}

// ── Per-Slot Snapshot ───────────────────────────────────────────────────────

/// Parameter snapshot for one effect slot.
///
/// Stores parameter values and cached STEPPED flags for efficient morph
/// interpolation (STEPPED params snap at t=0.5, no fractional values).
#[derive(Clone)]
struct SlotSnapshot {
    /// Parameter values.
    values: [f32; MAX_PARAMS],
    /// Whether each param is STEPPED (cached at capture time).
    stepped: [bool; MAX_PARAMS],
    /// Number of valid parameters.
    count: usize,
}

impl SlotSnapshot {
    fn new() -> Self {
        Self {
            values: [0.0; MAX_PARAMS],
            stepped: [false; MAX_PARAMS],
            count: 0,
        }
    }

    /// Capture parameters and STEPPED flags from a graph node.
    fn capture_from(&mut self, graph: &ProcessingGraph, node_id: NodeId) {
        if let Some(effect) = graph.effect_with_params_ref(node_id) {
            let count = effect.effect_param_count().min(MAX_PARAMS);
            self.count = count;
            for p in 0..count {
                self.values[p] = effect.effect_get_param(p);
                self.stepped[p] = effect
                    .effect_param_info(p)
                    .is_some_and(|d| d.flags.contains(ParamFlags::STEPPED));
            }
        }
    }

    /// Apply snapshot values to a graph node.
    fn apply_to(&self, graph: &mut ProcessingGraph, node_id: NodeId) {
        if let Some(effect) = graph.effect_with_params_mut(node_id) {
            for p in 0..self.count {
                effect.effect_set_param(p, self.values[p]);
            }
        }
    }
}

// ── Per-Node State ──────────────────────────────────────────────────────────

/// Per-node state — each of the 3 DAG slots has independent A/B snapshots.
struct NodeState {
    /// Index into `EFFECT_LIST`, or `None` if slot is empty (passthrough).
    effect_index: Option<usize>,
    /// A-state parameter snapshot. Always populated once an effect is selected.
    params_a: SlotSnapshot,
    /// B-state parameter snapshot. `None` until user first enters B mode for
    /// this node. Initialized as clone of `params_a` on first B-mode entry.
    params_b: Option<SlotSnapshot>,
    /// Browse cursor for effect scrolling.
    browse_cursor: usize,
}

impl NodeState {
    fn new() -> Self {
        Self {
            effect_index: None,
            params_a: SlotSnapshot::new(),
            params_b: None,
            browse_cursor: 0,
        }
    }

    /// Write knob parameter values into the current A or B snapshot.
    fn update_snapshot(&mut self, mode: &AbMode, param_vals: &[(u8, f32); 6]) {
        match mode {
            AbMode::A => {
                for &(pidx, val) in param_vals {
                    if pidx != NULL_KNOB {
                        self.params_a.values[pidx as usize] = val;
                    }
                }
            }
            AbMode::B => {
                if let Some(ref mut b) = self.params_b {
                    for &(pidx, val) in param_vals {
                        if pidx != NULL_KNOB {
                            b.values[pidx as usize] = val;
                        }
                    }
                }
            }
            AbMode::Morph => {} // unreachable — caller guards
        }
    }
}

/// Ensure all populated nodes have B snapshots (cloned from A if missing).
fn ensure_b_snapshots(nodes: &mut [NodeState; NUM_SLOTS]) {
    for slot in 0..NUM_SLOTS {
        if nodes[slot].params_b.is_none() && nodes[slot].effect_index.is_some() {
            nodes[slot].params_b = Some(nodes[slot].params_a.clone());
        }
    }
}

/// Scroll the focused node's effect by `delta` (+1 = next, -1 = prev).
///
/// Resets A/B snapshots and sets `needs_rebuild = true`.
fn scroll_effect(nodes: &mut [NodeState; NUM_SLOTS], node_idx: usize, delta: i32) {
    let node = &mut nodes[node_idx];
    let cursor = ((node.browse_cursor as i32 + delta).rem_euclid(NUM_EFFECTS as i32)) as usize;
    node.browse_cursor = cursor;
    node.effect_index = Some(cursor);
    node.params_a = SlotSnapshot::new();
    node.params_b = None;
}

// ── Factory Presets ──────────────────────────────────────────────────────────

/// One slot in a factory preset — defines the effect and A/B parameter values.
struct FactorySlot {
    /// Index into `EFFECT_LIST`, or `None` for passthrough.
    effect_idx: Option<usize>,
    /// A-state parameter values in descriptor units.
    params_a: [f32; MAX_PARAMS],
    /// B-state parameter values in descriptor units.
    params_b: [f32; MAX_PARAMS],
    /// Cached STEPPED flags per parameter.
    stepped: [bool; MAX_PARAMS],
    /// Number of active parameters.
    count: usize,
}

impl FactorySlot {
    const EMPTY: Self = Self {
        effect_idx: None,
        params_a: [0.0; MAX_PARAMS],
        params_b: [0.0; MAX_PARAMS],
        stepped: [false; MAX_PARAMS],
        count: 0,
    };
}

/// A complete factory preset — all 3 DAG node slots.
struct FactoryPreset {
    slots: [FactorySlot; NUM_SLOTS],
}

/// Three factory presets for first-time demo and on-stage recovery.
///
/// Loaded via FS1 hold in A mode. Parameter values are in descriptor units
/// (the same units shown in GUIs and stored in presets).
const FACTORY_PRESETS: [FactoryPreset; 3] = [
    // Preset 1: "Room → Shimmer" — Reverb on node 1
    // Morph story: intimate bright room → infinite dark shimmer
    FactoryPreset {
        slots: [
            FactorySlot {
                effect_idx: Some(7), // reverb
                //                room  decay damp  pre   mix   width er    out
                params_a: [
                    30.0, 40.0, 60.0, 5.0, 40.0, 80.0, 50.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
                    0.0, 0.0,
                ],
                params_b: [
                    90.0, 88.0, 10.0, 30.0, 80.0, 100.0, 30.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
                    0.0, 0.0,
                ],
                stepped: [false; MAX_PARAMS],
                count: 8,
            },
            FactorySlot::EMPTY,
            FactorySlot::EMPTY,
        ],
    },
    // Preset 2: "Slap → Self-Osc" — Delay on node 1
    // Morph story: tight slapback → darkening delay wall approaching self-oscillation
    FactoryPreset {
        slots: [
            FactorySlot {
                effect_idx: Some(6), // delay
                //                time  fb    mix   ping  fblp     fbhp  diff  sync  div   out
                params_a: [
                    80.0, 15.0, 25.0, 0.0, 20000.0, 20.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0,
                    0.0, 0.0,
                ],
                params_b: [
                    400.0, 93.0, 70.0, 0.0, 3000.0, 100.0, 30.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0,
                    0.0, 0.0,
                ],
                stepped: [
                    false, false, false, true, false, false, false, true, true, false, false,
                    false, false, false, false, false,
                ],
                count: 10,
            },
            FactorySlot::EMPTY,
            FactorySlot::EMPTY,
        ],
    },
    // Preset 3: "Clean → Saturated" — Distortion + Reverb on nodes 1-2
    // Morph story: clean guitar + tight room → saturated drive + lush verb
    FactoryPreset {
        slots: [
            FactorySlot {
                effect_idx: Some(11), // distortion
                //                drive tone  out   shape mix   dyn
                params_a: [
                    0.0, 0.0, 0.0, 0.0, 100.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
                    0.0,
                ],
                params_b: [
                    32.0, -3.0, -6.0, 3.0, 100.0, 60.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
                    0.0, 0.0,
                ],
                stepped: [
                    false, false, false, true, false, false, false, false, false, false, false,
                    false, false, false, false, false,
                ],
                count: 6,
            },
            FactorySlot {
                effect_idx: Some(7), // reverb
                //                room  decay damp  pre   mix   width er    out
                params_a: [
                    20.0, 30.0, 50.0, 5.0, 20.0, 60.0, 40.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
                    0.0, 0.0,
                ],
                params_b: [
                    60.0, 65.0, 30.0, 15.0, 45.0, 100.0, 50.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
                    0.0, 0.0,
                ],
                stepped: [false; MAX_PARAMS],
                count: 8,
            },
            FactorySlot::EMPTY,
        ],
    },
];

/// Load a factory preset into the node state arrays.
///
/// Sets effect indices, browse cursors, and A/B snapshots for all 3 slots.
/// Caller must set `needs_rebuild = true` to trigger graph reconstruction.
fn load_factory_preset(nodes: &mut [NodeState; NUM_SLOTS], cursor: usize) {
    let preset = &FACTORY_PRESETS[cursor];
    for i in 0..NUM_SLOTS {
        let slot = &preset.slots[i];
        if let Some(idx) = slot.effect_idx {
            nodes[i].effect_index = Some(idx);
            nodes[i].browse_cursor = idx;
            nodes[i].params_a = SlotSnapshot {
                values: slot.params_a,
                stepped: slot.stepped,
                count: slot.count,
            };
            nodes[i].params_b = Some(SlotSnapshot {
                values: slot.params_b,
                stepped: slot.stepped,
                count: slot.count,
            });
        } else {
            nodes[i].effect_index = None;
            nodes[i].params_a = SlotSnapshot::new();
            nodes[i].params_b = None;
        }
    }
}

// ── Graph construction ──────────────────────────────────────────────────────

/// Build a `ProcessingGraph` from the current node configuration.
///
/// Empty slots are skipped — adjacent populated nodes connect directly.
/// Returns the compiled graph and node IDs for each slot (`None` for empty).
fn build_graph(
    nodes: &[NodeState; NUM_SLOTS],
    topology: Topology,
    sr: f32,
    bs: usize,
) -> Result<(ProcessingGraph, [Option<NodeId>; NUM_SLOTS]), sonido_core::graph::GraphError> {
    let mut g = ProcessingGraph::new(sr, bs);
    let inp = g.add_input();
    let out = g.add_output();

    // Collect populated slots: (slot_index, node_id)
    let mut populated: Vec<(usize, NodeId)> = Vec::new();
    let mut node_ids: [Option<NodeId>; NUM_SLOTS] = [None; NUM_SLOTS];

    for (slot, node) in nodes.iter().enumerate() {
        if let Some(effect_idx) = node.effect_index
            && let Some(effect) = create_effect(effect_idx, sr)
        {
            let nid = g.add_effect(effect);
            node_ids[slot] = Some(nid);
            populated.push((slot, nid));
        }
    }

    if populated.is_empty() {
        // Passthrough: Input → Output
        g.connect(inp, out).unwrap();
    } else if populated.len() == 1 {
        // Single effect: Input → E → Output
        let nid = populated[0].1;
        g.connect(inp, nid).unwrap();
        g.connect(nid, out).unwrap();
    } else {
        match topology {
            Topology::Linear => {
                g.connect(inp, populated[0].1).unwrap();
                for i in 1..populated.len() {
                    g.connect(populated[i - 1].1, populated[i].1).unwrap();
                }
                g.connect(populated[populated.len() - 1].1, out).unwrap();
            }
            Topology::Parallel => {
                let s = g.add_split();
                let m = g.add_merge();
                g.connect(inp, s).unwrap();
                for &(_, nid) in &populated {
                    g.connect(s, nid).unwrap();
                    g.connect(nid, m).unwrap();
                }
                g.connect(m, out).unwrap();
            }
            Topology::Fan => {
                // First populated → split → remaining → merge → output
                let s = g.add_split();
                let m = g.add_merge();
                g.connect(inp, populated[0].1).unwrap();
                g.connect(populated[0].1, s).unwrap();
                for &(_, nid) in &populated[1..] {
                    g.connect(s, nid).unwrap();
                    g.connect(nid, m).unwrap();
                }
                g.connect(m, out).unwrap();
            }
        }
    }

    g.compile()?;
    Ok((g, node_ids))
}

/// Rebuild an existing graph in-place, preserving crossfade state.
///
/// Clears the topology and re-adds effects based on current node configuration.
/// The built-in ~5ms crossfade in `compile()` handles click-free transitions.
fn rebuild_graph(
    graph: &mut ProcessingGraph,
    nodes: &[NodeState; NUM_SLOTS],
    topology: Topology,
    sr: f32,
) -> Result<[Option<NodeId>; NUM_SLOTS], sonido_core::graph::GraphError> {
    graph.clear_topology();

    let inp = graph.input_id().unwrap();
    let out = graph.output_id().unwrap();

    let mut populated: Vec<(usize, NodeId)> = Vec::new();
    let mut node_ids: [Option<NodeId>; NUM_SLOTS] = [None; NUM_SLOTS];

    for (slot, node) in nodes.iter().enumerate() {
        if let Some(effect_idx) = node.effect_index
            && let Some(effect) = create_effect(effect_idx, sr)
        {
            let nid = graph.add_effect(effect);
            node_ids[slot] = Some(nid);
            populated.push((slot, nid));
        }
    }

    if populated.is_empty() {
        graph.connect(inp, out)?;
    } else if populated.len() == 1 {
        let nid = populated[0].1;
        graph.connect(inp, nid)?;
        graph.connect(nid, out)?;
    } else {
        match topology {
            Topology::Linear => {
                graph.connect(inp, populated[0].1)?;
                for i in 1..populated.len() {
                    graph.connect(populated[i - 1].1, populated[i].1)?;
                }
                graph.connect(populated[populated.len() - 1].1, out)?;
            }
            Topology::Parallel => {
                let s = graph.add_split();
                let m = graph.add_merge();
                graph.connect(inp, s)?;
                for &(_, nid) in &populated {
                    graph.connect(s, nid)?;
                    graph.connect(nid, m)?;
                }
                graph.connect(m, out)?;
            }
            Topology::Fan => {
                let s = graph.add_split();
                let m = graph.add_merge();
                graph.connect(inp, populated[0].1)?;
                graph.connect(populated[0].1, s)?;
                for &(_, nid) in &populated[1..] {
                    graph.connect(s, nid)?;
                    graph.connect(nid, m)?;
                }
                graph.connect(m, out)?;
            }
        }
    }

    graph.compile()?;
    Ok(node_ids)
}

// ── Morph interpolation ─────────────────────────────────────────────────────

/// Apply interpolated A/B parameters to all nodes in the graph.
///
/// STEPPED params snap at `t=0.5`. Continuous params interpolate linearly.
/// Nodes without B snapshots stay at their A values (no change during morph).
fn interpolate_and_apply(
    graph: &mut ProcessingGraph,
    node_ids: &[Option<NodeId>; NUM_SLOTS],
    nodes: &[NodeState; NUM_SLOTS],
    t: f32,
) {
    for slot in 0..NUM_SLOTS {
        if let Some(nid) = node_ids[slot]
            && let Some(effect) = graph.effect_with_params_mut(nid)
        {
            let a = &nodes[slot].params_a;
            let b = match &nodes[slot].params_b {
                Some(b) => b,
                None => a, // No B → stays at A
            };
            let count = a.count.min(b.count);
            for p in 0..count {
                let val = if a.stepped[p] {
                    if t < 0.5 { a.values[p] } else { b.values[p] }
                } else {
                    a.values[p] + (b.values[p] - a.values[p]) * t
                };
                effect.effect_set_param(p, val);
            }
        }
    }
}

/// Apply the A or B snapshot for a single node to the graph.
fn apply_node_snapshot(
    graph: &mut ProcessingGraph,
    node_ids: &[Option<NodeId>; NUM_SLOTS],
    nodes: &[NodeState; NUM_SLOTS],
    slot: usize,
    mode: AbMode,
) {
    if let Some(nid) = node_ids[slot] {
        match mode {
            AbMode::A => nodes[slot].params_a.apply_to(graph, nid),
            AbMode::B => {
                if let Some(ref b) = nodes[slot].params_b {
                    b.apply_to(graph, nid);
                } else {
                    nodes[slot].params_a.apply_to(graph, nid);
                }
            }
            AbMode::Morph => {} // Morph handled by interpolate_and_apply
        }
    }
}

/// Apply all node snapshots (A or B) to the graph.
fn apply_all_snapshots(
    graph: &mut ProcessingGraph,
    node_ids: &[Option<NodeId>; NUM_SLOTS],
    nodes: &[NodeState; NUM_SLOTS],
    mode: AbMode,
) {
    for slot in 0..NUM_SLOTS {
        apply_node_snapshot(graph, node_ids, nodes, slot, mode);
    }
}

// ── MIDI event handler ──────────────────────────────────────────────────────

/// Process a single MIDI event into the control buffer.
///
/// Maps CC 0–5 to knobs, CC 11 to expression pedal, and MIDI Clock to tap
/// tempo. Called per USB-MIDI packet once the USB task is spawned. See the
/// TODO comment block in `main` for USB task setup.
#[allow(dead_code)]
fn process_midi_event(
    event: &MidiEvent,
    controls: &HothouseBuffer,
    tap_tempo: &mut TapTempo,
    now_ticks: u64,
) {
    if event.is_cc() {
        match event.cc_number() {
            // CC 0–5 → knobs 0–5 (normalized 0.0–1.0 from 7-bit MIDI value).
            0..=5 => {
                let knob_idx = event.cc_number() as usize;
                let normalized = event.cc_value() as f32 / 127.0;
                // Write with alpha=1.0 (no IIR smoothing — MIDI is already smooth).
                controls.write_knob(knob_idx, normalized, 1.0);
            }
            // CC 11 (Expression) → expression pedal value.
            11 => {
                let normalized = event.cc_value() as f32 / 127.0;
                controls.write_expression(normalized);
            }
            _ => {}
        }
    } else if event.is_clock() {
        // MIDI Clock fires 24× per beat. Divide down to 1 tap per beat.
        let count = MIDI_CLOCK_DIV.fetch_add(1, Ordering::Relaxed);
        if count == 0 {
            tap_tempo.tap(now_ticks);
        }
        if count >= 23 {
            MIDI_CLOCK_DIV.store(0, Ordering::Relaxed);
        }
    } else if event.is_start() {
        // MIDI Start: reset divider so first clock triggers a tap.
        MIDI_CLOCK_DIV.store(0, Ordering::Relaxed);
    } else if event.is_stop() {
        // MIDI Stop: reset divider; no more taps until next Start.
        MIDI_CLOCK_DIV.store(0, Ordering::Relaxed);
    }
    // Program Change → preset load handled in the poll loop where preset state lives.
}

// ── Preset serialization ────────────────────────────────────────────────────

/// Capture current pedal state into a [`PresetSlot`] for persistence.
///
/// Topology is encoded as 0=Linear, 1=Parallel, 2=Fan matching the [`Topology`]
/// enum discriminants used in [`PresetSlot::topology`].
fn current_state_to_preset_slot(nodes: &[NodeState; NUM_SLOTS], topology: Topology) -> PresetSlot {
    let topology_byte = match topology {
        Topology::Linear => 0,
        Topology::Parallel => 1,
        Topology::Fan => 2,
    };

    let mut slot = PresetSlot {
        valid: 0x01,
        topology: topology_byte,
        num_slots: 0,
        _pad: 0,
        effects: [EffectSlotData::default(); 3],
    };

    let mut active = 0u8;
    for (i, node) in nodes.iter().enumerate() {
        if let Some(eff_idx) = node.effect_index {
            slot.effects[i].effect_idx = eff_idx as u8;
            slot.effects[i].param_count = node.params_a.count as u8;
            for p in 0..node.params_a.count.min(sonido_daisy::qspi::MAX_SLOT_PARAMS) {
                slot.effects[i].params_a[p] = node.params_a.values[p];
                slot.effects[i].params_b[p] = node
                    .params_b
                    .as_ref()
                    .map_or(node.params_a.values[p], |b| b.values[p]);
            }
            active += 1;
        }
    }
    slot.num_slots = active;
    slot
}

// ── Init diagnostics ────────────────────────────────────────────────────────

/// Single blink on LED2 for init milestone tracking.
///
/// Count the LED2 blinks to identify the last milestone reached before a crash.
async fn milestone(controls: &HothouseBuffer) {
    controls.write_led(1, 1.0);
    embassy_time::Timer::after_millis(200).await;
    controls.write_led(1, 0.0);
    embassy_time::Timer::after_millis(400).await;
}

// ── Bypass state ────────────────────────────────────────────────────────────

/// Global bypass flag — audio callback checks this.
static BYPASSED: AtomicBool = AtomicBool::new(false);

/// MIDI clock divider counter (0–23). MIDI clock fires 24× per beat;
/// `TapTempo` expects ~1 tap per beat. Only the first of every 24 clocks
/// triggers a tap.
static MIDI_CLOCK_DIV: AtomicU8 = AtomicU8::new(0);

// ── Deferred D-cache ────────────────────────────────────────────────────────

// ── Boot counter (TAMP backup register) ─────────────────────────────────────

/// TAMP backup register 0 — survives IWDG and software resets.
///
/// Address: 0x5800_2100 (TAMP_BKP0R on STM32H750, per RM0433 §8.5.20).
/// Requires RTCAPBEN (RCC_APB4ENR bit 16) — enabled below in `read_boot_count`.
const TAMP_BKP0R: *mut u32 = 0x5800_2100 as *mut u32;

/// RCC APB4 peripheral clock enable register.
const RCC_APB4ENR: *mut u32 = 0x5802_40F4 as *mut u32;

/// Reads the boot counter from TAMP backup register 0 (low byte).
///
/// # Safety
///
/// Writes to RCC and reads TAMP MMIO. Must be called after RCC is configured
/// (embassy-stm32 init guarantees this).
unsafe fn read_boot_count() -> u8 {
    unsafe {
        // Ensure TAMP clock is enabled (RTCAPBEN = bit 16 of RCC_APB4ENR).
        let apb4 = core::ptr::read_volatile(RCC_APB4ENR);
        core::ptr::write_volatile(RCC_APB4ENR, apb4 | (1 << 16));
        (core::ptr::read_volatile(TAMP_BKP0R) & 0xFF) as u8
    }
}

/// Writes the boot counter to TAMP backup register 0 (low byte).
///
/// # Safety
///
/// Writes to TAMP MMIO. RTCAPBEN must already be enabled (see `read_boot_count`).
unsafe fn write_boot_count(count: u8) {
    unsafe {
        let prev = core::ptr::read_volatile(TAMP_BKP0R) & !0xFF;
        core::ptr::write_volatile(TAMP_BKP0R, prev | count as u32);
    }
}

// ── Watchdog ─────────────────────────────────────────────────────────────────

/// Watchdog task — feeds the STM32H750 IWDG every 500 ms.
///
/// The IWDG has a ~1 second timeout (configured via prescaler + reload
/// register). If this task is starved (e.g., the audio callback hangs or the
/// executor deadlocks), the MCU resets after ~1 s.
///
/// # embassy-stm32 IWDG status
///
/// As of embassy-stm32 0.5 the `Iwdg` driver exists for STM32H7 targets but
/// the peripheral is not yet mapped in the `stm32h750ib` PAC feature. Until
/// that mapping ships, the implementation below uses a direct register write
/// to the IWDG key register (0x4000_3000) to pet the watchdog.
///
/// TODO: replace raw pointer writes with `embassy_stm32::iwdg::IndependentWatchdog`
///       once the HAL adds stm32h750ib support.
///
/// # Register-level fallback (current)
///
/// IWDG_KR  = 0x4000_3000: write 0xAAAA to reload, 0x5555 to unlock, 0xCCCC to start.
/// IWDG_PR  = 0x4000_3004: prescaler — 0b100 = /64 → 625 Hz LSI tick
/// IWDG_RLR = 0x4000_3008: reload value — 625 ticks ≈ 1.0 s timeout
///
/// LSI clock on STM32H750 is ~32 kHz. With /64 prescaler: 32000/64 = 500 Hz.
/// Reload of 500 → ~1 s timeout. Feed every 500 ms gives 2× safety margin.
#[embassy_executor::task]
async fn watchdog_task() {
    // SAFETY: Writes to IWDG MMIO registers. The IWDG is an independent
    // peripheral — once started it cannot be stopped. Ensure this task is
    // spawned unconditionally so it always feeds on schedule.
    unsafe {
        const IWDG_BASE: u32 = 0x4000_3000;
        const IWDG_KR: *mut u32 = IWDG_BASE as *mut u32;
        const IWDG_PR: *mut u32 = (IWDG_BASE + 0x04) as *mut u32;
        const IWDG_RLR: *mut u32 = (IWDG_BASE + 0x08) as *mut u32;

        // Unlock PR and RLR registers.
        core::ptr::write_volatile(IWDG_KR, 0x5555);
        // Prescaler /64 → ~500 Hz LSI tick rate.
        core::ptr::write_volatile(IWDG_PR, 0b100);
        // Reload = 500 → ~1.0 s timeout.
        core::ptr::write_volatile(IWDG_RLR, 500);
        // Start the watchdog.
        core::ptr::write_volatile(IWDG_KR, 0xCCCC);
    }

    // Read persistent boot counter, increment, write back.
    let boot_count = unsafe {
        let count = read_boot_count();
        let next = count.saturating_add(1);
        write_boot_count(next);
        next
    };
    defmt::info!("watchdog started (boot #{})", boot_count);

    // 3+ consecutive rapid reboots → force bypass (safe mode).
    if boot_count >= 3 {
        defmt::warn!("safe mode: {} rapid reboots, forcing bypass", boot_count);
        BYPASSED.store(true, Ordering::Relaxed);
    }

    loop {
        // Feed the watchdog — must happen within the ~1 s timeout window.
        // SAFETY: Reload key register write; no side effects beyond resetting
        // the IWDG down-counter.
        unsafe {
            core::ptr::write_volatile(0x4000_3000u32 as *mut u32, 0xAAAA);
        }
        embassy_time::Timer::after_millis(500).await;
    }
}

/// Enables D-cache ~500ms after boot.
///
/// D-cache must be enabled AFTER SAI DMA is running — enabling during DMA
/// init stalls the bus matrix and starves DMA (SAI overrun).
#[embassy_executor::task]
async fn deferred_dcache() {
    embassy_time::Timer::after_millis(500).await;
    sonido_daisy::sdram::enable_dcache();
    defmt::info!("D-cache enabled");
}

/// Clears the boot counter after 5 seconds of stable operation.
///
/// If audio runs cleanly for 5 s without a watchdog reset, the firmware
/// is healthy — clear the counter so future single resets don't trigger
/// safe mode.
#[embassy_executor::task]
async fn boot_success_guard() {
    embassy_time::Timer::after_secs(5).await;
    unsafe { write_boot_count(0) };
    defmt::info!("boot success — counter cleared");
}

// ── Main ────────────────────────────────────────────────────────────────────

#[embassy_executor::main]
async fn main(spawner: embassy_executor::Spawner) {
    let config = sonido_daisy::rcc_config(ClockProfile::Performance);
    let p = hal::init(config);

    // D2 SRAM clocks — needed for DMA buffers (.sram1_bss at 0x30000000).
    sonido_daisy::enable_d2_sram();

    // FPU flush-to-zero — hardware flushes denormals, saving ~5-10% DSP CPU.
    sonido_daisy::enable_fpu_ftz();

    // SDRAM heap — 64 MB via FMC. All DSP allocations go here.
    let mut cp = unsafe { cortex_m::Peripherals::steal() };
    let sdram_ptr = sonido_daisy::init_sdram!(p, &mut cp.MPU, &mut cp.SCB);
    unsafe {
        HEAP.init(sdram_ptr as usize, sonido_daisy::sdram::SDRAM_SIZE);
    }

    // Heartbeat LED (PC7 = Daisy Seed user LED)
    let led = UserLed::new(p.PC7);
    spawner.spawn(heartbeat(led)).unwrap();

    defmt::info!("sonido_pedal v3: initializing...");

    // ── Extract control pins and spawn control task ──
    // Must happen BEFORE audio start_interface → start_callback.
    let ctrl = sonido_daisy::hothouse_pins!(p);
    spawner
        .spawn(hothouse_control_task(ctrl, &CONTROLS))
        .unwrap();

    // LED1 starts on (active indicator).
    CONTROLS.write_led(0, 1.0);

    defmt::info!("sonido_pedal v3: controls initialized");

    // ── Initial state: all slots empty, passthrough ──

    let mut nodes: [NodeState; NUM_SLOTS] = core::array::from_fn(|_| NodeState::new());

    // Read initial toggle positions from ControlBuffer.
    // Give the control task one cycle to populate.
    embassy_time::Timer::after_millis(30).await;

    let t1_init = CONTROLS.read_toggle(0);
    let mut focused_node = toggle_to_node(t1_init);

    let t2_init = CONTROLS.read_toggle(1);
    let mut ab_mode = toggle_to_ab_mode(t2_init);

    let t3_init = CONTROLS.read_toggle(2);
    let mut topology = toggle_to_topology(t3_init);

    defmt::info!(
        "sonido_pedal v3: toggles — node={}, ab={}, topo={}",
        focused_node + 1,
        t2_init,
        t3_init
    );

    // Auto-load factory preset 1 for first-time experience.
    load_factory_preset(&mut nodes, 0);
    let (mut graph, mut node_ids) = build_graph(&nodes, topology, SAMPLE_RATE, BLOCK_SIZE).unwrap();
    apply_all_snapshots(&mut graph, &node_ids, &nodes, AbMode::A);
    defmt::info!("factory preset 1 loaded: Room → Shimmer");

    // Morph state
    let mut morph_t: f32 = 0.0; // 0.0 = A, 1.0 = B
    let mut morph_speed: f32 = 2.0; // seconds for full morph

    // Factory preset state
    let mut factory_cursor: usize = 0;
    let mut led_blink_remaining: u8 = 0;
    let mut led_blink_timer: u16 = 0;

    // ── Tap tempo + TempoManager ─────────────────────────────────────────────
    let mut tap_tempo = TapTempo::new();
    let mut tempo_manager = TempoManager::new(SAMPLE_RATE, 120.0);

    // ── Expression pedal ─────────────────────────────────────────────────────
    let mut expression = ExpressionInput::new();

    // ── MIDI ─────────────────────────────────────────────────────────────────
    // TODO: Spawn USB MIDI task
    // let usb_driver = embassy_stm32::usb::Driver::new(p.USB_OTG_FS, irqs, p.PA12, p.PA11, &mut ep_out_buffer, config);
    // let mut midi_config = embassy_usb::Config::new(0x1209, 0x0001);
    // midi_config.manufacturer = Some("Ampactor Labs");
    // midi_config.product = Some("Sonido Pedal");
    // midi_config.serial_number = Some("00000001");
    // spawner.spawn(usb_task(usb_device)).unwrap();
    // spawner.spawn(usb_midi_task(usb_driver, midi_handler)).unwrap();
    //
    // When the USB task is live, received 4-byte packets are dispatched via:
    //   if let Some(event) = midi.parse_packet(&packet) {
    //       process_midi_event(&event, &CONTROLS, &mut tap_tempo,
    //                          embassy_time::Instant::now().as_ticks());
    //   }
    let _midi = MidiHandler::new();

    // ── QSPI preset persistence ───────────────────────────────────────────────
    // Preset RAM buffer (4 KB).  On boot we'd read from QSPI flash into this
    // buffer and deserialize with PresetStore.  Writing back is deferred until
    // save_debounce fires.
    let mut preset_buffer = [0xFFu8; sonido_daisy::qspi::PRESET_SECTOR_SIZE];
    let mut user_presets: [Option<PresetSlot>; MAX_USER_PRESETS] = [None; MAX_USER_PRESETS];
    // On boot: init buffer and attempt to load user presets.
    {
        use sonido_daisy::qspi::PresetStore;
        let mut store = PresetStore::new(&mut preset_buffer);
        // TODO: Read from QSPI flash into preset_buffer before constructing store:
        // qspi.read(PRESET_SECTOR_ADDR, &mut preset_buffer).unwrap();
        let header = store.header();
        if header.is_valid() {
            user_presets = store.load_all();
            defmt::info!("QSPI: loaded {} user presets", header.count);
        } else {
            // No valid data — initialize empty store in RAM.
            store.init_empty();
            defmt::info!("QSPI: no valid presets, starting empty");
        }
    }
    let mut active_preset: usize = 0xFF; // 0xFF = no user preset active
    // Auto-save debounce: counts down poll ticks (~50 Hz).  3 seconds ≈ 150 ticks.
    let mut save_debounce: u32 = 0;

    // Effect-aware LED state
    let mut led_phase: f32 = 0.0; // LFO phase for modulation effects
    let mut led_envelope: f32 = 0.0; // one-pole follower for envelope effects
    let mut led_tap_counter: u32 = 0; // delay tap flash counter

    // Looper footswitch override: true when FS has set mode, K1 skipped
    let mut looper_fs_override: bool = false;

    // Footswitch state machine
    let mut fs1_held: u32 = 0;
    let mut fs2_held: u32 = 0;
    let mut both_held: u16 = 0;
    let mut both_held_peak: u16 = 0;
    let mut fs1_was_pressed = false;
    let mut fs2_was_pressed = false;

    let mut poll_counter: u16 = 0;
    let mut needs_rebuild = false;

    // Pre-allocate audio buffers for deinterleave/reinterleave.
    let mut left_in = [0.0f32; BLOCK_SIZE];
    let mut right_in = [0.0f32; BLOCK_SIZE];
    let mut left_out = [0.0f32; BLOCK_SIZE];
    let mut right_out = [0.0f32; BLOCK_SIZE];

    // ── Milestones before SAI starts ──
    milestone(&CONTROLS).await; // 1: init complete (controls + graph)
    milestone(&CONTROLS).await; // 2: ready to start audio

    // Spawn deferred D-cache BEFORE audio setup.
    spawner.spawn(deferred_dcache()).unwrap();

    // Spawn watchdog — must be alive for entire session.
    spawner.spawn(watchdog_task()).unwrap();

    // Clear boot counter after 5s of stable operation.
    spawner.spawn(boot_success_guard()).unwrap();

    // ── Audio setup — start SAI as late as possible ──
    let audio_peripherals = sonido_daisy::audio::AudioPeripherals {
        codec_pins: sonido_daisy::codec_pins!(p),
        sai1: p.SAI1,
        dma1_ch0: p.DMA1_CH0,
        dma1_ch1: p.DMA1_CH1,
    };
    let interface = audio_peripherals
        .prepare_interface(Default::default())
        .await;
    milestone(&CONTROLS).await; // 3: codec configured, about to start SAI

    let mut interface = match interface.start_interface().await {
        Ok(running) => running,
        Err(_e) => {
            defmt::error!("SAI start_interface failed");
            loop {
                CONTROLS.write_led(1, 1.0);
                embassy_time::Timer::after_millis(50).await;
                CONTROLS.write_led(1, 0.0);
                embassy_time::Timer::after_millis(50).await;
            }
        }
    };
    defmt::info!("SAI started — entering audio callback");

    match interface
        .start_callback(move |input, output| {
            // ── Bypass passthrough ──
            if BYPASSED.load(Ordering::Relaxed) {
                output.copy_from_slice(input);
                return;
            }

            // ── Deinterleave u32 → f32 ──
            for i in 0..BLOCK_SIZE {
                left_in[i] = u24_to_f32(input[i * 2]);
                right_in[i] = u24_to_f32(input[i * 2 + 1]);
            }

            // ── Propagate tempo context to effects (once per block) ──
            let tempo_ctx = tempo_manager.snapshot();
            graph.set_tempo_context(&tempo_ctx);

            // ── Process through graph ──
            graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);

            // ── Reinterleave f32 → u32 ──
            for i in 0..BLOCK_SIZE {
                output[i * 2] = f32_to_u24(left_out[i]);
                output[i * 2 + 1] = f32_to_u24(right_out[i]);
            }

            // ── Control poll (~100 Hz) ──
            poll_counter = poll_counter.wrapping_add(1);
            if !poll_counter.is_multiple_of(POLL_EVERY) {
                return;
            }

            // ── 1. Read toggles ──
            let t1 = CONTROLS.read_toggle(0);
            let t2 = CONTROLS.read_toggle(1);
            let t3 = CONTROLS.read_toggle(2);

            // ── 2. Handle T3 change → topology ──
            let new_topology = toggle_to_topology(t3);
            if new_topology != topology {
                topology = new_topology;
                needs_rebuild = true;
            }

            // ── 3. Handle T1 change → focused node ──
            let new_focused = toggle_to_node(t1);
            if new_focused != focused_node {
                focused_node = new_focused;
                // Apply current A/B snapshot for the new focused node.
                if ab_mode != AbMode::Morph {
                    apply_node_snapshot(&mut graph, &node_ids, &nodes, focused_node, ab_mode);
                }
                defmt::info!("node → {}", focused_node + 1);
            }

            // ── 4. Handle T2 change → A/B/Morph mode ──
            let new_ab = toggle_to_ab_mode(t2);
            if new_ab != ab_mode {
                let old_mode = ab_mode;
                ab_mode = new_ab;

                match (old_mode, ab_mode) {
                    (AbMode::A, AbMode::B) => {
                        ensure_b_snapshots(&mut nodes);
                        apply_all_snapshots(&mut graph, &node_ids, &nodes, AbMode::B);
                        defmt::info!("→ B mode");
                    }
                    (AbMode::B, AbMode::A) => {
                        apply_all_snapshots(&mut graph, &node_ids, &nodes, AbMode::A);
                        defmt::info!("→ A mode");
                    }
                    (_, AbMode::Morph) => {
                        ensure_b_snapshots(&mut nodes);
                        // Set morph_t based on which mode we came from.
                        morph_t = match old_mode {
                            AbMode::A => 0.0,
                            AbMode::B => 1.0,
                            AbMode::Morph => morph_t, // shouldn't happen
                        };
                        defmt::info!("→ MORPH mode (t={})", morph_t);
                    }
                    (AbMode::Morph, AbMode::A) => {
                        apply_all_snapshots(&mut graph, &node_ids, &nodes, AbMode::A);
                        defmt::info!("→ A mode (from morph)");
                    }
                    (AbMode::Morph, AbMode::B) => {
                        ensure_b_snapshots(&mut nodes);
                        apply_all_snapshots(&mut graph, &node_ids, &nodes, AbMode::B);
                        defmt::info!("→ B mode (from morph)");
                    }
                    _ => {} // same mode
                }
            }

            // ── 5. Footswitch state machine ──
            let fs1_pressed = CONTROLS.read_footswitch(0);
            let fs2_pressed = CONTROLS.read_footswitch(1);
            let both_pressed = fs1_pressed && fs2_pressed;

            if both_pressed {
                both_held += 1;
                if both_held > both_held_peak {
                    both_held_peak = both_held;
                }
            } else {
                both_held = 0;
            }

            if fs1_pressed {
                fs1_held += 1;
            }
            if fs2_pressed {
                fs2_held += 1;
            }

            // MORPH mode: continuous ramp while held.
            if ab_mode == AbMode::Morph && !both_pressed {
                let delta = 1.0 / (morph_speed * 100.0);
                if fs1_pressed && !fs2_pressed {
                    morph_t = if morph_t > delta {
                        morph_t - delta
                    } else {
                        0.0
                    };
                } else if fs2_pressed && !fs1_pressed {
                    morph_t = if morph_t + delta < 1.0 {
                        morph_t + delta
                    } else {
                        1.0
                    };
                }
            }

            // Both-FS release = bypass toggle (any duration, any mode).
            let was_both = both_held_peak > 0;
            if was_both && !fs1_pressed && !fs2_pressed {
                let was_bypassed = BYPASSED.load(Ordering::Relaxed);
                BYPASSED.store(!was_bypassed, Ordering::Relaxed);
                if was_bypassed {
                    CONTROLS.write_led(0, 1.0);
                } else {
                    CONTROLS.write_led(0, 0.0);
                    CONTROLS.write_led(1, 0.0);
                }
            }

            if !fs1_pressed && !fs2_pressed {
                both_held_peak = 0;
            }

            // ── A/B mode: looper FS control or normal scroll ──
            // Check if focused node is a looper in active (non-Stop) mode.
            let looper_active = (ab_mode == AbMode::A || ab_mode == AbMode::B)
                && !was_both
                && nodes[focused_node].effect_index == Some(14)
                && node_ids[focused_node]
                    .and_then(|nid| graph.effect_with_params_ref(nid))
                    .map_or(false, |e| e.effect_get_param(0) >= 0.5);

            if looper_active {
                // Looper FS: FS1 tap = toggle Record↔Play.
                if fs1_was_pressed && !fs1_pressed && fs1_held < TAP_LIMIT as u32 {
                    if let Some(nid) = node_ids[focused_node] {
                        let cur = graph
                            .effect_with_params_ref(nid)
                            .map_or(0.0, |e| e.effect_get_param(0));
                        // Record(1)↔Play(2), Overdub(3)→Play(2)
                        let new_mode = if (cur as u8) == 2 || (cur as u8) == 3 {
                            1.0
                        } else {
                            2.0
                        };
                        if let Some(e) = graph.effect_with_params_mut(nid) {
                            e.effect_set_param(0, new_mode);
                        }
                        looper_fs_override = true;
                    }
                }
                // Looper FS: FS2 tap = Stop.
                if fs2_was_pressed && !fs2_pressed && fs2_held < TAP_LIMIT as u32 {
                    if let Some(nid) = node_ids[focused_node] {
                        if let Some(e) = graph.effect_with_params_mut(nid) {
                            e.effect_set_param(0, 0.0);
                        }
                    }
                    looper_fs_override = false;
                }
            } else if (ab_mode == AbMode::A || ab_mode == AbMode::B) && !was_both {
                // Normal scroll + factory preset.
                // FS1 tap = scroll previous effect.
                if fs1_was_pressed && !fs1_pressed && fs1_held < TAP_LIMIT as u32 {
                    scroll_effect(&mut nodes, focused_node, -1);
                    needs_rebuild = true;
                    looper_fs_override = false;
                    defmt::info!(
                        "node {} ← {}",
                        focused_node + 1,
                        EFFECT_LIST[nodes[focused_node].browse_cursor].id
                    );
                }

                // FS1 hold in A mode = cycle factory preset.
                if ab_mode == AbMode::A
                    && fs1_was_pressed
                    && !fs1_pressed
                    && fs1_held >= TAP_LIMIT as u32
                {
                    factory_cursor = (factory_cursor + 1) % FACTORY_PRESETS.len();
                    load_factory_preset(&mut nodes, factory_cursor);
                    needs_rebuild = true;
                    looper_fs_override = false;
                    led_blink_remaining = (factory_cursor as u8 + 1) * 2;
                    led_blink_timer = 0;
                    defmt::info!("factory preset {}", factory_cursor + 1);
                }

                // FS2 tap = scroll next effect.
                if fs2_was_pressed && !fs2_pressed && fs2_held < TAP_LIMIT as u32 {
                    scroll_effect(&mut nodes, focused_node, 1);
                    needs_rebuild = true;
                    looper_fs_override = false;
                    defmt::info!(
                        "node {} → {}",
                        focused_node + 1,
                        EFFECT_LIST[nodes[focused_node].browse_cursor].id
                    );
                }

                // FS2 long-hold in B-mode = tap tempo.
                // Kept separate from scroll: released after TAP_LIMIT means hold.
                if ab_mode == AbMode::B
                    && fs2_was_pressed
                    && !fs2_pressed
                    && fs2_held >= TAP_LIMIT as u32
                {
                    tap_tempo.tap(embassy_time::Instant::now().as_ticks());
                    // Brief LED2 flash to confirm the tap was registered.
                    led_blink_remaining = 1;
                    led_blink_timer = 0;
                    defmt::info!("tap tempo tap");
                }
            }

            // Reset hold counters on release.
            if !fs1_pressed {
                fs1_held = 0;
            }
            if !fs2_pressed {
                fs2_held = 0;
            }
            fs1_was_pressed = fs1_pressed;
            fs2_was_pressed = fs2_pressed;

            // ── 5b. Expression pedal update ──
            // Update expression processor with the latest value from ControlBuffer.
            // The hothouse control task writes CONTROLS.write_expression() from the
            // ADC channel wired to the TRS expression jack.
            //
            // TODO (hothouse.rs): In hothouse_control_task, read the expression ADC
            // channel and call: CONTROLS.write_expression(expr_raw as f32 / 65535.0);
            expression.update(CONTROLS.read_expression());

            // In morph mode: expression pedal overrides morph_t when connected.
            if ab_mode == AbMode::Morph && expression.is_connected() {
                morph_t = expression.value();
            }

            // ── 5c. Tap tempo: BPM → TempoManager ──
            // (Tap is triggered in section 5 footswitch handling for FS2 long-hold
            // in B-mode; here we flush any new BPM into the TempoManager.)
            if let Some(bpm) = tap_tempo.bpm() {
                tempo_manager.set_bpm(bpm);
            }

            // ── 5d. Save debounce countdown ──
            if save_debounce > 0 {
                save_debounce -= 1;
                if save_debounce == 0 && ab_mode != AbMode::Morph {
                    // Auto-save: serialize current state into RAM preset buffer.
                    // TODO: after writing to preset_buffer, flush to QSPI flash:
                    // qspi.sector_erase(PRESET_SECTOR_ADDR);
                    // qspi.write(PRESET_SECTOR_ADDR, &preset_buffer);
                    let preset = current_state_to_preset_slot(&nodes, topology);
                    if active_preset == 0xFF {
                        active_preset = 0;
                    }
                    {
                        use sonido_daisy::qspi::PresetStore;
                        let mut store = PresetStore::new(&mut preset_buffer);
                        store.save(active_preset, &preset);
                    }
                    user_presets[active_preset] = Some(preset);
                    defmt::info!("auto-saved preset slot {}", active_preset);
                }
            }

            // ── 6. Handle knobs (A/B modes only, not morph) ──
            if ab_mode != AbMode::Morph {
                if let Some(eff_idx) = nodes[focused_node].effect_index
                    && let Some(nid) = node_ids[focused_node]
                {
                    let entry = &EFFECT_LIST[eff_idx];
                    // Read knob values from ControlBuffer.
                    let norm_knobs: [f32; 6] = core::array::from_fn(|k| CONTROLS.read_knob(k));

                    // Compute param values using descriptors (immutable borrow first).
                    let mut param_vals: [(u8, f32); 6] = [(NULL_KNOB, 0.0); 6];
                    if let Some(effect) = graph.effect_with_params_ref(nid) {
                        for k in 0..6 {
                            let param_idx = entry.knobs[k];
                            if param_idx != NULL_KNOB
                                && let Some(desc) = effect.effect_param_info(param_idx as usize)
                            {
                                let noon = noon_presets::noon_value(entry.id, param_idx as usize)
                                    .unwrap_or(desc.default);
                                let val = adc_to_param_biased(&desc, noon, norm_knobs[k]);
                                // Skip K1 (mode) for looper when FS override is active,
                                // unless the user turned K1 to Stop (clears override).
                                if looper_fs_override && eff_idx == 14 && param_idx == 0 {
                                    if val < 0.5 {
                                        looper_fs_override = false;
                                    } else {
                                        continue;
                                    }
                                }
                                param_vals[k] = (param_idx, val);
                            }
                        }
                    }

                    // Apply to graph and update snapshot.
                    if let Some(effect) = graph.effect_with_params_mut(nid) {
                        for &(pidx, val) in &param_vals {
                            if pidx != NULL_KNOB {
                                effect.effect_set_param(pidx as usize, val);
                            }
                        }
                    }

                    // Update the current snapshot (A or B).
                    nodes[focused_node].update_snapshot(&ab_mode, &param_vals);

                    // Any knob movement in A/B mode arms the auto-save timer.
                    save_debounce = 300; // 3 seconds at ~100 Hz poll rate
                }
            } else {
                // Morph mode: K6 = morph speed (0.2–10.0s). K1-K5 disabled.
                morph_speed = 0.2 + CONTROLS.read_knob(5) * 9.8;
            }

            // ── 7. Graph rebuild (in-place, preserves crossfade state) ──
            if needs_rebuild {
                match rebuild_graph(&mut graph, &nodes, topology, SAMPLE_RATE) {
                    Ok(new_nodes) => {
                        node_ids = new_nodes;

                        // Capture default params for newly created effects.
                        for slot in 0..NUM_SLOTS {
                            if nodes[slot].effect_index.is_some() && nodes[slot].params_a.count == 0
                            {
                                if let Some(nid) = node_ids[slot] {
                                    nodes[slot].params_a.capture_from(&graph, nid);
                                }
                            }
                        }

                        // Restore params to new graph based on mode.
                        match ab_mode {
                            AbMode::A => {
                                apply_all_snapshots(&mut graph, &node_ids, &nodes, AbMode::A);
                            }
                            AbMode::B => {
                                apply_all_snapshots(&mut graph, &node_ids, &nodes, AbMode::B);
                            }
                            AbMode::Morph => {
                                interpolate_and_apply(&mut graph, &node_ids, &nodes, morph_t);
                            }
                        }

                        defmt::info!("graph rebuilt");
                    }
                    Err(_) => {
                        defmt::error!("graph compile failed, keeping previous graph");
                    }
                }
                needs_rebuild = false;
            }

            // ── 7b. Morph interpolation (every poll) ──
            if ab_mode == AbMode::Morph {
                interpolate_and_apply(&mut graph, &node_ids, &nodes, morph_t);
            }

            // ── 8. LED feedback ──
            // LED1: bypass + looper state.
            let looper_mode_raw = if nodes[focused_node].effect_index == Some(14) {
                node_ids[focused_node]
                    .and_then(|nid| graph.effect_with_params_ref(nid))
                    .map_or(-1.0, |e| e.effect_get_param(0))
            } else {
                -1.0
            };

            if BYPASSED.load(Ordering::Relaxed) {
                CONTROLS.write_led(0, 0.0);
            } else {
                let looper_mode = looper_mode_raw as u8;
                if looper_mode_raw >= 0.5 && looper_mode == 1 {
                    // Recording: fast blink 5 Hz (10 polls on, 10 off).
                    CONTROLS.write_led(
                        0,
                        if (poll_counter / 10) % 2 == 0 {
                            1.0
                        } else {
                            0.0
                        },
                    );
                } else if looper_mode_raw >= 1.5 && looper_mode == 2 {
                    // Playing: slow pulse 1 Hz.
                    let phase = (poll_counter % 100) as f32 / 100.0;
                    let bright = 0.5 + 0.5 * libm::sinf(2.0 * core::f32::consts::PI * phase);
                    let pwm = poll_counter % 10;
                    CONTROLS.write_led(
                        0,
                        if pwm < (bright * 10.0) as u16 {
                            1.0
                        } else {
                            0.0
                        },
                    );
                } else if looper_mode_raw >= 2.5 {
                    // Overdubbing: double-blink (50 poll cycle = 500ms).
                    let cycle = (poll_counter % 50) as u16;
                    let on = cycle < 5 || (cycle >= 10 && cycle < 15);
                    CONTROLS.write_led(0, if on { 1.0 } else { 0.0 });
                } else {
                    CONTROLS.write_led(0, 1.0);
                }
            }

            // LED2: effect-specific feedback.
            if BYPASSED.load(Ordering::Relaxed) {
                CONTROLS.write_led(1, 0.0);
            } else if led_blink_remaining > 0 {
                // Transient overlay: factory preset blink (N blinks for preset N).
                led_blink_timer += 1;
                if led_blink_timer >= 10 {
                    led_blink_timer = 0;
                    led_blink_remaining -= 1;
                }
                CONTROLS.write_led(
                    1,
                    if led_blink_remaining % 2 == 0 {
                        1.0
                    } else {
                        0.0
                    },
                );
            } else if ab_mode == AbMode::Morph {
                // PWM duty = morph_t (dark=A, bright=B).
                let pwm_phase = poll_counter % 10;
                let threshold = (morph_t * 10.0) as u16;
                CONTROLS.write_led(1, if pwm_phase < threshold { 1.0 } else { 0.0 });
            } else {
                // A/B modes: effect-specific LED2 feedback.
                let mut output_peak = 0.0f32;
                for &s in left_out.iter().chain(right_out.iter()) {
                    let a = if s < 0.0 { -s } else { s };
                    if a > output_peak {
                        output_peak = a;
                    }
                }
                let mut input_peak = 0.0f32;
                for &s in left_in.iter().chain(right_in.iter()) {
                    let a = if s < 0.0 { -s } else { s };
                    if a > input_peak {
                        input_peak = a;
                    }
                }

                let effect_id = nodes[focused_node]
                    .effect_index
                    .map_or("", |idx| EFFECT_LIST[idx].id);
                let effect_ref =
                    node_ids[focused_node].and_then(|nid| graph.effect_with_params_ref(nid));

                let brightness = match effect_id {
                    "chorus" | "flanger" | "phaser" | "tremolo" => {
                        // LED pulses at LFO rate.
                        let rate = effect_ref.map_or(1.0, |e| e.effect_get_param(0));
                        let dt = POLL_EVERY as f32 * BLOCK_SIZE as f32 / SAMPLE_RATE;
                        led_phase += rate * dt;
                        if led_phase >= 1.0 {
                            led_phase -= 1.0;
                        }
                        0.5 + 0.5 * libm::sinf(2.0 * core::f32::consts::PI * led_phase)
                    }
                    "delay" => {
                        // Brief flash every delay period.
                        let time_ms = effect_ref.map_or(300.0, |e| e.effect_get_param(0));
                        let dt_ms = POLL_EVERY as f32 * BLOCK_SIZE as f32 / SAMPLE_RATE * 1000.0;
                        let period = (time_ms / dt_ms) as u32;
                        let period = if period < 1 { 1 } else { period };
                        led_tap_counter += 1;
                        if led_tap_counter >= period {
                            led_tap_counter = 0;
                        }
                        if led_tap_counter < 3 { 1.0 } else { 0.0 }
                    }
                    "compressor" | "limiter" => {
                        // Dims when compressing (gain reduction = output/input).
                        if input_peak > 0.001 {
                            let ratio = output_peak / input_peak;
                            if ratio > 1.0 { 1.0 } else { ratio }
                        } else {
                            1.0
                        }
                    }
                    "gate" => {
                        // Bright when open, dark when closed.
                        if output_peak > 0.01 { 1.0 } else { 0.0 }
                    }
                    "distortion" | "tape" | "preamp" | "vibrato" | "bitcrusher" | "ringmod"
                    | "wah" => {
                        // Output envelope follower (~30ms at 100 Hz poll rate).
                        led_envelope += 0.3 * (output_peak - led_envelope);
                        let v = led_envelope * 3.0;
                        if v > 1.0 { 1.0 } else { v }
                    }
                    "filter" => {
                        // Brightness = log-scaled cutoff position.
                        let cutoff = effect_ref.map_or(1000.0, |e| e.effect_get_param(0));
                        let norm = libm::log2f(cutoff / 20.0) / libm::log2f(1000.0);
                        let clamped = if norm < 0.0 {
                            0.0
                        } else if norm > 1.0 {
                            1.0
                        } else {
                            norm
                        };
                        0.1 + 0.9 * clamped
                    }
                    "reverb" => {
                        // Brightness = decay amount (param 1, 0-100%).
                        effect_ref.map_or(0.5, |e| e.effect_get_param(1) / 100.0)
                    }
                    _ => 0.0,
                };

                // Software PWM: 10 brightness levels at ~100 Hz.
                let pwm_phase = poll_counter % 10;
                let threshold = (brightness * 10.0) as u16;
                CONTROLS.write_led(1, if pwm_phase < threshold { 1.0 } else { 0.0 });
            }
        })
        .await
    {
        Ok(infallible) => match infallible {},
        Err(_e) => {
            defmt::error!("SAI callback error");
            loop {
                cortex_m::asm::wfi();
            }
        }
    }
}
