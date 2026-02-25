//! Sonido Core - DSP primitives for audio effects
//!
//! This crate provides the foundational building blocks for audio DSP, designed for
//! real-time audio processing with zero allocation in the audio path.
//!
//! # Core Abstractions
//!
//! ## Effect System
//!
//! - [`Effect`] - Object-safe trait for all audio effects
//! - [`EffectExt`] - Extension trait for effect chaining
//! - [`Chain`] - Zero-cost effect chain combinator
//!
//! ## Parameter Smoothing
//!
//! Zipper-free parameter changes for click-free automation:
//!
//! - [`SmoothedParam`] - Exponential smoothing (RC-like response)
//! - [`LinearSmoothedParam`] - Linear ramps (constant rate)
//!
//! ## Filters
//!
//! - [`Biquad`] - Second-order IIR filter with RBJ cookbook coefficients
//! - [`StateVariableFilter`] - Multi-output SVF (lowpass, highpass, bandpass simultaneously)
//! - [`CombFilter`] - Comb filter with damping for reverb algorithms
//! - [`ModulatedComb`] - Comb filter with LFO-modulated delay for FDN reverbs
//! - [`AllpassFilter`] - Schroeder allpass for diffusion
//! - [`ModulatedAllpass`] - Allpass filter with LFO-modulated delay for FDN reverbs
//!
//! ## Delay Lines
//!
//! - [`InterpolatedDelay`] - Variable-length delay with interpolation
//! - [`FixedDelayLine`] - Fixed-length delay (compile-time size)
//!
//! ## Modulation & Dynamics
//!
//! - [`Lfo`] - Low-frequency oscillator (5 waveforms)
//! - [`EnvelopeFollower`] - Amplitude envelope detection
//!
//! ## Anti-Aliasing
//!
//! - [`Oversampled`] - Generic wrapper for anti-aliased nonlinear processing (oversampling)
//! - [`Adaa1`] - First-order Anti-Derivative Anti-Aliasing for static waveshapers
//!
//! ## Utilities
//!
//! - Math functions: [`db_to_linear`], [`linear_to_db`], [`fast_tanh`], etc.
//! - Antiderivatives: [`soft_clip_ad`], [`hard_clip_ad`], [`tape_sat_ad`], etc.
//!
//! # no_std Support
//!
//! This crate is `no_std` compatible for embedded audio applications.
//! Disable the default `std` feature in your `Cargo.toml`:
//!
//! ```toml
//! [dependencies]
//! sonido-core = { version = "0.1", default-features = false }
//! ```
//!
//! # Example
//!
//! ```rust,ignore
//! use sonido_core::{Effect, EffectExt, SmoothedParam};
//!
//! // Create effects and chain them with zero-cost static dispatch
//! let mut chain = distortion.chain(chorus).chain(delay);
//!
//! // Process audio sample-by-sample
//! for sample in audio_buffer.iter_mut() {
//!     *sample = chain.process(*sample);
//! }
//!
//! // Or process entire blocks for efficiency
//! chain.process_block(&input, &mut output);
//!
//! // For runtime flexibility, use dynamic dispatch
//! let effects: Vec<Box<dyn Effect>> = vec![
//!     Box::new(distortion),
//!     Box::new(chorus),
//! ];
//! ```
//!
//! # Design Principles
//!
//! - **Real-time safe**: No allocations in audio processing paths
//! - **No dependencies on std**: Pure `no_std` with `libm` for math
//! - **Object-safe traits**: Dynamic dispatch when needed
//! - **Zero-cost abstractions**: Static dispatch chains optimize away

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate alloc;

pub mod adaa;
pub mod allpass;
pub mod biquad;
pub mod comb;
pub mod dc_blocker;
pub mod delay;
pub mod effect;
pub mod envelope;
pub mod fast_math;
pub mod gain;
pub mod graph;
pub mod lfo;
pub mod math;
pub mod modulation;
pub mod one_pole;
pub mod oversample;
pub mod param;
pub mod param_info;
pub mod svf;
pub mod tempo;

// Re-export main types at crate root
pub use adaa::Adaa1;
pub use allpass::{AllpassFilter, ModulatedAllpass};
pub use biquad::{
    Biquad, bandpass_coefficients, highpass_coefficients, lowpass_coefficients, notch_coefficients,
    peaking_eq_coefficients,
};
pub use comb::{CombFilter, ModulatedComb};
pub use dc_blocker::DcBlocker;
pub use delay::{FixedDelayLine, InterpolatedDelay, Interpolation};
pub use effect::{Chain, Effect, EffectExt};
pub use envelope::{DetectionMode, EnvelopeFollower};
pub use fast_math::{
    fast_db_to_linear, fast_exp2, fast_linear_to_db, fast_log2, fast_sin_turns, fast_tan,
};
pub use graph::{
    BufferPool, CompensationDelay, CompiledSchedule, EdgeId, GraphError, NodeId, NodeKind,
    ProcessStep, ProcessingGraph, StereoBuffer,
};
pub use lfo::{Lfo, LfoWaveform};
pub use math::{
    asymmetric_clip, asymmetric_clip_ad, db_to_linear, fast_tanh, flush_denormal, foldback,
    hard_clip, hard_clip_ad, linear_to_db, mono_sum, soft_clip, soft_clip_ad, tape_sat_ad,
    tape_sat_neg_ad, tape_sat_pos_ad, wet_dry_mix, wet_dry_mix_stereo,
};
pub use modulation::{ModulationAmount, ModulationSource};
pub use one_pole::OnePole;
pub use oversample::{MAX_OVERSAMPLE_FACTOR, Oversampled};
pub use param::{LinearSmoothedParam, SmoothedParam};
pub use param_info::{ParamDescriptor, ParamFlags, ParamId, ParamScale, ParamUnit, ParameterInfo};
pub use svf::{FourPoleSvf, StateVariableFilter, SvfOutput};
pub use tempo::{
    DIVISION_LABELS, NoteDivision, TempoContext, TempoManager, TransportState, division_to_index,
    index_to_division,
};
