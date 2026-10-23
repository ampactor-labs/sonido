//! Built-in factory rigs, at the end of the pedal's rig list.
//!
//! The same set the graph-player plugin and the editor offer, defined once in
//! `sonido-rigs` and encoded into the firmware image by `build.rs`. Each is
//! decoded like a rig from the bank when the pedal loads it.

extern crate alloc;

use sonido_patch::Patch;

include!(concat!(env!("OUT_DIR"), "/factory_rigs.rs"));

/// Number of factory rigs.
pub const FACTORY_RIG_COUNT: usize = FACTORY_RIGS.len();

/// Factory rig `index` (0-based), or `None` past the last one.
pub fn factory_rig(index: usize) -> Option<Patch> {
    sonido_patch::decode(FACTORY_RIGS.get(index)?).ok()
}
