//! Audio-thread processor and stream construction.
//!
//! This module separates audio-thread concerns from the GUI code in [`app`](super::app).
//! It contains:
//! - [`FilePlayback`] — in-memory file buffer with playback position tracking
//! - [`AudioProcessor`] — per-buffer DSP entry point (commands, param sync, effects, metering)
//! - [`build_audio_streams`] — factory function to create the cpal output stream
//!
//! Audio input is sourced from either the built-in [`SignalGenerator`] or file
//! playback — there is no microphone input stream.

use crate::atomic_param_bridge::AtomicParamBridge;
use crate::audio_bridge::{AtomicParam, MeteringData};
use crate::chain_manager::GraphCommand;
use crate::file_player::TransportCommand;
use crate::signal_generator::{SignalGenerator, SourceMode};
use crossbeam_channel::{Receiver, Sender};
use sonido_core::graph::GraphEngine;
use sonido_gui_core::{ParamBridge, SlotIndex};
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
/// effects via slot-indexed methods that keep bridge and graph in sync.
pub(crate) struct AudioProcessor {
    graph: GraphEngine,
    bridge: Arc<AtomicParamBridge>,
    input_gain: Arc<AtomicParam>,
    master_volume: Arc<AtomicParam>,
    chain_bypass: Arc<AtomicBool>,
    bypass_fade: sonido_core::SmoothedParam,
    command_rx: Receiver<GraphCommand>,
    transport_rx: Receiver<TransportCommand>,
    metering_tx: Sender<MeteringData>,
    file_pb: FilePlayback,
    /// Built-in signal generator (sine, sweep, noise, etc.).
    signal_gen: SignalGenerator,
    /// Active audio source mode (generator or file).
    source_mode: SourceMode,
    out_ch: usize,
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
        let slot_count = self.bridge.slot_count().min(self.graph.slot_count());
        for slot_raw in 0..slot_count {
            let slot = SlotIndex(slot_raw);

            // Sync bypass state
            let bridge_bypassed = self.bridge.is_bypassed(slot);
            if self.graph.is_bypassed_at(slot_raw) != bridge_bypassed {
                self.graph.set_bypass_at(slot_raw, bridge_bypassed);
            }

            // Sync all parameters for this slot
            let param_count = self.bridge.param_count(slot);
            for param_raw in 0..param_count {
                let val = self
                    .bridge
                    .get(slot, sonido_gui_core::ParamIndex(param_raw));
                self.graph.set_param_at(slot_raw, param_raw, val);
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
                TransportCommand::Play => match self.source_mode {
                    SourceMode::Generator => self.signal_gen.set_playing(true),
                    SourceMode::File => self.file_pb.playing = true,
                },
                TransportCommand::Pause => match self.source_mode {
                    SourceMode::Generator => self.signal_gen.set_playing(false),
                    SourceMode::File => self.file_pb.playing = false,
                },
                TransportCommand::Stop => match self.source_mode {
                    SourceMode::Generator => self.signal_gen.stop(),
                    SourceMode::File => {
                        self.file_pb.playing = false;
                        self.file_pb.position = 0;
                    }
                },
                TransportCommand::Seek(secs) => {
                    self.file_pb.position = (secs * self.file_pb.file_sample_rate) as usize;
                    if self.file_pb.position >= self.file_pb.left.len() {
                        self.file_pb.position = self.file_pb.left.len().saturating_sub(1);
                    }
                }
                TransportCommand::SetLoop(v) => self.file_pb.looping = v,
                TransportCommand::SetSourceMode(mode) => self.source_mode = mode,
                TransportCommand::SetSignalType(t) => self.signal_gen.set_signal_type(t),
                TransportCommand::SetGeneratorFreq(hz) => self.signal_gen.set_frequency(hz),
                TransportCommand::SetGeneratorAmplitude(amp) => {
                    self.signal_gen.set_amplitude(amp);
                }
                TransportCommand::SetSweepParams {
                    start_hz,
                    end_hz,
                    duration_secs,
                    looping,
                } => {
                    self.signal_gen
                        .set_sweep_params(start_hz, end_hz, duration_secs, looping);
                }
                TransportCommand::SetImpulseRate(hz) => self.signal_gen.set_impulse_rate(hz),
            }
        }

