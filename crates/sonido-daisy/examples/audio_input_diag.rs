//! Diagnostic: audio input noise floor measurement via USB serial.
//!
//! Reads the codec's audio ADC input, computes RMS and peak levels over 1-second
//! windows, and reports via USB serial (CDC ACM). Outputs silence to avoid
//! confusing the measurement with output-side noise.
//!
//! - High RMS with nothing plugged in → noise injected before codec ADC (input op-amp issue)
//! - Low RMS with nothing plugged in → input side is clean, problem is output-only
//! - RMS changes when pickup is plugged in → codec ADC is reading real signal
//!
//! User LED (PC7) blinks at 1 Hz (500ms on / 500ms off) — same as blinky.
//!
//! # Output format (USB serial)
//!
//! ```text
//! IN: rms=0.0023 peak=0.0089 dBFS=-52.8
//! ```
//!
//! Reports update every second. Connect with any serial terminal:
//!
//! ```bash
//! cat /dev/ttyACM0
//! # or: screen /dev/ttyACM0 115200
//! ```
//!
//! # Build & Flash
//!
//! ```bash
//! cd crates/sonido-daisy
//! cargo objcopy --example audio_input_diag --release -- -O binary -R .sram1_bss audio_input_diag.bin
//! # Press RESET, then flash within the 2.5s grace period:
//! dfu-util -a 0 -s 0x90040000:leave -D audio_input_diag.bin
//! # (Hold BOOT while pressing RESET to extend the grace period indefinitely)
//! ```

#![no_std]
#![no_main]

use core::fmt::Write as FmtWrite;
use core::sync::atomic::{AtomicU32, Ordering};

use daisy_embassy::new_daisy_board;
use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_stm32 as hal;
use embassy_stm32::usb::Driver;
use embassy_stm32::{bind_interrupts, peripherals, usb};
use embassy_time::Timer;
use embassy_usb::UsbDevice;
use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
use panic_probe as _;

use sonido_daisy::{BLOCK_SIZE, u24_to_f32};

bind_interrupts!(struct Irqs {
    OTG_FS => usb::InterruptHandler<peripherals::USB_OTG_FS>;
});

/// RMS level × 10000 (fixed-point) from last completed 1-second window.
static RMS_FP: AtomicU32 = AtomicU32::new(0);

/// Peak level × 10000 (fixed-point) from last completed 1-second window.
static PEAK_FP: AtomicU32 = AtomicU32::new(0);

/// dBFS × 10 stored as u32 with offset encoding: value + 1000.
///
/// -96.0 dBFS → stored as 40 ((-960 + 1000) as u32).
/// 0.0 dBFS → stored as 1000.
/// This avoids needing AtomicI32 while supporting negative dB values.
static DBFS_OFFSET: AtomicU32 = AtomicU32::new(40);

/// Blocks per 1-second measurement window: 48000 / 32 = 1500.
const BLOCKS_PER_WINDOW: u32 = 1500;

/// Minimum RMS for valid dBFS calculation (below this, report -96.0).
const RMS_FLOOR: f32 = 1e-10;

/// Blinks the user LED at 1 Hz (500ms on / 500ms off) — identical to blinky.
#[embassy_executor::task]
async fn heartbeat(mut led: daisy_embassy::led::UserLed<'static>) {
    loop {
        led.on();
        Timer::after_millis(500).await;
        led.off();
        Timer::after_millis(500).await;
    }
}

/// Formats measurement results into USB output buffer.
///
/// Format: `IN: rms=X.XXXX peak=X.XXXX dBFS=-XX.X\r\n`
///
/// Uses integer math since `f32` `Display` is unavailable in `no_std`.
/// Returns the number of bytes written.
fn format_measurement(rms_fp: u32, peak_fp: u32, dbfs_offset: u32, buf: &mut [u8]) -> usize {
    struct BufWriter<'a> {
        buf: &'a mut [u8],
        pos: usize,
    }
    impl<'a> core::fmt::Write for BufWriter<'a> {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            let bytes = s.as_bytes();
            let remaining = self.buf.len() - self.pos;
            let len = bytes.len().min(remaining);
            self.buf[self.pos..self.pos + len].copy_from_slice(&bytes[..len]);
            self.pos += len;
            Ok(())
        }
    }

    let mut w = BufWriter { buf, pos: 0 };

    // RMS: X.XXXX
    let rms_whole = rms_fp / 10000;
    let rms_frac = rms_fp % 10000;

    // Peak: X.XXXX
    let peak_whole = peak_fp / 10000;
    let peak_frac = peak_fp % 10000;

    // dBFS: decode offset encoding, format as -XX.X
    let dbfs_x10 = dbfs_offset as i32 - 1000;
    let dbfs_sign = if dbfs_x10 < 0 { "-" } else { "" };
    let dbfs_abs = if dbfs_x10 < 0 { -dbfs_x10 } else { dbfs_x10 } as u32;
    let dbfs_whole = dbfs_abs / 10;
    let dbfs_frac = dbfs_abs % 10;

    let _ = write!(
        w,
        "IN: rms={}.{:04} peak={}.{:04} dBFS={}{}.{}\r\n",
        rms_whole, rms_frac, peak_whole, peak_frac, dbfs_sign, dbfs_whole, dbfs_frac
    );
    w.pos
}

/// Background task that drives the USB device state machine.
#[embassy_executor::task]
async fn usb_task(mut device: UsbDevice<'static, Driver<'static, peripherals::USB_OTG_FS>>) -> ! {
    device.run().await
}

