//! Audio processor for the multi-effect chain plugin.
//!
//! [`ChainAudioProcessor`] drives a [`GraphEngine`] for audio routing,
//! drains structural commands from [`ChainShared`], and processes stereo
//! audio blocks via the compiled DAG schedule.
//!
//! # Design
//!
//! Effects are stored inside the [`GraphEngine`] as
//! `Box<dyn EffectWithParams + Send>`, preserving both the
//! [`Effect`](sonido_core::Effect) and
//! [`ParameterInfo`](sonido_core::ParameterInfo) vtables. Engine slots are
//! dense positions (`0..n`); plugin slots are a fixed pool (`0..MAX_SLOTS`).
//! `slot_map[engine_slot]` maps to the plugin slot index, bridging the two
//! namespaces.
//!
//! # Slot Mapping
//!
//! Plugin slots are sparse (0..MAX_SLOTS=16); engine slots are dense (0..n).
//! `slot_map: Vec<usize>` grows as effects are added and shrinks as they are
//! removed. `slot_map[engine_slot] = plugin_slot`.
//!
//! On every topology change (add/remove/reorder) the slot_map is updated and
//! `GraphEngine` handles the linear chain topology internally.
//!
//! # Parameter Sync
//!
//! The shared param array is the source of truth. On every `process()` call,
//! `sync_gui_changes()` diffs the shared values against `param_cache` (last
//! values written to the effects). Divergences cause:
//!
//! 1. A CLAP `ParamGestureBeginEvent` if a begin-gesture flag is pending.
//! 2. A CLAP `ParamValueEvent` so the DAW updates its automation display.
//! 3. A call to `set_param_at()` to apply the value.
//! 4. A CLAP `ParamGestureEndEvent` if an end-gesture flag is pending.

use crate::chain::main_thread::ChainMainThread;
use crate::chain::shared::{ChainCommand, ChainShared, GESTURE_BEGIN, GESTURE_END, SlotSnapshot};
use crate::chain::{ClapParamId, MAX_SLOTS, SLOT_STRIDE, TOTAL_PARAMS};
use clack_extensions::params::PluginAudioProcessorParams;
use clack_plugin::events::EventFlags;
use clack_plugin::events::event_types::{
    ParamGestureBeginEvent, ParamGestureEndEvent, ParamValueEvent,
};
use clack_plugin::prelude::*;
use clack_plugin::utils::Cookie;
use sonido_core::graph::GraphEngine;
use sonido_registry::EffectRegistry;

/// Audio-thread processor for the chain plugin.
///
/// Owns all active `EffectWithParams` instances (inside the engine) and
/// processes stereo audio in real time. Structural commands (add/remove/reorder)
/// arrive via [`ChainShared::try_drain_commands`] and are applied synchronously
/// at the start of each block.
///
/// ## Slot Mapping
///
/// Plugin slots are sparse (0..MAX_SLOTS); engine slots are dense (0..n).
/// `slot_map[engine_slot]` returns the plugin slot index. This allows stable
/// `ClapParamId` addresses even as effects are added and removed.
///
/// ## Graph Topology
///
/// ```text
/// Input → Effect[order[0]] → Effect[order[1]] → … → Effect[order[n]] → Output
/// ```
///
/// The Input and Output nodes are permanent inside `GraphEngine`; only the
/// effect slots are mutated on structural commands.
pub struct ChainAudioProcessor<'a> {
    shared: &'a ChainShared,
    /// DAG routing engine — owns all effect instances.
    engine: GraphEngine,
    /// Maps engine slot position → plugin slot index.
    ///
    /// Plugin slots are sparse (0..MAX_SLOTS); engine slots are dense (0..n).
    /// `slot_map[engine_slot]` returns the plugin slot index.
    slot_map: Vec<usize>,
    /// Registry used to instantiate new effects on `ChainCommand::Add`.
    registry: EffectRegistry,
    sample_rate: f32,
    #[allow(dead_code)]
    block_size: usize,
    /// Current processing order as an ordered list of occupied plugin slot indices.
    cached_order: Vec<usize>,
    /// Last param values written to the effects.
    ///
    /// Sized `TOTAL_PARAMS`. Compared against shared on every block to detect
    /// GUI-originated changes without reading back from the effects.
    param_cache: Vec<f32>,
    /// Left-channel scratch buffer for in-place audio processing.
    left_buf: Vec<f32>,
    /// Right-channel scratch buffer for in-place audio processing.
    right_buf: Vec<f32>,
    /// Additional scratch buffer used when mono processing needs a separate
    /// right-channel output sink (discarded after processing).
    mono_right_out: Vec<f32>,
}

