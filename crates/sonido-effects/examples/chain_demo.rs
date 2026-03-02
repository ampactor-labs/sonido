//! Demonstration of effect chaining in Sonido (kernel architecture)
//!
//! This example shows both static dispatch (zero-cost) and dynamic dispatch
//! (runtime flexibility) approaches to chaining effects.
//!
//! Run with: cargo run --example chain_demo

use sonido_core::{Effect, EffectExt, KernelAdapter, ParameterInfo};
use sonido_effects::kernels::{
    ChorusKernel, CompressorKernel, DelayKernel, DistortionKernel, FilterKernel, PreampKernel,
    TapeKernel,
};

const SAMPLE_RATE: f32 = 48000.0;

fn main() {
    println!("Sonido Effect Chain Demo");
    println!("========================\n");

    // Generate a test signal (440 Hz sine wave)
    let test_signal: Vec<f32> = (0..4800)
        .map(|i| {
            let t = i as f32 / SAMPLE_RATE;
            (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5
        })
        .collect();

    // Example 1: Static dispatch chain (zero-cost abstraction)
    println!("1. Static Dispatch Chain (Compile-time, Zero-Cost)");
    println!("-------------------------------------------------");

    let preamp = {
        let mut p = KernelAdapter::new(PreampKernel::new(SAMPLE_RATE), SAMPLE_RATE);
        p.set_param(0, 6.0); // gain_db
        p
    };

    let distortion = {
        let mut d = KernelAdapter::new(DistortionKernel::new(SAMPLE_RATE), SAMPLE_RATE);
        d.set_param(0, 12.0); // drive
        d.set_param(3, 0.0); // waveshape: SoftClip
        d.set_param(1, 3.0); // tone
        d
    };

    let tape = {
        let mut t = KernelAdapter::new(TapeKernel::new(SAMPLE_RATE), SAMPLE_RATE);
        t.set_param(0, 6.0); // drive (dB, range 0-24)
        t.set_param(1, 40.0); // saturation (percent)
        t
    };

    let chorus = {
        let mut c = KernelAdapter::new(ChorusKernel::new(SAMPLE_RATE), SAMPLE_RATE);
        c.set_param(0, 1.2); // rate
        c.set_param(1, 50.0); // depth (percent)
        c.set_param(2, 30.0); // mix (percent)
        c
    };

    let delay = {
        let mut d = KernelAdapter::new(DelayKernel::new(SAMPLE_RATE), SAMPLE_RATE);
        d.set_param(0, 375.0); // time_ms
        d.set_param(1, 40.0); // feedback (percent)
        d.set_param(2, 25.0); // mix (percent)
        d
    };

    // Chain using the EffectExt trait - all resolved at compile time
    let mut static_chain = preamp
        .chain(distortion)
        .chain(tape)
        .chain(chorus)
        .chain(delay);

    let mut output = vec![0.0; test_signal.len()];
    static_chain.process_block(&test_signal, &mut output);

    // Calculate RMS of output
    let rms: f32 = (output.iter().map(|x| x * x).sum::<f32>() / output.len() as f32).sqrt();
    println!("Chain: Preamp -> Distortion -> Tape -> Chorus -> Delay");
    println!("Output RMS: {:.4}", rms);
    println!(
        "Peak: {:.4}",
        output.iter().map(|x| x.abs()).fold(0.0_f32, f32::max)
    );
    println!("Latency: {} samples\n", static_chain.latency_samples());

    // Example 2: Dynamic dispatch chain (runtime flexibility)
    println!("2. Dynamic Dispatch Chain (Runtime Flexibility)");
    println!("-----------------------------------------------");

    let mut dynamic_chain: Vec<Box<dyn Effect>> = vec![
        Box::new({
            let mut f = KernelAdapter::new(FilterKernel::new(SAMPLE_RATE), SAMPLE_RATE);
            f.set_param(0, 2000.0); // cutoff
            f.set_param(1, 1.0); // resonance
            f
        }),
        Box::new({
            let mut c = KernelAdapter::new(CompressorKernel::new(SAMPLE_RATE), SAMPLE_RATE);
            c.set_param(0, -18.0); // threshold
            c.set_param(1, 4.0); // ratio
            c.set_param(2, 5.0); // attack
            c.set_param(3, 100.0); // release
            c
        }),
        Box::new({
            let mut d = KernelAdapter::new(DelayKernel::new(SAMPLE_RATE), SAMPLE_RATE);
            d.set_param(0, 250.0); // time_ms
            d.set_param(1, 30.0); // feedback (percent)
            d.set_param(2, 20.0); // mix (percent)
            d
        }),
    ];

    // Process with dynamic dispatch
    let mut output2 = test_signal.clone();
    for sample in output2.iter_mut() {
        for effect in dynamic_chain.iter_mut() {
            *sample = effect.process(*sample);
        }
    }

    let rms2: f32 = (output2.iter().map(|x| x * x).sum::<f32>() / output2.len() as f32).sqrt();
    println!("Chain: LowPass -> Compressor -> Delay");
    println!("Output RMS: {:.4}", rms2);
    println!(
        "Peak: {:.4}\n",
        output2.iter().map(|x| x.abs()).fold(0.0_f32, f32::max)
    );

    // Example 3: Individual effect demonstration
    println!("3. Individual Effects");
    println!("--------------------");

    let effects_info = [
        ("CleanPreamp", "High-headroom, zero-latency preamp"),
        (
            "Distortion",
            "4 waveshapes: soft/hard clip, foldback, asymmetric",
        ),
        ("Tape", "Asymmetric saturation with HF rolloff"),
        ("Compressor", "Soft-knee dynamics with envelope follower"),
        ("Chorus", "Dual-voice modulated delay"),
        ("Delay", "Feedback delay with smooth parameter changes"),
        ("LowPassFilter", "SVF-based resonant filter"),
    ];

    for (name, desc) in effects_info {
        println!("  {} - {}", name, desc);
    }

    println!("\n4. Tape Vibrato (Oxide Original)");
    println!("-------------------------------------");
    println!("  10 simultaneous subtle vibratos at different frequencies");
    println!("  Simulates authentic tape wow and flutter");
    println!("  Latency: 128 samples (~2.7ms at 48kHz)");

    println!("\nDemo complete!");
}
