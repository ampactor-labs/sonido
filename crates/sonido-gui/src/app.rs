//! Main application state and UI layout.
//!
//! Audio-thread processing (the `AudioProcessor` and stream construction) lives
//! in the sibling `audio_processor` module to keep GUI and real-time concerns
//! cleanly separated.

use crate::atomic_param_bridge::AtomicParamBridge;
use crate::audio_bridge::{AudioBridge, MeteringData};
use crate::audio_processor::build_audio_streams;
use crate::file_player::FilePlayer;
use crate::graph_view::GraphView;
use crate::morph_state::MorphState;
use crate::preset_manager::PresetManager;
use crate::theme::Theme;
use crate::widgets::{Knob, LevelMeter};
use egui::{
    Align, CentralPanel, Context, FontId, Frame, Layout, Margin, Rect, Stroke, TopBottomPanel,
    UiBuilder, pos2, vec2,
};
use sonido_gui_core::effects_ui;
use sonido_gui_core::theme::SonidoTheme;
use sonido_gui_core::widgets::glow;
use sonido_gui_core::widgets::morph_bar;
use sonido_gui_core::{LedDisplay, ParamBridge, SlotIndex};
use sonido_registry::EffectRegistry;
use std::sync::Arc;
use std::sync::atomic::Ordering;

#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

/// Main application state.
pub struct SonidoApp {
    // Audio
    audio_bridge: AudioBridge,
    /// Live cpal streams -- dropped to stop audio.
    _audio_streams: Vec<cpal::Stream>,
    /// Whether we've re-called play() after a user gesture (wasm autoplay policy).
    #[cfg(target_arch = "wasm32")]
    audio_resumed: bool,
    metering: MeteringData,

    /// Registry-driven parameter bridge (GUI ↔ audio thread).
    bridge: Arc<AtomicParamBridge>,

    /// Effect registry for creating new effects.
    registry: Arc<EffectRegistry>,

    // UI
    theme: Theme,
    graph_view: GraphView,
    morph_state: MorphState,
    file_player: FilePlayer,
    preset_manager: PresetManager,

    /// Cached effect panel: (slot, effect_id, panel).
    /// Avoids reconstructing the panel widget every frame.
    cached_panel: Option<(
        sonido_gui_core::SlotIndex,
        String,
        Box<dyn effects_ui::EffectPanel + Send + Sync>,
    )>,

    // Status
    sample_rate: f32,
    buffer_size: usize,
    cpu_usage: f32,
    audio_error: Option<String>,

    /// CPU usage history for real-time graph (last 60 frames)
    cpu_history: Vec<f32>,

    /// When set, the app runs in single-effect mode (no graph view).
    single_effect: bool,

    /// Last compilation error message, if any.
    compile_error: Option<String>,
    /// Frames remaining for compile success flash.
    compile_success_frames: u32,

    // Preset dialog (native only — no filesystem on wasm)
    #[cfg(not(target_arch = "wasm32"))]
    show_save_dialog: bool,
    #[cfg(not(target_arch = "wasm32"))]
    new_preset_name: String,
    #[cfg(not(target_arch = "wasm32"))]
    new_preset_description: String,
}

