//! Sonido Platform - Platform abstraction layer for hardware controllers
//!
//! This crate provides the abstraction layer for mapping physical controls (knobs,
//! switches, footswitches) and virtual controls (GUI widgets, MIDI CC, automation)
//! to effect parameters.
//!
//! # Core Abstractions
//!
//! ## Control System
//!
//! - [`ControlId`] - Namespaced control identifier (hardware, GUI, MIDI, automation)
//! - [`ControlType`] - Physical/virtual control type (knob, toggle, footswitch, LED)
//! - [`ControlState`] - Current control state with change flag
//!
//! ## Platform Controller
//!
//! - [`PlatformController`] - Trait for hardware/software platform implementations
//! - [`ControlMapper`] - Maps controls to effect parameters using [`ParameterInfo`]
//!
//! # Control ID Namespaces
//!
//! Control IDs use a 16-bit identifier with namespace prefixes:
//!
//! - `0x00XX` - Hardware controls (physical knobs, switches on the device)
//! - `0x01XX` - GUI controls (software UI elements)
//! - `0x02XX` - MIDI controls (CC messages, program changes)
//! - `0x03XX` - Automation (DAW automation lanes)
//!
//! # no_std Support
//!
//! This crate is `no_std` compatible for embedded audio applications.
//! Disable the default `std` feature in your `Cargo.toml`:
//!
//! ```toml
//! [dependencies]
//! sonido-platform = { version = "0.1", default-features = false }
//! ```
//!
//! # Example
//!
//! ```rust,ignore
//! use sonido_platform::{ControlId, ControlType, ControlState, PlatformController};
//!
//! // Define a hardware knob
//! let knob = ControlId::hardware(0x01);
//!
//! // Read the control state
//! if let Some(state) = controller.read_control(knob) {
//!     if state.changed {
//!         // Apply the new value to an effect parameter
//!         effect.set_param(0, mapper.denormalize(knob, state.value));
//!     }
//! }
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod control_context;
pub mod control_mapper;
pub mod graph_mapper;

// Re-export sonido-core types for convenience
pub use sonido_core::{ParamDescriptor, ParamUnit, ParameterInfo};

// Re-export main types at crate root
pub use control_context::ControlContext;
pub use control_mapper::ControlMapper;
pub use graph_mapper::GraphMapper;

/// Namespace identifiers for control sources.
pub mod namespace {
    /// Hardware controls (physical knobs, switches on the device).
    pub const HARDWARE: u16 = 0x0000;
    /// GUI controls (software UI elements).
    pub const GUI: u16 = 0x0100;
    /// MIDI controls (CC messages, program changes).
    pub const MIDI: u16 = 0x0200;
    /// Automation controls (DAW automation lanes).
    pub const AUTOMATION: u16 = 0x0300;
}

/// A namespaced control identifier.
///
/// Control IDs use a 16-bit identifier where the high byte indicates the
/// namespace (source type) and the low byte is the control index within
/// that namespace.
///
/// # Namespaces
///
/// - `0x00XX` - Hardware controls
/// - `0x01XX` - GUI controls
/// - `0x02XX` - MIDI controls
/// - `0x03XX` - Automation controls
///
/// # Example
///
/// ```rust
/// use sonido_platform::ControlId;
///
/// // Hardware knob at index 0
/// let hw_knob = ControlId::hardware(0x00);
/// assert_eq!(hw_knob.raw(), 0x0000);
/// assert!(hw_knob.is_hardware());
///
/// // GUI slider at index 5
/// let gui_slider = ControlId::gui(0x05);
/// assert_eq!(gui_slider.raw(), 0x0105);
/// assert!(gui_slider.is_gui());
///
/// // MIDI CC 74
/// let midi_cc = ControlId::midi(74);
/// assert_eq!(midi_cc.raw(), 0x024A);
/// assert!(midi_cc.is_midi());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ControlId(u16);

impl ControlId {
    /// Creates a new ControlId from a raw 16-bit value.
    #[inline]
    pub const fn from_raw(raw: u16) -> Self {
        Self(raw)
    }

