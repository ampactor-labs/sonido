//! Visual node-graph editor for DAG-based audio routing.
//!
//! Uses [`egui_snarl`] to render a draggable, connectable graph of audio
//! processing nodes. The Snarl topology compiles down to a
//! [`ProcessingGraph`] via
//! [`compile_to_engine()`](GraphView::compile_to_engine), producing a
//! [`GraphCommand::ReplaceTopology`] for atomic swap on the audio thread.

use std::collections::HashMap;

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
        }
    }

    /// Renders the graph editor and returns the slot index of the currently
    /// selected effect node, if any.
    ///
    /// The returned `usize` corresponds to the effect's position among
    /// all Effect nodes in the graph (useful for param-bridge indexing).
    pub fn show(&mut self, ui: &mut Ui) -> Option<usize> {
        self.topology_changed = false;
        let theme = SonidoTheme::get(ui.ctx());
        let mut click_handled = false;
        let mut viewer = SonidoViewer {
            selected_node: &mut self.selected_node,
            click_handled: &mut click_handled,
            topology_changed: &mut self.topology_changed,
            theme,
            slot_activity: &self.slot_activity,
        };
        self.snarl
            .show(&mut viewer, &self.style, "sonido_graph", ui);

        // Click on empty space deselects — only within the graph area.
        // Without the rect check, clicks on the effect panel (below the graph)
        // would deselect the node and hide the panel.
        if !click_handled && ui.input(|i| i.pointer.primary_pressed()) {
            if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                if ui.max_rect().contains(pos) {
                    self.selected_node = None;
                }
            }
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

        // Second pass: create all connections.
        for (out_pin, in_pin) in self.snarl.wires() {
            let from = snarl_to_graph[&out_pin.node];
            let to = snarl_to_graph[&in_pin.node];
            graph.connect(from, to)?;
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

        // I/O nodes are represented by the sidebar strips — render minimal
        if matches!(node_data, SonidoNode::Input | SonidoNode::Output) {
            return egui::Frame::new()
                .fill(Color32::TRANSPARENT)
                .stroke(Stroke::NONE)
                .corner_radius(0.0)
                .inner_margin(2.0);
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

        // I/O nodes: minimal transparent header
        if matches!(node_data, SonidoNode::Input | SonidoNode::Output) {
            return egui::Frame::new()
                .fill(Color32::TRANSPARENT)
                .stroke(Stroke::NONE)
                .corner_radius(0.0)
                .inner_margin(1.0);
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

        // I/O nodes: tiny label, they're implicit in the sidebar
        if matches!(node_data, SonidoNode::Input | SonidoNode::Output) {
            let title = self.title(node_data);
            ui.label(
                RichText::new(title)
                    .font(FontId::monospace(8.0))
                    .color(self.theme.colors.text_secondary.gamma_multiply(0.5)),
            );
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
                let led_pos =
                    egui::pos2(ui.max_rect().right() - 6.0, ui.max_rect().center().y);
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
        PinInfo::circle()
            .with_fill(color)
            .with_wire_color(color)
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
        PinInfo::circle()
            .with_fill(color)
            .with_wire_color(color)
    }

    fn has_body(&mut self, _node: &SonidoNode) -> bool {
        false
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

            // Plain label — selection is handled via final_node_rect()
            let body_text = format!("{} · {} params", category.name(), descriptors.len());
            let color = if is_selected { accent } else { dim };
            ui.label(
                RichText::new(body_text)
                    .font(FontId::monospace(9.0))
                    .color(color),
            );
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
        ui.label("Add Node");
        ui.separator();

        if ui.button("Input").clicked() {
            snarl.insert_node(pos, SonidoNode::Input);
            *self.topology_changed = true;
            ui.close_menu();
        }
        if ui.button("Output").clicked() {
            snarl.insert_node(pos, SonidoNode::Output);
            *self.topology_changed = true;
            ui.close_menu();
        }
        if ui.button("Split").clicked() {
            snarl.insert_node(pos, SonidoNode::Split);
            *self.topology_changed = true;
            ui.close_menu();
        }
        if ui.button("Merge").clicked() {
            snarl.insert_node(pos, SonidoNode::Merge);
            *self.topology_changed = true;
            ui.close_menu();
        }

        ui.separator();

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
    }

    fn has_node_menu(&mut self, _node: &SonidoNode) -> bool {
        true
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
        _snarl: &mut Snarl<SonidoNode>,
    ) {
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
        }) {
            if ui_rect.contains(pos) {
                *self.selected_node = Some(node);
                *self.click_handled = true;
            }
        }
    }

    fn connect(&mut self, from: &OutPin, to: &InPin, snarl: &mut Snarl<SonidoNode>) {
        // For non-Merge input pins, disconnect existing wires first
        // (single-input semantics for Effect, Output, Split nodes).
        let target_node = &snarl[to.id.node];
        if !matches!(target_node, SonidoNode::Merge) {
            snarl.drop_inputs(to.id);
        }
        snarl.connect(from.id, to.id);
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
