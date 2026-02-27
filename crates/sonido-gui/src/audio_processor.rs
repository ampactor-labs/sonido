//! Audio-thread processor and stream construction.
//!
//! This module separates audio-thread concerns from the GUI code in [`app`](super::app).
//! It contains:
//! - [`FilePlayback`] — in-memory file buffer with playback position tracking
//! - [`AudioProcessor`] — per-buffer DSP entry point (commands, param sync, effects, metering)
//! - [`build_audio_streams`] — factory function to create cpal input/output streams

use crate::atomic_param_bridge::AtomicParamBridge;
use crate::audio_bridge::{AtomicParam, MeteringData};
use crate::chain_manager::ChainCommand;
use crate::file_player::TransportCommand;
use crossbeam_channel::{Receiver, Sender};
use sonido_core::graph::NodeId;
use sonido_gui_core::{ChainMutator, ParamBridge, SlotIndex};
use sonido_io::GraphEngine;
use sonido_registry::EffectRegistry;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;
#[cfg(target_arch = "wasm32")]
use web_time::Instant;

/// File playback state owned by [`AudioProcessor`].
///
/// Manages in-memory audio buffers (left/right channels) and playback position.
/// Supports looping and file-mode switching from the GUI transport controls.
pub(crate) struct FilePlayback {
    left: Vec<f32>,
    right: Vec<f32>,
    position: usize,
    file_sample_rate: f32,
    playing: bool,
    looping: bool,
    file_mode: bool,
}

impl FilePlayback {
    pub(crate) fn new() -> Self {
        Self {
            left: Vec::new(),
            right: Vec::new(),
            position: 0,
            file_sample_rate: 48000.0,
            playing: false,
            looping: false,
            file_mode: false,
        }
    }

    /// Read the next stereo frame from the file buffer, advancing position.
    fn next_frame(&mut self) -> (f32, f32) {
        if self.left.is_empty() || !self.playing {
            return (0.0, 0.0);
        }
        if self.position >= self.left.len() {
            if self.looping {
                self.position = 0;
            } else {
                self.playing = false;
                self.position = 0;
                return (0.0, 0.0);
            }
        }
        let l = self.left[self.position];
        let r = self.right[self.position];
        self.position += 1;
        (l, r)
    }

    /// Current playback position in seconds.
    fn position_secs(&self) -> f32 {
        if self.file_sample_rate > 0.0 {
            self.position as f32 / self.file_sample_rate
        } else {
            0.0
        }
    }
}

/// All state needed by the audio output callback.
///
/// Constructed inside [`build_audio_streams`] and moved into the cpal output
/// closure. Encapsulates effect chain processing, file playback, parameter
/// sync, gain staging, metering, and buffer overrun detection.
///
/// The effect chain is backed by [`GraphEngine`], which manages a DAG of
/// effects. A `node_map` (`Vec<NodeId>`) maps bridge slot indices to graph
/// node IDs so the bridge's slot-indexed API aligns with the graph.
pub(crate) struct AudioProcessor {
    graph: GraphEngine,
    /// Maps bridge slot index → graph [`NodeId`].
    ///
    /// `node_map[slot_index]` returns the `NodeId` for that slot.
    /// Kept in sync with `ChainCommand::Add` / `Remove` operations.
    node_map: Vec<NodeId>,
    bridge: Arc<AtomicParamBridge>,
    /// Cached copy of the effect order; only refreshed when `bridge.order_is_dirty()`.
    cached_order: Vec<usize>,
    input_gain: Arc<AtomicParam>,
    master_volume: Arc<AtomicParam>,
    chain_bypass: Arc<AtomicBool>,
    bypass_fade: sonido_core::SmoothedParam,
    command_rx: Receiver<ChainCommand>,
    transport_rx: Receiver<TransportCommand>,
    metering_tx: Sender<MeteringData>,
    /// Receiver for mic input samples from the input stream.
    input_rx: Receiver<f32>,
    file_pb: FilePlayback,
    out_ch: usize,
    in_ch: usize,
    buffer_time_secs: f64,
}

