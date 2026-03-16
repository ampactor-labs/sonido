//! Kernel architecture — pure DSP separated from parameter ownership.
//!
//! This module introduces the three-layer separation that enables "one definition,
//! multiple platforms":
//!
//! 1. **[`DspKernel`]** — Pure DSP math. Receives parameters, processes audio.
//!    No smoothing, no atomics, no platform awareness. Compiles identically for
//!    x86_64 and ARM Cortex-M7.
//!
//! 2. **[`KernelParams`]** — Typed parameter struct with metadata. Describes what
//!    parameters exist, their ranges, units, and smoothing preferences. One
//!    definition serves GUIs, plugin hosts, hardware controllers, and presets.
//!
//! 3. **[`Adapter<K, S>`]** — Platform bridge. Wraps a kernel and its params into
//!    the existing [`Effect`](crate::Effect) + [`ParameterInfo`](crate::ParameterInfo)
//!    interface. The smoothing policy `S` controls how parameter changes are applied:
//!    [`SmoothedPolicy`] (desktop/plugin) or [`DirectPolicy`] (embedded).
//!
//! # Why This Exists
//!
//! The original [`Effect`](crate::Effect) trait couples DSP math with parameter ownership — each
//! effect owns its [`SmoothedParam`](crate::SmoothedParam) instances and manages
//! their targets internally. This works for a single platform, but creates tension
//! when the same effect must deploy to:
//!
//! - **Embedded hardware** (Daisy Seed): Knob ADCs have hardware RC filtering;
//!   software smoothing is redundant and wastes cycles.
//! - **Plugin hosts** (CLAP/VST3): Parameters live in atomic stores shared with
//!   the host; smoothing happens at the adapter boundary.
//! - **Preset recall**: Parameters should snap instantly, not smooth.
//!
//! By extracting parameters OUT of the kernel, each platform can supply values
//! in whatever way makes sense — pre-smoothed, raw, snapped — and the kernel
//! code remains identical.
//!
//! # Migration
//!
//! Existing `Effect` implementations continue to work unchanged. New effects can
//! use the kernel architecture from the start. Existing effects can be migrated
//! incrementally — `Adapter<K, SmoothedPolicy>` produces a standard `Effect` that
//! the rest of the system (graph, registry, plugin, GUI) consumes without knowing
//! whether the underlying implementation is a classic `Effect` or a `DspKernel`.
//!
//! See `docs/KERNEL_ARCHITECTURE.md` for the full architecture reference and
//! adding-new-effect checklist.
//!
//! # Example
//!
//! ```rust,ignore
//! use sonido_core::kernel::{DspKernel, KernelParams, Adapter, SmoothingStyle};
//!
//! // 1. Define parameters (one definition for all platforms)
//! struct GainParams { gain_db: f32 }
//! impl KernelParams for GainParams { /* ... */ }
//!
//! // 2. Define pure DSP (no parameter ownership)
//! struct GainKernel;
//! impl DspKernel for GainKernel {
//!     type Params = GainParams;
//!     fn process_stereo(&mut self, l: f32, r: f32, p: &GainParams) -> (f32, f32) {
//!         let g = sonido_core::fast_db_to_linear(p.gain_db);
//!         (l * g, r * g)
//!     }
//!     /* ... */
//! }
//!
//! // 3. Deploy anywhere
//! let adapter = Adapter::new(GainKernel, 48000.0); // → dyn Effect
//! ```
//!
//! # no_std Support
//!
//! This module is fully `no_std` compatible with `alloc` (for `Vec` in the adapter).

mod adapter;
pub mod hot_reload;
pub mod morph;
mod traits;

pub use adapter::{Adapter, DirectPolicy, SmoothedPolicy, SmoothingPolicy};
pub use morph::{MorphCurve, MorphSpace};
pub use traits::{DspKernel, KernelParams, SmoothingStyle};
