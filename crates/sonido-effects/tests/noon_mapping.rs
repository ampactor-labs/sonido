//! Knob mapping, checked against every registered effect.
//!
//! A knob at noon selects the parameter's default, the effect's voiced sweet
//! spot, so a pedal at noon and a freshly opened plugin sound the same, and
//! the level contract (`sonido_core::level`) holds at noon too. The ends of
//! travel reach the descriptor's range, and the mapping never runs backwards.
//! These run the real `sonido_platform::param_map::adc_to_param_biased`.

mod helpers;

use helpers::{all_ids_from, assert_no_violations};
use sonido_core::{ParamDescriptor, ParamFlags};
use sonido_platform::param_map::adc_to_param_biased;
use sonido_registry::EffectRegistry;

const SAMPLE_RATE: f32 = 48_000.0;

/// Every writable parameter of every registered effect.
fn each_param(mut check: impl FnMut(&str, usize, &ParamDescriptor)) {
    let registry = EffectRegistry::new();
    for id in all_ids_from(&registry) {
        let effect = registry.create(&id, SAMPLE_RATE).unwrap();
        for idx in 0..effect.effect_param_count() {
            if let Some(desc) = effect.effect_param_info(idx)
                && !desc.flags.contains(ParamFlags::READ_ONLY)
            {
                check(&id, idx, &desc);
            }
        }
    }
}

fn tolerance(desc: &ParamDescriptor) -> f32 {
    let range = (desc.max - desc.min).abs();
    if range > 0.0 { range * 0.01 } else { 1e-6 }
}

/// Knob 0 and 1 reach the descriptor's min and max: no dead ends.
#[test]
fn biased_mapping_endpoints() {
    let mut violations = Vec::new();
    each_param(|id, idx, desc| {
        let (lo, hi) = (
            adc_to_param_biased(desc, desc.default, 0.0),
            adc_to_param_biased(desc, desc.default, 1.0),
        );
        if (lo - desc.min).abs() > tolerance(desc) || (hi - desc.max).abs() > tolerance(desc) {
            violations.push(format!(
                "  {id}[{idx}] \"{}\": knob 0/1 → {lo:.4}/{hi:.4}, range {}..{}",
                desc.name, desc.min, desc.max
            ));
        }
    });
    assert_no_violations(
        "biased_mapping_endpoints",
        "knob 0/1 must reach the descriptor's min/max",
        violations,
    );
}

/// Knob at noon selects the default (stepped parameters: the default step).
#[test]
fn noon_is_the_default() {
    let mut violations = Vec::new();
    each_param(|id, idx, desc| {
        let at_noon = adc_to_param_biased(desc, desc.default, 0.5);
        let expected = if desc.flags.contains(ParamFlags::STEPPED) {
            desc.default.round()
        } else {
            desc.default
        };
        if (at_noon - expected).abs() > tolerance(desc) {
            violations.push(format!(
                "  {id}[{idx}] \"{}\": noon → {at_noon:.4}, default {expected:.4}",
                desc.name
            ));
        }
    });
    assert_no_violations(
        "noon_is_the_default",
        "noon must select the default",
        violations,
    );
}

/// Turning the knob up never turns the parameter down.
#[test]
fn biased_mapping_monotonic() {
    let mut violations = Vec::new();
    each_param(|id, idx, desc| {
        let mut prev = adc_to_param_biased(desc, desc.default, 0.0);
        for step in 1..=100 {
            let v = adc_to_param_biased(desc, desc.default, step as f32 / 100.0);
            if v < prev - 1e-4 * (desc.max - desc.min).abs().max(1.0) {
                violations.push(format!(
                    "  {id}[{idx}] \"{}\" decreases at {step}%",
                    desc.name
                ));
                break;
            }
            prev = v;
        }
    });
    assert_no_violations(
        "biased_mapping_monotonic",
        "the mapping must not reverse",
        violations,
    );
}
