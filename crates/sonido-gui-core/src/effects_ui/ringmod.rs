//! Ring modulator effect UI panel.

use crate::theme::SonidoTheme;
use crate::widgets::{BypassToggle, bridged_combo, bridged_fader};
use crate::{ParamBridge, ParamIndex, SlotIndex};
use egui::Ui;

/// Carrier waveform labels for the combo box.
const WAVEFORMS: &[&str] = &["Sine", "Triangle", "Square"];

/// UI panel for the ring modulator effect.
pub struct RingModPanel;

impl RingModPanel {
    /// Create a new ring modulator panel.
    pub fn new() -> Self {
        Self
    }

    /// Render the ring modulator controls.
    ///
    /// Param indices: 0 = frequency, 1 = depth, 2 = waveform, 3 = mix, 4 = output.
    pub fn ui(&mut self, ui: &mut Ui, bridge: &dyn ParamBridge, slot: SlotIndex) {
        let theme = SonidoTheme::get(ui.ctx());
        let fader_indices: &[usize] = &[0, 1, 3, 4];
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

                ui.add_space(20.0);

                ui.label("Wave:");
                bridged_combo(ui, bridge, slot, ParamIndex(2), "wave", WAVEFORMS);
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

impl Default for RingModPanel {
    fn default() -> Self {
        Self::new()
    }
}
