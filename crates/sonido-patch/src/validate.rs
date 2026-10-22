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
    /// Refuse once estimated cycles cross this fraction of the budget (e.g. 0.8).
    pub chain_fraction: f32,
    /// Warn once estimated cycles cross this fraction of the budget (e.g. 0.7).
    pub warn_fraction: f32,
    /// Graph cost per node, added to the effects' cycles.
    pub node_overhead: u32,
    /// Total SDRAM available to effects (bytes).
    pub sdram_budget: u32,
}

fn lookup(table: &[(u16, u32)], uid: u16) -> Option<u32> {
    table.iter().find(|(u, _)| *u == uid).map(|(_, v)| *v)
}

/// What a rig costs the pedal, from its effects alone: the numbers
/// [`validate_for_pedal`] judges, cheap enough for the editor to show live.
#[derive(Clone, Debug, PartialEq)]
pub struct PedalLoad {
    /// Effect nodes, of [`MAX_NODES`].
    pub nodes: usize,
    /// Nodes whose effect the pedal doesn't carry.
    pub unavailable: usize,
    /// Estimated share of the pedal's CPU: each effect's measured worst block
    /// plus the graph's per-node cost, over the block budget (0 with no
    /// budget). A lower bound when an effect has no measurement.
    pub cpu: f32,
    /// SDRAM the effects allocate, in bytes.
    pub sdram: u64,
    /// The CPU and memory findings, worded as export shows them.
    pub findings: Vec<Finding>,
}

impl PedalLoad {
    /// The verdict: `None` when the rig fits, a warning near the CPU limit, an
    /// error when the pedal would refuse it (too many effects, one it doesn't
    /// carry, or over its CPU or memory budget).
    pub fn severity(&self) -> Option<Severity> {
        if self.nodes > MAX_NODES || self.unavailable > 0 || has_errors(&self.findings) {
            Some(Severity::Error)
        } else if self.findings.is_empty() {
            None
        } else {
            Some(Severity::Warning)
        }
    }
}

impl PedalLimits<'_> {
    /// What a rig made of these effects costs the pedal.
    pub fn load(&self, uids: impl IntoIterator<Item = u16>) -> PedalLoad {
        let (mut nodes, mut unavailable, mut unmeasured) = (0, 0, false);
        let (mut cycles, mut sdram) = (0u64, 0u64);
        for uid in uids {
            nodes += 1;
            if !self.allowed_uids.contains(&uid) {
                unavailable += 1;
            }
            match lookup(self.cycle_table, uid) {
                Some(c) => cycles += u64::from(c),
                None => unmeasured = true,
            }
            cycles += u64::from(self.node_overhead);
            sdram += u64::from(lookup(self.sdram_table, uid).unwrap_or(0));
        }

        let mut findings = Vec::new();
        if unmeasured {
            findings.push(Finding::warn(
                "some effects have no measured cycle data; CPU estimate is a lower bound",
            ));
        }
        let budget = self.cycle_budget as f32;
        let cpu = if budget > 0.0 {
            cycles as f32 / budget
        } else {
            0.0
        };
        if budget > 0.0 {
            if cpu > self.chain_fraction {
                findings.push(Finding::error(format!(
                    "estimated {:.0}% of the pedal's CPU, over its {:.0}% limit — \
                     this rig would drop out on the pedal",
                    100.0 * cpu,
                    100.0 * self.chain_fraction
                )));
            } else if cpu > self.warn_fraction {
                findings.push(Finding::warn(format!(
                    "estimated {:.0}% of the pedal's CPU",
                    100.0 * cpu
                )));
            }
        }
        // An allocation past the heap would crash the pedal.
        if self.sdram_budget > 0 && sdram > u64::from(self.sdram_budget) {
            findings.push(Finding::error(format!(
                "needs {:.1} MB of the pedal's {:.0} MB SDRAM",
                sdram as f32 / 1_048_576.0,
                self.sdram_budget as f32 / 1_048_576.0
            )));
        }

        PedalLoad {
            nodes,
            unavailable,
            cpu,
            sdram,
            findings,
        }
    }
}

