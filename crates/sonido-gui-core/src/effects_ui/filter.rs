//! Filter effect UI panel.

use crate::theme::SonidoTheme;
use crate::widgets::{BypassToggle, bridged_fader};
use crate::{ParamBridge, ParamIndex, SlotIndex};
use egui::Ui;

/// UI panel for the low-pass filter effect.
pub struct FilterPanel;

impl FilterPanel {
    /// Create a new filter panel.
    pub fn new() -> Self {
        Self
    }

    /// Render the filter effect controls.
    ///
    /// Param indices: 0 = cutoff (Hz), 1 = resonance.
    pub fn ui(&mut self, ui: &mut Ui, bridge: &dyn ParamBridge, slot: SlotIndex) {
        let theme = SonidoTheme::get(ui.ctx());
        let param_count = 2;
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
                for i in 0..param_count {
                    bridged_fader(ui, bridge, slot, ParamIndex(i), fader_w, fader_h);
                }
            });
        });
    }
}

impl Default for FilterPanel {
    fn default() -> Self {
        Self::new()
    }
}
