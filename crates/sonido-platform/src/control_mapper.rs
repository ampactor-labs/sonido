//! Control-to-parameter mapping for connecting controls to effects.
//!
//! This module provides [`ControlMapper`] which maps platform controls to effect
//! parameters using the [`ParameterInfo`] trait for automatic denormalization.
//!
//! # Example
//!
//! ```rust,ignore
//! use sonido_platform::{ControlMapper, ControlId, ParameterInfo};
//!
//! // Create a mapper with 8 mapping slots
//! let mut mapper = ControlMapper::<8>::new();
//!
//! // Map hardware knob 0 to effect parameter 0
//! mapper.map(ControlId::hardware(0), 0);
//!
//! // In the audio loop
//! if let Some(state) = controller.read_control(ControlId::hardware(0)) {
//!     if state.changed {
//!         if let Some(param_idx) = mapper.get_param_index(ControlId::hardware(0)) {
//!             // Use ParameterInfo to denormalize
//!             if let Some(desc) = effect.param_info(param_idx) {
//!                 let value = desc.denormalize(state.value);
//!                 effect.set_param(param_idx, value);
//!             }
//!         }
//!     }
//! }
//! ```

use crate::{ControlId, ParamDescriptor, ParameterInfo};

/// A single mapping entry from control to parameter.
#[derive(Debug, Clone, Copy, PartialEq)]
struct MappingEntry {
    /// The control ID this mapping is for.
    control_id: ControlId,
    /// The parameter index in the effect.
    param_index: usize,
}

/// Maps controls to effect parameters.
///
/// `ControlMapper` provides a fixed-capacity mapping table that associates
/// control IDs with effect parameter indices. It uses the [`ParameterInfo`]
/// trait to automatically denormalize control values (0.0-1.0) to parameter
/// ranges.
///
/// # Type Parameter
///
/// - `N`: Maximum number of mappings (compile-time constant for no_std support)
///
/// # Example
///
/// ```rust
/// use sonido_platform::{ControlMapper, ControlId};
///
/// // Create a mapper with capacity for 16 mappings
/// let mut mapper = ControlMapper::<16>::new();
///
/// // Map controls to parameters
/// mapper.map(ControlId::hardware(0), 0); // Knob 0 -> param 0
/// mapper.map(ControlId::hardware(1), 1); // Knob 1 -> param 1
/// mapper.map(ControlId::midi(74), 2);    // MIDI CC 74 -> param 2
///
/// // Look up mappings
/// assert_eq!(mapper.get_param_index(ControlId::hardware(0)), Some(0));
/// assert_eq!(mapper.get_param_index(ControlId::midi(74)), Some(2));
/// ```
#[derive(Debug, Clone)]
pub struct ControlMapper<const N: usize> {
    /// Mapping entries (control -> parameter).
    mappings: [Option<MappingEntry>; N],
    /// Number of active mappings.
    count: usize,
}

impl<const N: usize> ControlMapper<N> {
    /// Creates a new empty control mapper.
    pub const fn new() -> Self {
        Self {
            mappings: [None; N],
            count: 0,
        }
    }

    /// Returns the number of active mappings.
    #[inline]
    pub const fn len(&self) -> usize {
        self.count
    }

    /// Returns true if there are no mappings.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Returns the maximum number of mappings.
    #[inline]
    pub const fn capacity(&self) -> usize {
        N
    }

    /// Maps a control to a parameter index.
    ///
    /// If the control is already mapped, updates the mapping.
    /// Returns `true` if the mapping was added/updated, `false` if at capacity.
    ///
    /// # Arguments
    ///
    /// * `control_id` - The control to map
    /// * `param_index` - The effect parameter index to map to
    ///
    /// # Example
    ///
    /// ```rust
    /// use sonido_platform::{ControlMapper, ControlId};
    ///
    /// let mut mapper = ControlMapper::<4>::new();
    /// assert!(mapper.map(ControlId::hardware(0), 0));
    /// assert!(mapper.map(ControlId::hardware(1), 1));
    /// ```
    pub fn map(&mut self, control_id: ControlId, param_index: usize) -> bool {
        // Check if already mapped - update if so
        for entry in self.mappings.iter_mut().flatten() {
            if entry.control_id == control_id {
                entry.param_index = param_index;
                return true;
            }
        }

        // Find empty slot
        for slot in self.mappings.iter_mut() {
            if slot.is_none() {
                *slot = Some(MappingEntry {
                    control_id,
                    param_index,
                });
                self.count += 1;
                return true;
            }
        }

        false // At capacity
    }

