//! Automated DSP Quality Standard compliance tests (Rules 1-7).
//!
//! Validates every registered effect against the quality rules defined in
//! `docs/DSP_QUALITY_STANDARD.md`. Uses the effect registry to iterate all
//! effects programmatically.
//!
//! Tests are organized by strictness tier:
//! - Universal rules (all 19 effects): R1 bounded, R3, R5, R6, R7
//! - Selective rules (subset of effects): R1 strict, R2, R4

use sonido_core::ParamUnit;
use sonido_registry::{EffectRegistry, EffectWithParams};

const SAMPLE_RATE: f32 = 48000.0;

/// Generate a sine wave at the given frequency and duration.
fn generate_sine(sample_rate: f32, freq_hz: f32, duration_s: f32) -> Vec<f32> {
    let num_samples = (sample_rate * duration_s) as usize;
    (0..num_samples)
        .map(|i| {
            let t = i as f32 / sample_rate;
            (2.0 * std::f32::consts::PI * freq_hz * t).sin()
        })
        .collect()
}

/// Compute peak absolute value of a signal.
fn peak(signal: &[f32]) -> f32 {
    signal.iter().copied().map(f32::abs).fold(0.0f32, f32::max)
}

/// Compute RMS of a signal.
fn rms(signal: &[f32]) -> f32 {
    if signal.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = signal.iter().map(|s| s * s).sum();
    (sum_sq / signal.len() as f32).sqrt()
}

/// Process a signal through an effect and return the output.
fn process_signal(effect: &mut Box<dyn EffectWithParams + Send>, input: &[f32]) -> Vec<f32> {
    let mut output = vec![0.0f32; input.len()];
    for (i, &sample) in input.iter().enumerate() {
        output[i] = effect.process(sample);
    }
    output
}

/// All effect IDs from the registry.
fn all_effect_ids() -> Vec<String> {
    let registry = EffectRegistry::new();
    registry
        .all_effects()
        .into_iter()
        .map(|d| d.id.to_string())
        .collect()
}

// --- Rule 1: Peak Ceiling ---
//
// Universal: all effects must produce bounded output (peak < 4.0 / +12 dBFS)
// with 1s of 0 dBFS 1kHz sine at default params.
//
// Strict subset: passive effects that don't add gain, feedback, or waveshaping
// must stay below -1 dBFS (peak <= 0.891).

/// Effects that should meet the strict -1 dBFS ceiling at default params.
/// Only the limiter has a built-in ceiling below -1 dBFS by default (-0.3 dBFS).
/// All other effects pass the input through at unity gain, so a 0 dBFS sine
/// will peak at 1.0 (which exceeds -1 dBFS = 0.891).
const RULE1_STRICT_EFFECTS: &[&str] = &["limiter"];

#[test]
fn rule1_peak_ceiling() {
    let registry = EffectRegistry::new();
    let input = generate_sine(SAMPLE_RATE, 1000.0, 1.0);
    let ceiling_linear = 10.0f32.powf(-1.0 / 20.0); // -1 dBFS ~ 0.891

    for id in all_effect_ids() {
        let mut effect = registry.create(&id, SAMPLE_RATE).unwrap();
        let output = process_signal(&mut effect, &input);
        let pk = peak(&output);

        // Universal bound: no effect should blow up
        assert!(
            pk < 4.0,
            "Rule 1 FAIL: effect '{}' peak {:.4} exceeds universal bound (4.0 / +12 dBFS)",
            id,
            pk
        );

        // Strict bound for passive effects
        if RULE1_STRICT_EFFECTS.contains(&id.as_str()) {
            assert!(
                pk <= ceiling_linear,
                "Rule 1 FAIL: effect '{}' peak {:.4} exceeds -1 dBFS ceiling ({:.4})",
                id,
                pk,
                ceiling_linear
            );
        }
    }
}

// --- Rule 2: Bypass Parity ---
//
// Effects tested here should have output RMS within ±3 dB of input RMS
// at default params with a 1 kHz sine. Only test effects that are
// expected to be roughly unity-gain and flat at 1 kHz by default.

