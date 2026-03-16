//! Compliance tests: verify all registered effects meet quality standards.
//!
//! These tests complement the per-rule checks in `quality_standard.rs` with
//! broader behavioral checks covering finite output, DC offset, reset safety,
//! sample rate changes, and parameter clamping.
//!
//! All tests use [`EffectRegistry`] to iterate effects programmatically so
//! the suite stays current as new effects are registered.

mod helpers;

use helpers::{all_ids, sine_440};
use sonido_registry::EffectRegistry;

const SAMPLE_RATE: f32 = 48000.0;
const BLOCK_SIZE: usize = 4800; // 100 ms at 48 kHz
const DC_CHECK_SAMPLES: usize = 4096;

/// Generate a silence block.
fn silence(len: usize) -> Vec<f32> {
    vec![0.0_f32; len]
}

// ─────────────────────────────────────────────────────────────────────────────
// Finite output — default params + sine
// ─────────────────────────────────────────────────────────────────────────────

/// All effects produce finite, bounded output for default params and a sine input.
#[test]
fn all_effects_finite_output() {
    let registry = EffectRegistry::new();
    let input = sine_440(BLOCK_SIZE);
    let mut output = vec![0.0_f32; BLOCK_SIZE];

    for id in all_ids() {
        let mut effect = registry.create(&id, SAMPLE_RATE).unwrap();
        effect.process_block(&input, &mut output);

        for (i, &s) in output.iter().enumerate() {
            assert!(
                s.is_finite(),
                "all_effects_finite_output: '{}' produced non-finite at sample {}",
                id,
                i
            );
            assert!(
                s.abs() < 20.0,
                "all_effects_finite_output: '{}' sample {} out of range ({:.4})",
                id,
                i,
                s
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Finite output — random params + sine
// ─────────────────────────────────────────────────────────────────────────────

/// All effects produce finite output after parameters are swept to random mid-range values.
///
/// Uses a deterministic LCG seeded at 0xDEAD so results are reproducible. Each
/// parameter is set to 25% of its [min, max] range to avoid extreme states
/// that are expected to be aggressive (e.g. max gain on an amp sim).
#[test]
fn all_effects_finite_random_params() {
    let registry = EffectRegistry::new();
    let input = sine_440(BLOCK_SIZE);
    let mut output = vec![0.0_f32; BLOCK_SIZE];

    // LCG: x = (x * 1664525 + 1013904223) % 2^32
    let mut rng: u32 = 0xDEAD_BEEF;
    let mut next_rand = move || -> f32 {
        rng = rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        // 25–50% of range to stay in moderate territory
        0.25 + (rng as f32 / u32::MAX as f32) * 0.25
    };

    for id in all_ids() {
        let mut effect = registry.create(&id, SAMPLE_RATE).unwrap();
        let count = effect.effect_param_count();

        for idx in 0..count {
            if let Some(desc) = effect.effect_param_info(idx)
                && !desc.flags.contains(sonido_core::ParamFlags::READ_ONLY)
            {
                let t = next_rand();
                let value = desc.min + t * (desc.max - desc.min);
                effect.effect_set_param(idx, value);
            }
        }

        // Warm up to let smoothed params settle
        let warmup = silence(2400);
        let mut warmup_out = vec![0.0_f32; 2400];
        effect.process_block(&warmup, &mut warmup_out);

        effect.process_block(&input, &mut output);

        for (i, &s) in output.iter().enumerate() {
            assert!(
                s.is_finite(),
                "all_effects_finite_random_params: '{}' non-finite at sample {} (params set to 25–50% range)",
                id,
                i
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// DC offset on silence
// ─────────────────────────────────────────────────────────────────────────────

/// No effect introduces significant DC offset when processing silence.
///
/// Threshold of 1e-5 (≈−100 dBFS) allows for sub-LSB DC from denormal flushing
/// while catching any real DC bias introduced by DSP bugs.
///
/// Effects with known asymmetric saturation (tape, amp, distortion, preamp) are
/// given a slightly wider tolerance (1e-3) since their nonlinearities can produce
/// a small DC component proportional to the asymmetry.
#[test]
fn no_dc_offset_on_silence() {
    let registry = EffectRegistry::new();
    let input = silence(DC_CHECK_SAMPLES);
    let mut output = vec![0.0_f32; DC_CHECK_SAMPLES];

    // Effects that may produce low-level asymmetric DC due to nonlinear processing
    const WIDE_TOLERANCE: &[&str] = &["tape", "amp", "distortion", "preamp", "cabinet"];
    const STRICT_THRESHOLD: f32 = 1e-5;
    const WIDE_THRESHOLD: f32 = 1e-3;

    for id in all_ids() {
        let mut effect = registry.create(&id, SAMPLE_RATE).unwrap();

        effect.process_block(&input, &mut output);

        let mean = output.iter().sum::<f32>() / output.len() as f32;
        let threshold = if WIDE_TOLERANCE.contains(&id.as_str()) {
            WIDE_THRESHOLD
        } else {
            STRICT_THRESHOLD
        };

        assert!(
            mean.abs() < threshold,
            "no_dc_offset_on_silence: '{}' DC offset {:.2e} exceeds threshold {:.2e}",
            id,
            mean,
            threshold
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Reset without panic
// ─────────────────────────────────────────────────────────────────────────────

/// All effects can be reset without panic, and produce finite output after reset.
#[test]
fn all_effects_reset_cleanly() {
    let registry = EffectRegistry::new();
    let input = sine_440(BLOCK_SIZE);
    let mut output = vec![0.0_f32; BLOCK_SIZE];

    for id in all_ids() {
        let mut effect = registry.create(&id, SAMPLE_RATE).unwrap();

        // Prime the effect with some audio
        let mut pre_output = vec![0.0_f32; BLOCK_SIZE];
        effect.process_block(&input, &mut pre_output);

        // Reset should not panic
        effect.reset();

        // Post-reset output should still be finite
        effect.process_block(&input, &mut output);
        for (i, &s) in output.iter().enumerate() {
            assert!(
                s.is_finite(),
                "all_effects_reset_cleanly: '{}' non-finite at sample {} after reset",
                id,
                i
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Sample rate changes
// ─────────────────────────────────────────────────────────────────────────────

/// All effects handle a mid-stream sample rate change without panic or divergence.
#[test]
fn all_effects_handle_sample_rate_change() {
    let registry = EffectRegistry::new();
    let input_44 = sine_440(BLOCK_SIZE);
    let input_48 = sine_440(BLOCK_SIZE);
    let mut output = vec![0.0_f32; BLOCK_SIZE];

    for id in all_ids() {
        let mut effect = registry.create(&id, 44100.0).unwrap();

        // Prime at 44.1 kHz
        effect.process_block(&input_44, &mut output);

        // Switch to 48 kHz
        effect.set_sample_rate(SAMPLE_RATE);

        // Process at 48 kHz
        effect.process_block(&input_48, &mut output);

        for (i, &s) in output.iter().enumerate() {
            assert!(
                s.is_finite(),
                "all_effects_handle_sample_rate_change: '{}' non-finite at sample {} after SR change",
                id,
                i
            );
            assert!(
                s.abs() < 20.0,
                "all_effects_handle_sample_rate_change: '{}' sample {} out of range after SR change ({:.4})",
                id,
                i,
                s
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Parameter bounds clamping
// ─────────────────────────────────────────────────────────────────────────────

/// Setting a parameter beyond its declared bounds should not cause non-finite output.
///
/// Verifies that the `Adapter`'s auto-clamp (from `impl_params!`) prevents
/// out-of-range values from reaching DSP internals.
#[test]
fn all_params_clamp_to_bounds() {
    let registry = EffectRegistry::new();
    let input = sine_440(BLOCK_SIZE);
    let mut output = vec![0.0_f32; BLOCK_SIZE];

    for id in all_ids() {
        let mut effect = registry.create(&id, SAMPLE_RATE).unwrap();
        let count = effect.effect_param_count();

        // Set all params to extreme out-of-range values
        for idx in 0..count {
            if let Some(desc) = effect.effect_param_info(idx)
                && !desc.flags.contains(sonido_core::ParamFlags::READ_ONLY)
            {
                // Set to well beyond max — adapter should clamp
                effect.effect_set_param(idx, desc.max * 10.0 + 1000.0);
            }
        }

        effect.process_block(&input, &mut output);

        for (i, &s) in output.iter().enumerate() {
            assert!(
                s.is_finite(),
                "all_params_clamp_to_bounds: '{}' non-finite at sample {} with over-range params",
                id,
                i
            );
        }

        // Now set to below min
        let mut effect = registry.create(&id, SAMPLE_RATE).unwrap();
        for idx in 0..count {
            if let Some(desc) = effect.effect_param_info(idx)
                && !desc.flags.contains(sonido_core::ParamFlags::READ_ONLY)
            {
                effect.effect_set_param(idx, desc.min - 1000.0);
            }
        }

        effect.process_block(&input, &mut output);

        for (i, &s) in output.iter().enumerate() {
            assert!(
                s.is_finite(),
                "all_params_clamp_to_bounds: '{}' non-finite at sample {} with under-range params",
                id,
                i
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Latency and is_true_stereo consistency
// ─────────────────────────────────────────────────────────────────────────────

/// All effects return a finite, non-negative latency value.
#[test]
fn all_effects_latency_non_negative() {
    let registry = EffectRegistry::new();

    for id in all_ids() {
        let effect = registry.create(&id, SAMPLE_RATE).unwrap();
        // latency_samples() returns usize — it's inherently non-negative
        let _ = effect.latency_samples();
    }
}

/// Stereo block processing matches mono processing for non-true-stereo effects.
///
/// For dual-mono effects (`is_true_stereo() == false`), processing L and R
/// independently must produce the same result as `process_block_stereo`. Both
/// channels of a dual-mono effect are independent instances of the same algorithm,
/// so if the same signal is fed to both channels the outputs must be identical.
#[test]
fn dual_mono_effects_consistent_lr() {
    let registry = EffectRegistry::new();
    let signal = sine_440(BLOCK_SIZE);
    let mut left_out = vec![0.0_f32; BLOCK_SIZE];
    let mut right_out = vec![0.0_f32; BLOCK_SIZE];

    for id in all_ids() {
        let mut effect = registry.create(&id, SAMPLE_RATE).unwrap();
        if effect.is_true_stereo() {
            continue; // true-stereo effects are decorrelated by design
        }

        effect.process_block_stereo(&signal, &signal, &mut left_out, &mut right_out);

        for (i, (&l, &r)) in left_out.iter().zip(right_out.iter()).enumerate() {
            assert!(
                (l - r).abs() < 1e-5,
                "dual_mono_effects_consistent_lr: '{}' L/R mismatch at sample {} (L={:.6}, R={:.6}) — dual-mono must be symmetric for identical input",
                id,
                i,
                l,
                r
            );
        }
    }
}
