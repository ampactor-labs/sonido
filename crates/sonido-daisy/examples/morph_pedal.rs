//! Morph Pedal v2 — DAG-based effect processor with A/B morphing.
//!
//! Boot to passthrough, incrementally build a 3-slot effect chain, capture
//! Sound A and Sound B snapshots, then morph between them with dual-footswitch
//! arrow control. Uses `ProcessingGraph` for DAG routing and `EmbeddedAdapter`
//! for zero-smoothing direct kernel access.
//!
//! # Three-Mode Architecture
//!
//! | Toggle 3 | Mode    | What it does                                       |
//! |----------|---------|----------------------------------------------------|
//! | UP       | EXPLORE | Scroll effects into 3 slots, shape params          |
//! | CENTER   | BUILD   | Capture A/B snapshots per-slot, preview with T1    |
//! | DOWN     | MORPH   | Footswitch-controlled crossfade between A and B    |
//!
//! Toggle 2 selects routing topology in all modes:
//! - UP: Serial (E0 → E1 → E2)
//! - CENTER: Parallel (split → E0,E1,E2 → merge)
//! - DOWN: Fan (E0 → split → E1,E2 → merge)
//!
//! # Hardware (Hothouse DIY)
//!
//! | Control     | Pin(s)               | Function                    |
//! |-------------|----------------------|-----------------------------|
//! | Knobs 1–6   | PA3,PB1,PA7,PA6,PC1,PC4 | Per-effect curated params |
//! | Toggle 1    | PB4/PB5              | Slot select / preview / speed |
//! | Toggle 2    | PG10/PG11            | Routing topology            |
//! | Toggle 3    | PD2/PC12             | Mode selector               |
//! | Footswitch 1| PA0                  | Mode-specific               |
//! | Footswitch 2| PD11                 | Mode-specific               |
//! | LED 1       | PA5                  | Active / bypassed           |
//! | LED 2       | PA4                  | Mode-specific feedback      |
//!
//! # Build & Flash
//!
//! ```bash
//! cd crates/sonido-daisy
//! cargo objcopy --example morph_pedal --release -- -O binary -R .sram1_bss morph_pedal.bin
//! dfu-util -a 0 -s 0x90040000:leave -D morph_pedal.bin
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

/// Maximum number of saveable presets (heap-resident, survives until power-off).
const MAX_PRESETS: usize = 9;

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
/// - K6: Level (output/makeup gain)
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
    EffectEntry { id: "filter",     knobs: [0, 1, NULL_KNOB, NULL_KNOB, NULL_KNOB, 2] },
    EffectEntry { id: "tremolo",    knobs: [0, 1, 2, 3, NULL_KNOB, 6] },
    EffectEntry { id: "vibrato",    knobs: [0, NULL_KNOB, NULL_KNOB, NULL_KNOB, 1, 2] },
    EffectEntry { id: "chorus",     knobs: [0, 1, 4, 3, 2, 8] },
    EffectEntry { id: "phaser",     knobs: [0, 1, 2, 3, 4, 9] },
    EffectEntry { id: "flanger",    knobs: [0, 1, 2, 4, 3, 7] },
    EffectEntry { id: "delay",      knobs: [0, 1, 4, 3, 2, 9] },
    EffectEntry { id: "reverb",     knobs: [0, 1, 2, 3, 4, 7] },
    EffectEntry { id: "tape",       knobs: [0, 1, 2, 4, 5, 9] },
    EffectEntry { id: "compressor", knobs: [0, 1, 2, 3, 10, 4] },
    EffectEntry { id: "wah",        knobs: [0, 1, 2, 3, NULL_KNOB, 4] },
    EffectEntry { id: "distortion", knobs: [0, 1, 3, NULL_KNOB, 4, 2] },
    EffectEntry { id: "bitcrusher", knobs: [0, 1, 2, NULL_KNOB, 3, 4] },
    EffectEntry { id: "ringmod",    knobs: [0, 1, 2, NULL_KNOB, 3, 4] },
];

// ── Enums ───────────────────────────────────────────────────────────────────

