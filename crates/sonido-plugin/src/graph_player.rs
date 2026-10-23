//! Graph-player CLAP plugin — runs any exported [`Patch`] as a single plugin.
//!
//! Where the per-effect plugins each wrap one effect, this wraps a whole rig: a
//! [`PatchPlayer`] driven by six macro params, a morph position/speed, input and
//! master gain, and a bypass. Its state *is* the patch JSON the Studio GUI
//! exports — so "Export as CLAP" is the same blob.
//!
//! The pedal plays rigs through this same runtime: `sonido_pedal.rs` is a
//! `PatchPlayer` of effects built by the same registry, so a rig's macros,
//! morph and gains behave the same in the DAW and on the pedal (and the same
//! [`Patch`] travels between them, as JSON here and as a 4 KB sector there).
//!
//! Its editor is the design system's cabinet in the RIG family: the loaded
//! rig's chain on the screen, a rig browser, the six macros, morph and gains
//! (see [`crate::graph_editor`]). Choosing a rig asks the host to restart the
//! plugin, which rebuilds the player off the audio thread.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPortsImpl,
};
use clack_extensions::latency::PluginLatencyImpl;
use clack_extensions::params::{
    ParamDisplayWriter, ParamInfo, ParamInfoWriter, PluginAudioProcessorParams,
    PluginMainThreadParams,
};
use clack_extensions::state::PluginStateImpl;
use clack_extensions::tail::{PluginTailImpl, TailLength};
use clack_plugin::events::EventFlags;
use clack_plugin::events::event_types::{
    ParamGestureBeginEvent, ParamGestureEndEvent, ParamValueEvent,
};
use clack_plugin::prelude::*;
use clack_plugin::stream::{InputStream, OutputStream};
use clack_plugin::utils::Cookie;

use sonido_core::{ParamDescriptor, ParamFlags, ParamId, ParamUnit, TempoContext};
use sonido_gui_core::telemetry::SignalTap;
use sonido_patch::controls::RIG_TOGGLES;
use sonido_patch::{NUM_MACROS, NUM_TOGGLES, Patch, PatchPlayer};
use sonido_registry::{EffectRegistry, EffectWithParams, effect_by_uid};

use crate::audio::{in_place, next_tempo};
use crate::graph_editor::GraphModel;
use crate::gui::{EditorHost, EditorModel, preferred_window_size};
use crate::main_thread::map_flags;
use crate::shared::{GESTURE_BEGIN, GESTURE_END};

// ── Stable CLAP param ids (never renumber) ──────────────────────────────────

const ID_MACRO_BASE: u32 = 0x1000; // 0x1000..0x1005
const ID_MORPH_POS: u32 = 0x1010;
const ID_MORPH_SPEED: u32 = 0x1011;
const ID_INPUT_GAIN: u32 = 0x1020;
const ID_MASTER_VOLUME: u32 = 0x1021;
const ID_BYPASS: u32 = 0x1030;
const ID_TOGGLE_BASE: u32 = 0x1040; // 0x1040 T1, 0x1041 T3

/// The player's parameter count: macros, morph, speed, gains, bypass, toggles.
const PARAM_COUNT: usize = NUM_MACROS + 5 + NUM_TOGGLES;

