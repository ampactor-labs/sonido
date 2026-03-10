//! Clock configuration for the Daisy Seed STM32H750.
//!
//! Provides two clock profiles:
//! - [`ClockProfile::Performance`]: 480 MHz, VOS0 — maximum DSP throughput
//! - [`ClockProfile::Efficient`]: 400 MHz, VOS1 — reduced thermals (~20 °C cooler)
//!
//! Both profiles fix the ADC clock: `PLL2_P = Some(DIV2)` provides the 100 MHz
//! ADC kernel clock that `daisy-embassy`'s `default_rcc()` accidentally left disabled
//! (`divp: None`). Without this, `Adc::new()` hangs forever in calibration because
//! the ADC clock mux defaults to `PLL2_P` at reset.
//!
//! # PLL Summary
//!
//! | PLL   | VCO      | Output | Freq      | Used for                     |
//! |-------|----------|--------|-----------|------------------------------|
//! | PLL1  | 960 MHz¹ | P      | 480 MHz¹  | SYSCLK                       |
//! | PLL1  | 960 MHz¹ | Q      | 48 MHz¹   | USB (Performance only)       |
//! | PLL2  | 200 MHz  | P      | 100 MHz   | ADC clock (both profiles)    |
//! | PLL2  | 200 MHz  | R      | 100 MHz   | FMC / SDRAM (both profiles)  |
//! | PLL3  | ~787 MHz | P      | ~49.2 MHz | SAI MCLK (audio, both)       |
//! | HSI48 | —        | —      | 48 MHz    | USB (Efficient only)         |
//!
//! ¹ Efficient profile: VCO=800 MHz, PLL1_P=400 MHz, PLL1_Q disabled.

use embassy_stm32 as hal;
use hal::rcc::*;

/// Clock speed profile for the STM32H750.
///
/// Select `Performance` for maximum DSP throughput (480 MHz, VOS0), or
/// `Efficient` for cooler operation when thermal budget is a concern
/// (400 MHz, VOS1 — bare PCB, no heatsink).
///
/// Both profiles produce identical audio output (same PLL3 for SAI)
/// and identical ADC clock (same PLL2_P = 100 MHz).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockProfile {
    /// 480 MHz SYSCLK, VOS0. USB from PLL1_Q (48 MHz).
    Performance,

    /// 400 MHz SYSCLK, VOS1. USB from HSI48 (48 MHz).
    ///
    /// PLL1_Q cannot produce 48 MHz from the 800 MHz VCO, so USB
    /// uses the independent HSI48 oscillator instead.
    Efficient,
}

