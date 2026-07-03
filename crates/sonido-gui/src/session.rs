//! Session save/load for the Sonido graph editor.
//!
//! A session captures the complete editor state: graph topology, node
//! positions, parameter values (A and B snapshots), bypass states, macros,
//! morph configuration, and I/O gains. Sessions serialize to JSON.
//!
//! # Versioning & migration
//!
//! v2 added the canonical macro/morph/B-snapshot fields and v3 adds the live
//! `macro_positions` / `morph_position` knob state. Every added field is
//! `#[serde(default)]`, so an older file loads transparently: macros come up
//! empty, morph defaults, knob positions zero, and each effect's B snapshot
//! mirrors its A snapshot. No separate migration pass is needed.
//!
//! [`Session::to_patch`] projects a session into the canonical
//! [`sonido_patch::Patch`] used for export to a CLAP plugin or the pedal.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use sonido_core::{MacroMap, MacroMapping};
use sonido_patch::{
    GlobalControls, MacroDef, MacroMappingSpec, MorphConfig, NUM_MACROS, Patch, PatchEdge,
    PatchEndpoint, PatchNode,
};
use sonido_registry::effect_uid;

use crate::morph_state::MorphSnapshot;

/// The macro + A/B-morph performance layer captured alongside the graph.
///
/// Bundles everything [`GraphView::capture_session`](crate::graph_view::GraphView::capture_session)
/// needs to persist beyond raw topology: the six macro definitions and their
/// live knob positions, the morph behaviour/locks, the current crossfade
/// position, and the two morph snapshots whose per-slot values become each
/// effect's A/B parameter sets.
pub struct PerformanceCapture {
    /// Six macro definitions (name + mappings) in knob order.
    pub macros: [MacroDef; NUM_MACROS],
    /// Live macro knob positions (0.0–1.0), restored on load.
    pub macro_positions: [f32; NUM_MACROS],
    /// Morph behaviour: speed, mode, and per-slot lock bitfield.
    pub morph: MorphConfig,
    /// Current A→B crossfade position (0.0–1.0).
    pub morph_position: f32,
    /// Pose A in slot order — its values become each effect's `params`.
    /// `None` ⇒ fall back to the live bridge values.
    pub morph_a: Option<MorphSnapshot>,
    /// Pose B in slot order — its values become each effect's `params_b`.
    /// `None` ⇒ leave `params_b` empty (B mirrors A).
    pub morph_b: Option<MorphSnapshot>,
}

/// Project the GUI's runtime [`MacroMap`] + display names into the persisted
/// `[MacroDef; NUM_MACROS]` form (one entry per knob, in knob order).
pub fn macro_map_to_defs(
    map: &MacroMap<NUM_MACROS>,
    names: &[String; NUM_MACROS],
) -> [MacroDef; NUM_MACROS] {
    core::array::from_fn(|i| MacroDef {
        name: names[i].clone(),
        mappings: map
            .mappings()
            .iter()
            .filter(|m| m.macro_index == i)
            .map(|m| MacroMappingSpec {
                target: m.target,
                min: m.min,
                max: m.max,
                curve: m.curve,
            })
            .collect(),
    })
}

/// Rebuild a runtime [`MacroMap`] and the per-knob display names from persisted
/// defs — the inverse of [`macro_map_to_defs`]. Knob positions are restored
/// separately by the caller.
pub fn defs_to_macro_map(
    defs: &[MacroDef; NUM_MACROS],
) -> (MacroMap<NUM_MACROS>, [String; NUM_MACROS]) {
    let mut map = MacroMap::new();
    for (i, def) in defs.iter().enumerate() {
        for spec in &def.mappings {
            map.add_mapping(MacroMapping {
                macro_index: i,
                target: spec.target,
                min: spec.min,
                max: spec.max,
                curve: spec.curve,
            });
        }
    }
    let names = core::array::from_fn(|i| defs[i].name.clone());
    (map, names)
}

/// Complete session state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Schema version (currently 2).
    pub version: u32,
    /// Ordered list of graph nodes with positions.
    pub nodes: Vec<SessionNodeEntry>,
    /// Wire connections: `(from_node_idx, from_output, to_node_idx, to_input)`.
    pub wires: Vec<(usize, usize, usize, usize)>,
    /// Per-effect parameter snapshots, keyed by node index.
    pub params: HashMap<usize, EffectState>,
    /// Input gain in dB.
    pub input_gain: f32,
    /// Master volume in dB.
    pub master_volume: f32,
    /// Six macro definitions (one per hardware knob). Empty in v1 sessions.
    #[serde(default = "default_macros")]
    pub macros: [MacroDef; NUM_MACROS],
    /// Live macro knob positions (0.0–1.0). Absent in v1/v2 sessions ⇒ all 0.0.
    #[serde(default = "default_macro_positions")]
    pub macro_positions: [f32; NUM_MACROS],
    /// A/B morph configuration. Default in v1 sessions.
    #[serde(default)]
    pub morph: MorphConfig,
    /// Current A→B crossfade position (0.0–1.0). Absent in v1/v2 ⇒ 0.0 (full A).
    #[serde(default)]
    pub morph_position: f32,
}

