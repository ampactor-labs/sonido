# Design Decisions

Architecture Decision Records (ADRs) for the Sonido DSP framework. Each record captures the context, options considered, and rationale behind a significant design choice.

## Table of Contents

- [ADR-001: Stereo-First Effect Trait](#adr-001-stereo-first-effect-trait)
- [ADR-002: `no_std` as First-Class Constraint](#adr-002-no_std-as-first-class-constraint)
- [ADR-003: Const Generic Oversampling](#adr-003-const-generic-oversampling)
- [ADR-004: Index-Based Parameter System](#adr-004-index-based-parameter-system)
- [ADR-005: Static vs. Dynamic Effect Chaining](#adr-005-static-vs-dynamic-effect-chaining)
- [ADR-006: SmoothedParam Smoothing Strategy](#adr-006-smoothedparam-smoothing-strategy)
- [ADR-007: Direct Form I for Biquad Filters](#adr-007-direct-form-i-for-biquad-filters)
- [ADR-008: SVF Alongside Biquad](#adr-008-svf-alongside-biquad)
- [ADR-009: Separate Reverb Tanks for Stereo](#adr-009-separate-reverb-tanks-for-stereo)
- [ADR-010: f32 Throughout](#adr-010-f32-throughout)
- [ADR-011: Effect Registry Pattern](#adr-011-effect-registry-pattern)
- [ADR-012: Platform Abstraction with ControlId Namespaces](#adr-012-platform-abstraction-with-controlid-namespaces)
- [ADR-013: Two Delay Line Implementations](#adr-013-two-delay-line-implementations)
- [ADR-014: PolyBLEP over BLIT/MinBLEP](#adr-014-polyblep-over-blitminblep)
- [ADR-015: ModulationSource Trait Unification](#adr-015-modulationsource-trait-unification)
- [ADR-016: Denormal Protection via flush_denormal()](#adr-016-denormal-protection-via-flush_denormal)
- [ADR-017: Shared DSP Vocabulary and Gain Staging](#adr-017-shared-dsp-vocabulary-and-gain-staging)
- [ADR-018: Feedback-Adaptive Reverb Gain Compensation](#adr-018-feedback-adaptive-reverb-gain-compensation) *(superseded by ADR-019)*
- [ADR-019: Generalized Feedback Compensation](#adr-019-generalized-feedback-compensation)
- [ADR-020: Fast Math Approximations for Embedded DSP](#adr-020-fast-math-approximations-for-embedded-dsp)
- [ADR-021: Block Processing Overrides](#adr-021-block-processing-overrides)
- [ADR-022: Parameter System Hardening for Plugin Integration](#adr-022-parameter-system-hardening-for-plugin-integration)
- [ADR-023: Pluggable Audio Backend Abstraction](#adr-023-pluggable-audio-backend-abstraction)
- [ADR-024: CLAP Plugin Architecture](#adr-024-clap-plugin-architecture)
- [ADR-025: DAG Routing Engine](#adr-025-dag-routing-engine)

---

## ADR-001: Stereo-First Effect Trait

**Status:** Accepted
**Source:** `crates/sonido-core/src/effect.rs`

### Context

Audio effects libraries typically start with a mono `process(f32) -> f32` interface and add stereo support later. This creates friction for effects that require cross-channel interaction (reverb, ping-pong delay, stereo widener) because the mono interface cannot express them.

### Decision

Make `process_stereo(f32, f32) -> (f32, f32)` the primary processing method. Mono `process()` remains for backwards compatibility and simple effects. Default implementations bridge between them:

- Mono effects implement `process()`. The default `process_stereo()` calls `process()` independently for each channel.
- True stereo effects implement `process_stereo()`. The default `process()` derives mono by sending the same signal to both channels and returning the left output.

An `is_true_stereo()` metadata method lets hosts distinguish the two cases.

### Rationale

- **No API break**: Existing mono effects continue to work without modification
- **Correct stereo by default**: Dual-mono processing (independent L/R) is the correct default for effects without cross-channel interaction
- **Explicit cross-channel**: True stereo effects declare themselves, enabling host optimizations (e.g., skip stereo processing when the source is mono)
- **Block processing**: Both `process_block` and `process_block_stereo` have default implementations, reducing boilerplate while allowing override for SIMD optimization

### Alternatives Considered

- **Separate mono/stereo traits**: Would fragment the API and make `dyn Effect` chaining impossible for mixed chains
- **Channel count as runtime parameter**: More flexible but adds branching to the hot path
- **Always stereo (no mono path)**: Would add overhead for purely mono effects and make the API less ergonomic for simple use cases

---

## ADR-002: `no_std` as First-Class Constraint

**Status:** Accepted
**Source:** All `no_std`-compatible crates

### Context

Sonido targets both desktop and embedded platforms (Electrosmith Daisy Seed, STM32H750). Embedded targets often lack a standard library, heap allocator, or floating-point unit.

### Decision

Core crates (`sonido-core`, `sonido-effects`, `sonido-synth`, `sonido-registry`, `sonido-platform`) are `no_std` by default with an optional `std` feature. This means:

- All math uses `libm` functions (`sinf`, `cosf`, `tanf`, etc.) instead of `f32` methods
- `alloc` is used where heap allocation is needed (delay lines), kept behind feature gates where possible
- `FixedDelayLine<const N: usize>` provides a fully stack-allocated alternative
- No `println!`, `format!`, `std::time`, or file I/O in core crates

### Rationale

- **Compile-time enforcement**: `#![cfg_attr(not(feature = "std"), no_std)]` at the crate level means any accidental `std` dependency is a compile error
- **Embedded-first, not embedded-afterthought**: Adding `no_std` support retroactively is painful; designing for it from the start is cleaner
- **Minimal cost on desktop**: Using `libm` instead of `std::f32` has negligible performance impact (the compiler often inlines to the same hardware instructions)

### Tradeoffs

- `libm` math functions may have slightly different rounding behavior compared to hardware FPU implementations, though this is inaudible for audio
- Some patterns require `alloc` (e.g., `Vec<f32>` for variable-length delay lines), which means truly allocation-free code must use `FixedDelayLine<N>`

---

## ADR-003: Const Generic Oversampling

**Status:** Accepted (updated Phase 5)
**Source:** `crates/sonido-core/src/oversample.rs`

### Context

Oversampling is needed to prevent aliasing from nonlinear processing (distortion, waveshaping). The oversampling factor (2x, 4x, 8x) determines the quality/CPU tradeoff.

### Decision

Use Rust const generics to make the oversampling factor a compile-time parameter:

```rust
pub struct Oversampled<const FACTOR: usize, E: Effect> { ... }
```

### Filter Design (Updated Phase 5)

The original implementation used linear interpolation for upsampling and a 16-tap FIR for downsampling, achieving roughly 40 dB stopband attenuation. This was upgraded to:

**Upsampling: Polyphase Blackman-Harris windowed sinc (8 taps/phase)**

Each oversampling factor has a precomputed `[FACTOR][8]` kernel array. Each row is one polyphase sub-filter for a fractional offset. The Blackman-Harris window provides >92 dB sidelobe suppression. This eliminates the HF rolloff inherent in linear interpolation (`sinc(pi*f/fs)^2`), which was audible as dullness with aggressive waveshaping at high frequencies.

**Downsampling: 48-tap Kaiser-windowed sinc FIR (beta = 8.0)**

The Kaiser beta of 8.0 was chosen via Kaiser's empirical formula to achieve >80 dB stopband attenuation with 48 taps. Three coefficient sets target different normalized cutoffs: 0.45 (2x), 0.225 (4x), 0.1125 (8x) of the oversampled Nyquist. All sets are symmetric (linear phase).

**Why Kaiser beta = 8.0:** The Kaiser window provides a direct, well-characterized tradeoff between main-lobe width (transition bandwidth) and sidelobe level (stopband attenuation). At beta=8.0, sidelobe suppression exceeds 80 dB — sufficient to keep alias products inaudible even for aggressive waveshaping. Higher beta (e.g., 10.0) would improve rejection but widen the transition band, requiring more taps to maintain the same passband flatness.

**Why polyphase decomposition for upsampling:** The textbook approach to upsampling (zero-stuff then filter) wastes computation multiplying zeros. Polyphase decomposition evaluates only the non-zero tap positions for each sub-sample, reducing work by a factor of FACTOR while producing identical results.

### Rationale

- **Zero-cost abstraction**: The compiler can unroll loops and optimize for the specific factor. A `for i in 0..FACTOR` loop where `FACTOR` is known at compile time becomes straight-line code.
- **Type safety**: `Oversampled<4, Distortion>` is a distinct type from `Oversampled<2, Distortion>`, preventing accidental factor changes
- **Static coefficient selection**: Both FIR downsample and polyphase upsample coefficients are selected at compile time via a match on `FACTOR`, eliminating runtime branching in the hot path
- **Composable**: The `Oversampled` wrapper implements `Effect`, so it can be chained, wrapped in `Box<dyn Effect>`, or used anywhere an effect is expected

### Tradeoffs

- The oversampling factor cannot be changed at runtime without creating a new instance
- Three sets of FIR downsample coefficients and three polyphase upsample kernels are compiled into the binary even if only one factor is used (though dead code elimination may remove unused ones)
- The 48-tap FIR + 8-tap sinc kernel adds 27 samples of latency (up from 7 with the old 16-tap design)

---

## ADR-004: Index-Based Parameter System

**Status:** Accepted
**Source:** `crates/sonido-core/src/param_info.rs`

### Context

Effects need a way to expose their parameters for GUIs, MIDI mapping, presets, and automation. The system must be `no_std` compatible and work with `dyn Effect`.

### Decision

Use integer-indexed parameter access via the `ParameterInfo` trait:

```rust
pub trait ParameterInfo {
    fn param_count(&self) -> usize;
    fn param_info(&self, index: usize) -> Option<ParamDescriptor>;
    fn get_param(&self, index: usize) -> f32;
    fn set_param(&mut self, index: usize, value: f32);
}
```

Each parameter is described by a `ParamDescriptor` with name, unit, range, default, and step size.

### Rationale

- **`no_std` compatible**: No `String`, `HashMap`, or dynamic dispatch needed. Parameter names are `&'static str`.
- **Object-safe**: Works with `dyn ParameterInfo` for runtime-polymorphic effects
- **Consistent**: Every effect uses the same interface, enabling generic GUI generation and preset serialization
- **Efficient**: Index-based access compiles to a simple match statement

### Alternatives Considered

- **String-keyed HashMap**: More ergonomic but requires `std`, heap allocation, and string hashing in the audio thread
- **Macro-generated parameter structs**: Type-safe but breaks object safety and makes dynamic effect chains impossible
- **Enum-based parameter IDs**: Type-safe per effect but incompatible across different effect types

---

## ADR-005: Static vs. Dynamic Effect Chaining

**Status:** Accepted
**Source:** `crates/sonido-core/src/effect.rs` (Chain type)

### Context

Users need to compose effects into signal chains. The two fundamental approaches are:

1. **Static (compile-time)**: The chain structure is known at compile time
2. **Dynamic (runtime)**: Effects can be added/removed/reordered at runtime

### Decision

Provide both:

- **Static**: `EffectExt::chain()` produces `Chain<A, B>` with zero overhead
- **Dynamic**: `Vec<Box<dyn Effect>>` for runtime-configurable chains

### Rationale for Static Chaining

The `Chain<A, B>` type nests statically:

```rust
let chain = dist.chain(chorus).chain(delay);
// Type: Chain<Chain<Distortion, Chorus>, Delay>
```

The compiler inlines `process()` calls across the entire chain, eliminating virtual dispatch and enabling cross-effect optimizations. For a fixed effect chain (common in embedded and plugin contexts), this achieves the same performance as hand-written sequential processing.

The `Chain` type correctly propagates:
- **Latency**: Sum of both effects' latencies
- **Stereo**: True stereo if either effect is true stereo
- **Sample rate**: Set on both effects
- **Reset**: Reset both effects

### When to Use Dynamic

Dynamic chains (`Vec<Box<dyn Effect>>`) are appropriate for:
- GUIs where users can reorder effects
- Configurations loaded at runtime
- Effect counts that vary

The virtual dispatch overhead (~2-3 ns per effect per sample) is negligible for most applications.

---

## ADR-006: SmoothedParam Smoothing Strategy

**Status:** Accepted
**Source:** `crates/sonido-core/src/param.rs`

### Context

Audio parameters need smooth transitions to avoid zipper noise. The choice of smoothing algorithm affects both the sound quality and the computational cost.

### Decision

Provide two smoothing strategies:

1. **`SmoothedParam`**: Exponential smoothing (one-pole lowpass)
2. **`LinearSmoothedParam`**: Linear ramp with exact endpoint

### Rationale

**Exponential** is the default because:
- One multiply-add per sample (minimal CPU)
- Natural-sounding transitions (like physical systems with inertia)
- No state beyond current value and coefficient
- Settles asymptotically without overshoot

**Linear** is provided for:
- Crossfades where equal-power behavior matters
- Situations requiring a predictable, exact transition time
- The final value is reached exactly (no asymptotic tail)

The coefficient formula for exponential smoothing (`param.rs:146-151`):

```rust
self.coeff = 1.0 - expf(-1.0 / samples);
```

This is derived from the one-pole lowpass time constant: after `smoothing_time_ms`, the parameter reaches 63.2% of the way to the target (one time constant). After 5x the smoothing time, it is within 0.7% (inaudible for most parameters).

---

## ADR-007: Direct Form I for Biquad Filters

**Status:** Accepted
**Source:** `crates/sonido-core/src/biquad.rs`

### Context

Biquad filters can be implemented in several topologies: Direct Form I, Direct Form II, Transposed Direct Form II, and others. The choice affects numerical accuracy, state storage, and modulation behavior.

### Decision

Use Direct Form I with 4 state variables (`x[n-1]`, `x[n-2]`, `y[n-1]`, `y[n-2]`).

### Rationale

- **Numerical stability**: Direct Form I separates input and output delay lines, reducing the dynamic range of intermediate values. With `f32`, this matters: Direct Form II can exhibit limit-cycle oscillations (small DC offsets that never decay) when filter coefficients produce poles near the unit circle (high Q, low frequency).
- **Modulation tolerance**: Changing biquad coefficients while the filter is active is technically incorrect for all direct forms, but Direct Form I handles it more gracefully because the delay line values remain valid across coefficient changes. Direct Form II stores internal state that depends on the current coefficients, making coefficient changes produce larger transients.
- **Simplicity**: The implementation is 5 multiply-adds and 4 state updates, easy to verify and audit.

### Tradeoffs

- **Memory**: 4 state variables instead of 2 (Direct Form II). This is 8 extra bytes per filter, negligible even with many parallel filters.
- **For clean modulation**: The SVF topology (ADR-008) is preferred over biquad when filter parameters change at audio rate.

---

## ADR-008: SVF Alongside Biquad

**Status:** Accepted
**Source:** `crates/sonido-core/src/svf.rs`

### Context

The biquad is versatile but has limitations for modulated filters. Rapidly changing biquad coefficients can cause instability or audible artifacts.

### Decision

Provide a State Variable Filter as an alternative filter topology, using the TPT (topology-preserving transform) formulation.

### Rationale

The SVF is preferred over the biquad in specific situations:

1. **Audio-rate modulation**: The SVF's state variables represent physical quantities (integrator outputs) that remain meaningful when parameters change. An envelope follower modulating a filter cutoff at audio rate is stable with the SVF but may click with a biquad.

2. **Multi-output**: A single SVF computation yields lowpass, highpass, bandpass, and notch outputs simultaneously (`svf.rs:105-119`). This is useful for crossover networks, multiband effects, or UI visualizations.

3. **Frequency accuracy at high frequencies**: The `tan()` frequency warping in the SVF provides better frequency accuracy near Nyquist compared to the bilinear transform used in RBJ biquad coefficients.

Both topologies are available because each has strengths:

| Feature | Biquad | SVF |
|---------|--------|-----|
| CPU cost | Lower (5 multiply-adds) | Higher (~12 operations) |
| Peaking/shelving EQ | Direct (RBJ cookbook) | Requires additional computation |
| Static coefficients | Preferred (stable, efficient) | Works but unnecessarily expensive |
| Modulated cutoff | Avoid | Preferred |
| Multi-output | No (one output per computation) | Yes (4 outputs per computation) |

---

## ADR-009: Separate Reverb Tanks for Stereo

**Status:** Accepted
**Source:** `crates/sonido-effects/src/reverb.rs`

### Context

The original Freeverb algorithm produces mono output from 8 comb filters and 4 allpass filters. Stereo can be achieved by either processing both channels through the same tank (fake stereo) or using independent tanks with different tunings (true stereo).

### Decision

Use separate left and right filter banks with slightly offset delay times. The right channel uses different comb and allpass tunings:

```rust
const COMB_TUNINGS_44K:   [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const COMB_TUNINGS_44K_R: [usize; 8] = [1139, 1211, 1300, 1379, 1445, 1514, 1580, 1640];
```

The offset is approximately 23 samples (~0.5 ms at 44.1 kHz).

### Rationale

- **Decorrelation**: Offset delay times ensure the left and right reverb tails are uncorrelated, producing a convincing stereo image. Identical tanks would produce a mono reverb panned to center.
- **Width control**: A `stereo_width` parameter (exposed via `ParameterInfo` at index 5, range 0.0–1.0, default 1.0) blends between the decorrelated channels (full stereo) and their average (mono), using mid-side processing. This gives users continuous control from mono to wide stereo.
- **Type presets**: A `reverb_type` parameter (index 6) selects between Room (0), Hall (1), and Plate (2) presets, each with tuned defaults for room size, decay, and damping. The default Room preset uses room_size 0.5 for a medium, natural-sounding space.
- **No cross-feed**: Each channel has its own processing path. The left input excites only the left tank. This is simpler than cross-coupled topologies and avoids feedback loops between channels.

### Tradeoffs

- **Memory**: Two complete sets of comb and allpass filters (2x the state of a mono reverb). At 48 kHz, the total delay memory is approximately 2 x (sum of all delay lengths) x 4 bytes, roughly 200 KB.
- **CPU**: Processing both tanks takes approximately 2x the CPU of a mono reverb. For the 8+4 filter structure, this is still modest.

---

## ADR-010: f32 Throughout

**Status:** Accepted

### Context

Audio processing can use `f32` (32-bit float, ~24 bits mantissa) or `f64` (64-bit float, ~53 bits mantissa). Some DSP libraries use `f64` internally for precision-critical operations like IIR filter state.

### Decision

Use `f32` exclusively for all audio processing, state storage, and parameter values.

### Rationale

- **Sufficient precision**: `f32` provides ~144 dB of dynamic range, exceeding 24-bit audio converters (~144 dB theoretical) and far exceeding the ~120 dB range of human hearing.
- **Cache efficiency**: `f32` uses half the memory bandwidth of `f64`. For delay lines (potentially tens of thousands of samples), this translates directly to fewer cache misses.
- **SIMD alignment**: Four `f32` values fit in a 128-bit SIMD register (SSE/NEON), enabling future vectorization. Only two `f64` values fit, halving throughput.
- **Embedded compatibility**: ARM Cortex-M7 (Daisy Seed) has a single-precision FPU. Double-precision operations would require software emulation, dramatically reducing performance.

### Where f64 Could Help

IIR filter state accumulation at very low frequencies or very high Q can benefit from `f64`. The current mitigation is to use the SVF topology (which has better numerical behavior with `f32`) for filters in those regimes.

---

## ADR-011: Effect Registry Pattern

**Status:** Accepted
**Source:** `crates/sonido-registry/src/lib.rs`

### Context

Applications need to create effects by name at runtime (for presets, configuration files, and dynamic UIs) without hardcoding every effect type.

### Decision

A centralized `EffectRegistry` that maps string names to factory functions, returning `Box<dyn EffectWithParams + Send>` (combining `Effect` + `ParameterInfo`).

### Rationale

- **Decoupling**: Application code does not need to import every effect type
- **Categorization**: Effects are organized by `EffectCategory` (Dynamics, Distortion, Modulation, etc.) for UI grouping
- **Metadata**: Each registry entry includes name, description, category, and parameter count — currently 15 effects with param counts ranging from 2 to 9
- **Parameter discovery**: `param_index_by_name()` enables CLI and config systems to resolve parameter names to indices at runtime
- **`no_std` compatible**: The registry uses `alloc` (for `Box` and `Vec`) but not `std`

---

## ADR-012: Platform Abstraction with ControlId Namespaces

**Status:** Accepted
**Source:** `crates/sonido-platform/src/lib.rs`

### Context

Sonido targets multiple platforms: hardware pedals with physical knobs, desktop GUIs, MIDI controllers, and DAW automation. Each platform has its own control mechanism, but the effect code should be platform-agnostic.

### Decision

A 16-bit `ControlId` with namespace prefixes:

| Prefix | Source |
|--------|--------|
| `0x00XX` | Hardware (physical knobs, switches) |
| `0x01XX` | GUI (software widgets) |
| `0x02XX` | MIDI (CC messages) |
| `0x03XX` | Automation (DAW lanes) |

A `ControlMapper` bridges between `ControlId` values and `ParameterInfo` indices, handling denormalization (mapping 0.0-1.0 control values to parameter ranges).

### Rationale

- **Unified mapping**: The same effect code works whether controlled by a physical knob, GUI slider, MIDI CC, or automation lane
- **Priority/conflict resolution**: The namespace makes it clear which control source has authority when multiple sources control the same parameter
- **Compact**: 16-bit IDs fit in embedded memory constraints while supporting up to 256 controls per namespace
- **Extensible**: New namespaces (e.g., OSC, network) can be added without changing existing code

---

## ADR-013: Two Delay Line Implementations

**Status:** Accepted
**Source:** `crates/sonido-core/src/delay.rs`

### Context

Delay lines are used by many effects (chorus, flanger, delay, reverb comb filters). The requirements differ between desktop and embedded targets.

### Decision

Provide two implementations:

1. **`InterpolatedDelay`**: Heap-allocated (`Vec<f32>`), variable-size, configurable interpolation (None/Linear/Cubic, default: Linear). General-purpose, used by most effects.
2. **`FixedDelayLine<const N: usize>`**: Stack-allocated (`[f32; N]`), compile-time fixed size, configurable interpolation (None/Linear/Cubic). For embedded targets or known-size use cases.

### Rationale

- **`InterpolatedDelay`** is appropriate when the delay size depends on sample rate (computed at runtime) or when multiple effects with different delay requirements share code
- **`FixedDelayLine<N>`** eliminates all heap allocation, making it suitable for bare-metal embedded where `alloc` may not be available
- Both share the same circular buffer concept and similar API, reducing cognitive overhead
- Both implementations offer cubic (Lagrange 3rd-order) interpolation, which is valuable for effects like chorus and flanger where modulated delay smoothness matters. JUCE uses Lagrange3rd as the default for modulated delays

---

## ADR-014: PolyBLEP over BLIT/MinBLEP

**Status:** Accepted
**Source:** `crates/sonido-synth/src/oscillator.rs`

### Context

Several algorithms exist for band-limited oscillator synthesis:

- **BLIT** (Band-Limited Impulse Train): Generate a band-limited impulse, integrate for saw/triangle
- **MinBLEP** (Minimum-phase Band-Limited Step): Precomputed tables for precise corrections
- **PolyBLEP**: Polynomial approximation of the band-limited step, applied near discontinuities
- **Wavetable**: Precomputed waveforms at multiple frequencies

### Decision

Use PolyBLEP for all non-sinusoidal waveforms.

### Rationale

- **Low CPU cost**: Only a few arithmetic operations per sample, with the correction limited to a small region near each discontinuity. Most samples pass through without any correction.
- **No lookup tables**: Unlike MinBLEP and wavetable, PolyBLEP requires zero precomputed data. This is important for `no_std` targets with limited memory.
- **Good quality**: While not as alias-free as MinBLEP at extreme frequencies, PolyBLEP provides adequate anti-aliasing for the audible frequency range. The remaining aliasing is typically 60-80 dB below the fundamental, inaudible in most musical contexts.
- **Phase modulation compatible**: PolyBLEP works correctly with the phase modulation used for FM synthesis, whereas wavetable approaches require special handling for phase modulation.
- **Simple implementation**: The entire `poly_blep` function is 10 lines of code, easy to understand and audit.

### Tradeoffs

- At very high fundamental frequencies (above ~8 kHz at 48 kHz sample rate), PolyBLEP's aliasing rejection degrades. For these cases, oversampling the oscillator or switching to sine would be cleaner.

---

## ADR-015: ModulationSource Trait Unification

**Status:** Accepted
**Source:** `crates/sonido-core/src/modulation.rs`

### Context

Audio effects are modulated by various time-varying signals: LFOs, envelope followers, ADSR envelopes, sequencers, and external audio. Each produces values in different ranges (bipolar vs. unipolar) with different semantics.

### Decision

Unify all modulation sources under a single trait:

```rust
pub trait ModulationSource {
    fn mod_advance(&mut self) -> f32;
    fn is_bipolar(&self) -> bool;
    fn mod_reset(&mut self);
    fn mod_value(&self) -> f32;
}
```

Default methods provide automatic range conversion (`mod_advance_unipolar`, `mod_advance_bipolar`).

### Rationale

- **Interchangeability**: An effect can accept `&mut dyn ModulationSource` and work with any modulation type without specialization
- **Range safety**: The `is_bipolar()` flag, combined with conversion methods, prevents the common bug of applying a [-1, 1] LFO where a [0, 1] value was expected
- **Composability**: A `ModulationAmount` struct combines source, depth, and inversion into a reusable modulation routing:

```rust
pub struct ModulationAmount {
    pub depth: f32,
    pub inverted: bool,
}
```

This is a building block for the `ModulationMatrix` in the synthesis engine, where arbitrary source-to-destination routings are configured at runtime.

---

## ADR-016: Denormal Protection via flush_denormal()

**Status:** Accepted
**Source:** `crates/sonido-core/src/math.rs`

### Context

IEEE 754 subnormal (denormalized) floating-point numbers cause severe CPU performance degradation -- up to 100x slower processing on x86 and ARM architectures. In audio DSP, subnormals arise naturally when feedback loops (comb filters, allpass chains, IIR filters) decay toward zero. A reverb tail fading to silence can spike CPU usage dramatically if subnormals are not handled.

### Options Considered

1. **FTZ/DAZ CPU flags**: Set the Flush-To-Zero and Denormals-Are-Zero bits in the CPU's floating-point control register (MXCSR on x86, FPSCR on ARM). This globally prevents subnormal generation at the hardware level.

2. **DC offset injection**: Add a tiny constant (~1e-15) to feedback signals to prevent values from reaching zero and entering the subnormal range.

3. **Explicit value-threshold flush**: A function that replaces values below a threshold with zero, applied at strategic points in feedback paths.

### Decision

Use option 3: an explicit `flush_denormal()` function with a threshold of 1e-20.

```rust
#[inline(always)]
pub fn flush_denormal(x: f32) -> f32 {
    if x.abs() < 1e-20 { 0.0 } else { x }
}
```

### Rationale

**Portability**: `flush_denormal()` is pure Rust with no platform-specific instructions, no inline assembly, and no CPU flag manipulation. It works identically on x86, ARM, RISC-V, and any other target Sonido supports, including `no_std` embedded systems.

**Precision**: The flush is applied only where needed (feedback paths, IIR state) rather than globally. FTZ/DAZ flags affect all floating-point operations in the thread, which could interfere with other libraries or computation running alongside the audio engine.

**Safety**: FTZ/DAZ flag manipulation requires `unsafe` code (inline assembly or intrinsics) and is thread-global state. Changing it can affect other code in the same thread without their knowledge. The explicit flush has no such side effects.

**DC offset rejection**: Unlike DC injection (option 2), the flush strategy does not add any DC content to the signal. DC injection can accumulate through cascaded effects and reduce headroom. A separate `DcBlocker` filter is provided for cases where DC offset from other sources (asymmetric waveshaping) needs to be removed.

### Tradeoffs

- **Branch per sample**: The `if` condition adds a branch in the feedback path. This is mitigated by `#[inline(always)]` and the fact that the branch is highly predictable (almost always "not flushing" during active signal processing). Modern branch predictors handle this with negligible overhead.
- **Not bit-exact zero**: Values between 0 and 1e-20 are forcibly zeroed, which technically changes the mathematical result. These values are at -400 dBFS, far below any audible or measurable signal level.
- **Manual placement**: Unlike FTZ/DAZ (which is automatic and covers all operations), `flush_denormal()` must be placed explicitly at each point where subnormals could accumulate. Missing a call site leaves that path vulnerable. This is mitigated by denormal stress tests in the test suite.

---

## ADR-017: Shared DSP Vocabulary and Gain Staging

**Status:** Accepted
**Source:** `crates/sonido-core/src/gain.rs`, `param.rs`, `param_info.rs`, `one_pole.rs`, `math.rs`

### Context

After implementing 15 effects, a codebase audit revealed significant duplication:

- 22 inline dry/wet mix calculations (`input * (1.0 - mix) + wet * mix`)
- 35+ smoothing time magic numbers (`10.0` ms scattered across constructors)
- 15+ identical `ParamDescriptor` struct literals for common params (Mix, Depth, Feedback)
- 3 ad-hoc one-pole lowpass implementations (distortion tone, tape HF rolloff)
- 3 local `db_to_linear`/`linear_to_db` functions (compressor, gate) duplicating core
- No universal output level control (Wah at +12.5 dB, TapeSaturation at +7.1 dB at defaults)

### Decision

Extract a shared vocabulary into `sonido-core` consisting of:

1. **`gain.rs`**: Universal output level contract — `output_level_param()`, `set_output_level_db()`, `output_param_descriptor()`. All 15 effects expose an output param at the last `ParameterInfo` index.

2. **`SmoothedParam` presets**: Named constructors (`fast`, `standard`, `slow`, `interpolated`) replacing magic numbers with documented semantics.

3. **`ParamDescriptor` factories**: `::mix()`, `::depth()`, `::feedback()`, `::time_ms()`, `::gain_db()` replacing 15+ identical struct literals.

4. **`wet_dry_mix()` / `wet_dry_mix_stereo()`**: Shared crossfade replacing 22 inline calculations.

5. **`OnePole`**: Reusable one-pole lowpass replacing 3 ad-hoc implementations.

6. **`ParameterInfo::find_param_by_name()`**: Default method for case-insensitive param lookup.

Fix gain staging bugs at the root:
- **Wah**: Normalize SVF bandpass by Q for unity peak gain (algebraically exact)
- **TapeSaturation**: Set default output to -6 dB to compensate for drive gain (matching Distortion's pattern)

### Rationale

**Single source of truth**: When the smoothing time convention changes, it changes in one place. When a new effect is added, it uses the vocabulary instead of reinventing it. The vocabulary enforces the project's DSP conventions at the API level.

**Root fixes over compensation**: The Wah's gain bug is fixed by normalizing the transfer function (`filtered / Q`), not by adding an output trim. The TapeSaturation fix matches the Distortion's established drive/level compensation pattern.

**Industry alignment**: NIH-plug uses `SmoothingStyle::Linear(10.0)` parameter presets, JUCE uses `NormalisableRange` with centralized param definitions, FunDSP uses shared smoothing filters. Our `SmoothedParam::standard()` / `ParamDescriptor::mix()` factories mirror these approaches without macro complexity.

### Alternatives Considered

- **Macro-based param generation**: Would reduce boilerplate further but adds compile-time complexity and makes debugging harder. The factory method approach is simpler and equally effective.
- **ParamGroup container**: A struct holding N `SmoothedParam`s that forwards `set_sample_rate()` and `snap_to_target()`. Deferred — requires a design decision on representation (Vec vs tuple vs macro) and the current per-field approach is explicit and clear.

### Consequences

- 15 effects migrated to vocabulary, reducing total lines of DSP boilerplate by ~30%
- Default-parameter golden regression tests (15 new) lock down factory defaults
- Adding a new effect requires less copy-paste and automatically gets the output level contract
- Param index stability is preserved — output is always the last index

---

## ADR-018: Feedback-Adaptive Reverb Gain Compensation

**Status:** Superseded by ADR-019
**Source:** `crates/sonido-effects/src/reverb.rs`

Original reverb-only quadratic compensation (`1 - x² * 0.88`). Superseded by ADR-019 which generalizes feedback compensation across all comb-based effects using topology-aware formulas.

---

## ADR-019: Generalized Feedback Compensation

**Status:** Accepted
**Source:** `crates/sonido-core/src/gain.rs`, `crates/sonido-effects/src/reverb.rs`

### Context

After applying `sqrt(1-fb)` wet-signal compensation to delay, flanger, phaser, and reverb, measurement showed 2/4 still exceeded the -1 dBFS peak ceiling on 440 Hz sine:

| Effect | Peak with sqrt | Status |
|--------|---------------|--------|
| delay (fb=0.4) | -0.8 dBFS | FAIL |
| flanger (fb=0.5) | -0.5 dBFS | FAIL |
| phaser (fb=0.5) | -1.1 dBFS | pass |
| reverb | -1.6 dBFS | pass |

**Root cause:** For a feedback comb filter with coefficient `g`, peak gain at resonance = `1/(1-g)`. With `c = sqrt(1-g)`, output at mix `m` = `(1-m) + m/sqrt(1-g)` > 1 for all g > 0. The `sqrt` form is mathematically guaranteed insufficient for comb topologies.

### Decision

Topology-aware compensation:

1. **Single comb filters** (delay, flanger, phaser): `c = (1-fb)` — exact peak-gain cancellation. Implemented in `gain::feedback_wet_compensation()`.

2. **Parallel comb banks** (reverb): `c = sqrt(1-fb)` — moderate compensation, inlined in `reverb.rs`. The 1/8 parallel averaging provides ~18 dB additional headroom, making exact compensation unnecessarily aggressive.

### Rationale

**Why `(1-fb)` is exact:** At resonance, a comb filter amplifies by `1/(1-fb)`. Scaling by `(1-fb)` gives: `(1-fb) × 1/(1-fb) = 1.0`. The compensated wet signal equals the dry signal — perfect crossfade, zero amplification at any mix setting.

**Why reverb keeps `sqrt`:** 8 parallel combs with mutually-prime delay lengths means only 1–2 resonate at any frequency. The 1/8 averaging absorbs the overshoot that `sqrt` leaves. Using exact `(1-fb)` at hall settings (fb≈0.98) would attenuate the wet signal to 0.02 (−34 dB), producing an inaudibly quiet reverb.

**Why phaser passes with `(1-fb)`:** Allpass cascades have unity magnitude at all frequencies. Resonance is narrowband (3 notch-peak pairs). Exact compensation is conservative but keeps the effect well within ceiling.

### Alternatives Considered

- **Dynamic AGC**: Per-sample envelope tracking would adapt to any topology. Rejected — too expensive for embedded targets (Daisy Seed, Hothouse). Static compensation is the industry standard for pedal/embedded DSP.
- **Fixed input attenuation**: Global -6 dB input pad. Rejected — unnecessarily quiet at low feedback, doesn't scale with the actual problem.
- **Soft clipper on wet output**: Would prevent overshoot but introduces unwanted nonlinearity into clean effects (delay, phaser). Defeats the purpose of a transparent modulation effect.

### Consequences

- All 15 effects pass -1 dBFS peak ceiling (15/15)
- delay: -0.8 → -2.0 dBFS, flanger: -0.5 → -2.1 dBFS, phaser: -1.1 → -2.6 dBFS
- Reverb unchanged at -1.6 dBFS (topology-specific `sqrt` preserved)
- At high feedback, wet signal is quieter but decay tail is longer — this IS the character of high feedback. The `output` param provides makeup gain.
- Golden files regenerated for delay, flanger, phaser, reverb

---

## ADR-020: Fast Math Approximations for Embedded DSP

**Status:** Accepted
**Source:** `crates/sonido-core/src/fast_math.rs`

### Context

Cortex-M7 targets (Daisy Seed, Hothouse) lack hardware transcendental instruction support. Standard `libm` implementations of `logf`, `expf`, `sinf`, and `tanf` consume 100-200 cycles each — significant when called per-sample in DSP loops running at 48 kHz with tight deadline budgets.

Profiling identified transcendental calls as the dominant cost in: LFO tick, compressor envelope detection, parametric EQ coefficient updates, and phaser allpass tuning.

### Decision

1. Implement purpose-built approximations in `sonido-core::fast_math` with documented error bounds
2. Use IEEE 754 bit manipulation (log2/exp2), Bhaskara parabolic (sin), and Padé rational (tan) techniques
3. Keep approximations in a dedicated module — callers opt in explicitly via `fast_log2()` etc.
4. Maintain the standard `libm` functions as the default; fast_math is for embedded hot paths only

### Rationale

- **Explicit opt-in** prevents accidental precision loss — callers choose fast_* when they've verified error tolerance
- **Dedicated module** keeps approximation code isolated and auditable
- **Error bounds < audible threshold** for all documented use cases (< 0.05 dB for gain, < 0.001 for LFO)
- **10-15x speedup** on Cortex-M7 justifies the precision trade-off for embedded targets

### Alternatives Considered

- **CMSIS-DSP library**: ARM-specific, not `no_std`-compatible without allocator, larger dependency
- **Inline assembly**: Not portable across ARM variants, maintenance burden
- **Lookup tables with interpolation**: Higher memory footprint, cache pressure on M7's small D-cache

### Consequences

- Effects using fast_math have slightly different output than libm versions (within documented error bounds)
- Golden file regression tests use libm — fast_math output verified via dedicated unit tests with tolerance assertions
- New effects targeting embedded should prefer fast_math for transcendental calls in per-sample loops

---

## ADR-021: Block Processing Overrides

**Status:** Accepted
**Source:** `crates/sonido-effects/src/distortion.rs`, `compressor.rs`, `chorus.rs`, `delay.rs`, `reverb.rs`

### Context

The `Effect` trait provides default `process_block_stereo` implementations that call `process_stereo` per sample. While correct, this prevents the compiler from optimizing across sample boundaries (loop hoisting, autovectorization) and forces per-sample virtual dispatch overhead for any branching logic.

A technical debt audit identified block processing as the single largest performance gap — 4-8x potential improvement on the hot path for effects with branch-heavy per-sample logic.

### Decision

Override `process_block_stereo` on the 5 most performance-critical effects: Distortion, Compressor, Chorus, Delay, and Reverb. Each override must produce bit-identical output to the per-sample path.

### Design Patterns

**Monomorphized waveshaper dispatch (Distortion):**

The per-sample `process_stereo` matches on `WaveShape` for every sample. The block override matches once at the top and calls a generic inner function:

```rust
fn process_block_stereo_inner<F: Fn(f32) -> f32>(
    &mut self, left_in: &[f32], right_in: &[f32],
    left_out: &mut [f32], right_out: &mut [f32],
    waveshaper: F,
) { ... }

fn process_block_stereo(...) {
    match self.wave_shape {
        WaveShape::SoftClip => self.process_block_stereo_inner(..., soft_clip),
        WaveShape::HardClip => self.process_block_stereo_inner(..., |x| x.clamp(-1.0, 1.0)),
        // ...
    }
}
```

The generic `F` parameter is monomorphized: the compiler generates specialized code for each waveshaper, enabling inlining and autovectorization of the inner loop. The `match` executes once per block instead of once per sample.

**Structural deduplication (Reverb):**

The per-sample `process()` and `process_stereo()` shared significant copy-paste logic for parameter advancing and comb processing. The block override consolidates this into a single loop body that processes both channels, eliminating the duplication.

### Bit-Identical Constraint

All block overrides are verified to produce identical output to the per-sample path. This is enforced by the existing golden-file regression tests, which compare sample-by-sample against reference WAV files with MSE < 1e-6. The block overrides must not change the DSP algorithm — only the iteration structure.

### Rationale

- **Compiler optimization boundary**: Per-sample dispatch prevents the compiler from seeing across sample boundaries. Block processing exposes the full loop to the optimizer.
- **Branch hoisting**: Match-once-per-block eliminates per-sample branching. For Distortion's 4 waveshaper modes, this removes 3 dead branches per sample.
- **Autovectorization opportunity**: Tight inner loops over contiguous buffers are the ideal pattern for LLVM's autovectorizer. The `SmoothedParam::advance()` call per sample limits full vectorization but the gain/filter math benefits.
- **Selective override**: Only 5 of 15 effects got overrides — those where profiling or structural analysis showed clear benefit. The remaining 10 effects are simple enough that the per-sample default is adequate.

### Alternatives Considered

- **SIMD intrinsics**: Explicit SIMD would provide guaranteed vectorization but requires `unsafe`, is platform-specific, and breaks `no_std` portability. Deferred until profiling shows autovectorization is insufficient.
- **Overriding all 15 effects**: Diminishing returns for simple effects (Preamp, Gate, Filter) where the per-sample cost is already minimal. Maintenance burden outweighs benefit.

## ADR-022: Parameter System Hardening for Plugin Integration

### Context

Sonido's parameter system was designed for standalone GUI use (runtime name-based lookup, per-sample smoothing, index-based access). Plugin hosts (CLAP, VST3) impose stricter requirements:

- **Stable identity**: Parameters must survive reordering and persist across sessions. CLAP requires `clap_id` (u32), VST3 requires `ParamID` (u32).
- **Scaling metadata**: Hosts need to know the normalization curve to render parameter arcs and handle mouse gestures. CLAP has `CLAP_PARAM_IS_STEPPED`, `CLAP_PARAM_IS_AUTOMATABLE`, etc.
- **Text display**: CLAP mandates `value_to_text()` / `text_to_value()` callbacks. VST3 has `getParamStringByValue()` / `getParamValueByString()`. Without these, hosts display raw floats with no units.
- **Modulation routing**: CLAP supports non-destructive modulation (`CLAP_PARAM_IS_MODULATABLE`) where the base value is preserved. This requires a per-parameter modulation ID.

### Decision

Extend `ParamDescriptor` with plugin-oriented metadata and methods:

| Addition | Purpose | CLAP mapping | VST3 mapping |
|----------|---------|-------------|-------------|
| `ParamId(u32)` | Stable identity | `clap_id` | `ParamID` |
| `string_id: &'static str` | Debug/serialization | — | — |
| `ParamScale` (Linear/Log/Power) | Normalization curve | `CLAP_PARAM_IS_STEPPED` (implicit) | `stepCount` |
| `ParamFlags` (AUTOMATABLE, STEPPED, HIDDEN, READ_ONLY, MODULATABLE) | Host capability bits | `clap_param_info_flags` | `ParameterInfo::flags` |
| `modulation_id: Option<u32>` | CLAP modulation routing | `CLAP_PARAM_IS_MODULATABLE` | — (no equivalent) |
| `step_labels: Option<&'static [&'static str]>` | Enum text display | `value_to_text()` | `getParamStringByValue()` |
| `format_value() → String` | Generic text display | `value_to_text()` | `getParamStringByValue()` |
| `parse_value() → Option<f32>` | Text-to-value parsing | `text_to_value()` | `getParamValueByString()` |

Stable IDs follow a base+offset convention per effect (Preamp=100, Distortion=200, ..., Reverb=1500) with sequential params from the base.

### Rationale

- **ParamId is u32, not string**: Both CLAP and VST3 use numeric IDs. String IDs are supplementary for debugging. Numeric IDs are cheaper to compare and hash.
- **ParamFlags as bitflags**: Direct mapping to CLAP's `clap_param_info_flags`. Custom bitflag type (not `bitflags` crate) keeps `no_std` and zero-dep.
- **step_labels is `&'static [&'static str]`**: Static lifetime matches the param descriptor's `&'static str` fields. No allocation for label storage. Labels are known at compile time for all current effects.
- **format_value/parse_value on ParamDescriptor**: Methods live on the descriptor, not a separate trait, because all formatting info (unit, min, max, step, labels) is already in the descriptor. No vtable dispatch needed.
- **alloc, not std**: `format_value()` returns `alloc::string::String`, which is available in both `std` and `no_std` (with `extern crate alloc`). No feature gate needed.
- **MODULATABLE flag**: CLAP-specific. VST3 treats all parameter changes as automation. The flag is harmless in VST3 wrappers (simply ignored).
- **modulation_id**: Separate from ParamId because CLAP's modulation routing needs its own namespace. Mirrors nih-plug's `poly_modulation_id()`.

### Alternatives Considered

- **Runtime string IDs only (no numeric ParamId)**: Fragile across reordering, expensive to hash per-audio-frame. Both CLAP and VST3 require numeric IDs.
- **Trait-method formatting (virtual dispatch)**: Would require `dyn` dispatch for text formatting and forces `alloc` into the trait definition. The descriptor-method approach is simpler and equally powerful since all formatting info is already in `ParamDescriptor`.
- **Callback fn pointers for formatting**: Can't capture state, awkward ergonomics in no_std. `step_labels` + unit-based formatting covers 100% of current effects.
- **Dynamic enum lists (Vec of labels)**: Would require heap allocation in the descriptor. Static `&[&str]` is sufficient since all current enums are known at compile time. Effects needing dynamic labels can override `EffectWithParams::effect_format_value()`.

### Tradeoffs

- `step_labels` is static — effects with runtime-dynamic enum lists would need to override `effect_format_value()` on the `EffectWithParams` trait.
- `format_value()` returns `String` (heap allocation), acceptable for GUI/host display paths but not audio-thread safe. Plugin wrappers should call it from the main thread or cache results.

---

## ADR-023: Pluggable Audio Backend Abstraction

**Status:** Accepted
**Source:** `crates/sonido-io/src/backend.rs`, `crates/sonido-io/src/cpal_backend.rs`

### Context

Sonido's audio I/O was tightly coupled to cpal. The `AudioStream` struct held cpal `Host`, `Device`, and `Stream` types directly in its fields, and the GUI (`audio_processor.rs`) independently reimplemented cpal stream setup because `AudioStream` lacked the flexibility it needed (optional mic input, per-sample crossbeam channels, atomic error counts). This created two maintenance points for the same platform dependency.

Meanwhile, sonido's DSP core is deliberately platform-agnostic (`no_std`, buffer-based `Effect` trait), and the `sonido-platform` crate already defines hardware control abstraction (`PlatformController`). The audio I/O layer was the remaining gap: no trait boundary existed between the processing pipeline and the platform audio API.

### Decision

Introduce an `AudioBackend` trait in `sonido-io` that abstracts over platform audio APIs. Provide `CpalBackend` as the default implementation.

**Key design choices:**

1. **Boxed closures for callbacks** — `OutputCallback = Box<dyn FnMut(&mut [f32]) + Send>` makes the trait object-safe, enabling `Box<dyn AudioBackend>` for runtime backend selection.

2. **Type-erased stream handles** — `StreamHandle` wraps `Box<dyn Send>` via RAII. Backend-specific stream types (cpal `Stream`, ALSA `pcm_t*`, etc.) stay internal. Dropping the handle stops playback.

3. **Interleaved f32 buffers** — Consistent with sonido's f32-throughout design (ADR-010) and the callback signatures of cpal, WASAPI, CoreAudio, and WebAudio.

4. **`BackendStreamConfig` separate from `StreamConfig`** — The new config struct adds `channels` and is purpose-built for the trait. The existing `StreamConfig` (with device name strings and input/output separation) remains for backward compatibility.

5. **Additive, non-breaking** — The existing `AudioStream` still works unchanged. The new trait is a parallel path that consumers can adopt incrementally.

### Alternatives Considered

- **Generic type parameter on `AudioStream<B: AudioBackend>`**: Compile-time backend selection, zero-cost dispatch. Rejected because it makes `AudioStream` generic everywhere it's stored, complicating GUI and CLI code. Runtime backend selection (important for testing and embedded targets) would require `Box<dyn AudioBackend>` anyway.

- **Trait with associated types for stream handles**: `type StreamHandle: Send + 'static` on the trait. More type-safe than `Box<dyn Send>`, but breaks object safety — you can't use `Box<dyn AudioBackend>` because the associated type is unknown. The type erasure overhead (one `Box` allocation per stream creation) is negligible for audio setup.

- **Remove cpal entirely, implement direct platform backends**: Maximally independent, but high effort per platform (ALSA, CoreAudio, WASAPI, AAudio all require separate unsafe FFI). cpal already provides tested implementations. The trait abstraction means we can add direct backends later without changing application code.

- **Feature-gated cpal in `sonido-io`**: Make cpal optional via `features = ["cpal-backend"]`. Valid future step, but premature now while cpal is the only implementation. The module structure (`cpal_backend.rs`) is already isolated for this.

### Tradeoffs

- **One `Box` allocation per callback**: Boxed closures for `OutputCallback`/`InputCallback`/`ErrorCallback` each allocate. This happens once at stream setup, not per audio buffer. Negligible.
- **`StreamHandle` is opaque**: Application code can't inspect or downcast the inner stream. This is intentional — it prevents backend types from leaking. If a consumer truly needs backend-specific access, they can use `CpalBackend` directly instead of `dyn AudioBackend`.
- **No duplex stream method**: The trait exposes `build_output_stream` and `build_input_stream` separately. Full-duplex (simultaneous I/O with shared buffer) is a future extension. Current users bridge input/output via channels, which is adequate for non-pro-audio latency requirements.

### Future Directions

- **Android AAudio backend**: Direct AAudio/Oboe integration for lower latency than cpal's Android backend.
- **Embedded DMA backend**: Interrupt-driven I/O for Cortex-M targets (Daisy Seed). The no_std DSP core is ready; the backend would bridge DMA buffer completion interrupts to the callback.
- **Mock backend**: Deterministic, clock-free backend for CI testing. Push a known buffer, assert output.
- **Feature-gate cpal**: Make `cpal` an optional dependency once an alternative backend exists.

---

## ADR-024: CLAP Plugin Architecture

**Status:** Accepted
**Source:** `crates/sonido-plugin/src/lib.rs`, `crates/sonido-plugin/src/audio.rs`, `crates/sonido-plugin/src/gui.rs`, `crates/sonido-plugin/src/main_thread.rs`, `crates/sonido-plugin/src/shared.rs`

### Context

Sonido needed DAW integration to be usable in professional music production workflows. The two main Rust-native options were CLAP (via the `clack` crate) and VST3 (via `vst3-sys` or `nih-plug`). Sonido already had a complete parameter system (`ParameterInfo`, `ParamDescriptor`, `ParamId`, `ParamFlags`), a GUI layer (`sonido-gui-core`), and a real-time DSP core. The plugin adapter needed to bridge these to the host protocol without duplicating parameter management logic.

### Decision

**CLAP over VST3, `clack` over `nih-plug`.** One plugin per effect, generated via the `sonido_effect_entry!` macro. A custom egui bridge for the GUI rather than the `egui-baseview` crate.

### Key Choices

**`clack` over `nih-plug`:**
`nih-plug` is an opinionated framework that owns parameter management. It provides macros like `#[id = "..."]` and `#[range = ...]` that define parameters in Rust structs. Sonido already has `ParameterInfo` and `ParamDescriptor` doing this work — adopting `nih-plug` would require either abandoning the existing system or maintaining a parallel one. `clack` provides direct, low-level access to the CLAP C API with Rust safety guarantees and no opinions about parameter representation. Sonido's `ParameterInfo` system maps directly to CLAP's `clap_param_info` without translation.

**One plugin per effect (not a single multi-effect plugin):**
The simplest mapping — each plugin is exactly one `Effect` + one `ParameterInfo` implementation. The host sees a single-purpose plugin with a known parameter count and stable IDs. This matches the mental model of guitar pedal plugins (one amp sim, one reverb, one delay). A multi-effect container plugin requires the DAG routing engine and is planned for v0.2.

The `sonido_effect_entry!` macro generates the complete CLAP plugin entry point from a single invocation:
```rust
sonido_effect_entry!(Distortion, "sonido.distortion", "Sonido Distortion");
```
This expands to the CLAP descriptor, factory, `PluginAudioProcessor` impl, parameter handling (`get_count`, `get_info`, `get_value`, `set_value`, `value_to_text`, `text_to_value`), and GUI bridge — roughly 300 lines of boilerplate per plugin, generated zero-cost at compile time.

**Custom egui bridge over `egui-baseview`:**
`egui-baseview` is an existing crate that integrates egui with baseview window handles. It was not usable here for two reasons: (1) it was version-locked to an older egui and had not tracked egui 0.31 at the time of implementation; (2) it imposes a specific render loop structure that conflicted with the plugin's need to share `SonidoShared` state between the GUI and audio threads. The custom bridge uses `baseview` + `egui_glow` directly, giving explicit control over the OpenGL context, frame timing, and shared-state access patterns. The `gui.rs` module's `#![allow(unsafe_code)]` is scoped specifically to the `ParentWindow` / `HasRawWindowHandle` implementation required for `baseview`.

**Resizable GUI via atomic channel:**
Plugin windows are resizable (320×240 to 1920×1080, default 480×380). The host calls `set_size()` on the main thread, which packs the new dimensions into an `AtomicU64` (`PendingResize`). The baseview `WindowHandler::on_frame()` reads the atomic each frame and calls `window.resize()` when changed. This avoids any cross-thread locking — the main thread writes, the render thread reads, both lock-free. `adjust_size()` clamps to bounds, `get_resize_hints()` reports free resize with no aspect ratio constraint.

**`ParamBridge` gesture protocol (`begin_set` / `end_set`):**
CLAP's automation recording protocol requires explicit gesture delimiters: `ParamGestureBeginEvent` before the first value change, `ParamGestureEndEvent` after the user releases the control. Without these, the DAW cannot distinguish a user drag (record this gesture) from a programmatic update (do not record). The `ParamBridge` trait exposes `begin_set(param_id)` and `end_set(param_id)` that map directly to these events. The GUI wraps every knob interaction in begin/end pairs. The same `ParamBridge` trait is used in the standalone GUI (`AtomicParamBridge`) with no-op begin/end implementations — the protocol is shared infrastructure.

**`SonidoShared` as `Arc<_>` (Clone):**
Three threads need access to shared state: audio processor (real-time), GUI (render thread), main thread (parameter state, host communication). `Arc<SonidoShared>` is cloned into each. Fields inside `SonidoShared` use `parking_lot::RwLock` for effect state and `Arc<AtomicParamBridge>` for lock-free parameter reads in the audio thread. No `Mutex` in the audio path.

**Plugin crate excluded from workspace `default-members`:**
Building plugins requires the `clack`, `baseview`, and `egui_glow` dependencies, which add non-trivial compile time and introduce system OpenGL requirements. `cargo build --workspace` does not build plugins by default. `make plugins` (or `cargo build -p sonido-plugin`) builds all 19 explicitly.

### Consequences

- 19 separate `.clap` shared library files are produced from one cargo build command
- GUI code (widgets, effect panels, theme) lives in `sonido-gui-core` and is shared between standalone and plugin, with no duplication
- `SonidoShared` is the coupling point between plugin threads; adding shared state means adding a field here
- Plugin-host parameter negotiation bypasses `ParameterInfo` defaults — plugin host sets initial values via `set_value`, not via `ParamDescriptor::default`
- `#![allow(unsafe_code)]` is scoped to `gui.rs` only — all other plugin code is safe Rust

### Alternatives Considered

- **`nih-plug` framework**: Rejected because it would require abandoning or duplicating Sonido's `ParameterInfo` system. `nih-plug` is excellent for greenfield plugins but expensive to retrofit onto an existing parameter abstraction.
- **VST3 (via `vst3-sys`)**: VST3's C++ COM interface is significantly more complex than CLAP's C API. `clack` provides better safety guarantees for CLAP than `vst3-sys` does for VST3. CLAP's growing DAW adoption (Bitwig, REAPER, Ableton 12, Logic) makes it the better investment.
- **LV2**: Strong Linux ecosystem, poor DAW support on macOS/Windows. Ruled out for cross-platform targets.
- **`egui-baseview` crate**: Version pinning and render loop conflicts (see above). May become viable in a future egui version.

### Future Directions

- **Resizable GUI** (v0.2): Implement `CLAP_EXT_GUI` resize callbacks.
- **Multi-effect container plugin** (v0.3): Full chain in a single plugin instance. Requires DAG routing engine.
- **VST3 adapter**: Could be added alongside CLAP via a separate `sonido-plugin-vst3` crate sharing `sonido-gui-core` and the same macro pattern.
- **CLAP note expression** (v0.4): Per-note modulation via `CLAP_EXT_NOTE_EXPRESSION`.

---

## ADR-025: DAG Routing Engine

**Status:** Accepted
**Supersedes:** ADR-005 (for DAG routing path; ADR-005 remains authoritative for linear chain semantics)

### Context

The linear effect chain model (`Vec<Box<dyn Effect>>` ordering) cannot express:

- Parallel signal paths (e.g., dry/wet branches with independent effect chains)
- Sidechain routing (e.g., compressor keyed by a separate signal)
- Multiband processing (e.g., split → per-band effects → recombine)
- Synth-effects hybrid routing (e.g., guitar DI → pitch detector → synth → effects, mixed with dry)

The multi-effect CLAP plugin, synth-effects hybrid, and spectral parallel processing all require graph-based routing.

### Decisions

1. **Directed acyclic graph (DAG)** — nodes are effects, edges are audio connections. The `Effect` trait is node-agnostic; no trait changes needed.

2. **Two-object split** (adapted from Tracktion Engine):
   - `ProcessingGraph` — owned by mutation thread. Holds topology, performs mutations, runs `compile()`.
   - `CompiledSchedule` — immutable snapshot shared with audio thread via `Arc`. Holds flat `Vec<ProcessStep>`, `BufferPool`, and compensation delay lines.

3. **Compiled schedule (flat instruction list)** — Kahn's topological sort at mutation time (O(V+E)). The audio thread executes a linear list of `ProcessStep` variants, never traverses the graph.

4. **Buffer liveness analysis (register allocation)** — instead of one buffer per edge (O(edges) memory), compute liveness intervals: first-write to last-read per virtual buffer. Greedy free-list assignment reuses slots. Result: 20-node linear chain = 2 buffers (ping-pong). Diamond = 3 buffers.

5. **Latency compensation** — each node reports `latency_samples()`. During compilation, compute longest-path latency to each Merge node. Insert `CompensationDelay` steps for shorter parallel paths. Matches the universal algorithm in JUCE, Tracktion, Ardour, and REAPER.

6. **Click-free schedule swap** — when `compile()` produces a new schedule, both old and new run simultaneously during a ~5ms crossfade (via `SmoothedParam`). No audible clicks from topology changes.

7. **Cycle detection at insertion time** — DFS reachability check in `connect()`. Defensive Kahn sort validation at `compile()` time catches any edge case.

8. **Hand-rolled implementation** — surveyed 9 major systems (JUCE, Tracktion, Ardour, REAPER, SuperCollider, Faust, Max/gen~, Bitwig, VCV Rack) and 8 Rust crates (fundsp, dasp_graph, auxide, klingt, audiograph, glicol_synth, dsp-chain, pp-audiograph). No crate serves sonido's combination of stereo-first + `no_std` + live mutation + native `Effect` trait + `ParameterInfo` introspection + `TempoContext` broadcast + variable block size.

9. **`no_std` with alloc** — entire graph module compiles without std. `Arc` from `alloc::sync`. Embeddable on Daisy Seed / Hothouse.

### Node Types

| Kind | Inputs | Outputs | Purpose |
|------|--------|---------|---------|
| `Input` | 0 | 1+ | External audio entry point (exactly 1 per graph) |
| `Output` | 1 | 0 | Final audio output (exactly 1 per graph) |
| `Effect` | 1 | 1 | Wraps `Box<dyn EffectWithParams + Send>`, per-node bypass with crossfade |
| `Split` | 1 | N | Fan-out: copies input to all outputs |
| `Merge` | N | 1 | Fan-in: sums all inputs |

### ProcessStep Instruction Set

| Variant | Purpose |
|---------|---------|
| `WriteInput` | Copy external audio into a buffer slot |
| `ProcessEffect` | Run effect: read `input_buf`, write `output_buf` |
| `SplitCopy` | Copy source buffer to N destination buffers |
| `ClearBuffer` | Zero a buffer before merge accumulation |
| `AccumulateBuffer` | Add source buffer into destination (merge) |
| `DelayCompensate` | Apply compensation delay to align parallel paths |
| `ReadOutput` | Copy buffer to external output |

### Migration Path

All consumers now use `GraphEngine` (CLI, GUI) or `ProcessingGraph` directly (plugin chain). `ProcessingEngine` and the `ChainManager` effect chain have been removed. DAG visual editor is a future feature; current consumers use linear chains through `GraphEngine::new_linear()` / `from_chain()`.

### Alternatives Considered

- **dasp_graph / fundsp Net** — generic graph abstractions that don't support sonido's stereo-first `Effect` trait or `ParameterInfo`. Wrapping would add complexity without benefit.
- **One buffer per edge** — simple but O(edges) memory. Liveness analysis gives O(max-live) which is much smaller for practical topologies.
- **Multi-threaded parallel execution** — Tracktion's activation-count work-stealing pattern. Deferred to v0.4; the `CompiledSchedule` architecture supports it without structural changes (steps become work units with activation counters).

### References

- `crates/sonido-core/src/graph/` — implementation (processing.rs, node.rs, edge.rs, buffer.rs, schedule.rs)
- `crates/sonido-io/src/graph_engine.rs` — `GraphEngine` integration layer
- `docs/ROADMAP.md` — DAG Routing Engine section (v0.3+)
- `docs/ARCHITECTURE.md` — DAG data flow section
- ADR-005 — original linear chain decision (remains the record for v0.1 architecture)
- JUCE AudioProcessorGraph — compiled schedule + liveness analysis reference
- Tracktion Engine — immutable graph + atomic swap pattern reference

---

## ADR-026: Multi-Effect Chain CLAP Plugin

**Status:** Accepted

### Context

Single-effect plugins (ADR-024) work well individually, but DAW users need a multi-effect chain in a single plugin instance. This requires dynamic effect add/remove/reorder at runtime while maintaining stable CLAP parameter IDs across sessions.

### Decision

Pre-allocate a flat parameter space: **16 slots × 32 params = 512 CLAP parameter IDs**. Slot metadata (effect ID, descriptors, active flag) is published via `ArcSwap` for wait-free reads. Structural mutations (add/remove/reorder) flow through a `Mutex<VecDeque<ChainCommand>>` drained by the audio thread.

**Key design choices:**

1. **`ClapParamId` validated newtype** — encodes `slot * 32 + local_param_index` in a `u32`. Validated at construction; all flat-space access goes through this type. `const fn` methods are zero-cost.

2. **Direct effect ownership** — the audio processor stores effects as `Vec<Option<Box<dyn EffectWithParams + Send>>>` rather than using `ProcessingGraph`. Reason: `ProcessingGraph::effect_mut()` returns `&mut dyn Effect` which loses the `ParameterInfo` vtable needed for parameter synchronization with the host.

3. **Param cache diffing** — the audio processor maintains a `param_cache: Vec<f32>` mirroring the last-seen shared values. Each process cycle, it diffs against `ChainShared` atomics and emits CLAP `ParamValueEvent` (with gesture begin/end) only for changed parameters. This correctly attributes GUI-originated changes to the host.

4. **`ChainMutator` trait** — abstraction over chain reorder operations shared between standalone GUI (`AtomicParamBridge`) and plugin GUI (`ChainParamBridge`). Minimal surface: `get_order()` + `move_effect()`.

5. **`&'static str` for effect IDs** — `SlotSnapshot::effect_id` is `&'static str` (not `String`) because all effect IDs come from `EffectDescriptor::id` which is `&'static str`. This avoids lifetime issues when `ParamBridge::effect_id()` returns `&str` through an `ArcSwap` guard.

6. **Host callbacks** — `ChainShared` stores both `request_process` (for GUI param changes) and `request_callback` (for structural mutations requiring main-thread rescan). `needs_rescan` atomic flag coordinates rescan dispatch.

### Alternatives Considered

- **Dynamic parameter registration** — CLAP supports `CLAP_PARAM_RESCAN_ALL` to add/remove params at runtime. Rejected: many hosts don't handle this well, and pre-allocation with hidden unused params is the standard approach (CLAP spec §params recommends it for multi-slot instruments).
- **ProcessingGraph for chain** — would provide latency compensation and buffer optimization. Rejected for v0.2: the `Effect`-only vtable from `effect_mut()` can't drive `ParameterInfo` sync. May revisit in v0.3 with a `EffectWithParams`-aware graph node type.
- **One parameter map per slot** — cleaner but requires runtime HashMap lookup per param access. The flat array with `ClapParamId` encoding is O(1) with zero allocation.

### References

- `crates/sonido-plugin/src/chain/` — implementation (mod.rs, shared.rs, audio.rs, main_thread.rs, param_bridge.rs, gui.rs)
- `crates/sonido-plugin/examples/sonido-chain.rs` — CLAP entry point
- `docs/ARCHITECTURE.md` — plugin section (multi-effect chain)
- ADR-024 — single-effect CLAP plugin architecture (foundation)
- CLAP specification §params — parameter pre-allocation pattern for multi-slot instruments
