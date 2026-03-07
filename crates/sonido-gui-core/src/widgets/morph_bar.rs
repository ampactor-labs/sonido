//! A/B morph crossfader widget.
//!
//! Provides a horizontal bar with A and B capture buttons flanking a
//! crossfade slider. Click a button to capture the current state into
//! that snapshot slot; right-click or double-click to recall it.

use egui::{Color32, Ui, vec2};

/// Response from the morph bar widget indicating which actions were triggered.
#[allow(clippy::struct_excessive_bools)]
pub struct MorphBarResponse {
    /// The crossfade slider value changed.
    pub t_changed: bool,
    /// The A button was clicked (capture).
    pub capture_a: bool,
    /// The B button was clicked (capture).
    pub capture_b: bool,
    /// The A button was right-clicked or double-clicked (recall).
    pub recall_a: bool,
    /// The B button was right-clicked or double-clicked (recall).
    pub recall_b: bool,
}

/// A/B crossfader with capture buttons.
///
/// Layout (horizontal):
/// ```text
/// [A] ────────────slider────────────── [B]
/// ```
///
/// - Click A/B to capture the current state.
/// - Right-click or double-click A/B to recall that snapshot.
/// - Slider is disabled unless both snapshots are captured.
///
/// # Arguments
///
/// * `t` — Mutable crossfade position, 0.0 (full A) to 1.0 (full B).
/// * `has_a` — Whether snapshot A has been captured.
/// * `has_b` — Whether snapshot B has been captured.
pub fn morph_bar(ui: &mut Ui, t: &mut f32, has_a: bool, has_b: bool) -> MorphBarResponse {
    let mut response = MorphBarResponse {
        t_changed: false,
        capture_a: false,
        capture_b: false,
        recall_a: false,
        recall_b: false,
    };

    ui.horizontal(|ui| {
        // A button
        let a_resp = snapshot_button(ui, "A", has_a, Color32::from_rgb(70, 130, 220));
        if a_resp.double_clicked() || a_resp.secondary_clicked() {
            response.recall_a = true;
        } else if a_resp.clicked() {
            response.capture_a = true;
        }

        // Slider
        let enabled = has_a && has_b;
        ui.add_enabled_ui(enabled, |ui| {
            let slider = egui::Slider::new(t, 0.0..=1.0)
                .show_value(false)
                .trailing_fill(true);
            let slider_size = vec2(ui.available_width() - 40.0, 18.0);
            ui.spacing_mut().slider_width = slider_size.x;
            if ui.add(slider).changed() {
                response.t_changed = true;
            }
        });

        // B button
        let b_resp = snapshot_button(ui, "B", has_b, Color32::from_rgb(220, 140, 50));
        if b_resp.double_clicked() || b_resp.secondary_clicked() {
            response.recall_b = true;
        } else if b_resp.clicked() {
            response.capture_b = true;
        }
    });

    response
}

/// Draw a snapshot capture button (filled circle if captured, empty if not).
fn snapshot_button(ui: &mut Ui, label: &str, captured: bool, color: Color32) -> egui::Response {
    let size = vec2(28.0, 20.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let center = rect.center();
        let radius = 5.0;

        if captured {
            painter.circle_filled(center, radius, color);
        } else {
            painter.circle_stroke(center, radius, egui::Stroke::new(1.5, color));
        }

        painter.text(
            egui::pos2(center.x, center.y + radius + 3.0),
            egui::Align2::CENTER_TOP,
            label,
            egui::FontId::proportional(10.0),
            Color32::from_rgb(180, 180, 190),
        );
    }

    response
}
