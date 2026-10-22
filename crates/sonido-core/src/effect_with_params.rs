//! Combined `Effect` + `ParameterInfo` trait for boxed effects.
//!
//! # Why this trait exists
//!
//! `Box<dyn Effect + ParameterInfo>` is not object-safe in Rust — a trait
//! object can only have a single vtable. `EffectWithParams` merges both
//! capabilities into one trait (one vtable) by re-exporting every
//! [`ParameterInfo`] method with an `effect_` prefix.
//!
//! A blanket impl (`impl<T: Effect + ParameterInfo> EffectWithParams for T`)
//! means **no one implements `EffectWithParams` directly** — it is
//! automatically available for any type that implements both base traits
//! (e.g., `Adapter<K, SmoothedPolicy>`).
//!
//! # Two vocabularies
//!
//! This creates two isomorphic APIs for the same parameter data:
//!
//! - **Direct methods** — `param_count()`, `set_param()`, `get_param()`, etc.
//!   Use these on concrete types or `&dyn ParameterInfo`.
//! - **Prefixed methods** — `effect_param_count()`, `effect_set_param()`, etc.
//!   Use these on `&dyn EffectWithParams` or `Box<dyn EffectWithParams + Send>`.
//!
//! # Where trait objects live
//!
//! [`ProcessingGraph`](crate::graph::ProcessingGraph) and the effect registry
//! store `Box<dyn EffectWithParams + Send>`. This is why the trait lives in
//! `sonido-core` rather than `sonido-registry` — both `Effect` and
//! `ParameterInfo` are defined here, and the graph engine depends on it.

#[cfg(all(feature = "param-text", not(feature = "std")))]
use alloc::string::String;

use crate::effect::Effect;
use crate::param_info::{ParamDescriptor, ParameterInfo};

/// Helper trait to get parameter info from a boxed effect.
///
/// Since `Box<dyn Effect>` doesn't automatically implement `ParameterInfo`,
/// this trait provides a way to access parameter information if the
/// underlying effect implements it.
pub trait EffectWithParams: Effect {
    /// Get the parameter count.
    fn effect_param_count(&self) -> usize;

    /// Get parameter info by index.
    fn effect_param_info(&self, index: usize) -> Option<ParamDescriptor>;

    /// Get parameter value by index.
    fn effect_get_param(&self, index: usize) -> f32;

    /// Set parameter value by index.
    fn effect_set_param(&mut self, index: usize, value: f32);

    /// Format a parameter value as display text.
    ///
    /// Delegates to [`ParamDescriptor::format_value()`]. Returns `None`
    /// if the index is out of range. Needs the `param-text` feature (on with
    /// `std`): every trait object's vtable links this, and with it float
    /// formatting, which the pedal firmware has no room or use for.
    #[cfg(feature = "param-text")]
    fn effect_format_value(&self, index: usize, value: f32) -> Option<String>;

    /// Parse display text back to a parameter value.
    ///
    /// Delegates to [`ParamDescriptor::parse_value()`]. Returns `None`
    /// if the index is out of range or parsing fails. Needs `param-text`.
    #[cfg(feature = "param-text")]
    fn effect_parse_value(&self, index: usize, text: &str) -> Option<f32>;
}

// Implement EffectWithParams for all types that implement both Effect and ParameterInfo
impl<T: Effect + ParameterInfo> EffectWithParams for T {
    fn effect_param_count(&self) -> usize {
        self.param_count()
    }

    fn effect_param_info(&self, index: usize) -> Option<ParamDescriptor> {
        self.param_info(index)
    }

    fn effect_get_param(&self, index: usize) -> f32 {
        self.get_param(index)
    }

    fn effect_set_param(&mut self, index: usize, value: f32) {
        self.set_param(index, value)
    }

    #[cfg(feature = "param-text")]
    fn effect_format_value(&self, index: usize, value: f32) -> Option<String> {
        self.param_info(index).map(|desc| desc.format_value(value))
    }

    #[cfg(feature = "param-text")]
    fn effect_parse_value(&self, index: usize, text: &str) -> Option<f32> {
        self.param_info(index)
            .and_then(|desc| desc.parse_value(text))
    }
}
