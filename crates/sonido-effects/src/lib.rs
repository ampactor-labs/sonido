//! Sonido Effects - Production-ready audio effect implementations
//!
//! This crate provides 19 audio effects built on `sonido-core` using the
//! [`DspKernel`](sonido_core::DspKernel) + [`KernelAdapter`](sonido_core::KernelAdapter)
//! architecture, suitable for real-time audio processing, plugins, and embedded systems.
//!
//! # Effect Categories
//!
//! ## Dynamics & Gain
//!
//! - [`CompressorKernel`] - Soft-knee dynamics compressor with attack/release controls
//! - [`LimiterKernel`] - Brickwall lookahead peak limiter with ceiling control
//! - [`GateKernel`] - Noise gate with threshold, attack, release, and hold
//! - [`PreampKernel`] - High-headroom gain stage with soft limiting
//!
//! ## Distortion & Saturation
//!
//! - [`DistortionKernel`] - Waveshaping distortion with 4 algorithms (soft clip, hard clip, foldback, asymmetric)
//! - [`BitcrusherKernel`] - Lo-fi bit depth and sample rate reduction with jitter
//! - [`TapeKernel`] - Analog tape warmth with asymmetric saturation and HF rolloff
//!
//! ## Modulation
//!
//! - [`ChorusKernel`] - Classic dual-voice stereo chorus
//! - [`FlangerKernel`] - Classic flanger with modulated short delay
//! - [`PhaserKernel`] - Multi-stage allpass phaser with LFO modulation
//! - [`RingModKernel`] - Ring modulation with sine, triangle, and square carriers
//! - [`TremoloKernel`] - Amplitude modulation with multiple waveforms
//! - [`VibratoKernel`] - 10-unit tape wow/flutter simulation (original algorithm)
//!
//! ## Time-Based
//!
//! - [`DelayKernel`] - Tape-style feedback delay with interpolation
//! - [`ReverbKernel`] - Freeverb-style algorithmic reverb (8 combs + 4 allpasses)
//!
//! ## Filters
//!
//! - [`FilterKernel`] - Resonant state-variable filter (lowpass, highpass, bandpass)
//!
//! ## Utility
//!
//! - [`StageKernel`] - Signal conditioning: gain, phase, width, balance, bass mono, Haas delay
//!
//! # Usage
//!
//! Effects are deployed via [`KernelAdapter`](sonido_core::KernelAdapter) for desktop/plugin
//! use, or called directly on embedded targets via [`DspKernel`](sonido_core::DspKernel).
//!
//! ## Desktop / Plugin (via KernelAdapter)
//!
//! ```rust,ignore
//! use sonido_core::{Effect, EffectExt, KernelAdapter, ParameterInfo};
//! use sonido_effects::kernels::{DistortionKernel, ChorusKernel, DelayKernel, ReverbKernel};
//!
//! let sr = 48000.0;
//!
//! // Create effects via KernelAdapter (handles parameter smoothing)
//! let mut dist = KernelAdapter::new(DistortionKernel::new(sr), sr);
//! dist.set_param(0, 15.0); // drive_db
//! dist.set_param(1, 3.0);  // tone_db
//!
//! let chorus = KernelAdapter::new(ChorusKernel::new(sr), sr);
//! let delay = KernelAdapter::new(DelayKernel::new(sr), sr);
//! let reverb = KernelAdapter::new(ReverbKernel::new(sr), sr);
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
//! ## Embedded / Daisy Seed (direct kernel)
//!
//! ```rust,ignore
//! use sonido_effects::kernels::{DistortionKernel, DistortionParams};
//! use sonido_core::DspKernel;
//!
//! let mut kernel = DistortionKernel::new(48000.0);
//! let params = DistortionParams::from_knobs(adc_drive, adc_tone, adc_output, adc_mix);
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
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

pub mod kernels;

// Re-export all kernel types at crate root
pub use kernels::{
    BitcrusherKernel, BitcrusherParams, ChorusKernel, ChorusParams, CompressorKernel,
    CompressorParams, DelayKernel, DelayParams, DistortionKernel, DistortionParams, EqKernel,
    EqParams, FilterKernel, FilterParams, FlangerKernel, FlangerParams, GateKernel, GateParams,
    LimiterKernel, LimiterParams, PhaserKernel, PhaserParams, PreampKernel, PreampParams,
    ReverbKernel, ReverbParams, RingModKernel, RingModParams, StageKernel, StageParams, TapeKernel,
    TapeParams, TremoloKernel, TremoloParams, VibratoKernel, VibratoParams, WahKernel, WahParams,
};
