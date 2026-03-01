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
//! Effects are being migrated from the classic `Effect`-owns-params pattern to the
//! kernel architecture. Both patterns coexist — the rest of the system (graph,
//! registry, plugin, GUI) sees standard `Effect` instances regardless.
//!
//! | Effect | Status |
//! |--------|--------|
//! | Distortion | ✅ Kernel available (`DistortionKernel`) |
//! | _others_ | 🔲 Classic `Effect` (migration pending) |

pub mod distortion;

pub use distortion::{DistortionKernel, DistortionParams};
