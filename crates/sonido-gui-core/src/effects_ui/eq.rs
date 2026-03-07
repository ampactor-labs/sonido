//! Parametric EQ effect UI panel.

use crate::theme::SonidoTheme;
use crate::widgets::{BypassToggle, Knob, gesture_wrap};
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
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                let mut active = !bridge.is_bypassed(slot);
                if ui.add(BypassToggle::new(&mut active, "Active")).changed() {
                    bridge.set_bypassed(slot, !active);
                }
            });

            ui.add_space(12.0);

            // Low band (params 0, 1, 2)
            Self::render_band(
                ui,
                bridge,
                slot,
                "LOW",
                ParamIndex(0),
                ParamIndex(1),
                ParamIndex(2),
            );
            ui.add_space(4.0);
            // Mid band (params 3, 4, 5)
            Self::render_band(
                ui,
                bridge,
                slot,
                "MID",
                ParamIndex(3),
                ParamIndex(4),
                ParamIndex(5),
            );
            ui.add_space(4.0);
            // High band (params 6, 7, 8)
            Self::render_band(
                ui,
                bridge,
                slot,
                "HIGH",
                ParamIndex(6),
                ParamIndex(7),
                ParamIndex(8),
            );
        });
    }

    /// Render a single EQ band (freq, gain, Q) with compact 50px knobs.
    fn render_band(
        ui: &mut Ui,
        bridge: &dyn ParamBridge,
        slot: SlotIndex,
        label: &str,
        freq_idx: ParamIndex,
        gain_idx: ParamIndex,
        q_idx: ParamIndex,
    ) {
        ui.horizontal(|ui| {
            let theme = SonidoTheme::get(ui.ctx());
            ui.label(
                egui::RichText::new(label)
                    .color(theme.colors.cyan)
                    .small(),
            );
            ui.add_space(8.0);

            // Frequency
            let desc = bridge.param_descriptor(slot, freq_idx);
            let (min, max, default) = desc
                .as_ref()
                .map_or((20.0, 20000.0, 1000.0), |d| (d.min, d.max, d.default));
            let mut freq = bridge.get(slot, freq_idx);
            let response = ui.add(
                Knob::new(&mut freq, min, max, "FREQ")
                    .default(default)
                    .format_hz()
                    .diameter(50.0),
            );
            gesture_wrap(&response, bridge, slot, freq_idx, freq, default);

            ui.add_space(8.0);

            // Gain
            let desc = bridge.param_descriptor(slot, gain_idx);
            let (min, max, default) = desc
                .as_ref()
                .map_or((-12.0, 12.0, 0.0), |d| (d.min, d.max, d.default));
            let mut gain = bridge.get(slot, gain_idx);
            let response = ui.add(
                Knob::new(&mut gain, min, max, "GAIN")
                    .default(default)
                    .format_db()
                    .diameter(50.0),
            );
            gesture_wrap(&response, bridge, slot, gain_idx, gain, default);

            ui.add_space(8.0);

            // Q
            let desc = bridge.param_descriptor(slot, q_idx);
            let (min, max, default) = desc
                .as_ref()
                .map_or((0.5, 5.0, 1.0), |d| (d.min, d.max, d.default));
            let mut q = bridge.get(slot, q_idx);
            let response = ui.add(
                Knob::new(&mut q, min, max, "Q")
                    .default(default)
                    .format(|v| format!("{v:.1}"))
                    .diameter(50.0),
            );
            gesture_wrap(&response, bridge, slot, q_idx, q, default);
        });
    }
}

impl Default for ParametricEqPanel {
    fn default() -> Self {
        Self::new()
    }
}
