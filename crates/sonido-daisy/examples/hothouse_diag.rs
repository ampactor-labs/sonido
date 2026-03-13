//! Comprehensive Hothouse DIY pedal hardware diagnostic.
//!
//! Single binary that validates all hardware subsystems simultaneously:
//!
//! | Subsystem        | What it validates                                      |
//! |------------------|--------------------------------------------------------|
//! | Audio passthrough| Codec + DMA path; hear input signal on output          |
//! | Input levels     | RMS, peak, dBFS per 1-second window (idle ≈ −47 dBFS) |
//! | 6 ADC knobs      | All potentiometers via ADC1 (0.0–1.0 normalized)       |
//! | GPIO (footswitches)| FS1 / FS2 momentary switches (active-low, pull-up)   |
//! | GPIO (toggle sw) | 3 × 3-position toggles, 2 pins each                   |
//! | User LEDs        | LED1 mirrors KNOB_1 > 50%; LED2 mirrors FS1 or FS2    |
//!
//! # Architecture
//!
//! ADC and GPIO reads run in [`hothouse_control_task`] (50 Hz), NOT inside the
//! audio DMA callback. Shared state flows through a lock-free [`HothouseBuffer`].
//! This matches libDaisy's design where `seed.adc` runs via DMA independently
//! of audio.
//!
//! # Expected baseline values
//!
//! - `AUDIO in=…dBFS`: ≈ −47 dBFS with nothing plugged in (analog noise floor)
//! - All knobs: 0.00–1.00 as you rotate them
//! - FS1/FS2: OFF at rest, ON while pressed
//! - Toggles: UP / MID / DN depending on switch position
//!
//! # Build & Flash
//!
//! ```bash
//! cd crates/sonido-daisy
//! cargo objcopy --example hothouse_diag --release -- -O binary -R .sram1_bss firmware/hothouse_diag.bin
//! # Press RESET, then flash within the 2.5s grace period:
//! dfu-util -a 0 -s 0x90040000:leave -D firmware/hothouse_diag.bin
//! ```
//!
//! # USB serial output
//!
//! ```bash
//! cat /dev/ttyACM0
//! # or: screen /dev/ttyACM0 115200
//! ```
//!
//! One line every 2 seconds (only when terminal is connected / DTR asserted):
//!
//! ```text
//! AUDIO in=-46.8dBFS rms=0.0045 peak=0.0078 | K1=0.50 K2=0.73 K3=0.00 K4=1.00 K5=0.50 K6=0.25 | FS1=OFF FS2=OFF T1=UP T2=MID T3=DN
//! ```
//!
//! # Hardware pin mapping (Cleveland Music Co. Hothouse / Electro-Smith Daisy Seed)
//!
//! | Function        | Daisy Pin | STM32 | ADC Channel | Notes                    |
//! |-----------------|-----------|-------|-------------|--------------------------|
//! | LED 1 out       | D22       | PA5   | —           | Active-high              |
//! | LED 2 out       | D23       | PA4   | —           | Active-high              |
//! | Footswitch 1    | D25       | PA0   | —           | Pull-up; active-low      |
//! | Footswitch 2    | D26       | PD11  | —           | Pull-up; active-low      |
//! | Toggle 1 Up     | D9        | PB4   | —           | Pull-up; active-low      |
//! | Toggle 1 Down   | D10       | PB5   | —           | Pull-up; active-low      |
//! | Toggle 2 Up     | D7        | PG10  | —           | Pull-up; active-low      |
//! | Toggle 2 Down   | D8        | PG11  | —           | Pull-up; active-low      |
//! | Toggle 3 Up     | D5        | PD2   | —           | Pull-up; active-low      |
//! | Toggle 3 Down   | D6        | PC12  | —           | Pull-up; active-low      |
//! | KNOB_1          | D16       | PA3   | ADC1_INP15  |                          |
//! | KNOB_2          | D17       | PB1   | ADC1_INP5   |                          |
//! | KNOB_3          | D18       | PA7   | ADC1_INP7   |                          |
//! | KNOB_4          | D19       | PA6   | ADC1_INP3   |                          |
//! | KNOB_5          | D20       | PC1   | ADC1_INP11  |                          |
//! | KNOB_6          | D21       | PC4   | ADC1_INP4   |                          |

#![no_std]
#![no_main]

extern crate alloc;

use core::fmt::Write as FmtWrite;
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

use sonido_daisy::controls::HothouseBuffer;
use sonido_daisy::hothouse::hothouse_control_task;
use sonido_daisy::{
    BLOCK_SIZE, BufWriter, ClockProfile, SAMPLE_RATE, heartbeat, led::UserLed, u24_to_f32,
    usb_task,
};

