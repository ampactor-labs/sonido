//! Tier 2: DWT cycle-count benchmarks for all 19 Sonido DSP kernels.
//!
//! Runs each kernel through a 32-sample stereo block and reports cycle counts.
//! Results are output via **USB serial** (CDC ACM) — no probe needed.
//!
//! Shows dual-budget percentages: both 480 MHz (Performance) and 400 MHz
//! (Efficient) profiles, so you can choose the right profile for your chain.
//!
//! # Build & Flash
//!
//! ```bash
//! cd crates/sonido-daisy
//! cargo objcopy --example bench_kernels --release -- -O binary bench.bin
//! # Press RESET, then flash within the 2.5s grace period:
//! dfu-util -a 0 -s 0x90040000:leave -D bench.bin
//! ```
//!
//! # Read results
//!
//! After flashing, the Daisy enumerates as a USB serial device.
//! Open it with any terminal:
//!
//! ```bash
//! cat /dev/ttyACM0
//! # or: screen /dev/ttyACM0 115200
//! ```
//!
//! Output repeats every 5 seconds so you can connect at any time.
//!
//! Results are also available via defmt RTT if a probe is connected.

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

use sonido_effects::kernels::{
    BitcrusherKernel, BitcrusherParams, ChorusKernel, ChorusParams, CompressorKernel,
    CompressorParams, DelayKernel, DelayParams, DistortionKernel, DistortionParams, EqKernel,
    EqParams, FilterKernel, FilterParams, FlangerKernel, FlangerParams, GateKernel, GateParams,
    LimiterKernel, LimiterParams, PhaserKernel, PhaserParams, PreampKernel, PreampParams,
    ReverbKernel, ReverbParams, RingModKernel, RingModParams, StageKernel, StageParams, TapeKernel,
    TapeParams, TremoloKernel, TremoloParams, VibratoKernel, VibratoParams, WahKernel, WahParams,
};

bind_interrupts!(struct Irqs {
    OTG_FS => usb::InterruptHandler<peripherals::USB_OTG_FS>;
});

const NUM_KERNELS: usize = 19;

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

/// Format all results into a fixed buffer. Returns the number of bytes written.
///
/// Shows dual-budget percentages for both Performance (480 MHz) and
/// Efficient (400 MHz) profiles.
fn format_results(results: &[u32; NUM_KERNELS], buf: &mut [u8]) -> usize {
    let budget_perf = rcc::cycles_per_block(ClockProfile::Performance);
    let budget_eff = rcc::cycles_per_block(ClockProfile::Efficient);

    let mut w = BufWriter::new(buf);
    let _ = writeln!(w, "\r\n=== Sonido Kernel Benchmarks ===");
    let _ = writeln!(w, "sample_rate=48000 block_size={}\r", BLOCK_SIZE);
    let _ = writeln!(
        w,
        "budget: {}cyc @480MHz (Performance) | {}cyc @400MHz (Efficient)\r",
        budget_perf, budget_eff
    );

    let mut total: u64 = 0;
    for (i, &cycles) in results.iter().enumerate() {
        let pct_perf = (cycles as u64 * 10000) / budget_perf as u64;
        let pct_eff = (cycles as u64 * 10000) / budget_eff as u64;
        let _ = writeln!(
            w,
            "  {:>12}  {:>8} cycles  {:>3}.{:02}% / {:>3}.{:02}%\r",
            NAMES[i],
            cycles,
            pct_perf / 100,
            pct_perf % 100,
            pct_eff / 100,
            pct_eff % 100,
        );
        total += cycles as u64;
    }

    let total_pct_perf = (total * 10000) / budget_perf as u64;
    let total_pct_eff = (total * 10000) / budget_eff as u64;
    let _ = writeln!(w, "---\r");
    let _ = writeln!(
        w,
        "  {:>12}  {:>8} cycles  {:>3}.{:02}% / {:>3}.{:02}% (all 19)\r",
        "TOTAL",
        total,
        total_pct_perf / 100,
        total_pct_perf % 100,
        total_pct_eff / 100,
        total_pct_eff % 100,
    );
    let _ = writeln!(w, "                                  ^480MHz   ^400MHz\r");
    let _ = writeln!(w, "=== End ===\r");
    w.pos
}

// Static buffers for USB CDC ACM — StaticCell guarantees single-init safety without unsafe.
static OUTPUT_BUF: StaticCell<[u8; 2048]> = StaticCell::new();
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

    let led = UserLed::new(p.PC7);
    spawner.spawn(heartbeat(led)).unwrap();

    enable_cycle_counter(&mut cp.DCB, &mut cp.DWT);

    // --- Run all benchmarks with diagnostic yields ---
    // Count heartbeats to identify which group crashes:
    //   ~1 beat  = crash in DWT/cycle counter setup
    //   ~2 beats = crash in group A (preamp..eq)
    //   ~3 beats = crash in group B (wah..tremolo)
    //   ~4 beats = crash in group C (delay..reverb)
    //   ~5 beats = crash in group D (limiter..stage)
    //   continuous = all benchmarks passed
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

    // Log via defmt (visible with probe)
    let budget = rcc::cycles_per_block(ClockProfile::Performance);
    for (i, &cycles) in results.iter().enumerate() {
        let pct_x100 = (cycles as u64 * 10000) / budget as u64;
        defmt::info!(
            "kernel={} cycles={} budget={} pct={}.{}%",
            NAMES[i],
            cycles,
            budget,
            pct_x100 / 100,
            pct_x100 % 100
        );
    }
    defmt::info!("=== Benchmarks complete, starting USB serial ===");

    // --- Format results into a static buffer ---
    let output_buf = OUTPUT_BUF.init([0u8; 2048]);
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