fn default_macros() -> [MacroDef; NUM_MACROS] {
    core::array::from_fn(|_| MacroDef::default())
}

fn default_macro_positions() -> [f32; NUM_MACROS] {
    [0.0; NUM_MACROS]
}

/// A node entry with type and 2D position.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionNodeEntry {
    /// The node type.
    pub node: SessionNode,
    /// Position `[x, y]` in the graph editor canvas.
    pub pos: [f32; 2],
}

/// Serializable node type (no `&'static str` or `ParamDescriptor`).
///
/// Maps 1:1 to [`SonidoNode`](crate::graph_view::SonidoNode) but uses
/// owned strings and omits runtime-only metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SessionNode {
    /// Audio input source.
    Input,
    /// Audio output sink.
    Output,
    /// An audio effect identified by registry ID.
    Effect {
        /// Registry identifier (e.g., `"distortion"`, `"reverb"`).
        effect_id: String,
    },
    /// Signal splitter.
    Split,
    /// Signal merger.
    Merge,
}

/// Parameter state snapshot for a single effect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectState {
    /// Registry identifier for the effect.
    pub effect_id: String,
    /// Parameter values (A snapshot) in `ParameterInfo` order.
    pub params: Vec<f32>,
    /// Parameter values (B snapshot) for A/B morphing. Empty in v1 sessions,
    /// in which case B mirrors A.
    #[serde(default)]
    pub params_b: Vec<f32>,
    /// Whether the effect is bypassed.
    pub bypassed: bool,
}

impl EffectState {
    /// The B snapshot, falling back to A when none was stored (v1 sessions).
    pub fn snapshot_b(&self) -> &[f32] {
        if self.params_b.is_empty() {
            &self.params
        } else {
            &self.params_b
        }
    }
}

impl Session {
    /// Current schema version.
    ///
    /// v3 adds `macro_positions` and `morph_position` (live knob/crossfade
    /// state). Both are `#[serde(default)]`, so v1/v2 files load unchanged.
    pub const VERSION: u32 = 3;

    /// Serialize to pretty JSON, without touching the filesystem.
    ///
    /// The web build has no file paths — it downloads this string — so both the
    /// native [`save`](Self::save) and the browser download share it.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Parse a session from a JSON string (v1, v2, or v3), without the
    /// filesystem. Shared by native [`load`](Self::load) and the browser upload.
    ///
    /// # Errors
    ///
    /// Returns an error if the JSON is malformed.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Save the session to a JSON file.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization or file I/O fails.
    pub fn save(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        std::fs::write(path, self.to_json()?)?;
        Ok(())
    }

