//! Vibrato effect UI panel.

use crate::theme::SonidoTheme;
use crate::widgets::{BypassToggle, bridged_knob};
use crate::{ParamBridge, ParamIndex, SlotIndex};
use egui::Ui;

/// UI panel for the Vibrato effect.
pub struct VibratoPanel;

impl VibratoPanel {
    /// Create a new vibrato panel.
    pub fn new() -> Self {
        Self
    }

    /// Render the vibrato effect controls.
    ///
    /// Param indices: 0 = depth (%).
    pub fn ui(&mut self, ui: &mut Ui, bridge: &dyn ParamBridge, slot: SlotIndex) {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                let mut active = !bridge.is_bypassed(slot);
                if ui.add(BypassToggle::new(&mut active, "Active")).changed() {
                    bridge.set_bypassed(slot, !active);
                }
            });

            ui.add_space(12.0);

            ui.horizontal(|ui| {
                bridged_knob(ui, bridge, slot, ParamIndex(0), "DEPTH");
            });

            ui.add_space(8.0);
            let theme = SonidoTheme::get(ui.ctx());
            ui.label(
                egui::RichText::new("6-unit tape wow/flutter simulation")
                    .small()
                    .color(theme.colors.text_secondary),
            );
        });
    }
}

impl Default for VibratoPanel {
    fn default() -> Self {
        Self::new()
    }
}
