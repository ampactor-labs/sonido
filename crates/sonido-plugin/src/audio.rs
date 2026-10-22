//! Audio processor for sonido CLAP plugins.
//!
//! Handles the real-time audio callback: reads parameter change events from
//! the host, updates the effect, and processes stereo audio buffers.

use crate::main_thread::SonidoMainThread;
use crate::shared::{GESTURE_BEGIN, GESTURE_END, SonidoShared};
use clack_extensions::params::PluginAudioProcessorParams;
use clack_extensions::tail::{PluginTailImpl, TailLength};
use clack_plugin::events::EventFlags;
use clack_plugin::events::event_types::{
    ParamGestureBeginEvent, ParamGestureEndEvent, ParamValueEvent, TransportEvent, TransportFlags,
};
use clack_plugin::prelude::*;
use clack_plugin::utils::Cookie;
use sonido_core::TempoContext;
use sonido_registry::{EffectRegistry, EffectWithParams};

/// Audio-thread processor wrapping a sonido effect.
///
/// Created during `activate()`, destroyed during `deactivate()`.
/// Owns the actual `Effect` instance and processes audio in real time.
pub struct SonidoAudioProcessor<'a> {
    shared: &'a SonidoShared,
    effect: Box<dyn EffectWithParams + Send>,
    /// Host tempo and transport, refreshed from each block's transport event.
    tempo: TempoContext,
}

impl<'a> PluginAudioProcessor<'a, SonidoShared, SonidoMainThread<'a>> for SonidoAudioProcessor<'a> {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut SonidoMainThread<'a>,
        shared: &'a SonidoShared,
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let sample_rate = audio_config.sample_rate as f32;
        let registry = EffectRegistry::new();
        let mut effect = registry
            .create(shared.effect_id(), sample_rate)
            .ok_or(PluginError::Message("Failed to create effect"))?;

        // Initialize effect parameters from shared atomic state.
        shared.apply_to_effect(effect.as_mut());

        // Cache latency so the main thread can report it to the host.
        shared.set_latency_samples(effect.latency_samples() as u32);
        shared.set_sample_rate(sample_rate);

        Ok(Self {
            shared,
            effect,
            tempo: TempoContext {
                sample_rate,
                ..TempoContext::default()
            },
        })
    }

    fn process(
        &mut self,
        process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        // Apply parameter changes from host automation events.
        self.handle_events(events.input);

        // Sync GUI-originated parameter changes back to the effect and host.
        self.sync_gui_changes(events.output);

        // Tempo-synced LFOs and delays follow the host transport.
        self.sync_transport(process.transport);

        // Bypass (GUI toggle) still runs the meters and scope, on dry audio.
        let bypassed = self.shared.is_bypassed();
        self.process_audio(&mut audio, bypassed)?;

        Ok(ProcessStatus::ContinueIfNotQuiet)
    }

    fn deactivate(self, _main_thread: &mut SonidoMainThread<'_>) {
        // Effect is dropped here, releasing all DSP state.
    }

    fn reset(&mut self) {
        self.effect.reset();
    }
}

/// The tempo context after this block's transport event, or `None` if
/// nothing changed.
///
/// Hosts send the transport with every block while it is known; without one
/// the last tempo stands and the transport counts as stopped.
pub(crate) fn next_tempo(
    current: &TempoContext,
    transport: Option<&TransportEvent>,
) -> Option<TempoContext> {
    let ctx = match transport {
        Some(t) => TempoContext {
            bpm: if t.flags.contains(TransportFlags::HAS_TEMPO) && t.tempo > 0.0 {
                t.tempo as f32
            } else {
                current.bpm
            },
            is_playing: t.flags.contains(TransportFlags::IS_PLAYING),
            beat_position: t.song_pos_beats.to_float() as f32,
            sample_rate: current.sample_rate,
        },
        None => TempoContext {
            is_playing: false,
            ..*current
        },
    };
    (ctx != *current).then_some(ctx)
}

/// Turn any channel pair into one buffer to process in place.
///
/// Separate input/output buffers get the input copied across first, an
/// output with no input is silenced, and an input with no output has nowhere
/// to go. After this every layout is processed, bypassed or metered the same
/// way.
pub(crate) fn in_place(pair: ChannelPair<'_, f32>) -> Option<&mut [f32]> {
    match pair {
        ChannelPair::InPlace(buf) => Some(buf),
        ChannelPair::InputOutput(input, output) => {
            let n = input.len().min(output.len());
            output[..n].copy_from_slice(&input[..n]);
            output[n..].fill(0.0);
            Some(output)
        }
        ChannelPair::OutputOnly(output) => {
            output.fill(0.0);
            Some(output)
        }
        ChannelPair::InputOnly(_) => None,
    }
}

