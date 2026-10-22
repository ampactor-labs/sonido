# Kernel Architecture

Reference for Sonido's kernel architecture -- the three-layer separation of DSP math,
parameter ownership, and runtime bridging. All 36 effects use this pattern exclusively;
the earlier per-effect `Effect` implementations have been removed.

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
            |  +---- Hardware mapping (descriptor-driven knob mapping, from_normalized)
            |  +---- Host bridge (CLAP normalized values -> from_normalized)
            |
            +----------+-----------------+
                       v
          +-------------------------------+
          | Adapter<K, Policy>            |  (provided by sonido-core)
          |                               |
          |  impl Effect                  |  <- automatic
          |  impl ParameterInfo           |  <- automatic
          |  impl EffectWithParams        |  <- blanket impl, free
          |  owns SmoothedParams          |  <- SmoothedPolicy only
          |  load_snapshot()              |  <- preset recall
          |  snapshot()                   |  <- preset save
          +-------------------------------+
                       |
       +---------------+------------------+
       v               v                  v
  DAG Graph        CLAP Plugin       Embedded
  (Box<dyn EWP>)   (SonidoShared)    (Adapter<K, DirectPolicy>)
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
LFO/oscillator phase, tempo state (`TempoManager`), sample rate, cached coefficients.

### Parameter identity

`ParamId` values and `string_id` strings in `KernelParams::descriptor()` **MUST NOT**
change once shipped. These are plugin host API contracts. Changing them silently
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

Effects with tempo sync (Delay, Tremolo, Chorus, Flanger, Phaser) receive tempo via
`set_tempo_context()`:

```rust
pub struct DelayKernel {
    tempo: TempoManager,
    // ...
}

impl DspKernel for DelayKernel {
    type Params = DelayParams;

    fn set_tempo_context(&mut self, ctx: &TempoContext) {
        self.tempo.set_bpm(ctx.bpm);
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

Effects with stereo spread use paired LFOs. Tremolo, for example, re-derives the
right LFO's phase from the left one each sample:

```rust
pub struct TremoloKernel {
    lfo_l: Lfo,
    lfo_r: Lfo,
    tempo: TempoManager,
    // ...
}

impl DspKernel for TremoloKernel {
    fn process_stereo(&mut self, l: f32, r: f32, params: &TremoloParams) -> (f32, f32) {
        // 0-100 % spread = 0-0.5 cycle (0-180 deg) offset for R
        let spread_offset = params.stereo_spread_pct / 100.0 * 0.5;
        let r_phase = (self.lfo_l.phase() + spread_offset).fract();
        self.lfo_r.set_phase(r_phase);

        let mod_l = self.lfo_l.advance();
        let mod_r = self.lfo_r.advance();
        // ...
    }
}
```

### from_knobs() for embedded

27 of the 36 params structs have a `from_knobs()` that takes one normalized
0.0--1.0 reading per parameter, in index order, and maps it through the
descriptors (most simply forward to `from_normalized()`):

```rust
impl DelayParams {
    pub fn from_knobs(
        time: f32, feedback: f32, mix: f32, ping_pong: f32, fb_lp: f32,
        fb_hp: f32, diffusion: f32, sync: f32, division: f32, output: f32,
    ) -> Self {
        Self::from_normalized(&[
            time, feedback, mix, ping_pong, fb_lp, fb_hp, diffusion, sync, division, output,
        ])
    }
}
```

(cabinet, deesser, drone, glitch, multiband_comp, pitch_shift, texture,
time_stretch and tuner have none.) The shipping pedal firmware does not use
`from_knobs()`: it wraps kernels in `Adapter<K, DirectPolicy>` and maps knobs with
`sonido_platform::knob_mapping::knob_to_param()` (built on `adc_to_param_biased()` in
`sonido-platform/src/param_map.rs`), which uses `ParamDescriptor` metadata for
scale-aware mapping with a biased center point ("noon = sweet spot"). See ADR-030
and `docs/EMBEDDED.md` for details. `from_knobs()` remains in use by the
`single_delay` / `single_reverb` firmware examples.

### Preset morphing

```rust
let clean = DistortionParams { drive_db: 3.0, mix_pct: 30.0, ..Default::default() };
let heavy = DistortionParams { drive_db: 35.0, mix_pct: 100.0, ..Default::default() };

// Morph 40% of the way from clean to heavy
let morphed = DistortionParams::lerp(&clean, &heavy, 0.4);
// Stepped params (waveshape) snap at t=0.5 -- no fractional enum values;
// logarithmic-scale params (frequencies) interpolate geometrically
```

### Preset save/restore via adapter

```rust
// Save
let saved: DistortionParams = adapter.snapshot();

// Instant recall (snap smoothers)
adapter.load_snapshot(&saved);
adapter.reset();

