//! Tier 5: Morph Pedal — three-slot effect processor with A/B morphing.
//!
//! The DigiTech Murray demo: browse 19 effects into 3 slots, shape two sounds,
//! morph between them with your feet. Uses the **identical code path** as the
//! desktop GUI and CLAP plugin: `EffectRegistry → KernelAdapter → ProcessingGraph`.
//!
//! # Three-Mode Architecture
//!
//! | Toggle 3 | Mode    | What it does                                    |
//! |----------|---------|-------------------------------------------------|
//! | UP       | EXPLORE | Browse effects into 3 slots, shape params       |
//! | CENTER   | BUILD   | Capture Sound A and Sound B parameter snapshots |
//! | DOWN     | MORPH   | Footswitch-controlled crossfade between A and B |
//!
//! Toggle 2 selects routing topology in all modes:
//! - UP: Serial (E1 → E2 → E3)
//! - CENTER: Parallel (split → E1,E2,E3 → merge)
//! - DOWN: Fan (E1 → split → E2,E3 → merge)
//!
//! # Hardware (Hothouse DIY)
//!
//! | Control     | Pin(s)      | Function                              |
//! |-------------|-------------|---------------------------------------|
//! | Knobs 1–6   | PA3,PB1,PA7,PA6,PC1,PC4 | Selected slot's first 6 params |
//! | Toggle 1    | PB4/PB5     | Slot (1/2/3) or morph speed           |
//! | Toggle 2    | PG10/PG11   | Routing topology                      |
//! | Toggle 3    | PD2/PC12    | Mode selector                         |
//! | Footswitch 1| PA0         | Mode-specific (see above)             |
//! | Footswitch 2| PD11        | Mode-specific (see above)             |
//! | LED 1       | PA5         | Active / bypassed                     |
//! | LED 2       | PA4         | Mode-specific feedback                |
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

extern crate alloc;

use core::sync::atomic::{AtomicBool, Ordering};

use defmt_rtt as _;
use embassy_stm32 as hal;
use embassy_stm32::adc::{Adc, SampleTime};
use embassy_stm32::gpio::{Input, Level, Output, Pull, Speed};
use embedded_alloc::LlffHeap as Heap;
use panic_probe as _;

use sonido_core::graph::{NodeId, ProcessingGraph};
use sonido_daisy::{
    BLOCK_SIZE, ClockProfile, SAMPLE_RATE, f32_to_u24, heartbeat, led::UserLed, u24_to_f32,
};
use sonido_registry::EffectRegistry;

// ── Heap ──────────────────────────────────────────────────────────────────

#[global_allocator]
static HEAP: Heap = Heap::empty();

// ── Constants ─────────────────────────────────────────────────────────────

/// All 19 effects in signal-chain order for browsing.
const ALL_EFFECTS: &[&str] = &[
    "preamp",
    "distortion",
    "bitcrusher",
    "compressor",
    "gate",
    "limiter",
    "chorus",
    "flanger",
    "phaser",
    "tremolo",
    "vibrato",
    "wah",
    "ringmod",
    "eq",
    "filter",
    "delay",
    "tape",
    "reverb",
    "stage",
];

/// Maximum parameters per effect slot (largest is Stage with 12).
const MAX_PARAMS: usize = 16;

/// Number of effect slots.
const NUM_SLOTS: usize = 3;

/// Control poll rate: every 15th block ≈ 100 Hz.
const POLL_EVERY: u16 = 15;

/// ADC sample time for potentiometers.
const KNOB_SAMPLE_TIME: SampleTime = SampleTime::CYCLES32_5;

/// Maximum raw ADC value (16-bit resolution).
const ADC_MAX: f32 = 65535.0;

/// Footswitch tap threshold: 30 polls × 10ms = 300ms.
const TAP_LIMIT: u16 = 30;

/// Both-footswitch bypass hold threshold: 100 polls × 10ms = 1s.
const BYPASS_HOLD: u16 = 100;

// ── Enums ─────────────────────────────────────────────────────────────────

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

/// Which sound is being edited in BUILD mode.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ActiveSound {
    A,
    B,
}

// ── Sound Snapshot ────────────────────────────────────────────────────────

/// Parameter snapshot for one sound (A or B) across all 3 slots.
#[derive(Clone)]
struct SoundSnapshot {
    /// Parameter values per slot.
    params: [[f32; MAX_PARAMS]; NUM_SLOTS],
    /// Number of parameters per slot.
    param_counts: [usize; NUM_SLOTS],
}