/// The graph player's fixed parameter set (UID-stable, never reordered on the wire).
fn graph_param_descriptors() -> Vec<ParamDescriptor> {
    const NAMES: [&str; NUM_MACROS] = [
        "Macro 1", "Macro 2", "Macro 3", "Macro 4", "Macro 5", "Macro 6",
    ];
    const SHORT: [&str; NUM_MACROS] = ["M1", "M2", "M3", "M4", "M5", "M6"];
    let mut v = Vec::with_capacity(PARAM_COUNT);
    // At noon by default: every rig is voiced with its knobs there.
    for i in 0..NUM_MACROS {
        v.push(
            ParamDescriptor::custom(NAMES[i], SHORT[i], 0.0, 1.0, 0.5)
                .with_id(ParamId(ID_MACRO_BASE + i as u32), "macro"),
        );
    }
    v.push(
        ParamDescriptor::custom("Morph", "Morph", 0.0, 1.0, 0.0)
            .with_id(ParamId(ID_MORPH_POS), "morph_pos"),
    );
    v.push(
        ParamDescriptor::custom("Morph Speed", "Speed", 0.1, 10.0, 2.0)
            .with_id(ParamId(ID_MORPH_SPEED), "morph_speed"),
    );
    v.push(
        ParamDescriptor::custom("Input Gain", "In", -24.0, 24.0, 0.0)
            .with_unit(ParamUnit::Decibels)
            .with_id(ParamId(ID_INPUT_GAIN), "input_gain"),
    );
    v.push(
        ParamDescriptor::custom("Master Volume", "Vol", -40.0, 6.0, 0.0)
            .with_unit(ParamUnit::Decibels)
            .with_id(ParamId(ID_MASTER_VOLUME), "master_volume"),
    );
    v.push(
        ParamDescriptor::custom("Bypass", "Byp", 0.0, 1.0, 0.0)
            .with_step(1.0)
            .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
            .with_step_labels(&["Off", "On"])
            .with_id(ParamId(ID_BYPASS), "bypass"),
    );
    // The rig's T1 and T3, in the middle by default (as on the pedal).
    for (t, toggle) in RIG_TOGGLES.iter().enumerate() {
        v.push(
            ParamDescriptor::custom(toggle.label(), toggle.label(), 0.0, 2.0, 1.0)
                .with_step(1.0)
                .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                .with_step_labels(&["Up", "Middle", "Down"])
                .with_id(ParamId(ID_TOGGLE_BASE + t as u32), "toggle"),
        );
    }
    v
}

// ── Shared state ─────────────────────────────────────────────────────────────

struct GraphSharedData {
    descriptors: Vec<ParamDescriptor>,
    values: Vec<AtomicU32>,
    /// The rig this plugin plays. Read on activate to build the `PatchPlayer`;
    /// replaced on state load (which happens while inactive in the common flow).
    patch: Mutex<Patch>,
    /// Bumped on every patch change, so the GUI can tell when to redraw the rig.
    generation: AtomicU64,
    latency_samples: AtomicU32,
    /// Host sample rate (`f32` bits), 0 until activated.
    sample_rate: AtomicU32,
    /// GUI → audio gesture flags per param (see [`crate::shared::GESTURE_BEGIN`]).
    gesture_flags: Vec<AtomicU8>,
    /// Audio → GUI meters and scope.
    tap: SignalTap,
    host_notify: Option<Box<dyn Fn() + Send + Sync>>,
    /// Asks the host to deactivate and reactivate the plugin (rebuild the rig).
    host_restart: Option<Box<dyn Fn() + Send + Sync>>,
}

/// Shared state for the graph-player plugin.
#[derive(Clone)]
pub struct GraphShared {
    inner: Arc<GraphSharedData>,
}

impl GraphShared {
    /// Create shared state with default params and an empty patch.
    ///
    /// `host_notify` asks the host for a `process()`/`flush()` call (GUI edits);
    /// `host_restart` asks it to restart the plugin (a new rig). `None` for
    /// standalone use.
    pub fn new(
        host_notify: Option<Box<dyn Fn() + Send + Sync>>,
        host_restart: Option<Box<dyn Fn() + Send + Sync>>,
    ) -> Self {
        let descriptors = graph_param_descriptors();
        let values = descriptors
            .iter()
            .map(|d| AtomicU32::new(d.default.to_bits()))
            .collect();
        let gesture_flags = descriptors.iter().map(|_| AtomicU8::new(0)).collect();
        Self {
            inner: Arc::new(GraphSharedData {
                descriptors,
                values,
                patch: Mutex::new(Patch::new("graph")),
                generation: AtomicU64::new(0),
                latency_samples: AtomicU32::new(0),
                sample_rate: AtomicU32::new(0),
                gesture_flags,
                tap: SignalTap::new(),
                host_notify,
                host_restart,
            }),
        }
    }

