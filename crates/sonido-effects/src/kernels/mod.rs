//! Kernel-architecture effect implementations.
//!
//! This module contains effects implemented using the [`DspKernel`](sonido_core::DspKernel)
//! pattern: pure DSP separated from parameter ownership. Each kernel defines:
//!
//! - A `Params` struct (parameter values + metadata via [`KernelParams`](sonido_core::KernelParams))
//! - A `Kernel` struct (DSP state only — filters, delay lines, ADAA processors)
//!
//! Kernels are deployed via [`KernelAdapter`](sonido_core::KernelAdapter) for desktop/plugin
//! use, or called directly on embedded targets.
//!
//! # Migration Status
//!
//! | Effect | Status |
//! |--------|--------|
//! | Distortion | ✅ Kernel |
//! | CleanPreamp | ✅ Kernel |
//! | LowPassFilter | ✅ Kernel |
//! | Gate | ✅ Kernel |
//! | Bitcrusher | ✅ Kernel |
//! | RingMod | ✅ Kernel |
//! | Wah | ✅ Kernel |
//! | Tremolo | ✅ Kernel |
//! | Limiter | ✅ Kernel |
//! | Flanger | ✅ Kernel |
//! | Chorus | ✅ Kernel |
//! | Phaser | ✅ Kernel |
//! | Compressor | ✅ Kernel |
//! | MultiVibrato | ✅ Kernel |
//! | ParametricEq | ✅ Kernel |
//! | Delay | ✅ Kernel |
//! | Stage | ✅ Kernel |
//! | Reverb | ✅ Kernel |
//! | TapeSaturation | ✅ Kernel |

pub mod bitcrusher;
pub mod chorus;
pub mod compressor;
pub mod delay;
pub mod distortion;
pub mod filter;
pub mod flanger;
pub mod gate;
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

pub use bitcrusher::{BitcrusherKernel, BitcrusherParams};
pub use chorus::{ChorusKernel, ChorusParams};
pub use compressor::{CompressorKernel, CompressorParams};
pub use delay::{DelayKernel, DelayParams};
pub use distortion::{DistortionKernel, DistortionParams};
pub use filter::{FilterKernel, FilterParams};
pub use flanger::{FlangerKernel, FlangerParams};
pub use gate::{GateKernel, GateParams};
pub use limiter::{LimiterKernel, LimiterParams};
pub use multi_vibrato::{MultiVibratoKernel, MultiVibratoParams};
pub use parametric_eq::{ParametricEqKernel, ParametricEqParams};
pub use phaser::{PhaserKernel, PhaserParams};
pub use preamp::{PreampKernel, PreampParams};
pub use reverb::{ReverbKernel, ReverbParams};
pub use ring_mod::{RingModKernel, RingModParams};
pub use stage::{StageKernel, StageParams};
pub use tape_saturation::{TapeSaturationKernel, TapeSaturationParams};
pub use tremolo::{TremoloKernel, TremoloParams};
pub use wah::{WahKernel, WahParams};
