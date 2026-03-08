//! Chorus effect UI panel.

use crate::theme::SonidoTheme;
use crate::widgets::{BypassToggle, bridged_combo, bridged_fader};
use crate::{ParamBridge, ParamIndex, SlotIndex};
use egui::Ui;
use sonido_core::DIVISION_LABELS;

/// Sync toggle labels.
const SYNC_LABELS: &[&str] = &["Off", "On"];

/// UI panel for the chorus effect.
pub struct ChorusPanel;

impl ChorusPanel {
    /// Create a new chorus panel.
    pub fn new() -> Self {
        Self
    }

    /// Render the chorus effect controls.
    ///
    /// Param indices: 0 = rate (Hz), 1 = depth (%), 2 = mix (%),
    /// 3 = voices, 4 = feedback (%), 5 = base delay (ms),
    /// 6 = sync (on/off), 7 = division (note value), 8 = output (dB).
    pub fn ui(&mut self, ui: &mut Ui, bridge: &dyn ParamBridge, slot: SlotIndex) {
        let theme = SonidoTheme::get(ui.ctx());
        let fader_indices: &[usize] = &[0, 1, 2, 3, 4, 5, 8];
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

                ui.label("Sync:");
                bridged_combo(ui, bridge, slot, ParamIndex(6), "chorus_sync", SYNC_LABELS);

                ui.add_space(8.0);

                ui.label("Div:");
                bridged_combo(
                    ui,
                    bridge,
                    slot,
                    ParamIndex(7),
                    "chorus_division",
                    DIVISION_LABELS,
                );
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

impl Default for ChorusPanel {
    fn default() -> Self {
        Self::new()
    }
}
