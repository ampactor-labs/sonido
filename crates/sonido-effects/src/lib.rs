//! Sonido Effects - Production-ready audio effect implementations
//!
//! This crate provides 36 audio effects (all in [`kernels`]) built on `sonido-core` using the
//! [`DspKernel`](sonido_core::DspKernel) + [`Adapter`](sonido_core::kernel::Adapter)
//! architecture, suitable for real-time audio processing, plugins, and embedded systems.
//!
//! # Effect Categories
//!
//! A selection by category; [`kernels`] has the full set of 36.
//!
//! ## Dynamics & Gain
//!
//! - [`CompressorKernel`] - Soft-knee dynamics compressor with attack/release controls
//! - [`LimiterKernel`] - Brickwall lookahead peak limiter with ceiling control
//! - [`GateKernel`] - Noise gate with threshold, attack, release, and hold
//! - [`PreampKernel`] - High-headroom gain stage with soft limiting
//!
//! ## Amp Simulation
//!
//! - [`AmpKernel`] - Full valve amp sim: dual gain stage + tone stack + sag + presence + bright
//! - [`CabinetKernel`] - Guitar speaker cabinet IR simulator (3 factory IRs, direct convolution)
//!
//! ## Distortion & Saturation
//!
//! - [`DistortionKernel`] - Waveshaping distortion with 4 algorithms (soft clip, hard clip, foldback, asymmetric)
//! - [`BitcrusherKernel`] - Lo-fi bit depth and sample rate reduction with jitter
//! - [`TapeKernel`] - Analog tape warmth with asymmetric saturation and HF rolloff
//!
//! ## Modulation
//!
//! - [`ChorusKernel`] - Stereo chorus with 2 to 4 voices
//! - [`FlangerKernel`] - Classic flanger with modulated short delay
//! - [`PhaserKernel`] - Multi-stage allpass phaser with LFO modulation
//! - [`RingModKernel`] - Ring modulation with sine, triangle, and square carriers
//! - [`TremoloKernel`] - Amplitude modulation with multiple waveforms
//! - [`VibratoKernel`] - 6-unit tape wow/flutter simulation (original algorithm)
//!
//! ## Time-Based
//!
//! - [`DelayKernel`] - Tape-style feedback delay with interpolation
//! - [`ReverbKernel`] - 8-line Hadamard feedback delay network on Freeverb's delay tunings
//!
//! ## Filters
//!
//! - [`FilterKernel`] - Resonant biquad filter (lowpass, highpass, bandpass, notch)
//!
//! ## Pitch & Tuning
//!
//! - [`TunerKernel`] - Chromatic tuner with YIN pitch detection and READ_ONLY diagnostic params
//! - [`PitchShiftKernel`] - Granular pitch shifter with Hann-windowed grain crossfade
//!
//! ## Utility
//!
//! - [`StageKernel`] - Signal conditioning: gain, phase, width, balance, bass mono, Haas delay
//!
//! # Usage
//!
//! Effects are deployed via [`Adapter`](sonido_core::kernel::Adapter): the smoothing
//! adapter (`Adapter::new`) for desktop/plugin use, and the zero-smoothing
//! `Adapter::new_direct` (`DirectPolicy`) on embedded targets. The shipping Daisy
//! pedal firmware wraps every kernel in `Adapter::new_direct`; only a few
//! single-effect test examples call [`DspKernel`](sonido_core::DspKernel) directly.
//!
//! ## Desktop / Plugin (via Adapter)
//!
//! ```rust,ignore
//! use sonido_core::{Effect, EffectExt, ParameterInfo};
//! use sonido_core::kernel::Adapter;
//! use sonido_effects::kernels::{DistortionKernel, ChorusKernel, DelayKernel, ReverbKernel};
//!
//! let sr = 48000.0;
//!
//! // Create effects via Adapter (handles parameter smoothing)
//! let mut dist = Adapter::new(DistortionKernel::new(sr), sr);
//! dist.set_param(0, 15.0); // drive_db
//! dist.set_param(1, 3.0);  // tone_db
//!
//! let chorus = Adapter::new(ChorusKernel::new(sr), sr);
//! let delay = Adapter::new(DelayKernel::new(sr), sr);
//! let reverb = Adapter::new(ReverbKernel::new(sr), sr);
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
//! ## Embedded / Daisy Seed (via `Adapter::new_direct`, as in the pedal firmware)
//!
//! ```rust,ignore
//! use sonido_core::{Effect, ParameterInfo};
//! use sonido_core::kernel::Adapter;
//! use sonido_effects::kernels::DistortionKernel;
//!
//! // Same Effect + ParameterInfo interface as desktop, no per-sample smoothing
//! let mut dist = Adapter::new_direct(DistortionKernel::new(48000.0), 48000.0);
//! dist.set_param(0, 15.0); // drive_db, applied on the next sample
//! let (left, right) = dist.process_stereo(input_l, input_r);
//! ```
//!
//! ## Embedded / Daisy Seed (direct kernel, as in the single-effect test examples)
//!
//! ```rust,ignore
//! use sonido_effects::kernels::{DistortionKernel, DistortionParams};
//! use sonido_core::DspKernel;
//!
//! let mut kernel = DistortionKernel::new(48000.0);
//! let params = DistortionParams::from_knobs(adc_drive, adc_tone, adc_output, adc_shape, adc_mix, adc_dynamics);
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

pub mod calibration;
pub mod kernels;

#[cfg(test)]
pub mod test_utils;

// Re-export kernel types at crate root (Harmonic Habitat is reachable via `kernels::` only)
pub use kernels::{
    AmpKernel, AmpParams, BitcrusherKernel, BitcrusherParams, CabinetKernel, CabinetParams,
    ChorusKernel, ChorusParams, CompressorKernel, CompressorParams, DeesserKernel, DeesserParams,
    DelayKernel, DelayParams, DistortionKernel, DistortionParams, DroneKernel, DroneParams,
    EqKernel, EqParams, FilterKernel, FilterParams, FlangerKernel, FlangerParams, GateKernel,
    GateParams, GlitchKernel, GlitchParams, LimiterKernel, LimiterParams, LooperKernel,
    LooperParams, MultibandCompKernel, MultibandCompParams, PhaserKernel, PhaserParams,
    PitchShiftKernel, PitchShiftParams, PlateReverbKernel, PlateReverbParams, PreampKernel,
    PreampParams, ReverbKernel, ReverbParams, RingModKernel, RingModParams, ShelvingEqKernel,
    ShelvingEqParams, SpringReverbKernel, SpringReverbParams, StageKernel, StageParams,
    StereoWidenerKernel, StereoWidenerParams, TapeKernel, TapeParams, TextureKernel, TextureParams,
    TimeStretchKernel, TimeStretchParams, TransientShaperKernel, TransientShaperParams,
    TremoloKernel, TremoloParams, TunerKernel, TunerParams, VibratoKernel, VibratoParams,
    WahKernel, WahParams,
};