    pub(crate) fn param_count(&self) -> usize {
        self.inner.descriptors.len()
    }
    pub(crate) fn descriptor(&self, i: usize) -> Option<&ParamDescriptor> {
        self.inner.descriptors.get(i)
    }
    pub(crate) fn descriptors(&self) -> &[ParamDescriptor] {
        &self.inner.descriptors
    }
    pub(crate) fn index_by_id(&self, id: u32) -> Option<usize> {
        self.inner.descriptors.iter().position(|d| d.id.0 == id)
    }
    /// Index of the Bypass param.
    pub(crate) fn bypass_index(&self) -> Option<usize> {
        self.index_by_id(ID_BYPASS)
    }
    /// Which macro (0–5, K1–K6) param `i` is, if it is one.
    pub(crate) fn macro_of(&self, i: usize) -> Option<usize> {
        let id = self.inner.descriptors.get(i)?.id.0;
        (ID_MACRO_BASE..ID_MACRO_BASE + NUM_MACROS as u32)
            .contains(&id)
            .then(|| (id - ID_MACRO_BASE) as usize)
    }
    /// Which rig toggle (0 = T1, 1 = T3) param `i` is, if it is one.
    pub(crate) fn toggle_of(&self, i: usize) -> Option<usize> {
        let id = self.inner.descriptors.get(i)?.id.0;
        (ID_TOGGLE_BASE..ID_TOGGLE_BASE + NUM_TOGGLES as u32)
            .contains(&id)
            .then(|| (id - ID_TOGGLE_BASE) as usize)
    }
    /// Audio → GUI meters and scope.
    pub(crate) fn tap(&self) -> &SignalTap {
        &self.inner.tap
    }
    /// Host sample rate in Hz (0.0 before activation).
    pub(crate) fn sample_rate(&self) -> f32 {
        f32::from_bits(self.inner.sample_rate.load(Ordering::Acquire))
    }
    pub(crate) fn set_sample_rate(&self, sr: f32) {
        self.inner
            .sample_rate
            .store(sr.to_bits(), Ordering::Release);
    }
    /// Patch generation: changes whenever the patch does.
    pub(crate) fn generation(&self) -> u64 {
        self.inner.generation.load(Ordering::Acquire)
    }
    /// GUI drag start on param `i` (forwarded to the host as a gesture).
    pub(crate) fn gesture_begin(&self, i: usize) {
        if let Some(f) = self.inner.gesture_flags.get(i) {
            f.fetch_or(GESTURE_BEGIN, Ordering::Release);
        }
    }
    /// GUI drag end on param `i`.
    pub(crate) fn gesture_end(&self, i: usize) {
        if let Some(f) = self.inner.gesture_flags.get(i) {
            f.fetch_or(GESTURE_END, Ordering::Release);
        }
    }
    fn take_gesture_flags(&self, i: usize) -> u8 {
        self.inner
            .gesture_flags
            .get(i)
            .map_or(0, |f| f.swap(0, Ordering::AcqRel))
    }
    /// Ask the host to restart the plugin so `activate()` builds the new rig.
    pub(crate) fn request_restart(&self) {
        if let Some(cb) = &self.inner.host_restart {
            cb();
        }
    }
    pub(crate) fn get_value(&self, i: usize) -> Option<f32> {
        self.inner
            .values
            .get(i)
            .map(|v| f32::from_bits(v.load(Ordering::Acquire)))
    }
    pub(crate) fn set_value(&self, i: usize, value: f32) {
        if let Some((a, d)) = self.inner.values.get(i).zip(self.inner.descriptors.get(i)) {
            a.store(value.clamp(d.min, d.max).to_bits(), Ordering::Release);
        }
    }
    fn set_latency_samples(&self, n: u32) {
        self.inner.latency_samples.store(n, Ordering::Release);
    }
    pub(crate) fn latency_samples(&self) -> u32 {
        self.inner.latency_samples.load(Ordering::Acquire)
    }
    /// Snapshot the current patch (for building a player / saving state).
    pub fn patch(&self) -> Patch {
        self.inner.patch.lock().unwrap().clone()
    }
    /// Replace the current patch (state load, or a rig chosen in the editor).
    pub fn set_patch(&self, patch: Patch) {
        *self.inner.patch.lock().unwrap() = patch;
        self.inner.generation.fetch_add(1, Ordering::AcqRel);
    }
    pub(crate) fn notify_host(&self) {
        if let Some(cb) = &self.inner.host_notify {
            cb();
        }
    }
}