impl SonidoAudioProcessor<'_> {
    /// Detect GUI-originated parameter changes and sync them to the effect
    /// and host.
    ///
    /// After `handle_events`, the local effect and shared state agree on the
    /// host's latest values. Any divergence between shared and effect means
    /// the GUI wrote a new value. We update the effect and emit a
    /// `ParamValueEvent` so the host updates its controls/automation.
    fn sync_gui_changes(&mut self, output: &mut OutputEvents) {
        for i in 0..self.shared.param_count() {
            // Read-only diagnostics flow the other way: the kernel writes them
            // and the GUI reads them. Writing them back would clobber the
            // kernel's value and announce a change to the host every block.
            if self.shared.is_read_only(i) {
                self.shared.set_value(i, self.effect.effect_get_param(i));
                continue;
            }

            let flags = self.shared.take_gesture_flags(i);
            let has_begin = flags & GESTURE_BEGIN != 0;
            let has_end = flags & GESTURE_END != 0;

            // 1. Begin gesture (before value) — groups edits into one undo entry.
            if has_begin && let Some(desc) = self.shared.descriptor(i) {
                let event = ParamGestureBeginEvent::new(0, ClapId::new(desc.id.0))
                    .with_flags(EventFlags::IS_LIVE);
                let _ = output.try_push(event);
            }

            // 2. Value change — sync GUI-written values to effect + host.
            let Some(shared_val) = self.shared.get_value(i) else {
                continue;
            };
            let effect_val = self.effect.effect_get_param(i);

            if shared_val.to_bits() != effect_val.to_bits() {
                self.effect.effect_set_param(i, shared_val);

                if let Some(desc) = self.shared.descriptor(i) {
                    let event = ParamValueEvent::new(
                        0,
                        ClapId::new(desc.id.0),
                        Pckn::match_all(),
                        f64::from(shared_val),
                        Cookie::empty(),
                    )
                    .with_flags(EventFlags::IS_LIVE);
                    let _ = output.try_push(event);
                }
            }

            // 3. End gesture (after value) — closes the undo group.
            if has_end && let Some(desc) = self.shared.descriptor(i) {
                let event = ParamGestureEndEvent::new(0, ClapId::new(desc.id.0))
                    .with_flags(EventFlags::IS_LIVE);
                let _ = output.try_push(event);
            }
        }
    }

    /// Handle incoming parameter change events from the host.
    ///
    /// Updates both the shared atomic state (so the main thread sees the
    /// latest values) and the effect's internal parameters directly.
    fn handle_events(&mut self, input: &InputEvents) {
        for event in input {
            if let Some(clack_plugin::events::spaces::CoreEventSpace::ParamValue(ev)) =
                event.as_core_event()
                && let Some(param_id) = ev.param_id()
            {
                let id = param_id.get();
                if let Some(index) = self.shared.index_by_id(id)
                    && !self.shared.is_read_only(index)
                {
                    let value = ev.value() as f32;
                    self.shared.set_value(index, value);
                    self.effect.effect_set_param(index, value);
                }
            }
        }
    }

    /// Refresh the effect's tempo context from the block's transport event.
    fn sync_transport(&mut self, transport: Option<&TransportEvent>) {
        if let Some(ctx) = next_tempo(&self.tempo, transport) {
            self.tempo = ctx;
            self.effect.set_tempo_context(&ctx);
        }
    }

    /// Process (or, bypassed, pass through) every audio port, feeding the
    /// GUI's meters and scope on the way.
    fn process_audio(&mut self, audio: &mut Audio, bypassed: bool) -> Result<(), PluginError> {
        let tap = self.shared.tap();
        for mut port_pair in audio {
            // Skip f64-only ports.
            let Some(mut channels) = port_pair.channels()?.into_f32() else {
                continue;
            };

            match channels.channel_pair_count() {
                0 => {}
                1 => {
                    if let Some(buf) = channels.channel_pair(0).and_then(in_place) {
                        tap.input(buf, buf);
                        if !bypassed {
                            self.effect.process_block_inplace(buf);
                        }
                        tap.output(buf, buf);
                    }
                }
                _ => {
                    let left = channels.channel_pair(0).and_then(in_place);
                    let right = channels.channel_pair(1).and_then(in_place);
                    if let (Some(left), Some(right)) = (left, right) {
                        let n = left.len().min(right.len());
                        let (left, right) = (&mut left[..n], &mut right[..n]);
                        tap.input(left, right);
                        if !bypassed {
                            self.effect.process_block_stereo_inplace(left, right);
                        }
                        tap.output(left, right);
                    }
                }
            }
        }

        Ok(())
    }
}

impl PluginAudioProcessorParams for SonidoAudioProcessor<'_> {
    fn flush(&mut self, input: &InputEvents, output: &mut OutputEvents) {
        self.handle_events(input);
        self.sync_gui_changes(output);
    }
}

// ── Tail Extension ──────────────────────────────────────────────────────────

/// Report the effect's ring-out to the host: how long it keeps producing
/// output after its input goes silent (reverb decay, delay repeats, a recorded
/// loop). Asked of the live effect, so it tracks the current decay settings.
impl PluginTailImpl for SonidoAudioProcessor<'_> {
    fn get(&self) -> TailLength {
        match u32::try_from(self.effect.tail_samples()) {
            Ok(samples) if samples < u32::MAX => TailLength::Finite(samples),
            _ => TailLength::Infinite,
        }
    }
}
