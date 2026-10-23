//! The Sonido plugin bundle: every effect, plus the graph player, behind one
//! CLAP entry point.
//!
//! CLAP lets one binary export many plugins through its plugin factory, so the
//! whole suite ships as a single `sonido.clap` (and one `sonido.vst3` shim).
//! That is one release link instead of one per effect, and one file to install.
//! Each plugin keeps its own stable ID (`com.sonido.<effect>`), which is what a
//! host stores in a project, so projects saved against the old one-file-per-
//! effect builds still find their plugins.
//!
//! [`CATALOG`] is the plugin list; `tests/plugin_inventory.rs` holds it to the
//! registry, so a kernel added there cannot miss its plugin.

use std::ffi::CStr;

use clack_plugin::entry::prelude::*;
use clack_plugin::plugin::features::{
    ANALYZER, AUDIO_EFFECT, CHORUS, COMPRESSOR, DEESSER, DELAY, DISTORTION, EQUALIZER, FILTER,
    FLANGER, GLITCH, GRANULAR, PHASER, PITCH_SHIFTER, REVERB, SAMPLER, STEREO, TRANSIENT_SHAPER,
    TREMOLO, UTILITY,
};
use clack_plugin::prelude::*;
use sonido_registry::EffectRegistry;

use crate::graph_player::GraphPlugin;
use crate::{SonidoAudioProcessor, SonidoMainThread, SonidoShared};

/// One single-effect plugin in the bundle.
#[derive(Clone, Copy, Debug)]
pub struct PluginSpec {
    /// Registry ID of the wrapped effect (e.g. `"distortion"`).
    pub effect_id: &'static str,
    /// Stable CLAP plugin ID, stored by hosts in saved projects. Never change.
    pub clap_id: &'static str,
    /// Display name shown in the host's plugin browser.
    pub name: &'static str,
    /// CLAP feature tags (category hints for the host's browser).
    pub features: &'static [&'static CStr],
}

const fn spec(
    effect_id: &'static str,
    clap_id: &'static str,
    name: &'static str,
    features: &'static [&'static CStr],
) -> PluginSpec {
    PluginSpec {
        effect_id,
        clap_id,
        name,
        features,
    }
}