// ── Heap ──────────────────────────────────────────────────────────────────

#[global_allocator]
static HEAP: Heap = Heap::empty();

// ── USB interrupt binding ─────────────────────────────────────────────────

bind_interrupts!(struct Irqs {
    OTG_FS => usb::InterruptHandler<peripherals::USB_OTG_FS>;
});

// ── Shared control buffer ─────────────────────────────────────────────────

static CONTROLS: HothouseBuffer = HothouseBuffer::new();

// ── Shared audio measurement (audio callback → report_task) ───────────────

/// RMS level × 10000 (fixed-point) from last completed 1-second window.
static RMS_FP: AtomicU32 = AtomicU32::new(0);

/// Peak level × 10000 (fixed-point) from last completed 1-second window.
static PEAK_FP: AtomicU32 = AtomicU32::new(0);

/// dBFS × 10 as a signed integer.
static DBFS_X10: AtomicI32 = AtomicI32::new(-960);

// ── Constants ─────────────────────────────────────────────────────────────

/// Blocks per 1-second measurement window: 48 000 / 32 = 1500.
const BLOCKS_PER_WINDOW: u32 = (SAMPLE_RATE as u32) / (BLOCK_SIZE as u32);

/// Minimum RMS for valid dBFS calculation; below this, report −96.0.
const RMS_FLOOR: f32 = 1e-10;

/// Reciprocal of total samples in a 1-second window (precomputed for the callback).
const INV_WINDOW_SAMPLES: f32 = 1.0 / ((BLOCK_SIZE as u32 * BLOCKS_PER_WINDOW) as f32);

// ── USB static buffers (StaticCell — no unsafe required) ─────────────────

static EP_OUT_BUF: StaticCell<[u8; 256]> = StaticCell::new();
static CONFIG_DESC: StaticCell<[u8; 256]> = StaticCell::new();
static BOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
static MSOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();
static CDC_STATE: StaticCell<State<'static>> = StaticCell::new();

// ── report_task ───────────────────────────────────────────────────────────

/// Reads measurement atomics + control buffer every 2 seconds and writes one line to USB serial.
#[embassy_executor::task]
async fn report_task(mut class: CdcAcmClass<'static, Driver<'static, peripherals::USB_OTG_FS>>) {
    let mut buf = [0u8; 256];

    loop {
        // Block until host opens the serial port
        class.wait_connection().await;
        defmt::info!("USB serial connected");

        // Inner loop: write one report every 2 seconds
        loop {
            embassy_time::Timer::after_millis(2000).await;

            // ── Read audio measurements ──
            let rms_fp = RMS_FP.load(Ordering::Relaxed);
            let peak_fp = PEAK_FP.load(Ordering::Relaxed);
            let dbfs_x10 = DBFS_X10.load(Ordering::Relaxed);

            // ── Read control state from ControlBuffer ──
            let knobs: [u32; 6] = core::array::from_fn(|i| {
                (CONTROLS.read_knob(i) * 100.0) as u32
            });
            let fs1 = CONTROLS.read_footswitch(0);
            let fs2 = CONTROLS.read_footswitch(1);
            let t1 = CONTROLS.read_toggle(0);
            let t2 = CONTROLS.read_toggle(1);
            let t3 = CONTROLS.read_toggle(2);

            // ── Format ──
            let mut w = BufWriter::new(&mut buf);

            // AUDIO section
            let dbfs_sign = if dbfs_x10 < 0 { "-" } else { "" };
            let dbfs_abs = if dbfs_x10 < 0 { -dbfs_x10 } else { dbfs_x10 } as u32;
            let _ = write!(
                w,
                "AUDIO in={}{}.{}dBFS rms={}.{:04} peak={}.{:04}",
                dbfs_sign,
                dbfs_abs / 10,
                dbfs_abs % 10,
                rms_fp / 10000,
                rms_fp % 10000,
                peak_fp / 10000,
                peak_fp % 10000,
            );

            // Knobs section
            let _ = write!(w, " | ");
            for (i, &k) in knobs.iter().enumerate() {
                if k >= 100 {
                    let _ = write!(w, "K{}=1.00", i + 1);
                } else {
                    let _ = write!(w, "K{}=0.{:02}", i + 1, k);
                }
                if i < 5 {
                    let _ = write!(w, " ");
                }
            }

            // GPIO section — toggle encoding: 0=UP, 1=MID, 2=DN
            let fs1_str = if fs1 { "ON" } else { "OFF" };
            let fs2_str = if fs2 { "ON" } else { "OFF" };
            let toggle_str = |pos: u8| -> &str {
                match pos {
                    0 => "UP",
                    2 => "DN",
                    _ => "MID",
                }
            };
            let _ = write!(
                w,
                " | FS1={} FS2={} T1={} T2={} T3={}\r\n",
                fs1_str,
                fs2_str,
                toggle_str(t1),
                toggle_str(t2),
                toggle_str(t3),
            );

            let len = w.pos;

            // Send in 64-byte chunks; break on disconnect
            let mut ok = true;
            for chunk in buf[..len].chunks(64) {
                if class.write_packet(chunk).await.is_err() {
                    ok = false;
                    break;
                }
            }
            if !ok {
                defmt::warn!("USB serial disconnected");
                break; // → back to wait_connection()
            }
        }
    }
}

