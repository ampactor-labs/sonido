//! Criterion benchmarks for sonido effects (kernel architecture)
//!
//! Run with: cargo bench
#![allow(missing_docs)]

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use sonido_core::{Adapter, Effect, EffectExt, Oversampled, ParameterInfo};
use sonido_effects::kernels::{
    BitcrusherKernel, ChorusKernel, CompressorKernel, DelayKernel, DistortionKernel, EqKernel,
    FilterKernel, FlangerKernel, GateKernel, LimiterKernel, PhaserKernel, PreampKernel,
    ReverbKernel, RingModKernel, StageKernel, TapeKernel, TremoloKernel, VibratoKernel, WahKernel,
};

const SAMPLE_RATE: f32 = 48000.0;
const BLOCK_SIZES: &[usize] = &[64, 128, 256, 512, 1024];

fn generate_test_signal(size: usize) -> Vec<f32> {
    (0..size)
        .map(|i| {
            let t = i as f32 / SAMPLE_RATE;
            (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5
        })
        .collect()
}

fn bench_effect<E: Effect>(c: &mut Criterion, name: &str, mut effect: E) {
    let mut group = c.benchmark_group(name);

    for &block_size in BLOCK_SIZES {
        let input = generate_test_signal(block_size);

        group.bench_with_input(
            BenchmarkId::from_parameter(block_size),
            &block_size,
            |b, _| {
                let mut output = vec![0.0; block_size];
                b.iter(|| {
                    effect.process_block(black_box(&input), &mut output);
                    black_box(output[0])
                })
            },
        );
    }

    group.finish();
}

fn bench_distortion(c: &mut Criterion) {
    let mut effect = Adapter::new(DistortionKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 20.0); // drive
    effect.set_param(1, 3.0); // tone
    bench_effect(c, "Distortion", effect);
}

fn bench_compressor(c: &mut Criterion) {
    let mut effect = Adapter::new(CompressorKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, -20.0); // threshold
    effect.set_param(1, 4.0); // ratio
    effect.set_param(2, 5.0); // attack
    effect.set_param(3, 50.0); // release
    bench_effect(c, "Compressor", effect);
}

fn bench_chorus(c: &mut Criterion) {
    let mut effect = Adapter::new(ChorusKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 2.0); // rate
    effect.set_param(1, 70.0); // depth (percent)
    effect.set_param(2, 50.0); // mix (percent)
    bench_effect(c, "Chorus", effect);
}

fn bench_delay(c: &mut Criterion) {
    let mut effect = Adapter::new(DelayKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 375.0); // time_ms
    effect.set_param(1, 50.0); // feedback (percent)
    effect.set_param(2, 30.0); // mix (percent)
    bench_effect(c, "Delay", effect);
}

fn bench_lowpass(c: &mut Criterion) {
    let mut effect = Adapter::new(FilterKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 1000.0); // cutoff
    effect.set_param(1, 0.707); // resonance
    bench_effect(c, "LowPassFilter", effect);
}

fn bench_vibrato(c: &mut Criterion) {
    let mut effect = Adapter::new(VibratoKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(1, 100.0); // mix (percent)
    effect.set_param(0, 100.0); // depth (percent)
    bench_effect(c, "Vibrato", effect);
}

fn bench_tape(c: &mut Criterion) {
    let mut effect = Adapter::new(TapeKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 6.0); // drive (dB, range 0-24, default 6)
    effect.set_param(1, 60.0); // saturation (percent)
    bench_effect(c, "Tape", effect);
}

fn bench_clean_preamp(c: &mut Criterion) {
    let mut effect = Adapter::new(PreampKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 12.0); // gain_db
    effect.set_param(2, -6.0); // output_db
    bench_effect(c, "CleanPreamp", effect);
}

fn bench_reverb(c: &mut Criterion) {
    let mut effect = Adapter::new(ReverbKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 70.0); // room_size (percent)
    effect.set_param(1, 80.0); // decay (percent)
    effect.set_param(2, 30.0); // damping (percent)
    effect.set_param(3, 15.0); // predelay_ms
    effect.set_param(4, 50.0); // mix (percent)
    bench_effect(c, "Reverb", effect);
}

fn bench_flanger(c: &mut Criterion) {
    let mut effect = Adapter::new(FlangerKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 0.5); // rate
    effect.set_param(1, 70.0); // depth (percent)
    effect.set_param(2, 50.0); // feedback (percent)
    effect.set_param(3, 50.0); // mix (percent)
    bench_effect(c, "Flanger", effect);
}

fn bench_phaser(c: &mut Criterion) {
    let mut effect = Adapter::new(PhaserKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 1.0); // rate
    effect.set_param(1, 80.0); // depth (percent)
    effect.set_param(2, 6.0); // stages
    effect.set_param(3, 30.0); // feedback (percent)
    effect.set_param(4, 50.0); // mix (percent)
    bench_effect(c, "Phaser", effect);
}

fn bench_gate(c: &mut Criterion) {
    let mut effect = Adapter::new(GateKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, -40.0); // threshold
    effect.set_param(1, 1.0); // attack
    effect.set_param(2, 50.0); // release
    effect.set_param(3, 10.0); // hold
    bench_effect(c, "Gate", effect);
}

fn bench_tremolo(c: &mut Criterion) {
    let mut effect = Adapter::new(TremoloKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 5.0); // rate
    effect.set_param(1, 80.0); // depth (percent)
    bench_effect(c, "Tremolo", effect);
}

fn bench_wah(c: &mut Criterion) {
    let mut effect = Adapter::new(WahKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    // Wah param 0 is frequency, param 1 is resonance, param 2 is sensitivity
    // Classic: set_sensitivity(0.7) maps to sensitivity at index 2 (range 0-100%)
    effect.set_param(2, 70.0); // sensitivity (percent)
    bench_effect(c, "Wah", effect);
}

fn bench_eq(c: &mut Criterion) {
    let mut effect = Adapter::new(EqKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 200.0); // low_freq
    effect.set_param(1, 3.0); // low_gain
    effect.set_param(2, 1.0); // low_q
    effect.set_param(3, 1000.0); // mid_freq
    effect.set_param(4, -2.0); // mid_gain
    effect.set_param(5, 1.5); // mid_q
    effect.set_param(6, 4000.0); // high_freq
    effect.set_param(7, 2.0); // high_gain
    effect.set_param(8, 1.0); // high_q
    bench_effect(c, "Eq", effect);
}

fn bench_limiter(c: &mut Criterion) {
    let mut effect = Adapter::new(LimiterKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, -6.0); // threshold
    effect.set_param(1, -0.3); // ceiling
    effect.set_param(2, 100.0); // release
    bench_effect(c, "Limiter", effect);
}

fn bench_bitcrusher(c: &mut Criterion) {
    let mut effect = Adapter::new(BitcrusherKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 8.0); // bit_depth
    effect.set_param(1, 4.0); // downsample
    bench_effect(c, "Bitcrusher", effect);
}

fn bench_ringmod(c: &mut Criterion) {
    let mut effect = Adapter::new(RingModKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 440.0); // frequency
    effect.set_param(1, 100.0); // depth (percent)
    effect.set_param(3, 50.0); // mix (percent)
    bench_effect(c, "RingMod", effect);
}

fn bench_stage(c: &mut Criterion) {
    let mut effect = Adapter::new(StageKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 6.0); // gain_db
    effect.set_param(1, 120.0); // width (percent)
    bench_effect(c, "Stage", effect);
}

// --- Stereo benchmarks ---

fn generate_stereo_test_signals(size: usize) -> (Vec<f32>, Vec<f32>) {
    let left: Vec<f32> = (0..size)
        .map(|i| {
            let t = i as f32 / SAMPLE_RATE;
            (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5
        })
        .collect();
    let right: Vec<f32> = (0..size)
        .map(|i| {
            let t = i as f32 / SAMPLE_RATE;
            (2.0 * std::f32::consts::PI * 440.0 * t + std::f32::consts::FRAC_PI_3).sin() * 0.5
        })
        .collect();
    (left, right)
}

fn bench_stereo_effect<E: Effect>(c: &mut Criterion, name: &str, mut effect: E) {
    let mut group = c.benchmark_group(name);

    for &block_size in BLOCK_SIZES {
        let (left_in, right_in) = generate_stereo_test_signals(block_size);

        group.bench_with_input(
            BenchmarkId::from_parameter(block_size),
            &block_size,
            |b, &size| {
                let mut left_out = vec![0.0; size];
                let mut right_out = vec![0.0; size];
                b.iter(|| {
                    effect.process_block_stereo(
                        black_box(&left_in),
                        black_box(&right_in),
                        &mut left_out,
                        &mut right_out,
                    );
                    black_box((left_out[0], right_out[0]))
                })
            },
        );
    }

    group.finish();
}

fn bench_stereo_chorus(c: &mut Criterion) {
    let mut effect = Adapter::new(ChorusKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 2.0); // rate
    effect.set_param(1, 70.0); // depth (percent)
    effect.set_param(2, 50.0); // mix (percent)
    bench_stereo_effect(c, "Chorus_Stereo", effect);
}

fn bench_stereo_reverb(c: &mut Criterion) {
    let mut effect = Adapter::new(ReverbKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 70.0); // room_size (percent)
    effect.set_param(1, 80.0); // decay (percent)
    effect.set_param(2, 30.0); // damping (percent)
    effect.set_param(3, 15.0); // predelay_ms
    effect.set_param(4, 50.0); // mix (percent)
    bench_stereo_effect(c, "Reverb_Stereo", effect);
}

fn bench_stereo_phaser(c: &mut Criterion) {
    let mut effect = Adapter::new(PhaserKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 1.0); // rate
    effect.set_param(1, 80.0); // depth (percent)
    effect.set_param(2, 6.0); // stages
    effect.set_param(3, 30.0); // feedback (percent)
    effect.set_param(4, 50.0); // mix (percent)
    bench_stereo_effect(c, "Phaser_Stereo", effect);
}

fn bench_stereo_flanger(c: &mut Criterion) {
    let mut effect = Adapter::new(FlangerKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 0.5); // rate
    effect.set_param(1, 70.0); // depth (percent)
    effect.set_param(2, 50.0); // feedback (percent)
    effect.set_param(3, 50.0); // mix (percent)
    bench_stereo_effect(c, "Flanger_Stereo", effect);
}

fn bench_stereo_delay(c: &mut Criterion) {
    let mut effect = Adapter::new(DelayKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 375.0); // time_ms
    effect.set_param(1, 50.0); // feedback (percent)
    effect.set_param(2, 30.0); // mix (percent)
    effect.set_param(3, 1.0); // ping_pong (on)
    bench_stereo_effect(c, "Delay_Stereo_PingPong", effect);
}

// --- Oversampling benchmarks ---

fn bench_oversampling(c: &mut Criterion) {
    // Inner effect created at base rate — Oversampled::new() handles the Nx rate internally
    let dist_2x = Oversampled::<2, _>::new(
        Adapter::new(DistortionKernel::new(SAMPLE_RATE), SAMPLE_RATE),
        SAMPLE_RATE,
    );
    bench_effect(c, "Oversampled_2x_Distortion", dist_2x);

    let dist_4x = Oversampled::<4, _>::new(
        Adapter::new(DistortionKernel::new(SAMPLE_RATE), SAMPLE_RATE),
        SAMPLE_RATE,
    );
    bench_effect(c, "Oversampled_4x_Distortion", dist_4x);

    let dist_8x = Oversampled::<8, _>::new(
        Adapter::new(DistortionKernel::new(SAMPLE_RATE), SAMPLE_RATE),
        SAMPLE_RATE,
    );
    bench_effect(c, "Oversampled_8x_Distortion", dist_8x);
}

fn bench_effect_chain(c: &mut Criterion) {
    let mut group = c.benchmark_group("EffectChain");

    // Typical guitar chain: preamp -> distortion -> chorus -> delay
    let preamp = {
        let mut p = Adapter::new(PreampKernel::new(SAMPLE_RATE), SAMPLE_RATE);
        p.set_param(0, 6.0); // gain_db
        p
    };
    let distortion = {
        let mut d = Adapter::new(DistortionKernel::new(SAMPLE_RATE), SAMPLE_RATE);
        d.set_param(0, 12.0); // drive
        d.set_param(1, 3.0); // tone
        d
    };
    let chorus = {
        let mut c = Adapter::new(ChorusKernel::new(SAMPLE_RATE), SAMPLE_RATE);
        c.set_param(0, 1.5); // rate
        c.set_param(1, 50.0); // depth (percent)
        c.set_param(2, 30.0); // mix (percent)
        c
    };
    let delay = {
        let mut d = Adapter::new(DelayKernel::new(SAMPLE_RATE), SAMPLE_RATE);
        d.set_param(0, 300.0); // time_ms
        d.set_param(1, 40.0); // feedback (percent)
        d.set_param(2, 25.0); // mix (percent)
        d
    };

    let mut chain = preamp.chain(distortion).chain(chorus).chain(delay);

    for &block_size in BLOCK_SIZES {
        let input = generate_test_signal(block_size);

        group.bench_with_input(
            BenchmarkId::from_parameter(block_size),
            &block_size,
            |b, _| {
                let mut output = vec![0.0; block_size];
                b.iter(|| {
                    chain.process_block(black_box(&input), &mut output);
                    black_box(output[0])
                })
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_distortion,
    bench_compressor,
    bench_chorus,
    bench_delay,
    bench_lowpass,
    bench_vibrato,
    bench_tape,
    bench_clean_preamp,
    bench_reverb,
    bench_flanger,
    bench_phaser,
    bench_gate,
    bench_tremolo,
    bench_wah,
    bench_eq,
    bench_limiter,
    bench_bitcrusher,
    bench_ringmod,
    bench_stage,
    bench_stereo_chorus,
    bench_stereo_reverb,
    bench_stereo_phaser,
    bench_stereo_flanger,
    bench_stereo_delay,
    bench_oversampling,
    bench_effect_chain,
);

criterion_main!(benches);
