//! Integration test: passthrough audio + LED heartbeat.
//!
//! If blinky blinks and passthrough audio works, this binary should do both.
//! Confirms `new_daisy_board!` + spawned LED task combination works on this hardware.
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

use daisy_embassy::new_daisy_board;
use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_stm32 as hal;
use embassy_time::Timer;
use panic_probe as _;

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

    let led = board.user_led;
    spawner.spawn(heartbeat(led)).unwrap();

    let interface = board.audio_peripherals.prepare_interface(Default::default()).await;
    let mut interface = defmt::unwrap!(interface.start_interface().await);

    defmt::unwrap!(
        interface.start_callback(|input, output| {
            output.copy_from_slice(input);
        }).await
    );
}
