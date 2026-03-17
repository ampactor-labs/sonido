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
//! **Note:** Effects with more than 6 parameters will have params beyond
//! index 5 fixed at their noon/default values (unreachable from hardware
//! knobs).
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
use sonido_core::param::SmoothedParam;
use sonido_core::{Effect, ParameterInfo};
use sonido_daisy::controls::HothouseBuffer;
use sonido_daisy::hothouse::hothouse_control_task;
use sonido_daisy::noon_presets;
use sonido_daisy::param_map::adc_to_param_biased;
use sonido_daisy::{ClockProfile, SAMPLE_RATE, f32_to_u24, heartbeat, led::UserLed, u24_to_f32};

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
    let mut effect =
        TestEffect::new_direct(TestKernel::new(SAMPLE_RATE), SAMPLE_RATE);

    // ── Setup Platform Controller & Mapper ──
    let mut mapper = sonido_platform::ControlMapper::<NUM_KNOBS>::new();

    let param_count = effect.param_count();
    let knob_count = param_count.min(NUM_KNOBS);

    // Map each knob to a parameter and log the table
    defmt::info!("{} params, {} knobs:", param_count, knob_count);
    for i in 0..param_count {
        if i < NUM_KNOBS {
            let ctrl_id = sonido_platform::ControlId::hardware(i as u8);
            mapper.map(ctrl_id, i);
            if let Some(d) = effect.param_info(i) {
                defmt::info!("  K{}: [{}] {} ({} .. {})", i + 1, i, d.name, d.min, d.max);
            }
        } else if let Some(d) = effect.param_info(i) {
            defmt::info!("  ~~: [{}] {} (fixed at noon/default)", i, d.name);
        }
    }
    
    if param_count > NUM_KNOBS {
        defmt::warn!(
            "{} params beyond knob {} are fixed at noon/default — not reachable from hardware",
            param_count - NUM_KNOBS,
            NUM_KNOBS
        );
    }

    // ── Audio callback ──
    let mut active = true;
    let mut foot_was_pressed = false;
    let mut poll_counter: u32 = 0;
    
    // Bypass crossfade: 5 ms ramp avoids clicks on engage/disengage
    let mut bypass_mix = SmoothedParam::fast(1.0, SAMPLE_RATE);

    defmt::info!("ready — play guitar");

    defmt::unwrap!(
        interface
            .start_callback(move |input, output| {
                let platform = sonido_daisy::hothouse::HothousePlatform::new(&CONTROLS);
                use sonido_platform::PlatformController;

                // Poll controls decimated to ~100 Hz
                poll_counter += 1;
                if poll_counter >= POLL_EVERY {
                    poll_counter = 0;
                    
                    // Footswitch 1: bypass toggle on press (transition to 1.0)
                    if let Some(state) = platform.read_control(sonido_platform::ControlId::hardware(9)) {
                        let fs_pressed = state.value > 0.5;
                        if !foot_was_pressed && fs_pressed {
                            active = !active;
                            bypass_mix.set_target(if active { 1.0 } else { 0.0 });
                            CONTROLS.write_led(0, if active { 1.0 } else { 0.0 });
                        }
                        foot_was_pressed = fs_pressed;
                    }

                    // Process knob changes with biased scaling
                    for k in 0..knob_count {
                        let ctrl_id = sonido_platform::ControlId::hardware(k as u8);
                        if let Some(state) = platform.read_control(ctrl_id) {
                            mapper.apply_with_fn(ctrl_id, state.value, &mut effect, |desc, norm| {
                                let noon = noon_presets::noon_value(EFFECT_ID, k).unwrap_or(desc.default);
                                adc_to_param_biased(desc, noon, norm)
                            });
                        }
                    }
                }

                // Process audio with bypass crossfade
                for i in (0..input.len()).step_by(2) {
                    let left_in = u24_to_f32(input[i]);
                    let right_in = u24_to_f32(input[i + 1]);

                    let (mut wet_l, mut wet_r) = effect.process_stereo(left_in, right_in);
                    if !wet_l.is_finite() {
                        wet_l = 0.0;
                    }
                    if !wet_r.is_finite() {
                        wet_r = 0.0;
                    }

                    let mix = bypass_mix.advance();
                    let l = left_in + (wet_l - left_in) * mix;
                    let r = right_in + (wet_r - right_in) * mix;

                    output[i] = f32_to_u24(l.clamp(-1.0, 1.0));
                    output[i + 1] = f32_to_u24(r.clamp(-1.0, 1.0));
                }
            })
            .await
    );
}
