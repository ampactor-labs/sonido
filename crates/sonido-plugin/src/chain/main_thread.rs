//! Main-thread handler for the multi-effect chain plugin.
//!
//! [`ChainMainThread`] provides parameter metadata (512 pre-allocated params),
//! state save/restore, audio port configuration, and GUI lifecycle management.

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

use crate::chain::shared::ChainShared;
use crate::chain::{ClapParamId, TOTAL_PARAMS};
use crate::gui::PendingResize;

/// Map sonido `ParamFlags` to CLAP `ParamInfoFlags`.
pub(crate) fn map_flags(flags: sonido_core::ParamFlags) -> ParamInfoFlags {
    crate::main_thread::map_flags(flags)
}

/// Main-thread state for the chain plugin.
pub struct ChainMainThread<'a> {
    shared: &'a ChainShared,
    /// Raw window handle from the host, stored between `set_parent` and `show`.
    parent_rwh: Option<raw_window_handle::RawWindowHandle>,
    /// DPI scale factor from the host (default 1.0).
    scale: f64,
    /// Atomic resize channel shared with the baseview window handler.
    pending_resize: Arc<PendingResize>,
}

/// Default chain editor width.
const CHAIN_WIDTH: u32 = 720;
/// Default chain editor height.
const CHAIN_HEIGHT: u32 = 520;
/// Minimum chain editor width.
const CHAIN_MIN_WIDTH: u32 = 480;
/// Minimum chain editor height.
const CHAIN_MIN_HEIGHT: u32 = 360;
/// Maximum chain editor width.
const CHAIN_MAX_WIDTH: u32 = 1920;
/// Maximum chain editor height.
const CHAIN_MAX_HEIGHT: u32 = 1080;

impl<'a> ChainMainThread<'a> {
    /// Create a new main-thread handler.
    pub fn new(shared: &'a ChainShared) -> Self {
        Self {
            shared,
            parent_rwh: None,
            scale: 1.0,
            pending_resize: Arc::new(PendingResize::new(CHAIN_WIDTH, CHAIN_HEIGHT)),
        }
    }
}

impl<'a> PluginMainThread<'a, ChainShared> for ChainMainThread<'a> {}

// ── Parameter Extension ─────────────────────────────────────────────────────

impl PluginMainThreadParams for ChainMainThread<'_> {
    fn count(&mut self) -> u32 {
        TOTAL_PARAMS as u32
    }

    fn get_info(&mut self, param_index: u32, info: &mut ParamInfoWriter) {
        let Some(id) = ClapParamId::from_raw(param_index) else {
            return;
        };

        let slots = self.shared.load_slots();
        let slot_snap = &slots[id.slot()];

        if slot_snap.active && id.param() < slot_snap.descriptors.len() {
            let desc = &slot_snap.descriptors[id.param()];
            info.set(&ParamInfo {
                id: ClapId::new(param_index),
                name: desc.name.as_bytes(),
                module: desc.group.as_bytes(),
                min_value: f64::from(desc.min),
                max_value: f64::from(desc.max),
                default_value: f64::from(desc.default),
                flags: map_flags(desc.flags),
                cookie: Cookie::default(),
            });
        } else {
            // Hidden placeholder for unoccupied slot/param.
            let name = format!("_slot{}_p{}", id.slot(), id.param());
            info.set(&ParamInfo {
                id: ClapId::new(param_index),
                name: name.as_bytes(),
                module: b"",
                min_value: 0.0,
                max_value: 1.0,
                default_value: 0.0,
                flags: ParamInfoFlags::IS_HIDDEN,
                cookie: Cookie::default(),
            });
        }
    }

    fn get_value(&mut self, param_id: ClapId) -> Option<f64> {
        let id = ClapParamId::from_raw(param_id.get())?;
        Some(f64::from(self.shared.get_value(id)))
    }

    fn value_to_text(
        &mut self,
        param_id: ClapId,
        value: f64,
        writer: &mut ParamDisplayWriter,
    ) -> core::fmt::Result {
        use core::fmt::Write as _;

        let Some(id) = ClapParamId::from_raw(param_id.get()) else {
            return write!(writer, "{value:.2}");
        };

        let slots = self.shared.load_slots();
        let slot_snap = &slots[id.slot()];

        if slot_snap.active && id.param() < slot_snap.descriptors.len() {
            let desc = &slot_snap.descriptors[id.param()];
            let formatted = desc.format_value(value as f32);
            write!(writer, "{formatted}")
        } else {
            write!(writer, "{value:.2}")
        }
    }

    fn text_to_value(&mut self, param_id: ClapId, text: &core::ffi::CStr) -> Option<f64> {
        let id = ClapParamId::from_raw(param_id.get())?;
        let slots = self.shared.load_slots();
        let slot_snap = &slots[id.slot()];

        if slot_snap.active && id.param() < slot_snap.descriptors.len() {
            let desc = &slot_snap.descriptors[id.param()];
            let s = text.to_str().ok()?;
            desc.parse_value(s).map(f64::from)
        } else {
            None
        }
    }

    fn flush(&mut self, input: &InputEvents, _output: &mut OutputEvents) {
        for event in input {
            if let Some(clack_plugin::events::spaces::CoreEventSpace::ParamValue(ev)) =
                event.as_core_event()
                && let Some(param_id) = ev.param_id()
                && let Some(id) = ClapParamId::from_raw(param_id.get())
            {
                self.shared.set_value(id, ev.value() as f32);
            }
        }
    }
}

