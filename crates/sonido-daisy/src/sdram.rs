//! SDRAM initialization for the Daisy Seed's 64 MB external memory.
//!
//! The Daisy Seed has an Alliance Memory AS4C16M32MSA-6 (64 MB, 32-bit)
//! connected via the STM32H750's FMC controller. This module configures
//! the MPU for cacheable access and runs the SDRAM power-up sequence.
//!
//! # Memory Architecture
//!
//! The Daisy Seed's memory map has a clear hot/cold hierarchy:
//!
//! | Region | Size | Latency | Best For |
//! |--------|------|---------|----------|
//! | DTCM | 128 KB | 0-wait | Stack, per-sample DSP state |
//! | AXI SRAM | 480 KB | 0-wait | Code execution (BOOT_SRAM) |
//! | D2 SRAM | 288 KB | 1–2 cycles | DMA buffers (SAI audio I/O) |
//! | **SDRAM** | **64 MB** | **4–8 cycles** | **Heap: delay lines, reverb, loopers** |
//!
//! The hot path (per-sample DSP) runs from DTCM stack and cached SDRAM.
//! The cold path (initialization, parameter updates) touches SDRAM uncached
//! during allocation, which is fine — `new()` runs once.
//!
//! Delay line access patterns (1 read + 1 write per sample, sequential)
//! are cache-friendly: a 32-byte cache line holds 8 `f32` samples, so
//! sequential reads have ~87.5% hit rate after the first miss.
//!
//! # Usage
//!
//! ```ignore
//! use sonido_daisy::{init_sdram, sdram};
//!
//! let config = sonido_daisy::rcc_config(ClockProfile::Performance);
//! let p = embassy_stm32::init(config);
//! let mut cp = unsafe { cortex_m::Peripherals::steal() };
//!
//! let sdram_ptr = init_sdram!(p, &mut cp.MPU, &mut cp.SCB);
//! unsafe { HEAP.init(sdram_ptr as usize, sdram::SDRAM_SIZE); }
//! ```

use cortex_m::peripheral::{MPU, SCB};

/// Re-export the FMC device definition for the Daisy Seed's SDRAM chip.
pub use stm32_fmc::devices::as4c16m32msa_6::As4c16m32msa as SdramDevice;

/// SDRAM capacity in bytes: 64 MB.
pub const SDRAM_SIZE: usize = 64 * 1024 * 1024;

/// SDRAM base address (FMC SDRAM Bank 1).
pub const SDRAM_BASE: usize = 0xC000_0000;

/// Configures the MPU for cacheable SDRAM access.
///
/// Sets MPU Region 0 at [`SDRAM_BASE`] (0xC000_0000) with:
/// - Full read/write access
/// - Cacheable, write-back (reads cached, writes propagate)
/// - 64 MB size
///
/// This enables the Cortex-M7 L1 data cache to accelerate SDRAM access
/// from 4–8 wait states to ~1 cycle for cache hits — critical for
/// delay line read performance.
///
/// # Note
///
/// `daisy-embassy` set the MPU base to `0xD000_0000` (SDRAM Bank 2),
/// which didn't cover the actual SDRAM at `0xC000_0000` (Bank 1).
/// This meant SDRAM accesses fell through to the default memory map
/// (Device type, non-cacheable) — functional but slow. Fixed here.
///
/// Called by the [`init_sdram!`] macro. Not typically called directly.
pub fn configure_mpu(mpu: &mut MPU, scb: &mut SCB) {
    // ARM®v7-M Architecture Reference Manual, Section B3.5
    const MEMFAULTENA: u32 = 1 << 16;

    unsafe {
        // Ensure outstanding transfers complete before MPU changes
        cortex_m::asm::dmb();
        scb.shcsr.modify(|r| r & !MEMFAULTENA);
        mpu.ctrl.write(0);
    }

    const REGION_FULL_ACCESS: u32 = 0x03;
    const REGION_CACHEABLE: u32 = 0x01;
    const REGION_WRITE_BACK: u32 = 0x01;
    const REGION_ENABLE: u32 = 0x01;

    // log2(64 MB) - 1 = 25
    const SIZE_BITS: u32 = 25;

    unsafe {
        mpu.rnr.write(0); // Region 0
        mpu.rbar.write(SDRAM_BASE as u32);
        mpu.rasr.write(
            (REGION_FULL_ACCESS << 24)
                | (REGION_CACHEABLE << 17)
                | (REGION_WRITE_BACK << 16)
                | (SIZE_BITS << 1)
                | REGION_ENABLE,
        );
    }

    const MPU_ENABLE: u32 = 0x01;
    const MPU_DEFAULT_MMAP_FOR_PRIVILEGED: u32 = 0x04;

    unsafe {
        mpu.ctrl
            .modify(|r| r | MPU_DEFAULT_MMAP_FOR_PRIVILEGED | MPU_ENABLE);
        scb.shcsr.modify(|r| r | MEMFAULTENA);
        cortex_m::asm::dsb();
        cortex_m::asm::isb();
    }
}

