//! Generic single-effect test harness for hardware tuning.
//!
//! Change the three lines in the `CHANGE THESE` block to test a different
//! effect.  Only that kernel gets compiled — no registry, no vtable, no
//! smoothing.
//!
//! `Adapter<K, DirectPolicy>` provides `ParameterInfo` (descriptors drive
//! `adc_to_param` automatically) and `Effect` (for `process_stereo`),
//! with `DirectPolicy` — knob changes land instantly.
//!
//! # Switching effects
//!
//! Change the three lines in the marked block (use, kernel alias, effect ID):
//!
//! ```rust,ignore
//! use sonido_effects::kernels::ReverbKernel;
//! type TestKernel = ReverbKernel;
//! const EFFECT_ID: &str = "reverb";
//! ```
//!
//! The `TestEffect` type and constructor derive from `TestKernel` automatically.
//!
//! **Note:** Unmapped knob positions (`NULL_KNOB`) are inactive for effects
//! with fewer than 6 mapped parameters.
//!
//! # Build & Flash
//!
//! ```bash
//! cd crates/sonido-daisy
//! cargo objcopy --example single_effect --release --features alloc,platform -- -O binary -R .sram1_bss single_effect.bin
//! # Press RESET, then flash within 2.5s:
//! dfu-util -a 0 -s 0x90040000:leave -D single_effect.bin
//! ```

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;

use defmt_rtt as _;
use embassy_stm32 as hal;
use embedded_alloc::LlffHeap as Heap;
use panic_probe as _;

use sonido_core::ParameterInfo;
use sonido_core::kernel::{Adapter, DirectPolicy};
use sonido_daisy::controls::HothouseBuffer;
use sonido_daisy::effect_slot::{CONTROL_POLL_EVERY, EffectSlot};
use sonido_daisy::hothouse::hothouse_control_task;
use sonido_daisy::{ClockProfile, SAMPLE_RATE, f32_to_u24, heartbeat, led::UserLed, u24_to_f32};
use sonido_platform::knob_mapping::{NULL_KNOB, knob_map};

// ═══════════════════════════════════════════════════════════════════════════
//  CHANGE THESE 3 LINES to test a different effect:
// ═══════════════════════════════════════════════════════════════════════════
use sonido_effects::kernels::DistortionKernel;
type TestKernel = DistortionKernel;
const EFFECT_ID: &str = "distortion";
// ═══════════════════════════════════════════════════════════════════════════

type TestEffect = Adapter<TestKernel, DirectPolicy>;

/// Number of Hothouse knobs.
const NUM_KNOBS: usize = 6;

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

    // ── Create effect slot (monomorphized kernel, boxed for EffectSlot) ──
    let effect = TestEffect::new_direct(TestKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let param_count = effect.param_count();

    // Log the knob mapping table
    let map = knob_map(EFFECT_ID).unwrap_or([0, 1, 2, 3, 4, 5]);
    defmt::info!("{} params, knob map:", param_count);
    for (k, &pidx) in map.iter().enumerate() {
        if pidx == NULL_KNOB {
            defmt::info!("  K{}: --", k + 1);
        } else if let Some(d) = effect.param_info(pidx as usize) {
            defmt::info!(
                "  K{}: [{}] {} ({} .. {})",
                k + 1,
                pidx,
                d.name,
                d.min,
                d.max
            );
        }
    }

    let mut slot = EffectSlot::new(Box::new(effect), EFFECT_ID, SAMPLE_RATE);

    // ── Audio callback ──
    let mut foot_was_pressed = false;
    let mut poll_counter: u16 = 0;

    defmt::info!("ready — play guitar");

    defmt::unwrap!(
        interface
            .start_callback(move |input, output| {
                // Poll controls decimated to ~100 Hz
                poll_counter += 1;
                if poll_counter >= CONTROL_POLL_EVERY {
                    poll_counter = 0;

                    // Footswitch 1: bypass toggle on press
                    let fs_pressed = CONTROLS.read_footswitch(0);
                    if !foot_was_pressed && fs_pressed {
                        let now_active = !slot.is_active();
                        slot.set_active(now_active);
                        CONTROLS.write_led(0, if now_active { 1.0 } else { 0.0 });
                    }
                    foot_was_pressed = fs_pressed;

                    // Read all 6 knobs and apply via EffectSlot
                    let mut knob_vals = [0.0f32; NUM_KNOBS];
                    for k in 0..NUM_KNOBS {
                        knob_vals[k] = CONTROLS.read_knob(k);
                    }
                    slot.apply_knobs(&knob_vals);
                }

                // Process audio: effect → sanitize → bypass crossfade
                for i in (0..input.len()).step_by(2) {
                    let left_in = u24_to_f32(input[i]);
                    let right_in = u24_to_f32(input[i + 1]);

                    let (l, r) = slot.process_stereo(left_in, right_in);

                    output[i] = f32_to_u24(l.clamp(-1.0, 1.0));
                    output[i + 1] = f32_to_u24(r.clamp(-1.0, 1.0));
                }
            })
            .await
    );
}
