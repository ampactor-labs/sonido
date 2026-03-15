//! Core traits for the kernel architecture.
//!
//! [`DspKernel`] defines pure DSP processing. [`KernelParams`] defines parameter
//! metadata and value access. [`SmoothingStyle`] specifies how each parameter
//! should be smoothed by the platform adapter.

use crate::param_info::ParamDescriptor;
use crate::tempo::TempoContext;

/// How a parameter should be smoothed when values change.
///
/// The kernel never sees smoothing — it receives pre-smoothed values each sample.
/// This enum tells the [`KernelAdapter`](super::KernelAdapter) (or any platform
/// layer) what smoothing strategy to use.
///
/// On embedded targets with hardware-filtered ADCs, the platform may ignore
/// this entirely and pass raw readings. That's the whole point.
///
/// The fixed tiers cover the vast majority of audio parameters. Use
/// [`Custom`](Self::Custom) when an effect needs a specific time constant
/// that falls between tiers.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum SmoothingStyle {
    /// No smoothing — snap to value immediately.
    ///
    /// Use for stepped/enum parameters (waveshape selection, on/off toggles)
    /// and on platforms with hardware-filtered inputs.
    None,

    /// 5 ms — drive, nonlinear gain, parameters where latency is audible.
    Fast,

    /// 10 ms — most parameters (rate, depth, mix, level).
    ///
    /// Good balance between responsiveness and click-free changes.
    #[default]
    Standard,

    /// 20 ms — filter coefficients, EQ, reverb decay.
    ///
    /// Prevents "zipper" artifacts on parameters that directly affect
    /// filter coefficient recalculation.
    Slow,

    /// 50 ms — delay time, predelay.
    ///
    /// Prevents pitch-shifting artifacts from delay line read pointer jumps.
    Interpolated,

    /// Arbitrary smoothing time in milliseconds.
    ///
    /// Use when the fixed tiers don't match. The recommended tiers exist
    /// for consistency — prefer them when possible, use this when you
    /// genuinely need a specific time constant.
    Custom(f32),
}

impl SmoothingStyle {
    /// Smoothing time in milliseconds.
    #[inline]
    pub fn time_ms(self) -> f32 {
        match self {
            Self::None => 0.0,
            Self::Fast => 5.0,
            Self::Standard => 10.0,
            Self::Slow => 20.0,
            Self::Interpolated => 50.0,
            Self::Custom(ms) => ms,
        }
    }
}

/// Typed parameter struct with metadata for a [`DspKernel`].
///
/// Implementors define a concrete struct (e.g., `DistortionParams`) whose fields
/// are parameter values in **user-facing units** (dB, %, Hz, enum index). The
/// trait provides indexed access for platform layers that need generic parameter
/// manipulation (GUIs, plugin hosts, presets, MIDI mapping).
///
/// # The Params Struct Is Everything
///
/// A `KernelParams` struct simultaneously serves as:
///
/// - **Processing input** — passed to [`DspKernel::process_stereo()`] each sample
/// - **Preset format** — clone it to save, restore it to load
/// - **Morph target** — [`lerp()`](Self::lerp) between any two snapshots
/// - **Serialization source** — indexed access for JSON/TOML/binary
/// - **Hardware mapping** — `from_knobs()` on each impl maps ADC readings
/// - **Host bridge** — [`from_normalized()`](Self::from_normalized) maps CLAP/MIDI values
///
/// One struct. No translation layers.
///
/// # Units Convention
///
/// Values are stored in the same units as `ParamDescriptor::min` / `max` / `default`:
/// - Gain parameters: decibels
/// - Mix/depth parameters: percent (0–100)
/// - Time parameters: milliseconds
/// - Stepped parameters: integer-valued float (0.0, 1.0, 2.0, ...)
///
/// The kernel converts to internal domain as needed (e.g., `db_to_linear()`).
/// This keeps the params struct self-documenting and consistent with the
/// descriptor metadata.
///
/// # Smoothing
///
/// Each parameter specifies its preferred [`SmoothingStyle`] via [`smoothing()`](Self::smoothing).
/// Platform adapters use this to configure per-parameter smoothers. The kernel
/// never sees smoothing state — it receives pre-smoothed values each sample.
///
/// # Example
///
/// ```rust
/// use sonido_core::kernel::{KernelParams, SmoothingStyle};
/// use sonido_core::{ParamDescriptor, ParamId};
///
/// #[derive(Debug, Clone, Copy)]
/// struct GainParams {
///     gain_db: f32,
/// }
///
/// impl Default for GainParams {
///     fn default() -> Self {
///         Self { gain_db: 0.0 }
///     }
/// }
///
/// impl KernelParams for GainParams {
///     const COUNT: usize = 1;
///
///     fn descriptor(index: usize) -> Option<ParamDescriptor> {
///         match index {
///             0 => Some(ParamDescriptor::gain_db("Gain", "Gain", -60.0, 12.0, 0.0)
///                     .with_id(ParamId(100), "gain_level")),
///             _ => None,
///         }
///     }
///
///     fn smoothing(index: usize) -> SmoothingStyle {
///         match index {
///             0 => SmoothingStyle::Standard,
///             _ => SmoothingStyle::Standard,
///         }
///     }
///
///     fn get(&self, index: usize) -> f32 {
///         match index {
///             0 => self.gain_db,
///             _ => 0.0,
///         }
///     }
///
///     fn set(&mut self, index: usize, value: f32) {
///         match index {
///             0 => self.gain_db = value,
///             _ => {}
///         }
///     }
/// }
/// ```
pub trait KernelParams: Default + Clone + Send {
    /// Number of parameters. Must be stable for the lifetime of the type.
    const COUNT: usize;

