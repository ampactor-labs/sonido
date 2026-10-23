//! Tier 2: DWT cycle-count benchmarks for all 36 of Sonido's DSP kernels.
//!
//! Each kernel is heap-allocated (SDRAM, as on the pedal), warmed up for
//! `WARMUP_BLOCKS`, then timed over `TIMED_BLOCKS` 32-sample stereo blocks of
//! a 220 Hz sine. It reports the mean and the worst block: kernels with
//! periodic work (the tuner's pitch analysis every 2048 samples, grain
//! schedulers) can be cheap on average and still miss one block's deadline.
//! Percentages are of the 480 MHz (`Performance`) budget: 320,000 cycles per
//! 32-sample block. Results go out over defmt RTT (with a probe) and USB serial.
//!
//! # Run with an SWD probe (no bootloader needed)
//!
//! ```bash
//! cd crates/sonido-daisy
//! cargo run --example bench_kernels --release
//! ```
//!
//! probe-rs loads the image into AXI SRAM, starts it, and prints the defmt log:
//! one `bench <kernel>` line as each starts (a hang names the culprit), then
//! `kernel=<name> mean=<cycles> worst=<cycles> budget=320000` per kernel.
//!
//! # Or flash over USB (Electrosmith bootloader)
//!
//! ```bash
//! cargo objcopy --example bench_kernels --release -- -O binary -R .sram1_bss bench.bin
//! # Press RESET, then flash within the 2.5 s grace period:
//! dfu-util -a 0 -s 0x90040000:leave -D bench.bin
//! ```
//!
//! After the benchmarks the Daisy enumerates as a USB serial device
//! (`1209:0001`); `cat /dev/ttyACM0` shows the table, repeated every 5 s.
//!
//! Measured results: `docs/BENCHMARKS.md`, "Cortex-M7, measured on a Daisy
//! Seed".

#![no_std]
#![no_main]

extern crate alloc;

use core::fmt::Write as FmtWrite;

use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_stm32::usb::Driver;
use embassy_stm32::{bind_interrupts, peripherals, usb};
use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
use embedded_alloc::LlffHeap as Heap;
use panic_probe as _;
use static_cell::StaticCell;

use sonido_core::kernel::DspKernel;

#[global_allocator]
static HEAP: Heap = Heap::empty();

use sonido_daisy::{
    BLOCK_SIZE, BufWriter, ClockProfile, SAMPLE_RATE, enable_cycle_counter, heartbeat,
    led::UserLed, measure_cycles, rcc, usb_task,
};

use sonido_effects::kernels::*;

bind_interrupts!(struct Irqs {
    OTG_FS => usb::InterruptHandler<peripherals::USB_OTG_FS>;
});

const NUM_KERNELS: usize = 36;

/// Blocks run before timing (fills delay lines, settles caches).
const WARMUP_BLOCKS: usize = 8;
/// Timed blocks per kernel: 4096 samples, enough to include one tuner analysis.
const TIMED_BLOCKS: usize = 128;
/// Input length: one sine period table long enough for every timed block.
const INPUT_LEN: usize = BLOCK_SIZE * (WARMUP_BLOCKS + TIMED_BLOCKS);

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
    "looper",
    "amp",
    "cabinet",
    "deesser",
    "drone",
    "glitch",
    "multiband_comp",
    "pitch_shift",
    "plate_reverb",
    "shelving_eq",
    "spring_reverb",
    "stereo_widener",
    "texture",
    "time_stretch",
    "transient_shaper",
    "tuner",
    "harmonic_habitat",
];

/// Per-kernel result: mean and worst cycles per 32-sample block.
#[derive(Clone, Copy, Default)]
struct BlockCycles {
    mean: u32,
    worst: u32,
}

