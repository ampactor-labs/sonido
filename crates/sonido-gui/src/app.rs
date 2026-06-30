//! Main application state and UI layout.
//!
//! Audio-thread processing (the `AudioProcessor` and stream construction) lives
//! in the sibling `audio_processor` module to keep GUI and real-time concerns
//! cleanly separated.

use crate::atomic_param_bridge::AtomicParamBridge;
use crate::audio_bridge::{AudioBridge, MeteringData};
use crate::audio_processor::build_audio_streams;
use crate::file_player::FilePlayer;
use crate::graph_view::{GraphView, SonidoNode};
use crate::morph_state::MorphState;
use crate::theme::Theme;
use crate::widgets::{Knob, LevelMeter};
use egui::{
    Align, CentralPanel, Context, FontId, Frame, Layout, Margin, Rect, Stroke, TopBottomPanel,
    UiBuilder, pos2, vec2,
};
use sonido_core::{GlobalParam, MacroMap, MacroMapping, MacroTarget, MorphCurve};
use sonido_gui_core::effects_ui;
use sonido_gui_core::theme::SonidoTheme;
use sonido_gui_core::widgets::glow;
use sonido_gui_core::widgets::{MacroAction, morph_bar, take_macro_action};
use sonido_gui_core::{ParamBridge, ParamIndex, SlotIndex};

use crate::info_view::{self, InfoPayload};
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

    /// Six performance macros (K1–K6) → effect params + globals. Authored via
    /// the param-knob right-click menu and driven from the macro row; the GUI
    /// owns the map and applies it through the bridge (the audio thread reads
    /// the bridge as usual, so a macro move is just a batched param write).
    macro_map: MacroMap<6>,
    /// Per-macro display names (empty ⇒ rendered as "macro N").
    macro_names: [String; 6],
    /// Index of the macro whose mapping-editor popup is open, if any.
    macro_editor: Option<usize>,

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

    /// Last export/flash result shown in the header: `Ok` = success (green),
    /// `Err` = failure (red). Cleared/replaced on the next export.
    #[cfg(not(target_arch = "wasm32"))]
    export_msg: Option<Result<String, String>>,

    /// Full-editor undo/redo history (snapshots taken before each structural edit).
    undo_stack: Vec<crate::session::Session>,
    /// Redo snapshots (pushed when undoing, cleared on a fresh edit).
    redo_stack: Vec<crate::session::Session>,
    /// Snapshot taken at the start of any frame with pointer activity, promoted
    /// to the undo stack if that frame produced a structural edit.
    undo_pending: Option<crate::session::Session>,

    /// Viewport size class, recomputed each frame from the window width. Drives
    /// touch-target sizing and (later) responsive layout. Resolves to Desktop on
    /// the native 1000×700 window, so desktop behavior is unchanged.
    breakpoint: Breakpoint,
    /// Whether the touch effect-palette ("+" FAB) sheet is open.
    palette_open: bool,
    /// Live search text for the effect-palette sheet.
    palette_filter: String,
    /// Which collapsible drawer is open on the Phone breakpoint. On wider tiers
    /// the performance band is always docked and this is ignored.
    drawer: Drawer,

    /// Selected effect slot as of the previous frame, captured from the graph
    /// view's `show()` return. Drives the contextual Info View, which is drawn
    /// (as a bottom panel) before the central graph each frame, so it reads
    /// last frame's selection — a one-frame lag that is never visible.
    info_slot: Option<usize>,

    /// Full-width-on-demand "focus mode": when true (Desktop/Tablet only),
    /// the docked chrome — I/O strips, morph band, Info View — collapses so the
    /// canvas spans the full width. Toggled by the header button or `F`. The
    /// floating add affordances (FAB / palette) stay live, so authoring still
    /// works. Ignored on Phone, where the canvas is already the hero.
    chrome_collapsed: bool,
}

/// A summonable drawer on the Phone layout, where the performance band can't be
/// permanently docked. `Rig` slides up the K1–K6 macros + A/B morph.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Drawer {
    /// Nothing summoned — the canvas is the hero.
    None,
    /// The performance rig: K1–K6 macros and the A/B morph band.
    Rig,
}

/// Viewport size class. Touch affordances are *additive* at every tier — a
/// Desktop user sees today's behavior; narrower tiers add finger-friendly
/// targets and (later) reflow chrome into drawers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Breakpoint {
    /// ≥900px — the full desktop layout (today's behavior, unchanged).
    Desktop,
    /// 600–900px — wrapped chrome plus enlarged touch targets.
    Tablet,
    /// <600px — full-width canvas, drawers, FAB; the only sub-800px path, which
    /// only the web demo reaches (native enforces an 800×550 minimum).
    Phone,
}

impl Breakpoint {
    /// Classify from available width in points.
    pub fn from_width(w: f32) -> Self {
        if w >= 900.0 {
            Breakpoint::Desktop
        } else if w >= 600.0 {
            Breakpoint::Tablet
        } else {
            Breakpoint::Phone
        }
    }

    /// True for Tablet/Phone — where touch targets enlarge and chrome collapses.
    pub fn is_compact(self) -> bool {
        !matches!(self, Breakpoint::Desktop)
    }
}

