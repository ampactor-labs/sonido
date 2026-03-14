//! Tier 3.5: u24↔f32 conversion roundtrip — isolate format conversion from DSP.
//!
//! Converts each audio sample u24→f32→u24 and writes to output. No DSP, no
//! controls, no allocation. If this buzzes when passthrough.rs is clean, the
//! bug is in the u24↔f32 conversion functions or SAI format configuration.
//!
//! # Diagnostic Tier
//!
//! | Tier | Example              | What it isolates                           |
//! |------|----------------------|--------------------------------------------|
//! | 3    | passthrough          | Codec/DMA path (raw u32 copy)              |
//! | 3.5  | convert_passthrough  | u24↔f32 roundtrip (this file)              |
//! | 3.75 | simple_gain          | Float math + ControlBuffer reads           |
//! | 4    | single_effect        | Full kernel + from_knobs                   |
//!
//! # Audio Format
//!
//! The SAI DMA driver delivers 32 stereo pairs per callback as interleaved `u32`:
//! `[L0, R0, L1, R1, ..., L31, R31]` — 64 elements total.
//! Each `u32` is a 24-bit signed sample left-justified in 32 bits.
//! `u24_to_f32` and `f32_to_u24` perform the signed normalization and packing.
//!
//! # Build & Flash
//!
//! ```bash
//! cd crates/sonido-daisy
//! cargo objcopy --example convert_passthrough --release -- -O binary -R .sram1_bss convert_passthrough.bin
//! # Press RESET, then flash within the 2.5s grace period:
//! dfu-util -a 0 -s 0x90040000:leave -D convert_passthrough.bin
//! ```
//!
//! # Testing
//!
//! 1. Connect guitar/synth to Hothouse input
//! 2. Connect Hothouse output to amp/interface
//! 3. Play — audio should pass through with no audible difference from passthrough.rs
//! 4. If buzz appears here but not in passthrough.rs, the bug is in u24↔f32 conversion

#![no_std]
#![no_main]

use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_stm32 as hal;
use panic_probe as _;

use sonido_daisy::{ClockProfile, f32_to_u24, heartbeat, led::UserLed, u24_to_f32};

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // D2 SRAM clocks off at reset — must enable before DMA buffers are touched.
    sonido_daisy::enable_d2_sram();

    let config = sonido_daisy::rcc_config(ClockProfile::Performance);
    let p = hal::init(config);

    let led = UserLed::new(p.PC7);
    spawner.spawn(heartbeat(led)).unwrap();

    defmt::info!("sonido-daisy convert_passthrough starting");

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

    defmt::info!("audio interface started — u24↔f32 roundtrip active");

    defmt::unwrap!(
        interface
            .start_callback(|input, output| {
                // Audio callback — conversion roundtrip, black_box prevents
                // LLVM from optimizing this to a raw copy.
                for i in 0..input.len() {
                    let sample = u24_to_f32(input[i]);
                    output[i] = f32_to_u24(core::hint::black_box(sample));
                }
            })
            .await
    );
}
