//! CLAP plugin GUI: parameter bridge and egui window management.
//!
//! Connects the CLAP host's parent window handle to egui rendering via
//! sonido's own egui bridge (OpenGL). Each plugin instance gets an
//! independent `egui::Context` and GL context — no shared global state.
//!
//! Every effect draws the design system's [`Cabinet`]: the same layout lit
//! in its family's color, fed by the audio thread's meters and scope.
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
    SonidoTheme,
    cabinet::{Cabinet, CabinetIo, preferred_size},
    param_bridge::{ParamBridge, ParamIndex, SlotIndex},
};

pub use crate::impl_plugin_gui;

use crate::egui_bridge;
use crate::shared::SonidoShared;

// ── Window size constants ────────────────────────────────────────────────────

/// Minimum plugin window width in logical pixels.
pub const MIN_WIDTH: u32 = 560;

/// Minimum plugin window height in logical pixels.
pub const MIN_HEIGHT: u32 = 380;

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

/// The window size (logical pixels) a plugin opens at: the cabinet's
/// preferred size for these parameters.
pub fn preferred_window_size(descriptors: &[ParamDescriptor]) -> (u32, u32) {
    let (w, h) = preferred_size(descriptors);
    (w as u32, (h as u32).max(MIN_HEIGHT))
}

/// What an editor window draws: a parameter bridge, the audio thread's
/// telemetry, and anything plugin-specific the cabinet should show.
pub trait EditorModel: Send + Sync + 'static {
    /// Parameter access for the cabinet's controls.
    fn bridge(&self) -> &dyn ParamBridge;
    /// Meters, scope and engine facts.
    fn io(&self) -> CabinetIo<'_>;
    /// A fresh cabinet for this plugin (title, family).
    fn new_cabinet(&self) -> Cabinet;
    /// Called before each frame, to update what the cabinet shows.
    fn prepare(&self, _cabinet: &mut Cabinet) {}
}

/// A single-effect plugin's editor model.
pub struct EffectModel {
    bridge: PluginParamBridge,
    shared: SonidoShared,
}

impl EffectModel {
    /// The editor model for one effect instance.
    pub fn new(shared: SonidoShared) -> Self {
        Self {
            bridge: PluginParamBridge::new(shared.clone()),
            shared,
        }
    }
}

impl EditorModel for EffectModel {
    fn bridge(&self) -> &dyn ParamBridge {
        &self.bridge
    }

    fn io(&self) -> CabinetIo<'_> {
        CabinetIo {
            tap: Some(self.shared.tap()),
            sample_rate: self.shared.sample_rate(),
            latency: self.shared.latency_samples(),
        }
    }

    fn new_cabinet(&self) -> Cabinet {
        Cabinet::new(self.shared.effect_id())
    }
}

/// Per-window GUI state: the model and the cabinet drawing it.
pub(crate) struct EditorState {
    model: Arc<dyn EditorModel>,
    cabinet: Cabinet,
}

impl EditorState {
    pub(crate) fn new(model: Arc<dyn EditorModel>) -> Self {
        let cabinet = model.new_cabinet();
        Self { model, cabinet }
    }

    /// One-time setup: the design system's theme and fonts.
    pub(crate) fn build(ctx: &egui::Context, _state: &mut Self) {
        SonidoTheme::default().apply(ctx);
    }

    /// One frame: the whole cabinet, edge to edge.
    pub(crate) fn update(ctx: &egui::Context, state: &mut Self) {
        state.model.prepare(&mut state.cabinet);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                let model = state.model.as_ref();
                state
                    .cabinet
                    .show(ui, model.bridge(), SlotIndex(0), model.io());
            });
    }
}

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
    pub fn open(
        parent_rwh: RawWindowHandle,
        title: String,
        model: Arc<dyn EditorModel>,
        scale: f64,
        pending_resize: Arc<PendingResize>,
    ) -> Self {
        let (width, height) = pending_resize.get();
        let window = egui_bridge::open_parented(
            &ParentWindow(parent_rwh),
            title,
            width,
            height,
            scale,
            pending_resize,
            EditorState::new(model),
            EditorState::build,
            EditorState::update,
        );
        Self { _window: window }
    }
}

/// Everything a plugin's CLAP `gui` extension needs: the window, its size
/// channel and scale, and how to make the editor model. Shared by every
/// Sonido plugin type (see [`impl_plugin_gui!`](crate::impl_plugin_gui)).
pub struct EditorHost {
    title: String,
    make_model: Box<dyn Fn() -> Arc<dyn EditorModel> + Send>,
    scale: f64,
    editor: Option<SonidoEditor>,
    pending_resize: Arc<PendingResize>,
}

impl EditorHost {
    /// A host for a window titled `title`, opening at `size` (logical px).
    pub fn new(
        title: impl Into<String>,
        size: (u32, u32),
        make_model: impl Fn() -> Arc<dyn EditorModel> + Send + 'static,
    ) -> Self {
        Self {
            title: title.into(),
            make_model: Box::new(make_model),
            scale: 1.0,
            editor: None,
            pending_resize: Arc::new(PendingResize::new(size.0, size.1)),
        }
    }

