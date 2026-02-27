//! Graph topology DSL parser and builder.
//!
//! Extends the existing pipe-chain syntax with `split()` for parallel routing:
//!
//! ```text
//! # Linear chain (compatible with --chain)
//! preamp:gain=6 | distortion:drive=15 | reverb:mix=0.3
//!
//! # Parallel split with dry path
//! split(distortion:drive=20; -) | limiter
//!
//! # Nested splits
//! preamp | split(distortion | split(chorus; flanger); -) | limiter
//! ```
//!
//! ## Grammar
//!
//! ```text
//! graph       ::= path
//! path        ::= segment ( '|' segment )*
//! segment     ::= '-' | split_expr | effect_spec
//! effect_spec ::= name ( ':' key '=' value ( ',' key '=' value )* )?
//! split_expr  ::= 'split(' path ( ';' path )+ ')'
//! ```
//!
//! Two-phase design: parse → [`GraphSpec`], then build → [`ProcessingGraph`].
//! The parse phase is pure (no audio dependencies, fully testable).

use crate::effects::{EffectError, create_effect_with_params};
use sonido_core::graph::{GraphError, MAX_SPLIT_TARGETS, NodeId, ProcessingGraph};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// IR types
// ---------------------------------------------------------------------------

/// A node in the parsed graph specification.
#[derive(Debug, Clone, PartialEq)]
pub enum GraphNode {
    /// An audio effect with optional parameters.
    Effect {
        /// Effect name (e.g., `"distortion"`).
        name: String,
        /// Parameter overrides (e.g., `{"drive": "15"}`).
        params: HashMap<String, String>,
    },
    /// Dry passthrough — only valid inside a split path.
    Dry,
    /// Parallel split: each inner `Vec<GraphNode>` is a serial path.
    Split {
        /// Two or more parallel paths.
        paths: Vec<Vec<GraphNode>>,
    },
}

