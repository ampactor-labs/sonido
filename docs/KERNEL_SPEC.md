# Sonido Kernel Architecture — Implementation Specification

**Purpose:** This document is the single source of truth for completing the kernel architecture migration. Hand this to Claude Code (or any implementation agent) alongside the codebase. Everything needed to execute is here.

**Status:** Foundation laid. Distortion proof-of-concept complete. 18 effects to convert. Classic effects will be replaced, not kept alongside — nothing is in production.

---

## 1. What This Is

The kernel architecture separates DSP math from parameter ownership. Three layers:

```
┌──────────────────────┐     ┌──────────────────────┐
│ XxxParams            │     │ XxxKernel             │
│                      │     │                       │
│  field_a: f32        │     │  filter: Biquad       │
│  field_b: f32        │     │  delay: DelayLine     │
│                      │     │  lfo: Lfo             │
│  impl KernelParams   │     │                       │
│   (one definition)   │     │  impl DspKernel       │
└──────────────────────┘     │   type Params = Xxx   │
            │                └──────────────────────┘
            │
            │  The params struct is everything:
            │
            │  ┌──── Processing input (&Params each sample)
            │  ├──── Preset format (clone to save, restore to load)
            │  ├──── Morph target (lerp between any two snapshots)
            │  ├──── Serialization (indexed get/set, to/from_normalized)
            │  ├──── Hardware mapping (from_knobs for ADC, from_normalized for MIDI)
            │  └──── Host bridge (CLAP normalized values → from_normalized)
            │
            └───────────┬───────────────┘
                        ▼
           ┌──────────────────────┐
           │ KernelAdapter<K>     │  (provided by sonido-core)
           │                      │
           │  impl Effect         │  ← automatic
           │  impl ParameterInfo  │  ← automatic
           │  impl EffectWithParams│ ← blanket impl, free
           │  owns SmoothedParams │  ← managed by adapter
           │  load_snapshot()     │  ← preset recall
           │  snapshot()          │  ← preset save
           └──────────────────────┘
                        │
        ┌───────────────┼──────────────────┐
        ▼               ▼                  ▼
   DAG Graph        CLAP Plugin       Embedded Direct
   (Box<dyn EWP>)   (SonidoShared)    (no adapter)
```

---

## 2. What's Already Done

### Files created and wired in:

**sonido-core** (`crates/sonido-core/`):
- `src/kernel/mod.rs` — module root, re-exports
- `src/kernel/traits.rs` — `DspKernel`, `KernelParams`, `SmoothingStyle`
- `src/kernel/adapter.rs` — `KernelAdapter<K>` with full `Effect` + `ParameterInfo` impl and tests
- `src/lib.rs` — `pub mod kernel;` added, re-exports added (`DspKernel`, `KernelAdapter`, `KernelParams`, `SmoothingStyle`)

**sonido-effects** (`crates/sonido-effects/`):
- `src/kernels/mod.rs` — module root with migration status table
- `src/kernels/distortion.rs` — `DistortionKernel` + `DistortionParams` (complete, tested)
- `src/lib.rs` — `pub mod kernels;` and `pub use kernels::{DistortionKernel, DistortionParams};` added

