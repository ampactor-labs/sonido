//! Effect registry and factory for sonido audio effects.
//!
//! This crate provides a centralized registry for discovering and instantiating
//! audio effects. It enables dynamic effect selection by name and provides
//! metadata for building user interfaces.
//!
//! # Features
//!
//! - **Effect Discovery**: List all available effects with metadata
//! - **Factory Pattern**: Create effects by name at runtime
//! - **Category System**: Effects organized by type (dynamics, distortion, etc.)
//! - **Parameter Info**: Access parameter descriptors for UI generation
//!
//! # Example
//!
//! ```rust
//! use sonido_registry::{EffectRegistry, EffectCategory};
//! use sonido_core::Effect;
//!
//! // Get the global registry
//! let registry = EffectRegistry::new();
//!
//! // List all effects
//! for effect in registry.all_effects() {
//!     println!("{}: {}", effect.name, effect.description);
//! }
//!
//! // Create an effect by name
//! if let Some(mut distortion) = registry.create("distortion", 48000.0) {
//!     let output = distortion.process(0.5);
//! }
//!
//! // Filter by category
//! for effect in registry.effects_in_category(EffectCategory::Modulation) {
//!     println!("Modulation effect: {}", effect.name);
//! }
//! ```
//!
//! # no_std Support
//!
//! This crate is `no_std` compatible. Disable the default `std` feature:
//!
//! ```toml
//! [dependencies]
//! sonido-registry = { version = "0.1", default-features = false }
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, vec::Vec};

// Re-export EffectWithParams from core (moved there to unblock ProcessingGraph).
pub use sonido_core::EffectWithParams;
use sonido_core::{KernelAdapter, ParamDescriptor};
use sonido_effects::kernels::{
    BitcrusherKernel, ChorusKernel, CompressorKernel, DelayKernel, DistortionKernel, EqKernel,
    FilterKernel, FlangerKernel, GateKernel, LimiterKernel, LooperKernel, PhaserKernel,
    PreampKernel, ReverbKernel, RingModKernel, StageKernel, TapeKernel, TremoloKernel,
    VibratoKernel, WahKernel,
};

/// Category of audio effect for organization and filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectCategory {
    /// Dynamics processing (compressor, limiter, gate)
    Dynamics,
    /// Distortion and saturation effects
    Distortion,
    /// Modulation effects (chorus, flanger, phaser, vibrato)
    Modulation,
    /// Time-based effects (delay, reverb)
    TimeBased,
    /// Filter effects (lowpass, highpass, etc.)
    Filter,
    /// Utility effects (gain, preamp)
    Utility,
}

impl EffectCategory {
    /// Returns a human-readable name for the category.
    pub const fn name(&self) -> &'static str {
        match self {
            EffectCategory::Dynamics => "Dynamics",
            EffectCategory::Distortion => "Distortion",
            EffectCategory::Modulation => "Modulation",
            EffectCategory::TimeBased => "Time-Based",
            EffectCategory::Filter => "Filter",
            EffectCategory::Utility => "Utility",
        }
    }

    /// Returns a description of the category.
    pub const fn description(&self) -> &'static str {
        match self {
            EffectCategory::Dynamics => {
                "Compressors, limiters, gates, and other dynamics processors"
            }
            EffectCategory::Distortion => {
                "Distortion, overdrive, saturation, and waveshaping effects"
            }
            EffectCategory::Modulation => {
                "Chorus, flanger, phaser, vibrato, and other modulation effects"
            }
            EffectCategory::TimeBased => "Delay, reverb, and other time-based effects",
            EffectCategory::Filter => "Lowpass, highpass, bandpass, and other filter effects",
            EffectCategory::Utility => "Gain stages, preamps, and utility processors",
        }
    }
}

/// Describes an effect in the registry.
#[derive(Debug, Clone)]
pub struct EffectDescriptor {
    /// Unique identifier for the effect (lowercase, no spaces).
    pub id: &'static str,
    /// Human-readable name.
    pub name: &'static str,
    /// Short display name for compact UI (e.g. "DIST", "COMP").
    pub short_name: &'static str,
    /// Brief description of the effect.
    pub description: &'static str,
    /// Category for organization.
    pub category: EffectCategory,
    /// Number of parameters.
    pub param_count: usize,
}

/// Factory function type for creating effects.
type EffectFactory = fn(f32) -> Box<dyn EffectWithParams + Send>;

/// Internal entry in the registry.
struct RegistryEntry {
    descriptor: EffectDescriptor,
    factory: EffectFactory,
    /// Cached parameter descriptors, populated once during registration.
    /// Avoids allocating a temporary effect instance in [`EffectRegistry::param_index_by_name()`].
    param_descriptors: Vec<ParamDescriptor>,
}

