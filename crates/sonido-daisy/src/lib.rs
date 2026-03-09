//! Sonido DSP firmware for Electrosmith Daisy Seed.
//!
//! This crate provides the hardware integration layer between Sonido's `no_std`
//! DSP kernels and the Daisy Seed's STM32H750 Cortex-M7 processor. It includes
//! DWT cycle-counting utilities for kernel benchmarking and audio callback types
//! for DMA double-buffered processing.
//!
//! # Target Hardware
//!
//! - **MCU**: STM32H750IBK6 (ARM Cortex-M7, 480 MHz, single-precision FPU)
//! - **Audio**: 24-bit stereo codec via SAI + DMA, 48 kHz default
//! - **Memory**: 128 KB DTCM (0-wait), 512 KB AXI SRAM, 64 MB SDRAM
//!
//! # Boot Mode
//!
//! All examples use BOOT_SRAM mode: the Electrosmith bootloader copies
//! firmware from QSPI flash to AXI SRAM (`0x24000000`) on each boot.
//! Code executes from zero-wait-state SRAM, allowing Embassy to safely
//! reconfigure clocks without disrupting QSPI memory-mapped mode.
//!
//! # Usage Tiers
//!
//! - **Tier 1**: Blinky (validate toolchain + flash)
//! - **Tier 2**: Kernel benchmarks (DWT cycle counts for all 19 effects)
//! - **Tier 3**: Audio passthrough (validate codec + DMA)
//! - **Tier 4**: Single effect processing (first real DSP on hardware)

#![no_std]

use cortex_m::peripheral::DWT;
use embassy_time::Timer;

/// Pulses the Daisy Seed user LED (PC7) with a double-blink heartbeat pattern.
///
/// The pattern mimics a cardiac lub-dub: two short flashes close together,
/// then a longer rest — one cycle per second. Every firmware binary spawns
/// this task before starting the audio loop so the LED confirms the firmware
/// is running regardless of whether audio initialises successfully.
///
/// Timing (total cycle ≈ 1 s):
/// - on 80 ms → off 80 ms → on 80 ms → off 760 ms
///
/// # Example
///
/// ```ignore
/// use sonido_daisy::heartbeat;
///
/// let led = board.user_led;
/// spawner.spawn(heartbeat(led)).unwrap();
/// ```
#[embassy_executor::task]
pub async fn heartbeat(mut led: daisy_embassy::led::UserLed<'static>) {
    loop {
        // lub
        led.on();
        Timer::after_millis(80).await;
        led.off();
        Timer::after_millis(80).await;
        // dub
        led.on();
        Timer::after_millis(80).await;
        led.off();
        // rest — total cycle ~1 s
        Timer::after_millis(760).await;
    }
}

/// Default audio sample rate in Hz.
pub const SAMPLE_RATE: f32 = 48_000.0;

/// Default block size in stereo sample pairs.
///
/// Daisy-embassy hardcodes `BLOCK_LENGTH = 32` in its SAI DMA driver.
/// The DMA callback receives `&[u32]` of length 64 (32 pairs × 2 channels,
/// interleaved `[L0, R0, L1, R1, ...]`). This constant must match that value.
pub const BLOCK_SIZE: usize = 32;

/// Number of audio channels (stereo).
pub const CHANNELS: usize = 2;

/// DMA buffer size: block_size * channels * 2 (double-buffer).
pub const DMA_BUFFER_SIZE: usize = BLOCK_SIZE * CHANNELS * 2;

/// CPU clock frequency in Hz (STM32H750 max).
pub const CPU_CLOCK_HZ: u32 = 480_000_000;

/// Available CPU cycles per audio block at 48 kHz.
///
/// At 480 MHz and 48 kHz with 32-sample blocks:
/// 480_000_000 / (48_000 / 32) = 320_000 cycles per block.
pub const CYCLES_PER_BLOCK: u32 = CPU_CLOCK_HZ / (SAMPLE_RATE as u32 / BLOCK_SIZE as u32);

/// Measures the number of CPU cycles consumed by a closure using the DWT cycle counter.
///
/// # Prerequisites
///
/// The DWT cycle counter must be enabled before calling this function.
/// Use [`enable_cycle_counter`] at startup.
///
/// # Example
///
/// ```ignore
/// enable_cycle_counter(&mut cp.DCB, &mut cp.DWT);
/// let cycles = measure_cycles(|| {
///     kernel.process_stereo(0.5, 0.5, &params);
/// });
/// defmt::info!("Kernel used {} cycles", cycles);
/// ```
#[inline]
pub fn measure_cycles<F: FnOnce()>(f: F) -> u32 {
    let start = DWT::cycle_count();
    f();
    let end = DWT::cycle_count();
    end.wrapping_sub(start)
}

/// Enables the DWT cycle counter.
///
/// Must be called once at startup before using [`measure_cycles`].
/// Takes mutable references to the DCB and DWT peripherals from `cortex_m::Peripherals`.
///
/// # Example
///
/// ```ignore
/// let mut cp = cortex_m::Peripherals::take().unwrap();
/// enable_cycle_counter(&mut cp.DCB, &mut cp.DWT);
/// ```
pub fn enable_cycle_counter(dcb: &mut cortex_m::peripheral::DCB, dwt: &mut DWT) {
    dcb.enable_trace();
    dwt.enable_cycle_counter();
}

/// Converts a u32 sample (24-bit signed, left-justified in 32 bits) to f32 [-1.0, 1.0].
///
/// The PCM3060 codec outputs 24-bit signed samples packed into 32-bit words.
/// The value is treated as a signed 32-bit integer and divided by 2^31.
#[inline]
pub fn u24_to_f32(sample: u32) -> f32 {
    (sample as i32) as f32 / 2_147_483_648.0
}

/// Converts an f32 sample [-1.0, 1.0] to u32 (24-bit signed, left-justified in 32 bits).
///
/// Inverse of [`u24_to_f32`]. Output is suitable for the PCM3060 codec DAC input.
#[inline]
pub fn f32_to_u24(sample: f32) -> u32 {
    (sample * 2_147_483_648.0) as i32 as u32
}
