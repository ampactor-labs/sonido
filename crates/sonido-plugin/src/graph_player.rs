//! Graph-player CLAP plugin — runs any exported [`Patch`] as a single plugin.
//!
//! Where the per-effect plugins each wrap one effect, this wraps a whole rig: a
//! [`PatchPlayer`] driven by six macro params, a morph position/speed, input and
//! master gain, and a bypass. Its state *is* the patch JSON the Studio GUI
//! exports — so "Export as CLAP" is the same blob, and a rig sounds identical in
//! the DAW and on the pedal (both wrap a `PatchPlayer`).
//!
//! This is the headless core (params + state + audio). It is loadable and
//! automatable in any host today; the editor GUI (reusing the macro/morph panels)
//! is a follow-up.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicU32, Ordering};
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
use clack_plugin::prelude::*;
use clack_plugin::stream::{InputStream, OutputStream};
use clack_plugin::utils::Cookie;

use sonido_core::{ParamDescriptor, ParamId};
use sonido_patch::{NUM_MACROS, Patch, PatchPlayer};
use sonido_registry::{EffectRegistry, EffectWithParams, effect_by_uid};

use crate::main_thread::map_flags;

// ── Stable CLAP param ids (never renumber) ──────────────────────────────────

const ID_MACRO_BASE: u32 = 0x1000; // 0x1000..0x1005
const ID_MORPH_POS: u32 = 0x1010;
const ID_MORPH_SPEED: u32 = 0x1011;
const ID_INPUT_GAIN: u32 = 0x1020;
const ID_MASTER_VOLUME: u32 = 0x1021;
const ID_BYPASS: u32 = 0x1030;

/// The graph player's fixed parameter set (UID-stable, never reordered on the wire).
fn graph_param_descriptors() -> Vec<ParamDescriptor> {
    let mut v = Vec::with_capacity(NUM_MACROS + 5);
    for i in 0..NUM_MACROS {
        v.push(
            ParamDescriptor::custom("Macro", "Macro", 0.0, 1.0, 0.0)
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
            .with_id(ParamId(ID_INPUT_GAIN), "input_gain"),
    );
    v.push(
        ParamDescriptor::custom("Master Volume", "Vol", -40.0, 6.0, 0.0)
            .with_id(ParamId(ID_MASTER_VOLUME), "master_volume"),
    );
    v.push(
        ParamDescriptor::custom("Bypass", "Byp", 0.0, 1.0, 0.0)
            .with_id(ParamId(ID_BYPASS), "bypass"),
    );
    v
}

// ── Shared state ─────────────────────────────────────────────────────────────

struct GraphSharedData {
    descriptors: Vec<ParamDescriptor>,
    values: Vec<AtomicU32>,
    /// The rig this plugin plays. Read on activate to build the `PatchPlayer`;
    /// replaced on state load (which happens while inactive in the common flow).
    patch: Mutex<Patch>,
    latency_samples: AtomicU32,
    host_notify: Option<Box<dyn Fn() + Send + Sync>>,
}

/// Shared state for the graph-player plugin.
#[derive(Clone)]
pub struct GraphShared {
    inner: Arc<GraphSharedData>,
}

impl GraphShared {
    /// Create shared state with default params and an empty patch.
    pub fn new(host_notify: Option<Box<dyn Fn() + Send + Sync>>) -> Self {
        let descriptors = graph_param_descriptors();
        let values = descriptors
            .iter()
            .map(|d| AtomicU32::new(d.default.to_bits()))
            .collect();
        Self {
            inner: Arc::new(GraphSharedData {
                descriptors,
                values,
                patch: Mutex::new(Patch::new("graph")),
                latency_samples: AtomicU32::new(0),
                host_notify,
            }),
        }
    }

    fn param_count(&self) -> usize {
        self.inner.descriptors.len()
    }
    fn descriptor(&self, i: usize) -> Option<&ParamDescriptor> {
        self.inner.descriptors.get(i)
    }
    fn descriptors(&self) -> &[ParamDescriptor] {
        &self.inner.descriptors
    }
    fn index_by_id(&self, id: u32) -> Option<usize> {
        self.inner.descriptors.iter().position(|d| d.id.0 == id)
    }
    fn get_value(&self, i: usize) -> Option<f32> {
        self.inner
            .values
            .get(i)
            .map(|v| f32::from_bits(v.load(Ordering::Acquire)))
    }
    fn set_value(&self, i: usize, value: f32) {
        if let Some((a, d)) = self.inner.values.get(i).zip(self.inner.descriptors.get(i)) {
            a.store(value.clamp(d.min, d.max).to_bits(), Ordering::Release);
        }
    }
    fn set_latency_samples(&self, n: u32) {
        self.inner.latency_samples.store(n, Ordering::Release);
    }
    fn latency_samples(&self) -> u32 {
        self.inner.latency_samples.load(Ordering::Acquire)
    }
    /// Snapshot the current patch (for building a player / saving state).
    pub fn patch(&self) -> Patch {
        self.inner.patch.lock().unwrap().clone()
    }
    /// Replace the current patch (state load).
    pub fn set_patch(&self, patch: Patch) {
        *self.inner.patch.lock().unwrap() = patch;
    }
    fn notify_host(&self) {
        if let Some(cb) = &self.inner.host_notify {
            cb();
        }
    }
}

impl PluginShared<'_> for GraphShared {}

// ── Main thread ──────────────────────────────────────────────────────────────

/// Main-thread handler: param metadata, state, ports, latency.
pub struct GraphMainThread<'a> {
    shared: &'a GraphShared,
}

