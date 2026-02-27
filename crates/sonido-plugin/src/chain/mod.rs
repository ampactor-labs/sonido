//! Multi-effect chain CLAP plugin.
//!
//! Exposes a full effect chain as a single CLAP plugin with add/remove/reorder,
//! backed by the DAG routing engine (`ProcessingGraph`). Parameter slots are
//! pre-allocated (16 slots × 32 params = 512 CLAP params) so chain mutations
//! never require host restart.
//!
//! # Architecture
//!
//! ```text
//! GUI thread            Audio thread           Main thread
//! ─────────             ────────────           ───────────
//! writes params         reads params           param queries
//! queues commands  ──►  drains commands        rescan dispatch
//!                       owns ProcessingGraph   state save/load
//!                       compiles schedule      GUI lifecycle
//! ```
//!
//! All inter-thread communication goes through [`ChainShared`] — atomics for
//! params, `ArcSwap` for slot descriptors, `Mutex` for structural commands.

pub mod audio;
pub mod gui;
pub mod main_thread;
pub mod param_bridge;
pub mod shared;

pub use audio::ChainAudioProcessor;
pub use main_thread::ChainMainThread;
pub use shared::ChainShared;

use clack_extensions::audio_ports::PluginAudioPorts;
use clack_extensions::gui::PluginGui;
use clack_extensions::latency::PluginLatency;
use clack_extensions::params::PluginParams;
use clack_extensions::state::PluginState;
use clack_plugin::prelude::*;

/// Maximum number of effect slots in the chain.
pub const MAX_SLOTS: usize = 16;

/// Number of CLAP parameter IDs reserved per slot.
///
/// Stage is the largest effect at 12 params; 32 provides headroom for future
/// effects without changing the parameter layout.
pub const SLOT_STRIDE: usize = 32;

/// Total pre-allocated CLAP parameter count (always visible to host).
pub const TOTAL_PARAMS: usize = MAX_SLOTS * SLOT_STRIDE;

/// Validated CLAP parameter ID for the chain plugin.
///
/// Encodes `slot * SLOT_STRIDE + local_param_index` in a single `u32`.
/// Validated at construction — all code paths that touch the flat param space
/// go through this type. Methods are `const fn` and zero-cost at runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ClapParamId(u32);

impl ClapParamId {
    /// Construct from slot and local parameter index.
    ///
    /// Returns `None` if `slot >= MAX_SLOTS` or `param >= SLOT_STRIDE`.
    pub const fn new(slot: usize, param: usize) -> Option<Self> {
        if slot >= MAX_SLOTS || param >= SLOT_STRIDE {
            return None;
        }
        Some(Self((slot * SLOT_STRIDE + param) as u32))
    }

    /// Construct from a raw CLAP `clap_id` value.
    ///
    /// Returns `None` if the value is out of the valid range.
    pub const fn from_raw(id: u32) -> Option<Self> {
        if (id as usize) >= TOTAL_PARAMS {
            return None;
        }
        // Validate decomposition is within bounds.
        let slot = id as usize / SLOT_STRIDE;
        let param = id as usize % SLOT_STRIDE;
        if slot >= MAX_SLOTS || param >= SLOT_STRIDE {
            return None;
        }
        Some(Self(id))
    }

    /// The slot index this parameter belongs to.
    pub const fn slot(self) -> usize {
        self.0 as usize / SLOT_STRIDE
    }

    /// The local parameter index within the slot.
    pub const fn param(self) -> usize {
        self.0 as usize % SLOT_STRIDE
    }

    /// Raw `u32` value for indexing into flat arrays and CLAP host communication.
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// Multi-effect chain plugin type.
///
/// This is the `Plugin` impl that ties together [`ChainShared`],
/// [`ChainAudioProcessor`], and [`ChainMainThread`].
pub struct ChainPlugin;

impl Plugin for ChainPlugin {
    type AudioProcessor<'a> = ChainAudioProcessor<'a>;
    type Shared<'a> = ChainShared;
    type MainThread<'a> = ChainMainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&ChainShared>) {
        builder.register::<PluginAudioPorts>();
        builder.register::<PluginGui>();
        builder.register::<PluginLatency>();
        builder.register::<PluginParams>();
        builder.register::<PluginState>();
    }
}

impl DefaultPluginFactory for ChainPlugin {
    fn get_descriptor() -> PluginDescriptor {
        use clack_plugin::plugin::features::{AUDIO_EFFECT, MULTI_EFFECTS, STEREO};
        PluginDescriptor::new("com.sonido.chain", "Sonido Chain").with_features([
            AUDIO_EFFECT,
            MULTI_EFFECTS,
            STEREO,
        ])
    }

    fn new_shared(host: HostSharedHandle<'_>) -> Result<ChainShared, PluginError> {
        // SAFETY: CLAP specification §plugin-instance guarantees the host outlives
        // the plugin. We extend the lifetime to 'static to store in closures.
        #[allow(unsafe_code)]
        let host: HostSharedHandle<'static> = unsafe { core::mem::transmute(host) };

        tracing::info!("chain plugin instance created");

        let request_process: Box<dyn Fn() + Send + Sync> = Box::new(move || {
            host.request_process();
        });
        let request_callback: Box<dyn Fn() + Send + Sync> = Box::new(move || {
            host.request_callback();
        });

        Ok(ChainShared::new(
            Some(request_process),
            Some(request_callback),
        ))
    }

    fn new_main_thread<'a>(
        _host: HostMainThreadHandle<'a>,
        shared: &'a ChainShared,
    ) -> Result<ChainMainThread<'a>, PluginError> {
        Ok(ChainMainThread::new(shared))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clap_param_id_roundtrip() {
        for slot in 0..MAX_SLOTS {
            for param in 0..SLOT_STRIDE {
                let id = ClapParamId::new(slot, param).unwrap();
                assert_eq!(id.slot(), slot);
                assert_eq!(id.param(), param);
                assert_eq!(id.raw() as usize, slot * SLOT_STRIDE + param);

                // from_raw roundtrip
                let id2 = ClapParamId::from_raw(id.raw()).unwrap();
                assert_eq!(id, id2);
            }
        }
    }

    #[test]
    fn clap_param_id_bounds() {
        assert!(ClapParamId::new(MAX_SLOTS, 0).is_none());
        assert!(ClapParamId::new(0, SLOT_STRIDE).is_none());
        assert!(ClapParamId::new(MAX_SLOTS, SLOT_STRIDE).is_none());
        assert!(ClapParamId::from_raw(TOTAL_PARAMS as u32).is_none());
    }

    #[test]
    fn clap_param_id_edge_cases() {
        // First valid
        let first = ClapParamId::new(0, 0).unwrap();
        assert_eq!(first.raw(), 0);

        // Last valid
        let last = ClapParamId::new(MAX_SLOTS - 1, SLOT_STRIDE - 1).unwrap();
        assert_eq!(last.raw(), (TOTAL_PARAMS - 1) as u32);
        assert_eq!(last.slot(), MAX_SLOTS - 1);
        assert_eq!(last.param(), SLOT_STRIDE - 1);
    }

    #[test]
    fn constants_consistent() {
        assert_eq!(TOTAL_PARAMS, MAX_SLOTS * SLOT_STRIDE);
        assert_eq!(TOTAL_PARAMS, 512);
    }
}
