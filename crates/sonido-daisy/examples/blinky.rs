//! Tier 1: Blinky — validate toolchain, flash, and Embassy runtime.
//!
//! Toggles the Daisy Seed's onboard LED at 500ms intervals.
//! If the LED blinks, your toolchain and flash process work.
//!
//! # Flash via DFU
//!
//! ```bash
//! cargo objcopy --example blinky --release -- -O binary blinky.bin
//! dfu-util -a 0 -s 0x08000000:leave -D blinky.bin
//! ```
//!
//! # Flash via SWD probe
//!
//! ```bash
//! cargo run --example blinky --release
//! ```

#![no_std]
#![no_main]

use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_time::Timer;
use panic_probe as _;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let config = daisy_embassy::default_rcc();
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
