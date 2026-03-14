//! Morph Pedal v3 — DAG-based effect processor with per-node A/B morphing.
//!
//! Three toggle switches control node focus, A/B/morph mode, and routing
//! topology. Each of the 3 DAG slots has independent A/B parameter snapshots.
//! Dual footswitches scroll effects (A/B modes) or ramp morph position (morph
//! mode). Both footswitches held ≥1s = bypass toggle.
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
use core::sync::atomic::{AtomicBool, Ordering};

use defmt_rtt as _;
use embassy_stm32 as hal;
use embedded_alloc::LlffHeap as Heap;
use panic_probe as _;

use sonido_core::graph::ProcessingGraph;
use sonido_core::{EffectWithParams, ParamFlags};
use sonido_daisy::controls::HothouseBuffer;
use sonido_daisy::hothouse::hothouse_control_task;
use sonido_daisy::{
    BLOCK_SIZE, ClockProfile, EmbeddedAdapter, SAMPLE_RATE, adc_to_param, f32_to_u24, heartbeat,
    led::UserLed, u24_to_f32,
};
use sonido_effects::{
    BitcrusherKernel, ChorusKernel, CompressorKernel, DelayKernel, DistortionKernel, FilterKernel,
    FlangerKernel, PhaserKernel, ReverbKernel, RingModKernel, TapeKernel, TremoloKernel,
    VibratoKernel, WahKernel,
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

/// Both-footswitch bypass hold threshold: 100 polls × ~10ms = 1s.
const BYPASS_HOLD: u16 = 100;

/// Number of effects in the curated list.
const NUM_EFFECTS: usize = 14;

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
/// | 0 | filter     | 0:Cutoff    | 1:Reso       | --           | --            | --   | 2:Out   |
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
/// |11 | distortion | 0:Drive     | 1:Tone       | 3:Shape(S)   | --            | 4:Mix| 2:Out   |
/// |12 | bitcrusher | 0:Bits(S)   | 1:Down(S)    | 2:Jitter     | --            | 3:Mix| 4:Out   |
/// |13 | ringmod    | 0:Freq      | 1:Depth      | 2:Wave(S)    | --            | 3:Mix| 4:Out   |
const EFFECT_LIST: [EffectEntry; NUM_EFFECTS] = [
    EffectEntry {
        id: "filter",
        knobs: [0, 1, NULL_KNOB, NULL_KNOB, NULL_KNOB, 2],
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
        knobs: [0, 1, 3, NULL_KNOB, 4, 2],
    },
    EffectEntry {
        id: "bitcrusher",
        knobs: [0, 1, 2, NULL_KNOB, 3, 4],
    },
    EffectEntry {
        id: "ringmod",
        knobs: [0, 1, 2, NULL_KNOB, 3, 4],
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
/// Each arm wraps a `DspKernel` in `EmbeddedAdapter` for zero-smoothing
/// `Effect + ParameterInfo`. Replaces `EffectRegistry` — we know our 14
/// effects at compile time.
fn create_effect(idx: usize, sr: f32) -> Option<Box<dyn EffectWithParams + Send>> {
    match idx {
        0 => Some(Box::new(EmbeddedAdapter::new(FilterKernel::new(sr)))),
        1 => Some(Box::new(EmbeddedAdapter::new(TremoloKernel::new(sr)))),
        2 => Some(Box::new(EmbeddedAdapter::new(VibratoKernel::new(sr)))),
        3 => Some(Box::new(EmbeddedAdapter::new(ChorusKernel::new(sr)))),
        4 => Some(Box::new(EmbeddedAdapter::new(PhaserKernel::new(sr)))),
        5 => Some(Box::new(EmbeddedAdapter::new(FlangerKernel::new(sr)))),
        6 => Some(Box::new(EmbeddedAdapter::new(DelayKernel::new(sr)))),
        7 => Some(Box::new(EmbeddedAdapter::new(ReverbKernel::new(sr)))),
        8 => Some(Box::new(EmbeddedAdapter::new(TapeKernel::new(sr)))),
        9 => Some(Box::new(EmbeddedAdapter::new(CompressorKernel::new(sr)))),
        10 => Some(Box::new(EmbeddedAdapter::new(WahKernel::new(sr)))),
        11 => Some(Box::new(EmbeddedAdapter::new(DistortionKernel::new(sr)))),
        12 => Some(Box::new(EmbeddedAdapter::new(BitcrusherKernel::new(sr)))),
        13 => Some(Box::new(EmbeddedAdapter::new(RingModKernel::new(sr)))),
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
) -> (ProcessingGraph, [Option<NodeId>; NUM_SLOTS]) {
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

    g.compile().unwrap();
    (g, node_ids)
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

// ── Deferred D-cache ────────────────────────────────────────────────────────

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

// ── Main ────────────────────────────────────────────────────────────────────

#[embassy_executor::main]
async fn main(spawner: embassy_executor::Spawner) {
    let config = sonido_daisy::rcc_config(ClockProfile::Performance);
    let p = hal::init(config);

    // D2 SRAM clocks — needed for DMA buffers (.sram1_bss at 0x30000000).
    sonido_daisy::enable_d2_sram();

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

    // Build initial graph (passthrough — no effects yet).
    let (mut graph, mut node_ids) = build_graph(&nodes, topology, SAMPLE_RATE, BLOCK_SIZE);
    defmt::info!("graph built: passthrough (no effects)");

    // Morph state
    let mut morph_t: f32 = 0.0; // 0.0 = A, 1.0 = B
    let mut morph_speed: f32 = 2.0; // seconds for full morph

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

            // Both-FS bypass toggle (all modes).
            if both_held == BYPASS_HOLD {
                let was_bypassed = BYPASSED.load(Ordering::Relaxed);
                BYPASSED.store(!was_bypassed, Ordering::Relaxed);
                if was_bypassed {
                    CONTROLS.write_led(0, 1.0);
                } else {
                    CONTROLS.write_led(0, 0.0);
                    CONTROLS.write_led(1, 0.0);
                }
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

            // Both-FS tap detection (consumed, no further action).
            let both_tapped = both_held_peak > 0
                && both_held_peak < TAP_LIMIT
                && !fs1_pressed
                && !fs2_pressed
                && fs1_was_pressed
                && fs2_was_pressed;

            if both_tapped {
                both_held_peak = 0;
            }

            if !fs1_pressed && !fs2_pressed {
                both_held_peak = 0;
            }

            // ── A/B mode: FS1/FS2 release = scroll effects ──
            let was_both = both_held_peak > 0 || both_tapped;

            if (ab_mode == AbMode::A || ab_mode == AbMode::B)
                && !was_both
                && both_held < BYPASS_HOLD
            {
                // FS1 release = scroll previous effect.
                if fs1_was_pressed && !fs1_pressed && fs1_held < TAP_LIMIT as u32 {
                    scroll_effect(&mut nodes, focused_node, -1);
                    needs_rebuild = true;
                    defmt::info!(
                        "node {} ← {}",
                        focused_node + 1,
                        EFFECT_LIST[nodes[focused_node].browse_cursor].id
                    );
                }

                // FS2 release = scroll next effect.
                if fs2_was_pressed && !fs2_pressed && fs2_held < TAP_LIMIT as u32 {
                    scroll_effect(&mut nodes, focused_node, 1);
                    needs_rebuild = true;
                    defmt::info!(
                        "node {} → {}",
                        focused_node + 1,
                        EFFECT_LIST[nodes[focused_node].browse_cursor].id
                    );
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
                                param_vals[k] = (param_idx, adc_to_param(&desc, norm_knobs[k]));
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
                }
            } else {
                // Morph mode: K6 = morph speed (0.2–10.0s). K1-K5 disabled.
                morph_speed = 0.2 + CONTROLS.read_knob(5) * 9.8;
            }

            // ── 7. Graph rebuild ──
            if needs_rebuild {
                let (new_graph, new_nodes) = build_graph(&nodes, topology, SAMPLE_RATE, BLOCK_SIZE);
                graph = new_graph;
                node_ids = new_nodes;

                // Capture default params for newly created effects.
                for slot in 0..NUM_SLOTS {
                    if nodes[slot].effect_index.is_some() && nodes[slot].params_a.count == 0 {
                        if let Some(nid) = node_ids[slot] {
                            nodes[slot].params_a.capture_from(&graph, nid);
                        }
                    }
                }

                // Restore params to new graph based on mode.
                match ab_mode {
                    AbMode::A => apply_all_snapshots(&mut graph, &node_ids, &nodes, AbMode::A),
                    AbMode::B => apply_all_snapshots(&mut graph, &node_ids, &nodes, AbMode::B),
                    AbMode::Morph => {
                        interpolate_and_apply(&mut graph, &node_ids, &nodes, morph_t);
                    }
                }

                needs_rebuild = false;
                defmt::info!("graph rebuilt");
            }

            // ── 7b. Morph interpolation (every poll) ──
            if ab_mode == AbMode::Morph {
                interpolate_and_apply(&mut graph, &node_ids, &nodes, morph_t);
            }

            // ── 8. LED feedback ──
            if BYPASSED.load(Ordering::Relaxed) {
                CONTROLS.write_led(1, 0.0);
            } else {
                match ab_mode {
                    AbMode::A => {
                        // LED2 off in A mode.
                        CONTROLS.write_led(1, 0.0);
                    }
                    AbMode::B => {
                        // LED2 solid on in B mode.
                        CONTROLS.write_led(1, 1.0);
                    }
                    AbMode::Morph => {
                        // PWM duty = morph_t (dark=A, bright=B).
                        let pwm_phase = poll_counter % 10;
                        let threshold = (morph_t * 10.0) as u16;
                        if pwm_phase < threshold {
                            CONTROLS.write_led(1, 1.0);
                        } else {
                            CONTROLS.write_led(1, 0.0);
                        }
                    }
                }
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