    /// Removes the mapping for a control.
    ///
    /// Returns `true` if a mapping was removed, `false` if not found.
    pub fn unmap(&mut self, control_id: ControlId) -> bool {
        for slot in self.mappings.iter_mut() {
            if let Some(entry) = slot
                && entry.control_id == control_id
            {
                *slot = None;
                self.count -= 1;
                return true;
            }
        }
        false
    }

    /// Gets the parameter index mapped to a control.
    ///
    /// Returns `None` if the control is not mapped.
    #[inline]
    pub fn get_param_index(&self, control_id: ControlId) -> Option<usize> {
        for entry in self.mappings.iter().flatten() {
            if entry.control_id == control_id {
                return Some(entry.param_index);
            }
        }
        None
    }

    /// Gets the control ID mapped to a parameter.
    ///
    /// Returns `None` if no control is mapped to this parameter.
    /// If multiple controls map to the same parameter, returns the first found.
    #[inline]
    pub fn get_control_for_param(&self, param_index: usize) -> Option<ControlId> {
        for entry in self.mappings.iter().flatten() {
            if entry.param_index == param_index {
                return Some(entry.control_id);
            }
        }
        None
    }

    /// Clears all mappings.
    pub fn clear(&mut self) {
        for slot in self.mappings.iter_mut() {
            *slot = None;
        }
        self.count = 0;
    }

    /// Denormalizes a control value using the effect's parameter info.
    ///
    /// Converts a normalized control value (0.0-1.0) to the actual parameter
    /// range using the [`ParamDescriptor`] for the mapped parameter.
    ///
    /// Returns `None` if:
    /// - The control is not mapped
    /// - The parameter index is out of range
    /// - The effect doesn't have parameter info for that index
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use sonido_platform::{ControlMapper, ControlId};
    ///
    /// let mut mapper = ControlMapper::<8>::new();
    /// mapper.map(ControlId::hardware(0), 0);
    ///
    /// // Denormalize control value to parameter range
    /// let control_value = 0.5; // Normalized (0.0-1.0)
    /// if let Some(param_value) = mapper.denormalize(ControlId::hardware(0), control_value, &effect) {
    ///     effect.set_param(0, param_value);
    /// }
    /// ```
    pub fn denormalize<E: ParameterInfo>(
        &self,
        control_id: ControlId,
        normalized_value: f32,
        effect: &E,
    ) -> Option<f32> {
        let param_index = self.get_param_index(control_id)?;
        let descriptor = effect.param_info(param_index)?;
        Some(descriptor.denormalize(normalized_value))
    }

    /// Normalizes a parameter value for control display.
    ///
    /// Converts a parameter value to normalized range (0.0-1.0) for
    /// displaying on a control or sending via MIDI.
    ///
    /// Returns `None` if:
    /// - The control is not mapped
    /// - The parameter index is out of range
    /// - The effect doesn't have parameter info for that index
    pub fn normalize<E: ParameterInfo>(
        &self,
        control_id: ControlId,
        param_value: f32,
        effect: &E,
    ) -> Option<f32> {
        let param_index = self.get_param_index(control_id)?;
        let descriptor = effect.param_info(param_index)?;
        Some(descriptor.normalize(param_value))
    }

    /// Applies a control change to an effect.
    ///
    /// Convenience method that denormalizes the control value and sets
    /// the corresponding effect parameter.
    ///
    /// Returns `true` if the parameter was set, `false` if the mapping
    /// or parameter info was not found.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // When a control changes
    /// if let Some(state) = controller.read_control(control_id) {
    ///     if state.changed {
    ///         mapper.apply(control_id, state.value, &mut effect);
    ///     }
    /// }
    /// ```
    pub fn apply<E: ParameterInfo>(
        &self,
        control_id: ControlId,
        normalized_value: f32,
        effect: &mut E,
    ) -> bool {
        if let Some(param_index) = self.get_param_index(control_id)
            && let Some(descriptor) = effect.param_info(param_index)
        {
            let value = descriptor.denormalize(normalized_value);
            effect.set_param(param_index, value);
            return true;
        }
        false
    }

