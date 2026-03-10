//! Tier 4: Single effect processing on hardware.
//!
//! Processes audio through a Sonido distortion kernel with parameters mapped
//! from ADC knob readings via `from_knobs()`. This is the first real DSP
//! running on the Daisy Seed / Hothouse platform.
//!
//! # Architecture
//!
//! A single `start_callback` closure handles both audio processing and control
//! polling. The callback runs in the Embassy executor thread (not a hardware
//! ISR), so blocking ADC reads (~1 us each) are safe.
//!
//! - **Audio processing** runs every block (1500 Hz at 48 kHz / 32 samples).
//! - **Control polling** runs every 15th block (~100 Hz): reads 4 ADC knobs,
//!   the 3-position toggle switch, and the bypass footswitch.
//!
//! Knob readings and mode are stored in `AtomicU16` / `AtomicBool` statics so
//! the audio processing section can read them without synchronization overhead.
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
//! Note: we construct `AudioPeripherals` directly instead of using a board macro
//! because the board macro consumes all GPIO pins (including the ones we need for
//! knobs, toggles, and footswitch). Audio only requires SAI1, DMA, and the
//! codec pins (PE2-PE6).
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

use core::sync::atomic::{AtomicBool, AtomicU16, Ordering};

use defmt_rtt as _;
use embassy_stm32 as hal;
use embassy_stm32::adc::{Adc, SampleTime};
use embassy_stm32::gpio::{Input, Level, Output, Pull, Speed};
use embedded_alloc::LlffHeap as Heap;
use panic_probe as _;

use sonido_core::kernel::DspKernel;
use sonido_daisy::{ClockProfile, SAMPLE_RATE, f32_to_u24, heartbeat, led::UserLed, u24_to_f32};
use sonido_effects::kernels::{DistortionKernel, DistortionParams};

// ── Heap allocator (DistortionKernel needs alloc for ADAA state) ─────────

#[global_allocator]
static HEAP: Heap = Heap::empty();

// ── Shared atomic state (control loop -> audio callback) ─────────────────

/// Knob ADC readings, stored as raw u16 (0-65535).
/// The audio callback normalizes to 0.0-1.0 on read.
static KNOB_DRIVE: AtomicU16 = AtomicU16::new(32768);
static KNOB_TONE: AtomicU16 = AtomicU16::new(32768);
static KNOB_OUTPUT: AtomicU16 = AtomicU16::new(32768);
static KNOB_MIX: AtomicU16 = AtomicU16::new(65535);

/// Distortion mode from toggle switch (0=SoftClip, 1=HardClip, 2=Foldback).
static MODE: AtomicU16 = AtomicU16::new(0);

/// True when effect is active (not bypassed).
static ACTIVE: AtomicBool = AtomicBool::new(true);

/// Maximum raw ADC value for 16-bit resolution.
const ADC_MAX: f32 = 65535.0;

/// ADC sample time for knob readings.
///
/// 32.5 cycles gives good accuracy for slowly-varying potentiometer voltages
/// without excessive conversion time.
const KNOB_SAMPLE_TIME: SampleTime = SampleTime::CYCLES32_5;

// ── Main ─────────────────────────────────────────────────────────────────

