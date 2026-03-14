//! Sonido DSP firmware for Electrosmith Daisy Seed.
//!
//! This crate provides the hardware integration layer between Sonido's `no_std`
//! DSP kernels and the Daisy Seed's STM32H750 Cortex-M7 processor. It owns the
//! full platform layer: clock configuration, audio codec, ADC, and LED — no
//! external BSP dependency.
//!
//! # Target Hardware
//!
//! - **MCU**: STM32H750IBK6 (ARM Cortex-M7, 480 MHz, single-precision FPU)
//! - **Audio**: PCM3060 codec (seed 1.2), 24-bit stereo via SAI + DMA, 48 kHz default
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

pub mod adc;
pub mod audio;
pub mod controls;
pub mod hothouse;
pub mod led;
pub mod rcc;
pub mod sdram;

/// Zero-smoothing DspKernel wrapper — requires `alloc` feature.
#[cfg(feature = "alloc")]
pub mod embedded_adapter;
/// Scale-aware ADC→parameter conversion — requires `alloc` feature.
#[cfg(feature = "alloc")]
pub mod param_map;

pub use controls::ControlBuffer;
#[cfg(feature = "alloc")]
pub use embedded_adapter::EmbeddedAdapter;
#[cfg(feature = "alloc")]
pub use param_map::adc_to_param;
pub use rcc::{ClockProfile, cycles_per_block, rcc_config};

use cortex_m::peripheral::DWT;

/// Enables D2-domain SRAM1/2/3 clocks via RCC AHB2ENR.
///
/// On STM32H750, D2 SRAM clocks are **disabled after reset**. Any access
/// to addresses in the 0x3000_0000–0x3004_7FFF range (D2 SRAM1/2/3) before
/// enabling these clocks causes a bus fault → HardFault.
///
/// Call this **before** initialising the heap or touching DMA buffers.
/// It is safe to call multiple times (idempotent OR-write).
pub fn enable_d2_sram() {
    // RCC AHB2ENR register: 0x5802_44DC
    // Bit 29 = SRAM1EN, Bit 30 = SRAM2EN, Bit 31 = SRAM3EN
    const RCC_AHB2ENR: *mut u32 = 0x5802_44DC as *mut u32;
    unsafe {
        let val = core::ptr::read_volatile(RCC_AHB2ENR);
        core::ptr::write_volatile(RCC_AHB2ENR, val | (0b111 << 29));
    }
}
use embassy_stm32::{peripherals, usb};
use embassy_time::Timer;
use embassy_usb::UsbDevice;

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
/// use sonido_daisy::{heartbeat, led::UserLed};
///
/// let led = UserLed::new(p.PC7);
/// spawner.spawn(heartbeat(led)).unwrap();
/// ```
#[embassy_executor::task]
pub async fn heartbeat(mut led: led::UserLed<'static>) {
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

/// Drives the USB device state machine.
///
/// Spawn once before any USB writes. Runs until power-off.
/// Import this instead of defining a local `usb_task` in each example.
///
/// # Example
///
/// ```ignore
/// use sonido_daisy::usb_task;
/// spawner.spawn(usb_task(usb)).unwrap();
/// ```
#[embassy_executor::task]
pub async fn usb_task(
    mut device: UsbDevice<'static, usb::Driver<'static, peripherals::USB_OTG_FS>>,
) -> ! {
    device.run().await
}

/// A `fmt::Write` adapter over a fixed byte slice.
///
/// Writes UTF-8 bytes into `buf` starting at `pos`, truncating silently
/// when the buffer is full. Never returns `Err`. Suitable for `write!`
/// in interrupt and DMA contexts where allocation is forbidden.
///
/// # Example
///
/// ```ignore
/// use sonido_daisy::BufWriter;
/// use core::fmt::Write;
///
/// let mut buf = [0u8; 256];
/// let mut w = BufWriter::new(&mut buf);
/// write!(w, "cycles={}", 42).ok();
/// let written = w.pos;
/// ```
pub struct BufWriter<'a> {
    /// Destination buffer.
    pub buf: &'a mut [u8],
    /// Current write position.
    pub pos: usize,
}

impl<'a> BufWriter<'a> {
    /// Creates a new writer starting at position 0.
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }
}

