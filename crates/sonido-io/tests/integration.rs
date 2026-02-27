//! Integration tests for sonido-io WAV I/O and graph engine.

use sonido_core::Effect;
use sonido_core::param_info::{ParamDescriptor, ParameterInfo};
use sonido_io::{
    GraphEngine, StereoSamples, WavSpec, read_wav, read_wav_info, read_wav_stereo, write_wav,
    write_wav_stereo,
};
use tempfile::NamedTempFile;

// ---------------------------------------------------------------------------
// WAV roundtrip tests -- mono
// ---------------------------------------------------------------------------

/// Generate a 1-second sine wave at the given sample rate.
fn sine_wave(sample_rate: u32, freq_hz: f32, num_samples: usize) -> Vec<f32> {
    (0..num_samples)
        .map(|i| (2.0 * std::f32::consts::PI * freq_hz * i as f32 / sample_rate as f32).sin())
        .collect()
}

#[test]
fn wav_roundtrip_mono_f32_44100() {
    let sr = 44100;
    let samples = sine_wave(sr, 440.0, sr as usize);
    let spec = WavSpec {
        channels: 1,
        sample_rate: sr,
        bits_per_sample: 32,
    };

    let file = NamedTempFile::new().unwrap();
    write_wav(file.path(), &samples, spec).unwrap();

    let (loaded, loaded_spec) = read_wav(file.path()).unwrap();
    assert_eq!(loaded_spec.sample_rate, sr);
    assert_eq!(loaded_spec.channels, 1);
    assert_eq!(loaded.len(), samples.len());

    for (a, b) in samples.iter().zip(loaded.iter()) {
        assert!(
            (a - b).abs() < 1e-6,
            "sample mismatch: {a} vs {b} (diff={})",
            (a - b).abs()
        );
    }
}

#[test]
fn wav_roundtrip_mono_f32_48000() {
    let sr = 48000;
    let samples = sine_wave(sr, 440.0, sr as usize);
    let spec = WavSpec {
        channels: 1,
        sample_rate: sr,
        bits_per_sample: 32,
    };

    let file = NamedTempFile::new().unwrap();
    write_wav(file.path(), &samples, spec).unwrap();

    let (loaded, loaded_spec) = read_wav(file.path()).unwrap();
    assert_eq!(loaded_spec.sample_rate, sr);
    assert_eq!(loaded.len(), samples.len());

    for (a, b) in samples.iter().zip(loaded.iter()) {
        assert!((a - b).abs() < 1e-6);
    }
}

#[test]
fn wav_roundtrip_mono_f32_96000() {
    let sr = 96000;
    let samples = sine_wave(sr, 1000.0, sr as usize);
    let spec = WavSpec {
        channels: 1,
        sample_rate: sr,
        bits_per_sample: 32,
    };

    let file = NamedTempFile::new().unwrap();
    write_wav(file.path(), &samples, spec).unwrap();

    let (loaded, loaded_spec) = read_wav(file.path()).unwrap();
    assert_eq!(loaded_spec.sample_rate, sr);
    assert_eq!(loaded.len(), samples.len());

    for (a, b) in samples.iter().zip(loaded.iter()) {
        assert!((a - b).abs() < 1e-6);
    }
}

// ---------------------------------------------------------------------------
// WAV roundtrip tests -- stereo
// ---------------------------------------------------------------------------

#[test]
fn wav_roundtrip_stereo_f32_44100() {
    let sr = 44100;
    let left = sine_wave(sr, 440.0, sr as usize);
    let right = sine_wave(sr, 880.0, sr as usize);
    let samples = StereoSamples::new(left.clone(), right.clone());

    let spec = WavSpec {
        channels: 2,
        sample_rate: sr,
        bits_per_sample: 32,
    };

    let file = NamedTempFile::new().unwrap();
    write_wav_stereo(file.path(), &samples, spec).unwrap();

    let (loaded, loaded_spec) = read_wav_stereo(file.path()).unwrap();
    assert_eq!(loaded_spec.sample_rate, sr);
    assert_eq!(loaded.len(), samples.len());

    for (a, b) in left.iter().zip(loaded.left.iter()) {
        assert!((a - b).abs() < 1e-6);
    }
    for (a, b) in right.iter().zip(loaded.right.iter()) {
        assert!((a - b).abs() < 1e-6);
    }
}

