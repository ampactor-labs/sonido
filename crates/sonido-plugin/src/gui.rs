//! CLAP plugin GUI: parameter bridge and egui window management.
//!
//! Connects the CLAP host's parent window handle to egui rendering via
//! sonido's own egui bridge (OpenGL). Each plugin instance gets an
//! independent `egui::Context` and GL context — no shared global state.
//!
//! The egui bridge (`crate::egui_bridge`) replaces the external `egui-baseview`
//! dependency with ~350 lines of focused glue code, giving sonido full control
//! over its plugin rendering pipeline.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use baseview::WindowHandle;
use raw_window_handle::{HasRawWindowHandle, RawWindowHandle};

use sonido_core::ParamDescriptor;
use sonido_gui_core::{
    effects_ui::{EffectPanel, create_panel},
    param_bridge::{ParamBridge, ParamIndex, SlotIndex},
};

use crate::egui_bridge;
use crate::shared::SonidoShared;

// ── Window size constants ────────────────────────────────────────────────────

/// Default plugin window width in logical pixels.
pub const PLUGIN_WIDTH: u32 = 480;

/// Default plugin window height in logical pixels.
pub const PLUGIN_HEIGHT: u32 = 380;

/// Minimum plugin window width in logical pixels.
pub const MIN_WIDTH: u32 = 320;

/// Minimum plugin window height in logical pixels.
pub const MIN_HEIGHT: u32 = 240;

/// Maximum plugin window width in logical pixels.
pub const MAX_WIDTH: u32 = 1920;

/// Maximum plugin window height in logical pixels.
pub const MAX_HEIGHT: u32 = 1080;

// ── Atomic resize channel ────────────────────────────────────────────────────

/// Lock-free resize channel between the CLAP host's main thread and baseview.
///
/// The host writes a new logical size via [`set`](Self::set) in response to
/// `set_size()` calls. The baseview `WindowHandler::on_frame` reads the
/// pending size and calls `window.resize()` if it differs from the last applied
/// dimensions.
///
/// Width and height are packed into a single `AtomicU64` (width in high 32 bits,
/// height in low 32 bits) for a single-word atomic update — no torn reads.
pub struct PendingResize {
    /// Packed `(width << 32) | height`.
    packed: AtomicU64,
}

impl PendingResize {
    /// Create a new resize channel with the given initial logical size.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            packed: AtomicU64::new(Self::pack(width, height)),
        }
    }

    /// Write a new pending logical size (host main thread → baseview handler).
    pub fn set(&self, width: u32, height: u32) {
        self.packed
            .store(Self::pack(width, height), Ordering::Release);
    }

    /// Read the current logical size.
    pub fn get(&self) -> (u32, u32) {
        Self::unpack(self.packed.load(Ordering::Acquire))
    }

    fn pack(width: u32, height: u32) -> u64 {
        (u64::from(width) << 32) | u64::from(height)
    }

    fn unpack(packed: u64) -> (u32, u32) {
        ((packed >> 32) as u32, packed as u32)
    }
}

// ── Parameter bridge ──────────────────────────────────────────────────────────

/// Implements [`ParamBridge`] backed by `SonidoShared` lock-free atomics.
///
/// Single-slot (slot 0 = the effect). Thread-safe reads/writes via `Acquire`/`Release`
/// atomics — safe to call from the egui render closure on any thread.
///
/// Gesture tracking (`begin_set`/`end_set`) sets atomic flags in shared state.
/// The audio thread reads and clears these flags, emitting CLAP
/// `ParamGestureBegin/EndEvent` to the host for proper undo grouping.
pub struct PluginParamBridge {
    shared: SonidoShared,
}

impl PluginParamBridge {
    /// Creates a new bridge backed by the given shared parameter store.
    pub fn new(shared: SonidoShared) -> Self {
        Self { shared }
    }
}

impl ParamBridge for PluginParamBridge {
    fn slot_count(&self) -> usize {
        1 // plugin exposes a single effect slot
    }

    fn effect_id(&self, _slot: SlotIndex) -> &str {
        self.shared.effect_id()
    }

    fn param_count(&self, _slot: SlotIndex) -> usize {
        self.shared.param_count()
    }

    fn param_descriptor(&self, _slot: SlotIndex, param: ParamIndex) -> Option<ParamDescriptor> {
        self.shared.descriptor(param.0).cloned()
    }

