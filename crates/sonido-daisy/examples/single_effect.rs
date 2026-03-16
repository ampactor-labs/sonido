//! Generic single-effect test harness for hardware tuning.
//!
//! Change the `use` + `type Effect` lines to test a different effect.
//! Only that kernel gets compiled — no registry, no vtable, no smoothing.
//!
//! `Adapter<K, DirectPolicy>` provides `ParameterInfo` (descriptors drive
//! `adc_to_param` automatically) and `Effect` (for `process_stereo`),
//! with `DirectPolicy` — knob changes land instantly.
//!
//! # Switching effects
//!
//! Change all three lines in the marked block:
//!
//! ```rust,ignore
//! use sonido_effects::kernels::ReverbKernel;
//! type TestEffect = Adapter<ReverbKernel, DirectPolicy>;
//! // ... and in main():
//! let mut effect = TestEffect::new_direct(ReverbKernel::new(SAMPLE_RATE), SAMPLE_RATE);
//! ```
//!
//! # Build & Flash
//!
//! ```bash
//! cd crates/sonido-daisy
//! cargo objcopy --example single_effect --release --features alloc -- -O binary -R .sram1_bss single_effect.bin
//! # Press RESET, then flash within 2.5s:
//! dfu-util -a 0 -s 0x90040000:leave -D single_effect.bin
//! ```

#![no_std]
#![no_main]

extern crate alloc;

use defmt_rtt as _;
use embassy_stm32 as hal;
use embedded_alloc::LlffHeap as Heap;
use panic_probe as _;

use sonido_core::kernel::{Adapter, DirectPolicy};
use sonido_core::{Effect, ParameterInfo};
use sonido_daisy::controls::HothouseBuffer;
use sonido_daisy::hothouse::hothouse_control_task;
use sonido_daisy::noon_presets;
use sonido_daisy::param_map::adc_to_param_biased;
use sonido_daisy::{ClockProfile, SAMPLE_RATE, f32_to_u24, heartbeat, led::UserLed, u24_to_f32};

// ═══════════════════════════════════════════════════════════════════════════
//  CHANGE THESE LINES to test a different effect (and the constructor below):
// ═══════════════════════════════════════════════════════════════════════════
use sonido_effects::kernels::DistortionKernel;
type TestEffect = Adapter<DistortionKernel, DirectPolicy>;
const EFFECT_ID: &str = "distortion";
// ═══════════════════════════════════════════════════════════════════════════

/// Number of Hothouse knobs.
const NUM_KNOBS: usize = 6;

/// Max params we cache descriptors for.
const MAX_PARAMS: usize = 16;

/// Control poll decimation: every 15th block ≈ 100 Hz at 48kHz/32.
const POLL_EVERY: u32 = 15;

#[global_allocator]
static HEAP: Heap = Heap::empty();

static CONTROLS: HothouseBuffer = HothouseBuffer::new();

#[embassy_executor::main]
async fn main(spawner: embassy_executor::Spawner) {
    sonido_daisy::enable_d2_sram();
    sonido_daisy::enable_fpu_ftz();

    #[allow(unsafe_code)]
    unsafe {
        HEAP.init(0x3000_8000, 256 * 1024);
    }

    let config = sonido_daisy::rcc_config(ClockProfile::Performance);
    let p = hal::init(config);

    let led = UserLed::new(p.PC7);
    spawner.spawn(heartbeat(led)).unwrap();

    defmt::info!("single_effect: booting...");

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

    // ── Create effect (monomorphized, zero smoothing) ──
    let mut effect = TestEffect::new_direct(DistortionKernel::new(SAMPLE_RATE), SAMPLE_RATE);

    let param_count = effect.param_count();
    let knob_count = param_count.min(NUM_KNOBS);

    // Cache descriptors for adc_to_param
    defmt::assert!(
        param_count <= MAX_PARAMS,
        "effect has {} params, max is {}",
        param_count,
        MAX_PARAMS
    );
    let mut descs = [None; MAX_PARAMS];
    for i in 0..param_count {
        descs[i] = effect.param_info(i);
    }

    // Log param table
    defmt::info!("{} params, {} knobs:", param_count, knob_count);
    for i in 0..param_count.min(MAX_PARAMS) {
        if let Some(ref d) = descs[i] {
            if i < NUM_KNOBS {
                defmt::info!("  K{}: [{}] {} ({} .. {})", i + 1, i, d.name, d.min, d.max);
            } else {
                defmt::info!("  --: [{}] {} ({} .. {})", i, d.name, d.min, d.max);
            }
        }
    }

    // ── Audio callback ──
    let mut active = true;
    let mut foot_was_pressed = false;
    let mut poll_counter: u32 = 0;
    let mut prev_raw = [0.0f32; NUM_KNOBS];

    defmt::info!("ready — play guitar");

    defmt::unwrap!(
        interface
            .start_callback(move |input, output| {
                // Footswitch: bypass toggle on release
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

                // Read knobs → params (decimated to ~100 Hz)
                poll_counter += 1;
                if poll_counter >= POLL_EVERY {
                    poll_counter = 0;

                    for k in 0..knob_count {
                        if let Some(ref desc) = descs[k] {
                            let raw = CONTROLS.read_knob(k);
                            let noon =
                                noon_presets::noon_value(EFFECT_ID, k).unwrap_or(desc.default);
                            let value = adc_to_param_biased(desc, noon, raw);
                            effect.set_param(k, value);

                            prev_raw[k] = raw;
                        }
                    }
                }

                // Process audio
                for i in (0..input.len()).step_by(2) {
                    let left_in = u24_to_f32(input[i]);
                    let right_in = u24_to_f32(input[i + 1]);

                    let (mut l, mut r) = effect.process_stereo(left_in, right_in);

                    if !l.is_finite() {
                        l = 0.0;
                    }
                    if !r.is_finite() {
                        r = 0.0;
                    }

                    output[i] = f32_to_u24(l.clamp(-1.0, 1.0));
                    output[i + 1] = f32_to_u24(r.clamp(-1.0, 1.0));
                }
            })
            .await
    );
}