impl PluginShared<'_> for GraphShared {}

// ── Main thread ──────────────────────────────────────────────────────────────

/// Main-thread handler: param metadata, state, ports, latency, editor.
pub struct GraphMainThread<'a> {
    shared: &'a GraphShared,
    /// The editor window and its host-facing size/scale state.
    editor: EditorHost,
}

impl<'a> GraphMainThread<'a> {
    /// Create the handler referencing shared state.
    pub fn new(shared: &'a GraphShared) -> Self {
        let owned = shared.clone();
        let (w, h) = preferred_window_size(&GraphModel::deck_descriptors(shared));
        let editor = EditorHost::new("graph-player", (w, h), move || {
            Arc::new(GraphModel::new(owned.clone())) as Arc<dyn EditorModel>
        });
        Self { shared, editor }
    }
}

crate::impl_plugin_gui!(GraphMainThread<'_>, editor);

impl<'a> PluginMainThread<'a, GraphShared> for GraphMainThread<'a> {}

impl PluginMainThreadParams for GraphMainThread<'_> {
    fn count(&mut self) -> u32 {
        self.shared.param_count() as u32
    }

    fn get_info(&mut self, param_index: u32, info: &mut ParamInfoWriter) {
        let Some(desc) = self.shared.descriptor(param_index as usize) else {
            return;
        };
        info.set(&ParamInfo {
            id: ClapId::new(desc.id.0),
            name: desc.name.as_bytes(),
            module: desc.group.as_bytes(),
            min_value: f64::from(desc.min),
            max_value: f64::from(desc.max),
            default_value: f64::from(desc.default),
            flags: map_flags(desc.flags),
            cookie: Cookie::default(),
        });
    }

    fn get_value(&mut self, param_id: ClapId) -> Option<f64> {
        let index = self.shared.index_by_id(param_id.get())?;
        self.shared.get_value(index).map(f64::from)
    }

    fn value_to_text(
        &mut self,
        param_id: ClapId,
        value: f64,
        writer: &mut ParamDisplayWriter,
    ) -> core::fmt::Result {
        use core::fmt::Write;
        let Some(index) = self.shared.index_by_id(param_id.get()) else {
            return write!(writer, "{value:.2}");
        };
        let Some(desc) = self.shared.descriptor(index) else {
            return write!(writer, "{value:.2}");
        };
        write!(writer, "{}", desc.format_value(value as f32))
    }

    fn text_to_value(&mut self, param_id: ClapId, text: &core::ffi::CStr) -> Option<f64> {
        let index = self.shared.index_by_id(param_id.get())?;
        let desc = self.shared.descriptor(index)?;
        desc.parse_value(text.to_str().ok()?).map(f64::from)
    }

    fn flush(&mut self, input: &InputEvents, _output: &mut OutputEvents) {
        for event in input {
            if let Some(clack_plugin::events::spaces::CoreEventSpace::ParamValue(ev)) =
                event.as_core_event()
                && let Some(param_id) = ev.param_id()
                && let Some(index) = self.shared.index_by_id(param_id.get())
            {
                self.shared.set_value(index, ev.value() as f32);
            }
        }
    }
}

