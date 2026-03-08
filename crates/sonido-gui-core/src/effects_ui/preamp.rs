//! Preamp effect UI panel.

use crate::theme::SonidoTheme;
use crate::widgets::bridged_fader;
use crate::{ParamBridge, ParamIndex, SlotIndex};
use egui::Ui;

/// UI panel for the clean preamp effect.
pub struct PreampPanel;

impl PreampPanel {
    /// Create a new preamp panel.
    pub fn new() -> Self {
        Self
    }

    /// Render the preamp controls.
    ///
    /// Param indices: 0 = gain (dB).
    pub fn ui(&mut self, ui: &mut Ui, bridge: &dyn ParamBridge, slot: SlotIndex) {
        let theme = SonidoTheme::get(ui.ctx());
        let param_count = 1;
        let avail_w = ui.available_width();
        let fader_w = theme.layout.fader_width(avail_w, param_count);
        let fader_h = theme.layout.fader_height(ui.available_height().min(200.0));

        ui.vertical(|ui| {
            ui.horizontal_wrapped(|ui| {
                bridged_fader(ui, bridge, slot, ParamIndex(0), fader_w, fader_h);
            });
        });
    }
}

impl Default for PreampPanel {
    fn default() -> Self {
        Self::new()
    }
}
