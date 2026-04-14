//! Parameter introspection system for discoverable effect parameters.
//!
//! This module provides the [`ParameterInfo`] trait and supporting types that enable
//! runtime discovery and manipulation of effect parameters. This is essential for:
//!
//! - **GUI applications**: Automatically generate parameter controls
//! - **Hardware controllers**: Map MIDI CC or encoder knobs to parameters
//! - **Preset systems**: Save and restore parameter state
//! - **Host automation**: DAW parameter automation and CLAP/VST3 integration
//!
//! # Design
//!
//! The system uses index-based parameter access for efficiency and simplicity.
//! Each parameter is described by a [`ParamDescriptor`] containing metadata for
//! display, validation, and plugin host communication. Parameters also carry:
//!
//! - [`ParamId`] — stable numeric ID for automation recording and preset persistence
//! - [`ParamScale`] — normalization curve (linear, logarithmic, power)
//! - [`ParamFlags`] — capability flags for plugin hosts (automatable, stepped, etc.)
//! - `string_id` — human-readable stable ID for debugging and serialization
//! - `group` — parameter grouping for host tree display
//!
//! # Example
//!
//! ```rust
//! use sonido_core::{ParameterInfo, ParamDescriptor, ParamUnit, ParamId, ParamScale, ParamFlags};
//!
//! struct SimpleGain {
//!     gain_db: f32,
//! }
//!
//! impl ParameterInfo for SimpleGain {
//!     fn param_count(&self) -> usize { 1 }
//!
//!     fn param_info(&self, index: usize) -> Option<ParamDescriptor> {
//!         match index {
//!             0 => Some(ParamDescriptor::gain_db("Gain", "Gain", -60.0, 12.0, 0.0)
//!                 .with_id(ParamId(100), "gain_level")),
//!             _ => None,
//!         }
//!     }
//!
//!     fn get_param(&self, index: usize) -> f32 {
//!         match index {
//!             0 => self.gain_db,
//!             _ => 0.0,
//!         }
//!     }
//!
//!     fn set_param(&mut self, index: usize, value: f32) {
//!         match index {
//!             0 => self.gain_db = value.clamp(-60.0, 12.0),
//!             _ => {}
//!         }
//!     }
//! }
//! ```
//!
//! # no_std Support
//!
//! This module is fully `no_std` compatible with no heap allocations required.

/// Generates a [`ParameterInfo`] implementation from a declarative parameter table.
///
/// Eliminates boilerplate match arms for `param_count`, `param_info`, `get_param`,
/// and `set_param`. Auto-clamps setter values to descriptor bounds.
///
/// # Syntax
///
/// ```ignore
/// impl_params! {
///     EffectType, this {
///         [0] ParamDescriptor::gain_db("Drive", "Drive", 0.0, 40.0, 12.0)
///                 .with_id(ParamId(200), "dist_drive"),
///             get: this.drive_db(),
///             set: |v| this.set_drive_db(v);
///     }
/// }
/// ```
///
/// - `this` — binding name for `self` (required due to Rust 2024 macro hygiene)
/// - `[index]` — stable parameter index (0-based, explicit, not auto-numbered)
/// - `descriptor_expr` — expression returning [`ParamDescriptor`]
/// - `getter_expr` — expression evaluating to `f32` (use the binding name, not `self`)
/// - `|val| setter_expr` — `val` receives the clamped value; use the binding name
///
/// # Auto-Clamping
///
/// `set_param` automatically clamps the incoming value to the descriptor's
/// `[min, max]` range before passing it to the setter expression. This
/// eliminates manual `.clamp(min, max)` calls and ensures bounds stay in
/// sync with the descriptor.
///
/// # Example
///
/// ```rust
/// use sonido_core::{impl_params, ParamDescriptor, ParamId, ParameterInfo};
///
/// struct MyEffect {
///     gain_db: f32,
///     mix: f32,
/// }
///
/// impl_params! {
///     MyEffect, this {
///         [0] ParamDescriptor::gain_db("Gain", "Gain", -60.0, 12.0, 0.0)
///                 .with_id(ParamId(100), "my_gain"),
///             get: this.gain_db,
///             set: |v| this.gain_db = v;
///
///         [1] ParamDescriptor::mix()
///                 .with_id(ParamId(101), "my_mix"),
///             get: this.mix,
///             set: |v| this.mix = v;
///     }
/// }
///
/// let mut fx = MyEffect { gain_db: 0.0, mix: 50.0 };
/// assert_eq!(fx.param_count(), 2);
/// fx.set_param(0, 100.0); // auto-clamped to 12.0
/// assert_eq!(fx.get_param(0), 12.0);
/// ```
#[macro_export]
macro_rules! impl_params {
    (
        $effect:ty, $this:ident {
            $(
                [$idx:literal] $desc:expr,
                    get: $getter:expr,
                    set: |$val:ident| $setter:expr ;
            )*
        }
    ) => {
        impl $crate::ParameterInfo for $effect {
            #[inline]
            fn param_count(&self) -> usize {
                0usize $( + { let _ = $idx; 1usize } )*
            }

            fn param_info(&self, index: usize) -> ::core::option::Option<$crate::ParamDescriptor> {
                match index {
                    $( $idx => ::core::option::Option::Some($desc), )*
                    _ => ::core::option::Option::None,
                }
            }

            fn get_param(&self, index: usize) -> f32 {
                let $this = self;
                match index {
                    $( $idx => $getter, )*
                    _ => 0.0,
                }
            }

            fn set_param(&mut self, index: usize, value: f32) {
                let $this = self;
                match index {
                    $(
                        $idx => {
                            let $val = { let _desc = $desc; _desc.clamp(value) };
                            $setter;
                        }
                    )*
                    _ => {}
                }
            }
        }
    };
}

#[cfg(not(feature = "std"))]
use alloc::{format, string::String};

/// Scaling curve for parameter normalization.
///
/// Determines how a parameter's plain value maps to normalized \[0.0, 1.0\] space.
/// Linear is default. Use Logarithmic for frequency parameters (20 Hz–20 kHz),
/// Power for parameters that need more resolution at one end.
///
/// # Normalization Formulas
///
/// - **Linear**: `normalized = (value - min) / (max - min)`
/// - **Logarithmic**: `normalized = ln(value/min) / ln(max/min)`
/// - **Power(exp)**: `normalized = ((value - min) / (max - min)).powf(1.0 / exp)`
///
/// Reference: JUCE `NormalisableRange` (skew factor), iPlug2 `ShapePowCurve`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[non_exhaustive]
pub enum ParamScale {
    /// Linear mapping (default). Equal resolution across the range.
    #[default]
    Linear,
    /// Logarithmic mapping. More resolution at low values.
    /// Ideal for frequency parameters (20 Hz → 20 kHz).
    /// Requires `min > 0.0`.
    Logarithmic,
    /// Power curve mapping with configurable exponent.
    /// exponent < 1.0 → more resolution at low end.
    /// exponent > 1.0 → more resolution at high end.
    /// Equivalent to JUCE's `NormalisableRange` skew factor.
    Power(f32),
}

/// Stable parameter identifier that survives reordering.
///
/// Used by plugin hosts for automation recording, preset save/restore,
/// and parameter mapping. Once assigned, a `ParamId` MUST NEVER change
/// for a given parameter — it's part of the public API contract.
///
/// Maps directly to CLAP `clap_id` and VST3 `ParamID`.
///
/// # Convention
///
/// Each effect gets a base ID; params are sequential from there:
/// - Distortion: 200, 201, 202, 203
/// - Reverb: 1500, 1501, 1502, ...
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ParamId(pub u32);

