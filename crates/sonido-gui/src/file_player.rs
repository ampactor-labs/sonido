//! WAV file player for testing effects without a microphone.
//!
//! [`FilePlayer`] manages file loading, transport controls, and UI rendering.
//! Audio data is sent to the audio thread via [`TransportCommand`]s through
//! a crossbeam channel. Playback position flows back through `MeteringData`.
//!
//! On native, uses synchronous `rfd::FileDialog`. On wasm, uses
//! `rfd::AsyncFileDialog` with bytes-based WAV parsing via `hound`.

use crossbeam_channel::Sender;
use egui::Ui;
use sonido_gui_core::theme::SonidoTheme;
#[cfg(not(target_arch = "wasm32"))]
use sonido_io::{read_wav_info, read_wav_stereo};
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;

/// Maximum total samples (all channels) allowed when loading WAV on wasm.
///
/// 60 seconds of stereo audio at 48 kHz = 5 760 000 samples (~23 MB of f32).
/// Beyond this the browser's linear memory is at risk of exhaustion.
#[cfg(target_arch = "wasm32")]
const MAX_WASM_SAMPLES: u32 = 48_000 * 60 * 2;

/// Commands sent from GUI thread to audio thread for file playback.
pub enum TransportCommand {
    /// Load a stereo file into the audio thread's playback buffer.
    LoadFile {
        /// Left channel samples.
        left: Vec<f32>,
        /// Right channel samples.
        right: Vec<f32>,
        /// File sample rate in Hz.
        sample_rate: f32,
    },
    /// Remove the loaded file.
    UnloadFile,
    /// Start or resume playback.
    Play,
    /// Pause playback (retains position).
    Pause,
    /// Stop playback and reset position to zero.
    Stop,
    /// Seek to a position in seconds.
    Seek(f32),
    /// Enable or disable loop mode.
    SetLoop(bool),
    /// Switch between file input (`true`) and mic input (`false`).
    SetFileMode(bool),
}

/// GUI-side file player state and controls.
///
/// Renders transport buttons, a position scrubber, and file info.
/// Communicates with the audio thread exclusively through commands.
#[allow(clippy::struct_excessive_bools)]
pub struct FilePlayer {
    transport_tx: Sender<TransportCommand>,
    file_name: String,
    #[cfg(not(target_arch = "wasm32"))]
    file_path: Option<PathBuf>,
    duration_secs: f32,
    position_secs: f32,
    is_playing: bool,
    is_looping: bool,
    use_file_input: bool,
    sample_rate: f32,
    has_file: bool,
    /// Receives file path from background file dialog (native only).
    #[cfg(not(target_arch = "wasm32"))]
    native_file_rx: crossbeam_channel::Receiver<PathBuf>,
    #[cfg(not(target_arch = "wasm32"))]
    native_file_tx: Sender<PathBuf>,
    /// Receives file bytes loaded by async dialog (wasm only).
    #[cfg(target_arch = "wasm32")]
    file_result_rx: crossbeam_channel::Receiver<(String, Vec<u8>)>,
    #[cfg(target_arch = "wasm32")]
    file_result_tx: Sender<(String, Vec<u8>)>,
}

impl FilePlayer {
    /// Create a new file player with the given transport command sender.
    pub fn new(transport_tx: Sender<TransportCommand>) -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let (native_file_tx, native_file_rx) = crossbeam_channel::unbounded();
        #[cfg(target_arch = "wasm32")]
        let (file_result_tx, file_result_rx) = crossbeam_channel::unbounded();

