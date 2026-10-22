//! Sonido pedal: a rig player for the Hothouse (Daisy Seed).
//!
//! Every sound is a rig played by `sonido_patch::PatchPlayer`, the runtime the
//! graph-player plugin uses, with effects built by the same registry the
//! plugins use.
//!
//! # The rig list
//!
//! The pedal powers on into the rig it last played (hold FS1 while powering
//! on to start in the jam). Hold a footswitch (A or B mode) to step through
//! the list; LED 2 blinks the rig's position (the jam is 1):
//!
//! 1. **Jam**: three slots you fill on the pedal from the eight
//!    `PEDAL_EFFECT_IDS`, routed by T3. It is saved to flash as you play.
//! 2. **Bank rigs**: up to 15 rigs sent from the editor or CLI over USB.
//! 3. **Factory rigs**: three built-in rigs (`sonido_daisy::rigs`).
//!
//! # Controls
//!
//! | Control | Jam | Rig |
//! |---------|-----|-----|
//! | K1–K6 | Edit the T1 slot's A or B state: a knob takes over once you move it. In Morph: K5 morph time, K6 master | The rig's six macros |
//! | T1 | Slot 1 / 2 / 3 | — |
//! | T2 | A / B / Morph | A / B / Morph |
//! | T3 | Linear / Parallel / Fan | — |
//! | FS tap | FS1 previous, FS2 next effect in the T1 slot; FS1 double-tap clears it | FS2 tap tempo |
//! | FS hold (A/B) | FS1 previous, FS2 next rig | same |
//! | FS hold (Morph) | Ramp toward A (FS1) or B (FS2) | same |
//! | Both | Tap: bypass. Hold 3 s: USB bootloader (DFU) | same |
//!
//! Effects that would take the jam past the CPU rule
//! (`sonido_platform::cycle_table::chain_fits`) are skipped when scrolling.
//!
//! # USB
//!
//! The Seed's USB port is a serial device speaking `sonido_patch::link`: the
//! editor and `sonido pedal` (CLI) list, send, read and erase rigs, and read
//! the jam back. A rig sent to a slot is checked (effects, routing, CPU and
//! memory) before it is written, and plays at once.
//!
//! # Build & run
//!
//! ```bash
//! cd crates/sonido-daisy
//! # From RAM with a probe, logging load over RTT:
//! cargo run --release --example sonido_pedal --features alloc,platform
//! # Or flash for keeps through the Daisy bootloader:
//! cargo objcopy --example sonido_pedal --release --features alloc,platform -- -O binary sonido_pedal.bin
//! dfu-util -a 0 -s 0x90040000:leave -D sonido_pedal.bin
//! ```

#![no_std]
#![no_main]
#![allow(static_mut_refs)]
// Jam loops walk several parallel per-slot arrays at once.
#![allow(clippy::needless_range_loop)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU32, Ordering};

use defmt_rtt as _;
use embassy_stm32 as hal;
use embassy_stm32::{bind_interrupts, peripherals, usb};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Instant, Timer};
use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
use embedded_alloc::LlffHeap as Heap;
use panic_probe as _;
use static_cell::StaticCell;

use sonido_core::{EffectWithParams, ParamDescriptor, TempoContext};
use sonido_daisy::controls::HothouseBuffer;
use sonido_daisy::effect_slot::{self, BypassCrossfade, sanitize_stereo};
use sonido_daisy::hothouse::hothouse_control_task;
use sonido_daisy::patch_loader::{self, LoadError};
use sonido_daisy::qspi_flash::{PATCH_SLOT_COUNT, QspiFlash, STATE_SECTOR_ADDR, patch_slot_addr};
use sonido_daisy::rigs::{FACTORY_RIG_COUNT, factory_rig};
use sonido_daisy::tap_tempo::TapTempo;
use sonido_daisy::{
    BLOCK_SIZE, ClockProfile, SAMPLE_RATE, f32_to_u24, heartbeat, led::UserLed, u24_to_f32,
    usb_task,
};
use sonido_patch::link::{self, ErrorCode, FrameDecoder, Info, Reply, Request, Status};
use sonido_patch::pedal::{
    JAM_SLOTS, JamSlot, PedalState, RigRef, Topology, jam_node_indices, jam_patch,
};
use sonido_patch::{Patch, PatchPlayer, SECTOR_SIZE};
use sonido_platform::knob_mapping::{self, NULL_KNOB};
use sonido_platform::{adc_to_param, cycle_table};
use sonido_registry::{EFFECT_UIDS, EffectRegistry, PEDAL_EFFECT_IDS, effect_uid};

bind_interrupts!(struct Irqs {
    OTG_FS => usb::InterruptHandler<peripherals::USB_OTG_FS>;
});

const FIRMWARE: &str = concat!("sonido_pedal ", env!("CARGO_PKG_VERSION"));

// ── Heap and shared state ───────────────────────────────────────────────────

#[global_allocator]
static HEAP: Heap = Heap::empty();

static CONTROLS: HothouseBuffer = HothouseBuffer::new();

static BYPASSED: AtomicBool = AtomicBool::new(false);
/// Set while the rebuild task swaps the player: the callback plays dry and
/// leaves `PLAYER` alone.
static UPDATING: AtomicBool = AtomicBool::new(false);
/// Rig to load next, as a [`RigRef`] byte, or [`NO_REQUEST`].
static REQUEST: AtomicU16 = AtomicU16::new(NO_REQUEST);
const NO_REQUEST: u16 = 0x100;
/// LED 2 blinks requested by a task (rig position or error code).
static BLINK: AtomicU8 = AtomicU8::new(0);
/// The jam or the active rig changed since the state sector was written.
static STATE_DIRTY: AtomicBool = AtomicBool::new(false);
/// When it last changed (milliseconds since boot, wrapping).
static STATE_CHANGED_MS: AtomicU32 = AtomicU32::new(0);

// CPU load meter: the callback adds its own cycle count per block (DWT).
static LOAD_SUM: AtomicU32 = AtomicU32::new(0);
static LOAD_BLOCKS: AtomicU32 = AtomicU32::new(0);
static LOAD_PEAK: AtomicU32 = AtomicU32::new(0);
static LOAD_OVER: AtomicU32 = AtomicU32::new(0);
/// Last second's mean and peak, in tenths of a percent (for USB status).
static LAST_MEAN: AtomicU16 = AtomicU16::new(0);
static LAST_PEAK: AtomicU16 = AtomicU16::new(0);
const LOAD_BUDGET: u32 = 480_000_000 / 48_000 * BLOCK_SIZE as u32;

