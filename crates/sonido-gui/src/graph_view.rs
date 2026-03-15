//! Visual node-graph editor for DAG-based audio routing.
//!
//! Uses [`egui_snarl`] to render a draggable, connectable graph of audio
//! processing nodes. The Snarl topology compiles down to a
//! [`ProcessingGraph`] via
//! [`compile_to_engine()`](GraphView::compile_to_engine), producing a
//! [`GraphCommand::ReplaceTopology`] for atomic swap on the audio thread.

use std::collections::{HashMap, HashSet};

use egui::{Color32, FontId, RichText, Stroke, Ui};
use egui_snarl::ui::{PinInfo, SnarlStyle, SnarlViewer};
use egui_snarl::{InPin, InPinId, NodeId, OutPin, OutPinId, Snarl};

use sonido_core::graph::{GraphEngine, MAX_SPLIT_TARGETS, ProcessingGraph};
use sonido_core::{ParamDescriptor, SmoothingStyle};
use sonido_gui_core::theme::SonidoTheme;
use sonido_gui_core::widgets::glow;
use sonido_registry::{EffectCategory, EffectRegistry};

use crate::chain_manager::GraphCommand;

/// Maximum number of fan-out/fan-in ports on Split/Merge nodes.
const MAX_PORTS: usize = MAX_SPLIT_TARGETS;

/// Horizontal offset (pixels) for auto-inserted Merge nodes relative to their
/// downstream target.
const AUTO_MERGE_OFFSET_PX: f32 = 80.0;

/// A node in the visual graph editor.
#[derive(Clone, Debug)]
pub enum SonidoNode {
    /// Audio input source (microphone, file, etc.).
    Input,
    /// Audio output sink (speakers, file, etc.).
    Output,
    /// An audio effect with its static metadata.
    Effect {
        /// Registry identifier (e.g., `"distortion"`, `"reverb"`).
        effect_id: &'static str,
        /// Human-readable display name.
        name: &'static str,
        /// Effect category for coloring.
        category: EffectCategory,
        /// Parameter descriptors for this effect.
        descriptors: Vec<ParamDescriptor>,
        /// Per-parameter smoothing hints.
        smoothing: Vec<SmoothingStyle>,
    },
    /// Signal splitter: 1 input, up to 8 outputs.
    Split,
    /// Signal merger: up to 8 inputs, 1 output.
    Merge,
}

impl SonidoNode {
    /// Convert to a serializable session node.
    pub fn to_session(&self) -> crate::session::SessionNode {
        match self {
            SonidoNode::Input => crate::session::SessionNode::Input,
            SonidoNode::Output => crate::session::SessionNode::Output,
            SonidoNode::Effect { effect_id, .. } => crate::session::SessionNode::Effect {
                effect_id: (*effect_id).to_string(),
            },
            SonidoNode::Split => crate::session::SessionNode::Split,
            SonidoNode::Merge => crate::session::SessionNode::Merge,
        }
    }
}

/// Error type for graph compilation failures.
#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    /// No Input node found in the graph.
    #[error("graph has no Input node")]
    NoInput,
    /// No Output node found in the graph.
    #[error("graph has no Output node")]
    NoOutput,
    /// Multiple Input nodes found.
    #[error("graph has multiple Input nodes")]
    MultipleInputs,
    /// Multiple Output nodes found.
    #[error("graph has multiple Output nodes")]
    MultipleOutputs,
    /// An effect could not be created from the registry.
    #[error("failed to create effect '{0}' from registry")]
    EffectCreation(String),
    /// Graph compilation failed.
    #[error("graph compilation failed: {0}")]
    GraphError(#[from] sonido_core::graph::GraphError),
}

/// Visual node-graph editor wrapping [`Snarl<SonidoNode>`].
///
/// Provides a high-level API for rendering the graph and compiling it
/// into a [`GraphCommand::ReplaceTopology`] for the audio thread.
pub struct GraphView {
    /// The underlying Snarl graph state.
    pub snarl: Snarl<SonidoNode>,
    /// Currently selected node, if any.
    pub selected_node: Option<NodeId>,
    /// Visual style configuration.
    pub style: SnarlStyle,
    /// Set to `true` when a connect/disconnect/remove changes the topology.
    /// Checked by the app after `show()` to trigger auto-compile.
    pub topology_changed: bool,
    /// Per-effect-slot activity level (0.0--1.0), updated each frame from
    /// audio-thread metering data. Drives the glow LED on each effect node.
    pub slot_activity: Vec<f32>,
    /// Per-effect-slot L/R peak levels (0.0--1.0), updated each frame from
    /// audio-thread metering data. Drives the inline L/R meter strips.
    pub slot_peaks: Vec<(f32, f32)>,
}

