//! Tier 3.75: Float math + ControlBuffer reads — isolate knob reads and float processing.
//!
//! Applies a simple volume gain (K1) to stereo audio using only float multiplication
//! and `ControlBuffer` reads. No kernel, no `from_knobs`, no allocation. If this
//! buzzes when convert_passthrough.rs is clean, the bug is in `ControlBuffer` reads
//! or basic float processing in the audio callback. If clean, the issue is in the
//! kernel or `from_knobs` mapping.
//!
//! # Diagnostic Tier
//!
//! | Tier | Example              | What it isolates                           |
//! |------|----------------------|--------------------------------------------|
//! | 3    | passthrough          | Codec/DMA path (raw u32 copy)              |
//! | 3.5  | convert_passthrough  | u24↔f32 roundtrip                          |
//! | 3.75 | simple_gain          | Float math + ControlBuffer reads (this)    |
//! | 4    | single_effect        | Full kernel + from_knobs                   |
//!
//! # Hardware Mapping
//!
//! | Control      | Pin(s)        | Function                     |
//! |--------------|---------------|------------------------------|
//! | KNOB_1       | PA3           | Volume/gain (0.0–1.0)        |
//! | FOOTSWITCH_1 | PA0 (pull-up) | Bypass toggle on release     |
//! | LED_1        | PA5           | Active (on) / Bypassed (off) |
//!
//! # Build & Flash
//!
//! ```bash
//! cd crates/sonido-daisy
//! cargo objcopy --example simple_gain --release -- -O binary -R .sram1_bss simple_gain.bin
//! # Press RESET, then flash within the 2.5s grace period:
//! dfu-util -a 0 -s 0x90040000:leave -D simple_gain.bin
//! ```
//!
//! # Testing
//!
//! 1. Connect guitar/synth to Hothouse input
//! 2. Connect Hothouse output to amp/interface
//! 3. Play — audio should pass through at volume set by K1 (full CW = unity)
//! 4. If buzz appears here but not in convert_passthrough.rs, the bug is in
//!    ControlBuffer reads or float multiply in the audio callback

#![no_std]
#![no_main]

use defmt_rtt as _;
use embassy_stm32 as hal;
use panic_probe as _;

use sonido_daisy::controls::HothouseBuffer;
use sonido_daisy::hothouse::hothouse_control_task;
use sonido_daisy::{ClockProfile, f32_to_u24, heartbeat, led::UserLed, u24_to_f32};

// ── Shared control buffer ────────────────────────────────────────────────

static CONTROLS: HothouseBuffer = HothouseBuffer::new();

// ── Main ─────────────────────────────────────────────────────────────────

#[embassy_executor::main]
async fn main(spawner: embassy_executor::Spawner) {
    // D2 SRAM clocks off at reset — must enable before DMA buffers are touched.
    sonido_daisy::enable_d2_sram();

    let config = sonido_daisy::rcc_config(ClockProfile::Performance);
    let p = hal::init(config);

    let led = UserLed::new(p.PC7);
    spawner.spawn(heartbeat(led)).unwrap();

    defmt::info!("sonido-daisy simple_gain: initializing...");

    // ── Extract control pins and spawn control task ──
    let ctrl = sonido_daisy::hothouse_pins!(p);
    spawner
        .spawn(hothouse_control_task(ctrl, &CONTROLS))
        .unwrap();
    CONTROLS.write_led(0, 1.0);

    // ── Construct audio peripherals directly ──
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

    defmt::info!("audio interface started — simple gain active");

    let mut active = true;
    let mut foot_was_pressed = false;

    defmt::unwrap!(
        interface
            .start_callback(move |input, output| {
                // ── Footswitch bypass toggle (fire on release) ──
                let foot_pressed = CONTROLS.read_footswitch(0);
                if foot_was_pressed && !foot_pressed {
                    active = !active;
                    CONTROLS.write_led(0, if active { 1.0 } else { 0.0 });
                }
                foot_was_pressed = foot_pressed;

                if !active {
                    output.copy_from_slice(input);
                    return;
                }

                let gain = CONTROLS.read_knob(0); // K1 = volume

                for i in (0..input.len()).step_by(2) {
                    let left = u24_to_f32(input[i]);
                    let right = u24_to_f32(input[i + 1]);
                    output[i] = f32_to_u24(left * gain);
                    output[i + 1] = f32_to_u24(right * gain);
                }
            })
            .await
    );
}