// SAFETY (all three): the executor is single-threaded and the audio callback
// runs between awaits, so nothing holds a reference across another's use.
// The rebuild task only touches `PLAYER` while `UPDATING` keeps the callback
// off it. No critical sections: they would block the DMA interrupts.
static mut PLAYER: Option<PatchPlayer> = None;
static mut CB: Option<Cb> = None;
static mut PEDAL: Option<Pedal> = None;

static REGISTRY: StaticCell<EffectRegistry> = StaticCell::new();
static mut REG: Option<&'static EffectRegistry> = None;
static FLASH: Mutex<CriticalSectionRawMutex, Option<QspiFlash<'static>>> = Mutex::new(None);

// ── Constants ───────────────────────────────────────────────────────────────

/// Footswitch tap/hold split, in control polls (100 Hz → 300 ms).
const TAP_LIMIT: u32 = 30;
/// Both footswitches held this long enters the DFU bootloader (100 Hz × 3 s).
const BOOTLOADER_HOLD_TICKS: u16 = 300;
/// Touch takeover: after a slot or mode change, a knob takes over its
/// parameter once it moves this far (fraction of travel). Untouched knobs leave
/// the stored values alone; there is no display to show where they are, so
/// waiting for a knob to sweep past a stored value (pickup) left knobs dead.
const TOUCH_TRAVEL: f32 = 0.02;
/// Two FS1 taps this close together clear the selected jam slot (ms).
const DOUBLE_TAP_MS: u64 = 350;
/// Write the state sector once nothing has changed for this long.
const SAVE_QUIET_MS: u32 = 2_000;
/// Tempo before the first tap.
const DEFAULT_BPM: f32 = 120.0;
/// LED 1 goes dark for this long at each beat.
const BEAT_FLASH_S: f32 = 0.07;
/// Longest rig list: jam, every bank slot, every factory rig.
const MAX_RIGS: usize = 1 + PATCH_SLOT_COUNT + FACTORY_RIG_COUNT;

// STM32H750 backup-SRAM bootloader trigger — see RM0433 §8.11 (AHB4ENR), §7.4 (PWR CR1).
const AHB4ENR_ADDR: *mut u32 = 0x5802_44E0 as *mut u32;
const AHB4ENR_BKPRAMEN: u32 = 1 << 28;
const PWR_CR1_ADDR: *mut u32 = 0x5802_4800 as *mut u32;
const PWR_CR1_DBP: u32 = 1 << 8;
const BACKUP_SRAM_ADDR: *mut u32 = 0x3880_0000 as *mut u32;
const DAISY_INFINITE_TIMEOUT: u32 = 0xB007_4EFA;

// ── The pedal's model ───────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    A,
    B,
    Morph,
}

impl Mode {
    fn from_toggle(position: u8) -> Self {
        match position {
            1 => Self::B,
            2 => Self::Morph,
            _ => Self::A,
        }
    }
}

/// What the pedal plays and the jam it keeps.
struct Pedal {
    /// The rig the player was last built from.
    active: RigRef,
    /// Bank slots holding a decodable rig.
    bank: [bool; PATCH_SLOT_COUNT],
    jam: [Option<JamSlot>; JAM_SLOTS],
    /// Per jam slot: 0 = empty, `n` = `PEDAL_EFFECT_IDS[n - 1]`.
    cursor: [usize; JAM_SLOTS],
    /// Per jam slot: B has been seeded from A (on the first move out of A).
    b_ready: [bool; JAM_SLOTS],
    topology: Topology,
    /// Each jam slot's node in the player, while the jam plays.
    nodes: [Option<usize>; JAM_SLOTS],
    /// Tapped tempo, applied to every rig (120 BPM until the first tap).
    bpm: f32,
}

impl Pedal {
    fn tempo(&self) -> TempoContext {
        TempoContext {
            bpm: self.bpm,
            sample_rate: SAMPLE_RATE,
            ..TempoContext::default()
        }
    }

    fn rigs(&self) -> ([RigRef; MAX_RIGS], usize) {
        let mut list = [RigRef::Jam; MAX_RIGS];
        let mut n = 1;
        for (slot, _) in self.bank.iter().enumerate().filter(|(_, ok)| **ok) {
            list[n] = RigRef::Bank(slot as u8);
            n += 1;
        }
        for i in 0..FACTORY_RIG_COUNT {
            list[n] = RigRef::Factory(i as u8);
            n += 1;
        }
        (list, n)
    }

    /// 1-based position of `rig` in the list (what LED 2 blinks).
    fn position(&self, rig: RigRef) -> usize {
        let (list, n) = self.rigs();
        list[..n]
            .iter()
            .position(|r| *r == rig)
            .map_or(1, |i| i + 1)
    }

    fn step(&self, dir: i32) -> RigRef {
        let (list, n) = self.rigs();
        let at = list[..n]
            .iter()
            .position(|r| *r == self.active)
            .unwrap_or(0);
        list[(at as i32 + dir).rem_euclid(n as i32) as usize]
    }

    fn has(&self, rig: RigRef) -> bool {
        match rig {
            RigRef::Jam => true,
            RigRef::Bank(n) => self.bank.get(n as usize).copied().unwrap_or(false),
            RigRef::Factory(n) => (n as usize) < FACTORY_RIG_COUNT,
        }
    }

    fn cursor_uid(cursor: usize) -> Option<u16> {
        cursor
            .checked_sub(1)
            .and_then(|i| PEDAL_EFFECT_IDS.get(i))
            .and_then(|id| effect_uid(id))
    }