impl SoundSnapshot {
    fn new() -> Self {
        Self {
            params: [[0.0; MAX_PARAMS]; NUM_SLOTS],
            param_counts: [0; NUM_SLOTS],
        }
    }

    /// Capture current parameter values from a graph's effect nodes.
    fn capture_from_graph(&mut self, graph: &ProcessingGraph, node_ids: &[NodeId; NUM_SLOTS]) {
        for (slot, &nid) in node_ids.iter().enumerate() {
            if let Some(effect) = graph.effect_with_params_ref(nid) {
                let count = effect.effect_param_count().min(MAX_PARAMS);
                self.param_counts[slot] = count;
                for p in 0..count {
                    self.params[slot][p] = effect.effect_get_param(p);
                }
            }
        }
    }

    /// Apply snapshot values to a graph's effect nodes.
    fn apply_to_graph(&self, graph: &mut ProcessingGraph, node_ids: &[NodeId; NUM_SLOTS]) {
        for (slot, &nid) in node_ids.iter().enumerate() {
            if let Some(effect) = graph.effect_with_params_mut(nid) {
                for p in 0..self.param_counts[slot] {
                    effect.effect_set_param(p, self.params[slot][p]);
                }
            }
        }
    }
}

// ── Preset ───────────────────────────────────────────────────────────────

/// Maximum number of saveable presets (heap-resident, survives until power-off).
const MAX_PRESETS: usize = 9;

/// Complete pedal state stored as a preset.
/// Fields are read during preset load (Phase 3 — future work).
#[derive(Clone)]
#[allow(dead_code)]
struct Preset {
    /// Effect IDs (indices into ALL_EFFECTS) for each slot.
    effect_indices: [usize; NUM_SLOTS],
    /// Routing topology.
    routing: Routing,
    /// Sound A parameter snapshot.
    sound_a: SoundSnapshot,
    /// Sound B parameter snapshot.
    sound_b: SoundSnapshot,
    /// Morph speed in seconds.
    morph_speed: f32,
    /// Whether this slot has been written to.
    occupied: bool,
}

impl Preset {
    fn empty() -> Self {
        Self {
            effect_indices: [0; NUM_SLOTS],
            routing: Routing::Serial,
            sound_a: SoundSnapshot::new(),
            sound_b: SoundSnapshot::new(),
            morph_speed: 2.0,
            occupied: false,
        }
    }
}

// ── Toggle decode ────────────────────────────────────────────────────────

/// Decodes a 3-position toggle: UP=0, MID=1, DN=2.
fn decode_toggle(up: &Input<'_>, dn: &Input<'_>) -> u8 {
    match (up.is_low(), dn.is_low()) {
        (true, false) => 0, // UP
        (false, true) => 2, // DN
        _ => 1,             // MID (or fault)
    }
}

// ── Graph construction ───────────────────────────────────────────────────

/// Build a ProcessingGraph with 3 effects in the given routing topology.
///
/// Returns the graph and the NodeIds of the 3 effect nodes (for param access).
fn build_graph(
    registry: &EffectRegistry,
    effects: &[&str; NUM_SLOTS],
    routing: Routing,
) -> (ProcessingGraph, [NodeId; NUM_SLOTS]) {
    let sr = SAMPLE_RATE;
    let bs = BLOCK_SIZE;

    let mut g = ProcessingGraph::new(sr, bs);
    let inp = g.add_input();
    let out = g.add_output();

    let nodes: [NodeId; NUM_SLOTS] =
        core::array::from_fn(|i| g.add_effect(registry.create(effects[i], sr).unwrap()));

    match routing {
        Routing::Serial => {
            g.connect(inp, nodes[0]).unwrap();
            g.connect(nodes[0], nodes[1]).unwrap();
            g.connect(nodes[1], nodes[2]).unwrap();
            g.connect(nodes[2], out).unwrap();
        }
        Routing::Parallel => {
            let s = g.add_split();
            let m = g.add_merge();
            g.connect(inp, s).unwrap();
            for &n in &nodes {
                g.connect(s, n).unwrap();
                g.connect(n, m).unwrap();
            }
            g.connect(m, out).unwrap();
        }
        Routing::Fan => {
            let s = g.add_split();
            let m = g.add_merge();
            g.connect(inp, nodes[0]).unwrap();
            g.connect(nodes[0], s).unwrap();
            g.connect(s, nodes[1]).unwrap();
            g.connect(s, nodes[2]).unwrap();
            g.connect(nodes[1], m).unwrap();
            g.connect(nodes[2], m).unwrap();
            g.connect(m, out).unwrap();
        }
    }

    g.compile().unwrap();
    (g, nodes)
}

