//! Visual node-graph editor for DAG-based audio routing.
//!
//! Uses [`egui_snarl`] to render a draggable, connectable graph of audio
//! processing nodes. The Snarl topology compiles down to a
//! [`ProcessingGraph`] via
//! [`compile_to_engine()`](GraphView::compile_to_engine), producing a
//! [`GraphCommand::ReplaceTopology`] for atomic swap on the audio thread.

use std::collections::{HashMap, HashSet};

use egui::{Color32, FontId, Painter, RichText, Stroke, Style, Ui};
use egui_snarl::ui::{BackgroundPattern, PinInfo, SnarlStyle, SnarlViewer, Viewport};
use egui_snarl::{InPin, InPinId, NodeId, OutPin, OutPinId, Snarl};

use sonido_core::graph::{GraphEngine, MAX_SPLIT_TARGETS, ProcessingGraph};
use sonido_core::{ParamDescriptor, ParamFlags, SmoothingStyle};
use sonido_gui_core::theme::SonidoTheme;
use sonido_gui_core::widgets::glow;
use sonido_registry::{EffectCategory, EffectRegistry};

/// Width (screen px) of each I/O wall bar. Wide enough for a legible vertical
/// meter plus a small foot knob, thin enough to read as a wall. Shared so
/// `graph_view` welds the I/O pins to the bar's inner edge and `app.rs` draws
/// the bar to match.
pub const IO_BAR_WIDTH: f32 = 30.0;

/// How far (screen px) inside the canvas edge the welded I/O pins sit. Small, so
/// the wire emerges right at the wall bar's inner edge. The bars are drawn in
/// their own columns *outside* the canvas (`vp.rect`), so this is measured from
/// the canvas edge — NOT offset by a full bar width.
const IO_PIN_INSET: f32 = 6.0;

use crate::chain_manager::GraphCommand;

