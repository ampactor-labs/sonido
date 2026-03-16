# Kernel Architecture

Reference for Sonido's kernel architecture -- the three-layer separation of DSP math,
parameter ownership, and runtime bridging. All 19 effects use this pattern exclusively.
Classic `Effect` implementations have been removed as of v0.2.

---

## Overview

The kernel architecture separates DSP math from parameter ownership:

```
+-----------------------+     +-----------------------+
| XxxParams             |     | XxxKernel             |
|                       |     |                       |
|  field_a: f32         |     |  filter: Biquad       |
|  field_b: f32         |     |  delay: DelayLine     |
|                       |     |  lfo: Lfo             |
|  impl KernelParams    |     |                       |
|   (one definition)    |     |  impl DspKernel       |
+-----------------------+     |   type Params = Xxx   |
            |                 +-----------------------+
            |
            |  The params struct is everything:
            |
            |  +---- Processing input (&Params each sample)
            |  +---- Preset format (clone to save, restore to load)
            |  +---- Morph target (lerp between any two snapshots)
            |  +---- Serialization (indexed get/set, to/from_normalized)
            |  +---- Hardware mapping (from_knobs for ADC, from_normalized for MIDI)
            |  +---- Host bridge (CLAP normalized values -> from_normalized)
            |
            +----------+-----------------+
                       v
          +-------------------------------+
          | Adapter<K, SmoothedPolicy>    |  (provided by sonido-core)
          |                               |
          |  impl Effect                  |  <- automatic
          |  impl ParameterInfo           |  <- automatic
          |  impl EffectWithParams        |  <- blanket impl, free
          |  owns SmoothedParams          |  <- managed by adapter
          |  load_snapshot()              |  <- preset recall
          |  snapshot()                   |  <- preset save
          +-------------------------------+
                       |
       +---------------+------------------+
       v               v                  v
  DAG Graph        CLAP Plugin       Embedded Direct
  (Box<dyn EWP>)   (SonidoShared)    (no adapter)
```

**Benefits:**
- **Kernel is platform-independent** -- identical binary on x86_64 and ARM Cortex-M7
- **One parameter definition** -- no separate `impl_params!` to keep in sync
- **Smoothing at the boundary** -- adapter smooths on desktop, embedded skips it
- **Testable in isolation** -- kernel can be tested with raw `&Params`, no smoothing noise

---

## Invariants

These are the non-negotiable rules. If any are violated, the architecture breaks.

### Kernel purity

A `DspKernel` struct **MUST NOT** contain:
- `SmoothedParam`
- `AtomicU32` / `AtomicF32` / any atomic
- `Arc` / `Mutex` / `RwLock` / any synchronization primitive
- Platform-specific types
- Parameter values (those belong in `Params`)

A kernel struct contains ONLY: filter state, delay buffers, ADAA processors,
LFO/oscillator phase, sample rate, cached coefficients.

### Parameter identity

`ParamId` values and `string_id` strings in `KernelParams::descriptor()` **MUST** exactly
match the original definitions. These are plugin host API contracts. Changing them silently
breaks saved automation, presets, and MIDI mappings.

### Params are user-facing units

`KernelParams` fields store values in the same units as `ParamDescriptor::min/max/default`:
- Gain: decibels (not linear)
- Mix/depth: percent 0--100 (not fraction 0--1)
- Time: milliseconds
- Stepped: integer-valued float (0.0, 1.0, 2.0, ...)

Unit conversion (`dB->linear`, `%->fraction`) happens INSIDE the kernel's
`process_stereo()`, not in the params struct.

### Smoothing is external

