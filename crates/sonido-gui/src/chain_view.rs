//! Effect chain visualization, reordering, and dynamic add/remove.
//!
//! The [`ChainView`] renders the pedalboard strip, letting users click to
//! select, double-click to bypass, drag to reorder, right-click to remove,
//! and press "+" to add new effects.

use crate::atomic_param_bridge::AtomicParamBridge;
use egui::{Color32, Response, ScrollArea, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use sonido_gui_core::{ParamBridge, SlotIndex};
use sonido_registry::EffectRegistry;
use std::sync::Arc;

/// Chain view state for drag-and-drop, selection, and pending commands.
pub struct ChainView {
    bridge: Arc<AtomicParamBridge>,
    dragging: Option<usize>,
    drag_offset: f32,
    selected: Option<SlotIndex>,
    /// Effect ID to add (set by "+" menu, polled by app).
    pending_add: Option<&'static str>,
    /// Slot index to remove (set by context menu, polled by app).
    pending_remove: Option<SlotIndex>,
}

impl ChainView {
    /// Create a new chain view backed by the given parameter bridge.
    pub fn new(bridge: Arc<AtomicParamBridge>) -> Self {
        Self {
            bridge,
            dragging: None,
            drag_offset: 0.0,
            selected: None,
            pending_add: None,
            pending_remove: None,
        }
    }

    pub fn set_bridge(&mut self, bridge: Arc<AtomicParamBridge>) {
        self.bridge = bridge;
        self.selected = None;
        self.dragging = None;
        self.drag_offset = 0.0;
    }

    /// Get the currently selected slot index.
    pub fn selected(&self) -> Option<SlotIndex> {
        self.selected
    }

    /// Set the selected slot.
    pub fn set_selected(&mut self, slot: SlotIndex) {
        self.selected = Some(slot);
    }

    /// Clear the current selection.
    pub fn clear_selection(&mut self) {
        self.selected = None;
    }

    /// Take the pending add request (if any), clearing it.
    pub fn take_pending_add(&mut self) -> Option<&'static str> {
        self.pending_add.take()
    }

    /// Take the pending remove request (if any), clearing it.
    pub fn take_pending_remove(&mut self) -> Option<SlotIndex> {
        self.pending_remove.take()
    }

    pub fn ui(
        &mut self,
        ui: &mut Ui,
        bridge: &dyn ParamBridge,
        registry: &EffectRegistry,
    ) -> Option<SlotIndex> {
        let order = self.bridge.get_order();
        let slot_count = bridge.slot_count();

        // Clear selection if the selected slot was removed
        if let Some(sel) = self.selected
            && sel.0 >= slot_count
        {
            self.selected = None;
        }
        let effect_width = 70.0;
        let spacing = 8.0;
        let arrow_width = 20.0;
        let add_button_width = 36.0;

        // Only show slots that exist in both the order and the bridge
        let visible: Vec<usize> = order.iter().copied().filter(|&i| i < slot_count).collect();

        let total_width = if visible.is_empty() {
            add_button_width
        } else {
            visible.len() as f32 * (effect_width + spacing + arrow_width) - arrow_width
                + spacing
                + add_button_width
        };

        // Scroll horizontally when the chain overflows the available width
        let available_width = ui.available_width();
        let needs_scroll = total_width > available_width;

        ScrollArea::horizontal().auto_shrink(true).show(ui, |ui| {
            ui.horizontal(|ui| {
                // Center the chain when it fits
                if !needs_scroll {
                    let available = ui.available_width();
                    if available > total_width {
                        ui.add_space((available - total_width) / 2.0);
                    }
                }

                for (pos, &slot_raw) in visible.iter().enumerate() {
                    let slot_idx = SlotIndex(slot_raw);
                    let effect_id = bridge.effect_id(slot_idx);
                    let short_name = registry
                        .descriptor(effect_id)
                        .map(|d| d.short_name)
                        .unwrap_or("???");

                    let is_selected = self.selected == Some(slot_idx);
                    let is_bypassed = bridge.is_bypassed(slot_idx);
                    let is_being_dragged = self.dragging == Some(pos);

                    // Drop target indicator
                    if self.dragging.is_some() && !is_being_dragged {
                        if let Some(pointer_pos) = ui.input(|i| i.pointer.latest_pos()) {
                            let (pedal_rect, _) =
                                ui.allocate_exact_size(vec2(effect_width, 50.0), Sense::hover());
                            if pedal_rect.contains(pointer_pos) {
                                ui.painter().rect_filled(
                                    pedal_rect.expand(2.0),
                                    6.0,
                                    Color32::from_white_alpha(30),
                                );
                            }
                        }
                    }

                    let (response, close_clicked) = self.effect_pedal(
                        ui,
                        short_name,
                        is_selected,
                        is_bypassed,
                        slot_idx,
                        bridge,
                        is_being_dragged,
                    );

                    // Close button → remove; otherwise click → select
                    if close_clicked {
                        self.pending_remove = Some(slot_idx);
                    } else if response.clicked() {
                        self.selected = Some(slot_idx);
                    }

                    // Right-click → context menu
                    response.context_menu(|ui| {
                        if ui.button("Remove Effect").clicked() {
                            self.pending_remove = Some(slot_idx);
                            ui.close_menu();
                        }
                        if ui
                            .button(if is_bypassed { "Enable" } else { "Bypass" })
                            .clicked()
                        {
                            bridge.set_bypassed(slot_idx, !is_bypassed);
                            ui.close_menu();
                        }
                    });

                    // Handle drag start
                    if response.drag_started() {
                        self.dragging = Some(pos);
                    }

                    // On drop, move the effect
                    if let Some(dragged_pos) = self.dragging {
                        if response.hovered() && ui.input(|i| i.pointer.any_released()) {
                            if dragged_pos != pos {
                                self.bridge.move_effect(dragged_pos, pos);
                            }
                        }
                    }

                    // Arrow between effects (except last)
                    if pos < visible.len() - 1 {
                        ui.add_space(spacing / 2.0);
                        self.draw_arrow(ui, arrow_width);
                        ui.add_space(spacing / 2.0);
                    }
                }

                // Handle drop outside any pedal
                if self.dragging.is_some() {
                    if ui.input(|i| i.pointer.any_released()) {
                        self.dragging = None;
                    }
                }

                // "+" button to add new effects
                ui.add_space(spacing);
                self.add_button(ui, registry, add_button_width);
            });
        });

        self.selected
    }
    /// Draw a single effect pedal in the chain.
    ///
    /// Returns `(response, close_clicked)` where `close_clicked` is `true`
    /// when the user clicks the "×" removal glyph in the top-right corner.
    fn effect_pedal(
        &self,
        ui: &mut Ui,
        short_name: &str,
        is_selected: bool,
        is_bypassed: bool,
        slot_idx: SlotIndex,
        bridge: &dyn ParamBridge,
        is_being_dragged: bool,
    ) -> (Response, bool) {
        let size = vec2(70.0, 50.0);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();

            // When dragging, leave a ghost of the pedal behind
            let alpha = if is_being_dragged { 50 } else { 255 };

            // Pedal body color based on state
            let body_color = if is_selected {
                if is_bypassed {
                    Color32::from_rgb(50, 55, 60).to_opaque()
                } else {
                    Color32::from_rgb(55, 70, 65).to_opaque()
                }
            } else if is_bypassed {
                Color32::from_rgb(35, 35, 42).to_opaque()
            } else {
                Color32::from_rgb(45, 55, 52).to_opaque()
            };

            // Draw pedal body
            painter.rect_filled(
                rect,
                6.0,
                Color32::from_rgba_premultiplied(
                    body_color.r(),
                    body_color.g(),
                    body_color.b(),
                    alpha,
                ),
            );

            // Border (highlighted if selected)
            let border_color = if is_selected {
                Color32::from_rgb(100, 180, 255)
            } else {
                Color32::from_rgb(60, 60, 70)
            };
            painter.rect_stroke(
                rect,
                6.0,
                Stroke::new(
                    if is_selected { 2.0 } else { 1.0 },
                    Color32::from_rgba_premultiplied(
                        border_color.r(),
                        border_color.g(),
                        border_color.b(),
                        alpha,
                    ),
                ),
                StrokeKind::Inside,
            );

            // LED indicator
            let led_pos = pos2(rect.center().x, rect.top() + 12.0);
            let led_color = if !is_bypassed {
                Color32::from_rgb(100, 255, 100)
            } else {
                Color32::from_rgb(40, 50, 40)
            };
            painter.circle_filled(
                led_pos,
                4.0,
                Color32::from_rgba_premultiplied(led_color.r(), led_color.g(), led_color.b(), alpha),
            );
            if !is_bypassed {
                // Glow effect
                painter.circle_filled(
                    led_pos,
                    7.0,
                    Color32::from_rgba_premultiplied(
                        led_color.r(),
                        led_color.g(),
                        led_color.b(),
                        if is_being_dragged { 0 } else { 64 },
                    ),
                );
            }

            // Effect name
            let text_color = if is_bypassed {
                Color32::from_rgb(100, 100, 110)
            } else {
                Color32::from_rgb(200, 200, 210)
            };
            painter.text(
                pos2(rect.center().x, rect.bottom() - 12.0),
                egui::Align2::CENTER_CENTER,
                short_name,
                egui::FontId::proportional(11.0),
                Color32::from_rgba_premultiplied(
                    text_color.r(),
                    text_color.g(),
                    text_color.b(),
                    alpha,
                ),
            );

            // "×" close button on hover
            if response.hovered() && !is_being_dragged {
                painter.text(
                    pos2(rect.right() - 8.0, rect.top() + 8.0),
                    egui::Align2::CENTER_CENTER,
                    "\u{00d7}",
                    egui::FontId::proportional(10.0),
                    Color32::from_rgb(180, 80, 80),
                );
            }
        }

        // Check if click landed in the close zone (top-right 16×16)
        let close_clicked = response.clicked()
            && response
                .interact_pointer_pos()
                .is_some_and(|pos| pos.x > rect.right() - 16.0 && pos.y < rect.top() + 16.0);

        // Double-click to toggle bypass
        if response.double_clicked() {
            bridge.set_bypassed(slot_idx, !bridge.is_bypassed(slot_idx));
        }

        let response = response
            .on_hover_text("Click: select | Double-click: bypass | Drag: reorder | X: remove");
        (response, close_clicked)
    }

    /// Draw the "+" button and its popup menu for adding effects.
    fn add_button(&mut self, ui: &mut Ui, registry: &EffectRegistry, width: f32) {
        let size = vec2(width, 50.0);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let body_color = Color32::from_rgb(40, 45, 50);
            painter.rect_filled(rect, 6.0, body_color);
            painter.rect_stroke(
                rect,
                6.0,
                Stroke::new(1.0, Color32::from_rgb(60, 70, 80)),
                StrokeKind::Inside,
            );
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "+",
                egui::FontId::proportional(20.0),
                Color32::from_rgb(120, 140, 160),
            );
        }

        // Popup menu listing all available effects
        let popup_id = ui.make_persistent_id("add_effect_popup");
        if response.clicked() {
            ui.memory_mut(|mem| mem.toggle_popup(popup_id));
        }

        egui::popup_below_widget(
            ui,
            popup_id,
            &response,
            egui::PopupCloseBehavior::CloseOnClick,
            |ui| {
                ui.set_min_width(160.0);
                for desc in registry.all_effects() {
                    if ui.button(desc.name).clicked() {
                        self.pending_add = Some(desc.id);
                    }
                }
            },
        );
    }

    /// Draw an arrow between effects.
    fn draw_arrow(&self, ui: &mut Ui, width: f32) {
        let (rect, _response) = ui.allocate_exact_size(vec2(width, 50.0), Sense::hover());

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let center_y = rect.center().y;
            let arrow_color = Color32::from_rgb(80, 80, 90);

            // Line
            painter.line_segment(
                [
                    pos2(rect.left(), center_y),
                    pos2(rect.right() - 6.0, center_y),
                ],
                Stroke::new(2.0, arrow_color),
            );

            // Arrow head
            let tip = pos2(rect.right(), center_y);
            let back = 6.0;
            let spread = 4.0;
            painter.line_segment(
                [pos2(tip.x - back, tip.y - spread), tip],
                Stroke::new(2.0, arrow_color),
            );
            painter.line_segment(
                [pos2(tip.x - back, tip.y + spread), tip],
                Stroke::new(2.0, arrow_color),
            );
        }
    }
}
