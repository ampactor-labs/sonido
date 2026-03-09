//! Bypass toggle widget for effects.

use egui::{Response, Sense, Stroke, StrokeKind, Ui, Widget, pos2, vec2};

use crate::theme::SonidoTheme;
use crate::widgets::glow;

/// A bypass toggle button for effects.
pub struct BypassToggle<'a> {
    active: &'a mut bool,
    label: &'a str,
    size: f32,
}

impl<'a> BypassToggle<'a> {
    /// Create a new bypass toggle.
    ///
    /// `active` is true when the effect is ON (not bypassed).
    pub fn new(active: &'a mut bool, label: &'a str) -> Self {
        Self {
            active,
            label,
            size: 20.0,
        }
    }

    /// Set the button size.
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }
}

impl Widget for BypassToggle<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let total_width = self.size
            + 8.0
            + ui.fonts(|f| {
                f.glyph_width(&egui::FontId::monospace(12.0), 'M') * self.label.len() as f32
            });
        let size = vec2(total_width.max(60.0), self.size + 4.0);

        let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());

        if response.clicked() {
            *self.active = !*self.active;
            response.mark_changed();
        }

        if ui.is_rect_visible(rect) {
            let theme = SonidoTheme::get(ui.ctx());
            let painter = ui.painter();

            // Toggle indicator (circle)
            let center = pos2(rect.left() + self.size / 2.0 + 2.0, rect.center().y);
            let radius = self.size / 2.0 - 2.0;

            if *self.active {
                // ON — filled green circle with phosphor bloom
                glow::glow_circle(painter, center, radius, theme.colors.green, &theme);
            } else {
                // OFF — dim ring outline
                glow::glow_circle_stroke(painter, center, radius, theme.colors.dim, 1.5, &theme);
            }

            // Hover ring
            if response.hovered() {
                let hover_color = theme.colors.cyan.gamma_multiply(0.4);
                painter.circle_stroke(center, radius + 2.0, Stroke::new(1.0, hover_color));
            }

            // Label
            let label_pos = pos2(rect.left() + self.size + 8.0, rect.center().y);
            let text_color = if *self.active {
                theme.colors.cyan
            } else {
                theme.colors.dim
            };
            painter.text(
                label_pos,
                egui::Align2::LEFT_CENTER,
                self.label,
                egui::FontId::monospace(12.0),
                text_color,
            );
        }

        response
    }
}

/// A larger footswitch-style toggle for the chain view.
pub struct FootswitchToggle<'a> {
    active: &'a mut bool,
    label: &'a str,
}

impl<'a> FootswitchToggle<'a> {
    /// Create a new footswitch toggle with the given state and label.
    pub fn new(active: &'a mut bool, label: &'a str) -> Self {
        Self { active, label }
    }
}

impl Widget for FootswitchToggle<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let size = vec2(70.0, 50.0);
        let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());

        if response.clicked() {
            *self.active = !*self.active;
            response.mark_changed();
        }

        if ui.is_rect_visible(rect) {
            let theme = SonidoTheme::get(ui.ctx());
            let painter = ui.painter();

            // Pedal body — void fill with dim border
            painter.rect_filled(rect, 6.0, theme.colors.void);
            painter.rect_stroke(
                rect,
                6.0,
                Stroke::new(1.0, theme.colors.dim),
                StrokeKind::Inside,
            );

            // LED indicator dot
            let led_pos = pos2(rect.center().x, rect.top() + 12.0);
            if *self.active {
                // ON — green with phosphor bloom
                glow::glow_circle(painter, led_pos, 5.0, theme.colors.green, &theme);
            } else {
                // OFF — ghosted green
                let ghost_color = glow::ghost(theme.colors.green, &theme);
                painter.circle_filled(led_pos, 5.0, ghost_color);
            }

            // Label
            let label_pos = pos2(rect.center().x, rect.bottom() - 12.0);
            let text_color = if *self.active {
                theme.colors.cyan
            } else {
                theme.colors.dim
            };
            painter.text(
                label_pos,
                egui::Align2::CENTER_CENTER,
                self.label,
                egui::FontId::monospace(10.0),
                text_color,
            );
        }

        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bypass_toggle_default_size() {
        let mut active = true;
        let toggle = BypassToggle::new(&mut active, "Test");
        assert_eq!(toggle.size, 20.0);
        assert_eq!(toggle.label, "Test");
        assert!(*toggle.active);
    }

    #[test]
    fn bypass_toggle_custom_size() {
        let mut active = false;
        let toggle = BypassToggle::new(&mut active, "FX").size(32.0);
        assert_eq!(toggle.size, 32.0);
        assert!(!*toggle.active);
    }

    #[test]
    fn footswitch_toggle_stores_state() {
        let mut active = true;
        let toggle = FootswitchToggle::new(&mut active, "Drive");
        assert!(toggle.active == &true);
        assert_eq!(toggle.label, "Drive");
    }
}