/// Initializes the Daisy Seed's 64 MB external SDRAM.
///
/// Configures the MPU for cacheable access, sets up all 54 FMC GPIO pins,
/// and runs the SDRAM power-up sequence (clock enable → 200 µs delay →
/// precharge all → auto-refresh × 8 → load mode register).
///
/// Returns `*mut u32` pointing to the SDRAM base at `0xC000_0000`.
/// Pass this to the heap allocator:
///
/// ```ignore
/// let ptr = init_sdram!(p, &mut cp.MPU, &mut cp.SCB);
/// unsafe { HEAP.init(ptr as usize, sdram::SDRAM_SIZE); }
/// ```
///
/// # Pin Consumption
///
/// This macro consumes 54 GPIO pins from the embassy peripheral struct.
/// These are all internal to the Daisy Seed module (connecting STM32 to
/// the SDRAM chip on the PCB) — they do NOT conflict with user-accessible
/// header pins or the SAI codec pins (PE2–PE6).
///
/// # Prerequisites
///
/// - `embassy_stm32::init()` must have been called first (enables FMC RCC clock)
/// - PLL2_R must provide the FMC clock (configured by [`rcc_config`](crate::rcc_config))
#[macro_export]
macro_rules! init_sdram {
    ($p:ident, $mpu:expr, $scb:expr) => {{
        $crate::sdram::configure_mpu($mpu, $scb);

        let mut sdram = embassy_stm32::fmc::Fmc::sdram_a13bits_d32bits_4banks_bank1(
            $p.FMC,
            // Address A0–A12
            $p.PF0,
            $p.PF1,
            $p.PF2,
            $p.PF3,
            $p.PF4,
            $p.PF5,
            $p.PF12,
            $p.PF13,
            $p.PF14,
            $p.PF15,
            $p.PG0,
            $p.PG1,
            $p.PG2,
            // Bank address BA0–BA1
            $p.PG4,
            $p.PG5,
            // Data D0–D31
            $p.PD14,
            $p.PD15,
            $p.PD0,
            $p.PD1,
            $p.PE7,
            $p.PE8,
            $p.PE9,
            $p.PE10,
            $p.PE11,
            $p.PE12,
            $p.PE13,
            $p.PE14,
            $p.PE15,
            $p.PD8,
            $p.PD9,
            $p.PD10,
            $p.PH8,
            $p.PH9,
            $p.PH10,
            $p.PH11,
            $p.PH12,
            $p.PH13,
            $p.PH14,
            $p.PH15,
            $p.PI0,
            $p.PI1,
            $p.PI2,
            $p.PI3,
            $p.PI6,
            $p.PI7,
            $p.PI9,
            $p.PI10,
            // Byte enables NBL0–NBL3
            $p.PE0,
            $p.PE1,
            $p.PI4,
            $p.PI5,
            // Control signals
            $p.PH2,  // SDCKE0
            $p.PG8,  // SDCLK
            $p.PG15, // SDNCAS
            $p.PH3,  // SDNE0
            $p.PF11, // SDNRAS
            $p.PH5,  // SDNWE
            $crate::sdram::SdramDevice {},
        );

        let mut delay = embassy_time::Delay;
        sdram.init(&mut delay)
    }};
}
