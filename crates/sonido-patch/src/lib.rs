//! Canonical Sonido **patch** format.
//!
//! A [`Patch`] is the single source of truth for a complete effect rig:
//! the graph topology, per-node A/B parameter snapshots, six macro
//! definitions, the A/B morph configuration, and graph-level globals. One
//! model has three projections that all round-trip back to the same `Patch`:
//!
//! | Projection      | Where                          | Module          |
//! |-----------------|--------------------------------|-----------------|
//! | JSON            | GUI session, CLAP plugin state | `serde` derives |
//! | 4 KB binary     | QSPI flash sector on the pedal | [`binary`]      |
//! | runtime engine  | GUI / plugin / firmware         | consumer crates |
//!
//! # Identity, not position
//!
//! Effects are referenced by their **stable UID**
//! ([`sonido_registry::EFFECT_UIDS`](../sonido_registry/constant.EFFECT_UIDS.html)),
//! never by a list position — positional indices have already rotted once in
//! this codebase. A patch authored today still resolves after effects are
//! added or reordered.
//!
//! # no_std
//!
//! Builds `no_std + alloc`. The firmware enables only the binary [`decode`]
//! path; it never needs `serde`. Host crates enable the default `std` feature
//! for JSON and file helpers.

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate alloc;
#[cfg(not(feature = "std"))]
use alloc::{string::String, vec::Vec};

pub mod binary;
pub mod build;
pub mod runtime;
pub mod validate;

pub use binary::{PatchError, decode, encode};
pub use build::{PatchBuildError, build_graph_from_patch};
pub use runtime::PatchPlayer;
// Re-export the shared runtime enums so consumers can name everything through
// `sonido_patch::` without also depending on `sonido-core` directly.
pub use sonido_core::{GlobalParam, MacroTarget, MorphCurve, MorphMode};

// ---------------------------------------------------------------------------
// Format constants
// ---------------------------------------------------------------------------

/// Binary magic: `"SNDP"` little-endian. Distinct from the legacy `"SOND"`
/// daisy-preset magic so the firmware can tell the formats apart.
pub const PATCH_MAGIC: u32 = 0x534E_4450;

/// Wire-format version. Bumped only on a breaking layout change; additive
/// fields ride the reserved `flags` word instead.
pub const PATCH_FORMAT_VERSION: u16 = 1;

/// One patch occupies exactly one QSPI flash sector.
pub const SECTOR_SIZE: usize = 4096;

/// Maximum effect nodes in a patch. A structural cap; the real ceiling on the
/// pedal is the CPU budget, enforced at export time ([`validate`]).
pub const MAX_NODES: usize = 8;

/// Maximum parameters per effect node (current kernel max is 11).
pub const MAX_PARAMS: usize = 16;

/// Number of macros, one per hardware knob (K1–K6).
pub const NUM_MACROS: usize = 6;

/// Fixed macro-name length in the binary projection (UTF-8, NUL-padded).
pub const MACRO_NAME_LEN: usize = 16;

/// Fixed patch-name length in the binary projection (UTF-8, NUL-padded).
pub const PATCH_NAME_LEN: usize = 24;

// ---------------------------------------------------------------------------
// Patch model
// ---------------------------------------------------------------------------

/// A complete effect rig: topology, A/B parameter snapshots, macros, morph,
/// and globals.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Patch {
    /// Human-readable name (truncated to [`PATCH_NAME_LEN`] in binary).
    pub name: String,
    /// Wire-format version this patch targets.
    pub format_version: u16,
    /// Effect nodes, in slot order.
    pub nodes: Vec<PatchNode>,
    /// Connections between nodes and the virtual input/output/split/merge.
    pub edges: Vec<PatchEdge>,
    /// Six macro definitions (one per hardware knob). Empty `mappings` ⇒ knob
    /// is unused for this patch.
    pub macros: [MacroDef; NUM_MACROS],
    /// A/B morph configuration.
    pub morph: MorphConfig,
    /// Graph-level controls outside any effect slot.
    pub globals: GlobalControls,
}

/// One effect node: its identity, bypass, and A/B parameter snapshots.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PatchNode {
    /// Stable effect UID (see `sonido_registry::EFFECT_UIDS`). `0` = none.
    pub effect_uid: u16,
    /// Whether this node is bypassed.
    pub bypassed: bool,
    /// Parameter snapshot "A". Length is the node's param count (≤ [`MAX_PARAMS`]).
    pub params_a: Vec<f32>,
    /// Parameter snapshot "B" — same length as `params_a`. Equal to A means
    /// "no morph movement" for this node.
    pub params_b: Vec<f32>,
}

impl PatchNode {
    /// A node with both snapshots set to `params` (A == B, not bypassed).
    pub fn new(effect_uid: u16, params: Vec<f32>) -> Self {
        Self {
            effect_uid,
            bypassed: false,
            params_b: params.clone(),
            params_a: params,
        }
    }

    /// Parameter count (defined by the A snapshot).
    pub fn param_count(&self) -> usize {
        self.params_a.len()
    }
}

