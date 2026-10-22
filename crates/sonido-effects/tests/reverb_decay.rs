//! The reverbs' Decay controls produce the RT60 they name.
//!
//! Regression: the plate and spring cached their coefficients behind a
//! `NaN`-initialized check that never fired (the plate had no tank feedback
//! at all; the spring's decay never changed), and both turned a per-sample
//! gain into a per-loop one, so even once computed, Decay did nothing.

use sonido_core::kernel::DspKernel;
use sonido_effects::kernels::{
    PlateReverbKernel, PlateReverbParams, SpringReverbKernel, SpringReverbParams,
};

const SR: f32 = 48_000.0;

/// RT60 of an impulse response, from its Schroeder energy-decay curve
/// between -5 and -35 dB (T30, doubled).
fn rt60(mut process: impl FnMut(f32) -> f32, seconds: f32) -> f32 {
    let n = (seconds * SR) as usize;
    let energy: Vec<f64> = (0..n)
        .map(|i| {
            let y = process(if i == 0 { 1.0 } else { 0.0 }) as f64;
            y * y
        })
        .collect();
    let mut curve = vec![0.0f64; n];
    let mut acc = 0.0;
    for i in (0..n).rev() {
        acc += energy[i];
        curve[i] = acc;
    }
    let db = |i: usize| 10.0 * (curve[i] / curve[0]).log10();
    let t5 = (0..n).find(|&i| db(i) < -5.0).unwrap_or(n);
    let t35 = (0..n).find(|&i| db(i) < -35.0).unwrap_or(n);
    (t35 - t5) as f32 / SR * 2.0
}

#[test]
fn plate_decay_sets_rt60() {
    for decay in [1.0, 4.0, 9.0] {
        let mut k = PlateReverbKernel::new(SR);
        let p = PlateReverbParams {
            decay_s: decay,
            mix_pct: 100.0,
            ..Default::default()
        };
        let t = rt60(|x| k.process_stereo(x, x, &p).0, decay * 2.0 + 2.0);
        assert!(
            (t / decay - 1.0).abs() < 0.25,
            "plate decay {decay}: RT60 {t:.2} s"
        );
    }
}

#[test]
fn spring_decay_sets_rt60() {
    for decay in [1.0, 2.5, 4.5] {
        let mut k = SpringReverbKernel::new(SR);
        let p = SpringReverbParams {
            decay_s: decay,
            mix_pct: 100.0,
            ..Default::default()
        };
        let t = rt60(|x| k.process_stereo(x, x, &p).0, decay * 2.0 + 2.0);
        assert!(
            (t / decay - 1.0).abs() < 0.25,
            "spring decay {decay}: RT60 {t:.2} s"
        );
    }
}

#[test]
fn reverbs_play_fully_wet_at_the_dry_level() {
    // Regression: fully wet, the plate measured 7.5 dB and Harmonic Habitat
    // 10.4 dB under the dry signal, so turning the mix up made them quieter.
    use sonido_analysis::loudness::LufsMeter;
    use sonido_analysis::signals::strummed_guitar;
    let lufs = |l: &[f32], r: &[f32]| {
        let mut m = LufsMeter::new(SR);
        m.push_samples(l, r);
        m.integrated()
    };
    let registry = sonido_registry::EffectRegistry::new();
    let input = strummed_guitar(SR, 6.0, 0.1);
    let dry = lufs(&input, &input);
    for id in [
        "reverb",
        "plate_reverb",
        "spring_reverb",
        "harmonic_habitat",
    ] {
        let mut fx = registry.create(id, SR).unwrap();
        let mix = (0..fx.effect_param_count())
            .find(|&i| fx.effect_param_info(i).unwrap().matches_key("mix"))
            .unwrap();
        fx.effect_set_param(mix, 100.0);
        fx.reset();
        let (mut out_l, mut out_r) = (Vec::new(), Vec::new());
        for chunk in input.chunks(32) {
            let (mut l, mut r) = (chunk.to_vec(), chunk.to_vec());
            fx.process_block_stereo_inplace(&mut l, &mut r);
            out_l.extend(l);
            out_r.extend(r);
        }
        let gain = lufs(&out_l, &out_r) - dry;
        assert!(gain.abs() < 1.5, "{id} fully wet: {gain:+.1} dB vs dry");
    }
}