/// Every single-effect plugin in the bundle, one per registered effect.
pub const CATALOG: &[PluginSpec] = &[
    spec(
        "amp",
        "com.sonido.amp",
        "Sonido Amp",
        &[AUDIO_EFFECT, DISTORTION, STEREO],
    ),
    spec(
        "bitcrusher",
        "com.sonido.bitcrusher",
        "Sonido Bitcrusher",
        &[AUDIO_EFFECT, DISTORTION, STEREO],
    ),
    spec(
        "cabinet",
        "com.sonido.cabinet",
        "Sonido Cabinet",
        &[AUDIO_EFFECT, FILTER, STEREO],
    ),
    spec(
        "chorus",
        "com.sonido.chorus",
        "Sonido Chorus",
        &[AUDIO_EFFECT, CHORUS, STEREO],
    ),
    spec(
        "compressor",
        "com.sonido.compressor",
        "Sonido Compressor",
        &[AUDIO_EFFECT, COMPRESSOR, STEREO],
    ),
    spec(
        "deesser",
        "com.sonido.deesser",
        "Sonido De-esser",
        &[AUDIO_EFFECT, DEESSER, STEREO],
    ),
    spec(
        "delay",
        "com.sonido.delay",
        "Sonido Delay",
        &[AUDIO_EFFECT, DELAY, STEREO],
    ),
    spec(
        "distortion",
        "com.sonido.distortion",
        "Sonido Distortion",
        &[AUDIO_EFFECT, DISTORTION, STEREO],
    ),
    spec(
        "drone",
        "com.sonido.drone",
        "Sonido Drone",
        &[AUDIO_EFFECT, REVERB, STEREO],
    ),
    spec(
        "eq",
        "com.sonido.eq",
        "Sonido Parametric EQ",
        &[AUDIO_EFFECT, EQUALIZER, STEREO],
    ),
    spec(
        "filter",
        "com.sonido.filter",
        "Sonido Filter",
        &[AUDIO_EFFECT, FILTER, STEREO],
    ),
    spec(
        "flanger",
        "com.sonido.flanger",
        "Sonido Flanger",
        &[AUDIO_EFFECT, FLANGER, STEREO],
    ),
    spec(
        "gate",
        "com.sonido.gate",
        "Sonido Gate",
        &[AUDIO_EFFECT, COMPRESSOR, STEREO],
    ),
    spec(
        "glitch",
        "com.sonido.glitch",
        "Sonido Glitch",
        &[AUDIO_EFFECT, GLITCH, STEREO],
    ),
    spec(
        "harmonic_habitat",
        "com.sonido.harmonic-habitat",
        "Sonido Harmonic Habitat",
        &[AUDIO_EFFECT, REVERB, STEREO],
    ),
    spec(
        "limiter",
        "com.sonido.limiter",
        "Sonido Limiter",
        &[AUDIO_EFFECT, COMPRESSOR, STEREO],
    ),
    spec(
        "looper",
        "com.sonido.looper",
        "Sonido Looper",
        &[AUDIO_EFFECT, SAMPLER, STEREO],
    ),
    spec(
        "multiband_comp",
        "com.sonido.multiband-comp",
        "Sonido Multiband Compressor",
        &[AUDIO_EFFECT, COMPRESSOR, STEREO],
    ),
    spec(
        "phaser",
        "com.sonido.phaser",
        "Sonido Phaser",
        &[AUDIO_EFFECT, PHASER, STEREO],
    ),
    spec(
        "pitch_shift",
        "com.sonido.pitch-shift",
        "Sonido Pitch Shift",
        &[AUDIO_EFFECT, PITCH_SHIFTER, STEREO],
    ),
    spec(
        "plate_reverb",
        "com.sonido.plate-reverb",
        "Sonido Plate Reverb",
        &[AUDIO_EFFECT, REVERB, STEREO],
    ),
    spec(
        "preamp",
        "com.sonido.preamp",
        "Sonido Preamp",
        &[AUDIO_EFFECT, UTILITY, STEREO],
    ),
    spec(
        "reverb",
        "com.sonido.reverb",
        "Sonido Reverb",
        &[AUDIO_EFFECT, REVERB, STEREO],
    ),
    spec(
        "ringmod",
        "com.sonido.ringmod",
        "Sonido Ring Mod",
        &[AUDIO_EFFECT, UTILITY, STEREO],
    ),
    spec(
        "shelving_eq",
        "com.sonido.shelving-eq",
        "Sonido Shelving EQ",
        &[AUDIO_EFFECT, EQUALIZER, STEREO],
    ),
    spec(
        "spring_reverb",
        "com.sonido.spring-reverb",
        "Sonido Spring Reverb",
        &[AUDIO_EFFECT, REVERB, STEREO],
    ),
    spec(
        "stage",
        "com.sonido.stage",
        "Sonido Stage",
        &[AUDIO_EFFECT, UTILITY, STEREO],
    ),
    spec(
        "stereo_widener",
        "com.sonido.stereo-widener",
        "Sonido Stereo Widener",
        &[AUDIO_EFFECT, UTILITY, STEREO],
    ),
    spec(
        "tape",
        "com.sonido.tape",
        "Sonido Tape Saturation",
        &[AUDIO_EFFECT, DISTORTION, STEREO],
    ),
    spec(
        "texture",
        "com.sonido.texture",
        "Sonido Texture",
        &[AUDIO_EFFECT, GRANULAR, STEREO],
    ),
    spec(
        "time_stretch",
        "com.sonido.time-stretch",
        "Sonido Time Stretch",
        &[AUDIO_EFFECT, PITCH_SHIFTER, GRANULAR, STEREO],
    ),
    spec(
        "transient_shaper",
        "com.sonido.transient-shaper",
        "Sonido Transient Shaper",
        &[AUDIO_EFFECT, TRANSIENT_SHAPER, STEREO],
    ),
    spec(
        "tremolo",
        "com.sonido.tremolo",
        "Sonido Tremolo",
        &[AUDIO_EFFECT, TREMOLO, STEREO],
    ),
    spec(
        "tuner",
        "com.sonido.tuner",
        "Sonido Tuner",
        &[AUDIO_EFFECT, ANALYZER, UTILITY, STEREO],
    ),
    spec(
        "vibrato",
        "com.sonido.vibrato",
        "Sonido Vibrato",
        &[AUDIO_EFFECT, CHORUS, STEREO],
    ),
    spec(
        "wah",
        "com.sonido.wah",
        "Sonido Wah",
        &[AUDIO_EFFECT, FILTER, STEREO],
    ),
];

/// Vendor string reported for every plugin in the bundle.
pub const VENDOR: &str = "Ampactor Labs";

/// The plugin type behind every single-effect plugin: which effect it wraps
/// is decided at instantiation, from the requested plugin ID.
pub struct EffectPlugin;

impl Plugin for EffectPlugin {
    type AudioProcessor<'a> = SonidoAudioProcessor<'a>;
    type Shared<'a> = SonidoShared;
    type MainThread<'a> = SonidoMainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&SonidoShared>) {
        use clack_extensions::audio_ports::PluginAudioPorts;
        use clack_extensions::gui::PluginGui;
        use clack_extensions::latency::PluginLatency;
        use clack_extensions::params::PluginParams;
        use clack_extensions::state::PluginState;
        use clack_extensions::tail::PluginTail;
        use clack_extensions::track_info::PluginTrackInfo;