/// Registry of all available audio effects.
///
/// The registry provides a centralized way to discover and instantiate
/// audio effects by name. All built-in effects are automatically registered.
pub struct EffectRegistry {
    entries: Vec<RegistryEntry>,
}

impl Default for EffectRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectRegistry {
    /// Create a new registry with all built-in effects registered.
    pub fn new() -> Self {
        let mut registry = Self {
            entries: Vec::with_capacity(20),
        };
        registry.register_builtin_effects();
        registry
    }

    /// Register all built-in effects.
    fn register_builtin_effects(&mut self) {
        // Distortion
        self.register(
            EffectDescriptor {
                id: "distortion",
                name: "Distortion",
                short_name: "DIST",
                description: "Anti-aliased waveshaping distortion with multiple algorithms",
                category: EffectCategory::Distortion,
                param_count: 6,
            },
            |sr| Box::new(KernelAdapter::new(DistortionKernel::new(sr), sr)),
        );

        // Compressor
        self.register(
            EffectDescriptor {
                id: "compressor",
                name: "Compressor",
                short_name: "COMP",
                description: "Dynamics compressor with program-dependent release",
                category: EffectCategory::Dynamics,
                param_count: 12,
            },
            |sr| Box::new(KernelAdapter::new(CompressorKernel::new(sr), sr)),
        );

        // Chorus
        self.register(
            EffectDescriptor {
                id: "chorus",
                name: "Chorus",
                short_name: "CHOR",
                description: "Multi-voice modulated delay chorus with feedback and tempo sync",
                category: EffectCategory::Modulation,
                param_count: 10,
            },
            |sr| Box::new(KernelAdapter::new(ChorusKernel::new(sr), sr)),
        );

        // Flanger
        self.register(
            EffectDescriptor {
                id: "flanger",
                name: "Flanger",
                short_name: "FLNG",
                description: "Classic flanger with through-zero mode, bipolar feedback, and tempo sync",
                category: EffectCategory::Modulation,
                param_count: 9,
            },
            |sr| Box::new(KernelAdapter::new(FlangerKernel::new(sr), sr)),
        );

        // Phaser
        self.register(
            EffectDescriptor {
                id: "phaser",
                name: "Phaser",
                short_name: "PHAS",
                description: "Multi-stage allpass phaser with LFO and tempo sync",
                category: EffectCategory::Modulation,
                param_count: 11,
            },
            |sr| Box::new(KernelAdapter::new(PhaserKernel::new(sr), sr)),
        );

        // Delay
        self.register(
            EffectDescriptor {
                id: "delay",
                name: "Delay",
                short_name: "DLY",
                description: "Feedback delay with filtering, diffusion, and tempo sync",
                category: EffectCategory::TimeBased,
                param_count: 10,
            },
            |sr| Box::new(KernelAdapter::new(DelayKernel::new(sr), sr)),
        );

        // LowPass Filter
        self.register(
            EffectDescriptor {
                id: "filter",
                name: "Low Pass Filter",
                short_name: "FILT",
                description: "Multimode biquad filter (LPF/HPF/BPF/Notch)",
                category: EffectCategory::Filter,
                param_count: 4,
            },
            |sr| Box::new(KernelAdapter::new(FilterKernel::new(sr), sr)),
        );

        // Vibrato
        self.register(
            EffectDescriptor {
                id: "vibrato",
                name: "Vibrato",
                short_name: "VIB",
                description: "6-unit tape wow/flutter simulation",
                category: EffectCategory::Modulation,
                param_count: 3,
            },
            |sr| Box::new(KernelAdapter::new(VibratoKernel::new(sr), sr)),
        );

        // Tape Saturation
        self.register(
            EffectDescriptor {
                id: "tape",
                name: "Tape Saturation",
                short_name: "TAPE",
                description: "Full tape-machine model with wow/flutter, hysteresis, head bump, and self-erasure",
                category: EffectCategory::Distortion,
                param_count: 10,
            },
            |sr| Box::new(KernelAdapter::new(TapeKernel::new(sr), sr)),
        );

        // Clean Preamp
        self.register(
            EffectDescriptor {
                id: "preamp",
                name: "Clean Preamp",
                short_name: "PRE",
                description: "High-headroom gain stage",
                category: EffectCategory::Utility,
                param_count: 3,
            },
            |sr| Box::new(KernelAdapter::new(PreampKernel::new(sr), sr)),
        );

        // Reverb
        self.register(
            EffectDescriptor {
                id: "reverb",
                name: "Reverb",
                short_name: "VERB",
                description: "Freeverb-style algorithmic reverb",
                category: EffectCategory::TimeBased,
                param_count: 8,
            },
            |sr| Box::new(KernelAdapter::new(ReverbKernel::new(sr), sr)),
        );

        // Tremolo
        self.register(
            EffectDescriptor {
                id: "tremolo",
                name: "Tremolo",
                short_name: "TREM",
                description: "Amplitude modulation with multiple waveforms and tempo sync",
                category: EffectCategory::Modulation,
                param_count: 8,
            },
            |sr| Box::new(KernelAdapter::new(TremoloKernel::new(sr), sr)),
        );

        // Gate
        self.register(
            EffectDescriptor {
                id: "gate",
                name: "Noise Gate",
                short_name: "GATE",
                description: "Noise gate with threshold, hold, hysteresis, and sidechain HPF",
                category: EffectCategory::Dynamics,
                param_count: 9,
            },
            |sr| Box::new(KernelAdapter::new(GateKernel::new(sr), sr)),
        );

        // Wah
        self.register(
            EffectDescriptor {
                id: "wah",
                name: "Wah",
                short_name: "WAH",
                description: "Auto-wah and manual wah with envelope follower",
                category: EffectCategory::Filter,
                param_count: 5,
            },
            |sr| Box::new(KernelAdapter::new(WahKernel::new(sr), sr)),
        );

        // Parametric EQ
        self.register(
            EffectDescriptor {
                id: "eq",
                name: "Parametric EQ",
                short_name: "PEQ",
                description: "3-band parametric equalizer with frequency, gain, and Q",
                category: EffectCategory::Filter,
                param_count: 10,
            },
            |sr| Box::new(KernelAdapter::new(EqKernel::new(sr), sr)),
        );

        // Limiter
        self.register(
            EffectDescriptor {
                id: "limiter",
                name: "Limiter",
                short_name: "LIM",
                description: "Brickwall lookahead peak limiter with ceiling control",
                category: EffectCategory::Dynamics,
                param_count: 5,
            },
            |sr| Box::new(KernelAdapter::new(LimiterKernel::new(sr), sr)),
        );

        // Bitcrusher
        self.register(
            EffectDescriptor {
                id: "bitcrusher",
                name: "Bitcrusher",
                short_name: "CRSH",
                description: "Lo-fi bit depth and sample rate reduction with jitter",
                category: EffectCategory::Distortion,
                param_count: 5,
            },
            |sr| Box::new(KernelAdapter::new(BitcrusherKernel::new(sr), sr)),
        );

        // Ring Modulator
        self.register(
            EffectDescriptor {
                id: "ringmod",
                name: "Ring Modulator",
                short_name: "RING",
                description: "Ring modulation with sine, triangle, and square carriers",
                category: EffectCategory::Modulation,
                param_count: 5,
            },
            |sr| Box::new(KernelAdapter::new(RingModKernel::new(sr), sr)),
        );

        // Stage (signal conditioning / stereo utility)
        self.register(
            EffectDescriptor {
                id: "stage",
                name: "Stage",
                short_name: "STGE",
                description: "Signal conditioning: gain, phase, width, balance, bass mono, Haas delay",
                category: EffectCategory::Utility,
                param_count: 12,
            },
            |sr| Box::new(KernelAdapter::new(StageKernel::new(sr), sr)),
        );

        // Looper
        self.register(
            EffectDescriptor {
                id: "looper",
                name: "Looper",
                short_name: "LOOP",
                description: "Stereo looper with record, play, and overdub",
                category: EffectCategory::TimeBased,
                param_count: 6,
            },
            |sr| Box::new(KernelAdapter::new(LooperKernel::new(sr), sr)),
        );
    }

