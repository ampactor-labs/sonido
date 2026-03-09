//! A/B morph crossfader widget.
//!
//! Provides a horizontal bar with A and B capture buttons flanking a
//! crossfade LED segment bar. Click a button to capture the current state
//! into that snapshot slot; right-click or double-click to recall it.
//! The segment bar interpolates from cyan (A) to amber (B), with lit
//! segments indicating the current crossfade position.

use egui::{Color32, Rect, Ui, vec2};

use crate::theme::SonidoTheme;
use crate::widgets::glow;

/// Number of LED segments in the crossfade bar.
const SEGMENT_COUNT: usize = 20;

/// Linearly interpolate between two `Color32` values.
fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let mix = |a: u8, b: u8, t: f32| -> u8 { (a as f32 * (1.0 - t) + b as f32 * t) as u8 };
    Color32::from_rgba_premultiplied(
        mix(a.r(), b.r(), t),
        mix(a.g(), b.g(), t),
        mix(a.b(), b.b(), t),
        mix(a.a(), b.a(), t),
    )
}

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

/// A/B crossfader with capture buttons and LED segment bar.
///
/// Layout (horizontal):
/// ```text
/// [A] ──── LED segments (cyan→amber) ──── [B]
/// ```
///
/// - Click A/B to capture the current state.
/// - Right-click or double-click A/B to recall that snapshot.
/// - Segment bar is ghosted unless both snapshots are captured.
/// - Segments at or before the crossfade position are lit with glow;
///   segments after are ghosted.
///
/// # Arguments
///
/// * `t` — Mutable crossfade position, 0.0 (full A) to 1.0 (full B).
/// * `has_a` — Whether snapshot A has been captured.
/// * `has_b` — Whether snapshot B has been captured.
pub fn morph_bar(ui: &mut Ui, t: &mut f32, has_a: bool, has_b: bool) -> MorphBarResponse {
    let theme = SonidoTheme::get(ui.ctx());

    let mut response = MorphBarResponse {
        t_changed: false,
        capture_a: false,
        capture_b: false,
        recall_a: false,
        recall_b: false,
    };

    ui.horizontal(|ui| {
        // A button — cyan
        let a_resp = snapshot_button(ui, "A", has_a, theme.colors.cyan, &theme);
        if a_resp.double_clicked() || a_resp.secondary_clicked() {
            response.recall_a = true;
        } else if a_resp.clicked() {
            response.capture_a = true;
        }

        // LED segment crossfade bar
        let enabled = has_a && has_b;
        led_segment_bar(ui, t, enabled, &theme, &mut response);

        // B button — amber
        let b_resp = snapshot_button(ui, "B", has_b, theme.colors.amber, &theme);
        if b_resp.double_clicked() || b_resp.secondary_clicked() {
            response.recall_b = true;
        } else if b_resp.clicked() {
            response.capture_b = true;
        }
    });

    response
}

/// Draw a snapshot capture button (glowing circle if captured, ghost stroke if not).
fn snapshot_button(
    ui: &mut Ui,
    label: &str,
    captured: bool,
    color: Color32,
    theme: &SonidoTheme,
) -> egui::Response {
    let size = vec2(28.0, 20.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let center = rect.center();
        let radius = 5.0;

        if captured {
            glow::glow_circle(painter, center, radius, color, theme);
        } else {
            glow::glow_circle_stroke(
                painter,
                center,
                radius,
                glow::ghost(color, theme),
                1.5,
                theme,
            );
        }

        painter.text(
            egui::pos2(center.x, center.y + radius + 3.0),
            egui::Align2::CENTER_TOP,
            label,
            egui::FontId::monospace(10.0),
            theme.colors.text_secondary,
        );
    }

    response
}

/// Draw a horizontal LED segment bar for the crossfade position.
///
/// 20 segments interpolate from cyan (left/A) to amber (right/B).
/// Segments at or before `*t` are lit with `glow_rect`; segments after
/// are ghosted. When disabled (not both snapshots captured), all segments
/// are ghosted. Dragging or clicking updates `*t`.
fn led_segment_bar(
    ui: &mut Ui,
    t: &mut f32,
    enabled: bool,
    theme: &SonidoTheme,
    response: &mut MorphBarResponse,
) {
    let bar_width = (ui.available_width() - 40.0).max(60.0);
    let bar_height = 14.0;
    let bar_size = vec2(bar_width, bar_height);

    let sense = if enabled {
        egui::Sense::click_and_drag()
    } else {
        egui::Sense::hover()
    };
    let (bar_rect, bar_response) = ui.allocate_exact_size(bar_size, sense);

    // Update t from drag/click interaction
    if enabled && (bar_response.dragged() || bar_response.clicked()) {
        if let Some(pointer) = bar_response.interact_pointer_pos() {
            *t = ((pointer.x - bar_rect.left()) / bar_rect.width()).clamp(0.0, 1.0);
            response.t_changed = true;
        }
    }

    if !ui.is_rect_visible(bar_rect) {
        return;
    }

    let painter = ui.painter();
    let cyan = theme.colors.cyan;
    let amber = theme.colors.amber;

    // Gap between segments (pixels).
    let gap = 2.0;
    let total_gaps = (SEGMENT_COUNT - 1) as f32 * gap;
    let seg_width = (bar_rect.width() - total_gaps) / SEGMENT_COUNT as f32;
    let seg_height = bar_rect.height();
    let corner = 1.5;

    // Which segment is the slider position at?
    let slider_seg = (*t * (SEGMENT_COUNT - 1) as f32).round() as usize;

    for i in 0..SEGMENT_COUNT {
        let t_seg = i as f32 / (SEGMENT_COUNT - 1) as f32;
        let seg_color = lerp_color(cyan, amber, t_seg);

        let x = bar_rect.left() + i as f32 * (seg_width + gap);
        let seg_rect =
            Rect::from_min_size(egui::pos2(x, bar_rect.top()), vec2(seg_width, seg_height));

        if enabled && i <= slider_seg {
            // Lit segment
            glow::glow_rect(painter, seg_rect, seg_color, corner, theme);
        } else {
            // Ghost segment
            let ghost_color = glow::ghost(seg_color, theme);
            painter.rect_filled(seg_rect, corner, ghost_color);
        }
    }
}