/// Parameter capability flags for plugin host communication.
///
/// Bitflag type that maps to CLAP `clap_param_info_flags` and
/// VST3 `ParameterInfo::flags`. Use [`union`](Self::union) to combine.
///
/// # Example
///
/// ```rust
/// use sonido_core::ParamFlags;
///
/// let flags = ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED);
/// assert!(flags.contains(ParamFlags::AUTOMATABLE));
/// assert!(flags.contains(ParamFlags::STEPPED));
/// assert!(!flags.contains(ParamFlags::HIDDEN));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParamFlags(u8);

impl ParamFlags {
    /// No flags set.
    pub const NONE: Self = Self(0);
    /// Host can automate this parameter (default for all params).
    pub const AUTOMATABLE: Self = Self(1 << 0);
    /// Parameter has discrete steps (enum-like, integer values).
    pub const STEPPED: Self = Self(1 << 1);
    /// Parameter should be hidden from generic host UI.
    pub const HIDDEN: Self = Self(1 << 2);
    /// Parameter is read-only (metering, display only).
    pub const READ_ONLY: Self = Self(1 << 3);
    /// Parameter supports non-destructive modulation (CLAP `CLAP_PARAM_IS_MODULATABLE`).
    ///
    /// Modulated value = base value + modulation offset. The base value is preserved
    /// when the modulation source stops. No VST3 equivalent — VST3 treats all
    /// parameter changes as automation.
    ///
    /// Requires `modulation_id` in the descriptor for CLAP host routing.
    pub const MODULATABLE: Self = Self(1 << 4);

    /// Returns `true` if all bits in `other` are set in `self`.
    #[inline]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Returns the union of two flag sets.
    #[inline]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl Default for ParamFlags {
    fn default() -> Self {
        Self::AUTOMATABLE
    }
}

/// Trait for effects that expose introspectable parameters.
///
/// Implement this trait to allow runtime discovery and manipulation of your
/// effect's parameters. This enables automatic GUI generation, preset systems,
/// and MIDI/hardware controller mapping.
///
/// # Parameter Indexing
///
/// Parameters are accessed by zero-based index. The index must be stable for
/// the lifetime of the effect instance. Use [`param_count`](Self::param_count)
/// to determine valid indices.
///
/// # Thread Safety
///
/// This trait does not require thread safety. If you need to access parameters
/// from multiple threads, wrap the effect in appropriate synchronization
/// primitives (e.g., `Mutex` or atomic parameters).
///
/// # Example
///
/// ```rust
/// use sonido_core::{ParameterInfo, ParamDescriptor, ParamId};
///
/// struct Compressor {
///     threshold_db: f32,
///     ratio: f32,
///     attack_ms: f32,
///     release_ms: f32,
/// }
///
/// impl ParameterInfo for Compressor {
///     fn param_count(&self) -> usize { 4 }
///
///     fn param_info(&self, index: usize) -> Option<ParamDescriptor> {
///         match index {
///             0 => Some(ParamDescriptor::gain_db("Threshold", "Thresh", -60.0, 0.0, -20.0)
///                 .with_id(ParamId(300), "comp_thresh")),
///             1 => Some(ParamDescriptor::time_ms("Ratio", "Ratio", 1.0, 20.0, 4.0)
///                 .with_id(ParamId(301), "comp_ratio")),
///             2 => Some(ParamDescriptor::time_ms("Attack", "Attack", 0.1, 100.0, 10.0)
///                 .with_id(ParamId(302), "comp_attack")),
///             3 => Some(ParamDescriptor::time_ms("Release", "Release", 10.0, 1000.0, 100.0)
///                 .with_id(ParamId(303), "comp_release")),
///             _ => None,
///         }
///     }
///
///     fn get_param(&self, index: usize) -> f32 {
///         match index {
///             0 => self.threshold_db,
///             1 => self.ratio,
///             2 => self.attack_ms,
///             3 => self.release_ms,
///             _ => 0.0,
///         }
///     }
///
///     fn set_param(&mut self, index: usize, value: f32) {
///         match index {
///             0 => self.threshold_db = value.clamp(-60.0, 0.0),
///             1 => self.ratio = value.clamp(1.0, 20.0),
///             2 => self.attack_ms = value.clamp(0.1, 100.0),
///             3 => self.release_ms = value.clamp(10.0, 1000.0),
///             _ => {}
///         }
///     }
/// }
/// ```
pub trait ParameterInfo {
    /// Returns the number of parameters this effect exposes.
    ///
    /// Valid parameter indices are `0..param_count()`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use sonido_core::ParameterInfo;
    ///
    /// fn iterate_params<T: ParameterInfo>(effect: &T) {
    ///     for i in 0..effect.param_count() {
    ///         if let Some(info) = effect.param_info(i) {
    ///             // Use parameter info
    ///         }
    ///     }
    /// }
    /// ```
    fn param_count(&self) -> usize;

    /// Returns the descriptor for the parameter at the given index.
    ///
    /// Returns `None` if `index >= param_count()`.
    ///
    /// # Arguments
    ///
    /// * `index` - Zero-based parameter index
    ///
    /// # Example
    ///
    /// ```rust
    /// use sonido_core::{ParameterInfo, ParamUnit};
    ///
    /// fn print_param_range<T: ParameterInfo>(effect: &T, index: usize) {
    ///     if let Some(desc) = effect.param_info(index) {
    ///         println!("{}: {} to {}", desc.name, desc.min, desc.max);
    ///     }
    /// }
    /// ```
    fn param_info(&self, index: usize) -> Option<ParamDescriptor>;

    /// Gets the current value of the parameter at the given index.
    ///
    /// Returns `0.0` if `index >= param_count()` (implementations should handle
    /// out-of-bounds gracefully).
    ///
    /// # Arguments
    ///
    /// * `index` - Zero-based parameter index
    ///
    /// # Example
    ///
    /// ```rust
    /// use sonido_core::ParameterInfo;
    ///
    /// fn save_preset<T: ParameterInfo>(effect: &T) -> Vec<f32> {
    ///     (0..effect.param_count())
    ///         .map(|i| effect.get_param(i))
    ///         .collect()
    /// }
    /// ```
    fn get_param(&self, index: usize) -> f32;

    /// Find a parameter index by name (case-insensitive).
    ///
    /// Matches against both [`ParamDescriptor::name`] and
    /// [`ParamDescriptor::short_name`].
    ///
    /// # Returns
    ///
    /// `Some(index)` if found, `None` if no parameter matches.
    fn find_param_by_name(&self, name: &str) -> Option<usize> {
        for i in 0..self.param_count() {
            if let Some(desc) = self.param_info(i)
                && (desc.name.eq_ignore_ascii_case(name)
                    || desc.short_name.eq_ignore_ascii_case(name))
            {
                return Some(i);
            }
        }
        None
    }

    /// Sets the value of the parameter at the given index.
    ///
    /// Implementations should clamp the value to the valid range specified
    /// in the parameter descriptor. Out-of-bounds indices should be ignored.
    ///
    /// # Arguments
    ///
    /// * `index` - Zero-based parameter index
    /// * `value` - New parameter value (will be clamped to valid range)
    ///
    /// # Example
    ///
    /// ```rust
    /// use sonido_core::ParameterInfo;
    ///
    /// fn load_preset<T: ParameterInfo>(effect: &mut T, values: &[f32]) {
    ///     for (i, &value) in values.iter().enumerate() {
    ///         effect.set_param(i, value);
    ///     }
    /// }
    /// ```
    fn set_param(&mut self, index: usize, value: f32);

