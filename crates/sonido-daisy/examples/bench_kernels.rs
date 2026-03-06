//! Tier 2: DWT cycle-count benchmarks for all 19 Sonido DSP kernels.
//!
//! Runs each kernel through a 128-sample stereo block and reports cycle counts
//! via defmt/RTT. Compare against the per-block budget of 1,280,000 cycles
//! (480 MHz / 375 blocks-per-second at 48 kHz).
//!
//! # Build & Flash
//!
//! ```bash
//! cd crates/sonido-daisy
//! cargo objcopy --example bench_kernels --release -- -O binary bench.bin
//! # Enter bootloader (hold BOOT, tap RESET, release BOOT — LED pulses)
//! dfu-util -a 0 -s 0x90040000:leave -D bench.bin
//! ```
//!
//! # Output
//!
//! **With probe** (defmt RTT): full table with cycle counts and percentages.
//!
//! **Without probe** (LED): after benchmarks complete, the LED blinks a
//! summary of all 19 kernels in order. For each kernel:
//!
//! 1. **Fast blinks** (100ms) = kernel index (1-based, so 1 blink = preamp,
//!    2 = distortion, ..., 19 = stage)
//! 2. **Pause** (600ms)
//! 3. **Slow blinks** (300ms) = budget percentage / 10 (so 3 blinks = 30%,
//!    0 blinks for <5%). A very long on (1s) means ≥100%.
//! 4. **Long pause** (1.2s) before next kernel
//!
//! The full sequence repeats forever so you can re-read any value.

#![no_std]
#![no_main]

extern crate alloc;

use defmt_rtt as _;
use embedded_alloc::LlffHeap as Heap;
use panic_probe as _;

use sonido_core::kernel::DspKernel;

#[global_allocator]
static HEAP: Heap = Heap::empty();

use sonido_daisy::{
    BLOCK_SIZE, CYCLES_PER_BLOCK, SAMPLE_RATE, enable_cycle_counter, measure_cycles,
};

use sonido_effects::kernels::{
    BitcrusherKernel, BitcrusherParams, ChorusKernel, ChorusParams, CompressorKernel,
    CompressorParams, DelayKernel, DelayParams, DistortionKernel, DistortionParams, EqKernel,
    EqParams, FilterKernel, FilterParams, FlangerKernel, FlangerParams, GateKernel, GateParams,
    LimiterKernel, LimiterParams, PhaserKernel, PhaserParams, PreampKernel, PreampParams,
    ReverbKernel, ReverbParams, RingModKernel, RingModParams, StageKernel, StageParams, TapeKernel,
    TapeParams, TremoloKernel, TremoloParams, VibratoKernel, VibratoParams, WahKernel, WahParams,
};

const NUM_KERNELS: usize = 19;

/// Kernel names in benchmark order (for defmt output).
const NAMES: [&str; NUM_KERNELS] = [
    "preamp",
    "distortion",
    "compressor",
    "gate",
    "eq",
    "wah",
    "chorus",
    "flanger",
    "phaser",
    "tremolo",
    "delay",
    "filter",
    "vibrato",
    "tape",
    "reverb",
    "limiter",
    "bitcrusher",
    "ringmod",
    "stage",
];

/// Benchmark a single kernel: create, process one block, return cycle count.
/// Kernel is dropped at end of scope (frees heap for the next one).
macro_rules! bench {
    ($results:expr, $idx:expr, $kernel:ty, $params:ty) => {{
        let mut k = <$kernel>::new(SAMPLE_RATE);
        let p = <$params>::default();
        $results[$idx] = measure_cycles(|| {
            for _ in 0..BLOCK_SIZE {
                let _ = k.process_stereo(0.5, -0.3, &p);
            }
        });
    }};
}

/// Log one result via defmt.
fn report(name: &str, cycles: u32) {
    let pct_x100 = (cycles as u64 * 10000) / CYCLES_PER_BLOCK as u64;
    defmt::info!(
        "kernel={} cycles={} budget={} pct={}.{}%",
        name,
        cycles,
        CYCLES_PER_BLOCK,
        pct_x100 / 100,
        pct_x100 % 100
    );
}

/// Blink the LED: `count` fast pulses (100ms on/off).
fn blink_fast(bsrr: *mut u32, count: u32) {
    for _ in 0..count {
        unsafe { core::ptr::write_volatile(bsrr, 1 << 7) }; // on
        cortex_m::asm::delay(10_000_000); // ~100ms at ~100MHz post-init
        unsafe { core::ptr::write_volatile(bsrr, 1 << (7 + 16)) }; // off
        cortex_m::asm::delay(10_000_000);
    }
}