    fn get(&self, _slot: SlotIndex, param: ParamIndex) -> f32 {
        self.shared.get_value(param.0).unwrap_or(0.0)
    }

    fn set(&self, _slot: SlotIndex, param: ParamIndex, value: f32) {
        self.shared.set_value(param.0, value);
        self.shared.notify_host();
    }

    fn is_bypassed(&self, _slot: SlotIndex) -> bool {
        self.shared.is_bypassed()
    }

    fn set_bypassed(&self, _slot: SlotIndex, bypassed: bool) {
        self.shared.set_bypassed(bypassed);
    }

    fn begin_set(&self, _slot: SlotIndex, param: ParamIndex) {
        self.shared.gesture_begin(param.0);
        self.shared.notify_host();
    }

    fn end_set(&self, _slot: SlotIndex, param: ParamIndex) {
        self.shared.gesture_end(param.0);
        self.shared.notify_host();
    }
}

// ── Parent window wrapper ─────────────────────────────────────────────────────

/// Wraps a [`RawWindowHandle`] from the CLAP host for use with baseview's
/// `open_parented`, which requires [`HasRawWindowHandle`] (raw-window-handle 0.5).
///
/// # Safety
///
/// The raw window handle is valid for the entire embedded window lifetime.
/// The CLAP specification guarantees the host's parent window outlives the
/// plugin's child window (`destroy()` is called before host closes the parent).
struct ParentWindow(RawWindowHandle);

#[allow(unsafe_code)]
// SAFETY: HasRawWindowHandle is unsafe in rwh 0.5 (safe in 0.6).
// The impl is trivial — returns a Copy value received from the CLAP host.
// CLAP gui extension guarantees the parent window handle is valid for the
// full plugin GUI lifecycle (destroy() called before host closes parent).
unsafe impl HasRawWindowHandle for ParentWindow {
    fn raw_window_handle(&self) -> RawWindowHandle {
        self.0
    }
}

// ── Editor ────────────────────────────────────────────────────────────────────

/// Holds the baseview rendering window for one plugin instance.
///
/// Dropping this value closes the plugin GUI window (baseview `WindowHandle`
/// cleanup is handled on drop).
pub struct SonidoEditor {
    _window: WindowHandle,
}

impl SonidoEditor {
    /// Opens an egui child window inside the host's parent window.
    ///
    /// The `pending_resize` channel allows the CLAP host to resize the window
    /// asynchronously — the host writes a new size via `set_size()`, and the
    /// baseview handler applies it on the next `on_frame()`.
    ///
    /// Returns `None` if no UI panel is registered for the effect's ID
    /// (should not happen for the 19 built-in effects).
    pub fn open(
        parent_rwh: RawWindowHandle,
        shared: SonidoShared,
        scale: f64,
        pending_resize: Arc<PendingResize>,
    ) -> Option<Self> {
        let effect_id = shared.effect_id().to_owned();
        let panel: Box<dyn EffectPanel + Send + Sync> = create_panel(&effect_id)?;
        let bridge = Arc::new(PluginParamBridge::new(shared));

        let (width, height) = pending_resize.get();

        struct GuiState {
            bridge: Arc<PluginParamBridge>,
            panel: Box<dyn EffectPanel + Send + Sync>,
        }

        let state = GuiState { bridge, panel };

        let window = egui_bridge::open_parented(
            &ParentWindow(parent_rwh),
            effect_id,
            width,
            height,
            scale,
            pending_resize,
            state,
            // build: one-time setup (fonts, theme — none needed, gui-core theme applies)
            |_ctx, _state| {},
            // update: called each frame by the bridge's render loop
            |ctx, state| {
                // Poll for host-originated parameter changes at 30 Hz.
                // Full vsync repaint is wasteful; 33ms latency is imperceptible
                // for knob positions updating from automation.
                ctx.request_repaint_after(std::time::Duration::from_millis(33));
                egui::CentralPanel::default().show(ctx, |ui| {
                    // No A/B morph in the single-effect plugin view yet, so the
                    // knobs draw no morph ghost ticks.
                    state.panel.ui(
                        ui,
                        state.bridge.as_ref() as &dyn ParamBridge,
                        SlotIndex(0),
                        &|_| None,
                    );
                });
            },
        );

        Some(Self { _window: window })
    }
}