impl SonidoApp {
    /// Create a new application instance.
    ///
    /// If `effect` is `Some("name")`, launches in single-effect mode with a
    /// simplified UI showing only that effect (no graph view, no presets).
    pub fn new(cc: &eframe::CreationContext<'_>, effect: Option<&str>) -> Self {
        let registry = Arc::new(EffectRegistry::new());

        let single_effect = effect.is_some();
        let chain: &[&'static str] = if let Some(name) = effect {
            // Look up the static ID from the registry to avoid Box::leak
            let desc = registry.get(name).unwrap_or_else(|| {
                panic!(
                    "Unknown effect: {name}. Available: {:?}",
                    registry
                        .all_effects()
                        .iter()
                        .map(|e| e.id)
                        .collect::<Vec<_>>()
                )
            });
            // Leak a single-element slice — lives for the process lifetime
            Box::leak(vec![desc.id].into_boxed_slice())
        } else {
            // Load ALL effects from the registry by default
            let all_ids: Vec<&'static str> = registry.all_effects().iter().map(|e| e.id).collect();
            Box::leak(all_ids.into_boxed_slice())
        };
        let bridge = Arc::new(AtomicParamBridge::new(&registry, chain, 48000.0));

        // Bypass all by default in multi-effect mode
        if !single_effect {
            for i in 0..chain.len() {
                bridge.set_default_bypass(SlotIndex(i), true);
            }
        }

        let audio_bridge = AudioBridge::new();
        let transport_tx = audio_bridge.transport_sender();

        let mut app = Self {
            audio_bridge,
            _audio_streams: Vec::new(),
            #[cfg(target_arch = "wasm32")]
            audio_resumed: false,
            metering: MeteringData::default(),
            bridge,
            registry,
            theme: Theme::default(),
            graph_view: GraphView::new(),
            morph_state: MorphState::new(),
            file_player: FilePlayer::new(transport_tx),
            preset_manager: PresetManager::new(),
            cached_panel: None,
            sample_rate: 48000.0,
            buffer_size: 1024, // Default buffer size
            cpu_usage: 0.0,
            audio_error: None,
            cpu_history: Vec::with_capacity(60),
            single_effect,
            compile_error: None,
            compile_success_frames: 0,
            #[cfg(not(target_arch = "wasm32"))]
            show_save_dialog: false,
            #[cfg(not(target_arch = "wasm32"))]
            new_preset_name: String::new(),
            #[cfg(not(target_arch = "wasm32"))]
            new_preset_description: "User".to_string(),
        };

        // Apply theme
        app.theme.apply(&cc.egui_ctx);

        // Load initial preset — select first preset which applies it to bridge
        if !app.preset_manager.presets().is_empty() {
            app.preset_manager.select(0, &*app.bridge);
        }

        tracing::info!(sample_rate = app.sample_rate, "app initialized");

        // Auto-compile the default graph (Input → Output) so audio
        // passthrough works immediately without requiring the user to
        // click Compile.
        if !single_effect {
            match app
                .graph_view
                .compile_to_engine(app.sample_rate, app.buffer_size)
            {
                Ok(cmd) => app.audio_bridge.send_command(cmd),
                Err(e) => tracing::warn!("initial graph compile failed: {e}"),
            }
        }

        // Start audio
        if let Err(e) = app.start_audio() {
            app.audio_error = Some(e);
        }
        app.file_player.resync_transport();

        app
    }

    /// Build cpal streams and start audio processing.
    ///
    /// Streams are stored in `_audio_streams` and stay alive until dropped.
    /// Works identically on native and wasm -- cpal handles threading internally.
    fn start_audio(&mut self) -> Result<(), String> {
        // Query actual device sample rate for GUI display and effect init
        {
            use cpal::traits::{DeviceTrait, HostTrait};
            if let Some(device) = cpal::default_host().default_output_device()
                && let Ok(config) = device.default_output_config()
            {
                self.sample_rate = config.sample_rate() as f32;
            }
        }

        let bridge = Arc::clone(&self.bridge);
        let registry = Arc::clone(&self.registry);
        let input_gain = self.audio_bridge.input_gain();
        let master_volume = self.audio_bridge.master_volume();
        let running = self.audio_bridge.running();
        let metering_tx = self.audio_bridge.metering_sender();
        let command_rx = self.audio_bridge.command_receiver();
        let transport_rx = self.audio_bridge.transport_receiver();
        let chain_bypass = self.audio_bridge.chain_bypass();

        running.store(true, Ordering::SeqCst);

        let error_count = self.audio_bridge.error_count();

        let streams = build_audio_streams(
            bridge,
            &registry,
            input_gain,
            master_volume,
            running,
            metering_tx,
            command_rx,
            transport_rx,
            chain_bypass,
            error_count,
            self.sample_rate,
            self.buffer_size,
        )?;

        self._audio_streams = streams;
        Ok(())
    }

    /// Stop audio by dropping stream handles.
    fn stop_audio(&mut self) {
        self.audio_bridge.running().store(false, Ordering::SeqCst);
        self._audio_streams.clear();
    }

    /// Get the current buffer size in samples.
    ///
    /// The buffer size determines the latency and CPU usage characteristics:
    /// - Smaller buffers (256-512): lower latency, higher CPU usage
    /// - Balanced (1024-2048): moderate latency and CPU (recommended)
    /// - Larger buffers (4096): higher latency, more stable under overload
    ///
    /// Default: 2048 samples
    pub fn get_buffer_size(&self) -> usize {
        self.buffer_size
    }

