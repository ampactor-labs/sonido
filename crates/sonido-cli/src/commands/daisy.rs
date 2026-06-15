//! Daisy Seed preset management commands.
//!
//! Provides `sonido daisy export` and `sonido daisy inspect` for packing
//! effect presets into the 4096-byte binary sectors that the Hothouse firmware
//! reads from QSPI flash.
//!
//! # Binary format
//!
//! Each sector is exactly [`SECTOR_SIZE`] (4096) bytes:
//!
//! ```text
//! Offset  Size  Field
//! ------  ----  -----
//!      0     4  magic = 0x534F4E44 ("SOND"), little-endian u32
//!      4     1  slot index (0-7)
//!      5     3  padding (zero)
//!      8   400  DaisyPresetSlot
//!    408  3688  zero padding
//! ```

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use sonido_config::Preset;
use sonido_registry::{EffectRegistry, PEDAL_EFFECT_IDS};

// ---------------------------------------------------------------------------
// CLI types
// ---------------------------------------------------------------------------

/// Daisy Seed preset management.
#[derive(Args)]
pub struct DaisyArgs {
    #[command(subcommand)]
    pub command: DaisyCommand,
}

/// Subcommands for `sonido daisy`.
#[derive(Subcommand)]
pub enum DaisyCommand {
    /// Export a preset TOML to Daisy binary format
    Export {
        /// Path to preset TOML file
        #[arg(conflicts_with = "from_dsl")]
        preset: Option<PathBuf>,
        /// DSL string to export directly (e.g., "distortion:drive=20|reverb:mix=30")
        #[arg(long, conflicts_with = "preset")]
        from_dsl: Option<String>,
        /// Output binary file
        #[arg(short, long)]
        output: PathBuf,
        /// Preset slot (0-7)
        #[arg(short, long, default_value = "0")]
        slot: u8,
    },
    /// Inspect a Daisy binary file and print a human-readable summary
    Inspect {
        /// Path to binary file
        file: PathBuf,
    },
}

// ---------------------------------------------------------------------------
// Binary layout constants
// ---------------------------------------------------------------------------

const MAGIC: u32 = 0x534F_4E44; // "SOND" in ASCII, little-endian
const SECTOR_SIZE: usize = 4096;

// ---------------------------------------------------------------------------
// Binary structs
// ---------------------------------------------------------------------------

/// Serialized state for one effect slot (132 bytes).
///
/// # Layout
///
/// ```text
/// effect_idx  : u8         (1 byte)
/// param_count : u8         (1 byte)
/// _pad        : [u8; 2]    (2 bytes)
/// params_a    : [f32; 16]  (64 bytes)  — morph position A
/// params_b    : [f32; 16]  (64 bytes)  — morph position B
/// ```
#[repr(C)]
struct DaisyEffectSlot {
    effect_idx: u8,
    param_count: u8,
    _pad: [u8; 2],
    params_a: [f32; 16],
    params_b: [f32; 16],
}

/// Serialized state for one preset slot (400 bytes).
///
/// # Layout
///
/// ```text
/// valid      : u8                     (1 byte)
/// topology   : u8                     (1 byte)
/// num_slots  : u8                     (1 byte)
/// _pad       : u8                     (1 byte)
/// effects    : [DaisyEffectSlot; 3]   (396 bytes)
/// ```
#[repr(C)]
struct DaisyPresetSlot {
    valid: u8,
    topology: u8,
    num_slots: u8,
    _pad: u8,
    effects: [DaisyEffectSlot; 3],
}