// Smooth transition to new preset (let smoothers glide; SmoothedPolicy only)
adapter.load_snapshot(&saved);
```

### Normalized bridge (CLAP hosts, MIDI CC)

```rust
// Normalized 0-1 values, one per parameter (distortion has 6)
let params = DistortionParams::from_normalized(&[0.3, 0.5, 0.5, 0.0, 1.0, 0.0]);

// Export back to normalized
let mut normalized = [0.0f32; 6];
params.to_normalized(&mut normalized);
```

---

## Emergent Capabilities

### Scene morphing

Because a `Params` struct is a plain value, two captures plus `lerp()` give continuous
morphing between them. The morph operates in parameter space (not control space): with
`KernelParams::lerp()` or `curve_lerp()` + `MorphCurve::Logarithmic`, a 50% morph between
200 Hz and 2000 Hz yields ~632 Hz (geometric midpoint), not 1100 Hz.

Where this is used today:
- **`ChainMorph`** (`sonido-core/src/kernel/chain_morph.rs`) — per-slot `MorphSpace`
  A/B morph with per-parameter curves and locks; used by `sonido_patch::PatchPlayer`
  (the graph-player plugin).
- **Standalone GUI** (`sonido-gui/src/morph_state.rs`) — its own A/B poses per
  effect, interpolated with `curve_lerp` / `MorphCurve`.
- **Pedal firmware** (`sonido_pedal.rs`) — captures an A and a B parameter set per node
  and ramps between them in Morph mode (FS1/FS2, K5 = morph time). Its
  `interpolate_and_apply` is plain linear interpolation with stepped params snapping at
  t = 0.5 (no geometric frequency morph), and scenes are not yet persisted to QSPI.

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

The morph position is just a `f32`, so any control source can drive it: an expression
pedal, envelope follower, LFO, or MIDI CC. (`sonido-daisy` has an `ExpressionInput`
reader, but the shipping pedal does not wire it to the morph yet.)

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
- **`Default + Clone + Send`** (required by `KernelParams`; the kernels also derive `Copy`)
- **Field order = parameter index order** (convention)

### Step 3: Implement `KernelParams`

The kernels implement it with the `kernel_params!` macro (`sonido-core`), which generates
`COUNT`, `descriptor`, `smoothing`, `get` and `set` from one table:

```rust
sonido_core::kernel_params! {
    MyEffectParams, this {
        [0] ParamDescriptor::gain_db("Drive", "Drive", 0.0, 40.0, 12.0)
                .with_id(ParamId(XXX), "myeffect_drive"),
            smoothing: SmoothingStyle::Fast,
            get: this.drive_db,
            set: |v| this.drive_db = v;
        // [1] mix, [2] output ...
    }
}
```

The equivalent hand-written impl:

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
        let drive = db_to_linear(params.drive_db);
        let mix = params.mix_pct / 100.0;
        let output = db_to_linear(params.output_db);

        // ... DSP math
    }

    fn reset(&mut self) { /* clear all DSP state */ }
    fn set_sample_rate(&mut self, sr: f32) { /* recalculate coefficients */ }
}
```

### Step 6: Add `from_knobs()` (optional)

```rust
impl MyEffectParams {
    pub fn from_knobs(drive: f32, mix: f32, output: f32) -> Self {
        Self::from_normalized(&[drive, mix, output])
    }
}
```

