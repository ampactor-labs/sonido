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
//! | _others_ | 🔲 Classic `Effect` (migration pending) |

pub mod bitcrusher;
pub mod distortion;
pub mod filter;
pub mod gate;
pub mod preamp;
pub mod ring_mod;
pub mod wah;

pub use bitcrusher::{BitcrusherKernel, BitcrusherParams};
pub use distortion::{DistortionKernel, DistortionParams};
pub use filter::{FilterKernel, FilterParams};
pub use gate::{GateKernel, GateParams};
pub use preamp::{PreampKernel, PreampParams};
pub use ring_mod::{RingModKernel, RingModParams};
pub use wah::{WahKernel, WahParams};