    /// Creates a hardware control ID (namespace 0x00XX).
    #[inline]
    pub const fn hardware(index: u8) -> Self {
        Self(namespace::HARDWARE | index as u16)
    }

    /// Creates a GUI control ID (namespace 0x01XX).
    #[inline]
    pub const fn gui(index: u8) -> Self {
        Self(namespace::GUI | index as u16)
    }

    /// Creates a MIDI control ID (namespace 0x02XX).
    #[inline]
    pub const fn midi(index: u8) -> Self {
        Self(namespace::MIDI | index as u16)
    }

    /// Creates an automation control ID (namespace 0x03XX).
    #[inline]
    pub const fn automation(index: u8) -> Self {
        Self(namespace::AUTOMATION | index as u16)
    }

    /// Returns the raw 16-bit value.
    #[inline]
    pub const fn raw(&self) -> u16 {
        self.0
    }

    /// Returns the namespace portion (high byte).
    #[inline]
    pub const fn namespace(&self) -> u16 {
        self.0 & 0xFF00
    }

    /// Returns the index within the namespace (low byte).
    #[inline]
    pub const fn index(&self) -> u8 {
        (self.0 & 0x00FF) as u8
    }

    /// Returns true if this is a hardware control.
    #[inline]
    pub const fn is_hardware(&self) -> bool {
        self.namespace() == namespace::HARDWARE
    }

    /// Returns true if this is a GUI control.
    #[inline]
    pub const fn is_gui(&self) -> bool {
        self.namespace() == namespace::GUI
    }

    /// Returns true if this is a MIDI control.
    #[inline]
    pub const fn is_midi(&self) -> bool {
        self.namespace() == namespace::MIDI
    }

    /// Returns true if this is an automation control.
    #[inline]
    pub const fn is_automation(&self) -> bool {
        self.namespace() == namespace::AUTOMATION
    }
}

/// Physical or virtual control type.
///
/// Describes the type of control for UI rendering and behavior.
///
/// # Example
///
/// ```rust
/// use sonido_platform::ControlType;
///
/// let control_type = ControlType::Knob;
/// assert!(matches!(control_type, ControlType::Knob));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ControlType {
    /// Rotary knob with continuous value (0.0 to 1.0).
    Knob,
    /// Three-way toggle switch (0.0 = left, 0.5 = center, 1.0 = right).
    Toggle3Way,
    /// Two-way toggle switch (0.0 = off, 1.0 = on).
    Toggle2Way,
    /// Momentary footswitch (pressed = 1.0, released = 0.0).
    Footswitch,
    /// LED indicator (0.0 = off, 1.0 = on, intermediate for brightness).
    Led,
}

impl ControlType {
    /// Returns the number of discrete positions for this control type.
    ///
    /// Returns `None` for continuous controls (Knob, Led).
    ///
    /// # Example
    ///
    /// ```rust
    /// use sonido_platform::ControlType;
    ///
    /// assert_eq!(ControlType::Toggle3Way.discrete_positions(), Some(3));
    /// assert_eq!(ControlType::Toggle2Way.discrete_positions(), Some(2));
    /// assert_eq!(ControlType::Footswitch.discrete_positions(), Some(2));
    /// assert_eq!(ControlType::Knob.discrete_positions(), None);
    /// ```
    #[inline]
    pub const fn discrete_positions(&self) -> Option<u8> {
        match self {
            ControlType::Knob => None,
            ControlType::Toggle3Way => Some(3),
            ControlType::Toggle2Way => Some(2),
            ControlType::Footswitch => Some(2),
            ControlType::Led => None,
        }
    }

    /// Returns true if this is an output control (LED).
    #[inline]
    pub const fn is_output(&self) -> bool {
        matches!(self, ControlType::Led)
    }

    /// Returns true if this is an input control.
    #[inline]
    pub const fn is_input(&self) -> bool {
        !self.is_output()
    }
}

