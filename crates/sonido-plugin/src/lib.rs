//! CLAP plugin adapter for sonido audio effects.
//!
//! This crate bridges sonido's `Effect` + `ParameterInfo` traits to the CLAP
//! plugin format via the `clack-plugin` safe wrapper. Each effect becomes an
//! independent `.clap` plugin binary.
//!
//! # Architecture
//!
//! The adapter maps sonido types directly to CLAP concepts:
//!
//! | Sonido | CLAP |
//! |--------|------|
//! | `ParamId(u32)` | `clap_id` |
//! | `ParamDescriptor::normalize()` | `value_to_normalized()` |
//! | `ParamDescriptor::format_value()` | `value_to_text()` |
//! | `ParamFlags::AUTOMATABLE` | `CLAP_PARAM_IS_AUTOMATABLE` |
//! | `ParamBridge::begin_set/end_set` | `request_begin_adjust` |
//!
//! # Plugin binary generation
//!
//! Each effect is a separate `cdylib` example target. Use the
//! [`sonido_effect_entry!`] macro to generate the CLAP entry point:
//!
//! ```rust,ignore
//! use sonido_plugin::sonido_effect_entry;
//!
//! sonido_effect_entry! {
//!     effect_id: "distortion",
//!     clap_id: "com.sonido.distortion",
//!     name: "Sonido Distortion",
//!     features: [AUDIO_EFFECT, DISTORTION, STEREO],
//! }
//! ```

pub mod audio;
pub mod chain;
mod egui_bridge;
pub mod gui;
pub mod main_thread;
pub mod shared;

pub use audio::SonidoAudioProcessor;
pub use main_thread::SonidoMainThread;
pub use shared::SonidoShared;

/// Generate a complete CLAP plugin entry point for a sonido effect.
///
/// This macro creates a zero-sized plugin type, implements `Plugin` and
/// `DefaultPluginFactory`, and exports the `clap_entry` symbol.
///
/// # Arguments
///
/// - `effect_id` — Registry ID (e.g., `"distortion"`, `"reverb"`)
/// - `clap_id` — Reverse-DNS plugin identifier
/// - `name` — Human-readable plugin name
/// - `features` — CLAP feature tags (from `clack_plugin::plugin::features`)
///
/// # Example
///
/// ```rust,ignore
/// sonido_effect_entry! {
///     effect_id: "distortion",
///     clap_id: "com.sonido.distortion",
///     name: "Sonido Distortion",
///     features: [AUDIO_EFFECT, DISTORTION, STEREO],
/// }
/// ```
#[macro_export]
macro_rules! sonido_effect_entry {
    (
        effect_id: $effect_id:literal,
        clap_id: $clap_id:literal,
        name: $name:literal,
        features: [$($feature:ident),+ $(,)?] $(,)?
    ) => {
        struct SonidoPlugin;

        impl ::clack_plugin::prelude::Plugin for SonidoPlugin {
            type AudioProcessor<'a> = $crate::SonidoAudioProcessor<'a>;
            type Shared<'a> = $crate::SonidoShared;
            type MainThread<'a> = $crate::SonidoMainThread<'a>;

            fn declare_extensions(
                builder: &mut ::clack_plugin::prelude::PluginExtensions<Self>,
                _shared: Option<&$crate::SonidoShared>,
            ) {
                use ::clack_extensions::audio_ports::PluginAudioPorts;
                use ::clack_extensions::gui::PluginGui;
                use ::clack_extensions::latency::PluginLatency;
                use ::clack_extensions::params::PluginParams;
                use ::clack_extensions::state::PluginState;

                builder.register::<PluginAudioPorts>();
                builder.register::<PluginGui>();
                builder.register::<PluginLatency>();
                builder.register::<PluginParams>();
                builder.register::<PluginState>();
            }
        }

        impl ::clack_plugin::prelude::DefaultPluginFactory for SonidoPlugin {
            fn get_descriptor() -> ::clack_plugin::prelude::PluginDescriptor {
                use ::clack_plugin::plugin::features::*;
                ::clack_plugin::prelude::PluginDescriptor::new($clap_id, $name)
                    .with_features([$($feature),+])
            }

            fn new_shared(
                host: ::clack_plugin::prelude::HostSharedHandle<'_>,
            ) -> Result<$crate::SonidoShared, ::clack_plugin::prelude::PluginError> {
                // CLAP specification §plugin-instance guarantees the host outlives the
                // plugin. HostSharedHandle is Copy + repr(transparent) over
                // NonNull<clap_host>; the lifetime is purely phantom.
                let host: ::clack_plugin::prelude::HostSharedHandle<'static> =
                    unsafe { ::core::mem::transmute(host) };

                ::tracing::info!(effect_id = $effect_id, "plugin instance created");

                let notify: Box<dyn Fn() + Send + Sync> =
                    Box::new(move || { host.request_process(); });
                Ok($crate::SonidoShared::new($effect_id, Some(notify)))
            }

            fn new_main_thread<'a>(
                _host: ::clack_plugin::prelude::HostMainThreadHandle<'a>,
                shared: &'a $crate::SonidoShared,
            ) -> Result<$crate::SonidoMainThread<'a>, ::clack_plugin::prelude::PluginError> {
                Ok($crate::SonidoMainThread::new(shared))
            }
        }

        ::clack_plugin::clack_export_entry!(
            ::clack_plugin::prelude::SinglePluginEntry<SonidoPlugin>
        );
    };
}
