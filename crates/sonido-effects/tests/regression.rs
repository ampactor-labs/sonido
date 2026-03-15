//! Regression test framework for sonido effects
//!
//! This framework ensures effect implementations remain stable across changes
//! by comparing current output against saved golden files.
//!
//! # Usage
//!
//! Run with `REGENERATE_GOLDEN=1` to create/update golden files:
//! ```bash
//! REGENERATE_GOLDEN=1 cargo test --test regression
//! ```
//!
//! Run normally to verify against golden files:
//! ```bash
//! cargo test --test regression
//! ```

use sonido_analysis::compare::{mse, snr_db, spectral_correlation};
use sonido_core::{Effect, KernelAdapter, ParameterInfo};
use sonido_effects::kernels::{
    AmpKernel, BitcrusherKernel, CabinetKernel, ChorusKernel, CompressorKernel, DeesserKernel,
    DelayKernel, DistortionKernel, EqKernel, FilterKernel, FlangerKernel, GateKernel,
    LimiterKernel, LooperKernel, MultibandCompKernel, PhaserKernel, PitchShiftKernel,
    PlateReverbKernel, PreampKernel, ReverbKernel, RingModKernel, ShelvingEqKernel,
    SpringReverbKernel, StageKernel, StereoWidenerKernel, TapeKernel, TransientShaperKernel,
    TremoloKernel, TunerKernel, VibratoKernel, WahKernel,
};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;

const SAMPLE_RATE: f32 = 48000.0;
const TEST_DURATION_SAMPLES: usize = 4800; // 100ms at 48kHz
const GOLDEN_DIR: &str = "tests/golden";

/// Mean Squared Error threshold for regression detection.
///
/// Chosen at 1e-6 based on f32 precision characteristics:
/// - f32 mantissa provides ~7 decimal digits of precision (~1.2e-7 relative error)
/// - 24-bit audio has a noise floor around -144 dBFS, corresponding to ~1e-7 amplitude
/// - 1e-6 provides approximately 1 bit of margin above the f32 precision limit,
///   allowing for minor floating-point rounding differences across compiler versions
///   or instruction reordering while still catching any meaningful algorithmic change
const MSE_THRESHOLD: f32 = 1e-6;

/// Signal-to-Noise Ratio threshold in decibels for regression detection.
///
/// Set at 60 dB, which corresponds to ~10-bit effective resolution (2^10 = 1024,
/// 20*log10(1024) ≈ 60 dB). This threshold accounts for accumulated f32 rounding
/// errors across multi-stage DSP chains (biquad coefficient calculations, feedback
/// paths, parameter smoothing), where each stage contributes small rounding errors
/// that compound. 60 dB is well above perceptual thresholds (~40 dB for subtle
/// artifacts) while allowing the headroom needed for legitimate floating-point
/// variation across platforms and optimization levels.
const SNR_THRESHOLD_DB: f32 = 60.0;

/// Spectral correlation threshold for regression detection.
///
/// Set at 0.9999 (four nines) to ensure the frequency-domain content is virtually
/// identical between current and golden outputs. This metric is sensitive to:
/// - New harmonics introduced by algorithm changes (e.g., distortion mode tweaks)
/// - Shifted resonance peaks from filter coefficient modifications
/// - Spectral smearing from changes to windowing or interpolation
///
/// A correlation below 0.9999 indicates measurable spectral deviation that would
/// be audible in A/B comparison. The threshold is stricter than time-domain MSE
/// because small phase shifts (inaudible) inflate MSE but leave spectral
/// correlation intact, making this metric a more targeted detector of timbral
/// changes.
const SPECTRAL_CORRELATION_THRESHOLD: f32 = 0.9999;

/// Generate a deterministic test signal (multi-frequency sine)
fn generate_test_signal(size: usize) -> Vec<f32> {
    (0..size)
        .map(|i| {
            let t = i as f32 / SAMPLE_RATE;
            let fundamental = (2.0 * std::f32::consts::PI * 440.0 * t).sin();
            let harmonic2 = 0.3 * (2.0 * std::f32::consts::PI * 880.0 * t).sin();
            let harmonic3 = 0.2 * (2.0 * std::f32::consts::PI * 1320.0 * t).sin();
            (fundamental + harmonic2 + harmonic3) * 0.5
        })
        .collect()
}

/// Generate an impulse signal for reverb/delay testing
fn generate_impulse(size: usize) -> Vec<f32> {
    let mut signal = vec![0.0; size];
    signal[0] = 1.0;
    signal
}

/// Get the golden file path for an effect
fn golden_path(effect_name: &str) -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir)
        .join(GOLDEN_DIR)
        .join(format!("{}.golden", effect_name))
}

/// Save output to golden file
fn save_golden(effect_name: &str, output: &[f32]) -> std::io::Result<()> {
    let path = golden_path(effect_name);
    let file = File::create(&path)?;
    let mut writer = BufWriter::new(file);
    for sample in output {
        writeln!(writer, "{:.10}", sample)?;
    }
    Ok(())
}

/// Load golden file
fn load_golden(effect_name: &str) -> std::io::Result<Vec<f32>> {
    let path = golden_path(effect_name);
    let file = File::open(&path)?;
    let reader = BufReader::new(file);
    let mut samples = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if let Ok(sample) = line.trim().parse::<f32>() {
            samples.push(sample);
        }
    }
    Ok(samples)
}

