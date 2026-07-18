//! Patch validation: structural integrity (always) and pedal-fit (export time).
//!
//! Structural checks need nothing but the patch. Pedal-fit checks (effect set,
//! CPU budget, SDRAM) need facts the registry/firmware own — those are passed
//! in as slices so this crate never depends on `sonido-registry` or the effect
//! set, keeping it light enough to sit under the firmware.

#[cfg(not(feature = "std"))]
use alloc::{format, string::String, vec::Vec};

use crate::{MAX_NODES, MAX_PARAMS, MacroTarget, Patch, PatchEndpoint};

/// Whether a finding blocks export or merely warns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// Export must be refused (override possible in the UI for budget warnings,
    /// never for structural errors).
    Error,
    /// Export is allowed but the user should know.
    Warning,
}

/// A single validation result.
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    /// Whether this blocks export.
    pub severity: Severity,
    /// Human-readable explanation.
    pub message: String,
}

impl Finding {
    fn error(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            message: message.into(),
        }
    }
    fn warn(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            message: message.into(),
        }
    }
}

/// Whether any finding is an [`Severity::Error`].
pub fn has_errors(findings: &[Finding]) -> bool {
    findings.iter().any(|f| f.severity == Severity::Error)
}

/// Structural integrity — independent of any platform.
///
/// Checks node/param caps, A/B snapshot agreement, non-zero UIDs, and edge
/// referential integrity (every `Node(i)` endpoint indexes a real node).
pub fn validate_structure(patch: &Patch) -> Vec<Finding> {
    let mut out = Vec::new();

    if patch.nodes.len() > MAX_NODES {
        out.push(Finding::error(format!(
            "{} effect nodes exceeds the {MAX_NODES}-node maximum",
            patch.nodes.len()
        )));
    }

    for (i, node) in patch.nodes.iter().enumerate() {
        if node.effect_uid == 0 {
            out.push(Finding::error(format!("node {i} has no effect (UID 0)")));
        }
        if node.params_a.len() != node.params_b.len() {
            out.push(Finding::error(format!(
                "node {i} A/B snapshots differ in length ({} vs {})",
                node.params_a.len(),
                node.params_b.len()
            )));
        }
        if node.params_a.len() > MAX_PARAMS {
            out.push(Finding::error(format!(
                "node {i} has {} params, exceeds max {MAX_PARAMS}",
                node.params_a.len()
            )));
        }
    }

    let n = patch.nodes.len() as u8;
    for (i, edge) in patch.edges.iter().enumerate() {
        for (which, ep) in [("from", edge.from), ("to", edge.to)] {
            if let PatchEndpoint::Node(idx) = ep
                && idx >= n
            {
                out.push(Finding::error(format!(
                    "edge {i} {which} references node {idx}, but only {n} nodes exist"
                )));
            }
        }
    }

    // Macro targets must address an existing slot.
    for (mi, m) in patch.macros.iter().enumerate() {
        for spec in &m.mappings {
            if let MacroTarget::Slot { slot, .. } = spec.target
                && (slot as usize) >= patch.nodes.len()
            {
                out.push(Finding::error(format!(
                    "macro {mi} targets slot {slot}, but only {} nodes exist",
                    patch.nodes.len()
                )));
            }
        }
    }

    out
}

/// Per-effect cost data the pedal owns, passed in to keep this crate decoupled.
pub struct PedalLimits<'a> {
    /// Effect UIDs the pedal firmware can instantiate.
    pub allowed_uids: &'a [u16],
    /// `(uid, cycles_per_block)` measured on-device.
    pub cycle_table: &'a [(u16, u32)],
    /// `(uid, sdram_bytes)` static allocation per effect.
    pub sdram_table: &'a [(u16, u32)],
    /// Audio-block cycle budget (e.g. 320_000 at 48 kHz / 32-sample blocks).
    pub cycle_budget: u32,
    /// Warn once estimated cycles cross this fraction of the budget (e.g. 0.7).
    pub warn_fraction: f32,
    /// Total SDRAM available to effects (bytes).
    pub sdram_budget: u32,
}

fn lookup(table: &[(u16, u32)], uid: u16) -> Option<u32> {
    table.iter().find(|(u, _)| *u == uid).map(|(_, v)| *v)
}

