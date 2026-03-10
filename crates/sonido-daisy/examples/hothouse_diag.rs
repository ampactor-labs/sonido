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
//! | CPU temperature  | STM32H750 internal sensor via ADC3 (idle ≈ 40–60 °C)  |
//! | User LEDs        | LED1 mirrors KNOB_1 > 50%; LED2 mirrors FS1 or FS2    |
//!
//! # Expected baseline values
//!
//! - `AUDIO in=…dBFS`: ≈ −47 dBFS with nothing plugged in (analog noise floor)
//! - `CPU …C`: 40–60 °C at idle, 50–75 °C under DSP load
//! - All knobs: 0.00–1.00 as you rotate them
//! - FS1/FS2: OFF at rest, ON while pressed
//! - Toggles: UP / MID / DN depending on switch position
//!
//! # Build & Flash
//!
//! ```bash
//! cd crates/sonido-daisy
//! cargo objcopy --example hothouse_diag --release -- -O binary -R .sram1_bss hothouse_diag.bin
//! # Press RESET, then flash within the 2.5s grace period:
//! dfu-util -a 0 -s 0x90040000:leave -D hothouse_diag.bin
//! ```
//!
//! # USB serial output
//!
//! ```bash
//! cat /dev/ttyACM0
//! # or: screen /dev/ttyACM0 115200
//! ```
//!
//! One line per second (only when terminal is connected / DTR asserted):
//!
//! ```text
//! AUDIO in=-46.8dBFS rms=0.0045 peak=0.0078 | K1=0.50 K2=0.73 K3=0.00 K4=1.00 K5=0.50 K6=0.25 | FS1=OFF FS2=OFF T1=UP T2=MID T3=DN | CPU 52C
//! ```
//!
//! # Hardware pin mapping
//!
//! | Function        | Pin   | Notes                                    |
//! |-----------------|-------|------------------------------------------|
//! | LED 1 out       | PA5   | Active-high; mirrors KNOB_1 > 50%        |
//! | LED 2 out       | PA4   | Active-high; mirrors FS1 or FS2 pressed  |
//! | Footswitch 1    | PA0   | Input, pull-up; `.is_low()` = pressed    |
//! | Footswitch 2    | PD11  | Input, pull-up                           |
//! | Toggle 1 Up     | PB4   | Input, pull-up; active-low               |
//! | Toggle 1 Down   | PB5   | Input, pull-up; active-low               |
//! | Toggle 2 Up     | PG10  | Input, pull-up; active-low               |
//! | Toggle 2 Down   | PG11  | Input, pull-up; active-low               |
//! | Toggle 3 Up     | PD2   | Input, pull-up; active-low               |
//! | Toggle 3 Down   | PC12  | Input, pull-up; active-low               |
//! | KNOB_1          | PA3   | ADC1_INP15                               |
//! | KNOB_2          | PB1   | ADC1_INP5                                |
//! | KNOB_3          | PA7   | ADC1_INP7                                |
//! | KNOB_4          | PA6   | ADC1_INP3                                |
//! | KNOB_5          | PC1   | ADC1_INP11                               |
//! | KNOB_6          | PC4   | ADC1_INP4                                |

#![no_std]
#![no_main]

extern crate alloc;

use core::fmt::Write as FmtWrite;
use core::sync::atomic::{AtomicI32, AtomicU32, Ordering};

use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_stm32 as hal;
use embassy_stm32::adc::{Adc, SampleTime};
use embassy_stm32::gpio::{Input, Level, Output, Pull, Speed};
use embassy_stm32::usb::Driver;
use embassy_stm32::{bind_interrupts, peripherals, usb};
use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
use embedded_alloc::LlffHeap as Heap;
use panic_probe as _;
use static_cell::StaticCell;

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

// ── Shared atomic state (audio callback → report_task) ───────────────────

/// RMS level × 10000 (fixed-point) from last completed 1-second window.
static RMS_FP: AtomicU32 = AtomicU32::new(0);

/// Peak level × 10000 (fixed-point) from last completed 1-second window.
static PEAK_FP: AtomicU32 = AtomicU32::new(0);

/// dBFS × 10 as a signed integer.
///
/// -96.0 dBFS → -960. 0.0 dBFS → 0.
static DBFS_X10: AtomicI32 = AtomicI32::new(-960);