    /// Returns the stable [`ParamId`] for the parameter at the given index.
    ///
    /// Default implementation reads it from the descriptor. Returns `None`
    /// if the index is out of range.
    fn param_id(&self, index: usize) -> Option<ParamId> {
        self.param_info(index).map(|d| d.id)
    }

    /// Finds a parameter index by its stable [`ParamId`].
    ///
    /// Scans all parameters (O(n)) — suitable for setup paths, not audio.
    fn param_index_by_id(&self, id: ParamId) -> Option<usize> {
        (0..self.param_count()).find(|&i| self.param_info(i).is_some_and(|d| d.id == id))
    }
}

/// Describes a single parameter's metadata for display and validation.
///
/// This struct provides all the information needed to:
/// - Display the parameter in a GUI or on hardware
/// - Validate parameter values
/// - Convert between normalized (0.0-1.0) and actual values
/// - Format values with appropriate units
///
/// # Short Name
///
/// The `short_name` field should be 8 characters or less for compatibility
/// with hardware displays (e.g., LCD screens on MIDI controllers or guitar
/// pedals like DigiTech audioDNA units).
///
/// # Step Size
///
/// The `step` field indicates the recommended increment for encoder-based
/// control. For continuous parameters, use a small value like `0.01`. For
/// discrete parameters, use `1.0`.
///
/// # Example
///
/// ```rust
/// use sonido_core::{ParamDescriptor, ParamId};
///
/// let delay_time = ParamDescriptor::time_ms("Delay Time", "Time", 1.0, 2000.0, 250.0)
///     .with_id(ParamId(1100), "dly_time");
///
/// // Format for display
/// let value = 500.0;
/// println!("{}: {} ms", delay_time.name, value);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamDescriptor {
    /// Full parameter name for display (e.g., "Delay Time", "Feedback Level").
    pub name: &'static str,

    /// Short name for hardware displays, max 8 characters (e.g., "Time", "Feedback").
    ///
    /// Keep this concise for compatibility with LCD screens on MIDI controllers
    /// and hardware effects processors.
    pub short_name: &'static str,

    /// Unit type for formatting the parameter value.
    pub unit: ParamUnit,

    /// Minimum allowed value for this parameter.
    pub min: f32,

    /// Maximum allowed value for this parameter.
    pub max: f32,

    /// Default value when the effect is initialized or reset.
    pub default: f32,

    /// Recommended step increment for encoder-based control.
    ///
    /// Use small values (e.g., `0.01`) for continuous parameters and
    /// larger values (e.g., `1.0`) for discrete or coarse parameters.
    pub step: f32,

    /// Stable numeric ID for plugin host automation and preset persistence.
    ///
    /// Maps directly to CLAP `clap_id` and VST3 `ParamID`. Once assigned,
    /// this value must never change for a given parameter.
    /// Default: `ParamId(0)` (unassigned).
    pub id: ParamId,

    /// Human-readable stable ID for presets, debugging, and serialization.
    ///
    /// Convention: `"effect_param"` (e.g., `"dist_drive"`, `"rev_decay"`).
    /// Default: `""` (unassigned).
    pub string_id: &'static str,

    /// Normalization curve for mapping between plain and normalized values.
    ///
    /// Default: [`ParamScale::Linear`].
    pub scale: ParamScale,

    /// Capability flags for plugin host communication.
    ///
    /// Default: [`ParamFlags::AUTOMATABLE`].
    pub flags: ParamFlags,

    /// Parameter group for host tree display (e.g., `"filter"`, `"modulation"`).
    ///
    /// Empty string means top-level (ungrouped). Used by CLAP hosts to
    /// organize parameters hierarchically.
    pub group: &'static str,

    /// Optional modulation routing ID for CLAP hosts.
    ///
    /// When `Some`, the host can apply non-destructive modulation to this parameter.
    /// Must be unique across all modulatable parameters in the plugin.
    /// Mirrors nih-plug's `poly_modulation_id()` approach.
    pub modulation_id: Option<u32>,

    /// Step labels for discrete/enum parameters.
    ///
    /// When present, `labels[i]` maps to value `min + i * step`.
    /// Used by [`format_value()`](Self::format_value) for CLAP `value_to_text()`
    /// and VST3 parameter display.
    pub step_labels: Option<&'static [&'static str]>,
}

impl ParamDescriptor {
    /// Standard mix parameter (0–100%, default 50%).
    ///
    /// Used by most effects with a wet/dry blend control.
    pub const fn mix() -> Self {
        Self {
            name: "Mix",
            short_name: "Mix",
            unit: ParamUnit::Percent,
            min: 0.0,
            max: 100.0,
            default: 50.0,
            step: 1.0,
            id: ParamId(0),
            string_id: "",
            scale: ParamScale::Linear,
            flags: ParamFlags::AUTOMATABLE,
            group: "",
            modulation_id: None,
            step_labels: None,
        }
    }

    /// Standard depth parameter (0–100%, default 50%).
    ///
    /// Used by modulation effects (chorus, flanger, phaser, vibrato).
    pub const fn depth() -> Self {
        Self {
            name: "Depth",
            short_name: "Depth",
            unit: ParamUnit::Percent,
            min: 0.0,
            max: 100.0,
            default: 50.0,
            step: 1.0,
            id: ParamId(0),
            string_id: "",
            scale: ParamScale::Linear,
            flags: ParamFlags::AUTOMATABLE,
            group: "",
            modulation_id: None,
            step_labels: None,
        }
    }

    /// Standard feedback parameter (5–95%, default 50%).
    ///
    /// Used by delay-based effects (delay, flanger, chorus).
    /// Range 0–95%, capped at 95% to prevent runaway oscillation.
    pub const fn feedback() -> Self {
        Self {
            name: "Feedback",
            short_name: "Fdbk",
            unit: ParamUnit::Percent,
            min: 0.0,
            max: 95.0,
            default: 50.0,
            step: 1.0,
            id: ParamId(0),
            string_id: "",
            scale: ParamScale::Linear,
            flags: ParamFlags::AUTOMATABLE,
            group: "",
            modulation_id: None,
            step_labels: None,
        }
    }

    /// Time parameter with custom name and range (milliseconds).
    ///
    /// # Arguments
    ///
    /// * `name` - Full parameter name (e.g., "Delay Time")
    /// * `short_name` - Short name for hardware displays (e.g., "Time")
    /// * `min` - Minimum time in ms
    /// * `max` - Maximum time in ms
    /// * `default` - Default time in ms
    pub const fn time_ms(
        name: &'static str,
        short_name: &'static str,
        min: f32,
        max: f32,
        default: f32,
    ) -> Self {
        Self {
            name,
            short_name,
            unit: ParamUnit::Milliseconds,
            min,
            max,
            default,
            step: 1.0,
            id: ParamId(0),
            string_id: "",
            scale: ParamScale::Linear,
            flags: ParamFlags::AUTOMATABLE,
            group: "",
            modulation_id: None,
            step_labels: None,
        }
    }

    /// Gain parameter with custom name and range (decibels).
    ///
    /// # Arguments
    ///
    /// * `name` - Full parameter name (e.g., "Makeup Gain")
    /// * `short_name` - Short name for hardware displays (e.g., "Makeup")
    /// * `min` - Minimum gain in dB
    /// * `max` - Maximum gain in dB
    /// * `default` - Default gain in dB
    pub const fn gain_db(
        name: &'static str,
        short_name: &'static str,
        min: f32,
        max: f32,
        default: f32,
    ) -> Self {
        Self {
            name,
            short_name,
            unit: ParamUnit::Decibels,
            min,
            max,
            default,
            step: 0.5,
            id: ParamId(0),
            string_id: "",
            scale: ParamScale::Linear,
            flags: ParamFlags::AUTOMATABLE,
            group: "",
            modulation_id: None,
            step_labels: None,
        }
    }

