//! The factory rigs fit the pedal, survive the binary format, and play at the
//! loudness of the dry guitar with every knob at noon, from A through B.

mod support;

use sonido_analysis::signals::reference_guitar;
use sonido_patch::validate::{Severity, validate_for_daisy};
use sonido_patch::{SECTOR_SIZE, decode, encode};
use sonido_registry::EffectRegistry;
use sonido_rigs::factory_rigs;
use support::measure::{SR, rig_gain_db};

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
    }
}

#[test]
fn rigs_fit_the_pedal() {
    for rig in factory_rigs() {
        let findings = validate_for_daisy(&rig);
        let errors: Vec<_> = findings
            .iter()
            .filter(|f| f.severity == Severity::Error)
            .map(|f| f.message.clone())
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