// ── Bypass state ─────────────────────────────────────────────────────────

/// Global bypass flag — audio callback checks this.
static BYPASSED: AtomicBool = AtomicBool::new(false);

/// Enables D-cache after audio DMA is running.
///
/// D-cache must be enabled AFTER SAI DMA is running — enabling during DMA
/// initialization causes bus matrix stalls that starve the DMA controller,
/// resulting in SAI overrun errors. This task is spawned before audio setup
/// (~50-200ms of codec I2C + SAI init), so 500ms total gives comfortable margin.
/// MPU Region 1 marks D2 SRAM non-cacheable, so DMA buffer coherency is safe.
#[embassy_executor::task]
async fn deferred_dcache() {
    embassy_time::Timer::after_millis(500).await;
    sonido_daisy::sdram::enable_dcache();
    defmt::info!("D-cache enabled");
}

// ── Main ──────────────────────────────────────────────────────────────────

#[embassy_executor::main]
async fn main(spawner: embassy_executor::Spawner) {
    let config = sonido_daisy::rcc_config(ClockProfile::Performance);
    let p = hal::init(config);

    // D2 SRAM clocks — needed for DMA buffers (.sram1_bss at 0x30000000).
    sonido_daisy::enable_d2_sram();

    // Initialize 64 MB SDRAM via FMC — configures MPU + I-cache + power-up sequence.
    // D-cache enabled later by deferred_dcache() task (must wait for audio DMA).
    let mut cp = unsafe { cortex_m::Peripherals::steal() };
    let sdram_ptr = sonido_daisy::init_sdram!(p, &mut cp.MPU, &mut cp.SCB);
    unsafe {
        HEAP.init(sdram_ptr as usize, sonido_daisy::sdram::SDRAM_SIZE);
    }

    // Heartbeat LED (PC7 = Daisy Seed user LED)
    let led = UserLed::new(p.PC7);
    spawner.spawn(heartbeat(led)).unwrap();

    defmt::info!("morph_pedal: initializing...");

    // ── Control pins FIRST (sync init, no .await needed) ──
    // Must happen BEFORE audio start_interface → start_callback so there's
    // zero gap for SAI TX FIFO to underrun. ADC calibration takes ~2ms
    // and would starve the SAI if done between start and callback.

    let mut adc = Adc::new(p.ADC1);
    let mut knob_pins = (
        p.PA3, // KNOB_1
        p.PB1, // KNOB_2
        p.PA7, // KNOB_3
        p.PA6, // KNOB_4
        p.PC1, // KNOB_5
        p.PC4, // KNOB_6
    );

    let tog1_up = Input::new(p.PB4, Pull::Up);
    let tog1_dn = Input::new(p.PB5, Pull::Up);
    let tog2_up = Input::new(p.PG10, Pull::Up);
    let tog2_dn = Input::new(p.PG11, Pull::Up);
    let tog3_up = Input::new(p.PD2, Pull::Up);
    let tog3_dn = Input::new(p.PC12, Pull::Up);

    let foot1 = Input::new(p.PA0, Pull::Up);
    let foot2 = Input::new(p.PD11, Pull::Up);

    let mut led1 = Output::new(p.PA5, Level::High, Speed::Low); // Active indicator
    let mut led2 = Output::new(p.PA4, Level::Low, Speed::Low); // Mode feedback

    defmt::info!("morph_pedal: controls initialized");

    // D-cache deferred: enabling during SAI DMA init causes overrun errors.
    // Enabling AFTER audio DMA is running works — MPU Region 1 protects D2 SRAM.
    spawner.spawn(deferred_dcache()).unwrap();

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

    defmt::info!("audio interface started — D-cache deferred (2s)");

    match interface
        .start_callback(|input, output| {
            output.copy_from_slice(input);
        })
        .await
    {
        Ok(infallible) => match infallible {},
        Err(_e) => {
            defmt::error!("SAI error in audio callback");
            led2.set_high(); // LED2 steady = SAI error
            loop {
                cortex_m::asm::wfi();
            }
        }
    }
}
