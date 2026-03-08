//! Parametric EQ effect UI panel.

use crate::theme::SonidoTheme;
use crate::widgets::{BypassToggle, bridged_fader};
use crate::{ParamBridge, ParamIndex, SlotIndex};
use egui::Ui;

/// UI panel for the 3-band parametric EQ effect.
pub struct ParametricEqPanel;

impl ParametricEqPanel {
    /// Create a new parametric EQ panel.
    pub fn new() -> Self {
        Self
    }

    /// Render the parametric EQ controls.
    ///
    /// Param indices: 0 = low_freq (Hz), 1 = low_gain (dB), 2 = low_q,
    /// 3 = mid_freq (Hz), 4 = mid_gain (dB), 5 = mid_q,
    /// 6 = high_freq (Hz), 7 = high_gain (dB), 8 = high_q.
    pub fn ui(&mut self, ui: &mut Ui, bridge: &dyn ParamBridge, slot: SlotIndex) {
        let theme = SonidoTheme::get(ui.ctx());
        // 3 params per band, 3 bands = 9 faders
        let params_per_band = 3;
        let avail_w = ui.available_width();
        let fader_w = theme.layout.fader_width(avail_w, params_per_band * 3);
        let fader_h = theme.layout.fader_height(ui.available_height().min(200.0));

        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                let mut active = !bridge.is_bypassed(slot);
                if ui.add(BypassToggle::new(&mut active, "Active")).changed() {
                    bridge.set_bypassed(slot, !active);
                }
            });

            ui.add_space(12.0);

            // Low band (params 0, 1, 2)
            Self::render_band(ui, bridge, slot, "LOW", 0, fader_w, fader_h, &theme);
            ui.add_space(4.0);
            // Mid band (params 3, 4, 5)
            Self::render_band(ui, bridge, slot, "MID", 3, fader_w, fader_h, &theme);
            ui.add_space(4.0);
            // High band (params 6, 7, 8)
            Self::render_band(ui, bridge, slot, "HIGH", 6, fader_w, fader_h, &theme);
        });
    }

    /// Render a single EQ band (freq, gain, Q) as a labeled fader row.
    fn render_band(
        ui: &mut Ui,
        bridge: &dyn ParamBridge,
        slot: SlotIndex,
        label: &str,
        base_idx: usize,
        fader_w: f32,
        fader_h: f32,
        theme: &SonidoTheme,
    ) {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(label)
                    .color(theme.colors.cyan)
                    .small(),
            );
            ui.add_space(8.0);

            for offset in 0..3 {
                bridged_fader(
                    ui,
                    bridge,
                    slot,
                    ParamIndex(base_idx + offset),
                    fader_w,
                    fader_h,
                );
            }
        });
    }
}

impl Default for ParametricEqPanel {
    fn default() -> Self {
        Self::new()
    }
}
