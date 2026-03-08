//! Vibrato effect UI panel.

use crate::theme::SonidoTheme;
use crate::widgets::{BypassToggle, bridged_fader};
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
        let theme = SonidoTheme::get(ui.ctx());
        let param_count = 1;
        let avail_w = ui.available_width();
        let fader_w = theme.layout.fader_width(avail_w, param_count);
        let fader_h = theme.layout.fader_height(ui.available_height().min(200.0));

        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                let mut active = !bridge.is_bypassed(slot);
                if ui.add(BypassToggle::new(&mut active, "Active")).changed() {
                    bridge.set_bypassed(slot, !active);
                }
            });

            ui.add_space(12.0);

            ui.horizontal_wrapped(|ui| {
                bridged_fader(ui, bridge, slot, ParamIndex(0), fader_w, fader_h);
            });

            ui.add_space(8.0);
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
