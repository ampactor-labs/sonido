//! User LED abstraction for the Daisy Seed (PC7).

use embassy_stm32::{self as hal, Peri};
use hal::gpio::{Level, Output, Speed};

/// Thin wrapper around the Daisy Seed's user LED on PC7.
///
/// Provides `on()` / `off()` instead of `set_high()` / `set_low()` for
/// clarity in LED-control code.
pub struct UserLed<'a>(Output<'a>);

impl<'a> UserLed<'a> {
    /// Creates a new `UserLed` from the PC7 peripheral. Starts off (low).
    pub fn new(pin: Peri<'a, hal::peripherals::PC7>) -> Self {
        Self(Output::new(pin, Level::Low, Speed::Low))
    }

    /// Turns the LED on (drives PC7 high).
    pub fn on(&mut self) {
        self.0.set_high();
    }

    /// Turns the LED off (drives PC7 low).
    pub fn off(&mut self) {
        self.0.set_low();
    }
}