/// A parsed graph specification: a serial chain of [`GraphNode`]s.
pub type GraphSpec = Vec<GraphNode>;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors from DSL parsing, validation, or graph construction.
#[derive(Debug, thiserror::Error)]
pub enum DslError {
    /// Unexpected character during parsing.
    #[error("unexpected character '{ch}' at position {pos}")]
    UnexpectedChar {
        /// Byte position in the input string.
        pos: usize,
        /// The unexpected character.
        ch: char,
    },
    /// Missing closing parenthesis for `split(...)`.
    #[error("unclosed split at position {pos} (expected ')')")]
    UnclosedSplit {
        /// Position of the opening `split(`.
        pos: usize,
    },
    /// `split()` requires at least 2 semicolon-separated paths.
    #[error("split requires at least 2 paths (found {count})")]
    SplitTooFewPaths {
        /// Number of paths found.
        count: usize,
    },
    /// A split path is empty (e.g., `split(distortion; ; reverb)`).
    #[error("empty path in split at position {pos}")]
    EmptySplitPath {
        /// Position of the empty path.
        pos: usize,
    },
    /// Too many paths in a single split.
    #[error("split exceeds maximum {max} paths (found {count})")]
    SplitTooManyPaths {
        /// Number of paths found.
        count: usize,
        /// Maximum allowed.
        max: usize,
    },
    /// Dry passthrough `-` used outside a split context.
    #[error("dry passthrough '-' is only valid inside a split")]
    DryAtTopLevel,
    /// Parameter parsing error.
    #[error("parameter error at position {pos}: {message}")]
    ParamError {
        /// Byte position.
        pos: usize,
        /// Description.
        message: String,
    },
    /// Effect creation error (unknown effect, bad param value, etc.).
    #[error(transparent)]
    EffectError(#[from] EffectError),
    /// Graph construction error (cycle, invalid connection, etc.).
    #[error(transparent)]
    GraphError(#[from] GraphError),
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Recursive descent parser for the graph topology DSL.
///
/// LL(1), single byte lookahead. All input is ASCII.
struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.input.get(self.pos).map(|&b| b as char)
    }

    fn advance(&mut self) {
        self.pos += 1;
    }

    fn skip_ws(&mut self) {
        while self.pos < self.input.len() && self.input[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn starts_with(&self, s: &str) -> bool {
        self.input[self.pos..].starts_with(s.as_bytes())
    }

    /// Entry: parse the entire input as a graph spec.
    fn parse_graph(&mut self) -> Result<GraphSpec, DslError> {
        let spec = self.parse_path()?;
        self.skip_ws();
        if let Some(ch) = self.peek() {
            return Err(DslError::UnexpectedChar { pos: self.pos, ch });
        }
        Ok(spec)
    }

    /// `path ::= segment ( '|' segment )*`
    fn parse_path(&mut self) -> Result<Vec<GraphNode>, DslError> {
        let mut nodes = vec![self.parse_segment()?];
        loop {
            self.skip_ws();
            if self.peek() == Some('|') {
                self.advance();
                nodes.push(self.parse_segment()?);
            } else {
                break;
            }
        }
        Ok(nodes)
    }

    /// `segment ::= '-' | split_expr | effect_spec`
    fn parse_segment(&mut self) -> Result<GraphNode, DslError> {
        self.skip_ws();

        if self.starts_with("split(") {
            return self.parse_split();
        }

        // Dry passthrough: '-' followed by a terminator
        if self.peek() == Some('-') {
            let after = self.pos + 1;
            let next = self.next_non_ws(after);
            if matches!(next, None | Some('|' | ';' | ')')) {
                self.advance();
                return Ok(GraphNode::Dry);
            }
        }

        self.parse_effect()
    }

    /// `split_expr ::= 'split(' path ( ';' path )+ ')'`
    fn parse_split(&mut self) -> Result<GraphNode, DslError> {
        let open_pos = self.pos;
        self.pos += 6; // consume "split("

        let mut paths = vec![self.parse_path()?];

        loop {
            self.skip_ws();
            if self.peek() == Some(';') {
                self.advance();
                self.skip_ws();
                // Reject empty paths like `split(a; ; b)`
                if matches!(self.peek(), Some(';' | ')')) {
                    return Err(DslError::EmptySplitPath { pos: self.pos });
                }
                paths.push(self.parse_path()?);
            } else {
                break;
            }
        }

        self.skip_ws();
        if self.peek() != Some(')') {
            return Err(DslError::UnclosedSplit { pos: open_pos });
        }
        self.advance(); // consume ')'

        if paths.len() < 2 {
            return Err(DslError::SplitTooFewPaths { count: paths.len() });
        }
        if paths.len() > MAX_SPLIT_TARGETS {
            return Err(DslError::SplitTooManyPaths {
                count: paths.len(),
                max: MAX_SPLIT_TARGETS,
            });
        }

        Ok(GraphNode::Split { paths })
    }

    /// `effect_spec ::= name ( ':' key '=' value ( ',' key '=' value )* )?`
    fn parse_effect(&mut self) -> Result<GraphNode, DslError> {
        self.skip_ws();
        let start = self.pos;

        // Name: everything up to ':', '|', ';', ')', or end
        while let Some(ch) = self.peek() {
            if matches!(ch, ':' | '|' | ';' | ')') {
                break;
            }
            self.advance();
        }

        let name = std::str::from_utf8(&self.input[start..self.pos])
            .expect("DSL input should be valid UTF-8")
            .trim()
            .to_string();

        if name.is_empty() {
            return Err(DslError::EmptySplitPath { pos: start });
        }

        let mut params = HashMap::new();
        if self.peek() == Some(':') {
            self.advance(); // consume ':'
            self.parse_params(&mut params)?;
        }

        Ok(GraphNode::Effect { name, params })
    }

    /// `param_list ::= key '=' value ( ',' key '=' value )*`
    fn parse_params(&mut self, params: &mut HashMap<String, String>) -> Result<(), DslError> {
        loop {
            self.skip_ws();
            let key_start = self.pos;

            // Key: up to '='
            while let Some(ch) = self.peek() {
                if ch == '=' {
                    break;
                }
                // Structural terminators mean the colon started a malformed param
                if matches!(ch, '|' | ';' | ')' | ',') {
                    return Err(DslError::ParamError {
                        pos: key_start,
                        message: format!(
                            "expected '=' in parameter near '{}'",
                            std::str::from_utf8(&self.input[key_start..self.pos]).unwrap_or("?")
                        ),
                    });
                }
                self.advance();
            }

            if self.peek() != Some('=') {
                return Err(DslError::ParamError {
                    pos: key_start,
                    message: "expected 'key=value'".to_string(),
                });
            }

            let key = std::str::from_utf8(&self.input[key_start..self.pos])
                .expect("UTF-8")
                .trim()
                .to_string();
            self.advance(); // consume '='

            // Value: up to ',', '|', ';', ')', or end
            let val_start = self.pos;
            while let Some(ch) = self.peek() {
                if matches!(ch, ',' | '|' | ';' | ')') {
                    break;
                }
                self.advance();
            }

            let value = std::str::from_utf8(&self.input[val_start..self.pos])
                .expect("UTF-8")
                .trim()
                .to_string();

            params.insert(key, value);

            if self.peek() == Some(',') {
                self.advance();
            } else {
                break;
            }
        }

        Ok(())
    }

    /// Returns the first non-whitespace character at or after `from`.
    fn next_non_ws(&self, from: usize) -> Option<char> {
        let mut p = from;
        while p < self.input.len() && self.input[p].is_ascii_whitespace() {
            p += 1;
        }
        self.input.get(p).map(|&b| b as char)
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Parse a graph DSL string into a [`GraphSpec`].
///
/// Does not validate semantic rules (e.g., dry at top level). Call
/// [`validate_spec`] on the result before building.
///
/// # Errors
///
/// Returns [`DslError`] on syntax errors (unclosed splits, missing `=`, etc.).
pub fn parse_graph_dsl(input: &str) -> Result<GraphSpec, DslError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(DslError::ParamError {
            pos: 0,
            message: "empty graph specification".to_string(),
        });
    }
    let mut parser = Parser::new(input);
    parser.parse_graph()
}

/// Validate a parsed graph spec.
///
/// Rejects dry passthrough (`-`) at top level and empty split paths.
///
/// # Errors
///
/// Returns [`DslError::DryAtTopLevel`] if `-` appears outside a split.
pub fn validate_spec(spec: &GraphSpec) -> Result<(), DslError> {
    validate_nodes(spec, false)
}

fn validate_nodes(nodes: &[GraphNode], in_split: bool) -> Result<(), DslError> {
    for node in nodes {
        match node {
            GraphNode::Dry if !in_split => return Err(DslError::DryAtTopLevel),
            GraphNode::Dry => {}
            GraphNode::Effect { .. } => {}
            GraphNode::Split { paths } => {
                for path in paths {
                    if path.is_empty() {
                        return Err(DslError::EmptySplitPath { pos: 0 });
                    }
                    validate_nodes(path, true)?;
                }
            }
        }
    }
    Ok(())
}

/// Build a [`ProcessingGraph`] from a parsed and validated [`GraphSpec`].
///
/// Creates effects via the sonido-effects registry, connects them according
/// to the topology, and compiles the graph for audio processing.
///
/// # Errors
///
/// Returns [`DslError`] if an effect name is unknown, a parameter is invalid,
/// or the resulting graph is malformed.
pub fn build_graph(
    spec: &GraphSpec,
    sample_rate: f32,
    block_size: usize,
) -> Result<ProcessingGraph, DslError> {
    let mut graph = ProcessingGraph::new(sample_rate, block_size);
    let input = graph.add_input();
    let output = graph.add_output();

    let (entry, exit) = build_path(&mut graph, spec, sample_rate)?;
    graph.connect(input, entry)?;
    graph.connect(exit, output)?;

    graph.compile()?;
    Ok(graph)
}

/// Build a serial path, returning `(entry, exit)` node IDs.
///
/// Dry nodes in mixed paths (e.g., `- | reverb`) are skipped — the dry is
/// a no-op when other effects are present. Fully-dry paths are handled at
/// the split level via direct `connect(split, merge)`.
fn build_path(
    graph: &mut ProcessingGraph,
    nodes: &[GraphNode],
    sample_rate: f32,
) -> Result<(NodeId, NodeId), DslError> {
    let mut segments: Vec<(NodeId, NodeId)> = Vec::with_capacity(nodes.len());

    for node in nodes {
        if matches!(node, GraphNode::Dry) {
            continue; // skip dry in paths with other effects
        }
        let (entry, exit) = build_segment(graph, node, sample_rate)?;
        segments.push((entry, exit));
    }

    // If we get here with no segments, the path was all-dry, which should have
    // been handled by build_split. Panic indicates a logic error.
    assert!(
        !segments.is_empty(),
        "all-dry paths should be handled by build_split"
    );

    // Wire segments in series
    for i in 1..segments.len() {
        graph.connect(segments[i - 1].1, segments[i].0)?;
    }

    Ok((segments[0].0, segments[segments.len() - 1].1))
}

/// Build a single segment, returning `(entry, exit)` node IDs.
fn build_segment(
    graph: &mut ProcessingGraph,
    node: &GraphNode,
    sample_rate: f32,
) -> Result<(NodeId, NodeId), DslError> {
    match node {
        GraphNode::Effect { name, params } => {
            let effect = create_effect_with_params(name, sample_rate, params)?;
            let id = graph.add_effect(effect);
            Ok((id, id))
        }
        GraphNode::Dry => {
            // Dry nodes are filtered by build_path or handled by build_split.
            unreachable!("Dry nodes are handled by build_path / build_split")
        }
        GraphNode::Split { paths } => build_split(graph, paths, sample_rate),
    }
}

/// Build a split/merge topology, returning `(split_node, merge_node)`.
///
/// Dry-only paths get a direct `connect(split, merge)` — no intermediate
/// node. Other paths are built as serial chains wired between the split
/// and merge nodes.
fn build_split(
    graph: &mut ProcessingGraph,
    paths: &[Vec<GraphNode>],
    sample_rate: f32,
) -> Result<(NodeId, NodeId), DslError> {
    let split = graph.add_split();
    let merge = graph.add_merge();

    for path in paths {
        let all_dry = path.iter().all(|n| matches!(n, GraphNode::Dry));
        if all_dry {
            graph.connect(split, merge)?;
        } else {
            let (entry, exit) = build_path(graph, path, sample_rate)?;
            graph.connect(split, entry)?;
            graph.connect(exit, merge)?;
        }
    }

    Ok((split, merge))
}

// ---------------------------------------------------------------------------
// Slug generation
// ---------------------------------------------------------------------------

/// Build a filename-safe slug from a graph DSL string.
///
/// Encoding:
/// - `|` → `+` (chain separator)
/// - `split(...)` → `S[...]`
/// - `;` → `+` within split (path separator)
/// - `:` → `_`, `,` → `_` (param separators)
/// - `-` → `-` (dry passthrough)
///
/// Falls back to basic string sanitization if parsing fails.
pub fn build_graph_slug(spec: &str) -> String {
    match parse_graph_dsl(spec) {
        Ok(nodes) => slug_from_nodes(&nodes),
        Err(_) => spec.replace('|', "+").replace([':', ','], "_"),
    }
}

fn slug_from_nodes(nodes: &[GraphNode]) -> String {
    let parts: Vec<String> = nodes.iter().map(slug_from_node).collect();
    parts.join("+")
}

fn slug_from_node(node: &GraphNode) -> String {
    match node {
        GraphNode::Effect { name, params } => {
            if params.is_empty() {
                name.clone()
            } else {
                let mut s = name.clone();
                let mut sorted: Vec<_> = params.iter().collect();
                sorted.sort_by_key(|(k, _)| k.as_str());
                for (k, v) in sorted {
                    s.push('_');
                    s.push_str(k);
                    s.push('=');
                    s.push_str(v);
                }
                s
            }
        }
        GraphNode::Dry => "-".to_string(),
        GraphNode::Split { paths } => {
            let inner: Vec<String> = paths.iter().map(|p| slug_from_nodes(p)).collect();
            format!("S[{}]", inner.join("+"))
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // --- Parser tests (no audio) ---

    #[test]
    fn parse_single_effect() {
        let spec = parse_graph_dsl("reverb").unwrap();
        assert_eq!(spec.len(), 1);
        assert_eq!(
            spec[0],
            GraphNode::Effect {
                name: "reverb".into(),
                params: HashMap::new()
            }
        );
    }

    #[test]
    fn parse_single_effect_with_params() {
        let spec = parse_graph_dsl("reverb:decay=0.8,mix=0.3").unwrap();
        assert_eq!(spec.len(), 1);
        let expected_params: HashMap<String, String> = [("decay", "0.8"), ("mix", "0.3")]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        assert_eq!(
            spec[0],
            GraphNode::Effect {
                name: "reverb".into(),
                params: expected_params
            }
        );
    }

    #[test]
    fn parse_linear_chain() {
        let spec = parse_graph_dsl("preamp:gain=6 | distortion:drive=15 | reverb:mix=0.3").unwrap();
        assert_eq!(spec.len(), 3);
        assert!(matches!(&spec[0], GraphNode::Effect { name, .. } if name == "preamp"));
        assert!(matches!(&spec[1], GraphNode::Effect { name, .. } if name == "distortion"));
        assert!(matches!(&spec[2], GraphNode::Effect { name, .. } if name == "reverb"));
    }

    #[test]
    fn parse_simple_split_with_dry() {
        let spec = parse_graph_dsl("split(distortion:drive=20; -)").unwrap();
        assert_eq!(spec.len(), 1);
        if let GraphNode::Split { paths } = &spec[0] {
            assert_eq!(paths.len(), 2);
            assert!(matches!(&paths[0][0], GraphNode::Effect { name, .. } if name == "distortion"));
            assert_eq!(paths[1][0], GraphNode::Dry);
        } else {
            panic!("expected Split node");
        }
    }

    #[test]
    fn parse_split_then_effect() {
        let spec = parse_graph_dsl("split(distortion; -) | limiter").unwrap();
        assert_eq!(spec.len(), 2);
        assert!(matches!(&spec[0], GraphNode::Split { .. }));
        assert!(matches!(&spec[1], GraphNode::Effect { name, .. } if name == "limiter"));
    }

    #[test]
    fn parse_chains_inside_split() {
        let spec = parse_graph_dsl("split(distortion | chorus; reverb:mix=1.0)").unwrap();
        if let GraphNode::Split { paths } = &spec[0] {
            assert_eq!(paths.len(), 2);
            assert_eq!(paths[0].len(), 2); // distortion | chorus
            assert_eq!(paths[1].len(), 1); // reverb
        } else {
            panic!("expected Split");
        }
    }

    #[test]
    fn parse_nested_split() {
        let spec = parse_graph_dsl("split(split(distortion; chorus); reverb)").unwrap();
        if let GraphNode::Split { paths } = &spec[0] {
            assert_eq!(paths.len(), 2);
            assert!(matches!(&paths[0][0], GraphNode::Split { .. }));
            assert!(matches!(&paths[1][0], GraphNode::Effect { name, .. } if name == "reverb"));
        } else {
            panic!("expected Split");
        }
    }

    #[test]
    fn parse_three_way_split() {
        let spec = parse_graph_dsl("split(distortion; chorus; reverb)").unwrap();
        if let GraphNode::Split { paths } = &spec[0] {
            assert_eq!(paths.len(), 3);
        } else {
            panic!("expected Split");
        }
    }

    #[test]
    fn parse_whitespace_handling() {
        let spec = parse_graph_dsl("  preamp  |  split( distortion ; - )  |  reverb  ").unwrap();
        assert_eq!(spec.len(), 3);
        assert!(matches!(&spec[0], GraphNode::Effect { name, .. } if name == "preamp"));
        assert!(matches!(&spec[1], GraphNode::Split { .. }));
        assert!(matches!(&spec[2], GraphNode::Effect { name, .. } if name == "reverb"));
    }

    #[test]
    fn parse_negative_param_value() {
        let spec = parse_graph_dsl("limiter:threshold=-12,ceiling=-0.5").unwrap();
        if let GraphNode::Effect { params, .. } = &spec[0] {
            assert_eq!(params.get("threshold").unwrap(), "-12");
            assert_eq!(params.get("ceiling").unwrap(), "-0.5");
        } else {
            panic!("expected Effect");
        }
    }

    // --- Validation error tests ---

    #[test]
    fn validate_dry_at_top_level() {
        let spec = parse_graph_dsl("-").unwrap();
        assert!(matches!(validate_spec(&spec), Err(DslError::DryAtTopLevel)));
    }

    #[test]
    fn validate_dry_in_chain_at_top_level() {
        let spec = parse_graph_dsl("distortion | - | reverb").unwrap();
        assert!(matches!(validate_spec(&spec), Err(DslError::DryAtTopLevel)));
    }

    #[test]
    fn validate_dry_inside_split_ok() {
        let spec = parse_graph_dsl("split(distortion; -)").unwrap();
        assert!(validate_spec(&spec).is_ok());
    }

    #[test]
    fn error_unclosed_split() {
        let err = parse_graph_dsl("split(distortion; reverb").unwrap_err();
        assert!(matches!(err, DslError::UnclosedSplit { .. }));
    }

    #[test]
    fn error_single_path_split() {
        let err = parse_graph_dsl("split(distortion)").unwrap_err();
        assert!(matches!(err, DslError::SplitTooFewPaths { count: 1 }));
    }

    #[test]
    fn error_empty_path_in_split() {
        let err = parse_graph_dsl("split(distortion; ; reverb)").unwrap_err();
        assert!(matches!(err, DslError::EmptySplitPath { .. }));
    }

    #[test]
    fn error_empty_input() {
        let err = parse_graph_dsl("").unwrap_err();
        assert!(matches!(err, DslError::ParamError { .. }));
    }

    // --- Build tests (with real effects) ---

    #[test]
    fn build_linear_chain() {
        let spec = parse_graph_dsl("distortion:drive=15 | reverb:mix=0.3").unwrap();
        validate_spec(&spec).unwrap();
        let graph = build_graph(&spec, 48000.0, 256).unwrap();
        // Input + 2 effects + Output = 4 nodes
        assert_eq!(graph.node_count(), 4);
    }

    #[test]
    fn build_single_effect() {
        let spec = parse_graph_dsl("reverb:decay=0.8").unwrap();
        validate_spec(&spec).unwrap();
        let graph = build_graph(&spec, 48000.0, 256).unwrap();
        assert_eq!(graph.node_count(), 3); // Input + Effect + Output
    }

    #[test]
    fn build_split_with_dry() {
        let spec = parse_graph_dsl("split(distortion:drive=20; -)").unwrap();
        validate_spec(&spec).unwrap();
        let graph = build_graph(&spec, 48000.0, 256).unwrap();
        // Input + Split + Distortion + Merge + Output = 5
        assert_eq!(graph.node_count(), 5);
    }

    #[test]
    fn build_split_with_chains() {
        let spec =
            parse_graph_dsl("split(distortion:drive=20 | chorus; phaser | flanger) | reverb")
                .unwrap();
        validate_spec(&spec).unwrap();
        let graph = build_graph(&spec, 48000.0, 256).unwrap();
        // Input + Split + Dist + Chorus + Phaser + Flanger + Merge + Reverb + Output = 9
        assert_eq!(graph.node_count(), 9);
    }

    #[test]
    fn build_nested_split() {
        let spec =
            parse_graph_dsl("split(split(distortion; chorus); reverb:mix=1.0) | limiter").unwrap();
        validate_spec(&spec).unwrap();
        let graph = build_graph(&spec, 48000.0, 256).unwrap();
        // Input + OuterSplit + InnerSplit + Dist + Chorus + InnerMerge + Reverb + OuterMerge + Limiter + Output = 10
        assert_eq!(graph.node_count(), 10);
    }

    #[test]
    fn build_graph_processes_audio() {
        let spec = parse_graph_dsl("split(distortion:drive=15; -) | limiter").unwrap();
        validate_spec(&spec).unwrap();
        let mut graph = build_graph(&spec, 48000.0, 256).unwrap();

        let left_in = vec![0.1_f32; 256];
        let right_in = vec![0.1_f32; 256];
        let mut left_out = vec![0.0_f32; 256];
        let mut right_out = vec![0.0_f32; 256];

        // Should not panic
        graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);

        // Output should be non-zero (distortion + dry path summed, then limited)
        let energy: f32 = left_out.iter().map(|s| s * s).sum();
        assert!(energy > 0.0, "output should contain signal");
    }

    // --- Slug tests ---

    #[test]
    fn slug_single_effect() {
        assert_eq!(build_graph_slug("reverb"), "reverb");
    }

    #[test]
    fn slug_linear_chain() {
        assert_eq!(
            build_graph_slug("preamp:gain=6 | reverb"),
            "preamp_gain=6+reverb"
        );
    }

    #[test]
    fn slug_split_with_dry() {
        assert_eq!(
            build_graph_slug("preamp | split(delay:time=300; -) | reverb"),
            "preamp+S[delay_time=300+-]+reverb"
        );
    }

    #[test]
    fn slug_nested_split() {
        assert_eq!(
            build_graph_slug("split(split(distortion; chorus); reverb)"),
            "S[S[distortion+chorus]+reverb]"
        );
    }

    #[test]
    fn slug_params_sorted() {
        // Params appear in alphabetical key order regardless of input order
        let slug = build_graph_slug("reverb:mix=0.3,decay=0.8");
        assert_eq!(slug, "reverb_decay=0.8_mix=0.3");
    }
}
