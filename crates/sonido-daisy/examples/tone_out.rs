//! Diagnostic: 440 Hz sine wave output through the audio codec.
//!
//! Outputs a known 440 Hz sine at full scale to test the DAC→analog output path.
//! Answers: "Does the DAC output reach the output jack, or is the oscillation
//! completely overriding it?"
//!
//! - Hear 440 Hz underneath the noise → DAC works, op-amp is oscillating on top
//! - Only hear "EEEEEEE" → analog output stage is completely overriding DAC output
//! - Hear 440 Hz clean, no "EEEEEEE" → the oscillation was input-side
//!
//! User LED (PC7) blinks at 1 Hz (500ms on / 500ms off) — same as blinky.
//!
//! # Build & Flash
//!
//! ```bash
//! cd crates/sonido-daisy
//! cargo objcopy --example tone_out --release -- -O binary -R .sram1_bss tone_out.bin
//! # Press RESET, then flash within the 2.5s grace period:
//! dfu-util -a 0 -s 0x90040000:leave -D tone_out.bin
//! # (Hold BOOT while pressing RESET to extend the grace period indefinitely)
//! ```

#![no_std]
#![no_main]

use core::f32::consts::PI;

use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_stm32 as hal;
use panic_probe as _;

use sonido_daisy::{ClockProfile, SAMPLE_RATE, f32_to_u24, heartbeat, led::UserLed};

/// Phase increment per sample for 440 Hz at 48 kHz.
///
/// `2π × 440 / 48000 ≈ 0.05759586`
const PHASE_INC: f32 = 2.0 * PI * 440.0 / SAMPLE_RATE;

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let config = sonido_daisy::rcc_config(ClockProfile::Performance);
    let p = hal::init(config);

    defmt::info!("tone_out: 440 Hz sine output starting");

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

    defmt::info!("audio interface started — outputting 440 Hz sine");

    let mut phase: f32 = 0.0;

    defmt::unwrap!(
        interface
            .start_callback(move |_input, output| {
                // Generate 440 Hz sine, write to both channels
                // Output is interleaved: [L0, R0, L1, R1, ..., L31, R31]
                for i in (0..output.len()).step_by(2) {
                    let sample = libm::sinf(phase);
                    let encoded = f32_to_u24(sample);
                    output[i] = encoded;
                    output[i + 1] = encoded;
                    phase += PHASE_INC;
                }

                // Wrap phase to prevent float precision loss over time
                if phase >= 2.0 * PI {
                    phase -= 2.0 * PI;
                }
            })
            .await
    );
}