/// Pedal operating mode, selected by Toggle 3.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Explore,
    Build,
    Morph,
}

/// Audio routing topology, selected by Toggle 2.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Routing {
    Serial,
    Parallel,
    Fan,
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

// ── Sound Snapshot ──────────────────────────────────────────────────────────

/// Parameter snapshot for one sound (A or B) across all 3 slots.
///
/// Includes cached STEPPED flags per parameter for efficient morph
/// interpolation (STEPPED params snap at t=0.5, no fractional values).
#[derive(Clone)]
struct SoundSnapshot {
    /// Parameter values per slot.
    params: [[f32; MAX_PARAMS]; NUM_SLOTS],
    /// Whether each param is STEPPED (cached at capture time).
    stepped: [[bool; MAX_PARAMS]; NUM_SLOTS],
    /// Number of parameters per slot.
    param_counts: [usize; NUM_SLOTS],
}

impl SoundSnapshot {
    fn new() -> Self {
        Self {
            params: [[0.0; MAX_PARAMS]; NUM_SLOTS],
            stepped: [[false; MAX_PARAMS]; NUM_SLOTS],
            param_counts: [0; NUM_SLOTS],
        }
    }

    /// Capture one slot's parameters and STEPPED flags from the graph.
    fn capture_slot(
        &mut self,
        slot: usize,
        graph: &ProcessingGraph,
        node_id: Option<NodeId>,
    ) {
        if let Some(nid) = node_id {
            if let Some(effect) = graph.effect_with_params_ref(nid) {
                let count = effect.effect_param_count().min(MAX_PARAMS);
                self.param_counts[slot] = count;
                for p in 0..count {
                    self.params[slot][p] = effect.effect_get_param(p);
                    self.stepped[slot][p] = effect
                        .effect_param_info(p)
                        .is_some_and(|d| d.flags.contains(ParamFlags::STEPPED));
                }
            }
        } else {
            self.param_counts[slot] = 0;
        }
    }

    /// Capture all slots from the graph.
    fn capture_all(
        &mut self,
        graph: &ProcessingGraph,
        node_ids: &[Option<NodeId>; NUM_SLOTS],
    ) {
        for slot in 0..NUM_SLOTS {
            self.capture_slot(slot, graph, node_ids[slot]);
        }
    }

    /// Apply snapshot values to all slots in the graph.
    fn apply_to_graph(
        &self,
        graph: &mut ProcessingGraph,
        node_ids: &[Option<NodeId>; NUM_SLOTS],
    ) {
        for slot in 0..NUM_SLOTS {
            if let Some(nid) = node_ids[slot]
                && let Some(effect) = graph.effect_with_params_mut(nid) {
                    for p in 0..self.param_counts[slot] {
                        effect.effect_set_param(p, self.params[slot][p]);
                    }
                }
        }
    }
}

// ── Preset ──────────────────────────────────────────────────────────────────

/// Complete pedal state stored as a preset.
#[derive(Clone)]
struct Preset {
    /// Effect indices (into EFFECT_LIST) for each slot. `None` = empty.
    slot_effects: [Option<usize>; NUM_SLOTS],
    /// Routing topology.
    routing: Routing,
    /// Sound A parameter snapshot.
    sound_a: SoundSnapshot,
    /// Sound B parameter snapshot.
    sound_b: SoundSnapshot,
    /// Morph speed in seconds.
    morph_speed: f32,
    /// Whether this preset slot has been written to.
    occupied: bool,
}

impl Preset {
    fn empty() -> Self {
        Self {
            slot_effects: [None; NUM_SLOTS],
            routing: Routing::Serial,
            sound_a: SoundSnapshot::new(),
            sound_b: SoundSnapshot::new(),
            morph_speed: 2.0,
            occupied: false,
        }
    }
}

// ── Graph construction ──────────────────────────────────────────────────────