#[test]
fn wav_roundtrip_stereo_f32_48000() {
    let sr = 48000;
    let left = sine_wave(sr, 440.0, sr as usize);
    let right = sine_wave(sr, 880.0, sr as usize);
    let samples = StereoSamples::new(left.clone(), right.clone());

    let spec = WavSpec {
        channels: 2,
        sample_rate: sr,
        bits_per_sample: 32,
    };

    let file = NamedTempFile::new().unwrap();
    write_wav_stereo(file.path(), &samples, spec).unwrap();

    let (loaded, _) = read_wav_stereo(file.path()).unwrap();
    assert_eq!(loaded.len(), samples.len());

    for (a, b) in left.iter().zip(loaded.left.iter()) {
        assert!((a - b).abs() < 1e-6);
    }
    for (a, b) in right.iter().zip(loaded.right.iter()) {
        assert!((a - b).abs() < 1e-6);
    }
}

#[test]
fn wav_roundtrip_stereo_f32_96000() {
    let sr = 96000;
    let left = sine_wave(sr, 440.0, sr as usize);
    let right = sine_wave(sr, 880.0, sr as usize);
    let samples = StereoSamples::new(left.clone(), right.clone());

    let spec = WavSpec {
        channels: 2,
        sample_rate: sr,
        bits_per_sample: 32,
    };

    let file = NamedTempFile::new().unwrap();
    write_wav_stereo(file.path(), &samples, spec).unwrap();

    let (loaded, loaded_spec) = read_wav_stereo(file.path()).unwrap();
    assert_eq!(loaded_spec.sample_rate, sr);
    assert_eq!(loaded.len(), samples.len());

    for (a, b) in left.iter().zip(loaded.left.iter()) {
        assert!((a - b).abs() < 1e-6);
    }
}

// ---------------------------------------------------------------------------
// WAV edge cases
// ---------------------------------------------------------------------------

#[test]
fn wav_write_empty_buffer() {
    let spec = WavSpec {
        channels: 1,
        sample_rate: 48000,
        bits_per_sample: 32,
    };

    let file = NamedTempFile::new().unwrap();
    write_wav(file.path(), &[], spec).unwrap();

    let (loaded, loaded_spec) = read_wav(file.path()).unwrap();
    assert_eq!(loaded_spec.sample_rate, 48000);
    assert!(loaded.is_empty());
}

#[test]
fn wav_write_single_sample() {
    let spec = WavSpec {
        channels: 1,
        sample_rate: 48000,
        bits_per_sample: 32,
    };

    let file = NamedTempFile::new().unwrap();
    write_wav(file.path(), &[0.42], spec).unwrap();

    let (loaded, _) = read_wav(file.path()).unwrap();
    assert_eq!(loaded.len(), 1);
    assert!((loaded[0] - 0.42).abs() < 1e-6);
}

#[test]
fn wav_stereo_write_empty_buffer() {
    let spec = WavSpec {
        channels: 2,
        sample_rate: 48000,
        bits_per_sample: 32,
    };
    let samples = StereoSamples::new(vec![], vec![]);

    let file = NamedTempFile::new().unwrap();
    write_wav_stereo(file.path(), &samples, spec).unwrap();

    let (loaded, _) = read_wav_stereo(file.path()).unwrap();
    assert!(loaded.is_empty());
}

#[test]
fn wav_stereo_write_single_sample() {
    let spec = WavSpec {
        channels: 2,
        sample_rate: 48000,
        bits_per_sample: 32,
    };
    let samples = StereoSamples::new(vec![0.25], vec![0.75]);

    let file = NamedTempFile::new().unwrap();
    write_wav_stereo(file.path(), &samples, spec).unwrap();

    let (loaded, _) = read_wav_stereo(file.path()).unwrap();
    assert_eq!(loaded.len(), 1);
    assert!((loaded.left[0] - 0.25).abs() < 1e-6);
    assert!((loaded.right[0] - 0.75).abs() < 1e-6);
}