**docs/**:
- `KERNEL_MIGRATION.md` — step-by-step migration guide with checklist
- `DESIGN_DECISIONS.md` — ADR-028 added (context, rationale, alternatives, consequences)

### What these files establish:

1. `KernelAdapter<K>` automatically satisfies `EffectWithParams` via the blanket impl in `effect_with_params.rs`. This means `Box::new(KernelAdapter::new(kernel, sr))` produces `Box<dyn EffectWithParams + Send>` — it drops into the registry, DAG graph, and CLAP plugin with zero changes to those systems.

2. `KernelAdapter` provides `load_snapshot()` and `snapshot()` for preset save/restore. Load + reset = instant recall. Load without reset = smooth transition.

3. `KernelParams` provides `lerp()` for preset morphing, `from_normalized()` / `to_normalized()` for CLAP host and MIDI CC bridging, and `from_defaults()` for initialization. The params struct IS the preset, the morph target, and the host bridge — simultaneously.

4. `SmoothingStyle` includes five standard tiers plus `Custom(f32)` for when an effect needs a specific time constant between tiers.

5. `DspKernel` includes block processing methods (`process_block`, `process_block_stereo`) for embedded use where params are stable across a block. The adapter calls per-sample methods (advancing smoothers each sample).

6. The distortion kernel is the proof-of-concept migration: same ADAA algorithms, same signal flow, same `ParamId` values. Once all effects are migrated, the classic `Distortion` struct and all other classic effect structs will be deleted.

7. All code is `no_std` compatible (with `alloc`). The kernel traits impose no `std` requirement.

---

## 3. Compilation Verification

**First thing to do:** Verify the foundation compiles.

```bash
cargo check -p sonido-core
cargo check -p sonido-effects
cargo test -p sonido-core -- kernel
cargo test -p sonido-effects -- kernels
```

If there are compilation errors, they will likely be:
- **Unused import `soft_limit` in adapter.rs** — remove it if present (should already be clean)
- **Missing `alloc` extern in kernel module** — add `#[cfg(not(feature = "std"))] extern crate alloc;` if needed
- **`TempoContext` path** — verify `crate::tempo::TempoContext` resolves correctly

Fix any issues before proceeding. The foundation must be green.

---

## 4. Remaining Work — Prioritized

### Phase 1: Complete the foundation (do first)

#### 4.1 Add `process_block_stereo` to `DspKernel` trait

The trait currently has only per-sample methods. Add block processing for effects that benefit from it:

```rust
// In crates/sonido-core/src/kernel/traits.rs, add to DspKernel:

    /// Process a block of stereo samples.
    ///
    /// Default: calls `process_stereo()` per sample. Override for
    /// effects that benefit from block-level optimizations (vectorization,
    /// per-block coefficient updates, etc.).
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
```

**Note:** The adapter's `process_block_stereo` currently calls `process_stereo()` per sample with per-sample smoother advancement. This is correct — the adapter controls smoothing granularity, not the kernel. The kernel's block method receives a single `&Params` snapshot for the entire block (suitable for embedded where params don't change mid-block). The adapter calls per-sample because it advances smoothers per sample.

#### 4.2 Add `process_block` (mono) to `DspKernel` trait

```rust
    /// Process a block of mono samples.
    fn process_block(
        &mut self,
        input: &[f32],
        output: &mut [f32],
        params: &Self::Params,
    ) {
        debug_assert_eq!(input.len(), output.len());
        for i in 0..input.len() {
            output[i] = self.process(input[i], params);
        }
    }
```

### Phase 2: Migrate effects (core effort)

Each effect follows the same pattern documented in `KERNEL_MIGRATION.md`. Here's the complete list with effect-specific notes:

#### Easy migrations (1-2 params, simple DSP):

| Effect | File | Params | Notes |
|--------|------|--------|-------|
| `CleanPreamp` | `clean_preamp.rs` | gain, output | Nearly trivial — just gain staging + soft limit |
| `LowPassFilter` | `filter.rs` | cutoff, resonance, output | Biquad coefficient recalc on cutoff change |
| `Gate` | `gate.rs` | threshold, attack, release, hold, output | Envelope follower is DSP state |
| `Bitcrusher` | `bitcrusher.rs` | bits, rate, jitter, mix, output | Sample-and-hold state |

#### Medium migrations (3-5 params, modulation or delay):

| Effect | File | Params | Notes |
|--------|------|--------|-------|
| `Tremolo` | `tremolo.rs` | rate, depth, waveform, spread, sync, division, output | LFO is DSP state. Tempo sync: `TempoContext` via `set_tempo_context()` |
| `Chorus` | `chorus.rs` | rate, depth, mix, voices, output | Modulated delay lines are DSP state |
| `Flanger` | `flanger.rs` | rate, depth, feedback, mix, output | Similar to chorus |
| `Phaser` | `phaser.rs` | rate, depth, stages, feedback, mix, output | Allpass chain is DSP state |
| `RingMod` | `ringmod.rs` | freq, depth, waveform, mix, output | Carrier oscillator is DSP state |
| `Delay` | `delay.rs` | time, feedback, mix, lpf, hpf, sync, division, output | Delay buffer is DSP state. `SmoothingStyle::Interpolated` for time param |
| `Compressor` | `compressor.rs` | threshold, ratio, attack, release, makeup, output | Envelope state + gain reduction |
| `Limiter` | `limiter.rs` | ceiling, release, output | Lookahead buffer is DSP state |
| `Tape` | `tape.rs` | drive, warmth, hf_rolloff, mix, output | Asymmetric saturation + filter |

#### Complex migrations (many params, complex DSP state):

| Effect | File | Params | Notes |
|--------|------|--------|-------|
| `Stage` | `stage.rs` | Multiple preamp/tone/cab params | Multi-stage amp sim — most params |
| `Reverb` | `reverb.rs` | predelay, decay, size, damping, mix, width, output | Comb bank + allpass chain is substantial DSP state. Coefficient caching critical |
| `Eq` | `eq.rs` | Per-band freq/gain/Q + global output | Multiple biquads, coefficient recalc per band |
| `Vibrato` | `vibrato.rs` | rate, depth, output | 10-unit modulation bank |

#### For EACH migration, follow this exact sequence:

1. **Create** `crates/sonido-effects/src/kernels/<effect>.rs`
2. **Define** `XxxParams` struct — fields in user-facing units, `f32` only
3. **Implement** `Default` for `XxxParams` — values from descriptor defaults
4. **Implement** `KernelParams` for `XxxParams`:
   - `COUNT` = exact param count (must match original `impl_params!`)
   - `descriptor()` = copy descriptors from original `impl_params!` block
   - **CRITICAL: `ParamId` values and `string_id`s must match exactly** — these are plugin API contracts
   - `smoothing()` = assign based on parameter character (see `SmoothingStyle` docs)
   - `get()`/`set()` = simple match on index → field
5. **Add** `from_knobs()` to `XxxParams` — maps normalized 0–1 inputs to param ranges
6. **Define** `XxxKernel` struct — ONLY DSP state fields. Copy from original struct, remove all `SmoothedParam` fields
7. **Implement** `XxxKernel::new(sample_rate)` — initialize DSP state
8. **Implement** `DspKernel for XxxKernel`:
   - Copy DSP math from original `Effect::process_stereo()`
   - Replace `self.xxx.advance()` → `params.xxx`
   - Add unit conversions at top of process (`db_to_gain`, `pct / 100.0`, etc.)
   - Add coefficient caching where appropriate (`if (params.xxx - self.last_xxx).abs() > threshold`)
   - `reset()` — clear all DSP state
   - `set_sample_rate()` — recalculate coefficients
   - `is_true_stereo()` — return true if cross-channel processing
   - `latency_samples()` — return lookahead if any
   - `set_tempo_context()` — forward to TempoManager if tempo-synced
9. **Register** in `kernels/mod.rs` — add `pub mod xxx;` and `pub use`
10. **Register** in `sonido-effects/src/lib.rs` — add to `pub use kernels::{...}`
11. **Write tests**:
    - Kernel unit tests (silence, basic processing, param effects)
    - Adapter integration tests (Effect interface, ParameterInfo)
12. **Update registry** — change the factory line for this effect immediately:
    ```rust
    "distortion" => Box::new(KernelAdapter::new(DistortionKernel::new(sample_rate), sample_rate)),
    ```
13. **Delete the classic effect struct** — remove the old `Distortion` struct, its `Effect` impl, its `ParameterInfo` impl, and its `impl_params!` block. Remove the `pub use` from `sonido-effects/src/lib.rs`. Nothing is in production — there is no reason to keep dead code.
14. **Run full test suite** — `cargo test --workspace`. Fix any breakage from the removal.

Do this for every effect. One at a time. Migrate, swap registry, delete classic, test. Repeat.

### Phase 3: Registry cleanup (after all effects migrated)

Once all 19 effects are kernels, the registry should construct ONLY `KernelAdapter<XxxKernel>` instances. At this point:

1. **Remove `impl_params!` macro entirely** — no consumers remain
2. **Remove classic effect re-exports** from `sonido-effects/src/lib.rs` — only kernel types exported
3. **Update `sonido-effects/src/lib.rs`** — the `kernels` module becomes the primary module. Consider flattening: rename `src/kernels/` to just be the effect files directly, or keep the `kernels/` namespace if it reads better
4. **Clean up `sonido-registry/src/lib.rs`** — remove old imports of classic effect types, import only kernel types

The registry import section changes from:
```rust
use sonido_effects::{
    Bitcrusher, Chorus, CleanPreamp, Compressor, Delay, Distortion, ...
};
```
to:
```rust
use sonido_effects::kernels::{
    BitcrusherKernel, ChorusKernel, CleanPreampKernel, CompressorKernel, ...
};
use sonido_core::KernelAdapter;
```

### Phase 4: Plugin integration (automatic)

The CLAP plugin adapter (`sonido-plugin`) doesn't need structural changes. `SonidoShared::new()` already goes through the registry:

```rust
let effect = registry.create(effect_id, 48000.0).expect("...");
```

The registry now returns `KernelAdapter<K>`. The plugin gets it automatically. The `SonidoShared` atomic array still works — it writes to `KernelAdapter::set_param()`, which sets the smoother target. The adapter's smoother replaces what was the effect's internal `SmoothedParam`.

**Key improvement:** With the old system, there were THREE copies of each param value (atomic → SmoothedParam target → SmoothedParam current). With kernels, there are TWO (atomic → SmoothedParam in adapter). One layer eliminated.

### Phase 5: Embedded integration

On Daisy Seed / Hothouse, use kernels directly without the adapter:

```rust
#![no_std]
#![no_main]

use sonido_effects::kernels::distortion::{DistortionKernel, DistortionParams};

static mut KERNEL: Option<DistortionKernel> = None;

#[entry]
fn main() -> ! {
    // ... hardware init ...
    unsafe { KERNEL = Some(DistortionKernel::new(SAMPLE_RATE)); }
    // ... start audio ...
}

fn audio_callback(buffer: &mut AudioBuffer) {
    let kernel = unsafe { KERNEL.as_mut().unwrap() };
    let params = DistortionParams::from_knobs(
        adc.read(0), adc.read(1), adc.read(2), adc.read(3), adc.read(4),
    );
    for frame in buffer.frames_mut() {
        let (l, r) = kernel.process_stereo(frame.left, frame.right, &params);
        frame.left = l;
        frame.right = r;
    }
}
```

No `SmoothedParam`. No `Vec`. No `Arc`. No allocation. The kernel processes audio with the exact same DSP math as the desktop plugin, but without any of the platform infrastructure.

### Phase 6: Future — `#[derive(KernelParams)]` proc macro

The manual `KernelParams` impl has repetitive `get/set/descriptor/smoothing` match arms. A proc macro would generate them from field attributes:

```rust
#[derive(KernelParams)]
pub struct DistortionParams {
    #[param(name = "Drive", short = "Drive", range = "0.0..40.0", default = 12.0,
            unit = "dB", id = 200, string_id = "dist_drive", smoothing = "fast")]
    pub drive_db: f32,
    
    #[param(name = "Tone", short = "Tone", range = "-12.0..12.0", default = 0.0,
            unit = "dB", step = 0.5, id = 201, string_id = "dist_tone", smoothing = "slow")]
    pub tone_db: f32,
    // ...
}
```

**Do not build this yet.** The manual impl is intentionally verbose for now — it keeps the architecture visible and auditable while the pattern stabilizes. Build the proc macro after 5+ effects are migrated and the `KernelParams` trait has proven stable.

---

## 5. Emergent Capabilities

These features fall out of the architecture naturally — they aren't bolted on, they're consequences of the params struct being a plain typed value.

### 5.1 Preset morphing

```rust
let clean = DistortionParams { drive_db: 3.0, mix_pct: 30.0, ..Default::default() };
let heavy = DistortionParams { drive_db: 35.0, mix_pct: 100.0, ..Default::default() };

// Morph 40% of the way from clean to heavy
let morphed = DistortionParams::lerp(&clean, &heavy, 0.4);
// morphed.drive_db ≈ 15.8, morphed.mix_pct ≈ 58.0

// Stepped params (waveshape) snap at t=0.5 — no fractional enum values
```

One function. Works for any effect. Preset morphing that takes weeks in JUCE falls out for free.

### 5.2 Preset save/restore via adapter

```rust
// Save
let saved: DistortionParams = adapter.snapshot();

// Instant recall (snap smoothers)
adapter.load_snapshot(&saved);
adapter.reset();

// Smooth transition to new preset (let smoothers glide)
adapter.load_snapshot(&saved);
```

### 5.3 Normalized bridge (CLAP hosts, MIDI CC)

```rust
// CLAP host sends normalized 0–1 values
let params = DistortionParams::from_normalized(&[0.3, 0.5, 0.5, 0.0, 1.0]);
// Uses each param's ParamDescriptor::denormalize() — respects log/power curves

// Export back to normalized (for state save, host reporting)
let mut normalized = [0.0f32; 5];
params.to_normalized(&mut normalized);
```

### 5.4 Custom smoothing times

```rust
fn smoothing(index: usize) -> SmoothingStyle {
    match index {
        0 => SmoothingStyle::Fast,           // 5 ms — drive
        1 => SmoothingStyle::Custom(15.0),   // 15 ms — exactly what this filter needs
        2 => SmoothingStyle::Interpolated,   // 50 ms — delay time
        _ => SmoothingStyle::Standard,
    }
}
```

---

## 6. Invariants — What Must Always Be True

These are the non-negotiable rules. If any of these are violated, the architecture breaks.

### 6.1 Kernel purity
A `DspKernel` struct **MUST NOT** contain:
- `SmoothedParam`
- `AtomicU32` / `AtomicF32` / any atomic
- `Arc` / `Mutex` / `RwLock` / any synchronization primitive
- Platform-specific types
- Parameter values (those belong in `Params`)

A kernel struct contains ONLY: filter state, delay buffers, ADAA processors, LFO/oscillator phase, sample rate, cached coefficients.

### 6.2 Parameter identity
`ParamId` values and `string_id` strings in `KernelParams::descriptor()` **MUST** exactly match the original `impl_params!` definitions. These are plugin host API contracts. Changing them silently breaks saved automation, presets, and MIDI mappings.

### 6.3 Params are user-facing units
`KernelParams` fields store values in the same units as `ParamDescriptor::min/max/default`:
- Gain → decibels (not linear)
- Mix/depth → percent 0–100 (not fraction 0–1)
- Time → milliseconds
- Stepped → integer-valued float (0.0, 1.0, 2.0, ...)

Unit conversion (`dB→linear`, `%→fraction`) happens INSIDE the kernel's `process_stereo()`, not in the params struct.

### 6.4 Smoothing is external
The kernel never smooths parameters. It receives instantaneous values. The `KernelAdapter` (or embedded hardware filtering, or the host's parameter smoothing) handles transitions. `KernelParams::smoothing()` is a *preference* that the adapter respects — not a guarantee.

### 6.5 One path, no duplication
Every effect is implemented as a `DspKernel`. The `KernelAdapter` is the **only** type that implements `Effect` for audio effects. No classic effect structs remain after migration. If you find yourself writing `impl Effect for MyEffect` directly (not through the adapter), you're doing it wrong — write a kernel instead.

### 6.6 Adapter is invisible
Code that uses `Box<dyn Effect>`, `Box<dyn EffectWithParams + Send>`, or the registry **must not know** that the underlying implementation is a kernel adapter. The adapter implements the full `Effect` + `ParameterInfo` interface. To consumers, it's just an effect.

### 6.7 Clean deletion
When migrating an effect, delete the classic struct immediately after the kernel passes tests and the registry is swapped. Do not accumulate dead code. The migration sequence for each effect is: create kernel → swap registry → delete classic → test. No "keep both around just in case."

---

## 7. Patterns Reference

### Pattern: Coefficient caching

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
        // Recalculate only when params actually change
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

### Pattern: Tempo-synced effects

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

### Pattern: Enum parameters

Discrete parameters (waveshape, waveform, filter type) are `f32` in the params struct and converted to enum/index in the kernel:

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

### Pattern: Stereo LFO effects

Effects with stereo spread (Tremolo, Chorus) that use paired LFOs:

```rust
pub struct TremoloKernel {
    lfo_l: Lfo,
    lfo_r: Lfo,
    // ...
}

impl DspKernel for TremoloKernel {
    fn process_stereo(&mut self, l: f32, r: f32, params: &TremoloParams) -> (f32, f32) {
        // Update LFO phase offset for stereo spread
        let spread_phase = params.stereo_spread / 100.0 * 0.5; // 0–50% = 0–180°
        self.lfo_r.set_phase_offset(spread_phase);

        let mod_l = self.lfo_l.next();
        let mod_r = self.lfo_r.next();
        // ...
    }
}
```

### Pattern: from_knobs() for embedded

Every params struct should have a `from_knobs()` that maps normalized 0.0–1.0 ADC readings:

```rust
impl DelayParams {
    pub fn from_knobs(time: f32, feedback: f32, mix: f32, tone: f32) -> Self {
        Self {
            time_ms: time * 1999.0 + 1.0,        // 1–2000 ms
            feedback_pct: feedback * 95.0,         // 0–95%
            mix_pct: mix * 100.0,                  // 0–100%
            lpf_hz: 200.0 + tone * 19800.0,       // 200–20000 Hz (log would be better)
            hpf_hz: 20.0,                          // fixed for embedded
            sync: 0.0,                             // no sync on hardware
            division: 0.0,
            output_db: 0.0,
        }
    }
}
```

---

## 8. File Map

After migration is complete, the effects crate looks like this:

```
crates/sonido-core/src/kernel/
├── mod.rs          # Module root, doc comments, re-exports
├── traits.rs       # DspKernel, KernelParams, SmoothingStyle
└── adapter.rs      # KernelAdapter<K> — the only Effect implementor

crates/sonido-effects/src/kernels/
├── mod.rs          # Module root, all effect exports
├── distortion.rs   # ✅ Done — proof-of-concept
├── tremolo.rs      # 🔲 Start here (simplest modulation)
├── clean_preamp.rs # 🔲 Easy
├── filter.rs       # 🔲 Easy
├── gate.rs         # 🔲 Easy
├── bitcrusher.rs   # 🔲 Easy
├── chorus.rs       # 🔲 Medium
├── flanger.rs      # 🔲 Medium
├── phaser.rs       # 🔲 Medium
├── ringmod.rs      # 🔲 Medium
├── delay.rs        # 🔲 Medium
├── compressor.rs   # 🔲 Medium
├── limiter.rs      # 🔲 Medium
├── tape.rs         # 🔲 Medium
├── reverb.rs       # 🔲 Complex
├── eq.rs           # 🔲 Complex
├── stage.rs        # 🔲 Complex
└── vibrato.rs      # 🔲 Complex

Files to DELETE after all migrations complete:
├── crates/sonido-effects/src/distortion.rs       # replaced by kernels/distortion.rs
├── crates/sonido-effects/src/tremolo.rs           # replaced by kernels/tremolo.rs
├── ... (all 19 classic effect files)
├── crates/sonido-effects/src/macros.rs            # impl_params! — no consumers remain
└── any classic effect re-exports from lib.rs

docs/
├── KERNEL_MIGRATION.md     # Step-by-step how-to
├── KERNEL_SPEC.md          # This document
└── DESIGN_DECISIONS.md     # ADR-028
```

---

## 9. Testing Strategy

### Unit test template for each kernel:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use sonido_core::kernel::KernelAdapter;
    use sonido_core::{Effect, ParameterInfo};

    #[test]
    fn silence_in_silence_out() {
        let mut kernel = XxxKernel::new(48000.0);
        let params = XxxParams::default();
        let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
        assert!(l.abs() < 1e-6 && r.abs() < 1e-6);
    }

    #[test]
    fn no_nan_or_inf() {
        let mut kernel = XxxKernel::new(48000.0);
        let params = XxxParams::default();
        for _ in 0..1000 {
            let (l, r) = kernel.process_stereo(0.5, -0.3, &params);
            assert!(l.is_finite() && r.is_finite());
        }
    }

    #[test]
    fn params_descriptor_count_matches() {
        assert_eq!(XxxParams::COUNT, /* expected count */);
        for i in 0..XxxParams::COUNT {
            assert!(XxxParams::descriptor(i).is_some(), "Missing descriptor at index {i}");
        }
        assert!(XxxParams::descriptor(XxxParams::COUNT).is_none());
    }

    #[test]
    fn params_get_set_roundtrip() {
        let mut params = XxxParams::default();
        for i in 0..XxxParams::COUNT {
            let original = params.get(i);
            params.set(i, original + 1.0);
            let new_val = params.get(i);
            assert!((new_val - (original + 1.0)).abs() < 0.01);
        }
    }

    #[test]
    fn adapter_wraps_as_effect() {
        let mut adapter = KernelAdapter::new(XxxKernel::new(48000.0), 48000.0);
        adapter.reset();
        let output = adapter.process(0.3);
        assert!(output.is_finite());
    }

    #[test]
    fn adapter_param_info_matches() {
        let adapter = KernelAdapter::new(XxxKernel::new(48000.0), 48000.0);
        assert_eq!(adapter.param_count(), XxxParams::COUNT);
        // Verify ParamIds match the original effect
        // ...
    }
}
```

### Behavioral tests (verify DSP correctness without reference to classic effects):

Since classic effects are deleted after migration, we can't compare against them. Instead, test DSP behavior directly:

```rust
#[test]
fn drive_increases_amplitude() {
    let mut kernel = XxxKernel::new(48000.0);
    let low_drive = XxxParams { drive_db: 6.0, ..Default::default() };
    let high_drive = XxxParams { drive_db: 30.0, ..Default::default() };

    let (low_l, _) = kernel.process_stereo(0.2, 0.2, &low_drive);
    kernel.reset();
    let (high_l, _) = kernel.process_stereo(0.2, 0.2, &high_drive);

    assert!(high_l.abs() > low_l.abs(), "Higher drive should produce more amplitude");
}

