//! Rotary knob: the design system's continuous control.
//!
//! An LED-segment ring around a dark body, like an arcade encoder: lit
//! segments in the accent (the effect family's phosphor), unlit ones as ghost
//! traces, and a white-hot pointer. Bipolar knobs (range spanning zero) fill
//! from twelve o'clock outward.
//!
//! Interaction:
//! - Drag vertically to adjust value; Shift+drag for fine control (10x)
//! - Mouse wheel over the knob, when enabled with [`Knob::wheel`]
//! - Up/Down arrows while focused (Shift = fine)
//! - Double-click to reset to default

use egui::{Response, Sense, Ui, Widget, pos2, vec2};
use std::f32::consts::PI;

use crate::theme::SonidoTheme;
use crate::widgets::glow;

/// Rotary knob parameters.
#[allow(clippy::struct_excessive_bools)] // independent builder switches
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
    accent: Option<egui::Color32>,
    bipolar: bool,
    wheel: bool,
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
            accent: None,
            bipolar: false,
            wheel: false,
        }
    }

    /// Ring and pointer color (default: the theme's amber).
    pub fn accent(mut self, color: egui::Color32) -> Self {
        self.accent = Some(color);
        self
    }

    /// Fill the ring from twelve o'clock (for ranges centered on zero).
    pub fn bipolar(mut self, bipolar: bool) -> Self {
        self.bipolar = bipolar;
        self
    }

    /// Let the mouse wheel adjust the knob while hovered.
    pub fn wheel(mut self, wheel: bool) -> Self {
        self.wheel = wheel;
        self
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

        // Wheel: 2% per notch (0.2% with Shift), only when opted in so knobs
        // inside a scroll area don't steal its scrolling.
        if self.wheel && response.hovered() {
            let scroll = ui.input(|i| i.raw_scroll_delta.y);
            if scroll != 0.0 {
                let fine = ui.input(|i| i.modifiers.shift);
                let per_point = if fine { 0.0001 } else { 0.001 };
                *self.value = (*self.value + scroll * per_point * (self.max - self.min))
                    .clamp(self.min, self.max);
                changed = true;
            }
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
            let hovered = response.hovered() || response.dragged();
            let focused = response.has_focus();
            let accent = self.accent.unwrap_or(theme.colors.amber);

            // 270° sweep from bottom-left (135°) clockwise to bottom-right.
            let start_angle = PI * 0.75;
            let sweep = PI * 1.5;
            let range = (self.max - self.min).max(f32::EPSILON);
            let normalized = ((*self.value - self.min) / range).clamp(0.0, 1.0);
            let value_angle = start_angle + normalized * sweep;
            let at =
                |angle: f32, r: f32| pos2(center.x + angle.cos() * r, center.y + angle.sin() * r);

            // Body: a dark well with a hairline rim; brightens on hover.
            let body_r = radius - 7.0;
            painter.circle_filled(center, body_r, theme.colors.panel_hi);
            painter.circle_stroke(
                center,
                body_r,
                egui::Stroke::new(
                    1.0_f32,
                    if hovered {
                        accent.gamma_multiply(0.6)
                    } else {
                        theme.colors.line
                    },
                ),
            );

            // LED ring: SEGMENTS ticks; lit between the origin and the value.
            const SEGMENTS: usize = 25;
            let origin = if self.bipolar { 0.5 } else { 0.0 };
            let (lo, hi) = if normalized < origin {
                (normalized, origin)
            } else {
                (origin, normalized)
            };
            let ring_in = radius - 4.5;
            let ring_out = radius;
            for i in 0..SEGMENTS {
                let t = i as f32 / (SEGMENTS - 1) as f32;
                let angle = start_angle + t * sweep;
                let lit = t >= lo - 0.5 / SEGMENTS as f32
                    && t <= hi + 0.5 / SEGMENTS as f32
                    && (hi - lo) > 0.002;
                if lit {
                    glow::neon_line(
                        painter,
                        at(angle, ring_in),
                        at(angle, ring_out),
                        accent,
                        2.2,
                        &theme,
                    );
                } else {
                    painter.line_segment(
                        [at(angle, ring_in), at(angle, ring_out)],
                        egui::Stroke::new(2.0_f32, theme.colors.dim),
                    );
                }
            }

            // Pointer: a white-hot beam across the body.
            let hover_mult = if hovered {
                theme.glow.hover_bloom_mult
            } else {
                1.0
            };
            glow::neon_line(
                painter,
                at(value_angle, body_r * 0.25),
                at(value_angle, body_r * 0.92),
                accent,
                2.0 * hover_mult.min(1.3),
                &theme,
            );

            if self.value_inside {
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
                    theme.colors.text_primary,
                );
            } else {
                glow::glow_circle(painter, center, 2.0, glow::hot_core(accent), &theme);
            }

            // Focus ring — thin cyan halo so the keyboard target is unmistakable.
            if focused {
                painter.circle_stroke(
                    center,
                    radius + 3.0,
                    egui::Stroke::new(1.0_f32, theme.colors.cyan),
                );
            }

            // Value (bright, it's the reading), then label (muted, it's the name).
            let below = center.y + radius + 5.0;
            if self.show_value && !self.value_inside {
                let value_text = if let Some(ref formatter) = self.format_value {
                    formatter(*self.value)
                } else {
                    format!("{:.2}", *self.value)
                };
                painter.text(
                    pos2(rect.center().x, below),
                    egui::Align2::CENTER_TOP,
                    value_text,
                    egui::FontId::monospace(12.0),
                    if hovered {
                        glow::hot_core(accent)
                    } else {
                        theme.colors.text_primary
                    },
                );
            }
            let label_y = if self.show_value && !self.value_inside {
                below + 15.0
            } else {
                below + 2.0
            };
            painter.text(
                pos2(rect.center().x, label_y),
                egui::Align2::CENTER_TOP,
                self.label.to_uppercase(),
                egui::FontId::monospace(if self.value_inside { 10.0 } else { 10.5 }),
                if hovered {
                    accent
                } else {
                    theme.colors.text_secondary
                },
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
