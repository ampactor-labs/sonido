//! Diagnostic: output silence through the audio codec.
//!
//! Writes zeros to every output sample. If you still hear noise through the
//! Hothouse output, the noise is coming from the analog circuit (op-amps,
//! power supply, ground loops) — not from the digital audio path.
//!
//! If this is silent, the codec and SAI/DMA are working correctly.

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

    defmt::info!("silence: outputting zeros");

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
            .start_callback(|_input, output| {
                // Zero every sample — pure digital silence
                for sample in output.iter_mut() {
                    *sample = 0;
                }
            })
            .await
    );
}
