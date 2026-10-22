//! Rig loudness, shared by the `calibrate` example and the `factory` test so
//! the trims are generated and checked by the same measurement.

use sonido_analysis::loudness::LufsMeter;
use sonido_patch::{NUM_TOGGLES, Patch, PatchPlayer};
use sonido_registry::{EffectRegistry, effect_by_uid};

/// The sample rate rigs are measured at.
pub const SR: f32 = 48_000.0;

/// Loudness of `patch` over the dry `input`, in dB (integrated LUFS, stereo),
/// with every knob at noon, the toggles in the middle and the morph at `t`.
pub fn rig_gain_db(registry: &EffectRegistry, patch: &Patch, t: f32, input: &[f32]) -> f32 {
    rig_gain_db_toggled(registry, patch, t, [1; NUM_TOGGLES], input)
}

/// [`rig_gain_db`] with the toggles at `toggles` (0 up, 1 middle, 2 down).
pub fn rig_gain_db_toggled(
    registry: &EffectRegistry,
    patch: &Patch,
    t: f32,
    toggles: [u8; NUM_TOGGLES],
    input: &[f32],
) -> f32 {
    let (out_l, out_r) = render(registry, patch, t, toggles, input);
    let lufs = |l: &[f32], r: &[f32]| {
        let mut meter = LufsMeter::new(SR);
        meter.push_samples(l, r);
        meter.integrated()
    };
    lufs(&out_l, &out_r) - lufs(input, input)
}

/// `patch`'s stereo output over the dry `input`, with every knob at noon, the
/// toggles at `toggles` and the morph at `t`.
#[allow(dead_code)] // the calibrate example measures only
pub fn render(
    registry: &EffectRegistry,
    patch: &Patch,
    t: f32,
    toggles: [u8; NUM_TOGGLES],
    input: &[f32],
) -> (Vec<f32>, Vec<f32>) {
    let mut player = PatchPlayer::from_patch(patch, SR, 32, |uid, sr| {
        let id = effect_by_uid(uid)?;
        Some((registry.create(id, sr)?, id))
    })
    .expect("rig builds");
    for k in 0..6 {
        player.set_macro(k, 0.5);
    }
    for (i, &position) in toggles.iter().enumerate() {
        player.set_toggle(i, position);
    }
    let (mut out_l, mut out_r) = (Vec::new(), Vec::new());
    for (i, chunk) in input.chunks(32).enumerate() {
        // The pedal's control tick: every 15 blocks.
        if i % 15 == 0 {
            player.set_morph_position(t);
            player.apply_controls();
        }
        let (mut l, mut r) = (chunk.to_vec(), chunk.to_vec());
        player.process_block_stereo(&mut l, &mut r);
        assert!(
            l.iter().chain(&r).all(|v| v.is_finite()),
            "{} at morph {t}: non-finite output",
            patch.name
        );
        out_l.extend(l);
        out_r.extend(r);
    }
    (out_l, out_r)
}
