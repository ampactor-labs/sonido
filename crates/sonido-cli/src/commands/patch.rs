//! Rig files: `sonido patch export` turns a preset TOML or a DSL chain into a
//! rig (`.json` for the editor, or a 4 KB `.bin` sector), and `sonido patch
//! inspect` prints one. [`RigSource`] and [`check_for_pedal`] are shared with
//! `sonido pedal push`.
//!
//! Arbitrary topologies (splits/merges) are authored in the editor; presets and
//! DSL chains take the well-defined `linear` and `parallel` shapes.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use sonido_config::Preset;
use sonido_patch::validate::{Severity, has_errors, validate_for_daisy};
use sonido_patch::{Patch, SECTOR_SIZE, binary};
use sonido_registry::{EffectRegistry, effect_by_uid};

/// Rig file management.
#[derive(Args)]
pub struct PatchArgs {
    #[command(subcommand)]
    pub command: PatchCommand,
}

/// Subcommands for `sonido patch`.
#[derive(Subcommand)]
pub enum PatchCommand {
    /// Export a preset TOML or DSL chain as a rig: `.json` or a `.bin` sector.
    Export {
        #[command(flatten)]
        rig: RigSource,
        /// Output file: `.json` (editor) or `.bin` (pedal sector).
        #[arg(short, long)]
        output: PathBuf,
        /// Fail unless the rig runs on the pedal (its effects and CPU rule).
        #[arg(long)]
        pedal: bool,
    },
    /// Decode a `.bin` or `.json` rig and print a human-readable summary.
    Inspect {
        /// Path to the rig.
        file: PathBuf,
    },
}

/// Where a rig comes from: a file, or a DSL chain.
#[derive(Args)]
pub struct RigSource {
    /// A rig (`.json` from the editor, a `.bin` sector) or a preset `.toml`.
    #[arg(conflicts_with = "from_dsl")]
    pub file: Option<PathBuf>,
    /// A DSL chain instead of a file (e.g. "distortion:drive=20 | reverb:mix=30").
    #[arg(long, conflicts_with = "file")]
    pub from_dsl: Option<String>,
    /// Topology for a preset or DSL chain: `linear` or `parallel`.
    #[arg(short, long, default_value = "linear")]
    pub topology: String,
    /// Name the rig (defaults to the file's or preset's name).
    #[arg(long)]
    pub name: Option<String>,
}

impl RigSource {
    /// Load the rig.
    pub fn load(&self) -> anyhow::Result<Patch> {
        let ext = self
            .file
            .as_deref()
            .and_then(Path::extension)
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        let mut patch = match (&self.file, ext.as_deref()) {
            (Some(path), Some("json")) => serde_json::from_slice(&read(path)?)
                .map_err(|e| anyhow::anyhow!("'{}' is not a rig: {e}", path.display()))?,
            (Some(path), Some("bin")) => binary::decode(&read(path)?)
                .map_err(|e| anyhow::anyhow!("'{}' is not a rig: {e}", path.display()))?,
            _ => {
                let registry = EffectRegistry::new();
                let preset =
                    load_preset(self.file.as_deref(), self.from_dsl.as_deref(), &registry)?;
                let name = self.name.clone().unwrap_or_else(|| preset.name.clone());
                // The shared conversion (sonido_config::patch): same parameter
                // resolution as preset loading.
                let converted = sonido_config::preset_to_patch(
                    &preset,
                    &name,
                    Some(&self.topology),
                    &registry,
                )?;
                for warning in &converted.warnings {
                    eprintln!("Warning: {warning}");
                }
                converted.patch
            }
        };
        if let Some(name) = &self.name {
            patch.name.clone_from(name);
        }
        Ok(patch)
    }
}

fn read(path: &Path) -> anyhow::Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| anyhow::anyhow!("Failed to read '{}': {e}", path.display()))
}

/// Check `patch` with the rule the pedal itself enforces
/// (`validate_for_daisy`): print warnings, fail on errors.
pub fn check_for_pedal(patch: &Patch) -> anyhow::Result<()> {
    let findings = validate_for_daisy(patch);
    for f in &findings {
        let level = match f.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        eprintln!("{level}: {}", f.message);
    }
    if has_errors(&findings) {
        anyhow::bail!("'{}' won't run on the pedal", patch.name);
    }
    Ok(())
}

/// Run the `sonido patch` command.
pub fn run(args: PatchArgs) -> anyhow::Result<()> {
    match args.command {
        PatchCommand::Export { rig, output, pedal } => {
            let patch = rig.load()?;
            if pedal {
                check_for_pedal(&patch)?;
            }
            export_patch(&patch, &output)
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

// ---------------------------------------------------------------------------
// Export / Inspect
// ---------------------------------------------------------------------------

fn export_patch(patch: &Patch, output: &Path) -> anyhow::Result<()> {
    let json = output
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("json"));
    let bytes = if json {
        serde_json::to_vec_pretty(patch)?
    } else {
        let mut buf = [0u8; SECTOR_SIZE];
        let len = binary::encode(patch, &mut buf)
            .map_err(|e| anyhow::anyhow!("Failed to encode patch: {e}"))?;
        buf[..len].to_vec()
    };
    std::fs::write(output, &bytes)
        .map_err(|e| anyhow::anyhow!("Failed to write '{}': {}", output.display(), e))?;
    println!(
        "Exported rig '{}' to '{}' ({} effect(s), {} byte(s), {} macro(s))",
        patch.name,
        output.display(),
        patch.nodes.len(),
        bytes.len(),
        patch.active_macro_count(),
    );
    Ok(())
}

fn inspect(file: &Path) -> anyhow::Result<()> {
    let patch = RigSource {
        file: Some(file.to_owned()),
        from_dsl: None,
        topology: "linear".into(),
        name: None,
    }
    .load()?;
    print_rig(&patch);
    Ok(())
}

/// Print a rig: globals, morph, nodes with their A/B values, edges, macros.
pub fn print_rig(patch: &Patch) {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_config::{EffectConfig, Preset};
    use sonido_patch::{MAX_NODES, PatchEndpoint};
    use sonido_registry::effect_uid;

    fn pedal_uid(id: &str) -> u16 {
        effect_uid(id).unwrap()
    }

    #[test]
    fn export_then_inspect_roundtrip_via_disk() {
        let preset = Preset::new("CLI Test")
            .with_effect(EffectConfig::new("distortion").with_param("drive", "20.0"))
            .with_effect(EffectConfig::new("reverb").with_param("mix", "30"));
        let registry = EffectRegistry::new();
        let patch = sonido_config::preset_to_patch(&preset, "CLI Test", Some("linear"), &registry)
            .unwrap()
            .patch;

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
        let edges = sonido_config::patch::build_edges("parallel", 3).unwrap();
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
        assert!(sonido_config::patch::build_edges("fan", 3).is_err());
    }

    #[test]
    fn too_many_effects_rejected() {
        let mut preset = Preset::new("big");
        for _ in 0..MAX_NODES + 1 {
            preset = preset.with_effect(EffectConfig::new("distortion"));
        }
        let registry = EffectRegistry::new();
        assert!(sonido_config::preset_to_patch(&preset, "big", Some("linear"), &registry).is_err());
    }
}
