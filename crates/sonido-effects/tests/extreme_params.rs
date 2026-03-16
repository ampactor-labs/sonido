//! Extreme parameter tests for all sonido effects.
//!
//! Verifies that every effect produces finite (non-NaN, non-Inf) output when
//! parameters are set to their minimum and maximum values, and when running
//! at extreme sample rates (8 kHz and 192 kHz).

use sonido_core::{Adapter, Effect, ParameterInfo};
use sonido_effects::kernels::{
    BitcrusherKernel, ChorusKernel, CompressorKernel, DelayKernel, DistortionKernel, EqKernel,
    FilterKernel, FlangerKernel, GateKernel, LimiterKernel, PhaserKernel, PreampKernel,
    ReverbKernel, RingModKernel, StageKernel, TapeKernel, TremoloKernel, VibratoKernel, WahKernel,
};

const DEFAULT_SAMPLE_RATE: f32 = 48000.0;
const LOW_SAMPLE_RATE: f32 = 8000.0;
const HIGH_SAMPLE_RATE: f32 = 192000.0;
const NUM_SAMPLES: usize = 1000;

/// Process `NUM_SAMPLES` through an effect and assert all outputs are finite.
fn assert_finite_output(effect: &mut dyn Effect, label: &str) {
    for i in 0..NUM_SAMPLES {
        let input = if i % 3 == 0 {
            0.5
        } else if i % 3 == 1 {
            -0.5
        } else {
            0.0
        };
        let output = effect.process(input);
        assert!(
            output.is_finite(),
            "{}: non-finite output at sample {}: {}",
            label,
            i,
            output
        );
    }
}

/// Set all parameters to their minimum values using ParameterInfo.
fn set_all_params_min(effect: &mut (impl Effect + ParameterInfo)) {
    for i in 0..effect.param_count() {
        if let Some(desc) = effect.param_info(i) {
            effect.set_param(i, desc.min);
        }
    }
}

/// Set all parameters to their maximum values using ParameterInfo.
fn set_all_params_max(effect: &mut (impl Effect + ParameterInfo)) {
    for i in 0..effect.param_count() {
        if let Some(desc) = effect.param_info(i) {
            effect.set_param(i, desc.max);
        }
    }
}

/// Run the full extreme parameter test suite for a single effect.
fn run_extreme_test<E: Effect + ParameterInfo>(name: &str, mut create: impl FnMut(f32) -> E) {
    // Test 1: All params at minimum
    {
        let mut effect = create(DEFAULT_SAMPLE_RATE);
        set_all_params_min(&mut effect);
        assert_finite_output(&mut effect, &format!("{} (all min)", name));
    }

    // Test 2: All params at maximum
    {
        let mut effect = create(DEFAULT_SAMPLE_RATE);
        set_all_params_max(&mut effect);
        assert_finite_output(&mut effect, &format!("{} (all max)", name));
    }

    // Test 3: Low sample rate (8 kHz)
    {
        let mut effect = create(LOW_SAMPLE_RATE);
        assert_finite_output(&mut effect, &format!("{} (8 kHz)", name));
    }

    // Test 4: High sample rate (192 kHz)
    {
        let mut effect = create(HIGH_SAMPLE_RATE);
        assert_finite_output(&mut effect, &format!("{} (192 kHz)", name));
    }

    // Test 5: Low sample rate with all max params
    {
        let mut effect = create(LOW_SAMPLE_RATE);
        set_all_params_max(&mut effect);
        assert_finite_output(&mut effect, &format!("{} (8 kHz, all max)", name));
    }

    // Test 6: High sample rate with all max params
    {
        let mut effect = create(HIGH_SAMPLE_RATE);
        set_all_params_max(&mut effect);
        assert_finite_output(&mut effect, &format!("{} (192 kHz, all max)", name));
    }
}

#[test]
fn test_extreme_distortion() {
    run_extreme_test("Distortion", |sr| {
        Adapter::new(DistortionKernel::new(sr), sr)
    });
}

#[test]
fn test_extreme_compressor() {
    run_extreme_test("Compressor", |sr| {
        Adapter::new(CompressorKernel::new(sr), sr)
    });
}

#[test]
fn test_extreme_chorus() {
    run_extreme_test("Chorus", |sr| Adapter::new(ChorusKernel::new(sr), sr));
}

#[test]
fn test_extreme_delay() {
    run_extreme_test("Delay", |sr| Adapter::new(DelayKernel::new(sr), sr));
}

#[test]
fn test_extreme_reverb() {
    run_extreme_test("Reverb", |sr| Adapter::new(ReverbKernel::new(sr), sr));
}

#[test]
fn test_extreme_lowpass() {
    run_extreme_test("LowPassFilter", |sr| {
        Adapter::new(FilterKernel::new(sr), sr)
    });
}

#[test]
fn test_extreme_phaser() {
    run_extreme_test("Phaser", |sr| Adapter::new(PhaserKernel::new(sr), sr));
}

#[test]
fn test_extreme_flanger() {
    run_extreme_test("Flanger", |sr| Adapter::new(FlangerKernel::new(sr), sr));
}

#[test]
fn test_extreme_tremolo() {
    run_extreme_test("Tremolo", |sr| Adapter::new(TremoloKernel::new(sr), sr));
}

#[test]
fn test_extreme_tape() {
    run_extreme_test("Tape", |sr| Adapter::new(TapeKernel::new(sr), sr));
}

#[test]
fn test_extreme_clean_preamp() {
    run_extreme_test("CleanPreamp", |sr| Adapter::new(PreampKernel::new(sr), sr));
}

#[test]
fn test_extreme_vibrato() {
    run_extreme_test("Vibrato", |sr| Adapter::new(VibratoKernel::new(sr), sr));
}

#[test]
fn test_extreme_gate() {
    run_extreme_test("Gate", |sr| Adapter::new(GateKernel::new(sr), sr));
}

#[test]
fn test_extreme_wah() {
    run_extreme_test("Wah", |sr| Adapter::new(WahKernel::new(sr), sr));
}

#[test]
fn test_extreme_eq() {
    run_extreme_test("Eq", |sr| Adapter::new(EqKernel::new(sr), sr));
}

#[test]
fn test_extreme_limiter() {
    run_extreme_test("Limiter", |sr| Adapter::new(LimiterKernel::new(sr), sr));
}

#[test]
fn test_extreme_bitcrusher() {
    run_extreme_test("Bitcrusher", |sr| {
        Adapter::new(BitcrusherKernel::new(sr), sr)
    });
}

#[test]
fn test_extreme_ringmod() {
    run_extreme_test("RingMod", |sr| Adapter::new(RingModKernel::new(sr), sr));
}

#[test]
fn test_extreme_stage() {
    run_extreme_test("Stage", |sr| Adapter::new(StageKernel::new(sr), sr));
}
