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
    ParamGestureBeginEvent, ParamGestureEndEvent, ParamValueEvent,
};
use clack_plugin::prelude::*;
use clack_plugin::utils::Cookie;
use sonido_registry::{EffectRegistry, EffectWithParams};

/// Audio-thread processor wrapping a sonido effect.
///
/// Created during `activate()`, destroyed during `deactivate()`.
/// Owns the actual `Effect` instance and processes audio in real time.
pub struct SonidoAudioProcessor<'a> {
    shared: &'a SonidoShared,
    effect: Box<dyn EffectWithParams + Send>,
}

impl<'a> PluginAudioProcessor<'a, SonidoShared, SonidoMainThread<'a>> for SonidoAudioProcessor<'a> {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut SonidoMainThread<'a>,
        shared: &'a SonidoShared,
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let registry = EffectRegistry::new();
        let mut effect = registry
            .create(shared.effect_id(), audio_config.sample_rate as f32)
            .ok_or(PluginError::Message("Failed to create effect"))?;

        // Initialize effect parameters from shared atomic state.
        shared.apply_to_effect(effect.as_mut());

        // Cache latency so the main thread can report it to the host.
        shared.set_latency_samples(effect.latency_samples() as u32);

        Ok(Self { shared, effect })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        // Apply parameter changes from host automation events.
        self.handle_events(events.input);

        // Sync GUI-originated parameter changes back to the effect and host.
        self.sync_gui_changes(events.output);

        // When bypassed via GUI toggle, pass audio through unprocessed.
        if self.shared.is_bypassed() {
            return Ok(ProcessStatus::ContinueIfNotQuiet);
        }

        // Process audio through the effect.
        self.process_audio(&mut audio)?;

        Ok(ProcessStatus::ContinueIfNotQuiet)
    }

    fn deactivate(self, _main_thread: &mut SonidoMainThread<'_>) {
        // Effect is dropped here, releasing all DSP state.
    }

    fn reset(&mut self) {
        self.effect.reset();
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
                if let Some(index) = self.shared.index_by_id(id) {
                    let value = ev.value() as f32;
                    self.shared.set_value(index, value);
                    self.effect.effect_set_param(index, value);
                }
            }
        }
    }

    /// Process stereo audio through the effect.
    fn process_audio(&mut self, audio: &mut Audio) -> Result<(), PluginError> {
        for mut port_pair in audio {
            let channels = port_pair.channels()?;

            // Extract f32 channels, skip f64-only ports.
            let Some(mut channels) = channels.into_f32() else {
                continue;
            };

            let pair_count = channels.channel_pair_count();
            match pair_count {
                0 => {}
                1 => {
                    // Mono: process single channel.
                    if let Some(pair) = channels.channel_pair(0) {
                        self.process_channel_pair(pair);
                    }
                }
                _ => {
                    // Stereo: get L and R channel pairs.
                    let left = channels.channel_pair(0);
                    let right = channels.channel_pair(1);

                    match (left, right) {
                        (
                            Some(ChannelPair::InputOutput(left_in, left_out)),
                            Some(ChannelPair::InputOutput(right_in, right_out)),
                        ) => {
                            self.effect
                                .process_block_stereo(left_in, right_in, left_out, right_out);
                        }
                        (Some(ChannelPair::InPlace(left)), Some(ChannelPair::InPlace(right))) => {
                            self.effect.process_block_stereo_inplace(left, right);
                        }
                        _ => {
                            // Asymmetric layout — process channels independently.
                            if let Some(pair) = channels.channel_pair(0) {
                                self.process_channel_pair(pair);
                            }
                            if let Some(pair) = channels.channel_pair(1) {
                                self.process_channel_pair(pair);
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Process a single channel pair (mono fallback).
    fn process_channel_pair(&mut self, pair: ChannelPair<f32>) {
        match pair {
            ChannelPair::InputOutput(input, output) => {
                self.effect.process_block(input, output);
            }
            ChannelPair::InPlace(buf) => {
                self.effect.process_block_inplace(buf);
            }
            _ => {}
        }
    }
}

impl PluginAudioProcessorParams for SonidoAudioProcessor<'_> {
    fn flush(&mut self, input: &InputEvents, output: &mut OutputEvents) {
        self.handle_events(input);
        self.sync_gui_changes(output);
    }
}

// ── Tail Extension ──────────────────────────────────────────────────────────

/// Report the effect's tail length to the host.
///
/// The tail is the number of samples the effect continues to produce
/// meaningful output after its input goes silent (e.g., reverb decay,
/// delay repeats). We use the effect's reported `latency_samples` as a
/// conservative lower bound; effects with significant tails (reverb, delay)
/// will provide a non-zero value via this path.
impl PluginTailImpl for SonidoAudioProcessor<'_> {
    fn get(&self) -> TailLength {
        let samples = self.shared.latency_samples();
        if samples == 0 {
            TailLength::Finite(0)
        } else {
            TailLength::Finite(samples)
        }
    }
}