    /// Register an effect with the registry.
    ///
    /// Creates a temporary instance at 48 kHz to cache parameter descriptors,
    /// so that [`param_index_by_name()`](Self::param_index_by_name) can scan
    /// names without allocating at query time.
    fn register(&mut self, descriptor: EffectDescriptor, factory: EffectFactory) {
        let temp = factory(48000.0);
        let param_descriptors = (0..temp.effect_param_count())
            .filter_map(|i| temp.effect_param_info(i))
            .collect();
        self.entries.push(RegistryEntry {
            descriptor,
            factory,
            param_descriptors,
        });
    }

    /// Returns descriptors for all registered effects.
    pub fn all_effects(&self) -> Vec<&EffectDescriptor> {
        self.entries.iter().map(|e| &e.descriptor).collect()
    }

    /// Returns descriptors for effects in a specific category.
    pub fn effects_in_category(&self, category: EffectCategory) -> Vec<&EffectDescriptor> {
        self.entries
            .iter()
            .filter(|e| e.descriptor.category == category)
            .map(|e| &e.descriptor)
            .collect()
    }

    /// Get a descriptor by effect ID.
    pub fn get(&self, id: &str) -> Option<&EffectDescriptor> {
        self.entries
            .iter()
            .find(|e| e.descriptor.id == id)
            .map(|e| &e.descriptor)
    }