    /// Standard LFO rate parameter in Hz.
    ///
    /// Uses logarithmic scaling for perceptually uniform rate control.
    ///
    /// # Arguments
    ///
    /// * `min` - Minimum rate in Hz
    /// * `max` - Maximum rate in Hz
    /// * `default` - Default rate in Hz
    pub const fn rate_hz(min: f32, max: f32, default: f32) -> Self {
        Self {
            name: "Rate",
            short_name: "Rate",
            unit: ParamUnit::Hertz,
            min,
            max,
            default,
            step: 0.05,
            id: ParamId(0),
            string_id: "",
            scale: ParamScale::Logarithmic,
            flags: ParamFlags::AUTOMATABLE,
            group: "",
            modulation_id: None,
            step_labels: None,
        }
    }

    /// Generic parameter constructor with neutral defaults.
    ///
    /// Use this for parameters that don't fit the common patterns
    /// (`mix()`, `depth()`, `gain_db()`, etc.). Provides sensible
    /// defaults for all metadata fields (unit: None, step: 0.01,
    /// scale: Linear, flags: AUTOMATABLE).
    ///
    /// Chain `.with_unit()`, `.with_scale()`, `.with_id()`, etc. to customize.
    pub const fn custom(
        name: &'static str,
        short_name: &'static str,
        min: f32,
        max: f32,
        default: f32,
    ) -> Self {
        Self {
            name,
            short_name,
            unit: ParamUnit::None,
            min,
            max,
            default,
            step: 0.01,
            id: ParamId(0),
            string_id: "",
            scale: ParamScale::Linear,
            flags: ParamFlags::AUTOMATABLE,
            group: "",
            modulation_id: None,
            step_labels: None,
        }
    }

    /// Sets the stable parameter ID and string ID.
    ///
    /// Builder pattern — call after a factory method or struct literal.
    ///
    /// # Example
    ///
    /// ```rust
    /// use sonido_core::{ParamDescriptor, ParamId};
    ///
    /// let desc = ParamDescriptor::mix().with_id(ParamId(700), "chor_mix");
    /// assert_eq!(desc.id, ParamId(700));
    /// assert_eq!(desc.string_id, "chor_mix");
    /// ```
    pub const fn with_id(mut self, id: ParamId, string_id: &'static str) -> Self {
        self.id = id;
        self.string_id = string_id;
        self
    }

    /// Sets the parameter unit type.
    ///
    /// Builder pattern — call after a factory method or struct literal.
    pub const fn with_unit(mut self, unit: ParamUnit) -> Self {
        self.unit = unit;
        self
    }

    /// Sets the step increment for encoder-based control.
    ///
    /// Builder pattern — call after a factory method or struct literal.
    pub const fn with_step(mut self, step: f32) -> Self {
        self.step = step;
        self
    }

    /// Sets the normalization scale.
    ///
    /// Builder pattern — call after a factory method or struct literal.
    pub const fn with_scale(mut self, scale: ParamScale) -> Self {
        self.scale = scale;
        self
    }

    /// Sets the parameter flags.
    ///
    /// Builder pattern — call after a factory method or struct literal.
    pub const fn with_flags(mut self, flags: ParamFlags) -> Self {
        self.flags = flags;
        self
    }

    /// Sets the parameter group.
    ///
    /// Builder pattern — call after a factory method or struct literal.
    pub const fn with_group(mut self, group: &'static str) -> Self {
        self.group = group;
        self
    }