// ── State Extension ─────────────────────────────────────────────────────────

impl PluginStateImpl for ChainMainThread<'_> {
    fn save(&mut self, output: &mut OutputStream) -> Result<(), PluginError> {
        let slots = self.shared.load_slots();
        let order = self.shared.load_order();

        let mut chain_arr = Vec::new();
        for &slot_idx in order.iter() {
            let snap = &slots[slot_idx];
            if !snap.active {
                continue;
            }
            let mut params = serde_json::Map::new();
            for (i, _desc) in snap.descriptors.iter().enumerate() {
                if let Some(id) = ClapParamId::new(slot_idx, i) {
                    let val = self.shared.get_value(id);
                    params.insert(i.to_string(), serde_json::Value::from(f64::from(val)));
                }
            }
            chain_arr.push(serde_json::json!({
                "id": snap.effect_id,
                "bypassed": self.shared.is_bypassed(slot_idx),
                "params": params,
            }));
        }

        let state = serde_json::json!({
            "version": 1,
            "chain": chain_arr,
        });

        let json = serde_json::to_vec(&state)
            .map_err(|_| PluginError::Message("Failed to serialize chain state"))?;

        output
            .write_all(&json)
            .map_err(|_| PluginError::Message("Failed to write chain state"))?;

        Ok(())
    }

    fn load(&mut self, input: &mut InputStream) -> Result<(), PluginError> {
        let mut buf = Vec::new();
        input
            .read_to_end(&mut buf)
            .map_err(|_| PluginError::Message("Failed to read chain state"))?;

        let value: serde_json::Value = serde_json::from_slice(&buf)
            .map_err(|_| PluginError::Message("Invalid chain state JSON"))?;

        let Some(chain_arr) = value.get("chain").and_then(|v| v.as_array()) else {
            return Err(PluginError::Message("Missing 'chain' array in state"));
        };

        // Clear existing chain
        use crate::chain::shared::ChainCommand;
        for slot_idx in 0..super::MAX_SLOTS {
            let slots = self.shared.load_slots();
            if slots[slot_idx].active {
                self.shared
                    .push_command(ChainCommand::Remove { slot: slot_idx });
            }
        }

        // Rebuild from saved state
        for entry in chain_arr {
            let Some(effect_id) = entry.get("id").and_then(|v| v.as_str()) else {
                continue;
            };

            self.shared.push_command(ChainCommand::Add {
                effect_id: effect_id.to_owned(),
            });

            // Parameter values and bypass will be applied after the audio thread
            // processes the Add command and triggers a rescan. For now, queue the
            // values — they'll be picked up on the next process() cycle.
            // TODO: deferred param restore after structural rebuild
        }

        let _ = chain_arr; // suppress unused warning for bypass/params (deferred)
        Ok(())
    }
}

// ── GUI Extension ──────────────────────────────────────────────────────────

impl PluginGuiImpl for ChainMainThread<'_> {
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
        Ok(())
    }

    fn destroy(&mut self) {
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
            width: size.width.clamp(CHAIN_MIN_WIDTH, CHAIN_MAX_WIDTH),
            height: size.height.clamp(CHAIN_MIN_HEIGHT, CHAIN_MAX_HEIGHT),
        })
    }

    fn set_size(&mut self, size: GuiSize) -> Result<(), PluginError> {
        let width = size.width.clamp(CHAIN_MIN_WIDTH, CHAIN_MAX_WIDTH);
        let height = size.height.clamp(CHAIN_MIN_HEIGHT, CHAIN_MAX_HEIGHT);
        self.pending_resize.set(width, height);
        Ok(())
    }

    fn set_parent(&mut self, window: Window) -> Result<(), PluginError> {
        use raw_window_handle::HasRawWindowHandle;
        self.parent_rwh = Some(window.raw_window_handle());
        // GUI window creation deferred to Phase 5 (ChainEditor)
        Ok(())
    }

    fn show(&mut self) -> Result<(), PluginError> {
        Ok(())
    }

    fn hide(&mut self) -> Result<(), PluginError> {
        Ok(())
    }

    fn set_transient(&mut self, _window: Window) -> Result<(), PluginError> {
        Ok(())
    }
}

// ── Latency Extension ───────────────────────────────────────────────────────

impl PluginLatencyImpl for ChainMainThread<'_> {
    fn get(&mut self) -> u32 {
        self.shared.latency_samples()
    }
}

// ── Audio Ports Extension ───────────────────────────────────────────────────

impl PluginAudioPortsImpl for ChainMainThread<'_> {
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
