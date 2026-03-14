//! Single reverb effect on hardware.
//!
//! Processes audio through a Sonido reverb kernel (8-line Hadamard FDN) with
//! parameters mapped from ADC knob readings via `from_knobs()`.
//!
//! # Hardware Mapping
//!
//! | Control      | Pin(s)       | Function                                      |
//! |--------------|--------------|-----------------------------------------------|
//! | KNOB_1       | PA3          | Room size (0–100%)                            |
//! | KNOB_2       | PB1          | Decay (0–100%)                                |
//! | KNOB_3       | PA7          | Damping (0–100%)                              |
//! | KNOB_4       | PA6          | Mix / dry-wet (0–100%)                        |
//! | KNOB_5       | PC1          | Output level (−20 to +20 dB)                  |
//! | KNOB_6       | PC4          | Predelay (0–100 ms)                           |
//! | TOGGLE_1 up  | PB4          | Width: Up=Wide (100%)                         |
//! | TOGGLE_1 mid | (neither)    | Width: Mid=Normal (50%)                       |
//! | TOGGLE_1 dn  | PB5          | Width: Down=Mono (0%)                         |
//! | FOOTSWITCH_1 | PA0 (pull-up)| Bypass toggle on release                      |
//! | LED_1        | PA5          | Active (on) / Bypassed (off)                  |
//!
//! # Build & Flash
//!
//! ```bash
//! cd crates/sonido-daisy
//! cargo objcopy --example single_reverb --release --features alloc -- -O binary -R .sram1_bss firmware/single_reverb.bin
//! # Press RESET, then flash within the 2.5s grace period:
//! dfu-util -a 0 -s 0x90040000:leave -D firmware/single_reverb.bin
//! ```

#![no_std]
#![no_main]

extern crate alloc;

use defmt_rtt as _;
use embassy_stm32 as hal;
use embedded_alloc::LlffHeap as Heap;
use panic_probe as _;

use sonido_core::kernel::DspKernel;
use sonido_daisy::controls::HothouseBuffer;
use sonido_daisy::hothouse::hothouse_control_task;
use sonido_daisy::{ClockProfile, SAMPLE_RATE, f32_to_u24, heartbeat, led::UserLed, u24_to_f32};
use sonido_effects::kernels::{ReverbKernel, ReverbParams};

#[global_allocator]
static HEAP: Heap = Heap::empty();

static CONTROLS: HothouseBuffer = HothouseBuffer::new();

#[embassy_executor::main]
async fn main(spawner: embassy_executor::Spawner) {
    sonido_daisy::enable_d2_sram();
    sonido_daisy::enable_fpu_ftz();

    unsafe {
        HEAP.init(0x3000_8000, 256 * 1024);
    }

    let config = sonido_daisy::rcc_config(ClockProfile::Performance);
    let p = hal::init(config);

    let led = UserLed::new(p.PC7);
    spawner.spawn(heartbeat(led)).unwrap();

    defmt::info!("single_reverb: initializing...");

    let ctrl = sonido_daisy::hothouse_pins!(p);
    spawner
        .spawn(hothouse_control_task(ctrl, &CONTROLS))
        .unwrap();

    CONTROLS.write_led(0, 1.0);

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

    defmt::info!("audio interface started — reverb active");

    let mut kernel = ReverbKernel::new(SAMPLE_RATE);

    let mut active = true;
    let mut foot_was_pressed = false;

    defmt::unwrap!(
        interface
            .start_callback(move |input, output| {
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

                let room = CONTROLS.read_knob(0); // K1: Room size
                let decay = CONTROLS.read_knob(1); // K2: Decay
                let damp = CONTROLS.read_knob(2); // K3: Damping
                let mix = CONTROLS.read_knob(3); // K4: Mix
                let out_level = CONTROLS.read_knob(4); // K5: Output level
                let predelay = CONTROLS.read_knob(5); // K6: Predelay

                // Toggle 1: stereo width (UP=100%, MID=50%, DN=0%)
                let width = match CONTROLS.read_toggle(0) {
                    0 => 1.0, // UP = Wide
                    2 => 0.0, // DN = Mono
                    _ => 0.5, // MID = Normal
                };

                // ER level fixed at 50% — use knobs for the important params
                let er_level = 0.5;

                let params = ReverbParams::from_knobs(
                    room, decay, damp, predelay, mix, width, er_level, out_level,
                );

                for i in (0..input.len()).step_by(2) {
                    let left_in = u24_to_f32(input[i]);
                    let right_in = u24_to_f32(input[i + 1]);

                    let (left_out, right_out) = kernel.process_stereo(left_in, right_in, &params);
                    let left_out = if left_out.is_finite() { left_out } else { 0.0 };
                    let right_out = if right_out.is_finite() {
                        right_out
                    } else {
                        0.0
                    };

                    output[i] = f32_to_u24(left_out.clamp(-1.0, 1.0));
                    output[i + 1] = f32_to_u24(right_out.clamp(-1.0, 1.0));
                }
            })
            .await
    );
}