// ---------------------------------------------------------------------------
// WAV i16 format tests
// ---------------------------------------------------------------------------

#[test]
fn wav_roundtrip_mono_i16() {
    let sr = 44100;
    // Use a signal well within [-1, 1] to stay safe with i16 quantization
    let samples = sine_wave(sr, 440.0, sr as usize)
        .into_iter()
        .map(|s| s * 0.9)
        .collect::<Vec<_>>();

    let spec = WavSpec {
        channels: 1,
        sample_rate: sr,
        bits_per_sample: 16,
    };

    let file = NamedTempFile::new().unwrap();
    write_wav(file.path(), &samples, spec).unwrap();

    let (loaded, loaded_spec) = read_wav(file.path()).unwrap();
    assert_eq!(loaded_spec.sample_rate, sr);
    assert_eq!(loaded_spec.bits_per_sample, 16);
    assert_eq!(loaded.len(), samples.len());

    // 16-bit has quantization error of about 1/32768 ~= 3e-5
    for (a, b) in samples.iter().zip(loaded.iter()) {
        assert!((a - b).abs() < 0.001, "i16 roundtrip mismatch: {a} vs {b}");
    }
}

#[test]
fn wav_roundtrip_stereo_i16() {
    let sr = 48000;
    let left: Vec<f32> = sine_wave(sr, 440.0, 1000)
        .into_iter()
        .map(|s| s * 0.9)
        .collect();
    let right: Vec<f32> = sine_wave(sr, 880.0, 1000)
        .into_iter()
        .map(|s| s * 0.9)
        .collect();
    let samples = StereoSamples::new(left.clone(), right.clone());

    let spec = WavSpec {
        channels: 2,
        sample_rate: sr,
        bits_per_sample: 16,
    };

    let file = NamedTempFile::new().unwrap();
    write_wav_stereo(file.path(), &samples, spec).unwrap();

    let (loaded, loaded_spec) = read_wav_stereo(file.path()).unwrap();
    assert_eq!(loaded_spec.bits_per_sample, 16);
    assert_eq!(loaded.len(), 1000);

    for (a, b) in left.iter().zip(loaded.left.iter()) {
        assert!((a - b).abs() < 0.001);
    }
    for (a, b) in right.iter().zip(loaded.right.iter()) {
        assert!((a - b).abs() < 0.001);
    }
}

#[test]
fn wav_roundtrip_mono_i24() {
    let sr = 48000;
    let samples = sine_wave(sr, 440.0, 1000)
        .into_iter()
        .map(|s| s * 0.9)
        .collect::<Vec<_>>();

    let spec = WavSpec {
        channels: 1,
        sample_rate: sr,
        bits_per_sample: 24,
    };

    let file = NamedTempFile::new().unwrap();
    write_wav(file.path(), &samples, spec).unwrap();

    let (loaded, loaded_spec) = read_wav(file.path()).unwrap();
    assert_eq!(loaded_spec.bits_per_sample, 24);
    assert_eq!(loaded.len(), samples.len());

    // 24-bit has quantization error of about 1/8388608 ~= 1.2e-7
    for (a, b) in samples.iter().zip(loaded.iter()) {
        assert!((a - b).abs() < 0.0001, "i24 roundtrip mismatch: {a} vs {b}");
    }
}

// ---------------------------------------------------------------------------
// read_wav_info tests
// ---------------------------------------------------------------------------

#[test]
fn wav_info_mono_f32() {
    let sr = 48000;
    let num_samples = 2400; // 50ms
    let samples = sine_wave(sr, 440.0, num_samples);

    let spec = WavSpec {
        channels: 1,
        sample_rate: sr,
        bits_per_sample: 32,
    };

    let file = NamedTempFile::new().unwrap();
    write_wav(file.path(), &samples, spec).unwrap();

    let info = read_wav_info(file.path()).unwrap();
    assert_eq!(info.channels, 1);
    assert_eq!(info.sample_rate, sr);
    assert_eq!(info.bits_per_sample, 32);
    assert_eq!(info.num_frames, num_samples as u64);
    assert!((info.duration_secs - 0.05).abs() < 1e-6);
}

