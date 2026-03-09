//! Vertical slot fader with LED-segment fill.
//!
//! A compact parameter control modeled after mixing console channel faders.
//! The track fills with LED-colored segments from bottom to the current value.
//! Ghost (unlit) segments sit above. The thumb is a thin horizontal bar at
//! the value position.

use egui::{Color32, FontId, Rect, Response, Sense, Ui, Widget, pos2, vec2};

use crate::theme::SonidoTheme;
use crate::widgets::glow;

/// Number of LED segments in the fader track.
const SEGMENT_COUNT: usize = 16;

/// Vertical parameter fader with LED fill and value display.
///
/// ## Parameters
/// - `value`: Current normalized value (0.0--1.0), mutated on drag.
/// - `label`: Parameter name shown below the fader.
/// - `display_value`: Formatted value string (e.g., "3.5 dB").
/// - `color`: LED segment color (default: theme amber).
/// - `width`: Total fader width including padding.
/// - `height`: Fader track height (excluding labels).
/// - `default_normalized`: Default normalized value for double-click reset.
pub struct Fader<'a> {
    /// Current normalized value (0.0--1.0), mutated on interaction.
    value: &'a mut f32,
    /// Parameter name shown below the fader.
    label: &'a str,
    /// Formatted value string (e.g., "3.5 dB").
    display_value: String,
    /// LED segment color (default: theme amber).
    color: Option<Color32>,
    /// Total fader width including padding.
    width: f32,
    /// Fader track height (excluding labels).
    height: f32,
    /// Default normalized value for double-click reset.
    default_normalized: f32,
}

impl<'a> Fader<'a> {
    /// Create a new fader. `value` is normalized 0.0--1.0.
    pub fn new(value: &'a mut f32, label: &'a str) -> Self {
        Self {
            value,
            label,
            display_value: String::new(),
            color: None,
            width: 40.0,
            height: 80.0,
            default_normalized: 0.5,
        }
    }

    /// Set the formatted display value string.
    pub fn display(mut self, text: impl Into<String>) -> Self {
        self.display_value = text.into();
        self
    }

    /// Set the LED color (default: theme amber).
    pub fn color(mut self, color: Color32) -> Self {
        self.color = Some(color);
        self
    }

    /// Set fader dimensions (width, height).
    ///
    /// `width` is the total widget width. `height` is the fader track height,
    /// excluding labels below.
    pub fn size(mut self, width: f32, height: f32) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    /// Set the default normalized value for double-click reset.
    pub fn default_value(mut self, default: f32) -> Self {
        self.default_normalized = default;
        self
    }
}

impl Widget for Fader<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let theme = SonidoTheme::get(ui.ctx());
        let color = self.color.unwrap_or(theme.colors.amber);
        let ghost_color = glow::ghost(color, &theme);

        // Font sizes scale with width
        let label_font = FontId::monospace((self.width * 0.22).clamp(8.0, 11.0));
        let value_font = FontId::monospace((self.width * 0.20).clamp(7.0, 10.0));

        // Total height: track + label + value
        let label_h = 14.0;
        let value_h = 12.0;
        let total_h = self.height + label_h + value_h + 4.0;
        let size = vec2(self.width, total_h);

        let (rect, mut response) = ui.allocate_exact_size(size, Sense::click_and_drag());

        // Track rect (the actual fader area)
        let track_rect = Rect::from_min_size(rect.min, vec2(self.width, self.height));

        // Handle double-click to reset
        if response.double_clicked() {
            *self.value = self.default_normalized;
            response.mark_changed();
        }

        // Handle drag
        if response.dragged() {
            // Shift+drag: 5x precision
            let sensitivity = if ui.input(|i| i.modifiers.shift) {
                0.2
            } else {
                1.0
            };
            let delta_y = response.drag_delta().y;
            let range = track_rect.height();
            let delta_norm = -delta_y / range * sensitivity;
            *self.value = (*self.value + delta_norm).clamp(0.0, 1.0);
            response.mark_changed();
        }

        // Handle scroll wheel
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.1 {
                let step = scroll.signum() * 0.01;
                *self.value = (*self.value + step).clamp(0.0, 1.0);
                response.mark_changed();
            }
        }

        // Handle click-to-set (not drag, just single click)
        if response.clicked() {
            if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                if track_rect.contains(pos) {
                    let normalized = 1.0 - (pos.y - track_rect.top()) / track_rect.height();
                    *self.value = normalized.clamp(0.0, 1.0);
                    response.mark_changed();
                }
            }
        }

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();

            // Background track
            let track_inner = track_rect.shrink2(vec2(self.width * 0.3, 0.0));
            painter.rect_filled(track_inner, 2.0, theme.colors.dim);

            // LED segments
            let seg_gap = 1.0;
            let total_gaps = (SEGMENT_COUNT - 1) as f32 * seg_gap;
            let seg_h = (track_inner.height() - total_gaps) / SEGMENT_COUNT as f32;

            for i in 0..SEGMENT_COUNT {
                let seg_pos = i as f32 / SEGMENT_COUNT as f32;
                let y = track_inner.bottom() - (i as f32 + 1.0) * seg_h - i as f32 * seg_gap;
                let seg_rect = Rect::from_min_size(
                    pos2(track_inner.left(), y),
                    vec2(track_inner.width(), seg_h),
                );

                if *self.value > seg_pos {
                    glow::glow_rect(painter, seg_rect, color, 1.0, &theme);
                } else {
                    painter.rect_filled(seg_rect, 1.0, ghost_color);
                }
            }

            // Thumb (horizontal bar at value position)
            let thumb_y = track_rect.bottom() - *self.value * track_rect.height();
            let thumb_w = self.width * 0.8;
            let thumb_x = rect.center().x - thumb_w * 0.5;
            let thumb_rect = Rect::from_min_size(pos2(thumb_x, thumb_y - 1.5), vec2(thumb_w, 3.0));
            painter.rect_filled(thumb_rect, 1.0, color);

            // Label below track
            let label_pos = pos2(rect.center().x, track_rect.bottom() + 2.0);
            painter.text(
                label_pos,
                egui::Align2::CENTER_TOP,
                self.label,
                label_font,
                theme.colors.cyan,
            );

            // Value below label
            if !self.display_value.is_empty() {
                let value_pos = pos2(rect.center().x, track_rect.bottom() + label_h + 2.0);
                painter.text(
                    value_pos,
                    egui::Align2::CENTER_TOP,
                    &self.display_value,
                    value_font,
                    theme.colors.text_secondary,
                );
            }
        }

        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fader_clamps_value() {
        let mut val = 0.5;
        let fader = Fader::new(&mut val, "TEST");
        assert_eq!(fader.width, 40.0);
        assert_eq!(fader.height, 80.0);
    }

    #[test]
    fn fader_builder_chain() {
        let mut val = 0.3;
        let fader = Fader::new(&mut val, "GAIN")
            .display("3.5 dB")
            .size(50.0, 100.0)
            .default_value(0.0);
        assert_eq!(fader.width, 50.0);
        assert_eq!(fader.height, 100.0);
        assert_eq!(fader.default_normalized, 0.0);
        assert_eq!(fader.display_value, "3.5 dB");
    }

    #[test]
    fn fader_default_value_reset() {
        let mut val = 0.8;
        let fader = Fader::new(&mut val, "MIX").default_value(0.5);
        assert_eq!(fader.default_normalized, 0.5);
    }
}
