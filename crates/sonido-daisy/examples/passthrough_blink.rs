//! Integration test: passthrough audio + LED heartbeat.
//!
//! If blinky blinks and passthrough audio works, this binary should do both.
//! Confirms audio + spawned LED task combination works on this hardware.
//! Flash this BEFORE the diagnostic examples to isolate any LED-spawn issues.
//!
//! User LED (PC7) blinks 1 Hz. Audio passes through unmodified.
//!
//! # Build & Flash
//! ```bash
//! cd crates/sonido-daisy
//! cargo objcopy --example passthrough_blink --release -- -O binary -R .sram1_bss passthrough_blink.bin
//! # Press RESET, then flash within 2.5s grace period:
//! dfu-util -a 0 -s 0x90040000:leave -D passthrough_blink.bin
//! ```

#![no_std]
#![no_main]

use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_stm32 as hal;
use panic_probe as _;

use sonido_daisy::{ClockProfile, heartbeat, led::UserLed};

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let config = sonido_daisy::rcc_config(ClockProfile::Performance);
    let p = hal::init(config);

    let led = UserLed::new(p.PC7);
    spawner.spawn(heartbeat(led)).unwrap();

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

    defmt::unwrap!(
        interface
            .start_callback(|input, output| {
                output.copy_from_slice(input);
            })
            .await
    );
}