impl core::fmt::Write for BufWriter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let n = bytes.len().min(self.buf.len() - self.pos);
        self.buf[self.pos..self.pos + n].copy_from_slice(&bytes[..n]);
        self.pos += n;
        Ok(())
    }
}

/// Default audio sample rate in Hz.
pub const SAMPLE_RATE: f32 = 48_000.0;

/// Default block size in stereo sample pairs.
///
/// The SAI DMA driver uses `BLOCK_LENGTH = 32` samples. The DMA callback
/// receives `&[u32]` of length 64 (32 pairs × 2 channels, interleaved
/// `[L0, R0, L1, R1, ...]`). This constant must match that value.
pub const BLOCK_SIZE: usize = 32;

/// Number of audio channels (stereo).
pub const CHANNELS: usize = 2;

/// DMA buffer size: block_size × channels × 2 (double-buffer).
pub const DMA_BUFFER_SIZE: usize = BLOCK_SIZE * CHANNELS * 2;

/// CPU clock frequency in Hz at Performance profile (480 MHz).
pub const CPU_CLOCK_HZ: u32 = 480_000_000;

/// Available CPU cycles per audio block at Performance profile.
///
/// At 480 MHz and 48 kHz with 32-sample blocks:
/// 480_000_000 / (48_000 / 32) = 320_000 cycles per block.
///
/// For profile-aware budgets, use [`cycles_per_block`].
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

/// Converts a u32 SAI sample (24-bit signed, right-aligned) to f32 in [-1.0, 1.0].
///
/// With `DataSize::Data24`, the STM32H7 SAI places 24-bit audio data in
/// bits [23:0] of the 32-bit data register. The sign bit is bit 23, not
/// bit 31. This function sign-extends from 24-bit to 32-bit, then scales
/// by 2^23 — matching libDaisy's `s242f()`.
///
/// Reference: libDaisy `daisy_core.h` — `S242F_SCALE = 1/(2^23)`.
#[inline]
pub fn u24_to_f32(sample: u32) -> f32 {
    // Sign-extend 24-bit → 32-bit: shift left 8 to put sign in bit 31,
    // then arithmetic shift right 8 to propagate sign.
    let signed = ((sample as i32) << 8) >> 8;
    signed as f32 / 8_388_608.0 // ÷ 2^23
}

/// Converts an f32 sample in [-1.0, 1.0] to u32 (24-bit signed, right-aligned).
///
/// Inverse of [`u24_to_f32`]. Clamps to [-1.0, 1.0] before conversion to
/// prevent wrap-around distortion. Matches libDaisy's `f2s24()`.
///
/// Reference: libDaisy `daisy_core.h` — `F2S24_SCALE = 2^23`.
#[inline]
pub fn f32_to_u24(sample: f32) -> u32 {
    let clamped = sample.clamp(-1.0, 1.0);
    (clamped * 8_388_608.0) as i32 as u32
}

/// Enables FPU flush-to-zero and default-NaN modes.
///
/// Best practice for real-time DSP on Cortex-M7 — prevents denormal
/// slowdowns that cause timing jitter in the audio callback.
///
/// - **FZ** (bit 24): Flush denormals to zero on output
/// - **DN** (bit 25): Default NaN mode — any NaN operation returns canonical NaN
///
/// Call once in `main()` before starting the audio interface.
///
/// # Safety
///
/// Modifies the FPSCR register via inline assembly. Safe on all Cortex-M4F/M7
/// targets with hardware FPU.
#[inline]
pub fn enable_fpu_ftz() {
    unsafe {
        let mut fpscr: u32;
        core::arch::asm!("vmrs {}, fpscr", out(reg) fpscr);
        fpscr |= (1 << 24) | (1 << 25); // FZ + DN bits
        core::arch::asm!("vmsr fpscr, {}", in(reg) fpscr);
    }
}

/// Convenience macro to construct [`audio::CodecPins`] from Embassy peripherals.
///
/// For the Daisy Seed 1.2 (PCM3060 codec, `seed_1_2` feature):
///
/// ```ignore
/// let pins = sonido_daisy::codec_pins!(p);
/// ```
#[macro_export]
macro_rules! codec_pins {
    ($p:ident) => {
        $crate::audio::CodecPins {
            MCLK_A: $p.PE2,
            SCK_A: $p.PE5,
            FS_A: $p.PE4,
            SD_A: $p.PE6,
            SD_B: $p.PE3,
        }
    };
}