        // Drain dynamic chain commands (transactional add/remove)
        while let Ok(cmd) = self.command_rx.try_recv() {
            match cmd {
                GraphCommand::Add {
                    id,
                    effect,
                    descriptors,
                } => {
                    let slot = self.graph.add_effect_named(effect, id);
                    self.bridge.add_slot(id, descriptors);
                    tracing::info!(effect_id = id, slot, "effect added to graph");
                }
                GraphCommand::Remove { slot } => {
                    if self.graph.remove_at(slot.0).is_some() {
                        self.bridge.remove_slot(slot);
                        tracing::info!(slot = slot.0, "effect removed from graph");
                    }
                }
                GraphCommand::ReplaceTopology {
                    engine,
                    effect_ids,
                    slot_descriptors,
                } => {
                    self.bridge
                        .rebuild_from_manifest(&effect_ids, &slot_descriptors);
                    self.graph = *engine;
                    tracing::info!(
                        effects = effect_ids.len(),
                        "topology replaced via ReplaceTopology"
                    );
                }
            }
        }

        // Global gain levels
        let ig = sonido_core::db_to_linear(self.input_gain.get());
        let mv = sonido_core::db_to_linear(self.master_volume.get());

        // Sync bridge -> graph effect parameters and bypass states
        self.sync_bridge_to_graph();

        let frames = data.len() / self.out_ch;

        // Collect raw input samples for this buffer (deinterleaved, pre-gain)
        let mut raw_left = vec![0.0f32; frames];
        let mut raw_right = vec![0.0f32; frames];

        let has_file = !self.file_pb.left.is_empty();
        match self.source_mode {
            SourceMode::Generator => {
                self.signal_gen.generate(&mut raw_left, &mut raw_right);
                for (l, r) in raw_left.iter_mut().zip(raw_right.iter_mut()) {
                    *l *= ig;
                    *r *= ig;
                }
            }
            SourceMode::File => {
                for i in 0..frames {
                    let (in_l_raw, in_r_raw) = if has_file {
                        self.file_pb.next_frame()
                    } else {
                        (0.0, 0.0)
                    };
                    let in_l = if in_l_raw.is_finite() { in_l_raw } else { 0.0 };
                    let in_r = if in_r_raw.is_finite() { in_r_raw } else { 0.0 };
                    raw_left[i] = in_l * ig;
                    raw_right[i] = in_r * ig;
                }
            }
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

/// Actual audio configuration negotiated with the device.
pub(crate) struct AudioStreamConfig {
    /// Stream handles — must stay alive for audio to continue.
    pub streams: Vec<cpal::Stream>,
    /// Sample rate negotiated with the output device.
    pub sample_rate: f32,
    /// Buffer size requested (device may differ per callback).
    pub buffer_size: usize,
}

/// Build and start the cpal output stream.
///
/// Returns the stream handle — caller must keep it alive for audio to continue.
/// Audio input comes from the built-in signal generator or file playback,
/// not from a microphone.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_audio_streams(
    bridge: Arc<AtomicParamBridge>,
    registry: &EffectRegistry,
    input_gain: Arc<AtomicParam>,
    master_volume: Arc<AtomicParam>,
    running: Arc<AtomicBool>,
    metering_tx: Sender<MeteringData>,
    command_rx: Receiver<GraphCommand>,
    transport_rx: Receiver<TransportCommand>,
    chain_bypass: Arc<AtomicBool>,
    error_count: Arc<AtomicU32>,
    sample_rate: f32,
    buffer_size: usize,
) -> Result<AudioStreamConfig, String> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let output_device = host
        .default_output_device()
        .ok_or("No output device available")?;

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

    for (i, &id) in effect_ids.iter().enumerate() {
        if let Some(effect) = registry.create(id, sample_rate) {
            let slot = graph.add_effect_named(effect, id);
            if bypass_states.get(i).copied().unwrap_or(false) {
                graph.set_bypass_at(slot, true);
            }
        } else {
            tracing::warn!(id, "unknown effect id during graph init, skipping");
        }
    }

    let mut streams: Vec<cpal::Stream> = Vec::with_capacity(1);

    let running_output = Arc::clone(&running);
    let out_ch = output_channels as usize;
    let buffer_time_secs = buffer_size as f64 / sample_rate as f64;

    let mut processor = AudioProcessor {
        graph,
        bridge,
        input_gain,
        master_volume,
        chain_bypass,
        bypass_fade: sonido_core::SmoothedParam::fast(1.0, sample_rate),
        command_rx,
        transport_rx,
        metering_tx,
        file_pb: FilePlayback::new(),
        signal_gen: SignalGenerator::new(sample_rate),
        source_mode: SourceMode::Generator,
        out_ch,
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

    Ok(AudioStreamConfig {
        streams,
        sample_rate,
        buffer_size,
    })
}
