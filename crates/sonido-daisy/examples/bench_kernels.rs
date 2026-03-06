//! Tier 2: DWT cycle-count benchmarks for all 19 Sonido DSP kernels.
//!
//! Runs each kernel through a 128-sample stereo block and reports cycle counts.
//! Results are output via **USB serial** (CDC ACM) — no probe needed.
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
use embassy_usb::UsbDevice;
use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
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
fn format_results(results: &[u32; NUM_KERNELS], buf: &mut [u8]) -> usize {
    struct BufWriter<'a> {
        buf: &'a mut [u8],
        pos: usize,
    }
    impl<'a> core::fmt::Write for BufWriter<'a> {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            let bytes = s.as_bytes();
            let remaining = self.buf.len() - self.pos;
            let len = bytes.len().min(remaining);
            self.buf[self.pos..self.pos + len].copy_from_slice(&bytes[..len]);
            self.pos += len;
            Ok(())
        }
    }

    let mut w = BufWriter { buf, pos: 0 };
    let _ = writeln!(w, "\r\n=== Sonido Kernel Benchmarks ===");
    let _ = writeln!(
        w,
        "sample_rate=48000 block_size={} budget={} cycles\r",
        BLOCK_SIZE, CYCLES_PER_BLOCK
    );

    let mut total: u64 = 0;
    for (i, &cycles) in results.iter().enumerate() {
        let pct_x100 = (cycles as u64 * 10000) / CYCLES_PER_BLOCK as u64;
        let _ = writeln!(
            w,
            "  {:>12}  {:>8} cycles  {:>3}.{:02}%\r",
            NAMES[i],
            cycles,
            pct_x100 / 100,
            pct_x100 % 100
        );
        total += cycles as u64;
    }

    let total_pct = (total * 10000) / CYCLES_PER_BLOCK as u64;
    let _ = writeln!(w, "---\r");
    let _ = writeln!(
        w,
        "  {:>12}  {:>8} cycles  {:>3}.{:02}% (all 19 kernels)\r",
        "TOTAL",
        total,
        total_pct / 100,
        total_pct % 100
    );
    let _ = writeln!(w, "=== End ===\r");
    w.pos
}

#[embassy_executor::task]
async fn usb_task(mut device: UsbDevice<'static, Driver<'static, peripherals::USB_OTG_FS>>) -> ! {
    device.run().await
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // Initialize heap — point at D2 SRAM (0x30008000, 256 KB).
    unsafe {
        HEAP.init(0x3000_8000, 256 * 1024);
    }

    let config = daisy_embassy::default_rcc();
    let p = embassy_stm32::init(config);

    let mut cp = cortex_m::Peripherals::take().unwrap();
    enable_cycle_counter(&mut cp.DCB, &mut cp.DWT);

    defmt::info!("=== Running benchmarks... ===");

    // --- Run all benchmarks first (before USB init) ---
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
    for (i, &cycles) in results.iter().enumerate() {
        let pct_x100 = (cycles as u64 * 10000) / CYCLES_PER_BLOCK as u64;
        defmt::info!(
            "kernel={} cycles={} budget={} pct={}.{}%",
            NAMES[i],
            cycles,
            CYCLES_PER_BLOCK,
            pct_x100 / 100,
            pct_x100 % 100
        );
    }
    defmt::info!("=== Benchmarks complete, starting USB serial ===");

    // --- Format results into a static buffer ---
    static mut OUTPUT_BUF: [u8; 2048] = [0u8; 2048];
    #[allow(static_mut_refs)]
    let output_len = unsafe { format_results(&results, &mut OUTPUT_BUF) };

    // --- USB CDC ACM setup ---
    static mut EP_OUT_BUF: [u8; 256] = [0u8; 256];

    #[allow(static_mut_refs)]
    let driver = Driver::new_fs(
        p.USB_OTG_FS,
        Irqs,
        p.PA12,
        p.PA11,
        unsafe { &mut EP_OUT_BUF },
        embassy_stm32::usb::Config::default(),
    );

    let mut usb_config = embassy_usb::Config::new(0x1209, 0x0001);
    usb_config.manufacturer = Some("Sonido");
    usb_config.product = Some("Kernel Benchmarks");
    usb_config.serial_number = Some("001");

    static mut CONFIG_DESC: [u8; 256] = [0; 256];
    static mut BOS_DESC: [u8; 256] = [0; 256];
    static mut MSOS_DESC: [u8; 256] = [0; 256];
    static mut CONTROL_BUF: [u8; 64] = [0; 64];
    static mut CDC_STATE: Option<State<'static>> = None;

    #[allow(static_mut_refs)]
    unsafe {
        CDC_STATE = Some(State::new());
    }

    #[allow(static_mut_refs)]
    let mut builder = unsafe {
        embassy_usb::Builder::new(
            driver,
            usb_config,
            &mut CONFIG_DESC,
            &mut BOS_DESC,
            &mut MSOS_DESC,
            &mut CONTROL_BUF,
        )
    };

    #[allow(static_mut_refs)]
    let mut class = unsafe { CdcAcmClass::new(&mut builder, CDC_STATE.as_mut().unwrap(), 64) };

    let usb = builder.build();
    spawner.spawn(usb_task(usb)).unwrap();

    // --- Send results over USB serial, repeating forever ---
    loop {
        class.wait_connection().await;
        defmt::info!("USB serial connected");

        #[allow(static_mut_refs)]
        let data = unsafe { &OUTPUT_BUF[..output_len] };

        loop {
            // Send in 64-byte chunks (USB FS max packet)
            let mut sent = false;
            for chunk in data.chunks(64) {
                if class.write_packet(chunk).await.is_err() {
                    break;
                }
                sent = true;
            }
            if !sent {
                break; // disconnected
            }

            // Wait 5 seconds before repeating
            embassy_time::Timer::after_secs(5).await;
        }
    }
}