impl AudioProcessor {
    /// Sync bridge parameter values into the graph's effect nodes.
    ///
    /// Called once per audio buffer. Reads atomic values from the bridge and
    /// pushes them into each effect via `effect_set_param()`. Also applies
    /// bypass states via `graph_mut().set_bypass()`.
    ///
    /// Params are pushed unconditionally; bridge atomic reads are wait-free and
    /// cheaper than tracking per-slot dirty flags across the borrow boundary.
    fn sync_bridge_to_graph(&mut self) {
        let slot_count = self.bridge.slot_count();
        for slot_raw in 0..slot_count {
            let slot = SlotIndex(slot_raw);
            let Some(&node_id) = self.node_map.get(slot_raw) else {
                continue;
            };

            // Sync bypass state
            let bridge_bypassed = self.bridge.is_bypassed(slot);
            let graph_bypassed = self.graph.graph().is_bypassed(node_id);
            if graph_bypassed != bridge_bypassed {
                self.graph.graph_mut().set_bypass(node_id, bridge_bypassed);
            }

            // Sync all parameters for this slot
            let param_count = self.bridge.param_count(slot);
            if let Some(effect) = self.graph.effect_with_params_mut(node_id) {
                for param_raw in 0..param_count {
                    let val = self
                        .bridge
                        .get(slot, sonido_gui_core::ParamIndex(param_raw));
                    effect.effect_set_param(param_raw, val);
                }
            }
        }
    }

    /// Process one output buffer: drain commands, sync params, run effects,
    /// apply gain, write interleaved output, and send metering.
    pub(crate) fn process_buffer(&mut self, data: &mut [f32]) {
        let process_start = Instant::now();

        // Drain transport commands
        while let Ok(cmd) = self.transport_rx.try_recv() {
            match cmd {
                TransportCommand::LoadFile {
                    left,
                    right,
                    sample_rate: sr,
                } => {
                    self.file_pb.left = left;
                    self.file_pb.right = right;
                    self.file_pb.file_sample_rate = sr;
                    self.file_pb.position = 0;
                    self.file_pb.playing = false;
                }
                TransportCommand::UnloadFile => {
                    self.file_pb.left.clear();
                    self.file_pb.right.clear();
                    self.file_pb.position = 0;
                    self.file_pb.playing = false;
                }
                TransportCommand::Play => self.file_pb.playing = true,
                TransportCommand::Pause => self.file_pb.playing = false,
                TransportCommand::Stop => {
                    self.file_pb.playing = false;
                    self.file_pb.position = 0;
                }
                TransportCommand::Seek(secs) => {
                    self.file_pb.position = (secs * self.file_pb.file_sample_rate) as usize;
                    if self.file_pb.position >= self.file_pb.left.len() {
                        self.file_pb.position = self.file_pb.left.len().saturating_sub(1);
                    }
                }
                TransportCommand::SetLoop(v) => self.file_pb.looping = v,
                TransportCommand::SetFileMode(v) => self.file_pb.file_mode = v,
            }
        }

        // Drain dynamic chain commands (transactional add/remove)
        while let Ok(cmd) = self.command_rx.try_recv() {
            match cmd {
                ChainCommand::Add {
                    id,
                    effect,
                    descriptors,
                } => {
                    let node_id = self.graph.add_effect(effect);
                    self.node_map.push(node_id);
                    self.bridge.add_slot(id, descriptors);
                    tracing::info!(effect_id = id, ?node_id, "effect added to graph");
                }
                ChainCommand::Remove { slot } => {
                    if let Some(node_id) = self.node_map.get(slot.0).copied() {
                        self.graph.remove_effect(node_id);
                        self.node_map.swap_remove(slot.0);
                        self.bridge.remove_slot(slot);
                        tracing::info!(slot = slot.0, ?node_id, "effect removed from graph");
                    }
                }
            }
        }

        // Global gain levels
        let ig = sonido_core::db_to_linear(self.input_gain.get());
        let mv = sonido_core::db_to_linear(self.master_volume.get());

        // Sync bridge -> graph effect parameters and bypass states
        self.sync_bridge_to_graph();

        // Sync effect order from GUI (only when changed)
        if self.bridge.order_is_dirty() {
            self.cached_order = self.bridge.get_order();
            // Map slot indices to NodeIds for the graph reorder call
            let node_order: Vec<NodeId> = self
                .cached_order
                .iter()
                .filter_map(|&slot| self.node_map.get(slot).copied())
                .collect();
            if node_order.len() == self.graph.effect_count() {
                self.graph.reorder(&node_order);
            }
            self.bridge.clear_order_dirty();
        }

        let frames = data.len() / self.out_ch;
        let file_mode = self.file_pb.file_mode;
        let has_file = !self.file_pb.left.is_empty();

        // Collect raw input samples for this buffer (deinterleaved, pre-gain)
        let mut raw_left = Vec::with_capacity(frames);
        let mut raw_right = Vec::with_capacity(frames);

        for _ in 0..frames {
            let (in_l_raw, in_r_raw) = if file_mode {
                for _ in 0..self.in_ch {
                    let _ = self.input_rx.try_recv();
                }
                if has_file {
                    self.file_pb.next_frame()
                } else {
                    (0.0, 0.0)
                }
            } else if self.in_ch >= 2 {
                let l = self.input_rx.try_recv().unwrap_or(0.0);
                let r = self.input_rx.try_recv().unwrap_or(0.0);
                for _ in 2..self.in_ch {
                    let _ = self.input_rx.try_recv();
                }
                (l, r)
            } else {
                let s = self.input_rx.try_recv().unwrap_or(0.0);
                (s, s)
            };

            let in_l = if in_l_raw.is_finite() { in_l_raw } else { 0.0 };
            let in_r = if in_r_raw.is_finite() { in_r_raw } else { 0.0 };

            raw_left.push(in_l * ig);
            raw_right.push(in_r * ig);
        }

        // Compute input metering (pre-chain)
        let mut input_peak = 0.0_f32;
        let mut input_rms_sum = 0.0_f32;
        for (&l, &r) in raw_left.iter().zip(raw_right.iter()) {
            let mono = (l + r) * 0.5;
            input_peak = input_peak.max(mono.abs());
            input_rms_sum += mono * mono;
        }

        // Determine global bypass crossfade target
        let bypass_target = if self.chain_bypass.load(Ordering::Relaxed) {
            0.0
        } else {
            1.0
        };
        self.bypass_fade.set_target(bypass_target);

        // Advance bypass fade per frame (block-level approximation using final value)
        // We need per-sample fade for accurate crossfade; advance once per frame.
        let mut wet_left = vec![0.0f32; frames];
        let mut wet_right = vec![0.0f32; frames];

        // Run the graph for the entire block
        self.graph
            .process_block_stereo(&raw_left, &raw_right, &mut wet_left, &mut wet_right);

        // Apply global bypass crossfade per sample and master volume, write output
        let mut output_peak = 0.0_f32;
        let mut output_rms_sum = 0.0_f32;

        for i in 0..frames {
            let dry_l = raw_left[i];
            let dry_r = raw_right[i];
            let fade = self.bypass_fade.advance();

            let (l, r) = if fade < 1e-6 {
                // Fully bypassed — pass dry signal
                (dry_l, dry_r)
            } else if (fade - 1.0).abs() < 1e-6 {
                // Fully active — pass wet signal
                (wet_left[i], wet_right[i])
            } else {
                // Mid-fade crossfade
                let dry_weight = 1.0 - fade;
                (
                    dry_l * dry_weight + wet_left[i] * fade,
                    dry_r * dry_weight + wet_right[i] * fade,
                )
            };

            let l = l * mv;
            let r = r * mv;

            let mono_out = (l + r) * 0.5;
            output_peak = output_peak.max(mono_out.abs());
            output_rms_sum += mono_out * mono_out;

            // Interleave output
            let idx = i * self.out_ch;
            match self.out_ch {
                1 => data[idx] = (l + r) * 0.5,
                2 => {
                    data[idx] = l;
                    data[idx + 1] = r;
                }
                _ => {
                    data[idx] = l;
                    data[idx + 1] = r;
                    for c in 2..self.out_ch {
                        data[idx + c] = 0.0;
                    }
                }
            }
        }

        // CPU usage measurement
        let elapsed = process_start.elapsed().as_secs_f64();
        let cpu_pct = (elapsed / self.buffer_time_secs * 100.0) as f32;

        // Send metering data (non-blocking)
        let count = frames.max(1) as f32;
        let _ = self.metering_tx.try_send(MeteringData {
            input_peak,
            input_rms: (input_rms_sum / count).sqrt(),
            output_peak,
            output_rms: (output_rms_sum / count).sqrt(),
            gain_reduction: 0.0,
            cpu_usage: cpu_pct,
            playback_position_secs: self.file_pb.position_secs(),
        });
    }
}

