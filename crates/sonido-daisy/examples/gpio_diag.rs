//! GPIO diagnostics for the Hothouse DIY pedal platform.
//!
//! Validates all Hothouse digital I/O: 2 LEDs, 2 footswitches, and 3 toggle
//! switches. Use this example to verify that your Hothouse hardware is wired
//! correctly before running audio examples.
//!
//! # Hardware mapping
//!
//! | Function        | Daisy Pin | STM32 Port | Direction   |
//! |-----------------|-----------|------------|-------------|
//! | LED 1           | D22       | PA5        | Output      |
//! | LED 2           | D23       | PA4        | Output      |
//! | Footswitch 1 (L)| D25      | PA0        | Input (pull-up, active-low) |
//! | Footswitch 2 (R)| D26      | PD11       | Input (pull-up, active-low) |
//! | Toggle 1 Up     | D9       | PB4        | Input (pull-up, active-low) |
//! | Toggle 1 Down   | D10      | PB5        | Input (pull-up, active-low) |
//! | Toggle 2 Up     | D7       | PG10       | Input (pull-up, active-low) |
//! | Toggle 2 Down   | D8       | PG11       | Input (pull-up, active-low) |
//! | Toggle 3 Up     | D5       | PD2        | Input (pull-up, active-low) |
//! | Toggle 3 Down   | D6       | PC12       | Input (pull-up, active-low) |
//!
//! # Startup sequence
//!
//! On boot, both LEDs alternate 5 times (100ms per phase) to confirm that
//! LED wiring is correct. After the startup sequence completes, the example
//! enters the main polling loop.
//!
//! # Main loop behavior (20ms poll rate)
//!
//! - **Footswitches**: press and release a footswitch to toggle its
//!   corresponding LED. The toggle fires on release (not press) for simple
//!   debounce. State changes are logged via defmt.
//!
//! - **Toggle switches**: when a toggle switch changes position, the new
//!   position is logged and the LEDs blink a pattern for 300ms:
//!   - Both LEDs on  = Up position
//!   - LED 1 only    = Middle position
//!   - LED 2 only    = Down position
//!
//!   After the pattern display, LEDs return to their footswitch-controlled
//!   state.
//!
//! # Build & Flash
//!
//! ```bash
//! cd crates/sonido-daisy
//! cargo objcopy --example gpio_diag --release -- -O binary gpio_diag.bin
//! # Press RESET, then flash within the 2.5s grace period:
//! dfu-util -a 0 -s 0x90040000:leave -D gpio_diag.bin
//! ```
//!
//! # Reading output
//!
//! Connect a debug probe and run:
//!
//! ```bash
//! probe-rs run --chip STM32H750IBKx target/thumbv7em-none-eabihf/release/examples/gpio_diag
//! ```
//!
//! Or use `defmt-print` to read RTT output from a running target.

#![no_std]
#![no_main]

use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_stm32 as hal;
use embassy_stm32::gpio::{Input, Level, Output, Pull, Speed};
use embassy_time::Timer;
use panic_probe as _;

/// 3-position toggle switch state.
///
/// Derived from two GPIO pins (up and down). The middle position is detected
/// when neither pin is active. Both pins active simultaneously indicates a
/// hardware fault (wiring error or broken switch).
#[derive(Clone, Copy, PartialEq, Eq, defmt::Format)]
enum TogglePosition {
    /// Up pin active, down pin inactive.
    Up,
    /// Neither pin active (center detent).
    Middle,
    /// Down pin active, up pin inactive.
    Down,
    /// Both pins active — should never happen in a correctly wired switch.
    Fault,
}

/// Reads the position of a 3-way toggle switch from its two GPIO pins.
///
/// Both pins use internal pull-ups and are active-low: a logical low means
/// the switch is connecting that pin to ground.
///
/// # Truth table
///
/// | up_pin low | down_pin low | Result  |
/// |------------|--------------|---------|
/// | yes        | no           | Up      |
/// | no         | no           | Middle  |
/// | no         | yes          | Down    |
/// | yes        | yes          | Fault   |
fn read_toggle(up_pin: &Input<'_>, down_pin: &Input<'_>) -> TogglePosition {
    let up_active = up_pin.is_low();
    let down_active = down_pin.is_low();

    match (up_active, down_active) {
        (true, false) => TogglePosition::Up,
        (false, false) => TogglePosition::Middle,
        (false, true) => TogglePosition::Down,
        (true, true) => TogglePosition::Fault,
    }
}