impl<'a> PluginAudioProcessor<'a, ChainShared, ChainMainThread<'a>> for ChainAudioProcessor<'a> {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut ChainMainThread<'a>,
        shared: &'a ChainShared,
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let block_size = audio_config.max_frames_count as usize;
        let sample_rate = audio_config.sample_rate as f32;

        Ok(Self {
            shared,
            engine: GraphEngine::new_linear(sample_rate, block_size),
            slot_map: Vec::new(),
            registry: EffectRegistry::new(),
            sample_rate,
            block_size,
            cached_order: Vec::new(),
            param_cache: vec![0.0_f32; TOTAL_PARAMS],
            left_buf: vec![0.0_f32; block_size],
            right_buf: vec![0.0_f32; block_size],
            mono_right_out: vec![0.0_f32; block_size],
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        // Drain structural commands first (may change which effects are active).
        self.drain_commands();

        // Apply host-automation parameter events.
        self.handle_events(events.input);

        // Sync GUI-originated parameter changes to effects and notify host.
        self.sync_gui_changes(events.output);

        // Route audio through the compiled graph.
        self.process_audio(&mut audio)?;

        Ok(ProcessStatus::ContinueIfNotQuiet)
    }

    fn deactivate(self, _main_thread: &mut ChainMainThread<'_>) {
        // Engine (and all effects) are dropped here, releasing all DSP state.
    }

    fn reset(&mut self) {
        self.engine.reset();
    }
}

impl ChainAudioProcessor<'_> {
    // ── Structural command handling ────────────────────────────────────────

    /// Drain and apply all pending structural commands.
    fn drain_commands(&mut self) {
        let Some(commands) = self.shared.try_drain_commands() else {
            return;
        };

        for cmd in commands {
            match cmd {
                ChainCommand::Add { effect_id } => self.handle_add(&effect_id),
                ChainCommand::Remove { slot } => self.handle_remove(slot),
                ChainCommand::Reorder { new_order } => self.handle_reorder(new_order),
                ChainCommand::Restore {
                    slot,
                    params,
                    bypassed,
                } => {
                    if let Some(engine_slot) = self.slot_map.iter().position(|&ps| ps == slot) {
                        for (i, &val) in params.iter().enumerate() {
                            self.engine.set_param_at(engine_slot, i, val);
                            if let Some(id) = ClapParamId::new(slot, i) {
                                self.shared.set_value(id, val);
                                self.param_cache[id.raw() as usize] = val;
                            }
                        }
                        self.engine.set_bypass_at(engine_slot, bypassed);
                        self.shared.set_bypassed(slot, bypassed);
                    }
                }
            }
        }
    }

    /// Add a new effect to the first available plugin slot.
    fn handle_add(&mut self, effect_id: &str) {
        // Find first vacant plugin slot.
        let Some(plugin_slot) = (0..MAX_SLOTS).find(|s| !self.slot_map.contains(s)) else {
            tracing::warn!("ChainAudioProcessor: no vacant slot for '{effect_id}' (chain full)");
            return;
        };

        // Look up the static effect ID from the registry descriptor.
        let Some(desc) = self.registry.descriptor(effect_id) else {
            tracing::warn!("ChainAudioProcessor: unknown effect id '{effect_id}'");
            return;
        };
        let static_id: &'static str = desc.id;

        let Some(effect) = self.registry.create(effect_id, self.sample_rate) else {
            tracing::warn!("ChainAudioProcessor: failed to create '{effect_id}'");
            return;
        };

        // Collect descriptors before moving the effect.
        let descriptors: Vec<_> = (0..effect.effect_param_count())
            .filter_map(|i| effect.effect_param_info(i))
            .collect();

        // Initialize shared param values to effect defaults.
        self.shared.init_slot_defaults(plugin_slot, &descriptors);

        // Sync param_cache so the first process() call doesn't emit spurious events.
        for (param_idx, desc) in descriptors.iter().enumerate() {
            if let Some(id) = ClapParamId::new(plugin_slot, param_idx) {
                self.param_cache[id.raw() as usize] = desc.default;
            }
        }

        tracing::info!("ChainAudioProcessor: adding '{effect_id}' in slot {plugin_slot}");
        self.engine.add_effect_named(effect, static_id);
        self.slot_map.push(plugin_slot);

        // Rebuild order: all occupied slots in current engine order.
        self.rebuild_order_from_slots();

        // Publish updated slot metadata to main thread.
        self.publish_slots();

        self.shared.set_needs_rescan();
        self.shared.request_callback();
    }

