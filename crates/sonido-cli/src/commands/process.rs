//! File-based effect processing command.

use super::common::{load_preset, parse_key_val};
use crate::effects::{create_effect_with_params, parse_chain};
use crate::graph_dsl::{build_graph, build_graph_slug, parse_graph_dsl, validate_spec};
use clap::Args;
use indicatif::{ProgressBar, ProgressStyle};
use sonido_analysis::dynamics;
use sonido_core::linear_to_db;
use sonido_io::{GraphEngine, WavSpec, read_wav_stereo, write_wav, write_wav_stereo};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct ProcessArgs {
    /// Input WAV file
    #[arg(value_name = "INPUT")]
    input: PathBuf,

    /// Output WAV file (auto-generated if omitted)
    #[arg(value_name = "OUTPUT")]
    output: Option<PathBuf>,

    /// Single effect to apply
    #[arg(short, long)]
    effect: Option<String>,

    /// Effect chain specification (e.g., "preamp:gain=6|distortion:drive=15")
    #[arg(short, long, conflicts_with = "graph")]
    chain: Option<String>,

    /// Graph topology specification with split/merge (e.g., "split(distortion; -)|limiter")
    #[arg(short, long, conflicts_with_all = ["effect", "chain", "preset"])]
    graph: Option<String>,

    /// Preset name or path (supports factory presets, user presets, and file paths)
    #[arg(short, long)]
    preset: Option<String>,

    /// Effect parameters (e.g., "drive=15")
    #[arg(long, value_parser = parse_key_val, number_of_values = 1)]
    param: Vec<(String, String)>,

    /// Processing block size
    #[arg(long, default_value = "512")]
    block_size: usize,

    /// Output bit depth (16, 24, or 32)
    #[arg(long, default_value = "32")]
    bit_depth: u16,

    /// Force mono output (mix stereo to mono)
    #[arg(long)]
    mono: bool,
}

pub fn run(args: ProcessArgs) -> anyhow::Result<()> {
    // Read input file as stereo (mono files are duplicated to both channels)
    println!("Reading {}...", args.input.display());
    let (samples, spec) = read_wav_stereo(&args.input)?;
    let sample_rate = spec.sample_rate as f32;

    println!(
        "  {} samples, {} Hz, {} channel(s), {:.2}s",
        samples.len(),
        spec.sample_rate,
        spec.channels,
        samples.len() as f32 / sample_rate
    );

    // Resolve output path before params are consumed
    let output_path = match args.output {
        Some(path) => path,
        None => generate_output_path(
            &args.input,
            args.effect.as_deref(),
            args.chain.as_deref(),
            args.graph.as_deref(),
            args.preset.as_deref(),
            &args.param,
        ),
    };

    // Build effect chain or graph topology
    let block_size = args.block_size;
    let is_graph_mode = args.graph.is_some();

    let mut engine = if let Some(graph_spec) = &args.graph {
        // Graph topology mode: parse DSL → build ProcessingGraph
        let spec = parse_graph_dsl(graph_spec)?;
        validate_spec(&spec)?;
        let graph = build_graph(&spec, sample_rate, block_size)?;
        GraphEngine::new(graph)
    } else {
        let mut engine = GraphEngine::new_linear(sample_rate, block_size);

        if let Some(preset_name) = &args.preset {
            // Load preset by name or path using sonido-config
            let preset = load_preset(preset_name)?;

            println!("Loading preset: {}", preset.name);
            for effect_cfg in &preset.effects {
                if effect_cfg.bypassed {
                    continue; // Skip bypassed effects
                }
                let effect = create_effect_with_params(
                    &effect_cfg.effect_type,
                    sample_rate,
                    &effect_cfg.params,
                )?;
                engine.add_effect(effect);
            }
        } else if let Some(chain_spec) = &args.chain {
            // Parse chain specification
            let effects = parse_chain(chain_spec, sample_rate)?;
            for effect in effects {
                engine.add_effect(effect);
            }
        } else if let Some(effect_name) = &args.effect {
            // Single effect with optional parameters
            let params: HashMap<String, String> = args.param.into_iter().collect();
            let effect = create_effect_with_params(effect_name, sample_rate, &params)?;
            engine.add_effect(effect);
        } else {
            anyhow::bail!("No effect specified. Use --effect, --chain, --graph, or --preset");
        }

        if engine.is_empty() {
            anyhow::bail!("No effects to process");
        }

        engine
    };

    // Determine output mode
    let output_stereo = !args.mono;
    if is_graph_mode {
        // node_count includes Input + Output; subtract 2 for "processing" nodes
        let processing_nodes = engine.graph().node_count().saturating_sub(2);
        println!(
            "Processing graph topology ({} nodes, {} output)...",
            processing_nodes,
            if output_stereo { "stereo" } else { "mono" }
        );
    } else {
        println!(
            "Processing with {} effect(s) ({} output)...",
            engine.effect_count(),
            if output_stereo { "stereo" } else { "mono" }
        );
    }

    // Process with progress bar using stereo processing
    let pb = ProgressBar::new(samples.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})")
            .unwrap()
            .progress_chars("##-"),
    );

    let output = engine.process_file_stereo(&samples, block_size);

    // Update progress (process_file_stereo handles blocks internally)
    pb.set_position(samples.len() as u64);
    pb.finish_with_message("done");

    // Calculate stats (using left channel for simplicity, or mono mix)
    let input_mono = samples.to_mono();
    let output_mono = output.to_mono();

    let input_rms = dynamics::rms(&input_mono);
    let output_rms = dynamics::rms(&output_mono);
    let input_peak = dynamics::peak(&input_mono);
    let output_peak = dynamics::peak(&output_mono);

    println!("\nStats:");
    println!(
        "  Input:  RMS {:.1} dB, Peak {:.1} dB",
        linear_to_db(input_rms),
        linear_to_db(input_peak)
    );
    println!(
        "  Output: RMS {:.1} dB, Peak {:.1} dB",
        linear_to_db(output_rms),
        linear_to_db(output_peak)
    );

    // Write output file
    println!("\nWriting {}...", output_path.display());

    if output_stereo {
        let out_spec = WavSpec {
            channels: 2,
            sample_rate: spec.sample_rate,
            bits_per_sample: args.bit_depth,
        };
        write_wav_stereo(&output_path, &output, out_spec)?;
    } else {
        let out_spec = WavSpec {
            channels: 1,
            sample_rate: spec.sample_rate,
            bits_per_sample: args.bit_depth,
        };
        let mono_output = output.to_mono();
        write_wav(&output_path, &mono_output, out_spec)?;
    }

    println!("Done!");

    Ok(())
}

