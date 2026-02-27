//! Plugin GUI for the multi-effect chain.
//!
//! [`ChainEditor`] opens an egui window with a chain strip (effect pedals
//! with add/remove/reorder) and a parameter panel for the selected effect.
//! Reuses the same `EffectPanel` widgets as the standalone GUI.

use std::sync::Arc;

use baseview::WindowHandle;
use raw_window_handle::{HasRawWindowHandle, RawWindowHandle};

use sonido_gui_core::{
    ChainMutator,
    effects_ui::{EffectPanel, create_panel},
    param_bridge::{ParamBridge, SlotIndex},
};
use sonido_registry::EffectRegistry;

use crate::chain::param_bridge::ChainParamBridge;
use crate::chain::shared::{ChainCommand, ChainShared};
use crate::egui_bridge;
use crate::gui::PendingResize;

/// Wraps a [`RawWindowHandle`] for baseview's `open_parented`.
struct ParentWindow(RawWindowHandle);

#[allow(unsafe_code)]
// SAFETY: Same justification as crate::gui — CLAP spec guarantees parent outlives child.
unsafe impl HasRawWindowHandle for ParentWindow {
    fn raw_window_handle(&self) -> RawWindowHandle {
        self.0
    }
}

/// GUI state for the chain plugin editor.
struct GuiState {
    bridge: Arc<ChainParamBridge>,
    shared: ChainShared,
    registry: EffectRegistry,
    selected: Option<SlotIndex>,
    cached_panel: Option<(String, Box<dyn EffectPanel + Send + Sync>)>,
    pending_add: Option<&'static str>,
    pending_remove: Option<SlotIndex>,
}

/// Holds the baseview rendering window for the chain plugin.
///
/// Dropping this value closes the plugin GUI window.
pub struct ChainEditor {
    _window: WindowHandle,
}

impl ChainEditor {
    /// Opens an egui child window inside the host's parent window.
    pub fn open(
        parent_rwh: RawWindowHandle,
        shared: ChainShared,
        scale: f64,
        pending_resize: Arc<PendingResize>,
    ) -> Option<Self> {
        let bridge = Arc::new(ChainParamBridge::new(shared.clone()));
        let registry = EffectRegistry::new();

        let (width, height) = pending_resize.get();

        let state = GuiState {
            bridge,
            shared,
            registry,
            selected: None,
            cached_panel: None,
            pending_add: None,
            pending_remove: None,
        };

        let window = egui_bridge::open_parented(
            &ParentWindow(parent_rwh),
            "Sonido Chain".to_owned(),
            width,
            height,
            scale,
            pending_resize,
            state,
            |_ctx, _state| {},
            |ctx, state| {
                ctx.request_repaint_after(std::time::Duration::from_millis(33));

                // Process pending add/remove commands
                if let Some(id) = state.pending_add.take() {
                    state.shared.push_command(ChainCommand::Add {
                        effect_id: id.to_owned(),
                    });
                }
                if let Some(slot) = state.pending_remove.take() {
                    if state.selected == Some(slot) {
                        state.selected = None;
                        state.cached_panel = None;
                    }
                    state
                        .shared
                        .push_command(ChainCommand::Remove { slot: slot.0 });
                }

                egui::TopBottomPanel::top("chain_header").show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.heading("Sonido Chain");
                    });
                });

                // Chain strip
                egui::TopBottomPanel::top("chain_strip").show(ctx, |ui| {
                    ui.add_space(4.0);
                    render_chain_strip(ui, state);
                    ui.add_space(4.0);
                });

                // Selected effect panel
                egui::CentralPanel::default().show(ctx, |ui| {
                    if let Some(slot) = state.selected {
                        let effect_id = state.bridge.effect_id(slot);
                        if !effect_id.is_empty() {
                            // Rebuild panel if effect changed
                            let needs_rebuild = state
                                .cached_panel
                                .as_ref()
                                .is_none_or(|(id, _)| id != effect_id);

                            if needs_rebuild && let Some(panel) = create_panel(effect_id) {
                                state.cached_panel = Some((effect_id.to_owned(), panel));
                            }

                            if let Some((_, panel)) = &mut state.cached_panel {
                                egui::ScrollArea::vertical().show(ui, |ui| {
                                    panel.ui(ui, state.bridge.as_ref() as &dyn ParamBridge, slot);
                                });
                            }
                        }
                    } else {
                        ui.centered_and_justified(|ui| {
                            ui.label("Select an effect in the chain strip above");
                        });
                    }
                });
            },
        );

        Some(Self { _window: window })
    }
}

/// Render the chain strip with effect pedals and add/remove buttons.
///
/// Simplified version of the standalone `ChainView`, embedded directly
/// in the plugin GUI. Uses click-to-select instead of drag-and-drop.
fn render_chain_strip(ui: &mut egui::Ui, state: &mut GuiState) {
    let order = state.bridge.get_order();
    let slot_count = state.bridge.slot_count();

    // Clear selection if slot was removed
    if let Some(sel) = state.selected
        && sel.0 >= slot_count
    {
        state.selected = None;
        state.cached_panel = None;
    }

    ui.horizontal(|ui| {
        for (pos, &slot_raw) in order.iter().enumerate() {
            if slot_raw >= slot_count {
                continue;
            }
            let slot_idx = SlotIndex(slot_raw);
            let effect_id = state.bridge.effect_id(slot_idx);
            let short_name = state
                .registry
                .descriptor(effect_id)
                .map(|d| d.short_name)
                .unwrap_or("???");

            let is_selected = state.selected == Some(slot_idx);
            let is_bypassed = state.bridge.is_bypassed(slot_idx);

            // Pedal button
            let label = if is_bypassed {
                format!("[{short_name}]")
            } else {
                short_name.to_owned()
            };

            let button = egui::Button::new(&label)
                .min_size(egui::vec2(60.0, 36.0))
                .selected(is_selected);

            let resp = ui.add(button);

            if resp.clicked() {
                state.selected = Some(slot_idx);
                state.cached_panel = None; // force panel rebuild
            }

            if resp.double_clicked() {
                state.bridge.set_bypassed(slot_idx, !is_bypassed);
            }

            resp.context_menu(|ui| {
                if ui.button("Remove").clicked() {
                    state.pending_remove = Some(slot_idx);
                    ui.close_menu();
                }
                if ui
                    .button(if is_bypassed { "Enable" } else { "Bypass" })
                    .clicked()
                {
                    state.bridge.set_bypassed(slot_idx, !is_bypassed);
                    ui.close_menu();
                }
            });

            // Arrow between pedals
            if pos < order.len() - 1 {
                ui.label("\u{2192}");
            }
        }

        // "+" button
        let add_resp = ui.add(egui::Button::new("+").min_size(egui::vec2(30.0, 36.0)));

        let popup_id = ui.make_persistent_id("chain_add_popup");
        if add_resp.clicked() {
            ui.memory_mut(|mem| mem.toggle_popup(popup_id));
        }

        egui::popup_below_widget(
            ui,
            popup_id,
            &add_resp,
            egui::PopupCloseBehavior::CloseOnClick,
            |ui| {
                ui.set_min_width(160.0);
                for desc in state.registry.all_effects() {
                    if ui.button(desc.name).clicked() {
                        state.pending_add = Some(desc.id);
                    }
                }
            },
        );
    });
}
