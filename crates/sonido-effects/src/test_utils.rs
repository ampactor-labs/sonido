//! Zero-config kernel testing macro.
//!
//! `test_kernel!` generates a proptest suite that validates fundamental kernel
//! invariants: finite output, reset clears state, morph validity, param bounds,
//! and panic-free processing with extreme inputs.
//!
//! # Usage
//!
//! ```rust,ignore
//! #[cfg(test)]
//! mod tests {
//!     use super::*;
//!     test_kernel!(DistortionKernel, DistortionParams);
//! }
//! ```

/// Generates a comprehensive proptest suite for a [`DspKernel`](sonido_core::DspKernel)
/// + [`KernelParams`](sonido_core::KernelParams) pair.
///
/// # Tests generated
///
/// 1. **`test_param_descriptors_valid`** — all indices `0..COUNT` return `Some`, each
///    descriptor satisfies `min <= default <= max` and has a non-empty name.
/// 2. **`test_finite_output`** — random params within descriptor bounds plus random
///    input in `[-1.0, 1.0]` produce no `NaN` or `Inf` over 512 samples.
/// 3. **`test_reset_clears_state`** — a kernel reset after warmup matches a fresh
///    kernel when both process 64 samples with default params (tolerance 0.02 to
///    accommodate path-dependent effects like tape hysteresis).
/// 4. **`test_morph_valid`** — `lerp()` between two random param sets at five `t`
///    values produces params within descriptor bounds and finite output.
/// 5. **`test_extreme_input`** — processing with inputs `±100.0`, `0.0`, and
///    `f32::MIN_POSITIVE` does not panic and yields finite output.
///
/// # Sample rate
///
/// The two-argument form defaults to 48000.0 Hz. The three-argument form accepts
/// an explicit sample rate expression as the third argument.
///
/// # Example
///
/// ```rust,ignore
/// #[cfg(test)]
/// mod tests {
///     use super::*;
///     // Uses default 48000 Hz sample rate:
///     test_kernel!(DistortionKernel, DistortionParams);
///     // Or with an explicit sample rate:
///     test_kernel!(DistortionKernel, DistortionParams, 44100.0);
/// }
/// ```
#[macro_export]
macro_rules! test_kernel {
    ($kernel:ty, $params:ty) => {
        $crate::test_kernel!($kernel, $params, 48000.0_f32);
    };
    ($kernel:ty, $params:ty, $sample_rate:expr) => {
        // ── Helper: build a Params from a Vec<f32> of normalised [0,1] values ──
        //
        // Each value is linearly mapped into the descriptor's [min, max] range.
        // STEPPED params are rounded to the nearest integer within the range.
        fn __build_params_from_normalized(raw: &[f32]) -> $params {
            use sonido_core::ParamFlags;
            let mut p = <$params>::from_defaults();
            for (i, &r) in raw.iter().enumerate() {
                if let Some(desc) = <$params>::descriptor(i) {
                    let range = desc.max - desc.min;
                    let v = if desc.flags.contains(ParamFlags::STEPPED) {
                        // Map to integer steps: round to nearest int in [min, max]
                        let stepped = desc.min + (r * range).round();
                        stepped.clamp(desc.min, desc.max)
                    } else {
                        (desc.min + r * range).clamp(desc.min, desc.max)
                    };
                    p.set(i, v);
                }
            }
            p
        }

        // ── 1. Param descriptor validity (regular #[test], no proptest) ──
        #[test]
        fn test_param_descriptors_valid() {
            use sonido_core::KernelParams;
            let count = <$params>::COUNT;
            assert!(count > 0, "COUNT must be > 0");
            for i in 0..count {
                let desc = <$params>::descriptor(i)
                    .unwrap_or_else(|| panic!("descriptor({i}) returned None, expected Some"));
                assert!(!desc.name.is_empty(), "descriptor({i}).name is empty");
                assert!(
                    desc.min <= desc.default,
                    "descriptor({i}): min ({}) > default ({})",
                    desc.min,
                    desc.default
                );
                assert!(
                    desc.default <= desc.max,
                    "descriptor({i}): default ({}) > max ({})",
                    desc.default,
                    desc.max
                );
                assert!(
                    desc.min <= desc.max,
                    "descriptor({i}): min ({}) > max ({})",
                    desc.min,
                    desc.max
                );
            }
            // Index past end must return None.
            assert!(
                <$params>::descriptor(count).is_none(),
                "descriptor({count}) should be None (past COUNT)"
            );
        }

        // ── 2. Finite output (proptest) ──
        proptest::proptest! {
            #[test]
            fn test_finite_output(
                raw_params in proptest::collection::vec(0.0_f32..=1.0_f32, <$params>::COUNT),
                input_l in -1.0_f32..=1.0_f32,
                input_r in -1.0_f32..=1.0_f32,
            ) {
                use sonido_core::DspKernel;
                let params = __build_params_from_normalized(&raw_params);
                let mut kernel = <$kernel>::new($sample_rate);
                for _ in 0..512 {
                    let (l, r) = kernel.process_stereo(input_l, input_r, &params);
                    proptest::prop_assert!(
                        l.is_finite(),
                        "Left output is not finite: {l}"
                    );
                    proptest::prop_assert!(
                        r.is_finite(),
                        "Right output is not finite: {r}"
                    );
                }
            }
        }

        // ── 3. Reset clears state (proptest) ──
        proptest::proptest! {
            #[test]
            fn test_reset_clears_state(
                raw_warmup in proptest::collection::vec(0.0_f32..=1.0_f32, <$params>::COUNT),
                input_l in -1.0_f32..=1.0_f32,
                input_r in -1.0_f32..=1.0_f32,
            ) {
                use sonido_core::DspKernel;
                let warmup_params = __build_params_from_normalized(&raw_warmup);
                let default_params = <$params>::from_defaults();

                // Kernel A: warm up, then reset.
                let mut kernel_a = <$kernel>::new($sample_rate);
                for _ in 0..256 {
                    let _ = kernel_a.process_stereo(input_l, input_r, &warmup_params);
                }
                kernel_a.reset();

                // Kernel B: fresh instance.
                let mut kernel_b = <$kernel>::new($sample_rate);

                // Both should now behave identically from silence.
                for _ in 0..64 {
                    let (al, ar) = kernel_a.process_stereo(0.0, 0.0, &default_params);
                    let (bl, br) = kernel_b.process_stereo(0.0, 0.0, &default_params);

                    // Tolerance 0.02: tape hysteresis path-dependence means exact equality
                    // is not guaranteed, but gross state leakage will exceed this threshold.
                    proptest::prop_assert!(
                        (al - bl).abs() <= 0.02,
                        "Left diverged after reset: kernel_a={al}, kernel_b={bl}"
                    );
                    proptest::prop_assert!(
                        (ar - br).abs() <= 0.02,
                        "Right diverged after reset: kernel_a={ar}, kernel_b={br}"
                    );
                }
            }
        }

        // ── 4. Morph validity (proptest) ──
        proptest::proptest! {
            #[test]
            fn test_morph_valid(
                raw_a in proptest::collection::vec(0.0_f32..=1.0_f32, <$params>::COUNT),
                raw_b in proptest::collection::vec(0.0_f32..=1.0_f32, <$params>::COUNT),
            ) {
                use sonido_core::{DspKernel, KernelParams};
                let pa = __build_params_from_normalized(&raw_a);
                let pb = __build_params_from_normalized(&raw_b);

                for &t in &[0.0_f32, 0.25, 0.5, 0.75, 1.0] {
                    let morphed = <$params>::lerp(&pa, &pb, t);

                    // All morphed param values must stay within descriptor bounds.
                    for i in 0..<$params>::COUNT {
                        if let Some(desc) = <$params>::descriptor(i) {
                            let v = morphed.get(i);
                            proptest::prop_assert!(
                                v >= desc.min - 1e-4 && v <= desc.max + 1e-4,
                                "lerp at t={t}, param[{i}]={v} outside [{}, {}]",
                                desc.min, desc.max
                            );
                        }
                    }

                    // Processing with morphed params must produce finite output.
                    let mut kernel = <$kernel>::new($sample_rate);
                    let (l, r) = kernel.process_stereo(0.3, -0.3, &morphed);
                    proptest::prop_assert!(
                        l.is_finite(),
                        "Left NaN/Inf at morph t={t}: {l}"
                    );
                    proptest::prop_assert!(
                        r.is_finite(),
                        "Right NaN/Inf at morph t={t}: {r}"
                    );
                }
            }
        }

        // ── 5. Extreme input (regular #[test]) ──
        #[test]
        fn test_extreme_input() {
            use sonido_core::DspKernel;
            let params = <$params>::from_defaults();
            let extreme_inputs: &[f32] = &[-100.0, 100.0, 0.0, f32::MIN_POSITIVE];

            for &input in extreme_inputs {
                let mut kernel = <$kernel>::new($sample_rate);
                let (l, r) = kernel.process_stereo(input, input, &params);
                assert!(
                    l.is_finite(),
                    "Left output is not finite for input {input}: {l}"
                );
                assert!(
                    r.is_finite(),
                    "Right output is not finite for input {input}: {r}"
                );
            }
        }
    };
}