    /// Applies a control change to an effect using a custom transform.
    ///
    /// Like [`apply()`](Self::apply), but instead of linear denormalization,
    /// calls `map_fn` with the parameter descriptor and normalized value.
    /// Use for biased mapping (e.g., noon presets on embedded hardware).
    ///
    /// Returns `true` if the parameter was set, `false` if the mapping
    /// or parameter info was not found.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// mapper.apply_with_fn(ctrl_id, state.value, &mut effect, |desc, norm| {
    ///     adc_to_param_biased(desc, noon_value, norm)
    /// });
    /// ```
    pub fn apply_with_fn<E, F>(
        &self,
        control_id: ControlId,
        normalized_value: f32,
        effect: &mut E,
        map_fn: F,
    ) -> bool
    where
        E: ParameterInfo,
        F: FnOnce(&ParamDescriptor, f32) -> f32,
    {
        if let Some(param_index) = self.get_param_index(control_id)
            && let Some(descriptor) = effect.param_info(param_index)
        {
            let value = map_fn(&descriptor, normalized_value);
            effect.set_param(param_index, value);
            return true;
        }
        false
    }

    /// Gets the parameter descriptor for a mapped control.
    ///
    /// Convenience method for getting the parameter info via control ID.
    pub fn get_param_descriptor<E: ParameterInfo>(
        &self,
        control_id: ControlId,
        effect: &E,
    ) -> Option<ParamDescriptor> {
        let param_index = self.get_param_index(control_id)?;
        effect.param_info(param_index)
    }
}