    /// Metadata for the parameter at `index`.
    ///
    /// Returns `None` if `index >= COUNT`.
    fn descriptor(index: usize) -> Option<ParamDescriptor>;

    /// Preferred smoothing style for the parameter at `index`.
    ///
    /// The platform adapter uses this to configure per-parameter smoothers.
    /// On embedded platforms, this may be ignored entirely.
    fn smoothing(index: usize) -> SmoothingStyle;

    /// Get parameter value in user-facing units.
    fn get(&self, index: usize) -> f32;

    /// Set parameter value in user-facing units.
    ///
    /// Implementors should NOT clamp here — the [`KernelAdapter`](super::KernelAdapter)
    /// clamps via the descriptor before calling this.
    fn set(&mut self, index: usize, value: f32);

    // ── Constructors ──

    /// Create a params struct with all values set to their descriptor defaults.
    fn from_defaults() -> Self {
        let mut params = Self::default();
        for i in 0..Self::COUNT {
            if let Some(desc) = Self::descriptor(i) {
                params.set(i, desc.default);
            }
        }
        params
    }

    /// Construct from normalized 0–1 values using descriptor denormalization.
    ///
    /// Maps through each parameter's [`ParamDescriptor::denormalize()`] — the
    /// same normalization curve used by CLAP hosts. This means host-normalized
    /// values, MIDI CC readings (0–127 scaled to 0–1), or any normalized source
    /// can construct a params struct directly.
    ///
    /// Values beyond `COUNT` are ignored. Missing values keep their defaults.
    fn from_normalized(values: &[f32]) -> Self {
        let mut params = Self::from_defaults();
        for (i, &v) in values.iter().take(Self::COUNT).enumerate() {
            if let Some(desc) = Self::descriptor(i) {
                params.set(i, desc.denormalize(v));
            }
        }
        params
    }

    // ── Serialization ──

    /// Export to normalized 0–1 values using descriptor normalization.
    ///
    /// Inverse of [`from_normalized()`](Self::from_normalized). Useful for
    /// serializing to formats that expect normalized values, or for sending
    /// to CLAP hosts.
    fn to_normalized(&self, output: &mut [f32]) {
        for i in 0..Self::COUNT.min(output.len()) {
            if let Some(desc) = Self::descriptor(i) {
                output[i] = desc.normalize(self.get(i));
            }
        }
    }

    // ── Morphing ──

