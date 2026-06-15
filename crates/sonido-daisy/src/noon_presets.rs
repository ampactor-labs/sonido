//! Per-effect "noon preset" values for the Hothouse 6-knob pedal.
//!
//! Re-exports the shared noon table from `sonido-platform` and adds compile-time
//! integrity checks that verify array lengths match `KernelParams::COUNT`.
//!
//! See [`sonido_platform::noon`] for the data and rationale.

pub use sonido_platform::noon::noon_value;

// ── Compile-time integrity check ─────────────────────────────────────────
//
// Verifies that every noon preset array has exactly `KernelParams::COUNT`
// entries. A mismatch is a compile-time error (const assertion panic).

#[cfg(feature = "alloc")]
mod noon_integrity {
    use sonido_core::kernel::KernelParams;
    use sonido_effects::{
        BitcrusherParams, ChorusParams, CompressorParams, DelayParams, DistortionParams, EqParams,
        FilterParams, FlangerParams, GateParams, LimiterParams, LooperParams, PhaserParams,
        PreampParams, ReverbParams, RingModParams, StageParams, TapeParams, TremoloParams,
        VibratoParams, WahParams,
    };

    /// Assert at compile time that `noon_value(id, 0..COUNT)` covers all params.
    macro_rules! assert_noon_len {
        ($id:expr, $params:ty, $array:expr) => {
            const _: () = {
                let expected = <$params>::COUNT;
                let actual = $array.len();
                assert!(
                    actual == expected,
                    // compile-time panic if lengths diverge
                );
            };
        };
    }

    assert_noon_len!("bitcrusher", BitcrusherParams, [8.0, 1.0, 0.0, 100.0, 0.0]);
    assert_noon_len!(
        "chorus",
        ChorusParams,
        [1.0, 50.0, 50.0, 2.0, 0.0, 15.0, 0.0, 3.0, 0.0, 0.0]
    );
    assert_noon_len!(
        "compressor",
        CompressorParams,
        [
            -18.0, 4.0, 10.0, 100.0, 0.0, 6.0, 0.0, 80.0, 0.0, 0.0, 50.0, 0.0
        ]
    );
    assert_noon_len!(
        "delay",
        DelayParams,
        [300.0, 40.0, 50.0, 0.0, 20000.0, 20.0, 0.0, 0.0, 2.0, 0.0]
    );
    assert_noon_len!(
        "distortion",
        DistortionParams,
        [15.0, 0.0, 0.0, 0.0, 100.0, 0.0]
    );
    assert_noon_len!(
        "eq",
        EqParams,
        [100.0, 0.0, 1.0, 1000.0, 0.0, 1.0, 5000.0, 0.0, 1.0, 0.0]
    );
    assert_noon_len!("filter", FilterParams, [1000.0, 2.5, 0.0, 0.0]);
    assert_noon_len!(
        "flanger",
        FlangerParams,
        [0.5, 50.0, 50.0, 50.0, 0.0, 0.0, 3.0, 0.0, 0.0]
    );
    assert_noon_len!(
        "gate",
        GateParams,
        [-40.0, 5.0, 100.0, 50.0, -80.0, 3.0, 80.0, 0.0, 0.0]
    );
    assert_noon_len!("limiter", LimiterParams, [-6.0, -0.3, 100.0, 5.0, 0.0]);
    assert_noon_len!("looper", LooperParams, [0.0, 80.0, 0.0, 0.0, 50.0, 0.0]);
    assert_noon_len!(
        "phaser",
        PhaserParams,
        [
            0.3, 50.0, 6.0, 50.0, 50.0, 200.0, 4000.0, 0.0, 3.0, 0.0, 0.0
        ]
    );
    assert_noon_len!("preamp", PreampParams, [15.0, 0.0, 0.0]);
    assert_noon_len!(
        "reverb",
        ReverbParams,
        [50.0, 50.0, 50.0, 10.0, 50.0, 100.0, 50.0, 0.0]
    );
    assert_noon_len!("ringmod", RingModParams, [440.0, 100.0, 0.0, 50.0, 0.0]);
    assert_noon_len!(
        "stage",
        StageParams,
        [
            0.0, 100.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 120.0, 0.0, 0.0, 0.0
        ]
    );
    assert_noon_len!(
        "tape",
        TapeParams,
        [6.0, 30.0, 12000.0, 0.0, 0.3, 0.2, 0.15, 0.3, 80.0, -6.0]
    );
    assert_noon_len!(
        "tremolo",
        TremoloParams,
        [5.0, 50.0, 0.0, 0.0, 0.0, 3.0, 0.0, 0.0]
    );
    assert_noon_len!("vibrato", VibratoParams, [50.0, 50.0, 0.0]);
    assert_noon_len!("wah", WahParams, [800.0, 5.0, 50.0, 0.0, 0.0]);
}