impl<'a> GraphMainThread<'a> {
    /// Create the handler referencing shared state.
    pub fn new(shared: &'a GraphShared) -> Self {
        Self { shared }
    }
}

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

        Ok(Self { shared, player })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        // Apply host automation, then push all control values into the player.
        handle_param_events(self.shared, events.input);
        apply_params(self.shared, &mut self.player);
        self.player.apply_controls();

        for mut port_pair in &mut audio {
            let Some(mut channels) = port_pair.channels()?.into_f32() else {
                continue;
            };
            if channels.channel_pair_count() < 2 {
                continue;
            }
            match (channels.channel_pair(0), channels.channel_pair(1)) {
                (
                    Some(ChannelPair::InputOutput(li, lo)),
                    Some(ChannelPair::InputOutput(ri, ro)),
                ) => {
                    lo.copy_from_slice(li);
                    ro.copy_from_slice(ri);
                    self.player.process_block_stereo(lo, ro);
                }
                (Some(ChannelPair::InPlace(l)), Some(ChannelPair::InPlace(r))) => {
                    self.player.process_block_stereo(l, r);
                }
                _ => {}
            }
        }

        Ok(ProcessStatus::ContinueIfNotQuiet)
    }

    fn deactivate(self, _main_thread: &mut GraphMainThread<'_>) {}
}

impl PluginAudioProcessorParams for GraphAudioProcessor<'_> {
    fn flush(&mut self, input: &InputEvents, _output: &mut OutputEvents) {
        handle_param_events(self.shared, input);
        apply_params(self.shared, &mut self.player);
        self.player.apply_controls();
    }
}

/// Update shared param atomics from incoming host parameter events.
fn handle_param_events(shared: &GraphShared, input: &InputEvents) {
    for event in input {
        if let Some(clack_plugin::events::spaces::CoreEventSpace::ParamValue(ev)) =
            event.as_core_event()
            && let Some(param_id) = ev.param_id()
            && let Some(index) = shared.index_by_id(param_id.get())
        {
            shared.set_value(index, ev.value() as f32);
        }
    }
}

/// Push the shared param atomics into the player (macros, morph, gains, bypass).
fn apply_params(shared: &GraphShared, player: &mut PatchPlayer) {
    for i in 0..NUM_MACROS {
        if let Some(index) = shared.index_by_id(ID_MACRO_BASE + i as u32)
            && let Some(v) = shared.get_value(index)
        {
            player.set_macro(i, v);
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

/// The graph-player plugin type. The example cdylib exports it via
/// `clack_export_entry!(SinglePluginEntry<GraphPlugin>)`.
pub struct GraphPlugin;

impl Plugin for GraphPlugin {
    type AudioProcessor<'a> = GraphAudioProcessor<'a>;
    type Shared<'a> = GraphShared;
    type MainThread<'a> = GraphMainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&GraphShared>) {
        use clack_extensions::audio_ports::PluginAudioPorts;
        use clack_extensions::latency::PluginLatency;
        use clack_extensions::params::PluginParams;
        use clack_extensions::state::PluginState;

        builder.register::<PluginAudioPorts>();
        builder.register::<PluginLatency>();
        builder.register::<PluginParams>();
        builder.register::<PluginState>();
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
        Ok(GraphShared::new(Some(notify)))
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
        assert_eq!(descs.len(), NUM_MACROS + 5);
        let mut ids: Vec<u32> = descs.iter().map(|d| d.id.0).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), NUM_MACROS + 5, "duplicate param id");
    }

    #[test]
    fn shared_defaults_and_patch_roundtrip() {
        let shared = GraphShared::new(None);
        assert_eq!(shared.param_count(), NUM_MACROS + 5);
        // Patch set/get roundtrips.
        let patch = patch_with_distortion();
        shared.set_patch(patch.clone());
        assert_eq!(shared.patch(), patch);
    }

    #[test]
    fn state_roundtrip_preserves_patch_and_params() {
        let shared = GraphShared::new(None);
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
        let shared2 = GraphShared::new(None);
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
