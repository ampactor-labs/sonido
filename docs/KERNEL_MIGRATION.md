# Kernel Migration Guide

> **Migration is complete as of v0.2** -- all 19 classic `Effect` implementations have been removed. All effects now use the `DspKernel` + `KernelAdapter` architecture. This guide is retained as a reference for adding new effects using the kernel pattern.

How to add a new Sonido effect using the kernel architecture. The kernel pattern separates pure DSP from parameter ownership. This guide uses the Distortion effect as the reference implementation.

## Why the Kernel Pattern

The old pattern (now deleted) coupled DSP math with parameter ownership:

```
┌─────────────────────────────────┐
│ Distortion (old — being replaced)│
│                                 │
│  drive: SmoothedParam      ◄── parameter ownership
│  tone_filter: Biquad       ◄── DSP state
│  adaa_soft_l: Adaa1        ◄── DSP state
│  ...                            │
│                                 │
│  impl Effect                ◄── DSP + smoothing interleaved
│  impl ParameterInfo         ◄── separate, manually synced
└─────────────────────────────────┘
```

The kernel pattern separates them:

```
┌──────────────────────┐     ┌──────────────────────┐
│ DistortionParams     │     │ DistortionKernel      │
│                      │     │                       │
│  drive_db: f32       │     │  tone_filter: Biquad  │
│  tone_db: f32        │     │  adaa_soft_l: Adaa1   │
│  output_db: f32      │     │  adaa_soft_r: Adaa1   │
│  shape: f32          │     │  ...                   │
│  mix_pct: f32        │     │                       │
│                      │     │  impl DspKernel        │
│  impl KernelParams   │     │    type Params =       │
│   (one definition)   │     │      DistortionParams  │
└──────────────────────┘     └──────────────────────┘
            │                           │
            └───────────┬───────────────┘
                        ▼
           ┌──────────────────────┐
           │ KernelAdapter<K>     │  (provided by sonido-core)
           │                      │
           │  impl Effect         │  ← automatic
           │  impl ParameterInfo  │  ← automatic
           │  owns SmoothedParams │  ← managed by adapter
           └──────────────────────┘
```

Benefits:
- **Kernel is platform-independent** — identical binary on x86_64 and ARM Cortex-M7
- **One parameter definition** — no separate `impl_params!` to keep in sync
- **Smoothing at the boundary** — adapter smooths on desktop, embedded skips it
- **Testable in isolation** — kernel can be tested with raw `&Params`, no smoothing noise

## Step-by-Step Migration

### Step 1: Identify the Split

Open the existing effect. Separate fields into two categories:

**DSP state** (stays in kernel):
- Filters (`Biquad`, `Svf`, `OnePole`)
- Delay lines
- ADAA processors
- LFO / oscillator phase state
- Cached coefficients
- `sample_rate`

**Parameter values** (moves to params struct):
- `SmoothedParam` fields → plain `f32` in user-facing units
- Enum selections (waveshape, mode) → `f32` index
- Anything exposed via `impl_params!`

**Example — Distortion:**

| Classic field | Category | Kernel destination |
|--------------|----------|-------------------|
| `drive: SmoothedParam` | Parameter | `DistortionParams::drive_db: f32` |
| `tone_filter: Biquad` | DSP state | `DistortionKernel::tone_filter` |
| `output_level: SmoothedParam` | Parameter | `DistortionParams::output_db: f32` |
| `waveshape: WaveShape` | Parameter | `DistortionParams::shape: f32` |
| `mix: SmoothedParam` | Parameter | `DistortionParams::mix_pct: f32` |
| `adaa_soft_l: Adaa1` | DSP state | `DistortionKernel::adaa_soft_l` |
| `tone_gain_db: f32` | Cached | `DistortionKernel::last_tone_db` |

### Step 2: Define the Params Struct

Create `crates/sonido-effects/src/kernels/<effect>.rs`.

```rust
#[derive(Debug, Clone, Copy)]
pub struct DistortionParams {
    pub drive_db: f32,
    pub tone_db: f32,
    pub output_db: f32,
    pub shape: f32,
    pub mix_pct: f32,
}
```

Rules:
- **User-facing units** — same as `ParamDescriptor::min/max/default` (dB, %, ms, index)
- **`f32` only** — no enums, no `SmoothedParam`, no atomics
- **`Clone + Copy + Default + Send`** — the struct is passed by reference every sample
- **Field order = parameter index order** — convention, not enforced

### Step 3: Implement `KernelParams`