    /// Load a session from a JSON file (v1, v2, or v3).
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or the JSON is malformed.
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let json = std::fs::read_to_string(path)?;
        Ok(Self::from_json(&json)?)
    }

    /// Project this session into the canonical [`Patch`].
    ///
    /// Effect nodes become [`PatchNode`]s (in appearance order, with A/B
    /// snapshots); Input/Output/Split/Merge nodes become endpoint kinds; wires
    /// become [`PatchEdge`]s. Unknown effect IDs resolve to UID `0`, which the
    /// patch validator flags — the GUI surfaces that at export time rather than
    /// silently dropping the node.
    pub fn to_patch(&self, name: impl Into<String>) -> Patch {
        // First pass: assign each visual node an endpoint identity.
        let mut endpoint = vec![PatchEndpoint::Input; self.nodes.len()];
        let mut nodes: Vec<PatchNode> = Vec::new();
        let mut next_split: u8 = 0;
        let mut next_merge: u8 = 0;

        for (idx, entry) in self.nodes.iter().enumerate() {
            match &entry.node {
                SessionNode::Input => endpoint[idx] = PatchEndpoint::Input,
                SessionNode::Output => endpoint[idx] = PatchEndpoint::Output,
                SessionNode::Split => {
                    endpoint[idx] = PatchEndpoint::Split(next_split);
                    next_split = next_split.wrapping_add(1);
                }
                SessionNode::Merge => {
                    endpoint[idx] = PatchEndpoint::Merge(next_merge);
                    next_merge = next_merge.wrapping_add(1);
                }
                SessionNode::Effect { effect_id } => {
                    let patch_idx = nodes.len() as u8;
                    endpoint[idx] = PatchEndpoint::Node(patch_idx);
                    let uid = effect_uid(effect_id).unwrap_or(0);
                    let (params_a, params_b, bypassed) = match self.params.get(&idx) {
                        Some(state) => (
                            state.params.clone(),
                            state.snapshot_b().to_vec(),
                            state.bypassed,
                        ),
                        None => (Vec::new(), Vec::new(), false),
                    };
                    nodes.push(PatchNode {
                        effect_uid: uid,
                        bypassed,
                        params_a,
                        params_b,
                    });
                }
            }
        }

        // Second pass: wires → edges, by visual-node identity.
        let mut edges = Vec::with_capacity(self.wires.len());
        for &(from_idx, _, to_idx, _) in &self.wires {
            if let (Some(&from), Some(&to)) = (endpoint.get(from_idx), endpoint.get(to_idx)) {
                edges.push(PatchEdge::new(from, to));
            }
        }

        Patch {
            name: name.into(),
            format_version: sonido_patch::PATCH_FORMAT_VERSION,
            nodes,
            edges,
            macros: self.macros.clone(),
            morph: self.morph,
            globals: GlobalControls {
                input_gain_db: self.input_gain,
                master_volume_db: self.master_volume,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_session() -> Session {
        Session {
            version: Session::VERSION,
            nodes: vec![
                SessionNodeEntry {
                    node: SessionNode::Input,
                    pos: [100.0, 200.0],
                },
                SessionNodeEntry {
                    node: SessionNode::Effect {
                        effect_id: "reverb".into(),
                    },
                    pos: [300.0, 200.0],
                },
                SessionNodeEntry {
                    node: SessionNode::Output,
                    pos: [500.0, 200.0],
                },
            ],
            wires: vec![(0, 0, 1, 0), (1, 0, 2, 0)],
            params: {
                let mut m = HashMap::new();
                m.insert(
                    1,
                    EffectState {
                        effect_id: "reverb".into(),
                        params: vec![0.5, 0.7, 0.3],
                        params_b: vec![0.9, 0.2, 0.8],
                        bypassed: false,
                    },
                );
                m
            },
            input_gain: 0.0,
            master_volume: -3.0,
            macros: default_macros(),
            macro_positions: default_macro_positions(),
            morph: MorphConfig::default(),
            morph_position: 0.0,
        }
    }

    #[test]
    fn session_roundtrip_json() {
        let session = sample_session();
        let json = serde_json::to_string(&session).unwrap();
        let restored: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.version, 3);
        assert_eq!(restored.nodes.len(), 3);
        assert_eq!(restored.wires.len(), 2);
        assert_eq!(restored.master_volume, -3.0);
    }

    #[test]
    fn v1_session_migrates_transparently() {
        // A v1 JSON has no `macros`, `morph`, or per-effect `params_b`.
        let v1 = r#"{
            "version": 1,
            "nodes": [
                {"node": "Input", "pos": [0.0, 0.0]},
                {"node": {"Effect": {"effect_id": "distortion"}}, "pos": [1.0, 0.0]},
                {"node": "Output", "pos": [2.0, 0.0]}
            ],
            "wires": [[0, 0, 1, 0], [1, 0, 2, 0]],
            "params": {"1": {"effect_id": "distortion", "params": [8.0, 0.0, 0.0], "bypassed": false}},
            "input_gain": 0.0,
            "master_volume": 0.0
        }"#;
        let s: Session = serde_json::from_str(v1).unwrap();
        assert_eq!(s.macros.iter().filter(|m| m.is_active()).count(), 0);
        assert_eq!(s.morph, MorphConfig::default());
        // B mirrors A when no B snapshot was stored.
        let state = s.params.get(&1).unwrap();
        assert_eq!(state.snapshot_b(), &[8.0, 0.0, 0.0]);
    }

    #[test]
    fn to_patch_builds_canonical() {
        let patch = sample_session().to_patch("My Rig");
        assert_eq!(patch.name, "My Rig");
        assert_eq!(patch.nodes.len(), 1);
        assert_eq!(patch.nodes[0].effect_uid, effect_uid("reverb").unwrap());
        assert_eq!(patch.nodes[0].params_a, vec![0.5, 0.7, 0.3]);
        assert_eq!(patch.nodes[0].params_b, vec![0.9, 0.2, 0.8]);
        // Input -> Node(0) -> Output.
        assert_eq!(patch.edges.len(), 2);
        assert!(matches!(patch.edges[0].from, PatchEndpoint::Input));
        assert!(matches!(patch.edges[0].to, PatchEndpoint::Node(0)));
        assert!(matches!(patch.edges[1].to, PatchEndpoint::Output));
        assert_eq!(patch.globals.master_volume_db, -3.0);
    }

    #[test]
    fn to_patch_maps_split_merge_ids() {
        let session = Session {
            version: Session::VERSION,
            nodes: vec![
                SessionNodeEntry {
                    node: SessionNode::Input,
                    pos: [0.0; 2],
                },
                SessionNodeEntry {
                    node: SessionNode::Split,
                    pos: [0.0; 2],
                },
                SessionNodeEntry {
                    node: SessionNode::Effect {
                        effect_id: "delay".into(),
                    },
                    pos: [0.0; 2],
                },
                SessionNodeEntry {
                    node: SessionNode::Effect {
                        effect_id: "reverb".into(),
                    },
                    pos: [0.0; 2],
                },
                SessionNodeEntry {
                    node: SessionNode::Merge,
                    pos: [0.0; 2],
                },
                SessionNodeEntry {
                    node: SessionNode::Output,
                    pos: [0.0; 2],
                },
            ],
            wires: vec![
                (0, 0, 1, 0),
                (1, 0, 2, 0),
                (1, 1, 3, 0),
                (2, 0, 4, 0),
                (3, 0, 4, 1),
                (4, 0, 5, 0),
            ],
            params: HashMap::new(),
            input_gain: 0.0,
            master_volume: 0.0,
            macros: default_macros(),
            macro_positions: default_macro_positions(),
            morph: MorphConfig::default(),
            morph_position: 0.0,
        };
        let patch = session.to_patch("parallel");
        assert_eq!(patch.nodes.len(), 2);
        assert!(
            patch
                .edges
                .iter()
                .any(|e| matches!(e.to, PatchEndpoint::Split(0)))
        );
        assert!(
            patch
                .edges
                .iter()
                .any(|e| matches!(e.to, PatchEndpoint::Merge(0)))
        );
    }

    #[test]
    fn macro_map_roundtrips_through_defs() {
        use sonido_core::{GlobalParam, MacroTarget, MorphCurve};

        let mut map: MacroMap<NUM_MACROS> = MacroMap::new();
        // K1 drives two targets; K3 drives a global, inverted + log.
        map.add_mapping(MacroMapping::linear(
            0,
            MacroTarget::Slot { slot: 0, param: 2 },
            0.0,
            10.0,
        ));
        map.add_mapping(MacroMapping {
            macro_index: 0,
            target: MacroTarget::Slot { slot: 1, param: 0 },
            min: 100.0,
            max: 8000.0,
            curve: MorphCurve::Logarithmic,
        });
        map.add_mapping(MacroMapping {
            macro_index: 2,
            target: MacroTarget::Global(GlobalParam::MasterVolume),
            min: 6.0,
            max: -40.0,
            curve: MorphCurve::Linear,
        });

        let mut names: [String; NUM_MACROS] = core::array::from_fn(|_| String::new());
        names[0] = "Grit".into();
        names[2] = "Volume".into();

        let defs = macro_map_to_defs(&map, &names);
        assert_eq!(defs[0].name, "Grit");
        assert_eq!(defs[0].mappings.len(), 2);
        assert_eq!(defs[2].mappings.len(), 1);
        assert!(!defs[1].is_active());

        let (back, back_names) = defs_to_macro_map(&defs);
        assert_eq!(back_names, names);
        assert_eq!(back.mappings().len(), 3);
        // Every original mapping survives with identical target/range/curve.
        for original in map.mappings() {
            assert!(
                back.mappings()
                    .iter()
                    .any(|m| m.macro_index == original.macro_index
                        && m.target == original.target
                        && m.min == original.min
                        && m.max == original.max
                        && m.curve == original.curve),
                "mapping {original:?} lost in roundtrip"
            );
        }
    }

    #[test]
    fn session_json_carries_macros_and_morph() {
        use sonido_core::{MacroTarget, MorphCurve};

        let mut session = sample_session();
        // Author a macro, set positions, lock a slot, set a crossfade.
        session.macros[1] = MacroDef {
            name: "Sweep".into(),
            mappings: vec![MacroMappingSpec {
                target: MacroTarget::Slot { slot: 0, param: 1 },
                min: 0.0,
                max: 1.0,
                curve: MorphCurve::Linear,
            }],
        };
        session.macro_positions[1] = 0.42;
        session.morph.set_locked(0, true);
        session.morph_position = 0.65;

        let json = serde_json::to_string(&session).unwrap();
        let restored: Session = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.macros[1].name, "Sweep");
        assert_eq!(restored.macros[1].mappings.len(), 1);
        assert!((restored.macro_positions[1] - 0.42).abs() < 1e-6);
        assert!(restored.morph.is_locked(0));
        assert!((restored.morph_position - 0.65).abs() < 1e-6);
    }
}
