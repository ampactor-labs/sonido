//! Reverb effect UI panel.

use crate::theme::SonidoTheme;
use crate::widgets::bridged_fader;
use crate::{ParamBridge, ParamIndex, SlotIndex};
use egui::Ui;

/// UI panel for the reverb effect.
pub struct ReverbPanel;

impl ReverbPanel {
    /// Create a new reverb panel.
    pub fn new() -> Self {
        Self
    }

    /// Render the reverb effect controls.
    ///
    /// Param indices: 0 = room\_size (%), 1 = decay (%), 2 = damping (%),
    /// 3 = predelay (ms), 4 = mix (%), 5 = width (%), 6 = er\_level (%), 7 = output (dB).
    pub fn ui(&mut self, ui: &mut Ui, bridge: &dyn ParamBridge, slot: SlotIndex) {
        let theme = SonidoTheme::get(ui.ctx());
        let param_count = 8;
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

impl Default for ReverbPanel {
    fn default() -> Self {
        Self::new()
    }
}