/// Current state of a control.
///
/// Represents the current value of a control along with a flag indicating
/// whether the value has changed since last read.
///
/// # Value Range
///
/// All control values are normalized to the range 0.0 to 1.0:
///
/// - **Knob**: 0.0 = fully counter-clockwise, 1.0 = fully clockwise
/// - **Toggle3Way**: 0.0 = left, 0.5 = center, 1.0 = right
/// - **Toggle2Way**: 0.0 = off, 1.0 = on
/// - **Footswitch**: 0.0 = released, 1.0 = pressed
/// - **LED**: 0.0 = off, 1.0 = full brightness
///
/// # Example
///
/// ```rust
/// use sonido_platform::ControlState;
///
/// let state = ControlState::new(0.75);
/// assert_eq!(state.value, 0.75);
/// assert!(!state.changed);
///
/// let changed_state = ControlState::changed(0.5);
/// assert!(changed_state.changed);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ControlState {
    /// Normalized control value (0.0 to 1.0).
    pub value: f32,
    /// True if the value has changed since last read.
    pub changed: bool,
}

impl ControlState {
    /// Creates a new control state with the given value (not marked as changed).
    #[inline]
    pub const fn new(value: f32) -> Self {
        Self {
            value,
            changed: false,
        }
    }

    /// Creates a control state marked as changed.
    #[inline]
    pub const fn changed(value: f32) -> Self {
        Self {
            value,
            changed: true,
        }
    }

    /// Clears the changed flag.
    #[inline]
    pub fn clear_changed(&mut self) {
        self.changed = false;
    }

    /// Sets a new value and marks the state as changed if different.
    ///
    /// Uses a small epsilon to avoid marking as changed due to floating point noise.
    #[inline]
    pub fn set(&mut self, value: f32) {
        const EPSILON: f32 = 1e-6;
        if (self.value - value).abs() > EPSILON {
            self.value = value;
            self.changed = true;
        }
    }
}

impl Default for ControlState {
    fn default() -> Self {
        Self::new(0.0)
    }
}

/// Trait for platform-specific controller implementations.
///
/// Implement this trait to create a controller for your target platform,
/// whether it's a hardware effects pedal, a GUI application, or a plugin host.
///
/// # Thread Safety
///
/// This trait does not require thread safety. If you need to access the
/// controller from multiple threads, wrap it in appropriate synchronization
/// primitives.
///
/// # Example
///
/// ```rust,ignore
/// use sonido_platform::{PlatformController, ControlId, ControlType, ControlState};
///
/// struct MyPedalController {
///     knob_values: [f32; 4],
///     footswitch_pressed: bool,
/// }
///
/// impl PlatformController for MyPedalController {
///     fn control_count(&self) -> usize { 5 }
///
///     fn control_id(&self, index: usize) -> Option<ControlId> {
///         match index {
///             0..=3 => Some(ControlId::hardware(index as u8)),
///             4 => Some(ControlId::hardware(0x10)), // Footswitch
///             _ => None,
///         }
///     }
///
///     fn control_type(&self, id: ControlId) -> Option<ControlType> {
///         match id.index() {
///             0..=3 => Some(ControlType::Knob),
///             0x10 => Some(ControlType::Footswitch),
///             _ => None,
///         }
///     }
///
///     fn read_control(&self, id: ControlId) -> Option<ControlState> {
///         match id.index() {
///             i @ 0..=3 => Some(ControlState::new(self.knob_values[i as usize])),
///             0x10 => Some(ControlState::new(if self.footswitch_pressed { 1.0 } else { 0.0 })),
///             _ => None,
///         }
///     }
///
///     fn write_control(&mut self, _id: ControlId, _value: f32) -> bool {
///         // No writable controls (LEDs) in this example
///         false
///     }
/// }
/// ```
pub trait PlatformController {
    /// Returns the total number of controls on this platform.
    fn control_count(&self) -> usize;

    /// Returns the control ID at the given index.
    ///
    /// Returns `None` if `index >= control_count()`.
    fn control_id(&self, index: usize) -> Option<ControlId>;

    /// Returns the type of the specified control.
    ///
    /// Returns `None` if the control ID is not recognized.
    fn control_type(&self, id: ControlId) -> Option<ControlType>;

    /// Reads the current state of a control.
    ///
    /// Returns `None` if the control ID is not recognized or is not readable.
    fn read_control(&self, id: ControlId) -> Option<ControlState>;

