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
//! - [`LevelMeter`] — Continuous dual-bar (RMS + peak) meter with dB scale
//! - [`GainReductionMeter`] — Compressor gain reduction display
//! - [`BypassToggle`] — Small bypass indicator for effect panels
//! - [`FootswitchToggle`] — Large pedal-style toggle for the chain view
//! - [`SpectrumWidget`] / [`SpectrumState`] — FFT magnitude display on log frequency axis
//! - [`WaveformWidget`] / [`WaveformState`] — Scrolling time-domain waveform display

mod bridged_knob;
pub mod fader;
pub mod glow;
mod knob;
pub mod led_display;
mod meter;
mod morph_bar;
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
pub use meter::{GainReductionMeter, LevelMeter};
pub use morph_bar::{MorphBarResponse, morph_bar};
pub use spectrum::{SpectrumState, SpectrumWidget};
pub use toggle::{BypassToggle, FootswitchToggle};
pub use waveform::{WaveformState, WaveformWidget};
