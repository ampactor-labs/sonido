//! Minimal benchmark — one kernel, no heap, no USB.
//! Blinks LED fast if PreampKernel runs, slow if it crashes.

#![no_std]
#![no_main]

extern crate alloc;

use defmt_rtt as _;
use embassy_executor::Spawner;
use embedded_alloc::LlffHeap as Heap;

#[global_allocator]
static HEAP: Heap = Heap::empty();
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_time::Timer;
use panic_probe as _;

use sonido_core::kernel::DspKernel;
use sonido_daisy::{BLOCK_SIZE, ClockProfile, SAMPLE_RATE, enable_cycle_counter, measure_cycles};
use sonido_effects::kernels::{PreampKernel, PreampParams};

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    // D2 SRAM clocks are disabled at reset — enable before heap init.
    sonido_daisy::enable_d2_sram();
    unsafe {
        HEAP.init(0x3000_8000, 256 * 1024);
    }

    let config = sonido_daisy::rcc_config(ClockProfile::Performance);
    let p = embassy_stm32::init(config);

    let mut led = Output::new(p.PC7, Level::Low, Speed::Low);

    // SAFETY: embassy-executor may consume cortex_m::Peripherals internally.
    let mut cp = unsafe { cortex_m::Peripherals::steal() };
    enable_cycle_counter(&mut cp.DCB, &mut cp.DWT);

    defmt::info!("bench_mini: running preamp benchmark");

    let mut kernel = PreampKernel::new(SAMPLE_RATE);
    let params = PreampParams::default();
    let cycles = measure_cycles(|| {
        for _ in 0..BLOCK_SIZE {
            let _ = kernel.process_stereo(0.5, -0.3, &params);
        }
    });

    defmt::info!("preamp: {} cycles", cycles);

    // Blink fast (200ms) = success
    loop {
        led.set_high();
        Timer::after_millis(200).await;
        led.set_low();
        Timer::after_millis(200).await;
    }
}
