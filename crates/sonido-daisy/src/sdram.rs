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
//! sdram::enable_dcache(); // only if no audio DMA running
//! ```

use cortex_m::peripheral::{MPU, SCB};

/// Re-export the FMC device definition for the Daisy Seed's SDRAM chip.
pub use stm32_fmc::devices::as4c16m32msa_6::As4c16m32msa as SdramDevice;

/// SDRAM capacity in bytes: 64 MB.
pub const SDRAM_SIZE: usize = 64 * 1024 * 1024;

/// SDRAM base address (FMC SDRAM Bank 1).
pub const SDRAM_BASE: usize = 0xC000_0000;

/// Configures the MPU for cacheable SDRAM access and enables I-cache.
///
/// Sets two MPU regions:
/// - **Region 0**: SDRAM at [`SDRAM_BASE`] (0xC000_0000), 64 MB, cacheable write-back
/// - **Region 1**: D2 SRAM at 0x3000_0000, 512 KB, non-cacheable (protects DMA buffers)
///
/// Enables the Cortex-M7 I-cache (~3x instruction fetch speedup).
///
/// **D-cache is NOT enabled here.** Call [`enable_dcache`] separately, and only
/// after DMA transfers are running. Enabling D-cache during SAI DMA initialization
/// causes overrun errors — the cache enable sequence briefly stalls the bus matrix,
/// which starves the DMA controller during its critical setup window.
///
/// # Note
///
/// `daisy-embassy` set the MPU base to `0xD000_0000` (SDRAM Bank 2),
/// which didn't cover the actual SDRAM at `0xC000_0000` (Bank 1).
/// This meant SDRAM accesses fell through to the default memory map
/// (Device type, non-cacheable) — functional but slow. Fixed here.
///
/// Called by the [`init_sdram!`](crate::init_sdram) macro. Not typically called directly.
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

    // ── Region 0: SDRAM (64 MB, cacheable write-back) ──
    // Delay lines, reverb buffers, heap allocations — CPU-only access,
    // cache-friendly sequential reads (~87.5% hit rate).
    const SDRAM_SIZE_BITS: u32 = 25; // log2(64 MB) - 1

    unsafe {
        mpu.rnr.write(0);
        mpu.rbar.write(SDRAM_BASE as u32);
        mpu.rasr.write(
            (REGION_FULL_ACCESS << 24)
                | (REGION_CACHEABLE << 17)
                | (REGION_WRITE_BACK << 16)
                | (SDRAM_SIZE_BITS << 1)
                | REGION_ENABLE,
        );
    }

    // ── Region 1: D2 SRAM (512 KB, non-cacheable) ──
    // DMA buffers (.sram1_bss at 0x30000000) live here. DMA writes bypass
    // the CPU cache, so D-cache must NOT cache this region — otherwise the
    // CPU reads stale data and DMA sends stale data (cache coherency bug).
    //
    // Without this region, 0x30000000 falls in the ARM default SRAM region
    // (0x20000000–0x3FFFFFFF) = Normal Write-Back Write-Allocate = CACHEABLE.
    // That breaks SAI DMA audio: stale reads → corrupted audio or SAI errors.
    //
    // TEX=001, C=0, B=0 = Normal, non-cacheable (ARMv7-M B3.5.1 Table B3-10).
    // Size covers all D2 SRAM (SRAM1 128K + SRAM2 128K + SRAM3 32K = 288K,
    // rounded up to 512K power-of-2 for MPU).
    const D2_SRAM_BASE: u32 = 0x3000_0000;
    const D2_SRAM_SIZE_BITS: u32 = 18; // 2^(18+1) = 512 KB ≥ 288 KB actual
    const TEX_NORMAL_NON_CACHEABLE: u32 = 0x01 << 19; // TEX=001, C=0, B=0

    unsafe {
        mpu.rnr.write(1);
        mpu.rbar.write(D2_SRAM_BASE);
        mpu.rasr.write(
            (REGION_FULL_ACCESS << 24)
                | TEX_NORMAL_NON_CACHEABLE
                | (D2_SRAM_SIZE_BITS << 1)
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

    // I-cache: ~3x speedup on instruction fetches from flash/AXI SRAM.
    // Safe to enable at any time — instruction fetches are read-only.
    scb.enable_icache();
    // D-cache: caller must enable separately via enable_dcache() AFTER DMA is running.
}

/// Enables the Cortex-M7 L1 D-cache (~3–5x speedup on SDRAM reads).
///
/// **Must be called AFTER SAI DMA is running**, not during initialization.
/// Enabling D-cache during DMA setup causes bus matrix stalls that starve
/// the DMA controller, resulting in SAI overrun errors.
///
/// The MPU (configured by [`configure_mpu`]) marks D2 SRAM as non-cacheable,
/// so D-cache will not interfere with DMA buffer coherency once enabled.
///
/// For firmware with audio: spawn a deferred task that calls this after
/// `start_callback()` has started DMA transfers.
///
/// For firmware without audio (e.g., benchmarks): safe to call immediately
/// after [`init_sdram!`](crate::init_sdram).
///
/// # Example
///
/// ```ignore
/// // Deferred enable (with audio — spawned before audio setup):
/// #[embassy_executor::task]
/// async fn deferred_dcache() {
///     embassy_time::Timer::after_millis(500).await;
///     sonido_daisy::sdram::enable_dcache();
/// }
///
/// // Immediate enable (no audio):
/// sonido_daisy::sdram::enable_dcache();
/// ```
pub fn enable_dcache() {
    unsafe {
        let mut cp = cortex_m::Peripherals::steal();
        cp.SCB.enable_dcache(&mut cp.CPUID);
    }
}

/// Initializes the Daisy Seed's 64 MB external SDRAM.
///
/// Configures the MPU (+ I-cache), sets up all 54 FMC GPIO pins,
/// and runs the SDRAM power-up sequence (clock enable → 200 µs delay →
/// precharge all → auto-refresh × 8 → load mode register).
///
/// **D-cache is NOT enabled.** Call [`enable_dcache()`](crate::sdram::enable_dcache) separately:
/// - Immediately after this macro if there is no audio DMA.
/// - After audio DMA is running (deferred task) if using SAI.
///
/// Returns `*mut u32` pointing to the SDRAM base at `0xC000_0000`.
/// Pass this to the heap allocator:
///
/// ```ignore
/// let ptr = init_sdram!(p, &mut cp.MPU, &mut cp.SCB);
/// unsafe { HEAP.init(ptr as usize, sdram::SDRAM_SIZE); }
/// sonido_daisy::sdram::enable_dcache(); // only if no audio DMA
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
