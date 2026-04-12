//! Minimal test: ProcessingGraph + SDRAM heap + audio.
//!
//! Proves that ProcessingGraph can coexist with SAI DMA when the heap
//! is on SDRAM (FMC bus, separate from D2 SRAM DMA bus).
//!
//! Follows `single_delay.rs` init pattern exactly:
//!   1. RCC + HAL init
//!   2. D2 SRAM clocks (for DMA buffers)
//!   3. SDRAM init (FMC + MPU)
//!   4. Heap on SDRAM (0xC0000000, 64 MB)
//!   5. D-cache enable
//!   6. Allocate ProcessingGraph + effects
//!   7. Start audio
//!
//! # Build & Flash
//!
//! ```bash
//! cd crates/sonido-daisy
//! cargo objcopy --example graph_audio_test --release --features alloc -- -O binary -R .sram1_bss graph_audio_test.bin
//! dfu-util -a 0 -s 0x90040000:leave -D graph_audio_test.bin
//! ```

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;

use defmt_rtt as _;
use embassy_stm32 as hal;
use embedded_alloc::LlffHeap as Heap;
use panic_probe as _;

use sonido_core::graph::ProcessingGraph;
use sonido_core::kernel::Adapter;
use sonido_daisy::{
    BLOCK_SIZE, ClockProfile, SAMPLE_RATE, f32_to_u24, heartbeat, led::UserLed, u24_to_f32,
};
use sonido_effects::DistortionKernel;

#[global_allocator]
static HEAP: Heap = Heap::empty();

#[embassy_executor::main]
async fn main(spawner: embassy_executor::Spawner) {
    // ── Clock + HAL ──
    let config = sonido_daisy::rcc_config(ClockProfile::Performance);
    let p = hal::init(config);

    // ── D2 SRAM clocks (DMA buffers at 0x30000000) ──
    sonido_daisy::enable_d2_sram();
    sonido_daisy::enable_fpu_ftz();

    // ── SDRAM init (FMC + MPU) ──
    let mut cp = unsafe { cortex_m::Peripherals::steal() };
    let sdram_ptr = sonido_daisy::init_sdram!(p, &mut cp.MPU, &mut cp.SCB);
    unsafe {
        HEAP.init(sdram_ptr as usize, sonido_daisy::sdram::SDRAM_SIZE);
    }

    // ── D-cache: safe before audio, required for SDRAM perf ──
    sonido_daisy::sdram::enable_dcache();

    let led = UserLed::new(p.PC7);
    spawner.spawn(heartbeat(led)).unwrap();

    defmt::info!("graph_audio_test: SDRAM heap initialized");

    // ── Build ProcessingGraph with one distortion effect ──
    let mut graph = ProcessingGraph::new(SAMPLE_RATE, BLOCK_SIZE);
    let inp = graph.add_input();
    let out = graph.add_output();
    let effect = Box::new(Adapter::new_direct(DistortionKernel::new(SAMPLE_RATE), SAMPLE_RATE));
    let eid = graph.add_effect(effect);
    graph.connect(inp, eid).unwrap();
    graph.connect(eid, out).unwrap();
    graph.compile().unwrap();

    defmt::info!("graph compiled: input -> distortion -> output");

    // ── Start audio ──
    let audio_peripherals = sonido_daisy::audio::AudioPeripherals {
        codec_pins: sonido_daisy::codec_pins!(p),
        sai1: p.SAI1,
        dma1_ch0: p.DMA1_CH0,
        dma1_ch1: p.DMA1_CH1,
    };

    let interface = audio_peripherals
        .prepare_interface(Default::default())
        .await;
    let mut interface = defmt::unwrap!(interface.start_interface().await);

    defmt::info!("audio started — processing through graph");

    let mut left_in = [0.0f32; BLOCK_SIZE];
    let mut right_in = [0.0f32; BLOCK_SIZE];
    let mut left_out = [0.0f32; BLOCK_SIZE];
    let mut right_out = [0.0f32; BLOCK_SIZE];

    defmt::unwrap!(
        interface
            .start_callback(move |input, output| {
                // Deinterleave
                for i in (0..input.len()).step_by(2) {
                    left_in[i / 2] = u24_to_f32(input[i]);
                    right_in[i / 2] = u24_to_f32(input[i + 1]);
                }

                // Process through graph
                graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);

                // Interleave output
                for i in (0..output.len()).step_by(2) {
                    let l = left_out[i / 2].clamp(-1.0, 1.0);
                    let r = right_out[i / 2].clamp(-1.0, 1.0);
                    output[i] = f32_to_u24(if l.is_finite() { l } else { 0.0 });
                    output[i + 1] = f32_to_u24(if r.is_finite() { r } else { 0.0 });
                }
            })
            .await
    );
}
