//! Zero-smoothing `DspKernel` wrapper for embedded deployment.
//!
//! [`EmbeddedAdapter`] is the embedded counterpart to [`KernelAdapter`]: same
//! `Effect + ParameterInfo` interface, zero smoothing overhead. Parameters
//! written via `set_param()` are live on the very next `process_stereo()` call.
//!
//! # Why no smoothing?
//!
//! On embedded targets (Daisy Seed / Hothouse), ADC readings are hardware-filtered
//! by analog RC circuits on the PCB and IIR-smoothed in the control task
//! ([`ControlBuffer`](crate::controls::ControlBuffer)). Adding per-sample
//! `SmoothedParam` advancement would be redundant CPU overhead. See ADR-028:
//! "smoothing belongs to the platform layer."
//!
//! # Usage
//!
//! ```ignore
//! use sonido_daisy::EmbeddedAdapter;
//! use sonido_effects::DistortionKernel;
//! use sonido_core::{Effect, ParameterInfo};
//!
//! let mut adapter = EmbeddedAdapter::new_direct(DistortionKernel::new(48000.0));
//! adapter.set_param(0, 20.0);  // Drive = 20 dB — live immediately
//! let (l, r) = adapter.process_stereo(0.5, 0.5);
//! ```
//!
//! [`KernelAdapter`]: sonido_core::kernel::KernelAdapter

use sonido_core::kernel::{Adapter, DirectPolicy};

/// Direct kernel wrapper with zero smoothing overhead.
///
/// Type alias for `Adapter<K, DirectPolicy>`. Implements `Effect + ParameterInfo`
/// (and thus `EffectWithParams` via blanket impl) by delegating directly to the
/// kernel. `set_param()` writes to the kernel's typed params struct immediately —
/// the value is live on the next `process_stereo()` call.
///
/// Use [`Adapter::new_direct`] to construct.
pub type EmbeddedAdapter<K> = Adapter<K, DirectPolicy>;