impl SonidoApp {
    /// Create a new application instance.
    ///
    /// If `effect` is `Some("name")`, launches in single-effect mode with a
    /// simplified UI showing only that effect (no graph view, no presets).
    ///
    /// `requested_sample_rate` and `requested_buffer_size` are initial hints;
    /// the actual device rate is detected in `start_audio()` and takes priority.
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        effect: Option<&str>,
        requested_sample_rate: Option<f32>,
        requested_buffer_size: Option<usize>,
    ) -> Self {
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
        let initial_rate = requested_sample_rate.unwrap_or(48000.0);
        let initial_buffer = requested_buffer_size.unwrap_or(2048);

        let bridge = Arc::new(AtomicParamBridge::new(&registry, chain, initial_rate));

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
            macro_map: MacroMap::new(),
            macro_names: std::array::from_fn(|_| String::new()),
            macro_editor: None,
            cached_panel: None,
            sample_rate: initial_rate,
            buffer_size: initial_buffer,
            cpu_usage: 0.0,
            audio_error: None,
            cpu_history: Vec::with_capacity(60),
            single_effect,
            compile_error: None,
            compile_success_frames: 0,
            #[cfg(not(target_arch = "wasm32"))]
            export_msg: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            undo_pending: None,
            breakpoint: Breakpoint::Desktop,
            palette_open: false,
            palette_filter: String::new(),
            drawer: Drawer::None,
            info_slot: None,
            chrome_collapsed: false,
        };

        // Apply theme
        app.theme.apply(&cc.egui_ctx);

        // Start audio first — detects actual device sample rate
        if let Err(e) = app.start_audio() {
            app.audio_error = Some(e);
        }

        tracing::info!(
            sample_rate = app.sample_rate,
            buffer_size = app.buffer_size,
            "app initialized (device rate detected)"
        );

        // Auto-compile AFTER start_audio so we use the real device rate
        if !single_effect {
            app.compile_and_apply();
        }

        app.file_player.resync_transport();

        app
    }

    /// Populate the editor with a representative demo chain and compile it.
    ///
    /// Used by `--screenshot` (and handy as a quick demo); not part of the
    /// default startup, which opens an empty canvas.
    pub fn populate_demo(&mut self) {
        self.graph_view.populate_demo();
        self.compile_and_apply();
    }

    /// Compile the current graph and send it to the audio thread.
    ///
    /// On success, clears any previous compile error and arms the success flash.
    /// On failure, stores the error string for display in the header.
    fn compile_and_apply(&mut self) {
        match self
            .graph_view
            .compile_to_engine(self.sample_rate, self.buffer_size, &self.registry)
        {
            Ok(cmd) => {
                self.audio_bridge.send_command(cmd);
                self.compile_error = None;
                self.compile_success_frames = 90;
            }
            Err(e) => {
                self.compile_error = Some(e.to_string());
                self.compile_success_frames = 0;
            }
        }
    }

    /// React to a structural graph edit: record the undo point and recompile the
    /// audio graph. Called both for edits made *inside* `GraphView::show()`
    /// (right-click, wiring) and for edits made *after* it (the "+" FAB add, the
    /// node action bar, the Delete key) — the latter would otherwise update only
    /// the visuals, because `show()` clears `topology_changed` at the next frame's
    /// start before the in-`show` check could see them. No-op when unchanged.
    fn apply_topology_change(&mut self) {
        if !self.graph_view.topology_changed {
            return;
        }
        if let Some(prev) = self.undo_pending.take() {
            self.undo_stack.push(prev);
            const UNDO_DEPTH: usize = 64;
            if self.undo_stack.len() > UNDO_DEPTH {
                self.undo_stack.remove(0);
            }
            self.redo_stack.clear();
        }
        self.compile_and_apply();
        self.graph_view.topology_changed = false;
    }

    /// Build cpal streams and start audio processing.
    ///
    /// Streams are stored in `_audio_streams` and stay alive until dropped.
    /// Updates `self.sample_rate` and `self.buffer_size` to the actual values
    /// negotiated with the audio device.
    fn start_audio(&mut self) -> Result<(), String> {
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

        let config = build_audio_streams(
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

        // Update to actual device-negotiated values
        self.sample_rate = config.sample_rate;
        self.buffer_size = config.buffer_size;
        self._audio_streams = config.streams;
        Ok(())
    }

    /// Stop audio by dropping stream handles.
    fn stop_audio(&mut self) {
        self.audio_bridge.running().store(false, Ordering::SeqCst);
        self._audio_streams.clear();
    }

    /// Toggle the transport from the single Play/Pause control.
    ///
    /// On the web a click is a user-activation gesture, so we also nudge the
    /// (possibly still-suspended) AudioContext to resume here — guaranteeing one
    /// tap on PLAY both starts the source and unblocks audio output. On native
    /// the cfg block compiles out and this is just a play/pause toggle.
    fn toggle_transport(&mut self) {
        #[cfg(target_arch = "wasm32")]
        {
            use cpal::traits::StreamTrait;
            for stream in &self._audio_streams {
                let _ = stream.play();
            }
            self.audio_resumed = true;
        }
        self.file_player.toggle_play_pause();
    }

    /// Floating touch-authoring overlays over the canvas `area`: the "+" add-node
    /// FAB, a contextual action bar for the selected node, and the effect-palette
    /// sheet. egui-snarl's add/remove menus are right-click-only (eframe-web never
    /// synthesizes a secondary click), so these give touch — and discoverability —
    /// a first-class path. All additive: desktop keeps right-click and keyboard.
    fn render_graph_overlays(&mut self, ctx: &Context, area: Rect) {
        if self.single_effect {
            return;
        }
        let theme = SonidoTheme::get(ctx);
        let fab = if self.breakpoint.is_compact() {
            52.0
        } else {
            38.0
        };
        let margin = 14.0;

        // "+" add-node FAB, Phone only (bottom-right, thumb-reachable). On wider
        // viewports the labeled "+ EFFECT" header button replaces it.
        if self.breakpoint == Breakpoint::Phone {
            egui::Area::new(egui::Id::new("add_node_fab"))
                .fixed_pos(pos2(area.max.x - fab - margin, area.max.y - fab - margin))
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    let (rect, resp) = ui.allocate_exact_size(vec2(fab, fab), egui::Sense::click());
                    let hot = resp.hovered() || self.palette_open;
                    let (bg, fg) = if hot {
                        (theme.colors.amber, theme.colors.void)
                    } else {
                        (theme.colors.void, theme.colors.amber)
                    };
                    let p = ui.painter();
                    p.circle_filled(rect.center(), fab * 0.5, bg);
                    p.circle_stroke(
                        rect.center(),
                        fab * 0.5,
                        egui::Stroke::new(1.5, theme.colors.amber),
                    );
                    p.text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "+",
                        egui::FontId::monospace(fab * 0.6),
                        fg,
                    );
                    if resp.on_hover_text("Add effect").clicked() {
                        self.palette_open = !self.palette_open;
                    }
                });
        }

        // Contextual action bar for the selected effect node (touch Remove/Dup).
        if self.graph_view.selected_is_effect() {
            egui::Area::new(egui::Id::new("node_action_bar"))
                .fixed_pos(pos2(area.min.x + margin, area.min.y + margin))
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .button(
                                    egui::RichText::new("Duplicate")
                                        .font(FontId::monospace(11.0))
                                        .color(theme.colors.text_primary),
                                )
                                .clicked()
                                && let Some(n) = self.graph_view.selected_node
                            {
                                self.graph_view.duplicate_node(n);
                            }
                            if ui
                                .button(
                                    egui::RichText::new("Remove")
                                        .font(FontId::monospace(11.0))
                                        .color(theme.colors.red),
                                )
                                .clicked()
                                && let Some(n) = self.graph_view.selected_node
                            {
                                self.graph_view.remove_node(n);
                            }
                        });
                    });
                });
        }

        // Effect-palette sheet (opened by the FAB).
        if self.palette_open {
            let filter = self.palette_filter.to_lowercase();
            let effects: Vec<(&'static str, &'static str)> = self
                .registry
                .all_effects()
                .iter()
                .filter(|d| {
                    filter.is_empty()
                        || d.name.to_lowercase().contains(&filter)
                        || d.id.contains(&filter)
                })
                .map(|d| (d.id, d.name))
                .collect();

            let mut open = true;
            let mut chosen: Option<&'static str> = None;
            egui::Window::new(
                egui::RichText::new("ADD EFFECT")
                    .font(FontId::monospace(12.0))
                    .color(theme.colors.amber),
            )
            .id(egui::Id::new("effect_palette"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_BOTTOM, vec2(0.0, -48.0))
            .show(ctx, |ui| {
                ui.set_min_width(260.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Search")
                            .font(FontId::monospace(10.0))
                            .color(theme.colors.text_secondary),
                    );
                    // Auto-focus on open so you can type-to-filter immediately
                    // (keyboard-driven add — no extra click into the field). Skip
                    // this on touch tiers: re-grabbing focus whenever nothing is
                    // focused would pop the soft keyboard the instant a thumb rests
                    // on the list to scroll. Touch users tap the field to type.
                    let auto_focus = !self.breakpoint.is_compact();
                    let search = ui.text_edit_singleline(&mut self.palette_filter);
                    if auto_focus && (search.gained_focus() || ui.memory(|m| m.focused().is_none()))
                    {
                        search.request_focus();
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(300.0)
                    .show(ui, |ui| {
                        for (id, name) in &effects {
                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new(*name).font(FontId::monospace(12.0)),
                                    )
                                    .min_size(vec2(244.0, 28.0)),
                                )
                                .clicked()
                            {
                                chosen = Some(id);
                            }
                        }
                    });
            });
            if let Some(id) = chosen {
                self.graph_view.add_effect_node(id);
                self.palette_filter.clear();
                open = false;
            }
            self.palette_open = open;
        }
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

        // Wrap so the toolbar flows onto extra rows on a narrow viewport instead
        // of running off-screen; on a desktop window everything fits one row, so
        // the layout is unchanged.
        ui.horizontal_wrapped(|ui| {
            // SONIDO brand
            ui.heading(
                egui::RichText::new("SONIDO")
                    .font(FontId::monospace(18.0))
                    .color(theme.colors.amber)
                    .strong(),
            );
            ui.add_space(12.0);

            // Transport: ONE prominent Play/Pause for both GEN and FILE sources.
            // Green when playing, amber when stopped. On the web the click is a
            // user gesture, so toggle_transport() also resumes the AudioContext —
            // one tap on PLAY both starts the source and unblocks the output.
            let playing = self.file_player.is_playing();
            let (play_label, play_color) = if playing {
                ("PAUSE", theme.colors.green)
            } else {
                ("PLAY", theme.colors.amber)
            };
            let play_btn = ui.button(
                egui::RichText::new(play_label)
                    .font(FontId::monospace(14.0))
                    .color(play_color)
                    .strong(),
            );
            let play_led = pos2(play_btn.rect.right() + 8.0, play_btn.rect.center().y);
            glow::glow_circle(ui.painter(), play_led, 3.0, play_color, &theme);
            ui.add_space(12.0);
            if play_btn.on_hover_text("Play / Pause  (Space)").clicked() {
                self.toggle_transport();
            }

            ui.separator();

            // RIG drawer toggle — Phone only. The performance band can't stay
            // docked on a 390px screen, so it becomes a summonable drawer.
            if self.breakpoint == Breakpoint::Phone {
                let rig_open = self.drawer == Drawer::Rig;
                let rig_color = if rig_open {
                    theme.colors.cyan
                } else {
                    theme.colors.dim
                };
                if ui
                    .button(
                        egui::RichText::new("RIG")
                            .font(FontId::monospace(11.0))
                            .color(rig_color)
                            .strong(),
                    )
                    .on_hover_text("Show/hide macros + morph")
                    .clicked()
                {
                    self.drawer = if rig_open { Drawer::None } else { Drawer::Rig };
                }
                ui.separator();
            }

            // FOCUS toggle — Desktop/Tablet. Collapses docked chrome for a
            // full-width canvas; the floating add affordances stay live.
            if self.breakpoint != Breakpoint::Phone {
                let focus_color = if self.chrome_collapsed {
                    theme.colors.cyan
                } else {
                    theme.colors.dim
                };
                if ui
                    .button(
                        egui::RichText::new("FOCUS")
                            .font(FontId::monospace(11.0))
                            .color(focus_color)
                            .strong(),
                    )
                    .on_hover_text("Full-width canvas \u{2014} hide I/O, morph, info  (F)")
                    .clicked()
                {
                    self.chrome_collapsed = !self.chrome_collapsed;
                }
                ui.separator();
            }

            // Undo / Redo — touch-reachable (was keyboard-only, dead on mobile).
            let can_undo = !self.undo_stack.is_empty();
            let can_redo = !self.redo_stack.is_empty();
            let undo_color = if can_undo {
                theme.colors.text_primary
            } else {
                theme.colors.dim
            };
            if ui
                .button(
                    egui::RichText::new("UNDO")
                        .font(FontId::monospace(11.0))
                        .color(undo_color),
                )
                .on_hover_text("Undo  (Ctrl+Z)")
                .clicked()
            {
                self.undo();
            }
            let redo_color = if can_redo {
                theme.colors.text_primary
            } else {
                theme.colors.dim
            };
            if ui
                .button(
                    egui::RichText::new("REDO")
                        .font(FontId::monospace(11.0))
                        .color(redo_color),
                )
                .on_hover_text("Redo  (Ctrl+Shift+Z)")
                .clicked()
            {
                self.redo();
            }

            // Add-node: a labeled button (clearer than a bare "+"). On phone the
            // thumb-reachable FAB over the canvas does this job instead.
            if self.breakpoint != Breakpoint::Phone {
                let add_color = if self.palette_open {
                    theme.colors.amber
                } else {
                    theme.colors.text_primary
                };
                if ui
                    .button(
                        egui::RichText::new("+ EFFECT")
                            .font(FontId::monospace(11.0))
                            .color(add_color)
                            .strong(),
                    )
                    .on_hover_text("Add an effect node to the graph")
                    .clicked()
                {
                    self.palette_open = !self.palette_open;
                }
            }

            ui.separator();

            // BYPASS (promoted from status bar)
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
            let circle_center = pos2(bypass_btn.rect.right() + 8.0, bypass_btn.rect.center().y);
            glow::glow_circle(ui.painter(), circle_center, 3.0, bypass_color, &theme);
            ui.add_space(10.0);
            if bypass_btn.clicked() {
                self.audio_bridge
                    .chain_bypass()
                    .store(!chain_bypassed, Ordering::SeqCst);
            }

            ui.separator();

            // Session save / load.
            #[cfg(not(target_arch = "wasm32"))]
            {
                if ui
                    .button(
                        egui::RichText::new("Save")
                            .font(FontId::monospace(12.0))
                            .color(theme.colors.text_primary),
                    )
                    .clicked()
                {
                    self.save_session();
                }
                if ui
                    .button(
                        egui::RichText::new("Load")
                            .font(FontId::monospace(12.0))
                            .color(theme.colors.text_primary),
                    )
                    .clicked()
                {
                    self.load_session();
                }

                // Export the rig as a CLAP patch / pedal binary / DFU flash.
                self.render_export_menu(ui);
            }

            ui.separator();

            // FILE source toggle
            self.file_player.render_source_toggle(ui);

            // Performance macros K1-K6, compact. On phone they live in the RIG
            // drawer instead (the header stays minimal there).
            if self.breakpoint != Breakpoint::Phone {
                ui.separator();
                self.render_macro_strip(ui);
            }

            // Right-aligned: audio status + compile error
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
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
                        egui::RichText::new(format!("errors: {err_count}"))
                            .font(FontId::monospace(10.0))
                            .color(theme.colors.red),
                    );
                }

                if let Some(ref err) = self.compile_error {
                    ui.label(
                        egui::RichText::new(err)
                            .font(FontId::monospace(10.0))
                            .color(theme.colors.red),
                    );
                }

                // Last export/flash result (green on success, red on failure).
                #[cfg(not(target_arch = "wasm32"))]
                if let Some(ref result) = self.export_msg {
                    let (msg, col) = match result {
                        Ok(m) => (m, theme.colors.green),
                        Err(m) => (m, theme.colors.red),
                    };
                    ui.label(
                        egui::RichText::new(msg)
                            .font(FontId::monospace(10.0))
                            .color(col),
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

    /// Render a unified I/O strip (INPUT or OUTPUT endpoint).
    ///
    /// `is_input` selects between input gain / output master controls and metering.
    fn render_io_strip(&mut self, ui: &mut egui::Ui, is_input: bool) {
        let theme = SonidoTheme::get(ui.ctx());
        let label = if is_input { "INPUT" } else { "OUTPUT" };

        ui.group(|ui| {
            ui.set_min_width(50.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new(label)
                        .font(FontId::monospace(11.0))
                        .color(theme.colors.cyan),
                );

                ui.add_space(4.0);

                // Meter
                let (peak, rms) = if is_input {
                    (self.metering.input_peak, self.metering.input_rms)
                } else {
                    (self.metering.output_peak, self.metering.output_rms)
                };
                ui.add(LevelMeter::new(peak, rms).size(20.0, 100.0));
                // The meter owns its clip + peak readout now: a numeric dBFS
                // header that turns red past 0 dBFS, click the meter to reset.

                ui.add_space(4.0);

                // Gain knob
                if is_input {
                    let input_gain = self.audio_bridge.input_gain();
                    let mut gain_val = input_gain.get();
                    if ui
                        .add(
                            Knob::new(&mut gain_val, -20.0, 20.0, "GAIN")
                                .default(0.0)
                                .format_db()
                                .diameter(44.0),
                        )
                        .changed()
                    {
                        input_gain.set(gain_val);
                    }
                } else {
                    let master_vol_param = self.audio_bridge.master_volume();
                    let mut master_val = master_vol_param.get();
                    if ui
                        .add(
                            Knob::new(&mut master_val, -40.0, 6.0, "VOL")
                                .default(0.0)
                                .format_db()
                                .diameter(44.0),
                        )
                        .changed()
                    {
                        master_vol_param.set(master_val);
                    }
                }
            });
        });
    }

    /// Compact horizontal levels row for the Phone layout: input gain, in/out
    /// horizontal meters with clip indicators, and master volume. Replaces the
    /// two tall vertical I/O strips so the canvas gets full width — nothing is
    /// removed, only re-laid-out.
    fn render_phone_levels(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // Input gain
            let input_gain = self.audio_bridge.input_gain();
            let mut gain_val = input_gain.get();
            if ui
                .add(
                    Knob::new(&mut gain_val, -20.0, 20.0, "IN")
                        .default(0.0)
                        .format_db()
                        .diameter(38.0),
                )
                .changed()
            {
                input_gain.set(gain_val);
            }
            ui.add(
                LevelMeter::new(self.metering.input_peak, self.metering.input_rms)
                    .horizontal()
                    .size(78.0, 10.0),
            );

            ui.separator();

            ui.add(
                LevelMeter::new(self.metering.output_peak, self.metering.output_rms)
                    .horizontal()
                    .size(78.0, 10.0),
            );
            // Master volume
            let master = self.audio_bridge.master_volume();
            let mut vol_val = master.get();
            if ui
                .add(
                    Knob::new(&mut vol_val, -40.0, 6.0, "OUT")
                        .default(0.0)
                        .format_db()
                        .diameter(38.0),
                )
                .changed()
            {
                master.set(vol_val);
            }
        });
    }

    /// Show a quick-reference hint when no node is selected in the graph.
    fn render_quick_reference(ui: &mut egui::Ui) {
        let theme = SonidoTheme::get(ui.ctx());
        ui.vertical_centered(|ui| {
            ui.add_space(8.0);
            // The full control reference now lives in the docked Info View
            // below; the empty center just prompts the next action.
            ui.label(
                egui::RichText::new("Select a node to edit it \u{00b7} + EFFECT to add one")
                    .font(FontId::monospace(10.0))
                    .color(theme.colors.text_secondary)
                    .italics(),
            );
        });
    }

    /// Assemble the contextual Info View payload from the current selection.
    ///
    /// When an effect node is selected, the title + prose come from the effect
    /// registry and each parameter's live value is read through the same
    /// [`ParamDescriptor`](sonido_core::ParamDescriptor) metadata that drives
    /// accessibility — one source, no drift. Otherwise the empty-state prompt
    /// plus the general shortcuts are shown.
    fn build_info_payload(&self) -> InfoPayload {
        let Some(slot_idx) = self.info_slot else {
            return InfoPayload::empty_state();
        };
        let slot = SlotIndex(slot_idx);
        if slot.0 >= self.bridge.slot_count() {
            return InfoPayload::empty_state();
        }

        let effect_id = self.bridge.effect_id(slot);
        let descriptor = self.registry.descriptor(effect_id);
        let name = descriptor.map(|d| d.name).unwrap_or(effect_id);
        let description = descriptor.map(|d| d.description).unwrap_or("");

        let mut params = Vec::new();
        for i in 0..self.bridge.param_count(slot) {
            let param = ParamIndex(i);
            if let Some(pd) = self.bridge.param_descriptor(slot, param) {
                let value = self.bridge.get(slot, param);
                params.push((pd.short_name.to_string(), pd.format_value(value)));
            }
        }

        InfoPayload::for_effect(name, description, params)
    }

    /// Estimate the needed effect panel height from the cached panel's param count.
    fn estimate_panel_height(&self) -> f32 {
        let param_count = self
            .cached_panel
            .as_ref()
            .map(|(slot, _, _)| self.bridge.param_count(*slot))
            .unwrap_or(6);
        // Rough estimate: title row + ~40px per row of 4-5 knobs
        let rows = param_count.div_ceil(5).max(1);
        80.0 + rows as f32 * 60.0
    }

    /// Render the effect panel for the selected slot.
    ///
    /// The panel widget is cached in `self.cached_panel` and only reconstructed
    /// when the selected slot or effect type changes. Includes an inline morph
    /// bar in the title row when in multi-effect mode.
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
            .stroke(Stroke::new(2.0, theme.colors.amber))
            .corner_radius(theme.sizing.panel_border_radius)
            .inner_margin(Margin::same(theme.sizing.panel_padding as i8));

        panel_frame.show(ui, |ui| {
            // Title row: effect name + morph bar + bypass
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(panel_name)
                        .font(FontId::monospace(12.0))
                        .color(theme.colors.amber)
                        .strong(),
                );

                // Bypass — a clearly labeled toggle on the right of the header.
                // (Global A/B morph lives in its own full-width band, not here —
                // morph crossfades the whole rig, not this one effect.)
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let is_bypassed = self.bridge.is_bypassed(slot);
                    let (label, col) = if is_bypassed {
                        ("BYPASSED", theme.colors.red)
                    } else {
                        ("ACTIVE", theme.colors.green)
                    };
                    let btn = egui::Button::new(
                        egui::RichText::new(label)
                            .font(FontId::monospace(10.0))
                            .color(col),
                    )
                    .stroke(Stroke::new(1.0, col))
                    .fill(col.gamma_multiply(0.10))
                    .min_size(vec2(76.0, 20.0));
                    if ui.add(btn).clicked() {
                        self.bridge.set_bypassed(slot, !is_bypassed);
                    }
                    // Breathing room between the bypass toggle and the morph bar.
                    ui.add_space(16.0);

                    // Morph-lock toggle (left of bypass): hold this effect fixed
                    // while the A/B morph sweeps the rest of the rig. Only shown
                    // once both morph snapshots exist, since locking is otherwise
                    // a no-op.
                    if self.morph_state.is_ready() {
                        if self.morph_state.locked_slots.len() <= slot.0 {
                            self.morph_state.locked_slots.resize(slot.0 + 1, false);
                        }
                        let locked = self.morph_state.locked_slots[slot.0];
                        let (label, col) = if locked {
                            ("LOCKED", theme.colors.amber)
                        } else {
                            ("LOCK", theme.colors.dim)
                        };
                        let lock_btn = egui::Button::new(
                            egui::RichText::new(label)
                                .font(FontId::monospace(10.0))
                                .color(col),
                        )
                        .stroke(Stroke::new(1.0, col))
                        .fill(col.gamma_multiply(0.10))
                        .min_size(vec2(64.0, 20.0));
                        if ui
                            .add(lock_btn)
                            .on_hover_text("Exclude this effect from the A/B morph")
                            .clicked()
                        {
                            self.morph_state.locked_slots[slot.0] = !locked;
                        }
                        ui.add_space(8.0);
                    }
                });
            });

            ui.add_space(4.0);

            // Effect controls
            if let Some((_, _, ref mut panel)) = self.cached_panel {
                let bridge: &dyn ParamBridge = &*self.bridge;
                panel.ui(ui, bridge, slot);
            }
        });
    }

    /// Render the global A/B morph band — a full-width performance strip.
    ///
    /// Morph is a whole-rig control: it crossfades *every* effect's parameters
    /// between snapshots A and B (curve-aware per parameter). It therefore lives
    /// in its own always-visible band rather than inside any single effect panel.
    /// Click A/B to capture; right-click/double-click to recall; drag the bar to
    /// sweep. Per-knob A/B ring markers show where each parameter sits.
    fn render_morph_band(&mut self, ui: &mut egui::Ui) {
        let theme = SonidoTheme::get(ui.ctx());
        let has_a = self.morph_state.a.is_some();
        let has_b = self.morph_state.b.is_some();
        let ready = has_a && has_b;

        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("MORPH")
                    .font(FontId::monospace(12.0))
                    .color(theme.colors.amber)
                    .strong(),
            );
            ui.add_space(10.0);

            // Position readout (or a hint to capture both snapshots first).
            let (readout, readout_color) = if ready {
                (
                    format!("A→B {:>3.0}%", self.morph_state.t * 100.0),
                    theme.colors.text_primary,
                )
            } else {
                ("capture A + B".to_owned(), theme.colors.text_secondary)
            };
            ui.label(
                egui::RichText::new(readout)
                    .font(FontId::monospace(11.0))
                    .color(readout_color),
            );
            ui.add_space(10.0);

            // The A/B crossfader fills the remaining width.
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
        });
    }

    /// Compact macro strip: six small K1-K6 position knobs with no label, percent,
    /// or name rows. Hover a knob for its name and target count; right-click to
    /// open its mapping editor. Lives in the header on desktop and in the phone
    /// RIG drawer.
    ///
    /// Turning a knob sweeps every parameter mapped to that macro at once; the
    /// resolved values go straight to the bridge so the rig responds immediately.
    fn render_macro_strip(&mut self, ui: &mut egui::Ui) {
        let theme = SonidoTheme::get(ui.ctx());
        // Positions are canonical in the map; copy out, let the knobs mutate, then
        // write back the one that changed.
        let mut pos: [f32; 6] = std::array::from_fn(|i| self.macro_map.position(i));
        let mut changed: Option<usize> = None;
        let mut edit: Option<usize> = None;

        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("MACROS")
                    .font(FontId::monospace(10.0))
                    .color(theme.colors.text_secondary),
            );
            for i in 0..6 {
                let mapped = self.macro_map.mapping_count_for(i);
                let resp = ui.add(
                    Knob::new(&mut pos[i], 0.0, 1.0, "")
                        .diameter(26.0)
                        .show_value(false),
                );
                let name = if self.macro_names[i].is_empty() {
                    format!("Macro {}", i + 1)
                } else {
                    self.macro_names[i].clone()
                };
                let tip = if mapped > 0 {
                    format!("{name}: {mapped} target(s) (right-click to edit)")
                } else {
                    format!("{name}: unmapped (map a param from its right-click menu)")
                };
                let resp = resp.on_hover_text(tip);
                if resp.changed() {
                    changed = Some(i);
                }
                if resp.secondary_clicked() {
                    edit = Some(i);
                }
                // A small cyan tick under mapped macros shows which knobs are live.
                if mapped > 0 && ui.is_rect_visible(resp.rect) {
                    let tick = pos2(resp.rect.center().x, resp.rect.bottom() - 1.0);
                    glow::glow_circle(ui.painter(), tick, 1.5, theme.colors.cyan, &theme);
                }
            }
        });

        if let Some(i) = changed {
            self.macro_map.set_position(i, pos[i]);
            self.apply_macro(i);
        }
        if let Some(i) = edit {
            self.macro_editor = Some(i);
        }
    }

    /// Apply macro `index`'s mappings at its current position to the rig.
    ///
    /// Slot targets write to the bridge; global targets route through
    /// [`apply_global`](Self::apply_global). Resolved writes are collected first
    /// so the immutable borrow of `macro_map` ends before the mutable apply.
    fn apply_macro(&mut self, index: usize) {
        let mut writes: Vec<(MacroTarget, f32)> = Vec::new();
        self.macro_map.apply(index, |t, v| writes.push((t, v)));
        for (target, value) in writes {
            match target {
                MacroTarget::Slot { slot, param } => {
                    self.bridge
                        .set(SlotIndex(slot as usize), ParamIndex(param as usize), value);
                }
                MacroTarget::Global(g) => self.apply_global(g, value),
            }
        }
    }

    /// Write one global-target value (input gain, master volume, morph position).
    fn apply_global(&mut self, g: GlobalParam, value: f32) {
        match g {
            GlobalParam::InputGain => self.audio_bridge.input_gain().set(value),
            GlobalParam::MasterVolume => self.audio_bridge.master_volume().set(value),
            GlobalParam::MorphPosition => {
                self.morph_state.t = value.clamp(0.0, 1.0);
                self.morph_state.active = true;
                self.morph_state.apply(&*self.bridge);
            }
            // MorphSpeed (and any future global) — no GUI control yet; the morph
            // band grows a speed knob in Workstream C.
            _ => {}
        }
    }

    /// Apply a macro-mapping action from a parameter knob's right-click menu.
    ///
    /// Binding is exclusive (a parameter drives at most one macro): any prior
    /// binding for the target is cleared first. The range and curve come from the
    /// parameter's own descriptor, so the macro sweeps its full range with the
    /// right shape (log for frequency, snap for stepped). Binding does *not* snap
    /// the parameter — it keeps its current value until the macro is next moved.
    fn handle_macro_action(&mut self, action: MacroAction) {
        match action {
            MacroAction::Map {
                slot,
                param,
                macro_index,
            } => {
                let target = MacroTarget::Slot {
                    slot: slot as u8,
                    param: param as u8,
                };
                self.macro_map.clear_target(target);
                let (min, max, curve) = self
                    .bridge
                    .param_descriptor(SlotIndex(slot), ParamIndex(param))
                    .map_or((0.0, 1.0, MorphCurve::Linear), |d| {
                        (d.min, d.max, MorphCurve::from_descriptor(&d))
                    });
                self.macro_map.add_mapping(MacroMapping {
                    macro_index,
                    target,
                    min,
                    max,
                    curve,
                });
            }
            MacroAction::Clear { slot, param } => {
                self.macro_map.clear_target(MacroTarget::Slot {
                    slot: slot as u8,
                    param: param as u8,
                });
            }
        }
    }

    /// Human-readable label for a macro target ("Distortion · Drive").
    fn describe_target(&self, target: MacroTarget) -> String {
        match target {
            MacroTarget::Slot { slot, param } => {
                let s = SlotIndex(slot as usize);
                let effect = self
                    .registry
                    .descriptor(self.bridge.effect_id(s))
                    .map_or("?", |d| d.name);
                let pname = self
                    .bridge
                    .param_descriptor(s, ParamIndex(param as usize))
                    .map_or("?", |d| d.short_name);
                format!("{effect} · {pname}")
            }
            MacroTarget::Global(g) => format!("{g:?}"),
        }
    }

    /// Render the floating mapping editor for the open macro, if any.
    ///
    /// Lets the user rename the macro (its name maps to a physical pedal knob),
    /// review/remove the parameters it drives, and set each mapping's `[min,
    /// max]` range — drag the ends or hit ⇄ to invert (`max < min`, which the
    /// engine clamps). Range edits re-apply the macro at its current position so
    /// the change is audible immediately.
    fn render_macro_editor(&mut self, ctx: &Context) {
        let Some(idx) = self.macro_editor else {
            return;
        };

        // Pre-resolve each mapping (Copy) + its label so the window closure can
        // edit ranges in place without borrowing the bridge/registry.
        let mut rows: Vec<(MacroMapping, String)> = self
            .macro_map
            .mappings()
            .iter()
            .filter(|m| m.macro_index == idx)
            .map(|m| (*m, self.describe_target(m.target)))
            .collect();

        let mut to_clear: Option<MacroTarget> = None;
        let mut range_update: Option<(MacroTarget, f32, f32)> = None;
        let mut clear_all = false;

        let mut open = true;
        egui::Window::new(format!("Macro K{} mapping", idx + 1))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Name:");
                    ui.text_edit_singleline(&mut self.macro_names[idx]);
                });
                ui.separator();

                if rows.is_empty() {
                    ui.label(
                        egui::RichText::new(
                            "No parameters mapped.\nRight-click a knob → Map to Macro.",
                        )
                        .font(FontId::monospace(10.0)),
                    );
                } else {
                    for (mapping, label) in &mut rows {
                        let target = mapping.target;
                        ui.horizontal(|ui| {
                            if ui.small_button("✕").clicked() {
                                to_clear = Some(target);
                            }
                            ui.label(label.as_str());
                        });
                        // Drag the macro's 0% and 100% values; ⇄ swaps them to
                        // invert the sweep. Speed scales with the span so wide
                        // (Hz) and narrow (dB) ranges both drag sensibly.
                        let span = (mapping.max - mapping.min).abs().max(1.0);
                        let speed = (span * 0.005).max(0.001);
                        let mut changed = false;
                        ui.horizontal(|ui| {
                            ui.add_space(24.0);
                            ui.label(
                                egui::RichText::new("0%")
                                    .font(FontId::monospace(9.0))
                                    .weak(),
                            );
                            changed |= ui
                                .add(egui::DragValue::new(&mut mapping.min).speed(speed))
                                .changed();
                            ui.label(
                                egui::RichText::new("100%")
                                    .font(FontId::monospace(9.0))
                                    .weak(),
                            );
                            changed |= ui
                                .add(egui::DragValue::new(&mut mapping.max).speed(speed))
                                .changed();
                            if ui.small_button("⇄").on_hover_text("Invert").clicked() {
                                std::mem::swap(&mut mapping.min, &mut mapping.max);
                                changed = true;
                            }
                        });
                        if changed {
                            range_update = Some((target, mapping.min, mapping.max));
                        }
                    }
                    ui.separator();
                    if ui.button("Clear all").clicked() {
                        clear_all = true;
                    }
                }
            });

        // Apply edits after the closure so `self` is free of the window borrow.
        if let Some(t) = to_clear {
            self.macro_map.clear_target(t);
        }
        if clear_all {
            self.macro_map.clear_macro(idx);
        }
        if let Some((target, min, max)) = range_update {
            self.macro_map.set_target_range(target, min, max);
            self.apply_macro(idx);
        }

        if !open {
            self.macro_editor = None;
        }
    }

    /// Render the status bar.
    fn render_status_bar(&mut self, ui: &mut egui::Ui) {
        let theme = SonidoTheme::get(ui.ctx());

        // Wrap so the diagnostics flow onto a second row on a narrow viewport
        // rather than clipping; unchanged on a desktop-width window.
        ui.horizontal_wrapped(|ui| {
            // Sample rate
            ui.label(
                egui::RichText::new(format!("{:.0}Hz", self.sample_rate))
                    .font(FontId::monospace(11.0))
                    .color(theme.colors.amber),
            );
            ui.separator();

            // Latency
            let latency_ms = self.buffer_size as f32 / self.sample_rate * 1000.0;
            ui.label(
                egui::RichText::new(format!("{latency_ms:.1}ms"))
                    .font(FontId::monospace(11.0))
                    .color(theme.colors.amber),
            );
            ui.separator();

            // CPU meter — fixed-width allocation to prevent sparkline jitter
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

            ui.allocate_ui_with_layout(
                vec2(240.0, 24.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.set_min_width(240.0);
                    // Fixed-width label so sparkline position doesn't jitter
                    ui.allocate_ui_with_layout(
                        vec2(120.0, 24.0),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.label(
                                egui::RichText::new(&cpu_text)
                                    .font(FontId::monospace(11.0))
                                    .color(cpu_color),
                            );
                        },
                    );
                    if !self.cpu_history.is_empty() {
                        draw_sparkline(ui, &self.cpu_history, cpu_color, 100.0, 24.0);
                    }
                },
            );

            // Daisy eligibility badge — green if ≤3 effects, red if >3
            ui.separator();
            let effect_count = self.graph_view.effect_node_count();
            let daisy_color = if effect_count <= 3 {
                theme.colors.green
            } else {
                theme.colors.red
            };
            ui.label(
                egui::RichText::new(format!("Daisy: {effect_count}/3"))
                    .font(FontId::monospace(11.0))
                    .color(daisy_color),
            );

            // File player / generator transport (inline)
            ui.separator();
            self.file_player.render_compact(ui);
        });
    }

    /// Snapshot the macro + morph performance layer for a session capture.
    ///
    /// Bundles the six macros (defs + live knob positions), the morph
    /// behaviour/locks and crossfade position, and the A/B snapshots whose
    /// per-slot values become each effect's parameter sets.
    fn performance_capture(&self) -> crate::session::PerformanceCapture<'_> {
        let mut morph = sonido_patch::MorphConfig::default();
        for (i, &locked) in self.morph_state.locked_slots.iter().enumerate() {
            morph.set_locked(i, locked);
        }
        crate::session::PerformanceCapture {
            macros: crate::session::macro_map_to_defs(&self.macro_map, &self.macro_names),
            macro_positions: std::array::from_fn(|i| self.macro_map.position(i)),
            morph,
            morph_position: self.morph_state.t,
            morph_a: self.morph_state.a.as_ref(),
            morph_b: self.morph_state.b.as_ref(),
        }
    }

    /// Capture the complete editor state (graph + params + performance layer)
    /// as a [`Session`](crate::session::Session) — the unit of undo and export.
    fn snapshot(&self) -> crate::session::Session {
        self.graph_view.capture_session(
            &*self.bridge,
            self.audio_bridge.input_gain().get(),
            self.audio_bridge.master_volume().get(),
            self.performance_capture(),
        )
    }

    /// Restore the full editor from a session: rebuild topology, recompile,
    /// and reapply gains, per-effect params/bypass, and the macro/morph layer.
    ///
    /// Shared by session load and undo/redo so all three paths restore identically.
    fn apply_session(&mut self, session: &crate::session::Session) {
        self.graph_view.restore_session(session, &self.registry);
        self.compile_and_apply();
        self.audio_bridge.input_gain().set(session.input_gain);
        self.audio_bridge.master_volume().set(session.master_volume);
        // Restore per-effect A params + bypass, matching node index → chain slot.
        for (node_idx, state) in &session.params {
            let mut slot = 0usize;
            for (i, entry) in session.nodes.iter().enumerate() {
                if matches!(entry.node, crate::session::SessionNode::Effect { .. }) {
                    if i == *node_idx {
                        for (p, &val) in state.params.iter().enumerate() {
                            self.bridge
                                .set(SlotIndex(slot), sonido_gui_core::ParamIndex(p), val);
                        }
                        self.bridge.set_bypassed(SlotIndex(slot), state.bypassed);
                        break;
                    }
                    slot += 1;
                }
            }
        }
        self.restore_performance(session);
    }

    /// Undo the last structural edit, returning `true` if anything was undone.
    fn undo(&mut self) -> bool {
        if let Some(prev) = self.undo_stack.pop() {
            self.redo_stack.push(self.snapshot());
            self.apply_session(&prev);
            true
        } else {
            false
        }
    }

    /// Redo the last undone edit, returning `true` if anything was redone.
    fn redo(&mut self) -> bool {
        if let Some(next) = self.redo_stack.pop() {
            self.undo_stack.push(self.snapshot());
            self.apply_session(&next);
            true
        } else {
            false
        }
    }

    /// Build the canonical [`Patch`](sonido_patch::Patch) from the current rig,
    /// for export to a CLAP plugin, a `.bin` sector, or the pedal.
    #[cfg(not(target_arch = "wasm32"))]
    fn current_patch(&self, name: &str) -> sonido_patch::Patch {
        self.snapshot().to_patch(name)
    }

    /// Record an export/flash result for the header status line.
    #[cfg(not(target_arch = "wasm32"))]
    fn set_export_msg(&mut self, msg: String, ok: bool) {
        if ok {
            tracing::info!(message = %msg, "export");
        } else {
            tracing::error!(message = %msg, "export");
        }
        self.export_msg = Some(if ok { Ok(msg) } else { Err(msg) });
    }

    /// Render the Export menu: project the rig to a [`Patch`] and offer the four
    /// destinations — CLAP sidecar, portable JSON, pedal `.bin` sector, and DFU
    /// flash. Pedal targets validate against the device's effect/CPU/SDRAM
    /// budget first and are disabled (with the failing findings shown) when the
    /// rig won't fit.
    #[cfg(not(target_arch = "wasm32"))]
    fn render_export_menu(&mut self, ui: &mut egui::Ui) {
        use sonido_patch::validate::Severity;
        let theme = SonidoTheme::get(ui.ctx());

        ui.menu_button(
            egui::RichText::new("Export")
                .font(FontId::monospace(12.0))
                .color(theme.colors.text_primary),
            |ui| {
                let patch = self.current_patch("Sonido Rig");
                let findings = crate::export::validate_patch_for_pedal(&patch);
                let pedal_ok = crate::export::can_export_to_pedal(&patch);

                // ── Plugin / portable destinations (always available) ──
                if ui
                    .button("Export as CLAP patch")
                    .on_hover_text("Write to the graph-player plugin's patch folder")
                    .clicked()
                {
                    match crate::export::export_as_clap(&patch) {
                        Ok(p) => self
                            .set_export_msg(format!("Exported CLAP patch → {}", p.display()), true),
                        Err(e) => self.set_export_msg(format!("CLAP export failed: {e}"), false),
                    }
                    ui.close_menu();
                }
                if ui.button("Save patch JSON…").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_title("Save Patch JSON")
                        .add_filter("Sonido Patch", &["json"])
                        .save_file()
                    {
                        let result = crate::export::patch_to_json(&patch)
                            .map_err(|e| e.to_string())
                            .and_then(|j| std::fs::write(&path, j).map_err(|e| e.to_string()));
                        match result {
                            Ok(()) => self
                                .set_export_msg(format!("Saved patch JSON → {}", path.display()), true),
                            Err(e) => self.set_export_msg(format!("JSON export failed: {e}"), false),
                        }
                    }
                    ui.close_menu();
                }

                // ── Pedal (Daisy) destinations — gated on validation ──
                ui.separator();
                ui.label(
                    egui::RichText::new("Pedal (Daisy)")
                        .font(FontId::monospace(9.0))
                        .color(theme.colors.text_secondary),
                );
                if pedal_ok {
                    ui.label(
                        egui::RichText::new("✓ fits pedal budget")
                            .font(FontId::monospace(10.0))
                            .color(theme.colors.green),
                    );
                } else {
                    ui.label(
                        egui::RichText::new("✗ exceeds pedal limits")
                            .font(FontId::monospace(10.0))
                            .color(theme.colors.red),
                    );
                }
                for f in &findings {
                    let col = match f.severity {
                        Severity::Error => theme.colors.red,
                        Severity::Warning => theme.colors.yellow,
                    };
                    ui.label(
                        egui::RichText::new(format!("· {}", f.message))
                            .font(FontId::monospace(9.0))
                            .color(col),
                    );
                }

                if ui
                    .add_enabled(pedal_ok, egui::Button::new("Save pedal binary (.bin)…"))
                    .clicked()
                {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_title("Save Pedal Binary")
                        .add_filter("Pedal Sector", &["bin"])
                        .save_file()
                    {
                        let result = crate::export::encode_patch_sector(&patch)
                            .map_err(|e| e.to_string())
                            .and_then(|buf| std::fs::write(&path, buf).map_err(|e| e.to_string()));
                        match result {
                            Ok(()) => self.set_export_msg(
                                format!("Saved pedal binary → {}", path.display()),
                                true,
                            ),
                            Err(e) => self.set_export_msg(format!("Binary export failed: {e}"), false),
                        }
                    }
                    ui.close_menu();
                }
                if ui
                    .add_enabled(pedal_ok, egui::Button::new("Flash to pedal (DFU)"))
                    .on_hover_text(
                        "Put the pedal in bootloader (hold both footswitches ~1.5 s), then flash slot 0",
                    )
                    .clicked()
                {
                    let result = crate::export::encode_patch_sector(&patch)
                        .map_err(|e| e.to_string())
                        .and_then(|buf| crate::dfu::flash_patch(0, &buf).map_err(|e| e.to_string()));
                    match result {
                        Ok(()) => {
                            self.set_export_msg("Flashed patch to pedal slot 0".to_owned(), true)
                        }
                        Err(e) => self.set_export_msg(format!("Flash failed: {e}"), false),
                    }
                    ui.close_menu();
                }
            },
        );
    }

    /// Save the current session to a JSON file via file dialog.
    #[cfg(not(target_arch = "wasm32"))]
    fn save_session(&self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Save Session")
            .add_filter("Sonido Session", &["json"])
            .save_file()
        {
            let session = self.graph_view.capture_session(
                &*self.bridge,
                self.audio_bridge.input_gain().get(),
                self.audio_bridge.master_volume().get(),
                self.performance_capture(),
            );
            if let Err(e) = session.save(&path) {
                tracing::error!(error = %e, "failed to save session");
            }
        }
    }

    /// Load a session from a JSON file via file dialog.
    #[cfg(not(target_arch = "wasm32"))]
    fn load_session(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Load Session")
            .add_filter("Sonido Session", &["json"])
            .pick_file()
        {
            match crate::session::Session::load(&path) {
                Ok(session) => {
                    // Loading a session is a fresh start — drop edit history so
                    // undo can't step back into the previous rig.
                    self.undo_stack.clear();
                    self.redo_stack.clear();
                    self.apply_session(&session);
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to load session");
                }
            }
        }
    }

    /// Restore the macro map + A/B morph state from a loaded session.
    ///
    /// Rebuilds the six macros (defs + knob positions) and the per-slot morph
    /// locks. When the session carried a B snapshot, it also restores the A/B
    /// morph snapshots and crossfade position and re-applies the morph, so the
    /// live rig sounds exactly as it did when saved.
    fn restore_performance(&mut self, session: &crate::session::Session) {
        use crate::morph_state::{MorphSnapshot, SlotSnapshot};
        use crate::session::SessionNode;

        // Macros: defs → runtime map + names, then live knob positions.
        let (map, names) = crate::session::defs_to_macro_map(&session.macros);
        self.macro_map = map;
        self.macro_names = names;
        for (i, &pos) in session.macro_positions.iter().enumerate() {
            self.macro_map.set_position(i, pos);
        }

        // Build A/B snapshots from each effect's stored params, in slot order.
        let mut a_slots = Vec::new();
        let mut b_slots = Vec::new();
        let mut has_b = false;
        for (i, entry) in session.nodes.iter().enumerate() {
            if matches!(entry.node, SessionNode::Effect { .. })
                && let Some(state) = session.params.get(&i)
            {
                if !state.params_b.is_empty() {
                    has_b = true;
                }
                a_slots.push(SlotSnapshot {
                    effect_id: state.effect_id.clone(),
                    values: state.params.clone(),
                    bypassed: state.bypassed,
                });
                b_slots.push(SlotSnapshot {
                    effect_id: state.effect_id.clone(),
                    values: state.snapshot_b().to_vec(),
                    bypassed: state.bypassed,
                });
            }
        }

        // Per-slot morph locks from the config bitfield.
        self.morph_state.locked_slots = (0..a_slots.len())
            .map(|i| session.morph.is_locked(i))
            .collect();

        if has_b {
            // A real A/B morph was saved — restore both snapshots and position,
            // then re-apply so the live rig matches the saved crossfade.
            self.morph_state.a = Some(MorphSnapshot { slots: a_slots });
            self.morph_state.b = Some(MorphSnapshot { slots: b_slots });
            self.morph_state.t = session.morph_position.clamp(0.0, 1.0);
            self.morph_state.active = true;
            self.morph_state.apply(&*self.bridge);
        } else {
            // No B snapshot stored ⇒ morph was never set up; leave it idle.
            self.morph_state.a = None;
            self.morph_state.b = None;
            self.morph_state.t = 0.0;
            self.morph_state.active = false;
        }
    }
}