/// Briefly shows a toggle position on the LEDs, then restores previous state.
///
/// LED pattern held for 300ms:
/// - Up:     both LEDs on
/// - Middle: LED 1 on, LED 2 off
/// - Down:   LED 1 off, LED 2 on
/// - Fault:  both LEDs rapid-blink 3 times (50ms per phase)
async fn show_toggle_leds(
    led1: &mut Output<'_>,
    led2: &mut Output<'_>,
    position: TogglePosition,
    led1_state: bool,
    led2_state: bool,
) {
    match position {
        TogglePosition::Up => {
            led1.set_high();
            led2.set_high();
            Timer::after_millis(300).await;
        }
        TogglePosition::Middle => {
            led1.set_high();
            led2.set_low();
            Timer::after_millis(300).await;
        }
        TogglePosition::Down => {
            led1.set_low();
            led2.set_high();
            Timer::after_millis(300).await;
        }
        TogglePosition::Fault => {
            // Rapid blink both LEDs to signal hardware fault
            for _ in 0..3 {
                led1.set_high();
                led2.set_high();
                Timer::after_millis(50).await;
                led1.set_low();
                led2.set_low();
                Timer::after_millis(50).await;
            }
        }
    }

    // Restore footswitch-controlled LED state
    if led1_state {
        led1.set_high();
    } else {
        led1.set_low();
    }
    if led2_state {
        led2.set_high();
    } else {
        led2.set_low();
    }
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let config = daisy_embassy::default_rcc();
    let p = hal::init(config);

    // --- LEDs (active-high outputs) ---
    let mut led1 = Output::new(p.PA5, Level::Low, Speed::Low);
    let mut led2 = Output::new(p.PA4, Level::Low, Speed::Low);

    // --- Footswitches (momentary, active-low with pull-up) ---
    let foot1 = Input::new(p.PA0, Pull::Up);
    let foot2 = Input::new(p.PD11, Pull::Up);

    // --- Toggle switches (3-way, 2 pins each, pull-up) ---
    let tog1_up = Input::new(p.PB4, Pull::Up);
    let tog1_down = Input::new(p.PB5, Pull::Up);
    let tog2_up = Input::new(p.PG10, Pull::Up);
    let tog2_down = Input::new(p.PG11, Pull::Up);
    let tog3_up = Input::new(p.PD2, Pull::Up);
    let tog3_down = Input::new(p.PC12, Pull::Up);

    defmt::info!("gpio_diag: starting LED test sequence");

    // === Startup: alternate LEDs 5 times ===
    for i in 0..5 {
        led1.set_high();
        led2.set_low();
        Timer::after_millis(100).await;
        led1.set_low();
        led2.set_high();
        Timer::after_millis(100).await;
        defmt::info!("LED alternate cycle {}/5", i + 1);
    }

    // Both off after startup sequence
    led1.set_low();
    led2.set_low();

    defmt::info!("gpio_diag: LED test complete, entering main loop");

    // === Main loop state ===

    // Footswitch state: tracks whether each footswitch was pressed last poll
    // (for release detection) and the LED toggle state.
    let mut foot1_was_pressed = foot1.is_low();
    let mut foot2_was_pressed = foot2.is_low();
    let mut led1_on = false;
    let mut led2_on = false;

    // Toggle switch state: tracks last known position for change detection.
    let mut tog1_pos = read_toggle(&tog1_up, &tog1_down);
    let mut tog2_pos = read_toggle(&tog2_up, &tog2_down);
    let mut tog3_pos = read_toggle(&tog3_up, &tog3_down);

    defmt::info!(
        "Initial toggles: SW1={} SW2={} SW3={}",
        tog1_pos,
        tog2_pos,
        tog3_pos
    );

    // === Main polling loop (20ms period) ===
    loop {
        // --- Footswitch handling (toggle LED on release) ---
        let foot1_pressed = foot1.is_low();
        let foot2_pressed = foot2.is_low();

        // Footswitch 1: was pressed, now released → toggle LED 1
        if foot1_was_pressed && !foot1_pressed {
            led1_on = !led1_on;
            if led1_on {
                led1.set_high();
            } else {
                led1.set_low();
            }
            defmt::info!("Footswitch 1 released → LED 1 {}", if led1_on { "ON" } else { "OFF" });
        }

        // Footswitch 2: was pressed, now released → toggle LED 2
        if foot2_was_pressed && !foot2_pressed {
            led2_on = !led2_on;
            if led2_on {
                led2.set_high();
            } else {
                led2.set_low();
            }
            defmt::info!("Footswitch 2 released → LED 2 {}", if led2_on { "ON" } else { "OFF" });
        }

        foot1_was_pressed = foot1_pressed;
        foot2_was_pressed = foot2_pressed;

        // --- Toggle switch handling (detect position changes) ---
        let new_tog1 = read_toggle(&tog1_up, &tog1_down);
        let new_tog2 = read_toggle(&tog2_up, &tog2_down);
        let new_tog3 = read_toggle(&tog3_up, &tog3_down);

        if new_tog1 != tog1_pos {
            defmt::info!("Toggle 1: {} → {}", tog1_pos, new_tog1);
            show_toggle_leds(&mut led1, &mut led2, new_tog1, led1_on, led2_on).await;
            tog1_pos = new_tog1;
        }

        if new_tog2 != tog2_pos {
            defmt::info!("Toggle 2: {} → {}", tog2_pos, new_tog2);
            show_toggle_leds(&mut led1, &mut led2, new_tog2, led1_on, led2_on).await;
            tog2_pos = new_tog2;
        }

        if new_tog3 != tog3_pos {
            defmt::info!("Toggle 3: {} → {}", tog3_pos, new_tog3);
            show_toggle_leds(&mut led1, &mut led2, new_tog3, led1_on, led2_on).await;
            tog3_pos = new_tog3;
        }

        Timer::after_millis(20).await;
    }
}
