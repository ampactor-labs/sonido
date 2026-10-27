//! grid_pedal: the HUM ear in a guitar pedal.
//!
//! The Hothouse's input is the sensor: a guitar's pickup, or a bare cable,
//! carries the room's mains hum, and `sonido_core::grid::GridEar` reads the
//! grid's frequency from it on the Daisy Seed, the same estimator the desktop
//! ear runs. Every 50 ms the pedal writes one line over USB serial in the C5
//! contract of `demos/grid-rush-hour/CONTRACTS.md`, which the director reads
//! in place of the desktop ear:
//!
//!   {"hz_fast":59.9810,"hz":59.9830,"rocof":-0.004,"level_db":-18.5,"harmonic":1,"source":"pedal"}
//!
//! Audio passes through, so the pedal is still a cable. The two LEDs are the
//! page's heartbeat in light: LED 1 breathes at half a hertz while the grid
//! sits inside its band and faster as the frequency falls, up to four hertz
//! at 200 mHz; LED 2 holds while the rate of fall is past the event trigger,
//! and both hold solid when the hum is gone.
//!
//! Build and flash (Seed in DFU mode: hold BOOT, tap RESET):
//!
//!   cargo objcopy --example grid_pedal --release -- -O binary -R .sram1_bss firmware/grid_pedal.bin
//!   dfu-util -a 0 -s 0x90040000:leave -D firmware/grid_pedal.bin
//!
//! Then `./run.sh pedal /dev/ttyACM0` from the demo directory, or
//! `cat /dev/ttyACM0` to watch the lines.

#![no_std]
#![no_main]

use core::fmt::Write;
use core::sync::atomic::{AtomicI32, AtomicU32, Ordering};

use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_stm32 as hal;
use embassy_stm32::usb::Driver;
use embassy_stm32::{bind_interrupts, peripherals, usb};
use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
use embedded_alloc::LlffHeap as Heap;
use panic_probe as _;
use static_cell::StaticCell;

use sonido_core::grid::GridEar;
use sonido_daisy::controls::HothouseBuffer;
use sonido_daisy::hothouse::hothouse_control_task;
use sonido_daisy::{BLOCK_SIZE, BufWriter, ClockProfile, SAMPLE_RATE, heartbeat, led::UserLed, u24_to_f32, usb_task};

#[global_allocator]
static HEAP: Heap = Heap::empty();

bind_interrupts!(struct Irqs {
    OTG_FS => usb::InterruptHandler<peripherals::USB_OTG_FS>;
});

static CONTROLS: HothouseBuffer = HothouseBuffer::new();

// ── The reading, published by the audio callback and read by the serial task ──
// Fixed point so the serial line formats with integers only: no float
// formatting is linked into the image.

/// Frequency over the last 100 ms, ×10000. Valid when VALID is 1.
static HZ_FAST_X1E4: AtomicI32 = AtomicI32::new(0);
/// Frequency over the last second, ×10000.
static HZ_X1E4: AtomicI32 = AtomicI32::new(0);
/// Rate of change over 300 ms, ×1000 (mHz per second).
static ROCOF_X1E3: AtomicI32 = AtomicI32::new(0);
/// The tracked band's level in dBFS, ×10.
static LEVEL_X10: AtomicI32 = AtomicI32::new(-1200);
/// The harmonic tracked: 1, 2 or 3.
static HARMONIC: AtomicU32 = AtomicU32::new(1);
/// 1 when the ear has a reading, 0 while it listens or the hum is gone.
static VALID: AtomicU32 = AtomicU32::new(0);
/// Counts ticks, so the serial task writes one line per tick and never repeats one.
static SEQ: AtomicU32 = AtomicU32::new(0);

/// Blocks per tick: 50 ms at 48 kHz in 32-sample blocks is 75 blocks exactly.
const BLOCKS_PER_TICK: u32 = (SAMPLE_RATE as u32 / 20) / BLOCK_SIZE as u32;
const NOMINAL_HZ: f32 = 60.0;
const DEADBAND: f32 = 0.036;
const FULL_AT: f32 = 0.200;
const ROCOF_ENTER: f32 = 0.10;

static EP_OUT_BUF: StaticCell<[u8; 256]> = StaticCell::new();
static CONFIG_DESC: StaticCell<[u8; 256]> = StaticCell::new();
static BOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
static MSOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();
static CDC_STATE: StaticCell<State<'static>> = StaticCell::new();

/// Writes a fixed-point integer as a decimal with `decimals` places.
fn write_fixed(w: &mut BufWriter<'_>, value: i32, decimals: u32) {
    let scale = 10i32.pow(decimals);
    let sign = if value < 0 { "-" } else { "" };
    let abs = value.unsigned_abs();
    let whole = abs / scale as u32;
    let frac = abs % scale as u32;
    match decimals {
        4 => { let _ = write!(w, "{sign}{whole}.{frac:04}"); }
        3 => { let _ = write!(w, "{sign}{whole}.{frac:03}"); }
        1 => { let _ = write!(w, "{sign}{whole}.{frac:01}"); }
        _ => { let _ = write!(w, "{sign}{whole}"); }
    }
}

