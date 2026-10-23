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

// The sonido_daisy library links `alloc`, so every binary needs a global
// allocator even if, like this one, it never allocates. 256 KB of D2 SRAM2+3,
// clear of the SAI DMA buffers in SRAM1 (0x3000_0000).
#[global_allocator]
static HEAP: embedded_alloc::LlffHeap = embedded_alloc::LlffHeap::empty();

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    sonido_daisy::enable_d2_sram();

    #[allow(unsafe_code)]
    // SAFETY: called once, before any allocation; 0x3000_8000..+256 KB is D2
    // SRAM2+3 (clock enabled just above), used by nothing else here.
    unsafe {
        HEAP.init(0x3000_8000, 256 * 1024);
    }
    let config = sonido_daisy::rcc_config(ClockProfile::Efficient);
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