#[test]
fn dry_mix_passes_input() {
    let mut kernel = XxxKernel::new(48000.0);
    let dry = XxxParams { mix_pct: 0.0, ..Default::default() };

    let input = 0.4;
    let (l, _) = kernel.process_stereo(input, input, &dry);
    assert!((l - input).abs() < 0.01, "0% mix should pass dry signal");
}

#[test]
fn morph_produces_valid_output() {
    let mut kernel = XxxKernel::new(48000.0);
    let a = XxxParams { ..Default::default() };
    let b = XxxParams { drive_db: 30.0, mix_pct: 100.0, ..Default::default() };

    for i in 0..=10 {
        let t = i as f32 / 10.0;
        let morphed = XxxParams::lerp(&a, &b, t);
        let (l, r) = kernel.process_stereo(0.3, -0.3, &morphed);
        assert!(l.is_finite() && r.is_finite(), "Morph at t={t} produced NaN/Inf");
        kernel.reset();
    }
}

#[test]
fn snapshot_roundtrip_through_adapter() {
    let mut adapter = KernelAdapter::new(XxxKernel::new(48000.0), 48000.0);
    adapter.set_param(0, 20.0);
    let saved = adapter.snapshot();
    
    let mut adapter2 = KernelAdapter::new(XxxKernel::new(48000.0), 48000.0);
    adapter2.load_snapshot(&saved);
    assert!((adapter2.get_param(0) - 20.0).abs() < 0.01);
}
```

Key principle: test **what the effect does**, not **that it matches old code**. Does drive increase loudness? Does 0% mix pass dry? Does the filter attenuate above cutoff? Does the delay produce echoes? These tests survive code deletion because they test physics, not implementation.

---

## 10. Potential Compilation Issues

Things to watch for when building:

1. **`alloc` gating**: The adapter uses `Vec` for smoothers. Under `no_std`, ensure `extern crate alloc;` is present and `Vec` is imported from `alloc::vec::Vec`.

2. **`Send` bound**: `DspKernel: Send` is required. If a kernel contains types that aren't `Send` (rare for DSP state — `Biquad`, `DelayLine`, `Lfo` are all `Send`), you'll get a compile error. Fix by ensuring all DSP primitives in sonido-core implement `Send`.

3. **`Clone` on `KernelParams`**: Required by the trait. The params snapshot in the adapter is cloned/rebuilt each sample. For `Copy` types (all params should be `Copy` — they're just `f32` fields), this is free.

4. **`fast_db_to_linear` path**: In the distortion kernel, this is called as `sonido_core::fast_db_to_linear()`. Verify it's re-exported from `sonido_core::fast_math`.

5. **ADAA function pointer types**: The `Adaa1<fn(f32) -> f32, fn(f32) -> f32>` type alias requires `fn` pointer types, not closures. The existing distortion uses this pattern correctly.

---

## 11. Summary & Execution Strategy

The kernel architecture replaces the classic effect system entirely. Nothing is in production. There's no reason to maintain two paths.

**Architecture:**
- `DspKernel` + `KernelParams` = pure DSP + typed parameters
- `KernelAdapter<K>` = the ONLY `Effect` implementor
- `Effect` trait stays as the runtime dispatch interface (object-safe)
- Classic effect structs are deleted after each migration

**The params struct is simultaneously:** a processing input, a preset format, a morph target, a serialization source, a hardware mapping, and a host bridge. One struct. No translation layers. Preset morphing, normalized parameter bridges, scene capture, instant vs. smooth recall — all free.

**For Claude Code — overnight execution plan:**

1. Read this spec top to bottom
2. Read `KERNEL_MIGRATION.md` for the step-by-step checklist
3. Run `cargo check -p sonido-core && cargo test -p sonido-core -- kernel` to verify foundation compiles
4. Migrate effects one at a time in this order:
   - Easy first: `Tremolo`, `CleanPreamp`, `LowPassFilter`, `Gate`, `Bitcrusher`
   - Then medium: `Chorus`, `Flanger`, `Phaser`, `RingMod`, `Delay`, `Compressor`, `Limiter`, `Tape`
   - Then complex: `Reverb`, `Eq`, `Stage`, `Vibrato`
5. For each effect: create kernel → write tests → swap registry → delete classic → `cargo test --workspace`
6. After all 19 are done: clean up `impl_params!` macro, remove dead imports, run full test suite
7. Verify `cargo test --workspace` passes clean with zero classic effects remaining

---

## 12. Future Capabilities — Scene Morphing on the Hothouse

This section documents what the kernel architecture enables beyond basic effect processing. None of this requires new architecture — it's composition of existing pieces on the Hothouse hardware (6 knobs, 3 three-way toggles, 2 footswitches, 2 LEDs, stereo in/out).

### 12.1 The Core Insight

The six knobs produce six `f32` values each frame. With `from_knobs()`, those become a typed `Params` struct. That struct can be cloned. Cloning it means capturing the complete state of the effect at that instant. Two captures plus `lerp()` means continuous morphing between them. The hardware already has everything needed.

### 12.2 Three-Mode Pedal Architecture

Toggle 3 selects the operating mode. Each mode redefines what the controls mean.

**Mode 1: DIRECT (Toggle 3 = UP)**

Normal pedal. Knobs are parameters. Nothing unusual.

```
Knob 1–5:  Effect parameters (via from_knobs)
Knob 6:    Output level
Toggle 1:  Effect select (3 effects: e.g., Distortion / Delay / Reverb)
Toggle 2:  Algorithm variant (3 per effect: e.g., SoftClip / HardClip / Foldback)
Foot 1:    Bypass (latching)
Foot 2:    Tap tempo (for time-based effects)
LED 1:     Bypass indicator (on = active)
LED 2:     Tempo blink
```

This gives you 9 distinct algorithms (3 × 3) with 5 continuous parameters each. Already more than most single pedals.

**Mode 2: MORPH (Toggle 3 = MIDDLE)**

This is the one that doesn't exist anywhere in production hardware.

```
Knob 1–5:  Effect parameters (same as Direct)
Knob 6:    MORPH POSITION (t = 0.0 → Scene A, t = 1.0 → Scene B)
Toggle 1:  Effect select (same)
Toggle 2:  Algorithm variant (same)
Foot 1:    Bypass (same)
Foot 2:    CAPTURE — press to save current knob 1–5 state as a scene
LED 1:     Scene A captured (solid = yes)
LED 2:     Scene B captured (solid = yes)
```

Workflow:
1. Flip Toggle 3 to MIDDLE (morph mode)
2. Dial in your first sound with knobs 1–5
3. Press Foot 2 → Scene A captured (LED 1 lights up)
4. Dial in your second sound with knobs 1–5
5. Press Foot 2 → Scene B captured (LED 2 lights up)
6. Now Knob 6 morphs continuously between Scene A and Scene B

That's it. No menu. No screen. No software. Capture, capture, morph.

The implementation:

```rust
// State machine
enum CaptureState { Empty, SceneA, Ready }
static mut CAPTURE: CaptureState = CaptureState::Empty;
static mut SCENE_A: Option<DistortionParams> = None;
static mut SCENE_B: Option<DistortionParams> = None;