/// State = the patch plus current param values.
#[derive(serde::Serialize, serde::Deserialize)]
struct GraphState {
    patch: Patch,
    /// stable param id → value
    params: std::collections::BTreeMap<u32, f64>,
}

impl PluginStateImpl for GraphMainThread<'_> {
    fn save(&mut self, output: &mut OutputStream) -> Result<(), PluginError> {
        let mut params = std::collections::BTreeMap::new();
        for (i, desc) in self.shared.descriptors().iter().enumerate() {
            if let Some(v) = self.shared.get_value(i) {
                params.insert(desc.id.0, f64::from(v));
            }
        }
        let state = GraphState {
            patch: self.shared.patch(),
            params,
        };
        let json = serde_json::to_vec(&state)
            .map_err(|_| PluginError::Message("Failed to serialize state"))?;
        output
            .write_all(&json)
            .map_err(|_| PluginError::Message("Failed to write state"))
    }

    fn load(&mut self, input: &mut InputStream) -> Result<(), PluginError> {
        let mut buf = Vec::new();
        input
            .read_to_end(&mut buf)
            .map_err(|_| PluginError::Message("Failed to read state"))?;
        let state: GraphState =
            serde_json::from_slice(&buf).map_err(|_| PluginError::Message("Invalid state JSON"))?;
        self.shared.set_patch(state.patch);
        for (id, v) in state.params {
            if let Some(index) = self.shared.index_by_id(id) {
                self.shared.set_value(index, v as f32);
            }
        }
        self.shared.notify_host();
        Ok(())
    }
}

impl PluginLatencyImpl for GraphMainThread<'_> {
    fn get(&mut self) -> u32 {
        self.shared.latency_samples()
    }
}

impl PluginAudioPortsImpl for GraphMainThread<'_> {
    fn count(&mut self, _is_input: bool) -> u32 {
        1
    }
    fn get(&mut self, index: u32, _is_input: bool, writer: &mut AudioPortInfoWriter) {
        if index == 0 {
            writer.set(&AudioPortInfo {
                id: ClapId::new(0),
                name: b"Main",
                channel_count: 2,
                flags: AudioPortFlags::IS_MAIN,
                port_type: Some(AudioPortType::STEREO),
                in_place_pair: None,
            });
        }
    }
}

// ── Audio processor ──────────────────────────────────────────────────────────

/// Builds effects for the player from the full registry (the plugin links all effects).
fn registry_make_effect(
    registry: &EffectRegistry,
    uid: u16,
    sr: f32,
) -> Option<(Box<dyn EffectWithParams + Send>, &'static str)> {
    let id = effect_by_uid(uid)?;
    Some((registry.create(id, sr)?, id))
}

/// Real-time audio processor wrapping a [`PatchPlayer`].
pub struct GraphAudioProcessor<'a> {
    shared: &'a GraphShared,
    player: PatchPlayer,
    /// Last value of each param the host knows about; a GUI write shows up as
    /// a difference and is forwarded to the host.
    last_sent: Vec<f32>,
    /// Right channel for mono ports (the player is stereo), sized at activate.
    mono_scratch: Vec<f32>,
    /// Host tempo and transport, refreshed from each block's transport event.
    tempo: TempoContext,
}

impl<'a> PluginAudioProcessor<'a, GraphShared, GraphMainThread<'a>> for GraphAudioProcessor<'a> {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut GraphMainThread<'a>,
        shared: &'a GraphShared,
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let sr = audio_config.sample_rate as f32;
        let block = audio_config.max_frames_count as usize;
        let patch = shared.patch();
        let registry = EffectRegistry::new();
        let mut player = PatchPlayer::from_patch(&patch, sr, block, |uid, s| {
            registry_make_effect(&registry, uid, s)
        })
        .map_err(|_| PluginError::Message("Failed to build patch graph"))?;