/// Blink the LED: `count` slow pulses (300ms on/off).
/// If count is 0, do one very short flash (50ms) so you know it's "zero".
fn blink_slow(bsrr: *mut u32, count: u32) {
    if count == 0 {
        unsafe { core::ptr::write_volatile(bsrr, 1 << 7) };
        cortex_m::asm::delay(5_000_000); // ~50ms
        unsafe { core::ptr::write_volatile(bsrr, 1 << (7 + 16)) };
        cortex_m::asm::delay(5_000_000);
        return;
    }
    if count >= 10 {
        // ≥100% — one long on (1s)
        unsafe { core::ptr::write_volatile(bsrr, 1 << 7) };
        cortex_m::asm::delay(100_000_000);
        unsafe { core::ptr::write_volatile(bsrr, 1 << (7 + 16)) };
        cortex_m::asm::delay(30_000_000);
        return;
    }
    for _ in 0..count {
        unsafe { core::ptr::write_volatile(bsrr, 1 << 7) };
        cortex_m::asm::delay(30_000_000); // ~300ms
        unsafe { core::ptr::write_volatile(bsrr, 1 << (7 + 16)) };
        cortex_m::asm::delay(30_000_000);
    }
}

#[embassy_executor::main]
async fn main(_spawner: embassy_executor::Spawner) {
    // Initialize heap — point at D2 SRAM (0x30008000, 256 KB).
    // Safe during benchmarks: no audio DMA running, region is unused.
    unsafe {
        HEAP.init(0x3000_8000, 256 * 1024);
    }

    let config = daisy_embassy::default_rcc();
    let _p = embassy_stm32::init(config);

    let mut cp = cortex_m::Peripherals::take().unwrap();
    enable_cycle_counter(&mut cp.DCB, &mut cp.DWT);

    defmt::info!("=== Sonido Kernel Benchmarks ===");
    defmt::info!(
        "sample_rate={} block_size={} budget={} cycles",
        SAMPLE_RATE as u32,
        BLOCK_SIZE,
        CYCLES_PER_BLOCK
    );

    let mut results = [0u32; NUM_KERNELS];

    bench!(results, 0, PreampKernel, PreampParams);
    bench!(results, 1, DistortionKernel, DistortionParams);
    bench!(results, 2, CompressorKernel, CompressorParams);
    bench!(results, 3, GateKernel, GateParams);
    bench!(results, 4, EqKernel, EqParams);
    bench!(results, 5, WahKernel, WahParams);
    bench!(results, 6, ChorusKernel, ChorusParams);
    bench!(results, 7, FlangerKernel, FlangerParams);
    bench!(results, 8, PhaserKernel, PhaserParams);
    bench!(results, 9, TremoloKernel, TremoloParams);
    bench!(results, 10, DelayKernel, DelayParams);
    bench!(results, 11, FilterKernel, FilterParams);
    bench!(results, 12, VibratoKernel, VibratoParams);
    bench!(results, 13, TapeKernel, TapeParams);
    bench!(results, 14, ReverbKernel, ReverbParams);
    bench!(results, 15, LimiterKernel, LimiterParams);
    bench!(results, 16, BitcrusherKernel, BitcrusherParams);
    bench!(results, 17, RingModKernel, RingModParams);
    bench!(results, 18, StageKernel, StageParams);

    // Print full results via defmt (visible with probe)
    for (i, &cycles) in results.iter().enumerate() {
        report(NAMES[i], cycles);
    }
    defmt::info!("=== Benchmarks complete ===");

    // --- LED output (visible without probe) ---
    // Set up PC7 as output for LED signaling
    const GPIOC_BASE: u32 = 0x5802_0800;
    const GPIOC_BSRR: *mut u32 = (GPIOC_BASE + 0x18) as *mut u32;
    // GPIO is already configured by embassy_stm32::init(), but ensure PC7 is output
    const GPIOC_MODER: *mut u32 = GPIOC_BASE as *mut u32;
    unsafe {
        let val = core::ptr::read_volatile(GPIOC_MODER);
        let val = val & !(0b11 << 14);
        let val = val | (0b01 << 14);
        core::ptr::write_volatile(GPIOC_MODER, val);
    }

    // Precompute budget percentages (rounded to nearest 10%)
    let mut pct_tens = [0u32; NUM_KERNELS];
    for i in 0..NUM_KERNELS {
        // (cycles * 100 / budget + 5) / 10 = rounded to nearest 10%
        let pct = (results[i] as u64 * 100) / CYCLES_PER_BLOCK as u64;
        pct_tens[i] = ((pct + 5) / 10) as u32;
    }

    // Signal "ready": 3 quick flashes
    blink_fast(GPIOC_BSRR, 3);
    cortex_m::asm::delay(60_000_000); // 600ms pause

    // Loop forever: blink all 19 results, then repeat
    loop {
        for i in 0..NUM_KERNELS {
            // Kernel index (1-based): fast blinks
            blink_fast(GPIOC_BSRR, (i as u32) + 1);
            // Pause between index and value
            cortex_m::asm::delay(60_000_000); // 600ms
            // Budget percentage / 10: slow blinks
            blink_slow(GPIOC_BSRR, pct_tens[i]);
            // Long pause before next kernel
            cortex_m::asm::delay(120_000_000); // 1.2s
        }
        // Extra-long pause between full cycles
        cortex_m::asm::delay(300_000_000); // 3s
    }
}
