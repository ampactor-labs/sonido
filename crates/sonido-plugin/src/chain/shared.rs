//! Thread-safe shared state for the multi-effect chain plugin.
//!
//! [`ChainShared`] is the atomic bridge between all plugin threads. Parameter
//! values live in a flat `[AtomicU32; 512]` array indexed by [`ClapParamId`].
//! Slot metadata (effect ID, descriptors, active flag) is published via
//! `ArcSwap` for wait-free reads. Structural commands (add/remove/reorder)
//! flow through a `Mutex<VecDeque>` drained by the audio thread.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

use arc_swap::ArcSwap;
use parking_lot::Mutex;
use sonido_core::ParamDescriptor;

use super::{ClapParamId, MAX_SLOTS, SLOT_STRIDE, TOTAL_PARAMS};

/// Flag indicating a gesture-begin is pending (GUI → audio).
pub const GESTURE_BEGIN: u8 = 1;
/// Flag indicating a gesture-end is pending (GUI → audio).
pub const GESTURE_END: u8 = 2;

/// Snapshot of a single slot's metadata.
///
/// Published via `ArcSwap<Vec<SlotSnapshot>>` so readers get a consistent
/// view of all slots. The audio thread updates this after structural mutations.
///
/// Effect IDs are stored as `&'static str` because they always originate from
/// [`EffectDescriptor::id`](sonido_registry::EffectDescriptor), which is
/// a compile-time string literal. This avoids lifetime issues when the
/// `ParamBridge::effect_id()` return value must outlive the `ArcSwap` guard.
#[derive(Clone, Debug)]
pub struct SlotSnapshot {
    /// Effect registry ID (e.g., `"distortion"`), or empty if slot is vacant.
    pub effect_id: &'static str,
    /// Parameter descriptors for the effect in this slot.
    pub descriptors: Vec<ParamDescriptor>,
    /// Whether this slot is occupied (has an effect).
    pub active: bool,
}

impl SlotSnapshot {
    /// Create an empty (vacant) slot snapshot.
    pub fn empty() -> Self {
        Self {
            effect_id: "",
            descriptors: Vec::new(),
            active: false,
        }
    }

    /// Create an active slot snapshot for the given effect.
    pub fn occupied(effect_id: &'static str, descriptors: Vec<ParamDescriptor>) -> Self {
        Self {
            effect_id,
            descriptors,
            active: true,
        }
    }
}

/// Structural command sent from the GUI thread to the audio thread.
///
/// Commands are queued in [`ChainShared::push_command`] and drained by the
/// audio processor at the start of each block via `try_lock`.
#[derive(Debug)]
pub enum ChainCommand {
    /// Add a new effect to the chain.
    Add {
        /// Effect registry ID (e.g., `"reverb"`).
        effect_id: String,
    },
    /// Remove the effect at the given slot index.
    Remove {
        /// Slot index to clear.
        slot: usize,
    },
    /// Reorder the chain to the given slot sequence.
    Reorder {
        /// New processing order as slot indices.
        new_order: Vec<usize>,
    },
}

/// Inner storage behind `Arc` so `ChainShared` can be cheaply cloned.
struct ChainSharedData {
    /// Parameter values as f32 bit-cast to u32 for atomic access.
    ///
    /// Indexed by `ClapParamId::raw()`. All 512 entries are always allocated;
    /// unoccupied slots hold default values (0.0).
    values: [AtomicU32; TOTAL_PARAMS],

    /// Gesture flags per parameter: bit 0 = begin pending, bit 1 = end pending.
    ///
    /// GUI thread sets flags via `fetch_or`; audio thread clears via `swap(0)`.
    gesture_flags: [AtomicU8; TOTAL_PARAMS],

    /// Per-slot bypass state.
    bypass_flags: [AtomicBool; MAX_SLOTS],

    /// Slot metadata snapshots, published atomically after structural mutations.
    slots: ArcSwap<Vec<SlotSnapshot>>,

    /// Current processing order (indices into the slots vec).
    order: ArcSwap<Vec<usize>>,

    /// Pending structural commands from the GUI thread.
    commands: Mutex<VecDeque<ChainCommand>>,

    /// Set by the audio thread after structural mutations; cleared by main
    /// thread after calling `rescan(INFO | VALUES)`.
    needs_rescan: AtomicBool,

    /// Cached total latency in samples (sum of all active effects).
    latency_samples: AtomicU32,

    /// Host callback: request a `process()` call (for GUI-originated changes).
    request_process: Option<Box<dyn Fn() + Send + Sync>>,

    /// Host callback: request a main-thread callback (for rescan dispatch).
    request_callback: Option<Box<dyn Fn() + Send + Sync>>,
}