const RULE2_TESTED: &[&str] = &["gate", "eq", "bitcrusher", "tremolo", "vibrato"];

#[test]
fn rule2_bypass_parity() {
    let registry = EffectRegistry::new();
    let input = generate_sine(SAMPLE_RATE, 1000.0, 1.0);
    let input_rms = rms(&input);

    let lower = 10.0f32.powf(-3.0 / 20.0); // -3 dB ratio ~ 0.708
    let upper = 10.0f32.powf(3.0 / 20.0); //  +3 dB ratio ~ 1.413

    for &id in RULE2_TESTED {
        let mut effect = registry.create(id, SAMPLE_RATE).unwrap();
        let output = process_signal(&mut effect, &input);
        let output_rms = rms(&output);
        let ratio = output_rms / input_rms;
        assert!(
            ratio >= lower && ratio <= upper,
            "Rule 2 FAIL: effect '{}' RMS ratio {:.4} not within ±3 dB (expected {:.4}..{:.4})",
            id,
            ratio,
            lower,
            upper
        );
    }
}

// --- Rule 3: Output Parameter ---
// Every effect must have a gain-staging parameter with unit = Decibels.
// Most effects place it as the last param. Documented exceptions (distortion,
// compressor) place it at a non-final index with a domain-appropriate name.

const RULE3_NON_LAST_EXCEPTIONS: &[&str] = &["distortion", "compressor"];

#[test]
fn rule3_output_parameter() {
    let registry = EffectRegistry::new();

    for id in all_effect_ids() {
        let effect = registry.create(&id, SAMPLE_RATE).unwrap();
        let count = effect.effect_param_count();
        assert!(count > 0, "Effect '{}' has no parameters", id);

        if RULE3_NON_LAST_EXCEPTIONS.contains(&id.as_str()) {
            // Exception: verify a dB-unit gain/output/level param exists somewhere
            let has_db_gain = (0..count).any(|i| {
                if let Some(desc) = effect.effect_param_info(i) {
                    desc.unit == ParamUnit::Decibels
                        && (desc.name.to_lowercase().contains("output")
                            || desc.name.to_lowercase().contains("level")
                            || desc.name.to_lowercase().contains("makeup")
                            || desc.name.to_lowercase().contains("gain"))
                } else {
                    false
                }
            });
            assert!(
                has_db_gain,
                "Rule 3 FAIL: effect '{}' (exception) has no dB-unit gain/output param",
                id
            );
        } else {
            // Standard: last param must have unit = Decibels
            let last = effect.effect_param_info(count - 1).unwrap();
            assert_eq!(
                last.unit,
                ParamUnit::Decibels,
                "Rule 3 FAIL: effect '{}' last param '{}' (index {}) has unit {:?}, expected Decibels",
                id,
                last.name,
                count - 1,
                last.unit
            );
        }
    }
}

// --- Rule 4: Wet/Dry Exactness ---
// Set mix=0 → output must match input within tolerance.
// Only test effects that have a "Mix" parameter and a clean dry path.

const RULE4_TESTED: &[&str] = &[
    "delay",
    "flanger",
    "phaser",
    "reverb",
    "ringmod",
    "bitcrusher",
    "vibrato",
];

#[test]
fn rule4_wet_dry_exactness() {
    let registry = EffectRegistry::new();
    let input = generate_sine(SAMPLE_RATE, 440.0, 0.5);

    for &id in RULE4_TESTED {
        let mut effect = registry.create(id, SAMPLE_RATE).unwrap();

        // Find the "Mix" parameter and set it to 0
        let count = effect.effect_param_count();
        let mut mix_idx = None;
        for i in 0..count {
            if let Some(desc) = effect.effect_param_info(i)
                && desc.name.to_lowercase().contains("mix")
            {
                mix_idx = Some(i);
                break;
            }
        }

        let Some(mix_idx) = mix_idx else {
            panic!(
                "Rule 4: effect '{}' is in RULE4_TESTED but has no Mix parameter",
                id
            );
        };

        effect.effect_set_param(mix_idx, 0.0);

        // Warm up to let SmoothedParam settle (100ms at 48kHz)
        for _ in 0..4800 {
            effect.process(0.0);
        }

        let output = process_signal(&mut effect, &input);

        for (i, (&inp, &out)) in input.iter().zip(output.iter()).enumerate() {
            let diff = (inp - out).abs();
            // Tolerance accounts for SmoothedParam floating-point residue
            // and output_level gain application (also SmoothedParam at 1.0).
            assert!(
                diff < 5e-4,
                "Rule 4 FAIL: effect '{}' at mix=0, sample {} input={} output={} diff={}",
                id,
                i,
                inp,
                out,
                diff
            );
        }
    }
}

