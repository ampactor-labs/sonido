//! Morph Pedal v2 — DAG-based effect processor with static shared state.
//!
//! 3 slots, each holding one of 8 effects (`PEDAL_EFFECT_IDS`: chorus, phaser,
//! distortion, bitcrusher, delay, reverb, ringmod, filter) or bypass, routed
//! linear, parallel or fan (T3).
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
//! cargo objcopy --example sonido_pedal --release --features alloc,platform -- -O binary -R .sram1_bss sonido_pedal.bin
//! dfu-util -a 0 -s 0x90040000:leave -D sonido_pedal.bin
//! ```

#![no_std]
#![no_main]
#![allow(clippy::needless_range_loop)]
#![allow(static_mut_refs)]

extern crate alloc;

use alloc::boxed::Box;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use defmt_rtt as _;
use embassy_stm32 as hal;
use embedded_alloc::LlffHeap as Heap;
use panic_probe as _;

use sonido_core::ParamDescriptor;
use sonido_core::graph::{NodeId, ProcessingGraph};
use sonido_core::kernel::{Adapter, KernelParams};
use sonido_core::{EffectWithParams, ParamFlags};
use sonido_daisy::controls::HothouseBuffer;
use sonido_daisy::effect_slot::{self, BypassCrossfade, sanitize_stereo};
use sonido_daisy::hothouse::hothouse_control_task;
use sonido_daisy::{
    BLOCK_SIZE, ClockProfile, SAMPLE_RATE, f32_to_u24, heartbeat, led::UserLed, u24_to_f32,
};
use sonido_effects::{
    BitcrusherKernel, ChorusKernel, DelayKernel, DelayParams, DistortionKernel, DistortionParams,
    FilterKernel, PhaserKernel, ReverbKernel, ReverbParams, RingModKernel,
};
use sonido_platform::adc_to_param;
use sonido_platform::knob_mapping::{self, NULL_KNOB};
use sonido_registry::PEDAL_EFFECT_IDS;

// ── Heap ────────────────────────────────────────────────────────────────────

#[global_allocator]
static HEAP: Heap = Heap::empty();

// ── Shared control buffer ───────────────────────────────────────────────────

static CONTROLS: HothouseBuffer = HothouseBuffer::new();

// ── Global bypass ───────────────────────────────────────────────────────────

static BYPASSED: AtomicBool = AtomicBool::new(false);
static GRAPH_UPDATING: AtomicBool = AtomicBool::new(false);
static NEEDS_REBUILD: AtomicBool = AtomicBool::new(false);

// ── CPU load meter ──
// The audio callback adds its own cycle count per block (DWT); the load
// report task logs and resets these once a second over defmt RTT. Two
// register reads and three atomics per 32-sample block.
static LOAD_SUM: AtomicU32 = AtomicU32::new(0);
static LOAD_BLOCKS: AtomicU32 = AtomicU32::new(0);
static LOAD_PEAK: AtomicU32 = AtomicU32::new(0);
/// Blocks whose callback took longer than the block's real-time budget.
static LOAD_OVER: AtomicU32 = AtomicU32::new(0);

// ── Constants ───────────────────────────────────────────────────────────────

const MAX_PARAMS: usize = 16;
const NUM_SLOTS: usize = 3;

/// Footswitch-hold threshold separating tap from long-press, in poll ticks
/// (100 Hz effective poll rate → 30 ticks = 300 ms).
const TAP_LIMIT: u32 = 30;

/// Dual-footswitch hold duration that triggers DFU bootloader entry, in poll ticks
/// (100 Hz × 3 s = 300 ticks). Long enough that a bypass stomp never trips it.
const BOOTLOADER_HOLD_TICKS: u16 = 300;

/// Pickup threshold for soft-takeover: knob is unlocked once within 5% of range.
const PICKUP_THRESHOLD_FRAC: f32 = 0.05;

// STM32H750 backup-SRAM bootloader trigger — see RM0433 §8.11 (AHB4ENR), §7.4 (PWR CR1).
const AHB4ENR_ADDR: *mut u32 = 0x5802_44E0 as *mut u32;
const AHB4ENR_BKPRAMEN: u32 = 1 << 28;
const PWR_CR1_ADDR: *mut u32 = 0x5802_4800 as *mut u32;
const PWR_CR1_DBP: u32 = 1 << 8;
const BACKUP_SRAM_ADDR: *mut u32 = 0x3880_0000 as *mut u32;
const DAISY_INFINITE_TIMEOUT: u32 = 0xB007_4EFA;

const EFFECT_IDS: &[&str] = PEDAL_EFFECT_IDS;
const NUM_EFFECTS: usize = EFFECT_IDS.len() + 1; // +1 for null/bypass at index 0

// ── Shared mutable state (ALL callback state lives here, not in the closure) ─

