//! Morph Pedal v2 — DAG-based effect processor with static shared state.
//!
//! Phase 1: 3 effects (Chorus, Distortion, Reverb), linear chain.
//! Heap on 64 MB SDRAM (FMC bus, separate from D2 SRAM DMA bus).
//!
//! # Toggle Mapping
//!
//! | Toggle | UP (0)         | MID (1)        | DOWN (2)                    |
//! |--------|----------------|----------------|-----------------------------|
//! | **T1** | Node 1         | Node 2         | Node 3                      |
//! | **T2** | A mode (edit)  | B mode (edit)  | Morph (FS1/FS2 ramp)        |
//! | **T3** | Linear 1→2→3   | Parallel split | Fan 1→split→[2,3]→merge     |
//!
//! # Build & Flash
//!
//! ```bash
//! cd crates/sonido-daisy
//! cargo objcopy --example sonido_pedal_v2 --release --features alloc,platform -- -O binary -R .sram1_bss sonido_pedal_v2.bin
//! dfu-util -a 0 -s 0x90040000:leave -D sonido_pedal_v2.bin
//! ```

#![no_std]
#![no_main]
#![allow(clippy::needless_range_loop)]
#![allow(static_mut_refs)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use defmt_rtt as _;
use embassy_stm32 as hal;
use embedded_alloc::LlffHeap as Heap;
use panic_probe as _;

use sonido_core::graph::{NodeId, ProcessingGraph};
use sonido_core::kernel::Adapter;
use sonido_core::{EffectWithParams, ParamFlags};
use sonido_daisy::controls::HothouseBuffer;
use sonido_daisy::effect_slot::{self, BypassCrossfade, sanitize_stereo};
use sonido_daisy::hothouse::hothouse_control_task;
use sonido_daisy::{
    BLOCK_SIZE, ClockProfile, SAMPLE_RATE, f32_to_u24, heartbeat, led::UserLed, u24_to_f32,
};
use sonido_effects::{
    BitcrusherKernel, ChorusKernel, CompressorKernel, DelayKernel, DistortionKernel, FilterKernel,
    FlangerKernel, LooperKernel, PhaserKernel, ReverbKernel, RingModKernel, TapeKernel,
    TremoloKernel, VibratoKernel, WahKernel,
};
use sonido_platform::knob_mapping::{self, NULL_KNOB};
use sonido_registry::PEDAL_EFFECT_IDS;
use sonido_core::ParamDescriptor;
use sonido_platform::adc_to_param;

// ── Heap ────────────────────────────────────────────────────────────────────

#[global_allocator]
static HEAP: Heap = Heap::empty();

// ── Shared control buffer ───────────────────────────────────────────────────

static CONTROLS: HothouseBuffer = HothouseBuffer::new();

// ── Global bypass ───────────────────────────────────────────────────────────

static BYPASSED: AtomicBool = AtomicBool::new(false);

// ── Constants ───────────────────────────────────────────────────────────────

const MAX_PARAMS: usize = 16;
const NUM_SLOTS: usize = 3;
const TAP_LIMIT: u16 = 30;

const EFFECT_IDS: &[&str] = PEDAL_EFFECT_IDS;
const NUM_EFFECTS: usize = EFFECT_IDS.len();

// ── Shared mutable state (ALL callback state lives here, not in the closure) ─

/// SAFETY: Only accessed from the audio callback (single-threaded embassy executor).
/// No critical_section to avoid disabling DMA interrupts.
static mut GRAPH_STORAGE: Option<ProcessingGraph> = None;
static mut NODES_STORAGE: Option<[NodeState; NUM_SLOTS]> = None;
static mut NODE_IDS_STORAGE: [Option<NodeId>; NUM_SLOTS] = [None; NUM_SLOTS];

/// All mutable callback state packed into one struct for a single static.
/// This keeps the closure zero-capture.
struct CallbackState {
    bypass_xfade: BypassCrossfade,
    left_in: [f32; BLOCK_SIZE],
    right_in: [f32; BLOCK_SIZE],
    left_out: [f32; BLOCK_SIZE],
    right_out: [f32; BLOCK_SIZE],
    poll_counter: u16,
    needs_rebuild: bool,
    focused_node: usize,
    ab_mode: AbMode,
    topology: Topology,
    morph_t: f32,
    morph_speed: f32,
    master_gain: f32,
    factory_cursor: usize,
    led_blink_remaining: u8,
    led_blink_timer: u16,
    pickup_locked: [bool; 6],
    fs1_held: u32,
    fs2_held: u32,
    both_held: u16,
    both_held_peak: u16,
    fs1_was_pressed: bool,
    fs2_was_pressed: bool,
    led_envelope: f32,
}