/// Shared state accessible from all plugin threads.
///
/// Wraps an `Arc<ChainSharedData>` so it can be cloned into `'static + Send`
/// closures (e.g., the egui bridge render loop).
///
/// # Thread Safety
///
/// - **Values/gestures/bypass**: `AtomicU32`/`AtomicU8`/`AtomicBool` — lock-free.
/// - **Slot metadata**: `ArcSwap::load()` — wait-free reads.
/// - **Commands**: `Mutex` — GUI locks to push, audio `try_lock`s to drain.
/// - **Rescan flag**: `AtomicBool` — audio sets, main thread clears.
#[derive(Clone)]
pub struct ChainShared {
    inner: Arc<ChainSharedData>,
}

// Helper: create a fixed-size array of atomics initialized to a value.
const fn zero_u32() -> AtomicU32 {
    AtomicU32::new(0)
}
const fn zero_u8() -> AtomicU8 {
    AtomicU8::new(0)
}
const fn false_bool() -> AtomicBool {
    AtomicBool::new(false)
}

impl ChainShared {
    /// Create shared state for the chain plugin.
    ///
    /// Starts with all slots empty (hidden from host). The `request_process`
    /// and `request_callback` closures are called to notify the host of
    /// GUI-originated changes and structural mutations respectively.
    pub fn new(
        request_process: Option<Box<dyn Fn() + Send + Sync>>,
        request_callback: Option<Box<dyn Fn() + Send + Sync>>,
    ) -> Self {
        let empty_slots: Vec<SlotSnapshot> =
            (0..MAX_SLOTS).map(|_| SlotSnapshot::empty()).collect();

        Self {
            inner: Arc::new(ChainSharedData {
                values: core::array::from_fn(|_| zero_u32()),
                gesture_flags: core::array::from_fn(|_| zero_u8()),
                bypass_flags: core::array::from_fn(|_| false_bool()),
                slots: ArcSwap::from_pointee(empty_slots),
                order: ArcSwap::from_pointee(Vec::new()),
                commands: Mutex::new(VecDeque::new()),
                needs_rescan: AtomicBool::new(false),
                latency_samples: AtomicU32::new(0),
                request_process,
                request_callback,
            }),
        }
    }

    // ── Parameter access (lock-free) ────────────────────────────────────────

    /// Read the current value of a parameter.
    pub fn get_value(&self, id: ClapParamId) -> f32 {
        f32::from_bits(self.inner.values[id.raw() as usize].load(Ordering::Acquire))
    }

    /// Write a parameter value. Clamps to descriptor bounds if available.
    pub fn set_value(&self, id: ClapParamId, value: f32) {
        let slots = self.inner.slots.load();
        let slot = id.slot();
        let param = id.param();

        let clamped = if let Some(snap) = slots.get(slot)
            && snap.active
            && let Some(desc) = snap.descriptors.get(param)
        {
            value.clamp(desc.min, desc.max)
        } else {
            value
        };

        self.inner.values[id.raw() as usize].store(clamped.to_bits(), Ordering::Release);
    }

    /// Read raw atomic value (no clamping, used by audio thread).
    pub fn get_value_raw(&self, flat_index: usize) -> f32 {
        f32::from_bits(self.inner.values[flat_index].load(Ordering::Acquire))
    }

    /// Write raw atomic value (no clamping, used by audio thread for host events).
    pub fn set_value_raw(&self, flat_index: usize, value: f32) {
        self.inner.values[flat_index].store(value.to_bits(), Ordering::Release);
    }

    // ── Gesture flags ────────────────────────────────────────────────────────

    /// Signal the start of a parameter gesture (GUI drag start).
    pub fn gesture_begin(&self, id: ClapParamId) {
        self.inner.gesture_flags[id.raw() as usize].fetch_or(GESTURE_BEGIN, Ordering::Release);
    }

    /// Signal the end of a parameter gesture (GUI drag stop).
    pub fn gesture_end(&self, id: ClapParamId) {
        self.inner.gesture_flags[id.raw() as usize].fetch_or(GESTURE_END, Ordering::Release);
    }

    /// Atomically read and clear gesture flags for a parameter.
    pub fn take_gesture_flags(&self, flat_index: usize) -> u8 {
        self.inner.gesture_flags[flat_index].swap(0, Ordering::AcqRel)
    }

    // ── Bypass ───────────────────────────────────────────────────────────────

    /// Whether the effect in the given slot is bypassed.
    pub fn is_bypassed(&self, slot: usize) -> bool {
        self.inner
            .bypass_flags
            .get(slot)
            .is_some_and(|f| f.load(Ordering::Acquire))
    }

    /// Set the bypass state for a slot.
    pub fn set_bypassed(&self, slot: usize, bypassed: bool) {
        if let Some(flag) = self.inner.bypass_flags.get(slot) {
            flag.store(bypassed, Ordering::Release);
        }
    }