impl GraphView {
    /// Creates a new graph view with default Input and Output nodes.
    ///
    /// The two nodes are connected so that audio passes through immediately
    /// after the first compile. Users can right-click to add effects between
    /// them.
    pub fn new() -> Self {
        let mut snarl = Snarl::new();
        let input = snarl.insert_node(egui::pos2(100.0, 200.0), SonidoNode::Input);
        let output = snarl.insert_node(egui::pos2(500.0, 200.0), SonidoNode::Output);
        snarl.connect(
            OutPinId {
                node: input,
                output: 0,
            },
            InPinId {
                node: output,
                input: 0,
            },
        );
        let mut style = SnarlStyle::new();
        // Audio graph nodes should never collapse — collapsing hides pins
        // and body, breaking visual wire connections and confusing the layout.
        style.collapsible = Some(false);

        Self {
            snarl,
            selected_node: None,
            style,
            topology_changed: false,
            slot_activity: Vec::new(),
            slot_peaks: Vec::new(),
        }
    }

    /// Pin Input/Output nodes to the left/right edges of the effect bounding box.
    ///
    /// Called at the start of each frame so I/O nodes act as fixed wire anchors
    /// that cannot be dragged out of position.
    fn pin_io_nodes(&mut self) {
        let mut input_id = None;
        let mut output_id = None;
        let mut min_x = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut sum_y = 0.0f32;
        let mut effect_count = 0u32;

        for (id, node) in self.snarl.node_ids() {
            match node {
                SonidoNode::Input => input_id = Some(id),
                SonidoNode::Output => output_id = Some(id),
                SonidoNode::Effect { .. } => {
                    if let Some(info) = self.snarl.get_node_info(id) {
                        min_x = min_x.min(info.pos.x);
                        max_x = max_x.max(info.pos.x);
                        sum_y += info.pos.y;
                        effect_count += 1;
                    }
                }
                _ => {}
            }
        }

        let (input_pos, output_pos) = if effect_count > 0 {
            let avg_y = sum_y / effect_count as f32;
            (
                egui::pos2(min_x - 150.0, avg_y),
                egui::pos2(max_x + 200.0, avg_y),
            )
        } else {
            (egui::pos2(50.0, 200.0), egui::pos2(400.0, 200.0))
        };

        if let Some(id) = input_id
            && let Some(info) = self.snarl.get_node_info_mut(id)
        {
            info.pos = input_pos;
        }
        if let Some(id) = output_id
            && let Some(info) = self.snarl.get_node_info_mut(id)
        {
            info.pos = output_pos;
        }
    }

    /// Renders the graph editor and returns the slot index of the currently
    /// selected effect node, if any.
    ///
    /// The returned `usize` corresponds to the effect's position among
    /// all Effect nodes in the graph (useful for param-bridge indexing).
    pub fn show(&mut self, ui: &mut Ui) -> Option<usize> {
        self.topology_changed = false;
        self.pin_io_nodes();
        let theme = SonidoTheme::get(ui.ctx());
        let mut click_handled = false;
        let mut viewer = SonidoViewer {
            selected_node: &mut self.selected_node,
            click_handled: &mut click_handled,
            topology_changed: &mut self.topology_changed,
            theme,
            slot_activity: &self.slot_activity,
            slot_peaks: &self.slot_peaks,
        };
        self.snarl
            .show(&mut viewer, &self.style, "sonido_graph", ui);

        // Click on empty space deselects — only within the graph area.
        // Without the rect check, clicks on the effect panel (below the graph)
        // would deselect the node and hide the panel.
        if !click_handled
            && ui.input(|i| i.pointer.primary_pressed())
            && let Some(pos) = ui.input(|i| i.pointer.interact_pos())
            && ui.max_rect().contains(pos)
        {
            self.selected_node = None;
        }

        // Map selected NodeId to an effect slot index.
        let selected = self.selected_node?;
        let mut slot = 0usize;
        for (id, node) in self.snarl.node_ids() {
            if matches!(node, SonidoNode::Effect { .. }) {
                if id == selected {
                    return Some(slot);
                }
                slot += 1;
            }
        }
        None
    }