    /// Remove the effect at `plugin_slot`, clearing all associated state.
    fn handle_remove(&mut self, plugin_slot: usize) {
        let Some(engine_slot) = self.slot_map.iter().position(|&ps| ps == plugin_slot) else {
            tracing::warn!("ChainAudioProcessor: remove on vacant/invalid slot {plugin_slot}");
            return;
        };

        tracing::info!("ChainAudioProcessor: removing slot {plugin_slot}");

        self.engine.remove_at(engine_slot);
        self.slot_map.remove(engine_slot);
        self.shared.clear_slot_values(plugin_slot);

        // Clear param_cache for this slot.
        for param_idx in 0..SLOT_STRIDE {
            if let Some(id) = ClapParamId::new(plugin_slot, param_idx) {
                self.param_cache[id.raw() as usize] = 0.0;
            }
        }

        self.rebuild_order_from_slots();
        self.publish_slots();

        self.shared.set_needs_rescan();
        self.shared.request_callback();
    }

    /// Reorder the chain to the provided plugin slot sequence.
    fn handle_reorder(&mut self, new_order: Vec<usize>) {
        // Validate: only include plugin slots present in slot_map.
        let valid_order: Vec<usize> = new_order
            .into_iter()
            .filter(|s| self.slot_map.contains(s))
            .collect();

        tracing::info!("ChainAudioProcessor: reorder → {valid_order:?}");
        self.shared.store_order(valid_order.clone());
        self.cached_order = valid_order.clone();

        // Translate plugin slot order to engine slot order.
        let engine_order: Vec<usize> = valid_order
            .iter()
            .filter_map(|ps| self.slot_map.iter().position(|&s| s == *ps))
            .collect();

        if engine_order.len() == self.engine.slot_count() {
            self.engine.reorder_slots(&engine_order);
            // Rebuild slot_map to match new engine order.
            self.slot_map = valid_order;
        }
    }

    /// Rebuild `cached_order` from the current slot_map order.
    fn rebuild_order_from_slots(&mut self) {
        self.cached_order = self.slot_map.clone();
        self.shared.store_order(self.cached_order.clone());
    }

    /// Publish a fresh `Vec<SlotSnapshot>` to the main thread.
    fn publish_slots(&self) {
        let slots: Vec<SlotSnapshot> = (0..MAX_SLOTS)
            .map(|plugin_slot| {
                if let Some(engine_slot) = self.slot_map.iter().position(|&ps| ps == plugin_slot) {
                    if let Some(effect) = self.engine.effect_at(engine_slot) {
                        let descriptors: Vec<_> = (0..effect.effect_param_count())
                            .filter_map(|i| effect.effect_param_info(i))
                            .collect();
                        let id = self.engine.effect_id_at(engine_slot).unwrap_or("");
                        SlotSnapshot::occupied(id, descriptors)
                    } else {
                        SlotSnapshot::empty()
                    }
                } else {
                    SlotSnapshot::empty()
                }
            })
            .collect();
        self.shared.store_slots(slots);
    }

    // ── Host parameter event handling ─────────────────────────────────────

    /// Apply incoming host-automation `ParamValue` events.
    ///
    /// Updates both the shared atomic state and the effect's internal
    /// parameter so that shared and effect stay in agreement.
    fn handle_events(&mut self, input: &InputEvents) {
        for event in input {
            let Some(clack_plugin::events::spaces::CoreEventSpace::ParamValue(ev)) =
                event.as_core_event()
            else {
                continue;
            };
            let Some(param_id) = ev.param_id() else {
                continue;
            };
            let raw = param_id.get();
            let Some(clap_id) = ClapParamId::from_raw(raw) else {
                continue;
            };

            let plugin_slot = clap_id.slot();
            let local_idx = clap_id.param();
            let value = ev.value() as f32;

            // Write to shared so main thread sees latest host value.
            self.shared.set_value(clap_id, value);

            // Apply directly to effect via engine slot lookup.
            if let Some(engine_slot) = self.slot_map.iter().position(|&ps| ps == plugin_slot) {
                self.engine.set_param_at(engine_slot, local_idx, value);
            }

            // Update cache — host change is authoritative, no need to re-emit.
            self.param_cache[raw as usize] = value;
        }
    }

    // ── GUI → effect → host param sync ───────────────────────────────────

