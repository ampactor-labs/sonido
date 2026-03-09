//! Diagnostic: 1 kHz full-scale square wave output.
//!
//! Outputs the loudest, most obvious digital signal possible through the DAC.
//! If you can't hear THIS through the analog noise, the DAC output is completely
//! disconnected from the output jack.
//!
//! Zero external DSP dependencies — just a counter and a toggle between +0.95
//! and -0.95. If this doesn't work, nothing will.
//!
//! User LED (PC7) blinks at 1 Hz (500ms on / 500ms off) — same as blinky.
//!
//! # Build & Flash
//!
//! ```bash
//! cd crates/sonido-daisy
//! cargo objcopy --example square_out --release -- -O binary -R .sram1_bss square_out.bin
//! # Press RESET, then flash within the 2.5s grace period:
//! dfu-util -a 0 -s 0x90040000:leave -D square_out.bin
//! # (Hold BOOT while pressing RESET to extend the grace period indefinitely)
//! ```

#![no_std]
#![no_main]

use daisy_embassy::new_daisy_board;
use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_stm32 as hal;
use embassy_time::Timer;
use panic_probe as _;

use sonido_daisy::f32_to_u24;

/// Half-period in samples for 1 kHz at 48 kHz sample rate.
///
/// 48000 / 1000 / 2 = 24 samples per half-cycle.
const HALF_PERIOD: u32 = 24;

/// Blinks the user LED at 1 Hz (500ms on / 500ms off) — identical to blinky.
#[embassy_executor::task]
async fn heartbeat(mut led: daisy_embassy::led::UserLed<'static>) {
    loop {
        led.on();
        Timer::after_millis(500).await;
        led.off();
        Timer::after_millis(500).await;
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let config = daisy_embassy::default_rcc();
    let p = hal::init(config);
    let board = new_daisy_board!(p);

    defmt::info!("square_out: 1 kHz full-scale square wave starting");

    // Spawn LED heartbeat as independent async task (not in audio callback)
    let led = board.user_led;
    spawner.spawn(heartbeat(led)).unwrap();

    let interface = board
        .audio_peripherals
        .prepare_interface(Default::default())
        .await;
    let mut interface = defmt::unwrap!(interface.start_interface().await);

    defmt::info!("audio interface started — outputting 1 kHz square wave");

    // Pre-compute the two output values (near full-scale, ±0.95)
    let high = f32_to_u24(0.95);
    let low = f32_to_u24(-0.95);

    let mut sample_counter: u32 = 0;

    defmt::unwrap!(
        interface
            .start_callback(move |_input, output| {
                // 1 kHz square wave: toggle between +0.95 and -0.95 every 24 samples
                // Output is interleaved: [L0, R0, L1, R1, ..., L31, R31]
                for i in (0..output.len()).step_by(2) {
                    let val = if (sample_counter / HALF_PERIOD).is_multiple_of(2) {
                        high
                    } else {
                        low
                    };
                    output[i] = val;
                    output[i + 1] = val;
                    sample_counter = sample_counter.wrapping_add(1);
                }
            })
            .await
    );
}
