//! Hothouse pedal platform — pin definitions, Embassy control task, and platform controller.
//!
//! Provides the hardware-specific layer for the Cleveland Music Co. Hothouse DIY pedal
//! on the Electrosmith Daisy Seed. Includes:
//!
//! - [`HothouseControls`] — peripheral bundle for the control task
//! - [`hothouse_control_task`] — Embassy task that reads ADC + GPIO at 50 Hz
//! - [`decode_toggle`] — standardized 3-position toggle decoder
//! - `hothouse_pins!` — macro to extract control pins from Embassy peripherals
//! - [`HothousePlatform`] — `PlatformController` implementation
//!
//! # Pin Mapping
//!
//! | Function        | Daisy Pin | STM32 | ADC Channel | Notes               |
//! |-----------------|-----------|-------|-------------|---------------------|
//! | LED 1 out       | D22       | PA5   | —           | Active-high         |
//! | LED 2 out       | D23       | PA4   | —           | Active-high         |
//! | Footswitch 1    | D25       | PA0   | —           | Pull-up; active-low |
//! | Footswitch 2    | D26       | PD11  | —           | Pull-up; active-low |
//! | Toggle 1 Up     | D9        | PB4   | —           | Pull-up; active-low |
//! | Toggle 1 Down   | D10       | PB5   | —           | Pull-up; active-low |
//! | Toggle 2 Up     | D7        | PG10  | —           | Pull-up; active-low |
//! | Toggle 2 Down   | D8        | PG11  | —           | Pull-up; active-low |
//! | Toggle 3 Up     | D5        | PD2   | —           | Pull-up; active-low |
//! | Toggle 3 Down   | D6        | PC12  | —           | Pull-up; active-low |
//! | KNOB_1          | D16       | PA3   | ADC1_INP15  |                     |
//! | KNOB_2          | D17       | PB1   | ADC1_INP5   |                     |
//! | KNOB_3          | D18       | PA7   | ADC1_INP7   |                     |
//! | KNOB_4          | D19       | PA6   | ADC1_INP3   |                     |
//! | KNOB_5          | D20       | PC1   | ADC1_INP11  |                     |
//! | KNOB_6          | D21       | PC4   | ADC1_INP4   |                     |
//!
//! # Architecture
//!
//! All 6 knobs use uniform `blocking_read()` polling — one ADC conversion per
//! knob per 50 Hz cycle. At ~8 µs per channel (CYCLES387_5 sample time),
//! 6 reads cost ~48 µs per 20 ms cycle (0.24% CPU). This matches libDaisy's
//! own `AnalogControl` polling approach. No DMA channel, DMA buffer, or D2
//! SRAM allocation is needed for the control path.
//!
//! GPIO reads (toggles, footswitches) and LED writes are instantaneous register
//! accesses. All control I/O runs at 50 Hz in its own Embassy task, **never**
//! inside the audio DMA callback.

use embassy_stm32::adc::{Adc, AnyAdcChannel, SampleTime};
use embassy_stm32::gpio::{Input, Output};
use embassy_stm32::peripherals;

use crate::controls::HothouseBuffer;

// ── Constants ─────────────────────────────────────────────────────────────

/// ADC sample time for knob readings.
///
/// CYCLES387_5 = 7.75 µs at 50 MHz — required for high-impedance pot sources
/// (~10 kΩ). Shorter times cause crosstalk between adjacent channels because
/// the S&H capacitor doesn't fully settle.
pub const KNOB_SAMPLE_TIME: SampleTime = SampleTime::CYCLES387_5;

/// Control task poll interval in milliseconds (50 Hz).
pub const POLL_INTERVAL_MS: u64 = 20;

/// IIR smoothing coefficient for knob readings.
///
/// At 50 Hz with alpha=0.1: 90% step response in ~460 ms.
/// Matches libDaisy's `AnalogControl` default (1 kHz / 0.01 alpha).
pub const KNOB_ALPHA: f32 = 0.1;

// ── Toggle decode ─────────────────────────────────────────────────────────

/// Decodes a 3-position toggle switch from its two GPIO pins (both pull-up, active-low).
///
/// # Returns
///
/// - `0` = UP (up pin active)
/// - `1` = MID (neither pin active, or fault — both active)
/// - `2` = DN (down pin active)
///
/// # Convention
///
/// This encoding is standardized across all Hothouse examples. The
/// [`ControlBuffer`](crate::controls::ControlBuffer) stores these values directly.
#[inline]
pub fn decode_toggle(up: &Input<'_>, dn: &Input<'_>) -> u8 {
    match (up.is_low(), dn.is_low()) {
        (true, false) => 0, // UP
        (false, true) => 2, // DN
        _ => 1,             // MID (or fault → treat as MID)
    }
}

// ── Peripheral bundle ─────────────────────────────────────────────────────

