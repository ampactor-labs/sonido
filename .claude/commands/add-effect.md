# Add New Effect Workflow

Complete workflow for adding a new kernel effect to sonido.

## Pre-flight

Read a similar existing kernel for pattern reference:
- Simple effect: `crates/sonido-effects/src/kernels/distortion.rs`
- Complex effect: `crates/sonido-effects/src/kernels/reverb.rs`
- Modulation effect: `crates/sonido-effects/src/kernels/chorus.rs`

## Step 1: Create kernel file

Create `crates/sonido-effects/src/kernels/my_effect.rs`:

```rust
//! My Effect -- one-line description of the DSP algorithm.
//!
//! Theory section: what problem it solves, mathematical basis,
//! reference source (paper/textbook/cookbook).

use sonido_core::kernel::traits::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::param_info::{ParamDescriptor, ParamFlags, ParamId, ParamScale};

/// Parameter struct for MyEffect.
///
/// ## Parameters
/// - `param_a`: Description (min to max, default X)
/// - `mix`: Wet/dry ratio (0.0 to 1.0, default 0.5)
/// - `output`: Output level in dB (-24.0 to 12.0, default 0.0)
#[derive(Clone, Default)]
pub struct MyEffectParams {
    pub param_a: f32,
    pub mix: f32,
    pub output_db: f32,
}

impl MyEffectParams {
    /// Maps normalized ADC knob values (0.0-1.0) to typed parameters.
    /// Used for embedded deployment (Daisy Seed / Hothouse).
    pub fn from_knobs(knob_a: f32, knob_mix: f32, knob_output: f32) -> Self {
        Self {
            param_a: /* scale knob_a to param range */,
            mix: knob_mix,
            output_db: -24.0 + knob_output * 36.0,
        }
    }
}

impl KernelParams for MyEffectParams {
    const COUNT: usize = 3;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(ParamDescriptor::custom("Param A", "ParamA", 0.0, 100.0, 50.0)
                .with_id(ParamId(2000), "my_effect_param_a")),
            1 => Some(ParamDescriptor::mix()
                .with_id(ParamId(2001), "my_effect_mix")),
            2 => Some(sonido_core::gain::output_level_param()
                .with_id(ParamId(2002), "my_effect_output")),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Standard,  // 10ms
            1 => SmoothingStyle::Standard,  // 10ms
            2 => SmoothingStyle::Fast,      // 5ms
            _ => SmoothingStyle::None,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.param_a,
            1 => self.mix,
            2 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.param_a = value,
            1 => self.mix = value,
            2 => self.output_db = value,
            _ => {}
        }
    }
}

/// MyEffect kernel -- pure DSP state.
pub struct MyEffectKernel {
    sample_rate: f32,
    // DSP state fields here (filters, delay lines, etc.)
}

impl MyEffectKernel {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
        }
    }
}

impl DspKernel for MyEffectKernel {
    type Params = MyEffectParams;

    fn process_stereo(&mut self, left: f32, right: f32, params: &Self::Params) -> (f32, f32) {
        // DSP processing here
        // Use sonido_core::math::wet_dry_mix() for mix parameter
        // Use sonido_core::gain::db_to_linear() for output level
        (left, right)
    }

    fn reset(&mut self) {
        // Clear ALL state: delay buffers, filter history, LFO phase, etc.
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        // Recalculate any sample-rate-dependent coefficients
    }
}
```

## Step 2: Add module + re-export

In `crates/sonido-effects/src/kernels/mod.rs`:
```rust
pub mod my_effect;
pub use my_effect::{MyEffectKernel, MyEffectParams};
```

In `crates/sonido-effects/src/lib.rs`:
```rust
pub use kernels::{MyEffectKernel, MyEffectParams};
```

## Step 3: Register in registry

In `crates/sonido-registry/src/lib.rs`, add to `register_builtin_effects()`:

```rust
self.register(
    EffectDescriptor {
        id: "my_effect",
        name: "My Effect",
        description: "Description of the effect",
        category: EffectCategory::Modulation, // or Dynamics, Filter, Delay, Reverb, Distortion, Utility
        param_count: 3,
    },
    |sr| Box::new(KernelAdapter::new(MyEffectKernel::new(sr), sr)),
);
```

Update the import at the top of the file. Adjust test assertions for `registry.len()` and category counts.

## Step 4: Add to CLI effect list

In `crates/sonido-cli/src/commands/effects.rs`, add to the effect listing if needed.

## Step 5: Add regression test + golden file

```bash
REGENERATE_GOLDEN=1 cargo test --test regression -p sonido-effects
```

Verify metrics pass: MSE < 1e-6, SNR > 60 dB, spectral correlation > 0.9999.

## Step 6: Update docs

- `docs/EFFECTS_REFERENCE.md` -- add effect entry
- `README.md` -- update effect count
- `CLAUDE.md` Key Files table -- if needed
- `docs/DSP_FUNDAMENTALS.md` -- if new DSP algorithm
- `docs/DESIGN_DECISIONS.md` -- ADR if architectural choice involved

## ParamId Guidance

Next available base: **2000** (Stage=1900 is the highest current base). Assign sequential params from the base: 2000, 2001, 2002, etc.

## Common Pitfalls

- **no_std math**: Use `libm::sinf()` / `libm::floorf()`, never `f32::sin()` in the DSP crates
- **Frozen string_ids**: Once published, `string_id` values in `with_id()` can never be renamed
- **SmoothedParam advance()**: The kernel never calls this -- `KernelAdapter` handles it. But if you use `SmoothedParam` directly in a kernel, call `advance()` per sample.
- **Stable indices**: Parameter indices are part of the public API. Never reorder existing params; add new ones at the end.
- **reset() must clear ALL state**: Delay buffers, filter history, LFO phase, envelope state. Missing any causes bleed.
- **is_true_stereo()**: Return `true` only if L/R outputs are decorrelated (different delay times, LFO phases, etc.)
