//! Thread-safe shared state for sonido CLAP plugins.
//!
//! `SonidoShared` lives for the lifetime of the plugin instance and is
//! accessible from both the main thread (params, state) and the audio
//! thread (processing). Parameter values are stored as atomic `u32`
//! (f32 bit-cast) for lock-free access.

use sonido_core::ParamDescriptor;
use sonido_registry::EffectRegistry;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

/// Inner storage for plugin shared state.
///
/// Holds all data behind an `Arc` so that `SonidoShared` can be cheaply
/// cloned into `'static + Send` GUI closures.
struct SonidoSharedData {
    /// Registry ID of the effect (e.g., `"distortion"`).
    effect_id: &'static str,
    /// Parameter descriptors, indexed by parameter position.
    descriptors: Vec<ParamDescriptor>,
    /// Current parameter values as f32 bit-cast to u32 for atomic access.
    values: Vec<AtomicU32>,
    /// Whether the effect is bypassed (GUI toggle, not host bypass).
    bypassed: AtomicBool,
    /// Gesture flags per parameter: bit 0 = begin pending, bit 1 = end pending.
    ///
    /// GUI thread sets flags via `fetch_or`; audio thread clears via `swap(0)`.
    gesture_flags: Vec<AtomicU8>,
    /// Cached latency in samples, written by audio thread during `activate()`.
    ///
    /// Read by the main thread to implement `PluginLatencyImpl::get()`.
    latency_samples: AtomicU32,
    /// Host notification callback, called from GUI thread to request
    /// the host to schedule a `process()`/`flush()` call.
    ///
    /// Without this, GUI-originated param changes passively wait for the
    /// next audio block — which may never come when playback is stopped.
    host_notify: Option<Box<dyn Fn() + Send + Sync>>,
}

/// Shared state accessible from all plugin threads.
///
/// Holds the effect's parameter descriptors (immutable after construction)
/// and the current parameter values as atomics. The audio processor reads
/// values here; the main thread writes them in response to host automation
/// or GUI interaction.
///
/// Wraps an `Arc<SonidoSharedData>` so it can be cloned into `'static + Send`
/// closures (e.g., the egui bridge render loop) without lifetime issues.
#[derive(Clone)]
pub struct SonidoShared {
    inner: Arc<SonidoSharedData>,
}

/// Flag indicating a gesture-begin is pending (GUI → audio).
pub const GESTURE_BEGIN: u8 = 1;
/// Flag indicating a gesture-end is pending (GUI → audio).
pub const GESTURE_END: u8 = 2;

impl SonidoShared {
    /// Create shared state for the given effect.
    ///
    /// Instantiates a temporary effect to extract parameter descriptors,
    /// then initializes all values to their defaults.
    ///
    /// `host_notify` is called from the GUI thread after every parameter
    /// gesture or value change, prompting the CLAP host to schedule a
    /// `process()` or `flush()` callback. Pass `None` for standalone/test use.
    pub fn new(effect_id: &'static str, host_notify: Option<Box<dyn Fn() + Send + Sync>>) -> Self {
        let registry = EffectRegistry::new();
        // Use a dummy sample rate — we only need descriptors, not DSP state.
        let effect = registry
            .create(effect_id, 48000.0)
            .expect("Unknown effect ID in SonidoShared::new");

        let count = effect.effect_param_count();
        let mut descriptors = Vec::with_capacity(count);
        let mut values = Vec::with_capacity(count);

        for i in 0..count {
            if let Some(desc) = effect.effect_param_info(i) {
                values.push(AtomicU32::new(desc.default.to_bits()));
                descriptors.push(desc);
            }
        }

        let gesture_flags = (0..descriptors.len()).map(|_| AtomicU8::new(0)).collect();

        Self {
            inner: Arc::new(SonidoSharedData {
                effect_id,
                descriptors,
                values,
                bypassed: AtomicBool::new(false),
                gesture_flags,
                latency_samples: AtomicU32::new(0),
                host_notify,
            }),
        }
    }

