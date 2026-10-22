//! Jam knob calibration, measured.
//!
//! With every knob at noon, each pedal jam effect plays back at about the
//! loudness of the dry guitar, so scrolling through effects never jumps in
//! volume; and every knob's extremes stay usable (finite, and neither silent
//! nor blasting). Loudness is integrated LUFS (ITU-R BS.1770-4, K-weighted
//! and gated), so added harmonics count the way they sound.
//!
//! The input is a synthetic strummed guitar at the level measured on a
//! Hothouse: peaks at -20 dBFS, the median of a real player's session.
//! Run with `--nocapture` to print the table used for tuning.

use sonido_analysis::loudness::LufsMeter;
use sonido_analysis::signals::strummed_guitar;
use sonido_platform::knob_mapping::{NULL_KNOB, knob_map, knob_to_param};
use sonido_registry::{EffectRegistry, PEDAL_EFFECT_IDS};

const SR: f32 = 48_000.0;
const BLOCK: usize = 32;
/// Peak level of the test guitar: -20 dBFS.
const GUITAR_PEAK: f32 = 0.1;

/// Integrated loudness (ITU-R BS.1770-4, LUFS) of a mono signal.
fn loudness_db(x: &[f32]) -> f32 {
    let mut meter = LufsMeter::new(SR);
    meter.push_samples(x, x);
    meter.integrated()
}

/// Render `input` (mono, fed to both sides) through `id` with the six knobs
/// at `knobs`; returns the mono mix of the output.
fn render(registry: &EffectRegistry, id: &str, knobs: [f32; 6], input: &[f32]) -> Vec<f32> {
    let mut fx = registry.create(id, SR).expect("pedal effect");
    let map = knob_map(id).expect("knob map");
    for (k, &idx) in map.iter().enumerate() {
        if idx == NULL_KNOB {
            continue;
        }
        let desc = fx.effect_param_info(idx as usize).unwrap();
        let v = knob_to_param(id, idx as usize, &desc, knobs[k]);
        fx.effect_set_param(idx as usize, v);
    }
    fx.reset();
    let mut out = Vec::with_capacity(input.len());
    let (mut l, mut r) = ([0.0f32; BLOCK], [0.0f32; BLOCK]);
    for chunk in input.chunks(BLOCK) {
        let n = chunk.len();
        l[..n].copy_from_slice(chunk);
        r[..n].copy_from_slice(chunk);
        fx.process_block_stereo_inplace(&mut l[..n], &mut r[..n]);
        out.extend(l[..n].iter().zip(&r[..n]).map(|(a, b)| 0.5 * (a + b)));
    }
    out
}

fn gain_db(registry: &EffectRegistry, id: &str, knobs: [f32; 6], input: &[f32]) -> f32 {
    let out = render(registry, id, knobs, input);
    assert!(
        out.iter().all(|v| v.is_finite()),
        "{id} {knobs:?}: non-finite output"
    );
    loudness_db(&out) - loudness_db(input)
}

#[test]
fn noon_is_level_matched_and_extremes_are_usable() {
    let registry = EffectRegistry::new();
    let input = strummed_guitar(SR, 4.0, GUITAR_PEAK);
    let mut failures = Vec::new();
    for id in PEDAL_EFFECT_IDS {
        let noon = gain_db(&registry, id, [0.5; 6], &input);
        let mut row = format!("{id:>11} noon {noon:+5.1} dB |");
        if noon.abs() > 1.5 {
            failures.push(format!("{id}: noon {noon:+.1} dB (want within ±1.5)"));
        }
        let map = knob_map(id).unwrap();
        for (k, &idx) in map.iter().enumerate() {
            if idx == NULL_KNOB {
                row.push_str("       --        |");
                continue;
            }
            let mut ends = [0.0f32; 2];
            for (e, pos) in [0.0f32, 1.0].into_iter().enumerate() {
                let mut knobs = [0.5; 6];
                knobs[k] = pos;
                ends[e] = gain_db(&registry, id, knobs, &input);
                // K6 is the level knob: quiet at its minimum and a clean
                // boost at its maximum are its job.
                let (floor, ceiling) = if k == 5 { (-30.0, 13.0) } else { (-15.0, 8.0) };
                if !(floor..=ceiling).contains(&ends[e]) {
                    failures.push(format!(
                        "{id}: K{} at {pos} gives {:+.1} dB",
                        k + 1,
                        ends[e]
                    ));
                }
            }
            row.push_str(&format!(" K{} {:+5.1}/{:+5.1} |", k + 1, ends[0], ends[1]));
        }
        println!("{row}");
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