    /// Set the buffer size with validation.
    ///
    /// Validates that the buffer size is within acceptable hardware limits
    /// (typically 64-4096 samples). If the size is invalid, it is clamped
    /// to the nearest valid value. The audio stream is restarted to apply
    /// the new buffer size.
    pub fn set_buffer_size(&mut self, size: usize) {
        // Validate buffer size - most audio hardware supports 64-4096
        let valid_sizes = [64, 128, 256, 512, 1024, 2048, 4096];
        let clamped_size = if valid_sizes.contains(&size) {
            size
        } else {
            // Find closest valid size by absolute difference
            valid_sizes
                .iter()
                .min_by_key(|&s| (*s).abs_diff(size))
                .copied()
                .unwrap_or(2048)
        };

        if clamped_size != size {
            tracing::warn!(
                requested = size,
                using = clamped_size,
                "buffer size not in valid set, clamping"
            );
        }

        self.buffer_size = clamped_size;
        self.stop_audio();
        if let Err(e) = self.start_audio() {
            tracing::error!(
                buffer_size = clamped_size,
                error = %e,
                "failed to restart audio"
            );
        }
        self.file_player.resync_transport();
    }

    /// Get the buffer size in milliseconds.
    pub fn get_buffer_duration_ms(&self) -> f32 {
        (self.buffer_size as f32 / self.sample_rate) * 1000.0
    }

    /// Get available buffer size presets with descriptions and duration.
    ///
    /// Returns a vector of (size, description, latency_ms) tuples.
    /// The presets are designed to cover common use cases from low latency
    /// to maximum stability. The latency values are calculated dynamically
    /// based on the current sample rate.
    pub fn get_buffer_presets(&self) -> Vec<(usize, String, f32)> {
        vec![
            (
                256,
                format!(
                    "Low Latency (256 samples, {:.1}ms)",
                    256.0 / self.sample_rate * 1000.0
                ),
                256.0 / self.sample_rate * 1000.0,
            ),
            (
                512,
                format!(
                    "Very Low (512 samples, {:.1}ms)",
                    512.0 / self.sample_rate * 1000.0
                ),
                512.0 / self.sample_rate * 1000.0,
            ),
            (
                1024,
                format!(
                    "Balanced (1024 samples, {:.1}ms)",
                    1024.0 / self.sample_rate * 1000.0
                ),
                1024.0 / self.sample_rate * 1000.0,
            ),
            (
                2048,
                format!(
                    "Stable (2048 samples, {:.1}ms)",
                    2048.0 / self.sample_rate * 1000.0
                ),
                2048.0 / self.sample_rate * 1000.0,
            ),
            (
                4096,
                format!(
                    "Maximum (4096 samples, {:.1}ms)",
                    4096.0 / self.sample_rate * 1000.0
                ),
                4096.0 / self.sample_rate * 1000.0,
            ),
        ]
    }

