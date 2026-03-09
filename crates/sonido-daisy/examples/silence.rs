//! Diagnostic: output silence through the audio codec.
//!
//! Writes zeros to every output sample. If you still hear noise through the
//! Hothouse output, the noise is coming from the analog circuit (op-amps,
//! power supply, ground loops) — not from the digital audio path.
//!
//! If this is silent, the codec and SAI/DMA are working correctly.

#![no_std]
#![no_main]

use daisy_embassy::new_daisy_board;
use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_stm32 as hal;
use panic_probe as _;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let config = daisy_embassy::default_rcc();
    let p = hal::init(config);
    let board = new_daisy_board!(p);

    defmt::info!("silence: outputting zeros");

    let interface = board
        .audio_peripherals
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
