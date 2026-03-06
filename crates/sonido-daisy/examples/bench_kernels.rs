//! Tier 2: DWT cycle-count benchmarks for all 19 Sonido DSP kernels.
//!
//! Runs each kernel through a 128-sample stereo block and reports cycle counts
//! via defmt/RTT. Compare against the per-block budget of 1,280,000 cycles
//! (480 MHz / 375 blocks-per-second at 48 kHz).
//!
//! # Run
//!
//! ```bash
//! cargo run --example bench_kernels --release
//! ```
//!
//! # Output (via defmt RTT)
//!
//! ```text
//! kernel=distortion   cycles=12345  budget=1280000  pct=0.96%
//! kernel=reverb       cycles=98765  budget=1280000  pct=7.72%
//! ...
//! ```

#![no_std]
#![no_main]

use defmt_rtt as _;
use panic_probe as _;

use sonido_core::kernel::DspKernel;
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

/// Benchmark a single kernel: process BLOCK_SIZE stereo samples, return cycle count.
fn bench_kernel<K: DspKernel>(kernel: &mut K, params: &K::Params) -> u32 {
    measure_cycles(|| {
        for _ in 0..BLOCK_SIZE {
            let _ = kernel.process_stereo(0.5, -0.3, params);
        }
    })
}

/// Log benchmark result via defmt.
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

#[embassy_executor::main]
async fn main(_spawner: embassy_executor::Spawner) {
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

    // Preamp
    let mut k = PreampKernel::new(SAMPLE_RATE);
    let p = PreampParams::default();
    report("preamp", bench_kernel(&mut k, &p));

    // Distortion
    let mut k = DistortionKernel::new(SAMPLE_RATE);
    let p = DistortionParams::default();
    report("distortion", bench_kernel(&mut k, &p));

    // Compressor
    let mut k = CompressorKernel::new(SAMPLE_RATE);
    let p = CompressorParams::default();
    report("compressor", bench_kernel(&mut k, &p));

    // Gate
    let mut k = GateKernel::new(SAMPLE_RATE);
    let p = GateParams::default();
    report("gate", bench_kernel(&mut k, &p));

    // EQ
    let mut k = EqKernel::new(SAMPLE_RATE);
    let p = EqParams::default();
    report("eq", bench_kernel(&mut k, &p));

    // Wah
    let mut k = WahKernel::new(SAMPLE_RATE);
    let p = WahParams::default();
    report("wah", bench_kernel(&mut k, &p));

    // Chorus
    let mut k = ChorusKernel::new(SAMPLE_RATE);
    let p = ChorusParams::default();
    report("chorus", bench_kernel(&mut k, &p));

    // Flanger
    let mut k = FlangerKernel::new(SAMPLE_RATE);
    let p = FlangerParams::default();
    report("flanger", bench_kernel(&mut k, &p));

    // Phaser
    let mut k = PhaserKernel::new(SAMPLE_RATE);
    let p = PhaserParams::default();
    report("phaser", bench_kernel(&mut k, &p));

    // Tremolo
    let mut k = TremoloKernel::new(SAMPLE_RATE);
    let p = TremoloParams::default();
    report("tremolo", bench_kernel(&mut k, &p));

    // Delay
    let mut k = DelayKernel::new(SAMPLE_RATE);
    let p = DelayParams::default();
    report("delay", bench_kernel(&mut k, &p));

    // Filter
    let mut k = FilterKernel::new(SAMPLE_RATE);
    let p = FilterParams::default();
    report("filter", bench_kernel(&mut k, &p));

    // Vibrato
    let mut k = VibratoKernel::new(SAMPLE_RATE);
    let p = VibratoParams::default();
    report("vibrato", bench_kernel(&mut k, &p));

    // Tape
    let mut k = TapeKernel::new(SAMPLE_RATE);
    let p = TapeParams::default();
    report("tape", bench_kernel(&mut k, &p));

    // Reverb
    let mut k = ReverbKernel::new(SAMPLE_RATE);
    let p = ReverbParams::default();
    report("reverb", bench_kernel(&mut k, &p));

    // Limiter
    let mut k = LimiterKernel::new(SAMPLE_RATE);
    let p = LimiterParams::default();
    report("limiter", bench_kernel(&mut k, &p));

    // Bitcrusher
    let mut k = BitcrusherKernel::new(SAMPLE_RATE);
    let p = BitcrusherParams::default();
    report("bitcrusher", bench_kernel(&mut k, &p));

    // Ring Modulator
    let mut k = RingModKernel::new(SAMPLE_RATE);
    let p = RingModParams::default();
    report("ringmod", bench_kernel(&mut k, &p));

    // Stage
    let mut k = StageKernel::new(SAMPLE_RATE);
    let p = StageParams::default();
    report("stage", bench_kernel(&mut k, &p));

    defmt::info!("=== Benchmarks complete ===");

    // Halt — benchmarks are one-shot
    loop {
        cortex_m::asm::wfi();
    }
}
