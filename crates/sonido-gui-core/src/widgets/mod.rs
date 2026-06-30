//! Audio-specific GUI widgets.
//!
//! Reusable widgets for building audio effect interfaces:
//! - [`Knob`] — Rotary control with drag, fine control, and double-click reset
//! - [`Fader`] — Vertical slot fader with LED-segment fill
//! - [`bridged_knob`] — Bridge-aware knob with auto-format and gesture protocol
//! - [`bridged_knob_fmt`] — Bridge-aware knob with custom formatter
//! - [`bridged_fader`] — Bridge-aware vertical fader with gesture protocol
//! - [`bridged_combo`] — Bridge-aware combo box for enum parameters
//! - [`gesture_wrap`] — Gesture protocol helper for custom widget layouts
//! - [`LevelMeter`] — Dual-color RMS/peak meter: warped dB scale, peak-hold, numeric readout
//! - [`GainReductionMeter`] — Compressor gain reduction display
//! - [`BypassToggle`] — Small bypass indicator for effect panels
//! - [`FootswitchToggle`] — Large pedal-style toggle for the chain view
//! - [`SpectrumWidget`] / [`SpectrumState`] — FFT magnitude display on log frequency axis
//! - [`WaveformWidget`] / [`WaveformState`] — Scrolling time-domain waveform display
//! - [`macro_panel`] / [`MacroView`] — Six-macro (K1–K6) performance row
//! - [`param_macro_menu`] / [`take_macro_action`] — Right-click param → macro mapping

mod bridged_knob;
pub mod fader;
pub mod glow;
mod knob;
pub mod led_display;
mod macro_panel;
mod meter;
mod morph_bar;
mod param_menu;
pub mod spectrum;
mod toggle;
pub mod waveform;

pub use bridged_knob::{
    bridged_combo, bridged_fader, bridged_knob, bridged_knob_fmt, bridged_knob_with_morph,
    gesture_wrap,
};
pub use fader::Fader;
pub use knob::Knob;
pub use led_display::LedDisplay;
pub use macro_panel::{MacroPanelResponse, MacroView, macro_panel};
pub use meter::{GainReductionMeter, LevelMeter};
pub use morph_bar::{MorphBarResponse, morph_bar};
pub use param_menu::{MacroAction, NUM_MACROS, param_macro_menu, take_macro_action};
pub use spectrum::{SpectrumState, SpectrumWidget};
pub use toggle::{BypassToggle, FootswitchToggle};
pub use waveform::{WaveformState, WaveformWidget};