The pedal maps knobs from descriptors instead; to put the effect on the pedal, add it to
`sonido-platform`'s `knob_mapping.rs` and `noon.rs` (see `docs/EMBEDDED.md`).

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
        short_name: "MYFX",
        description: "...",
        category: EffectCategory::Dynamics,
        param_count: 0, // filled in by register() from the adapter
    },
    |sr| Box::new(Adapter::new(MyEffectKernel::new(sr), sr)),
);
```

Also give it a new, never-reused UID in `EFFECT_UIDS` (the patch format stores effects
by UID), update the test assertions for `registry.len()` and category counts, and add a
`PluginSpec` to `crates/sonido-plugin/src/bundle.rs` `CATALOG`
(`crates/sonido-plugin/tests/plugin_inventory.rs` fails until you do).

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
- `docs/ARCHITECTURE.md` -- effect table and count
- `README.md` -- update effect count

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
- [ ] `from_knobs()` constructor (optional; for direct-kernel firmware examples)
- [ ] Module registered in `kernels/mod.rs` and `lib.rs`
- [ ] Registry entry + `EFFECT_UIDS` entry added in `sonido-registry/src/lib.rs`
- [ ] Plugin `CATALOG` entry added in `sonido-plugin/src/bundle.rs`
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
+-- traits.rs       # DspKernel, KernelParams, SmoothingStyle, kernel_params!
+-- adapter.rs      # Adapter<K, P> -- the only Effect implementor for effects
+-- morph.rs        # MorphSpace, MorphCurve, MorphMode, curve_lerp
+-- chain_morph.rs  # ChainMorph -- per-slot A/B morph over a chain

crates/sonido-effects/src/kernels/
+-- mod.rs              # Module root, re-exports all 36 kernels
+-- amp.rs              # AmpKernel + AmpParams
+-- bitcrusher.rs       # BitcrusherKernel + BitcrusherParams
+-- cabinet.rs          # CabinetKernel + CabinetParams
+-- chorus.rs           # ChorusKernel + ChorusParams
+-- compressor.rs       # CompressorKernel + CompressorParams
+-- deesser.rs          # DeesserKernel + DeesserParams
+-- delay.rs            # DelayKernel + DelayParams
+-- distortion.rs       # DistortionKernel + DistortionParams
+-- drone.rs            # DroneKernel + DroneParams
+-- eq.rs               # EqKernel + EqParams
+-- filter.rs           # FilterKernel + FilterParams
+-- flanger.rs          # FlangerKernel + FlangerParams
+-- gate.rs             # GateKernel + GateParams
+-- glitch.rs           # GlitchKernel + GlitchParams
+-- harmonic_habitat.rs # HarmonicHabitatKernel + HarmonicHabitatParams
+-- limiter.rs          # LimiterKernel + LimiterParams
+-- looper.rs           # LooperKernel + LooperParams
+-- multiband_comp.rs   # MultibandCompKernel + MultibandCompParams
+-- phaser.rs           # PhaserKernel + PhaserParams
+-- pitch_shift.rs      # PitchShiftKernel + PitchShiftParams
+-- plate_reverb.rs     # PlateReverbKernel + PlateReverbParams
+-- preamp.rs           # PreampKernel + PreampParams
+-- reverb.rs           # ReverbKernel + ReverbParams
+-- ringmod.rs          # RingModKernel + RingModParams
+-- shelving_eq.rs      # ShelvingEqKernel + ShelvingEqParams
+-- spring_reverb.rs    # SpringReverbKernel + SpringReverbParams
+-- stage.rs            # StageKernel + StageParams
+-- stereo_widener.rs   # StereoWidenerKernel + StereoWidenerParams
+-- tape.rs             # TapeKernel + TapeParams
+-- texture.rs          # TextureKernel + TextureParams
+-- time_stretch.rs     # TimeStretchKernel + TimeStretchParams
+-- transient_shaper.rs # TransientShaperKernel + TransientShaperParams
+-- tremolo.rs          # TremoloKernel + TremoloParams
+-- tuner.rs            # TunerKernel + TunerParams
+-- vibrato.rs          # VibratoKernel + VibratoParams
+-- wah.rs              # WahKernel + WahParams
```

---

## FAQ

**Q: Can I use `DspKernel` directly in the DAG graph without the adapter?**
No. The graph operates on `dyn EffectWithParams`. The adapter overhead is small (one
smoother advance per parameter per sample with `SmoothedPolicy`, none with
`DirectPolicy`) and the integration simplicity is worth it.

**Q: What about block processing optimizations?**
`Adapter<K, P>` calls `process_stereo()` per sample inside its block methods because it
advances smoothers per sample. The `DspKernel` trait also has `process_block_stereo()`,
which takes a single params snapshot for the entire block (its default is a per-sample
loop; no kernel currently overrides it).

**Q: Do I hand-write `get`/`set`/`descriptor`/`smoothing`?**
No -- use the `kernel_params!` macro (Step 3). The `Adapter` uses `KernelParams` to
implement `ParameterInfo` automatically.

**Q: How does the plugin integration work?**
The CLAP plugin gets kernels through the registry. `SonidoAudioProcessor` (`audio.rs`)
calls `registry.create(effect_id, sr)`, which returns `Adapter<K, SmoothedPolicy>`;
`SonidoShared` builds its atomic parameter store from the registry's descriptors. Each
process call copies the atomic values into `Adapter::set_param()`, which sets the
smoother target.

**Q: How does embedded use work?**
The shipping pedal (`sonido_pedal.rs`) wraps each kernel in `Adapter::new_direct()`
(`DirectPolicy`: same `Effect + ParameterInfo` interface, no smoothing) inside a
`ProcessingGraph`. Single-effect examples can also call a kernel directly:
```rust
let mut kernel = DelayKernel::new(SAMPLE_RATE);
let params = DelayParams::from_knobs(k1, k2, k3, /* ... one per param */);
let (l, r) = kernel.process_stereo(input_l, input_r, &params);
```
No `SmoothedParam`, no `Arc`, and no allocation in the audio path (delay buffers are
allocated once in `new()`). Same DSP math as the desktop plugin.

---

## See Also

- [Architecture Overview](ARCHITECTURE.md) -- Crate dependency graph and design overview
- [Design Decisions (ADR-028)](DESIGN_DECISIONS.md#adr-028-kernel-architecture--dspparameter-separation) -- Architectural decision record
- [Effects Reference](EFFECTS_REFERENCE.md) -- All 36 effects with parameters and DSP theory
- [Embedded Guide](EMBEDDED.md) -- Hardware targets and deployment