#[test]
fn wav_info_stereo_i16() {
    let sr = 44100;
    let num_samples = 44100; // 1 second
    let left = sine_wave(sr, 440.0, num_samples);
    let right = sine_wave(sr, 880.0, num_samples);
    let samples = StereoSamples::new(left, right);

    let spec = WavSpec {
        channels: 2,
        sample_rate: sr,
        bits_per_sample: 16,
    };

    let file = NamedTempFile::new().unwrap();
    write_wav_stereo(file.path(), &samples, spec).unwrap();

    let info = read_wav_info(file.path()).unwrap();
    assert_eq!(info.channels, 2);
    assert_eq!(info.sample_rate, sr);
    assert_eq!(info.bits_per_sample, 16);
    assert_eq!(info.num_frames, num_samples as u64);
    assert!((info.duration_secs - 1.0).abs() < 1e-6);
}

// ---------------------------------------------------------------------------
// StereoSamples utility tests
// ---------------------------------------------------------------------------

#[test]
fn stereo_from_mono_duplicates() {
    let mono = vec![0.1, 0.2, 0.3];
    let stereo = StereoSamples::from_mono(mono.clone());
    assert_eq!(stereo.left, mono);
    assert_eq!(stereo.right, mono);
    assert_eq!(stereo.len(), 3);
    assert!(!stereo.is_empty());
}

#[test]
fn stereo_to_mono_averages() {
    let stereo = StereoSamples::new(vec![1.0, 0.0], vec![0.0, 1.0]);
    let mono = stereo.to_mono();
    assert!((mono[0] - 0.5).abs() < 1e-6);
    assert!((mono[1] - 0.5).abs() < 1e-6);
}

#[test]
fn stereo_interleaved_roundtrip() {
    let left = vec![0.1, 0.3, 0.5];
    let right = vec![0.2, 0.4, 0.6];
    let stereo = StereoSamples::new(left.clone(), right.clone());

    let interleaved = stereo.to_interleaved();
    assert_eq!(interleaved, vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6]);

    let back = StereoSamples::from_interleaved(&interleaved);
    assert_eq!(back.left, left);
    assert_eq!(back.right, right);
}

// ---------------------------------------------------------------------------
// Read mono file as stereo (should duplicate channels)
// ---------------------------------------------------------------------------

#[test]
fn read_mono_as_stereo_duplicates_channels() {
    let sr = 48000;
    let samples = sine_wave(sr, 440.0, 1000);
    let spec = WavSpec {
        channels: 1,
        sample_rate: sr,
        bits_per_sample: 32,
    };

    let file = NamedTempFile::new().unwrap();
    write_wav(file.path(), &samples, spec).unwrap();

    let (stereo, _) = read_wav_stereo(file.path()).unwrap();
    assert_eq!(stereo.len(), 1000);

    for (orig, left) in samples.iter().zip(stereo.left.iter()) {
        assert!((orig - left).abs() < 1e-6);
    }
    for (orig, right) in samples.iter().zip(stereo.right.iter()) {
        assert!((orig - right).abs() < 1e-6);
    }
}

// ---------------------------------------------------------------------------
// GraphEngine tests
// ---------------------------------------------------------------------------

/// Simple test effect that multiplies by a constant factor.
struct Gain {
    factor: f32,
}

impl Effect for Gain {
    fn process(&mut self, input: f32) -> f32 {
        input * self.factor
    }

    fn set_sample_rate(&mut self, _sample_rate: f32) {}
    fn reset(&mut self) {}
    fn latency_samples(&self) -> usize {
        0
    }
}

impl ParameterInfo for Gain {
    fn param_count(&self) -> usize {
        0
    }
    fn param_info(&self, _index: usize) -> Option<ParamDescriptor> {
        None
    }
    fn get_param(&self, _index: usize) -> f32 {
        0.0
    }
    fn set_param(&mut self, _index: usize, _value: f32) {}
}

fn gain(factor: f32) -> Box<dyn sonido_core::EffectWithParams + Send> {
    Box::new(Gain { factor })
}