/// Pedal-fit validation: structural checks plus effect-set, CPU, and memory
/// budgets. Call before flashing or exporting a pedal patch.
///
/// Returns all findings; the caller refuses export iff [`has_errors`].
pub fn validate_for_pedal(patch: &Patch, limits: &PedalLimits) -> Vec<Finding> {
    let mut out = validate_structure(patch);

    // Effect set.
    for (i, node) in patch.nodes.iter().enumerate() {
        if !limits.allowed_uids.contains(&node.effect_uid) {
            out.push(Finding::error(format!(
                "node {i} (UID {}) is not available on the pedal",
                node.effect_uid
            )));
        }
    }

    // CPU budget — sum measured cycles, plus a small per-node graph overhead.
    let mut total_cycles: u64 = 0;
    let mut missing_cycle_data = false;
    for node in &patch.nodes {
        match lookup(limits.cycle_table, node.effect_uid) {
            Some(c) => total_cycles += c as u64,
            None => missing_cycle_data = true,
        }
    }
    // Graph scheduling/buffer overhead grows with node count.
    total_cycles += (patch.nodes.len() as u64) * 400;

    if missing_cycle_data {
        out.push(Finding::warn(
            "some effects have no measured cycle data; CPU estimate is a lower bound",
        ));
    }
    let budget = limits.cycle_budget as u64;
    if budget > 0 {
        if total_cycles > budget {
            out.push(Finding::error(format!(
                "estimated {total_cycles} cycles/block exceeds the {budget} budget — \
                 this patch will not run in real time on the pedal"
            )));
        } else if (total_cycles as f32) > (budget as f32) * limits.warn_fraction {
            out.push(Finding::warn(format!(
                "estimated {total_cycles} cycles/block is {:.0}% of the budget",
                100.0 * total_cycles as f32 / budget as f32
            )));
        }
    }

    // SDRAM — warn only (never the gating constraint at these sizes).
    let mut total_sdram: u64 = 0;
    for node in &patch.nodes {
        if let Some(b) = lookup(limits.sdram_table, node.effect_uid) {
            total_sdram += b as u64;
        }
    }
    if limits.sdram_budget > 0 && total_sdram > limits.sdram_budget as u64 {
        out.push(Finding::warn(format!(
            "estimated {total_sdram} bytes SDRAM exceeds the {} budget",
            limits.sdram_budget
        )));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MacroMappingSpec, MorphCurve, PatchEdge, PatchEndpoint, PatchNode};
    use alloc::vec;

    fn two_node() -> Patch {
        Patch::linear_chain(
            "t",
            vec![PatchNode::new(1, vec![8.0]), PatchNode::new(11, vec![0.5])],
        )
    }

    #[test]
    fn clean_patch_has_no_errors() {
        assert!(!has_errors(&validate_structure(&two_node())));
    }

    #[test]
    fn dangling_edge_is_error() {
        let mut p = two_node();
        p.edges.push(PatchEdge::new(
            PatchEndpoint::Node(9),
            PatchEndpoint::Output,
        ));
        assert!(has_errors(&validate_structure(&p)));
    }

    #[test]
    fn macro_targeting_missing_slot_is_error() {
        let mut p = two_node();
        p.macros[0].mappings.push(MacroMappingSpec {
            target: MacroTarget::Slot { slot: 5, param: 0 },
            min: 0.0,
            max: 1.0,
            curve: MorphCurve::Linear,
        });
        assert!(has_errors(&validate_structure(&p)));
    }

    #[test]
    fn effect_not_on_pedal_is_error() {
        let p = two_node();
        let limits = PedalLimits {
            allowed_uids: &[1], // UID 11 (reverb) not allowed
            cycle_table: &[(1, 1000), (11, 1000)],
            sdram_table: &[],
            cycle_budget: 320_000,
            warn_fraction: 0.7,
            sdram_budget: 64 * 1024 * 1024,
        };
        assert!(has_errors(&validate_for_pedal(&p, &limits)));
    }

    #[test]
    fn over_cpu_budget_is_error() {
        let p = two_node();
        let limits = PedalLimits {
            allowed_uids: &[1, 11],
            cycle_table: &[(1, 200_000), (11, 200_000)],
            sdram_table: &[],
            cycle_budget: 320_000,
            warn_fraction: 0.7,
            sdram_budget: 0,
        };
        let findings = validate_for_pedal(&p, &limits);
        assert!(has_errors(&findings));
    }

    #[test]
    fn near_budget_warns_but_allows() {
        let p = two_node();
        let limits = PedalLimits {
            allowed_uids: &[1, 11],
            cycle_table: &[(1, 130_000), (11, 110_000)], // 240k + overhead = 75% of 320k
            sdram_table: &[],
            cycle_budget: 320_000,
            warn_fraction: 0.7,
            sdram_budget: 0,
        };
        let findings = validate_for_pedal(&p, &limits);
        assert!(!has_errors(&findings));
        assert!(findings.iter().any(|f| f.severity == Severity::Warning));
    }
}
