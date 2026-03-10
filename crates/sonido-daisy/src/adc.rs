//! ADC helpers for the Daisy Seed STM32H750.
//!
//! The STM32H750's internal temperature sensor is physically isolated
//! from ADC3 until the `VSENSEEN` bit is set in ADC3_COMMON CCR
//! (RM0433 §24.5.6). Embassy's `Adc::new()` does not set this bit —
//! you must call [`enable_temperature`] before the first temperature
//! reading or the ADC will return garbage (~103 °C).
//!
//! # ADC clock
//!
//! The ADC clock comes from PLL2_P (100 MHz after `rcc_config()` fix).
//! With `SampleTime::CYCLES810_5`, the temperature sensor sampling time
//! is 810.5 / 100 MHz = 8.1 µs. The datasheet minimum is 9 µs — close
//! enough for diagnostic use (within ~2 °C). For production accuracy,
//! set the ADC3_COMMON prescaler to DIV2 (50 MHz → 16.2 µs).

use embassy_stm32::adc::{Adc, Temperature};
use embassy_stm32::peripherals::ADC3;

/// Enables the internal temperature sensor on ADC3 and returns the channel.
///
/// Must be called once after `Adc::new(p.ADC3)`. Without this, `VSENSEEN = 0`
/// in ADC3_COMMON CCR and the sensor is physically disconnected — producing
/// garbage readings (typically ~103 °C because the floating input voltage
/// maps to roughly that temperature via the calibration formula).
///
/// # Example
///
/// ```ignore
/// let mut adc3 = Adc::new(p.ADC3);
/// let mut temp_ch = sonido_daisy::adc::enable_temperature(&mut adc3);
/// let raw = adc3.blocking_read(&mut temp_ch, SampleTime::CYCLES810_5);
/// ```
pub fn enable_temperature(adc: &mut Adc<'_, ADC3>) -> Temperature {
    adc.enable_temperature()
}

/// STM32H750 factory calibration address: raw ADC at 30 °C (16-bit, 3.3 V).
pub const TS_CAL1_ADDR: *const u16 = 0x1FF1_E820 as *const u16;

/// STM32H750 factory calibration address: raw ADC at 110 °C (16-bit, 3.3 V).
pub const TS_CAL2_ADDR: *const u16 = 0x1FF1_E824 as *const u16;

/// Reads factory calibration values for the temperature sensor.
///
/// Returns `(cal1, cal2)` where cal1 is the raw ADC reading at 30 °C
/// and cal2 is the raw ADC reading at 110 °C.
///
/// # Safety
///
/// The calibration addresses are factory-programmed ROM in the STM32H750
/// system memory area. They are always valid and read-only.
pub fn read_calibration() -> (i32, i32) {
    // Safety: TS_CAL1/CAL2 are factory-programmed at documented addresses.
    let cal1 = unsafe { TS_CAL1_ADDR.read() } as i32;
    let cal2 = unsafe { TS_CAL2_ADDR.read() } as i32;
    (cal1, cal2)
}

/// Converts a raw ADC temperature reading to degrees Celsius.
///
/// Uses the linear formula from RM0433 §24.4.2:
/// `T = 80 × (raw - CAL1) / (CAL2 - CAL1) + 30`
///
/// where CAL1/CAL2 are the factory calibration values at 30 °C / 110 °C.
pub fn raw_to_celsius(raw: u16, cal1: i32, cal2: i32) -> i32 {
    80 * (raw as i32 - cal1) / (cal2 - cal1) + 30
}