/// One C5 line per tick over USB serial.
#[embassy_executor::task]
async fn line_task(mut class: CdcAcmClass<'static, Driver<'static, peripherals::USB_OTG_FS>>) {
    let mut buf = [0u8; 192];
    let mut last_seq = 0u32;
    loop {
        class.wait_connection().await;
        defmt::info!("grid_pedal: serial connected");
        loop {
            embassy_time::Timer::after_millis(10).await;
            let seq = SEQ.load(Ordering::Relaxed);
            if seq == last_seq {
                continue;
            }
            last_seq = seq;
            let valid = VALID.load(Ordering::Relaxed) == 1;
            let mut w = BufWriter::new(&mut buf);
            let _ = write!(w, "{{\"hz_fast\":");
            if valid { write_fixed(&mut w, HZ_FAST_X1E4.load(Ordering::Relaxed), 4); } else { let _ = write!(w, "null"); }
            let _ = write!(w, ",\"hz\":");
            if valid { write_fixed(&mut w, HZ_X1E4.load(Ordering::Relaxed), 4); } else { let _ = write!(w, "null"); }
            let _ = write!(w, ",\"rocof\":");
            if valid { write_fixed(&mut w, ROCOF_X1E3.load(Ordering::Relaxed), 3); } else { let _ = write!(w, "null"); }
            let _ = write!(w, ",\"level_db\":");
            write_fixed(&mut w, LEVEL_X10.load(Ordering::Relaxed), 1);
            let _ = write!(w, ",\"harmonic\":{},\"source\":\"pedal\"}}\r\n", HARMONIC.load(Ordering::Relaxed));
            let len = w.pos;
            let mut ok = true;
            for chunk in buf[..len].chunks(64) {
                if class.write_packet(chunk).await.is_err() {
                    ok = false;
                    break;
                }
            }
            if !ok {
                defmt::warn!("grid_pedal: serial disconnected");
                break;
            }
        }
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    sonido_daisy::enable_d2_sram();
    unsafe {
        HEAP.init(0x3000_8000, 256 * 1024);
    }
    let config = sonido_daisy::rcc_config(ClockProfile::Performance);
    let p = hal::init(config);

    let led = UserLed::new(p.PC7);
    spawner.spawn(heartbeat(led)).unwrap();

    let ctrl = sonido_daisy::hothouse_pins!(p);
    spawner.spawn(hothouse_control_task(ctrl, &CONTROLS)).unwrap();

    let driver = Driver::new_fs(p.USB_OTG_FS, Irqs, p.PA12, p.PA11, EP_OUT_BUF.init([0u8; 256]), hal::usb::Config::default());
    let mut usb_config = embassy_usb::Config::new(0x1209, 0x0001);
    usb_config.manufacturer = Some("Sonido");
    usb_config.product = Some("HUM grid pedal");
    usb_config.serial_number = Some("060");
    let cdc_state = CDC_STATE.init(State::new());
    let mut builder = embassy_usb::Builder::new(
        driver,
        usb_config,
        CONFIG_DESC.init([0; 256]),
        BOS_DESC.init([0; 256]),
        MSOS_DESC.init([0; 256]),
        CONTROL_BUF.init([0; 64]),
    );
    let class = CdcAcmClass::new(&mut builder, cdc_state, 64);
    let usb = builder.build();
    spawner.spawn(usb_task(usb)).unwrap();
    spawner.spawn(line_task(class)).unwrap();

    let audio_peripherals = sonido_daisy::audio::AudioPeripherals {
        codec_pins: sonido_daisy::codec_pins!(p),
        sai1: p.SAI1,
        dma1_ch0: p.DMA1_CH0,
        dma1_ch1: p.DMA1_CH1,
    };
    let interface = audio_peripherals.prepare_interface(Default::default()).await;
    let mut interface = defmt::unwrap!(interface.start_interface().await);
    defmt::info!("grid_pedal: listening");

    let mut ear = GridEar::new(NOMINAL_HZ, SAMPLE_RATE);
    let mut blocks: u32 = 0;
    let mut phase: f32 = 0.0;
    let mut rate_hz: f32 = 0.5;
    let mut event = false;
    let mut lost = true;
    let block_seconds = BLOCK_SIZE as f32 / SAMPLE_RATE;

    defmt::unwrap!(
        interface
            .start_callback(move |input, output| {
                output.copy_from_slice(input);
                // The left channel is the ear's input: the pickup, or the cable.
                for i in (0..input.len()).step_by(2) {
                    ear.process(u24_to_f32(input[i]));
                }
                blocks += 1;
                if blocks >= BLOCKS_PER_TICK {
                    blocks = 0;
                    match ear.tick() {
                        Some(r) => {
                            HZ_FAST_X1E4.store((r.hz_fast * 10000.0) as i32, Ordering::Relaxed);
                            HZ_X1E4.store((r.hz * 10000.0) as i32, Ordering::Relaxed);
                            ROCOF_X1E3.store((r.rocof * 1000.0) as i32, Ordering::Relaxed);
                            LEVEL_X10.store((r.level_db * 10.0) as i32, Ordering::Relaxed);
                            HARMONIC.store(r.harmonic as u32, Ordering::Relaxed);
                            VALID.store(1, Ordering::Relaxed);
                            let dev = libm::fabsf(r.hz_fast - NOMINAL_HZ);
                            let past = if dev > DEADBAND { (dev - DEADBAND) / FULL_AT } else { 0.0 };
                            rate_hz = 0.5 + 3.5 * if past > 1.0 { 1.0 } else { past };
                            event = libm::fabsf(r.rocof) > ROCOF_ENTER;
                            lost = false;
                        }
                        None => {
                            VALID.store(0, Ordering::Relaxed);
                            LEVEL_X10.store(-1200, Ordering::Relaxed);
                            lost = true;
                        }
                    }
                    SEQ.fetch_add(1, Ordering::Relaxed);
                }
                // The LEDs: the heartbeat in light.
                phase += rate_hz * block_seconds;
                if phase >= 1.0 {
                    phase -= 1.0;
                }
                let breathe = if lost { 1.0 } else if phase < 0.5 { 1.0 } else { 0.0 };
                CONTROLS.write_led(0, breathe);
                CONTROLS.write_led(1, if lost || event { 1.0 } else { 0.0 });
            })
            .await
    );
}
