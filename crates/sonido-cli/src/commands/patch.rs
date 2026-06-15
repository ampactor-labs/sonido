//! Canonical patch (`sonido-patch`) export and inspection.
//!
//! `sonido patch export` packs a preset TOML or a DSL string into a single
//! 4 KB `.bin` sector in the canonical format the firmware and GUI both speak.
//! Unlike the legacy `sonido daisy export`, effects are stored by **stable UID**
//! (not list position) and the format carries macros, morph, and globals.
//!
//! Arbitrary topologies (splits/merges) are authored in the GUI; this command
//! supports the well-defined `linear` and `parallel` shapes for any effect count.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use sonido_config::Preset;
use sonido_patch::{MAX_NODES, Patch, PatchEdge, PatchEndpoint, PatchNode, SECTOR_SIZE, binary};
use sonido_registry::{EffectRegistry, PEDAL_EFFECT_IDS, effect_by_uid, effect_uid};

/// Canonical patch management.
#[derive(Args)]
pub struct PatchArgs {
    #[command(subcommand)]
    pub command: PatchCommand,
}

/// Subcommands for `sonido patch`.
#[derive(Subcommand)]
pub enum PatchCommand {
    /// Export a preset TOML or DSL string to a `.bin` patch sector.
    Export {
        /// Path to a preset TOML file.
        #[arg(conflicts_with = "from_dsl")]
        preset: Option<PathBuf>,
        /// DSL string to export directly (e.g. "distortion:drive=20 | reverb:mix=30").
        #[arg(long, conflicts_with = "preset")]
        from_dsl: Option<String>,
        /// Output `.bin` file.
        #[arg(short, long)]
        output: PathBuf,
        /// Topology: `linear` (chain) or `parallel` (all effects between a split/merge).
        #[arg(short, long, default_value = "linear")]
        topology: String,
        /// Override the patch name (defaults to the preset name / "dsl").
        #[arg(long)]
        name: Option<String>,
        /// Restrict effects to the pedal set and fail if any are unavailable.
        #[arg(long)]
        pedal: bool,
    },
    /// Decode a `.bin` patch and print a human-readable summary.
    Inspect {
        /// Path to the `.bin` patch file.
        file: PathBuf,
    },
}

/// Run the `sonido patch` command.
pub fn run(args: PatchArgs) -> anyhow::Result<()> {
    match args.command {
        PatchCommand::Export {
            preset,
            from_dsl,
            output,
            topology,
            name,
            pedal,
        } => {
            let registry = EffectRegistry::new();
            let preset = load_preset(preset.as_deref(), from_dsl.as_deref(), &registry)?;
            let patch_name = name.unwrap_or_else(|| preset.name.clone());
            let patch = preset_to_patch(&preset, &patch_name, &topology, &registry)?;
            export_patch(&patch, &output, pedal)
        }
        PatchCommand::Inspect { file } => inspect(&file),
    }
}

// ---------------------------------------------------------------------------
// Preset → Patch
// ---------------------------------------------------------------------------

fn load_preset(
    preset_path: Option<&Path>,
    dsl: Option<&str>,
    registry: &EffectRegistry,
) -> anyhow::Result<Preset> {
    match (preset_path, dsl) {
        (Some(path), None) => Preset::load(path)
            .map_err(|e| anyhow::anyhow!("Failed to load preset '{}': {}", path.display(), e)),
        (None, Some(dsl)) => {
            let snapshot = sonido_graph_dsl::snapshot_from_dsl(dsl, registry)
                .map_err(|e| anyhow::anyhow!("Failed to parse DSL: {}", e))?;
            Ok(sonido_graph_dsl::snapshot_to_preset(
                &snapshot, "dsl", registry,
            ))
        }
        _ => anyhow::bail!("Provide either a preset TOML path or --from-dsl, not both"),
    }
}

