//! Distortion effect UI panel.

use crate::theme::SonidoTheme;
use crate::widgets::{BypassToggle, bridged_combo, bridged_fader};
use crate::{ParamBridge, ParamIndex, SlotIndex};
use egui::Ui;

/// Waveshape types matching `sonido_effects::WaveShape`.
const WAVESHAPES: &[&str] = &["Soft Clip", "Hard Clip", "Foldback", "Asymmetric"];

/// UI panel for the distortion effect.
pub struct DistortionPanel;

impl DistortionPanel {
    /// Create a new distortion panel.
    pub fn new() -> Self {
        Self
    }

    /// Render the distortion effect controls.
    ///
    /// Param indices: 0 = drive (dB), 1 = tone (dB), 2 = output (dB),
    /// 3 = waveshape (enum), 4 = mix (%).
    pub fn ui(&mut self, ui: &mut Ui, bridge: &dyn ParamBridge, slot: SlotIndex) {
        let theme = SonidoTheme::get(ui.ctx());
        let fader_indices: &[usize] = &[0, 1, 4, 2];
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

                // Waveshape selector (param 3)
                ui.label("Type:");
                bridged_combo(ui, bridge, slot, ParamIndex(3), "waveshape", WAVESHAPES);
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

impl Default for DistortionPanel {
    fn default() -> Self {
        Self::new()
    }
}
