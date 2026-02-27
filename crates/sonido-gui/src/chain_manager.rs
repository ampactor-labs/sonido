//! Chain command types for GUI→audio thread communication.
//!
//! [`ChainCommand`] is sent over a lock-free channel to mutate the effect
//! chain from the GUI thread. The audio thread drains commands at the start
//! of each buffer.

use sonido_core::ParamDescriptor;
use sonido_gui_core::SlotIndex;
use sonido_registry::EffectWithParams;

/// A command to mutate the effect chain from the GUI thread.
///
/// Commands are sent over a lock-free channel and drained by the audio thread
/// at the start of each buffer. This decouples GUI interaction from real-time
/// processing.
pub enum ChainCommand {
    /// Add a new effect to the end of the chain.
    Add {
        /// Effect identifier (e.g., `"reverb"`, `"distortion"`).
        id: &'static str,
        /// Pre-created effect instance (constructed on the GUI thread).
        effect: Box<dyn EffectWithParams + Send>,
        /// Parameter descriptors for bridge registration (applied atomically on audio thread).
        descriptors: Vec<ParamDescriptor>,
    },
    /// Remove an effect slot from the chain.
    Remove {
        /// Slot index to remove.
        slot: SlotIndex,
    },
}
