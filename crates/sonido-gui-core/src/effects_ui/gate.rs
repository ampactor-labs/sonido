//! Gate effect UI panel.

use crate::theme::SonidoTheme;
use crate::widgets::bridged_fader;
use crate::{ParamBridge, ParamIndex, SlotIndex};
use egui::Ui;

/// UI panel for the noise gate effect.
pub struct GatePanel;

impl GatePanel {
    /// Create a new gate panel.
    pub fn new() -> Self {
        Self
    }

    /// Render the noise gate controls.
    ///
    /// Param indices: 0 = threshold (dB), 1 = attack (ms), 2 = release (ms), 3 = hold (ms).
    pub fn ui(&mut self, ui: &mut Ui, bridge: &dyn ParamBridge, slot: SlotIndex) {
        let theme = SonidoTheme::get(ui.ctx());
        let param_count = 4;
        let avail_w = ui.available_width();
        let fader_w = theme.layout.fader_width(avail_w, param_count);
        let fader_h = theme.layout.fader_height(ui.available_height().min(200.0));

        ui.vertical(|ui| {
            ui.horizontal_wrapped(|ui| {
                for i in 0..param_count {
                    bridged_fader(ui, bridge, slot, ParamIndex(i), fader_w, fader_h);
                }
            });
        });
    }
}

impl Default for GatePanel {
    fn default() -> Self {
        Self::new()
    }
}