    // ── Slot metadata ────────────────────────────────────────────────────────

    /// Load the current slot snapshots (wait-free read).
    pub fn load_slots(&self) -> arc_swap::Guard<Arc<Vec<SlotSnapshot>>> {
        self.inner.slots.load()
    }

    /// Publish new slot metadata after a structural mutation.
    pub fn store_slots(&self, slots: Vec<SlotSnapshot>) {
        self.inner.slots.store(Arc::new(slots));
    }

    /// Load the current processing order (wait-free read).
    pub fn load_order(&self) -> arc_swap::Guard<Arc<Vec<usize>>> {
        self.inner.order.load()
    }

    /// Publish a new processing order.
    pub fn store_order(&self, order: Vec<usize>) {
        self.inner.order.store(Arc::new(order));
    }

    // ── Structural commands ──────────────────────────────────────────────────

    /// Queue a structural command from the GUI thread.
    pub fn push_command(&self, cmd: ChainCommand) {
        self.inner.commands.lock().push_back(cmd);
        self.request_callback();
    }

    /// Drain all pending commands (audio thread, non-blocking).
    ///
    /// Returns `None` if the lock is contended (GUI thread is pushing).
    pub fn try_drain_commands(&self) -> Option<VecDeque<ChainCommand>> {
        self.inner
            .commands
            .try_lock()
            .map(|mut q| q.drain(..).collect())
    }

    // ── Rescan flag ──────────────────────────────────────────────────────────

    /// Set the rescan flag (audio thread → main thread).
    pub fn set_needs_rescan(&self) {
        self.inner.needs_rescan.store(true, Ordering::Release);
    }

    /// Check and clear the rescan flag (main thread).
    pub fn take_needs_rescan(&self) -> bool {
        self.inner.needs_rescan.swap(false, Ordering::AcqRel)
    }

    // ── Latency ──────────────────────────────────────────────────────────────

    /// Get the cached latency in samples.
    pub fn latency_samples(&self) -> u32 {
        self.inner.latency_samples.load(Ordering::Acquire)
    }

    /// Update the cached latency.
    pub fn set_latency_samples(&self, samples: u32) {
        self.inner.latency_samples.store(samples, Ordering::Release);
    }

    // ── Host notifications ───────────────────────────────────────────────────

    /// Notify the host that GUI-originated changes are pending.
    pub fn request_process(&self) {
        if let Some(cb) = &self.inner.request_process {
            cb();
        }
    }

    /// Request a main-thread callback from the host.
    pub fn request_callback(&self) {
        if let Some(cb) = &self.inner.request_callback {
            cb();
        }
    }

    /// Initialize parameter values for a slot from its descriptors.
    ///
    /// Sets all params in the slot to their descriptor defaults.
    pub fn init_slot_defaults(&self, slot: usize, descriptors: &[ParamDescriptor]) {
        for (param_idx, desc) in descriptors.iter().enumerate() {
            if let Some(id) = ClapParamId::new(slot, param_idx) {
                self.inner.values[id.raw() as usize]
                    .store(desc.default.to_bits(), Ordering::Release);
            }
        }
    }

    /// Clear all parameter values for a slot (set to 0.0).
    pub fn clear_slot_values(&self, slot: usize) {
        for param_idx in 0..SLOT_STRIDE {
            if let Some(id) = ClapParamId::new(slot, param_idx) {
                self.inner.values[id.raw() as usize].store(0f32.to_bits(), Ordering::Release);
            }
        }
    }

    /// Number of active (occupied) slots.
    pub fn active_slot_count(&self) -> usize {
        let slots = self.inner.slots.load();
        slots.iter().filter(|s| s.active).count()
    }
}