    /// Step jam slot `slot` to the next effect in `dir`, skipping any that
    /// would break the CPU rule. Empty always fits, so the walk ends.
    fn scroll(&mut self, slot: usize, dir: i32) {
        let count = PEDAL_EFFECT_IDS.len() as i32 + 1;
        let mut cursor = self.cursor[slot];
        loop {
            cursor = (cursor as i32 + dir).rem_euclid(count) as usize;
            let uids = (0..JAM_SLOTS)
                .filter_map(|s| Self::cursor_uid(if s == slot { cursor } else { self.cursor[s] }));
            if cursor == 0 || cycle_table::chain_fits(uids) {
                break;
            }
        }
        self.cursor[slot] = cursor;
        self.b_ready[slot] = false;
    }
}

/// Everything the audio callback keeps between blocks.
struct Cb {
    xfade: BypassCrossfade,
    left: [f32; BLOCK_SIZE],
    right: [f32; BLOCK_SIZE],
    dry: [f32; BLOCK_SIZE],
    poll: u16,
    focused: usize,
    mode: Mode,
    morph_t: f32,
    /// Jam morph ramp, A/B positions per second (K5 in Morph).
    jam_speed: f32,
    /// Jam master volume (K6 in Morph), dB.
    jam_master_db: f32,
    /// Knob position when takeover was last re-armed, and whether each knob
    /// has since moved far enough to take over.
    anchor: [f32; 6],
    touched: [bool; 6],
    /// When FS1 was last tapped (ms since boot), for the double-tap clear.
    fs1_tap_ms: u64,
    fs1_held: u32,
    fs2_held: u32,
    fs1_was: bool,
    fs2_was: bool,
    both_held: u16,
    both_held_peak: u16,
    blink_left: u8,
    blink_timer: u16,
    led_env: f32,
    tap: TapTempo,
    /// Position within the current beat, 0–1 (drives the tempo LED).
    beat: f32,
    /// Level meters for the load report: input and output peak and energy.
    in_peak: f32,
    in_energy: f32,
    out_peak: f32,
    out_energy: f32,
    metered: u32,
}

fn mark_dirty() {
    STATE_DIRTY.store(true, Ordering::Release);
    STATE_CHANGED_MS.store(Instant::now().as_millis() as u32, Ordering::Release);
}

fn request(rig: RigRef) {
    REQUEST.store(u16::from(rig.to_byte()), Ordering::Release);
}

// ── Main ────────────────────────────────────────────────────────────────────

