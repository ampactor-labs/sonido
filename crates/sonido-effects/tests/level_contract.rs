//! Sonido's level contract (`sonido_core::level`), held for every effect.
//!
//! 1. The calibration table is current: each effect, fully wet when it has a
//!    mix control and at its defaults otherwise, plays the reference guitar
//!    within 0.5 dB of the dry loudness. If this fails, run
//!    `cargo run -p sonido-effects --example calibrate --release`.
//! 2. The mix control keeps the level: across 0–100% and at the default mix,
//!    within 1.5 dB, which is what each kernel's `MIX_LAW` is for.

use sonido_analysis::loudness::effect_gain_db;
use sonido_analysis::signals::reference_guitar;
use sonido_registry::{EFFECT_UIDS, EffectRegistry};

const SR: f32 = 48_000.0;
const UNCALIBRATED: &[&str] = &["looper", "tuner"];

fn gain(registry: &EffectRegistry, id: &str, mix: Option<f32>, input: &[f32]) -> f32 {
    let mut fx = registry.create(id, SR).unwrap();
    if let Some(m) = mix {
        let i = (0..fx.effect_param_count())
            .find(|&i| {
                fx.effect_param_info(i)
                    .is_some_and(|d| d.matches_key("mix"))
            })
            .unwrap();
        fx.effect_set_param(i, m);
    }
    fx.reset();
    effect_gain_db(fx.as_mut(), input, SR)
}

#[test]
fn every_effect_meets_the_level_contract() {
    let registry = EffectRegistry::new();
    let input = reference_guitar(SR);
    let mut failures = Vec::new();
    for (id, _) in EFFECT_UIDS {
        if UNCALIBRATED.contains(id) {
            continue;
        }
        let fx = registry.create(id, SR).unwrap();
        let has_mix = (0..fx.effect_param_count()).any(|i| {
            fx.effect_param_info(i)
                .is_some_and(|d| d.matches_key("mix"))
        });
        let reference = gain(&registry, id, has_mix.then_some(100.0), &input);
        let mut row = format!("{id:>18} {reference:+5.1}");
        if reference.abs() > 0.5 {
            failures.push(format!(
                "{id}: {reference:+.1} dB (calibration out of date: run the calibrate example)"
            ));
        }
        if has_mix {
            for m in [0.0, 25.0, 50.0, 75.0] {
                let g = gain(&registry, id, Some(m), &input);
                row.push_str(&format!(" | {m:>3}% {g:+5.1}"));
                if g.abs() > 1.5 {
                    failures.push(format!("{id}: {g:+.1} dB at {m}% mix"));
                }
            }
            let g = gain(&registry, id, None, &input);
            row.push_str(&format!(" | default {g:+5.1}"));
            if g.abs() > 1.5 {
                failures.push(format!("{id}: {g:+.1} dB at its default mix"));
            }
        }
        println!("{row}");
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
