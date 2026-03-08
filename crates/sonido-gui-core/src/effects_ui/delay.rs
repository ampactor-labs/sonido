//! Delay effect UI panel.

use crate::theme::SonidoTheme;
use crate::widgets::{bridged_combo, bridged_fader};
use crate::{ParamBridge, ParamIndex, SlotIndex};
use egui::Ui;
use sonido_core::DIVISION_LABELS;

/// Sync toggle labels.
const SYNC_LABELS: &[&str] = &["Off", "On"];

/// Ping-pong toggle labels.
const PING_PONG_LABELS: &[&str] = &["Off", "On"];

/// UI panel for the delay effect.
pub struct DelayPanel;

impl DelayPanel {
    /// Create a new delay panel.
    pub fn new() -> Self {
        Self
    }

    /// Render the delay effect controls.
    ///
    /// Param indices: 0 = time (ms), 1 = feedback (%), 2 = mix (%),
    /// 3 = ping pong (on/off), 4 = feedback LP (Hz), 5 = feedback HP (Hz),
    /// 6 = diffusion (%), 7 = sync (on/off), 8 = division (note value),
    /// 9 = output (dB).
    pub fn ui(&mut self, ui: &mut Ui, bridge: &dyn ParamBridge, slot: SlotIndex) {
        let theme = SonidoTheme::get(ui.ctx());
        let fader_indices: &[usize] = &[0, 1, 2, 4, 5, 6, 9];
        let param_count = fader_indices.len();
        let avail_w = ui.available_width();
        let fader_w = theme.layout.fader_width(avail_w, param_count);
        let fader_h = theme.layout.fader_height(ui.available_height().min(200.0));

        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label("Ping Pong:");
                bridged_combo(
                    ui,
                    bridge,
                    slot,
                    ParamIndex(3),
                    "delay_ping_pong",
                    PING_PONG_LABELS,
                );

                ui.add_space(12.0);

                ui.label("Sync:");
                bridged_combo(ui, bridge, slot, ParamIndex(7), "delay_sync", SYNC_LABELS);

                ui.add_space(8.0);

                ui.label("Div:");
                bridged_combo(
                    ui,
                    bridge,
                    slot,
                    ParamIndex(8),
                    "delay_division",
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

impl Default for DelayPanel {
    fn default() -> Self {
        Self::new()
    }
}