/// Generate an output file path from input path and effect specification.
///
/// Slug construction: single effect uses `effect_param=val`, chains use
/// `effect1+effect2`, graphs use `S[path1+path2]` notation, presets use
/// the preset name. Only user-specified params appear in the slug.
fn generate_output_path(
    input: &Path,
    effect: Option<&str>,
    chain: Option<&str>,
    graph: Option<&str>,
    preset: Option<&str>,
    params: &[(String, String)],
) -> PathBuf {
    let parent = input.parent().unwrap_or(Path::new("."));
    let stem = input.file_stem().unwrap_or_default().to_string_lossy();

    let slug = if let Some(preset_name) = preset {
        preset_name.to_string()
    } else if let Some(graph_spec) = graph {
        build_graph_slug(graph_spec)
    } else if let Some(chain_spec) = chain {
        build_chain_slug(chain_spec)
    } else if let Some(effect_name) = effect {
        let mut slug = effect_name.to_string();
        for (k, v) in params {
            slug.push('_');
            slug.push_str(k);
            slug.push('=');
            slug.push_str(v);
        }
        slug
    } else {
        "processed".to_string()
    };

    let filename = format!("{stem}_{slug}.wav");
    let filename = if filename.len() > 200 {
        format!("{}.wav", &filename[..196])
    } else {
        filename
    };

    parent.join(filename)
}

/// Build a slug from a chain specification string.
///
/// Converts chain format `effect1:param1=val1,param2=val2|effect2:param=val`
/// to slug format `effect1_param1=val1_param2=val2+effect2_param=val`.
fn build_chain_slug(chain_spec: &str) -> String {
    let mut parts = Vec::new();
    for effect_spec in chain_spec.split('|') {
        let effect_spec = effect_spec.trim();
        if effect_spec.is_empty() {
            continue;
        }
        let colon_parts: Vec<&str> = effect_spec.splitn(2, ':').collect();
        let name = colon_parts[0].trim();
        if colon_parts.len() > 1 {
            let params = colon_parts[1].replace(',', "_");
            parts.push(format!("{name}_{params}"));
        } else {
            parts.push(name.to_string());
        }
    }
    parts.join("+")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_path_preset_slug() {
        let path = generate_output_path(
            Path::new("input.wav"),
            None,
            None,
            None,
            Some("clean-boost"),
            &[],
        );
        assert_eq!(path, PathBuf::from("input_clean-boost.wav"));
    }

    #[test]
    fn output_path_effect_slug() {
        let path = generate_output_path(
            Path::new("input.wav"),
            Some("distortion"),
            None,
            None,
            None,
            &[("drive".into(), "15".into())],
        );
        assert_eq!(path, PathBuf::from("input_distortion_drive=15.wav"));
    }

    #[test]
    fn output_path_chain_slug() {
        let path = generate_output_path(
            Path::new("input.wav"),
            None,
            Some("preamp:gain=6|delay:time=300"),
            None,
            None,
            &[],
        );
        assert_eq!(
            path,
            PathBuf::from("input_preamp_gain=6+delay_time=300.wav")
        );
    }

    #[test]
    fn output_path_graph_slug() {
        let path = generate_output_path(
            Path::new("input.wav"),
            None,
            None,
            Some("split(delay:time=300; -) | reverb"),
            None,
            &[],
        );
        assert_eq!(path, PathBuf::from("input_S[delay_time=300+-]+reverb.wav"));
    }

    #[test]
    fn output_path_no_effect() {
        let path = generate_output_path(Path::new("input.wav"), None, None, None, None, &[]);
        assert_eq!(path, PathBuf::from("input_processed.wav"));
    }

    #[test]
    fn output_path_long_filename_clamped() {
        let long_name = "a".repeat(200);
        let chain = format!("effect:{long_name}=1");
        let path =
            generate_output_path(Path::new("input.wav"), None, Some(&chain), None, None, &[]);
        let filename = path.file_name().unwrap().to_string_lossy();
        assert!(
            filename.len() <= 200,
            "filename length {} exceeds 200",
            filename.len()
        );
        assert!(filename.ends_with(".wav"));
    }

    #[test]
    fn chain_slug_single_effect() {
        assert_eq!(build_chain_slug("distortion"), "distortion");
    }

    #[test]
    fn chain_slug_pipe_separated() {
        assert_eq!(
            build_chain_slug("preamp|distortion|delay"),
            "preamp+distortion+delay"
        );
    }

    #[test]
    fn chain_slug_with_params() {
        assert_eq!(
            build_chain_slug("distortion:drive=15,tone=4000"),
            "distortion_drive=15_tone=4000"
        );
    }
}