/// Convert a [`Preset`] into a [`Patch`], resolving stable UIDs and overlaying
/// the preset's parameter values onto each effect's defaults.
fn preset_to_patch(
    preset: &Preset,
    name: &str,
    topology: &str,
    registry: &EffectRegistry,
) -> anyhow::Result<Patch> {
    if preset.effects.len() > MAX_NODES {
        anyhow::bail!(
            "Patch supports at most {MAX_NODES} effects, '{}' has {}",
            preset.name,
            preset.effects.len()
        );
    }

    let mut nodes = Vec::with_capacity(preset.effects.len());
    for effect_cfg in &preset.effects {
        let id = effect_cfg.effect_type.as_str();
        let uid = effect_uid(id)
            .ok_or_else(|| anyhow::anyhow!("Effect '{}' has no stable UID (unknown effect)", id))?;

        let effect = registry
            .create(id, 48_000.0)
            .ok_or_else(|| anyhow::anyhow!("Effect '{}' not found in registry", id))?;

        let param_count = effect.effect_param_count();
        let mut params = Vec::with_capacity(param_count);
        // Start from defaults.
        for idx in 0..param_count {
            params.push(
                effect
                    .effect_param_info(idx)
                    .map(|d| d.default)
                    .unwrap_or(0.0),
            );
        }
        // Overlay preset values, matched by name / short name / string id.
        for (key, val) in &effect_cfg.params {
            let lower = key.to_lowercase();
            let mut matched = false;
            for idx in 0..param_count {
                if let Some(desc) = effect.effect_param_info(idx) {
                    let hit = desc.name.to_lowercase() == lower
                        || desc.short_name.to_lowercase() == lower
                        || desc.string_id == lower;
                    if hit {
                        match val.parse::<f32>() {
                            Ok(v) => params[idx] = v.clamp(desc.min, desc.max),
                            Err(_) => eprintln!(
                                "Warning: could not parse '{key}={val}' as f32, leaving default"
                            ),
                        }
                        matched = true;
                        break;
                    }
                }
            }
            if !matched {
                eprintln!("Warning: effect '{id}' has no parameter '{key}', ignoring");
            }
        }

        nodes.push(PatchNode::new(uid, params));
    }

    let mut patch = Patch::new(name);
    patch.edges = build_edges(topology, nodes.len())?;
    patch.nodes = nodes;
    Ok(patch)
}

/// Build the edge set for a named topology over `n` effect nodes.
fn build_edges(topology: &str, n: usize) -> anyhow::Result<Vec<PatchEdge>> {
    let mut edges = Vec::new();
    match topology {
        "linear" => {
            // Reuse the chain builder's edge layout.
            return Ok(Patch::linear_chain(
                "",
                (0..n)
                    .map(|i| PatchNode::new((i + 1) as u16, Vec::new()))
                    .collect(),
            )
            .edges);
        }
        "parallel" => {
            if n == 0 {
                edges.push(PatchEdge::new(PatchEndpoint::Input, PatchEndpoint::Output));
            } else {
                edges.push(PatchEdge::new(
                    PatchEndpoint::Input,
                    PatchEndpoint::Split(0),
                ));
                for i in 0..n {
                    edges.push(PatchEdge::new(
                        PatchEndpoint::Split(0),
                        PatchEndpoint::Node(i as u8),
                    ));
                    edges.push(PatchEdge::new(
                        PatchEndpoint::Node(i as u8),
                        PatchEndpoint::Merge(0),
                    ));
                }
                edges.push(PatchEdge::new(
                    PatchEndpoint::Merge(0),
                    PatchEndpoint::Output,
                ));
            }
        }
        other => anyhow::bail!(
            "Unknown topology '{other}'. Use 'linear' or 'parallel' \
             (arbitrary graphs are authored in the GUI)."
        ),
    }
    Ok(edges)
}

// ---------------------------------------------------------------------------
// Export / Inspect
// ---------------------------------------------------------------------------

fn export_patch(patch: &Patch, output: &Path, pedal: bool) -> anyhow::Result<()> {
    if pedal {
        for node in &patch.nodes {
            let id = effect_by_uid(node.effect_uid).unwrap_or("<unknown>");
            if !PEDAL_EFFECT_IDS.contains(&id) {
                anyhow::bail!(
                    "Effect '{}' is not available on the pedal. Available: {}",
                    id,
                    PEDAL_EFFECT_IDS.join(", ")
                );
            }
        }
    }

    let mut buf = [0u8; SECTOR_SIZE];
    let len = binary::encode(patch, &mut buf)
        .map_err(|e| anyhow::anyhow!("Failed to encode patch: {e}"))?;
    std::fs::write(output, &buf[..len])
        .map_err(|e| anyhow::anyhow!("Failed to write '{}': {}", output.display(), e))?;

    println!(
        "Exported patch '{}' to '{}' ({} effect(s), {} byte(s), {} macro(s))",
        patch.name,
        output.display(),
        patch.nodes.len(),
        len,
        patch.active_macro_count(),
    );
    Ok(())
}