/// Normalized knob readings × 100 (0–100 maps to 0.00–1.00).
static KNOBS: [AtomicU32; 6] = [
    const { AtomicU32::new(0) },
    const { AtomicU32::new(0) },
    const { AtomicU32::new(0) },
    const { AtomicU32::new(0) },
    const { AtomicU32::new(0) },
    const { AtomicU32::new(0) },
];

/// Packed GPIO state.
///
/// Bit layout:
/// - bit 0: FS1 pressed (1=pressed)
/// - bit 1: FS2 pressed (1=pressed)
/// - bits 2–3: Toggle 1 (0=MID, 1=UP, 2=DN)
/// - bits 4–5: Toggle 2 (0=MID, 1=UP, 2=DN)
/// - bits 6–7: Toggle 3 (0=MID, 1=UP, 2=DN)
static GPIO_BITS: AtomicU32 = AtomicU32::new(0);

/// CPU temperature in degrees Celsius.
static TEMP_C: AtomicI32 = AtomicI32::new(0);

// ── Constants ─────────────────────────────────────────────────────────────

/// Blocks per 1-second measurement window: 48 000 / 32 = 1500.
const BLOCKS_PER_WINDOW: u32 = (SAMPLE_RATE as u32) / (BLOCK_SIZE as u32);

/// ADC control poll rate: every 150 blocks ≈ 100 ms.
const POLL_EVERY: u32 = 150;

/// Minimum RMS for valid dBFS calculation; below this, report −96.0.
const RMS_FLOOR: f32 = 1e-10;

/// Reciprocal of total samples in a 1-second window (precomputed for the callback).
const INV_WINDOW_SAMPLES: f32 = 1.0 / ((BLOCK_SIZE as u32 * BLOCKS_PER_WINDOW) as f32);

/// ADC sample time for knob readings (32.5 cycles, ≈ adequate for pots).
const KNOB_SAMPLE_TIME: SampleTime = SampleTime::CYCLES32_5;

/// ADC sample time for internal temperature sensor.
///
/// STM32H750 datasheet requires ≥ 9 µs. CYCLES810_5 at 100 MHz = 8.1 µs —
/// slightly short but adequate for diagnostic purposes (within ~2 °C).
const TEMP_SAMPLE_TIME: SampleTime = SampleTime::CYCLES810_5;

// ── Toggle decode ─────────────────────────────────────────────────────────

/// Decodes a 3-position toggle from its two GPIO pins (both pull-up, active-low).
///
/// Returns: 1=UP, 0=MID, 2=DN (matches GPIO_BITS encoding).
fn decode_toggle(up: &Input<'_>, dn: &Input<'_>) -> u32 {
    match (up.is_low(), dn.is_low()) {
        (true, false) => 1,  // UP
        (false, true) => 2,  // DN
        _ => 0,              // MID (or fault → treat as MID)
    }
}

// ── USB static buffers (StaticCell — no unsafe required) ─────────────────

static EP_OUT_BUF:  StaticCell<[u8; 256]>      = StaticCell::new();
static CONFIG_DESC: StaticCell<[u8; 256]>      = StaticCell::new();
static BOS_DESC:    StaticCell<[u8; 256]>      = StaticCell::new();
static MSOS_DESC:   StaticCell<[u8; 256]>      = StaticCell::new();
static CONTROL_BUF: StaticCell<[u8; 64]>       = StaticCell::new();
static CDC_STATE:   StaticCell<State<'static>> = StaticCell::new();

// ── report_task ───────────────────────────────────────────────────────────