/// Maximum number of fan-out/fan-in ports on legacy Split/Merge nodes.
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
    /// Per-effect-slot L/R peak levels (0.0--1.0), updated each frame from
    /// audio-thread metering data. Drives the inline L/R meter strips.
    pub slot_peaks: Vec<(f32, f32)>,
    /// Target positions a re-flow is easing nodes toward. Non-empty while the
    /// layout is animating; each frame nodes glide a fraction of the remaining
    /// distance, so a topology edit settles smoothly instead of snapping.
    arrange_targets: HashMap<NodeId, egui::Pos2>,
    /// Most recent pan/zoom transform, captured during `draw_background`. Used
    /// to weld the Input/Output pins to the screen walls each frame. `None`
    /// until the first frame has rendered.
    last_viewport: Option<Viewport>,
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
        // Bound zoom so nodes can't be pinched into illegibility or shrunk to a
        // vanishing point (the "weird resizing" report). The I/O wall bars stay
        // welded regardless; this only constrains the middle content's scale.
        style.min_scale = Some(0.5);
        style.max_scale = Some(1.5);

        Self {
            snarl,
            selected_node: None,
            style,
            topology_changed: false,
            slot_activity: Vec::new(),
            slot_peaks: Vec::new(),
            arrange_targets: HashMap::new(),
            last_viewport: None,
        }
    }

    /// Build a representative chain (Input → Distortion → Delay → Reverb →
    /// Output), laid out left→right. Used to generate the README screenshot and
    /// as a quick demo graph; not part of normal startup.
    pub fn populate_demo(&mut self) {
        let registry = EffectRegistry::new();

        let mut input_id = None;
        let mut output_id = None;
        for (id, node) in self.snarl.node_ids() {
            match node {
                SonidoNode::Input => input_id = Some(id),
                SonidoNode::Output => output_id = Some(id),
                _ => {}
            }
        }
        let (Some(input), Some(output)) = (input_id, output_id) else {
            return;
        };

        // Drop the default passthrough wire so the chain reads linearly.
        self.snarl.disconnect(
            OutPinId {
                node: input,
                output: 0,
            },
            InPinId {
                node: output,
                input: 0,
            },
        );

        let mut prev = OutPinId {
            node: input,
            output: 0,
        };
        let mut x = 300.0;
        let mut first_effect = None;
        for effect_id in ["distortion", "delay", "reverb"] {
            let Some(desc) = registry.get(effect_id) else {
                continue;
            };
            let node = self.snarl.insert_node(
                egui::pos2(x, 200.0),
                SonidoNode::Effect {
                    effect_id: desc.id,
                    name: desc.name,
                    category: desc.category,
                    descriptors: collect_descriptors(desc.id, 48000.0),
                    smoothing: collect_smoothing(desc.id, 48000.0),
                },
            );
            first_effect.get_or_insert(node);
            self.snarl.connect(prev, InPinId { node, input: 0 });
            prev = OutPinId { node, output: 0 };
            x += 220.0;
        }
        self.snarl.connect(
            prev,
            InPinId {
                node: output,
                input: 0,
            },
        );
        // Open with the first effect selected so its param panel is visible.
        self.selected_node = first_effect;
        self.topology_changed = true;
    }

    /// Weld the Input/Output pins to the left/right screen walls.
    ///
    /// Called at the start of each frame. When a viewport transform has been
    /// captured (every frame after the first), each I/O node is positioned so
    /// its pin lands on the inner edge of its wall bar at vertical center —
    /// converting the wall's screen point into graph space via the live
    /// transform, so the pin stays welded under any pan/zoom. Before the first
    /// frame renders (no transform yet) it falls back to anchoring the I/O to
    /// the effect bounding box.
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

        let (input_pos, output_pos) = if let Some(vp) = &self.last_viewport {
            let y = vp.rect.center().y;
            let input_screen = egui::pos2(vp.rect.left() + IO_PIN_INSET, y);
            let output_screen = egui::pos2(vp.rect.right() - IO_PIN_INSET, y);
            (
                vp.screen_pos_to_graph(input_screen),
                vp.screen_pos_to_graph(output_screen),
            )
        } else if effect_count > 0 {
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

    /// Re-flow effect/structural nodes left→right by dependency depth.
    ///
    /// Each node lands in the column matching its longest path from an Input,
    /// and nodes sharing a column stack vertically — so the signal path reads
    /// left to right and nodes never overlap. Input/Output are left to
    /// [`pin_io_nodes`](Self::pin_io_nodes), which anchors them to the bounding
    /// box. Called only after a *user* topology edit (never on session restore,
    /// which preserves saved positions).
    fn auto_arrange(&mut self) {
        use std::collections::HashMap;

        // Adjacency + in-degree over the current snarl nodes.
        let mut adj: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        let mut indeg: HashMap<NodeId, usize> = HashMap::new();
        for (id, _) in self.snarl.node_ids() {
            adj.entry(id).or_default();
            indeg.entry(id).or_insert(0);
        }
        for (out, inp) in self.snarl.wires() {
            if out.node == inp.node {
                continue;
            }
            adj.entry(out.node).or_default().push(inp.node);
            *indeg.entry(inp.node).or_insert(0) += 1;
        }

        // Longest-path layering (Kahn). Any node left in a cycle keeps depth 0;
        // `compile_to_engine` rejects cycles, so layout only needs best-effort.
        let mut depth: HashMap<NodeId, usize> = HashMap::new();
        let mut remaining = indeg.clone();
        let mut queue: Vec<NodeId> = remaining
            .iter()
            .filter(|&(_, &d)| d == 0)
            .map(|(&n, _)| n)
            .collect();
        while let Some(n) = queue.pop() {
            let dn = depth.get(&n).copied().unwrap_or(0);
            for &c in &adj[&n] {
                if dn + 1 > depth.get(&c).copied().unwrap_or(0) {
                    depth.insert(c, dn + 1);
                }
                if let Some(r) = remaining.get_mut(&c) {
                    *r -= 1;
                    if *r == 0 {
                        queue.push(c);
                    }
                }
            }
        }

        const COL_SPACING: f32 = 170.0;
        const ROW_SPACING: f32 = 80.0;

        // Center the chain on the current viewport center (in graph space), so a
        // freshly arranged chain sits centered between the welded I/O walls
        // instead of pushed off to one side. Falls back to a fixed point before
        // the first frame has captured a transform.
        let (center_x, center_y) = self
            .last_viewport
            .as_ref()
            .map(|vp| {
                let c = vp.screen_pos_to_graph(vp.rect.center());
                (c.x, c.y)
            })
            .unwrap_or((380.0, 200.0));

        // Pre-count rows per column so each column's stack centers vertically.
        let max_col = depth.values().copied().max().unwrap_or(0);
        let start_x = center_x - (max_col as f32 * COL_SPACING) / 2.0;
        let mut col_rows: HashMap<usize, usize> = HashMap::new();
        for (id, _) in self.snarl.node_ids() {
            if matches!(self.snarl[id], SonidoNode::Input | SonidoNode::Output) {
                continue;
            }
            *col_rows
                .entry(depth.get(&id).copied().unwrap_or(0))
                .or_insert(0) += 1;
        }

        let mut rows_at: HashMap<usize, usize> = HashMap::new();
        let ids: Vec<NodeId> = self.snarl.node_ids().map(|(id, _)| id).collect();
        self.arrange_targets.clear();
        for id in ids {
            // I/O nodes are anchored by `pin_io_nodes`, not laid out here.
            if matches!(self.snarl[id], SonidoNode::Input | SonidoNode::Output) {
                continue;
            }
            let col = depth.get(&id).copied().unwrap_or(0);
            let rows_in_col = col_rows.get(&col).copied().unwrap_or(1);
            let row = rows_at.entry(col).or_insert(0);
            let pos = egui::pos2(
                start_x + col as f32 * COL_SPACING,
                center_y + (*row as f32 - (rows_in_col as f32 - 1.0) / 2.0) * ROW_SPACING,
            );
            *row += 1;
            // Record the target; `animate_arrange` eases toward it each frame.
            self.arrange_targets.insert(id, pos);
        }
    }

    /// Ease node positions a fraction of the way toward their re-flow targets,
    /// snapping when within half a pixel. Keeps the layout tidy without the
    /// jarring teleport of an instant rearrange.
    fn animate_arrange(&mut self, ctx: &egui::Context) {
        if self.arrange_targets.is_empty() {
            return;
        }
        let mut settled: Vec<NodeId> = Vec::new();
        for (&id, &target) in &self.arrange_targets {
            if let Some(info) = self.snarl.get_node_info_mut(id) {
                let delta = target - info.pos;
                if delta.length() < 0.5 {
                    info.pos = target;
                    settled.push(id);
                } else {
                    info.pos += delta * 0.25;
                }
            } else {
                settled.push(id); // node was removed — drop its target
            }
        }
        for id in settled {
            self.arrange_targets.remove(&id);
        }
        if !self.arrange_targets.is_empty() {
            ctx.request_repaint();
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
        self.animate_arrange(ui.ctx());
        let theme = SonidoTheme::get(ui.ctx());
        let mut click_handled = false;
        let mut needs_arrange = false;
        let mut captured_viewport = None;
        let mut viewer = SonidoViewer {
            selected_node: &mut self.selected_node,
            click_handled: &mut click_handled,
            topology_changed: &mut self.topology_changed,
            needs_arrange: &mut needs_arrange,
            theme,
            slot_activity: &self.slot_activity,
            slot_peaks: &self.slot_peaks,
            captured_viewport: &mut captured_viewport,
        };
        self.snarl
            .show(&mut viewer, &self.style, "sonido_graph", ui);

        // Keep this frame's transform for next frame's I/O-pin welding.
        if captured_viewport.is_some() {
            self.last_viewport = captured_viewport;
        }

        // Re-flow the graph after a user topology edit so nodes never overlap
        // and the signal path reads left→right. Applied next frame.
        if needs_arrange {
            self.auto_arrange();
        }

        // Click on empty space deselects — only within the graph area, and only
        // when the press isn't over a floating overlay. The rect check excludes
        // the effect panel below the graph; the `is_pointer_over_area` check
        // excludes the foreground touch overlays (the node action bar, FAB,
        // palette) — without it, tapping Duplicate/Remove deselected the node
        // and hid the action bar before the button could act.
        if !click_handled
            && ui.input(|i| i.pointer.primary_pressed())
            && !ui.ctx().is_pointer_over_area()
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

    /// Count of [`SonidoNode::Effect`] nodes currently in the graph.
    ///
    /// Used by the Daisy eligibility badge in the status bar: the Hothouse
    /// pedal supports up to 3 effects in a single-chain topology.
    pub fn effect_node_count(&self) -> usize {
        self.snarl
            .node_ids()
            .filter(|(_, node)| matches!(node, SonidoNode::Effect { .. }))
            .count()
    }

    /// The graph's effect nodes in chain-slot order.
    ///
    /// Slot `i` in the parameter bridge is `effect_node_ids()[i]` — the i-th
    /// `Effect` node in `node_ids()` order, the same order [`capture_session`]
    /// and the selected-slot mapping use. The `NodeId`s are stable across
    /// reordering, so the morph keys its per-effect endpoints on them and never
    /// desyncs when the chain changes.
    ///
    /// [`capture_session`]: Self::capture_session
    pub fn effect_node_ids(&self) -> Vec<NodeId> {
        self.snarl
            .node_ids()
            .filter(|(_, node)| matches!(node, SonidoNode::Effect { .. }))
            .map(|(id, _)| id)
            .collect()
    }

    /// Add an effect node by registry id, splicing it into the nearest wire.
    ///
    /// The add-button path (phone "+"-FAB and desktop "+ EFFECT" both route
    /// here; egui-snarl's own add menu is right-click-only, with no touch
    /// equivalent). The button carries no positional intent, so the new effect
    /// is always appended as the final pre-output node via
    /// [`append_before_output`]: every wire currently feeding the Output is
    /// rerouted through the new effect, which then becomes the sole node
    /// reaching the Output. (Right-click-on-wire keeps its positional splice.)
    /// Selects the new node so its param panel opens immediately.
    pub fn add_effect_node(&mut self, effect_id: &str) {
        let registry = EffectRegistry::new();
        let Some(desc) = registry.get(effect_id) else {
            return;
        };
        let pos = self.nodes_centroid();
        let new_id = self.snarl.insert_node(
            pos,
            SonidoNode::Effect {
                effect_id: desc.id,
                name: desc.name,
                category: desc.category,
                descriptors: collect_descriptors(desc.id, 48000.0),
                smoothing: collect_smoothing(desc.id, 48000.0),
            },
        );
        append_before_output(&mut self.snarl, new_id);
        self.selected_node = Some(new_id);
        self.topology_changed = true;
        self.auto_arrange();
    }

    /// Remove a node (touch action-bar / Delete-key path; mirrors right-click).
    pub fn remove_node(&mut self, node: NodeId) {
        if self.selected_node == Some(node) {
            self.selected_node = None;
        }
        self.snarl.remove_node(node);
        ensure_output_connected(&mut self.snarl);
        self.topology_changed = true;
        self.auto_arrange();
    }

    /// Duplicate a node (touch action-bar path; mirrors the right-click Duplicate).
    pub fn duplicate_node(&mut self, node: NodeId) {
        let original = self.snarl[node].clone();
        let base = self
            .snarl
            .get_node_info(node)
            .map_or(egui::pos2(0.0, 0.0), |n| n.pos);
        let new_id = self
            .snarl
            .insert_node(base + egui::vec2(30.0, 30.0), original);
        self.selected_node = Some(new_id);
        self.topology_changed = true;
        self.auto_arrange();
    }

    /// Whether the currently selected node is an editable effect (not I/O).
    pub fn selected_is_effect(&self) -> bool {
        self.selected_node
            .map(|n| matches!(self.snarl[n], SonidoNode::Effect { .. }))
            .unwrap_or(false)
    }

    /// Centroid of all node positions — a sensible fallback insert anchor.
    fn nodes_centroid(&self) -> egui::Pos2 {
        let mut sum = egui::Vec2::ZERO;
        let mut count = 0.0;
        for (id, _) in self.snarl.node_ids() {
            if let Some(info) = self.snarl.get_node_info(id) {
                sum += info.pos.to_vec2();
                count += 1.0;
            }
        }
        if count > 0.0 {
            (sum / count).to_pos2()
        } else {
            egui::pos2(300.0, 200.0)
        }
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
    /// Walks all nodes and wires in the Snarl graph and bundles everything into
    /// a serializable session. Each effect's A parameters come from morph
    /// snapshot A (or the live bridge when no snapshot is captured) and its B
    /// parameters from morph snapshot B; the macro layer and morph
    /// behaviour/position ride along via [`PerformanceCapture`].
    pub fn capture_session(
        &self,
        bridge: &dyn sonido_gui_core::ParamBridge,
        input_gain: f32,
        master_volume: f32,
        perf: crate::session::PerformanceCapture,
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
                // A snapshot: morph snapshot A if captured, else the live bridge.
                let params_a: Vec<f32> = perf
                    .morph_a
                    .as_ref()
                    .and_then(|s| s.slots.get(effect_slot))
                    .map(|s| s.values.clone())
                    .unwrap_or_else(|| {
                        (0..param_count)
                            .map(|i| bridge.get(slot, ParamIndex(i)))
                            .collect()
                    });
                // B snapshot: morph snapshot B if captured, else empty (mirrors A).
                let params_b: Vec<f32> = perf
                    .morph_b
                    .as_ref()
                    .and_then(|s| s.slots.get(effect_slot))
                    .map(|s| s.values.clone())
                    .unwrap_or_default();
                params.insert(
                    idx,
                    EffectState {
                        effect_id: effect_id.clone(),
                        params: params_a,
                        params_b,
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
            macros: perf.macros,
            macro_positions: perf.macro_positions,
            morph: perf.morph,
            morph_position: perf.morph_position,
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
        // Saved positions are authoritative — discard any in-flight re-flow.
        self.arrange_targets.clear();
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
    /// Set to `true` when a *user* topology edit should re-flow the layout.
    /// Distinct from `topology_changed` so session restore (which also marks
    /// the topology changed, to recompile) does not discard saved positions.
    needs_arrange: &'a mut bool,
    /// Arcade CRT theme snapshot for palette access.
    theme: SonidoTheme,
    /// Per-effect-slot activity level (0.0--1.0) for LED indicators.
    slot_activity: &'a [f32],
    /// Per-effect-slot L/R peak levels (0.0--1.0) for inline mini meters.
    slot_peaks: &'a [(f32, f32)],
    /// Receives this frame's pan/zoom transform, captured in `draw_background`,
    /// so [`GraphView`] can weld the I/O pins to the screen walls next frame.
    captured_viewport: &'a mut Option<Viewport>,
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
    /// Capture this frame's pan/zoom transform (so the I/O pins can be welded to
    /// the screen walls), then draw the default background.
    fn draw_background(
        &mut self,
        background: Option<&BackgroundPattern>,
        viewport: &Viewport,
        snarl_style: &SnarlStyle,
        style: &Style,
        painter: &Painter,
        _snarl: &Snarl<SonidoNode>,
    ) {
        // Viewport isn't Clone, but its fields are public Copy values.
        *self.captured_viewport = Some(Viewport {
            rect: viewport.rect,
            scale: viewport.scale,
            offset: viewport.offset,
        });
        if let Some(background) = background {
            background.draw(viewport, snarl_style, style, painter);
        }
    }

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

            // Plain label — selection is handled via final_node_rect().
            // Count only *visible* params so the badge matches the editor panel,
            // which skips HIDDEN/READ_ONLY params (e.g. the tuner's outputs).
            let visible_params = descriptors
                .iter()
                .filter(|d| {
                    !d.flags.contains(ParamFlags::HIDDEN)
                        && !d.flags.contains(ParamFlags::READ_ONLY)
                })
                .count();
            // Compact: the category is already conveyed by the node color and
            // header, so the body just needs the param count. Keeps nodes narrow
            // enough to fit several across a phone canvas without overlap.
            let body_text = format!("{visible_params} params");
            let color = if is_selected { accent } else { dim };
            let body_resp = ui.label(
                RichText::new(body_text)
                    .font(FontId::monospace(9.0))
                    .color(color),
            );

            // Inline L/R peak meters — thin colored strips at the bottom of the node.
            let (peak_l, peak_r) = self.slot_peaks.get(slot_idx).copied().unwrap_or((0.0, 0.0));

            // Size the strips to the body label, NOT `ui.available_width()`:
            // inside a snarl node (which sizes to its content) grabbing the
            // available width creates a layout feedback loop — the node balloons
            // to the whole canvas and jitters frame-to-frame, badly on mobile.
            let meter_height = 4.0_f32;
            let meter_w = body_resp.rect.width().max(48.0);
            let (meter_rect, _) = ui.allocate_exact_size(
                egui::vec2(meter_w, meter_height * 2.0 + 2.0),
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
                            let new_id = snarl.insert_node(
                                pos,
                                SonidoNode::Effect {
                                    effect_id: desc.id,
                                    name: desc.name,
                                    category: desc.category,
                                    descriptors,
                                    smoothing,
                                },
                            );
                            splice_at_nearest(snarl, new_id, pos);
                            *self.topology_changed = true;
                            *self.needs_arrange = true;
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
                        let new_id = snarl.insert_node(
                            pos,
                            SonidoNode::Effect {
                                effect_id: desc.id,
                                name: desc.name,
                                category: desc.category,
                                descriptors,
                                smoothing,
                            },
                        );
                        splice_at_nearest(snarl, new_id, pos);
                        *self.topology_changed = true;
                        *self.needs_arrange = true;
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
            ensure_output_connected(snarl);
            *self.topology_changed = true;
            *self.needs_arrange = true;
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
            *self.needs_arrange = true;
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
        // Any input may receive multiple wires. Fan-in is summed implicitly when
        // the graph compiles (`compile_to_engine` auto-inserts a Merge for any
        // node with >1 source), so the editor never materializes a Merge node —
        // wires simply converge on the single input connector.
        snarl.connect(from.id, to.id);
        *self.topology_changed = true;
        *self.needs_arrange = true;
    }

    fn disconnect(&mut self, from: &OutPin, to: &InPin, snarl: &mut Snarl<SonidoNode>) {
        snarl.disconnect(from.id, to.id);
        *self.topology_changed = true;
        *self.needs_arrange = true;
    }

    fn drop_outputs(&mut self, pin: &OutPin, snarl: &mut Snarl<SonidoNode>) {
        snarl.drop_outputs(pin.id);
        *self.topology_changed = true;
        *self.needs_arrange = true;
    }

    fn drop_inputs(&mut self, pin: &InPin, snarl: &mut Snarl<SonidoNode>) {
        snarl.drop_inputs(pin.id);
        *self.topology_changed = true;
        *self.needs_arrange = true;
    }
}

/// Keep the Output reachable: if nothing feeds the Output node (e.g. after the
/// last node in the chain is removed), auto-connect the node nearest the output
/// — the rightmost in the left→right flow — so the graph never goes silently
/// disconnected. No-op if the Output already has an incoming wire.
fn ensure_output_connected(snarl: &mut Snarl<SonidoNode>) {
    let mut output = None;
    for (id, node) in snarl.node_ids() {
        if matches!(node, SonidoNode::Output) {
            output = Some(id);
        }
    }
    let Some(output) = output else { return };
    if snarl.wires().any(|(_, inp)| inp.node == output) {
        return;
    }
    // Rightmost non-Output node = nearest the output in signal-flow order.
    let mut best: Option<NodeId> = None;
    let mut best_x = f32::NEG_INFINITY;
    for (id, node) in snarl.node_ids() {
        if matches!(node, SonidoNode::Output) {
            continue;
        }
        if let Some(info) = snarl.get_node_info(id)
            && info.pos.x > best_x
        {
            best_x = info.pos.x;
            best = Some(id);
        }
    }
    if let Some(node) = best {
        snarl.connect(
            OutPinId { node, output: 0 },
            InPinId {
                node: output,
                input: 0,
            },
        );
    }
}

/// Perpendicular distance from a point to a line segment `a`–`b`.
fn dist_point_to_segment(p: egui::Pos2, a: egui::Pos2, b: egui::Pos2) -> f32 {
    let ab = b - a;
    let len_sq = ab.length_sq();
    if len_sq <= f32::EPSILON {
        return (p - a).length();
    }
    let t = ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    (p - (a + ab * t)).length()
}

/// Splice a freshly added node into the wire nearest the insertion point.
///
/// The new node would otherwise sit disconnected yet *look* wired (the
/// auto-flow places it in the signal lane). Finding the closest existing wire
/// to where the user right-clicked and inserting the node into it makes the
/// appearance honest and puts the effect exactly where they aimed. Falls back
/// to [`append_before_output`] when there is no wire to splice into.
fn splice_at_nearest(snarl: &mut Snarl<SonidoNode>, new_id: NodeId, pos: egui::Pos2) {
    let wires: Vec<(OutPinId, InPinId)> = snarl.wires().collect();
    let mut best: Option<(OutPinId, InPinId)> = None;
    let mut best_d = f32::INFINITY;
    for (out_pin, in_pin) in wires {
        if out_pin.node == new_id || in_pin.node == new_id {
            continue;
        }
        let (Some(a), Some(b)) = (
            snarl.get_node_info(out_pin.node).map(|n| n.pos),
            snarl.get_node_info(in_pin.node).map(|n| n.pos),
        ) else {
            continue;
        };
        let d = dist_point_to_segment(pos, a, b);
        if d < best_d {
            best_d = d;
            best = Some((out_pin, in_pin));
        }
    }

    if let Some((out_pin, in_pin)) = best {
        snarl.disconnect(out_pin, in_pin);
        snarl.connect(
            out_pin,
            InPinId {
                node: new_id,
                input: 0,
            },
        );
        snarl.connect(
            OutPinId {
                node: new_id,
                output: 0,
            },
            in_pin,
        );
    } else {
        append_before_output(snarl, new_id);
    }
}

/// Splice a freshly added node into the chain just before the Output.
///
/// Used as the fallback when there is no existing wire to splice into (e.g. the
/// graph is empty). Reroutes everything feeding the Output through the new node,
/// or wires Input → new → Output if the Output was unconnected.
fn append_before_output(snarl: &mut Snarl<SonidoNode>, new_id: NodeId) {
    let mut output = None;
    let mut input = None;
    for (id, n) in snarl.node_ids() {
        match n {
            SonidoNode::Output => output = Some(id),
            SonidoNode::Input => input = Some(id),
            _ => {}
        }
    }
    let Some(output) = output else { return };

    let feeders: Vec<OutPinId> = snarl
        .wires()
        .filter(|(_, inp)| inp.node == output)
        .map(|(out, _)| out)
        .collect();

    if feeders.is_empty() {
        if let Some(input) = input {
            snarl.connect(
                OutPinId {
                    node: input,
                    output: 0,
                },
                InPinId {
                    node: new_id,
                    input: 0,
                },
            );
        }
    } else {
        for src in feeders {
            snarl.disconnect(
                src,
                InPinId {
                    node: output,
                    input: 0,
                },
            );
            snarl.connect(
                src,
                InPinId {
                    node: new_id,
                    input: 0,
                },
            );
        }
    }
    snarl.connect(
        OutPinId {
            node: new_id,
            output: 0,
        },
        InPinId {
            node: output,
            input: 0,
        },
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

#[cfg(test)]
mod tests {
    use super::*;

    fn output_has_input(gv: &GraphView) -> bool {
        let mut output = None;
        for (id, node) in gv.snarl.node_ids() {
            if matches!(node, SonidoNode::Output) {
                output = Some(id);
            }
        }
        match output {
            Some(out) => gv.snarl.wires().any(|(_, inp)| inp.node == out),
            None => false,
        }
    }

    fn first_effect(gv: &GraphView) -> Option<NodeId> {
        for (id, node) in gv.snarl.node_ids() {
            if matches!(node, SonidoNode::Effect { .. }) {
                return Some(id);
            }
        }
        None
    }

    fn io_ids(gv: &GraphView) -> (NodeId, NodeId) {
        let (mut input, mut output) = (None, None);
        for (id, node) in gv.snarl.node_ids() {
            match node {
                SonidoNode::Input => input = Some(id),
                SonidoNode::Output => output = Some(id),
                _ => {}
            }
        }
        (input.expect("input"), output.expect("output"))
    }

    fn insert_effect(gv: &mut GraphView, id: &str) -> NodeId {
        let registry = EffectRegistry::new();
        let desc = registry.get(id).expect("effect exists");
        gv.snarl.insert_node(
            egui::pos2(0.0, 0.0),
            SonidoNode::Effect {
                effect_id: desc.id,
                name: desc.name,
                category: desc.category,
                descriptors: collect_descriptors(desc.id, 48000.0),
                smoothing: collect_smoothing(desc.id, 48000.0),
            },
        )
    }

    #[test]
    fn restore_session_preserves_effect_order_for_morph_alignment() {
        // The morph keys each effect's A/B poses to `effect_node_ids()`, and
        // `restore_performance` realigns saved poses (in session-node order) to
        // that. If a load reordered the effects, poses would land on the wrong
        // effect. Lock the invariant: after restore, effect_node_ids() is in the
        // session's Effect order.
        use crate::session::{EffectState, Session, SessionNode, SessionNodeEntry};
        use std::collections::HashMap;

        let effect = |id: &str| SessionNodeEntry {
            node: SessionNode::Effect {
                effect_id: id.into(),
            },
            pos: [0.0, 0.0],
        };
        let io = |node| SessionNodeEntry {
            node,
            pos: [0.0, 0.0],
        };
        let nodes = vec![
            io(SessionNode::Input),
            effect("distortion"),
            effect("reverb"),
            effect("delay"),
            io(SessionNode::Output),
        ];
        let mut params = HashMap::new();
        for (idx, id) in [(1usize, "distortion"), (2, "reverb"), (3, "delay")] {
            params.insert(
                idx,
                EffectState {
                    effect_id: id.into(),
                    params: vec![idx as f32], // A pose, distinct per effect
                    params_b: vec![idx as f32 + 10.0], // B pose, distinct
                    bypassed: false,
                },
            );
        }
        let session = Session {
            version: Session::VERSION,
            nodes,
            wires: vec![],
            params,
            input_gain: 0.0,
            master_volume: 0.0,
            macros: std::array::from_fn(|_| sonido_patch::MacroDef::default()),
            macro_positions: [0.0; sonido_patch::NUM_MACROS],
            morph: sonido_patch::MorphConfig::default(),
            morph_position: 0.0,
        };

        let mut gv = GraphView::new();
        gv.restore_session(&session, &EffectRegistry::new());

        let effect_ids: Vec<&str> = gv
            .effect_node_ids()
            .iter()
            .map(|id| match gv.snarl.get_node(*id) {
                Some(SonidoNode::Effect { effect_id, .. }) => *effect_id,
                _ => "?",
            })
            .collect();
        assert_eq!(effect_ids, ["distortion", "reverb", "delay"]);
    }

    #[test]
    fn add_button_appends_last_absorbing_output_feeders() {
        // Build a split that merges at the Output: Input → A → Output and
        // Input → B → Output (two feeders into Output).
        let mut gv = GraphView::new();
        let (input, output) = io_ids(&gv);
        gv.snarl.disconnect(
            OutPinId {
                node: input,
                output: 0,
            },
            InPinId {
                node: output,
                input: 0,
            },
        );
        let a = insert_effect(&mut gv, "distortion");
        let b = insert_effect(&mut gv, "delay");
        for n in [a, b] {
            gv.snarl.connect(
                OutPinId {
                    node: input,
                    output: 0,
                },
                InPinId { node: n, input: 0 },
            );
            gv.snarl.connect(
                OutPinId { node: n, output: 0 },
                InPinId {
                    node: output,
                    input: 0,
                },
            );
        }
        assert_eq!(
            gv.snarl
                .wires()
                .filter(|(_, inp)| inp.node == output)
                .count(),
            2,
            "two feeders into Output before the add"
        );

        // Add via the add button: the new effect must become the SOLE feeder of
        // Output, with both prior feeders rerouted into it.
        gv.add_effect_node("reverb");
        let x = gv.selected_node.expect("new node selected");

        let out_feeders: Vec<NodeId> = gv
            .snarl
            .wires()
            .filter(|(_, inp)| inp.node == output)
            .map(|(o, _)| o.node)
            .collect();
        assert_eq!(
            out_feeders,
            vec![x],
            "added effect is the sole Output feeder"
        );

        let x_feeders: HashSet<NodeId> = gv
            .snarl
            .wires()
            .filter(|(_, inp)| inp.node == x)
            .map(|(o, _)| o.node)
            .collect();
        assert!(
            x_feeders.contains(&a) && x_feeders.contains(&b),
            "both prior feeders now merge into the added effect"
        );
    }

    #[test]
    fn pin_io_nodes_welds_input_left_output_right() {
        let mut gv = GraphView::new();
        // Simulate a captured transform: an 800x400 canvas, no pan, unity scale.
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 400.0));
        gv.last_viewport = Some(Viewport {
            rect,
            scale: 1.0,
            offset: egui::vec2(0.0, 0.0),
        });
        gv.pin_io_nodes();

        let (input, output) = io_ids(&gv);
        let ip = gv.snarl.get_node_info(input).unwrap().pos;
        let op = gv.snarl.get_node_info(output).unwrap().pos;
        assert!(ip.x < op.x, "input sits left of output in graph space");

        // The graph positions must map back onto the wall inner edges.
        let vp = gv.last_viewport.as_ref().unwrap();
        assert!((vp.graph_pos_to_screen(ip).x - (rect.left() + IO_PIN_INSET)).abs() < 1.0);
        assert!((vp.graph_pos_to_screen(op).x - (rect.right() - IO_PIN_INSET)).abs() < 1.0);
    }

    #[test]
    fn removing_last_node_keeps_output_connected() {
        let mut gv = GraphView::new(); // Input → Output
        gv.add_effect_node("distortion"); // Input → Distortion → Output
        assert_eq!(gv.effect_node_count(), 1);
        assert!(output_has_input(&gv));

        // Remove the only effect: the Output must auto-reconnect (to Input),
        // never left silently orphaned.
        let effect = first_effect(&gv).expect("effect node present");
        gv.remove_node(effect);

        assert_eq!(gv.effect_node_count(), 0);
        assert!(
            output_has_input(&gv),
            "Output was orphaned after removing the last node"
        );
    }
}
