//! CPU temperature diagnostic — STM32H750 internal temperature sensor.
//!
//! Reads the internal temperature sensor via ADC3 every 2 seconds.
//! Reports current temperature, session min/max, and a thermal warning
//! above 80°C via USB serial (CDC ACM) and defmt RTT.
//!
//! Uses the STM32H750 factory calibration values burned at 30°C and 110°C:
//!
//! ```text
//! T(°C) = 80 × (raw − TS_CAL1) / (TS_CAL2 − TS_CAL1) + 30
//! ```
//!
//! Reference: STM32H750 Reference Manual, section "Temperature sensor".
//!
//! # Expected readings
//!
//! | Condition                  | Typical °C |
//! |----------------------------|-----------|
//! | Idle (no audio)            | 40–50     |
//! | Passthrough (48 kHz DMA)   | 50–60     |
//! | Full DSP load (19 effects) | 60–75     |
//! | Warning threshold          | 80        |
//! | Max operating (industrial) | 85        |
//!
//! # Build & Flash
//!
//! ```bash
//! cd crates/sonido-daisy
//! cargo objcopy --example temp_diag --release -- -O binary temp_diag.bin
//! # Press RESET, then flash within the 2.5s grace period:
//! dfu-util -a 0 -s 0x90040000:leave -D temp_diag.bin
//! ```
//!
//! # Reading results
//!
//! ```bash
//! cat /dev/ttyACM0
//! # or: screen /dev/ttyACM0 115200
//! ```
//!
//! Output format:
//! ```text
//! temp=52C min=48C max=54C
//! ```
//!
//! A `WARN: temp > 80C` line is appended when the threshold is exceeded.

#![no_std]
#![no_main]

use core::fmt::Write as FmtWrite;

use daisy_embassy::new_daisy_board;
use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_stm32 as hal;
use embassy_stm32::adc::{Adc, SampleTime, Temperature};
use embassy_stm32::usb::Driver;
use embassy_stm32::{bind_interrupts, peripherals, usb};
use embassy_usb::UsbDevice;
use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
use panic_probe as _;
use sonido_daisy::heartbeat;

bind_interrupts!(struct Irqs {
    OTG_FS => usb::InterruptHandler<peripherals::USB_OTG_FS>;
});

/// Temperature warning threshold in °C.
const WARN_TEMP_C: i32 = 80;

/// ADC sample time for temperature sensor.
///
/// STM32H750 datasheet requires ≥ 9 µs sampling time for the internal
/// temperature sensor. CYCLES810_5 is conservatively safe across all
/// ADC clock configurations.
const TEMP_SAMPLE_TIME: SampleTime = SampleTime::CYCLES810_5;

/// STM32H750 factory temperature calibration — raw ADC value at 30°C, 3.3V.
///
/// Burned at production. Address from RM0433 section "Temperature sensor".
const TS_CAL1_ADDR: *const u16 = 0x1FF1_E820 as *const u16;

/// STM32H750 factory temperature calibration — raw ADC value at 110°C, 3.3V.
///
/// Burned at production. Address from RM0433 section "Temperature sensor".
const TS_CAL2_ADDR: *const u16 = 0x1FF1_E824 as *const u16;

/// Converts a raw ADC temperature reading to degrees Celsius.
///
/// Uses factory calibration values to account for per-chip variation.
/// Integer arithmetic throughout — no f32 required.
///
/// Formula (RM0433):
/// `T = 80 × (raw − TS_CAL1) / (TS_CAL2 − TS_CAL1) + 30`
fn raw_to_celsius(raw: u16) -> i32 {
    // Safety: these addresses are valid read-only flash locations on STM32H750.
    let cal1 = unsafe { TS_CAL1_ADDR.read() } as i32;
    let cal2 = unsafe { TS_CAL2_ADDR.read() } as i32;
    let raw = raw as i32;
    80 * (raw - cal1) / (cal2 - cal1) + 30
}

/// Background task that drives the USB device state machine.
#[embassy_executor::task]
async fn usb_task(mut device: UsbDevice<'static, Driver<'static, peripherals::USB_OTG_FS>>) -> ! {
    device.run().await
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let config = daisy_embassy::default_rcc();
    let p = hal::init(config);

    let board = new_daisy_board!(p);

    let led = board.user_led;
    spawner.spawn(heartbeat(led)).unwrap();

    defmt::info!("temp_diag: initializing ADC3 and USB...");

    // Temperature sensor is on ADC3.
    let mut adc = Adc::new(p.ADC3);
    let mut temp_ch = Temperature;

    // --- USB CDC ACM setup (same pattern as adc_diag) ---
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
    usb_config.product = Some("Temp Diagnostics");
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
    let mut class = unsafe { CdcAcmClass::new(&mut builder, CDC_STATE.as_mut().unwrap(), 64) };

    let usb = builder.build();
    spawner.spawn(usb_task(usb)).unwrap();

    defmt::info!("temp_diag: USB initialized, sampling every 2s");

    let mut min_c = i32::MAX;
    let mut max_c = i32::MIN;
    let mut out_buf = [0u8; 128];

    loop {
        let raw = adc.blocking_read(&mut temp_ch, TEMP_SAMPLE_TIME);
        let temp_c = raw_to_celsius(raw);

        if temp_c < min_c { min_c = temp_c; }
        if temp_c > max_c { max_c = temp_c; }

        defmt::info!("temp={}C min={}C max={}C", temp_c, min_c, max_c);

        if temp_c > WARN_TEMP_C {
            defmt::warn!("WARN: temp > {}C", WARN_TEMP_C);
        }

        // Format for USB serial
        let len = {
            struct W<'a> { buf: &'a mut [u8], pos: usize }
            impl core::fmt::Write for W<'_> {
                fn write_str(&mut self, s: &str) -> core::fmt::Result {
                    let b = s.as_bytes();
                    let n = b.len().min(self.buf.len() - self.pos);
                    self.buf[self.pos..self.pos + n].copy_from_slice(&b[..n]);
                    self.pos += n;
                    Ok(())
                }
            }
            let mut w = W { buf: &mut out_buf, pos: 0 };
            let _ = write!(w, "temp={}C min={}C max={}C\r\n", temp_c, min_c, max_c);
            if temp_c > WARN_TEMP_C {
                let _ = write!(w, "WARN: temp > {}C\r\n", WARN_TEMP_C);
            }
            w.pos
        };

        if class.dtr() {
            for chunk in out_buf[..len].chunks(64) {
                if class.write_packet(chunk).await.is_err() {
                    break;
                }
            }
        }

        embassy_time::Timer::after_millis(2000).await;
    }
}