    /// Registry ID of the wrapped effect.
    pub fn effect_id(&self) -> &'static str {
        self.inner.effect_id
    }

    /// Number of parameters.
    pub fn param_count(&self) -> usize {
        self.inner.descriptors.len()
    }

    /// Get parameter descriptor by index.
    pub fn descriptor(&self, index: usize) -> Option<&ParamDescriptor> {
        self.inner.descriptors.get(index)
    }

    /// All parameter descriptors.
    pub fn descriptors(&self) -> &[ParamDescriptor] {
        &self.inner.descriptors
    }

    /// Find parameter index by stable `ParamId`.
    pub fn index_by_id(&self, id: u32) -> Option<usize> {
        self.inner.descriptors.iter().position(|d| d.id.0 == id)
    }

    /// Read the current value of a parameter (lock-free).
    pub fn get_value(&self, index: usize) -> Option<f32> {
        self.inner
            .values
            .get(index)
            .map(|v| f32::from_bits(v.load(Ordering::Acquire)))
    }

    /// Write a parameter value (lock-free). Clamps to descriptor bounds.
    pub fn set_value(&self, index: usize, value: f32) {
        if let Some((atomic, desc)) = self
            .inner
            .values
            .get(index)
            .zip(self.inner.descriptors.get(index))
        {
            let clamped = value.clamp(desc.min, desc.max);
            atomic.store(clamped.to_bits(), Ordering::Release);
        }
    }

    /// Whether the effect is bypassed (GUI toggle).
    pub fn is_bypassed(&self) -> bool {
        self.inner.bypassed.load(Ordering::Acquire)
    }

    /// Set the bypass state (GUI toggle).
    pub fn set_bypassed(&self, bypassed: bool) {
        self.inner.bypassed.store(bypassed, Ordering::Release);
    }

    /// Get the cached latency in samples.
    ///
    /// Written by the audio thread during `activate()`; read by the main
    /// thread to implement the CLAP latency extension.
    pub fn latency_samples(&self) -> u32 {
        self.inner.latency_samples.load(Ordering::Acquire)
    }

    /// Cache the effect's latency in samples.
    ///
    /// Called by the audio processor after creating the effect instance.
    pub fn set_latency_samples(&self, samples: u32) {
        self.inner.latency_samples.store(samples, Ordering::Release);
    }

    /// Notify the host that GUI-originated changes are pending.
    ///
    /// Calls `host.request_process()` so the host schedules a `process()` or
    /// `flush()` callback, ensuring gesture/value events reach the host even
    /// when playback is stopped. No-op if no callback is set (standalone mode).
    pub fn notify_host(&self) {
        if let Some(cb) = &self.inner.host_notify {
            cb();
        }
    }

    /// Signal the start of a parameter gesture (GUI drag start).
    ///
    /// Sets the `GESTURE_BEGIN` flag for the given parameter. The audio thread
    /// picks this up via [`SonidoShared::take_gesture_flags`] and emits a CLAP
    /// `ParamGestureBeginEvent` to the host.
    pub fn gesture_begin(&self, index: usize) {
        if let Some(flag) = self.inner.gesture_flags.get(index) {
            flag.fetch_or(GESTURE_BEGIN, Ordering::Release);
        }
    }

    /// Signal the end of a parameter gesture (GUI drag stop).
    ///
    /// Sets the `GESTURE_END` flag for the given parameter.
    pub fn gesture_end(&self, index: usize) {
        if let Some(flag) = self.inner.gesture_flags.get(index) {
            flag.fetch_or(GESTURE_END, Ordering::Release);
        }
    }

    /// Atomically read and clear gesture flags for a parameter.
    ///
    /// Returns the accumulated flags since the last call. The audio thread
    /// calls this once per process block per parameter.
    pub fn take_gesture_flags(&self, index: usize) -> u8 {
        self.inner
            .gesture_flags
            .get(index)
            .map_or(0, |flag| flag.swap(0, Ordering::AcqRel))
    }

    /// Sync all current parameter values into an effect instance.
    ///
    /// Called when the audio processor is activated or after loading state.
    pub fn apply_to_effect(&self, effect: &mut dyn sonido_registry::EffectWithParams) {
        for (i, atomic) in self.inner.values.iter().enumerate() {
            let val = f32::from_bits(atomic.load(Ordering::Acquire));
            effect.effect_set_param(i, val);
        }
    }
}