// ── Main ──────────────────────────────────────────────────────────────────

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // D2 SRAM clocks are disabled at reset — enable before heap init.
    sonido_daisy::enable_d2_sram();

    // Heap at D2 SRAM (256 KB)
    unsafe {
        HEAP.init(0x3000_8000, 256 * 1024);
    }

    let config = sonido_daisy::rcc_config(ClockProfile::Performance);
    let p = hal::init(config);

    // Heartbeat LED (PC7 = Daisy Seed user LED)
    let led = UserLed::new(p.PC7);
    spawner.spawn(heartbeat(led)).unwrap();

    defmt::info!("hothouse_diag: initializing...");

    // ── Extract control pins and spawn control task ──
    let ctrl = sonido_daisy::hothouse_pins!(p);
    spawner
        .spawn(hothouse_control_task(ctrl, &CONTROLS))
        .unwrap();

    // ── USB CDC ACM ──
    let driver = Driver::new_fs(
        p.USB_OTG_FS,
        Irqs,
        p.PA12,
        p.PA11,
        EP_OUT_BUF.init([0u8; 256]),
        hal::usb::Config::default(),
    );

    let mut usb_config = embassy_usb::Config::new(0x1209, 0x0001);
    usb_config.manufacturer = Some("Sonido");
    usb_config.product = Some("Hothouse Diagnostics");
    usb_config.serial_number = Some("010");

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
    spawner.spawn(report_task(class)).unwrap();

    defmt::info!("hothouse_diag: USB initialized, starting audio");

    // ── Audio interface (passthrough only — no ADC in callback) ──
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

    defmt::info!("hothouse_diag: audio interface started — passthrough active");

    // Audio callback: passthrough + level metering only.
    // LED feedback is driven via ControlBuffer (control task reads + drives GPIO).
    let mut sum_sq: f32 = 0.0;
    let mut peak: f32 = 0.0;
    let mut block_count: u32 = 0;

    defmt::unwrap!(
        interface
            .start_callback(move |input, output| {
                // ── Audio passthrough ──
                output.copy_from_slice(input);

                // ── Accumulate RMS + peak (mono average) ──
                for i in (0..input.len()).step_by(2) {
                    let left = u24_to_f32(input[i]);
                    let right = u24_to_f32(input[i + 1]);
                    let mono = (left + right) * 0.5;
                    sum_sq += mono * mono;
                    let abs_val = libm::fabsf(mono);
                    if abs_val > peak {
                        peak = abs_val;
                    }
                }
                block_count += 1;

                // ── Publish 1-second audio measurement ──
                if block_count >= BLOCKS_PER_WINDOW {
                    let rms = libm::sqrtf(sum_sq * INV_WINDOW_SAMPLES);
                    let dbfs = if rms > RMS_FLOOR {
                        20.0 * libm::log10f(rms)
                    } else {
                        -96.0
                    };

                    RMS_FP.store((rms * 10000.0) as u32, Ordering::Relaxed);
                    PEAK_FP.store((peak * 10000.0) as u32, Ordering::Relaxed);
                    DBFS_X10.store((dbfs * 10.0) as i32, Ordering::Relaxed);

                    sum_sq = 0.0;
                    peak = 0.0;
                    block_count = 0;
                }

                // ── LED feedback via ControlBuffer ──
                // LED1: mirrors KNOB_1 > 50%
                let k1 = CONTROLS.read_knob(0);
                CONTROLS.write_led(0, if k1 > 0.5 { 1.0 } else { 0.0 });

                // LED2: mirrors FS1 or FS2 pressed
                let fs1 = CONTROLS.read_footswitch(0);
                let fs2 = CONTROLS.read_footswitch(1);
                CONTROLS.write_led(
                    1,
                    if fs1 || fs2 { 1.0 } else { 0.0 },
                );
            })
            .await
    );
}