```rust
impl KernelParams for DistortionParams {
    const COUNT: usize = 5;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(ParamDescriptor::gain_db("Drive", "Drive", 0.0, 40.0, 12.0)
                    .with_id(ParamId(200), "dist_drive")),
            // ... same descriptors as the original impl_params!
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Fast,       // drive — fast for feel
            1 => SmoothingStyle::Slow,       // tone — filter coeff, avoid zipper
            2 => SmoothingStyle::Standard,   // output level
            3 => SmoothingStyle::None,       // waveshape — discrete, snap
            4 => SmoothingStyle::Standard,   // mix
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.drive_db,
            1 => self.tone_db,
            2 => self.output_db,
            3 => self.shape,
            4 => self.mix_pct,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.drive_db = value,
            1 => self.tone_db = value,
            2 => self.output_db = value,
            3 => self.shape = value,
            4 => self.mix_pct = value,
            _ => {}
        }
    }
}
```

Critical: **`ParamId` values and `string_id`s MUST match the original** — these are plugin host API contracts. Changing them breaks saved automation and presets.

### Step 4: Define the Kernel Struct

```rust
pub struct DistortionKernel {
    sample_rate: f32,
    tone_filter: Biquad,
    tone_filter_r: Biquad,
    adaa_soft_l: AdaaProc,
    adaa_soft_r: AdaaProc,
    // ... all DSP state, NO parameters
    last_tone_db: f32,  // coefficient cache
}
```

Rules:
- **No `SmoothedParam`** — ever
- **No parameter values** — only DSP state
- **`sample_rate` stays** — needed for coefficient recalculation
- **Coefficient caches** are DSP state, not parameters

### Step 5: Implement `DspKernel`

Move the DSP math from the original `Effect::process_stereo()` into `DspKernel::process_stereo()`. The key change: parameter values come from `&Params` instead of `self.xxx.advance()`.

```rust
impl DspKernel for DistortionKernel {
    type Params = DistortionParams;

    fn process_stereo(
        &mut self,
        left: f32,
        right: f32,
        params: &DistortionParams,
    ) -> (f32, f32) {
        // Coefficient update (only when tone changes)
        if (params.tone_db - self.last_tone_db).abs() > 0.001 {
            self.update_tone_coefficients(params.tone_db);
        }

        // Unit conversion (user-facing → internal)
        let drive = db_to_gain(params.drive_db);
        let output = db_to_gain(params.output_db);
        let mix = params.mix_pct / 100.0;

        // ... identical DSP math from here
    }

    fn reset(&mut self) {
        self.tone_filter.clear();
        // ... reset all DSP state
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.update_tone_coefficients(self.last_tone_db);
    }
}
```

Key patterns:
- **`params.xxx` replaces `self.xxx.advance()`** — values are already smoothed (or not, on embedded)
- **Unit conversion in the kernel** — `db_to_gain(params.drive_db)` not in the params struct
- **Coefficient caching** — compare current param value to cached, recompute only on change
- **No `advance()` calls** — the kernel sees instantaneous values

### Step 6: Add `from_knobs()` for Embedded

```rust
impl DistortionParams {
    pub fn from_knobs(drive: f32, tone: f32, output: f32, shape: f32, mix: f32) -> Self {
        Self {
            drive_db: drive * 40.0,            // 0–40 dB
            tone_db: tone * 24.0 - 12.0,       // −12–12 dB
            output_db: output * 40.0 - 20.0,   // −20–20 dB
            shape: (shape * 3.99).floor(),      // 0, 1, 2, 3
            mix_pct: mix * 100.0,               // 0–100%
        }
    }
}
```

This maps normalized ADC readings (0.0–1.0) to parameter ranges. On the Daisy Seed:

```rust
// Audio callback — no adapter, no smoothing, no allocation
fn audio_callback(kernel: &mut DistortionKernel, buffer: &mut AudioBuffer) {
    let params = DistortionParams::from_knobs(
        adc.read(0),  // drive pot
        adc.read(1),  // tone pot
        adc.read(2),  // output pot
        adc.read(3),  // shape selector
        adc.read(4),  // mix pot
    );
    for frame in buffer.frames_mut() {
        let (l, r) = kernel.process_stereo(frame.left, frame.right, &params);
        frame.left = l;
        frame.right = r;
    }
}
```

### Step 7: Register in the Kernels Module

Add to `crates/sonido-effects/src/kernels/mod.rs`:

```rust
pub mod distortion;
pub use distortion::{DistortionKernel, DistortionParams};
```

And in `crates/sonido-effects/src/lib.rs`:

```rust
pub mod kernels;
pub use kernels::{DistortionKernel, DistortionParams};
```

### Step 8: Write Tests

Three categories:

**Kernel unit tests** — raw kernel with explicit params, no smoothing:
```rust
#[test]
fn kernel_silence_in_silence_out() {
    let mut kernel = DistortionKernel::new(48000.0);
    let params = DistortionParams::default();
    let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
    assert!(l.abs() < 1e-6);
    assert!(r.abs() < 1e-6);
}
```

**Adapter integration tests** — kernel wrapped in adapter, testing `Effect` interface:
```rust
#[test]
fn adapter_wraps_kernel_as_effect() {
    let mut adapter = KernelAdapter::new(DistortionKernel::new(48000.0), 48000.0);
    adapter.reset();
    let output = adapter.process(0.3);
    assert!(!output.is_nan());
}
```

**Behavioral tests** — verify DSP correctness directly (no reference to old effect):
```rust
#[test]
fn drive_increases_amplitude() {
    let mut kernel = DistortionKernel::new(48000.0);
    let low = DistortionParams { drive_db: 6.0, ..Default::default() };
    let high = DistortionParams { drive_db: 30.0, ..Default::default() };

    let (low_l, _) = kernel.process_stereo(0.2, 0.2, &low);
    kernel.reset();
    let (high_l, _) = kernel.process_stereo(0.2, 0.2, &high);
    assert!(high_l.abs() > low_l.abs());
}

#[test]
fn morph_produces_valid_output() {
    let mut kernel = DistortionKernel::new(48000.0);
    let a = DistortionParams::default();
    let b = DistortionParams { drive_db: 35.0, mix_pct: 100.0, ..Default::default() };
    for i in 0..=10 {
        let t = i as f32 / 10.0;
        let morphed = DistortionParams::lerp(&a, &b, t);
        let (l, r) = kernel.process_stereo(0.3, -0.3, &morphed);
        assert!(l.is_finite() && r.is_finite(), "Morph at t={t} produced NaN/Inf");
        kernel.reset();
    }
}
```

Test **what the effect does**, not **that it matches old code**. The old code is being deleted.

### Step 9: Swap Registry and Delete the Old Effect

Immediately after the kernel passes tests:

1. **Update the registry** — change the factory line in `crates/sonido-registry/src/lib.rs`:
    ```rust
    // Old:
    "distortion" => Box::new(Distortion::new(sample_rate)),
    // New:
    "distortion" => Box::new(KernelAdapter::new(DistortionKernel::new(sample_rate), sample_rate)),
    ```

2. **Delete the old effect** — remove the classic `Distortion` struct and its file (`crates/sonido-effects/src/distortion.rs`). Remove its `pub mod distortion;` and `pub use Distortion;` from `sonido-effects/src/lib.rs`. Remove its import from the registry.

3. **Run `cargo test --workspace`** — fix any breakage from the removal. Common issues:
   - Other tests that imported the old type directly
   - CLI or GUI code that referenced the old type name
   - Fix by importing the kernel type instead: `use sonido_effects::kernels::DistortionKernel;`

4. **Commit** — one clean commit per effect: "Replace Distortion with DistortionKernel"

Nothing is in production. There's no reason to keep dead code.

## Checklist

For each effect migration, verify:

- [ ] `XxxParams` struct defined with all fields in user-facing units
- [ ] `KernelParams` impl with correct `COUNT`, descriptors, smoothing styles
- [ ] `ParamId` values and `string_id`s match the original exactly
- [ ] `XxxKernel` struct contains ONLY DSP state (no `SmoothedParam`)
- [ ] `DspKernel` impl with `process_stereo()` containing the actual DSP math
- [ ] Unit conversions (`dB→linear`, `%→fraction`) happen in the kernel
- [ ] Coefficient caches update only when relevant params change
- [ ] `reset()` clears all DSP state
- [ ] `set_sample_rate()` recalculates coefficients
- [ ] `from_knobs()` constructor for embedded deployment
- [ ] Module registered in `kernels/mod.rs` and `lib.rs`
- [ ] Kernel unit tests (silence, NaN/Inf, basic processing, parameter effects)
- [ ] Behavioral tests (drive increases amplitude, 0% mix passes dry, etc.)
- [ ] Adapter integration tests (Effect interface, ParameterInfo interface)
- [ ] Morph test (lerp between two param states produces finite output)
- [ ] Snapshot roundtrip test (save/load through adapter)
- [ ] Registry updated to construct `KernelAdapter<XxxKernel>`
- [ ] Old effect struct and file deleted
- [ ] Old imports removed from `lib.rs` and registry
- [ ] `cargo test --workspace` passes clean

## File Structure