#[test]
fn engine_process_mono_block() {
    let mut engine = GraphEngine::from_chain(vec![gain(0.5)], 48000.0, 512).unwrap();

    let input = sine_wave(48000, 440.0, 512);
    let mut output = vec![0.0; 512];
    engine.process_block(&input, &mut output);

    for (i, o) in input.iter().zip(output.iter()) {
        assert!((o - i * 0.5).abs() < 1e-6);
    }
}

#[test]
fn engine_process_stereo_block() {
    let mut engine = GraphEngine::from_chain(vec![gain(0.5)], 48000.0, 256).unwrap();

    let left_in = sine_wave(48000, 440.0, 256);
    let right_in = sine_wave(48000, 880.0, 256);
    let mut left_out = vec![0.0; 256];
    let mut right_out = vec![0.0; 256];

    engine.process_block_stereo(&left_in, &right_in, &mut left_out, &mut right_out);

    for (i, o) in left_in.iter().zip(left_out.iter()) {
        assert!((o - i * 0.5).abs() < 1e-6);
    }
    for (i, o) in right_in.iter().zip(right_out.iter()) {
        assert!((o - i * 0.5).abs() < 1e-6);
    }
}

#[test]
fn engine_chain_two_effects() {
    let mut engine = GraphEngine::from_chain(vec![gain(2.0), gain(3.0)], 48000.0, 4).unwrap();

    // 0.1 * 2.0 * 3.0 = 0.6
    let input = [0.1; 4];
    let mut output = [0.0; 4];
    engine.process_block(&input, &mut output);
    for &s in &output {
        assert!((s - 0.6).abs() < 1e-6);
    }
    assert_eq!(engine.effect_count(), 2);
}

#[test]
fn engine_empty_passes_through() {
    let mut engine = GraphEngine::new_linear(48000.0, 4);
    assert!(engine.is_empty());

    // Stereo pass-through
    let left_in = [0.1, 0.2, 0.3, 0.4];
    let right_in = [0.9, 0.8, 0.7, 0.6];
    let mut left_out = [0.0; 4];
    let mut right_out = [0.0; 4];

    engine.process_block_stereo(&left_in, &right_in, &mut left_out, &mut right_out);
    assert_eq!(left_out, left_in);
    assert_eq!(right_out, right_in);
}

#[test]
fn engine_process_file_mono() {
    let mut engine = GraphEngine::from_chain(vec![gain(0.5)], 48000.0, 256).unwrap();

    let input = sine_wave(48000, 440.0, 2048);
    let output = engine.process_file(&input, 256);

    assert_eq!(output.len(), input.len());
    for (i, o) in input.iter().zip(output.iter()) {
        assert!((o - i * 0.5).abs() < 1e-6);
    }
}

#[test]
fn engine_process_file_stereo() {
    let mut engine = GraphEngine::from_chain(vec![gain(0.5)], 48000.0, 256).unwrap();

    let left = sine_wave(48000, 440.0, 2048);
    let right = sine_wave(48000, 880.0, 2048);
    let input = StereoSamples::new(left.clone(), right.clone());

    let output = engine.process_file_stereo(&input, 256);

    assert_eq!(output.len(), input.len());
    for (i, o) in left.iter().zip(output.left.iter()) {
        assert!((o - i * 0.5).abs() < 1e-6);
    }
    for (i, o) in right.iter().zip(output.right.iter()) {
        assert!((o - i * 0.5).abs() < 1e-6);
    }
}

#[test]
fn engine_set_sample_rate() {
    let engine = GraphEngine::new_linear(44100.0, 256);
    assert!((engine.sample_rate() - 44100.0).abs() < 1e-6);
}

#[test]
fn engine_latency_accumulates() {
    let engine = GraphEngine::from_chain(vec![gain(1.0), gain(1.0)], 48000.0, 256).unwrap();
    // Gain has 0 latency
    assert_eq!(engine.latency_samples(), 0);
}