/// Check if we should regenerate golden files
fn should_regenerate() -> bool {
    std::env::var("REGENERATE_GOLDEN").is_ok()
}

/// Run regression test for an effect
fn run_regression_test<E: Effect>(
    effect_name: &str,
    mut effect: E,
    input: &[f32],
) -> Result<(), String> {
    // Process input through effect
    let mut output = vec![0.0; input.len()];
    effect.process_block(input, &mut output);

    if should_regenerate() {
        // Save new golden file
        save_golden(effect_name, &output)
            .map_err(|e| format!("Failed to save golden file: {}", e))?;
        println!("Regenerated golden file for {}", effect_name);
        return Ok(());
    }

    // Load expected output
    let expected = load_golden(effect_name).map_err(|e| {
        format!(
            "Failed to load golden file for {} (run with REGENERATE_GOLDEN=1 to create): {}",
            effect_name, e
        )
    })?;

    // Verify lengths match
    if output.len() != expected.len() {
        return Err(format!(
            "{}: Output length mismatch (got {}, expected {})",
            effect_name,
            output.len(),
            expected.len()
        ));
    }

    // Compare using multiple metrics
    let mse_val = mse(&output, &expected);
    let snr = snr_db(&expected, &output);
    let correlation = spectral_correlation(&output, &expected, 2048.min(output.len()));

    // Check thresholds
    let mut errors = Vec::new();

    if mse_val > MSE_THRESHOLD {
        errors.push(format!(
            "MSE {} exceeds threshold {} (sample deviation)",
            mse_val, MSE_THRESHOLD
        ));
    }

    if snr < SNR_THRESHOLD_DB && snr.is_finite() {
        errors.push(format!(
            "SNR {:.1} dB below threshold {:.1} dB",
            snr, SNR_THRESHOLD_DB
        ));
    }

    if correlation < SPECTRAL_CORRELATION_THRESHOLD {
        errors.push(format!(
            "Spectral correlation {:.6} below threshold {:.6}",
            correlation, SPECTRAL_CORRELATION_THRESHOLD
        ));
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} regression detected:\n  - {}",
            effect_name,
            errors.join("\n  - ")
        ))
    }
}

// Individual effect regression tests

#[test]
fn test_distortion_regression() {
    // Distortion params: 0=drive_db, 1=tone_db, 2=output_db, 3=shape, 4=mix_pct
    let mut effect = KernelAdapter::new(DistortionKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 15.0); // drive_db
    effect.set_param(1, 3.0); // tone_db

    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("distortion", effect, &input).expect("Distortion regression test failed");
}

#[test]
fn test_compressor_regression() {
    // Compressor params: 0=threshold_db, 1=ratio, 2=attack_ms, 3=release_ms, ...
    let mut effect = KernelAdapter::new(CompressorKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, -20.0); // threshold_db
    effect.set_param(1, 4.0); // ratio
    effect.set_param(2, 5.0); // attack_ms
    effect.set_param(3, 50.0); // release_ms

    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("compressor", effect, &input).expect("Compressor regression test failed");
}

#[test]
fn test_chorus_regression() {
    // Chorus params: 0=rate, 1=depth_pct, 2=mix_pct, ...
    let mut effect = KernelAdapter::new(ChorusKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 2.0); // rate Hz
    effect.set_param(1, 70.0); // depth_pct (classic: 0.7 -> 70%)
    effect.set_param(2, 50.0); // mix_pct (classic: 0.5 -> 50%)

    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("chorus", effect, &input).expect("Chorus regression test failed");
}

#[test]
fn test_delay_regression() {
    // Delay params: 0=time_ms, 1=feedback_pct, 2=mix_pct, 3=ping_pong, ...
    let mut effect = KernelAdapter::new(DelayKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 20.0); // time_ms
    effect.set_param(1, 50.0); // feedback_pct (classic: 0.5 -> 50%)
    effect.set_param(2, 50.0); // mix_pct (classic: 0.5 -> 50%)

    // Use test signal instead of impulse for better spectral comparison
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("delay", effect, &input).expect("Delay regression test failed");
}

#[test]
fn test_reverb_regression() {
    // Reverb params: 0=room_size_pct, 1=decay_pct, 2=damping_pct, 3=predelay_ms, 4=mix_pct, ...
    let mut effect = KernelAdapter::new(ReverbKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 70.0); // room_size_pct (classic: 0.7 -> 70%)
    effect.set_param(1, 60.0); // decay_pct (classic: 0.6 -> 60%)
    effect.set_param(2, 30.0); // damping_pct (classic: 0.3 -> 30%)
    effect.set_param(3, 10.0); // predelay_ms
    effect.set_param(4, 50.0); // mix_pct (classic: 0.5 -> 50%)

    let input = generate_impulse(TEST_DURATION_SAMPLES);
    run_regression_test("reverb", effect, &input).expect("Reverb regression test failed");
}

#[test]
fn test_lowpass_regression() {
    // Filter params: 0=cutoff_hz, 1=resonance, 2=output_db
    let mut effect = KernelAdapter::new(FilterKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 1000.0); // cutoff_hz
    effect.set_param(1, 2.0); // resonance (Q)

    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("lowpass", effect, &input).expect("LowPass regression test failed");
}