        builder.register::<PluginAudioPorts>();
        builder.register::<PluginGui>();
        builder.register::<PluginLatency>();
        builder.register::<PluginParams>();
        builder.register::<PluginState>();
        builder.register::<PluginTail>();
        builder.register::<PluginTrackInfo>();
    }
}

/// Shared state for one effect instance, wired to ask the host for a
/// `process()` call whenever the GUI changes a parameter.
pub fn new_effect_shared(
    effect_id: &'static str,
    host: HostSharedHandle<'_>,
) -> Result<SonidoShared, PluginError> {
    // CLAP guarantees the host outlives the plugin. HostSharedHandle is
    // Copy + repr(transparent) over NonNull<clap_host>; the lifetime is phantom.
    #[allow(unsafe_code)]
    let host: HostSharedHandle<'static> = unsafe { core::mem::transmute(host) };
    tracing::info!(effect_id, "plugin instance created");
    let notify: Box<dyn Fn() + Send + Sync> = Box::new(move || {
        host.request_process();
    });
    Ok(SonidoShared::new(effect_id, Some(notify)))
}

/// What a factory slot instantiates.
enum Slot {
    Effect(&'static str),
    Graph,
}

/// The bundle's plugin factory: [`CATALOG`] in order, then the graph player.
pub struct SonidoFactory {
    plugins: Vec<(PluginDescriptor, Slot)>,
}

impl SonidoFactory {
    fn new() -> Self {
        let registry = EffectRegistry::new();
        let version = env!("CARGO_PKG_VERSION");
        let mut plugins: Vec<(PluginDescriptor, Slot)> = CATALOG
            .iter()
            .map(|p| {
                let description = registry
                    .descriptor(p.effect_id)
                    .map_or("", |d| d.description);
                let desc = PluginDescriptor::new(p.clap_id, p.name)
                    .with_vendor(VENDOR)
                    .with_version(version)
                    .with_description(description)
                    .with_features(p.features.iter().copied());
                (desc, Slot::Effect(p.effect_id))
            })
            .collect();
        let graph = GraphPlugin::get_descriptor()
            .with_vendor(VENDOR)
            .with_version(version);
        plugins.push((graph, Slot::Graph));
        Self { plugins }
    }
}

impl PluginFactoryImpl for SonidoFactory {
    fn plugin_count(&self) -> u32 {
        self.plugins.len() as u32
    }

    fn plugin_descriptor(&self, index: u32) -> Option<&PluginDescriptor> {
        self.plugins.get(index as usize).map(|(d, _)| d)
    }

    fn create_plugin<'a>(
        &'a self,
        host_info: HostInfo<'a>,
        plugin_id: &CStr,
    ) -> Option<PluginInstance<'a>> {
        let (desc, slot) = self
            .plugins
            .iter()
            .find(|(d, _)| d.id() == Some(plugin_id))?;
        Some(match *slot {
            Slot::Effect(effect_id) => PluginInstance::new::<EffectPlugin>(
                host_info,
                desc,
                move |host| new_effect_shared(effect_id, host),
                |_host, shared| Ok(SonidoMainThread::new(shared)),
            ),
            Slot::Graph => PluginInstance::new::<GraphPlugin>(
                host_info,
                desc,
                GraphPlugin::new_shared,
                GraphPlugin::new_main_thread,
            ),
        })
    }
}

/// The bundle's CLAP entry: export it with
/// `clack_export_entry!(sonido_plugin::bundle::SonidoEntry)`.
pub struct SonidoEntry {
    factory: PluginFactoryWrapper<SonidoFactory>,
}

impl Entry for SonidoEntry {
    fn new(_bundle_path: &CStr) -> Result<Self, EntryLoadError> {
        Ok(Self {
            factory: PluginFactoryWrapper::new(SonidoFactory::new()),
        })
    }

    fn declare_factories<'a>(&'a self, builder: &mut EntryFactories<'a>) {
        builder.register_factory(&self.factory);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_lists_every_catalog_plugin_then_the_graph_player() {
        let factory = SonidoFactory::new();
        assert_eq!(factory.plugin_count() as usize, CATALOG.len() + 1);
        for (i, spec) in CATALOG.iter().enumerate() {
            let desc = factory.plugin_descriptor(i as u32).unwrap();
            assert_eq!(desc.id().unwrap().to_str().unwrap(), spec.clap_id);
        }
        let last = factory.plugin_descriptor(CATALOG.len() as u32).unwrap();
        assert_eq!(
            last.id().unwrap().to_str().unwrap(),
            "com.sonido.graph-player"
        );
    }
}