/// SAFETY: Only accessed from the audio callback (single-threaded, no ISR).
/// Using static mut instead of critical_section::Mutex because
/// critical_section disables interrupts, which prevents DMA completion
/// interrupts from firing during process_block → SAI overrun.
static mut CB_STORAGE: Option<CallbackState> = None;

// ── Enums ───────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum AbMode {
    A,
    B,
    Morph,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Topology {
    Linear,
    Parallel,
    Fan,
}

// ── Per-Slot Snapshot ───────────────────────────────────────────────────────

#[derive(Clone)]
struct SlotSnapshot {
    values: [f32; MAX_PARAMS],
    stepped: [bool; MAX_PARAMS],
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

    fn apply_to(&self, graph: &mut ProcessingGraph, node_id: NodeId) {
        if let Some(effect) = graph.effect_with_params_mut(node_id) {
            for p in 0..self.count {
                effect.effect_set_param(p, self.values[p]);
            }
        }
    }
}

// ── Per-Node State ──────────────────────────────────────────────────────────

struct NodeState {
    effect_index: Option<usize>,
    params_a: SlotSnapshot,
    params_b: Option<SlotSnapshot>,
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
            AbMode::Morph => {}
        }
    }
}

// ── Toggle parsing ──────────────────────────────────────────────────────────

fn toggle_to_node(val: u8) -> usize {
    match val {
        0 => 0,
        2 => 2,
        _ => 1,
    }
}

fn toggle_to_ab_mode(val: u8) -> AbMode {
    match val {
        0 => AbMode::A,
        2 => AbMode::Morph,
        _ => AbMode::B,
    }
}

fn toggle_to_topology(val: u8) -> Topology {
    match val {
        0 => Topology::Linear,
        2 => Topology::Fan,
        _ => Topology::Parallel,
    }
}

// ── Effect factory ──────────────────────────────────────────────────────────

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

// ── Graph construction ──────────────────────────────────────────────────────

fn build_graph(
    nodes: &[NodeState; NUM_SLOTS],
    topology: Topology,
    sr: f32,
    bs: usize,
) -> Result<(ProcessingGraph, [Option<NodeId>; NUM_SLOTS]), sonido_core::graph::GraphError> {
    let mut g = ProcessingGraph::new(sr, bs);
    let inp = g.add_input();
    let out = g.add_output();

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
        g.connect(inp, out).unwrap();
    } else if populated.len() == 1 {
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

fn rebuild_graph_in_place(
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

// ── Snapshot helpers ────────────────────────────────────────────────────────

fn ensure_b_snapshots(nodes: &mut [NodeState; NUM_SLOTS]) {
    for slot in 0..NUM_SLOTS {
        if nodes[slot].params_b.is_none() && nodes[slot].effect_index.is_some() {
            nodes[slot].params_b = Some(nodes[slot].params_a.clone());
        }
    }
}

fn scroll_effect(nodes: &mut [NodeState; NUM_SLOTS], node_idx: usize, delta: i32) {
    let node = &mut nodes[node_idx];
    let cursor = ((node.browse_cursor as i32 + delta).rem_euclid(NUM_EFFECTS as i32)) as usize;
    node.browse_cursor = cursor;
    node.effect_index = Some(cursor);
    node.params_a = SlotSnapshot::new();
    node.params_b = None;
}

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
            AbMode::Morph => {}
        }
    }
}

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
                None => a,
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

// ── Factory Presets ─────────────────────────────────────────────────────────