#[test]
fn engine_process_block_inplace() {
    let mut engine = GraphEngine::from_chain(vec![gain(2.0)], 48000.0, 4).unwrap();

    let mut buffer = [0.1, 0.2, 0.3, 0.4];
    engine.process_block_inplace(&mut buffer);

    assert!((buffer[0] - 0.2).abs() < 1e-6);
    assert!((buffer[1] - 0.4).abs() < 1e-6);
    assert!((buffer[2] - 0.6).abs() < 1e-6);
    assert!((buffer[3] - 0.8).abs() < 1e-6);
}

// ---------------------------------------------------------------------------
// Engine with real effects (from sonido-effects)
// ---------------------------------------------------------------------------

#[test]
fn engine_with_real_distortion_effect() {
    use sonido_effects::Distortion;

    let mut dist = Distortion::new(48000.0);
    dist.set_drive_db(20.0);
    let mut engine = GraphEngine::from_chain(vec![Box::new(dist)], 48000.0, 256).unwrap();

    let input = sine_wave(48000, 440.0, 1024);
    let output = engine.process_file(&input, 256);

    // Distortion should change the signal
    let mut any_different = false;
    for (i, o) in input.iter().zip(output.iter()) {
        if (i - o).abs() > 1e-4 {
            any_different = true;
            break;
        }
    }
    assert!(
        any_different,
        "distortion effect should alter the signal when drive is high"
    );

    // Output should still be bounded (soft limiting)
    for &s in &output {
        assert!(s.abs() <= 2.0, "distortion output out of bounds: {s}");
    }
}

#[test]
fn engine_with_real_reverb_stereo() {
    use sonido_effects::Reverb;

    let mut reverb = Reverb::new(48000.0);
    reverb.set_mix(0.5);
    reverb.set_decay(0.8);
    let mut engine = GraphEngine::from_chain(vec![Box::new(reverb)], 48000.0, 256).unwrap();

    let left = sine_wave(48000, 440.0, 4096);
    let right = sine_wave(48000, 880.0, 4096);
    let input = StereoSamples::new(left.clone(), right.clone());

    let output = engine.process_file_stereo(&input, 256);
    assert_eq!(output.len(), input.len());

    // Reverb is a true stereo effect -- L and R should be different from input
    let mut left_differs = false;
    let mut right_differs = false;

    for (orig, processed) in left.iter().zip(output.left.iter()) {
        if (orig - processed).abs() > 1e-4 {
            left_differs = true;
            break;
        }
    }
    for (orig, processed) in right.iter().zip(output.right.iter()) {
        if (orig - processed).abs() > 1e-4 {
            right_differs = true;
            break;
        }
    }

    assert!(left_differs, "reverb should alter left channel");
    assert!(right_differs, "reverb should alter right channel");
}

// ---------------------------------------------------------------------------
// End-to-end: write WAV, process through engine, write output, verify
// ---------------------------------------------------------------------------

#[test]
fn end_to_end_wav_process_wav() {
    use sonido_effects::Compressor;

    // Write input WAV
    let sr = 48000;
    let input_samples = sine_wave(sr, 440.0, sr as usize);
    let in_spec = WavSpec {
        channels: 1,
        sample_rate: sr,
        bits_per_sample: 32,
    };
    let in_file = NamedTempFile::new().unwrap();
    write_wav(in_file.path(), &input_samples, in_spec).unwrap();

    // Read input
    let (loaded, spec) = read_wav(in_file.path()).unwrap();
    assert_eq!(loaded.len(), sr as usize);

    // Process through engine
    let mut engine = GraphEngine::from_chain(
        vec![Box::new(Compressor::new(spec.sample_rate as f32))],
        spec.sample_rate as f32,
        512,
    )
    .unwrap();
    let processed = engine.process_file(&loaded, 512);
    assert_eq!(processed.len(), loaded.len());

    // Write output WAV
    let out_file = NamedTempFile::new().unwrap();
    write_wav(out_file.path(), &processed, spec).unwrap();

    // Verify output is a valid WAV
    let (reloaded, reloaded_spec) = read_wav(out_file.path()).unwrap();
    assert_eq!(reloaded_spec.sample_rate, sr);
    assert_eq!(reloaded.len(), sr as usize);

    // Verify output matches processed data
    for (a, b) in processed.iter().zip(reloaded.iter()) {
        assert!((a - b).abs() < 1e-6);
    }
}