    /// Render the header/toolbar.
    fn render_header(&mut self, ui: &mut egui::Ui) {
        let theme = SonidoTheme::get(ui.ctx());

        ui.horizontal(|ui| {
            ui.heading(
                egui::RichText::new("SONIDO")
                    .font(FontId::monospace(18.0))
                    .color(theme.colors.amber)
                    .strong(),
            );

            ui.add_space(20.0);

            // Preset selector
            let current_name = self
                .preset_manager
                .current()
                .map(|p| p.preset.name.as_str())
                .unwrap_or("Init");
            let display_name = if self.preset_manager.is_modified() {
                format!("{}*", current_name)
            } else {
                current_name.to_string()
            };

            // Collect preset names to avoid borrow issues
            let preset_names: Vec<(usize, String)> = self
                .preset_manager
                .presets()
                .iter()
                .enumerate()
                .map(|(i, p)| (i, p.preset.name.clone()))
                .collect();
            let current_idx = self.preset_manager.current_preset();

            let mut selected_preset = None;
            egui::ComboBox::from_id_salt("preset_selector")
                .selected_text(
                    egui::RichText::new(&display_name)
                        .font(FontId::monospace(12.0))
                        .color(theme.colors.text_primary),
                )
                .width(150.0)
                .show_ui(ui, |ui| {
                    for (i, name) in &preset_names {
                        if ui.selectable_label(*i == current_idx, name).clicked() {
                            selected_preset = Some(*i);
                        }
                    }
                });

            // Apply preset selection after borrow ends
            if let Some(idx) = selected_preset {
                self.apply_preset(idx);
            }

            ui.add_space(8.0);
            self.file_player.render_source_toggle(ui);

            // Compile button — only in multi-effect mode (graph editor)
            if !self.single_effect {
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);

                let compile_btn = ui.button(
                    egui::RichText::new("Compile")
                        .font(FontId::monospace(12.0))
                        .color(theme.colors.green)
                        .strong(),
                );
                if compile_btn.clicked() {
                    match self
                        .graph_view
                        .compile_to_engine(self.sample_rate, self.buffer_size)
                    {
                        Ok(cmd) => {
                            self.audio_bridge.send_command(cmd);
                            self.compile_error = None;
                            self.compile_success_frames = 90; // ~1.5s at 60fps
                        }
                        Err(e) => {
                            self.compile_error = Some(e.to_string());
                            self.compile_success_frames = 0;
                        }
                    }
                }

                if let Some(ref err) = self.compile_error {
                    ui.label(
                        egui::RichText::new(err)
                            .font(FontId::monospace(10.0))
                            .color(theme.colors.red)
                            .small(),
                    );
                } else if self.compile_success_frames > 0 {
                    self.compile_success_frames -= 1;
                    ui.label(
                        egui::RichText::new("OK")
                            .font(FontId::monospace(10.0))
                            .color(theme.colors.green)
                            .strong(),
                    );
                }
            }

            // Save button (native only — no filesystem on wasm)
            #[cfg(not(target_arch = "wasm32"))]
            if ui
                .button(
                    egui::RichText::new("Save")
                        .font(FontId::monospace(12.0))
                        .color(theme.colors.text_primary),
                )
                .clicked()
            {
                self.show_save_dialog = true;
                self.new_preset_name = self
                    .preset_manager
                    .current()
                    .map(|p| p.preset.name.clone())
                    .unwrap_or_default();
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Audio status indicator — glowing circle
                let status_color = if self.audio_bridge.is_running() {
                    theme.colors.green
                } else {
                    theme.colors.red
                };
                let (indicator_rect, _) =
                    ui.allocate_exact_size(vec2(14.0, 14.0), egui::Sense::hover());
                glow::glow_circle(
                    ui.painter(),
                    indicator_rect.center(),
                    4.0,
                    status_color,
                    &theme,
                );

                let err_count = self.audio_bridge.error_count().load(Ordering::Relaxed);
                if err_count > 0 {
                    ui.label(
                        egui::RichText::new(format!("audio errors: {err_count}"))
                            .font(FontId::monospace(10.0))
                            .color(theme.colors.red),
                    );
                }

                let mut retry = false;
                if let Some(ref error) = self.audio_error {
                    ui.label(
                        egui::RichText::new(error)
                            .font(FontId::monospace(10.0))
                            .color(theme.colors.red),
                    );
                    retry = ui.small_button("Retry").clicked();
                }
                if retry {
                    self.stop_audio();
                    match self.start_audio() {
                        Ok(()) => {
                            self.audio_error = None;
                            self.file_player.resync_transport();
                        }
                        Err(e) => self.audio_error = Some(e),
                    }
                }
            });
        });
    }

    /// Reconfigures the app to use a new preset.
    ///
    /// This is a "hard" reset: it stops audio, rebuilds the parameter bridge
    /// and effect chain from the preset, and restarts audio. This ensures the
    /// chain exactly matches the preset, adding and removing effects as needed.
    fn apply_preset(&mut self, preset_idx: usize) {
        if preset_idx >= self.preset_manager.presets().len() {
            return;
        }

        // Set the preset as current in the manager.
        self.preset_manager.select(preset_idx, &*self.bridge);

        let preset = self.preset_manager.current().unwrap().preset.clone();
        let effect_ids: Vec<&'static str> = preset
            .effects
            .iter()
            .filter_map(|config| self.registry.get(&config.effect_type).map(|desc| desc.id))
            .collect();

        // 1. Stop audio
        self.stop_audio();

        // 2. Create and configure a new bridge for the preset's chain
        let new_bridge = Arc::new(AtomicParamBridge::new(
            &self.registry,
            &effect_ids,
            self.sample_rate,
        ));
        crate::preset_manager::preset_to_params(&preset, &*new_bridge);

        // 3. Swap in the new bridge
        self.bridge = new_bridge;

        // 4. Restart audio with the new chain
        if let Err(e) = self.start_audio() {
            self.audio_error = Some(e);
        }
        self.file_player.resync_transport();
    }

    /// Render the I/O section with meters and gain controls.
    fn render_io_section(&mut self, ui: &mut egui::Ui) {
        let theme = SonidoTheme::get(ui.ctx());

        ui.group(|ui| {
            ui.set_min_width(80.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new("INPUT")
                        .font(FontId::monospace(12.0))
                        .color(theme.colors.cyan),
                );

                ui.add_space(4.0);

                // Input meter
                ui.add(
                    LevelMeter::new(self.metering.input_peak, self.metering.input_rms)
                        .size(24.0, 100.0),
                );

                ui.add_space(8.0);

                // Input gain knob
                let input_gain = self.audio_bridge.input_gain();
                let mut gain_val = input_gain.get();
                if ui
                    .add(
                        Knob::new(&mut gain_val, -20.0, 20.0, "GAIN")
                            .default(0.0)
                            .format_db()
                            .diameter(50.0),
                    )
                    .changed()
                {
                    input_gain.set(gain_val);
                    self.preset_manager.mark_modified();
                }
            });
        });
    }

    /// Render the output section.
    fn render_output_section(&mut self, ui: &mut egui::Ui) {
        let theme = SonidoTheme::get(ui.ctx());

        ui.group(|ui| {
            ui.set_min_width(80.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new("OUTPUT")
                        .font(FontId::monospace(12.0))
                        .color(theme.colors.cyan),
                );

                ui.add_space(4.0);

                // Output meter
                ui.add(
                    LevelMeter::new(self.metering.output_peak, self.metering.output_rms)
                        .size(24.0, 100.0),
                );

                ui.add_space(8.0);

                // Master volume knob
                let master_vol_param = self.audio_bridge.master_volume();
                let mut master_val = master_vol_param.get();
                if ui
                    .add(
                        Knob::new(&mut master_val, -40.0, 6.0, "MASTER")
                            .default(0.0)
                            .format_db()
                            .diameter(50.0),
                    )
                    .changed()
                {
                    master_vol_param.set(master_val);
                    self.preset_manager.mark_modified();
                }
            });
        });
    }

    /// Render the effect panel for the selected slot.
    ///
    /// The panel widget is cached in `self.cached_panel` and only reconstructed
    /// when the selected slot or effect type changes.
    fn render_effect_panel(&mut self, ui: &mut egui::Ui, slot: sonido_gui_core::SlotIndex) {
        let effect_id = self.bridge.effect_id(slot);
        let panel_name = self
            .registry
            .descriptor(effect_id)
            .map(|d| d.name)
            .unwrap_or("Unknown");

        // Populate cache if the slot or effect type changed
        let cache_hit = self
            .cached_panel
            .as_ref()
            .is_some_and(|(s, id, _)| *s == slot && id == effect_id);
        if !cache_hit {
            self.cached_panel =
                effects_ui::create_panel(effect_id).map(|p| (slot, effect_id.to_owned(), p));
        }

        let theme = SonidoTheme::get(ui.ctx());

        let panel_frame = Frame::new()
            .fill(theme.colors.void)
            .stroke(Stroke::new(1.0, theme.colors.amber))
            .corner_radius(theme.sizing.panel_border_radius)
            .inner_margin(Margin::same(theme.sizing.panel_padding as i8));

        let panel_response = panel_frame.show(ui, |ui| {
            ui.set_min_height(160.0);
            let max_h = ui.available_height().max(160.0);
            egui::ScrollArea::vertical()
                .max_height(max_h)
                .auto_shrink(true)
                .show(ui, |ui| {
                    // Panel title — monospace amber
                    ui.label(
                        egui::RichText::new(panel_name)
                            .font(FontId::monospace(12.0))
                            .color(theme.colors.amber),
                    );
                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(12.0);

                    // Effect-specific controls from cache
                    if let Some((_, _, ref mut panel)) = self.cached_panel {
                        let bridge: &dyn ParamBridge = &*self.bridge;
                        panel.ui(ui, bridge, slot);
                    }
                });
        });

        // Scanline overlay on the effect panel
        let panel_rect = panel_response.response.rect;
        glow::scanlines(ui.painter(), panel_rect, &theme);
    }

    /// Render the morph crossfader bar.
    ///
    /// Only shown when not in single-effect mode.
    fn render_morph_bar(&mut self, ui: &mut egui::Ui) {
        let has_a = self.morph_state.a.is_some();
        let has_b = self.morph_state.b.is_some();

        let resp = morph_bar(ui, &mut self.morph_state.t, has_a, has_b);

        if resp.capture_a {
            self.morph_state.capture_a(&*self.bridge);
        }
        if resp.capture_b {
            self.morph_state.capture_b(&*self.bridge);
        }
        if resp.recall_a {
            self.morph_state.recall_a(&*self.bridge);
        }
        if resp.recall_b {
            self.morph_state.recall_b(&*self.bridge);
        }
        if resp.t_changed {
            self.morph_state.active = true;
            self.morph_state.apply(&*self.bridge);
        }
    }

    /// Render the status bar.
    fn render_status_bar(&mut self, ui: &mut egui::Ui) {
        let theme = SonidoTheme::get(ui.ctx());

        ui.horizontal(|ui| {
            // BYPASS button with glow indicator
            let chain_bypassed = self.audio_bridge.chain_bypass().load(Ordering::Relaxed);
            let bypass_color = if chain_bypassed {
                theme.colors.red
            } else {
                theme.colors.dim
            };

            let bypass_btn = ui.button(
                egui::RichText::new("BYPASS")
                    .font(FontId::monospace(11.0))
                    .color(bypass_color)
                    .strong(),
            );

            // Draw glow circle next to the bypass button
            let circle_center = pos2(
                bypass_btn.rect.right() + 8.0,
                bypass_btn.rect.center().y,
            );
            glow::glow_circle(ui.painter(), circle_center, 3.0, bypass_color, &theme);
            // Reserve space for the glow indicator
            ui.add_space(10.0);

            if bypass_btn.clicked() {
                self.audio_bridge
                    .chain_bypass()
                    .store(!chain_bypassed, Ordering::SeqCst);
            }
            ui.separator();

            // Sample rate — LED display
            ui.add(LedDisplay::new(format!("{:.0}Hz", self.sample_rate)).color(theme.colors.amber));
            ui.separator();

            // Buffer size selector
            let presets = self.get_buffer_presets();
            let preset_names: Vec<String> = presets
                .iter()
                .map(|(_, desc, _)| desc.to_string())
                .collect();
            let current_idx = presets
                .iter()
                .position(|&(size, _, _)| size == self.buffer_size)
                .unwrap_or(2); // Default to "Stable"

            let mut selected_preset = None;
            egui::ComboBox::from_id_salt("buffer_size_selector")
                .selected_text(
                    egui::RichText::new(
                        preset_names
                            .get(current_idx)
                            .cloned()
                            .unwrap_or_else(|| "Unknown".to_string()),
                    )
                    .font(FontId::monospace(10.0))
                    .color(theme.colors.text_secondary),
                )
                .width(200.0)
                .show_ui(ui, |ui| {
                    for (idx, name) in preset_names.iter().enumerate() {
                        if ui.selectable_label(idx == current_idx, name).clicked() {
                            selected_preset = Some(idx);
                        }
                    }
                });

            if let Some((size, _, _)) = selected_preset.and_then(|i| presets.get(i)) {
                self.set_buffer_size(*size);
            }

            ui.separator();

            // Latency — LED display
            let latency_ms = self.buffer_size as f32 / self.sample_rate * 1000.0;
            ui.add(
                LedDisplay::new(format!("{:.1}ms", latency_ms)).color(theme.colors.amber),
            );
            ui.separator();

            // CPU meter — color-coded by load
            let cpu_text = format!("CPU: {:.1}%", self.cpu_usage);
            #[cfg(debug_assertions)]
            let cpu_text = format!("{cpu_text} (debug)");
            let cpu_color = if self.cpu_usage > 100.0 {
                theme.colors.red
            } else if self.cpu_usage > 80.0 {
                theme.colors.yellow
            } else {
                theme.colors.green
            };
            ui.label(
                egui::RichText::new(&cpu_text)
                    .font(FontId::monospace(11.0))
                    .color(cpu_color),
            );

            // CPU usage sparkline graph (glow oscilloscope style)
            if !self.cpu_history.is_empty() {
                draw_sparkline(ui, &self.cpu_history, cpu_color, 100.0, 24.0);
            }
        });
    }

    /// Render save preset dialog (native only — no filesystem on wasm).
    #[cfg(not(target_arch = "wasm32"))]
    fn render_save_dialog(&mut self, ctx: &Context) {
        if !self.show_save_dialog {
            return;
        }

        egui::Window::new("Save Preset")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Name:");
                    ui.text_edit_singleline(&mut self.new_preset_name);
                });

                ui.horizontal(|ui| {
                    ui.label("Description:");
                    ui.text_edit_singleline(&mut self.new_preset_description);
                });

                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        self.show_save_dialog = false;
                    }

                    if ui.button("Save").clicked() && !self.new_preset_name.is_empty() {
                        let description = if self.new_preset_description.is_empty() {
                            None
                        } else {
                            Some(self.new_preset_description.as_str())
                        };
                        if let Err(e) = self.preset_manager.save_as(
                            &self.new_preset_name,
                            description,
                            &*self.bridge,
                        ) {
                            tracing::error!(error = %e, "failed to save preset");
                        }
                        self.show_save_dialog = false;
                    }
                });
            });
    }
}