        apply_params(shared, &mut player);
        player.apply_controls();
        shared.set_latency_samples(player.engine().latency_samples() as u32);
        shared.set_sample_rate(sr);
        let last_sent = (0..shared.param_count())
            .map(|i| shared.get_value(i).unwrap_or(0.0))
            .collect();

        Ok(Self {
            shared,
            player,
            last_sent,
            mono_scratch: vec![0.0; block.max(1)],
            tempo: TempoContext {
                sample_rate: sr,
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
        // Tempo-synced effects in the rig follow the host transport.
        if let Some(ctx) = next_tempo(&self.tempo, process.transport) {
            self.tempo = ctx;
            self.player.set_tempo_context(&ctx);
        }
        // Host automation in, GUI edits out, then push controls into the player.
        self.handle_events(events.input);
        self.sync_gui_changes(events.output);
        apply_params(self.shared, &mut self.player);
        self.player.apply_controls();

        let tap = self.shared.tap();
        for mut port_pair in &mut audio {
            let Some(mut channels) = port_pair.channels()?.into_f32() else {
                continue;
            };
            match channels.channel_pair_count() {
                0 => {}
                1 => {
                    // Mono: run the rig on L = R and keep the left output.
                    if let Some(buf) = channels.channel_pair(0).and_then(in_place) {
                        let n = buf.len().min(self.mono_scratch.len());
                        let (buf, right) = (&mut buf[..n], &mut self.mono_scratch[..n]);
                        right.copy_from_slice(buf);
                        tap.input(buf, buf);
                        self.player.process_block_stereo(buf, right);
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
                        // Bypassed, the player leaves the buffers (the dry input) alone.
                        self.player.process_block_stereo(left, right);
                        tap.output(left, right);
                    }
                }
            }
        }

        Ok(ProcessStatus::ContinueIfNotQuiet)
    }

    fn deactivate(self, _main_thread: &mut GraphMainThread<'_>) {}
}

impl GraphAudioProcessor<'_> {
    /// Host automation → shared params (and what the host now knows).
    fn handle_events(&mut self, input: &InputEvents) {
        for event in input {
            if let Some(clack_plugin::events::spaces::CoreEventSpace::ParamValue(ev)) =
                event.as_core_event()
                && let Some(param_id) = ev.param_id()
                && let Some(index) = self.shared.index_by_id(param_id.get())
            {
                self.shared.set_value(index, ev.value() as f32);
                if let Some(v) = self.shared.get_value(index) {
                    self.last_sent[index] = v;
                }
            }
        }
    }

    /// GUI edits → host: gesture begin, the new value, gesture end, so the
    /// host records automation and groups undo per drag.
    fn sync_gui_changes(&mut self, output: &mut OutputEvents) {
        for i in 0..self.shared.param_count() {
            let flags = self.shared.take_gesture_flags(i);
            let Some(desc) = self.shared.descriptor(i) else {
                continue;
            };
            let id = ClapId::new(desc.id.0);
            if flags & GESTURE_BEGIN != 0 {
                let _ = output
                    .try_push(ParamGestureBeginEvent::new(0, id).with_flags(EventFlags::IS_LIVE));
            }
            if let Some(v) = self.shared.get_value(i)
                && v.to_bits() != self.last_sent[i].to_bits()
            {
                self.last_sent[i] = v;
                let ev =
                    ParamValueEvent::new(0, id, Pckn::match_all(), f64::from(v), Cookie::empty())
                        .with_flags(EventFlags::IS_LIVE);
                let _ = output.try_push(ev);
            }
            if flags & GESTURE_END != 0 {
                let _ = output
                    .try_push(ParamGestureEndEvent::new(0, id).with_flags(EventFlags::IS_LIVE));
            }
        }
    }
}

/// The rig rings out as long as its longest tail (reverb, delay, looper).
impl PluginTailImpl for GraphAudioProcessor<'_> {
    fn get(&self) -> TailLength {
        let engine = self.player.engine();
        let longest = (0..engine.slot_count())
            .filter_map(|slot| engine.effect_at(slot))
            .map(|e| e.tail_samples())
            .max()
            .unwrap_or(0);
        match u32::try_from(longest) {
            Ok(samples) if samples < u32::MAX => TailLength::Finite(samples),
            _ => TailLength::Infinite,
        }
    }
}

impl PluginAudioProcessorParams for GraphAudioProcessor<'_> {
    fn flush(&mut self, input: &InputEvents, output: &mut OutputEvents) {
        self.handle_events(input);
        self.sync_gui_changes(output);
        apply_params(self.shared, &mut self.player);
        self.player.apply_controls();
    }
}

