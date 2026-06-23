//! Six-macro performance panel.
//!
//! Renders the six macros that map to the pedal's six knobs (K1–K6): a large
//! position knob each, the macro's name, and an "active" glow when the macro
//! drives at least one parameter. This is the *performance* surface — turning
//! the knobs sweeps every mapped target at once.
//!
//! Mapping authoring (assigning a parameter to a macro, editing its range) is
//! driven from the parameter knobs' context menu in the editor, not here, so
//! this widget stays dependency-light: it takes a borrowed view per macro and
//! reports which positions changed and which label was clicked (to open a
//! mapping editor for that macro).

use egui::{Ui, vec2};

use crate::theme::SonidoTheme;
use crate::widgets::{Knob, glow};

/// Borrowed view of one macro for [`macro_panel`].
pub struct MacroView<'a> {
    /// Display name (e.g. "Drive", "Space"). Empty renders as the slot label.
    pub name: &'a str,
    /// Macro position in `[0.0, 1.0]`, mutated by the knob.
    pub position: &'a mut f32,
    /// How many targets this macro drives (0 ⇒ inactive/ghosted).
    pub mapping_count: usize,
}

/// What the user did in the macro panel this frame.
#[derive(Default)]
pub struct MacroPanelResponse {
    /// Index of the macro whose position changed, if any.
    pub changed: Option<usize>,
    /// Index of the macro whose label was clicked (open its mapping editor).
    pub edit_requested: Option<usize>,
}

/// Render the six-macro performance row. `macros` must have length 6 (K1–K6).
pub fn macro_panel(ui: &mut Ui, macros: &mut [MacroView]) -> MacroPanelResponse {
    let theme = SonidoTheme::get(ui.ctx());
    let mut response = MacroPanelResponse::default();

    // Wrap so the six 64px macro blocks flow onto multiple rows on narrow
    // viewports (phone / the RIG drawer); on desktop they stay a single row.
    ui.horizontal_wrapped(|ui| {
        for (i, m) in macros.iter_mut().enumerate() {
            ui.vertical(|ui| {
                ui.set_width(64.0);

                // Hardware-knob label + active indicator.
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("K{}", i + 1))
                            .font(egui::FontId::monospace(10.0))
                            .color(theme.colors.text_secondary),
                    );
                    let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), egui::Sense::hover());
                    if ui.is_rect_visible(dot) {
                        let color = if m.mapping_count > 0 {
                            theme.colors.cyan
                        } else {
                            glow::ghost(theme.colors.text_secondary, &theme)
                        };
                        glow::glow_circle(ui.painter(), dot.center(), 3.0, color, &theme);
                    }
                });

                // Position knob (0–100%).
                let knob = Knob::new(m.position, 0.0, 1.0, "")
                    .diameter(44.0)
                    .format(|v| format!("{:.0}%", v * 100.0));
                if ui.add(knob).changed() {
                    response.changed = Some(i);
                }

                // Clickable name → open mapping editor for this macro.
                let label_text = if m.name.is_empty() {
                    format!("macro {}", i + 1)
                } else {
                    m.name.to_owned()
                };
                let name_color = if m.mapping_count > 0 {
                    theme.colors.text_primary
                } else {
                    theme.colors.text_secondary
                };
                let label = ui.add(
                    egui::Label::new(
                        egui::RichText::new(label_text)
                            .font(egui::FontId::monospace(11.0))
                            .color(name_color),
                    )
                    .sense(egui::Sense::click()),
                );
                if label.clicked() {
                    response.edit_requested = Some(i);
                }
                if m.mapping_count > 0 {
                    ui.label(
                        egui::RichText::new(format!("→{}", m.mapping_count))
                            .font(egui::FontId::monospace(9.0))
                            .color(theme.colors.text_secondary),
                    );
                }
            });
        }
    });

    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_defaults_to_no_action() {
        let r = MacroPanelResponse::default();
        assert!(r.changed.is_none());
        assert!(r.edit_requested.is_none());
    }

    #[test]
    fn macro_view_holds_borrow() {
        let mut pos = 0.5;
        let view = MacroView {
            name: "Drive",
            position: &mut pos,
            mapping_count: 2,
        };
        assert_eq!(view.name, "Drive");
        assert_eq!(view.mapping_count, 2);
        assert!((*view.position - 0.5).abs() < f32::EPSILON);
    }
}