/// Build a `ProcessingGraph` from the current slot configuration.
///
/// Empty slots are skipped — adjacent populated nodes connect directly.
/// Returns the compiled graph and node IDs for each slot (None for empty).
fn build_graph(
    effect_indices: &[Option<usize>; NUM_SLOTS],
    routing: Routing,
    sr: f32,
    bs: usize,
) -> (ProcessingGraph, [Option<NodeId>; NUM_SLOTS]) {
    let mut g = ProcessingGraph::new(sr, bs);
    let inp = g.add_input();
    let out = g.add_output();

    // Collect populated slots: (slot_index, node_id)
    let mut populated: Vec<(usize, NodeId)> = Vec::new();
    let mut node_ids: [Option<NodeId>; NUM_SLOTS] = [None; NUM_SLOTS];

    for (slot, idx) in effect_indices.iter().enumerate() {
        if let Some(effect_idx) = idx
            && let Some(effect) = create_effect(*effect_idx, sr) {
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
        match routing {
            Routing::Serial => {
                g.connect(inp, populated[0].1).unwrap();
                for i in 1..populated.len() {
                    g.connect(populated[i - 1].1, populated[i].1).unwrap();
                }
                g.connect(populated[populated.len() - 1].1, out).unwrap();
            }
            Routing::Parallel => {
                let s = g.add_split();
                let m = g.add_merge();
                g.connect(inp, s).unwrap();
                for &(_, nid) in &populated {
                    g.connect(s, nid).unwrap();
                    g.connect(nid, m).unwrap();
                }
                g.connect(m, out).unwrap();
            }
            Routing::Fan => {
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

/// Apply interpolated parameters to the graph (same logic as `KernelParams::lerp`).
///
/// STEPPED params snap at `t=0.5`. Continuous params interpolate linearly.
/// Called every control poll (~100Hz) during morph mode.
fn interpolate_and_apply(
    graph: &mut ProcessingGraph,
    node_ids: &[Option<NodeId>; NUM_SLOTS],
    a: &SoundSnapshot,
    b: &SoundSnapshot,
    t: f32,
) {
    for slot in 0..NUM_SLOTS {
        if let Some(nid) = node_ids[slot]
            && let Some(effect) = graph.effect_with_params_mut(nid) {
                let count = a.param_counts[slot].min(b.param_counts[slot]);
                for p in 0..count {
                    let val = if a.stepped[slot][p] {
                        if t < 0.5 { a.params[slot][p] } else { b.params[slot][p] }
                    } else {
                        a.params[slot][p] + (b.params[slot][p] - a.params[slot][p]) * t
                    };
                    effect.effect_set_param(p, val);
                }
            }
    }
}

// ── Slot navigation ─────────────────────────────────────────────────────────

/// Find the next populated slot (wrapping), or return `current` if none.
fn next_populated(effect_indices: &[Option<usize>; NUM_SLOTS], current: usize) -> usize {
    for i in 1..=NUM_SLOTS {
        let idx = (current + i) % NUM_SLOTS;
        if effect_indices[idx].is_some() {
            return idx;
        }
    }
    current
}

/// Find the previous populated slot (wrapping), or return `current` if none.
fn prev_populated(effect_indices: &[Option<usize>; NUM_SLOTS], current: usize) -> usize {
    for i in 1..=NUM_SLOTS {
        let idx = (current + NUM_SLOTS - i) % NUM_SLOTS;
        if effect_indices[idx].is_some() {
            return idx;
        }
    }
    current
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

    defmt::info!("morph_pedal v2: initializing...");

    // ── Extract control pins and spawn control task ──
    // Must happen BEFORE audio start_interface → start_callback.
    let ctrl = sonido_daisy::hothouse_pins!(p);
    spawner
        .spawn(hothouse_control_task(ctrl, &CONTROLS))
        .unwrap();

    // LED1 starts on (active indicator).
    CONTROLS.write_led(0, 1.0);

    defmt::info!("morph_pedal v2: controls initialized");

    // ── Initial state: all slots empty, passthrough ──

    let mut effect_indices: [Option<usize>; NUM_SLOTS] = [None; NUM_SLOTS];
    // Per-slot browse cursor (which effect in EFFECT_LIST to scroll to next).
    // Starts at 0 so first FS2 tap creates filter (index 0).
    let mut browse_cursor: [usize; NUM_SLOTS] = [0; NUM_SLOTS];

    // Read initial toggle positions from ControlBuffer.
    // Give the control task one cycle to populate.
    embassy_time::Timer::after_millis(30).await;

    let t2_init = CONTROLS.read_toggle(1);
    let mut routing = match t2_init {
        0 => Routing::Serial,
        2 => Routing::Fan,
        _ => Routing::Parallel,
    };
    let t3_init = CONTROLS.read_toggle(2);
    let mut mode = match t3_init {
        0 => Mode::Explore,
        2 => Mode::Morph,
        _ => Mode::Build,
    };
    let t1_init = CONTROLS.read_toggle(0);
    let mut focused_slot: usize = match t1_init {
        0 => 0,
        2 => 2,
        _ => 1,
    };
    defmt::info!(
        "morph_pedal v2: toggles — slot={}, routing={}, mode={}",
        focused_slot + 1,
        t2_init,
        t3_init
    );

    // Build initial graph (passthrough — no effects yet).
    let (mut graph, mut node_ids) =
        build_graph(&effect_indices, routing, SAMPLE_RATE, BLOCK_SIZE);
    defmt::info!("graph built: passthrough (no effects)");

    // Sound snapshots (boxed — saves stack in closure captures).
    let mut sound_a = Box::new(SoundSnapshot::new());
    let mut sound_b = Box::new(SoundSnapshot::new());

    // Build mode state
    let mut build_slot: usize = 0;

    // Morph state
    let mut morph_t: f32 = 0.0; // 0.0 = Sound A, 1.0 = Sound B
    let mut morph_speed: f32 = 2.0; // seconds for full morph

    // Preset storage (boxed — saves ~4 KB in closure captures).
    let mut presets: Box<[Preset; MAX_PRESETS]> =
        Box::new(core::array::from_fn(|_| Preset::empty()));
    let mut next_preset_slot: usize = 0;
    let mut load_preset_cursor: usize = 0;

    // Footswitch state machine
    let mut fs1_held: u16 = 0;
    let mut fs2_held: u16 = 0;
    let mut both_held: u16 = 0;
    let mut both_held_peak: u16 = 0;
    let mut fs1_was_pressed = false;
    let mut fs2_was_pressed = false;

    // LED2 feedback
    let mut led2_counter: u16 = 0;
    let mut led2_blink_pattern: u8 = 0; // 1/2/3 for Build slot indication

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

            // ── Read toggles from ControlBuffer ──
            let t1 = CONTROLS.read_toggle(0);
            let t2 = CONTROLS.read_toggle(1);
            let t3 = CONTROLS.read_toggle(2);

            // ── Mode from T3 ──
            let new_mode = match t3 {
                0 => Mode::Explore,
                2 => Mode::Morph,
                _ => Mode::Build,
            };
            if new_mode != mode {
                // ── Mode transition logic ──
                match (mode, new_mode) {
                    (Mode::Explore, Mode::Build) => {
                        // Capture current graph as Sound A, copy to Sound B.
                        sound_a.capture_all(&graph, &node_ids);
                        *sound_b = (*sound_a).clone();
                        // Set build_slot to first populated slot (or 0).
                        build_slot = 0;
                        for s in 0..NUM_SLOTS {
                            if effect_indices[s].is_some() {
                                build_slot = s;
                                break;
                            }
                        }
                        led2_blink_pattern = (build_slot + 1) as u8;
                        defmt::info!("→ BUILD mode, slot {}", build_slot + 1);
                    }
                    (Mode::Build, Mode::Morph) => {
                        // Sound B is already up-to-date from knob writes.
                        // Apply Sound A to graph (morph starts at t=0.0).
                        sound_a.apply_to_graph(&mut graph, &node_ids);
                        morph_t = 0.0;
                        defmt::info!("→ MORPH mode");
                    }
                    (Mode::Build, Mode::Explore) => {
                        // Sound B already captured from knob writes.
                        defmt::info!("→ EXPLORE mode");
                    }
                    (Mode::Morph, Mode::Explore) => {
                        defmt::info!("→ EXPLORE mode (from morph)");
                    }
                    (Mode::Morph, Mode::Build) => {
                        // Recapture A from graph (might be mid-morph), B stays.
                        sound_a.capture_all(&graph, &node_ids);
                        *sound_b = (*sound_a).clone();
                        build_slot = 0;
                        for s in 0..NUM_SLOTS {
                            if effect_indices[s].is_some() {
                                build_slot = s;
                                break;
                            }
                        }
                        led2_blink_pattern = (build_slot + 1) as u8;
                        defmt::info!("→ BUILD mode (from morph), slot {}", build_slot + 1);
                    }
                    (Mode::Explore, Mode::Morph) => {
                        // Capture A from current graph.
                        sound_a.capture_all(&graph, &node_ids);
                        *sound_b = (*sound_a).clone();
                        morph_t = 0.0;
                        defmt::info!("→ MORPH mode (from explore)");
                    }
                    _ => {} // same mode, shouldn't reach
                }
                mode = new_mode;
            }

            // ── Routing from T2 ──
            let new_routing = match t2 {
                0 => Routing::Serial,
                2 => Routing::Fan,
                _ => Routing::Parallel,
            };
            if new_routing != routing {
                routing = new_routing;
                needs_rebuild = true;
            }

            // ── T1 meaning depends on mode ──
            match mode {
                Mode::Explore => {
                    focused_slot = match t1 {
                        0 => 0,
                        2 => 2,
                        _ => 1,
                    };
                }
                Mode::Build => {
                    // T1 = preview selector (applied after knob updates below)
                    // 0=Sound A, 1=50% lerp, 2=Sound B
                }
                Mode::Morph => {
                    morph_speed = match t1 {
                        0 => 0.5,
                        2 => 5.0,
                        _ => 2.0,
                    };
                }
            }

            // ── Read knobs from ControlBuffer ──
            let norm_knobs: [f32; 6] = core::array::from_fn(|k| CONTROLS.read_knob(k));

            // ── Apply knobs based on mode ──
            match mode {
                Mode::Explore => {
                    // Knobs control focused slot's effect via curated mapping.
                    if let Some(eff_idx) = effect_indices[focused_slot]
                        && let Some(nid) = node_ids[focused_slot] {
                            let entry = &EFFECT_LIST[eff_idx];
                            if let Some(effect) = graph.effect_with_params_mut(nid) {
                                for k in 0..6 {
                                    let param_idx = entry.knobs[k];
                                    if param_idx != NULL_KNOB
                                        && let Some(desc) =
                                            effect.effect_param_info(param_idx as usize)
                                        {
                                            let val = adc_to_param(&desc, norm_knobs[k]);
                                            effect.effect_set_param(param_idx as usize, val);
                                        }
                                }
                            }
                        }
                }
                Mode::Build => {
                    // Knobs update Sound B for build_slot.
                    if let Some(eff_idx) = effect_indices[build_slot]
                        && let Some(nid) = node_ids[build_slot] {
                            let entry = &EFFECT_LIST[eff_idx];
                            // Get descriptors from graph to scale ADC properly.
                            // Read descriptors first (immutable borrow).
                            let mut param_vals: [(u8, f32); 6] = [(NULL_KNOB, 0.0); 6];
                            if let Some(effect) = graph.effect_with_params_ref(nid) {
                                for k in 0..6 {
                                    let param_idx = entry.knobs[k];
                                    if param_idx != NULL_KNOB
                                        && let Some(desc) =
                                            effect.effect_param_info(param_idx as usize)
                                        {
                                            param_vals[k] =
                                                (param_idx, adc_to_param(&desc, norm_knobs[k]));
                                        }
                                }
                            }
                            // Write to sound_b snapshot.
                            for &(pidx, val) in &param_vals {
                                if pidx != NULL_KNOB {
                                    sound_b.params[build_slot][pidx as usize] = val;
                                }
                            }
                        }

                    // Apply T1 preview.
                    match t1 {
                        0 => {
                            // Preview Sound A.
                            sound_a.apply_to_graph(&mut graph, &node_ids);
                        }
                        2 => {
                            // Preview Sound B (includes live knob values for build_slot).
                            sound_b.apply_to_graph(&mut graph, &node_ids);
                        }
                        _ => {
                            // 50% lerp preview.
                            interpolate_and_apply(
                                &mut graph, &node_ids, &sound_a, &sound_b, 0.5,
                            );
                        }
                    }
                }
                Mode::Morph => {
                    // K6 = fine speed override (0.2–10.0s). K1-K5 inactive.
                    morph_speed = 0.2 + norm_knobs[5] * 9.8;
                }
            }

            // ── Footswitch state machine (read from ControlBuffer) ──
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
            if mode == Mode::Morph && !both_pressed {
                let delta = 1.0 / (morph_speed * 100.0);
                if fs1_pressed && !fs2_pressed {
                    morph_t = if morph_t > delta { morph_t - delta } else { 0.0 };
                } else if fs2_pressed && !fs1_pressed {
                    morph_t = if morph_t + delta < 1.0 { morph_t + delta } else { 1.0 };
                }
            }

            // Both-FS tap detection.
            let both_tapped = both_held_peak > 0
                && both_held_peak < TAP_LIMIT
                && !fs1_pressed
                && !fs2_pressed
                && fs1_was_pressed
                && fs2_was_pressed;

            if both_tapped {
                match mode {
                    Mode::Build => {
                        // Save preset to ring buffer.
                        let slot = next_preset_slot;
                        presets[slot] = Preset {
                            slot_effects: effect_indices,
                            routing,
                            sound_a: (*sound_a).clone(),
                            sound_b: (*sound_b).clone(),
                            morph_speed,
                            occupied: true,
                        };
                        next_preset_slot = (slot + 1) % MAX_PRESETS;
                        defmt::info!("PRESET saved to slot {}", slot + 1);
                        CONTROLS.write_led(1, 1.0);
                        led2_counter = 20;
                    }
                    Mode::Explore => {
                        // Load preset (cycle through occupied slots).
                        let mut found = false;
                        for i in 0..MAX_PRESETS {
                            let idx = (load_preset_cursor + i) % MAX_PRESETS;
                            if presets[idx].occupied {
                                let preset = &presets[idx];
                                effect_indices = preset.slot_effects;
                                routing = preset.routing;
                                *sound_a = preset.sound_a.clone();
                                *sound_b = preset.sound_b.clone();
                                morph_speed = preset.morph_speed;
                                load_preset_cursor = (idx + 1) % MAX_PRESETS;
                                needs_rebuild = true;
                                defmt::info!("PRESET loaded from slot {}", idx + 1);
                                CONTROLS.write_led(1, 1.0);
                                led2_counter = 20;
                                found = true;
                                break;
                            }
                        }
                        if !found {
                            defmt::info!("No presets saved");
                        }
                    }
                    Mode::Morph => {} // Both-FS tap = no-op in morph (only hold=bypass).
                }
                both_held_peak = 0;
            }

            if !fs1_pressed && !fs2_pressed {
                both_held_peak = 0;
            }

            // ── FS1 release actions ──
            let was_both = both_held_peak > 0 || both_tapped;
            if fs1_was_pressed && !fs1_pressed && !was_both && both_held < BYPASS_HOLD
                && fs1_held < TAP_LIMIT {
                    match mode {
                        Mode::Explore => {
                            // Scroll LEFT in focused slot.
                            let cursor = &mut browse_cursor[focused_slot];
                            *cursor = if *cursor == 0 {
                                NUM_EFFECTS - 1
                            } else {
                                *cursor - 1
                            };
                            effect_indices[focused_slot] = Some(*cursor);
                            needs_rebuild = true;
                            defmt::info!(
                                "slot {} ← {}",
                                focused_slot + 1,
                                EFFECT_LIST[*cursor].id
                            );
                            CONTROLS.write_led(1, 1.0);
                            led2_counter = 10;
                        }
                        Mode::Build => {
                            // Capture current build_slot's B, go to previous slot.
                            // (sound_b already up-to-date from knob writes)
                            let prev = prev_populated(&effect_indices, build_slot);
                            if prev != build_slot {
                                build_slot = prev;
                                led2_blink_pattern = (build_slot + 1) as u8;
                                defmt::info!("BUILD ← slot {}", build_slot + 1);
                            }
                        }
                        Mode::Morph => {} // Morph uses held, not tap.
                    }
                }

            // ── FS2 release actions ──
            if fs2_was_pressed && !fs2_pressed && !was_both && both_held < BYPASS_HOLD
                && fs2_held < TAP_LIMIT {
                    match mode {
                        Mode::Explore => {
                            // Scroll RIGHT in focused slot.
                            let cursor = &mut browse_cursor[focused_slot];
                            *cursor = (*cursor + 1) % NUM_EFFECTS;
                            effect_indices[focused_slot] = Some(*cursor);
                            needs_rebuild = true;
                            defmt::info!(
                                "slot {} → {}",
                                focused_slot + 1,
                                EFFECT_LIST[*cursor].id
                            );
                            CONTROLS.write_led(1, 1.0);
                            led2_counter = 10;
                        }
                        Mode::Build => {
                            // Capture current build_slot's B, go to next slot.
                            // (sound_b already up-to-date from knob writes)
                            let next = next_populated(&effect_indices, build_slot);
                            if next != build_slot {
                                build_slot = next;
                                led2_blink_pattern = (build_slot + 1) as u8;
                                defmt::info!("BUILD → slot {}", build_slot + 1);
                            }
                        }
                        Mode::Morph => {} // Morph uses held, not tap.
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

            // ── Graph rebuild ──
            if needs_rebuild {
                // Preserve snapshots across rebuild.
                if mode == Mode::Build {
                    // Sound B is managed by knob writes, no extra capture needed.
                } else if mode == Mode::Explore {
                    // Capture current params before destroying old graph.
                    sound_a.capture_all(&graph, &node_ids);
                }

                let (new_graph, new_nodes) =
                    build_graph(&effect_indices, routing, SAMPLE_RATE, BLOCK_SIZE);
                graph = new_graph;
                node_ids = new_nodes;

                // Restore params to new graph.
                if mode == Mode::Build {
                    // Apply based on current T1 preview.
                    match t1 {
                        0 => sound_a.apply_to_graph(&mut graph, &node_ids),
                        2 => sound_b.apply_to_graph(&mut graph, &node_ids),
                        _ => interpolate_and_apply(
                            &mut graph, &node_ids, &sound_a, &sound_b, 0.5,
                        ),
                    }
                } else if mode == Mode::Explore {
                    sound_a.apply_to_graph(&mut graph, &node_ids);
                } else if mode == Mode::Morph {
                    interpolate_and_apply(
                        &mut graph, &node_ids, &sound_a, &sound_b, morph_t,
                    );
                }

                needs_rebuild = false;
                defmt::info!("graph rebuilt");
            }

            // ── Morph interpolation ──
            if mode == Mode::Morph {
                interpolate_and_apply(&mut graph, &node_ids, &sound_a, &sound_b, morph_t);
            }

            // ── LED2 feedback via ControlBuffer ──
            if BYPASSED.load(Ordering::Relaxed) {
                CONTROLS.write_led(1, 0.0);
            } else if led2_counter > 0 {
                led2_counter -= 1;
                if led2_counter == 0 {
                    CONTROLS.write_led(1, 0.0);
                }
            } else {
                match mode {
                    Mode::Explore => CONTROLS.write_led(1, 0.0),
                    Mode::Build => {
                        // Blink pattern = slot number (1/2/3 rapid pulses, pause).
                        // 100-tick cycle: pulses in first half, dark in second half.
                        let phase = poll_counter % 100;
                        let pulses = led2_blink_pattern as u16;
                        // Each pulse: 5 ticks on, 5 ticks off = 10 ticks per pulse.
                        let pulse_window = pulses * 10;
                        if phase < pulse_window {
                            let within = phase % 10;
                            if within < 5 {
                                CONTROLS.write_led(1, 1.0);
                            } else {
                                CONTROLS.write_led(1, 0.0);
                            }
                        } else {
                            CONTROLS.write_led(1, 0.0);
                        }
                    }
                    Mode::Morph => {
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
