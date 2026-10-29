# Your First Sonido Effect in 30 Minutes

A step-by-step guide to implementing a new DSP effect using the kernel architecture.

## Architecture Overview

Every effect is built from three layers:

```
XxxParams  (KernelParams)              — typed parameter struct, metadata, smoothing hints
XxxKernel  (DspKernel)                 — pure DSP state: filters, delay lines, ADAA state
Adapter<XxxKernel, SmoothedPolicy>     — bridges to Effect + ParameterInfo for all consumers
```

The kernel never owns parameters. Parameters are passed in on every call.
This lets the same kernel run on embedded hardware (raw ADC values) and desktop
(smoothed, host-normalized values) without any conditional code.

Reference: `docs/KERNEL_ARCHITECTURE.md` (full design rationale)

## Step 1: Define Your Params Struct

Create `crates/sonido-effects/src/kernels/mygain.rs`:

```rust
//! Simple gain effect — illustrates the kernel architecture.

use sonido_core::kernel::{KernelParams, SmoothingStyle};
use sonido_core::{ParamDescriptor, ParamId};

/// Parameters for the gain kernel.
///
/// Stored in user-facing units (dB). The kernel converts internally.
#[derive(Debug, Clone, Copy)]
pub struct GainParams {
    /// Output gain in dB. Range: -60.0 to +12.0. Default: 0.0.
    pub gain_db: f32,
}

impl Default for GainParams {
    fn default() -> Self {
        Self { gain_db: 0.0 }
    }
}
```

The defaults should be the setting where the effect sounds best: a freshly
opened plugin starts at the descriptor defaults, and a pedal knob at noon
selects them, so choose the setting you want heard first. `Default` must return the same values as the
descriptors (Step 2); `crates/sonido-effects/tests/default_parity.rs` checks
every kernel, so add a `check::<GainParams>("mygain", &mut v)` line there.

## Step 2: Implement KernelParams

```rust
impl KernelParams for GainParams {
    const COUNT: usize = 1;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(
                ParamDescriptor::gain_db("Gain", "Gain", -60.0, 12.0, 0.0)
                    .with_id(ParamId(3700), "mygain_gain"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Standard, // 10ms
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.gain_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.gain_db = value,
            _ => {}
        }
    }
}
```

**Conventions:**
- `ParamId` must be unique across all effects. Each effect owns a block of 100
  (distortion 200.., reverb 1500.., harmonic_habitat 3600..); grep
  `crates/sonido-effects/src/kernels` for `ParamId(` and take the next free block.
- `string_id` is the serialization key, so never rename it once shipped.
- Use `SmoothingStyle::None` for stepped/enum params, `Slow` (20ms) for filter coefficients.
- When a factory descriptor's default doesn't suit the effect (`ParamDescriptor::mix()`
  and `depth()` default to 50%), set the effect's own default with `.with_default(v)`.

## Step 3: Implement DspKernel

```rust
use sonido_core::kernel::DspKernel;
use sonido_core::fast_db_to_linear;

/// Pure DSP state for the gain effect. No parameter values stored here.
pub struct GainKernel;

impl GainKernel {
    /// Every kernel exposes `new(sample_rate)`; the registry factory and
    /// `test_kernel!` both construct kernels through it.
    pub fn new(_sample_rate: f32) -> Self {
        Self
    }
}

impl DspKernel for GainKernel {
    type Params = GainParams;

    /// Process a mono sample. Stereo derives from this automatically.
    fn process(&mut self, input: f32, params: &GainParams) -> f32 {
        input * fast_db_to_linear(params.gain_db)
    }

    fn reset(&mut self) {
        // No state to clear for a pure gain.
    }

    fn set_sample_rate(&mut self, _sample_rate: f32) {
        // No coefficients to recalculate.
    }
}
```