impl clack_plugin::prelude::PluginShared<'_> for SonidoShared {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_new_creates_all_params() {
        let shared = SonidoShared::new("distortion", None);
        assert_eq!(shared.effect_id(), "distortion");
        assert_eq!(shared.param_count(), 6);
        assert!(shared.param_count() > 0);
    }

    #[test]
    fn shared_defaults_match_descriptors() {
        let shared = SonidoShared::new("reverb", None);
        for (i, desc) in shared.descriptors().iter().enumerate() {
            let val = shared.get_value(i).unwrap();
            assert_eq!(
                val, desc.default,
                "param {i} ({}) default mismatch: got {val}, expected {}",
                desc.name, desc.default
            );
        }
    }

    #[test]
    fn shared_set_value_clamps() {
        let shared = SonidoShared::new("distortion", None);
        let desc = shared.descriptor(0).unwrap();

        // Set above max — should clamp.
        shared.set_value(0, desc.max + 100.0);
        assert_eq!(shared.get_value(0).unwrap(), desc.max);

        // Set below min — should clamp.
        shared.set_value(0, desc.min - 100.0);
        assert_eq!(shared.get_value(0).unwrap(), desc.min);
    }

    #[test]
    fn shared_index_by_id_finds_params() {
        let shared = SonidoShared::new("distortion", None);
        // Distortion base ID is 200.
        assert_eq!(shared.index_by_id(200), Some(0));
        assert_eq!(shared.index_by_id(201), Some(1));
        // Non-existent ID.
        assert_eq!(shared.index_by_id(999), None);
    }

    #[test]
    fn shared_out_of_range_safe() {
        let shared = SonidoShared::new("distortion", None);
        assert_eq!(shared.get_value(999), None);
        assert_eq!(shared.descriptor(999), None);
        // Should not panic.
        shared.set_value(999, 1.0);
    }

    #[test]
    fn shared_apply_to_effect() {
        let shared = SonidoShared::new("distortion", None);
        // Set a non-default value.
        shared.set_value(0, 20.0);

        let registry = EffectRegistry::new();
        let mut effect = registry.create("distortion", 48000.0).unwrap();
        shared.apply_to_effect(effect.as_mut());

        // Verify the effect got the value.
        assert_eq!(effect.effect_get_param(0), 20.0);
    }

    #[test]
    fn all_effects_create_shared() {
        let registry = EffectRegistry::new();
        for desc in registry.all_effects() {
            let shared = SonidoShared::new(desc.id, None);
            assert!(shared.param_count() > 0, "effect {} has no params", desc.id);
            // Verify all param IDs are unique within the effect.
            let ids: Vec<u32> = shared.descriptors().iter().map(|d| d.id.0).collect();
            for (i, id) in ids.iter().enumerate() {
                assert_eq!(
                    shared.index_by_id(*id),
                    Some(i),
                    "effect {} param ID {id} lookup mismatch",
                    desc.id
                );
            }
        }
    }

    #[test]
    fn gesture_flags_begin_end() {
        let shared = SonidoShared::new("distortion", None);

        // No flags initially.
        assert_eq!(shared.take_gesture_flags(0), 0);

        // Set begin.
        shared.gesture_begin(0);
        let flags = shared.take_gesture_flags(0);
        assert_ne!(flags & GESTURE_BEGIN, 0);
        assert_eq!(flags & GESTURE_END, 0);

        // Cleared after take.
        assert_eq!(shared.take_gesture_flags(0), 0);

        // Set end.
        shared.gesture_end(0);
        let flags = shared.take_gesture_flags(0);
        assert_eq!(flags & GESTURE_BEGIN, 0);
        assert_ne!(flags & GESTURE_END, 0);
    }

    #[test]
    fn gesture_flags_accumulate() {
        let shared = SonidoShared::new("distortion", None);

        // Both begin and end set before audio reads — both present.
        shared.gesture_begin(0);
        shared.gesture_end(0);
        let flags = shared.take_gesture_flags(0);
        assert_ne!(flags & GESTURE_BEGIN, 0);
        assert_ne!(flags & GESTURE_END, 0);

        // Cleared.
        assert_eq!(shared.take_gesture_flags(0), 0);
    }

    #[test]
    fn gesture_flags_out_of_range() {
        let shared = SonidoShared::new("distortion", None);
        // Should not panic.
        shared.gesture_begin(999);
        shared.gesture_end(999);
        assert_eq!(shared.take_gesture_flags(999), 0);
    }

    #[test]
    fn state_roundtrip_json() {
        let shared = SonidoShared::new("compressor", None);

        // Set some non-default values.
        shared.set_value(0, 10.0);
        shared.set_value(1, 42.0);

        // Serialize.
        let mut state = serde_json::Map::new();
        for (i, desc) in shared.descriptors().iter().enumerate() {
            if let Some(val) = shared.get_value(i) {
                state.insert(
                    desc.id.0.to_string(),
                    serde_json::Value::from(f64::from(val)),
                );
            }
        }
        let json = serde_json::to_vec(&serde_json::Value::Object(state)).unwrap();

        // Create fresh shared, load state.
        let shared2 = SonidoShared::new("compressor", None);
        let value: serde_json::Value = serde_json::from_slice(&json).unwrap();
        let obj = value.as_object().unwrap();
        for (key, val) in obj {
            let id: u32 = key.parse().unwrap();
            let v = val.as_f64().unwrap();
            if let Some(index) = shared2.index_by_id(id) {
                shared2.set_value(index, v as f32);
            }
        }

        // Verify values match.
        for i in 0..shared.param_count() {
            assert_eq!(
                shared.get_value(i).unwrap(),
                shared2.get_value(i).unwrap(),
                "param {i} value mismatch after roundtrip"
            );
        }
    }
}