struct FactorySlot {
    effect_idx: Option<usize>,
    params_a: [f32; MAX_PARAMS],
    params_b: [f32; MAX_PARAMS],
    stepped: [bool; MAX_PARAMS],
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

struct FactoryPreset {
    slots: [FactorySlot; NUM_SLOTS],
}

const FACTORY_PRESETS: [FactoryPreset; 3] = [
    // Preset 1: "Room → Shimmer" — Reverb on node 1
    // Morph story: intimate bright room → infinite dark shimmer
    FactoryPreset {
        slots: [
            FactorySlot {
                effect_idx: Some(7), // reverb
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

// ── Static initialization (non-async to avoid inflating the future) ─────────

fn init_statics(focused: usize, ab: AbMode, topo: Topology) {
    let mut nodes: [NodeState; NUM_SLOTS] = core::array::from_fn(|_| NodeState::new());
    load_factory_preset(&mut nodes, 0);
    let (mut graph, node_ids) = build_graph(&nodes, topo, SAMPLE_RATE, BLOCK_SIZE).unwrap();
    apply_all_snapshots(&mut graph, &node_ids, &nodes, AbMode::A);

    unsafe {
        GRAPH_STORAGE = Some(graph);
        NODES_STORAGE = Some(nodes);
        NODE_IDS_STORAGE = node_ids;
        CB_STORAGE = Some(CallbackState {
            bypass_xfade: BypassCrossfade::new(SAMPLE_RATE),
            left_in: [0.0; BLOCK_SIZE],
            right_in: [0.0; BLOCK_SIZE],
            left_out: [0.0; BLOCK_SIZE],
            right_out: [0.0; BLOCK_SIZE],
            poll_counter: 0,
            needs_rebuild: false,
            focused_node: focused,
            ab_mode: ab,
            topology: topo,
            morph_t: 0.0,
            morph_speed: 2.0,
            master_gain: 1.0,
            factory_cursor: 0,
            led_blink_remaining: 0,
            led_blink_timer: 0,
            pickup_locked: [false; 6],
            fs1_held: 0,
            fs2_held: 0,
            both_held: 0,
            both_held_peak: 0,
            fs1_was_pressed: false,
            fs2_was_pressed: false,
            led_envelope: 0.0,
        });
    }
}

// ── Main ────────────────────────────────────────────────────────────────────

#[embassy_executor::main]
async fn main(spawner: embassy_executor::Spawner) {
    let config = sonido_daisy::rcc_config(ClockProfile::Performance);
    let p = hal::init(config);

    sonido_daisy::enable_d2_sram();
    sonido_daisy::enable_fpu_ftz();

    // SDRAM heap — 64 MB on FMC bus, separate from D2 SRAM (DMA) bus.
    let mut cp = unsafe { cortex_m::Peripherals::steal() };
    let sdram_ptr = sonido_daisy::init_sdram!(p, &mut cp.MPU, &mut cp.SCB);
    #[allow(unsafe_code)]
    unsafe {
        HEAP.init(sdram_ptr as usize, sonido_daisy::sdram::SDRAM_SIZE);
    }
    sonido_daisy::sdram::enable_dcache();

    let led = UserLed::new(p.PC7);
    spawner.spawn(heartbeat(led)).unwrap();

    defmt::info!("sonido_pedal v2: booting...");

    let ctrl = sonido_daisy::hothouse_pins!(p);
    spawner
        .spawn(hothouse_control_task(ctrl, &CONTROLS))
        .unwrap();

    CONTROLS.write_led(0, 1.0);

    embassy_time::Timer::after_millis(30).await;
    let t1_init = CONTROLS.read_toggle(0);
    let t2_init = CONTROLS.read_toggle(1);
    let t3_init = CONTROLS.read_toggle(2);

    let init_focused = toggle_to_node(t1_init);
    let init_ab = toggle_to_ab_mode(t2_init);
    let init_topo = toggle_to_topology(t3_init);

    defmt::info!("toggles: node={}, ab={}, topo={}", init_focused + 1, t2_init, t3_init);

    // ── Audio setup ──
    let audio_peripherals = sonido_daisy::audio::AudioPeripherals {
        codec_pins: sonido_daisy::codec_pins!(p),
        sai1: p.SAI1,
        dma1_ch0: p.DMA1_CH0,
        dma1_ch1: p.DMA1_CH1,
    };
    let interface = audio_peripherals
        .prepare_interface(Default::default())
        .await;
    let mut interface = defmt::unwrap!(interface.start_interface().await);

    // Init graph (SDRAM heap — no bus contention with DMA).
    init_statics(init_focused, init_ab, init_topo);
    defmt::info!("factory preset 1 loaded");
    defmt::info!("entering audio callback");

    // Zero-capture closure — ALL state accessed via statics.
    // SAFETY: CB_STORAGE is only accessed here (single-threaded audio callback).
    // No critical_section — interrupts must stay enabled for DMA completion.
    defmt::unwrap!(
        interface
            .start_callback(|input, output| {
                let cb = unsafe { CB_STORAGE.as_mut().unwrap() };

                    // ── Hard bypass ──
                    if BYPASSED.load(Ordering::Relaxed) {
                        output.copy_from_slice(input);
                        cb.poll_counter = cb.poll_counter.wrapping_add(1);
                        if cb.poll_counter.is_multiple_of(effect_slot::CONTROL_POLL_EVERY) {
                            let fs1 = CONTROLS.read_footswitch(0);
                            let fs2 = CONTROLS.read_footswitch(1);
                            if fs1 && fs2 {
                                cb.both_held += 1;
                                if cb.both_held > cb.both_held_peak { cb.both_held_peak = cb.both_held; }
                            } else { cb.both_held = 0; }
                            if cb.both_held_peak > 0 && !fs1 && !fs2 {
                                BYPASSED.store(false, Ordering::Relaxed);
                                CONTROLS.write_led(0, 1.0);
                                cb.both_held_peak = 0;
                            }
                            if !fs1 && !fs2 { cb.both_held_peak = 0; }
                        }
                        return;
                    }

                    cb.bypass_xfade.set_active(true);

                    for i in 0..BLOCK_SIZE {
                        let l = u24_to_f32(input[i * 2]);
                        let r = u24_to_f32(input[i * 2 + 1]);
                        let mono = (l + r) * 0.5;
                        cb.left_in[i] = mono;
                        cb.right_in[i] = mono;
                    }

                    if let Some(graph) = unsafe { GRAPH_STORAGE.as_mut() } {
                        graph.process_block(
                            &cb.left_in, &cb.right_in, &mut cb.left_out, &mut cb.right_out,
                        );
                    }

                    for i in 0..BLOCK_SIZE {
                        let (wet_l, wet_r) = sanitize_stereo(cb.left_out[i], cb.right_out[i]);
                        let (mut out_l, mut out_r) =
                            cb.bypass_xfade.advance(cb.left_in[i], cb.right_in[i], wet_l, wet_r);
                        if cb.ab_mode == AbMode::Morph {
                            out_l *= cb.master_gain;
                            out_r *= cb.master_gain;
                        }
                        output[i * 2] = f32_to_u24(out_l);
                        output[i * 2 + 1] = f32_to_u24(out_r);
                    }

                    cb.poll_counter = cb.poll_counter.wrapping_add(1);
                    if !cb.poll_counter.is_multiple_of(effect_slot::CONTROL_POLL_EVERY) {
                        return;
                    }

                    // ── Toggles ──
                    let t1 = CONTROLS.read_toggle(0);
                    let t2 = CONTROLS.read_toggle(1);
                    let t3 = CONTROLS.read_toggle(2);

                    let new_topo = toggle_to_topology(t3);
                    if new_topo != cb.topology {
                        cb.topology = new_topo;
                        cb.needs_rebuild = true;
                    }
                    let new_focused = toggle_to_node(t1);
                    if new_focused != cb.focused_node { cb.focused_node = new_focused; }

                    let new_ab = toggle_to_ab_mode(t2);
                    if new_ab != cb.ab_mode {
                        let old_mode = cb.ab_mode;
                        cb.ab_mode = new_ab;
                        let nodes = unsafe { NODES_STORAGE.as_mut().unwrap() };
                        let graph = unsafe { GRAPH_STORAGE.as_mut().unwrap() };
                        let nids = unsafe { NODE_IDS_STORAGE };
                        match (old_mode, cb.ab_mode) {
                            (AbMode::A, AbMode::B) => {
                                ensure_b_snapshots(nodes);
                                apply_all_snapshots(graph, &nids, nodes, AbMode::B);
                            }
                            (AbMode::B, AbMode::A) => {
                                apply_all_snapshots(graph, &nids, nodes, AbMode::A);
                            }
                            (_, AbMode::Morph) => {
                                ensure_b_snapshots(nodes);
                                cb.morph_t = match old_mode {
                                    AbMode::A => 0.0, AbMode::B => 1.0, _ => cb.morph_t,
                                };
                            }
                            (AbMode::Morph, AbMode::A) => {
                                apply_all_snapshots(graph, &nids, nodes, AbMode::A);
                                cb.pickup_locked = [true; 6];
                            }
                            (AbMode::Morph, AbMode::B) => {
                                ensure_b_snapshots(nodes);
                                apply_all_snapshots(graph, &nids, nodes, AbMode::B);
                                cb.pickup_locked = [true; 6];
                            }
                            _ => {}
                        }
                    }

                    // ── Footswitches ──
                    let fs1_pressed = CONTROLS.read_footswitch(0);
                    let fs2_pressed = CONTROLS.read_footswitch(1);
                    let both_pressed = fs1_pressed && fs2_pressed;
                    if both_pressed {
                        cb.both_held += 1;
                        if cb.both_held > cb.both_held_peak { cb.both_held_peak = cb.both_held; }
                    } else { cb.both_held = 0; }
                    if fs1_pressed { cb.fs1_held += 1; }
                    if fs2_pressed { cb.fs2_held += 1; }

                    if cb.ab_mode == AbMode::Morph && !both_pressed {
                        let delta = 1.0 / (cb.morph_speed * 100.0);
                        if fs1_pressed && !fs2_pressed {
                            cb.morph_t = if cb.morph_t > delta { cb.morph_t - delta } else { 0.0 };
                        } else if fs2_pressed && !fs1_pressed {
                            cb.morph_t = if cb.morph_t + delta < 1.0 { cb.morph_t + delta } else { 1.0 };
                        }
                    }

                    let was_both = cb.both_held_peak > 0;
                    if was_both && !fs1_pressed && !fs2_pressed {
                        let was_bypassed = BYPASSED.load(Ordering::Relaxed);
                        BYPASSED.store(!was_bypassed, Ordering::Relaxed);
                        CONTROLS.write_led(0, if was_bypassed { 1.0 } else { 0.0 });
                        if !was_bypassed { CONTROLS.write_led(1, 0.0); }
                    }
                    if !fs1_pressed && !fs2_pressed { cb.both_held_peak = 0; }

                    if (cb.ab_mode == AbMode::A || cb.ab_mode == AbMode::B) && !was_both {
                        if cb.fs1_was_pressed && !fs1_pressed && cb.fs1_held < TAP_LIMIT as u32 {
                            let nodes = unsafe { NODES_STORAGE.as_mut().unwrap() };
                            scroll_effect(nodes, cb.focused_node, -1);
                            cb.needs_rebuild = true;
                            cb.pickup_locked = [true; 6];
                        }
                        if cb.ab_mode == AbMode::A && cb.fs1_was_pressed && !fs1_pressed && cb.fs1_held >= TAP_LIMIT as u32 {
                            cb.factory_cursor = (cb.factory_cursor + 1) % FACTORY_PRESETS.len();
                            let nodes = unsafe { NODES_STORAGE.as_mut().unwrap() };
                            load_factory_preset(nodes, cb.factory_cursor);
                            cb.needs_rebuild = true;
                            cb.pickup_locked = [true; 6];
                            cb.led_blink_remaining = (cb.factory_cursor as u8 + 1) * 2;
                            cb.led_blink_timer = 0;
                        }
                        if cb.fs2_was_pressed && !fs2_pressed && cb.fs2_held < TAP_LIMIT as u32 {
                            let nodes = unsafe { NODES_STORAGE.as_mut().unwrap() };
                            scroll_effect(nodes, cb.focused_node, 1);
                            cb.needs_rebuild = true;
                            cb.pickup_locked = [true; 6];
                        }
                    }

                    if !fs1_pressed { cb.fs1_held = 0; }
                    if !fs2_pressed { cb.fs2_held = 0; }
                    cb.fs1_was_pressed = fs1_pressed;
                    cb.fs2_was_pressed = fs2_pressed;

                    // ── Knobs ──
                    if cb.ab_mode != AbMode::Morph {
                        let nodes = unsafe { NODES_STORAGE.as_mut().unwrap() };
                        let graph = unsafe { GRAPH_STORAGE.as_mut().unwrap() };
                        let nids = unsafe { NODE_IDS_STORAGE };

                        if let Some(eff_idx) = nodes[cb.focused_node].effect_index
                            && let Some(nid) = nids[cb.focused_node]
                        {
                            let effect_id = EFFECT_IDS[eff_idx];
                            let knobs = knob_mapping::knob_map(effect_id).unwrap_or([NULL_KNOB; 6]);
                            let platform = sonido_daisy::hothouse::HothousePlatform::new(&CONTROLS);
                            use sonido_platform::PlatformController;
                            let mut param_vals: [(u8, f32); 6] = [(NULL_KNOB, 0.0); 6];
                            if let Some(effect) = graph.effect_with_params_ref(nid) {
                                for k in 0..6 {
                                    let ctrl_id = sonido_platform::ControlId::hardware(k as u8);
                                    if let Some(state) = platform.read_control(ctrl_id) {
                                        let pidx = knobs[k];
                                        if pidx == NULL_KNOB { continue; }
                                        let idx = pidx as usize;
                                        if let Some(desc) = effect.effect_param_info(idx) {
                                            let val = knob_mapping::knob_to_param(effect_id, idx, &desc, state.value);
                                            if cb.pickup_locked[k] {
                                                let current = effect.effect_get_param(idx);
                                                let range = desc.max - desc.min;
                                                if (val - current).abs() < range * 0.05 {
                                                    cb.pickup_locked[k] = false;
                                                } else { continue; }
                                            }
                                            param_vals[k] = (pidx, val);
                                        }
                                    }
                                }
                            }
                            if let Some(effect) = graph.effect_with_params_mut(nid) {
                                for &(pidx, val) in &param_vals {
                                    if pidx != NULL_KNOB { effect.effect_set_param(pidx as usize, val); }
                                }
                            }
                            nodes[cb.focused_node].update_snapshot(&cb.ab_mode, &param_vals);
                        }
                    } else {
                        let speed_desc = ParamDescriptor::custom("Morph Speed", "Morph Speed", 0.1, 5.0, 2.0);
                        cb.morph_speed = adc_to_param(&speed_desc, CONTROLS.read_knob(4));

                        let master_desc = ParamDescriptor::gain_db("Master", "Master", -60.0, 12.0, 0.0);
                        let master_gain_db = adc_to_param(&master_desc, CONTROLS.read_knob(5));
                        cb.master_gain = sonido_core::fast_db_to_linear(master_gain_db);
                    }

                    // ── Graph rebuild ──
                    if cb.needs_rebuild {
                        let nodes = unsafe { NODES_STORAGE.as_mut().unwrap() };
                        let graph = unsafe { GRAPH_STORAGE.as_mut().unwrap() };
                        if let Ok(new_nids) = rebuild_graph_in_place(graph, nodes, cb.topology, SAMPLE_RATE) {
                            unsafe { NODE_IDS_STORAGE = new_nids; }
                            for slot in 0..NUM_SLOTS {
                                if nodes[slot].effect_index.is_some() && nodes[slot].params_a.count == 0 {
                                    if let Some(nid) = new_nids[slot] {
                                        nodes[slot].params_a.capture_from(graph, nid);
                                    }
                                }
                            }
                            match cb.ab_mode {
                                AbMode::A => apply_all_snapshots(graph, &new_nids, nodes, AbMode::A),
                                AbMode::B => apply_all_snapshots(graph, &new_nids, nodes, AbMode::B),
                                AbMode::Morph => interpolate_and_apply(graph, &new_nids, nodes, cb.morph_t),
                            }
                        }
                        cb.needs_rebuild = false;
                    }

                    // ── Morph interpolation ──
                    if cb.ab_mode == AbMode::Morph {
                        let nodes = unsafe { NODES_STORAGE.as_ref().unwrap() };
                        let graph = unsafe { GRAPH_STORAGE.as_mut().unwrap() };
                        let nids = unsafe { NODE_IDS_STORAGE };
                        interpolate_and_apply(graph, &nids, nodes, cb.morph_t);
                    }

                    // ── LED feedback ──
                    if BYPASSED.load(Ordering::Relaxed) {
                        CONTROLS.write_led(0, 0.0);
                        CONTROLS.write_led(1, 0.0);
                    } else if cb.led_blink_remaining > 0 {
                        cb.led_blink_timer += 1;
                        if cb.led_blink_timer >= 10 { cb.led_blink_timer = 0; cb.led_blink_remaining -= 1; }
                        CONTROLS.write_led(1, if cb.led_blink_remaining % 2 == 0 { 1.0 } else { 0.0 });
                    } else if cb.ab_mode == AbMode::Morph {
                        let pwm = cb.poll_counter % 10;
                        CONTROLS.write_led(1, if pwm < (cb.morph_t * 10.0) as u16 { 1.0 } else { 0.0 });
                    } else {
                        let mut peak = 0.0f32;
                        for &s in cb.left_out.iter().chain(cb.right_out.iter()) {
                            let a = if s < 0.0 { -s } else { s };
                            if a > peak { peak = a; }
                        }
                        cb.led_envelope += 0.3 * (peak - cb.led_envelope);
                        let v = cb.led_envelope * 3.0;
                        let brightness = if v > 1.0 { 1.0 } else { v };
                        let pwm = cb.poll_counter % 10;
                        CONTROLS.write_led(1, if pwm < (brightness * 10.0) as u16 { 1.0 } else { 0.0 });
                    }
            })
            .await
    );
}