    /// Linearly interpolate between two parameter snapshots.
    ///
    /// `t = 0.0` returns `a`, `t = 1.0` returns `b`. Stepped parameters
    /// snap at `t = 0.5` (no fractional enum values). Continuous parameters
    /// interpolate linearly in user-facing units.
    ///
    /// This is **preset morphing** — smoothly blend between any two states
    /// of an effect with a single parameter.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let clean = DistortionParams { drive_db: 3.0, mix_pct: 30.0, ..Default::default() };
    /// let heavy = DistortionParams { drive_db: 35.0, mix_pct: 100.0, ..Default::default() };
    ///
    /// // Morph 40% of the way from clean to heavy
    /// let morphed = DistortionParams::lerp(&clean, &heavy, 0.4);
    /// // morphed.drive_db ≈ 15.8, morphed.mix_pct ≈ 58.0
    /// ```
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let mut result = Self::from_defaults();
        for i in 0..Self::COUNT {
            let va = a.get(i);
            let vb = b.get(i);

            let value = if Self::descriptor(i)
                .is_some_and(|d| d.flags.contains(crate::ParamFlags::STEPPED))
            {
                // Stepped parameters: snap at midpoint, no fractional values
                if t < 0.5 { va } else { vb }
            } else {
                // Continuous parameters: linear interpolation
                va + (vb - va) * t
            };

            result.set(i, value);
        }
        result
    }
}

/// Pure DSP processing kernel — the invariant core of an audio effect.
///
/// A `DspKernel` contains ONLY the mutable state required for DSP computation:
/// filter memories, delay buffers, ADAA state, LFO phases, etc. It does NOT
/// contain parameter values, smoothers, or any platform-specific state.
///
/// Parameters are passed IN via `&Self::Params` on every processing call.
/// The kernel is a pure function: `(audio_in, params, state) → audio_out`.
///
/// # Stereo-First
///
/// Like [`Effect`](crate::Effect), the primary processing method is
/// [`process_stereo()`](Self::process_stereo). Implement either:
///
/// - `process()` for mono kernels (stereo derives by processing channels independently)
/// - `process_stereo()` for true stereo kernels (mono derives from left channel)
///
/// # Safety Contract
///
/// Implementors **must** override at least one of `process()` or `process_stereo()`.
/// Failing to override either causes infinite recursion (identical to [`Effect`](crate::Effect)).
///
/// # Block Processing
///
/// The default [`process_block_stereo()`](Self::process_block_stereo) calls
/// `process_stereo()` per sample with a single params snapshot. Override for
/// effects that benefit from block-level optimizations (vectorization, per-block
/// coefficient updates, etc.).
///
/// Note: The [`KernelAdapter`](super::KernelAdapter) does NOT call the kernel's
/// block methods — it calls `process_stereo()` per sample because it advances
/// smoothers each sample. The kernel's block methods are for **embedded use**
/// where params don't change mid-block.
///
/// # no_std
///
/// This trait imposes no `std` requirements. Kernels should use `libm` or
/// [`fast_math`](crate::fast_math) for transcendentals.
///
/// # Example
///
/// ```rust,ignore
/// use sonido_core::kernel::DspKernel;
/// use sonido_core::fast_db_to_linear;
///
/// struct GainKernel;
///
/// impl DspKernel for GainKernel {
///     type Params = GainParams;
///
///     fn process(&mut self, input: f32, params: &GainParams) -> f32 {
///         input * fast_db_to_linear(params.gain_db)
///     }
///
///     fn reset(&mut self) {}
///     fn set_sample_rate(&mut self, _sample_rate: f32) {}
/// }
/// ```
pub trait DspKernel: Send {
    /// The parameter struct for this kernel.
    type Params: KernelParams;

    /// Process a single mono sample.
    ///
    /// Default: derives from `process_stereo(input, input).0`.
    fn process(&mut self, input: f32, params: &Self::Params) -> f32 {
        self.process_stereo(input, input, params).0
    }

    /// Process a stereo sample pair.
    ///
    /// Default: processes channels independently via `process()`.
    fn process_stereo(&mut self, left: f32, right: f32, params: &Self::Params) -> (f32, f32) {
        (self.process(left, params), self.process(right, params))
    }

    /// Process a block of mono samples with a single parameter snapshot.
    ///
    /// Default: calls `process()` per sample. Override for vectorized
    /// processing on embedded targets.
    fn process_block(&mut self, input: &[f32], output: &mut [f32], params: &Self::Params) {
        debug_assert_eq!(input.len(), output.len());
        for i in 0..input.len() {
            output[i] = self.process(input[i], params);
        }
    }