/// Returns an [`embassy_stm32::Config`] for the given clock profile.
///
/// Both profiles fix the ADC clock that `daisy-embassy`'s `default_rcc()`
/// left broken: `PLL2_P = Some(DIV2)` enables the 100 MHz ADC kernel clock.
/// Without this, `Adc::new()` hangs in calibration because the ADC clock mux
/// defaults to `PLL2_P` which has no clock when `divp = None`.
pub fn rcc_config(profile: ClockProfile) -> hal::Config {
    let mut config = hal::Config::default();

    // HSE: 16 MHz crystal oscillator on the Daisy Seed.
    config.rcc.hse = Some(Hse {
        freq: hal::time::Hertz::mhz(16),
        mode: HseMode::Oscillator,
    });

    // PLL3: SAI audio clock — identical for both profiles.
    // HSE(16) / 6 × 295 = ~786.67 MHz VCO
    // PLL3_P = VCO / 16 ≈ 49.17 MHz → SAI kernel clock
    // At 49.17 MHz, MCLK divider ≈ 4 → 48 kHz × 256 = 12.288 MHz MCLK.
    config.rcc.pll3 = Some(Pll {
        source: PllSource::HSE,
        prediv: PllPreDiv::DIV6,
        mul: PllMul::MUL295,
        divp: Some(PllDiv::DIV16),
        divq: Some(PllDiv::DIV4),
        divr: Some(PllDiv::DIV32),
    });
    config.rcc.mux.sai1sel = hal::rcc::mux::Saisel::PLL3_P;

    // PLL2: ADC + FMC clock — identical for both profiles.
    // HSE(16) / 4 × 50 = 200 MHz VCO
    // PLL2_P = 200 / 2 = 100 MHz (ADC kernel clock)
    // PLL2_R = 200 / 2 = 100 MHz (FMC / SDRAM)
    config.rcc.pll2 = Some(Pll {
        source: PllSource::HSE,
        prediv: PllPreDiv::DIV4,
        mul: PllMul::MUL50,
        divp: Some(PllDiv::DIV2), // ← KEY FIX: was None in daisy-embassy
        divq: None,
        divr: Some(PllDiv::DIV2),
    });
    config.rcc.mux.fmcsel = hal::rcc::mux::Fmcsel::PLL2_R;

    // Bus prescalers — identical for both profiles.
    // AHB = SYSCLK/2, APBx = AHB/2.
    config.rcc.ahb_pre = AHBPrescaler::DIV2;
    config.rcc.apb1_pre = APBPrescaler::DIV2;
    config.rcc.apb2_pre = APBPrescaler::DIV2;
    config.rcc.apb3_pre = APBPrescaler::DIV2;
    config.rcc.apb4_pre = APBPrescaler::DIV2;

    match profile {
        ClockProfile::Performance => {
            // PLL1: HSE(16) / 4 × 240 = 960 MHz VCO
            // PLL1_P = 960 / 2 = 480 MHz (SYSCLK)
            // PLL1_Q = 960 / 20 = 48 MHz (USB)
            config.rcc.pll1 = Some(Pll {
                source: PllSource::HSE,
                prediv: PllPreDiv::DIV4,
                mul: PllMul::MUL240,
                divp: Some(PllDiv::DIV2),
                divq: Some(PllDiv::DIV20),
                divr: Some(PllDiv::DIV2),
            });
            config.rcc.sys = Sysclk::PLL1_P;
            config.rcc.mux.usbsel = hal::rcc::mux::Usbsel::PLL1_Q;
            config.rcc.voltage_scale = VoltageScale::Scale0;
        }
        ClockProfile::Efficient => {
            // PLL1: HSE(16) / 4 × 200 = 800 MHz VCO
            // PLL1_P = 800 / 2 = 400 MHz (SYSCLK)
            // PLL1_Q = 800 / 20 = 40 MHz — NOT 48, can't use for USB.
            config.rcc.pll1 = Some(Pll {
                source: PllSource::HSE,
                prediv: PllPreDiv::DIV4,
                mul: PllMul::MUL200,
                divp: Some(PllDiv::DIV2),
                divq: None, // Not usable for USB at this VCO frequency
                divr: None,
            });
            config.rcc.sys = Sysclk::PLL1_P;
            // USB from HSI48 (independent 48 MHz oscillator).
            config.rcc.hsi48 = Some(Default::default());
            config.rcc.mux.usbsel = hal::rcc::mux::Usbsel::HSI48;
            config.rcc.voltage_scale = VoltageScale::Scale1;
        }
    }

    config
}

/// CPU clock frequency in Hz for the given profile.
pub const fn cpu_clock_hz(profile: ClockProfile) -> u32 {
    match profile {
        ClockProfile::Performance => 480_000_000,
        ClockProfile::Efficient => 400_000_000,
    }
}

/// Available CPU cycles per audio block for the given profile.
///
/// At 48 kHz with 32-sample blocks, the block rate is 1500 Hz.
///
/// - Performance (480 MHz): 320,000 cycles/block
/// - Efficient (400 MHz): 266,666 cycles/block
pub const fn cycles_per_block(profile: ClockProfile) -> u32 {
    cpu_clock_hz(profile) / 1500
}
