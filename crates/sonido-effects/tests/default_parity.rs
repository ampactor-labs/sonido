//! Default parity: `XxxParams::default()` must equal each descriptor's default.
//!
//! The registry path (plugins, editor, pedal) starts a kernel from its
//! descriptor defaults (`Adapter::new` → `KernelParams::from_defaults`), while
//! direct kernel use starts from `XxxParams::default()`. "One kernel, every
//! target" requires both to be the same sound, so every param of every kernel
//! is checked here.

use sonido_core::KernelParams;
use sonido_effects::kernels::*;

/// Returns one violation string per param whose `Default` value differs
/// from its descriptor default.
fn check<P: KernelParams>(name: &str, out: &mut Vec<String>) {
    let p = P::default();
    for i in 0..P::COUNT {
        let desc = P::descriptor(i).expect("descriptor for every index < COUNT");
        let got = p.get(i);
        if (got - desc.default).abs() > 1e-6 * desc.default.abs().max(1.0) {
            out.push(format!(
                "{name}[{i}] {}: Default = {got}, descriptor default = {}",
                desc.name, desc.default
            ));
        }
    }
}

#[test]
fn params_default_matches_descriptor_defaults() {
    let mut v = Vec::new();
    check::<AmpParams>("amp", &mut v);
    check::<BitcrusherParams>("bitcrusher", &mut v);
    check::<CabinetParams>("cabinet", &mut v);
    check::<ChorusParams>("chorus", &mut v);
    check::<CompressorParams>("compressor", &mut v);
    check::<DeesserParams>("deesser", &mut v);
    check::<DelayParams>("delay", &mut v);
    check::<DistortionParams>("distortion", &mut v);
    check::<DroneParams>("drone", &mut v);
    check::<EqParams>("eq", &mut v);
    check::<FilterParams>("filter", &mut v);
    check::<FlangerParams>("flanger", &mut v);
    check::<GateParams>("gate", &mut v);
    check::<GlitchParams>("glitch", &mut v);
    check::<HarmonicHabitatParams>("harmonic_habitat", &mut v);
    check::<LimiterParams>("limiter", &mut v);
    check::<LooperParams>("looper", &mut v);
    check::<MultibandCompParams>("multiband_comp", &mut v);
    check::<PhaserParams>("phaser", &mut v);
    check::<PitchShiftParams>("pitch_shift", &mut v);
    check::<PlateReverbParams>("plate_reverb", &mut v);
    check::<PreampParams>("preamp", &mut v);
    check::<ReverbParams>("reverb", &mut v);
    check::<RingModParams>("ringmod", &mut v);
    check::<ShelvingEqParams>("shelving_eq", &mut v);
    check::<SpringReverbParams>("spring_reverb", &mut v);
    check::<StageParams>("stage", &mut v);
    check::<StereoWidenerParams>("stereo_widener", &mut v);
    check::<TapeParams>("tape", &mut v);
    check::<TextureParams>("texture", &mut v);
    check::<TimeStretchParams>("time_stretch", &mut v);
    check::<TransientShaperParams>("transient_shaper", &mut v);
    check::<TremoloParams>("tremolo", &mut v);
    check::<TunerParams>("tuner", &mut v);
    check::<VibratoParams>("vibrato", &mut v);
    check::<WahParams>("wah", &mut v);
    assert!(
        v.is_empty(),
        "{} default mismatches:\n{}",
        v.len(),
        v.join("\n")
    );
}