/// Draw a sparkline graph with phosphor glow from a history of values.
fn draw_sparkline(ui: &mut egui::Ui, history: &[f32], color: egui::Color32, width: f32, height: f32) {
    if history.is_empty() {
        return;
    }

    let theme = SonidoTheme::get(ui.ctx());

    let (graph_rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let painter = ui.painter();

    // Find min/max for scaling
    let min_val = history.iter().copied().fold(f32::INFINITY, f32::min);
    let max_val = history.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let range = (max_val - min_val).max(1.0); // Avoid division by zero

    // Draw background area (void)
    painter.rect_filled(graph_rect, 2.0, theme.colors.void);

    // Draw polyline
    let mut points = Vec::new();
    let step = width / (history.len() - 1).max(1) as f32;
    for (i, &value) in history.iter().enumerate() {
        let x = graph_rect.left() + i as f32 * step;
        // Invert Y: higher values at top
        let normalized = (value - min_val) / range;
        let y = graph_rect.bottom() - normalized * height;
        points.push(pos2(x, y));
    }

    // Glow line segments for CRT oscilloscope look
    if points.len() >= 2 {
        for window in points.windows(2) {
            glow::glow_line(painter, window[0], window[1], color, 1.5, &theme);
        }
    }

    // Glow dots at data points
    for point in &points {
        glow::glow_circle(painter, *point, 1.0, color, &theme);
    }
}

impl eframe::App for SonidoApp {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        // Update metering data
        if let Some(data) = self.audio_bridge.receive_metering() {
            self.cpu_usage = data.cpu_usage;
            self.file_player.set_position(data.playback_position_secs);
            self.metering = data;

            // Collect CPU usage history for real-time graph
            self.cpu_history.push(data.cpu_usage);
            if self.cpu_history.len() > 60 {
                self.cpu_history.remove(0);
            }
        }

        // Resume audio on first user gesture (wasm autoplay policy).
        // Browsers suspend AudioContext until a trusted user interaction.
        // Re-calling play() from within the user-activation window resumes it.
        #[cfg(target_arch = "wasm32")]
        if !self.audio_resumed && ctx.input(|i| i.pointer.any_pressed() || !i.events.is_empty()) {
            use cpal::traits::StreamTrait;
            for stream in &self._audio_streams {
                let _ = stream.play();
            }
            self.audio_resumed = true;
        }

        // Request continuous repaint for metering
        #[cfg(target_arch = "wasm32")]
        ctx.request_repaint_after(std::time::Duration::from_millis(33)); // 30fps
        #[cfg(not(target_arch = "wasm32"))]
        ctx.request_repaint_after(Duration::from_millis(16)); // ~60fps cap

        // Global keyboard shortcuts (only when no text widget is focused)
        let no_widget_focused = ctx.memory(|m| m.focused().is_none());
        if no_widget_focused
            && ctx.input(|i| i.key_pressed(egui::Key::Space))
            && self.file_player.use_file_input()
            && self.file_player.has_file()
        {
            self.file_player.toggle_play_pause();
        }

        // Header
        TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(4.0);
            self.render_header(ui);
            ui.add_space(4.0);
        });

        // Status bar
        TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.add_space(2.0);
            self.render_status_bar(ui);
            ui.add_space(2.0);
        });

        // File player bar (above status bar when file input active)
        if self.file_player.use_file_input() {
            TopBottomPanel::bottom("file_player").show(ctx, |ui| {
                ui.add_space(2.0);
                self.file_player.ui(ui);
                ui.add_space(2.0);
            });
        }

        // Morph bar (above file player, only in multi-effect mode)
        if !self.single_effect {
            TopBottomPanel::bottom("morph_bar").show(ctx, |ui| {
                ui.add_space(2.0);
                self.render_morph_bar(ui);
                ui.add_space(2.0);
            });
        }

        // Main content
        CentralPanel::default().show(ctx, |ui| {
            #[cfg(target_arch = "wasm32")]
            if !self.audio_resumed {
                tracing::debug!(
                    width = ui.available_width() as u32,
                    height = ui.available_height() as u32,
                    ppp = ctx.pixels_per_point(),
                    "wasm layout"
                );
            }

            ui.add_space(8.0);

            // Main layout: INPUT (100px) | 16px gap | CENTER (flex) | 16px gap | OUTPUT (100px)
            // Manual rect splitting avoids the vertical_centered-inside-horizontal width bug.
            let avail = ui.available_rect_before_wrap();
            let io_width = 100.0;
            let gap = 16.0;
            let center_width = (avail.width() - 2.0 * io_width - 2.0 * gap).max(200.0);

            let input_rect = Rect::from_min_size(avail.min, vec2(io_width, avail.height()));
            let center_rect = Rect::from_min_size(
                pos2(avail.min.x + io_width + gap, avail.min.y),
                vec2(center_width, avail.height()),
            );
            let output_rect = Rect::from_min_size(
                pos2(
                    avail.min.x + io_width + gap + center_width + gap,
                    avail.min.y,
                ),
                vec2(io_width, avail.height()),
            );

            // Input column
            {
                let mut child = ui.new_child(
                    UiBuilder::new()
                        .id_salt("input_col")
                        .max_rect(input_rect)
                        .layout(Layout::top_down(Align::Center)),
                );
                self.render_io_section(&mut child);
            }

            // Center column (graph editor + effect panel)
            {
                let mut child = ui.new_child(
                    UiBuilder::new()
                        .id_salt("center_col")
                        .max_rect(center_rect)
                        .layout(Layout::top_down(Align::LEFT)),
                );

                if self.single_effect {
                    // Single-effect mode: show only the effect panel, no graph
                    self.render_effect_panel(&mut child, SlotIndex(0));
                } else {
                    // Graph editor fills the upper portion (constrained)
                    let avail_h = child.available_height();
                    let graph_h = (avail_h * 0.6).max(200.0);
                    let theme = SonidoTheme::get(child.ctx());
                    let selected_slot = child
                        .group(|ui| {
                            ui.set_max_height(graph_h);
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    egui::RichText::new("GRAPH EDITOR")
                                        .font(FontId::monospace(12.0))
                                        .color(theme.colors.cyan),
                                );
                                ui.add_space(4.0);
                                self.graph_view.show(ui)
                            })
                            .inner
                        })
                        .inner;

                    child.add_space(16.0);

                    // Effect panel for the selected node
                    if let Some(slot_idx) = selected_slot {
                        let slot = SlotIndex(slot_idx);
                        if slot.0 < self.bridge.slot_count() {
                            self.render_effect_panel(&mut child, slot);
                        }
                    } else {
                        // Hint when no node is selected
                        let theme = SonidoTheme::get(child.ctx());
                        child.vertical_centered(|ui| {
                            ui.add_space(8.0);
                            ui.label(
                                egui::RichText::new(
                                    "Right-click to add nodes · Click a node to edit params · Compile to apply",
                                )
                                .font(FontId::monospace(10.0))
                                .color(theme.colors.text_secondary)
                                .italics(),
                            );
                        });
                    }
                }
            }

            // Output column
            {
                let mut child = ui.new_child(
                    UiBuilder::new()
                        .id_salt("output_col")
                        .max_rect(output_rect)
                        .layout(Layout::top_down(Align::Center)),
                );
                self.render_output_section(&mut child);
            }

            // Advance parent cursor past all three columns
            ui.advance_cursor_after_rect(Rect::from_min_max(
                avail.min,
                pos2(
                    avail.min.x + io_width + gap + center_width + gap + io_width,
                    avail.max.y,
                ),
            ));
        });

        // Dialogs (save dialog is native-only)
        #[cfg(not(target_arch = "wasm32"))]
        self.render_save_dialog(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.stop_audio();
    }
}
