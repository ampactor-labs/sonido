//! Flanger effect UI panel.

use crate::theme::SonidoTheme;
use crate::widgets::{bridged_combo, bridged_fader};
use crate::{ParamBridge, ParamIndex, SlotIndex};
use egui::Ui;
use sonido_core::DIVISION_LABELS;

/// Sync toggle labels.
const SYNC_LABELS: &[&str] = &["Off", "On"];

/// UI panel for the flanger effect.
pub struct FlangerPanel;

impl FlangerPanel {
    /// Create a new flanger panel.
    pub fn new() -> Self {
        Self
    }

    /// Render the flanger effect controls.
    ///
    /// Param indices: 0 = rate (Hz), 1 = depth (%), 2 = feedback (%),
    /// 3 = mix (%), 4 = TZF (on/off),
    /// 5 = sync (on/off), 6 = division (note value), 7 = output (dB).
    pub fn ui(&mut self, ui: &mut Ui, bridge: &dyn ParamBridge, slot: SlotIndex) {
        let theme = SonidoTheme::get(ui.ctx());
        let fader_indices: &[usize] = &[0, 1, 2, 3, 7];
        let param_count = fader_indices.len();
        let avail_w = ui.available_width();
        let fader_w = theme.layout.fader_width(avail_w, param_count);
        let fader_h = theme.layout.fader_height(ui.available_height().min(200.0));

        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label("TZF:");
                bridged_combo(
                    ui,
                    bridge,
                    slot,
                    ParamIndex(4),
                    "flanger_tzf",
                    &["Off", "On"],
                );

                ui.add_space(12.0);

                ui.label("Sync:");
                bridged_combo(ui, bridge, slot, ParamIndex(5), "flanger_sync", SYNC_LABELS);

                ui.add_space(8.0);

                ui.label("Div:");
                bridged_combo(
                    ui,
                    bridge,
                    slot,
                    ParamIndex(6),
                    "flanger_division",
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

impl Default for FlangerPanel {
    fn default() -> Self {
        Self::new()
    }
}