// Compile-time size assertions — these catch any accidental struct padding.
const _: () = assert!(core::mem::size_of::<DaisyEffectSlot>() == 132);
const _: () = assert!(core::mem::size_of::<DaisyPresetSlot>() == 400);

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Run the `sonido daisy` command.
pub fn run(args: DaisyArgs) -> anyhow::Result<()> {
    match args.command {
        DaisyCommand::Export {
            preset,
            from_dsl,
            output,
            slot,
        } => match (preset, from_dsl) {
            (Some(path), None) => export_preset(&path, &output, slot),
            (None, Some(dsl)) => export_from_dsl(&dsl, &output, slot),
            _ => anyhow::bail!("Provide either a preset TOML path or --from-dsl, not both"),
        },
        DaisyCommand::Inspect { file } => inspect_binary(&file),
    }
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

fn export_preset(preset_path: &Path, output_path: &Path, slot: u8) -> anyhow::Result<()> {
    let preset = Preset::load(preset_path)
        .map_err(|e| anyhow::anyhow!("Failed to load preset '{}': {}", preset_path.display(), e))?;
    export_preset_obj(&preset, output_path, slot)
}

fn export_from_dsl(dsl: &str, output_path: &Path, slot: u8) -> anyhow::Result<()> {
    let registry = EffectRegistry::new();
    let snapshot = sonido_graph_dsl::snapshot_from_dsl(dsl, &registry)
        .map_err(|e| anyhow::anyhow!("Failed to parse DSL: {}", e))?;
    let preset = sonido_graph_dsl::snapshot_to_preset(&snapshot, "dsl_export", &registry);
    export_preset_obj(&preset, output_path, slot)
}

/// Core export: Preset → binary sector file.
fn export_preset_obj(preset: &Preset, output_path: &Path, slot: u8) -> anyhow::Result<()> {
    // Validate slot range.
    if slot > 7 {
        anyhow::bail!("Slot must be 0-7, got {}", slot);
    }

    // Validate effect count.
    if preset.effects.len() > 3 {
        anyhow::bail!(
            "Daisy firmware supports at most 3 effects, preset '{}' has {}",
            preset.name,
            preset.effects.len()
        );
    }

    // Resolve topology byte.
    let topology = topology_byte_from_name(preset.topology.as_deref()).ok_or_else(|| {
        anyhow::anyhow!(
            "Unrecognized topology '{}'. Valid values: linear, parallel, fan",
            preset.topology.as_deref().unwrap_or("<none>")
        )
    })?;

    // Build effect registry for parameter descriptors.
    let registry = EffectRegistry::new();

    // Build each effect slot.
    let mut effect_slots: [DaisyEffectSlot; 3] = [
        zeroed_effect_slot(),
        zeroed_effect_slot(),
        zeroed_effect_slot(),
    ];

    for (i, effect_cfg) in preset.effects.iter().enumerate() {
        let id = effect_cfg.effect_type.as_str();

        // Find the effect index in the pedal list.
        let effect_idx = PEDAL_EFFECT_IDS
            .iter()
            .position(|&e| e == id)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Effect '{}' is not available on the Daisy pedal. \
                     Available effects: {}",
                    id,
                    PEDAL_EFFECT_IDS.join(", ")
                )
            })?;

        // Create a temporary effect instance to read parameter descriptors.
        let effect = registry
            .create(id, 48000.0)
            .ok_or_else(|| anyhow::anyhow!("Effect '{}' not found in registry", id))?;

        let param_count = effect.effect_param_count();
        let clamped_count = param_count.min(16) as u8;

        // Collect descriptors and build a name -> index map.
        let mut name_to_index: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        let mut defaults = [0.0f32; 16];

        for idx in 0..param_count.min(16) {
            if let Some(desc) = effect.effect_param_info(idx) {
                defaults[idx] = desc.default;
                name_to_index.insert(desc.name.to_lowercase(), idx);
                name_to_index.insert(desc.short_name.to_lowercase(), idx);
                name_to_index.insert(desc.string_id.to_string(), idx);
            }
        }

        // Start params_a from defaults, then overlay TOML values.
        let mut params_a = defaults;

        for (key, val_str) in &effect_cfg.params {
            let lower: String = key.to_lowercase();
            if let Some(&param_idx) = name_to_index.get(&lower) {
                match val_str.parse::<f32>() {
                    Ok(value) => {
                        // Clamp to descriptor range.
                        if let Some(desc) = effect.effect_param_info(param_idx) {
                            params_a[param_idx] = value.clamp(desc.min, desc.max);
                        } else {
                            params_a[param_idx] = value;
                        }
                    }
                    Err(_) => {
                        eprintln!(
                            "Warning: could not parse param '{}' = '{}' as f32, using default",
                            key, val_str
                        );
                    }
                }
            } else {
                eprintln!(
                    "Warning: effect '{}' has no parameter named '{}', ignoring",
                    id, key
                );
            }
        }

        // params_b mirrors params_a (morph A = B initially).
        effect_slots[i] = DaisyEffectSlot {
            effect_idx: effect_idx as u8,
            param_count: clamped_count,
            _pad: [0; 2],
            params_a,
            params_b: params_a,
        };
    }

    let preset_slot = DaisyPresetSlot {
        valid: 1,
        topology,
        num_slots: preset.effects.len() as u8,
        _pad: 0,
        effects: effect_slots,
    };

    // Serialize to 4096-byte sector.
    let sector = build_sector(slot, &preset_slot);

    std::fs::write(output_path, &sector)
        .map_err(|e| anyhow::anyhow!("Failed to write '{}': {}", output_path.display(), e))?;

    println!(
        "Exported preset '{}' to '{}' (slot {}, {} effect(s), topology={})",
        preset.name,
        output_path.display(),
        slot,
        preset.effects.len(),
        preset.topology.as_deref().unwrap_or("linear"),
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Inspect
// ---------------------------------------------------------------------------

fn inspect_binary(file_path: &Path) -> anyhow::Result<()> {
    let data = std::fs::read(file_path)
        .map_err(|e| anyhow::anyhow!("Failed to read '{}': {}", file_path.display(), e))?;

    if data.len() < 8 {
        anyhow::bail!(
            "'{}' is too small ({} bytes, expected at least 8)",
            file_path.display(),
            data.len()
        );
    }

    // Parse magic (little-endian u32 at offset 0).
    let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
    if magic != MAGIC {
        anyhow::bail!(
            "'{}' has unexpected magic 0x{:08X} (expected 0x{:08X} \"SOND\")",
            file_path.display(),
            magic,
            MAGIC
        );
    }

    let slot = data[4];
    // data[5..8] is padding

    if data.len() < 8 + core::mem::size_of::<DaisyPresetSlot>() {
        anyhow::bail!(
            "'{}' is too small for a full preset slot ({} bytes)",
            file_path.display(),
            data.len()
        );
    }

    // Read DaisyPresetSlot from offset 8.
    let ps = read_preset_slot(&data[8..]);

    println!("File:      {}", file_path.display());
    println!("Slot:      {}", slot);
    println!("Valid:     {}", if ps.valid != 0 { "yes" } else { "no" });
    println!(
        "Topology:  {} ({})",
        ps.topology,
        topology_name(ps.topology)
    );
    println!("Effects:   {}", ps.num_slots);
    println!();

    let registry = EffectRegistry::new();

    for i in 0..ps.num_slots.min(3) as usize {
        let es = &ps.effects[i];
        let effect_name = PEDAL_EFFECT_IDS
            .get(es.effect_idx as usize)
            .copied()
            .unwrap_or("<unknown>");

        println!(
            "  Effect {}: {} (idx={}, {} params)",
            i, effect_name, es.effect_idx, es.param_count
        );

        // Try to print named params using registry descriptors.
        let names: Vec<Option<&'static str>> =
            if let Some(effect) = registry.create(effect_name, 48000.0) {
                (0..es.param_count as usize)
                    .map(|j| effect.effect_param_info(j).map(|d| d.name))
                    .collect()
            } else {
                vec![None; es.param_count as usize]
            };

        for j in 0..es.param_count.min(16) as usize {
            let name = names.get(j).and_then(|n| *n).unwrap_or("param");
            let va = es.params_a[j];
            let vb = es.params_b[j];
            if (va - vb).abs() < f32::EPSILON {
                println!("    [{j}] {name}: {va:.4}");
            } else {
                println!("    [{j}] {name}: A={va:.4}  B={vb:.4}");
            }
        }
        println!();
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn zeroed_effect_slot() -> DaisyEffectSlot {
    DaisyEffectSlot {
        effect_idx: 0,
        param_count: 0,
        _pad: [0; 2],
        params_a: [0.0; 16],
        params_b: [0.0; 16],
    }
}

/// Map a topology name to its Daisy binary byte value.
///
/// | Name | Byte |
/// |------|------|
/// | `None` or `"linear"` | `0` |
/// | `"parallel"` | `1` |
/// | `"fan"` | `2` |
///
/// Returns `None` for unrecognized names.
fn topology_byte_from_name(name: Option<&str>) -> Option<u8> {
    match name {
        None | Some("linear") => Some(0),
        Some("parallel") => Some(1),
        Some("fan") => Some(2),
        _ => None,
    }
}

fn topology_name(byte: u8) -> &'static str {
    match byte {
        0 => "linear",
        1 => "parallel",
        2 => "fan",
        _ => "unknown",
    }
}

/// Serialize one `DaisyEffectSlot` into `dst` (must be exactly 132 bytes).
fn encode_effect_slot(es: &DaisyEffectSlot, dst: &mut [u8]) {
    dst[0] = es.effect_idx;
    dst[1] = es.param_count;
    dst[2] = 0;
    dst[3] = 0;
    for (i, &v) in es.params_a.iter().enumerate() {
        let off = 4 + i * 4;
        dst[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }
    for (i, &v) in es.params_b.iter().enumerate() {
        let off = 4 + 64 + i * 4;
        dst[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }
}

/// Decode one `DaisyEffectSlot` from `src` (must be at least 132 bytes).
fn decode_effect_slot(src: &[u8]) -> DaisyEffectSlot {
    let mut params_a = [0.0f32; 16];
    let mut params_b = [0.0f32; 16];
    for i in 0..16 {
        let off = 4 + i * 4;
        params_a[i] = f32::from_le_bytes(src[off..off + 4].try_into().unwrap());
    }
    for i in 0..16 {
        let off = 4 + 64 + i * 4;
        params_b[i] = f32::from_le_bytes(src[off..off + 4].try_into().unwrap());
    }
    DaisyEffectSlot {
        effect_idx: src[0],
        param_count: src[1],
        _pad: [0; 2],
        params_a,
        params_b,
    }
}

/// Serialize the header + preset slot into a 4096-byte sector.
fn build_sector(slot: u8, preset: &DaisyPresetSlot) -> Vec<u8> {
    let mut buf = vec![0u8; SECTOR_SIZE];

    // Magic (4 bytes, little-endian).
    buf[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    // Slot index.
    buf[4] = slot;
    // Bytes 5-7: zero padding (already zero from vec initialization).

    // DaisyPresetSlot at offset 8 (400 bytes total).
    // Header bytes (4): valid, topology, num_slots, _pad.
    buf[8] = preset.valid;
    buf[9] = preset.topology;
    buf[10] = preset.num_slots;
    buf[11] = 0;
    // Three effect slots at offsets 12, 144, 276 (each 132 bytes).
    for (i, es) in preset.effects.iter().enumerate() {
        let off = 12 + i * 132;
        encode_effect_slot(es, &mut buf[off..off + 132]);
    }

    buf
}

/// Parse a `DaisyPresetSlot` from a byte slice (must be at least 400 bytes).
fn read_preset_slot(src: &[u8]) -> DaisyPresetSlot {
    DaisyPresetSlot {
        valid: src[0],
        topology: src[1],
        num_slots: src[2],
        _pad: 0,
        effects: [
            decode_effect_slot(&src[4..136]),
            decode_effect_slot(&src[136..268]),
            decode_effect_slot(&src[268..400]),
        ],
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_config::{EffectConfig, Preset};

    #[test]
    fn struct_sizes_are_correct() {
        assert_eq!(core::mem::size_of::<DaisyEffectSlot>(), 132);
        assert_eq!(core::mem::size_of::<DaisyPresetSlot>(), 400);
    }

    #[test]
    fn sector_size_is_4096() {
        let slot = DaisyPresetSlot {
            valid: 0,
            topology: 0,
            num_slots: 0,
            _pad: 0,
            effects: [
                zeroed_effect_slot(),
                zeroed_effect_slot(),
                zeroed_effect_slot(),
            ],
        };
        let buf = build_sector(0, &slot);
        assert_eq!(buf.len(), SECTOR_SIZE);
    }

    #[test]
    fn sector_magic_and_slot_written_correctly() {
        let slot = DaisyPresetSlot {
            valid: 1,
            topology: 0,
            num_slots: 0,
            _pad: 0,
            effects: [
                zeroed_effect_slot(),
                zeroed_effect_slot(),
                zeroed_effect_slot(),
            ],
        };
        let buf = build_sector(3, &slot);

        let magic = u32::from_le_bytes(buf[0..4].try_into().unwrap());
        assert_eq!(magic, MAGIC);
        assert_eq!(buf[4], 3); // slot index
        assert_eq!(&buf[5..8], &[0, 0, 0]); // padding
    }

    #[test]
    fn roundtrip_preset_slot() {
        let mut params_a = [0.0f32; 16];
        params_a[0] = 15.0;
        params_a[1] = 0.5;
        params_a[2] = 1.0;

        let original = DaisyPresetSlot {
            valid: 1,
            topology: 1,
            num_slots: 2,
            _pad: 0,
            effects: [
                DaisyEffectSlot {
                    effect_idx: 11, // arbitrary index — this test checks byte roundtrip only
                    param_count: 3,
                    _pad: [0; 2],
                    params_a,
                    params_b: params_a,
                },
                zeroed_effect_slot(),
                zeroed_effect_slot(),
            ],
        };

        let buf = build_sector(0, &original);
        let restored = read_preset_slot(&buf[8..]);

        assert_eq!(restored.valid, original.valid);
        assert_eq!(restored.topology, original.topology);
        assert_eq!(restored.num_slots, original.num_slots);
        assert_eq!(
            restored.effects[0].effect_idx,
            original.effects[0].effect_idx
        );
        assert_eq!(
            restored.effects[0].param_count,
            original.effects[0].param_count
        );
        assert_eq!(
            restored.effects[0].params_a[0],
            original.effects[0].params_a[0]
        );
    }

    #[test]
    fn export_rejects_more_than_3_effects() {
        let preset = Preset::new("test")
            .with_effect(EffectConfig::new("distortion"))
            .with_effect(EffectConfig::new("reverb"))
            .with_effect(EffectConfig::new("chorus"))
            .with_effect(EffectConfig::new("delay"));

        let tmp = tempfile::NamedTempFile::new().unwrap();
        preset.save(tmp.path()).unwrap();

        let out = tempfile::NamedTempFile::new().unwrap();
        let result = export_preset(tmp.path(), out.path(), 0);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("at most 3 effects")
        );
    }

    #[test]
    fn export_rejects_unknown_effect() {
        let preset = Preset::new("test").with_effect(EffectConfig::new("not_a_pedal_effect"));

        let tmp = tempfile::NamedTempFile::new().unwrap();
        preset.save(tmp.path()).unwrap();

        let out = tempfile::NamedTempFile::new().unwrap();
        let result = export_preset(tmp.path(), out.path(), 0);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("not available on the Daisy pedal")
        );
    }

    #[test]
    fn export_rejects_invalid_slot() {
        let preset = Preset::new("test").with_effect(EffectConfig::new("distortion"));

        let tmp = tempfile::NamedTempFile::new().unwrap();
        preset.save(tmp.path()).unwrap();

        let out = tempfile::NamedTempFile::new().unwrap();
        let result = export_preset(tmp.path(), out.path(), 8);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Slot must be 0-7"));
    }

    #[test]
    fn export_single_effect_produces_valid_sector() {
        let preset = Preset::new("smoke")
            .with_effect(EffectConfig::new("distortion").with_param("drive", "20.0"));

        let tmp = tempfile::NamedTempFile::new().unwrap();
        preset.save(tmp.path()).unwrap();

        let out = tempfile::NamedTempFile::new().unwrap();
        export_preset(tmp.path(), out.path(), 0).unwrap();

        let data = std::fs::read(out.path()).unwrap();
        assert_eq!(data.len(), SECTOR_SIZE);

        let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
        assert_eq!(magic, MAGIC);

        let ps = read_preset_slot(&data[8..]);
        assert_eq!(ps.valid, 1);
        assert_eq!(ps.num_slots, 1);
        assert_eq!(ps.effects[0].effect_idx, pedal_idx("distortion"));
    }

    #[test]
    fn topology_name_roundtrips() {
        assert_eq!(topology_name(0), "linear");
        assert_eq!(topology_name(1), "parallel");
        assert_eq!(topology_name(2), "fan");
        assert_eq!(topology_name(99), "unknown");
    }

    #[test]
    fn topology_byte_from_name_maps_correctly() {
        assert_eq!(topology_byte_from_name(None), Some(0));
        assert_eq!(topology_byte_from_name(Some("linear")), Some(0));
        assert_eq!(topology_byte_from_name(Some("parallel")), Some(1));
        assert_eq!(topology_byte_from_name(Some("fan")), Some(2));
        assert_eq!(topology_byte_from_name(Some("bogus")), None);
    }

    #[test]
    fn export_from_dsl_creates_valid_binary() {
        let dir = std::env::temp_dir();
        let output = dir.join("test_dsl_export.bin");
        export_from_dsl("distortion:drive=20 | reverb:mix=30", &output, 0).unwrap();

        let data = std::fs::read(&output).unwrap();
        assert_eq!(data.len(), SECTOR_SIZE);

        // Check magic.
        let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
        assert_eq!(magic, MAGIC);

        // Check two effects were serialized.
        let ps = read_preset_slot(&data[8..]);
        assert_eq!(ps.valid, 1);
        assert_eq!(ps.num_slots, 2);

        // Indices are positions within PEDAL_EFFECT_IDS; derive them so the
        // assertion tracks the list instead of hard-coding stale positions.
        assert_eq!(ps.effects[0].effect_idx, pedal_idx("distortion"));
        assert_eq!(ps.effects[1].effect_idx, pedal_idx("reverb"));

        let _ = std::fs::remove_file(&output);
    }

    /// Position of an effect id within `PEDAL_EFFECT_IDS`, as the exporter encodes it.
    fn pedal_idx(id: &str) -> u8 {
        PEDAL_EFFECT_IDS
            .iter()
            .position(|&e| e == id)
            .unwrap_or_else(|| panic!("'{id}' missing from PEDAL_EFFECT_IDS")) as u8
    }
}
