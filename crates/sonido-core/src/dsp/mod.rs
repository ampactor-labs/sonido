//! Shared DSP sub-chains for complex effect architectures.
//!
//! This module provides reusable building blocks that multiple effects can
//! compose into their signal paths. Sub-chains encapsulate a discrete
//! processing unit (a gain stage, a tone stack) without imposing a specific
//! parameter ownership model.
//!
//! # Modules
//!
//! - [`GainStage`] — Nonlinear gain stage with ADAA anti-aliasing, parameterized
//!   by a waveshaper function and its antiderivative.
//! - [`ToneStack`] — 3-band interactive tone control (bass/mid/treble peaking EQ).
//!
//! # Usage
//!
//! Sub-chains are used inside [`DspKernel`](crate::DspKernel) structs to build
//! complex amp simulations:
//!
//! ```rust,ignore
//! use sonido_core::dsp::{GainStage, ToneStack};
//! use sonido_core::math::{soft_clip, soft_clip_ad};
//!
//! let mut preamp = GainStage::new(soft_clip, soft_clip_ad);
//! preamp.set_pre_gain_db(18.0);
//!
//! let mut tone = ToneStack::new(48000.0);
//! tone.set_controls(0.6, 0.4, 0.7);
//!
//! let out = tone.process(preamp.process(input));
//! ```

pub mod gain_stage;
pub mod tone_stack;

pub use gain_stage::GainStage;
pub use tone_stack::ToneStack;
