//! Kernel-architecture effect implementations.
//!
//! All 20 effects in `sonido-effects` are implemented using the
//! [`DspKernel`](sonido_core::DspKernel) pattern: pure DSP separated from parameter
//! ownership. Each effect defines:
//!
//! - A `Params` struct (parameter values + metadata via [`KernelParams`](sonido_core::KernelParams))
//! - A `Kernel` struct (DSP state only — filters, delay lines, ADAA processors)
//!
//! Kernels are deployed via [`KernelAdapter`](sonido_core::KernelAdapter) for desktop/plugin
//! use, or called directly on embedded targets.

pub mod bitcrusher;
pub mod chorus;
pub mod compressor;
pub mod delay;
pub mod distortion;
pub mod eq;
pub mod filter;
pub mod flanger;
pub mod gate;
pub mod limiter;
pub mod looper;
pub mod phaser;
pub mod preamp;
pub mod reverb;
pub mod ringmod;
pub mod stage;
pub mod tape;
pub mod tremolo;
pub mod vibrato;
pub mod wah;

pub use bitcrusher::{BitcrusherKernel, BitcrusherParams};
pub use chorus::{ChorusKernel, ChorusParams};
pub use compressor::{CompressorKernel, CompressorParams};
pub use delay::{DelayKernel, DelayParams};
pub use distortion::{DistortionKernel, DistortionParams};
pub use eq::{EqKernel, EqParams};
pub use filter::{FilterKernel, FilterParams};
pub use flanger::{FlangerKernel, FlangerParams};
pub use gate::{GateKernel, GateParams};
pub use limiter::{LimiterKernel, LimiterParams};
pub use looper::{LooperKernel, LooperParams};
pub use phaser::{PhaserKernel, PhaserParams};
pub use preamp::{PreampKernel, PreampParams};
pub use reverb::{ReverbKernel, ReverbParams};
pub use ringmod::{RingModKernel, RingModParams};
pub use stage::{StageKernel, StageParams};
pub use tape::{TapeKernel, TapeParams};
pub use tremolo::{TremoloKernel, TremoloParams};
pub use vibrato::{VibratoKernel, VibratoParams};
pub use wah::{WahKernel, WahParams};