    /// Sets step labels for discrete/enum parameters.
    ///
    /// Builder pattern — call after a factory method or struct literal.
    /// `labels[i]` corresponds to value `min + i * step`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use sonido_core::{ParamDescriptor, ParamUnit, ParamFlags};
    ///
    /// let desc = ParamDescriptor {
    ///     name: "Shape", short_name: "Shape",
    ///     unit: ParamUnit::None,
    ///     min: 0.0, max: 3.0, default: 0.0, step: 1.0,
    ///     ..ParamDescriptor::mix()
    /// }
    /// .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
    /// .with_step_labels(&["Soft Clip", "Hard Clip", "Foldback", "Asymmetric"]);
    /// assert_eq!(desc.format_value(0.0), "Soft Clip");
    /// assert_eq!(desc.format_value(2.0), "Foldback");
    /// ```
    pub const fn with_step_labels(mut self, labels: &'static [&'static str]) -> Self {
        self.step_labels = Some(labels);
        self
    }

    /// Sets the modulation routing ID for CLAP host integration.
    ///
    /// Builder pattern — call after a factory method or struct literal.
    /// Setting this also implies the parameter is modulatable.
    ///
    /// # Example
    ///
    /// ```rust
    /// use sonido_core::{ParamDescriptor, ParamId, ParamFlags};
    ///
    /// let desc = ParamDescriptor::mix()
    ///     .with_id(ParamId(700), "chor_mix")
    ///     .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::MODULATABLE))
    ///     .with_modulation_id(700);
    /// assert_eq!(desc.modulation_id, Some(700));
    /// ```
    pub const fn with_modulation_id(mut self, id: u32) -> Self {
        self.modulation_id = Some(id);
        self
    }

    /// Clamps a value to this parameter's valid range.
    ///
    /// # Example
    ///
    /// ```rust
    /// use sonido_core::ParamDescriptor;
    ///
    /// let desc = ParamDescriptor::gain_db("Gain", "Gain", -60.0, 12.0, 0.0);
    /// assert_eq!(desc.clamp(0.0), 0.0);
    /// assert_eq!(desc.clamp(-100.0), -60.0);
    /// assert_eq!(desc.clamp(100.0), 12.0);
    /// ```
    #[inline]
    pub fn clamp(&self, value: f32) -> f32 {
        if value < self.min {
            self.min
        } else if value > self.max {
            self.max
        } else {
            value
        }
    }

    /// Converts a plain value to normalized range (0.0 to 1.0).
    ///
    /// Respects the parameter's [`ParamScale`]:
    /// - **Linear**: `(value - min) / (max - min)`
    /// - **Logarithmic**: `ln(value/min) / ln(max/min)` — requires `min > 0`
    /// - **Power(exp)**: `((value - min) / (max - min)).powf(1.0 / exp)`
    ///
    /// # Example
    ///
    /// ```rust
    /// use sonido_core::ParamDescriptor;
    ///
    /// let desc = ParamDescriptor::mix();
    /// assert_eq!(desc.normalize(0.0), 0.0);
    /// assert_eq!(desc.normalize(50.0), 0.5);
    /// assert_eq!(desc.normalize(100.0), 1.0);
    /// ```
    #[inline]
    pub fn normalize(&self, value: f32) -> f32 {
        let range = self.max - self.min;
        if range == 0.0 {
            return 0.0;
        }
        match self.scale {
            ParamScale::Linear => (value - self.min) / range,
            ParamScale::Logarithmic => {
                if self.min <= 0.0 || value <= 0.0 {
                    return 0.0;
                }
                libm::logf(value / self.min) / libm::logf(self.max / self.min)
            }
            ParamScale::Power(exp) => {
                let linear = (value - self.min) / range;
                libm::powf(linear, 1.0 / exp)
            }
        }
    }

    /// Converts a normalized value (0.0 to 1.0) to the actual parameter range.
    ///
    /// Inverse of [`normalize`](Self::normalize), respecting [`ParamScale`].
    ///
    /// # Example
    ///
    /// ```rust
    /// use sonido_core::ParamDescriptor;
    ///
    /// let desc = ParamDescriptor::mix();
    /// assert_eq!(desc.denormalize(0.0), 0.0);
    /// assert_eq!(desc.denormalize(0.5), 50.0);
    /// assert_eq!(desc.denormalize(1.0), 100.0);
    /// ```
    #[inline]
    pub fn denormalize(&self, normalized: f32) -> f32 {
        match self.scale {
            ParamScale::Linear => self.min + normalized * (self.max - self.min),
            ParamScale::Logarithmic => {
                if self.min <= 0.0 {
                    return self.min;
                }
                self.min * libm::powf(self.max / self.min, normalized)
            }
            ParamScale::Power(exp) => {
                let curved = libm::powf(normalized, exp);
                self.min + curved * (self.max - self.min)
            }
        }
    }

    /// Adjusts `min` or `max` so that `denormalize(0.5)` equals `default`.
    ///
    /// The method shrinks whichever end of the range requires less reduction.
    /// STEPPED parameters are returned unchanged (discrete selectors have
    /// no meaningful "noon" position).
    ///
    /// # Panics
    ///
    /// Panics if the computed range is degenerate (max ≤ min), which happens
    /// when `default` equals `min` or `max`. For those "bypass-default" params,
    /// don't call this method — their extreme defaults are intentional.
    ///
    /// # Scale math
    ///
    /// - **Linear**: `noon = (min + max) / 2 = default` → pick `max = 2·default − min`
    ///   or `min = 2·default − max`, whichever shrinks the range.
    /// - **Logarithmic**: `noon = √(min · max) = default` → `min · max = default²`.
    ///   Solve for the bound that shrinks the range.
    /// - **Power(e)**: `noon = min + 0.5ᵉ · (max − min) = default` → solve for `max`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use sonido_core::ParamDescriptor;
    ///
    /// // Drive: range [0, 40], default 8 → noon_aligned shrinks to [0, 16]
    /// let d = ParamDescriptor::gain_db("Drive", "Drive", 0.0, 40.0, 8.0).noon_aligned();
    /// assert!((d.denormalize(0.5) - 8.0).abs() < 0.01);
    /// assert_eq!(d.min, 0.0);
    /// assert_eq!(d.max, 16.0);
    /// ```
    pub fn noon_aligned(self) -> Self {
        if self.flags.contains(ParamFlags::STEPPED) {
            return self;
        }
        let (new_min, new_max) = match self.scale {
            ParamScale::Linear => {
                let max_adj = 2.0 * self.default - self.min;
                let min_adj = 2.0 * self.default - self.max;
                if max_adj < self.max && max_adj > self.min {
                    (self.min, max_adj)
                } else if min_adj > self.min && min_adj < self.max {
                    (min_adj, self.max)
                } else {
                    (self.min, self.max)
                }
            }
            ParamScale::Logarithmic => {
                if self.min <= 0.0 || self.default <= 0.0 {
                    return self;
                }
                let d2 = self.default * self.default;
                let max_adj = d2 / self.min;
                let min_adj = d2 / self.max;
                if max_adj < self.max && max_adj > self.min {
                    (self.min, max_adj)
                } else if min_adj > self.min && min_adj < self.max {
                    (min_adj, self.max)
                } else {
                    (self.min, self.max)
                }
            }
            ParamScale::Power(exp) => {
                let factor = libm::powf(0.5, exp);
                if factor <= 0.0 {
                    return self;
                }
                let max_adj = self.min + (self.default - self.min) / factor;
                if max_adj > self.min {
                    (self.min, max_adj)
                } else {
                    (self.min, self.max)
                }
            }
        };
        assert!(
            new_max > new_min,
            "noon_aligned: default={} produces degenerate range [{new_min}, {new_max}]",
            self.default,
        );
        Self {
            min: new_min,
            max: new_max,
            ..self
        }
    }

    /// Formats a parameter value as human-readable text for DAW display.
    ///
    /// Stepped parameters with [`step_labels`](Self::step_labels) return the label
    /// string. Continuous parameters format by [`ParamUnit`] with appropriate
    /// precision and unit suffixes. Large values auto-scale (Hz → kHz, ms → s).
    ///
    /// Maps directly to CLAP `value_to_text()` and VST3 parameter display.
    ///
    /// # Example
    ///
    /// ```rust
    /// use sonido_core::ParamDescriptor;
    ///
    /// let desc = ParamDescriptor::gain_db("Gain", "Gain", -60.0, 12.0, 0.0);
    /// assert_eq!(desc.format_value(-6.0), "-6.0 dB");
    ///
    /// let desc = ParamDescriptor::rate_hz(20.0, 20000.0, 1000.0);
    /// assert_eq!(desc.format_value(1500.0), "1.5 kHz");
    /// ```
    pub fn format_value(&self, value: f32) -> String {
        if let Some(labels) = self.step_labels {
            let idx = ((value - self.min) / self.step.max(1.0)) as usize;
            return String::from(*labels.get(idx).unwrap_or(&"?"));
        }
        match self.unit {
            ParamUnit::Decibels => format!("{:.1} dB", value),
            ParamUnit::Hertz if value >= 1000.0 => format!("{:.1} kHz", value / 1000.0),
            ParamUnit::Hertz => format!("{:.0} Hz", value),
            ParamUnit::Milliseconds if value >= 1000.0 => format!("{:.2} s", value / 1000.0),
            ParamUnit::Milliseconds => format!("{:.0} ms", value),
            ParamUnit::Percent => format!("{:.0}%", value),
            ParamUnit::Ratio => format!("{:.1}:1", value),
            ParamUnit::None => format!("{:.2}", value),
        }
    }

    /// Parses human-readable text back to a parameter value.
    ///
    /// Handles step labels (case-insensitive match) and numeric values with
    /// unit suffixes. Auto-converts scaled units (kHz → Hz, s → ms).
    ///
    /// Maps directly to CLAP `text_to_value()` and VST3 string-to-parameter.
    ///
    /// # Example
    ///
    /// ```rust
    /// use sonido_core::ParamDescriptor;
    ///
    /// let desc = ParamDescriptor::gain_db("Gain", "Gain", -60.0, 12.0, 0.0);
    /// assert_eq!(desc.parse_value("-6.0 dB"), Some(-6.0));
    ///
    /// let desc = ParamDescriptor::rate_hz(20.0, 20000.0, 1000.0);
    /// assert_eq!(desc.parse_value("1.5 kHz"), Some(1500.0));
    /// ```
    pub fn parse_value(&self, text: &str) -> Option<f32> {
        if let Some(labels) = self.step_labels
            && let Some(i) = labels
                .iter()
                .position(|l| l.eq_ignore_ascii_case(text.trim()))
        {
            return Some(self.min + i as f32 * self.step.max(1.0));
        }
        // Strip known suffixes and parse
        let cleaned = text
            .trim()
            .trim_end_matches(" dB")
            .trim_end_matches(" Hz")
            .trim_end_matches(" kHz")
            .trim_end_matches(" ms")
            .trim_end_matches(" s")
            .trim_end_matches('%')
            .trim_end_matches(":1")
            .trim();
        let val: f32 = cleaned.parse().ok()?;
        // Handle kHz → Hz, s → ms conversions
        if text.contains("kHz") || (text.ends_with(" s") && !text.ends_with(" ms")) {
            Some(val * 1000.0)
        } else {
            Some(val)
        }
    }
}