    /// Compiles the Snarl topology into a [`GraphCommand::ReplaceTopology`].
    ///
    /// Walks all nodes and connections, builds a [`ProcessingGraph`], creates
    /// effects via the registry, and produces a compiled engine ready for
    /// atomic swap on the audio thread.
    ///
    /// # Errors
    ///
    /// Returns [`CompileError`] if the graph is malformed (missing Input/Output,
    /// unknown effects, cycles, etc.).
    pub fn compile_to_engine(
        &self,
        sample_rate: f32,
        block_size: usize,
        registry: &EffectRegistry,
    ) -> Result<GraphCommand, CompileError> {
        let mut graph = ProcessingGraph::new(sample_rate, block_size);

        // Map Snarl NodeIds to ProcessingGraph NodeIds.
        let mut snarl_to_graph: HashMap<NodeId, sonido_core::graph::NodeId> = HashMap::new();
        let mut manifest: Vec<(sonido_core::graph::NodeId, &'static str)> = Vec::new();
        let mut slot_descriptors: Vec<Vec<ParamDescriptor>> = Vec::new();
        let mut effect_ids: Vec<&'static str> = Vec::new();

        let mut input_count = 0u32;
        let mut output_count = 0u32;

        // First pass: create all nodes.
        for (snarl_id, node) in self.snarl.node_ids() {
            let graph_id = match node {
                SonidoNode::Input => {
                    input_count += 1;
                    if input_count > 1 {
                        return Err(CompileError::MultipleInputs);
                    }
                    graph.add_input()
                }
                SonidoNode::Output => {
                    output_count += 1;
                    if output_count > 1 {
                        return Err(CompileError::MultipleOutputs);
                    }
                    graph.add_output()
                }
                SonidoNode::Effect {
                    effect_id,
                    descriptors,
                    ..
                } => {
                    let effect = registry
                        .create(effect_id, sample_rate)
                        .ok_or_else(|| CompileError::EffectCreation((*effect_id).to_string()))?;
                    let gid = graph.add_effect(effect);
                    manifest.push((gid, effect_id));
                    effect_ids.push(effect_id);
                    slot_descriptors.push(descriptors.clone());
                    gid
                }
                // Legacy Split/Merge from old sessions — preserve them.
                SonidoNode::Split => graph.add_split(),
                SonidoNode::Merge => graph.add_merge(),
            };
            snarl_to_graph.insert(snarl_id, graph_id);
        }

        if input_count == 0 {
            return Err(CompileError::NoInput);
        }
        if output_count == 0 {
            return Err(CompileError::NoOutput);
        }

        // --- Auto-wire: analyze snarl topology for fan-out / fan-in ---

        // Build per-snarl-node target/source lists from wires.
        let mut out_targets: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        let mut in_sources: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        for (out_pin, in_pin) in self.snarl.wires() {
            let targets = out_targets.entry(out_pin.node).or_default();
            if !targets.contains(&in_pin.node) {
                targets.push(in_pin.node);
            }
            let sources = in_sources.entry(in_pin.node).or_default();
            if !sources.contains(&out_pin.node) {
                sources.push(out_pin.node);
            }
        }

        // Auto-insert Splits for fan-out: any non-Split node with >1 distinct targets.
        let mut split_map: HashMap<NodeId, sonido_core::graph::NodeId> = HashMap::new();
        for (&snarl_id, targets) in &out_targets {
            if targets.len() > 1 && !matches!(self.snarl[snarl_id], SonidoNode::Split) {
                let split_gid = graph.add_split();
                let source_gid = snarl_to_graph[&snarl_id];
                graph.connect(source_gid, split_gid)?;
                split_map.insert(snarl_id, split_gid);
            }
        }

        // Auto-insert Merges for fan-in: any node with >1 distinct sources
        // that isn't already a Merge node.
        let mut merge_map: HashMap<NodeId, sonido_core::graph::NodeId> = HashMap::new();
        for (&snarl_id, sources) in &in_sources {
            if sources.len() > 1 && !matches!(self.snarl[snarl_id], SonidoNode::Merge) {
                let merge_gid = graph.add_merge();
                let target_gid = snarl_to_graph[&snarl_id];
                graph.connect(merge_gid, target_gid)?;
                merge_map.insert(snarl_id, merge_gid);
            }
        }

        // Second pass: wire through auto-inserted nodes.
        // Deduplicate because multiple snarl pins can map to the same graph edge.
        let mut wired: HashSet<(sonido_core::graph::NodeId, sonido_core::graph::NodeId)> =
            HashSet::new();
        for (out_pin, in_pin) in self.snarl.wires() {
            let from = split_map
                .get(&out_pin.node)
                .copied()
                .unwrap_or_else(|| snarl_to_graph[&out_pin.node]);
            let to = merge_map
                .get(&in_pin.node)
                .copied()
                .unwrap_or_else(|| snarl_to_graph[&in_pin.node]);
            if wired.insert((from, to)) {
                graph.connect(from, to)?;
            }
        }

        graph.compile()?;

        let engine = GraphEngine::new_dag(graph, manifest);

        Ok(GraphCommand::ReplaceTopology {
            engine: Box::new(engine),
            effect_ids,
            slot_descriptors,
        })
    }

    /// Capture the current graph state as a [`Session`](crate::session::Session).
    ///
    /// Walks all nodes and wires in the Snarl graph, reads parameter values
    /// from the bridge, and bundles everything into a serializable session.
    pub fn capture_session(
        &self,
        bridge: &dyn sonido_gui_core::ParamBridge,
        input_gain: f32,
        master_volume: f32,
    ) -> crate::session::Session {
        use crate::session::{EffectState, Session, SessionNodeEntry};
        use sonido_gui_core::{ParamIndex, SlotIndex};

        let mut nodes = Vec::new();
        let mut node_id_to_idx: HashMap<NodeId, usize> = HashMap::new();

        for (id, node) in self.snarl.node_ids() {
            let idx = nodes.len();
            node_id_to_idx.insert(id, idx);
            let pos = self
                .snarl
                .get_node_info(id)
                .map_or([0.0, 0.0], |info| [info.pos.x, info.pos.y]);
            nodes.push(SessionNodeEntry {
                node: node.to_session(),
                pos,
            });
        }

        let mut wires = Vec::new();
        for (out_pin, in_pin) in self.snarl.wires() {
            if let (Some(&from_idx), Some(&to_idx)) = (
                node_id_to_idx.get(&out_pin.node),
                node_id_to_idx.get(&in_pin.node),
            ) {
                wires.push((from_idx, out_pin.output, to_idx, in_pin.input));
            }
        }

        let mut params = HashMap::new();
        let mut effect_slot = 0usize;
        for (idx, entry) in nodes.iter().enumerate() {
            if let crate::session::SessionNode::Effect { ref effect_id } = entry.node {
                let slot = SlotIndex(effect_slot);
                let param_count = bridge.param_count(slot);
                let param_values: Vec<f32> = (0..param_count)
                    .map(|i| bridge.get(slot, ParamIndex(i)))
                    .collect();
                params.insert(
                    idx,
                    EffectState {
                        effect_id: effect_id.clone(),
                        params: param_values,
                        bypassed: bridge.is_bypassed(slot),
                    },
                );
                effect_slot += 1;
            }
        }

        Session {
            version: Session::VERSION,
            nodes,
            wires,
            params,
            input_gain,
            master_volume,
        }
    }

    /// Restore graph from a session, rebuilding the Snarl topology.
    ///
    /// Creates new nodes and wires from the session data. Unknown effects
    /// (not found in the registry) are logged and skipped. After calling
    /// this method, the caller should compile the graph and apply params.
    pub fn restore_session(
        &mut self,
        session: &crate::session::Session,
        registry: &EffectRegistry,
    ) {
        use crate::session::SessionNode;

        let mut snarl = Snarl::new();
        let mut idx_to_node_id: Vec<Option<NodeId>> = Vec::new();

        for entry in &session.nodes {
            let pos = egui::pos2(entry.pos[0], entry.pos[1]);
            let node = match &entry.node {
                SessionNode::Input => SonidoNode::Input,
                SessionNode::Output => SonidoNode::Output,
                SessionNode::Effect { effect_id } => {
                    if let Some(desc) = registry.get(effect_id) {
                        let descriptors = collect_descriptors(desc.id, 48000.0);
                        let smoothing = collect_smoothing(desc.id, 48000.0);
                        SonidoNode::Effect {
                            effect_id: desc.id,
                            name: desc.name,
                            category: desc.category,
                            descriptors,
                            smoothing,
                        }
                    } else {
                        tracing::warn!("unknown effect in session: {effect_id}");
                        idx_to_node_id.push(None);
                        continue;
                    }
                }
                SessionNode::Split => SonidoNode::Split,
                SessionNode::Merge => SonidoNode::Merge,
            };
            let id = snarl.insert_node(pos, node);
            idx_to_node_id.push(Some(id));
        }

        // Restore wires
        for &(from_idx, from_output, to_idx, to_input) in &session.wires {
            if let (Some(Some(from_node)), Some(Some(to_node))) =
                (idx_to_node_id.get(from_idx), idx_to_node_id.get(to_idx))
            {
                snarl.connect(
                    OutPinId {
                        node: *from_node,
                        output: from_output,
                    },
                    InPinId {
                        node: *to_node,
                        input: to_input,
                    },
                );
            }
        }

        self.snarl = snarl;
        self.selected_node = None;
        self.topology_changed = true;
    }
}

impl Default for GraphView {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns a category-based color from the arcade CRT theme palette.
///
/// Mapping:
/// - Dynamics  -> cyan (info / signal labels)
/// - Distortion -> red (danger / clip)
/// - Modulation -> magenta (modulation category)
/// - Filter    -> yellow (caution / filter)
/// - TimeBased -> purple (delay / reverb)
/// - Utility   -> amber (brand primary / default)
fn category_color(cat: EffectCategory, theme: &SonidoTheme) -> Color32 {
    match cat {
        EffectCategory::Dynamics => theme.colors.cyan,
        EffectCategory::Distortion => theme.colors.red,
        EffectCategory::Modulation => theme.colors.magenta,
        EffectCategory::Filter => theme.colors.yellow,
        EffectCategory::TimeBased => theme.colors.purple,
        EffectCategory::Utility => theme.colors.amber,
    }
}

/// Color for structural nodes (Input, Output, Split, Merge) — uses theme dim.
fn structural_color(theme: &SonidoTheme) -> Color32 {
    theme.colors.text_secondary
}

/// Color for inline node peak meters based on level thresholds.
///
/// - Green below -12 dBFS (linear ≈ 0.251)
/// - Yellow -12 to -3 dBFS (linear ≈ 0.251 to 0.708)
/// - Red above -3 dBFS (linear > 0.708)
fn meter_color(level: f32, theme: &SonidoTheme) -> Color32 {
    if level > 0.708 {
        theme.colors.red
    } else if level > 0.251 {
        theme.colors.yellow
    } else {
        theme.colors.green
    }
}

/// [`SnarlViewer`] implementation for [`SonidoNode`].
///
/// Handles rendering, context menus, and connection logic for the
/// Sonido audio graph editor. Carries a snapshot of [`SonidoTheme`]
/// so that `node_frame` / `header_frame` (which lack a `Ui` handle)
/// can still read the arcade CRT palette.
struct SonidoViewer<'a> {
    /// Mutable reference to the selected-node state in [`GraphView`].
    selected_node: &'a mut Option<NodeId>,
    /// Set to `true` when a node click is detected, preventing empty-space
    /// deselection on the same frame.
    click_handled: &'a mut bool,
    /// Set to `true` when a connect/disconnect/remove changes the topology,
    /// signalling the app to auto-compile.
    topology_changed: &'a mut bool,
    /// Arcade CRT theme snapshot for palette access.
    theme: SonidoTheme,
    /// Per-effect-slot activity level (0.0--1.0) for LED indicators.
    slot_activity: &'a [f32],
    /// Per-effect-slot L/R peak levels (0.0--1.0) for inline mini meters.
    slot_peaks: &'a [(f32, f32)],
}

impl SonidoViewer<'_> {
    /// Resolve the accent color for a node (category color for effects,
    /// structural color for Input/Output/Split/Merge).
    fn node_accent(&self, node: &SonidoNode) -> Color32 {
        match node {
            SonidoNode::Effect { category, .. } => category_color(*category, &self.theme),
            _ => structural_color(&self.theme),
        }
    }
}