/// Draw a sparkline graph with phosphor glow from a history of values.
fn draw_sparkline(
    ui: &mut egui::Ui,
    history: &[f32],
    color: egui::Color32,
    width: f32,
    height: f32,
) {
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
        // Classify the viewport once per frame; touch affordances key off this.
        self.breakpoint = Breakpoint::from_width(ctx.screen_rect().width());

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
        // Browsers suspend the AudioContext until a *trusted* user interaction;
        // re-calling play() from within that activation window resumes it.
        //
        // Gate on a pointer press OR a held key, both of which are genuine
        // user-activation gestures the browser honours for AudioContext.resume().
        // (The old `!i.events.is_empty()` clause fired on a mere mouse-move, which
        // is NOT activation, so resume() was denied yet `audio_resumed` latched
        // true, leaving the context suspended forever. A key press was missing
        // entirely, so pressing space resumed nothing until a click happened.)
        #[cfg(target_arch = "wasm32")]
        if !self.audio_resumed && ctx.input(|i| i.pointer.any_pressed() || !i.keys_down.is_empty())
        {
            use cpal::traits::StreamTrait;
            for stream in &self._audio_streams {
                let _ = stream.play();
            }
            self.audio_resumed = true;
        }

        // Adaptive repaint: 60fps when audio/metering is active, 4fps when idle
        let is_animating = self.audio_bridge.is_running()
            || self.file_player.is_playing()
            || self.metering.output_peak > 0.001;
        #[cfg(target_arch = "wasm32")]
        ctx.request_repaint_after(std::time::Duration::from_millis(if is_animating {
            33
        } else {
            250
        }));
        #[cfg(not(target_arch = "wasm32"))]
        ctx.request_repaint_after(Duration::from_millis(if is_animating { 16 } else { 250 }));

        // Global keyboard shortcuts. Active whenever a text field is NOT capturing
        // the keyboard — so they work while a knob (or the graph) is focused, but
        // not while typing in a search/name field. Space is *consumed* so a focused
        // knob can't swallow it (egui otherwise treats Space as a click on the
        // focused widget).
        let typing = ctx.wants_keyboard_input();
        if !typing && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Space)) {
            let can_play = match self.file_player.source_mode() {
                crate::signal_generator::SourceMode::Generator => true,
                crate::signal_generator::SourceMode::File => self.file_player.has_file(),
            };
            if can_play {
                // Route through toggle_transport (not file_player directly) so the
                // web AudioContext resumes on the spacebar gesture too. Otherwise
                // the transport reads "playing" but stays silent until a mouse
                // click happens to satisfy the resume gate.
                self.toggle_transport();
            }
        }

        // Focus mode: `F` collapses the docked chrome to give the canvas full
        // width (Desktop/Tablet only — Phone is already canvas-first).
        if !typing
            && self.breakpoint != Breakpoint::Phone
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F))
        {
            self.chrome_collapsed = !self.chrome_collapsed;
        }

        // Undo / redo for structural edits (Ctrl+Z, Ctrl+Shift+Z, or Ctrl+Y).
        // `command` is Ctrl on Linux/Windows and ⌘ on macOS.
        if !typing {
            let (do_undo, do_redo) = ctx.input(|i| {
                let cmd = i.modifiers.command;
                let shift = i.modifiers.shift;
                let z = i.key_pressed(egui::Key::Z);
                let y = i.key_pressed(egui::Key::Y);
                (cmd && z && !shift, cmd && ((z && shift) || y))
            });
            if do_undo {
                self.undo();
            } else if do_redo {
                self.redo();
            }
        }

        // Snapshot the editor at the start of any pointer-interaction frame; if
        // that frame produces a structural edit, this becomes the undo point.
        if ctx.input(|i| i.pointer.any_pressed()) {
            self.undo_pending = Some(self.snapshot());
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

        // Focus mode collapses docked chrome for a full-width canvas. Only
        // meaningful off Phone (Phone is already canvas-first), so guard the
        // flag here — a user who collapses on Desktop then shrinks to Phone
        // still gets the normal Phone layout and its RIG drawer.
        let collapsed = self.chrome_collapsed && self.breakpoint != Breakpoint::Phone;

        // Bottom performance band. On Desktop/Tablet it carries just the global
        // A/B morph (the macros live in the header now, which frees this band's
        // height for the selected effect's params). On Phone it is the summonable
        // RIG drawer and carries the macro strip plus morph. Graph mode only.
        let show_perf = !self.single_effect
            && !collapsed
            && (self.breakpoint != Breakpoint::Phone || self.drawer == Drawer::Rig);
        if show_perf {
            TopBottomPanel::bottom("performance").show(ctx, |ui| {
                ui.add_space(4.0);
                if self.breakpoint == Breakpoint::Phone {
                    self.render_macro_strip(ui);
                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(4.0);
                }
                self.render_morph_band(ui);
                ui.add_space(4.0);
            });
        }

        // Contextual Info View — explains the selected node, shows its live
        // parameter values, and lists the relevant shortcuts. Docked just under
        // the canvas on Desktop/Tablet in graph mode (Ableton's Info View, but
        // unified with the registry + accessibility description source).
        if !self.single_effect && self.breakpoint != Breakpoint::Phone && !collapsed {
            let payload = self.build_info_payload();
            TopBottomPanel::bottom("info_view").show(ctx, |ui| {
                let theme = SonidoTheme::get(ui.ctx());
                ui.add_space(3.0);
                info_view::render(ui, &theme, &payload);
                ui.add_space(3.0);
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

            ui.add_space(4.0);

            let theme = SonidoTheme::get(ui.ctx());

            // PHONE: the canvas is the hero. The two flanking I/O strips would
            // crush the graph to ~180px, so fold them into one compact levels
            // row at the top and give the graph full width below.
            let phone = self.breakpoint == Breakpoint::Phone;
            if phone {
                self.render_phone_levels(ui);
                ui.add_space(4.0);
            }

            let avail = ui.available_rect_before_wrap();

            // Hide the flanking I/O strips on Phone (folded into the levels row)
            // and in focus mode (full-width canvas on demand).
            let hide_io = phone || collapsed;

            // Responsive I/O strip widths from ThemeLayout (zero when hidden).
            let io_width = if hide_io {
                0.0
            } else {
                theme.layout.io_strip_width(avail.width())
            };
            let gap = if hide_io { 0.0 } else { 8.0 };
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

            // Input strip (hidden on Phone and in focus mode).
            if !hide_io {
                let mut child = ui.new_child(
                    UiBuilder::new()
                        .id_salt("input_col")
                        .max_rect(input_rect)
                        .layout(Layout::top_down(Align::Center)),
                );
                self.render_io_strip(&mut child, true);
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
                    // Dynamic graph/panel split from ThemeLayout
                    let content_h = child.available_height();
                    let panel_content_h = self.estimate_panel_height();
                    let (graph_h, _panel_h) =
                        theme.layout.split_vertical(content_h, panel_content_h);

                    let selected_slot = child
                        .group(|ui| {
                            ui.set_max_height(graph_h);
                            ui.vertical_centered(|ui| {
                                // Update per-slot activity from output metering
                                let slot_count = self
                                    .graph_view
                                    .snarl
                                    .node_ids()
                                    .filter(|(_, n)| matches!(n, SonidoNode::Effect { .. }))
                                    .count();
                                self.graph_view.slot_activity =
                                    vec![self.metering.output_peak; slot_count];

                                self.graph_view.show(ui)
                            })
                            .inner
                        })
                        .inner;

                    // Feed the contextual Info View (drawn as a bottom panel
                    // before this central panel, so it reads this value next
                    // frame — an invisible one-frame lag).
                    self.info_slot = selected_slot;

                    // Auto-compile in-`show` topology edits (connect/disconnect,
                    // right-click add/remove), recording the pre-edit snapshot as
                    // an undo point. Undo/redo restores rebuild the snarl too, but
                    // `show()` clears the flag each frame before this check, so our
                    // own restores never re-enter here and corrupt the stacks.
                    self.apply_topology_change();

                    child.add_space(8.0);

                    // Effect panel for the selected node, in a scroll area so a
                    // tall panel (e.g. an 8-param reverb) scrolls within the
                    // leftover height instead of being clipped by the band below.
                    if let Some(slot_idx) = selected_slot {
                        let slot = SlotIndex(slot_idx);
                        if slot.0 < self.bridge.slot_count() {
                            egui::ScrollArea::vertical().show(&mut child, |ui| {
                                self.render_effect_panel(ui, slot);
                            });
                        }
                    } else {
                        Self::render_quick_reference(&mut child);
                    }
                }
            }

            // Output strip (hidden on Phone and in focus mode).
            if !hide_io {
                let mut child = ui.new_child(
                    UiBuilder::new()
                        .id_salt("output_col")
                        .max_rect(output_rect)
                        .layout(Layout::top_down(Align::Center)),
                );
                self.render_io_strip(&mut child, false);
            }

            // Touch-authoring overlays floated over the canvas (FAB + node action
            // bar + effect palette). Additive on desktop; the only add/edit path
            // on touch, where snarl's right-click menus are unreachable.
            self.render_graph_overlays(ctx, center_rect);

            // Delete / Backspace removes the selected effect node — no action-bar
            // click needed. Gated to graph mode, a selected effect, and not typing.
            if !typing
                && !self.single_effect
                && self.graph_view.selected_is_effect()
                && ctx.input(|i| {
                    i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace)
                })
                && let Some(node) = self.graph_view.selected_node
            {
                if self.undo_pending.is_none() {
                    self.undo_pending = Some(self.snapshot());
                }
                self.graph_view.remove_node(node);
            }

            // Recompile edits made *after* show() — the "+" FAB add, the node
            // action bar, and the Delete key above all land here and would
            // otherwise update only the visual graph, not the audio engine.
            self.apply_topology_change();

            // Advance parent cursor past all three columns
            ui.advance_cursor_after_rect(Rect::from_min_max(
                avail.min,
                pos2(
                    avail.min.x + io_width + gap + center_width + gap + io_width,
                    avail.max.y,
                ),
            ));
        });

        // Drain a pending "map param → macro" action raised by a knob's
        // right-click menu during this frame's panel render, then float the
        // macro mapping editor if one is open.
        if !self.single_effect {
            if let Some(action) = take_macro_action(ctx) {
                self.handle_macro_action(action);
            }
            self.render_macro_editor(ctx);
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.stop_audio();
    }
}