/// Hand the Daisy pedal's limits to `f`: the effects its firmware carries (the
/// measured ones) and the one CPU and memory rule
/// ([`sonido_platform::cycle_table`]) the pedal itself enforces.
fn with_daisy_limits<R>(f: impl FnOnce(&PedalLimits<'_>) -> R) -> R {
    use sonido_platform::cycle_table;
    let allowed: Vec<u16> = cycle_table::EFFECT_CYCLES.iter().map(|(u, _)| *u).collect();
    f(&PedalLimits {
        allowed_uids: &allowed,
        cycle_table: cycle_table::EFFECT_CYCLES,
        sdram_table: cycle_table::EFFECT_SDRAM,
        cycle_budget: cycle_table::CYCLE_BUDGET_PER_BLOCK,
        chain_fraction: cycle_table::CHAIN_FRACTION,
        warn_fraction: cycle_table::WARN_FRACTION,
        node_overhead: cycle_table::NODE_OVERHEAD_CYCLES,
        sdram_budget: cycle_table::SDRAM_BUDGET_BYTES,
    })
}

/// What a rig of these effects costs the Daisy pedal, by the rule
/// [`validate_for_daisy`] applies: the editor's status bar shows it as you
/// build.
pub fn daisy_load(uids: impl IntoIterator<Item = u16>) -> PedalLoad {
    with_daisy_limits(|limits| limits.load(uids))
}

/// Validate `patch` for the Daisy pedal: the effects its firmware carries and
/// the one CPU and memory rule ([`sonido_platform::cycle_table`]) the pedal
/// itself enforces. The editor's export and `sonido pedal push` both check
/// rigs with this.
pub fn validate_for_daisy(patch: &Patch) -> Vec<Finding> {
    with_daisy_limits(|limits| validate_for_pedal(patch, limits))
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

    // CPU and memory.
    out.extend(
        limits
            .load(patch.nodes.iter().map(|n| n.effect_uid))
            .findings,
    );

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
            chain_fraction: 0.8,
            warn_fraction: 0.7,
            node_overhead: 400,
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
            chain_fraction: 0.8,
            warn_fraction: 0.7,
            node_overhead: 400,
            sdram_budget: 0,
        };
        let findings = validate_for_pedal(&p, &limits);
        assert!(has_errors(&findings));
    }

    #[test]
    fn between_the_chain_limit_and_the_budget_is_error() {
        // 259.2k of 320k is 81%: inside the block, but over the 80% rule.
        let p = two_node();
        let limits = PedalLimits {
            allowed_uids: &[1, 11],
            cycle_table: &[(1, 129_200), (11, 129_200)],
            sdram_table: &[],
            cycle_budget: 320_000,
            chain_fraction: 0.8,
            warn_fraction: 0.7,
            node_overhead: 400,
            sdram_budget: 0,
        };
        assert!(has_errors(&validate_for_pedal(&p, &limits)));
    }

    #[test]
    fn sdram_overflow_is_error() {
        let p = two_node();
        let limits = PedalLimits {
            allowed_uids: &[1, 11],
            cycle_table: &[(1, 1000), (11, 1000)],
            sdram_table: &[(1, 40 << 20), (11, 30 << 20)],
            cycle_budget: 320_000,
            chain_fraction: 0.8,
            warn_fraction: 0.7,
            node_overhead: 400,
            sdram_budget: 64 << 20,
        };
        assert!(has_errors(&validate_for_pedal(&p, &limits)));
    }

    #[test]
    fn load_counts_what_the_pedal_judges() {
        let limits = PedalLimits {
            allowed_uids: &[1, 11],
            cycle_table: &[(1, 100_000), (11, 70_000)],
            sdram_table: &[(11, 1 << 20)],
            cycle_budget: 320_000,
            chain_fraction: 0.8,
            warn_fraction: 0.7,
            node_overhead: 400,
            sdram_budget: 64 << 20,
        };
        let load = limits.load([1, 11]);
        assert_eq!((load.nodes, load.unavailable, load.sdram), (2, 0, 1 << 20));
        assert!((load.cpu - 170_800.0 / 320_000.0).abs() < 1e-6);
        assert_eq!(load.severity(), None);

        assert_eq!(limits.load([1, 11, 11]).severity(), Some(Severity::Warning));
        assert_eq!(limits.load([1, 99]).severity(), Some(Severity::Error));
        assert_eq!(
            limits
                .load(core::iter::repeat_n(11, MAX_NODES + 1))
                .severity(),
            Some(Severity::Error)
        );
    }

    #[test]
    fn daisy_load_matches_validate_for_daisy() {
        use sonido_platform::cycle_table::{CYCLE_BUDGET_PER_BLOCK, EFFECT_CYCLES, chain_cycles};
        let p = two_node();
        let load = daisy_load(p.nodes.iter().map(|n| n.effect_uid));
        let findings = validate_for_daisy(&p);
        for f in &load.findings {
            assert!(
                findings.contains(f),
                "{f:?} missing from validate_for_daisy"
            );
        }
        let heavy = EFFECT_CYCLES.iter().max_by_key(|(_, c)| *c).unwrap().0;
        let load = daisy_load([heavy, heavy]);
        let expected = chain_cycles([heavy, heavy]) as f32 / CYCLE_BUDGET_PER_BLOCK as f32;
        assert!((load.cpu - expected).abs() < 1e-6);
        assert_eq!(
            daisy_load([heavy; MAX_NODES]).severity(),
            Some(Severity::Error)
        );
    }

    #[test]
    fn near_budget_warns_but_allows() {
        let p = two_node();
        let limits = PedalLimits {
            allowed_uids: &[1, 11],
            cycle_table: &[(1, 130_000), (11, 110_000)], // 240k + overhead = 75% of 320k, under 80%
            sdram_table: &[],
            cycle_budget: 320_000,
            chain_fraction: 0.8,
            warn_fraction: 0.7,
            node_overhead: 400,
            sdram_budget: 0,
        };
        let findings = validate_for_pedal(&p, &limits);
        assert!(!has_errors(&findings));
        assert!(findings.iter().any(|f| f.severity == Severity::Warning));
    }
}
