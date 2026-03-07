//! Sonido GUI - Professional DSP effect processor interface
//!
//! This crate provides a real-time audio effects GUI built on egui,
//! designed for musicians and audio engineers.

pub mod app;
pub mod atomic_param_bridge;
pub mod audio_bridge;
mod audio_processor;
pub mod chain_manager;
pub mod file_player;
pub mod graph_view;
pub mod morph_state;
pub mod preset_manager;
pub mod theme;
pub mod widgets;

pub use app::SonidoApp;
pub use audio_bridge::AtomicParam;
pub use preset_manager::{PresetEntry, PresetManager, PresetSource};
pub use sonido_config::Preset;
pub use theme::Theme;
