//! Sonido Harmonic Habitat — CLAP audio effect plugin.
//!
//! Pitch-aware modal reverb tank.

use sonido_plugin::sonido_effect_entry;

sonido_effect_entry! {
    effect_id: "harmonic_habitat",
    clap_id: "com.sonido.harmonic-habitat",
    name: "Sonido Harmonic Habitat",
    features: [AUDIO_EFFECT, REVERB, STEREO],
}