    /// Look up an effect descriptor by ID.
    ///
    /// Alias for [`get()`](Self::get) — provides semantically explicit access
    /// to the full descriptor including `short_name` and other metadata.
    pub fn descriptor(&self, id: &str) -> Option<&EffectDescriptor> {
        self.get(id)
    }

    /// Create an effect instance by ID.
    ///
    /// Returns `None` if the effect ID is not found. The returned effect
    /// supports both audio processing (via `Effect`) and parameter access
    /// (via `EffectWithParams`).
    pub fn create(&self, id: &str, sample_rate: f32) -> Option<Box<dyn EffectWithParams + Send>> {
        self.entries
            .iter()
            .find(|e| e.descriptor.id == id)
            .map(|e| (e.factory)(sample_rate))
    }

    /// Find a parameter index by name for a given effect type.
    ///
    /// Scans cached parameter descriptors — no allocation required.
    /// Returns `None` if the effect type or parameter name is not found.
    pub fn param_index_by_name(&self, effect_id: &str, param_name: &str) -> Option<usize> {
        let entry = self.entries.iter().find(|e| e.descriptor.id == effect_id)?;
        let lower = param_name.to_lowercase();
        for (i, desc) in entry.param_descriptors.iter().enumerate() {
            if desc.name.to_lowercase() == lower || desc.short_name.to_lowercase() == lower {
                return Some(i);
            }
        }
        None
    }

    /// Returns the number of registered effects.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true if no effects are registered.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Default effect chain in signal-flow order (gain stage → dynamics → EQ → modulation → time → reverb).
    ///
    /// Used by the GUI to initialize the effect chain. The ordering reflects
    /// standard guitar pedalboard convention.
    ///
    /// Excluded from the default chain (available via "Add Effect" in the GUI):
    /// - `limiter` — typically a mastering/bus effect, not a pedalboard staple
    /// - `bitcrusher` — niche lo-fi effect, not part of a standard signal chain
    /// - `ringmod` — experimental/sound-design effect
    /// - `stage` — stereo imaging utility, applied globally rather than per-chain
    pub fn default_chain_ids(&self) -> &'static [&'static str] {
        &[
            "preamp",     // Utility — gain stage
            "distortion", // Distortion
            "compressor", // Dynamics
            "gate",       // Dynamics
            "eq",         // Filter — tone shaping
            "wah",        // Filter — sweep
            "chorus",     // Modulation
            "flanger",    // Modulation
            "phaser",     // Modulation
            "tremolo",    // Modulation
            "delay",      // Time-based
            "filter",     // Filter — synth-style
            "vibrato",    // Modulation
            "tape",       // Distortion — saturation
            "reverb",     // Time-based — last
        ]
    }
}