/// All control peripherals for the Hothouse pedal.
///
/// Embassy tasks have a limited number of arguments, so we pass everything
/// in a single struct. Create this with the `hothouse_pins!` macro.
pub struct HothouseControls {
    /// ADC1 instance for knob readings.
    pub adc1: Adc<'static, peripherals::ADC1>,
    /// Knobs 1–6, type-erased for uniform `blocking_read()` polling.
    ///
    /// Index order: K1 (PA3), K2 (PB1), K3 (PA7), K4 (PA6), K5 (PC1), K6 (PC4).
    pub knobs: [AnyAdcChannel<'static, peripherals::ADC1>; 6],
    /// Toggle 1 up pin (PB4, pull-up, active-low).
    pub tog1_up: Input<'static>,
    /// Toggle 1 down pin (PB5, pull-up, active-low).
    pub tog1_dn: Input<'static>,
    /// Toggle 2 up pin (PG10, pull-up, active-low).
    pub tog2_up: Input<'static>,
    /// Toggle 2 down pin (PG11, pull-up, active-low).
    pub tog2_dn: Input<'static>,
    /// Toggle 3 up pin (PD2, pull-up, active-low).
    pub tog3_up: Input<'static>,
    /// Toggle 3 down pin (PC12, pull-up, active-low).
    pub tog3_dn: Input<'static>,
    /// Footswitch 1 (PA0, pull-up, active-low).
    pub foot1: Input<'static>,
    /// Footswitch 2 (PD11, pull-up, active-low).
    pub foot2: Input<'static>,
    /// LED 1 output (PA5, active-high).
    pub led1: Output<'static>,
    /// LED 2 output (PA4, active-high).
    pub led2: Output<'static>,
}

// ── hothouse_pins! macro ──────────────────────────────────────────────────

/// Extracts all Hothouse control pins from Embassy peripherals.
///
/// Call this **before** constructing [`AudioPeripherals`](crate::audio::AudioPeripherals),
/// as both consume pins from the same `embassy_stm32::init()` struct.
///
/// # Example
///
/// ```ignore
/// let p = embassy_stm32::init(config);
/// let controls = sonido_daisy::hothouse_pins!(p);
/// // `p` still has SAI1, DMA, codec pins for AudioPeripherals
/// ```
#[macro_export]
macro_rules! hothouse_pins {
    ($p:ident) => {{
        use embassy_stm32::adc::{Adc, AdcChannel};
        use embassy_stm32::gpio::{Input, Level, Output, Pull, Speed};

        $crate::hothouse::HothouseControls {
            adc1: Adc::new($p.ADC1),
            knobs: [
                $p.PA3.degrade_adc(), // K1
                $p.PB1.degrade_adc(), // K2
                $p.PA7.degrade_adc(), // K3
                $p.PA6.degrade_adc(), // K4
                $p.PC1.degrade_adc(), // K5
                $p.PC4.degrade_adc(), // K6
            ],
            tog1_up: Input::new($p.PB4, Pull::Up),
            tog1_dn: Input::new($p.PB5, Pull::Up),
            tog2_up: Input::new($p.PG10, Pull::Up),
            tog2_dn: Input::new($p.PG11, Pull::Up),
            tog3_up: Input::new($p.PD2, Pull::Up),
            tog3_dn: Input::new($p.PC12, Pull::Up),
            foot1: Input::new($p.PA0, Pull::Up),
            foot2: Input::new($p.PD11, Pull::Up),
            led1: Output::new($p.PA5, Level::Low, Speed::Low),
            led2: Output::new($p.PA4, Level::Low, Speed::Low),
        }
    }};
}

// ── Embassy control task ──────────────────────────────────────────────────

/// Reads ADC knobs and GPIO at 50 Hz, writes smoothed values to a [`HothouseBuffer`].
///
/// All 6 knobs are read via uniform `blocking_read()` polling — ~48 µs total
/// per 20 ms cycle (0.24% CPU). This matches libDaisy's `AnalogControl` approach.
///
/// Drives LEDs from the buffer's LED values (written by the audio callback).
///
/// # Arguments
///
/// - `ctrl`: Hothouse control peripherals (created by `hothouse_pins!`).
/// - `buf`: Static reference to the shared [`HothouseBuffer`].
///
/// # Example
///
/// ```ignore
/// use sonido_daisy::controls::HothouseBuffer;
/// use sonido_daisy::hothouse::hothouse_control_task;
/// use static_cell::StaticCell;
///
/// static CONTROLS: HothouseBuffer = HothouseBuffer::new();
///
/// let ctrl = sonido_daisy::hothouse_pins!(p);
/// spawner.spawn(hothouse_control_task(ctrl, &CONTROLS)).unwrap();
/// ```
#[embassy_executor::task]
pub async fn hothouse_control_task(mut ctrl: HothouseControls, buf: &'static HothouseBuffer) {
    loop {
        embassy_time::Timer::after_millis(POLL_INTERVAL_MS).await;

        // ── Read all 6 knobs via uniform blocking_read (~48 µs total) ──
        for (i, knob) in ctrl.knobs.iter_mut().enumerate() {
            let raw = ctrl.adc1.blocking_read(knob, KNOB_SAMPLE_TIME);
            buf.write_knob_raw(i, raw, KNOB_ALPHA);
        }

        // ── Read toggles ──
        buf.write_toggle(0, decode_toggle(&ctrl.tog1_up, &ctrl.tog1_dn));
        buf.write_toggle(1, decode_toggle(&ctrl.tog2_up, &ctrl.tog2_dn));
        buf.write_toggle(2, decode_toggle(&ctrl.tog3_up, &ctrl.tog3_dn));

        // ── Read footswitches ──
        buf.write_footswitch(0, ctrl.foot1.is_low());
        buf.write_footswitch(1, ctrl.foot2.is_low());

        // ── Drive LEDs from buffer (audio callback writes, we drive GPIO) ──
        if buf.read_led(0) > 0.5 {
            ctrl.led1.set_high();
        } else {
            ctrl.led1.set_low();
        }
        if buf.read_led(1) > 0.5 {
            ctrl.led2.set_high();
        } else {
            ctrl.led2.set_low();
        }
    }
}

// ── PlatformController implementation ─────────────────────────────────────

/// Hothouse platform controller backed by a [`HothouseBuffer`].
///
/// Implements `PlatformController` for integration with `sonido-platform`'s
/// control mapping infrastructure.
///
/// Requires the `platform` feature.
///
/// # Control IDs
///
/// | Index | Type       | Control     |
/// |-------|------------|-------------|
/// | 0–5   | Knob       | Knobs 1–6   |
/// | 6–8   | Toggle3Way | Toggles 1–3 |
/// | 9–10  | Footswitch | FS 1–2      |
/// | 11–12 | Led        | LEDs 1–2    |
#[cfg(feature = "platform")]
pub struct HothousePlatform {
    buf: &'static HothouseBuffer,
}

#[cfg(feature = "platform")]
impl HothousePlatform {
    /// Creates a new platform controller wrapping a [`HothouseBuffer`].
    pub fn new(buf: &'static HothouseBuffer) -> Self {
        Self { buf }
    }
}

#[cfg(feature = "platform")]
impl sonido_platform::PlatformController for HothousePlatform {
    fn control_count(&self) -> usize {
        13 // 6 knobs + 3 toggles + 2 footswitches + 2 LEDs
    }

