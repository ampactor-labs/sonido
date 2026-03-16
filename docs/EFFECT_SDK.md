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

## Step 2: Implement KernelParams

```rust
impl KernelParams for GainParams {
    const COUNT: usize = 1;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(
                ParamDescriptor::gain_db("Gain", "Gain", -60.0, 12.0, 0.0)
                    .with_id(ParamId(2100), "mygain_gain"),
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
- `ParamId` must be unique across all effects. See `CLAUDE.md` Stable ParamIds table.
- `string_id` is the serialization key — never rename once shipped.
- Use `SmoothingStyle::None` for stepped/enum params, `Slow` (20ms) for filter coefficients.

## Step 3: Implement DspKernel

```rust
use sonido_core::kernel::DspKernel;
use sonido_core::fast_db_to_linear;

/// Pure DSP state for the gain effect. No parameter values stored here.
pub struct GainKernel;

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
- Use `libm::sinf()` / `libm::expf()` — never `f32::sin()` (breaks `no_std`).
- `reset()` must clear ALL mutable state: filter memories, delay buffers, LFO phases.
- `set_sample_rate()` must invalidate cached coefficients (NaN sentinel pattern).
- Override `process_stereo()` directly for true stereo (decorrelated L/R) effects.

## Step 4: Register the Effect

### 4a. Add to kernels/mod.rs

```rust
// crates/sonido-effects/src/kernels/mod.rs
pub mod mygain;
pub use mygain::{GainKernel, GainParams};
```

### 4b. Add to effects lib.rs

```rust
// crates/sonido-effects/src/lib.rs
use sonido_core::kernel::{Adapter, SmoothedPolicy};
pub type GainEffect = Adapter<kernels::GainKernel, SmoothedPolicy>;
```

### 4c. Register in the Effect Registry

```rust
// crates/sonido-registry/src/lib.rs
"mygain" => {
    let k = kernels::GainKernel;
    Box::new(Adapter::new(k, sample_rate))
}
```

## Step 5: Add a test_kernel! Test

Add to `crates/sonido-effects/src/kernels/mygain.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use sonido_core::kernel::DspKernel;

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

## Step 6: Golden File Setup

Add your effect to `crates/sonido-effects/tests/regression.rs`:

```rust
test_effect!("mygain", GainEffect::default_at(48000.0));
```

Generate the initial golden file:

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
pub struct FilterKernel {
    biquad: Biquad,
    cached_freq: f32, // NaN = needs recalc
}

impl DspKernel for FilterKernel {
    fn process(&mut self, input: f32, params: &FilterParams) -> f32 {
        if params.cutoff_hz != self.cached_freq {
            self.biquad = Biquad::lowpass(params.cutoff_hz, self.sample_rate);
            self.cached_freq = params.cutoff_hz;
        }
        self.biquad.process(input)
    }

    fn reset(&mut self) {
        self.biquad.reset();
        self.cached_freq = f32::NAN; // force recalc on next sample
    }
}
```

### ADAA (Anti-Derivative Anti-Aliasing)

Wrap nonlinear waveshapers to suppress aliasing without oversampling:

```rust
use sonido_core::{Adaa1, soft_clip, soft_clip_ad};

pub struct ClipKernel {
    adaa: Adaa1,
}

impl DspKernel for ClipKernel {
    fn process(&mut self, input: f32, _params: &ClipParams) -> f32 {
        self.adaa.process(input, soft_clip, soft_clip_ad)
    }

    fn reset(&mut self) {
        self.adaa = Adaa1::default();
    }
}
```

### Wet/Dry Mix

```rust
use sonido_core::math::wet_dry_mix;

fn process(&mut self, input: f32, params: &MyParams) -> f32 {
    let wet = self.do_processing(input, params);
    wet_dry_mix(input, wet, params.mix_pct / 100.0)
}
```

## Dynamic Loading (Experimental)

To expose your effect as a `cdylib` for runtime loading, implement the
C ABI defined in `sonido_core::plugin_host`:

```rust
use sonido_core::plugin_host::{EffectDescriptorC, EffectVTable};

#[no_mangle]
pub static EFFECT_DESCRIPTOR: EffectDescriptorC = EffectDescriptorC {
    id: b"mygain\0".as_ptr() as *const core::ffi::c_char,
    name: b"My Gain\0".as_ptr() as *const core::ffi::c_char,
    param_count: 1,
    version: 10000, // 1.0.0
};
```

Full runtime loading is not yet implemented. See `crates/sonido-core/src/plugin_host.rs`
for the type definitions and ABI contract.