static EP_OUT_BUF: StaticCell<[u8; 256]> = StaticCell::new();
static CONFIG_DESC: StaticCell<[u8; 256]> = StaticCell::new();
static BOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
static MSOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();
static CDC_STATE: StaticCell<State> = StaticCell::new();

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
    defmt::info!("{}: booting", FIRMWARE);

    let ctrl = sonido_daisy::hothouse_pins!(p);
    spawner
        .spawn(hothouse_control_task(ctrl, &CONTROLS))
        .unwrap();
    CONTROLS.write_led(0, 1.0);

    // ── Flash: state sector and rig bank ──
    let mut flash = QspiFlash::new(p.QUADSPI, p.PF8, p.PF9, p.PF7, p.PF6, p.PF10, p.PG6);
    let mut sector = Box::new([0u8; SECTOR_SIZE]);
    flash.read(STATE_SECTOR_ADDR, &mut sector[..]).await;
    let state = PedalState::decode(&sector).unwrap_or_default();
    let mut bank = [false; PATCH_SLOT_COUNT];
    for (slot, ok) in bank.iter_mut().enumerate() {
        flash.read(patch_slot_addr(slot), &mut sector[..]).await;
        *ok = sonido_patch::decode(&sector[..]).is_ok();
    }
    drop(sector);
    *FLASH.lock().await = Some(flash);
    defmt::info!(
        "rig bank: {} of {} slots hold rigs",
        bank.iter().filter(|b| **b).count(),
        PATCH_SLOT_COUNT
    );

    unsafe {
        REG = Some(REGISTRY.init(EffectRegistry::new()));
    }

    embassy_time::Timer::after_millis(30).await;
    let topology = Topology::from_toggle(CONTROLS.read_toggle(2));
    let mut jam: [Option<JamSlot>; JAM_SLOTS] = Default::default();
    let mut cursor = [0usize; JAM_SLOTS];
    let mut b_ready = [false; JAM_SLOTS];
    for (s, slot) in state.jam.into_iter().enumerate() {
        // Keep only jam effects the scroll list knows.
        if let Some(slot) = slot
            && let Some(i) = PEDAL_EFFECT_IDS
                .iter()
                .position(|id| effect_uid(id) == Some(slot.effect_uid))
        {
            cursor[s] = i + 1;
            b_ready[s] = slot.params_a != slot.params_b;
            jam[s] = Some(slot);
        }
    }
    let mut pedal = Pedal {
        active: RigRef::Jam,
        bank,
        jam,
        cursor,
        b_ready,
        topology,
        nodes: [None; JAM_SLOTS],
        bpm: state.bpm.unwrap_or(DEFAULT_BPM),
    };
    // Power on into the last rig played; FS1 held at power-on starts the jam.
    let start = if CONTROLS.read_footswitch(0) {
        defmt::info!("FS1 held at power-on: starting in the jam");
        RigRef::Jam
    } else if pedal.has(state.active) {
        state.active
    } else {
        RigRef::Jam
    };
    pedal.active = start;
    unsafe {
        PEDAL = Some(pedal);
        CB = Some(Cb {
            xfade: BypassCrossfade::new(SAMPLE_RATE),
            left: [0.0; BLOCK_SIZE],
            right: [0.0; BLOCK_SIZE],
            dry: [0.0; BLOCK_SIZE],
            poll: 0,
            focused: (CONTROLS.read_toggle(0) as usize).min(JAM_SLOTS - 1),
            mode: Mode::from_toggle(CONTROLS.read_toggle(1)),
            morph_t: 0.0,
            jam_speed: 1.0,
            jam_master_db: 0.0,
            anchor: core::array::from_fn(|k| CONTROLS.read_knob(k)),
            touched: [false; 6],
            fs1_tap_ms: 0,
            fs1_held: 0,
            fs2_held: 0,
            fs1_was: false,
            fs2_was: false,
            both_held: 0,
            both_held_peak: 0,
            blink_left: 0,
            blink_timer: 0,
            led_env: 0.0,
            tap: TapTempo::new(),
            beat: 0.0,
            in_peak: 0.0,
            in_energy: 0.0,
            out_peak: 0.0,
            out_energy: 0.0,
            metered: 0,
        });
    }
    // Build the first rig before the codec starts: building a rig is
    // milliseconds of work that running audio would have to wait out.
    switch_to(start).await;

    // ── USB serial (rig link) ──
    let driver = usb::Driver::new_fs(
        p.USB_OTG_FS,
        Irqs,
        p.PA12,
        p.PA11,
        EP_OUT_BUF.init([0u8; 256]),
        usb::Config::default(),
    );
    let mut usb_config = embassy_usb::Config::new(0x1209, 0x0001);
    usb_config.manufacturer = Some("Sonido");
    usb_config.product = Some("Sonido Pedal");
    usb_config.serial_number = Some("sonido-pedal");
    let mut builder = embassy_usb::Builder::new(
        driver,
        usb_config,
        CONFIG_DESC.init([0; 256]),
        BOS_DESC.init([0; 256]),
        MSOS_DESC.init([0; 256]),
        CONTROL_BUF.init([0; 64]),
    );
    let class = CdcAcmClass::new(&mut builder, CDC_STATE.init(State::new()), 64);
    spawner.spawn(usb_task(builder.build())).unwrap();
    spawner.spawn(link_task(class)).unwrap();

    // ── Audio ──
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

    spawner.spawn(rebuild_task()).unwrap();
    spawner.spawn(save_task()).unwrap();
    spawner.spawn(load_report_task()).unwrap();

    defmt::unwrap!(
        interface
            .start_callback(|input, output| {
                let t0 = cortex_m::peripheral::DWT::cycle_count();
                audio_block(input, output);
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

// ── Audio callback ──────────────────────────────────────────────────────────

fn audio_block(input: &[u32], output: &mut [u32]) {
    let cb = unsafe { CB.as_mut().unwrap() };

    if UPDATING.load(Ordering::Acquire) {
        // The player is being swapped: ramp to dry so the change doesn't click.
        cb.xfade.set_active(false);
        for i in 0..BLOCK_SIZE {
            let mono = (u24_to_f32(input[i * 2]) + u24_to_f32(input[i * 2 + 1])) * 0.5;
            let (l, r) = cb.xfade.advance(mono, mono, 0.0, 0.0);
            output[i * 2] = f32_to_u24(l);
            output[i * 2 + 1] = f32_to_u24(r);
        }
        return;
    }

    if BYPASSED.load(Ordering::Relaxed) {
        output.copy_from_slice(input);
        cb.poll = cb.poll.wrapping_add(1);
        if cb.poll.is_multiple_of(effect_slot::CONTROL_POLL_EVERY) {
            let fs1 = CONTROLS.read_footswitch(0);
            let fs2 = CONTROLS.read_footswitch(1);
            accumulate_both_held(cb, fs1, fs2);
            handle_both_release_toggle(cb, fs1, fs2);
        }
        return;
    }

    cb.xfade.set_active(true);
    for i in 0..BLOCK_SIZE {
        let mono = (u24_to_f32(input[i * 2]) + u24_to_f32(input[i * 2 + 1])) * 0.5;
        cb.dry[i] = mono;
        cb.left[i] = mono;
        cb.right[i] = mono;
        cb.in_peak = cb.in_peak.max(mono.abs());
        cb.in_energy += mono * mono;
    }
    cb.metered += BLOCK_SIZE as u32;
    if let Some(player) = unsafe { PLAYER.as_mut() } {
        player.process_block_stereo(&mut cb.left, &mut cb.right);
    }
    let mut peak = 0.0f32;
    for i in 0..BLOCK_SIZE {
        let (wet_l, wet_r) = sanitize_stereo(cb.left[i], cb.right[i]);
        let (l, r) = cb.xfade.advance(cb.dry[i], cb.dry[i], wet_l, wet_r);
        peak = peak.max(l.abs()).max(r.abs());
        cb.out_energy += 0.5 * (l * l + r * r);
        output[i * 2] = f32_to_u24(l);
        output[i * 2 + 1] = f32_to_u24(r);
    }

    cb.out_peak = cb.out_peak.max(peak);
    cb.poll = cb.poll.wrapping_add(1);
    if cb.poll.is_multiple_of(effect_slot::CONTROL_POLL_EVERY) {
        controls(cb, peak);
    }
}

/// The 100 Hz control tick: toggles, footswitches, knobs, morph, LEDs.
fn controls(cb: &mut Cb, peak: f32) {
    let pedal = unsafe { PEDAL.as_mut().unwrap() };
    let Some(player) = (unsafe { PLAYER.as_mut() }) else {
        return;
    };
    let jam = pedal.active == RigRef::Jam;

    // ── Toggles ──
    let focused = (CONTROLS.read_toggle(0) as usize).min(JAM_SLOTS - 1);
    if focused != cb.focused {
        cb.focused = focused;
        rearm_takeover(cb);
    }
    let mode = Mode::from_toggle(CONTROLS.read_toggle(1));
    if mode != cb.mode {
        if mode == Mode::Morph {
            cb.morph_t = if cb.mode == Mode::B { 1.0 } else { 0.0 };
        }
        if jam && mode != Mode::A {
            seed_b(pedal, player);
        }
        cb.mode = mode;
        rearm_takeover(cb);
    }
    let topology = Topology::from_toggle(CONTROLS.read_toggle(2));
    if topology != pedal.topology {
        pedal.topology = topology;
        if jam {
            request(RigRef::Jam);
        }
        mark_dirty();
    }

    // ── Footswitches ──
    let fs1 = CONTROLS.read_footswitch(0);
    let fs2 = CONTROLS.read_footswitch(1);
    accumulate_both_held(cb, fs1, fs2);
    if fs1 {
        cb.fs1_held += 1;
    }
    if fs2 {
        cb.fs2_held += 1;
    }
    let speed = if jam {
        cb.jam_speed
    } else {
        player.morph_speed().max(0.05)
    };
    if cb.mode == Mode::Morph && !(fs1 && fs2) {
        let step = speed * 0.01;
        if fs1 && !fs2 {
            cb.morph_t = (cb.morph_t - step).max(0.0);
        } else if fs2 && !fs1 {
            cb.morph_t = (cb.morph_t + step).min(1.0);
        }
    }
    let was_both = cb.both_held_peak > 0;
    handle_both_release_toggle(cb, fs1, fs2);
    if cb.mode != Mode::Morph && !was_both {
        if cb.fs1_was && !fs1 {
            let now = Instant::now().as_millis();
            if cb.fs1_held >= TAP_LIMIT {
                request(pedal.step(-1));
            } else if jam && now.saturating_sub(cb.fs1_tap_ms) < DOUBLE_TAP_MS {
                // Double tap: clear the selected slot.
                pedal.cursor[cb.focused] = 0;
                pedal.b_ready[cb.focused] = false;
                request(RigRef::Jam);
                mark_dirty();
                cb.fs1_tap_ms = 0;
            } else if jam {
                pedal.scroll(cb.focused, -1);
                request(RigRef::Jam);
                mark_dirty();
                cb.fs1_tap_ms = now;
            }
        }
        if cb.fs2_was && !fs2 {
            if cb.fs2_held >= TAP_LIMIT {
                request(pedal.step(1));
            } else if jam {
                pedal.scroll(cb.focused, 1);
                request(RigRef::Jam);
                mark_dirty();
            } else {
                cb.tap.tap(Instant::now().as_ticks());
                cb.beat = 0.0;
                if let Some(bpm) = cb.tap.bpm() {
                    pedal.bpm = bpm;
                    player.set_tempo_context(&pedal.tempo());
                    mark_dirty();
                }
            }
        }
    }
    if !fs1 {
        cb.fs1_held = 0;
    }
    if !fs2 {
        cb.fs2_held = 0;
    }
    cb.fs1_was = fs1;
    cb.fs2_was = fs2;

    // ── Knobs ──
    if !jam {
        for k in 0..6 {
            player.set_macro(k, CONTROLS.read_knob(k));
        }
    } else if cb.mode == Mode::Morph {
        const TIME: ParamDescriptor =
            ParamDescriptor::custom("Morph Time", "Morph Time", 0.1, 5.0, 1.0);
        const MASTER: ParamDescriptor =
            ParamDescriptor::gain_db("Master", "Master", -60.0, 12.0, 0.0);
        cb.jam_speed = 1.0 / adc_to_param(&TIME, CONTROLS.read_knob(4));
        cb.jam_master_db = adc_to_param(&MASTER, CONTROLS.read_knob(5));
    } else {
        edit_jam_slot(cb, pedal, player);
    }
    if jam {
        let master = if cb.mode == Mode::Morph {
            cb.jam_master_db
        } else {
            0.0
        };
        player.set_master_volume_db(master);
    }

    // ── Morph ──
    let t = match cb.mode {
        Mode::A => 0.0,
        Mode::B => 1.0,
        Mode::Morph => cb.morph_t,
    };
    player.set_morph_position(t);
    player.apply_controls();

    // ── LEDs ──
    let blink = BLINK.swap(0, Ordering::Acquire);
    if blink > 0 {
        cb.blink_left = blink * 2;
        cb.blink_timer = 0;
    }
    // (Bypass skips this tick entirely; LED 1 is off then.)
    // LED 1: on while the pedal is on, dark for a moment on every beat.
    cb.beat += 0.01 * pedal.bpm / 60.0;
    if cb.beat >= 1.0 {
        cb.beat -= 1.0;
    }
    let beat_dark = cb.beat < BEAT_FLASH_S * pedal.bpm / 60.0;
    CONTROLS.write_led(0, if beat_dark { 0.0 } else { 1.0 });
    // LED 2: rig/error blinks, else the morph position, else the output level.
    if cb.blink_left > 0 {
        cb.blink_timer += 1;
        if cb.blink_timer >= 12 {
            cb.blink_timer = 0;
            cb.blink_left -= 1;
        }
        let on = cb.blink_left % 2 == 1;
        CONTROLS.write_led(1, if on { 1.0 } else { 0.0 });
    } else if cb.mode == Mode::Morph {
        let on = cb.poll % 10 < (cb.morph_t * 10.0) as u16;
        CONTROLS.write_led(1, if on { 1.0 } else { 0.0 });
    } else {
        cb.led_env = if peak > cb.led_env {
            peak
        } else {
            cb.led_env * 0.9
        };
        CONTROLS.write_led(1, (cb.led_env * 2.0).min(1.0));
    }
}

/// Re-arm touch takeover: knobs edit again only once moved from where they are.
fn rearm_takeover(cb: &mut Cb) {
    cb.anchor = core::array::from_fn(|k| CONTROLS.read_knob(k));
    cb.touched = [false; 6];
}

/// On the first move out of A, B starts as a copy of A (per jam slot).
fn seed_b(pedal: &mut Pedal, player: &mut PatchPlayer) {
    for s in 0..JAM_SLOTS {
        if pedal.b_ready[s] {
            continue;
        }
        if let (Some(slot), Some(node)) = (pedal.jam[s].as_mut(), pedal.nodes[s]) {
            slot.params_b.copy_from_slice(&slot.params_a);
            for (p, &v) in slot.params_a.iter().enumerate() {
                player.set_corner_param(node, 1, p, v);
            }
            pedal.b_ready[s] = true;
            mark_dirty();
        }
    }
}

/// Knobs edit the focused jam slot's A or B state, through the effect's knob
/// map, once each knob has been moved (touch takeover).
fn edit_jam_slot(cb: &mut Cb, pedal: &mut Pedal, player: &mut PatchPlayer) {
    let s = cb.focused;
    let (Some(node), Some(id)) = (
        pedal.nodes[s],
        pedal.cursor[s]
            .checked_sub(1)
            .and_then(|i| PEDAL_EFFECT_IDS.get(i)),
    ) else {
        return;
    };
    let Some(slot) = pedal.jam[s].as_mut() else {
        return;
    };
    let corner = usize::from(cb.mode == Mode::B);
    let map = knob_mapping::knob_map(id).unwrap_or([NULL_KNOB; 6]);
    for (k, &idx) in map.iter().enumerate() {
        if idx == NULL_KNOB {
            continue;
        }
        let idx = idx as usize;
        let Some(desc) = player.engine().param_descriptor_at(node, idx) else {
            continue;
        };
        let value = knob_mapping::knob_to_param(id, idx, &desc, CONTROLS.read_knob(k));
        let current = player.corner_param(node, corner, idx).unwrap_or(value);
        if !cb.touched[k] {
            if (CONTROLS.read_knob(k) - cb.anchor[k]).abs() < TOUCH_TRAVEL {
                continue;
            }
            cb.touched[k] = true;
        }
        if (value - current).abs() > f32::EPSILON {
            player.set_corner_param(node, corner, idx, value);
            let params = if corner == 0 {
                &mut slot.params_a
            } else {
                &mut slot.params_b
            };
            if let Some(v) = params.get_mut(idx) {
                *v = value;
            }
            mark_dirty();
        }
    }
}

// ── Rig loading ─────────────────────────────────────────────────────────────

#[embassy_executor::task]
async fn rebuild_task() {
    loop {
        Timer::after_millis(5).await;
        let req = REQUEST.swap(NO_REQUEST, Ordering::AcqRel);
        if req == NO_REQUEST {
            continue;
        }
        switch_to(RigRef::from_byte(req as u8)).await;
    }
}

/// Load `rig` and blink its position in the rig list, or the refusal's code.
async fn switch_to(rig: RigRef) {
    match load(rig).await {
        Ok(()) => {
            let pedal = unsafe { PEDAL.as_ref().unwrap() };
            BLINK.store(pedal.position(rig) as u8, Ordering::Release);
        }
        Err(e) => {
            let code = patch_loader::error_blink_code(&e);
            defmt::warn!("rig {} refused (blink code {})", rig.to_byte(), code);
            BLINK.store(code, Ordering::Release);
        }
    }
}

/// Load `rig` into the player. Other rigs are read and checked before the old
/// one stops, so a refused rig leaves the current one playing. A jam rebuild
/// keeps the effects of slots that did not change, with their tails.
async fn load(rig: RigRef) -> Result<(), LoadError> {
    let registry = unsafe { REG.unwrap() };
    let patch = match rig {
        RigRef::Jam => None,
        RigRef::Bank(slot) => {
            let mut sector = Box::new([0u8; SECTOR_SIZE]);
            let mut guard = FLASH.lock().await;
            let flash = guard.as_mut().unwrap();
            let patch = patch_loader::load_patch(flash, slot as usize, &mut sector).await?;
            drop(guard);
            patch_loader::check_rig(&patch).map_err(LoadError::Refused)?;
            Some(patch)
        }
        RigRef::Factory(i) => {
            let patch = factory_rig(i as usize).ok_or(LoadError::Refused(ErrorCode::BadSlot))?;
            patch_loader::check_rig(&patch).map_err(LoadError::Refused)?;
            Some(patch)
        }
    };

    // Ramp to dry, then take the old player off the audio path.
    UPDATING.store(true, Ordering::Release);
    Timer::after_millis(8).await;
    let pedal = unsafe { PEDAL.as_mut().unwrap() };
    let old = unsafe { PLAYER.take() };

    let mut kept: [Option<Box<dyn EffectWithParams + Send>>; JAM_SLOTS] = Default::default();
    let patch = match patch {
        Some(patch) => {
            drop(old);
            patch
        }
        None => {
            // Keep the jam effects whose slot still holds the same effect.
            if let Some(old) = old {
                let was_jam = pedal.active == RigRef::Jam;
                let mut effects: Vec<_> = old.into_effects().into_iter().map(Some).collect();
                if was_jam {
                    for s in 0..JAM_SLOTS {
                        if let Some(node) = pedal.nodes[s] {
                            kept[s] = effects.get_mut(node).and_then(Option::take);
                        }
                    }
                }
            }
            for s in 0..JAM_SLOTS {
                let want = Pedal::cursor_uid(pedal.cursor[s]);
                let have = pedal.jam[s].as_ref().map(|j| j.effect_uid);
                if want != have {
                    kept[s] = None;
                    pedal.jam[s] = want.and_then(|uid| {
                        let (mut effect, id) =
                            patch_loader::make_effect(registry, uid, SAMPLE_RATE)?;
                        // A new effect starts where the knobs are, not at its
                        // defaults: the knobs are the truth.
                        let map = knob_mapping::knob_map(id).unwrap_or([NULL_KNOB; 6]);
                        for (k, &idx) in map.iter().enumerate() {
                            if let Some(desc) = (idx != NULL_KNOB)
                                .then(|| effect.effect_param_info(idx as usize))
                                .flatten()
                            {
                                let value = knob_mapping::knob_to_param(
                                    id,
                                    idx as usize,
                                    &desc,
                                    CONTROLS.read_knob(k),
                                );
                                effect.effect_set_param(idx as usize, value);
                            }
                        }
                        effect.reset();
                        let params: Vec<f32> = (0..effect.effect_param_count())
                            .map(|i| effect.effect_get_param(i))
                            .collect();
                        kept[s] = Some(effect);
                        Some(JamSlot {
                            effect_uid: uid,
                            params_a: params.clone(),
                            params_b: params,
                        })
                    });
                }
                Timer::after_micros(1).await;
            }
            jam_patch(&pedal.jam, pedal.topology)
        }
    };

    // Build each node's effect, yielding between them so audio keeps up.
    let jam_nodes = jam_node_indices(&pedal.jam);
    let mut effects: Vec<Option<(Box<dyn EffectWithParams + Send>, &'static str)>> = Vec::new();
    for (i, node) in patch.nodes.iter().enumerate() {
        let reused = (rig == RigRef::Jam)
            .then(|| (0..JAM_SLOTS).find(|&s| jam_nodes[s] == Some(i)))
            .flatten()
            .and_then(|s| kept[s].take())
            .zip(sonido_registry::effect_by_uid(node.effect_uid));
        effects.push(
            reused.or_else(|| patch_loader::make_effect(registry, node.effect_uid, SAMPLE_RATE)),
        );
        Timer::after_micros(1).await;
    }
    let mut effects = effects.into_iter();
    let built = PatchPlayer::from_patch(&patch, SAMPLE_RATE, BLOCK_SIZE, |_, _| {
        effects.next().flatten()
    });
    let result = match built {
        Ok(mut player) => {
            if let Some(cb) = unsafe { CB.as_ref() } {
                let t = match cb.mode {
                    Mode::A => 0.0,
                    Mode::B => 1.0,
                    Mode::Morph => cb.morph_t,
                };
                player.set_morph_position(t);
            }
            if rig != RigRef::Jam {
                for k in 0..6 {
                    player.set_macro(k, CONTROLS.read_knob(k));
                }
            }
            player.set_tempo_context(&pedal.tempo());
            player.apply_controls_now();
            unsafe {
                PLAYER = Some(player);
            }
            if pedal.active != rig {
                mark_dirty();
            }
            pedal.active = rig;
            pedal.nodes = if rig == RigRef::Jam {
                jam_nodes
            } else {
                [None; JAM_SLOTS]
            };
            defmt::info!(
                "playing rig {} ({} nodes)",
                rig.to_byte(),
                patch.nodes.len()
            );
            Ok(())
        }
        Err(e) => {
            // Leave a playable pedal: fall back to the jam.
            if rig != RigRef::Jam {
                request(RigRef::Jam);
            }
            Err(LoadError::Build(e))
        }
    };
    if let Some(cb) = unsafe { CB.as_mut() } {
        rearm_takeover(cb);
    }
    UPDATING.store(false, Ordering::Release);
    result
}

// ── State sector ────────────────────────────────────────────────────────────

#[embassy_executor::task]
async fn save_task() {
    let mut sector = Box::new([0u8; SECTOR_SIZE]);
    loop {
        Timer::after_millis(500).await;
        let quiet = (Instant::now().as_millis() as u32)
            .wrapping_sub(STATE_CHANGED_MS.load(Ordering::Acquire));
        if quiet < SAVE_QUIET_MS || !STATE_DIRTY.swap(false, Ordering::AcqRel) {
            continue;
        }
        let state = {
            let pedal = unsafe { PEDAL.as_ref().unwrap() };
            PedalState {
                active: pedal.active,
                topology: pedal.topology,
                jam: pedal.jam.clone(),
                bpm: Some(pedal.bpm),
            }
        };
        if state.encode(&mut sector).is_err() {
            defmt::warn!("state does not fit its sector");
            continue;
        }
        let mut guard = FLASH.lock().await;
        if let Some(flash) = guard.as_mut() {
            match flash.write_sector(STATE_SECTOR_ADDR, &sector).await {
                Ok(()) => defmt::info!("state saved"),
                Err(e) => defmt::warn!("state save failed: {}", e),
            }
        }
    }
}

// ── USB rig link ────────────────────────────────────────────────────────────

static DECODER: StaticCell<FrameDecoder> = StaticCell::new();
static RX: StaticCell<[u8; SECTOR_SIZE]> = StaticCell::new();
static TX: StaticCell<[u8; link::MAX_FRAME]> = StaticCell::new();

#[embassy_executor::task]
async fn link_task(mut class: CdcAcmClass<'static, usb::Driver<'static, peripherals::USB_OTG_FS>>) {
    let decoder = DECODER.init(FrameDecoder::new());
    let rx = RX.init([0u8; SECTOR_SIZE]);
    let tx = TX.init([0u8; link::MAX_FRAME]);
    loop {
        class.wait_connection().await;
        defmt::info!("USB link connected");
        loop {
            let mut packet = [0u8; 64];
            let Ok(n) = class.read_packet(&mut packet).await else {
                break;
            };
            let mut got = None;
            decoder.feed(&packet[..n], |f| {
                rx[..f.payload.len()].copy_from_slice(f.payload);
                got = Some((f.kind, f.arg, f.payload.len()));
            });
            let Some((kind, arg, len)) = got else {
                continue;
            };
            let (reply, reply_arg, payload) = handle(kind, arg, &rx[..len]).await;
            let Some(total) = link::encode_frame(reply as u8, reply_arg, &payload, tx) else {
                continue;
            };
            let mut ok = true;
            for chunk in tx[..total].chunks(64) {
                if class.write_packet(chunk).await.is_err() {
                    ok = false;
                    break;
                }
            }
            if ok && total % 64 == 0 {
                ok = class.write_packet(&[]).await.is_ok();
            }
            if !ok {
                break;
            }
        }
        defmt::info!("USB link disconnected");
    }
}

/// Answer one request: (reply kind, argument, payload).
async fn handle(kind: u8, arg: u8, payload: &[u8]) -> (Reply, u8, Vec<u8>) {
    let refuse = |code: ErrorCode| (Reply::Error, code as u8, Vec::new());
    let pedal = unsafe { PEDAL.as_mut().unwrap() };
    match Request::from_byte(kind) {
        Some(Request::Hello) => {
            let info = Info {
                protocol: link::PROTOCOL_VERSION,
                slot_count: PATCH_SLOT_COUNT as u8,
                factory_count: FACTORY_RIG_COUNT as u8,
                active: pedal.active,
                effects: EFFECT_UIDS.iter().map(|(_, uid)| *uid).collect(),
                firmware: FIRMWARE.into(),
            };
            (Reply::Info, 0, info.encode())
        }
        Some(Request::ReadSlot) => {
            if arg as usize >= PATCH_SLOT_COUNT {
                return refuse(ErrorCode::BadSlot);
            }
            let mut sector = alloc::vec![0u8; SECTOR_SIZE];
            if let Some(flash) = FLASH.lock().await.as_mut() {
                flash.read(patch_slot_addr(arg as usize), &mut sector).await;
            }
            (Reply::Sector, arg, sector)
        }
        Some(Request::WriteSlot) => {
            if arg as usize >= PATCH_SLOT_COUNT {
                return refuse(ErrorCode::BadSlot);
            }
            let Ok(sector) = <&[u8; SECTOR_SIZE]>::try_from(payload) else {
                return refuse(ErrorCode::NotARig);
            };
            let Ok(patch) = sonido_patch::decode(sector) else {
                return refuse(ErrorCode::NotARig);
            };
            if let Err(code) = patch_loader::check_rig(&patch) {
                return refuse(code);
            }
            let written = match FLASH.lock().await.as_mut() {
                Some(flash) => {
                    flash
                        .write_sector(patch_slot_addr(arg as usize), sector)
                        .await
                }
                None => return refuse(ErrorCode::Flash),
            };
            if written.is_err() {
                return refuse(ErrorCode::Flash);
            }
            pedal.bank[arg as usize] = true;
            defmt::info!("rig written to slot {}", arg);
            request(RigRef::Bank(arg));
            (Reply::Ok, 0, Vec::new())
        }
        Some(Request::EraseSlot) => {
            if arg as usize >= PATCH_SLOT_COUNT {
                return refuse(ErrorCode::BadSlot);
            }
            let erased = Box::new([0xFFu8; SECTOR_SIZE]);
            let done = match FLASH.lock().await.as_mut() {
                Some(flash) => {
                    flash
                        .write_sector(patch_slot_addr(arg as usize), &erased)
                        .await
                }
                None => return refuse(ErrorCode::Flash),
            };
            if done.is_err() {
                return refuse(ErrorCode::Flash);
            }
            pedal.bank[arg as usize] = false;
            if pedal.active == RigRef::Bank(arg) {
                request(RigRef::Jam);
            }
            (Reply::Ok, 0, Vec::new())
        }
        Some(Request::Activate) => {
            let rig = RigRef::from_byte(arg);
            if !pedal.has(rig) {
                return refuse(ErrorCode::BadSlot);
            }
            request(rig);
            (Reply::Ok, 0, Vec::new())
        }
        Some(Request::ReadJam) => {
            let patch: Patch = jam_patch(&pedal.jam, pedal.topology);
            let mut sector = alloc::vec![0xFFu8; SECTOR_SIZE];
            if sonido_patch::encode(&patch, &mut sector).is_err() {
                return refuse(ErrorCode::NotARig);
            }
            (Reply::Sector, 0, sector)
        }
        Some(Request::Status) => {
            let morph = unsafe { CB.as_ref() }.map_or(0, |cb| match cb.mode {
                Mode::A => 0,
                Mode::B => 255,
                Mode::Morph => (cb.morph_t * 255.0) as u8,
            });
            let status = Status {
                active: pedal.active,
                bypassed: BYPASSED.load(Ordering::Relaxed),
                morph,
                load_mean: LAST_MEAN.load(Ordering::Relaxed),
                load_peak: LAST_PEAK.load(Ordering::Relaxed),
            };
            (Reply::Status, 0, status.encode().to_vec())
        }
        None => refuse(ErrorCode::Unsupported),
    }
}

// ── Load report ─────────────────────────────────────────────────────────────

#[embassy_executor::task]
async fn load_report_task() {
    loop {
        Timer::after_secs(1).await;
        let sum = LOAD_SUM.swap(0, Ordering::Relaxed);
        let blocks = LOAD_BLOCKS.swap(0, Ordering::Relaxed);
        let peak = LOAD_PEAK.swap(0, Ordering::Relaxed);
        let over = LOAD_OVER.swap(0, Ordering::Relaxed);
        if blocks == 0 {
            continue;
        }
        let pct_x10 = |c: u32| (u64::from(c) * 1000 / u64::from(LOAD_BUDGET)) as u16;
        let (mean, peak) = (pct_x10(sum / blocks), pct_x10(peak));
        LAST_MEAN.store(mean, Ordering::Relaxed);
        LAST_PEAK.store(peak, Ordering::Relaxed);
        // Levels over the last second, in tenths of a dB (dBFS).
        let (in_peak, in_rms, out_peak, out_rms) = match unsafe { CB.as_mut() } {
            Some(cb) if cb.metered > 0 => {
                let n = cb.metered as f32;
                let v = (
                    cb.in_peak,
                    libm::sqrtf(cb.in_energy / n),
                    cb.out_peak,
                    libm::sqrtf(cb.out_energy / n),
                );
                cb.in_peak = 0.0;
                cb.in_energy = 0.0;
                cb.out_peak = 0.0;
                cb.out_energy = 0.0;
                cb.metered = 0;
                v
            }
            _ => (0.0, 0.0, 0.0, 0.0),
        };
        let db = |x: f32| (200.0 * libm::log10f(x.max(1e-6))) as i32;
        defmt::info!(
            "level in peak={}dB rms={}dB  out peak={}dB rms={}dB (tenths)",
            db(in_peak),
            db(in_rms),
            db(out_peak),
            db(out_rms)
        );
        let pedal = unsafe { PEDAL.as_ref().unwrap() };
        let slot = |s: usize| {
            pedal.cursor[s]
                .checked_sub(1)
                .and_then(|i| PEDAL_EFFECT_IDS.get(i).copied())
                .unwrap_or("-")
        };
        let topo = match pedal.topology {
            Topology::Linear => "linear",
            Topology::Parallel => "parallel",
            Topology::Fan => "fan",
        };
        match pedal.active {
            RigRef::Jam => defmt::info!(
                "load mean={}.{}% peak={}.{}% over_budget={}/{} rig=jam slots=[{}, {}, {}] topo={} bypassed={}",
                mean / 10,
                mean % 10,
                peak / 10,
                peak % 10,
                over,
                blocks,
                slot(0),
                slot(1),
                slot(2),
                topo,
                BYPASSED.load(Ordering::Relaxed)
            ),
            rig => defmt::info!(
                "load mean={}.{}% peak={}.{}% over_budget={}/{} rig={} bypassed={}",
                mean / 10,
                mean % 10,
                peak / 10,
                peak % 10,
                over,
                blocks,
                rig.to_byte(),
                BYPASSED.load(Ordering::Relaxed)
            ),
        }
    }
}

// ── Bypass and bootloader ───────────────────────────────────────────────────

/// Count how long both footswitches are held; enter the DFU bootloader at
/// [`BOOTLOADER_HOLD_TICKS`]. Resets when either is released.
fn accumulate_both_held(cb: &mut Cb, fs1: bool, fs2: bool) {
    if fs1 && fs2 {
        cb.both_held = cb.both_held.saturating_add(1);
        cb.both_held_peak = cb.both_held_peak.max(cb.both_held);
        if cb.both_held >= BOOTLOADER_HOLD_TICKS {
            enter_daisy_bootloader();
        }
    } else {
        cb.both_held = 0;
    }
}

/// When both footswitches are released after being held together, toggle
/// bypass (LED 1 off while bypassed).
fn handle_both_release_toggle(cb: &mut Cb, fs1: bool, fs2: bool) {
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
