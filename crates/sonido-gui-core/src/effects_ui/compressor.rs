//! Compressor effect UI panel.

use crate::theme::SonidoTheme;
use crate::widgets::{BypassToggle, bridged_fader};
use crate::{ParamBridge, ParamIndex, SlotIndex};
use egui::Ui;

/// UI panel for the compressor effect.
pub struct CompressorPanel;

impl CompressorPanel {
    /// Create a new compressor panel.
    pub fn new() -> Self {
        Self
    }

    /// Render the compressor effect controls.
    ///
    /// Param indices: 0 = threshold (dB), 1 = ratio, 2 = attack (ms),
    /// 3 = release (ms), 4 = makeup (dB), 10 = mix (%).
    ///
    /// Only continuous fader params are shown; indices 5–9 are internal.
    pub fn ui(&mut self, ui: &mut Ui, bridge: &dyn ParamBridge, slot: SlotIndex) {
        let theme = SonidoTheme::get(ui.ctx());
        let fader_indices: &[usize] = &[0, 1, 2, 3, 4, 10];
        let param_count = fader_indices.len();
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
                for &i in fader_indices {
                    bridged_fader(ui, bridge, slot, ParamIndex(i), fader_w, fader_h);
                }
            });
        });
    }
}

impl Default for CompressorPanel {
    fn default() -> Self {
        Self::new()
    }
}