/// Build and start cpal audio streams.
///
/// Creates an output stream (always) and an input stream (if a mic is available).
/// Returns the stream handles — caller must keep them alive for audio to continue.
/// Input is optional so the app works without mic permission (e.g., wasm, headless).
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_audio_streams(
    bridge: Arc<AtomicParamBridge>,
    registry: &EffectRegistry,
    input_gain: Arc<AtomicParam>,
    master_volume: Arc<AtomicParam>,
    running: Arc<AtomicBool>,
    metering_tx: Sender<MeteringData>,
    command_rx: Receiver<ChainCommand>,
    transport_rx: Receiver<TransportCommand>,
    chain_bypass: Arc<AtomicBool>,
    error_count: Arc<AtomicU32>,
    sample_rate: f32,
    buffer_size: usize,
) -> Result<Vec<cpal::Stream>, String> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let output_device = host
        .default_output_device()
        .ok_or("No output device available")?;

    // Input device is optional (mic permission may be denied on wasm)
    let input_device = host.default_input_device();

    // Use device's actual sample rate; fall back to passed-in value on error
    let (output_channels, sample_rate) = match output_device.default_output_config() {
        Ok(config) => (config.channels(), config.sample_rate() as f32),
        Err(_) => (2, sample_rate),
    };

    let output_config = cpal::StreamConfig {
        channels: output_channels,
        sample_rate: sample_rate as u32,
        buffer_size: cpal::BufferSize::Fixed(buffer_size as u32),
    };

    // Build initial GraphEngine from the bridge's current ordered effect list.
    // The bridge knows which effects are in the chain and their bypass states.
    let effect_ids = bridge.ordered_static_ids();
    let bypass_states = bridge.ordered_bypass_states();

    let mut graph = GraphEngine::new_linear(sample_rate, buffer_size);
    let mut node_map: Vec<NodeId> = Vec::with_capacity(effect_ids.len());

    for (i, &id) in effect_ids.iter().enumerate() {
        if let Some(effect) = registry.create(id, sample_rate) {
            let node_id = graph.add_effect(effect);
            node_map.push(node_id);
            // Apply initial bypass state
            if bypass_states.get(i).copied().unwrap_or(false) {
                graph.graph_mut().set_bypass(node_id, true);
            }
        } else {
            tracing::warn!(id, "unknown effect id during graph init, skipping");
        }
    }

    // Stereo audio buffer (interleaved L, R pairs)
    let (tx, rx) = crossbeam_channel::bounded::<f32>(16384);

    let mut streams: Vec<cpal::Stream> = Vec::with_capacity(2);

    // Input stream (if mic available)
    let tx_fallback = tx.clone();
    let in_ch = if let Some(ref input_dev) = input_device {
        let input_channels = input_dev
            .default_input_config()
            .map(|c| c.channels())
            .unwrap_or(1);

        let input_config = cpal::StreamConfig {
            channels: input_channels,
            sample_rate: sample_rate as u32,
            buffer_size: cpal::BufferSize::Fixed(buffer_size as u32),
        };

        // Pre-fill with silence
        for _ in 0..(1024 * input_channels as usize) {
            let _ = tx.try_send(0.0);
        }

        let running_input = Arc::clone(&running);
        match input_dev
            .build_input_stream(
                &input_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    if !running_input.load(Ordering::Relaxed) {
                        return;
                    }
                    for &sample in data {
                        let _ = tx.try_send(sample);
                    }
                },
                {
                    let ec = Arc::clone(&error_count);
                    move |err| {
                        ec.fetch_add(1, Ordering::Relaxed);
                        tracing::error!(error = %err, "input stream error");
                    }
                },
                None,
            )
            .and_then(|stream| {
                stream
                    .play()
                    .map_err(|e| cpal::BuildStreamError::BackendSpecific {
                        err: cpal::BackendSpecificError {
                            description: e.to_string(),
                        },
                    })?;
                Ok(stream)
            }) {
            Ok(stream) => {
                streams.push(stream);
                input_channels as usize
            }
            Err(e) => {
                tracing::warn!(error = %e, "input stream unavailable, mic disabled");
                for _ in 0..2048 {
                    let _ = tx_fallback.try_send(0.0);
                }
                1
            }
        }
    } else {
        tracing::warn!("no input device available, mic disabled");
        // Pre-fill silence so output callback doesn't block
        for _ in 0..2048 {
            let _ = tx.try_send(0.0);
        }
        1 // default: mono input channel count for deinterleave logic
    };

    let running_output = Arc::clone(&running);
    let out_ch = output_channels as usize;
    let buffer_time_secs = buffer_size as f64 / sample_rate as f64;

    let mut processor = AudioProcessor {
        graph,
        node_map,
        bridge,
        cached_order: Vec::new(),
        input_gain,
        master_volume,
        chain_bypass,
        bypass_fade: sonido_core::SmoothedParam::fast(1.0, sample_rate),
        command_rx,
        transport_rx,
        metering_tx,
        input_rx: rx,
        file_pb: FilePlayback::new(),
        out_ch,
        in_ch,
        buffer_time_secs,
    };

    // Output stream -- delegates to AudioProcessor
    let output_stream = output_device
        .build_output_stream(
            &output_config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                if !running_output.load(Ordering::Relaxed) {
                    data.fill(0.0);
                    return;
                }
                processor.process_buffer(data);
            },
            {
                let ec = Arc::clone(&error_count);
                move |err| {
                    ec.fetch_add(1, Ordering::Relaxed);
                    tracing::error!(error = %err, "output stream error");
                }
            },
            None,
        )
        .map_err(|e| format!("Failed to build output stream: {}", e))?;

    output_stream
        .play()
        .map_err(|e| format!("Failed to play output stream: {}", e))?;
    streams.push(output_stream);

    tracing::info!(
        sample_rate = sample_rate as u32,
        buffer_size,
        channels = out_ch,
        "audio streams started"
    );

    Ok(streams)
}