#[embassy_executor::main]
async fn main(spawner: embassy_executor::Spawner) {
    // Initialize heap at D2 SRAM (256 KB)
    unsafe {
        HEAP.init(0x3000_8000, 256 * 1024);
    }

    let config = sonido_daisy::rcc_config(ClockProfile::Performance);
    let p = hal::init(config);

    let led = UserLed::new(p.PC7);
    spawner.spawn(heartbeat(led)).unwrap();

    defmt::info!("sonido-daisy single_effect: initializing...");

    // ── Extract control pins BEFORE constructing audio peripherals ──
    // The ADC, knob, toggle, footswitch, and LED pins are used by the
    // control loop. Audio only needs SAI1, DMA, and codec pins.

    let mut adc = Adc::new(p.ADC1);
    let mut knob1_pin = p.PA3; // KNOB_1 (Drive)
    let mut knob2_pin = p.PB1; // KNOB_2 (Tone)
    let mut knob3_pin = p.PA6; // KNOB_3 (Output)
    let mut knob4_pin = p.PC1; // KNOB_4 (Mix)

    let tog1_up = Input::new(p.PB4, Pull::Up);   // TOGGLE_1 up
    let tog1_down = Input::new(p.PB5, Pull::Up);  // TOGGLE_1 down
    let footswitch = Input::new(p.PA0, Pull::Up);  // FOOTSWITCH_1
    let mut led = Output::new(p.PA5, Level::High, Speed::Low); // LED_1 (start active)

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

    // ── Start audio + control callback ──
    //
    // The callback runs an infinite async loop: it awaits DMA completion,
    // calls our closure synchronously, then awaits the next block. The
    // closure is FnMut, not async — but it runs in the executor thread,
    // NOT a hardware ISR. This means blocking_read (~1us per knob at
    // CYCLES32_5 on 480 MHz) is safe here.
    //
    // We integrate control polling directly into the callback at a decimated
    // rate: every 15th invocation (~100 Hz at 1500 blocks/sec).
    //
    // Both slices are 64 interleaved u32 values: [L0, R0, L1, R1, ..., L31, R31]

    // Control state for footswitch debounce (closure-private, no atomics needed)
    let mut poll_counter: u16 = 0;
    let mut foot_was_pressed = false;

    defmt::unwrap!(
        interface
            .start_callback(move |input, output| {
                // ── Control polling (every 15th block = ~100 Hz) ──
                poll_counter = poll_counter.wrapping_add(1);

                if poll_counter % 15 == 0 {
                    // Read ADC knobs (blocking_read takes ~1us each — negligible)
                    let raw1: u16 = adc.blocking_read(&mut knob1_pin, KNOB_SAMPLE_TIME);
                    let raw2: u16 = adc.blocking_read(&mut knob2_pin, KNOB_SAMPLE_TIME);
                    let raw3: u16 = adc.blocking_read(&mut knob3_pin, KNOB_SAMPLE_TIME);
                    let raw4: u16 = adc.blocking_read(&mut knob4_pin, KNOB_SAMPLE_TIME);

                    KNOB_DRIVE.store(raw1, Ordering::Relaxed);
                    KNOB_TONE.store(raw2, Ordering::Relaxed);
                    KNOB_OUTPUT.store(raw3, Ordering::Relaxed);
                    KNOB_MIX.store(raw4, Ordering::Relaxed);

                    // Read toggle switch (3-position -> distortion mode)
                    let up_active = tog1_up.is_low();
                    let down_active = tog1_down.is_low();
                    let mode: u16 = match (up_active, down_active) {
                        (true, false) => 0,  // Up = Overdrive (SoftClip)
                        (false, false) => 1, // Middle = Distortion (HardClip)
                        (false, true) => 2,  // Down = Fuzz (Foldback)
                        (true, true) => 1,   // Fault — fall back to middle
                    };
                    MODE.store(mode, Ordering::Relaxed);

                    // Footswitch bypass toggle (fire on release)
                    let foot_pressed = footswitch.is_low();
                    if foot_was_pressed && !foot_pressed {
                        let active = !ACTIVE.load(Ordering::Relaxed);
                        ACTIVE.store(active, Ordering::Relaxed);
                        if active {
                            led.set_high();
                        } else {
                            led.set_low();
                        }
                    }
                    foot_was_pressed = foot_pressed;
                }

                // ── Audio processing ──
                let active = ACTIVE.load(Ordering::Relaxed);

                if !active {
                    // Bypass: copy input directly to output
                    output.copy_from_slice(input);
                    return;
                }

                // Build params from knob readings
                let drive = KNOB_DRIVE.load(Ordering::Relaxed) as f32 / ADC_MAX;
                let tone = KNOB_TONE.load(Ordering::Relaxed) as f32 / ADC_MAX;
                let out_level = KNOB_OUTPUT.load(Ordering::Relaxed) as f32 / ADC_MAX;
                let mix = KNOB_MIX.load(Ordering::Relaxed) as f32 / ADC_MAX;
                let mode = MODE.load(Ordering::Relaxed) as f32 / 3.99;

                // from_knobs maps 0.0-1.0 to parameter ranges
                let params = DistortionParams::from_knobs(drive, tone, out_level, mode, mix);

                // Process 32 stereo sample pairs (64 interleaved u32 values)
                for i in (0..input.len()).step_by(2) {
                    let left_in = u24_to_f32(input[i]);
                    let right_in = u24_to_f32(input[i + 1]);

                    let (left_out, right_out) =
                        kernel.process_stereo(left_in, right_in, &params);

                    output[i] = f32_to_u24(left_out);
                    output[i + 1] = f32_to_u24(right_out);
                }
            })
            .await
    );
}