**Rules:**
- Use `libm::sinf()` / `libm::expf()`, never `f32::sin()` (it breaks `no_std`).
- `reset()` must clear all mutable state: filter memories, delay buffers, LFO phases.
- `set_sample_rate()` must invalidate cached coefficients (set the cache to `NaN`
  and test it with `needs_update`, or call `Cached::invalidate`; see
  [Cached Filter Coefficients](#cached-filter-coefficients)).
- Override `process_stereo()` directly for true stereo (decorrelated L/R) effects.
- An effect with a mix control mixes with `Self::blend` / `Self::blend_stereo`,
  never its own crossfade (see [Wet/Dry Mix](#wetdry-mix)).
- The kernel's level calibration, `const LEVEL`, comes in Step 5.

## Step 4: Register the Effect

### 4a. Add to kernels/mod.rs

```rust
// crates/sonido-effects/src/kernels/mod.rs
pub mod mygain;
pub use mygain::{GainKernel, GainParams};
```

### 4b. Re-export from the crate root

Add `GainKernel, GainParams` to the `pub use kernels::{...}` list in
`crates/sonido-effects/src/lib.rs`. There are no per-effect type aliases;
consumers wrap kernels in `Adapter<K, SmoothedPolicy>` (the default policy) via
`Adapter::new(kernel, sample_rate)`.

### 4c. Register in the Effect Registry

In `crates/sonido-registry/src/lib.rs`, append a stable UID to `EFFECT_UIDS`
(append-only; the next free value is 37) and register the factory in
`register_builtin_effects()`:

```rust
("mygain", 37),
```

```rust
self.register(
    EffectDescriptor {
        id: "mygain",
        name: "My Gain",
        short_name: "GAIN",
        description: "Simple output gain",
        category: EffectCategory::Utility,
        param_count: 0, // filled in by register()
    },
    |sr| Box::new(Adapter::new(GainKernel::new(sr), sr)),
);
```

### 4d. Ship it in the plugin bundle

Add a `spec("mygain", "com.sonido.mygain", "Sonido My Gain", &[...])` entry to
`CATALOG` in `crates/sonido-plugin/src/bundle.rs`.
`crates/sonido-plugin/tests/plugin_inventory.rs` fails if a registered effect
has no catalog entry. Tests that hard-code the effect count (e.g.
`crates/sonido-config/src/validation.rs`) also need bumping.

## Step 5: Calibrate Its Level

Every effect meets one level contract (`sonido_core::level`): at its defaults it
plays at the loudness of its input, and its mix control does not change that
loudness. The level is measured, never chosen by hand. With the effect
registered, run:

```bash
cargo run -p sonido-effects --example calibrate --release
```

It plays `sonido_analysis::signals::reference_guitar` (a synthetic strummed
guitar peaking at `NOMINAL_PEAK_DBFS`, -20 dBFS) through every registered
effect, fully wet when it has a mix control (a parameter matching the key
`mix`) and at its defaults otherwise, measures the loudness change as
integrated LUFS (ITU-R BS.1770-4), and regenerates
`crates/sonido-effects/src/calibration.rs` for all of them. Your effect gets an
entry named after its registry ID, uppercased (`MYGAIN`). Declare it on the
kernel:

```rust
use sonido_core::level::Level;

impl DspKernel for GainKernel {
    type Params = GainParams;
    const LEVEL: Level = crate::calibration::MYGAIN;
    // ...
}
```

Do this before running the example again: it scales the trim already in the
table by the error it measures, which is only correct if the kernel applies that
trim.

`Level` holds a `trim` and, for an effect with a mix control, the measured
correlation of its wet signal with the dry input (`mix_correlation`).
`Self::blend` applies the trim to the wet signal. An effect without a mix
control passes its sound through `Self::level` (or `Self::level_stereo`) where
it leaves the processing: before the output knob and any `soft_limit`, so the
limiter acts on the final level. Either way the kernel itself is calibrated,
with or without an `Adapter`.

Run the example again whenever you change a kernel's sound or its defaults.
The level contract test fails until you do:

```bash
cargo test -p sonido-effects --test level_contract
```

It holds every registered effect except the looper and tuner (uncalibrated
utilities) to two limits: within 0.5 dB of the dry loudness fully wet (at its
defaults, if it has no mix control), and within 1.5 dB at 0, 25, 50 and 75% mix
and at its default mix. Never edit `calibration.rs` by hand, and don't
level-match with the output parameter or a fixed gain inside the kernel.

## Step 6: Add a test_kernel! Test

Add to `crates/sonido-effects/src/kernels/mygain.rs`. `test_kernel!` (in
`crates/sonido-effects/src/test_utils.rs`) generates the standard invariant
suite: descriptor validity, finite output, reset, morph, extreme inputs.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use sonido_core::kernel::DspKernel;

    crate::test_kernel!(GainKernel, GainParams);

    #[test]
    fn unity_gain_passes_signal() {
        let mut kernel = GainKernel;
        let params = GainParams { gain_db: 0.0 };
        let (l, r) = kernel.process_stereo(0.5, -0.5, &params);
        assert!((l - 0.5).abs() < 1e-6);
        assert!((r + 0.5).abs() < 1e-6);
    }

    #[test]
    fn reset_is_safe() {
        let mut kernel = GainKernel;
        kernel.reset(); // must not panic
    }
}
```

## Step 7: Golden File Setup

Add a test to `crates/sonido-effects/tests/regression.rs`:

```rust
#[test]
fn test_mygain_regression() {
    let mut effect = Adapter::new(GainKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, -6.0); // gain_db

    let input = generate_test_signal(TEST_DURATION_SAMPLES);
    run_regression_test("mygain", effect, &input).expect("MyGain regression test failed");
}
```

Generate the initial golden file after Step 5, and regenerate it whenever a new
calibration changes this effect's trim (the trim is part of its output):

```bash
REGENERATE_GOLDEN=1 cargo test --test regression -p sonido-effects -- mygain
```

Run the regression suite to verify:

```bash
cargo test --test regression -p sonido-effects -- mygain
```

Thresholds: MSE < 1e-6, SNR > 60 dB, spectral correlation > 0.9999.

## Common Patterns

### Cached Filter Coefficients

```rust
use sonido_core::{Biquad, lowpass_coefficients, needs_update};

pub struct LowpassKernel {
    biquad: Biquad,
    sample_rate: f32,
    cached_freq: f32, // NaN = needs recalc
}

impl DspKernel for LowpassKernel {
    type Params = LowpassParams;

    fn process(&mut self, input: f32, params: &LowpassParams) -> f32 {
        if needs_update(self.cached_freq, params.cutoff_hz, 0.01) {
            let (b0, b1, b2, a0, a1, a2) =
                lowpass_coefficients(params.cutoff_hz, 0.707, self.sample_rate);
            self.biquad.set_coefficients(b0, b1, b2, a0, a1, a2);
            self.cached_freq = params.cutoff_hz;
        }
        self.biquad.process(input)
    }

    fn reset(&mut self) {
        self.biquad.clear();
        self.cached_freq = f32::NAN; // force recalc on next sample
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.cached_freq = f32::NAN;
    }
}
```

`needs_update(cached, new, epsilon)` fires when `cached` is `NaN` or `new` has
moved more than `epsilon`. Don't write the test as
`(new - cached).abs() > epsilon`: every comparison with `NaN` is false, so a
cache initialized to `NaN` never fires and the coefficient is never computed
(that left the plate reverb with no tank feedback). `sonido_core::Cached` wraps
the same pattern for several inputs; see `FilterKernel` in
`crates/sonido-effects/src/kernels/filter.rs`.

### ADAA (Anti-Derivative Anti-Aliasing)

Wrap nonlinear waveshapers to suppress aliasing without oversampling:

```rust
use sonido_core::{Adaa1, soft_clip, soft_clip_ad};

type AdaaProc = Adaa1<fn(f32) -> f32, fn(f32) -> f32>;

pub struct ClipKernel {
    adaa: AdaaProc,
}

impl ClipKernel {
    pub fn new(_sample_rate: f32) -> Self {
        Self {
            adaa: Adaa1::new(soft_clip as fn(f32) -> f32, soft_clip_ad as fn(f32) -> f32),
        }
    }
}

impl DspKernel for ClipKernel {
    type Params = ClipParams;

    fn process(&mut self, input: f32, _params: &ClipParams) -> f32 {
        self.adaa.process(input)
    }

    fn reset(&mut self) {
        self.adaa.reset();
    }

    fn set_sample_rate(&mut self, _sample_rate: f32) {}
}
```

`DistortionKernel` (`crates/sonido-effects/src/kernels/distortion.rs`) uses
this exact pattern for its four waveshapers.

### Wet/Dry Mix

```rust
fn process(&mut self, input: f32, params: &MyParams) -> f32 {
    let wet = self.do_processing(input, params);
    Self::blend(input, wet, params.mix_pct / 100.0)
}
```

`blend` and `blend_stereo(dry_l, dry_r, wet_l, wet_r, mix)` are provided by
`DspKernel`. They scale the wet signal by the kernel's `LEVEL` trim and
crossfade on a constant-power sine/cosine law normalized for the measured
wet/dry correlation, so turning the mix knob doesn't change the level
(`crates/sonido-core/src/level.rs` has the math). Mix 0 returns the dry input
exactly; mix 1 returns the trimmed wet. Gain that depends on a parameter goes
on the wet before the blend (delay, flanger and phaser scale it by
`gain::feedback_wet_compensation(feedback)`), and the output parameter after.
`sonido_core::math::wet_dry_mix` is a plain linear crossfade, used by the graph
engine for bypass fades, not by kernels.

## Dynamic Loading (Experimental)

To expose your effect as a `cdylib` for runtime loading, implement the
C ABI defined in `sonido_core::plugin_host`:

- `EffectDescriptorC { id, name, param_count, version }`: `id` and `name` are
  null-terminated C strings (`b"mygain\0".as_ptr() as *const core::ffi::c_char`);
  `version` is `major * 10000 + minor * 100 + patch`.
- `EffectVTable`: `create(sample_rate)`, `destroy`, `process_stereo`,
  `set_param`, `get_param`, `reset` as `extern "C"` function pointers over an
  opaque `instance` pointer.

`EffectDescriptorC` holds raw pointers and does not implement `Sync`, so
exporting it as a `static` needs a wrapper type with `unsafe impl Sync` in the
exporting crate. No exporter or loader exists yet: runtime loading is not implemented. See `crates/sonido-core/src/plugin_host.rs`
for the type definitions and ABI contract.