    /// Writes a value to a control (for output controls like LEDs).
    ///
    /// Returns `true` if the write was successful, `false` if the control
    /// is not writable or the ID is not recognized.
    ///
    /// The value should be normalized (0.0 to 1.0).
    fn write_control(&mut self, id: ControlId, value: f32) -> bool;

    /// Updates the controller state by reading from hardware/OS.
    ///
    /// Call this at the start of each processing cycle to poll for changes.
    /// Default implementation does nothing (for controllers with interrupt-driven updates).
    fn poll(&mut self) {}

    /// Flushes pending output changes to hardware/OS.
    ///
    /// Call this at the end of each processing cycle to apply LED changes, etc.
    /// Default implementation does nothing.
    fn flush(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_control_id_hardware() {
        let id = ControlId::hardware(0x05);
        assert_eq!(id.raw(), 0x0005);
        assert_eq!(id.namespace(), namespace::HARDWARE);
        assert_eq!(id.index(), 0x05);
        assert!(id.is_hardware());
        assert!(!id.is_gui());
        assert!(!id.is_midi());
        assert!(!id.is_automation());
    }

    #[test]
    fn test_control_id_gui() {
        let id = ControlId::gui(0x0A);
        assert_eq!(id.raw(), 0x010A);
        assert_eq!(id.namespace(), namespace::GUI);
        assert_eq!(id.index(), 0x0A);
        assert!(!id.is_hardware());
        assert!(id.is_gui());
    }

    #[test]
    fn test_control_id_midi() {
        let id = ControlId::midi(74); // MIDI CC 74
        assert_eq!(id.raw(), 0x024A);
        assert_eq!(id.namespace(), namespace::MIDI);
        assert_eq!(id.index(), 74);
        assert!(id.is_midi());
    }

    #[test]
    fn test_control_id_automation() {
        let id = ControlId::automation(0x00);
        assert_eq!(id.raw(), 0x0300);
        assert!(id.is_automation());
    }

    #[test]
    fn test_control_id_from_raw() {
        let id = ControlId::from_raw(0x0205);
        assert_eq!(id.namespace(), namespace::MIDI);
        assert_eq!(id.index(), 0x05);
    }

    #[test]
    fn test_control_type_discrete_positions() {
        assert_eq!(ControlType::Knob.discrete_positions(), None);
        assert_eq!(ControlType::Toggle3Way.discrete_positions(), Some(3));
        assert_eq!(ControlType::Toggle2Way.discrete_positions(), Some(2));
        assert_eq!(ControlType::Footswitch.discrete_positions(), Some(2));
        assert_eq!(ControlType::Led.discrete_positions(), None);
    }

    #[test]
    fn test_control_type_input_output() {
        assert!(ControlType::Knob.is_input());
        assert!(ControlType::Toggle3Way.is_input());
        assert!(ControlType::Toggle2Way.is_input());
        assert!(ControlType::Footswitch.is_input());
        assert!(!ControlType::Led.is_input());

        assert!(!ControlType::Knob.is_output());
        assert!(ControlType::Led.is_output());
    }

    #[test]
    fn test_control_state_new() {
        let state = ControlState::new(0.5);
        assert_eq!(state.value, 0.5);
        assert!(!state.changed);
    }

    #[test]
    fn test_control_state_changed() {
        let state = ControlState::changed(0.75);
        assert_eq!(state.value, 0.75);
        assert!(state.changed);
    }

    #[test]
    fn test_control_state_set() {
        let mut state = ControlState::new(0.0);
        assert!(!state.changed);

        state.set(0.5);
        assert!(state.changed);
        assert_eq!(state.value, 0.5);

        state.clear_changed();
        assert!(!state.changed);

        // Setting the same value should not mark as changed
        state.set(0.5);
        assert!(!state.changed);

        // Setting a different value should mark as changed
        state.set(0.6);
        assert!(state.changed);
    }

    #[test]
    fn test_control_state_default() {
        let state = ControlState::default();
        assert_eq!(state.value, 0.0);
        assert!(!state.changed);
    }
}