/// Push the shared param atomics into the player (macros, toggles, morph,
/// gains, bypass).
pub(crate) fn apply_params(shared: &GraphShared, player: &mut PatchPlayer) {
    for i in 0..NUM_MACROS {
        if let Some(index) = shared.index_by_id(ID_MACRO_BASE + i as u32)
            && let Some(v) = shared.get_value(index)
        {
            player.set_macro(i, v);
        }
    }
    for t in 0..NUM_TOGGLES {
        if let Some(v) = shared
            .index_by_id(ID_TOGGLE_BASE + t as u32)
            .and_then(|i| shared.get_value(i))
        {
            player.set_toggle(t, v.round().clamp(0.0, 2.0) as u8);
        }
    }
    if let Some(v) = shared
        .index_by_id(ID_MORPH_POS)
        .and_then(|i| shared.get_value(i))
    {
        player.set_morph_position(v);
    }
    if let Some(v) = shared
        .index_by_id(ID_INPUT_GAIN)
        .and_then(|i| shared.get_value(i))
    {
        player.set_input_gain_db(v);
    }
    if let Some(v) = shared
        .index_by_id(ID_MASTER_VOLUME)
        .and_then(|i| shared.get_value(i))
    {
        player.set_master_volume_db(v);
    }
    if let Some(v) = shared
        .index_by_id(ID_BYPASS)
        .and_then(|i| shared.get_value(i))
    {
        player.set_bypassed(v > 0.5);
    }
}

// ── Plugin entry ─────────────────────────────────────────────────────────────

/// The graph-player plugin type, exported from the bundle's factory
/// ([`crate::bundle`]) alongside the effects.
pub struct GraphPlugin;

impl Plugin for GraphPlugin {
    type AudioProcessor<'a> = GraphAudioProcessor<'a>;
    type Shared<'a> = GraphShared;
    type MainThread<'a> = GraphMainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&GraphShared>) {
        use clack_extensions::audio_ports::PluginAudioPorts;
        use clack_extensions::gui::PluginGui;
        use clack_extensions::latency::PluginLatency;
        use clack_extensions::params::PluginParams;
        use clack_extensions::state::PluginState;
        use clack_extensions::tail::PluginTail;

        builder.register::<PluginAudioPorts>();
        builder.register::<PluginGui>();
        builder.register::<PluginLatency>();
        builder.register::<PluginParams>();
        builder.register::<PluginState>();
        builder.register::<PluginTail>();
    }
}

impl DefaultPluginFactory for GraphPlugin {
    fn get_descriptor() -> PluginDescriptor {
        use clack_plugin::plugin::features::{AUDIO_EFFECT, STEREO};
        PluginDescriptor::new("com.sonido.graph-player", "Sonido Graph Player")
            .with_features([AUDIO_EFFECT, STEREO])
    }

