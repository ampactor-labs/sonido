//! ADC diagnostics for the Hothouse DIY pedal platform.
//!
//! Reads all 6 Hothouse knob potentiometers via ADC1 and outputs normalized
//! 0.0-1.0 readings via USB serial (CDC ACM). Also logs via defmt for probe users.
//!
//! # Hardware mapping
//!
//! | Knob   | Daisy Pin | STM32 Port | ADC Channel |
//! |--------|-----------|------------|-------------|
//! | KNOB_1 | D16       | PA3        | ADC1_INP15  |
//! | KNOB_2 | D17       | PB1        | ADC1_INP5   |
//! | KNOB_3 | D18       | PA7        | ADC1_INP7   |
//! | KNOB_4 | D19       | PA6        | ADC1_INP3   |
//! | KNOB_5 | D20       | PC1        | ADC1_INP11  |
//! | KNOB_6 | D21       | PC4        | ADC1_INP4   |
//!
//! LED 1 (D22 / PA5) brightness tracks KNOB_1: on when KNOB_1 > 0.5, off otherwise.
//!
//! # Output format (USB serial)
//!
//! ```text
//! K1=0.50 K2=0.73 K3=0.00 K4=1.00 K5=0.50 K6=0.25
//! ```
//!
//! Readings update every 500ms. Connect with any serial terminal:
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
//! cargo objcopy --example adc_diag --release -- -O binary adc_diag.bin
//! # Enter bootloader (hold BOOT, tap RESET, release BOOT — LED pulses)
//! dfu-util -a 0 -s 0x90040000:leave -D adc_diag.bin
//! ```
//!
//! # Reading results
//!
//! After flashing, the Daisy enumerates as a USB serial device:
//!
//! ```bash
//! cat /dev/ttyACM0
//! ```
//!
//! Results are also available via defmt RTT if a debug probe is connected:
//!
//! ```bash
//! probe-rs run --chip STM32H750IBKx target/thumbv7em-none-eabihf/release/examples/adc_diag
//! ```
//!
//! # Testing procedure
//!
//! 1. Flash and connect USB serial
//! 2. Turn each knob fully CCW — verify reading near 0.00
//! 3. Turn each knob fully CW — verify reading near 1.00
//! 4. Turn KNOB_1 past halfway — verify LED 1 turns on
//! 5. Turn KNOB_1 below halfway — verify LED 1 turns off
//! 6. Verify all 6 knobs respond independently

#![no_std]
#![no_main]

use core::fmt::Write as FmtWrite;

use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_stm32 as hal;
use embassy_stm32::adc::{Adc, SampleTime};
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_stm32::usb::Driver;
use embassy_stm32::{bind_interrupts, peripherals, usb};
use embassy_usb::UsbDevice;
use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
use panic_probe as _;

bind_interrupts!(struct Irqs {
    OTG_FS => usb::InterruptHandler<peripherals::USB_OTG_FS>;
});

/// Number of knob potentiometers on the Hothouse.
const NUM_KNOBS: usize = 6;

/// ADC sample time for knob readings.
///
/// 32.5 cycles gives good accuracy for slowly-varying potentiometer voltages
/// without excessive conversion time.
const KNOB_SAMPLE_TIME: SampleTime = SampleTime::CYCLES32_5;

/// LED brightness threshold for KNOB_1 (normalized 0.0-1.0).
///
/// When KNOB_1 reading exceeds this value, LED 1 turns on.
const LED_THRESHOLD: f32 = 0.5;

/// Maximum raw ADC value for 16-bit resolution.
const ADC_MAX: f32 = 65535.0;

/// Formats 6 knob readings into the output buffer.
///
/// Output format: `K1=0.50 K2=0.73 K3=0.00 K4=1.00 K5=0.50 K6=0.25\r\n`
///
/// Uses integer math to format since `f32` `Display` is not available in `no_std`.
/// Each reading is shown as `0.XX` where XX is the percentage (0-99), clamped
/// so that 1.0 displays as `1.00`.
///
/// Returns the number of bytes written.
fn format_readings(readings: &[f32; NUM_KNOBS], buf: &mut [u8]) -> usize {
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

    for (i, &val) in readings.iter().enumerate() {
        let pct = (val * 100.0) as u32;
        if pct >= 100 {
            let _ = write!(w, "K{}=1.00", i + 1);
        } else {
            let _ = write!(w, "K{}=0.{:02}", i + 1, pct);
        }
        if i < NUM_KNOBS - 1 {
            let _ = write!(w, " ");
        }
    }
    let _ = write!(w, "\r\n");
    w.pos
}