impl<const N: usize> Default for ControlMapper<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test effect for ParameterInfo
    struct TestEffect {
        gain: f32,
        frequency: f32,
    }

    impl TestEffect {
        fn new() -> Self {
            Self {
                gain: 0.0,
                frequency: 1000.0,
            }
        }
    }

    impl ParameterInfo for TestEffect {
        fn param_count(&self) -> usize {
            2
        }

        fn param_info(&self, index: usize) -> Option<ParamDescriptor> {
            match index {
                0 => Some(ParamDescriptor::gain_db("Gain", "Gain", -60.0, 12.0, 0.0)),
                1 => Some(ParamDescriptor::rate_hz(20.0, 20000.0, 1000.0)),
                _ => None,
            }
        }

        fn get_param(&self, index: usize) -> f32 {
            match index {
                0 => self.gain,
                1 => self.frequency,
                _ => 0.0,
            }
        }

        fn set_param(&mut self, index: usize, value: f32) {
            match index {
                0 => self.gain = value.clamp(-60.0, 12.0),
                1 => self.frequency = value.clamp(20.0, 20000.0),
                _ => {}
            }
        }
    }

    #[test]
    fn test_mapper_new() {
        let mapper = ControlMapper::<8>::new();
        assert_eq!(mapper.len(), 0);
        assert!(mapper.is_empty());
        assert_eq!(mapper.capacity(), 8);
    }

    #[test]
    fn test_mapper_map_and_lookup() {
        let mut mapper = ControlMapper::<8>::new();

        assert!(mapper.map(ControlId::hardware(0), 0));
        assert!(mapper.map(ControlId::hardware(1), 1));

        assert_eq!(mapper.len(), 2);
        assert!(!mapper.is_empty());

        assert_eq!(mapper.get_param_index(ControlId::hardware(0)), Some(0));
        assert_eq!(mapper.get_param_index(ControlId::hardware(1)), Some(1));
        assert_eq!(mapper.get_param_index(ControlId::hardware(2)), None);
    }

    #[test]
    fn test_mapper_update_existing() {
        let mut mapper = ControlMapper::<8>::new();

        mapper.map(ControlId::hardware(0), 0);
        assert_eq!(mapper.get_param_index(ControlId::hardware(0)), Some(0));

        // Update same control to different param
        mapper.map(ControlId::hardware(0), 1);
        assert_eq!(mapper.get_param_index(ControlId::hardware(0)), Some(1));
        assert_eq!(mapper.len(), 1); // Count should not increase
    }

    #[test]
    fn test_mapper_at_capacity() {
        let mut mapper = ControlMapper::<2>::new();

        assert!(mapper.map(ControlId::hardware(0), 0));
        assert!(mapper.map(ControlId::hardware(1), 1));
        assert!(!mapper.map(ControlId::hardware(2), 2)); // Should fail

        assert_eq!(mapper.len(), 2);
    }

    #[test]
    fn test_mapper_unmap() {
        let mut mapper = ControlMapper::<8>::new();

        mapper.map(ControlId::hardware(0), 0);
        mapper.map(ControlId::hardware(1), 1);
        assert_eq!(mapper.len(), 2);

        assert!(mapper.unmap(ControlId::hardware(0)));
        assert_eq!(mapper.len(), 1);
        assert_eq!(mapper.get_param_index(ControlId::hardware(0)), None);
        assert_eq!(mapper.get_param_index(ControlId::hardware(1)), Some(1));

        assert!(!mapper.unmap(ControlId::hardware(0))); // Already removed
    }

    #[test]
    fn test_mapper_get_control_for_param() {
        let mut mapper = ControlMapper::<8>::new();

        mapper.map(ControlId::hardware(0), 0);
        mapper.map(ControlId::midi(74), 1);

        assert_eq!(
            mapper.get_control_for_param(0),
            Some(ControlId::hardware(0))
        );
        assert_eq!(mapper.get_control_for_param(1), Some(ControlId::midi(74)));
        assert_eq!(mapper.get_control_for_param(2), None);
    }

    #[test]
    fn test_mapper_clear() {
        let mut mapper = ControlMapper::<8>::new();

        mapper.map(ControlId::hardware(0), 0);
        mapper.map(ControlId::hardware(1), 1);
        assert_eq!(mapper.len(), 2);

        mapper.clear();
        assert_eq!(mapper.len(), 0);
        assert!(mapper.is_empty());
        assert_eq!(mapper.get_param_index(ControlId::hardware(0)), None);
    }

    #[test]
    fn test_mapper_denormalize() {
        let mut mapper = ControlMapper::<8>::new();
        let effect = TestEffect::new();

        mapper.map(ControlId::hardware(0), 0); // Gain: -60 to 12 dB
        mapper.map(ControlId::hardware(1), 1); // Freq: 20 to 20000 Hz

        // Test gain denormalization
        let gain = mapper.denormalize(ControlId::hardware(0), 0.0, &effect);
        assert_eq!(gain, Some(-60.0));

        let gain = mapper.denormalize(ControlId::hardware(0), 1.0, &effect);
        assert_eq!(gain, Some(12.0));

        let gain = mapper.denormalize(ControlId::hardware(0), 0.5, &effect);
        assert!((gain.unwrap() - (-24.0)).abs() < 0.01);

        // Test unmapped control
        assert_eq!(
            mapper.denormalize(ControlId::hardware(99), 0.5, &effect),
            None
        );
    }

    #[test]
    fn test_mapper_normalize() {
        let mut mapper = ControlMapper::<8>::new();
        let effect = TestEffect::new();

        mapper.map(ControlId::hardware(0), 0); // Gain: -60 to 12 dB

        assert_eq!(
            mapper.normalize(ControlId::hardware(0), -60.0, &effect),
            Some(0.0)
        );
        assert_eq!(
            mapper.normalize(ControlId::hardware(0), 12.0, &effect),
            Some(1.0)
        );
    }

    #[test]
    fn test_mapper_apply() {
        let mut mapper = ControlMapper::<8>::new();
        let mut effect = TestEffect::new();

        mapper.map(ControlId::hardware(0), 0);

        // Apply normalized value
        assert!(mapper.apply(ControlId::hardware(0), 0.5, &mut effect));
        assert!((effect.gain - (-24.0)).abs() < 0.01);

        // Apply to unmapped control
        assert!(!mapper.apply(ControlId::hardware(99), 0.5, &mut effect));
    }

    #[test]
    fn test_mapper_get_param_descriptor() {
        let mut mapper = ControlMapper::<8>::new();
        let effect = TestEffect::new();

        mapper.map(ControlId::hardware(0), 0);

        let desc = mapper.get_param_descriptor(ControlId::hardware(0), &effect);
        assert!(desc.is_some());
        assert_eq!(desc.unwrap().name, "Gain");

        assert!(
            mapper
                .get_param_descriptor(ControlId::hardware(99), &effect)
                .is_none()
        );
    }

    #[test]
    fn test_mapper_default() {
        let mapper = ControlMapper::<8>::default();
        assert!(mapper.is_empty());
    }

    #[test]
    fn test_apply_with_fn_uses_custom_transform() {
        let mut mapper = ControlMapper::<8>::new();
        let mut effect = TestEffect::new();

        mapper.map(ControlId::hardware(0), 0); // Gain: -60 to 12 dB

        // Custom transform that always returns 5.0 regardless of input
        let applied =
            mapper.apply_with_fn(ControlId::hardware(0), 0.5, &mut effect, |_desc, _norm| 5.0);
        assert!(applied);
        assert!((effect.gain - 5.0).abs() < 0.01);

        // Unmapped control should return false
        assert!(!mapper.apply_with_fn(
            ControlId::hardware(99),
            0.5,
            &mut effect,
            |_desc, _norm| 0.0,
        ));
    }
}