#[test]
fn test_phaser_regression() {
    // Phaser params: 0=rate, 1=depth_pct, 2=stages, 3=feedback_pct, 4=mix_pct, ...
    let mut effect = KernelAdapter::new(PhaserKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 1.0); // rate Hz
    effect.set_param(1, 80.0); // depth_pct (classic: 0.8 -> 80%)
    effect.set_param(3, 50.0); // feedback_pct (classic: 0.5 -> 50%)

    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("phaser", effect, &input).expect("Phaser regression test failed");
}

#[test]
fn test_flanger_regression() {
    // Flanger params: 0=rate, 1=depth_pct, 2=feedback_pct, 3=mix_pct, ...
    let mut effect = KernelAdapter::new(FlangerKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 0.5); // rate Hz
    effect.set_param(1, 70.0); // depth_pct (classic: 0.7 -> 70%)
    effect.set_param(2, 50.0); // feedback_pct (classic: 0.5 -> 50%)
    effect.set_param(3, 50.0); // mix_pct (classic: 0.5 -> 50%)

    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("flanger", effect, &input).expect("Flanger regression test failed");
}

#[test]
fn test_tremolo_regression() {
    // Tremolo params: 0=rate, 1=depth_pct, 2=waveform, ...
    let mut effect = KernelAdapter::new(TremoloKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 5.0); // rate Hz
    effect.set_param(1, 80.0); // depth_pct (classic: 0.8 -> 80%)

    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("tremolo", effect, &input).expect("Tremolo regression test failed");
}

#[test]
fn test_tape_regression() {
    // Tape params: 0=drive_db, 1=saturation_pct, ...
    // Classic: set_drive(2.0) linear -> kernel drive_db
    // Classic: set_saturation(0.6) 0-1 -> kernel saturation_pct 0-100
    // from_knobs: drive_db = knob * 24.0, so drive=2.0 on classic (0.1-10 linear)
    // isn't directly comparable. We use from_knobs-style conversion: for the test,
    // set drive_db and saturation_pct to reasonable values that represent
    // similar processing characteristics.
    let mut effect = KernelAdapter::new(TapeKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 6.0); // drive_db (moderate drive)
    effect.set_param(1, 60.0); // saturation_pct (classic: 0.6 -> 60%)

    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("tape", effect, &input).expect("TapeSaturation regression test failed");
}

#[test]
fn test_clean_preamp_regression() {
    // Preamp params: 0=gain_db, 1=tone_db, 2=output_db
    let mut effect = KernelAdapter::new(PreampKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 12.0); // gain_db
    effect.set_param(2, -6.0); // output_db

    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("clean_preamp", effect, &input)
        .expect("CleanPreamp regression test failed");
}

#[test]
fn test_vibrato_regression() {
    // MultiVibrato params: 0=depth_pct, 1=mix_pct, 2=output_db
    // Classic: set_depth(0.8) 0-4 scale -> depth_pct 0-100
    // Classic depth scale is 0-4, kernel depth_pct is 0-100
    // Classic set_depth(0.8) with clamp 0-4 -> kernel default depth range is 0-100%
    let mut effect = KernelAdapter::new(VibratoKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 80.0); // depth_pct (classic: 0.8 -> 80%)
    effect.set_param(1, 100.0); // mix_pct (classic: 1.0 -> 100%)

    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("vibrato", effect, &input).expect("MultiVibrato regression test failed");
}

#[test]
fn test_gate_regression() {
    // Gate params: 0=threshold_db, 1=attack_ms, 2=release_ms, ...
    let mut effect = KernelAdapter::new(GateKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, -30.0); // threshold_db
    effect.set_param(1, 1.0); // attack_ms
    effect.set_param(2, 50.0); // release_ms

    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("gate", effect, &input).expect("Gate regression test failed");
}

#[test]
fn test_wah_regression() {
    // Wah params: 0=freq_hz, 1=resonance, 2=sensitivity_pct, 3=mode, 4=output_db
    let mut effect = KernelAdapter::new(WahKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 1500.0); // freq_hz
    effect.set_param(1, 4.0); // resonance

    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("wah", effect, &input).expect("Wah regression test failed");
}