/// Unit type for parameter display and formatting.
///
/// This enum helps GUI applications and hardware displays format parameter
/// values with appropriate units and precision.
///
/// # Example
///
/// ```rust
/// use sonido_core::ParamUnit;
///
/// fn format_value(value: f32, unit: ParamUnit) -> String {
///     match unit {
///         ParamUnit::Decibels => format!("{:.1} dB", value),
///         ParamUnit::Hertz => format!("{:.0} Hz", value),
///         ParamUnit::Milliseconds => format!("{:.0} ms", value),
///         ParamUnit::Percent => format!("{:.0}%", value),
///         ParamUnit::Ratio => format!("{:.1}:1", value),
///         ParamUnit::None => format!("{:.2}", value),
///         _ => format!("{:.2}", value),
///     }
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ParamUnit {
    /// Decibels (dB) - for gain, threshold, and level parameters.
    Decibels,

    /// Hertz (Hz) - for frequency parameters like filter cutoff or LFO rate.
    Hertz,

    /// Milliseconds (ms) - for time parameters like delay, attack, release.
    Milliseconds,

    /// Percentage (%) - for mix, blend, and normalized parameters.
    Percent,

    /// Ratio (n:1) - for compressor ratios and similar.
    Ratio,

    /// No unit - for dimensionless or custom parameters.
    None,
}