/// An endpoint a [`PatchEdge`] connects to.
///
/// `Node` indexes [`Patch::nodes`]. `Split`/`Merge` are virtual fan-out /
/// fan-in elements addressed by a small id, letting a patch express parallel
/// paths without a node entry for the junction itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PatchEndpoint {
    /// The graph's single audio input.
    Input,
    /// The graph's single audio output.
    Output,
    /// Effect node at this index in [`Patch::nodes`].
    Node(u8),
    /// A 1→N fan-out junction, addressed by id.
    Split(u8),
    /// An N→1 fan-in junction, addressed by id.
    Merge(u8),
}

/// A directed connection between two [`PatchEndpoint`]s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PatchEdge {
    /// Source endpoint.
    pub from: PatchEndpoint,
    /// Destination endpoint.
    pub to: PatchEndpoint,
}

impl PatchEdge {
    /// Construct an edge.
    pub fn new(from: PatchEndpoint, to: PatchEndpoint) -> Self {
        Self { from, to }
    }
}

/// One macro → parameter mapping with its scaled range and curve.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MacroMappingSpec {
    /// Where this mapping writes.
    pub target: MacroTarget,
    /// Target value when the macro knob is at 0.0.
    pub min: f32,
    /// Target value when the macro knob is at 1.0 (`max < min` inverts).
    pub max: f32,
    /// Interpolation curve across the knob's travel.
    pub curve: MorphCurve,
}

/// A named macro: a knob plus the parameters it drives.
#[derive(Clone, Debug, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MacroDef {
    /// Display name (truncated to [`MACRO_NAME_LEN`] in binary). Empty for an
    /// unnamed/unused macro.
    pub name: String,
    /// Targets this macro drives in lock-step.
    pub mappings: Vec<MacroMappingSpec>,
}

impl MacroDef {
    /// Whether this macro drives anything.
    pub fn is_active(&self) -> bool {
        !self.mappings.is_empty()
    }
}

/// A/B morph behavior and per-slot lock state.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MorphConfig {
    /// Ramp speed (units per control tick scale; consumer-defined).
    pub speed: f32,
    /// How a footswitch/gesture drives the morph position.
    pub mode: MorphMode,
    /// Bitfield: bit `i` set ⇒ node `i` is excluded from morphing.
    pub slot_locks: u8,
}

impl Default for MorphConfig {
    fn default() -> Self {
        Self {
            speed: 2.0,
            mode: MorphMode::Ramp,
            slot_locks: 0,
        }
    }
}

impl MorphConfig {
    /// Whether node `slot` is locked out of morphing.
    pub fn is_locked(&self, slot: usize) -> bool {
        slot < 8 && (self.slot_locks & (1 << slot)) != 0
    }

    /// Set node `slot`'s morph lock.
    pub fn set_locked(&mut self, slot: usize, locked: bool) {
        if slot < 8 {
            if locked {
                self.slot_locks |= 1 << slot;
            } else {
                self.slot_locks &= !(1 << slot);
            }
        }
    }
}

/// Graph-level controls that sit outside the effect chain.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct GlobalControls {
    /// Pre-graph input gain (dB).
    pub input_gain_db: f32,
    /// Post-graph master volume (dB).
    pub master_volume_db: f32,
}

impl Default for GlobalControls {
    fn default() -> Self {
        Self {
            input_gain_db: 0.0,
            master_volume_db: 0.0,
        }
    }
}

impl Patch {
    /// An empty patch (Input → Output, no effects) with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            format_version: PATCH_FORMAT_VERSION,
            nodes: Vec::new(),
            edges: Vec::new(),
            macros: core::array::from_fn(|_| MacroDef::default()),
            morph: MorphConfig::default(),
            globals: GlobalControls::default(),
        }
    }

    /// Build a simple linear chain `Input → n0 → n1 → … → Output` from nodes.
    ///
    /// A convenience for the common case; arbitrary topologies are expressed by
    /// constructing [`Patch::edges`] directly.
    pub fn linear_chain(name: impl Into<String>, nodes: Vec<PatchNode>) -> Self {
        let mut patch = Self::new(name);
        let n = nodes.len();
        patch.nodes = nodes;
        if n == 0 {
            patch
                .edges
                .push(PatchEdge::new(PatchEndpoint::Input, PatchEndpoint::Output));
        } else {
            patch
                .edges
                .push(PatchEdge::new(PatchEndpoint::Input, PatchEndpoint::Node(0)));
            for i in 0..n - 1 {
                patch.edges.push(PatchEdge::new(
                    PatchEndpoint::Node(i as u8),
                    PatchEndpoint::Node((i + 1) as u8),
                ));
            }
            patch.edges.push(PatchEdge::new(
                PatchEndpoint::Node((n - 1) as u8),
                PatchEndpoint::Output,
            ));
        }
        patch
    }

    /// Active macros (those with at least one mapping).
    pub fn active_macro_count(&self) -> usize {
        self.macros.iter().filter(|m| m.is_active()).count()
    }
}