    /// CLAP `gui.set_scale`.
    pub fn set_scale(&mut self, scale: f64) {
        self.scale = scale;
    }

    /// CLAP `gui.get_size`.
    pub fn size(&self) -> (u32, u32) {
        self.pending_resize.get()
    }

    /// CLAP `gui.adjust_size` / `set_size` clamping.
    pub fn clamp(width: u32, height: u32) -> (u32, u32) {
        (
            width.clamp(MIN_WIDTH, MAX_WIDTH),
            height.clamp(MIN_HEIGHT, MAX_HEIGHT),
        )
    }

    /// CLAP `gui.set_size`.
    pub fn set_size(&mut self, width: u32, height: u32) {
        let (w, h) = Self::clamp(width, height);
        self.pending_resize.set(w, h);
    }

    /// CLAP `gui.set_parent`: open the editor inside `parent`. Bitwig expects
    /// the child window to exist as soon as this returns, so it opens here
    /// rather than in `show()`.
    pub fn open(&mut self, parent: RawWindowHandle) {
        self.editor = Some(SonidoEditor::open(
            parent,
            self.title.clone(),
            (self.make_model)(),
            self.scale,
            Arc::clone(&self.pending_resize),
        ));
    }

    /// CLAP `gui.hide` / `gui.destroy`: close the window.
    pub fn close(&mut self) {
        self.editor = None;
    }
}

/// Implement CLAP's `gui` extension for a main-thread type by forwarding to
/// its [`EditorHost`] field.
#[macro_export]
#[doc(hidden)]
macro_rules! impl_plugin_gui {
    ($ty:ty, $field:ident) => {
        impl ::clack_extensions::gui::PluginGuiImpl for $ty {
            fn is_api_supported(
                &mut self,
                config: ::clack_extensions::gui::GuiConfiguration,
            ) -> bool {
                let api = ::clack_extensions::gui::GuiApiType::default_for_current_platform();
                !config.is_floating && api == Some(config.api_type)
            }

            fn get_preferred_api(
                &mut self,
            ) -> Option<::clack_extensions::gui::GuiConfiguration<'_>> {
                Some(::clack_extensions::gui::GuiConfiguration {
                    api_type: ::clack_extensions::gui::GuiApiType::default_for_current_platform()?,
                    is_floating: false,
                })
            }

            fn create(
                &mut self,
                _config: ::clack_extensions::gui::GuiConfiguration,
            ) -> Result<(), ::clack_plugin::prelude::PluginError> {
                Ok(()) // the window opens in set_parent()
            }

            fn destroy(&mut self) {
                self.$field.close();
            }

            fn set_scale(
                &mut self,
                scale: f64,
            ) -> Result<(), ::clack_plugin::prelude::PluginError> {
                self.$field.set_scale(scale);
                Ok(())
            }

            fn get_size(&mut self) -> Option<::clack_extensions::gui::GuiSize> {
                let (width, height) = self.$field.size();
                Some(::clack_extensions::gui::GuiSize { width, height })
            }

            fn can_resize(&mut self) -> bool {
                true
            }

            fn get_resize_hints(&mut self) -> Option<::clack_extensions::gui::GuiResizeHints> {
                Some(::clack_extensions::gui::GuiResizeHints {
                    can_resize_horizontally: true,
                    can_resize_vertically: true,
                    strategy: ::clack_extensions::gui::AspectRatioStrategy::Disregard,
                })
            }

            fn adjust_size(
                &mut self,
                size: ::clack_extensions::gui::GuiSize,
            ) -> Option<::clack_extensions::gui::GuiSize> {
                let (width, height) = $crate::gui::EditorHost::clamp(size.width, size.height);
                Some(::clack_extensions::gui::GuiSize { width, height })
            }

            fn set_size(
                &mut self,
                size: ::clack_extensions::gui::GuiSize,
            ) -> Result<(), ::clack_plugin::prelude::PluginError> {
                self.$field.set_size(size.width, size.height);
                Ok(())
            }

            fn set_parent(
                &mut self,
                window: ::clack_extensions::gui::Window,
            ) -> Result<(), ::clack_plugin::prelude::PluginError> {
                use ::raw_window_handle::HasRawWindowHandle;
                self.$field.open(window.raw_window_handle());
                Ok(())
            }

            fn show(&mut self) -> Result<(), ::clack_plugin::prelude::PluginError> {
                Ok(()) // already open since set_parent()
            }

            fn hide(&mut self) -> Result<(), ::clack_plugin::prelude::PluginError> {
                self.$field.close();
                Ok(())
            }

            fn set_transient(
                &mut self,
                _window: ::clack_extensions::gui::Window,
            ) -> Result<(), ::clack_plugin::prelude::PluginError> {
                Ok(()) // embedded only, no floating window support
            }
        }
    };
}
