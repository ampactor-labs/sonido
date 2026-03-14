//! Single delay effect on hardware.
//!
//! Processes audio through a Sonido delay kernel (stereo/ping-pong with
//! diffusion and filtering) with parameters mapped from ADC knob readings
//! via `from_knobs()`.
//!
//! # Hardware Mapping
//!
//! | Control      | Pin(s)       | Function                                      |
//! |--------------|--------------|-----------------------------------------------|
//! | KNOB_1       | PA3          | Delay time (1–2000 ms, log)                   |
//! | KNOB_2       | PB1          | Feedback (0–95%)                              |
//! | KNOB_3       | PA7          | Diffusion (0–100%)                            |
//! | KNOB_4       | PA6          | Mix / dry-wet (0–100%)                        |
//! | KNOB_5       | PC1          | Output level (−20 to +6 dB)                   |
//! | KNOB_6       | PC4          | Feedback LP filter (200–20k Hz, log)          |
//! | TOGGLE_1 up  | PB4          | Mode: Up=Ping-pong                            |
//! | TOGGLE_1 mid | (neither)    | Mode: Mid=Stereo                              |
//! | TOGGLE_1 dn  | PB5          | Mode: Down=Stereo (same as mid)               |
//! | FOOTSWITCH_1 | PA0 (pull-up)| Bypass toggle on release                      |
//! | LED_1        | PA5          | Active (on) / Bypassed (off)                  |
//!
//! # Build & Flash
//!
//! ```bash
//! cd crates/sonido-daisy
//! cargo objcopy --example single_delay --release --features alloc -- -O binary -R .sram1_bss firmware/single_delay.bin
//! # Press RESET, then flash within the 2.5s grace period:
//! dfu-util -a 0 -s 0x90040000:leave -D firmware/single_delay.bin
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
use sonido_effects::kernels::{DelayKernel, DelayParams};

#[global_allocator]
static HEAP: Heap = Heap::empty();

static CONTROLS: HothouseBuffer = HothouseBuffer::new();

#[embassy_executor::main]
async fn main(spawner: embassy_executor::Spawner) {
    let config = sonido_daisy::rcc_config(ClockProfile::Performance);
    let p = hal::init(config);

    // D2 SRAM clocks — needed for DMA buffers (.sram1_bss at 0x30000000).
    sonido_daisy::enable_d2_sram();
    sonido_daisy::enable_fpu_ftz();

    // SDRAM heap — delay lines need ~776 KB, exceeds D2 SRAM (256 KB).
    let mut cp = unsafe { cortex_m::Peripherals::steal() };
    let sdram_ptr = sonido_daisy::init_sdram!(p, &mut cp.MPU, &mut cp.SCB);
    unsafe {
        HEAP.init(sdram_ptr as usize, sonido_daisy::sdram::SDRAM_SIZE);
    }

    // Enable D-cache immediately — no audio DMA running yet, so bus matrix
    // stall concern doesn't apply. Required before large SDRAM allocations:
    // uncached vec![0.0; 96000] writes 384 KB through FMC, which crashes
    // without D-cache (cache controller handles burst timing to SDRAM).
    sonido_daisy::sdram::enable_dcache();

    let led = UserLed::new(p.PC7);
    spawner.spawn(heartbeat(led)).unwrap();

    let ctrl = sonido_daisy::hothouse_pins!(p);
    spawner
        .spawn(hothouse_control_task(ctrl, &CONTROLS))
        .unwrap();

    CONTROLS.write_led(0, 1.0);

    // Allocate delay kernel — 757 KB from SDRAM (D-cache now active).
    let mut kernel = DelayKernel::new(SAMPLE_RATE);

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

    defmt::info!("M7: audio interface started — delay active");

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

                let time = CONTROLS.read_knob(0); // K1: Delay time (log mapped by from_knobs)
                let feedback = CONTROLS.read_knob(1); // K2: Feedback
                let diffusion = CONTROLS.read_knob(2); // K3: Diffusion
                let mix = CONTROLS.read_knob(3); // K4: Mix
                let out_level = CONTROLS.read_knob(4); // K5: Output (−20 to +6 dB)
                let fb_lp = CONTROLS.read_knob(5); // K6: Feedback LP filter

                // Toggle 1: ping-pong mode (UP=ping-pong, MID/DN=stereo)
                let ping_pong = match CONTROLS.read_toggle(0) {
                    0 => 1.0, // UP = Ping-pong
                    _ => 0.0, // MID/DN = Stereo
                };

                // No tempo sync, no HP filter — keep it simple
                let fb_hp = 0.0; // HP filter at minimum (20 Hz)
                let sync = 0.0; // No tempo sync
                let division = 0.0; // Unused when sync=0

                let params = DelayParams::from_knobs(
                    time, feedback, mix, ping_pong, fb_lp, fb_hp, diffusion, sync, division,
                    out_level,
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
