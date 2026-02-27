//! Main-thread plugin implementation for sonido CLAP plugins.
//!
//! Handles parameter metadata queries, state save/restore, and audio port
//! configuration. All methods run on the host's main thread — never on the
//! audio thread.

use std::io::{Read, Write};
use std::sync::Arc;

use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPortsImpl,
};
use clack_extensions::gui::{
    AspectRatioStrategy, GuiApiType, GuiConfiguration, GuiResizeHints, GuiSize, PluginGuiImpl,
    Window,
};
use clack_extensions::latency::PluginLatencyImpl;
use clack_extensions::params::{
    ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, PluginMainThreadParams,
};
use clack_extensions::state::PluginStateImpl;
use clack_plugin::prelude::*;
use clack_plugin::stream::{InputStream, OutputStream};
use clack_plugin::utils::Cookie;
use raw_window_handle::HasRawWindowHandle;

use crate::gui::{
    MAX_HEIGHT, MAX_WIDTH, MIN_HEIGHT, MIN_WIDTH, PLUGIN_HEIGHT, PLUGIN_WIDTH, PendingResize,
    SonidoEditor,
};
use crate::shared::SonidoShared;

/// Main-thread state for a sonido CLAP plugin.
///
/// Provides parameter metadata to the host (count, info, display formatting),
/// handles state save/restore, and declares audio port configuration.
pub struct SonidoMainThread<'a> {
    shared: &'a SonidoShared,
    /// Raw window handle from the host, stored between `set_parent` and `show`.
    parent_rwh: Option<raw_window_handle::RawWindowHandle>,
    /// DPI scale factor from the host (default 1.0).
    scale: f64,
    /// Active editor window (dropped to close).
    editor: Option<SonidoEditor>,
    /// Atomic resize channel shared with the baseview window handler.
    ///
    /// The host writes sizes here; the handler applies them in `on_frame()`.
    pending_resize: Arc<PendingResize>,
}

impl<'a> SonidoMainThread<'a> {
    /// Create a new main-thread handler referencing the shared state.
    pub fn new(shared: &'a SonidoShared) -> Self {
        Self {
            shared,
            parent_rwh: None,
            scale: 1.0,
            editor: None,
            pending_resize: Arc::new(PendingResize::new(PLUGIN_WIDTH, PLUGIN_HEIGHT)),
        }
    }
}

impl<'a> PluginMainThread<'a, SonidoShared> for SonidoMainThread<'a> {}

// ── Parameter Extension ─────────────────────────────────────────────────────

/// Map sonido `ParamFlags` to CLAP `ParamInfoFlags`.
pub(crate) fn map_flags(flags: sonido_core::ParamFlags) -> ParamInfoFlags {
    let mut clap_flags = ParamInfoFlags::empty();

    if flags.contains(sonido_core::ParamFlags::AUTOMATABLE) {
        clap_flags |= ParamInfoFlags::IS_AUTOMATABLE;
    }
    if flags.contains(sonido_core::ParamFlags::STEPPED) {
        clap_flags |= ParamInfoFlags::IS_STEPPED;
    }
    if flags.contains(sonido_core::ParamFlags::HIDDEN) {
        clap_flags |= ParamInfoFlags::IS_HIDDEN;
    }
    if flags.contains(sonido_core::ParamFlags::READ_ONLY) {
        clap_flags |= ParamInfoFlags::IS_READONLY;
    }
    if flags.contains(sonido_core::ParamFlags::MODULATABLE) {
        clap_flags |= ParamInfoFlags::IS_MODULATABLE;
    }

    clap_flags
}

impl PluginMainThreadParams for SonidoMainThread<'_> {
    fn count(&mut self) -> u32 {
        self.shared.param_count() as u32
    }

    fn get_info(&mut self, param_index: u32, info: &mut ParamInfoWriter) {
        let Some(desc) = self.shared.descriptor(param_index as usize) else {
            return;
        };

        info.set(&ParamInfo {
            id: ClapId::new(desc.id.0),
            name: desc.name.as_bytes(),
            module: desc.group.as_bytes(),
            min_value: f64::from(desc.min),
            max_value: f64::from(desc.max),
            default_value: f64::from(desc.default),
            flags: map_flags(desc.flags),
            cookie: Cookie::default(),
        });
    }

    fn get_value(&mut self, param_id: ClapId) -> Option<f64> {
        let id = param_id.get();
        let index = self.shared.index_by_id(id)?;
        self.shared.get_value(index).map(f64::from)
    }

    fn value_to_text(
        &mut self,
        param_id: ClapId,
        value: f64,
        writer: &mut ParamDisplayWriter,
    ) -> core::fmt::Result {
        use core::fmt::Write;

        let id = param_id.get();
        let Some(index) = self.shared.index_by_id(id) else {
            return write!(writer, "{value:.2}");
        };
        let Some(desc) = self.shared.descriptor(index) else {
            return write!(writer, "{value:.2}");
        };

        let formatted = desc.format_value(value as f32);
        write!(writer, "{formatted}")
    }

    fn text_to_value(&mut self, param_id: ClapId, text: &core::ffi::CStr) -> Option<f64> {
        let id = param_id.get();
        let index = self.shared.index_by_id(id)?;
        let desc = self.shared.descriptor(index)?;
        let s = text.to_str().ok()?;
        desc.parse_value(s).map(f64::from)
    }

    fn flush(&mut self, input: &InputEvents, _output: &mut OutputEvents) {
        for event in input {
            if let Some(clack_plugin::events::spaces::CoreEventSpace::ParamValue(ev)) =
                event.as_core_event()
                && let Some(param_id) = ev.param_id()
            {
                let id = param_id.get();
                if let Some(index) = self.shared.index_by_id(id) {
                    self.shared.set_value(index, ev.value() as f32);
                }
            }
        }
    }
}