/// SAFETY: Only accessed from the audio callback (single-threaded embassy executor).
/// No critical_section to avoid disabling DMA interrupts.
static mut GRAPH_STORAGE: Option<ProcessingGraph> = None;
static mut NODES_STORAGE: Option<[NodeState; NUM_SLOTS]> = None;
static mut NODE_IDS_STORAGE: [Option<NodeId>; NUM_SLOTS] = [None; NUM_SLOTS];
/// Effect index each slot's graph node was built from (what is playing now,
/// as opposed to `NodeState::effect_index`, which is what the user picked).
static mut LIVE_EFFECTS: [Option<usize>; NUM_SLOTS] = [None; NUM_SLOTS];

/// All mutable callback state packed into one struct for a single static.
/// This keeps the closure zero-capture.
struct CallbackState {
    bypass_xfade: BypassCrossfade,
    left_in: [f32; BLOCK_SIZE],
    right_in: [f32; BLOCK_SIZE],
    left_out: [f32; BLOCK_SIZE],
    right_out: [f32; BLOCK_SIZE],
    poll_counter: u16,
    focused_node: usize,
    ab_mode: AbMode,
    topology: Topology,
    morph_t: f32,
    morph_speed: f32,
    /// Reciprocal cache of `morph_speed * 100`, updated when `morph_speed` changes.
    morph_delta: f32,
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

/// Create an effect by its scroll index.
/// Index 0 = null/bypass (returns None), 1–8 = EFFECT_IDS[0..8].
fn create_effect(idx: usize, sr: f32) -> Option<Box<dyn EffectWithParams + Send>> {
    match idx {
        0 => None, // null / bypass
        1 => Some(Box::new(Adapter::new_direct(ChorusKernel::new(sr), sr))),
        2 => Some(Box::new(Adapter::new_direct(PhaserKernel::new(sr), sr))),
        3 => Some(Box::new(Adapter::new_direct(DistortionKernel::new(sr), sr))),
        4 => Some(Box::new(Adapter::new_direct(BitcrusherKernel::new(sr), sr))),
        5 => Some(Box::new(Adapter::new_direct(DelayKernel::new(sr), sr))),
        6 => Some(Box::new(Adapter::new_direct(ReverbKernel::new(sr), sr))),
        7 => Some(Box::new(Adapter::new_direct(RingModKernel::new(sr), sr))),
        8 => Some(Box::new(Adapter::new_direct(FilterKernel::new(sr), sr))),
        _ => None,
    }
}

// ── Graph construction ──────────────────────────────────────────────────────

/// Insert effects into `graph` for each populated slot, returning
/// `(node_ids_by_slot, populated_slice)` where `populated` holds only the
/// filled `(slot, NodeId)` entries.
fn populate_effects(
    graph: &mut ProcessingGraph,
    nodes: &[NodeState; NUM_SLOTS],
    sr: f32,
) -> (
    [Option<NodeId>; NUM_SLOTS],
    [Option<NodeId>; NUM_SLOTS],
    usize,
) {
    let mut node_ids: [Option<NodeId>; NUM_SLOTS] = [None; NUM_SLOTS];
    let mut populated: [Option<NodeId>; NUM_SLOTS] = [None; NUM_SLOTS];
    let mut n = 0;

    for (slot, node) in nodes.iter().enumerate() {
        if let Some(effect_idx) = node.effect_index
            && let Some(effect) = create_effect(effect_idx, sr)
        {
            let nid = graph.add_effect(effect);
            node_ids[slot] = Some(nid);
            populated[n] = Some(nid);
            n += 1;
        }
    }
    (node_ids, populated, n)
}

fn wire_topology(
    g: &mut ProcessingGraph,
    inp: NodeId,
    out: NodeId,
    populated: &[Option<NodeId>],
    topology: Topology,
) -> Result<(), sonido_core::graph::GraphError> {
    match populated.len() {
        0 => {
            g.connect(inp, out)?;
        }
        1 => {
            let nid = populated[0].unwrap();
            g.connect(inp, nid)?;
            g.connect(nid, out)?;
        }
        _ => match topology {
            Topology::Linear => {
                g.connect(inp, populated[0].unwrap())?;
                for i in 1..populated.len() {
                    g.connect(populated[i - 1].unwrap(), populated[i].unwrap())?;
                }
                g.connect(populated[populated.len() - 1].unwrap(), out)?;
            }
            Topology::Parallel => {
                let s = g.add_split();
                let m = g.add_merge();
                g.connect(inp, s)?;
                for &slot in populated {
                    let nid = slot.unwrap();
                    g.connect(s, nid)?;
                    g.connect(nid, m)?;
                }
                g.connect(m, out)?;
            }
            Topology::Fan => {
                let s = g.add_split();
                let m = g.add_merge();
                let first = populated[0].unwrap();
                g.connect(inp, first)?;
                g.connect(first, s)?;
                for &slot in &populated[1..] {
                    let nid = slot.unwrap();
                    g.connect(s, nid)?;
                    g.connect(nid, m)?;
                }
                g.connect(m, out)?;
            }
        },
    }
    Ok(())
}

fn build_graph(
    nodes: &[NodeState; NUM_SLOTS],
    topology: Topology,
    sr: f32,
    bs: usize,
) -> Result<(ProcessingGraph, [Option<NodeId>; NUM_SLOTS]), sonido_core::graph::GraphError> {
    let mut g = ProcessingGraph::new(sr, bs);
    let inp = g.add_input();
    let out = g.add_output();

    let (node_ids, populated, n) = populate_effects(&mut g, nodes, sr);
    wire_topology(&mut g, inp, out, &populated[..n], topology)?;
    g.compile()?;
    Ok((g, node_ids))
}

/// Share of the block budget a chain's effects may use, summing each
/// kernel's measured worst block. The rest covers the firmware's own audio
/// and control work.
const CHAIN_BUDGET: u32 = sonido_platform::cycle_table::CYCLE_BUDGET_PER_BLOCK / 100 * 85;

/// Share of the block budget for the new chain plus the old chain's tails
/// during a rebuild. Lower than [`CHAIN_BUDGET`]: measured in a Hothouse,
/// delay + reverb with two delay tails (sum of worsts 78%) ran at 88% mean
/// and 98% peak, so tails are kept only up to 70%.
const SPILLOVER_BUDGET: u32 = sonido_platform::cycle_table::CYCLE_BUDGET_PER_BLOCK / 100 * 70;

/// Measured worst-case cycles per block for an effect index (0 = bypass).
fn effect_cycles(effect_idx: Option<usize>) -> u32 {
    effect_idx
        .and_then(|i| i.checked_sub(1))
        .and_then(|i| EFFECT_IDS.get(i))
        .and_then(|id| sonido_registry::effect_uid(id))
        .and_then(sonido_platform::cycle_table::effect_cycles)
        .unwrap_or(0)
}

/// Whether the old chain's tails can ring out next to the new chain.
///
/// Every node is rebuilt, so a tail runs alongside a fresh copy of the same
/// effect: two reverbs measure ~125% of the block budget, and the SAI then
/// overruns for the whole 3 s tail. Tails that don't fit are cut instead
/// (the wet→dry ramp around the rebuild covers the cut).
fn spillover_fits(
    graph: &mut ProcessingGraph,
    live_ids: &[Option<NodeId>; NUM_SLOTS],
    live_effects: &[Option<usize>; NUM_SLOTS],
    nodes: &[NodeState; NUM_SLOTS],
) -> bool {
    let tails: u32 = (0..NUM_SLOTS)
        .filter(|&slot| {
            live_ids[slot]
                .and_then(|nid| graph.effect_mut(nid))
                .is_some_and(|e| e.tail_samples() > 0)
        })
        .map(|slot| effect_cycles(live_effects[slot]))
        .sum();
    let chain: u32 = nodes.iter().map(|n| effect_cycles(n.effect_index)).sum();
    tails + chain <= SPILLOVER_BUDGET
}

fn rebuild_graph_in_place(
    graph: &mut ProcessingGraph,
    nodes: &[NodeState; NUM_SLOTS],
    topology: Topology,
    sr: f32,
    spillover: bool,
) -> Result<[Option<NodeId>; NUM_SLOTS], sonido_core::graph::GraphError> {
    // Toggling drops tails still ringing from the previous rebuild.
    graph.set_spillover(false);
    graph.set_spillover(spillover);
    graph.clear_topology();

    let inp = graph.input_id().unwrap();
    let out = graph.output_id().unwrap();

    let (node_ids, populated, n) = populate_effects(graph, nodes, sr);
    wire_topology(graph, inp, out, &populated[..n], topology)?;
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

/// Step `node_idx` to the next effect in `delta`'s direction, skipping any
/// that would push the chain past [`CHAIN_BUDGET`] (in practice, a second
/// reverb). Bypass always fits, so the walk ends.
fn scroll_effect(nodes: &mut [NodeState; NUM_SLOTS], node_idx: usize, delta: i32) {
    let others: u32 = (0..NUM_SLOTS)
        .filter(|&slot| slot != node_idx)
        .map(|slot| effect_cycles(nodes[slot].effect_index))
        .sum();
    let mut cursor = nodes[node_idx].browse_cursor;
    loop {
        cursor = ((cursor as i32 + delta).rem_euclid(NUM_EFFECTS as i32)) as usize;
        if others + effect_cycles(Some(cursor)) <= CHAIN_BUDGET {
            break;
        }
    }
    let node = &mut nodes[node_idx];
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

/// Number of factory presets FS1 long-press cycles through.
const FACTORY_PRESET_COUNT: usize = 3;

/// One slot of a factory preset: the effect and its A and B states.
struct FactorySlot {
    effect_index: usize,
    a: SlotSnapshot,
    b: SlotSnapshot,
}

/// Index of a pedal effect by registry id (0 is bypass).
fn effect_index(id: &str) -> usize {
    EFFECT_IDS
        .iter()
        .position(|e| *e == id)
        .map_or(0, |i| i + 1)
}

/// Snapshot of a kernel's typed params, in descriptor order, with the
/// stepped flags read from its descriptors.
fn snapshot<P: KernelParams>(params: &P) -> SlotSnapshot {
    let mut snap = SlotSnapshot::new();
    snap.count = P::COUNT.min(MAX_PARAMS);
    for i in 0..snap.count {
        snap.values[i] = params.get(i);
        snap.stepped[i] = P::descriptor(i).is_some_and(|d| d.flags.contains(ParamFlags::STEPPED));
    }
    snap
}

fn factory_slot<P: KernelParams>(id: &str, a: P, b: P) -> Option<FactorySlot> {
    Some(FactorySlot {
        effect_index: effect_index(id),
        a: snapshot(&a),
        b: snapshot(&b),
    })
}

/// The factory presets, written as the kernels' typed params so a renamed,
/// added or reordered parameter is a compile error here rather than a
/// silently shifted value.
fn factory_preset(cursor: usize) -> [Option<FactorySlot>; NUM_SLOTS] {
    match cursor {
        // "Room → Shimmer": reverb. Intimate bright room → infinite dark shimmer.
        0 => [
            factory_slot(
                "reverb",
                ReverbParams {
                    room_size_pct: 30.0,
                    decay_pct: 40.0,
                    damping_pct: 60.0,
                    predelay_ms: 5.0,
                    mix_pct: 40.0,
                    width_pct: 80.0,
                    er_level_pct: 50.0,
                    output_db: 0.0,
                },
                ReverbParams {
                    room_size_pct: 90.0,
                    decay_pct: 88.0,
                    damping_pct: 10.0,
                    predelay_ms: 30.0,
                    mix_pct: 80.0,
                    width_pct: 100.0,
                    er_level_pct: 30.0,
                    output_db: 0.0,
                },
            ),
            None,
            None,
        ],
        // "Slap → Self-Osc": delay. Tight slapback → darkening wall near
        // self-oscillation.
        1 => [
            factory_slot(
                "delay",
                DelayParams {
                    time_ms: 80.0,
                    feedback_pct: 15.0,
                    mix_pct: 25.0,
                    ping_pong: 0.0,
                    fb_lp_hz: 20_000.0,
                    fb_hp_hz: 20.0,
                    diffusion_pct: 0.0,
                    sync: 0.0,
                    division: 2.0,
                    output_db: 0.0,
                },
                DelayParams {
                    time_ms: 400.0,
                    feedback_pct: 93.0,
                    mix_pct: 70.0,
                    ping_pong: 0.0,
                    fb_lp_hz: 3_000.0,
                    fb_hp_hz: 100.0,
                    diffusion_pct: 30.0,
                    sync: 0.0,
                    division: 2.0,
                    output_db: 0.0,
                },
            ),
            None,
            None,
        ],
        // "Clean → Saturated": distortion → reverb. Clean guitar in a tight
        // room → saturated drive in a lush verb.
        _ => [
            factory_slot(
                "distortion",
                DistortionParams {
                    drive_db: 0.0,
                    tone_db: 0.0,
                    output_db: 0.0,
                    shape: 0.0,
                    mix_pct: 100.0,
                    dynamics_pct: 0.0,
                },
                DistortionParams {
                    drive_db: 32.0,
                    tone_db: -3.0,
                    output_db: -6.0,
                    shape: 3.0,
                    mix_pct: 100.0,
                    dynamics_pct: 60.0,
                },
            ),
            factory_slot(
                "reverb",
                ReverbParams {
                    room_size_pct: 20.0,
                    decay_pct: 30.0,
                    damping_pct: 50.0,
                    predelay_ms: 5.0,
                    mix_pct: 20.0,
                    width_pct: 60.0,
                    er_level_pct: 40.0,
                    output_db: 0.0,
                },
                ReverbParams {
                    room_size_pct: 60.0,
                    decay_pct: 65.0,
                    damping_pct: 30.0,
                    predelay_ms: 15.0,
                    mix_pct: 45.0,
                    width_pct: 100.0,
                    er_level_pct: 50.0,
                    output_db: 0.0,
                },
            ),
            None,
        ],
    }
}

fn load_factory_preset(nodes: &mut [NodeState; NUM_SLOTS], cursor: usize) {
    for (node, slot) in nodes.iter_mut().zip(factory_preset(cursor)) {
        match slot {
            Some(slot) => {
                node.effect_index = Some(slot.effect_index);
                node.browse_cursor = slot.effect_index;
                node.params_a = slot.a;
                node.params_b = Some(slot.b);
            }
            None => {
                node.effect_index = None;
                node.params_a = SlotSnapshot::new();
                node.params_b = None;
            }
        }
    }
}

// ── Static initialization (non-async to avoid inflating the future) ─────────

fn init_statics(focused: usize, ab: AbMode, topo: Topology) {
    let nodes: [NodeState; NUM_SLOTS] = core::array::from_fn(|_| NodeState::new());
    // Boot into null/bypass — factory presets available via FS1 long-press.
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
            focused_node: focused,
            ab_mode: ab,
            topology: topo,
            morph_t: 0.0,
            morph_speed: 2.0,
            morph_delta: 1.0 / (2.0 * 100.0),
            master_gain: 1.0,
            // The first FS1 long-press advances to preset 1 (index 0).
            factory_cursor: FACTORY_PRESET_COUNT - 1,
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
    sonido_daisy::enable_cycle_counter(&mut cp.DCB, &mut cp.DWT);

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

    defmt::info!(
        "toggles: node={}, ab={}, topo={}",
        init_focused + 1,
        t2_init,
        t3_init
    );

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
    defmt::info!("graph ready (all slots bypassed; FS1 hold loads factory presets)");
    defmt::info!("entering audio callback");

    // Zero-capture closure — ALL state accessed via statics.
    // SAFETY: CB_STORAGE is only accessed here (single-threaded audio callback).
    // No critical_section — interrupts must stay enabled for DMA completion.
    NEEDS_REBUILD.store(true, Ordering::Release);
    spawner.spawn(graph_rebuild_task()).unwrap();
    spawner.spawn(load_report_task()).unwrap();
    defmt::unwrap!(
        interface
            .start_callback(|input, output| {
                // Time the whole callback, every path (the early returns are
                // inside `run`).
                let t0 = cortex_m::peripheral::DWT::cycle_count();
                let mut run = || {
                    let cb = unsafe { CB_STORAGE.as_mut().unwrap() };

                    if GRAPH_UPDATING.load(Ordering::Acquire) {
                        // Audio callback can't run process_block while the graph is being
                        // rebuilt. Ramp the bypass crossfade toward dry so the wet→dry
                        // transition is smooth; set_active(true) on exit will ramp it
                        // back up. Without this, every topology / scroll / preset rebuild
                        // produces an audible click.
                        cb.bypass_xfade.set_active(false);
                        for i in 0..BLOCK_SIZE {
                            let l = u24_to_f32(input[i * 2]);
                            let r = u24_to_f32(input[i * 2 + 1]);
                            let mono = (l + r) * 0.5;
                            let (out_l, out_r) = cb.bypass_xfade.advance(mono, mono, 0.0, 0.0);
                            output[i * 2] = f32_to_u24(out_l);
                            output[i * 2 + 1] = f32_to_u24(out_r);
                        }
                        return;
                    }

                    // ── Hard bypass ──
                    if BYPASSED.load(Ordering::Relaxed) {
                        output.copy_from_slice(input);
                        cb.poll_counter = cb.poll_counter.wrapping_add(1);
                        if cb
                            .poll_counter
                            .is_multiple_of(effect_slot::CONTROL_POLL_EVERY)
                        {
                            let fs1 = CONTROLS.read_footswitch(0);
                            let fs2 = CONTROLS.read_footswitch(1);
                            accumulate_both_held(cb, fs1, fs2);
                            handle_both_release_toggle(cb, fs1, fs2);
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
                            &cb.left_in,
                            &cb.right_in,
                            &mut cb.left_out,
                            &mut cb.right_out,
                        );
                    }

                    for i in 0..BLOCK_SIZE {
                        let (wet_l, wet_r) = sanitize_stereo(cb.left_out[i], cb.right_out[i]);
                        let (mut out_l, mut out_r) =
                            cb.bypass_xfade
                                .advance(cb.left_in[i], cb.right_in[i], wet_l, wet_r);
                        if cb.ab_mode == AbMode::Morph {
                            out_l *= cb.master_gain;
                            out_r *= cb.master_gain;
                        }
                        output[i * 2] = f32_to_u24(out_l);
                        output[i * 2 + 1] = f32_to_u24(out_r);
                    }

                    cb.poll_counter = cb.poll_counter.wrapping_add(1);
                    if !cb
                        .poll_counter
                        .is_multiple_of(effect_slot::CONTROL_POLL_EVERY)
                    {
                        return;
                    }

                    // ── Toggles ──
                    let t1 = CONTROLS.read_toggle(0);
                    let t2 = CONTROLS.read_toggle(1);
                    let t3 = CONTROLS.read_toggle(2);

                    let new_topo = toggle_to_topology(t3);
                    if new_topo != cb.topology {
                        cb.topology = new_topo;
                        NEEDS_REBUILD.store(true, Ordering::Release);
                    }
                    let new_focused = toggle_to_node(t1);
                    if new_focused != cb.focused_node {
                        cb.focused_node = new_focused;
                        // Knobs were tracking the previous node's params; require a
                        // soft-takeover gesture before they overwrite the new node's state.
                        cb.pickup_locked = [true; 6];
                    }

                    let new_ab = toggle_to_ab_mode(t2);
                    if new_ab != cb.ab_mode {
                        let old_mode = cb.ab_mode;
                        cb.ab_mode = new_ab;
                        let nodes = unsafe { NODES_STORAGE.as_mut().unwrap() };
                        let graph = unsafe { GRAPH_STORAGE.as_mut().unwrap() };
                        let nids = unsafe { NODE_IDS_STORAGE };
                        // Any A/B/Morph transition repaints params on the graph; the
                        // physical knobs were tracking the previous mode's snapshot, so
                        // re-arm pickup_locked across the board to prevent immediate clobber.
                        cb.pickup_locked = [true; 6];
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
                                    AbMode::A => 0.0,
                                    AbMode::B => 1.0,
                                    _ => cb.morph_t,
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
                    accumulate_both_held(cb, fs1_pressed, fs2_pressed);
                    if fs1_pressed {
                        cb.fs1_held += 1;
                    }
                    if fs2_pressed {
                        cb.fs2_held += 1;
                    }

                    if cb.ab_mode == AbMode::Morph && !both_pressed {
                        let delta = cb.morph_delta;
                        if fs1_pressed && !fs2_pressed {
                            cb.morph_t = if cb.morph_t > delta {
                                cb.morph_t - delta
                            } else {
                                0.0
                            };
                        } else if fs2_pressed && !fs1_pressed {
                            cb.morph_t = if cb.morph_t + delta < 1.0 {
                                cb.morph_t + delta
                            } else {
                                1.0
                            };
                        }
                    }

                    let was_both = cb.both_held_peak > 0;
                    handle_both_release_toggle(cb, fs1_pressed, fs2_pressed);

                    if (cb.ab_mode == AbMode::A || cb.ab_mode == AbMode::B) && !was_both {
                        if cb.fs1_was_pressed && !fs1_pressed && cb.fs1_held < TAP_LIMIT {
                            let nodes = unsafe { NODES_STORAGE.as_mut().unwrap() };
                            scroll_effect(nodes, cb.focused_node, -1);
                            NEEDS_REBUILD.store(true, Ordering::Release);
                            cb.pickup_locked = [true; 6];
                        }
                        if cb.ab_mode == AbMode::A
                            && cb.fs1_was_pressed
                            && !fs1_pressed
                            && cb.fs1_held >= TAP_LIMIT
                        {
                            cb.factory_cursor = (cb.factory_cursor + 1) % FACTORY_PRESET_COUNT;
                            let nodes = unsafe { NODES_STORAGE.as_mut().unwrap() };
                            load_factory_preset(nodes, cb.factory_cursor);
                            NEEDS_REBUILD.store(true, Ordering::Release);
                            cb.pickup_locked = [true; 6];
                            cb.led_blink_remaining = (cb.factory_cursor as u8 + 1) * 2;
                            cb.led_blink_timer = 0;
                        }
                        if cb.fs2_was_pressed && !fs2_pressed && cb.fs2_held < TAP_LIMIT {
                            let nodes = unsafe { NODES_STORAGE.as_mut().unwrap() };
                            scroll_effect(nodes, cb.focused_node, 1);
                            NEEDS_REBUILD.store(true, Ordering::Release);
                            cb.pickup_locked = [true; 6];
                        }
                    }

                    if !fs1_pressed {
                        cb.fs1_held = 0;
                    }
                    if !fs2_pressed {
                        cb.fs2_held = 0;
                    }
                    cb.fs1_was_pressed = fs1_pressed;
                    cb.fs2_was_pressed = fs2_pressed;

                    // ── Knobs ──
                    if cb.ab_mode != AbMode::Morph {
                        let nodes = unsafe { NODES_STORAGE.as_mut().unwrap() };
                        let graph = unsafe { GRAPH_STORAGE.as_mut().unwrap() };
                        let nids = unsafe { NODE_IDS_STORAGE };

                        if let Some(eff_idx) = nodes[cb.focused_node].effect_index
                            && eff_idx > 0
                            && let Some(nid) = nids[cb.focused_node]
                        {
                            let effect_id = EFFECT_IDS[eff_idx - 1];
                            let knobs = knob_mapping::knob_map(effect_id).unwrap_or([NULL_KNOB; 6]);
                            let platform = sonido_daisy::hothouse::HothousePlatform::new(&CONTROLS);
                            use sonido_platform::PlatformController;
                            let mut param_vals: [(u8, f32); 6] = [(NULL_KNOB, 0.0); 6];
                            if let Some(effect) = graph.effect_with_params_ref(nid) {
                                for k in 0..6 {
                                    let ctrl_id = sonido_platform::ControlId::hardware(k as u8);
                                    if let Some(state) = platform.read_control(ctrl_id) {
                                        let pidx = knobs[k];
                                        if pidx == NULL_KNOB {
                                            continue;
                                        }
                                        let idx = pidx as usize;
                                        if let Some(desc) = effect.effect_param_info(idx) {
                                            let val = knob_mapping::knob_to_param(
                                                effect_id,
                                                idx,
                                                &desc,
                                                state.value,
                                            );
                                            if cb.pickup_locked[k] {
                                                let current = effect.effect_get_param(idx);
                                                let range = desc.max - desc.min;
                                                if (val - current).abs()
                                                    < range * PICKUP_THRESHOLD_FRAC
                                                {
                                                    cb.pickup_locked[k] = false;
                                                } else {
                                                    continue;
                                                }
                                            }
                                            param_vals[k] = (pidx, val);
                                        }
                                    }
                                }
                            }
                            if let Some(effect) = graph.effect_with_params_mut(nid) {
                                for &(pidx, val) in &param_vals {
                                    if pidx != NULL_KNOB {
                                        effect.effect_set_param(pidx as usize, val);
                                    }
                                }
                            }
                            nodes[cb.focused_node].update_snapshot(&cb.ab_mode, &param_vals);
                        }
                    } else {
                        const SPEED_DESC: ParamDescriptor =
                            ParamDescriptor::custom("Morph Speed", "Morph Speed", 0.1, 5.0, 2.0);
                        const MASTER_DESC: ParamDescriptor =
                            ParamDescriptor::gain_db("Master", "Master", -60.0, 12.0, 0.0);

                        let new_speed = adc_to_param(&SPEED_DESC, CONTROLS.read_knob(4));
                        if new_speed != cb.morph_speed {
                            cb.morph_speed = new_speed;
                            cb.morph_delta = 1.0 / (new_speed * 100.0);
                        }

                        let master_gain_db = adc_to_param(&MASTER_DESC, CONTROLS.read_knob(5));
                        cb.master_gain = sonido_core::fast_db_to_linear(master_gain_db);
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
                        if cb.led_blink_timer >= 10 {
                            cb.led_blink_timer = 0;
                            cb.led_blink_remaining -= 1;
                        }
                        CONTROLS.write_led(
                            1,
                            if cb.led_blink_remaining % 2 == 0 {
                                1.0
                            } else {
                                0.0
                            },
                        );
                    } else if cb.ab_mode == AbMode::Morph {
                        let pwm = cb.poll_counter % 10;
                        CONTROLS.write_led(
                            1,
                            if pwm < (cb.morph_t * 10.0) as u16 {
                                1.0
                            } else {
                                0.0
                            },
                        );
                    } else {
                        let mut peak = 0.0f32;
                        for &s in cb.left_out.iter().chain(cb.right_out.iter()) {
                            let a = if s < 0.0 { -s } else { s };
                            if a > peak {
                                peak = a;
                            }
                        }
                        cb.led_envelope += 0.3 * (peak - cb.led_envelope);
                        let v = cb.led_envelope * 3.0;
                        let brightness = if v > 1.0 { 1.0 } else { v };
                        let pwm = cb.poll_counter % 10;
                        CONTROLS.write_led(
                            1,
                            if pwm < (brightness * 10.0) as u16 {
                                1.0
                            } else {
                                0.0
                            },
                        );
                    }
                };
                run();
                let cycles = cortex_m::peripheral::DWT::cycle_count().wrapping_sub(t0);
                LOAD_SUM.fetch_add(cycles, Ordering::Relaxed);
                LOAD_BLOCKS.fetch_add(1, Ordering::Relaxed);
                LOAD_PEAK.fetch_max(cycles, Ordering::Relaxed);
                if cycles > LOAD_BUDGET {
                    LOAD_OVER.fetch_add(1, Ordering::Relaxed);
                }
            })
            .await
    );
}

/// The audio callback's real-time budget per block, in cycles at 480 MHz.
const LOAD_BUDGET: u32 = 480_000_000 / 48_000 * BLOCK_SIZE as u32;

/// Once a second: the callback's mean and peak cost as a share of the block
/// budget, blocks over budget, and what is loaded — over defmt RTT (attach a
/// probe: `probe-rs attach --chip STM32H750IBKx <elf>`).
#[embassy_executor::task]
async fn load_report_task() {
    loop {
        embassy_time::Timer::after_secs(1).await;
        let sum = LOAD_SUM.swap(0, Ordering::Relaxed);
        let blocks = LOAD_BLOCKS.swap(0, Ordering::Relaxed);
        let peak = LOAD_PEAK.swap(0, Ordering::Relaxed);
        let over = LOAD_OVER.swap(0, Ordering::Relaxed);
        if blocks == 0 {
            continue;
        }
        let pct_x10 = |c: u32| (u64::from(c) * 1000 / u64::from(LOAD_BUDGET)) as u32;
        let (mean, peak) = (pct_x10(sum / blocks), pct_x10(peak));
        // Single-threaded executor: the callback isn't mid-update while this runs.
        let name = |slot: usize| {
            unsafe { NODES_STORAGE.as_ref() }
                .and_then(|n| n[slot].effect_index)
                // Index 0 is bypass; 1..=8 map to EFFECT_IDS.
                .and_then(|i| i.checked_sub(1))
                .and_then(|i| EFFECT_IDS.get(i).copied())
                .unwrap_or("-")
        };
        let topo = match unsafe { CB_STORAGE.as_ref() }.map(|cb| cb.topology) {
            Some(Topology::Linear) => "linear",
            Some(Topology::Parallel) => "parallel",
            Some(Topology::Fan) => "fan",
            None => "?",
        };
        defmt::info!(
            "load mean={}.{}% peak={}.{}% over_budget={}/{} slots=[{}, {}, {}] topo={} bypassed={}",
            mean / 10,
            mean % 10,
            peak / 10,
            peak % 10,
            over,
            blocks,
            name(0),
            name(1),
            name(2),
            topo,
            BYPASSED.load(Ordering::Relaxed)
        );
    }
}

/// Accumulate the both-footswitches-held counter and trigger the DFU bootloader
/// once `BOOTLOADER_HOLD_TICKS` is reached. Resets to 0 when either switch is released.
fn accumulate_both_held(cb: &mut CallbackState, fs1: bool, fs2: bool) {
    if fs1 && fs2 {
        cb.both_held = cb.both_held.saturating_add(1);
        if cb.both_held > cb.both_held_peak {
            cb.both_held_peak = cb.both_held;
        }
        if cb.both_held >= BOOTLOADER_HOLD_TICKS {
            enter_daisy_bootloader();
        }
    } else {
        cb.both_held = 0;
    }
}

/// On release after a both-held gesture, toggle the global `BYPASSED` flag and
/// update LED 0. LED 1 is cleared when bypass activates. Clears `both_held_peak`
/// whenever both switches are released so a fresh gesture can start.
fn handle_both_release_toggle(cb: &mut CallbackState, fs1: bool, fs2: bool) {
    if cb.both_held_peak > 0 && !fs1 && !fs2 {
        let was_bypassed = BYPASSED.load(Ordering::Relaxed);
        BYPASSED.store(!was_bypassed, Ordering::Relaxed);
        CONTROLS.write_led(0, if was_bypassed { 1.0 } else { 0.0 });
        if !was_bypassed {
            CONTROLS.write_led(1, 0.0);
        }
    }
    if !fs1 && !fs2 {
        cb.both_held_peak = 0;
    }
}

/// Write the DFU-timeout magic into STM32H7 Backup SRAM and reset, which causes the
/// Daisy Seed bootloader to remain in DFU mode indefinitely on the next boot.
/// See RM0433 §8.11 (AHB4ENR) / §7.4 (PWR CR1) and the libDaisy bootloader source.
#[inline(never)]
fn enter_daisy_bootloader() -> ! {
    unsafe {
        core::ptr::write_volatile(
            AHB4ENR_ADDR,
            core::ptr::read_volatile(AHB4ENR_ADDR) | AHB4ENR_BKPRAMEN,
        );
        core::ptr::write_volatile(
            PWR_CR1_ADDR,
            core::ptr::read_volatile(PWR_CR1_ADDR) | PWR_CR1_DBP,
        );
        core::ptr::write_volatile(BACKUP_SRAM_ADDR, DAISY_INFINITE_TIMEOUT);
    }
    cortex_m::peripheral::SCB::sys_reset();
}

#[embassy_executor::task]
async fn graph_rebuild_task() {
    loop {
        embassy_time::Timer::after_millis(20).await;
        if !GRAPH_UPDATING.load(Ordering::Acquire) {
            let graph = unsafe { GRAPH_STORAGE.as_mut().unwrap() };
            graph.clear_garbage();
        }
        if NEEDS_REBUILD.load(Ordering::Acquire) {
            GRAPH_UPDATING.store(true, Ordering::Release);
            // Wait to ensure audio thread enters the bypass state
            embassy_time::Timer::after_millis(5).await;

            let nodes = unsafe { NODES_STORAGE.as_mut().unwrap() };
            let graph = unsafe { GRAPH_STORAGE.as_mut().unwrap() };
            let cb = unsafe { CB_STORAGE.as_mut().unwrap() };

            let spillover =
                unsafe { spillover_fits(graph, &NODE_IDS_STORAGE, &LIVE_EFFECTS, nodes) };
            if let Ok(new_nids) =
                rebuild_graph_in_place(graph, nodes, cb.topology, SAMPLE_RATE, spillover)
            {
                unsafe {
                    NODE_IDS_STORAGE = new_nids;
                    LIVE_EFFECTS = core::array::from_fn(|slot| nodes[slot].effect_index);
                }
                for slot in 0..NUM_SLOTS {
                    if nodes[slot].effect_index.is_some()
                        && nodes[slot].params_a.count == 0
                        && let Some(nid) = new_nids[slot]
                    {
                        nodes[slot].params_a.capture_from(graph, nid);
                    }
                }
                match cb.ab_mode {
                    AbMode::A => apply_all_snapshots(graph, &new_nids, nodes, AbMode::A),
                    AbMode::B => apply_all_snapshots(graph, &new_nids, nodes, AbMode::B),
                    AbMode::Morph => interpolate_and_apply(graph, &new_nids, nodes, cb.morph_t),
                }
            }
            NEEDS_REBUILD.store(false, Ordering::Release);
            GRAPH_UPDATING.store(false, Ordering::Release);
        }
    }
}