// Footswitch 2 handler (debounced)
fn on_capture_press(knobs: &[f32; 6]) {
    let params = DistortionParams::from_knobs(knobs[0], knobs[1], knobs[2], knobs[3], knobs[4]);
    match CAPTURE {
        CaptureState::Empty => {
            SCENE_A = Some(params);
            CAPTURE = CaptureState::SceneA;
            led_1.set(true);   // Scene A indicator
        }
        CaptureState::SceneA => {
            SCENE_B = Some(params);
            CAPTURE = CaptureState::Ready;
            led_2.set(true);   // Scene B indicator
        }
        CaptureState::Ready => {
            // Re-capture: cycle back to A
            SCENE_A = Some(params);
            SCENE_B = None;
            CAPTURE = CaptureState::SceneA;
            led_1.set(true);
            led_2.set(false);
        }
    }
}

// Audio callback in morph mode
fn audio_callback_morph(kernel: &mut DistortionKernel, buffer: &mut AudioBuffer) {
    let morph_t = adc.read(KNOB_6);  // 0.0–1.0

    let params = match (&SCENE_A, &SCENE_B) {
        (Some(a), Some(b)) => DistortionParams::lerp(a, b, morph_t),
        (Some(a), None)    => *a,               // Only A captured, use it directly
        _                  => DistortionParams::from_knobs(/* live knobs */),
    };

    for frame in buffer.frames_mut() {
        let (l, r) = kernel.process_stereo(frame.left, frame.right, &params);
        frame.left = l;
        frame.right = r;
    }
}
```

Key detail: capture stores the *typed params struct*, not raw knob positions. The `lerp` operates in parameter space, not control space. This matters for parameters with non-linear mappings — a 50% morph between 200 Hz and 2000 Hz should be ~632 Hz (perceptually halfway), not 1100 Hz (arithmetic midpoint of the knob positions). Because `from_knobs()` applies the mapping at capture time and `lerp` operates on the mapped values, the morph is musically correct.

**Mode 3: SCENE CHAIN (Toggle 3 = DOWN)**

Multi-effect scene morphing. The graph runs a chain. One knob morphs everything.

```
Knob 1–2:  Key params for Effect A (e.g., Drive + Tone)
Knob 3–4:  Key params for Effect B (e.g., Time + Feedback)
Knob 5:    Mix balance between effects (parallel) or global param
Knob 6:    SCENE MORPH (same as Mode 2, but across the whole chain)
Toggle 1:  Effect A select
Toggle 2:  Effect B select
Foot 1:    Bypass
Foot 2:    CAPTURE (same protocol — press for A, press for B)
LED 1/2:   Scene indicators
```

The scene snapshot is now a compound struct:

```rust
struct ChainScene {
    effect_a_params: [f32; 2],  // knobs 1–2 as captured
    effect_b_params: [f32; 2],  // knobs 3–4 as captured
    mix: f32,                    // knob 5 as captured
}