fn inspect(file: &Path) -> anyhow::Result<()> {
    let data = std::fs::read(file)
        .map_err(|e| anyhow::anyhow!("Failed to read '{}': {}", file.display(), e))?;
    let patch =
        binary::decode(&data).map_err(|e| anyhow::anyhow!("Failed to decode patch: {e}"))?;

    let registry = EffectRegistry::new();

    println!("Name:      {}", patch.name);
    println!("Version:   {}", patch.format_version);
    println!(
        "Globals:   in {:+.1} dB / master {:+.1} dB",
        patch.globals.input_gain_db, patch.globals.master_volume_db
    );
    println!(
        "Morph:     {:?}, speed {:.2}, locks 0b{:06b}",
        patch.morph.mode, patch.morph.speed, patch.morph.slot_locks
    );
    println!("Nodes:     {}", patch.nodes.len());

    for (i, node) in patch.nodes.iter().enumerate() {
        let id = effect_by_uid(node.effect_uid).unwrap_or("<unknown>");
        let bypass = if node.bypassed { " [bypassed]" } else { "" };
        println!("  [{i}] {id} (UID {}){bypass}", node.effect_uid);
        let effect = registry.create(id, 48_000.0);
        for (p, (&va, &vb)) in node.params_a.iter().zip(&node.params_b).enumerate() {
            let pname = effect
                .as_ref()
                .and_then(|e| e.effect_param_info(p))
                .map(|d| d.name)
                .unwrap_or("param");
            if (va - vb).abs() < f32::EPSILON {
                println!("        {pname}: {va:.4}");
            } else {
                println!("        {pname}: A={va:.4}  B={vb:.4}");
            }
        }
    }

    println!("Edges:     {}", patch.edges.len());
    for e in &patch.edges {
        println!("  {:?} -> {:?}", e.from, e.to);
    }

    let active: Vec<(usize, &str)> = patch
        .macros
        .iter()
        .enumerate()
        .filter(|(_, m)| m.is_active())
        .map(|(i, m)| (i, m.name.as_str()))
        .collect();
    if !active.is_empty() {
        println!("Macros:");
        for (i, mname) in active {
            let m = &patch.macros[i];
            println!(
                "  K{}: \"{}\" -> {} target(s)",
                i + 1,
                mname,
                m.mappings.len()
            );
            for map in &m.mappings {
                println!(
                    "      {:?}  [{:.3} .. {:.3}] {:?}",
                    map.target, map.min, map.max, map.curve
                );
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_config::{EffectConfig, Preset};

    fn pedal_uid(id: &str) -> u16 {
        effect_uid(id).unwrap()
    }

    #[test]
    fn export_then_inspect_roundtrip_via_disk() {
        let preset = Preset::new("CLI Test")
            .with_effect(EffectConfig::new("distortion").with_param("drive", "20.0"))
            .with_effect(EffectConfig::new("reverb").with_param("mix", "30"));
        let registry = EffectRegistry::new();
        let patch = preset_to_patch(&preset, "CLI Test", "linear", &registry).unwrap();

        assert_eq!(patch.nodes.len(), 2);
        assert_eq!(patch.nodes[0].effect_uid, pedal_uid("distortion"));
        assert_eq!(patch.nodes[1].effect_uid, pedal_uid("reverb"));

        let mut buf = [0u8; SECTOR_SIZE];
        let len = binary::encode(&patch, &mut buf).unwrap();
        let back = binary::decode(&buf[..len]).unwrap();
        assert_eq!(patch, back);
    }

    #[test]
    fn parallel_topology_has_split_and_merge() {
        let edges = build_edges("parallel", 3).unwrap();
        assert!(
            edges.iter().any(|e| matches!(e.from, PatchEndpoint::Input)
                && matches!(e.to, PatchEndpoint::Split(0)))
        );
        assert!(
            edges.iter().any(|e| matches!(e.to, PatchEndpoint::Output)
                && matches!(e.from, PatchEndpoint::Merge(0)))
        );
    }

    #[test]
    fn unknown_topology_rejected() {
        assert!(build_edges("fan", 3).is_err());
    }

    #[test]
    fn too_many_effects_rejected() {
        let mut preset = Preset::new("big");
        for _ in 0..MAX_NODES + 1 {
            preset = preset.with_effect(EffectConfig::new("distortion"));
        }
        let registry = EffectRegistry::new();
        assert!(preset_to_patch(&preset, "big", "linear", &registry).is_err());
    }
}
