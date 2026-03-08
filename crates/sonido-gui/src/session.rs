//! Session save/load for the Sonido graph editor.
//!
//! A session captures the complete editor state: graph topology,
//! node positions, parameter values, bypass states, and I/O gains.
//! Sessions serialize to JSON.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Complete session state.
///
/// ## Fields
/// - `version`: Schema version for forward compatibility.
/// - `nodes`: Ordered list of graph nodes with positions.
/// - `wires`: Connections as `(from_idx, from_output, to_idx, to_input)` tuples.
/// - `params`: Per-effect parameter snapshots, keyed by node index.
/// - `input_gain`: Input gain in dB.
/// - `master_volume`: Master volume in dB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Schema version (currently 1).
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
    /// Parameter values in ParameterInfo order.
    pub params: Vec<f32>,
    /// Whether the effect is bypassed.
    pub bypassed: bool,
}

impl Session {
    /// Current schema version.
    pub const VERSION: u32 = 1;

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

    /// Load a session from a JSON file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or the JSON is malformed.
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let json = std::fs::read_to_string(path)?;
        let session: Self = serde_json::from_str(&json)?;
        Ok(session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_roundtrip_json() {
        let session = Session {
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
                        bypassed: false,
                    },
                );
                m
            },
            input_gain: 0.0,
            master_volume: -3.0,
        };

        let json = serde_json::to_string(&session).unwrap();
        let restored: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.version, 1);
        assert_eq!(restored.nodes.len(), 3);
        assert_eq!(restored.wires.len(), 2);
        assert_eq!(restored.master_volume, -3.0);
    }
}