    /// Detect GUI-originated parameter changes and forward them to the effect
    /// and host.
    ///
    /// For each occupied slot and each of its parameters, compares the shared
    /// atomic value against `param_cache` (the last value written to the
    /// effect). Divergences trigger a CLAP `ParamValueEvent` so the DAW can
    /// update automation lanes, and the new value is applied to the effect.
    fn sync_gui_changes(&mut self, output: &mut OutputEvents) {
        let order = self.cached_order.clone();
        for &plugin_slot in &order {
            if !self.shared.take_slot_dirty(plugin_slot) {
                continue;
            }

            let Some(engine_slot) = self.slot_map.iter().position(|&ps| ps == plugin_slot) else {
                continue;
            };

            let param_count = self.engine.param_count_at(engine_slot);

            for local_idx in 0..param_count {
                let Some(clap_id) = ClapParamId::new(plugin_slot, local_idx) else {
                    continue;
                };
                let flat = clap_id.raw() as usize;

                // Atomically read and clear gesture flags.
                let flags = self.shared.take_gesture_flags(flat);
                let has_begin = flags & GESTURE_BEGIN != 0;
                let has_end = flags & GESTURE_END != 0;

                // 1. Gesture begin — opens DAW undo group before value change.
                if has_begin {
                    let ev = ParamGestureBeginEvent::new(0, ClapId::new(clap_id.raw()))
                        .with_flags(EventFlags::IS_LIVE);
                    let _ = output.try_push(ev);
                }

                // 2. Value sync — detect GUI write and push to effect + host.
                let shared_val = self.shared.get_value_raw(flat);
                let cached_val = self.param_cache[flat];

                if shared_val.to_bits() != cached_val.to_bits() {
                    self.engine.set_param_at(engine_slot, local_idx, shared_val);
                    self.param_cache[flat] = shared_val;

                    let ev = ParamValueEvent::new(
                        0,
                        ClapId::new(clap_id.raw()),
                        Pckn::match_all(),
                        f64::from(shared_val),
                        Cookie::empty(),
                    )
                    .with_flags(EventFlags::IS_LIVE);
                    let _ = output.try_push(ev);
                }

                // 3. Gesture end — closes DAW undo group after value change.
                if has_end {
                    let ev = ParamGestureEndEvent::new(0, ClapId::new(clap_id.raw()))
                        .with_flags(EventFlags::IS_LIVE);
                    let _ = output.try_push(ev);
                }
            }
        }
    }

    // ── Audio processing ──────────────────────────────────────────────────

