//! Tier 4: Single effect processing on hardware.
//!
//! Processes audio through a Sonido distortion kernel with parameters mapped
//! from ADC knob readings via `from_knobs()`. This is the first real DSP
//! running on the Daisy Seed / Hothouse platform.
//!
//! # Architecture
//!
//! ADC and GPIO reads run in [`hothouse_control_task`] at 50 Hz, separate
//! from the audio DMA callback. Shared state flows through a lock-free
//! [`HothouseBuffer`]. The audio callback reads smoothed knob values and
//! processes audio through a `DistortionKernel`.
//!
//! # Hardware Mapping
//!
//! | Control      | Pin(s)       | Function                                      |
//! |--------------|--------------|-----------------------------------------------|
//! | KNOB_1       | PA3          | Drive (0-40 dB)                               |
//! | KNOB_2       | PB1          | Tone (-12 to 12 dB)                           |
//! | KNOB_3       | PA6          | Output level (-20 to 20 dB)                   |
//! | KNOB_4       | PC1          | Mix / dry-wet (0-100%)                        |
//! | TOGGLE_1 up  | PB4          | Distortion mode: Up=Overdrive (SoftClip)      |
//! | TOGGLE_1 mid | (neither)    | Distortion mode: Mid=Distortion (HardClip)    |
//! | TOGGLE_1 dn  | PB5          | Distortion mode: Down=Fuzz (Foldback)         |
//! | FOOTSWITCH_1 | PA0 (pull-up)| Bypass toggle on release                      |
//! | LED_1        | PA5          | Active (on) / Bypassed (off)                  |
//!
//! # Build & Flash
//!
//! ```bash
//! cd crates/sonido-daisy
//! cargo objcopy --example single_effect --release -- -O binary -R .sram1_bss single_effect.bin
//! # Press RESET, then flash within the 2.5s grace period:
//! dfu-util -a 0 -s 0x90040000:leave -D single_effect.bin
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
use sonido_effects::kernels::{DistortionKernel, DistortionParams};

// ── Heap allocator (DistortionKernel needs alloc for ADAA state) ─────────

#[global_allocator]
static HEAP: Heap = Heap::empty();

// ── Shared control buffer ────────────────────────────────────────────────

static CONTROLS: HothouseBuffer = HothouseBuffer::new();

// ── Main ─────────────────────────────────────────────────────────────────

#[embassy_executor::main]
async fn main(spawner: embassy_executor::Spawner) {
    // D2 SRAM clocks are disabled at reset — enable before heap init.
    sonido_daisy::enable_d2_sram();
    sonido_daisy::enable_fpu_ftz();

    // Initialize heap at D2 SRAM (256 KB)
    unsafe {
        HEAP.init(0x3000_8000, 256 * 1024);
    }

    let config = sonido_daisy::rcc_config(ClockProfile::Performance);
    let p = hal::init(config);

    let led = UserLed::new(p.PC7);
    spawner.spawn(heartbeat(led)).unwrap();

    defmt::info!("sonido-daisy single_effect: initializing...");

    // ── Extract control pins and spawn control task ──
    // Must happen BEFORE constructing audio peripherals (both consume from p).
    let ctrl = sonido_daisy::hothouse_pins!(p);
    spawner
        .spawn(hothouse_control_task(ctrl, &CONTROLS))
        .unwrap();

    // LED1 starts on (effect active). Write via ControlBuffer.
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

    defmt::info!("audio interface started — distortion effect active");

    // ── Create distortion kernel (captured by audio callback closure) ──
    let mut kernel = DistortionKernel::new(SAMPLE_RATE);

    // ── Start audio callback ──
    // Audio processing and control reads are cleanly separated:
    // - hothouse_control_task reads ADC/GPIO at 50 Hz → ControlBuffer
    // - Audio callback reads ControlBuffer (lock-free) → processes audio
    let mut active = true;
    let mut foot_was_pressed = false;

    defmt::unwrap!(
        interface
            .start_callback(move |input, output| {
                // ── Read controls from ControlBuffer ──
                // Footswitch bypass toggle (fire on release)
                let foot_pressed = CONTROLS.read_footswitch(0);
                if foot_was_pressed && !foot_pressed {
                    active = !active;
                    CONTROLS.write_led(0, if active { 1.0 } else { 0.0 });
                }
                foot_was_pressed = foot_pressed;

                if !active {
                    // Bypass: copy input directly to output
                    output.copy_from_slice(input);
                    return;
                }

                // Read knob values (0.0–1.0, IIR-smoothed by control task)
                // HothouseControls order: 0=PA3, 1=PB1, 2=PA7, 3=PA6, 4=PC1
                let drive = CONTROLS.read_knob(0); // K1=PA3
                let tone = CONTROLS.read_knob(1); // K2=PB1
                let out_level = CONTROLS.read_knob(3); // K4=PA6 (Output level)
                let mix = CONTROLS.read_knob(4); // K5=PC1 (Mix)

                // Read toggle switch: 0=UP, 1=MID, 2=DN → mode 0/1/2
                let toggle = CONTROLS.read_toggle(0);
                let mode = match toggle {
                    0 => 0.0 / 3.99, // UP = Overdrive (SoftClip)
                    2 => 2.0 / 3.99, // DN = Fuzz (Foldback)
                    _ => 1.0 / 3.99, // MID = Distortion (HardClip)
                };

                // from_knobs maps 0.0-1.0 to parameter ranges
                let params = DistortionParams::from_knobs(drive, tone, out_level, mode, mix);

                // Process 32 stereo sample pairs (64 interleaved u32 values)
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