/// Background task that reads measurement atomics and reports via USB serial.
#[embassy_executor::task]
async fn report_task(
    mut class: CdcAcmClass<'static, Driver<'static, peripherals::USB_OTG_FS>>,
) {
    let mut out_buf = [0u8; 128];

    loop {
        Timer::after_millis(1000).await;

        if !class.dtr() {
            continue;
        }

        let rms_fp = RMS_FP.load(Ordering::Relaxed);
        let peak_fp = PEAK_FP.load(Ordering::Relaxed);
        let dbfs_offset = DBFS_OFFSET.load(Ordering::Relaxed);

        let len = format_measurement(rms_fp, peak_fp, dbfs_offset, &mut out_buf);
        let data = &out_buf[..len];

        defmt::info!(
            "IN: rms_fp={} peak_fp={} dbfs_offset={}",
            rms_fp,
            peak_fp,
            dbfs_offset
        );

        // Send in 64-byte chunks (USB FS max packet size)
        for chunk in data.chunks(64) {
            if class.write_packet(chunk).await.is_err() {
                break;
            }
        }
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let config = daisy_embassy::default_rcc();
    let p = hal::init(config);
    let board = new_daisy_board!(p);

    defmt::info!("audio_input_diag: initializing audio + USB...");

    // Spawn LED heartbeat as independent async task (not in audio callback)
    let led = board.user_led;
    spawner.spawn(heartbeat(led)).unwrap();

    // ── USB CDC ACM setup ──
    static mut EP_OUT_BUF: [u8; 256] = [0u8; 256];

    #[allow(static_mut_refs)]
    let driver = Driver::new_fs(
        board.usb_peripherals.usb_otg_fs,
        Irqs,
        board.usb_peripherals.pins.DP,
        board.usb_peripherals.pins.DN,
        unsafe { &mut EP_OUT_BUF },
        hal::usb::Config::default(),
    );

    let mut usb_config = embassy_usb::Config::new(0x1209, 0x0001);
    usb_config.manufacturer = Some("Sonido");
    usb_config.product = Some("Audio Input Diagnostics");
    usb_config.serial_number = Some("003");

    static mut CONFIG_DESC: [u8; 256] = [0; 256];
    static mut BOS_DESC: [u8; 256] = [0; 256];
    static mut MSOS_DESC: [u8; 256] = [0; 256];
    static mut CONTROL_BUF: [u8; 64] = [0; 64];
    static mut CDC_STATE: Option<State<'static>> = None;

    #[allow(static_mut_refs)]
    unsafe {
        CDC_STATE = Some(State::new());
    }

    #[allow(static_mut_refs)]
    let mut builder = unsafe {
        embassy_usb::Builder::new(
            driver,
            usb_config,
            &mut CONFIG_DESC,
            &mut BOS_DESC,
            &mut MSOS_DESC,
            &mut CONTROL_BUF,
        )
    };

    #[allow(static_mut_refs)]
    let class = unsafe { CdcAcmClass::new(&mut builder, CDC_STATE.as_mut().unwrap(), 64) };

    let usb = builder.build();
    spawner.spawn(usb_task(usb)).unwrap();
    spawner.spawn(report_task(class)).unwrap();

    defmt::info!("USB initialized, starting audio interface...");

    // ── Audio interface setup ──
    let interface = board
        .audio_peripherals
        .prepare_interface(Default::default())
        .await;
    let mut interface = defmt::unwrap!(interface.start_interface().await);

    defmt::info!("audio interface started — measuring input noise floor");

    // Measurement accumulators (captured by audio callback)
    let mut sum_sq: f32 = 0.0;
    let mut peak: f32 = 0.0;
    let mut block_count: u32 = 0;

    defmt::unwrap!(
        interface
            .start_callback(move |input, output| {
                // Output silence — don't confuse measurement with output noise
                for sample in output.iter_mut() {
                    *sample = 0;
                }

                // Measure input: accumulate sum-of-squares and track peak
                // Input is interleaved: [L0, R0, L1, R1, ..., L31, R31]
                // Average L+R for mono measurement
                for i in (0..input.len()).step_by(2) {
                    let left = u24_to_f32(input[i]);
                    let right = u24_to_f32(input[i + 1]);
                    let mono = (left + right) * 0.5;
                    sum_sq += mono * mono;
                    let abs_val = if mono >= 0.0 { mono } else { -mono };
                    if abs_val > peak {
                        peak = abs_val;
                    }
                }

                block_count += 1;

                // At window boundary (1 second), publish results and reset
                if block_count >= BLOCKS_PER_WINDOW {
                    let total_samples = (BLOCK_SIZE as u32) * BLOCKS_PER_WINDOW;
                    let rms = libm::sqrtf(sum_sq / total_samples as f32);

                    // Compute dBFS: 20 * log10(rms), clamp to -96.0
                    let dbfs = if rms > RMS_FLOOR {
                        20.0 * libm::log10f(rms)
                    } else {
                        -96.0
                    };

                    // Store as fixed-point in atomics
                    RMS_FP.store((rms * 10000.0) as u32, Ordering::Relaxed);
                    PEAK_FP.store((peak * 10000.0) as u32, Ordering::Relaxed);

                    // dBFS offset encoding: value * 10 + 1000
                    let dbfs_enc = ((dbfs * 10.0) as i32 + 1000) as u32;
                    DBFS_OFFSET.store(dbfs_enc, Ordering::Relaxed);

                    // Reset accumulators
                    sum_sq = 0.0;
                    peak = 0.0;
                    block_count = 0;
                }
            })
            .await
    );
}