impl ParamUnit {
    /// Returns the unit suffix string for display.
    ///
    /// # Example
    ///
    /// ```rust
    /// use sonido_core::ParamUnit;
    ///
    /// assert_eq!(ParamUnit::Decibels.suffix(), " dB");
    /// assert_eq!(ParamUnit::Hertz.suffix(), " Hz");
    /// assert_eq!(ParamUnit::None.suffix(), "");
    /// ```
    pub const fn suffix(&self) -> &'static str {
        match self {
            ParamUnit::Decibels => " dB",
            ParamUnit::Hertz => " Hz",
            ParamUnit::Milliseconds => " ms",
            ParamUnit::Percent => "%",
            ParamUnit::Ratio => ":1",
            ParamUnit::None => "",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(feature = "std"))]
    extern crate alloc;
    #[cfg(not(feature = "std"))]
    use alloc::format;

    // Test struct for ParameterInfo implementation
    struct TestEffect {
        gain: f32,
        mix: f32,
    }

    impl TestEffect {
        fn new() -> Self {
            Self {
                gain: 0.0,
                mix: 50.0,
            }
        }
    }

    impl ParameterInfo for TestEffect {
        fn param_count(&self) -> usize {
            2
        }

        fn param_info(&self, index: usize) -> Option<ParamDescriptor> {
            match index {
                0 => Some(
                    ParamDescriptor::gain_db("Gain", "Gain", -60.0, 12.0, 0.0)
                        .with_id(ParamId(100), "test_gain"),
                ),
                1 => Some(ParamDescriptor::mix().with_id(ParamId(101), "test_mix")),
                _ => None,
            }
        }

        fn get_param(&self, index: usize) -> f32 {
            match index {
                0 => self.gain,
                1 => self.mix,
                _ => 0.0,
            }
        }

        fn set_param(&mut self, index: usize, value: f32) {
            match index {
                0 => {
                    if let Some(desc) = self.param_info(0) {
                        self.gain = desc.clamp(value);
                    }
                }
                1 => {
                    if let Some(desc) = self.param_info(1) {
                        self.mix = desc.clamp(value);
                    }
                }
                _ => {}
            }
        }
    }

    #[test]
    fn test_param_count() {
        let effect = TestEffect::new();
        assert_eq!(effect.param_count(), 2);
    }

    #[test]
    fn test_param_info() {
        let effect = TestEffect::new();

        let gain_info = effect.param_info(0).expect("should have gain param");
        assert_eq!(gain_info.name, "Gain");
        assert_eq!(gain_info.short_name, "Gain");
        assert_eq!(gain_info.unit, ParamUnit::Decibels);
        assert_eq!(gain_info.min, -60.0);
        assert_eq!(gain_info.max, 12.0);

        let mix_info = effect.param_info(1).expect("should have mix param");
        assert_eq!(mix_info.name, "Mix");
        assert_eq!(mix_info.unit, ParamUnit::Percent);

        assert!(effect.param_info(2).is_none());
        assert!(effect.param_info(100).is_none());
    }

    #[test]
    fn test_get_set_param() {
        let mut effect = TestEffect::new();

        assert_eq!(effect.get_param(0), 0.0);
        assert_eq!(effect.get_param(1), 50.0);

        effect.set_param(0, 6.0);
        assert_eq!(effect.get_param(0), 6.0);

        effect.set_param(1, 75.0);
        assert_eq!(effect.get_param(1), 75.0);
    }

    #[test]
    fn test_param_clamping() {
        let mut effect = TestEffect::new();

        // Test clamping to max
        effect.set_param(0, 100.0);
        assert_eq!(effect.get_param(0), 12.0);

        // Test clamping to min
        effect.set_param(0, -100.0);
        assert_eq!(effect.get_param(0), -60.0);

        // Test mix clamping
        effect.set_param(1, 150.0);
        assert_eq!(effect.get_param(1), 100.0);

        effect.set_param(1, -50.0);
        assert_eq!(effect.get_param(1), 0.0);
    }

    #[test]
    fn test_out_of_bounds_index() {
        let mut effect = TestEffect::new();

        // Out of bounds get should return 0.0
        assert_eq!(effect.get_param(99), 0.0);

        // Out of bounds set should do nothing
        effect.set_param(99, 42.0);
        // No panic, and existing params unchanged
        assert_eq!(effect.get_param(0), 0.0);
        assert_eq!(effect.get_param(1), 50.0);
    }

    #[test]
    fn test_descriptor_clamp() {
        let desc = ParamDescriptor::mix(); // 0..100
        assert_eq!(desc.clamp(50.0), 50.0);
        assert_eq!(desc.clamp(-10.0), 0.0);
        assert_eq!(desc.clamp(200.0), 100.0);
        assert_eq!(desc.clamp(0.0), 0.0);
        assert_eq!(desc.clamp(100.0), 100.0);
    }

    #[test]
    fn test_normalize_denormalize_linear() {
        let desc = ParamDescriptor::mix(); // 0..100, linear

        assert_eq!(desc.normalize(0.0), 0.0);
        assert_eq!(desc.normalize(50.0), 0.5);
        assert_eq!(desc.normalize(100.0), 1.0);

        assert_eq!(desc.denormalize(0.0), 0.0);
        assert_eq!(desc.denormalize(0.5), 50.0);
        assert_eq!(desc.denormalize(1.0), 100.0);

        // Round-trip
        let original = 73.0;
        let rt = desc.denormalize(desc.normalize(original));
        assert!((rt - original).abs() < 0.001);
    }

    #[test]
    fn test_normalize_denormalize_logarithmic() {
        let desc = ParamDescriptor::rate_hz(20.0, 20000.0, 1000.0); // logarithmic

        // Endpoints
        assert!((desc.normalize(20.0) - 0.0).abs() < 1e-6);
        assert!((desc.normalize(20000.0) - 1.0).abs() < 1e-6);

        // Midpoint in log space: sqrt(20 * 20000) ≈ 632.5
        let mid = desc.denormalize(0.5);
        let expected_mid = libm::sqrtf(20.0 * 20000.0);
        assert!(
            (mid - expected_mid).abs() < 1.0,
            "log midpoint: expected ~{expected_mid}, got {mid}"
        );

        // Round-trip
        for &val in &[20.0, 100.0, 1000.0, 5000.0, 20000.0] {
            let rt = desc.denormalize(desc.normalize(val));
            assert!(
                (rt - val).abs() / val < 1e-4,
                "log round-trip failed for {val}: got {rt}"
            );
        }
    }

    #[test]
    fn test_normalize_denormalize_power() {
        let desc = ParamDescriptor::depth().with_scale(ParamScale::Power(2.0));

        // Endpoints unchanged
        assert_eq!(desc.normalize(0.0), 0.0);
        assert_eq!(desc.normalize(100.0), 1.0);

        // Power(2): normalize(x) = sqrt(x/100), denormalize(n) = n^2 * 100
        let n = desc.normalize(25.0); // sqrt(0.25) = 0.5
        assert!(
            (n - 0.5).abs() < 1e-6,
            "power normalize: expected 0.5, got {n}"
        );

        let v = desc.denormalize(0.5); // 0.5^2 * 100 = 25
        assert!(
            (v - 25.0).abs() < 1e-4,
            "power denormalize: expected 25, got {v}"
        );

        // Round-trip
        for &val in &[0.0, 10.0, 25.0, 50.0, 75.0, 100.0] {
            let rt = desc.denormalize(desc.normalize(val));
            assert!(
                (rt - val).abs() < 0.01,
                "power round-trip failed for {val}: got {rt}"
            );
        }
    }

    #[test]
    fn test_normalize_zero_range() {
        let desc = ParamDescriptor::gain_db("Fixed", "Fixed", 42.0, 42.0, 42.0);
        assert_eq!(desc.normalize(42.0), 0.0);
    }

    #[test]
    fn test_param_unit_suffix() {
        assert_eq!(ParamUnit::Decibels.suffix(), " dB");
        assert_eq!(ParamUnit::Hertz.suffix(), " Hz");
        assert_eq!(ParamUnit::Milliseconds.suffix(), " ms");
        assert_eq!(ParamUnit::Percent.suffix(), "%");
        assert_eq!(ParamUnit::Ratio.suffix(), ":1");
        assert_eq!(ParamUnit::None.suffix(), "");
    }

    #[test]
    fn test_param_unit_debug() {
        let _ = format!("{:?}", ParamUnit::Decibels);
    }

    #[test]
    fn test_descriptor_debug_clone() {
        let desc = ParamDescriptor::depth();

        // Test Debug
        let _ = format!("{:?}", desc);

        // Test Clone
        let cloned = desc;
        assert_eq!(cloned.name, desc.name);

        // Test PartialEq
        assert_eq!(desc, cloned);
    }

    #[test]
    fn test_rate_hz_factory() {
        let desc = ParamDescriptor::rate_hz(0.05, 5.0, 0.5);
        assert_eq!(desc.name, "Rate");
        assert_eq!(desc.short_name, "Rate");
        assert_eq!(desc.unit, ParamUnit::Hertz);
        assert_eq!(desc.min, 0.05);
        assert_eq!(desc.max, 5.0);
        assert_eq!(desc.default, 0.5);
        assert_eq!(desc.step, 0.05);
        assert_eq!(desc.scale, ParamScale::Logarithmic);
    }

    #[test]
    fn test_param_id_lookup() {
        let effect = TestEffect::new();

        assert_eq!(effect.param_id(0), Some(ParamId(100)));
        assert_eq!(effect.param_id(1), Some(ParamId(101)));
        assert_eq!(effect.param_id(2), None);

        assert_eq!(effect.param_index_by_id(ParamId(100)), Some(0));
        assert_eq!(effect.param_index_by_id(ParamId(101)), Some(1));
        assert_eq!(effect.param_index_by_id(ParamId(999)), None);
    }

    #[test]
    fn test_param_flags() {
        assert!(ParamFlags::AUTOMATABLE.contains(ParamFlags::AUTOMATABLE));
        assert!(!ParamFlags::AUTOMATABLE.contains(ParamFlags::STEPPED));
        assert!(!ParamFlags::NONE.contains(ParamFlags::AUTOMATABLE));

        let combined = ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED);
        assert!(combined.contains(ParamFlags::AUTOMATABLE));
        assert!(combined.contains(ParamFlags::STEPPED));
        assert!(!combined.contains(ParamFlags::HIDDEN));
    }

    #[test]
    fn test_with_id_builder() {
        let desc = ParamDescriptor::mix().with_id(ParamId(42), "test_mix");
        assert_eq!(desc.id, ParamId(42));
        assert_eq!(desc.string_id, "test_mix");
        assert_eq!(desc.name, "Mix"); // unchanged
    }

    #[test]
    fn test_with_scale_builder() {
        let desc = ParamDescriptor::depth().with_scale(ParamScale::Power(3.0));
        assert_eq!(desc.scale, ParamScale::Power(3.0));
        assert_eq!(desc.name, "Depth"); // unchanged
    }

    #[test]
    fn test_with_flags_builder() {
        let desc =
            ParamDescriptor::mix().with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED));
        assert!(desc.flags.contains(ParamFlags::STEPPED));
    }

    #[test]
    fn test_defaults() {
        let desc = ParamDescriptor::mix();
        assert_eq!(desc.id, ParamId(0));
        assert_eq!(desc.string_id, "");
        assert_eq!(desc.scale, ParamScale::Linear);
        assert_eq!(desc.flags, ParamFlags::AUTOMATABLE);
        assert_eq!(desc.group, "");
        assert_eq!(desc.modulation_id, None);
        assert_eq!(desc.step_labels, None);
    }

    #[test]
    fn test_modulatable_flag() {
        let flags = ParamFlags::AUTOMATABLE.union(ParamFlags::MODULATABLE);
        assert!(flags.contains(ParamFlags::MODULATABLE));
        assert!(flags.contains(ParamFlags::AUTOMATABLE));
        assert!(!flags.contains(ParamFlags::STEPPED));
    }

    #[test]
    fn test_modulation_id_builder() {
        let desc = ParamDescriptor::mix().with_modulation_id(42);
        assert_eq!(desc.modulation_id, Some(42));

        let desc_none = ParamDescriptor::mix();
        assert_eq!(desc_none.modulation_id, None);
    }

    #[test]
    fn test_with_step_labels_builder() {
        let labels: &[&str] = &["Soft", "Hard", "Fold"];
        let desc = ParamDescriptor {
            name: "Shape",
            short_name: "Shape",
            unit: ParamUnit::None,
            min: 0.0,
            max: 2.0,
            default: 0.0,
            step: 1.0,
            ..ParamDescriptor::mix()
        }
        .with_step_labels(labels);
        assert_eq!(desc.step_labels, Some(labels));
    }

    #[test]
    fn test_format_value_decibels() {
        let desc = ParamDescriptor::gain_db("Gain", "Gain", -60.0, 12.0, 0.0);
        assert_eq!(desc.format_value(-6.0), "-6.0 dB");
        assert_eq!(desc.format_value(0.0), "0.0 dB");
        assert_eq!(desc.format_value(12.0), "12.0 dB");
    }

    #[test]
    fn test_format_value_hertz() {
        let desc = ParamDescriptor::rate_hz(20.0, 20000.0, 1000.0);
        assert_eq!(desc.format_value(440.0), "440 Hz");
        assert_eq!(desc.format_value(1500.0), "1.5 kHz");
        assert_eq!(desc.format_value(20000.0), "20.0 kHz");
    }

    #[test]
    fn test_format_value_milliseconds() {
        let desc = ParamDescriptor::time_ms("Time", "Time", 1.0, 2000.0, 250.0);
        assert_eq!(desc.format_value(250.0), "250 ms");
        assert_eq!(desc.format_value(1500.0), "1.50 s");
        assert_eq!(desc.format_value(5.0), "5 ms");
    }

    #[test]
    fn test_format_value_percent() {
        let desc = ParamDescriptor::mix();
        assert_eq!(desc.format_value(50.0), "50%");
        assert_eq!(desc.format_value(0.0), "0%");
        assert_eq!(desc.format_value(100.0), "100%");
    }

    #[test]
    fn test_format_value_ratio() {
        let desc = ParamDescriptor {
            unit: ParamUnit::Ratio,
            ..ParamDescriptor::mix()
        };
        assert_eq!(desc.format_value(4.0), "4.0:1");
        assert_eq!(desc.format_value(1.0), "1.0:1");
    }

    #[test]
    fn test_format_value_none() {
        let desc = ParamDescriptor {
            unit: ParamUnit::None,
            ..ParamDescriptor::mix()
        };
        assert_eq!(desc.format_value(0.5), "0.50");
    }

    #[test]
    fn test_format_value_step_labels() {
        let desc = ParamDescriptor {
            name: "Shape",
            short_name: "Shape",
            unit: ParamUnit::None,
            min: 0.0,
            max: 3.0,
            default: 0.0,
            step: 1.0,
            ..ParamDescriptor::mix()
        }
        .with_step_labels(&["Soft Clip", "Hard Clip", "Foldback", "Asymmetric"]);

        assert_eq!(desc.format_value(0.0), "Soft Clip");
        assert_eq!(desc.format_value(1.0), "Hard Clip");
        assert_eq!(desc.format_value(2.0), "Foldback");
        assert_eq!(desc.format_value(3.0), "Asymmetric");
        // Out of range returns "?"
        assert_eq!(desc.format_value(4.0), "?");
    }

    #[test]
    fn test_parse_value_decibels() {
        let desc = ParamDescriptor::gain_db("Gain", "Gain", -60.0, 12.0, 0.0);
        assert_eq!(desc.parse_value("-6.0 dB"), Some(-6.0));
        assert_eq!(desc.parse_value("0.0 dB"), Some(0.0));
        assert_eq!(desc.parse_value("-6.0"), Some(-6.0));
    }

    #[test]
    fn test_parse_value_hertz() {
        let desc = ParamDescriptor::rate_hz(20.0, 20000.0, 1000.0);
        assert_eq!(desc.parse_value("440 Hz"), Some(440.0));
        assert_eq!(desc.parse_value("1.5 kHz"), Some(1500.0));
    }

    #[test]
    fn test_parse_value_milliseconds() {
        let desc = ParamDescriptor::time_ms("Time", "Time", 1.0, 2000.0, 250.0);
        assert_eq!(desc.parse_value("250 ms"), Some(250.0));
        assert_eq!(desc.parse_value("1.5 s"), Some(1500.0));
    }

    #[test]
    fn test_parse_value_percent() {
        let desc = ParamDescriptor::mix();
        assert_eq!(desc.parse_value("50%"), Some(50.0));
    }

    #[test]
    fn test_parse_value_ratio() {
        let desc = ParamDescriptor {
            unit: ParamUnit::Ratio,
            ..ParamDescriptor::mix()
        };
        assert_eq!(desc.parse_value("4.0:1"), Some(4.0));
    }

    #[test]
    fn test_parse_value_step_labels() {
        let desc = ParamDescriptor {
            name: "Shape",
            short_name: "Shape",
            unit: ParamUnit::None,
            min: 0.0,
            max: 3.0,
            default: 0.0,
            step: 1.0,
            ..ParamDescriptor::mix()
        }
        .with_step_labels(&["Soft Clip", "Hard Clip", "Foldback", "Asymmetric"]);

        assert_eq!(desc.parse_value("Hard Clip"), Some(1.0));
        assert_eq!(desc.parse_value("hard clip"), Some(1.0)); // case-insensitive
        assert_eq!(desc.parse_value("Foldback"), Some(2.0));
        assert_eq!(desc.parse_value("Unknown"), None); // no numeric fallback
    }

    #[test]
    fn test_custom_factory() {
        let desc = ParamDescriptor::custom("Test Param", "Test", 0.0, 10.0, 5.0);
        assert_eq!(desc.name, "Test Param");
        assert_eq!(desc.short_name, "Test");
        assert_eq!(desc.unit, ParamUnit::None);
        assert_eq!(desc.min, 0.0);
        assert_eq!(desc.max, 10.0);
        assert_eq!(desc.default, 5.0);
        assert_eq!(desc.step, 0.01);
        assert_eq!(desc.id, ParamId(0));
        assert_eq!(desc.string_id, "");
        assert_eq!(desc.scale, ParamScale::Linear);
        assert_eq!(desc.flags, ParamFlags::AUTOMATABLE);
        assert_eq!(desc.group, "");
        assert_eq!(desc.modulation_id, None);
        assert_eq!(desc.step_labels, None);
    }

    #[test]
    fn test_with_unit_builder() {
        let desc = ParamDescriptor::custom("Freq", "Freq", 20.0, 20000.0, 1000.0)
            .with_unit(ParamUnit::Hertz);
        assert_eq!(desc.unit, ParamUnit::Hertz);
        assert_eq!(desc.name, "Freq"); // unchanged
    }

    #[test]
    fn test_with_step_builder() {
        let desc = ParamDescriptor::custom("Ratio", "Ratio", 1.0, 20.0, 4.0).with_step(0.1);
        assert_eq!(desc.step, 0.1);
        assert_eq!(desc.name, "Ratio"); // unchanged
    }

    #[test]
    fn test_parse_value_invalid() {
        let desc = ParamDescriptor::mix();
        assert_eq!(desc.parse_value("not a number"), None);
    }

    #[test]
    fn test_format_parse_roundtrip() {
        let desc = ParamDescriptor::gain_db("Gain", "Gain", -60.0, 12.0, 0.0);
        let text = desc.format_value(-6.0);
        assert_eq!(desc.parse_value(&text), Some(-6.0));

        let desc = ParamDescriptor::mix();
        let text = desc.format_value(75.0);
        assert_eq!(desc.parse_value(&text), Some(75.0));
    }

    // Test struct for impl_params! macro
    struct MacroTestEffect {
        gain: f32,
        mix: f32,
    }

    impl MacroTestEffect {
        fn new() -> Self {
            Self {
                gain: 0.0,
                mix: 50.0,
            }
        }
    }

    impl_params! {
        MacroTestEffect, this {
            [0] ParamDescriptor::gain_db("Gain", "Gain", -60.0, 12.0, 0.0)
                    .with_id(ParamId(100), "test_gain"),
                get: this.gain,
                set: |v| this.gain = v;

            [1] ParamDescriptor::mix()
                    .with_id(ParamId(101), "test_mix"),
                get: this.mix,
                set: |v| this.mix = v;
        }
    }

    #[test]
    fn test_impl_params_macro() {
        let mut fx = MacroTestEffect::new();
        assert_eq!(fx.param_count(), 2);
        assert_eq!(fx.get_param(0), 0.0);
        assert_eq!(fx.get_param(1), 50.0);

        // Auto-clamping
        fx.set_param(0, 100.0);
        assert_eq!(fx.get_param(0), 12.0);

        fx.set_param(0, -100.0);
        assert_eq!(fx.get_param(0), -60.0);

        // Out of bounds
        assert_eq!(fx.get_param(99), 0.0);
        fx.set_param(99, 42.0); // no panic

        // param_info
        let info = fx.param_info(0).unwrap();
        assert_eq!(info.name, "Gain");
        assert_eq!(info.id, ParamId(100));
        assert!(fx.param_info(99).is_none());
    }
}
