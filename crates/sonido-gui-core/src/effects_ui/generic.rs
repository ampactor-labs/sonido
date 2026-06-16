//! Unified UI panel that renders any effect's full parameter set.
//!
//! [`GenericPanel`] renders every visible parameter using [`bridged_knob`] for
//! continuous parameters and [`bridged_combo`] for stepped/enum parameters. It
//! discovers parameter metadata at render time via the [`ParamBridge`], so it
//! works for any effect ID and always shows *all* params — keeping the node's
//! "N params" badge honest and every parameter reachable for macros and morph.
//!
//! This is the panel returned by [`create_panel`](super::create_panel) for every
//! effect except the looper (which has a bespoke transport panel).

use crate::effects_ui::EffectPanel;
use crate::theme::SonidoTheme;
use crate::widgets::{bridged_combo, bridged_knob, param_macro_menu};
use crate::{ParamBridge, ParamIndex, SlotIndex};
use egui::Ui;
use sonido_core::ParamFlags;

/// Fixed column width (px) reserved for each knob + its label/LED readout.
const KNOB_CELL_WIDTH: f32 = 64.0;

/// Unified UI panel for any registered effect.
///
/// Renders all visible parameters as a wrapping bank of knobs, using
/// [`bridged_combo`] for stepped (enum) parameters and [`bridged_knob`]
/// for continuous parameters. Parameters flagged `READ_ONLY` or `HIDDEN`
/// are skipped.
///
/// The display name and short name are derived from the effect ID at
/// construction time (capitalized ID, first 4 characters short).
pub struct GenericPanel {
    /// Effect registry ID (e.g., `"amp"`, `"cabinet"`).
    effect_id: String,
    /// Display name derived from effect ID — leaked as `&'static str` for
    /// the [`EffectPanel`] trait contract.
    name: &'static str,
    /// Short name for chain view — leaked as `&'static str`.
    short_name: &'static str,
}

impl GenericPanel {
    /// Create a generic panel for the given effect ID.
    ///
    /// Returns `Some` for any non-empty effect ID. The display name is
    /// derived by capitalizing the first character of `effect_id`.
    /// The short name is the first four characters, upper-cased.
    pub fn try_new(effect_id: &str) -> Option<Self> {
        if effect_id.is_empty() {
            return None;
        }

        let effect_name: String = {
            let mut chars = effect_id.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        };

        let effect_short: String = effect_id
            .chars()
            .take(4)
            .flat_map(char::to_uppercase)
            .collect();

        // Leak once at construction time — generic panels are created once per
        // effect type and live for the application lifetime.
        let name: &'static str = Box::leak(effect_name.into_boxed_str());
        let short_name: &'static str = Box::leak(effect_short.into_boxed_str());

        Some(Self {
            effect_id: effect_id.to_owned(),
            name,
            short_name,
        })
    }

    /// Effect registry ID.
    pub fn effect_id(&self) -> &str {
        &self.effect_id
    }

    /// Render the generic effect controls.
    ///
    /// Stepped (enum) parameters render as a combo box; continuous parameters
    /// render as knobs that wrap to fit the panel width. `READ_ONLY` and
    /// `HIDDEN` parameters are skipped.
    pub fn ui(&mut self, ui: &mut Ui, bridge: &dyn ParamBridge, slot: SlotIndex) {
        let theme = SonidoTheme::get(ui.ctx());
        let param_count = bridge.param_count(slot);

        if param_count == 0 {
            ui.label(
                egui::RichText::new("No parameters")
                    .font(egui::FontId::monospace(10.0))
                    .color(theme.colors.text_secondary),
            );
            return;
        }

        // Collect visible param indices, split into stepped vs continuous.
        let mut stepped: Vec<usize> = Vec::new();
        let mut continuous: Vec<usize> = Vec::new();

        for i in 0..param_count {
            let desc = bridge.param_descriptor(slot, ParamIndex(i));
            if let Some(ref d) = desc {
                if d.flags.contains(ParamFlags::HIDDEN) || d.flags.contains(ParamFlags::READ_ONLY) {
                    continue;
                }
                if d.flags.contains(ParamFlags::STEPPED) {
                    stepped.push(i);
                } else {
                    continuous.push(i);
                }
            } else {
                continuous.push(i);
            }
        }

        ui.vertical(|ui| {
            // Stepped (combo) params in a horizontal row
            if !stepped.is_empty() {
                ui.horizontal(|ui| {
                    for &i in &stepped {
                        let desc = bridge.param_descriptor(slot, ParamIndex(i));
                        let label_str = desc.as_ref().map_or("Param", |d| d.short_name);
                        ui.label(
                            egui::RichText::new(format!("{label_str}:"))
                                .font(egui::FontId::monospace(10.0))
                                .color(theme.colors.text_secondary),
                        );

                        let id_salt = format!("{}_{}", self.effect_id, i);
                        if let Some(ref d) = desc {
                            let resp = if let Some(labels) = d.step_labels {
                                bridged_combo(ui, bridge, slot, ParamIndex(i), &id_salt, labels)
                            } else {
                                let count = (d.max - d.min).round() as usize + 1;
                                let generated: Vec<String> =
                                    (0..count).map(|n| n.to_string()).collect();
                                let refs: Vec<&str> =
                                    generated.iter().map(String::as_str).collect();
                                bridged_combo(ui, bridge, slot, ParamIndex(i), &id_salt, &refs)
                            };
                            // Right-click a stepped param → map it to a macro
                            // (Snap curve, chosen from the descriptor at bind time).
                            param_macro_menu(&resp, slot, ParamIndex(i));
                        }
                        ui.add_space(8.0);
                    }
                });
                ui.add_space(8.0);
            }

            // Continuous params as knobs, wrapping to fit the panel width. Each
            // knob carries its own LED readout and (via the bridge) its A/B morph
            // ring markers, so the whole effect reads as one bank of knobs.
            if !continuous.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    for &i in &continuous {
                        let label = bridge
                            .param_descriptor(slot, ParamIndex(i))
                            .map_or("", |d| d.short_name);
                        ui.vertical(|ui| {
                            ui.set_width(KNOB_CELL_WIDTH);
                            let resp = bridged_knob(ui, bridge, slot, ParamIndex(i), label);
                            // Right-click a knob → map it to a performance macro.
                            param_macro_menu(&resp, slot, ParamIndex(i));
                        });
                    }
                });
            }
        });
    }
}

impl EffectPanel for GenericPanel {
    fn name(&self) -> &'static str {
        self.name
    }

    fn short_name(&self) -> &'static str {
        self.short_name
    }

    fn ui(&mut self, ui: &mut Ui, bridge: &dyn ParamBridge, slot: SlotIndex) {
        GenericPanel::ui(self, ui, bridge, slot);
    }
}
