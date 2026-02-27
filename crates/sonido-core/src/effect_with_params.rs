//! Combined `Effect` + `ParameterInfo` trait for boxed effects.
//!
//! [`EffectWithParams`] bridges the gap between the object-safe [`Effect`] trait
//! and [`ParameterInfo`]: it provides prefixed methods (`effect_param_count()`,
//! `effect_set_param()`, etc.) that are dispatched through a single vtable. A
//! blanket impl covers every concrete type that implements both traits.
//!
//! This trait lives in `sonido-core` (rather than `sonido-registry`) because
//! both `Effect` and `ParameterInfo` are defined here, and the DAG routing
//! engine (`ProcessingGraph`) stores `Box<dyn EffectWithParams + Send>` to
//! enable runtime parameter access on graph nodes.

#[cfg(not(feature = "std"))]
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
    /// if the index is out of range.
    fn effect_format_value(&self, index: usize, value: f32) -> Option<String>;

    /// Parse display text back to a parameter value.
    ///
    /// Delegates to [`ParamDescriptor::parse_value()`]. Returns `None`
    /// if the index is out of range or parsing fails.
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

    fn effect_format_value(&self, index: usize, value: f32) -> Option<String> {
        self.param_info(index).map(|desc| desc.format_value(value))
    }

    fn effect_parse_value(&self, index: usize, text: &str) -> Option<f32> {
        self.param_info(index)
            .and_then(|desc| desc.parse_value(text))
    }
}