    fn new_shared(host: HostSharedHandle<'_>) -> Result<GraphShared, PluginError> {
        // See sonido_effect_entry!: the host outlives the plugin; the lifetime is
        // phantom over a NonNull<clap_host>.
        #[allow(unsafe_code)]
        let host: HostSharedHandle<'static> = unsafe { core::mem::transmute(host) };
        let notify: Box<dyn Fn() + Send + Sync> = Box::new(move || {
            host.request_process();
        });
        let restart: Box<dyn Fn() + Send + Sync> = Box::new(move || {
            host.request_restart();
        });
        Ok(GraphShared::new(Some(notify), Some(restart)))
    }

    fn new_main_thread<'a>(
        _host: HostMainThreadHandle<'a>,
        shared: &'a GraphShared,
    ) -> Result<GraphMainThread<'a>, PluginError> {
        Ok(GraphMainThread::new(shared))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_patch::{PatchNode, binary};

    fn patch_with_distortion() -> Patch {
        Patch::linear_chain(
            "test",
            vec![PatchNode::new(
                sonido_registry::effect_uid("distortion").unwrap(),
                vec![20.0],
            )],
        )
    }

    #[test]
    fn descriptors_are_stable_and_unique() {
        let descs = graph_param_descriptors();
        assert_eq!(descs.len(), PARAM_COUNT);
        let mut ids: Vec<u32> = descs.iter().map(|d| d.id.0).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), PARAM_COUNT, "duplicate param id");
    }

    #[test]
    fn shared_defaults_and_patch_roundtrip() {
        let shared = GraphShared::new(None, None);
        assert_eq!(shared.param_count(), PARAM_COUNT);
        let toggles: Vec<usize> = (0..PARAM_COUNT)
            .filter_map(|i| shared.toggle_of(i))
            .collect();
        assert_eq!(toggles, [0, 1]);
        // Patch set/get roundtrips.
        let patch = patch_with_distortion();
        shared.set_patch(patch.clone());
        assert_eq!(shared.patch(), patch);
    }

    #[test]
    fn state_roundtrip_preserves_patch_and_params() {
        let shared = GraphShared::new(None, None);
        shared.set_patch(patch_with_distortion());
        // Set macro 0 and master volume to non-defaults.
        let m0 = shared.index_by_id(ID_MACRO_BASE).unwrap();
        let mv = shared.index_by_id(ID_MASTER_VOLUME).unwrap();
        shared.set_value(m0, 0.75);
        shared.set_value(mv, -6.0);

        // Build the GraphState the way save() does.
        let mut params = std::collections::BTreeMap::new();
        for (i, desc) in shared.descriptors().iter().enumerate() {
            params.insert(desc.id.0, f64::from(shared.get_value(i).unwrap()));
        }
        let state = GraphState {
            patch: shared.patch(),
            params,
        };
        let json = serde_json::to_vec(&state).unwrap();

        // Load into a fresh shared.
        let shared2 = GraphShared::new(None, None);
        let loaded: GraphState = serde_json::from_slice(&json).unwrap();
        shared2.set_patch(loaded.patch);
        for (id, v) in loaded.params {
            if let Some(index) = shared2.index_by_id(id) {
                shared2.set_value(index, v as f32);
            }
        }
        assert_eq!(shared2.patch(), shared.patch());
        assert_eq!(shared2.get_value(m0).unwrap(), 0.75);
        assert_eq!(shared2.get_value(mv).unwrap(), -6.0);
    }

    #[test]
    fn patch_state_is_the_exported_blob() {
        // "Export as CLAP" writes the patch JSON; the plugin's state embeds the
        // same Patch, so a sector-encoded patch and the plugin's patch agree.
        let patch = patch_with_distortion();
        let mut buf = [0u8; sonido_patch::SECTOR_SIZE];
        let len = binary::encode(&patch, &mut buf).unwrap();
        let from_bin = binary::decode(&buf[..len]).unwrap();
        let json = serde_json::to_string(&patch).unwrap();
        let from_json: Patch = serde_json::from_str(&json).unwrap();
        assert_eq!(from_bin, from_json);
    }
}
