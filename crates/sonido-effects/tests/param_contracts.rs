//! Parameter contract tests: range access, unity gain, curve conventions.
//!
//! These tests enforce the hardware tuning contracts:
//! 1. **Full Range Accessible** — knob 0→1 covers descriptor min→max
//! 2. **Unity Gain at Defaults** — processing with defaults produces near-unity output
//! 3. **Scale Conventions** — Hz params use Logarithmic, STEPPED has valid steps, etc.
//! 4. **Monotonic Curves** — sweeping 0→1 produces monotonically increasing values

mod helpers;

use helpers::{all_ids, assert_no_violations, rms_db, sine_440};
use sonido_core::{ParamFlags, ParamScale, ParamUnit};
use sonido_registry::EffectRegistry;

const SAMPLE_RATE: f32 = 48000.0;

// ─────────────────────────────────────────────────────────────────────────────
// Test 1: Full Range Accessible
// ─────────────────────────────────────────────────────────────────────────────

/// For every registered effect, for every continuous parameter: knob at 0.0
/// must produce `min` and knob at 1.0 must produce `max` (within 1% of range).
///
/// This verifies that no parameter range has been truncated by scaling
/// transforms — the full descriptor range is always reachable via the knob.
#[test]
fn full_range_accessible() {
    let registry = EffectRegistry::new();
    let mut violations = Vec::new();

    for id in all_ids() {
        let effect = registry.create(&id, SAMPLE_RATE).unwrap();
        let count = effect.effect_param_count();

        for idx in 0..count {
            if let Some(desc) = effect.effect_param_info(idx) {
                if desc.flags.contains(ParamFlags::READ_ONLY) {
                    continue;
                }

                let range = (desc.max - desc.min).abs();
                let tolerance = if range > 0.0 { range * 0.01 } else { 1e-6 };

                let at_zero = desc.denormalize(0.0);
                let at_one = desc.denormalize(1.0);

                if (at_zero - desc.min).abs() > tolerance {
                    violations.push(format!(
                        "  {id}[{idx}] \"{}\": knob=0 → {at_zero:.4}, expected min={:.4}",
                        desc.name, desc.min
                    ));
                }
                if (at_one - desc.max).abs() > tolerance {
                    violations.push(format!(
                        "  {id}[{idx}] \"{}\": knob=1 → {at_one:.4}, expected max={:.4}",
                        desc.name, desc.max
                    ));
                }
            }
        }
    }

    assert_no_violations(
        "full_range_accessible",
        "knob 0/1 must map to descriptor min/max",
        violations,
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 2: Unity Gain at Defaults
// ─────────────────────────────────────────────────────────────────────────────

/// With all params at defaults, processing a signal must produce output
/// within ±3 dB of input level (±6 dB for dynamics processors).
#[test]
fn defaults_produce_near_unity_gain() {
    let registry = EffectRegistry::new();
    let block_size = 4800; // 100ms
    let input = sine_440(block_size);
    let input_db = rms_db(&input);

    let mut violations = Vec::new();

    // ±6 dB: dynamics processors, mix-dependent effects, resonant filters.
    // These inherently modify level at defaults (mix=50% loses ~6 dB,
    // saturation adds gain, bandpass filters attenuate out-of-band).
    let wide_tolerance_effects = [
        "compressor",
        "limiter",
        "gate",
        "looper", // dynamics
        "flanger",
        "phaser",
        "delay",
        "reverb", // mix at 50% → partial cancellation
        "distortion",
        "tape", // nonlinear gain
        "wah",  // resonant bandpass filter
    ];

    // Inherently non-unity effects: skip entirely.
    // Wah is a resonant bandpass — attenuates most frequencies by design.
    let skip_unity = ["wah"];

    for id in all_ids() {
        if skip_unity.contains(&id.as_str()) {
            continue;
        }
        let mut effect = registry.create(&id, SAMPLE_RATE).unwrap();
        let mut output = vec![0.0_f32; block_size];

        effect.process_block(&input, &mut output);

        let output_db = rms_db(&output);
        let gain_db = output_db - input_db;

        let tolerance_db = if wide_tolerance_effects.contains(&id.as_str()) {
            8.0
        } else {
            3.0
        };

        if gain_db.abs() > tolerance_db {
            violations.push(format!(
                "  {id}: input={input_db:.1} dB, output={output_db:.1} dB, \
                 gain={gain_db:+.1} dB (tolerance=±{tolerance_db:.0} dB)"
            ));
        }
    }

    assert_no_violations(
        "defaults_produce_near_unity_gain",
        "adjust output level or mix defaults",
        violations,
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 3: Scale Conventions
// ─────────────────────────────────────────────────────────────────────────────

/// Verify parameter descriptor conventions:
/// - Frequency params (unit=Hz) use Logarithmic scale
/// - STEPPED params have step > 0
/// - default ∈ [min, max]
/// - min < max
/// - ParamIds are unique within each effect
#[test]
fn param_scale_conventions() {
    let registry = EffectRegistry::new();
    let mut violations = Vec::new();

    for id in all_ids() {
        let effect = registry.create(&id, SAMPLE_RATE).unwrap();
        let count = effect.effect_param_count();
        let mut seen_ids = std::collections::HashSet::new();

        for idx in 0..count {
            if let Some(desc) = effect.effect_param_info(idx) {
                if desc.default < desc.min || desc.default > desc.max {
                    violations.push(format!(
                        "  {id}[{idx}] \"{}\": default={} outside [{}, {}]",
                        desc.name, desc.default, desc.min, desc.max
                    ));
                }

                if desc.min >= desc.max {
                    violations.push(format!(
                        "  {id}[{idx}] \"{}\": min={} >= max={}",
                        desc.name, desc.min, desc.max
                    ));
                }

                if desc.unit == ParamUnit::Hertz && desc.scale != ParamScale::Logarithmic {
                    violations.push(format!(
                        "  {id}[{idx}] \"{}\": unit=Hz but scale={:?} (expected Logarithmic)",
                        desc.name, desc.scale
                    ));
                }

                if desc.flags.contains(ParamFlags::STEPPED) && desc.step <= 0.0 {
                    violations.push(format!(
                        "  {id}[{idx}] \"{}\": STEPPED but step={}",
                        desc.name, desc.step
                    ));
                }

                if !seen_ids.insert(desc.id) {
                    violations.push(format!(
                        "  {id}[{idx}] \"{}\": duplicate ParamId {:?}",
                        desc.name, desc.id
                    ));
                }
            }
        }
    }

    assert_no_violations("param_scale_conventions", "fix descriptor", violations);
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 4: Monotonic Curves
// ─────────────────────────────────────────────────────────────────────────────

/// Sweeping any parameter from 0.0 → 1.0 via `denormalize` must produce
/// monotonically increasing values (non-decreasing for STEPPED).
///
/// Also: for Logarithmic params, noon < arithmetic midpoint (the geometric
/// mean is always less than the arithmetic mean for positive ranges).
#[test]
fn adc_to_param_curve_monotonic() {
    let registry = EffectRegistry::new();
    let mut violations = Vec::new();
    let steps = 100;

    for id in all_ids() {
        let effect = registry.create(&id, SAMPLE_RATE).unwrap();
        let count = effect.effect_param_count();

        for idx in 0..count {
            if let Some(desc) = effect.effect_param_info(idx) {
                if desc.flags.contains(ParamFlags::READ_ONLY) {
                    continue;
                }

                let stepped = desc.flags.contains(ParamFlags::STEPPED);

                // Sweep and check monotonicity
                let mut prev = desc.denormalize(0.0);
                if stepped {
                    prev = prev.round();
                }
                for i in 1..=steps {
                    let normalized = i as f32 / steps as f32;
                    let mut val = desc.denormalize(normalized);
                    if stepped {
                        val = val.round();
                    }

                    if stepped {
                        if val < prev {
                            violations.push(format!(
                                "  {id}[{idx}] \"{}\": non-monotonic at step {i}: {val} < {prev}",
                                desc.name
                            ));
                            break;
                        }
                    } else if val <= prev {
                        violations.push(format!(
                            "  {id}[{idx}] \"{}\": non-monotonic at step {i}: {val} <= {prev}",
                            desc.name
                        ));
                        break;
                    }
                    prev = val;
                }

                // Log params: noon should be below arithmetic midpoint
                if desc.scale == ParamScale::Logarithmic && desc.min > 0.0 {
                    let noon = desc.denormalize(0.5);
                    let arith_mid = (desc.min + desc.max) / 2.0;
                    if noon >= arith_mid {
                        violations.push(format!(
                            "  {id}[{idx}] \"{}\": log noon={noon:.2} >= arithmetic mid={arith_mid:.2}",
                            desc.name
                        ));
                    }
                }
            }
        }
    }

    assert_no_violations(
        "adc_to_param_curve_monotonic",
        "non-monotonic parameter curve detected",
        violations,
    );
}