// ── State Extension ─────────────────────────────────────────────────────────

/// State format: JSON object mapping stable ParamId to f64 value.
///
/// ```json
/// {"200": 12.0, "201": 0.5, "202": 1.0}
/// ```
///
/// Using stable IDs (not indices) ensures state survives parameter reordering
/// across plugin versions.
impl PluginStateImpl for SonidoMainThread<'_> {
    fn save(&mut self, output: &mut OutputStream) -> Result<(), PluginError> {
        let mut state = serde_json::Map::new();

        for (i, desc) in self.shared.descriptors().iter().enumerate() {
            if let Some(val) = self.shared.get_value(i) {
                state.insert(
                    desc.id.0.to_string(),
                    serde_json::Value::from(f64::from(val)),
                );
            }
        }

        let json = serde_json::to_vec(&serde_json::Value::Object(state))
            .map_err(|_| PluginError::Message("Failed to serialize state"))?;

        output
            .write_all(&json)
            .map_err(|_| PluginError::Message("Failed to write state"))?;

        Ok(())
    }

    fn load(&mut self, input: &mut InputStream) -> Result<(), PluginError> {
        let mut buf = Vec::new();
        input
            .read_to_end(&mut buf)
            .map_err(|_| PluginError::Message("Failed to read state"))?;

        let value: serde_json::Value =
            serde_json::from_slice(&buf).map_err(|_| PluginError::Message("Invalid state JSON"))?;

        let Some(obj) = value.as_object() else {
            return Err(PluginError::Message("State is not a JSON object"));
        };

        for (key, val) in obj {
            let Ok(id) = key.parse::<u32>() else {
                continue;
            };
            let Some(v) = val.as_f64() else { continue };
            if let Some(index) = self.shared.index_by_id(id) {
                self.shared.set_value(index, v as f32);
            }
        }

        Ok(())
    }
}

// ── GUI Extension ──────────────────────────────────────────────────────────

impl PluginGuiImpl for SonidoMainThread<'_> {
    fn is_api_supported(&mut self, config: GuiConfiguration) -> bool {
        let platform_api = GuiApiType::default_for_current_platform();
        !config.is_floating && platform_api == Some(config.api_type)
    }

    fn get_preferred_api(&mut self) -> Option<GuiConfiguration<'_>> {
        let api = GuiApiType::default_for_current_platform();
        Some(GuiConfiguration {
            api_type: api?,
            is_floating: false,
        })
    }

    fn create(&mut self, _config: GuiConfiguration) -> Result<(), PluginError> {
        Ok(()) // resources allocated lazily in set_parent()
    }

    fn destroy(&mut self) {
        self.editor = None;
        self.parent_rwh = None;
    }

    fn set_scale(&mut self, scale: f64) -> Result<(), PluginError> {
        self.scale = scale;
        Ok(())
    }

    fn get_size(&mut self) -> Option<GuiSize> {
        let (width, height) = self.pending_resize.get();
        Some(GuiSize { width, height })
    }

    fn can_resize(&mut self) -> bool {
        true
    }

    fn get_resize_hints(&mut self) -> Option<GuiResizeHints> {
        Some(GuiResizeHints {
            can_resize_horizontally: true,
            can_resize_vertically: true,
            strategy: AspectRatioStrategy::Disregard,
        })
    }

    fn adjust_size(&mut self, size: GuiSize) -> Option<GuiSize> {
        Some(GuiSize {
            width: size.width.clamp(MIN_WIDTH, MAX_WIDTH),
            height: size.height.clamp(MIN_HEIGHT, MAX_HEIGHT),
        })
    }

    fn set_size(&mut self, size: GuiSize) -> Result<(), PluginError> {
        let width = size.width.clamp(MIN_WIDTH, MAX_WIDTH);
        let height = size.height.clamp(MIN_HEIGHT, MAX_HEIGHT);
        self.pending_resize.set(width, height);
        Ok(())
    }

    fn set_parent(&mut self, window: Window) -> Result<(), PluginError> {
        let rwh = window.raw_window_handle();
        self.parent_rwh = Some(rwh);

        // Bitwig expects the child window to exist after set_parent.
        // Create the editor immediately rather than deferring to show().
        self.editor = SonidoEditor::open(
            rwh,
            self.shared.clone(),
            self.scale,
            Arc::clone(&self.pending_resize),
        );

        if self.editor.is_none() {
            return Err(PluginError::Message("Failed to open editor window"));
        }
        Ok(())
    }

    fn show(&mut self) -> Result<(), PluginError> {
        // Window already created in set_parent; baseview shows it immediately.
        Ok(())
    }

    fn hide(&mut self) -> Result<(), PluginError> {
        self.editor = None;
        Ok(())
    }

    fn set_transient(&mut self, _window: Window) -> Result<(), PluginError> {
        Ok(()) // embedded only, no floating window support
    }
}

// ── Latency Extension ───────────────────────────────────────────────────────

impl PluginLatencyImpl for SonidoMainThread<'_> {
    fn get(&mut self) -> u32 {
        self.shared.latency_samples()
    }
}

// ── Audio Ports Extension ───────────────────────────────────────────────────

impl PluginAudioPortsImpl for SonidoMainThread<'_> {
    fn count(&mut self, _is_input: bool) -> u32 {
        1
    }

    fn get(&mut self, index: u32, _is_input: bool, writer: &mut AudioPortInfoWriter) {
        if index == 0 {
            writer.set(&AudioPortInfo {
                id: ClapId::new(0),
                name: b"Main",
                channel_count: 2,
                flags: AudioPortFlags::IS_MAIN,
                port_type: Some(AudioPortType::STEREO),
                in_place_pair: None,
            });
        }
    }
}
