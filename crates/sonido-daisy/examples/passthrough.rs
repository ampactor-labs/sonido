//! Tier 3: Audio passthrough — validate codec and DMA.
//!
//! Copies audio input directly to output with no processing.
//! Validates the full audio path: codec ADC -> SAI RX -> DMA -> CPU -> DMA -> SAI TX -> codec DAC.
//!
//! # Audio Format
//!
//! The SAI DMA driver delivers 32 stereo pairs per callback as interleaved `u32`:
//! `[L0, R0, L1, R1, ..., L31, R31]` — 64 elements total.
//! Each `u32` is a 24-bit signed sample left-justified in 32 bits.
//!
//! Passthrough = `output.copy_from_slice(input)` — no format conversion needed.
//!
//! # Build & Flash
//!
//! ```bash
//! cd crates/sonido-daisy
//! cargo objcopy --example passthrough --release -- -O binary -R .sram1_bss passthrough.bin
//! # Press RESET, then flash within the 2.5s grace period:
//! dfu-util -a 0 -s 0x90040000:leave -D passthrough.bin
//! ```
//!
//! # Testing
//!
//! 1. Connect guitar/synth to Hothouse input
//! 2. Connect Hothouse output to amp/interface
//! 3. Play — audio should pass through unmodified
//! 4. Verify: no clicks, pops, level changes, or added noise

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

    defmt::info!("sonido-daisy passthrough starting");

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

    defmt::info!("audio interface started — passthrough active");

    defmt::unwrap!(
        interface
            .start_callback(|input, output| {
                output.copy_from_slice(input);
            })
            .await
    );
}