    /// Route stereo audio through the compiled graph.
    fn process_audio(&mut self, audio: &mut Audio) -> Result<(), PluginError> {
        for mut port_pair in audio {
            let channels = port_pair.channels()?;
            let Some(mut channels) = channels.into_f32() else {
                continue;
            };

            match channels.channel_pair_count() {
                0 => {}
                1 => {
                    if let Some(pair) = channels.channel_pair(0) {
                        self.process_channel_pair_mono(pair);
                    }
                }
                _ => {
                    let left = channels.channel_pair(0);
                    let right = channels.channel_pair(1);

                    match (left, right) {
                        (
                            Some(ChannelPair::InputOutput(left_in, left_out)),
                            Some(ChannelPair::InputOutput(right_in, right_out)),
                        ) => {
                            self.process_stereo_separate(left_in, right_in, left_out, right_out);
                        }
                        (Some(ChannelPair::InPlace(left)), Some(ChannelPair::InPlace(right))) => {
                            self.process_stereo_inplace(left, right);
                        }
                        _ => {
                            // Asymmetric layout — fall back to independent mono processing.
                            if let Some(pair) = channels.channel_pair(0) {
                                self.process_channel_pair_mono(pair);
                            }
                            if let Some(pair) = channels.channel_pair(1) {
                                self.process_channel_pair_mono(pair);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Process separate input/output stereo buffers through the engine.
    fn process_stereo_separate(
        &mut self,
        left_in: &[f32],
        right_in: &[f32],
        left_out: &mut [f32],
        right_out: &mut [f32],
    ) {
        let len = left_in.len().min(left_out.len());
        if len == 0 {
            return;
        }

        self.engine
            .process_block_stereo(&left_in[..len], &right_in[..len], left_out, right_out);
    }

    /// Process in-place stereo buffers through the engine.
    fn process_stereo_inplace(&mut self, left: &mut [f32], right: &mut [f32]) {
        let len = left.len().min(right.len());
        if len == 0 {
            return;
        }

        self.left_buf[..len].copy_from_slice(&left[..len]);
        self.right_buf[..len].copy_from_slice(&right[..len]);

        self.engine.process_block_stereo(
            &self.left_buf[..len],
            &self.right_buf[..len],
            left,
            right,
        );
    }

    /// Process a single mono channel pair through the engine.
    ///
    /// Duplicates the mono signal to both channels (stereo-symmetric
    /// processing) and takes the left output as the mono result. The right
    /// output is discarded into `mono_right_out`.
    fn process_channel_pair_mono(&mut self, pair: ChannelPair<f32>) {
        match pair {
            ChannelPair::InputOutput(input, output) => {
                let len = input.len().min(output.len());
                if len == 0 {
                    return;
                }
                // Duplicate mono input into both scratch buffers.
                self.left_buf[..len].copy_from_slice(&input[..len]);
                self.right_buf[..len].copy_from_slice(&input[..len]);
                // Output: left → `output`, right → `mono_right_out` (discarded).
                self.engine.process_block_stereo(
                    &self.left_buf[..len],
                    &self.right_buf[..len],
                    output,
                    &mut self.mono_right_out[..len],
                );
            }
            ChannelPair::InPlace(buf) => {
                let len = buf.len();
                if len == 0 {
                    return;
                }
                // Copy mono input into left and right scratch buffers.
                self.left_buf[..len].copy_from_slice(buf as &[f32]);
                self.right_buf[..len].copy_from_slice(buf as &[f32]);
                // Output: left → `buf` (in-place), right → `mono_right_out` (discarded).
                self.engine.process_block_stereo(
                    &self.left_buf[..len],
                    &self.right_buf[..len],
                    buf,
                    &mut self.mono_right_out[..len],
                );
            }
            _ => {}
        }
    }
}

impl PluginAudioProcessorParams for ChainAudioProcessor<'_> {
    /// Handle parameter flush (called when audio is not running).
    ///
    /// Applies host-automation events and syncs any pending GUI changes.
    fn flush(&mut self, input: &InputEvents, output: &mut OutputEvents) {
        self.handle_events(input);
        self.sync_gui_changes(output);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_shared() -> ChainShared {
        ChainShared::new(None, None)
    }

    fn make_proc(shared: &ChainShared) -> ChainAudioProcessor<'_> {
        let block_size = 256_usize;
        let sample_rate = 48000.0_f32;

        ChainAudioProcessor {
            shared,
            engine: GraphEngine::new_linear(sample_rate, block_size),
            slot_map: Vec::new(),
            registry: EffectRegistry::new(),
            sample_rate,
            block_size,
            cached_order: Vec::new(),
            param_cache: vec![0.0_f32; TOTAL_PARAMS],
            left_buf: vec![0.0_f32; block_size],
            right_buf: vec![0.0_f32; block_size],
            mono_right_out: vec![0.0_f32; block_size],
        }
    }

    #[test]
    fn empty_chain_has_no_active_slots() {
        let shared = make_shared();
        assert_eq!(shared.active_slot_count(), 0);
        assert_eq!(shared.load_order().len(), 0);
    }

    #[test]
    fn add_command_populates_slot() {
        let shared = make_shared();
        shared.push_command(ChainCommand::Add {
            effect_id: "distortion".to_owned(),
        });

        let mut proc = make_proc(&shared);
        proc.drain_commands();

        assert!(!proc.slot_map.is_empty());
        assert_eq!(proc.cached_order, vec![0]);
        assert_eq!(shared.active_slot_count(), 1);
    }

    #[test]
    fn remove_command_clears_slot() {
        let shared = make_shared();
        shared.push_command(ChainCommand::Add {
            effect_id: "reverb".to_owned(),
        });
        shared.push_command(ChainCommand::Remove { slot: 0 });

        let mut proc = make_proc(&shared);
        proc.drain_commands();

        assert!(proc.slot_map.is_empty());
        assert!(proc.cached_order.is_empty());
        assert_eq!(shared.active_slot_count(), 0);
    }

    #[test]
    fn reorder_command_updates_order() {
        let shared = make_shared();
        shared.push_command(ChainCommand::Add {
            effect_id: "distortion".to_owned(),
        });
        shared.push_command(ChainCommand::Add {
            effect_id: "reverb".to_owned(),
        });
        shared.push_command(ChainCommand::Reorder {
            new_order: vec![1, 0],
        });

        let mut proc = make_proc(&shared);
        proc.drain_commands();

        assert_eq!(proc.cached_order, vec![1, 0]);
    }
}