The kernel never smooths parameters. It receives instantaneous values. The `Adapter<K, SmoothedPolicy>`
(or embedded hardware filtering, or the host's parameter smoothing) handles transitions.
`KernelParams::smoothing()` is a *preference* that the adapter respects -- not a guarantee.

### One path, no duplication

Every effect is implemented as a `DspKernel`. The `Adapter` is the **only** type that
implements `Effect` for audio effects. If you find yourself writing `impl Effect for MyEffect`
directly, write a kernel instead.

### Adapter is invisible

Code that uses `Box<dyn Effect>`, `Box<dyn EffectWithParams + Send>`, or the registry
**must not know** that the underlying implementation is `Adapter<K, SmoothedPolicy>`. The
adapter implements the full `Effect` + `ParameterInfo` interface.

---

## Patterns

### Coefficient caching

When a parameter change requires expensive coefficient recalculation (biquad, SVF, FIR):

```rust
pub struct MyKernel {
    filter: Biquad,
    last_cutoff: f32,
    last_resonance: f32,
    sample_rate: f32,
}

impl DspKernel for MyKernel {
    fn process_stereo(&mut self, l: f32, r: f32, params: &MyParams) -> (f32, f32) {
        if (params.cutoff - self.last_cutoff).abs() > 0.01
            || (params.resonance - self.last_resonance).abs() > 0.001
        {
            let coeffs = low_pass_coefficients(params.cutoff, params.resonance, self.sample_rate);
            self.filter.set_coefficients(coeffs);
            self.last_cutoff = params.cutoff;
            self.last_resonance = params.resonance;
        }
        // ... process
    }
}
```

Use NaN as the sentinel for cache invalidation in `reset()`.

### Tempo-synced effects

Effects with tempo sync (Delay, Tremolo) receive tempo via `set_tempo_context()`:

```rust
pub struct DelayKernel {
    tempo: TempoManager,
    // ...
}

impl DspKernel for DelayKernel {
    type Params = DelayParams;

    fn set_tempo_context(&mut self, ctx: &TempoContext) {
        self.tempo.update(ctx);
    }

    fn process_stereo(&mut self, l: f32, r: f32, params: &DelayParams) -> (f32, f32) {
        let delay_ms = if params.sync > 0.5 {
            self.tempo.division_to_ms(index_to_division(params.division as usize))
        } else {
            params.time_ms
        };
        // ... use delay_ms
    }
}
```

### Enum parameters

Discrete parameters (waveshape, waveform, filter type) are `f32` in the params struct and
converted to enum/index in the kernel:

```rust
// In params struct:
pub shape: f32,  // 0.0, 1.0, 2.0, 3.0

// In kernel process:
let shape = params.shape as u8;
match shape {
    0 => self.adaa_soft.process(x),
    1 => self.adaa_hard.process(x),
    // ...
}
```

The `ParamDescriptor` uses `STEPPED` flag and `with_step_labels()` for GUI display.

### Stereo LFO effects

Effects with stereo spread (Tremolo, Chorus) that use paired LFOs:

```rust
pub struct TremoloKernel {
    lfo_l: Lfo,
    lfo_r: Lfo,
}

impl DspKernel for TremoloKernel {
    fn process_stereo(&mut self, l: f32, r: f32, params: &TremoloParams) -> (f32, f32) {
        let spread_phase = params.stereo_spread / 100.0 * 0.5; // 0-50% = 0-180 deg
        self.lfo_r.set_phase_offset(spread_phase);

        let mod_l = self.lfo_l.next();
        let mod_r = self.lfo_r.next();
        // ...
    }
}
```

### from_knobs() for embedded

Every params struct has a `from_knobs()` that maps normalized 0.0--1.0 ADC readings:

```rust
impl DelayParams {
    pub fn from_knobs(time: f32, feedback: f32, mix: f32, tone: f32) -> Self {
        Self {
            time_ms: time * 1999.0 + 1.0,        // 1-2000 ms
            feedback_pct: feedback * 95.0,         // 0-95%
            mix_pct: mix * 100.0,                  // 0-100%
            lpf_hz: 200.0 + tone * 19800.0,       // 200-20000 Hz
            hpf_hz: 20.0,                          // fixed for embedded
            sync: 0.0,                             // no sync on hardware
            division: 0.0,
            output_db: 0.0,
        }
    }
}
```

When using `Adapter<K, DirectPolicy>` (the preferred embedded path), parameter mapping
is handled by `adc_to_param_biased()` in `sonido-daisy` instead of `from_knobs()`.
This uses `ParamDescriptor` metadata to provide scale-aware mapping with a biased
center point ("noon = sweet spot"). See ADR-030 and `docs/EMBEDDED.md` for details.

### Preset morphing

```rust
let clean = DistortionParams { drive_db: 3.0, mix_pct: 30.0, ..Default::default() };
let heavy = DistortionParams { drive_db: 35.0, mix_pct: 100.0, ..Default::default() };

// Morph 40% of the way from clean to heavy
let morphed = DistortionParams::lerp(&clean, &heavy, 0.4);
// Stepped params (waveshape) snap at t=0.5 -- no fractional enum values
```

### Preset save/restore via adapter

```rust
// Save
let saved: DistortionParams = adapter.snapshot();

// Instant recall (snap smoothers)
adapter.load_snapshot(&saved);
adapter.reset();

// Smooth transition to new preset (let smoothers glide)
adapter.load_snapshot(&saved);
```

### Normalized bridge (CLAP hosts, MIDI CC)

```rust
// CLAP host sends normalized 0-1 values
let params = DistortionParams::from_normalized(&[0.3, 0.5, 0.5, 0.0, 1.0]);

// Export back to normalized
let mut normalized = [0.0f32; 5];
params.to_normalized(&mut normalized);
```

---

## Emergent Capabilities

### Scene morphing on embedded hardware

Six knobs produce six `f32` values each frame. With `from_knobs()`, those become a typed
`Params` struct that can be cloned. Two captures plus `lerp()` means continuous morphing
between them. On the Hothouse (6 knobs, 3 toggles, 2 footswitches):

1. Dial in first sound with knobs 1--5
2. Press footswitch to capture Scene A
3. Dial in second sound
4. Press footswitch to capture Scene B
5. Knob 6 morphs continuously between Scene A and Scene B

The morph operates in parameter space (not control space), so non-linear parameter
mappings are preserved. A 50% morph between 200 Hz and 2000 Hz yields ~632 Hz
(geometric midpoint), not 1100 Hz (arithmetic midpoint of knob positions).

Scene data is tiny (5 params x 4 bytes = 20 bytes per scene). Scenes persist across
power cycles by writing to the Daisy Seed's QSPI flash (8 MB available).

### Cross-effect morphing

Using the DAG graph with parallel topology and complementary mix parameters:

```rust
fn cross_effect_morph(t: f32) -> (DistortionParams, ReverbParams) {
    let mut dist = distortion_scene;
    let mut verb = reverb_scene;
    dist.mix_pct = (1.0 - t) * 100.0;
    verb.mix_pct = t * 100.0;
    (dist, verb)
}
```

### Expression pedal integration

The morph source is just a `f32` -- it can come from an expression pedal, envelope
follower, LFO, MIDI CC, or any other source. One line of code swaps the morph source.

---

## Adding a New Effect -- Checklist

### Step 1: Create the kernel file

Create `crates/sonido-effects/src/kernels/<effect>.rs`.

### Step 2: Define the params struct

```rust
#[derive(Debug, Clone, Copy)]
pub struct MyEffectParams {
    pub drive_db: f32,
    pub mix_pct: f32,
    pub output_db: f32,
}
```

Rules:
- **User-facing units** -- same as `ParamDescriptor::min/max/default` (dB, %, ms, index)
- **`f32` only** -- no enums, no `SmoothedParam`, no atomics
- **`Clone + Copy + Default + Send`**
- **Field order = parameter index order** (convention)

### Step 3: Implement `KernelParams`

```rust
impl KernelParams for MyEffectParams {
    const COUNT: usize = 3;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(ParamDescriptor::gain_db("Drive", "Drive", 0.0, 40.0, 12.0)
                    .with_id(ParamId(XXX), "myeffect_drive")),
            // ...
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Fast,       // drive
            1 => SmoothingStyle::Standard,   // mix
            2 => SmoothingStyle::Standard,   // output
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 { /* match index -> field */ }
    fn set(&mut self, index: usize, value: f32) { /* match index -> field */ }
}
```

Critical: **`ParamId` values and `string_id`s are frozen** -- they are plugin API contracts.

### Step 4: Define the kernel struct

```rust
pub struct MyEffectKernel {
    sample_rate: f32,
    filter: Biquad,
    // ... ONLY DSP state, NO parameters
    last_drive: f32,  // coefficient cache
}
```

### Step 5: Implement `DspKernel`

```rust
impl DspKernel for MyEffectKernel {
    type Params = MyEffectParams;

    fn process_stereo(&mut self, left: f32, right: f32, params: &MyEffectParams) -> (f32, f32) {
        // Coefficient caching
        if (params.drive_db - self.last_drive).abs() > 0.001 {
            self.update_coefficients(params.drive_db);
        }

        // Unit conversion
        let drive = db_to_gain(params.drive_db);
        let mix = params.mix_pct / 100.0;
        let output = db_to_gain(params.output_db);

        // ... DSP math
    }

    fn reset(&mut self) { /* clear all DSP state */ }
    fn set_sample_rate(&mut self, sr: f32) { /* recalculate coefficients */ }
}
```

### Step 6: Add `from_knobs()`

```rust
impl MyEffectParams {
    pub fn from_knobs(drive: f32, mix: f32, output: f32) -> Self {
        Self {
            drive_db: drive * 40.0,
            mix_pct: mix * 100.0,
            output_db: output * 40.0 - 20.0,
        }
    }
}
```

### Step 7: Register in modules

In `crates/sonido-effects/src/kernels/mod.rs`:
```rust
pub mod my_effect;
pub use my_effect::{MyEffectKernel, MyEffectParams};
```

In `crates/sonido-effects/src/lib.rs`:
```rust
pub use kernels::{MyEffectKernel, MyEffectParams};
```

### Step 8: Register in the effect registry

In `crates/sonido-registry/src/lib.rs`, add to `register_builtin_effects()`:
```rust
self.register(
    EffectDescriptor {
        id: "my_effect",
        name: "My Effect",
        description: "...",
        category: EffectCategory::Dynamics,
        param_count: 3,
    },
    |sr| Box::new(Adapter::new(MyEffectKernel::new(sr), sr)),
);
```

Update test assertions for `registry.len()` and category counts.

### Step 9: Write tests

Three categories:

**Kernel unit tests** -- raw kernel with explicit params:
```rust
#[test]
fn silence_in_silence_out() {
    let mut kernel = MyEffectKernel::new(48000.0);
    let params = MyEffectParams::default();
    let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
    assert!(l.abs() < 1e-6 && r.abs() < 1e-6);
}
```

**Adapter integration tests** -- kernel wrapped in adapter:
```rust
#[test]
fn adapter_wraps_as_effect() {
    let mut adapter = Adapter::new(MyEffectKernel::new(48000.0), 48000.0);
    adapter.reset();
    let output = adapter.process(0.3);
    assert!(output.is_finite());
}
```

**Behavioral tests** -- verify DSP correctness:
```rust
#[test]
fn morph_produces_valid_output() {
    let mut kernel = MyEffectKernel::new(48000.0);
    let a = MyEffectParams::default();
    let b = MyEffectParams { drive_db: 30.0, mix_pct: 100.0, ..Default::default() };
    for i in 0..=10 {
        let t = i as f32 / 10.0;
        let morphed = MyEffectParams::lerp(&a, &b, t);
        let (l, r) = kernel.process_stereo(0.3, -0.3, &morphed);
        assert!(l.is_finite() && r.is_finite());
        kernel.reset();
    }
}
```

### Step 10: Add golden file regression test

Run `REGENERATE_GOLDEN=1 cargo test --test regression -p sonido-effects` to generate
the baseline, then verify the output sounds correct.

### Step 11: Update documentation

- `docs/EFFECTS_REFERENCE.md` -- full effect entry with parameters and DSP theory
- `README.md` -- update effect count
- `CLAUDE.md` -- Key Files table (updated separately)

### Migration checklist

- [ ] `XxxParams` struct defined with all fields in user-facing units
- [ ] `KernelParams` impl with correct `COUNT`, descriptors, smoothing styles
- [ ] `ParamId` values and `string_id`s are correct and frozen
- [ ] `XxxKernel` struct contains ONLY DSP state (no `SmoothedParam`)
- [ ] `DspKernel` impl with `process_stereo()` containing the DSP math
- [ ] Unit conversions (`dB->linear`, `%->fraction`) happen in the kernel
- [ ] Coefficient caches update only when relevant params change
- [ ] `reset()` clears all DSP state (NaN sentinel for cache invalidation)
- [ ] `set_sample_rate()` recalculates coefficients
- [ ] `from_knobs()` constructor for embedded deployment
- [ ] Module registered in `kernels/mod.rs` and `lib.rs`
- [ ] Registry entry added in `sonido-registry/src/lib.rs`
- [ ] Kernel unit tests (silence, NaN/Inf, basic processing)
- [ ] Behavioral tests (parameter effects, 0% mix passes dry, etc.)
- [ ] Adapter integration tests (Effect interface, ParameterInfo interface)
- [ ] Morph test (lerp between two param states produces finite output)
- [ ] Snapshot roundtrip test (save/load through adapter)
- [ ] Golden file regression test
- [ ] `cargo test --workspace` passes clean

---

## File Map

```
crates/sonido-core/src/kernel/
+-- mod.rs          # Module root, doc comments, re-exports
+-- traits.rs       # DspKernel, KernelParams, SmoothingStyle
+-- adapter.rs      # Adapter<K, P> -- the only Effect implementor

crates/sonido-effects/src/kernels/
+-- mod.rs          # Module root, re-exports all 19 kernels
+-- bitcrusher.rs   # BitcrusherKernel + BitcrusherParams
+-- chorus.rs       # ChorusKernel + ChorusParams
+-- compressor.rs   # CompressorKernel + CompressorParams
+-- delay.rs        # DelayKernel + DelayParams
+-- distortion.rs   # DistortionKernel + DistortionParams
+-- eq.rs           # EqKernel + EqParams
+-- filter.rs       # FilterKernel + FilterParams
+-- flanger.rs      # FlangerKernel + FlangerParams
+-- gate.rs         # GateKernel + GateParams
+-- limiter.rs      # LimiterKernel + LimiterParams
+-- phaser.rs       # PhaserKernel + PhaserParams
+-- preamp.rs       # PreampKernel + PreampParams
+-- reverb.rs       # ReverbKernel + ReverbParams
+-- ringmod.rs      # RingModKernel + RingModParams
+-- stage.rs        # StageKernel + StageParams
+-- tape.rs         # TapeKernel + TapeParams
+-- tremolo.rs      # TremoloKernel + TremoloParams
+-- vibrato.rs      # VibratoKernel + VibratoParams
+-- wah.rs          # WahKernel + WahParams
```

---

## FAQ

**Q: Can I use `DspKernel` directly in the DAG graph without the adapter?**
Not currently. The graph operates on `dyn EffectWithParams`. The adapter overhead is
negligible (~5 smoother advances per sample per effect) and the integration simplicity
is worth it.

**Q: What about block processing optimizations?**
`Adapter<K, SmoothedPolicy>` calls `process_stereo()` per sample inside its block methods
because it advances smoothers per sample. The `DspKernel` trait also has
`process_block_stereo()` which takes a single params snapshot for the entire block -- this
is for embedded use where params don't change mid-block.

**Q: What about the `impl_params!` macro?**
`KernelParams` replaces the need for per-effect `impl_params!`. The `Adapter` uses
`KernelParams` to implement `ParameterInfo` automatically. Eventually, a
`#[derive(KernelParams)]` proc macro will eliminate the manual `get/set/descriptor/smoothing`
match arms.

**Q: What if something breaks when I delete the old effect?**
Fix forward. If tests fail after deletion, update references to use the kernel type or the
adapter. Common fixup: `use sonido_effects::Distortion` ->
`use sonido_effects::kernels::DistortionKernel` (or use the registry, which is preferred).

**Q: How does the plugin integration work?**
The CLAP plugin adapter gets kernels automatically through the registry.
`SonidoShared::new()` calls `registry.create(effect_id, sr)` which returns
`Adapter<K, SmoothedPolicy>`. The plugin's atomic parameter array writes to
`Adapter::set_param()`, which sets the smoother target.

**Q: How does embedded use work without the adapter?**
On Daisy Seed / Hothouse, use kernels directly:
```rust
let mut kernel = DistortionKernel::new(SAMPLE_RATE);
let params = DistortionParams::from_knobs(adc.read(0), adc.read(1), ...);
let (l, r) = kernel.process_stereo(input_l, input_r, &params);
```
No `SmoothedParam`. No `Vec`. No `Arc`. No allocation. Same DSP math as the desktop plugin.

---

## See Also

- [Architecture Overview](ARCHITECTURE.md) -- Crate dependency graph and design overview
- [Design Decisions (ADR-028)](DESIGN_DECISIONS.md#adr-028-kernel-architecture--dspparameter-separation) -- Architectural decision record
- [Effects Reference](EFFECTS_REFERENCE.md) -- All 19 effects with parameters and DSP theory
- [Embedded Guide](EMBEDDED.md) -- Hardware targets and deployment