        Self {
            transport_tx,
            file_name: String::new(),
            #[cfg(not(target_arch = "wasm32"))]
            file_path: None,
            duration_secs: 0.0,
            position_secs: 0.0,
            is_playing: false,
            is_looping: true,
            use_file_input: true,
            sample_rate: 48000.0,
            has_file: false,
            #[cfg(not(target_arch = "wasm32"))]
            native_file_rx,
            #[cfg(not(target_arch = "wasm32"))]
            native_file_tx,
            #[cfg(target_arch = "wasm32")]
            file_result_rx,
            #[cfg(target_arch = "wasm32")]
            file_result_tx,
        }
    }

    /// Update playback position from metering data (called each frame).
    pub fn set_position(&mut self, position_secs: f32) {
        self.position_secs = position_secs;
        // Detect playback stop (position reset by audio thread)
        if self.is_playing && position_secs <= 0.0 && self.duration_secs > 0.0 && !self.is_looping {
            self.is_playing = false;
        }
    }

    /// Whether file input mode is active.
    pub fn use_file_input(&self) -> bool {
        self.use_file_input
    }

    /// Whether a file is currently loaded.
    pub fn has_file(&self) -> bool {
        self.has_file
    }

    /// Toggle between play and pause states.
    ///
    /// If playing, sends [`TransportCommand::Pause`]. If paused, sends
    /// [`TransportCommand::Play`]. No-op if no file is loaded.
    pub fn toggle_play_pause(&mut self) {
        if !self.has_file {
            return;
        }
        if self.is_playing {
            self.is_playing = false;
            let _ = self.transport_tx.send(TransportCommand::Pause);
        } else {
            self.is_playing = true;
            let _ = self.transport_tx.send(TransportCommand::Play);
        }
    }

    /// Re-send current file_mode and file data to the audio thread.
    ///
    /// Called after audio stream restart (buffer size change, preset load)
    /// because the `AudioProcessor` is recreated with a fresh `FilePlayback`.
    pub fn resync_transport(&mut self) {
        // Always sync the current file_mode
        let _ = self
            .transport_tx
            .send(TransportCommand::SetFileMode(self.use_file_input));

        // Re-send file data and restore playback state
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(ref path) = self.file_path {
            if self.has_file {
                if let Ok((samples, _spec)) = read_wav_stereo(path) {
                    let _ = self.transport_tx.send(TransportCommand::LoadFile {
                        left: samples.left,
                        right: samples.right,
                        sample_rate: self.sample_rate,
                    });
                    let _ = self
                        .transport_tx
                        .send(TransportCommand::SetLoop(self.is_looping));
                    if self.position_secs > 0.0 {
                        let _ = self
                            .transport_tx
                            .send(TransportCommand::Seek(self.position_secs));
                    }
                    if self.is_playing {
                        let _ = self.transport_tx.send(TransportCommand::Play);
                    }
                }
            }
        }
    }

    /// Load a WAV file from disk (native only).
    #[cfg(not(target_arch = "wasm32"))]
    fn load_file(&mut self, path: PathBuf) {
        // Read metadata first for duration
        let info = match read_wav_info(&path) {
            Ok(info) => info,
            Err(e) => {
                tracing::error!("Failed to read WAV info: {e}");
                return;
            }
        };

        // Load stereo samples
        let (samples, _spec) = match read_wav_stereo(&path) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Failed to load WAV: {e}");
                return;
            }
        };

        self.file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.duration_secs = info.duration_secs as f32;
        self.sample_rate = info.sample_rate as f32;
        self.position_secs = 0.0;
        self.is_playing = false;
        self.file_path = Some(path);
        self.has_file = true;

        let _ = self.transport_tx.send(TransportCommand::LoadFile {
            left: samples.left,
            right: samples.right,
            sample_rate: self.sample_rate,
        });
    }

    /// Load a WAV file from raw bytes (wasm).
    ///
    /// Files exceeding [`MAX_WASM_SAMPLES`] are rejected to prevent
    /// out-of-memory crashes in the browser's linear memory.
    #[cfg(target_arch = "wasm32")]
    fn load_file_from_bytes(&mut self, name: String, bytes: Vec<u8>) {
        use std::io::Cursor;

        let reader = match hound::WavReader::new(Cursor::new(&bytes)) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Failed to parse WAV: {e}");
                return;
            }
        };

        let spec = reader.spec();
        let sample_rate = spec.sample_rate as f32;
        let channels = spec.channels as usize;

        // Guard against oversized files that would exhaust wasm linear memory.
        // len() returns total samples across all channels.
        let total_samples = reader.len();
        if total_samples > MAX_WASM_SAMPLES {
            let secs = total_samples / u32::from(spec.channels) / spec.sample_rate;
            tracing::error!(
                "WAV too large for wasm: {total_samples} samples ({secs}s). Max is 60s stereo."
            );
            return;
        }

        // Read all samples as f32
        let raw_samples: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Float => reader
                .into_samples::<f32>()
                .filter_map(|s| s.ok())
                .collect(),
            hound::SampleFormat::Int => {
                let bits = spec.bits_per_sample;
                let max_val = (1u32 << (bits - 1)) as f32;
                reader
                    .into_samples::<i32>()
                    .filter_map(|s| s.ok())
                    .map(|s| s as f32 / max_val)
                    .collect()
            }
        };

        // Deinterleave to stereo
        let frames = raw_samples.len() / channels.max(1);
        let mut left = Vec::with_capacity(frames);
        let mut right = Vec::with_capacity(frames);

        for frame in raw_samples.chunks(channels.max(1)) {
            left.push(frame[0]);
            right.push(if channels >= 2 { frame[1] } else { frame[0] });
        }

        self.duration_secs = frames as f32 / sample_rate;
        self.sample_rate = sample_rate;
        self.file_name = name;
        self.position_secs = 0.0;
        self.is_playing = false;
        self.has_file = true;

        let _ = self.transport_tx.send(TransportCommand::LoadFile {
            left,
            right,
            sample_rate,
        });
    }

    /// Render the input source toggle (Mic / File) for the header bar.
    pub fn render_source_toggle(&mut self, ui: &mut Ui) {
        let label = if self.use_file_input { "File" } else { "Mic" };
        if ui
            .selectable_label(
                self.use_file_input,
                egui::RichText::new(label).small().strong(),
            )
            .clicked()
        {
            self.use_file_input = !self.use_file_input;
            let _ = self
                .transport_tx
                .send(TransportCommand::SetFileMode(self.use_file_input));

            // If switching away from file mode, stop playback
            if !self.use_file_input {
                let _ = self.transport_tx.send(TransportCommand::Stop);
                self.is_playing = false;
                self.position_secs = 0.0;
            }
        }
    }

    /// Render the file player panel (bottom bar).
    pub fn ui(&mut self, ui: &mut Ui) {
        // Check for completed file dialog (native — spawned on background thread)
        #[cfg(not(target_arch = "wasm32"))]
        if let Ok(path) = self.native_file_rx.try_recv() {
            self.load_file(path);
        }

        // Check for completed async file loads (wasm)
        #[cfg(target_arch = "wasm32")]
        if let Ok((name, bytes)) = self.file_result_rx.try_recv() {
            self.load_file_from_bytes(name, bytes);
        }

        ui.horizontal(|ui| {
            // Browse button — spawned on background thread to avoid blocking GUI
            #[cfg(not(target_arch = "wasm32"))]
            if ui.button("Open").clicked() {
                let tx = self.native_file_tx.clone();
                std::thread::spawn(move || {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("WAV", &["wav"])
                        .pick_file()
                    {
                        let _ = tx.send(path);
                    }
                });
            }

            #[cfg(target_arch = "wasm32")]
            if ui.button("Open").clicked() {
                let tx = self.file_result_tx.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    if let Some(file) = rfd::AsyncFileDialog::new()
                        .add_filter("WAV", &["wav"])
                        .pick_file()
                        .await
                    {
                        let name = file.file_name();
                        let bytes = file.read().await;
                        let _ = tx.send((name, bytes));
                    }
                });
            }

            // File name display
            if self.file_name.is_empty() {
                ui.label(
                    egui::RichText::new("No file loaded")
                        .color(SonidoTheme::get(ui.ctx()).colors.text_secondary)
                        .italics(),
                );
            } else {
                ui.label(
                    egui::RichText::new(&self.file_name).color(SonidoTheme::get(ui.ctx()).colors.text_primary),
                );
            }

            ui.add_space(12.0);

            // Transport controls (only if file loaded)
            if self.has_file {
                // Play / Pause
                let play_label = if self.is_playing { "||" } else { ">" };
                if ui.button(play_label).clicked() {
                    if self.is_playing {
                        self.is_playing = false;
                        let _ = self.transport_tx.send(TransportCommand::Pause);
                    } else {
                        self.is_playing = true;
                        let _ = self.transport_tx.send(TransportCommand::Play);
                    }
                }

                // Stop
                if ui.button("[]").clicked() {
                    self.is_playing = false;
                    self.position_secs = 0.0;
                    let _ = self.transport_tx.send(TransportCommand::Stop);
                }

                // Loop toggle
                let theme = SonidoTheme::get(ui.ctx());
                let loop_color = if self.is_looping {
                    theme.colors.green
                } else {
                    theme.colors.dim
                };
                if ui
                    .button(egui::RichText::new("L").color(loop_color))
                    .clicked()
                {
                    self.is_looping = !self.is_looping;
                    let _ = self
                        .transport_tx
                        .send(TransportCommand::SetLoop(self.is_looping));
                }

                ui.add_space(8.0);

                // Position scrubber
                let mut pos = self.position_secs;
                let slider = egui::Slider::new(&mut pos, 0.0..=self.duration_secs)
                    .show_value(false)
                    .trailing_fill(true);
                let response = ui.add_sized([200.0, 18.0], slider);
                if response.changed() {
                    let _ = self.transport_tx.send(TransportCommand::Seek(pos));
                    self.position_secs = pos;
                }

                // Time display
                ui.label(
                    egui::RichText::new(format!(
                        "{} / {}",
                        format_time(self.position_secs),
                        format_time(self.duration_secs),
                    ))
                    .color(SonidoTheme::get(ui.ctx()).colors.text_secondary)
                    .small(),
                );
            }
        });

        // Handle drag-and-drop (native only — wasm drag-and-drop doesn't provide paths)
        #[cfg(not(target_arch = "wasm32"))]
        {
            ui.ctx().input(|i| {
                if let Some(dropped) = i.raw.dropped_files.first()
                    && let Some(path) = &dropped.path
                    && path
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
                {
                    self.file_path = Some(path.clone());
                }
            });

            // Deferred drag-drop load: check if file_path changed without a loaded file_name match
            if let Some(ref path) = self.file_path {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if name != self.file_name && !name.is_empty() {
                    let path = path.clone();
                    self.load_file(path);
                }
            }
        }
    }
}

/// Format seconds as `M:SS`.
fn format_time(secs: f32) -> String {
    let total = secs.max(0.0) as u32;
    let m = total / 60;
    let s = total % 60;
    format!("{m}:{s:02}")
}
