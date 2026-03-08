//! Phaser effect UI panel.

use crate::theme::SonidoTheme;
use crate::widgets::{bridged_combo, bridged_fader};
use crate::{ParamBridge, ParamIndex, SlotIndex};
use egui::Ui;
use sonido_core::DIVISION_LABELS;

/// Sync toggle labels.
const SYNC_LABELS: &[&str] = &["Off", "On"];

/// UI panel for the phaser effect.
pub struct PhaserPanel;

impl PhaserPanel {
    /// Create a new phaser panel.
    pub fn new() -> Self {
        Self
    }

    /// Render the phaser effect controls.
    ///
    /// Param indices: 0 = rate (Hz), 1 = depth (%), 2 = stages (enum),
    /// 3 = feedback (%), 4 = mix (%), 5 = min freq (Hz), 6 = max freq (Hz),
    /// 7 = sync (on/off), 8 = division (note value), 9 = output (dB).
    pub fn ui(&mut self, ui: &mut Ui, bridge: &dyn ParamBridge, slot: SlotIndex) {
        let theme = SonidoTheme::get(ui.ctx());
        let fader_indices: &[usize] = &[0, 1, 3, 4, 5, 6, 9];
        let param_count = fader_indices.len();
        let avail_w = ui.available_width();
        let fader_w = theme.layout.fader_width(avail_w, param_count);
        let fader_h = theme.layout.fader_height(ui.available_height().min(200.0));

        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                // Stages selector (param 2) — non-sequential values, manual gesture wrap
                ui.label("Stages:");
                let current_stages = bridge.get(slot, ParamIndex(2)) as usize;
                egui::ComboBox::from_id_salt(("phaser_stages", slot.0))
                    .selected_text(format!("{current_stages}"))
                    .show_ui(ui, |ui| {
                        for stages in [2, 4, 6, 8, 10, 12] {
                            if ui
                                .selectable_label(stages == current_stages, format!("{stages}"))
                                .clicked()
                            {
                                bridge.begin_set(slot, ParamIndex(2));
                                bridge.set(slot, ParamIndex(2), stages as f32);
                                bridge.end_set(slot, ParamIndex(2));
                            }
                        }
                    });

                ui.add_space(12.0);

                ui.label("Sync:");
                bridged_combo(ui, bridge, slot, ParamIndex(7), "phaser_sync", SYNC_LABELS);

                ui.add_space(8.0);

                ui.label("Div:");
                bridged_combo(
                    ui,
                    bridge,
                    slot,
                    ParamIndex(8),
                    "phaser_division",
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

impl Default for PhaserPanel {
    fn default() -> Self {
        Self::new()
    }
}
