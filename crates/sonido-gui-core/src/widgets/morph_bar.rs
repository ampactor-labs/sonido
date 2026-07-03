//! A/B morph crossfader widget.
//!
//! A horizontal bar with A and B pose dots flanking a crossfade LED segment
//! bar. Left-click a dot to focus that pose for editing (park + sculpt);
//! right-click or double-click to grab the current sound into it. The focused
//! dot glows. The segment bar interpolates from cyan (A) to amber (B), with lit
//! segments indicating the current crossfade position; drag it to perform.

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
#[derive(Default)]
pub struct MorphBarResponse {
    /// The crossfade slider value changed (perform).
    pub t_changed: bool,
    /// The A dot was left-clicked — focus pose A for editing.
    pub focus_a: bool,
    /// The B dot was left-clicked — focus pose B for editing.
    pub focus_b: bool,
    /// The A dot was right-clicked or double-clicked — grab current into A.
    pub grab_a: bool,
    /// The B dot was right-clicked or double-clicked — grab current into B.
    pub grab_b: bool,
}

/// A/B crossfader with pose dots and an LED segment bar.
///
/// Layout (horizontal):
/// ```text
/// [A] ──── LED segments (cyan→amber) ──── [B]
/// ```
///
/// - Left-click A/B to focus that pose for editing (the dot glows).
/// - Right-click or double-click A/B to grab the current sound into it.
/// - Drag the segment bar to perform; it is ghosted until the chain has an effect.
///
/// # Arguments
///
/// * `t` — Mutable crossfade position, 0.0 (full A) to 1.0 (full B).
/// * `editing_a` / `editing_b` — Which pose is currently focused (its dot glows).
/// * `enabled` — Whether the bar can be dragged (the chain has an effect).
pub fn morph_bar(
    ui: &mut Ui,
    t: &mut f32,
    editing_a: bool,
    editing_b: bool,
    enabled: bool,
) -> MorphBarResponse {
    let theme = SonidoTheme::get(ui.ctx());
    let mut response = MorphBarResponse::default();

    ui.horizontal(|ui| {
        // A dot — cyan, glows while focused.
        let a_resp = snapshot_button(ui, "A", editing_a, theme.colors.cyan, &theme).on_hover_text(
            "Pose A — left-click to sculpt it, right-click to grab the current sound into it",
        );
        if a_resp.double_clicked() || a_resp.secondary_clicked() {
            response.grab_a = true;
        } else if a_resp.clicked() {
            response.focus_a = true;
        }

        // LED segment crossfade bar.
        led_segment_bar(ui, t, enabled, &theme, &mut response);

        // B dot — amber, glows while focused.
        let b_resp = snapshot_button(ui, "B", editing_b, theme.colors.amber, &theme).on_hover_text(
            "Pose B — left-click to sculpt it, right-click to grab the current sound into it",
        );
        if b_resp.double_clicked() || b_resp.secondary_clicked() {
            response.grab_b = true;
        } else if b_resp.clicked() {
            response.focus_b = true;
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
    if enabled
        && (bar_response.dragged() || bar_response.clicked())
        && let Some(pointer) = bar_response.interact_pointer_pos()
    {
        *t = ((pointer.x - bar_rect.left()) / bar_rect.width()).clamp(0.0, 1.0);
        response.t_changed = true;
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