```
crates/sonido-core/src/kernel/
├── mod.rs          # Module root, re-exports
├── traits.rs       # DspKernel, KernelParams, SmoothingStyle
└── adapter.rs      # KernelAdapter<K> — bridges to Effect + ParameterInfo

crates/sonido-effects/src/kernels/
├── mod.rs          # Module root, re-exports all 19 kernels
├── bitcrusher.rs   # BitcrusherKernel + BitcrusherParams
├── chorus.rs       # ChorusKernel + ChorusParams
├── compressor.rs   # CompressorKernel + CompressorParams
├── delay.rs        # DelayKernel + DelayParams
├── distortion.rs   # DistortionKernel + DistortionParams
├── filter.rs       # FilterKernel + FilterParams
├── flanger.rs      # FlangerKernel + FlangerParams
├── gate.rs         # GateKernel + GateParams
├── limiter.rs      # LimiterKernel + LimiterParams
├── vibrato.rs      # VibratoKernel + VibratoParams
├── eq.rs           # EqKernel + EqParams
├── phaser.rs       # PhaserKernel + PhaserParams
├── preamp.rs       # PreampKernel + PreampParams
├── reverb.rs       # ReverbKernel + ReverbParams
├── ringmod.rs      # RingModKernel + RingModParams
├── stage.rs        # StageKernel + StageParams
├── tape.rs         # TapeKernel + TapeParams
├── tremolo.rs      # TremoloKernel + TremoloParams
└── wah.rs          # WahKernel + WahParams
```

## Migration Status

All 19 effects have been migrated. Classic `Effect` implementations have been deleted.

| Effect | Status | Notes |
|--------|--------|-------|
| Distortion | Complete -- classic deleted | Proof-of-concept, first migrated |
| Tremolo | Complete -- classic deleted | Simple LFO |
| Delay | Complete -- classic deleted | Delay lines |
| Chorus | Complete -- classic deleted | Modulated delay |
| Reverb | Complete -- classic deleted | 8 comb + 4 allpass |
| Compressor | Complete -- classic deleted | Dynamic state management |
| Filter | Complete -- classic deleted | SVF-based |
| Flanger | Complete -- classic deleted | Similar to Chorus |
| Phaser | Complete -- classic deleted | Multi-stage allpass |
| Preamp | Complete -- classic deleted | Simple gain stage |
| Gate | Complete -- classic deleted | Noise gate |
| Eq | Complete -- classic deleted | 3-band EQ |
| Tape | Complete -- classic deleted | ADAA + hysteresis |
| Vibrato | Complete -- classic deleted | Multi-voice pitch |
| Wah | Complete -- classic deleted | SVF bandpass |
| Limiter | Complete -- classic deleted | Lookahead brickwall |
| Bitcrusher | Complete -- classic deleted | Bit/sample reduction |
| Ring Modulator | Complete -- classic deleted | Carrier oscillator |
| Stage | Complete -- classic deleted | Stereo utility |

## FAQ

**Q: What's the migration strategy?**
Replace, don't coexist. For each effect: create kernel → write tests → swap registry → delete old → test workspace. Nothing is in production, so there's no backwards compatibility concern.

**Q: What about `EffectWithParams` and the graph's `Box<dyn EffectWithParams + Send>`?**
`KernelAdapter<K>` implements both `Effect` and `ParameterInfo`, so it automatically satisfies the `EffectWithParams` blanket impl. It drops into the graph with zero changes.

**Q: Can I use `DspKernel` directly in the DAG graph without the adapter?**
Not currently. The graph operates on `dyn EffectWithParams`. You could build a specialized graph that operates on kernels directly, but the adapter overhead is negligible (~5 smoother advances per sample per effect) and the integration simplicity is worth it.

**Q: What about block processing optimizations?**
The `KernelAdapter` calls `process_stereo()` per sample inside its block methods because it advances smoothers per sample. The `DspKernel` trait also has `process_block_stereo()` which takes a single params snapshot for the entire block — this is for embedded use where params don't change mid-block.

**Q: What about the `impl_params!` macro?**
Delete it after all effects are migrated. `KernelParams` replaces it entirely. Eventually, a `#[derive(KernelParams)]` proc macro will generate the `get/set/descriptor/smoothing` match arms from field attributes, eliminating the boilerplate.

**Q: What if something breaks when I delete the old effect?**
Fix forward. The kernel is the replacement. If tests fail after deletion, it means something was referencing the old type directly — update it to use the kernel type or the adapter. Common fixup: `use sonido_effects::Distortion` → `use sonido_effects::kernels::DistortionKernel` (or access through the registry, which is preferred).

**Q: What order should I migrate effects?**
Easy effects first (fewer params, simpler DSP), complex last. See the priority table below and the execution plan in KERNEL_SPEC.md section 11.