/// Benchmark one kernel: heap-allocate it (SDRAM, like the pedal's boxed
/// effects), warm up, then time each of `TIMED_BLOCKS` blocks.
macro_rules! bench {
    ($results:expr, $idx:expr, $input:expr, $kernel:ty, $params:ty) => {{
        defmt::info!("bench {}", NAMES[$idx]);
        let mut k = alloc::boxed::Box::new(<$kernel>::new(SAMPLE_RATE));
        let p = <$params>::default();
        let input: &[f32] = $input;
        let mut block = |b: usize| {
            for &x in &input[b * BLOCK_SIZE..(b + 1) * BLOCK_SIZE] {
                core::hint::black_box(k.process_stereo(x, -x, &p));
            }
        };
        for b in 0..WARMUP_BLOCKS {
            block(b);
        }
        let (mut total, mut worst) = (0u64, 0u32);
        for b in WARMUP_BLOCKS..WARMUP_BLOCKS + TIMED_BLOCKS {
            let c = measure_cycles(|| block(b));
            total += u64::from(c);
            worst = worst.max(c);
        }
        $results[$idx] = BlockCycles {
            mean: (total / TIMED_BLOCKS as u64) as u32,
            worst,
        };
        drop(k);
        // Let the heartbeat task run between kernels.
        embassy_time::Timer::after_millis(1).await;
    }};
}

/// Format all results into a fixed buffer. Returns the number of bytes written.
///
/// Shows dual-budget percentages for both Performance (480 MHz) and
/// Efficient (400 MHz) profiles.
fn format_results(results: &[BlockCycles; NUM_KERNELS], buf: &mut [u8]) -> usize {
    let budget = rcc::cycles_per_block(ClockProfile::Performance);
    let pct = |c: u32| {
        let x = (u64::from(c) * 10000) / u64::from(budget);
        (x / 100, x % 100)
    };

    let mut w = BufWriter::new(buf);
    let _ = writeln!(w, "\r\n=== Sonido Kernel Benchmarks ===\r");
    let _ = writeln!(
        w,
        "sample_rate=48000 block={} warmup={} timed={} budget={}cyc/block @480MHz\r",
        BLOCK_SIZE, WARMUP_BLOCKS, TIMED_BLOCKS, budget
    );
    let _ = writeln!(
        w,
        "  kernel                mean_cyc  mean%   worst_cyc  worst%\r"
    );
    for (i, r) in results.iter().enumerate() {
        let (mp, mf) = pct(r.mean);
        let (wp, wf) = pct(r.worst);
        let _ = writeln!(
            w,
            "  {:<18} {:>11} {:>4}.{:02} {:>11} {:>4}.{:02}\r",
            NAMES[i], r.mean, mp, mf, r.worst, wp, wf
        );
    }
    let _ = writeln!(w, "=== End ===\r");
    w.pos
}

