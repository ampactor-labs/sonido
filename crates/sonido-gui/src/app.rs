//! Main application state and UI layout.
//!
//! Audio-thread processing (the `AudioProcessor` and stream construction) lives
//! in the sibling `audio_processor` module to keep GUI and real-time concerns
//! cleanly separated.

use crate::atomic_param_bridge::AtomicParamBridge;
use crate::audio_bridge::{AudioBridge, MeteringData};
use crate::audio_processor::build_audio_streams;
use crate::chain_manager::ChainCommand;
use crate::chain_view::ChainView;
use crate::file_player::FilePlayer;
use crate::preset_manager::PresetManager;
use crate::theme::Theme;
use crate::widgets::{Knob, LevelMeter};
use egui::{
    Align, CentralPanel, Color32, Context, Frame, Layout, Margin, Rect, TopBottomPanel, UiBuilder,
    pos2, vec2,
};
use sonido_gui_core::effects_ui;
use sonido_gui_core::{ParamBridge, SlotIndex};
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
    chain_view: ChainView,
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

    /// When set, the app runs in single-effect mode (no chain view).
    single_effect: bool,

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
    /// simplified UI showing only that effect (no chain view, no presets).
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
            &[]
        };
        let bridge = Arc::new(AtomicParamBridge::new(&registry, chain, 48000.0));

        let audio_bridge = AudioBridge::new();
        let transport_tx = audio_bridge.transport_sender();

        let mut chain_view = ChainView::new(Arc::clone(&bridge));
        if single_effect {
            chain_view.set_selected(SlotIndex(0));
        }

        let mut app = Self {
            audio_bridge,
            _audio_streams: Vec::new(),
            #[cfg(target_arch = "wasm32")]
            audio_resumed: false,
            metering: MeteringData::default(),
            bridge,
            registry,
            theme: Theme::default(),
            chain_view,
            file_player: FilePlayer::new(transport_tx),
            preset_manager: PresetManager::new(),
            cached_panel: None,
            sample_rate: 48000.0,
            buffer_size: 512,
            cpu_usage: 0.0,
            audio_error: None,
            single_effect,
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

        // Start audio
        if let Err(e) = app.start_audio() {
            app.audio_error = Some(e);
        }

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

    /// Render the header/toolbar.
    fn render_header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading(
                egui::RichText::new("SONIDO")
                    .color(Color32::from_rgb(100, 180, 255))
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
                .selected_text(&display_name)
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

            // Save button (native only — no filesystem on wasm)
            #[cfg(not(target_arch = "wasm32"))]
            if ui.button("Save").clicked() {
                self.show_save_dialog = true;
                self.new_preset_name = self
                    .preset_manager
                    .current()
                    .map(|p| p.preset.name.clone())
                    .unwrap_or_default();
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Audio status indicator
                let status_color = if self.audio_bridge.is_running() {
                    Color32::from_rgb(80, 200, 80)
                } else {
                    Color32::from_rgb(200, 80, 80)
                };
                ui.label(egui::RichText::new("●").color(status_color).size(12.0));

                let err_count = self.audio_bridge.error_count().load(Ordering::Relaxed);
                if err_count > 0 {
                    ui.label(
                        egui::RichText::new(format!("audio errors: {err_count}"))
                            .color(Color32::from_rgb(220, 100, 100))
                            .small(),
                    );
                }

                let mut retry = false;
                if let Some(ref error) = self.audio_error {
                    ui.label(
                        egui::RichText::new(error)
                            .color(Color32::from_rgb(220, 100, 100))
                            .small(),
                    );
                    retry = ui.small_button("Retry").clicked();
                }
                if retry {
                    self.stop_audio();
                    match self.start_audio() {
                        Ok(()) => self.audio_error = None,
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
        let new_bridge =
            Arc::new(AtomicParamBridge::new(&self.registry, &effect_ids, self.sample_rate));
        crate::preset_manager::preset_to_params(&preset, &*new_bridge);

        // 3. Swap in the new bridge
        self.bridge = new_bridge;
        self.chain_view.set_bridge(Arc::clone(&self.bridge));

        // 4. Restart audio with the new chain
        if let Err(e) = self.start_audio() {
            self.audio_error = Some(e);
        }
    }

    /// Render the I/O section with meters and gain controls.
    fn render_io_section(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.set_min_width(80.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new("INPUT")
                        .small()
                        .color(Color32::from_rgb(150, 150, 160)),
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
                        Knob::new(&mut gain_val, input_gain.min(), input_gain.max(), "GAIN")
                            .default(-120.0)
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
        ui.group(|ui| {
            ui.set_min_width(80.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new("OUTPUT")
                        .small()
                        .color(Color32::from_rgb(150, 150, 160)),
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

        let panel_frame = Frame::new()
            .fill(Color32::from_rgb(40, 40, 48))
            .corner_radius(8.0)
            .inner_margin(Margin::same(16));

        panel_frame.show(ui, |ui| {
            ui.set_min_height(160.0);
            let max_h = ui.available_height().max(160.0);
            egui::ScrollArea::vertical()
                .max_height(max_h)
                .auto_shrink(true)
                .show(ui, |ui| {
                    // Panel title
                    ui.heading(
                        egui::RichText::new(panel_name).color(Color32::from_rgb(100, 180, 255)),
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
    }

    /// Render the status bar.
    fn render_status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let chain_bypassed = self.audio_bridge.chain_bypass().load(Ordering::Relaxed);
            let bypass_text = if chain_bypassed {
                egui::RichText::new("BYPASS")
                    .color(Color32::from_rgb(255, 80, 80))
                    .strong()
            } else {
                egui::RichText::new("BYPASS").color(Color32::from_rgb(100, 100, 110))
            };
            if ui.button(bypass_text).clicked() {
                self.audio_bridge
                    .chain_bypass()
                    .store(!chain_bypassed, Ordering::SeqCst);
            }
            ui.separator();

            ui.label(format!("{:.0} Hz", self.sample_rate));
            ui.separator();
            ui.label(format!("{} samples", self.buffer_size));
            ui.separator();
            let latency_ms = self.buffer_size as f32 / self.sample_rate * 1000.0;
            ui.label(format!("{:.1} ms", latency_ms));
            ui.separator();
            let cpu_text = format!("CPU: {:.1}%", self.cpu_usage);
            #[cfg(debug_assertions)]
            let cpu_text = format!("{cpu_text} (debug)");
            let cpu_color = if self.cpu_usage > 100.0 {
                Color32::from_rgb(255, 80, 80)
            } else if self.cpu_usage > 80.0 {
                Color32::from_rgb(255, 200, 80)
            } else {
                Color32::from_rgb(150, 150, 160)
            };
            ui.label(egui::RichText::new(&cpu_text).color(cpu_color));
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
                            log::error!("Failed to save preset: {}", e);
                        }
                        self.show_save_dialog = false;
                    }
                });
            });
    }
}

impl eframe::App for SonidoApp {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        // Update metering data
        if let Some(data) = self.audio_bridge.receive_metering() {
            self.cpu_usage = data.cpu_usage;
            self.file_player.set_position(data.playback_position_secs);
            self.metering = data;
        }

        // Handle pending add/remove from chain view
        if let Some(id) = self.chain_view.take_pending_add()
            && let Some(effect) = self.registry.create(id, self.sample_rate)
        {
            self.audio_bridge
                .send_command(ChainCommand::Add { id, effect });
        }
        if let Some(slot) = self.chain_view.take_pending_remove() {
            if self.chain_view.selected() == Some(slot) {
                self.chain_view.clear_selection();
                self.cached_panel = None;
            }
            self.audio_bridge
                .send_command(ChainCommand::Remove { slot });
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

        // Main content
        CentralPanel::default().show(ctx, |ui| {
            #[cfg(target_arch = "wasm32")]
            if !self.audio_resumed {
                log::debug!(
                    "wasm layout: available={:.0}x{:.0} ppp={:.2}",
                    ui.available_width(),
                    ui.available_height(),
                    ctx.pixels_per_point()
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

            // Center column (chain strip + effect panel)
            {
                let mut child = ui.new_child(
                    UiBuilder::new()
                        .id_salt("center_col")
                        .max_rect(center_rect)
                        .layout(Layout::top_down(Align::LEFT)),
                );

                if self.single_effect {
                    // Single-effect mode: show only the effect panel, no chain strip
                    self.render_effect_panel(&mut child, SlotIndex(0));
                } else {
                    // Full chain mode: chain strip + selected effect panel
                    child.group(|ui| {
                        ui.vertical_centered(|ui| {
                            ui.label(
                                egui::RichText::new("EFFECT CHAIN")
                                    .small()
                                    .color(Color32::from_rgb(150, 150, 160)),
                            );
                            ui.add_space(4.0);
                            self.chain_view.ui(ui, &*self.bridge, &self.registry);
                        });
                    });

                    child.add_space(16.0);

                    if let Some(slot) = self.chain_view.selected() {
                        self.render_effect_panel(&mut child, slot);
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
