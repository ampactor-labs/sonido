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
use egui_snarl::{InPin, NodeId, OutPin, Snarl};

use sonido_core::graph::{GraphEngine, MAX_SPLIT_TARGETS, ProcessingGraph};
use sonido_core::{ParamDescriptor, SmoothingStyle};
use sonido_gui_core::theme::SonidoTheme;
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
}

impl GraphView {
    /// Creates a new empty graph view.
    pub fn new() -> Self {
        Self {
            snarl: Snarl::new(),
            selected_node: None,
            style: SnarlStyle::new(),
        }
    }

    /// Renders the graph editor and returns the slot index of the currently
    /// selected effect node, if any.
    ///
    /// The returned `usize` corresponds to the effect's position among
    /// all Effect nodes in the graph (useful for param-bridge indexing).
    pub fn show(&mut self, ui: &mut Ui) -> Option<usize> {
        let theme = SonidoTheme::get(ui.ctx());
        let mut viewer = SonidoViewer {
            selected_node: &mut self.selected_node,
            theme,
        };
        self.snarl
            .show(&mut viewer, &self.style, "sonido_graph", ui);

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
    ) -> Result<GraphCommand, CompileError> {
        let registry = EffectRegistry::new();
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
    /// Arcade CRT theme snapshot for palette access.
    theme: SonidoTheme,
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
        let accent = self.node_accent(&snarl[node]);
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
        let accent = self.node_accent(&snarl[node]);
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
        let accent = self.node_accent(&snarl[node]);
        let is_selected = *self.selected_node == Some(node);
        let title = self.title(&snarl[node]);

        // Bold the title if selected
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

        // Clickable label — sets the selected node on click
        let response = ui.add(egui::Label::new(text).sense(egui::Sense::click()));
        if response.clicked() {
            *self.selected_node = Some(node);
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

            // Clickable body — clicking selects this node
            let body_text = format!("{} · {} params", category.name(), descriptors.len());
            let text = if is_selected {
                RichText::new(body_text)
                    .font(FontId::monospace(9.0))
                    .color(accent)
            } else {
                RichText::new(body_text)
                    .font(FontId::monospace(9.0))
                    .color(dim)
            };
            let response = ui.add(egui::Label::new(text).sense(egui::Sense::click()));
            if response.clicked() {
                *self.selected_node = Some(node);
            }

            // Selected indicator
            if is_selected {
                ui.label(
                    RichText::new("▸ selected")
                        .font(FontId::monospace(8.0))
                        .color(accent),
                );
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
        ui.label("Add Node");
        ui.separator();

        if ui.button("Input").clicked() {
            snarl.insert_node(pos, SonidoNode::Input);
            ui.close_menu();
        }
        if ui.button("Output").clicked() {
            snarl.insert_node(pos, SonidoNode::Output);
            ui.close_menu();
        }
        if ui.button("Split").clicked() {
            snarl.insert_node(pos, SonidoNode::Split);
            ui.close_menu();
        }
        if ui.button("Merge").clicked() {
            snarl.insert_node(pos, SonidoNode::Merge);
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
            ui.close_menu();
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
