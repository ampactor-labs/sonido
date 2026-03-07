//! Rotary knob control widget with arcade CRT phosphor aesthetic.
//!
//! Pointer-on-void design: no filled knob body, just a glowing amber arc
//! and pointer line emerging from darkness. Uses [`glow`](super::glow)
//! primitives for phosphor bloom on all drawn elements.
//!
//! Interaction (unchanged from original):
//! - Drag vertically to adjust value
//! - Shift+drag for fine control (10x reduction)
//! - Double-click to reset to default
//! - Cyan label, amber value text below knob

use egui::{Response, Sense, Ui, Widget, pos2, vec2};
use std::f32::consts::PI;

use crate::theme::SonidoTheme;
use crate::widgets::glow;

/// Rotary knob parameters.
pub struct Knob<'a> {
    value: &'a mut f32,
    min: f32,
    max: f32,
    default: f32,
    label: &'a str,
    format_value: Option<Box<dyn Fn(f32) -> String + 'a>>,
    diameter: f32,
    sensitivity: f32,
}

impl<'a> Knob<'a> {
    /// Create a new knob.
    pub fn new(value: &'a mut f32, min: f32, max: f32, label: &'a str) -> Self {
        Self {
            value,
            min,
            max,
            default: (min + max) / 2.0,
            label,
            format_value: None,
            diameter: 60.0,
            sensitivity: 0.004,
        }
    }

    /// Set the default (reset) value.
    pub fn default(mut self, default: f32) -> Self {
        self.default = default;
        self
    }

    /// Set a custom value formatter.
    pub fn format(mut self, formatter: impl Fn(f32) -> String + 'a) -> Self {
        self.format_value = Some(Box::new(formatter));
        self
    }

    /// Set knob diameter in pixels.
    pub fn diameter(mut self, diameter: f32) -> Self {
        self.diameter = diameter;
        self
    }

    /// Set sensitivity (value change per pixel dragged).
    pub fn sensitivity(mut self, sensitivity: f32) -> Self {
        self.sensitivity = sensitivity;
        self
    }

    /// Format as decibels.
    pub fn format_db(self) -> Self {
        self.format(|v| format!("{:.1} dB", v))
    }

    /// Format as Hertz.
    pub fn format_hz(self) -> Self {
        self.format(|v| {
            if v >= 1000.0 {
                format!("{:.1} kHz", v / 1000.0)
            } else {
                format!("{:.0} Hz", v)
            }
        })
    }

    /// Format as milliseconds.
    pub fn format_ms(self) -> Self {
        self.format(|v| {
            if v >= 1000.0 {
                format!("{:.2} s", v / 1000.0)
            } else {
                format!("{:.1} ms", v)
            }
        })
    }

    /// Format as percentage.
    pub fn format_percent(self) -> Self {
        self.format(|v| format!("{:.0}%", v * 100.0))
    }

    /// Format as ratio (e.g., "4:1").
    pub fn format_ratio(self) -> Self {
        self.format(|v| format!("{:.1}:1", v))
    }
}

impl Widget for Knob<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let size = vec2(self.diameter, self.diameter + 35.0); // Extra space for label
        let (rect, mut response) = ui.allocate_exact_size(size, Sense::click_and_drag());

        let center = pos2(rect.center().x, rect.top() + self.diameter / 2.0);
        let radius = self.diameter / 2.0 - 4.0;

        // Handle interaction
        let mut changed = false;

        // Double-click to reset
        if response.double_clicked() {
            *self.value = self.default;
            changed = true;
        }

        // Drag to adjust
        if response.dragged() {
            let delta = response.drag_delta();
            let sensitivity = if ui.input(|i| i.modifiers.shift) {
                self.sensitivity * 0.1 // Fine control
            } else {
                self.sensitivity
            };

            // Vertical drag changes value (up = increase)
            let value_delta = -delta.y * sensitivity * (self.max - self.min);
            *self.value = (*self.value + value_delta).clamp(self.min, self.max);
            changed = true;
        }

        // Draw knob
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let theme = SonidoTheme::get(ui.ctx());

            // Knob arc angles (270 degree sweep, starting from bottom-left)
            let start_angle = PI * 0.75; // 135 degrees
            let end_angle = PI * 2.25; // 405 degrees (wraps around)
            let sweep = end_angle - start_angle;

            // Normalized value position
            let normalized = (*self.value - self.min) / (self.max - self.min);
            let value_angle = start_angle + normalized * sweep;

            // Track (background arc) — dim ghost trace
            glow::glow_arc(
                painter,
                center,
                radius - 2.0,
                start_angle,
                end_angle,
                theme.colors.dim,
                4.0,
                &theme,
            );

            // Value arc (filled portion) — phosphor amber glow
            if normalized > 0.001 {
                glow::glow_arc(
                    painter,
                    center,
                    radius - 2.0,
                    start_angle,
                    value_angle,
                    theme.colors.amber,
                    6.0,
                    &theme,
                );
            }

            // Pointer line — from center to value position
            let pointer_len = radius - 14.0;
            let pointer_end = pos2(
                center.x + value_angle.cos() * pointer_len,
                center.y + value_angle.sin() * pointer_len,
            );
            glow::glow_line(painter, center, pointer_end, theme.colors.amber, 2.0, &theme);

            // Center dot
            glow::glow_circle(painter, center, 2.0, theme.colors.amber, &theme);

            // Label
            let label_pos = pos2(rect.center().x, center.y + radius + 8.0);
            painter.text(
                label_pos,
                egui::Align2::CENTER_TOP,
                self.label,
                egui::FontId::monospace(11.0),
                theme.colors.cyan,
            );

            // Value text
            let value_text = if let Some(ref formatter) = self.format_value {
                formatter(*self.value)
            } else {
                format!("{:.2}", *self.value)
            };
            let value_pos = pos2(rect.center().x, center.y + radius + 22.0);
            painter.text(
                value_pos,
                egui::Align2::CENTER_TOP,
                value_text,
                egui::FontId::monospace(11.0),
                theme.colors.amber,
            );
        }

        if changed {
            response.mark_changed();
        }

        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_knob_default_value() {
        let mut value = 0.5;
        let knob = Knob::new(&mut value, 0.0, 1.0, "Test").default(0.25);
        assert_eq!(knob.default, 0.25);
    }
}
