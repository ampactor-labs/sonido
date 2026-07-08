//! Effect UI panels.
//!
//! Every effect renders through one unified [`GenericPanel`] that discovers its
//! parameters from the [`ParamBridge`] at render time and shows **all** of them
//! as knobs — so the node's "N params" badge always matches what's editable, and
//! every parameter is reachable for macro-mapping and A/B morph.
//!
//! The sole exception is the [`LooperPanel`]: a looper is a transport (record /
//! play / overdub), not a bank of continuous knobs, so it keeps a bespoke layout.

pub mod generic;
pub mod looper;

pub use generic::GenericPanel;
pub use looper::LooperPanel;

use crate::{ParamBridge, SlotIndex};
use egui::Ui;

/// A per-parameter A/B morph endpoint provider for a panel's knobs.
///
/// `markers(param_index)` returns `Some((a, b))` when that parameter has a
/// distinct A and B pose — the knob draws cyan/amber ghost ticks for them — or
/// `None` when it is flat (or has no endpoint).
pub type MorphMarkers<'a> = &'a dyn Fn(usize) -> Option<(f32, f32)>;

/// Trait for effect UI panels.
///
/// Panels render controls for a specific effect type, using the
/// [`ParamBridge`] for all parameter access. The `slot` argument
/// identifies which effect in the chain this panel controls.
pub trait EffectPanel: Send + Sync {
    /// The display name of the effect.
    fn name(&self) -> &'static str;

    /// Short name for chain view.
    fn short_name(&self) -> &'static str;

    /// Render the effect's controls.
    ///
    /// `markers` supplies each parameter's A/B morph endpoints for the knob
    /// ghost ticks; see [`MorphMarkers`].
    fn ui(
        &mut self,
        ui: &mut Ui,
        bridge: &dyn ParamBridge,
        slot: SlotIndex,
        markers: MorphMarkers<'_>,
    );
}

impl EffectPanel for LooperPanel {
    fn name(&self) -> &'static str {
        "Looper"
    }
    fn short_name(&self) -> &'static str {
        "Loop"
    }
    fn ui(
        &mut self,
        ui: &mut Ui,
        bridge: &dyn ParamBridge,
        slot: SlotIndex,
        markers: MorphMarkers<'_>,
    ) {
        LooperPanel::ui(self, ui, bridge, slot, markers);
    }
}

/// Create an effect panel for the given registry effect ID.
///
/// The looper gets its bespoke transport panel; every other effect — known or
/// not — gets the unified [`GenericPanel`], which renders all of its parameters
/// as knobs. Returns `None` only for an empty ID.
pub fn create_panel(effect_id: &str) -> Option<Box<dyn EffectPanel + Send + Sync>> {
    match effect_id {
        "looper" => Some(Box::new(LooperPanel::new())),
        other => GenericPanel::try_new(other).map(|p| Box::new(p) as _),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_EFFECT_IDS: [&str; 20] = [
        "preamp",
        "distortion",
        "compressor",
        "gate",
        "eq",
        "wah",
        "chorus",
        "flanger",
        "phaser",
        "tremolo",
        "delay",
        "filter",
        "vibrato",
        "tape",
        "reverb",
        "limiter",
        "bitcrusher",
        "ringmod",
        "stage",
        "looper",
    ];

    #[test]
    fn create_panel_returns_some_for_all_known_ids() {
        for id in &ALL_EFFECT_IDS {
            assert!(
                create_panel(id).is_some(),
                "create_panel({id:?}) returned None"
            );
        }
    }

    #[test]
    fn create_panel_returns_none_for_empty_id() {
        assert!(create_panel("").is_none());
    }

    #[test]
    fn create_panel_looper_keeps_transport_panel() {
        let panel = create_panel("looper").expect("looper panel");
        assert_eq!(panel.name(), "Looper");
        assert_eq!(panel.short_name(), "Loop");
    }

    #[test]
    fn create_panel_unifies_param_effects_on_generic() {
        // A former curated effect and an unknown effect both fall through to the
        // unified generic panel — no per-effect subset panels remain.
        for id in [
            "tape",
            "compressor",
            "reverb",
            "harmonic_habitat",
            "amp",
            "custom_effect",
        ] {
            assert!(
                create_panel(id).is_some(),
                "create_panel({id:?}) should produce a GenericPanel"
            );
        }
    }
}
