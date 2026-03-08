//! Tape saturation effect UI panel.

use crate::theme::SonidoTheme;
use crate::widgets::bridged_fader;
use crate::{ParamBridge, ParamIndex, SlotIndex};
use egui::Ui;

/// UI panel for the tape saturation effect.
pub struct TapePanel;

impl TapePanel {
    /// Create a new tape saturation panel.
    pub fn new() -> Self {
        Self
    }

    /// Render the tape saturation controls.
    ///
    /// Param indices: 0 = drive (dB), 1 = saturation (%).
    pub fn ui(&mut self, ui: &mut Ui, bridge: &dyn ParamBridge, slot: SlotIndex) {
        let theme = SonidoTheme::get(ui.ctx());
        let param_count = 2;
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

impl Default for TapePanel {
    fn default() -> Self {
        Self::new()
    }
}