impl clack_plugin::prelude::PluginShared<'_> for ChainShared {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_starts_empty() {
        let shared = ChainShared::new(None, None);
        assert_eq!(shared.active_slot_count(), 0);
        assert_eq!(shared.load_order().len(), 0);
    }

    #[test]
    fn value_roundtrip() {
        let shared = ChainShared::new(None, None);
        let id = ClapParamId::new(0, 0).unwrap();
        shared.set_value_raw(id.raw() as usize, 42.0);
        assert_eq!(shared.get_value(id), 42.0);
    }

    #[test]
    fn value_clamped_when_slot_active() {
        let shared = ChainShared::new(None, None);
        let desc = ParamDescriptor::custom("test", "T", 0.0, 10.0, 5.0);
        let snap = SlotSnapshot::occupied("test", vec![desc]);
        let mut slots: Vec<SlotSnapshot> = (0..MAX_SLOTS).map(|_| SlotSnapshot::empty()).collect();
        slots[0] = snap;
        shared.store_slots(slots);

        let id = ClapParamId::new(0, 0).unwrap();
        shared.set_value(id, 999.0);
        assert_eq!(shared.get_value(id), 10.0);

        shared.set_value(id, -5.0);
        assert_eq!(shared.get_value(id), 0.0);
    }

    #[test]
    fn gesture_flags_roundtrip() {
        let shared = ChainShared::new(None, None);
        let id = ClapParamId::new(1, 3).unwrap();
        let flat = id.raw() as usize;

        assert_eq!(shared.take_gesture_flags(flat), 0);

        shared.gesture_begin(id);
        let flags = shared.take_gesture_flags(flat);
        assert_ne!(flags & GESTURE_BEGIN, 0);
        assert_eq!(flags & GESTURE_END, 0);
        assert_eq!(shared.take_gesture_flags(flat), 0);

        shared.gesture_end(id);
        let flags = shared.take_gesture_flags(flat);
        assert_eq!(flags & GESTURE_BEGIN, 0);
        assert_ne!(flags & GESTURE_END, 0);
    }

    #[test]
    fn gesture_flags_accumulate() {
        let shared = ChainShared::new(None, None);
        let id = ClapParamId::new(0, 0).unwrap();
        let flat = id.raw() as usize;

        shared.gesture_begin(id);
        shared.gesture_end(id);
        let flags = shared.take_gesture_flags(flat);
        assert_ne!(flags & GESTURE_BEGIN, 0);
        assert_ne!(flags & GESTURE_END, 0);
        assert_eq!(shared.take_gesture_flags(flat), 0);
    }

    #[test]
    fn bypass_roundtrip() {
        let shared = ChainShared::new(None, None);
        assert!(!shared.is_bypassed(0));
        shared.set_bypassed(0, true);
        assert!(shared.is_bypassed(0));
        shared.set_bypassed(0, false);
        assert!(!shared.is_bypassed(0));
    }

    #[test]
    fn bypass_out_of_range() {
        let shared = ChainShared::new(None, None);
        assert!(!shared.is_bypassed(MAX_SLOTS + 1));
        shared.set_bypassed(MAX_SLOTS + 1, true); // should not panic
    }

    #[test]
    fn command_queue_drain() {
        let shared = ChainShared::new(None, None);
        shared.push_command(ChainCommand::Add {
            effect_id: "distortion".to_owned(),
        });
        shared.push_command(ChainCommand::Remove { slot: 0 });

        let cmds = shared.try_drain_commands().unwrap();
        assert_eq!(cmds.len(), 2);

        // Queue is now empty
        let cmds2 = shared.try_drain_commands().unwrap();
        assert!(cmds2.is_empty());
    }

    #[test]
    fn rescan_flag() {
        let shared = ChainShared::new(None, None);
        assert!(!shared.take_needs_rescan());
        shared.set_needs_rescan();
        assert!(shared.take_needs_rescan());
        assert!(!shared.take_needs_rescan()); // cleared
    }

    #[test]
    fn slot_metadata_publish() {
        let shared = ChainShared::new(None, None);
        let mut slots: Vec<SlotSnapshot> = (0..MAX_SLOTS).map(|_| SlotSnapshot::empty()).collect();
        slots[0] = SlotSnapshot::occupied("reverb", vec![]);
        slots[2] = SlotSnapshot::occupied("delay", vec![]);
        shared.store_slots(slots);

        assert_eq!(shared.active_slot_count(), 2);
        let loaded = shared.load_slots();
        assert!(loaded[0].active);
        assert_eq!(loaded[0].effect_id, "reverb");
        assert!(!loaded[1].active);
        assert!(loaded[2].active);
        assert_eq!(loaded[2].effect_id, "delay");
    }

    #[test]
    fn init_and_clear_slot_values() {
        let shared = ChainShared::new(None, None);
        let descs = vec![
            ParamDescriptor::custom("a", "A", 0.0, 1.0, 0.5),
            ParamDescriptor::custom("b", "B", 0.0, 100.0, 42.0),
        ];
        shared.init_slot_defaults(3, &descs);

        let id0 = ClapParamId::new(3, 0).unwrap();
        let id1 = ClapParamId::new(3, 1).unwrap();
        assert_eq!(shared.get_value(id0), 0.5);
        assert_eq!(shared.get_value(id1), 42.0);

        shared.clear_slot_values(3);
        assert_eq!(shared.get_value(id0), 0.0);
        assert_eq!(shared.get_value(id1), 0.0);
    }

    #[test]
    fn latency_roundtrip() {
        let shared = ChainShared::new(None, None);
        assert_eq!(shared.latency_samples(), 0);
        shared.set_latency_samples(128);
        assert_eq!(shared.latency_samples(), 128);
    }
}