/// Background task that drives the USB device state machine.
///
/// Must run continuously for USB enumeration and data transfer to work.
#[embassy_executor::task]
async fn usb_task(mut device: UsbDevice<'static, Driver<'static, peripherals::USB_OTG_FS>>) -> ! {
    device.run().await
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let config = daisy_embassy::default_rcc();
    let p = hal::init(config);

    defmt::info!("adc_diag: initializing ADC and USB...");

    // --- LED output (active-high) ---
    let mut led1 = Output::new(p.PA5, Level::Low, Speed::Low);

    // --- ADC setup ---
    let mut adc = Adc::new(p.ADC1);

    // --- Knob pins (mutable for ADC reads) ---
    // KNOB_1: D16 / PA3
    let mut knob1_pin = p.PA3;
    // KNOB_2: D17 / PB1
    let mut knob2_pin = p.PB1;
    // KNOB_3: D18 / PA7
    let mut knob3_pin = p.PA7;
    // KNOB_4: D19 / PA6
    let mut knob4_pin = p.PA6;
    // KNOB_5: D20 / PC1
    let mut knob5_pin = p.PC1;
    // KNOB_6: D21 / PC4
    let mut knob6_pin = p.PC4;

    // --- USB CDC ACM setup (same pattern as bench_kernels) ---
    static mut EP_OUT_BUF: [u8; 256] = [0u8; 256];

    #[allow(static_mut_refs)]
    let driver = Driver::new_fs(
        p.USB_OTG_FS,
        Irqs,
        p.PA12,
        p.PA11,
        unsafe { &mut EP_OUT_BUF },
        hal::usb::Config::default(),
    );

    let mut usb_config = embassy_usb::Config::new(0x1209, 0x0001);
    usb_config.manufacturer = Some("Sonido");
    usb_config.product = Some("ADC Diagnostics");
    usb_config.serial_number = Some("002");

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
    let mut class = unsafe { CdcAcmClass::new(&mut builder, CDC_STATE.as_mut().unwrap(), 64) };

    let usb = builder.build();
    spawner.spawn(usb_task(usb)).unwrap();

    defmt::info!("adc_diag: USB initialized, entering main loop");

    // --- Main loop: read ADC, format, send via USB every 500ms ---
    let mut readings = [0.0f32; NUM_KNOBS];
    let mut out_buf = [0u8; 128];

    loop {
        // Read all 6 knob potentiometers
        let raw1: u16 = adc.blocking_read(&mut knob1_pin, KNOB_SAMPLE_TIME);
        let raw2: u16 = adc.blocking_read(&mut knob2_pin, KNOB_SAMPLE_TIME);
        let raw3: u16 = adc.blocking_read(&mut knob3_pin, KNOB_SAMPLE_TIME);
        let raw4: u16 = adc.blocking_read(&mut knob4_pin, KNOB_SAMPLE_TIME);
        let raw5: u16 = adc.blocking_read(&mut knob5_pin, KNOB_SAMPLE_TIME);
        let raw6: u16 = adc.blocking_read(&mut knob6_pin, KNOB_SAMPLE_TIME);

        // Normalize to 0.0-1.0
        readings[0] = raw1 as f32 / ADC_MAX;
        readings[1] = raw2 as f32 / ADC_MAX;
        readings[2] = raw3 as f32 / ADC_MAX;
        readings[3] = raw4 as f32 / ADC_MAX;
        readings[4] = raw5 as f32 / ADC_MAX;
        readings[5] = raw6 as f32 / ADC_MAX;

        // LED 1 tracks KNOB_1: on above threshold, off below
        if readings[0] > LED_THRESHOLD {
            led1.set_high();
        } else {
            led1.set_low();
        }

        // Log via defmt for probe users (integer percentages)
        let pcts: [u32; NUM_KNOBS] = [
            (readings[0] * 100.0) as u32,
            (readings[1] * 100.0) as u32,
            (readings[2] * 100.0) as u32,
            (readings[3] * 100.0) as u32,
            (readings[4] * 100.0) as u32,
            (readings[5] * 100.0) as u32,
        ];
        defmt::info!(
            "K1={} K2={} K3={} K4={} K5={} K6={} (x100)",
            pcts[0],
            pcts[1],
            pcts[2],
            pcts[3],
            pcts[4],
            pcts[5]
        );

        // Format readings for USB output
        let len = format_readings(&readings, &mut out_buf);
        let data = &out_buf[..len];

        // Send via USB serial (if connected)
        // Use try-write approach: if not connected or write fails, just skip.
        // The outer loop keeps running regardless of USB state.
        if class.dtr() {
            // Send in 64-byte chunks (USB FS max packet size)
            for chunk in data.chunks(64) {
                if class.write_packet(chunk).await.is_err() {
                    break;
                }
            }
        }

        // Wait 500ms before next reading
        embassy_time::Timer::after_millis(500).await;
    }
}
