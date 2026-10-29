//! The chip: the design system's one text button.
//!
//! Uppercase mono on a tint of its own color, outlined in that color, so a
//! toolbar of chips reads as one set and color alone carries state (amber
//! stopped, green playing, red bypassed, dim unavailable).

use egui::{Button, Color32, FontId, Response, RichText, Stroke, Ui, vec2};

/// Minimum chip height, in points.
pub const CHIP_HEIGHT: f32 = 22.0;

/// The chip as a [`Button`], for callers that hand a button to something else
/// (a menu's custom button). `active` fills it a little more and draws the
/// full-strength outline.
pub fn chip_button(label: &str, color: Color32, active: bool) -> Button<'static> {
    let text = RichText::new(label.to_uppercase())
        .font(FontId::monospace(11.0))
        .color(color);
    let (fill, stroke) = if active {
        (color.gamma_multiply(0.22), color)
    } else {
        (color.gamma_multiply(0.10), color.gamma_multiply(0.55))
    };
    Button::new(text)
        .fill(fill)
        .stroke(Stroke::new(1.0_f32, stroke))
        .min_size(vec2(0.0, CHIP_HEIGHT))
}

/// Add a chip to `ui`.
pub fn chip(ui: &mut Ui, label: &str, color: Color32, active: bool) -> Response {
    ui.add(chip_button(label, color, active))
}
