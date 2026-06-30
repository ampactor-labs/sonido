//! Sonido GUI - Professional DSP effect processor interface
//!
//! This crate provides a real-time audio effects GUI built on egui,
//! designed for musicians and audio engineers.

pub mod app;
pub mod atomic_param_bridge;
pub mod audio_bridge;
mod audio_processor;
pub mod chain_manager;
#[cfg(not(target_arch = "wasm32"))]
pub mod dfu;
pub mod export;
pub mod file_player;
pub mod graph_view;
pub mod info_view;
pub mod morph_state;
pub mod session;
pub mod signal_generator;
pub mod theme;
pub mod widgets;

pub use app::SonidoApp;
pub use audio_bridge::AtomicParam;
pub use theme::{SonidoTheme, Theme};