/// Reads measurement atomics every second and writes one line to USB serial.
///
/// Output format (matches the spec):
/// ```text
/// AUDIO in=-46.8dBFS rms=0.0045 peak=0.0078 | K1=0.50 K2=0.73 K3=0.00 K4=1.00 K5=0.50 K6=0.25 | FS1=OFF FS2=OFF T1=UP T2=MID T3=DN | CPU 52C
/// ```
///
/// Blocks on `wait_connection()` until a host opens the serial port, then
/// reports once every 2 seconds. On write error, breaks back to
/// `wait_connection()` for clean USB reconnection.
#[embassy_executor::task]
async fn report_task(
    mut class: CdcAcmClass<'static, Driver<'static, peripherals::USB_OTG_FS>>,
) {
    let mut buf = [0u8; 256];

    loop {
        // Block until host opens the serial port
        class.wait_connection().await;
        defmt::info!("USB serial connected");

        // Inner loop: write one report every 2 seconds
        loop {
            embassy_time::Timer::after_millis(2000).await;

            // ── Read all atomics ──
            let rms_fp   = RMS_FP.load(Ordering::Relaxed);
            let peak_fp  = PEAK_FP.load(Ordering::Relaxed);
            let dbfs_x10 = DBFS_X10.load(Ordering::Relaxed);
            let knobs: [u32; 6] = core::array::from_fn(|i| KNOBS[i].load(Ordering::Relaxed));
            let gpio     = GPIO_BITS.load(Ordering::Relaxed);
            let temp     = TEMP_C.load(Ordering::Relaxed);

            // ── Format ──
            let mut w = BufWriter::new(&mut buf);

            // AUDIO section
            let dbfs_sign = if dbfs_x10 < 0 { "-" } else { "" };
            let dbfs_abs  = if dbfs_x10 < 0 { -dbfs_x10 } else { dbfs_x10 } as u32;
            let _ = write!(w,
                "AUDIO in={}{}.{}dBFS rms={}.{:04} peak={}.{:04}",
                dbfs_sign, dbfs_abs / 10, dbfs_abs % 10,
                rms_fp / 10000, rms_fp % 10000,
                peak_fp / 10000, peak_fp % 10000,
            );

            // Knobs section
            let _ = write!(w, " | ");
            for (i, &k) in knobs.iter().enumerate() {
                if k >= 100 {
                    let _ = write!(w, "K{}=1.00", i + 1);
                } else {
                    let _ = write!(w, "K{}=0.{:02}", i + 1, k);
                }
                if i < 5 { let _ = write!(w, " "); }
            }

            // GPIO section
            let fs1 = if gpio & 1 != 0 { "ON" } else { "OFF" };
            let fs2 = if gpio & 2 != 0 { "ON" } else { "OFF" };
            let t1 = match (gpio >> 2) & 3 { 1 => "UP", 2 => "DN", _ => "MID" };
            let t2 = match (gpio >> 4) & 3 { 1 => "UP", 2 => "DN", _ => "MID" };
            let t3 = match (gpio >> 6) & 3 { 1 => "UP", 2 => "DN", _ => "MID" };
            let _ = write!(w, " | FS1={} FS2={} T1={} T2={} T3={}", fs1, fs2, t1, t2, t3);

            // CPU temp
            let _ = write!(w, " | CPU {}C\r\n", temp);

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

    // ── GPIO pins ──
    let mut led1 = Output::new(p.PA5, Level::Low, Speed::Low);   // LED 1 (K1 > 50%)
    let mut led2 = Output::new(p.PA4, Level::Low, Speed::Low);   // LED 2 (FS1 or FS2)
    let foot1  = Input::new(p.PA0,  Pull::Up);  // Footswitch 1
    let foot2  = Input::new(p.PD11, Pull::Up);  // Footswitch 2
    let tog1_up  = Input::new(p.PB4,  Pull::Up);
    let tog1_dn  = Input::new(p.PB5,  Pull::Up);
    let tog2_up  = Input::new(p.PG10, Pull::Up);
    let tog2_dn  = Input::new(p.PG11, Pull::Up);
    let tog3_up  = Input::new(p.PD2,  Pull::Up);
    let tog3_dn  = Input::new(p.PC12, Pull::Up);

    // ── ADC1 for knobs ──
    let mut adc1 = Adc::new(p.ADC1);
    let mut knob1_pin = p.PA3;
    let mut knob2_pin = p.PB1;
    let mut knob3_pin = p.PA7;
    let mut knob4_pin = p.PA6;
    let mut knob5_pin = p.PC1;
    let mut knob6_pin = p.PC4;

    // ── ADC3 for CPU temperature ──
    let mut adc3 = Adc::new(p.ADC3);
    // FIX: enable_temperature() sets VSENSEEN=1 in ADC3_COMMON CCR,
    // physically connecting the temp sensor. Without this, the ADC reads
    // garbage voltage that maps to ~103°C.
    let mut temp_ch = sonido_daisy::adc::enable_temperature(&mut adc3);

    // Cache factory calibration values (read-only flash, never changes).
    let (ts_cal1, ts_cal2) = sonido_daisy::adc::read_calibration();

    // ── Audio peripherals (direct construction — not board macro) ──
    let audio_peripherals = sonido_daisy::audio::AudioPeripherals {
        codec_pins: sonido_daisy::codec_pins!(p),
        sai1: p.SAI1,
        dma1_ch0: p.DMA1_CH0,
        dma1_ch1: p.DMA1_CH1,
    };

    // ── USB CDC ACM (StaticCell — no unsafe) ──
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

    defmt::info!("hothouse_diag: USB initialized, starting audio + control loop");

    // ── Audio interface ──
    let interface = audio_peripherals
        .prepare_interface(Default::default())
        .await;
    let mut interface = defmt::unwrap!(interface.start_interface().await);

    defmt::info!("hothouse_diag: audio interface started — passthrough active");

    // ── Audio + control callback ──
    // Measurement accumulators (captured by the closure)
    let mut sum_sq: f32 = 0.0;
    let mut peak: f32 = 0.0;
    let mut block_count: u32 = 0;
    let mut poll_count: u32 = 0;

    defmt::unwrap!(
        interface
            .start_callback(move |input, output| {
                // ── Audio passthrough ──
                output.copy_from_slice(input);

                // ── Accumulate RMS + peak (mono average) ──
                for i in (0..input.len()).step_by(2) {
                    let left  = u24_to_f32(input[i]);
                    let right = u24_to_f32(input[i + 1]);
                    let mono  = (left + right) * 0.5;
                    sum_sq += mono * mono;
                    let abs_val = libm::fabsf(mono);
                    if abs_val > peak { peak = abs_val; }
                }
                block_count += 1;
                poll_count  += 1;

                // ── Control poll (every 150 blocks ≈ 100 ms) ──
                if poll_count >= POLL_EVERY {
                    poll_count = 0;

                    // Read 6 knobs
                    let k: [u16; 6] = [
                        adc1.blocking_read(&mut knob1_pin, KNOB_SAMPLE_TIME),
                        adc1.blocking_read(&mut knob2_pin, KNOB_SAMPLE_TIME),
                        adc1.blocking_read(&mut knob3_pin, KNOB_SAMPLE_TIME),
                        adc1.blocking_read(&mut knob4_pin, KNOB_SAMPLE_TIME),
                        adc1.blocking_read(&mut knob5_pin, KNOB_SAMPLE_TIME),
                        adc1.blocking_read(&mut knob6_pin, KNOB_SAMPLE_TIME),
                    ];
                    let k1_pct = (k[0] as u32 * 100) / 65535;
                    for (i, &raw) in k.iter().enumerate() {
                        KNOBS[i].store((raw as u32 * 100) / 65535, Ordering::Relaxed);
                    }

                    // Read GPIO
                    let fs1 = foot1.is_low() as u32;
                    let fs2 = foot2.is_low() as u32;
                    let t1  = decode_toggle(&tog1_up, &tog1_dn);
                    let t2  = decode_toggle(&tog2_up, &tog2_dn);
                    let t3  = decode_toggle(&tog3_up, &tog3_dn);
                    GPIO_BITS.store(fs1 | (fs2 << 1) | (t1 << 2) | (t2 << 4) | (t3 << 6), Ordering::Relaxed);

                    // LED feedback (use local k1_pct, not redundant atomic load)
                    if k1_pct > 50 { led1.set_high(); } else { led1.set_low(); }
                    if fs1 != 0 || fs2 != 0 { led2.set_high(); } else { led2.set_low(); }

                    // CPU temperature (RM0433: T = 80×(raw−CAL1)/(CAL2−CAL1) + 30)
                    let raw_temp = adc3.blocking_read(&mut temp_ch, TEMP_SAMPLE_TIME);
                    TEMP_C.store(
                        sonido_daisy::adc::raw_to_celsius(raw_temp, ts_cal1, ts_cal2),
                        Ordering::Relaxed,
                    );
                }

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
            })
            .await
    );
}
