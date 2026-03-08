//! Stage (signal conditioning / stereo utility) effect UI panel.

use crate::theme::SonidoTheme;
use crate::widgets::{bridged_combo, bridged_fader};
use crate::{ParamBridge, ParamIndex, SlotIndex};
use egui::Ui;

/// Channel mode labels for the combo box.
const CHANNEL_MODES: &[&str] = &["Normal", "Swap", "Mono L", "Mono R"];

/// On/off labels reused for toggle-style combos.
const ON_OFF: &[&str] = &["Off", "On"];

/// Haas side labels.
const HAAS_SIDES: &[&str] = &["Left", "Right"];

/// UI panel for the Stage effect.
pub struct StagePanel;

impl StagePanel {
    /// Create a new Stage panel.
    pub fn new() -> Self {
        Self
    }

    /// Render the Stage controls.
    ///
    /// Param indices: 0 = gain, 1 = width, 2 = balance, 3 = phase L,
    /// 4 = phase R, 5 = channel, 6 = DC block, 7 = bass mono,
    /// 8 = bass freq, 9 = haas, 10 = haas side, 11 = output.
    pub fn ui(&mut self, ui: &mut Ui, bridge: &dyn ParamBridge, slot: SlotIndex) {
        let theme = SonidoTheme::get(ui.ctx());
        let fader_indices: &[usize] = &[0, 1, 2, 8, 9, 11];
        let param_count = fader_indices.len();
        let avail_w = ui.available_width();
        let fader_w = theme.layout.fader_width(avail_w, param_count);
        let fader_h = theme.layout.fader_height(ui.available_height().min(200.0));

        ui.vertical(|ui| {
            // -- Header: channel mode --
            ui.horizontal(|ui| {
                ui.label("Chan:");
                bridged_combo(ui, bridge, slot, ParamIndex(5), "chan", CHANNEL_MODES);
            });

            ui.add_space(8.0);

            // -- Toggle row: Phase L, Phase R, DC Block, Bass Mono, Haas Side --
            ui.horizontal(|ui| {
                ui.label("Phase L:");
                bridged_combo(ui, bridge, slot, ParamIndex(3), "phl", ON_OFF);
                ui.add_space(12.0);
                ui.label("Phase R:");
                bridged_combo(ui, bridge, slot, ParamIndex(4), "phr", ON_OFF);
                ui.add_space(12.0);
                ui.label("DC Block:");
                bridged_combo(ui, bridge, slot, ParamIndex(6), "dc", ON_OFF);
                ui.add_space(12.0);
                ui.label("Bass Mono:");
                bridged_combo(ui, bridge, slot, ParamIndex(7), "bmono", ON_OFF);
                ui.add_space(12.0);
                ui.label("Side:");
                bridged_combo(ui, bridge, slot, ParamIndex(10), "hside", HAAS_SIDES);
            });

            ui.add_space(12.0);

            // -- Continuous faders --
            ui.horizontal_wrapped(|ui| {
                for &i in fader_indices {
                    bridged_fader(ui, bridge, slot, ParamIndex(i), fader_w, fader_h);
                }
            });
        });
    }
}

impl Default for StagePanel {
    fn default() -> Self {
        Self::new()
    }
}