#[test]
fn test_eq_regression() {
    // EQ params: 0=low_freq, 1=low_gain_db, 2=low_q, 3=mid_freq, 4=mid_gain_db, ...
    let mut effect = KernelAdapter::new(EqKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(1, 3.0); // low_gain_db
    effect.set_param(4, -2.0); // mid_gain_db
    effect.set_param(3, 1000.0); // mid_freq
    effect.set_param(7, 2.0); // high_gain_db

    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("eq", effect, &input).expect("ParametricEq regression test failed");
}

// Default-parameter regression tests
//
// These verify that each effect's output at factory defaults remains stable.
// Unlike the tests above (which set specific param values), these use only
// `KernelAdapter::new()` with no overrides.

#[test]
fn test_distortion_defaults_regression() {
    let effect = KernelAdapter::new(DistortionKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("distortion_defaults", effect, &input)
        .expect("Distortion defaults regression test failed");
}

#[test]
fn test_compressor_defaults_regression() {
    let effect = KernelAdapter::new(CompressorKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("compressor_defaults", effect, &input)
        .expect("Compressor defaults regression test failed");
}

#[test]
fn test_chorus_defaults_regression() {
    let effect = KernelAdapter::new(ChorusKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("chorus_defaults", effect, &input)
        .expect("Chorus defaults regression test failed");
}

#[test]
fn test_delay_defaults_regression() {
    let effect = KernelAdapter::new(DelayKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("delay_defaults", effect, &input)
        .expect("Delay defaults regression test failed");
}

#[test]
fn test_reverb_defaults_regression() {
    let effect = KernelAdapter::new(ReverbKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("reverb_defaults", effect, &input)
        .expect("Reverb defaults regression test failed");
}

#[test]
fn test_lowpass_defaults_regression() {
    let effect = KernelAdapter::new(FilterKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("lowpass_defaults", effect, &input)
        .expect("LowPass defaults regression test failed");
}

#[test]
fn test_phaser_defaults_regression() {
    let effect = KernelAdapter::new(PhaserKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("phaser_defaults", effect, &input)
        .expect("Phaser defaults regression test failed");
}

#[test]
fn test_flanger_defaults_regression() {
    let effect = KernelAdapter::new(FlangerKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("flanger_defaults", effect, &input)
        .expect("Flanger defaults regression test failed");
}

#[test]
fn test_tremolo_defaults_regression() {
    let effect = KernelAdapter::new(TremoloKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("tremolo_defaults", effect, &input)
        .expect("Tremolo defaults regression test failed");
}

#[test]
fn test_tape_defaults_regression() {
    let effect = KernelAdapter::new(TapeKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("tape_defaults", effect, &input)
        .expect("TapeSaturation defaults regression test failed");
}

#[test]
fn test_clean_preamp_defaults_regression() {
    let effect = KernelAdapter::new(PreampKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("clean_preamp_defaults", effect, &input)
        .expect("CleanPreamp defaults regression test failed");
}

#[test]
fn test_vibrato_defaults_regression() {
    let effect = KernelAdapter::new(VibratoKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("vibrato_defaults", effect, &input)
        .expect("MultiVibrato defaults regression test failed");
}

#[test]
fn test_gate_defaults_regression() {
    let effect = KernelAdapter::new(GateKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("gate_defaults", effect, &input)
        .expect("Gate defaults regression test failed");
}

#[test]
fn test_wah_defaults_regression() {
    let effect = KernelAdapter::new(WahKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("wah_defaults", effect, &input)
        .expect("Wah defaults regression test failed");
}

#[test]
fn test_eq_defaults_regression() {
    let effect = KernelAdapter::new(EqKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("eq_defaults", effect, &input)
        .expect("ParametricEq defaults regression test failed");
}

// Stereo regression tests
//
// These verify that true-stereo effects produce decorrelated L/R output
// and remain stable across changes. Each test uses `process_block_stereo`
// and saves separate golden files per channel.

/// Generate a deterministic stereo test signal.
///
/// Left channel: identical to `generate_test_signal()` (440+880+1320 Hz sines).
/// Right channel: same frequencies with 90° phase offset for deterministic
/// L/R decorrelation while preserving identical spectral content.
fn generate_test_signal_stereo(size: usize) -> (Vec<f32>, Vec<f32>) {
    let left = generate_test_signal(size);
    let right = (0..size)
        .map(|i| {
            let t = i as f32 / SAMPLE_RATE;
            let phase_offset = std::f32::consts::FRAC_PI_2;
            let fundamental = (2.0 * std::f32::consts::PI * 440.0 * t + phase_offset).sin();
            let harmonic2 = 0.3 * (2.0 * std::f32::consts::PI * 880.0 * t + phase_offset).sin();
            let harmonic3 = 0.2 * (2.0 * std::f32::consts::PI * 1320.0 * t + phase_offset).sin();
            (fundamental + harmonic2 + harmonic3) * 0.5
        })
        .collect();
    (left, right)
}

/// Normalized cross-correlation between two signals.
///
/// Returns a value in \[-1, 1\] where 1.0 means identical signals.
/// Used to verify that true-stereo effects produce decorrelated L/R output.
fn cross_correlation(a: &[f32], b: &[f32]) -> f32 {
    let ab: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let aa: f32 = a.iter().map(|x| x * x).sum();
    let bb: f32 = b.iter().map(|x| x * x).sum();
    ab / (aa.sqrt() * bb.sqrt())
}

/// Decorrelation threshold for true-stereo effects.
///
/// A cross-correlation below 0.99 between L/R outputs confirms that the
/// effect produces meaningfully different content per channel (different
/// delay taps, LFO phases, comb tunings, etc.) rather than processing
/// each channel identically.
const STEREO_DECORRELATION_THRESHOLD: f32 = 0.99;

/// Run regression test for a stereo effect.
///
/// Processes input through `process_block_stereo`, saves/loads separate golden
/// files for each channel, and verifies decorrelation between L/R outputs.
fn run_regression_test_stereo<E: Effect>(
    effect_name: &str,
    mut effect: E,
    left_in: &[f32],
    right_in: &[f32],
) -> Result<(), String> {
    let len = left_in.len();
    let mut left_out = vec![0.0; len];
    let mut right_out = vec![0.0; len];
    effect.process_block_stereo(left_in, right_in, &mut left_out, &mut right_out);

    // Sanity checks (always run, even during regeneration)
    for (ch_name, ch_out) in [("left", &left_out), ("right", &right_out)] {
        for (i, &s) in ch_out.iter().enumerate() {
            if s.is_nan() || s.is_infinite() {
                return Err(format!(
                    "{} stereo {}: NaN/Inf at sample {}",
                    effect_name, ch_name, i
                ));
            }
            if s.abs() > 10.0 {
                return Err(format!(
                    "{} stereo {}: sample {} out of bounds ({:.4})",
                    effect_name, ch_name, i, s
                ));
            }
        }
        let has_signal = ch_out.iter().any(|&s| s.abs() > 1e-6);
        if !has_signal {
            return Err(format!(
                "{} stereo {}: output is silent",
                effect_name, ch_name
            ));
        }
    }

    let left_name = format!("{}_stereo_left", effect_name);
    let right_name = format!("{}_stereo_right", effect_name);

    if should_regenerate() {
        save_golden(&left_name, &left_out)
            .map_err(|e| format!("Failed to save golden file: {}", e))?;
        save_golden(&right_name, &right_out)
            .map_err(|e| format!("Failed to save golden file: {}", e))?;
        println!("Regenerated stereo golden files for {}", effect_name);

        // Verify decorrelation even during regeneration
        let corr = cross_correlation(&left_out, &right_out);
        if corr >= STEREO_DECORRELATION_THRESHOLD {
            return Err(format!(
                "{} stereo: L/R cross-correlation {:.6} >= {} (not decorrelated)",
                effect_name, corr, STEREO_DECORRELATION_THRESHOLD
            ));
        }

        return Ok(());
    }

    // Load expected output for both channels
    let expected_left = load_golden(&left_name).map_err(|e| {
        format!(
            "Failed to load golden file for {} (run with REGENERATE_GOLDEN=1 to create): {}",
            left_name, e
        )
    })?;
    let expected_right = load_golden(&right_name).map_err(|e| {
        format!(
            "Failed to load golden file for {} (run with REGENERATE_GOLDEN=1 to create): {}",
            right_name, e
        )
    })?;

    let mut errors = Vec::new();

    // Verify both channels against golden files
    for (ch_name, output, expected) in [
        ("left", &left_out, &expected_left),
        ("right", &right_out, &expected_right),
    ] {
        if output.len() != expected.len() {
            errors.push(format!(
                "{} channel: length mismatch (got {}, expected {})",
                ch_name,
                output.len(),
                expected.len()
            ));
            continue;
        }

        let mse_val = mse(output, expected);
        let snr = snr_db(expected, output);
        let correlation = spectral_correlation(output, expected, 2048.min(output.len()));

        if mse_val > MSE_THRESHOLD {
            errors.push(format!(
                "{} channel: MSE {} exceeds threshold {}",
                ch_name, mse_val, MSE_THRESHOLD
            ));
        }
        if snr < SNR_THRESHOLD_DB && snr.is_finite() {
            errors.push(format!(
                "{} channel: SNR {:.1} dB below threshold {:.1} dB",
                ch_name, snr, SNR_THRESHOLD_DB
            ));
        }
        if correlation < SPECTRAL_CORRELATION_THRESHOLD {
            errors.push(format!(
                "{} channel: spectral correlation {:.6} below threshold {:.6}",
                ch_name, correlation, SPECTRAL_CORRELATION_THRESHOLD
            ));
        }
    }

    // Decorrelation check
    let corr = cross_correlation(&left_out, &right_out);
    if corr >= STEREO_DECORRELATION_THRESHOLD {
        errors.push(format!(
            "L/R cross-correlation {:.6} >= {} (not decorrelated)",
            corr, STEREO_DECORRELATION_THRESHOLD
        ));
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} stereo regression detected:\n  - {}",
            effect_name,
            errors.join("\n  - ")
        ))
    }
}

#[test]
fn test_chorus_stereo_regression() {
    // Chorus params: 0=rate, 1=depth_pct, 2=mix_pct
    let mut effect = KernelAdapter::new(ChorusKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 2.0); // rate Hz
    effect.set_param(1, 70.0); // depth_pct
    effect.set_param(2, 50.0); // mix_pct
    let (left, right) = generate_test_signal_stereo(TEST_DURATION_SAMPLES);
    run_regression_test_stereo("chorus", effect, &left, &right)
        .expect("Chorus stereo regression test failed");
}

#[test]
fn test_reverb_stereo_regression() {
    // Reverb params: 0=room_size_pct, 1=decay_pct, 2=damping_pct, 3=predelay_ms, 4=mix_pct
    let mut effect = KernelAdapter::new(ReverbKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 70.0); // room_size_pct
    effect.set_param(1, 60.0); // decay_pct
    effect.set_param(2, 30.0); // damping_pct
    effect.set_param(3, 10.0); // predelay_ms
    effect.set_param(4, 50.0); // mix_pct
    // Use impulse input — consistent with existing mono reverb test
    let left = generate_impulse(TEST_DURATION_SAMPLES);
    let right = generate_impulse(TEST_DURATION_SAMPLES);
    run_regression_test_stereo("reverb", effect, &left, &right)
        .expect("Reverb stereo regression test failed");
}

#[test]
fn test_delay_stereo_regression() {
    // Delay params: 0=time_ms, 1=feedback_pct, 2=mix_pct, 3=ping_pong
    let mut effect = KernelAdapter::new(DelayKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 20.0); // time_ms
    effect.set_param(1, 50.0); // feedback_pct
    effect.set_param(2, 50.0); // mix_pct
    effect.set_param(3, 1.0); // ping_pong = true
    let (left, right) = generate_test_signal_stereo(TEST_DURATION_SAMPLES);
    run_regression_test_stereo("delay", effect, &left, &right)
        .expect("Delay stereo regression test failed");
}

#[test]
fn test_phaser_stereo_regression() {
    // Phaser params: 0=rate, 1=depth_pct, 3=feedback_pct
    let mut effect = KernelAdapter::new(PhaserKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 1.0); // rate Hz
    effect.set_param(1, 80.0); // depth_pct
    effect.set_param(3, 50.0); // feedback_pct
    let (left, right) = generate_test_signal_stereo(TEST_DURATION_SAMPLES);
    run_regression_test_stereo("phaser", effect, &left, &right)
        .expect("Phaser stereo regression test failed");
}

#[test]
fn test_flanger_stereo_regression() {
    // Flanger params: 0=rate, 1=depth_pct, 2=feedback_pct, 3=mix_pct
    let mut effect = KernelAdapter::new(FlangerKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 0.5); // rate Hz
    effect.set_param(1, 70.0); // depth_pct
    effect.set_param(2, 50.0); // feedback_pct
    effect.set_param(3, 50.0); // mix_pct
    let (left, right) = generate_test_signal_stereo(TEST_DURATION_SAMPLES);
    run_regression_test_stereo("flanger", effect, &left, &right)
        .expect("Flanger stereo regression test failed");
}

#[test]
fn test_limiter_regression() {
    // Limiter params: 0=threshold_db, 1=ceiling_db, 2=release_ms, 3=lookahead_ms, 4=output_db
    let mut effect = KernelAdapter::new(LimiterKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, -12.0); // threshold_db
    effect.set_param(1, -0.3); // ceiling_db
    effect.set_param(2, 50.0); // release_ms

    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("limiter", effect, &input).expect("Limiter regression test failed");
}

#[test]
fn test_limiter_defaults_regression() {
    let effect = KernelAdapter::new(LimiterKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("limiter_defaults", effect, &input)
        .expect("Limiter defaults regression test failed");
}

#[test]
fn test_bitcrusher_regression() {
    // Bitcrusher params: 0=bits, 1=rate(downsample), 2=jitter_pct, 3=mix_pct, 4=output_db
    let mut effect = KernelAdapter::new(BitcrusherKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 4.0); // bit_depth
    effect.set_param(1, 8.0); // downsample rate

    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("bitcrusher", effect, &input).expect("Bitcrusher regression test failed");
}

#[test]
fn test_bitcrusher_defaults_regression() {
    let effect = KernelAdapter::new(BitcrusherKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("bitcrusher_defaults", effect, &input)
        .expect("Bitcrusher defaults regression test failed");
}

#[test]
fn test_ringmod_regression() {
    // RingMod params: 0=freq_hz, 1=depth_pct, 2=waveform, 3=mix_pct, 4=output_db
    let mut effect = KernelAdapter::new(RingModKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 440.0); // freq_hz
    effect.set_param(1, 100.0); // depth_pct (classic: 1.0 -> 100%)
    effect.set_param(3, 100.0); // mix_pct (classic: 1.0 -> 100%)

    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("ringmod", effect, &input).expect("RingMod regression test failed");
}

#[test]
fn test_ringmod_defaults_regression() {
    let effect = KernelAdapter::new(RingModKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("ringmod_defaults", effect, &input)
        .expect("RingMod defaults regression test failed");
}

#[test]
fn test_stage_regression() {
    // Stage params: 0=gain_db, 1=width_pct, 2=balance_pct, 3=phase_l, 4=phase_r,
    //   5=channel, 6=dc_block, 7=bass_mono, 8=bass_freq_hz, 9=haas_ms, 10=haas_side, 11=output_db
    let mut effect = KernelAdapter::new(StageKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 3.0); // gain_db
    effect.set_param(1, 150.0); // width_pct
    effect.set_param(2, 25.0); // balance_pct
    effect.set_param(7, 1.0); // bass_mono = true
    effect.set_param(9, 10.0); // haas_ms

    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("stage", effect, &input).expect("Stage regression test failed");
}

#[test]
fn test_stage_defaults_regression() {
    let effect = KernelAdapter::new(StageKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("stage_defaults", effect, &input)
        .expect("Stage defaults regression test failed");
}

// ─────────────────────────────────────────────────────────────────────────────
// Phase 5 & 6 kernel tests (amp, cabinet, tuner, pitch_shift, shelving_eq,
// multiband_comp, plate_reverb, spring_reverb, transient_shaper, deesser,
// stereo_widener, looper)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_amp_regression() {
    // AmpKernel params: 0=gain_pct, 1=bass_pct, 2=mid_pct, 3=treble_pct,
    //   4=presence_pct, 5=sag_pct, 6=bright, 7=master_db, 8=output_db
    let mut effect = KernelAdapter::new(AmpKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 60.0); // gain_pct
    effect.set_param(1, 50.0); // bass_pct
    effect.set_param(2, 50.0); // mid_pct
    effect.set_param(3, 60.0); // treble_pct
    effect.set_param(5, 30.0); // sag_pct
    effect.set_param(7, -6.0); // master_db

    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("amp", effect, &input).expect("Amp regression test failed");
}

#[test]
fn test_amp_defaults_regression() {
    let effect = KernelAdapter::new(AmpKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("amp_defaults", effect, &input)
        .expect("Amp defaults regression test failed");
}

#[test]
fn test_cabinet_regression() {
    // CabinetKernel params: 0=ir_select, 1=mix_pct, 2=low_cut_hz, 3=output_db
    let mut effect = KernelAdapter::new(CabinetKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 1.0); // ir_select: British Stack
    effect.set_param(1, 100.0); // mix_pct
    effect.set_param(2, 100.0); // low_cut_hz

    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("cabinet", effect, &input).expect("Cabinet regression test failed");
}

#[test]
fn test_cabinet_defaults_regression() {
    let effect = KernelAdapter::new(CabinetKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("cabinet_defaults", effect, &input)
        .expect("Cabinet defaults regression test failed");
}

#[test]
fn test_tuner_regression() {
    // TunerKernel params: 0=reference_hz, 1=mute, 2=output_db, 3=detected_hz (RO), 4=cents (RO)
    // Use a 440 Hz sine so the tuner detects a real frequency
    let effect = KernelAdapter::new(TunerKernel::new(SAMPLE_RATE), SAMPLE_RATE);

    // Generate 440 Hz sine (A4)
    let input: Vec<f32> = (0..TEST_DURATION_SAMPLES)
        .map(|i| {
            let t = i as f32 / SAMPLE_RATE;
            (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5
        })
        .collect();
    run_regression_test("tuner", effect, &input).expect("Tuner regression test failed");
}

#[test]
fn test_tuner_defaults_regression() {
    let effect = KernelAdapter::new(TunerKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("tuner_defaults", effect, &input)
        .expect("Tuner defaults regression test failed");
}

#[test]
fn test_pitch_shift_regression() {
    // PitchShiftKernel params: 0=semitones, 1=cents, 2=grain_ms, 3=mix_pct, 4=quality, 5=output_db
    let mut effect = KernelAdapter::new(PitchShiftKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 5.0); // semitones: +5
    effect.set_param(1, 0.0); // cents
    effect.set_param(2, 20.0); // grain_ms
    effect.set_param(3, 100.0); // mix_pct

    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("pitch_shift", effect, &input).expect("PitchShift regression test failed");
}

#[test]
fn test_pitch_shift_defaults_regression() {
    let effect = KernelAdapter::new(PitchShiftKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("pitch_shift_defaults", effect, &input)
        .expect("PitchShift defaults regression test failed");
}

#[test]
fn test_shelving_eq_regression() {
    // ShelvingEqKernel params: 0=low_freq_hz, 1=low_gain_db, 2=high_freq_hz, 3=high_gain_db, 4=output_db
    let mut effect = KernelAdapter::new(ShelvingEqKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 150.0); // low_freq_hz
    effect.set_param(1, 4.0); // low_gain_db: +4 dB boost
    effect.set_param(2, 6000.0); // high_freq_hz
    effect.set_param(3, -3.0); // high_gain_db: -3 dB cut

    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("shelving_eq", effect, &input).expect("ShelvingEq regression test failed");
}

#[test]
fn test_shelving_eq_defaults_regression() {
    let effect = KernelAdapter::new(ShelvingEqKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("shelving_eq_defaults", effect, &input)
        .expect("ShelvingEq defaults regression test failed");
}

#[test]
fn test_multiband_comp_regression() {
    // MultibandCompKernel params: 0=xover_low_hz, 1=xover_high_hz,
    //   2=low_thresh, 3=low_ratio, 4=mid_thresh, 5=mid_ratio,
    //   6=high_thresh, 7=high_ratio, 8=low_gain, 9=mid_gain, 10=high_gain, 11=output_db
    let mut effect = KernelAdapter::new(MultibandCompKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 250.0); // xover_low_hz
    effect.set_param(1, 3000.0); // xover_high_hz
    effect.set_param(2, -24.0); // low_thresh_db
    effect.set_param(3, 4.0); // low_ratio
    effect.set_param(4, -20.0); // mid_thresh_db
    effect.set_param(5, 4.0); // mid_ratio
    effect.set_param(6, -16.0); // high_thresh_db
    effect.set_param(7, 6.0); // high_ratio

    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("multiband_comp", effect, &input)
        .expect("MultibandComp regression test failed");
}

#[test]
fn test_multiband_comp_defaults_regression() {
    let effect = KernelAdapter::new(MultibandCompKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("multiband_comp_defaults", effect, &input)
        .expect("MultibandComp defaults regression test failed");
}

#[test]
fn test_plate_reverb_regression() {
    // PlateReverbKernel params: 0=decay_s, 1=damping_pct, 2=predelay_ms,
    //   3=bandwidth_pct, 4=diffusion_pct, 5=size_pct, 6=mix_pct, 7=output_db
    let mut effect = KernelAdapter::new(PlateReverbKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 2.0); // decay_s
    effect.set_param(1, 50.0); // damping_pct
    effect.set_param(2, 20.0); // predelay_ms
    effect.set_param(3, 80.0); // bandwidth_pct
    effect.set_param(4, 70.0); // diffusion_pct
    effect.set_param(5, 50.0); // size_pct
    effect.set_param(6, 30.0); // mix_pct

    let input = generate_impulse(TEST_DURATION_SAMPLES);
    run_regression_test("plate_reverb", effect, &input)
        .expect("PlateReverb regression test failed");
}

#[test]
fn test_plate_reverb_defaults_regression() {
    let effect = KernelAdapter::new(PlateReverbKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_impulse(TEST_DURATION_SAMPLES);
    run_regression_test("plate_reverb_defaults", effect, &input)
        .expect("PlateReverb defaults regression test failed");
}

#[test]
fn test_plate_reverb_stereo_regression() {
    let mut effect = KernelAdapter::new(PlateReverbKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 2.0); // decay_s
    effect.set_param(4, 70.0); // diffusion_pct
    effect.set_param(6, 30.0); // mix_pct
    let left = generate_impulse(TEST_DURATION_SAMPLES);
    let right = generate_impulse(TEST_DURATION_SAMPLES);
    run_regression_test_stereo("plate_reverb", effect, &left, &right)
        .expect("PlateReverb stereo regression test failed");
}

#[test]
fn test_spring_reverb_regression() {
    // SpringReverbKernel params: 0=decay_s, 1=tension_pct, 2=drip_pct, 3=damping_pct, 4=mix_pct, 5=output_db
    let mut effect = KernelAdapter::new(SpringReverbKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 2.0); // decay_s
    effect.set_param(1, 50.0); // tension_pct
    effect.set_param(2, 40.0); // drip_pct
    effect.set_param(3, 50.0); // damping_pct
    effect.set_param(4, 30.0); // mix_pct

    // Use test signal (not impulse): impulse has too sparse spectral content for
    // spectral_correlation to produce a reliable score with a 100ms window.
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("spring_reverb", effect, &input)
        .expect("SpringReverb regression test failed");
}

#[test]
fn test_spring_reverb_defaults_regression() {
    let effect = KernelAdapter::new(SpringReverbKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("spring_reverb_defaults", effect, &input)
        .expect("SpringReverb defaults regression test failed");
}

#[test]
fn test_transient_shaper_regression() {
    // TransientShaperKernel params: 0=attack_pct, 1=sustain_pct, 2=sensitivity_pct, 3=mix_pct, 4=output_db
    let mut effect = KernelAdapter::new(TransientShaperKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 50.0); // attack_pct: boost transients
    effect.set_param(1, -30.0); // sustain_pct: cut sustain
    effect.set_param(2, 60.0); // sensitivity_pct
    effect.set_param(3, 100.0); // mix_pct

    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("transient_shaper", effect, &input)
        .expect("TransientShaper regression test failed");
}

#[test]
fn test_transient_shaper_defaults_regression() {
    let effect = KernelAdapter::new(TransientShaperKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("transient_shaper_defaults", effect, &input)
        .expect("TransientShaper defaults regression test failed");
}

#[test]
fn test_deesser_regression() {
    // DeesserKernel params: 0=freq_hz, 1=thresh_db, 2=ratio, 3=range_db, 4=output_db
    let mut effect = KernelAdapter::new(DeesserKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 6000.0); // freq_hz
    effect.set_param(1, -20.0); // thresh_db
    effect.set_param(2, 8.0); // ratio
    effect.set_param(3, 12.0); // range_db

    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("deesser", effect, &input).expect("Deesser regression test failed");
}

#[test]
fn test_deesser_defaults_regression() {
    let effect = KernelAdapter::new(DeesserKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("deesser_defaults", effect, &input)
        .expect("Deesser defaults regression test failed");
}

#[test]
fn test_stereo_widener_regression() {
    // StereoWidenerKernel params: 0=width_pct, 1=haas_delay_ms, 2=bass_mono_hz, 3=output_db
    let mut effect = KernelAdapter::new(StereoWidenerKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 150.0); // width_pct: widen
    effect.set_param(1, 8.0); // haas_delay_ms
    effect.set_param(2, 120.0); // bass_mono_hz

    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("stereo_widener", effect, &input)
        .expect("StereoWidener regression test failed");
}

#[test]
fn test_stereo_widener_defaults_regression() {
    let effect = KernelAdapter::new(StereoWidenerKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("stereo_widener_defaults", effect, &input)
        .expect("StereoWidener defaults regression test failed");
}

#[test]
fn test_looper_defaults_regression() {
    // LooperKernel at default params (mode=Stop) should pass audio through.
    let effect = KernelAdapter::new(LooperKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("looper_defaults", effect, &input)
        .expect("Looper defaults regression test failed");
}

/// Run all regression tests and provide summary
#[test]
fn test_regression_summary() {
    // This test just verifies the framework works
    // Individual tests above handle the actual regression checking

    let golden_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(GOLDEN_DIR);

    if !golden_dir.exists() {
        fs::create_dir_all(&golden_dir).expect("Failed to create golden directory");
    }

    println!("\nRegression test framework initialized");
    println!("Golden files directory: {:?}", golden_dir);

    if should_regenerate() {
        println!("REGENERATE_GOLDEN is set - golden files will be updated");
    } else {
        let file_count = fs::read_dir(&golden_dir)
            .map(|rd| rd.filter_map(|e| e.ok()).count())
            .unwrap_or(0);
        println!("Found {} golden files", file_count);
    }
}
