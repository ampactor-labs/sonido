//! Embassy blinky — async runtime with Embassy HAL.
//!
//! Toggles the Daisy Seed's onboard LED at 500ms intervals using
//! Embassy's async timer and GPIO abstractions.
//!
//! Uses BOOT_SRAM mode: the Electrosmith bootloader copies firmware from
//! QSPI flash to AXI SRAM before jumping. Code runs from SRAM, so
//! `embassy_stm32::init()` can safely reconfigure clocks.
//!
//! # Build & Flash
//!
//! ```bash
//! cd crates/sonido-daisy
//! cargo objcopy --example blinky --release -- -O binary blinky.bin
//! # Press RESET, then flash within the 2.5s grace period:
//! dfu-util -a 0 -s 0x90040000:leave -D blinky.bin
//! ```

#![no_std]
#![no_main]

use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_time::Timer;
use panic_probe as _;

use sonido_daisy::ClockProfile;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let config = sonido_daisy::rcc_config(ClockProfile::Performance);
    let p = embassy_stm32::init(config);

    // Daisy Seed onboard LED is on PC7
    use embassy_stm32::gpio::{Level, Output, Speed};
    let mut led = Output::new(p.PC7, Level::Low, Speed::Low);

    defmt::info!("sonido-daisy blinky started");

    loop {
        led.set_high();
        Timer::after_millis(500).await;
        led.set_low();
        Timer::after_millis(500).await;
    }
}
