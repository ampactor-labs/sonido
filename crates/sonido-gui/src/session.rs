//! Session save/load for the Sonido graph editor.
//!
//! A session captures the complete editor state: graph topology, node
//! positions, parameter values (A and B snapshots), bypass states, macros,
//! morph configuration, and I/O gains. Sessions serialize to JSON.
//!
//! # Versioning & migration
//!
//! v2 adds the canonical macro/morph/B-snapshot fields. They are
//! `#[serde(default)]`, so a v1 file (which lacks them) loads transparently:
//! macros come up empty, morph defaults, and each effect's B snapshot mirrors
//! its A snapshot. No separate migration pass is needed.
//!
//! [`Session::to_patch`] projects a session into the canonical
//! [`sonido_patch::Patch`] used for export to a CLAP plugin or the pedal.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use sonido_patch::{
    GlobalControls, MacroDef, MorphConfig, Patch, PatchEdge, PatchEndpoint, PatchNode,
};
use sonido_registry::effect_uid;

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
    pub macros: [MacroDef; sonido_patch::NUM_MACROS],
    /// A/B morph configuration. Default in v1 sessions.
    #[serde(default)]
    pub morph: MorphConfig,
}

fn default_macros() -> [MacroDef; sonido_patch::NUM_MACROS] {
    core::array::from_fn(|_| MacroDef::default())
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
    pub const VERSION: u32 = 2;

    /// Save the session to a JSON file.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization or file I/O fails.
    pub fn save(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Load a session from a JSON file (v1 or v2).
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or the JSON is malformed.
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let json = std::fs::read_to_string(path)?;
        let session: Self = serde_json::from_str(&json)?;
        Ok(session)
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
            morph: MorphConfig::default(),
        }
    }

    #[test]
    fn session_roundtrip_json() {
        let session = sample_session();
        let json = serde_json::to_string(&session).unwrap();
        let restored: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.version, 2);
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
            morph: MorphConfig::default(),
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
}