// EffectWithParams trait + blanket impl are in sonido-core::effect_with_params.
// Re-exported above via `pub use sonido_core::EffectWithParams;`.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_creation() {
        let registry = EffectRegistry::new();
        assert_eq!(registry.len(), 20);
    }

    #[test]
    fn test_all_effects() {
        let registry = EffectRegistry::new();
        let effects = registry.all_effects();
        assert_eq!(effects.len(), 20);
    }

    #[test]
    fn test_get_effect() {
        let registry = EffectRegistry::new();

        let distortion = registry.get("distortion");
        assert!(distortion.is_some());
        assert_eq!(distortion.unwrap().name, "Distortion");

        let nonexistent = registry.get("nonexistent");
        assert!(nonexistent.is_none());
    }

    #[test]
    fn test_create_effect() {
        let registry = EffectRegistry::new();

        let effect = registry.create("distortion", 48000.0);
        assert!(effect.is_some());

        let mut effect = effect.unwrap();
        let output = effect.process(0.5);
        assert!(output.is_finite());
    }

    #[test]
    fn test_effects_by_category() {
        let registry = EffectRegistry::new();

        let modulation = registry.effects_in_category(EffectCategory::Modulation);
        assert_eq!(modulation.len(), 6); // Chorus, Flanger, Phaser, Vibrato, Tremolo, RingMod

        let dynamics = registry.effects_in_category(EffectCategory::Dynamics);
        assert_eq!(dynamics.len(), 3); // Compressor, Gate, Limiter

        let distortion = registry.effects_in_category(EffectCategory::Distortion);
        assert_eq!(distortion.len(), 3); // Distortion, Tape, Bitcrusher

        let time_based = registry.effects_in_category(EffectCategory::TimeBased);
        assert_eq!(time_based.len(), 3); // Delay, Reverb, Looper

        let filter = registry.effects_in_category(EffectCategory::Filter);
        assert_eq!(filter.len(), 3); // LowPass, Wah, ParametricEQ

        let utility = registry.effects_in_category(EffectCategory::Utility);
        assert_eq!(utility.len(), 2); // Preamp, Stage
    }

    #[test]
    fn test_category_names() {
        assert_eq!(EffectCategory::Dynamics.name(), "Dynamics");
        assert_eq!(EffectCategory::Modulation.name(), "Modulation");
    }

    #[test]
    fn test_effect_descriptor() {
        let registry = EffectRegistry::new();

        let reverb = registry.get("reverb").unwrap();
        assert_eq!(reverb.id, "reverb");
        assert_eq!(reverb.name, "Reverb");
        assert_eq!(reverb.short_name, "VERB");
        assert_eq!(reverb.category, EffectCategory::TimeBased);
        assert_eq!(reverb.param_count, 8);
    }

    #[test]
    fn test_descriptor_lookup() {
        let registry = EffectRegistry::new();
        let dist = registry.descriptor("distortion").unwrap();
        assert_eq!(dist.short_name, "DIST");
        let comp = registry.descriptor("compressor").unwrap();
        assert_eq!(comp.short_name, "COMP");
        assert!(registry.descriptor("nonexistent").is_none());
    }

    #[test]
    fn test_all_effects_have_short_names() {
        let registry = EffectRegistry::new();
        for desc in registry.all_effects() {
            assert!(
                !desc.short_name.is_empty(),
                "Effect {} has empty short_name",
                desc.id
            );
        }
    }

    #[test]
    fn test_all_effects_can_be_created() {
        let registry = EffectRegistry::new();

        for descriptor in registry.all_effects() {
            let effect = registry.create(descriptor.id, 48000.0);
            assert!(
                effect.is_some(),
                "Failed to create effect: {}",
                descriptor.id
            );

            let mut effect = effect.unwrap();
            let output = effect.process(0.5);
            assert!(
                output.is_finite(),
                "Effect {} produced non-finite output",
                descriptor.id
            );
        }
    }

    #[test]
    fn param_count_matches_implementation() {
        let registry = EffectRegistry::new();
        for descriptor in registry.all_effects() {
            let effect = registry.create(descriptor.id, 48000.0).unwrap();
            assert_eq!(
                descriptor.param_count,
                effect.effect_param_count(),
                "EffectDescriptor.param_count ({}) != ParameterInfo::param_count() ({}) for '{}'",
                descriptor.param_count,
                effect.effect_param_count(),
                descriptor.id,
            );
        }
    }

    /// Roundtrip test: create every registered effect, process an impulse followed
    /// by 1023 silence samples, verify all outputs are finite. Catches registration
    /// mismatches, uninitialized state, and NaN/inf propagation.
    #[test]
    fn all_registered_effects_process_finite_output() {
        let registry = EffectRegistry::new();
        for descriptor in registry.all_effects() {
            let id = descriptor.id;
            let mut effect = registry
                .create(id, 48000.0)
                .unwrap_or_else(|| panic!("Failed to create {id}"));

            // Impulse
            let out = effect.process(1.0);
            assert!(out.is_finite(), "{id}: non-finite output on impulse");

            // Silence tail — exposes feedback blowup and denormal issues
            for i in 0..1023 {
                let out = effect.process(0.0);
                assert!(
                    out.is_finite(),
                    "{id}: non-finite output on silence sample {i}"
                );
            }

            // Also verify stereo path
            let (l, r) = effect.process_stereo(1.0, 1.0);
            assert!(l.is_finite(), "{id}: non-finite left on stereo impulse");
            assert!(r.is_finite(), "{id}: non-finite right on stereo impulse");

            for i in 0..1023 {
                let (l, r) = effect.process_stereo(0.0, 0.0);
                assert!(
                    l.is_finite() && r.is_finite(),
                    "{id}: non-finite stereo output on silence sample {i}"
                );
            }
        }
    }
}