// --- Rule 5: Feedback Stability ---
// Effects with feedback: set feedback to max → 10s of processing → bounded output.

const RULE5_EFFECTS: &[&str] = &["delay", "flanger", "phaser", "reverb", "chorus"];

#[test]
fn rule5_feedback_stability() {
    let registry = EffectRegistry::new();
    let input = generate_sine(SAMPLE_RATE, 1000.0, 10.0);

    for &id in RULE5_EFFECTS {
        let mut effect = registry.create(id, SAMPLE_RATE).unwrap();

        // Find and max out the feedback parameter
        let count = effect.effect_param_count();
        for i in 0..count {
            if let Some(desc) = effect.effect_param_info(i)
                && (desc.name.to_lowercase().contains("feedback")
                    || desc.name.to_lowercase().contains("decay"))
            {
                effect.effect_set_param(i, desc.max);
            }
        }

        let output = process_signal(&mut effect, &input);

        for (i, &sample) in output.iter().enumerate() {
            assert!(
                sample.is_finite(),
                "Rule 5 FAIL: effect '{}' produced non-finite output at sample {}",
                id,
                i
            );
            assert!(
                sample.abs() < 10.0,
                "Rule 5 FAIL: effect '{}' output {} exceeds bound at sample {}",
                id,
                sample,
                i
            );
        }
    }
}

// --- Rule 6: Headroom ---
// Process at -18 dBFS and 0 dBFS → all outputs finite and bounded.

#[test]
fn rule6_headroom() {
    let registry = EffectRegistry::new();
    let amplitudes = [("0 dBFS", 1.0f32), ("-18 dBFS", 10.0f32.powf(-18.0 / 20.0))];

    for id in all_effect_ids() {
        for &(label, amplitude) in &amplitudes {
            let mut effect = registry.create(&id, SAMPLE_RATE).unwrap();
            let input: Vec<f32> = generate_sine(SAMPLE_RATE, 1000.0, 1.0)
                .iter()
                .map(|s| s * amplitude)
                .collect();

            let output = process_signal(&mut effect, &input);

            for (i, &sample) in output.iter().enumerate() {
                assert!(
                    sample.is_finite(),
                    "Rule 6 FAIL: effect '{}' at {} produced non-finite output at sample {}",
                    id,
                    label,
                    i
                );
                assert!(
                    sample.abs() < 10.0,
                    "Rule 6 FAIL: effect '{}' at {} output {} exceeds bound at sample {}",
                    id,
                    label,
                    sample,
                    i
                );
            }
        }
    }
}

// --- Rule 7: Parameter Vocabulary ---
// All params: non-empty name, min < max.

#[test]
fn rule7_vocabulary() {
    let registry = EffectRegistry::new();

    for id in all_effect_ids() {
        let effect = registry.create(&id, SAMPLE_RATE).unwrap();
        let count = effect.effect_param_count();

        for i in 0..count {
            let desc = effect
                .effect_param_info(i)
                .unwrap_or_else(|| panic!("Effect '{}' param index {} returned None", id, i));

            assert!(
                !desc.name.is_empty(),
                "Rule 7 FAIL: effect '{}' param index {} has empty name",
                id,
                i
            );

            assert!(
                desc.min < desc.max,
                "Rule 7 FAIL: effect '{}' param '{}' (index {}): min ({}) >= max ({})",
                id,
                desc.name,
                i,
                desc.min,
                desc.max
            );
        }
    }
}