impl ChainScene {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        Self {
            effect_a_params: [
                a.effect_a_params[0] + (b.effect_a_params[0] - a.effect_a_params[0]) * t,
                a.effect_a_params[1] + (b.effect_a_params[1] - a.effect_a_params[1]) * t,
            ],
            effect_b_params: [
                a.effect_b_params[0] + (b.effect_b_params[0] - a.effect_b_params[0]) * t,
                a.effect_b_params[1] + (b.effect_b_params[1] - a.effect_b_params[1]) * t,
            ],
            mix: a.mix + (b.mix - a.mix) * t,
        }
    }
}
```

With Toggle 1 and Toggle 2 selecting effects: the morph knob sweeps between two complete multi-effect states simultaneously. Distortion drive backing off while delay feedback grows while the parallel mix shifts toward the delay path. One knob. One gesture. A performance transformation.

### 12.3 Scene Persistence

Captured scenes can persist across power cycles by writing to the Daisy Seed's QSPI flash. The data is tiny — each `KernelParams` struct is a handful of `f32` values:

```rust
// 5 params × 4 bytes = 20 bytes per scene
// 2 scenes × 9 effect slots = 18 scenes
// 18 × 20 = 360 bytes total
// QSPI flash has 8 MB. This is nothing.
```

Save on capture. Load on boot. Scenes survive power cycling without a file system, database, or operating system.

### 12.4 Expression Pedal (Future Hardware Expansion)

The Daisy Seed has unused ADC pins. Adding an expression pedal input replaces Knob 6 as the morph source — now the morph is foot-controlled and all six knobs stay as effect parameters:

```rust
let morph_t = if expression_pedal_connected {
    adc.read(EXPRESSION_PIN)
} else {
    adc.read(KNOB_6)
};
```

And here's where it gets strange: the expression pedal input is just another `f32`. It can come from anywhere. An envelope follower on the input signal. An LFO. A random walk. MIDI CC. The morph parameter doesn't care about its source — it's just `t`.

Play softly → clean preset. Play hard → driven preset. Your playing dynamics control the scene position. The parameter space becomes responsive to your performance without any dedicated firmware code — it's just a different source for `t`.

### 12.5 Cross-Effect Morphing via Graph Topology

True morphing *between* effect types (not just between presets of the same effect) uses the DAG graph with a parallel topology:

```
Input → Split ──→ Distortion ──→ Merge → Output
              └──→ Reverb     ──┘