impl SnarlViewer<SonidoNode> for SonidoViewer<'_> {
    fn title(&mut self, node: &SonidoNode) -> String {
        match node {
            SonidoNode::Input => "Input".to_string(),
            SonidoNode::Output => "Output".to_string(),
            SonidoNode::Effect { name, .. } => (*name).to_string(),
            SonidoNode::Split => "Split".to_string(),
            SonidoNode::Merge => "Merge".to_string(),
        }
    }

    fn node_frame(
        &mut self,
        _default: egui::Frame,
        node: NodeId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        snarl: &Snarl<SonidoNode>,
    ) -> egui::Frame {
        let node_data = &snarl[node];

        // I/O nodes are invisible — the sidebar strips are the visual.
        // Zero margin so only the wire pin dot remains.
        if matches!(node_data, SonidoNode::Input | SonidoNode::Output) {
            return egui::Frame::new()
                .fill(Color32::TRANSPARENT)
                .stroke(Stroke::NONE)
                .corner_radius(0.0)
                .inner_margin(0.0);
        }

        let accent = self.node_accent(node_data);
        let is_selected = *self.selected_node == Some(node);
        let (stroke_width, fill) = if is_selected {
            (2.0, accent.gamma_multiply(0.08))
        } else {
            (1.0, self.theme.colors.void)
        };
        egui::Frame::new()
            .fill(fill)
            .stroke(Stroke::new(stroke_width, accent))
            .corner_radius(4.0)
            .inner_margin(6.0)
    }

    fn header_frame(
        &mut self,
        _default: egui::Frame,
        node: NodeId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        snarl: &Snarl<SonidoNode>,
    ) -> egui::Frame {
        let node_data = &snarl[node];

        // I/O nodes: invisible header (no text rendered in show_header).
        if matches!(node_data, SonidoNode::Input | SonidoNode::Output) {
            return egui::Frame::new()
                .fill(Color32::TRANSPARENT)
                .stroke(Stroke::NONE)
                .corner_radius(0.0)
                .inner_margin(0.0);
        }

        let accent = self.node_accent(node_data);
        // Subtle tinted header background — the accent at very low alpha
        let header_bg = accent.gamma_multiply(0.10);
        egui::Frame::new()
            .fill(header_bg)
            .stroke(Stroke::NONE)
            .corner_radius(4.0)
            .inner_margin(4.0)
    }

    fn show_header(
        &mut self,
        node: NodeId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        ui: &mut Ui,
        _scale: f32,
        snarl: &mut Snarl<SonidoNode>,
    ) {
        let node_data = &snarl[node];

        // I/O nodes: no label — the sidebar strips are the visual representation.
        // Only the wire pin is visible.
        if matches!(node_data, SonidoNode::Input | SonidoNode::Output) {
            return;
        }

        let accent = self.node_accent(node_data);
        let is_selected = *self.selected_node == Some(node);
        let title = self.title(node_data);

        // Bold the title if selected — plain label (no Sense) to avoid
        // stealing pointer events from snarl's node drag system.
        let text = if is_selected {
            RichText::new(title)
                .font(FontId::monospace(12.0))
                .color(accent)
                .strong()
        } else {
            RichText::new(title)
                .font(FontId::monospace(11.0))
                .color(accent)
        };

        ui.label(text);

        // Activity LED for effect nodes — glows when signal passes through
        if matches!(node_data, SonidoNode::Effect { .. }) {
            let mut slot_idx = 0usize;
            for (id, n) in snarl.node_ids() {
                if id == node {
                    break;
                }
                if matches!(n, SonidoNode::Effect { .. }) {
                    slot_idx += 1;
                }
            }
            let activity = self.slot_activity.get(slot_idx).copied().unwrap_or(0.0);
            if activity > 0.01 {
                let led_pos = egui::pos2(ui.max_rect().right() - 6.0, ui.max_rect().center().y);
                let led_alpha = activity.clamp(0.2, 1.0);
                let led_color = accent.gamma_multiply(led_alpha);
                glow::glow_circle(ui.painter(), led_pos, 3.0, led_color, &self.theme);
            }
        }
    }

    fn inputs(&mut self, node: &SonidoNode) -> usize {
        match node {
            SonidoNode::Input => 0,
            SonidoNode::Output => 1,
            SonidoNode::Effect { .. } => 1,
            SonidoNode::Split => 1,
            SonidoNode::Merge => MAX_PORTS,
        }
    }

    fn outputs(&mut self, node: &SonidoNode) -> usize {
        match node {
            SonidoNode::Input => 1,
            SonidoNode::Output => 0,
            SonidoNode::Effect { .. } => 1,
            SonidoNode::Split => MAX_PORTS,
            SonidoNode::Merge => 1,
        }
    }

    fn show_input(
        &mut self,
        pin: &InPin,
        _ui: &mut Ui,
        _scale: f32,
        snarl: &mut Snarl<SonidoNode>,
    ) -> impl egui_snarl::ui::SnarlPin + 'static {
        let color = self.node_accent(&snarl[pin.id.node]);
        PinInfo::circle().with_fill(color).with_wire_color(color)
    }

    fn show_output(
        &mut self,
        pin: &OutPin,
        _ui: &mut Ui,
        _scale: f32,
        snarl: &mut Snarl<SonidoNode>,
    ) -> impl egui_snarl::ui::SnarlPin + 'static {
        // Wire color follows the source (output) node's category.
        let color = self.node_accent(&snarl[pin.id.node]);
        PinInfo::circle().with_fill(color).with_wire_color(color)
    }

    fn has_body(&mut self, node: &SonidoNode) -> bool {
        matches!(node, SonidoNode::Effect { .. })
    }

    fn show_body(
        &mut self,
        node: NodeId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        ui: &mut Ui,
        _scale: f32,
        snarl: &mut Snarl<SonidoNode>,
    ) {
        if let SonidoNode::Effect {
            category,
            descriptors,
            ..
        } = &snarl[node]
        {
            let dim = self.theme.colors.text_secondary;
            let is_selected = *self.selected_node == Some(node);
            let accent = category_color(*category, &self.theme);

            // Resolve slot index for this effect node.
            let mut slot_idx = 0usize;
            for (id, n) in snarl.node_ids() {
                if id == node {
                    break;
                }
                if matches!(n, SonidoNode::Effect { .. }) {
                    slot_idx += 1;
                }
            }

            // Plain label — selection is handled via final_node_rect()
            let body_text = format!("{} · {} params", category.name(), descriptors.len());
            let color = if is_selected { accent } else { dim };
            ui.label(
                RichText::new(body_text)
                    .font(FontId::monospace(9.0))
                    .color(color),
            );

            // Inline L/R peak meters — thin colored strips at the bottom of the node.
            let (peak_l, peak_r) = self.slot_peaks.get(slot_idx).copied().unwrap_or((0.0, 0.0));

            // Allocate a thin horizontal rect for the two meter strips.
            let meter_height = 4.0_f32;
            let (meter_rect, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), meter_height * 2.0 + 2.0),
                egui::Sense::hover(),
            );

            if ui.is_rect_visible(meter_rect) {
                let painter = ui.painter();
                let w = meter_rect.width();

                // L strip (top), R strip (bottom)
                let l_rect = egui::Rect::from_min_size(meter_rect.min, egui::vec2(w, meter_height));
                let r_rect = egui::Rect::from_min_size(
                    egui::pos2(meter_rect.min.x, meter_rect.min.y + meter_height + 2.0),
                    egui::vec2(w, meter_height),
                );

                painter.rect_filled(l_rect, 0.0, self.theme.colors.void);
                painter.rect_filled(r_rect, 0.0, self.theme.colors.void);

                if peak_l > 0.001 {
                    let bar_w = (w * peak_l.min(1.0)).max(1.0);
                    let bar =
                        egui::Rect::from_min_size(l_rect.min, egui::vec2(bar_w, meter_height));
                    painter.rect_filled(bar, 0.0, meter_color(peak_l, &self.theme));
                }

                if peak_r > 0.001 {
                    let bar_w = (w * peak_r.min(1.0)).max(1.0);
                    let bar =
                        egui::Rect::from_min_size(r_rect.min, egui::vec2(bar_w, meter_height));
                    painter.rect_filled(bar, 0.0, meter_color(peak_r, &self.theme));
                }
            }
        }
    }

    fn has_graph_menu(&mut self, _pos: egui::Pos2, _snarl: &mut Snarl<SonidoNode>) -> bool {
        true
    }

    fn show_graph_menu(
        &mut self,
        pos: egui::Pos2,
        ui: &mut Ui,
        _scale: f32,
        snarl: &mut Snarl<SonidoNode>,
    ) {
        // Search filter — persisted across frames via egui temp data
        let filter_id = egui::Id::new("graph_menu_filter");
        let mut filter: String = ui
            .data(|d| d.get_temp::<String>(filter_id))
            .unwrap_or_default();

        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Search")
                    .font(FontId::monospace(10.0))
                    .color(self.theme.colors.text_secondary),
            );
            let response = ui.text_edit_singleline(&mut filter);
            // Auto-focus the search field when the menu opens
            if response.gained_focus() || ui.memory(|m| m.focused().is_none()) {
                response.request_focus();
            }
        });
        ui.data_mut(|d| d.insert_temp(filter_id, filter.clone()));

        let filter_lower = filter.to_lowercase();
        ui.separator();

        if filter.is_empty() {
            // Category submenus (existing behavior when no filter)
            let registry = EffectRegistry::new();
            let categories = [
                EffectCategory::Dynamics,
                EffectCategory::Distortion,
                EffectCategory::Modulation,
                EffectCategory::Filter,
                EffectCategory::TimeBased,
                EffectCategory::Utility,
            ];

            for cat in categories {
                ui.menu_button(cat.name(), |ui| {
                    for desc in registry.effects_in_category(cat) {
                        if ui.button(desc.name).clicked() {
                            let descriptors = collect_descriptors(desc.id, 48000.0);
                            let smoothing = collect_smoothing(desc.id, 48000.0);
                            snarl.insert_node(
                                pos,
                                SonidoNode::Effect {
                                    effect_id: desc.id,
                                    name: desc.name,
                                    category: desc.category,
                                    descriptors,
                                    smoothing,
                                },
                            );
                            *self.topology_changed = true;
                            ui.close_menu();
                        }
                    }
                });
            }
        } else {
            // Flat filtered list — show matching effects with category color
            let registry = EffectRegistry::new();
            for desc in registry.all_effects() {
                if desc.name.to_lowercase().contains(&filter_lower)
                    || desc.id.contains(&filter_lower)
                {
                    let cat_color = category_color(desc.category, &self.theme);
                    if ui
                        .button(RichText::new(desc.name).color(cat_color))
                        .clicked()
                    {
                        let descriptors = collect_descriptors(desc.id, 48000.0);
                        let smoothing = collect_smoothing(desc.id, 48000.0);
                        snarl.insert_node(
                            pos,
                            SonidoNode::Effect {
                                effect_id: desc.id,
                                name: desc.name,
                                category: desc.category,
                                descriptors,
                                smoothing,
                            },
                        );
                        *self.topology_changed = true;
                        // Clear filter for next open
                        ui.data_mut(|d| d.insert_temp::<String>(filter_id, String::new()));
                        ui.close_menu();
                    }
                }
            }
        }
    }

    fn has_node_menu(&mut self, node: &SonidoNode) -> bool {
        // I/O nodes are fixed — no remove/duplicate menu.
        !matches!(node, SonidoNode::Input | SonidoNode::Output)
    }

    fn show_node_menu(
        &mut self,
        node: NodeId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        ui: &mut Ui,
        _scale: f32,
        snarl: &mut Snarl<SonidoNode>,
    ) {
        if ui.button("Remove").clicked() {
            // Clear selection if this node was selected.
            if *self.selected_node == Some(node) {
                *self.selected_node = None;
            }
            snarl.remove_node(node);
            *self.topology_changed = true;
            ui.close_menu();
            return;
        }

        if ui.button("Duplicate").clicked() {
            let original = snarl[node].clone();
            let original_pos = snarl
                .get_node_info(node)
                .map_or(egui::pos2(0.0, 0.0), |n| n.pos);
            let offset = egui::vec2(30.0, 30.0);
            snarl.insert_node(original_pos + offset, original);
            *self.topology_changed = true;
            ui.close_menu();
        }
    }

    fn final_node_rect(
        &mut self,
        node: NodeId,
        ui_rect: egui::Rect,
        _graph_rect: egui::Rect,
        ui: &mut Ui,
        _scale: f32,
        snarl: &mut Snarl<SonidoNode>,
    ) {
        // I/O nodes are not selectable — they have no param panel.
        if matches!(snarl[node], SonidoNode::Input | SonidoNode::Output) {
            return;
        }

        // Detect clicks on nodes without adding interactive widgets that
        // would steal pointer events from snarl's built-in drag system.
        // Use primary_pressed() (button-down) instead of primary_clicked()
        // (button-up) — the latter fails when the mouse moves even slightly
        // during a click. Allow all overlapping nodes to set themselves as
        // selected; since snarl iterates in draw order (back-to-front), the
        // topmost (last-drawn) node wins.
        if let Some(pos) = ui.input(|i| {
            i.pointer
                .primary_pressed()
                .then(|| i.pointer.interact_pos())
                .flatten()
        }) && ui_rect.contains(pos)
        {
            *self.selected_node = Some(node);
            *self.click_handled = true;
        }
    }

    fn connect(&mut self, from: &OutPin, to: &InPin, snarl: &mut Snarl<SonidoNode>) {
        if matches!(snarl[to.id.node], SonidoNode::Merge) {
            // Merge nodes accept multiple inputs directly.
            snarl.connect(from.id, to.id);
        } else {
            // Check if the target already has an incoming wire.
            let existing_source: Option<OutPinId> = snarl
                .wires()
                .find(|&(_, inp)| inp.node == to.id.node)
                .map(|(out, _)| out);

            match existing_source {
                Some(old) if matches!(snarl[old.node], SonidoNode::Merge) => {
                    add_to_existing_merge(from.id, old.node, snarl);
                }
                Some(old) => {
                    insert_merge_between(from.id, old, to.id, snarl);
                }
                None => {
                    snarl.connect(from.id, to.id);
                }
            }
        }
        *self.topology_changed = true;
    }

    fn disconnect(&mut self, from: &OutPin, to: &InPin, snarl: &mut Snarl<SonidoNode>) {
        snarl.disconnect(from.id, to.id);
        *self.topology_changed = true;
    }

    fn drop_outputs(&mut self, pin: &OutPin, snarl: &mut Snarl<SonidoNode>) {
        snarl.drop_outputs(pin.id);
        *self.topology_changed = true;
    }

    fn drop_inputs(&mut self, pin: &InPin, snarl: &mut Snarl<SonidoNode>) {
        snarl.drop_inputs(pin.id);
        *self.topology_changed = true;
    }
}

