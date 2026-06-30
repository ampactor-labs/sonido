//! Contextual Info View — a docked strip that explains the current
//! selection, shows its live parameter values, and lists the relevant
//! keyboard shortcuts.
//!
//! Modeled on Ableton Live's Info View, but unified with the project's own
//! description substrate: the title + prose come from the effect registry, and
//! the parameter values are read live from the same [`ParamDescriptor`] /
//! [`ParamBridge`] metadata that drives accessibility — so the help text and
//! the screen-reader text can never drift apart. Unlike Ableton's static box,
//! the parameter readout updates in real time.
//!
//! [`ParamDescriptor`]: sonido_core::ParamDescriptor
//! [`ParamBridge`]: sonido_gui_core::ParamBridge

use sonido_gui_core::SonidoTheme;

/// Shortcuts shown when nothing is selected (the general canvas controls).
const GENERAL_SHORTCUTS: &[&str] = &[
    "+ Effect / right-click: add node",
    "Select a node, Delete: remove",
    "\u{2191}/\u{2193}: adjust focused knob",
    "\u{2190}/\u{2192}: move between knobs",
    "Space: play",
    "Ctrl+Z: undo",
    "Ctrl+Scroll: zoom",
];

/// Shortcuts shown when an effect node is selected (the contextual controls).
const EFFECT_SHORTCUTS: &[&str] = &[
    "Delete: remove node",
    "\u{2191}/\u{2193}: adjust focused knob",
    "Click a meter: reset peak",
    "Space: play",
];

/// Everything the Info View needs to render one target.
#[derive(Clone, Debug, PartialEq)]
pub struct InfoPayload {
    /// Title line — effect name, or a placeholder when nothing is selected.
    pub title: String,
    /// Prose description of the target, if any.
    pub description: Option<String>,
    /// Live `(name, formatted value)` pairs for the target's parameters.
    pub params: Vec<(String, String)>,
    /// Context-relevant keyboard shortcuts.
    pub shortcuts: &'static [&'static str],
}

impl InfoPayload {
    /// The empty-state payload: a prompt plus the general shortcuts.
    pub fn empty_state() -> Self {
        Self {
            title: "NO SELECTION".to_string(),
            description: Some(
                "Select a node to see what it does, or + Effect to add one.".to_string(),
            ),
            params: Vec::new(),
            shortcuts: GENERAL_SHORTCUTS,
        }
    }

    /// Payload for a selected effect: registry name + prose + live params.
    pub fn for_effect(
        name: impl Into<String>,
        description: impl Into<String>,
        params: Vec<(String, String)>,
    ) -> Self {
        Self {
            title: name.into(),
            description: Some(description.into()),
            params,
            shortcuts: EFFECT_SHORTCUTS,
        }
    }
}

/// Render the Info View strip into `ui`.
///
/// Layout: a title + prose row, an optional live-parameter row, and a
/// shortcuts row — all compact, monospace, and muted so the strip reads as
/// chrome rather than competing with the canvas.
pub fn render(ui: &mut egui::Ui, theme: &SonidoTheme, payload: &InfoPayload) {
    use egui::{FontId, RichText};

    // Title + description.
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(&payload.title)
                .font(FontId::monospace(12.0))
                .color(theme.colors.amber)
                .strong(),
        );
        if let Some(desc) = &payload.description {
            ui.label(
                RichText::new(format!("\u{00b7} {desc}"))
                    .font(FontId::monospace(11.0))
                    .color(theme.colors.text_secondary)
                    .italics(),
            );
        }
    });

    // Live parameter values.
    if !payload.params.is_empty() {
        ui.horizontal_wrapped(|ui| {
            for (i, (name, value)) in payload.params.iter().enumerate() {
                if i > 0 {
                    ui.label(
                        RichText::new("\u{00b7}")
                            .font(FontId::monospace(11.0))
                            .color(theme.colors.dim),
                    );
                }
                ui.label(
                    RichText::new(format!("{name}:"))
                        .font(FontId::monospace(10.0))
                        .color(theme.colors.text_secondary),
                );
                ui.label(
                    RichText::new(value)
                        .font(FontId::monospace(10.0))
                        .color(theme.colors.text_primary),
                );
            }
        });
    }

    // Shortcuts.
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(payload.shortcuts.join("   \u{00b7}   "))
                .font(FontId::monospace(10.0))
                .color(theme.colors.text_secondary),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state_uses_general_shortcuts() {
        let p = InfoPayload::empty_state();
        assert!(p.params.is_empty());
        assert_eq!(p.shortcuts, GENERAL_SHORTCUTS);
        assert!(p.description.is_some());
    }

    #[test]
    fn for_effect_carries_name_prose_and_live_params() {
        let p = InfoPayload::for_effect(
            "Compressor",
            "Dynamics compressor with program-dependent release",
            vec![
                ("Threshold".to_string(), "-18.0 dB".to_string()),
                ("Ratio".to_string(), "4.0:1".to_string()),
            ],
        );
        assert_eq!(p.title, "Compressor");
        assert_eq!(
            p.description.as_deref(),
            Some("Dynamics compressor with program-dependent release")
        );
        assert_eq!(p.params.len(), 2);
        assert_eq!(
            p.params[0],
            ("Threshold".to_string(), "-18.0 dB".to_string())
        );
        assert_eq!(p.shortcuts, EFFECT_SHORTCUTS);
    }

    #[test]
    fn effect_and_general_shortcut_sets_differ() {
        // The contextual set is not just a copy of the general one.
        assert_ne!(EFFECT_SHORTCUTS, GENERAL_SHORTCUTS);
        assert!(EFFECT_SHORTCUTS.iter().any(|s| s.contains("reset peak")));
    }
}