// Static buffers for USB CDC ACM — StaticCell guarantees single-init safety without unsafe.
static OUTPUT_BUF: StaticCell<[u8; 4096]> = StaticCell::new();
static INPUT: StaticCell<[f32; INPUT_LEN]> = StaticCell::new();
static EP_OUT_BUF: StaticCell<[u8; 256]> = StaticCell::new();
static CONFIG_DESC: StaticCell<[u8; 256]> = StaticCell::new();
static BOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
static MSOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();
static CDC_STATE: StaticCell<State<'static>> = StaticCell::new();

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let config = sonido_daisy::rcc_config(ClockProfile::Performance);
    let p = embassy_stm32::init(config);

    // SAFETY: embassy-executor 0.9 may consume cortex_m::Peripherals internally.
    let mut cp = unsafe { cortex_m::Peripherals::steal() };

    // Initialize 64 MB SDRAM via FMC — configures MPU + power-up sequence.
    // Must come after embassy_stm32::init() (enables FMC clock via PLL2_R).
    let sdram_ptr = sonido_daisy::init_sdram!(p, &mut cp.MPU, &mut cp.SCB);
    unsafe {
        HEAP.init(sdram_ptr as usize, sonido_daisy::sdram::SDRAM_SIZE);
    }
    // No audio DMA — safe to enable D-cache immediately for SDRAM performance.
    sonido_daisy::sdram::enable_dcache();

    let led = UserLed::new(p.PC7);
    spawner.spawn(heartbeat(led)).unwrap();

    enable_cycle_counter(&mut cp.DCB, &mut cp.DWT);

    // --- Run all benchmarks with diagnostic yields ---
    // The LED heartbeat keeps blinking between kernels; if it stops, a kernel
    // hung. With a probe attached, the last "bench <name>" defmt line names it.
    // 220 Hz sine input, in DTCM (zero-wait), shared by every kernel.
    let input = INPUT.init([0.0; INPUT_LEN]);
    for (n, x) in input.iter_mut().enumerate() {
        *x = 0.5 * libm::sinf(2.0 * core::f32::consts::PI * 220.0 * n as f32 / SAMPLE_RATE);
    }
    let input: &[f32] = input;

    let mut results = [BlockCycles::default(); NUM_KERNELS];
    bench!(results, 0, input, PreampKernel, PreampParams);
    bench!(results, 1, input, DistortionKernel, DistortionParams);
    bench!(results, 2, input, CompressorKernel, CompressorParams);
    bench!(results, 3, input, GateKernel, GateParams);
    bench!(results, 4, input, EqKernel, EqParams);
    bench!(results, 5, input, WahKernel, WahParams);
    bench!(results, 6, input, ChorusKernel, ChorusParams);
    bench!(results, 7, input, FlangerKernel, FlangerParams);
    bench!(results, 8, input, PhaserKernel, PhaserParams);
    bench!(results, 9, input, TremoloKernel, TremoloParams);
    bench!(results, 10, input, DelayKernel, DelayParams);
    bench!(results, 11, input, FilterKernel, FilterParams);
    bench!(results, 12, input, VibratoKernel, VibratoParams);
    bench!(results, 13, input, TapeKernel, TapeParams);
    bench!(results, 14, input, ReverbKernel, ReverbParams);
    bench!(results, 15, input, LimiterKernel, LimiterParams);
    bench!(results, 16, input, BitcrusherKernel, BitcrusherParams);
    bench!(results, 17, input, RingModKernel, RingModParams);
    bench!(results, 18, input, StageKernel, StageParams);
    bench!(results, 19, input, LooperKernel, LooperParams);
    bench!(results, 20, input, AmpKernel, AmpParams);
    bench!(results, 21, input, CabinetKernel, CabinetParams);
    bench!(results, 22, input, DeesserKernel, DeesserParams);
    bench!(results, 23, input, DroneKernel, DroneParams);
    bench!(results, 24, input, GlitchKernel, GlitchParams);
    bench!(results, 25, input, MultibandCompKernel, MultibandCompParams);
    bench!(results, 26, input, PitchShiftKernel, PitchShiftParams);
    bench!(results, 27, input, PlateReverbKernel, PlateReverbParams);
    bench!(results, 28, input, ShelvingEqKernel, ShelvingEqParams);
    bench!(results, 29, input, SpringReverbKernel, SpringReverbParams);
    bench!(results, 30, input, StereoWidenerKernel, StereoWidenerParams);
    bench!(results, 31, input, TextureKernel, TextureParams);
    bench!(results, 32, input, TimeStretchKernel, TimeStretchParams);
    bench!(
        results,
        33,
        input,
        TransientShaperKernel,
        TransientShaperParams
    );
    bench!(results, 34, input, TunerKernel, TunerParams);
    bench!(
        results,
        35,
        input,
        HarmonicHabitatKernel,
        HarmonicHabitatParams
    );

    // Log via defmt (visible with probe)
    let budget = rcc::cycles_per_block(ClockProfile::Performance);
    for (i, r) in results.iter().enumerate() {
        defmt::info!(
            "kernel={} mean={} worst={} budget={}",
            NAMES[i],
            r.mean,
            r.worst,
            budget
        );
    }
    // --- Second pass: the path a rig takes on the pedal ---
    // The registry's smoothed adapter (what `PatchPlayer` builds), one
    // `process_block_stereo` per block, with every parameter re-set to its own
    // value every 15th block, as the pedal's 100 Hz control tick does.
    let registry = sonido_registry::EffectRegistry::new();
    for id in NAMES {
        defmt::info!("bench rig {}", id);
        let Some(mut effect) = registry.create(id, SAMPLE_RATE) else {
            continue;
        };
        let params = effect.effect_param_count();
        let (mut l, mut r) = ([0.0f32; BLOCK_SIZE], [0.0f32; BLOCK_SIZE]);
        let (mut out_l, mut out_r) = ([0.0f32; BLOCK_SIZE], [0.0f32; BLOCK_SIZE]);
        let mut block = |b: usize| {
            for i in 0..BLOCK_SIZE {
                l[i] = input[b * BLOCK_SIZE + i];
                r[i] = -l[i];
            }
            if b.is_multiple_of(15) {
                for p in 0..params {
                    let v = effect.effect_get_param(p);
                    effect.effect_set_param(p, v);
                }
            }
            effect.process_block_stereo(&l, &r, &mut out_l, &mut out_r);
            core::hint::black_box(&out_l);
        };
        for b in 0..WARMUP_BLOCKS {
            block(b);
        }
        let (mut total, mut worst) = (0u64, 0u32);
        for b in WARMUP_BLOCKS..WARMUP_BLOCKS + TIMED_BLOCKS {
            let c = measure_cycles(|| block(b));
            total += u64::from(c);
            worst = worst.max(c);
        }
        defmt::info!(
            "rig={} mean={} worst={} budget={}",
            id,
            (total / TIMED_BLOCKS as u64) as u32,
            worst,
            budget
        );
        embassy_time::Timer::after_millis(1).await;
    }

    defmt::info!("=== Benchmarks complete, starting USB serial ===");

    // --- Format results into a static buffer ---
    let output_buf = OUTPUT_BUF.init([0u8; 4096]);
    let output_len = format_results(&results, output_buf);

    // --- USB CDC ACM setup ---
    let driver = Driver::new_fs(
        p.USB_OTG_FS,
        Irqs,
        p.PA12,
        p.PA11,
        EP_OUT_BUF.init([0u8; 256]),
        embassy_stm32::usb::Config::default(),
    );

    let mut usb_config = embassy_usb::Config::new(0x1209, 0x0001);
    usb_config.manufacturer = Some("Sonido");
    usb_config.product = Some("Kernel Benchmarks");
    usb_config.serial_number = Some("001");

    let cdc_state = CDC_STATE.init(State::new());
    let mut builder = embassy_usb::Builder::new(
        driver,
        usb_config,
        CONFIG_DESC.init([0; 256]),
        BOS_DESC.init([0; 256]),
        MSOS_DESC.init([0; 256]),
        CONTROL_BUF.init([0; 64]),
    );

    let mut class = CdcAcmClass::new(&mut builder, cdc_state, 64);

    let usb = builder.build();
    spawner.spawn(usb_task(usb)).unwrap();

    // --- Send results over USB serial, repeating forever ---
    loop {
        class.wait_connection().await;
        defmt::info!("USB serial connected");

        let data = &output_buf[..output_len];

        loop {
            // Send in 64-byte chunks (USB FS max packet)
            let mut ok = true;
            for chunk in data.chunks(64) {
                if class.write_packet(chunk).await.is_err() {
                    ok = false;
                    break;
                }
            }
            if !ok {
                break; // disconnected
            }

            // Wait 5 seconds before repeating
            embassy_time::Timer::after_secs(5).await;
        }
    }
}
