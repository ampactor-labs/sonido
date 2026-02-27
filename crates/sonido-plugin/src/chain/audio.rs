//! Audio processor for the multi-effect chain plugin.
//!
//! [`ChainAudioProcessor`] drives a [`ProcessingGraph`] for audio routing,
//! drains structural commands from [`ChainShared`], and processes stereo
//! audio blocks via the compiled DAG schedule.
//!
//! # Design
//!
//! Effects are stored inside the [`ProcessingGraph`] as
//! `Box<dyn EffectWithParams + Send>`, preserving both the
//! [`Effect`](sonido_core::Effect) and
//! [`ParameterInfo`](sonido_core::ParameterInfo) vtables. Slot indices are mapped to graph [`NodeId`]s
//! via `slot_node_ids`. On every topology change (add/remove/reorder) the
//! linear chain `Input → E[0] → … → E[n] → Output` is rebuilt and
//! recompiled.

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
use sonido_core::graph::{NodeId, ProcessingGraph};
use sonido_registry::EffectRegistry;

/// Audio-thread processor for the chain plugin.
///
/// Owns all active `EffectWithParams` instances (inside the graph) and
/// processes stereo audio in real time. Structural commands (add/remove/reorder)
/// arrive via [`ChainShared::try_drain_commands`] and are applied synchronously
/// at the start of each block.
///
/// ## Graph Topology
///
/// ```text
/// Input → Effect[order[0]] → Effect[order[1]] → … → Effect[order[n]] → Output
/// ```
///
/// The Input and Output nodes are permanent; only the effect edges are rebuilt
/// on each structural mutation.
///
/// ## Parameter Sync
///
/// The shared param array is the source of truth. On every `process()` call,
/// `sync_gui_changes()` diffs the shared values against `param_cache` (last
/// values written to the effects). Divergences cause:
///
/// 1. A CLAP `ParamGestureBeginEvent` if a begin-gesture flag is pending.
/// 2. A CLAP `ParamValueEvent` so the DAW updates its automation display.
/// 3. A call to `effect_set_param()` to apply the value.
/// 4. A CLAP `ParamGestureEndEvent` if an end-gesture flag is pending.
pub struct ChainAudioProcessor<'a> {
    shared: &'a ChainShared,
    /// DAG routing engine — owns all effect instances.
    graph: ProcessingGraph,
    /// Maps slot index → [`NodeId`] in the graph.
    ///
    /// `None` entries are vacant slots. Active slots have `Some(NodeId)`.
    slot_node_ids: [Option<NodeId>; MAX_SLOTS],
    /// Static effect IDs per slot, tracked alongside `slot_node_ids`.
    ///
    /// Used when publishing `SlotSnapshot`s to the main thread. The `&'static str`
    /// comes from `EffectDescriptor::id` via the registry.
    slot_ids: [Option<&'static str>; MAX_SLOTS],
    /// Permanent Input node in the graph.
    input_node: NodeId,
    /// Permanent Output node in the graph.
    output_node: NodeId,
    /// Registry used to instantiate new effects on `ChainCommand::Add`.
    registry: EffectRegistry,
    sample_rate: f32,
    #[allow(dead_code)]
    block_size: usize,
    /// Current processing order as an ordered list of occupied slot indices.
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

        let mut graph = ProcessingGraph::new(sample_rate, block_size);
        let input_node = graph.add_input();
        let output_node = graph.add_output();
        // Compile the empty (passthrough-via-direct) chain. Input→Output directly.
        graph
            .connect(input_node, output_node)
            .expect("initial Input→Output connection must succeed");
        graph.compile().expect("initial empty graph must compile");

        Ok(Self {
            shared,
            graph,
            slot_node_ids: [None; MAX_SLOTS],
            slot_ids: [None; MAX_SLOTS],
            input_node,
            output_node,
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
        // Graph (and all effects) are dropped here, releasing all DSP state.
    }

    fn reset(&mut self) {
        for slot in 0..MAX_SLOTS {
            if let Some(node_id) = self.slot_node_ids[slot]
                && let Some(effect) = self.graph.effect_with_params_mut(node_id)
            {
                effect.reset();
            }
        }
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
            }
        }
    }

    /// Add a new effect to the first available slot.
    fn handle_add(&mut self, effect_id: &str) {
        // Find first vacant slot.
        let Some(slot) = self.slot_node_ids.iter().position(|s| s.is_none()) else {
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
        self.shared.init_slot_defaults(slot, &descriptors);

        // Sync param_cache so the first process() call doesn't emit spurious events.
        for (param_idx, desc) in descriptors.iter().enumerate() {
            if let Some(id) = ClapParamId::new(slot, param_idx) {
                self.param_cache[id.raw() as usize] = desc.default;
            }
        }

        tracing::info!("ChainAudioProcessor: adding '{effect_id}' in slot {slot}");
        let node_id = self.graph.add_effect(effect);
        self.slot_node_ids[slot] = Some(node_id);
        self.slot_ids[slot] = Some(static_id);

        // Rebuild order: all occupied slots in index order.
        self.rebuild_order_from_slots();
        // Reconnect graph in new order and recompile.
        self.reconnect_and_compile();

        // Publish updated slot metadata to main thread.
        self.publish_slots();

        self.shared.set_needs_rescan();
        self.shared.request_callback();
    }

    /// Remove the effect at `slot`, clearing all associated state.
    fn handle_remove(&mut self, slot: usize) {
        let Some(node_id) = self.slot_node_ids.get(slot).copied().flatten() else {
            tracing::warn!("ChainAudioProcessor: remove on vacant/invalid slot {slot}");
            return;
        };

        tracing::info!("ChainAudioProcessor: removing slot {slot}");

        // Remove the node (and all its edges) from the graph.
        if let Err(e) = self.graph.remove_node(node_id) {
            tracing::warn!("ChainAudioProcessor: remove_node failed: {e:?}");
        }
        self.slot_node_ids[slot] = None;
        self.slot_ids[slot] = None;
        self.shared.clear_slot_values(slot);

        // Clear param_cache for this slot.
        for param_idx in 0..SLOT_STRIDE {
            if let Some(id) = ClapParamId::new(slot, param_idx) {
                self.param_cache[id.raw() as usize] = 0.0;
            }
        }

        self.rebuild_order_from_slots();
        self.reconnect_and_compile();
        self.publish_slots();

        self.shared.set_needs_rescan();
        self.shared.request_callback();
    }

    /// Reorder the chain to the provided slot sequence.
    fn handle_reorder(&mut self, new_order: Vec<usize>) {
        // Validate: only include occupied slots, reject out-of-range indices.
        let valid_order: Vec<usize> = new_order
            .into_iter()
            .filter(|&s| s < MAX_SLOTS && self.slot_node_ids[s].is_some())
            .collect();

        tracing::info!("ChainAudioProcessor: reorder → {valid_order:?}");
        self.shared.store_order(valid_order.clone());
        self.cached_order = valid_order;

        // Reconnect graph in new order and recompile.
        self.reconnect_and_compile();
    }

    /// Rebuild `cached_order` from the occupied slots in index order.
    fn rebuild_order_from_slots(&mut self) {
        self.cached_order = self
            .slot_node_ids
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.map(|_| i))
            .collect();
        self.shared.store_order(self.cached_order.clone());
    }

    /// Disconnect all inter-effect edges and reconnect in `cached_order`, then recompile.
    ///
    /// Topology after reconnect:
    /// `Input → Effect[order[0]] → Effect[order[1]] → … → Effect[order[n]] → Output`
    fn reconnect_and_compile(&mut self) {
        // Disconnect everything from input_node and output_node, and between effects.
        // Strategy: collect all edges from/to nodes in the chain and disconnect them.
        let all_nodes: Vec<NodeId> = core::iter::once(self.input_node)
            .chain(
                self.cached_order
                    .iter()
                    .filter_map(|&s| self.slot_node_ids[s]),
            )
            .chain(core::iter::once(self.output_node))
            .collect();

        // Disconnect all adjacent pairs in current topology.
        // We try all pairs in `all_nodes` since order may have changed.
        // collect edge IDs first to avoid mutating while iterating.
        let mut edges_to_remove = Vec::new();
        for i in 0..all_nodes.len() {
            for j in 0..all_nodes.len() {
                if i != j
                    && let Some(edge_id) = self.graph.find_edge(all_nodes[i], all_nodes[j])
                {
                    edges_to_remove.push(edge_id);
                }
            }
        }
        for edge_id in edges_to_remove {
            let _ = self.graph.disconnect(edge_id);
        }

        // Build the linear chain: Input → E[0] → … → E[n] → Output.
        let chain_nodes: Vec<NodeId> = core::iter::once(self.input_node)
            .chain(
                self.cached_order
                    .iter()
                    .filter_map(|&s| self.slot_node_ids[s]),
            )
            .chain(core::iter::once(self.output_node))
            .collect();

        for window in chain_nodes.windows(2) {
            if let [from, to] = *window
                && let Err(e) = self.graph.connect(from, to)
            {
                tracing::warn!("ChainAudioProcessor: connect failed: {e:?}");
            }
        }

        if let Err(e) = self.graph.compile() {
            tracing::warn!("ChainAudioProcessor: compile failed: {e:?}");
        }
    }

    /// Publish a fresh `Vec<SlotSnapshot>` to the main thread.
    fn publish_slots(&self) {
        let slots: Vec<SlotSnapshot> = (0..MAX_SLOTS)
            .map(|slot| {
                if let Some(node_id) = self.slot_node_ids[slot] {
                    if let Some(effect) = self.graph.effect_with_params_ref(node_id) {
                        let descriptors: Vec<_> = (0..effect.effect_param_count())
                            .filter_map(|i| effect.effect_param_info(i))
                            .collect();
                        let id = self.slot_ids[slot].unwrap_or("");
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

            let slot = clap_id.slot();
            let local_idx = clap_id.param();
            let value = ev.value() as f32;

            // Write to shared so main thread sees latest host value.
            self.shared.set_value(clap_id, value);

            // Apply directly to effect via graph.
            if let Some(node_id) = self.slot_node_ids.get(slot).copied().flatten()
                && let Some(effect) = self.graph.effect_with_params_mut(node_id)
            {
                effect.effect_set_param(local_idx, value);
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
        for &slot in &self.cached_order.clone() {
            let Some(node_id) = self.slot_node_ids[slot] else {
                continue;
            };

            let param_count = {
                let Some(effect) = self.graph.effect_with_params_ref(node_id) else {
                    continue;
                };
                effect.effect_param_count()
            };

            for local_idx in 0..param_count {
                let Some(clap_id) = ClapParamId::new(slot, local_idx) else {
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
                    if let Some(effect) = self.graph.effect_with_params_mut(node_id) {
                        effect.effect_set_param(local_idx, shared_val);
                    }
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

    /// Process separate input/output stereo buffers through the graph.
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

        self.graph
            .process_block(&left_in[..len], &right_in[..len], left_out, right_out);
    }

    /// Process in-place stereo buffers through the graph.
    fn process_stereo_inplace(&mut self, left: &mut [f32], right: &mut [f32]) {
        let len = left.len().min(right.len());
        if len == 0 {
            return;
        }

        self.left_buf[..len].copy_from_slice(&left[..len]);
        self.right_buf[..len].copy_from_slice(&right[..len]);

        self.graph
            .process_block(&self.left_buf[..len], &self.right_buf[..len], left, right);
    }

    /// Process a single mono channel pair through the graph.
    ///
    /// Duplicates the mono signal to both graph channels (stereo-symmetric
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
                self.graph.process_block(
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
                self.graph.process_block(
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
        let mut graph = ProcessingGraph::new(sample_rate, block_size);
        let input_node = graph.add_input();
        let output_node = graph.add_output();
        graph
            .connect(input_node, output_node)
            .expect("test graph init");
        graph.compile().expect("test graph compile");

        ChainAudioProcessor {
            shared,
            graph,
            slot_node_ids: [None; MAX_SLOTS],
            slot_ids: [None; MAX_SLOTS],
            input_node,
            output_node,
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

        assert!(proc.slot_node_ids[0].is_some());
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

        assert!(proc.slot_node_ids[0].is_none());
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
