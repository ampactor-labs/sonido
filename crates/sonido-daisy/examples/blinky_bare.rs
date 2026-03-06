//! Bare-metal blinky — no clock reinit, no Embassy runtime.
//!
//! Validates that QSPI XIP works with the Electrosmith bootloader.
//! Toggles PC7 (onboard LED) using only raw register writes and
//! a busy-wait delay, avoiding any clock reconfiguration that could
//! disrupt QSPI memory-mapped mode.

#![no_std]
#![no_main]

use cortex_m_rt::entry;
use defmt_rtt as _; // defmt transport (required by embassy-stm32)
use embassy_stm32 as _; // link interrupt vectors
use panic_probe as _;

#[entry]
fn main() -> ! {
    // Enable GPIOC clock (RCC AHB4ENR bit 2)
    const RCC_BASE: u32 = 0x5802_4400;
    const RCC_AHB4ENR: *mut u32 = (RCC_BASE + 0xE0) as *mut u32;
    unsafe {
        let val = core::ptr::read_volatile(RCC_AHB4ENR);
        core::ptr::write_volatile(RCC_AHB4ENR, val | (1 << 2)); // GPIOCEN
    }

    // Configure PC7 as output (MODER bits [15:14] = 01)
    const GPIOC_BASE: u32 = 0x5802_0800;
    const GPIOC_MODER: *mut u32 = GPIOC_BASE as *mut u32;
    const GPIOC_BSRR: *mut u32 = (GPIOC_BASE + 0x18) as *mut u32;
    unsafe {
        let val = core::ptr::read_volatile(GPIOC_MODER);
        let val = val & !(0b11 << 14); // clear bits 15:14
        let val = val | (0b01 << 14); // set output mode
        core::ptr::write_volatile(GPIOC_MODER, val);
    }

    // Blink forever — cortex_m::asm::delay() is an intrinsic that
    // the compiler cannot optimize out, unlike a nop loop.
    // At the bootloader's default ~64 MHz, 20M cycles ≈ 300ms.
    loop {
        // Set PC7 high
        unsafe { core::ptr::write_volatile(GPIOC_BSRR, 1 << 7) };
        cortex_m::asm::delay(20_000_000);

        // Set PC7 low (reset = bit 7 + 16)
        unsafe { core::ptr::write_volatile(GPIOC_BSRR, 1 << (7 + 16)) };
        cortex_m::asm::delay(20_000_000);
    }
}