```

The morph parameter controls each effect's mix:

```rust
fn cross_effect_morph(t: f32) -> (DistortionParams, ReverbParams) {
    let mut dist = distortion_scene;
    let mut verb = reverb_scene;
    
    // Complementary mix: as one rises, the other falls
    dist.mix_pct = (1.0 - t) * 100.0;
    verb.mix_pct = t * 100.0;
    
    (dist, verb)
}
```

At `t = 0.0`: pure distortion. At `t = 1.0`: pure reverb. At `t = 0.5`: both effects active at 50%, creating a sound that doesn't exist as either effect alone. And because each effect's *other* parameters can also be morphing (drive decreasing as decay increases), the transition is musically shaped, not just a volume crossfade.

### 12.6 What This Means for DigiTech

The Whammy is legendary because it mapped a continuous physical gesture (expression pedal) to a musical parameter space (pitch) so naturally that guitarists think of it as an instrument, not an effect. It took dedicated DSP engineering to make that one mapping work.

What the kernel architecture gives you is the Whammy principle generalized: any effect, any two parameter states, one continuous control, zero special-case firmware. The morph isn't a feature bolted onto the effect — it's a property of the data structure. Every kernel gets it for free because every `KernelParams` struct supports `lerp`.

A DigiTech engineer looks at this and sees:
- No per-effect morph firmware (it's generic over all `KernelParams`)
- No morph table storage (it's computed, not stored)
- No morph curve calibration (parameter-space interpolation is inherently musically correct)
- Scene capture is 20 bytes of flash, not a preset management system
- Expression pedal integration is one line of code, not a firmware subsystem

The complexity that took teams of engineers months to build for single-purpose products falls out of three things: a struct, a trait, and a function that's five lines long.

### 12.7 Implementation Dependency

All of the above requires:
1. Kernel migrations complete for the effects you want to deploy (Phase 2 in section 4)
2. Hothouse platform crate with ADC reading, toggle/footswitch debouncing, LED control
3. A thin `PedalController` state machine for mode/capture logic (~200 lines)

None of it requires new architecture. The kernel traits, the adapter, the params struct, `lerp`, `from_knobs`, `from_normalized` — it's all already there. The pedal controller is application code, not framework code.