/// Add a new connection to an existing Merge node.
///
/// Counts current inputs on the Merge and appends the new source to the next
/// available input slot.
fn add_to_existing_merge(from: OutPinId, merge_node: NodeId, snarl: &mut Snarl<SonidoNode>) {
    let used = snarl
        .wires()
        .filter(|&(_, inp)| inp.node == merge_node)
        .count();
    snarl.connect(
        from,
        InPinId {
            node: merge_node,
            input: used,
        },
    );
}

/// Auto-insert a Merge node between a new source, an existing source, and
/// their shared target.
///
/// Creates a Merge node offset [`AUTO_MERGE_OFFSET_PX`] pixels to the left of
/// the target, rewires the existing connection through it, and adds the new
/// connection.
fn insert_merge_between(
    from: OutPinId,
    existing: OutPinId,
    target: InPinId,
    snarl: &mut Snarl<SonidoNode>,
) {
    let target_pos = snarl
        .get_node_info(target.node)
        .map_or(egui::pos2(0.0, 0.0), |n| n.pos);
    let merge_pos = target_pos - egui::vec2(AUTO_MERGE_OFFSET_PX, 0.0);
    let merge_id = snarl.insert_node(merge_pos, SonidoNode::Merge);

    // Reroute: existing source → Merge input 0
    snarl.disconnect(existing, target);
    snarl.connect(
        existing,
        InPinId {
            node: merge_id,
            input: 0,
        },
    );
    // New source → Merge input 1
    snarl.connect(
        from,
        InPinId {
            node: merge_id,
            input: 1,
        },
    );
    // Merge output → original target
    snarl.connect(
        OutPinId {
            node: merge_id,
            output: 0,
        },
        target,
    );
}

/// Collect parameter descriptors for an effect by creating a temporary instance.
fn collect_descriptors(effect_id: &str, sample_rate: f32) -> Vec<ParamDescriptor> {
    let registry = EffectRegistry::new();
    let Some(effect) = registry.create(effect_id, sample_rate) else {
        return Vec::new();
    };
    (0..effect.effect_param_count())
        .filter_map(|i| effect.effect_param_info(i))
        .collect()
}

/// Collect smoothing styles for an effect by creating a temporary instance.
///
/// Uses the `KernelParams::smoothing()` function indirectly via the registry's
/// param count. Since we cannot call the trait method without a concrete type,
/// we store default smoothing for all params.
fn collect_smoothing(effect_id: &str, sample_rate: f32) -> Vec<SmoothingStyle> {
    let registry = EffectRegistry::new();
    let Some(effect) = registry.create(effect_id, sample_rate) else {
        return Vec::new();
    };
    // Default to Standard smoothing for all params since we cannot access
    // the concrete KernelParams type through the trait-object registry.
    vec![SmoothingStyle::default(); effect.effect_param_count()]
}