    fn control_id(&self, index: usize) -> Option<sonido_platform::ControlId> {
        if index < 13 {
            Some(sonido_platform::ControlId::hardware(index as u8))
        } else {
            None
        }
    }

    fn control_type(&self, id: sonido_platform::ControlId) -> Option<sonido_platform::ControlType> {
        match id.index() {
            0..=5 => Some(sonido_platform::ControlType::Knob),
            6..=8 => Some(sonido_platform::ControlType::Toggle3Way),
            9..=10 => Some(sonido_platform::ControlType::Footswitch),
            11..=12 => Some(sonido_platform::ControlType::Led),
            _ => None,
        }
    }

    fn read_control(
        &self,
        id: sonido_platform::ControlId,
    ) -> Option<sonido_platform::ControlState> {
        match id.index() {
            i @ 0..=5 => Some(sonido_platform::ControlState::new(
                self.buf.read_knob(i as usize),
            )),
            i @ 6..=8 => {
                let toggle_idx = (i - 6) as usize;
                let pos = self.buf.read_toggle(toggle_idx);
                // Normalize: 0=UP→0.0, 1=MID→0.5, 2=DN→1.0
                let val = match pos {
                    0 => 0.0,
                    2 => 1.0,
                    _ => 0.5,
                };
                Some(sonido_platform::ControlState::new(val))
            }
            i @ 9..=10 => {
                let fs_idx = (i - 9) as usize;
                let pressed = self.buf.read_footswitch(fs_idx);
                Some(sonido_platform::ControlState::new(if pressed {
                    1.0
                } else {
                    0.0
                }))
            }
            i @ 11..=12 => {
                let led_idx = (i - 11) as usize;
                Some(sonido_platform::ControlState::new(
                    self.buf.read_led(led_idx),
                ))
            }
            _ => None,
        }
    }

    fn write_control(&mut self, id: sonido_platform::ControlId, value: f32) -> bool {
        match id.index() {
            i @ 11..=12 => {
                let led_idx = (i - 11) as usize;
                self.buf.write_led(led_idx, value);
                true
            }
            _ => false, // Only LEDs are writable
        }
    }
}
