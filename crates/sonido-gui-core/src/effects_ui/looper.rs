//! Looper effect UI panel.

use crate::effects_ui::MorphMarkers;
use crate::theme::SonidoTheme;
use crate::widgets::{bridged_combo, bridged_fader};
use crate::{ParamBridge, ParamIndex, SlotIndex};
use egui::Ui;

/// Looper mode labels matching `LooperParams` mode indices.
const MODE_LABELS: &[&str] = &["Stop", "Record", "Play", "Overdub"];

/// Half-speed toggle labels.
const TOGGLE_LABELS: &[&str] = &["Off", "On"];

/// UI panel for the looper effect.
pub struct LooperPanel;

impl LooperPanel {
    /// Create a new looper panel.
    pub fn new() -> Self {
        Self
    }

    /// Render the looper effect controls.
    ///
    /// Param indices:
    /// - 0 = mode (STEPPED: Stop/Record/Play/Overdub)
    /// - 1 = feedback (0–100 %)
    /// - 2 = half_speed (STEPPED: Off/On)
    /// - 3 = reverse (STEPPED: Off/On)
    /// - 4 = mix (0–100 %)
    /// - 5 = output (−20–+6 dB)
    ///
    /// Layout:
    /// - Row 1: Mode combo, Half Speed combo, Reverse combo
    /// - Row 2: Feedback fader, Mix fader, Output fader
    ///
    /// `_markers`: the looper's controls are faders/combos, which carry no A/B
    /// morph ring — its params still morph, they just have no ghost tick here.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        bridge: &dyn ParamBridge,
        slot: SlotIndex,
        _markers: MorphMarkers<'_>,
    ) {
        let theme = SonidoTheme::get(ui.ctx());
        let fader_indices: &[usize] = &[1, 4, 5];
        let fader_count = fader_indices.len();
        let avail_w = ui.available_width();
        let fader_w = theme.layout.fader_width(avail_w, fader_count);
        let fader_h = theme.layout.fader_height(ui.available_height().min(200.0));

        ui.vertical(|ui| {
            // Row 1: stepped controls
            ui.horizontal(|ui| {
                ui.label("Mode:");
                bridged_combo(ui, bridge, slot, ParamIndex(0), "looper_mode", MODE_LABELS);

                ui.add_space(12.0);

                ui.label("Half Spd:");
                bridged_combo(
                    ui,
                    bridge,
                    slot,
                    ParamIndex(2),
                    "looper_half_speed",
                    TOGGLE_LABELS,
                );

                ui.add_space(12.0);

                ui.label("Reverse:");
                bridged_combo(
                    ui,
                    bridge,
                    slot,
                    ParamIndex(3),
                    "looper_reverse",
                    TOGGLE_LABELS,
                );
            });

            ui.add_space(12.0);

            // Row 2: continuous faders
            ui.horizontal_wrapped(|ui| {
                for &i in fader_indices {
                    bridged_fader(ui, bridge, slot, ParamIndex(i), fader_w, fader_h);
                }
            });
        });
    }
}

impl Default for LooperPanel {
    fn default() -> Self {
        Self::new()
    }
}
