//! The factory rigs fit the pedal, survive the binary format, and play at the
//! loudness of the dry guitar with every knob at noon, from A through B.

mod support;

use sonido_analysis::signals::reference_guitar;
use sonido_patch::validate::{Severity, validate_for_daisy};
use sonido_patch::{SECTOR_SIZE, decode, encode};
use sonido_registry::EffectRegistry;
use sonido_rigs::factory_rigs;
use support::measure::{SR, render, rig_gain_db, rig_gain_db_toggled};

#[test]
fn rigs_round_trip_and_name_their_knobs() {
    for rig in factory_rigs() {
        let mut sector = [0u8; SECTOR_SIZE];
        encode(&rig, &mut sector).unwrap_or_else(|e| panic!("{}: {e}", rig.name));
        let back = decode(&sector).unwrap();
        assert_eq!(back.nodes.len(), rig.nodes.len(), "{}", rig.name);
        for (k, m) in rig.macros.iter().enumerate() {
            assert!(
                m.is_active() && !m.name.is_empty(),
                "{}: K{} unmapped",
                rig.name,
                k + 1
            );
        }
        assert_eq!(back.toggles, rig.toggles, "{}", rig.name);
        for (t, toggle) in rig.toggles.iter().enumerate() {
            assert!(
                toggle.is_active()
                    && !toggle.name.is_empty()
                    && toggle
                        .positions
                        .iter()
                        .all(|l| !l.is_empty() && l.len() <= 8),
                "{}: toggle {t} unset or mislabelled",
                rig.name
            );
        }
    }
}

/// Every toggle position that differs from the middle changes the sound: the
/// output differs from the middle's by more than -60 dB of its energy. That
/// catches a toggle whose parameter does nothing in its rig (spring Drip only
/// clips above 0.3, which Space Echo's spring never sees). An open gate, at
/// about -53 dB on this clean guitar, is as audible as the noise it lets by.
#[test]
fn rig_toggles_change_the_sound() {
    let registry = EffectRegistry::new();
    let input = &reference_guitar(SR);
    let energy = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>();
    let mut failures = Vec::new();
    for rig in factory_rigs() {
        let middle = render(&registry, &rig, 0.5, [1; sonido_patch::NUM_TOGGLES], input);
        for (t, toggle) in rig.toggles.iter().enumerate() {
            for position in [0usize, 2] {
                let same = toggle
                    .mappings
                    .iter()
                    .all(|m| m.values[position] == m.values[1]);
                if same {
                    continue; // e.g. Amp Stack's "Bright: On · Off · Off"
                }
                let mut toggles = [1; sonido_patch::NUM_TOGGLES];
                toggles[t] = position as u8;
                let (l, _) = render(&registry, &rig, 0.5, toggles, input);
                let diff: Vec<f32> = l.iter().zip(&middle.0).map(|(a, b)| a - b).collect();
                let ratio_db = 10.0 * (energy(&diff) / energy(&middle.0)).log10();
                if ratio_db < -60.0 {
                    failures.push(format!(
                        "{} {} {}: only {ratio_db:.1} dB from the middle",
                        rig.name, toggle.name, toggle.positions[position]
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// Every toggle position plays near the rig's loudness (the middle is the
/// rig's own sound, measured above).
#[test]
fn rig_toggles_keep_the_level() {
    let registry = EffectRegistry::new();
    let input = reference_guitar(SR);
    let mut failures = Vec::new();
    for rig in factory_rigs() {
        let mut row = format!("{:>18}", rig.name);
        for t in 0..rig.toggles.len() {
            for position in [0, 2] {
                let mut toggles = [1; sonido_patch::NUM_TOGGLES];
                toggles[t] = position;
                let gain = rig_gain_db_toggled(&registry, &rig, 0.5, toggles, &input);
                let label = &rig.toggles[t].positions[usize::from(position)];
                row.push_str(&format!("  {label}: {gain:+5.1} dB"));
                if gain.abs() > 4.0 {
                    failures.push(format!(
                        "{} with {} {label}: {gain:+.1} dB (limit ±4)",
                        rig.name, rig.toggles[t].name
                    ));
                }
            }
        }
        println!("{row}");
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn rigs_fit_the_pedal() {
    for rig in factory_rigs() {
        let findings = validate_for_daisy(&rig);
        let errors: Vec<_> = findings
            .iter()
            .filter(|f| f.severity == Severity::Error)
            .map(ToString::to_string)
            .collect();
        assert!(errors.is_empty(), "{}: {errors:?}", rig.name);
    }
}

/// Each rig plays at the dry guitar's loudness halfway through its morph
/// (its generated trim) and stays within 3 dB of it from A to B.
#[test]
fn rigs_play_at_the_guitars_loudness() {
    let registry = EffectRegistry::new();
    let input = reference_guitar(SR);
    let mut failures = Vec::new();
    for rig in factory_rigs() {
        let mut row = format!("{:>18}", rig.name);
        for (t, limit) in [(0.0, 3.0), (0.5, 0.5), (1.0, 3.0)] {
            let gain = rig_gain_db(&registry, &rig, t, &input);
            row.push_str(&format!("  {t:.1}: {gain:+5.1} dB"));
            if gain.abs() > limit {
                failures.push(format!(
                    "{} at morph {t}: {gain:+.1} dB (limit ±{limit})",
                    rig.name
                ));
            }
        }
        println!("{row}");
    }
    assert!(
        failures.is_empty(),
        "\n{}\nIf a rig changed, run `cargo run -p sonido-rigs --example calibrate --release`.",
        failures.join("\n")
    );
}
