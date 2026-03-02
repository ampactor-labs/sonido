//! Sonido Effects - Production-ready audio effect implementations
//!
//! This crate provides a comprehensive suite of audio effects built on `sonido-core`,
//! suitable for real-time audio processing, plugins, and embedded systems.
//!
//! # Effect Categories
//!
//! ## Dynamics & Gain
//!
//! - [`Compressor`] - Soft-knee dynamics compressor with attack/release controls
//! - [`Limiter`] - Brickwall lookahead peak limiter with ceiling control
//! - [`Gate`] - Noise gate with threshold, attack, release, and hold
//! - [`CleanPreamp`] - High-headroom gain stage with soft limiting
//!
//! ## Distortion & Saturation
//!
//! - [`Distortion`] - Waveshaping distortion with 4 algorithms (soft clip, hard clip, foldback, asymmetric)
//! - [`Bitcrusher`] - Lo-fi bit depth and sample rate reduction with jitter
//! - [`TapeSaturation`] - Analog tape warmth with asymmetric saturation and HF rolloff
//!
//! ## Modulation
//!
//! - [`Chorus`] - Classic dual-voice stereo chorus
//! - [`Flanger`] - Classic flanger with modulated short delay
//! - [`Phaser`] - Multi-stage allpass phaser with LFO modulation
//! - [`RingMod`] - Ring modulation with sine, triangle, and square carriers
//! - [`Tremolo`] - Amplitude modulation with multiple waveforms
//! - [`MultiVibrato`] - 10-unit tape wow/flutter simulation (original algorithm)
//!
//! ## Time-Based
//!
//! - [`Delay`] - Tape-style feedback delay with interpolation
//! - [`Reverb`] - Freeverb-style algorithmic reverb (8 combs + 4 allpasses)
//!
//! ## Filters
//!
//! - [`LowPassFilter`] - Resonant biquad lowpass filter
//!
//! ## Utility
//!
//! - [`Stage`] - Signal conditioning: gain, phase, width, balance, bass mono, Haas delay
//!
//! # Common Patterns
//!
//! All effects implement the [`Effect`](sonido_core::Effect) trait and follow these patterns:
//!
//! - Constructor: `Effect::new(sample_rate)` - Creates with default parameters
//! - Parameters: `set_xxx()` / `xxx()` - Setters and getters with smoothing
//! - Processing: `process(sample)` or `process_block(&input, &mut output)`
//!
//! # Example: Effect Chain
//!
//! ```rust,ignore
//! use sonido_core::{Effect, EffectExt};
//! use sonido_effects::{Distortion, Chorus, Delay, Reverb};
//!
//! // Create effects
//! let mut dist = Distortion::new(48000.0);
//! dist.set_drive_db(15.0);
//! dist.set_tone_db(3.0);
//!
//! let mut chorus = Chorus::new(48000.0);
//! chorus.set_depth(0.6);
//!
//! let mut delay = Delay::new(48000.0);
//! delay.set_delay_time_ms(375.0); // Dotted eighth at 120 BPM
//! delay.set_feedback(0.4);
//!
//! let reverb = Reverb::new(48000.0);
//!
//! // Chain with zero-cost static dispatch
//! let mut chain = dist.chain(chorus).chain(delay).chain(reverb);
//!
//! // Process audio
//! for sample in audio_buffer.iter_mut() {
//!     *sample = chain.process(*sample);
//! }
//! ```
//!
//! # no_std Support
//!
//! This crate is `no_std` compatible for embedded audio applications.
//! Disable the default `std` feature in your `Cargo.toml`:
//!
//! ```toml
//! [dependencies]
//! sonido-effects = { version = "0.1", default-features = false }
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate alloc;

pub mod bitcrusher;
pub mod chorus;
pub mod compressor;
pub mod delay;
pub mod distortion;
pub mod filter;
pub mod flanger;
pub mod gate;
pub mod kernels;
pub mod limiter;
pub mod multi_vibrato;
pub mod parametric_eq;
pub mod phaser;
pub mod preamp;
pub mod reverb;
pub mod ring_mod;
pub mod stage;
pub mod tape_saturation;
pub mod tremolo;
pub mod wah;

// Re-export main types at crate root
pub use bitcrusher::Bitcrusher;
pub use chorus::Chorus;
pub use compressor::Compressor;
pub use delay::Delay;
pub use distortion::{Distortion, WaveShape};
pub use filter::LowPassFilter;
pub use flanger::Flanger;
pub use gate::Gate;
pub use limiter::Limiter;
pub use multi_vibrato::MultiVibrato;
pub use parametric_eq::ParametricEq;
pub use phaser::Phaser;
pub use preamp::CleanPreamp;
pub use reverb::{Reverb, ReverbType};
pub use ring_mod::{CarrierWaveform, RingMod};
pub use stage::{ChannelMode, HaasSide, Stage};
pub use tape_saturation::TapeSaturation;
pub use tremolo::{Tremolo, TremoloWaveform};
pub use wah::{Wah, WahMode};

// Kernel-architecture implementations
pub use kernels::{
    BitcrusherKernel, BitcrusherParams, ChorusKernel, ChorusParams, CompressorKernel,
    CompressorParams, DistortionKernel, DistortionParams, FilterKernel, FilterParams,
    FlangerKernel, FlangerParams, GateKernel, GateParams, LimiterKernel, LimiterParams,
    PhaserKernel, PhaserParams, PreampKernel, PreampParams, RingModKernel, RingModParams,
    TremoloKernel, TremoloParams, WahKernel, WahParams,
};
