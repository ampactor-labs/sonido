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

// ── Main ──────────────────────────────────────────────────────────────────

#[embassy_executor::main]
async fn main(spawner: embassy_executor::Spawner) {
    // D2 SRAM clocks are disabled at reset — enable before heap init.
    sonido_daisy::enable_d2_sram();

    // Initialize heap at D2 SRAM (256 KB)
    unsafe {
        HEAP.init(0x3000_8000, 256 * 1024);
    }

    let config = sonido_daisy::rcc_config(ClockProfile::Performance);
    let p = hal::init(config);

    // Heartbeat LED (PC7 = Daisy Seed user LED)
    let led = UserLed::new(p.PC7);
    spawner.spawn(heartbeat(led)).unwrap();

    defmt::info!("morph_pedal: initializing...");

    // ── Control pins ──

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

    defmt::info!("audio interface started");

    // ── Effect registry (created once, moved into closure) ──

    let registry = EffectRegistry::new();

    // ── Initial state ──

    let mut effect_indices: [usize; NUM_SLOTS] = [1, 6, 17]; // distortion, chorus, reverb
    let mut current_effects: [&str; NUM_SLOTS] = [
        ALL_EFFECTS[effect_indices[0]],
        ALL_EFFECTS[effect_indices[1]],
        ALL_EFFECTS[effect_indices[2]],
    ];
    let mut routing = Routing::Serial;
    let mut mode = Mode::Explore;

    let (mut graph, mut node_ids) = build_graph(&registry, &current_effects, routing);

    // Sound snapshots (capture defaults)
    let mut sound_a = SoundSnapshot::new();
    let mut sound_b = SoundSnapshot::new();
    sound_a.capture_from_graph(&graph, &node_ids);
    sound_b.capture_from_graph(&graph, &node_ids);

    let mut active_sound = ActiveSound::A;
    let mut focused_slot: usize = 0;

    // Morph state
    let mut morph_t: f32 = 0.0; // 0.0 = Sound A, 1.0 = Sound B
    let mut morph_speed: f32 = 2.0; // seconds for full morph

    // Preset storage (heap-resident, survives until power-off)
    let mut presets: [Preset; MAX_PRESETS] = core::array::from_fn(|_| Preset::empty());
    let mut next_preset_slot: usize = 0;

    // Footswitch state machine
    let mut fs1_held: u16 = 0;
    let mut fs2_held: u16 = 0;
    let mut both_held: u16 = 0;
    let mut both_held_peak: u16 = 0; // tracks max both_held before release
    let mut fs1_was_pressed = false;
    let mut fs2_was_pressed = false;

    // LED2 blink counter for BUILD mode
    let mut led2_counter: u16 = 0;

    let mut poll_counter: u16 = 0;
    let mut needs_rebuild = false;

    // Pre-allocate audio buffers for deinterleave/reinterleave
    let mut left_in = [0.0f32; BLOCK_SIZE];
    let mut right_in = [0.0f32; BLOCK_SIZE];
    let mut left_out = [0.0f32; BLOCK_SIZE];
    let mut right_out = [0.0f32; BLOCK_SIZE];

    defmt::info!("morph_pedal: ready — EXPLORE mode, distortion→chorus→reverb serial");

    // ── Audio + control callback ──

    defmt::unwrap!(
        interface
            .start_callback(move |input, output| {
                poll_counter = poll_counter.wrapping_add(1);

                // ── Control polling (~100 Hz) ──
                if poll_counter % POLL_EVERY == 0 {
                    // Read toggles
                    let t1 = decode_toggle(&tog1_up, &tog1_dn);
                    let t2 = decode_toggle(&tog2_up, &tog2_dn);
                    let t3 = decode_toggle(&tog3_up, &tog3_dn);

                    // Mode from T3
                    let new_mode = match t3 {
                        0 => Mode::Explore,
                        2 => Mode::Morph,
                        _ => Mode::Build,
                    };

                    // On mode change: save current snapshot if in BUILD
                    if new_mode != mode {
                        if mode == Mode::Build {
                            match active_sound {
                                ActiveSound::A => sound_a.capture_from_graph(&graph, &node_ids),
                                ActiveSound::B => sound_b.capture_from_graph(&graph, &node_ids),
                            }
                        }
                        mode = new_mode;
                    }

                    // Routing from T2
                    let new_routing = match t2 {
                        0 => Routing::Serial,
                        2 => Routing::Fan,
                        _ => Routing::Parallel,
                    };
                    if new_routing != routing {
                        routing = new_routing;
                        needs_rebuild = true;
                    }

                    // T1 meaning depends on mode
                    match mode {
                        Mode::Explore | Mode::Build => {
                            focused_slot = match t1 {
                                0 => 0, // UP = Slot 1
                                2 => 2, // DN = Slot 3
                                _ => 1, // MID = Slot 2
                            };
                        }
                        Mode::Morph => {
                            morph_speed = match t1 {
                                0 => 0.5, // UP = Fast
                                2 => 5.0, // DN = Slow
                                _ => 2.0, // MID = Medium
                            };
                        }
                    }

                    // ── Read knobs → set params on focused slot ──
                    let raw_knobs: [u16; 6] = [
                        adc.blocking_read(&mut knob_pins.0, KNOB_SAMPLE_TIME),
                        adc.blocking_read(&mut knob_pins.1, KNOB_SAMPLE_TIME),
                        adc.blocking_read(&mut knob_pins.2, KNOB_SAMPLE_TIME),
                        adc.blocking_read(&mut knob_pins.3, KNOB_SAMPLE_TIME),
                        adc.blocking_read(&mut knob_pins.4, KNOB_SAMPLE_TIME),
                        adc.blocking_read(&mut knob_pins.5, KNOB_SAMPLE_TIME),
                    ];

                    if mode != Mode::Morph {
                        // Map knobs to effect params on focused slot
                        if let Some(effect) = graph.effect_with_params_mut(node_ids[focused_slot]) {
                            let param_count = effect.effect_param_count();
                            for k in 0..6usize {
                                if k < param_count {
                                    let norm = raw_knobs[k] as f32 / ADC_MAX;
                                    if let Some(desc) = effect.effect_param_info(k) {
                                        let value = desc.min + norm * (desc.max - desc.min);
                                        effect.effect_set_param(k, value);
                                    }
                                }
                            }
                        }
                    } else {
                        // In MORPH mode, Knob 6 fine-tunes morph speed
                        let speed_knob = raw_knobs[5] as f32 / ADC_MAX;
                        morph_speed = 0.2 + speed_knob * 9.8; // 0.2s to 10s
                    }

                    // ── Footswitch state machine ──
                    let fs1_pressed = foot1.is_low();
                    let fs2_pressed = foot2.is_low();
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

                    // Both-FS bypass toggle (fire once at threshold)
                    if both_held == BYPASS_HOLD {
                        let was_bypassed = BYPASSED.load(Ordering::Relaxed);
                        BYPASSED.store(!was_bypassed, Ordering::Relaxed);
                        if was_bypassed {
                            led1.set_high();
                        } else {
                            led1.set_low();
                        }
                    }

                    // MORPH mode: continuous ramp while held (solo, not both)
                    if mode == Mode::Morph && !both_pressed {
                        // Morph rate: delta per poll = 1 / (morph_speed * 100Hz)
                        let delta = 1.0 / (morph_speed * 100.0);
                        if fs1_pressed && !fs2_pressed {
                            morph_t = (morph_t - delta).max(0.0); // Toward A
                        } else if fs2_pressed && !fs1_pressed {
                            morph_t = (morph_t + delta).min(1.0); // Toward B
                        }
                    }

                    // Detect both-FS tap: both were pressed, now either released,
                    // peak hold was short (< TAP_LIMIT) and didn't reach bypass.
                    let both_tapped = both_held_peak > 0
                        && both_held_peak < TAP_LIMIT
                        && (!fs1_pressed || !fs2_pressed)
                        && (fs1_was_pressed && fs2_was_pressed)
                        && (!fs1_pressed && !fs2_pressed);

                    if both_tapped {
                        match mode {
                            Mode::Explore => {
                                // Lock effect into slot — confirmation blink
                                defmt::info!(
                                    "LOCKED slot {} = {}",
                                    focused_slot + 1,
                                    current_effects[focused_slot]
                                );
                                led2.set_high();
                                led2_counter = 30; // longer blink = confirmation
                            }
                            _ => {} // No both-tap action in BUILD/MORPH
                        }
                        // Reset peak — don't fire individual FS actions
                        both_held_peak = 0;
                    }

                    // Reset peak when both released
                    if !fs1_pressed && !fs2_pressed {
                        both_held_peak = 0;
                    }

                    // FS1 release actions (skip if both-tap just fired)
                    let was_both = both_held_peak > 0 || both_tapped;
                    if fs1_was_pressed && !fs1_pressed && !was_both && both_held < BYPASS_HOLD {
                        match mode {
                            Mode::Explore => {
                                if fs1_held < TAP_LIMIT {
                                    // Previous effect
                                    let idx = &mut effect_indices[focused_slot];
                                    *idx = if *idx == 0 {
                                        ALL_EFFECTS.len() - 1
                                    } else {
                                        *idx - 1
                                    };
                                    current_effects[focused_slot] = ALL_EFFECTS[*idx];
                                    needs_rebuild = true;
                                    // Blink LED2
                                    led2.set_high();
                                    led2_counter = 10; // will turn off in 10 polls
                                }
                            }
                            Mode::Build => {
                                if fs1_held < TAP_LIMIT {
                                    // Toggle A/B editing
                                    match active_sound {
                                        ActiveSound::A => {
                                            sound_a.capture_from_graph(&graph, &node_ids);
                                            active_sound = ActiveSound::B;
                                            sound_b.apply_to_graph(&mut graph, &node_ids);
                                        }
                                        ActiveSound::B => {
                                            sound_b.capture_from_graph(&graph, &node_ids);
                                            active_sound = ActiveSound::A;
                                            sound_a.apply_to_graph(&mut graph, &node_ids);
                                        }
                                    }
                                }
                            }
                            Mode::Morph => {
                                if fs1_held < TAP_LIMIT {
                                    morph_t = 0.0; // Snap to A
                                }
                                // Hold = ramp (handled above), release = freeze (no-op)
                            }
                        }
                    }

                    // FS2 release actions (skip if both-tap just fired)
                    if fs2_was_pressed && !fs2_pressed && !was_both && both_held < BYPASS_HOLD {
                        match mode {
                            Mode::Explore => {
                                if fs2_held < TAP_LIMIT {
                                    // Next effect
                                    let idx = &mut effect_indices[focused_slot];
                                    *idx = (*idx + 1) % ALL_EFFECTS.len();
                                    current_effects[focused_slot] = ALL_EFFECTS[*idx];
                                    needs_rebuild = true;
                                    led2.set_high();
                                    led2_counter = 10;
                                }
                            }
                            Mode::Build => {
                                if fs2_held < TAP_LIMIT {
                                    // Save preset to next available slot
                                    match active_sound {
                                        ActiveSound::A => {
                                            sound_a.capture_from_graph(&graph, &node_ids)
                                        }
                                        ActiveSound::B => {
                                            sound_b.capture_from_graph(&graph, &node_ids)
                                        }
                                    }
                                    let slot = next_preset_slot;
                                    presets[slot] = Preset {
                                        effect_indices,
                                        routing,
                                        sound_a: sound_a.clone(),
                                        sound_b: sound_b.clone(),
                                        morph_speed,
                                        occupied: true,
                                    };
                                    next_preset_slot = (slot + 1) % MAX_PRESETS;
                                    defmt::info!("PRESET saved to slot {}", slot + 1);
                                    // Confirmation: blink LED2 (slot+1) times
                                    led2.set_high();
                                    led2_counter = 20;
                                }
                            }
                            Mode::Morph => {
                                if fs2_held < TAP_LIMIT {
                                    morph_t = 1.0; // Snap to B
                                }
                            }
                        }
                    }

                    // Reset hold counters on release
                    if !fs1_pressed {
                        fs1_held = 0;
                    }
                    if !fs2_pressed {
                        fs2_held = 0;
                    }
                    fs1_was_pressed = fs1_pressed;
                    fs2_was_pressed = fs2_pressed;

                    // ── Rebuild graph if needed ──
                    if needs_rebuild {
                        // Save current snapshot before rebuild
                        match active_sound {
                            ActiveSound::A => sound_a.capture_from_graph(&graph, &node_ids),
                            ActiveSound::B => sound_b.capture_from_graph(&graph, &node_ids),
                        }

                        let (new_graph, new_nodes) =
                            build_graph(&registry, &current_effects, routing);
                        graph = new_graph;
                        node_ids = new_nodes;

                        // Recapture defaults for changed slots, restore active sound
                        match active_sound {
                            ActiveSound::A => sound_a.apply_to_graph(&mut graph, &node_ids),
                            ActiveSound::B => sound_b.apply_to_graph(&mut graph, &node_ids),
                        }

                        needs_rebuild = false;
                    }

                    // ── Morph interpolation ──
                    if mode == Mode::Morph {
                        for slot in 0..NUM_SLOTS {
                            if let Some(effect) = graph.effect_with_params_mut(node_ids[slot]) {
                                let count =
                                    sound_a.param_counts[slot].min(sound_b.param_counts[slot]);
                                for p_idx in 0..count {
                                    let a = sound_a.params[slot][p_idx];
                                    let b = sound_b.params[slot][p_idx];
                                    let v = a + (b - a) * morph_t;
                                    effect.effect_set_param(p_idx, v);
                                }
                            }
                        }
                    }

                    // ── LED2 feedback ──
                    if led2_counter > 0 {
                        led2_counter -= 1;
                        if led2_counter == 0 {
                            led2.set_low();
                        }
                    } else {
                        match mode {
                            Mode::Explore => {
                                led2.set_low(); // Off unless blinking
                            }
                            Mode::Build => {
                                // Blink pattern: A = slow (1Hz), B = fast double (2Hz)
                                let phase = poll_counter % 100; // 100 polls = 1s
                                match active_sound {
                                    ActiveSound::A => {
                                        if phase < 10 {
                                            led2.set_high();
                                        } else {
                                            led2.set_low();
                                        }
                                    }
                                    ActiveSound::B => {
                                        if phase < 5 || (10..15).contains(&phase) {
                                            led2.set_high();
                                        } else {
                                            led2.set_low();
                                        }
                                    }
                                }
                            }
                            Mode::Morph => {
                                // PWM-like: on-time proportional to morph_t
                                let pwm_phase = poll_counter % 10; // 10 polls = 100ms
                                let threshold = (morph_t * 10.0) as u16;
                                if pwm_phase < threshold {
                                    led2.set_high();
                                } else {
                                    led2.set_low();
                                }
                            }
                        }
                    }
                }

                // ── Audio processing ──

                if BYPASSED.load(Ordering::Relaxed) {
                    output.copy_from_slice(input);
                    return;
                }

                // Deinterleave u32 → f32
                for i in 0..BLOCK_SIZE {
                    left_in[i] = u24_to_f32(input[i * 2]);
                    right_in[i] = u24_to_f32(input[i * 2 + 1]);
                }

                // Process through the graph
                graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);

                // Reinterleave f32 → u32
                for i in 0..BLOCK_SIZE {
                    output[i * 2] = f32_to_u24(left_out[i]);
                    output[i * 2 + 1] = f32_to_u24(right_out[i]);
                }
            })
            .await
    );
}
