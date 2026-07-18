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
    show_value: bool,
    value_inside: bool,
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
            show_value: true,
            value_inside: false,
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

    /// Hide the value text below the knob.
    ///
    /// Use when an external display (e.g., LED) shows the value instead.
    pub fn show_value(mut self, show: bool) -> Self {
        self.show_value = show;
        self
    }

    /// Render the formatted value *inside* the ring (centered) instead of
    /// below it, and label below. Compact — no separate value row to clip.
    pub fn value_inside(mut self, inside: bool) -> Self {
        self.value_inside = inside;
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
        // Vertical room beyond the ring: value-inside reserves space for the
        // label only; value-below reserves label + value; bare reserves label.
        let extra = if self.value_inside {
            18.0
        } else if self.show_value {
            35.0
        } else {
            20.0
        };
        let size = vec2(self.diameter, self.diameter + extra);
        let (rect, mut response) = ui.allocate_exact_size(size, Sense::click_and_drag());

        let center = pos2(rect.center().x, rect.top() + self.diameter / 2.0);
        let radius = self.diameter / 2.0 - 4.0;

        // Handle interaction
        let mut changed = false;

        // Clicking or grabbing the knob focuses it so the keyboard can drive it.
        if response.clicked() || response.drag_started() {
            response.request_focus();
        }

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

        // Keyboard: while focused, Up/Down nudge the value (1% per press, Shift =
        // 0.2% fine). Only Up/Down are consumed — Left/Right are deliberately left
        // for egui's built-in directional focus navigation, so the arrows move
        // *between* knobs left↔right and adjust the focused one up↔down.
        if response.has_focus() {
            let step = (self.max - self.min)
                * if ui.input(|i| i.modifiers.shift) {
                    0.002
                } else {
                    0.01
                };
            let up = ui.input_mut(|i| {
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)
                    || i.consume_key(egui::Modifiers::SHIFT, egui::Key::ArrowUp)
            });
            let down = ui.input_mut(|i| {
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)
                    || i.consume_key(egui::Modifiers::SHIFT, egui::Key::ArrowDown)
            });
            if up {
                *self.value = (*self.value + step).clamp(self.min, self.max);
                changed = true;
            }
            if down {
                *self.value = (*self.value - step).clamp(self.min, self.max);
                changed = true;
            }
        }

        // Draw knob
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let theme = SonidoTheme::get(ui.ctx());
            let hovered = response.hovered();
            let focused = response.has_focus();

            // Hover multiplier — bloom doubles on pointer + value arc
            let hover_mult = if hovered {
                theme.glow.hover_bloom_mult
            } else {
                1.0
            };

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
                    6.0 * hover_mult,
                    &theme,
                );
            }

            if self.value_inside {
                // Pointer as an outer-rim tick, leaving the center free for the
                // value readout.
                let p_inner = pos2(
                    center.x + value_angle.cos() * (radius * 0.6),
                    center.y + value_angle.sin() * (radius * 0.6),
                );
                let p_outer = pos2(
                    center.x + value_angle.cos() * (radius - 2.0),
                    center.y + value_angle.sin() * (radius - 2.0),
                );
                glow::glow_line(
                    painter,
                    p_inner,
                    p_outer,
                    theme.colors.amber,
                    2.0 * hover_mult,
                    &theme,
                );

                // Value centered inside the ring.
                let value_text = if let Some(ref formatter) = self.format_value {
                    formatter(*self.value)
                } else {
                    format!("{:.2}", *self.value)
                };
                painter.text(
                    center,
                    egui::Align2::CENTER_CENTER,
                    value_text,
                    egui::FontId::monospace(9.0),
                    theme.colors.amber,
                );
            } else {
                // Pointer line from center + center dot.
                let pointer_len = radius - 14.0;
                let pointer_end = pos2(
                    center.x + value_angle.cos() * pointer_len,
                    center.y + value_angle.sin() * pointer_len,
                );
                glow::glow_line(
                    painter,
                    center,
                    pointer_end,
                    theme.colors.amber,
                    2.0 * hover_mult,
                    &theme,
                );
                glow::glow_circle(painter, center, 2.0, theme.colors.amber, &theme);
            }

            // Focus ring — thin cyan halo so the keyboard target is unmistakable.
            if focused {
                painter.circle_stroke(
                    center,
                    radius + 3.0,
                    egui::Stroke::new(1.0_f32, theme.colors.cyan),
                );
            }

            // Label — brightens to full cyan on hover
            let label_color = if hovered {
                theme.colors.cyan
            } else {
                theme.colors.cyan.gamma_multiply(0.7)
            };
            let label_pos = pos2(rect.center().x, center.y + radius + 8.0);
            painter.text(
                label_pos,
                egui::Align2::CENTER_TOP,
                self.label,
                egui::FontId::monospace(if self.value_inside { 10.0 } else { 11.0 }),
                label_color,
            );

            // Value below — only in the legacy value-below mode.
            if self.show_value && !self.value_inside {
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