    /// Process a block of stereo samples with a single parameter snapshot.
    ///
    /// Default: calls `process_stereo()` per sample. Override for vectorized
    /// processing or per-block coefficient updates on embedded targets.
    ///
    /// The parameter snapshot is constant for the entire block. On desktop,
    /// the [`KernelAdapter`](super::KernelAdapter) calls per-sample methods
    /// instead (advancing smoothers each sample). This block method is
    /// primarily for embedded use where params are stable across a block.
    fn process_block_stereo(
        &mut self,
        left_in: &[f32],
        right_in: &[f32],
        left_out: &mut [f32],
        right_out: &mut [f32],
        params: &Self::Params,
    ) {
        debug_assert_eq!(left_in.len(), right_in.len());
        debug_assert_eq!(left_in.len(), left_out.len());
        debug_assert_eq!(left_out.len(), right_out.len());
        for i in 0..left_in.len() {
            let (l, r) = self.process_stereo(left_in[i], right_in[i], params);
            left_out[i] = l;
            right_out[i] = r;
        }
    }

    /// Reset all internal DSP state (filter memories, delay buffers, etc.).
    ///
    /// Called when the host resets the plugin, or on sample rate change.
    fn reset(&mut self);

    /// Update internal state for a new sample rate.
    ///
    /// Recalculate filter coefficients, delay line sizes, etc.
    fn set_sample_rate(&mut self, sample_rate: f32);

    /// Report processing latency in samples.
    ///
    /// Used by the DAG engine for latency compensation across parallel paths.
    fn latency_samples(&self) -> usize {
        0
    }

    /// Whether this kernel has true stereo processing (cross-channel interaction).
    ///
    /// Mono kernels (independent L/R processing) return `false` (default).
    fn is_true_stereo(&self) -> bool {
        false
    }

    /// Receive tempo context for tempo-synced effects.
    ///
    /// Default: no-op. Override for delay, tremolo, and other tempo-aware kernels.
    fn set_tempo_context(&mut self, _ctx: &TempoContext) {}

    /// Report the effect's ring-out tail duration in samples.
    ///
    /// Used by [`KernelAdapter`](super::KernelAdapter) to implement
    /// [`TailReporting`](crate::TailReporting). Override for effects that
    /// produce output after input stops (reverb decay, delay feedback,
    /// looper playback).
    ///
    /// Default: 0 (no tail — most effects).
    fn tail_samples(&self) -> usize {
        0
    }

    /// Write READ_ONLY diagnostic values into the params snapshot.
    ///
    /// Called by [`KernelAdapter`](super::KernelAdapter) after each
    /// `process_stereo()` call. Override in kernels that have `ParamFlags::READ_ONLY`
    /// diagnostic parameters (gain reduction, gate state, LFO phase, etc.).
    ///
    /// The adapter then exposes these values through `ParameterInfo::get_param()`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// fn update_diagnostics(&self, params: &mut CompressorParams) {
    ///     params.gain_reduction_db = self.gain_reduction_db;
    /// }
    /// ```
    ///
    /// Default: no-op (most kernels have no READ_ONLY params).
    fn update_diagnostics(&self, _params: &mut Self::Params) {}

    /// Process a stereo sample pair with an external sidechain signal.
    ///
    /// Called by the graph engine when a sidechain edge is connected to this
    /// node. The `sc_left` / `sc_right` values are the sidechain channel samples
    /// for the current frame.
    ///
    /// Override in kernels that support external sidechain control (compressor,
    /// gate, de-esser). The default delegates to the regular
    /// [`process_stereo()`](Self::process_stereo), ignoring the sidechain input.
    ///
    /// # Parameters
    /// - `left` / `right`: Main signal input samples.
    /// - `sc_left` / `sc_right`: Sidechain signal input samples.
    /// - `params`: Kernel parameters snapshot.
    #[allow(unused_variables)]
    fn process_stereo_with_sidechain(
        &mut self,
        left: f32,
        right: f32,
        sc_left: f32,
        sc_right: f32,
        params: &Self::Params,
    ) -> (f32, f32) {
        // Default: ignore sidechain, fall back to regular stereo processing.
        self.process_stereo(left, right, params)
    }
}
