//! Export glue: project the editor into a [`Patch`], validate it for the pedal,
//! and serialize it for the three destinations (`.bin` sector, `.json`, and the
//! CLAP graph-player's sidecar preset dir).
//!
//! This is the non-UI logic behind the export panel — pure and unit-tested, so
//! the panel only has to call it and render the results.

use std::path::PathBuf;

use sonido_patch::validate::{Finding, PedalLimits, has_errors, validate_for_pedal};
use sonido_patch::{Patch, SECTOR_SIZE, binary};
use sonido_platform::cycle_table;

/// UIDs the pedal can run — exactly the effects with a cost-table entry.
pub fn pedal_allowed_uids() -> Vec<u16> {
    cycle_table::EFFECT_CYCLES.iter().map(|(u, _)| *u).collect()
}

/// Validate a patch against the pedal's effect set and CPU/memory budgets.
///
/// Returns all findings; the caller refuses the pedal export iff [`has_errors`].
pub fn validate_patch_for_pedal(patch: &Patch) -> Vec<Finding> {
    let allowed = pedal_allowed_uids();
    let limits = PedalLimits {
        allowed_uids: &allowed,
        cycle_table: cycle_table::EFFECT_CYCLES,
        sdram_table: cycle_table::EFFECT_SDRAM,
        cycle_budget: cycle_table::CYCLE_BUDGET_PER_BLOCK,
        chain_fraction: cycle_table::CHAIN_FRACTION,
        warn_fraction: cycle_table::WARN_FRACTION,
        node_overhead: cycle_table::NODE_OVERHEAD_CYCLES,
        sdram_budget: cycle_table::SDRAM_BUDGET_BYTES,
    };
    validate_for_pedal(patch, &limits)
}

/// Whether a patch is safe to flash to the pedal (no error-level findings).
pub fn can_export_to_pedal(patch: &Patch) -> bool {
    !has_errors(&validate_patch_for_pedal(patch))
}

/// Encode a patch into a full zero-padded 4 KB sector, ready to flash or save.
///
/// # Errors
///
/// Propagates [`binary::encode`] errors (too many nodes/params, oversize).
pub fn encode_patch_sector(patch: &Patch) -> Result<[u8; SECTOR_SIZE], binary::PatchError> {
    let mut buf = [0u8; SECTOR_SIZE];
    binary::encode(patch, &mut buf)?;
    Ok(buf)
}

/// Serialize a patch to pretty JSON (CLAP plugin state / portable file).
pub fn patch_to_json(patch: &Patch) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(patch)
}

/// The rig library the graph-player plugin lists
/// ([`sonido_config::paths::rig_library_dir`]); created on first export.
pub fn clap_patches_dir() -> Option<PathBuf> {
    sonido_config::paths::rig_library_dir()
}

/// Sanitize a patch name into a safe file stem.
pub fn safe_file_stem(name: &str) -> String {
    let stem: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = stem.trim_matches('_');
    if trimmed.is_empty() {
        "patch".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Write a patch as JSON into the CLAP sidecar dir, returning the written path.
///
/// "Export as CLAP" is literally this: the plugin's state blob *is* the patch
/// JSON, so the plugin just lists this directory.
///
/// # Errors
///
/// Returns an error if the data dir can't be resolved or I/O fails.
pub fn export_as_clap(patch: &Patch) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let dir = clap_patches_dir().ok_or("could not resolve a data directory")?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.sonidopatch.json", safe_file_stem(&patch.name)));
    std::fs::write(&path, patch_to_json(patch)?)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_patch::{PatchNode, validate::Severity};

    fn dist_uid() -> u16 {
        // distortion is UID 1 and a pedal effect.
        1
    }

    #[test]
    fn clean_pedal_patch_validates() {
        let patch = Patch::linear_chain("ok", vec![PatchNode::new(dist_uid(), vec![8.0])]);
        assert!(can_export_to_pedal(&patch));
    }

    #[test]
    fn unknown_effect_blocks_pedal_export() {
        let patch = Patch::linear_chain("nope", vec![PatchNode::new(99, vec![])]);
        assert!(!can_export_to_pedal(&patch));
    }

    #[test]
    fn every_effect_can_go_to_the_pedal() {
        assert_eq!(pedal_allowed_uids().len(), 36);
    }

    #[test]
    fn editor_and_pedal_apply_the_same_cpu_rule() {
        // The pedal checks rigs with `cycle_table::chain_fits`; the editor with
        // `validate_for_pedal`. They must agree on every rig.
        let rigs: [&[u16]; 5] = [
            &[1, 11, 17],
            &[11, 11],
            &[35],
            &[11, 36],
            &[9, 6, 3, 2, 1, 1],
        ];
        for uids in rigs {
            let nodes = uids.iter().map(|&u| PatchNode::new(u, vec![])).collect();
            let patch = Patch::linear_chain("rig", nodes);
            assert_eq!(
                can_export_to_pedal(&patch),
                cycle_table::chain_fits(uids.iter().copied()),
                "{uids:?}"
            );
        }
    }

    #[test]
    fn two_reverbs_are_refused() {
        // 2 × 179k measured worst blocks = 112% of the budget.
        let patch = Patch::linear_chain(
            "stack",
            vec![PatchNode::new(11, vec![]), PatchNode::new(11, vec![])],
        );
        let findings = validate_patch_for_pedal(&patch);
        assert!(findings.iter().any(|f| f.severity == Severity::Error));
    }

    #[test]
    fn encode_sector_is_full_size() {
        let patch = Patch::linear_chain("x", vec![PatchNode::new(dist_uid(), vec![8.0])]);
        let sector = encode_patch_sector(&patch).unwrap();
        assert_eq!(sector.len(), SECTOR_SIZE);
        // Decodes back.
        assert_eq!(sonido_patch::decode(&sector).unwrap(), patch);
    }

    #[test]
    fn json_roundtrips() {
        let patch = Patch::linear_chain("j", vec![PatchNode::new(dist_uid(), vec![8.0])]);
        let json = patch_to_json(&patch).unwrap();
        let back: Patch = serde_json::from_str(&json).unwrap();
        assert_eq!(patch, back);
    }

    #[test]
    fn file_stem_is_sanitized() {
        assert_eq!(safe_file_stem("My Rig! v2"), "My_Rig__v2");
        assert_eq!(safe_file_stem("///"), "patch");
        assert_eq!(safe_file_stem("clean"), "clean");
    }
}
