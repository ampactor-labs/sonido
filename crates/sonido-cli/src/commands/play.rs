//! Audio file playback command with optional effect processing.

use super::common::{load_preset, parse_key_val};
use crate::effects::{create_effect_with_params, parse_chain};
use clap::Args;
use sonido_io::{AudioStream, GraphEngine, StreamConfig, read_wav_stereo};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Args)]
pub struct PlayArgs {
    /// WAV file to play
    #[arg(value_name = "FILE")]
    file: PathBuf,

    /// Single effect to apply during playback
    #[arg(short, long)]
    effect: Option<String>,

    /// Effect chain specification (e.g., "preamp:gain=6|distortion:drive=15")
    #[arg(short, long)]
    chain: Option<String>,

    /// Preset name or path
    #[arg(short, long)]
    preset: Option<String>,

    /// Effect parameters (e.g., "drive=15")
    #[arg(long, value_parser = parse_key_val, number_of_values = 1)]
    param: Vec<(String, String)>,

    /// Output device (index, exact name, or partial name)
    #[arg(short, long)]
    output: Option<String>,

    /// Loop playback
    #[arg(short, long, alias = "repeat")]
    r#loop: bool,

    /// Force mono output
    #[arg(long)]
    mono: bool,
}

pub fn run(args: PlayArgs) -> anyhow::Result<()> {
    // Load file
    println!("Loading {}...", args.file.display());
    let (samples, spec) = read_wav_stereo(&args.file)?;
    let sample_rate = spec.sample_rate as f32;
    let total_frames = samples.len();

    println!(
        "  {} frames, {} Hz, {:.1}s",
        total_frames,
        spec.sample_rate,
        total_frames as f32 / sample_rate
    );

    // Build optional effect chain
    let mut engine = GraphEngine::new_linear(sample_rate, 256);
    let has_effects;

    if let Some(preset_name) = &args.preset {
        let preset = load_preset(preset_name)?;
        println!("Loading preset: {}", preset.name);
        for effect_cfg in &preset.effects {
            if effect_cfg.bypassed {
                continue;
            }
            let effect = create_effect_with_params(
                &effect_cfg.effect_type,
                sample_rate,
                &effect_cfg.params,
            )?;
            engine.add_effect(effect);
        }
        has_effects = !engine.is_empty();
    } else if let Some(chain_spec) = &args.chain {
        let effects = parse_chain(chain_spec, sample_rate)?;
        for effect in effects {
            engine.add_effect(effect);
        }
        has_effects = !engine.is_empty();
    } else if let Some(effect_name) = &args.effect {
        let params: HashMap<String, String> = args.param.into_iter().collect();
        let effect = create_effect_with_params(effect_name, sample_rate, &params)?;
        engine.add_effect(effect);
        has_effects = true;
    } else {
        has_effects = false;
    }

    if has_effects {
        println!("Processing through {} effect(s)", engine.effect_count());
    }

    let looping = args.r#loop;

    println!(
        "\nPlaying{}... Press Ctrl+C to stop.\n",
        if looping { " (looping)" } else { "" }
    );

    // Set up Ctrl+C handler
    let running = Arc::new(AtomicBool::new(true));
    let r = Arc::clone(&running);
    ctrlc::set_handler(move || {
        println!("\nStopping...");
        r.store(false, Ordering::SeqCst);
    })?;

    // Playback position (in frames)
    let position = Arc::new(AtomicUsize::new(0));

    // Pre-copy sample data for the audio callback
    let left = Arc::new(samples.left);
    let right = Arc::new(samples.right);

    let stream_config = StreamConfig {
        sample_rate: spec.sample_rate,
        buffer_size: 256,
        input_device: None,
        output_device: args.output,
    };

    let mut stream = AudioStream::new(stream_config)?;
    let out_channels = stream.output_channels() as usize;
    // If mono requested, force 1-channel logic; otherwise use device channels
    let channels = if args.mono { 1 } else { out_channels };

    let cb_running = Arc::clone(&running);
    let cb_position = Arc::clone(&position);

    // Scratch buffers for block-based processing (allocated once, grown if needed).
    let mut left_buf = vec![0.0f32; 256];
    let mut right_buf = vec![0.0f32; 256];
    let mut left_out = vec![0.0f32; 256];
    let mut right_out = vec![0.0f32; 256];

    stream.run_output(move |data: &mut [f32]| {
        if !cb_running.load(Ordering::Relaxed) {
            data.fill(0.0);
            return;
        }

        let frames = data.len() / channels;
        let mut pos = cb_position.load(Ordering::Relaxed);

        // Grow scratch buffers if callback delivers more frames than expected.
        if frames > left_buf.len() {
            left_buf.resize(frames, 0.0);
            right_buf.resize(frames, 0.0);
            left_out.resize(frames, 0.0);
            right_out.resize(frames, 0.0);
        }

        // Gather source audio into contiguous L/R buffers, handling loop/end.
        let mut filled = 0;
        while filled < frames {
            if pos >= total_frames {
                if looping {
                    pos = 0;
                } else {
                    // Zero remaining output and stop.
                    let start = filled * channels;
                    data[start..].fill(0.0);
                    cb_running.store(false, Ordering::Relaxed);
                    break;
                }
            }
            let avail = (total_frames - pos).min(frames - filled);
            left_buf[filled..filled + avail].copy_from_slice(&left[pos..pos + avail]);
            right_buf[filled..filled + avail].copy_from_slice(&right[pos..pos + avail]);
            pos += avail;
            filled += avail;
        }

        // Process entire block through the effect chain.
        if has_effects && filled > 0 {
            engine.process_block_stereo(
                &left_buf[..filled],
                &right_buf[..filled],
                &mut left_out[..filled],
                &mut right_out[..filled],
            );
        } else {
            left_out[..filled].copy_from_slice(&left_buf[..filled]);
            right_out[..filled].copy_from_slice(&right_buf[..filled]);
        }

        // Interleave into output buffer.
        for i in 0..filled {
            let idx = i * channels;
            match channels {
                1 => data[idx] = (left_out[i] + right_out[i]) * 0.5,
                2 => {
                    data[idx] = left_out[i];
                    data[idx + 1] = right_out[i];
                }
                _ => {
                    data[idx] = left_out[i];
                    data[idx + 1] = right_out[i];
                    for c in 2..channels {
                        data[idx + c] = 0.0;
                    }
                }
            }
        }

        cb_position.store(pos, Ordering::Relaxed);
    })?;

    println!("Done!");
    Ok(())
}
