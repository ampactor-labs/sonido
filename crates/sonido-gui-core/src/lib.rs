//! Shared GUI components for the Sonido DSP framework.
//!
//! This crate provides reusable widgets, theme definitions, effect UI panels,
//! and the [`ParamBridge`] trait that decouples GUI parameter access from the
//! underlying storage mechanism. Used by both the standalone dashboard
//! (`sonido-gui`) and CLAP/VST3 plugins (`sonido-plugin`).
//!
//! # Modules
//!
//! - [`param_bridge`] — Parameter bridge trait with gesture protocol for GUI↔audio communication
//! - [`theme`] — Visual styling constants and egui theme application
//! - [`widgets`] — Audio-specific widgets (knobs, meters, toggles)
//! - [`effects_ui`] — Per-effect UI panels (one per effect type)

pub mod effects_ui;
pub mod param_bridge;
pub mod theme;
pub mod widgets;

pub use effects_ui::{EffectPanel, create_panel};
pub use param_bridge::{ChainMutator, ParamBridge, ParamIndex, SlotIndex};
pub use theme::Theme;
pub use widgets::{
    BypassToggle, FootswitchToggle, GainReductionMeter, Knob, LevelMeter, bridged_combo,
    bridged_knob, bridged_knob_fmt, gesture_wrap,
};
