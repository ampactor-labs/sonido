# Hendrix-Inspired Effects — Implementation Brief

This document contains everything needed to implement new Sonido effects derived from Jimi Hendrix's analog signal chain: the DSP, the architecture decisions, the documentation conventions, and the validation targets.

## Source Material

- **Article**: ["Jimi Hendrix Was a Systems Engineer"](https://spectrum.ieee.org/jimi-hendrix-systems-engineer), Rohan S. Puranik, IEEE Spectrum, March 2026.
- **Repo**: [github.com/nahorov/Hendrix-Systems-Lab](https://github.com/nahorov/Hendrix-Systems-Lab), containing SPICE netlists, Python DSP models, WAV stems (48kHz/24-bit) and SVG figures.
- **Sonido reference doc**: `docs/reference/hendrix-signal-chain.md`, a full cross-reference between Hendrix's rig and Sonido's existing effects, including validation opportunities.

The Hendrix Systems Lab repo is open source and contains physically-grounded SPICE simulations of every pedal in the chain. The Python layer re-implements the same effects as discrete-time DSP and produces audible output. Both tiers are useful: SPICE for validating transfer functions, Python for validating audio output.

---

## New Effects to Implement

### 1. Octavia (Rectifier Octave-Up)

**What it is**: A frequency-doubling effect based on full-wave rectification. It is a waveform symmetry transformation that maps fundamental frequency `f` to `2f`, which is different from distortion in the harmonic-addition sense.

**Core algorithm**:
```
rect(x) = abs(x)                        // full-wave rectification → octave up
shaped(x) = tanh(drive * (rect - bias))  // asymmetric saturation adds character
output = tone_filter(shaped)             // post-rect LP to tame harshness
```

`|sin(2πft)|` has fundamental frequency `2f` because folding negative half-cycles positive doubles the number of peaks per second. This is the mechanism Roger Mayer built into the original Octavia pedal for Hendrix.

**ADAA compatibility**: The antiderivative of `abs(x)` is `x * |x| / 2`. This is closed-form and piecewise differentiable, so it is fully compatible with Sonido's `Adaa1` wrapper. Use ADAA for the rectification stage to suppress aliasing from the discontinuity at zero crossings.

**Parameters**:

| Name | Range | Default | Unit | Scale | Description |
|------|-------|---------|------|-------|-------------|
| Drive | 1.0-20.0 | 6.0 | none | Logarithmic | Pre-rectification gain |
| Bias | -0.5 to 0.5 | -0.1 | none | Linear | DC offset before saturation (controls asymmetry) |
| Tone | 500-8000 | 3500 | Hz | Logarithmic | Post-rect lowpass cutoff |
| Mix | 0.0-1.0 | 0.7 | % | Linear | Dry/wet blend |

**Reference signals from Hendrix Systems Lab**:
- `octavia_behavioral.cir` / `octavia_transformer_rect.cir`: SPICE netlists
- `oct_time.svg`: transient waveform plot (input sine → rectified → shaped output)
- Python model in `hendrix_lab.py`, function `octavia(x)`:
  ```python
  rect = np.abs(x)
  out = np.tanh(6 * (rect - 0.1))
  ```

**Architecture decision**: Implement as a standalone effect in `sonido-effects`, not as a new `Distortion` mode. Rationale: rectification is a different kind of signal transformation from waveshaping. Distortion adds harmonics to the existing fundamental; Octavia shifts the fundamental itself. They compose well in sequence (Fuzz → Octavia = Purple Haze tone) but shouldn't share a mode enum.

**Suggested file**: `crates/sonido-effects/src/kernels/octavia.rs`

### 2. Fuzz (Vintage Two-Transistor)

**What it is**: A mixed soft/hard clipping distortion with a pre-emphasis lowpass filter. It differs from Sonido's existing `Distortion` in two ways: (1) the soft/hard mix ratio, and (2) the "cleanup effect", a drive-dependent character transition where reducing input amplitude restores a clean sinusoidal output.

**Core algorithm**:
```
pre_emphasized = lp_filter(x, 3500 Hz)     // 1-pole IIR, tames HF before clipping
soft = tanh(drive * pre_emphasized)         // odd harmonics
hard = clamp(drive * pre_emphasized, -t, t) // even + odd harmonics (t = threshold)
mixed = 0.55 * soft + 0.45 * hard          // fixed mix ratio from SPICE match
output = tone_eq(mixed)                     // post-clip tone shaping
```

The 0.55/0.45 soft/hard mix ratio comes from the Hendrix Systems Lab's calibration against SPICE transient output of the actual Fuzz Face circuit.

**The cleanup effect**: The Fuzz Face's input impedance is ~20k ohm. When guitar volume is reduced, the pedal operates in its linear region and the output becomes sinusoidal. This is a nonlinear, frequency-dependent transition, which an amplitude gate alone would not reproduce. Model it by scaling the drive parameter with an `EnvelopeFollower` on the input signal:

```
env = envelope_follower(input, attack=5ms, release=50ms)
effective_drive = drive * env.normalize(0.3, 1.0)  // minimum 30% drive at silence
```

**Parameters**:

| Name | Range | Default | Unit | Scale | Description |
|------|-------|---------|------|-------|-------------|
| Fuzz | 0.0-40.0 | 20.0 | dB | Linear | Drive amount |
| Volume | -20.0 to 6.0 | 0.0 | dB | Linear | Output level |
| Tone | 500-8000 | 2000 | Hz | Logarithmic | Post-clip LP cutoff |
| Mode | Si / Ge | Si | none | Stepped | Silicon (brighter, tighter) vs Germanium (warmer, softer) preset curves |
| Cleanup | 0.0-1.0 | 0.5 | none | Linear | Envelope-follower depth for drive modulation |

**Si vs Ge differences** (from SPICE transistor models):
- **Silicon**: BF=200, sharper clipping knee, less HF rolloff, temperature-stable
- **Germanium**: BF=120, softer knee, more HF rolloff (higher junction capacitance), temperature-dependent bias drift

Model as different soft/hard mix ratios and pre-emphasis filter cutoffs, not as different transistor simulations:
- Si: 0.55 soft / 0.45 hard, pre-emphasis at 3500 Hz
- Ge: 0.70 soft / 0.30 hard, pre-emphasis at 2800 Hz (more HF rolloff)

**Reference signals from Hendrix Systems Lab**:
- `fuzzface_si.cir` / `fuzzface_ge_pnp_posgnd.cir`: full SPICE netlists with transistor models
- `ff_si_time.svg` / `ff_ge_time.svg`: transient waveform comparisons
- `ff_si_zin_mag.svg`: input impedance vs frequency (for validating the cleanup interaction)
- `temp_bias_plot.py`: Ge thermal drift analysis

**Suggested file**: `crates/sonido-effects/src/kernels/fuzz.rs`

### 3. Enhancement: Phaser AM Component (Uni-Vibe Character)

**What it is**: The existing `Phaser` effect implements the allpass cascade but lacks the amplitude modulation component that distinguishes a Uni-Vibe from a generic phaser. The real Uni-Vibe's lamp brightness varies signal level (AM) alongside phase (PM), with the AM running at half the phase modulation rate.

**Change**: Add an optional `am_depth` parameter to the existing `Phaser`.

**Algorithm addition** (applied after the existing allpass processing):
```
am_mod = 1.0 - am_depth * 0.5 * (1.0 + sin(2π * (rate / 2) * t))
output = phased_signal * am_mod
```

The half-rate AM creates the slow "throb" underneath the phase sweep, which is the characteristic Uni-Vibe sound.

**New parameter**:

| Name | Range | Default | Unit | Description |
|------|-------|---------|------|-------------|
| AM Depth | 0.0-0.5 | 0.0 | none | Half-rate amplitude modulation depth. 0 = pure phaser (backward compatible). 0.15 = classic Uni-Vibe. |

**Reference**: `univibe_frozen_lfo.cir` and the Python model's AM layer:
```python
y *= 0.9 * (1 + 0.15 * np.sin(2 * np.pi * (rate / 2) * t))
```

A default of 0.0 preserves backward compatibility: existing presets and tests are unaffected.

---

## Documentation Conventions

When implementing these effects, documentation goes in three layers. Don't duplicate content across layers; link between them instead.

### Layer 1: Source Code Rustdoc (on the struct)

The rustdoc holds the algorithm derivation, the math, and the reason for this approach, and stays brief. This is where the DSP is documented.

Example for Octavia:
```rust
/// Rectifier-based octave-up effect.
///
/// Full-wave rectification (`|x|`) maps fundamental frequency `f` to `2f`
/// by folding negative half-cycles positive. Followed by asymmetric
/// saturation and tone filtering.
///
/// Uses ADAA (first-order anti-derivative anti-aliasing) on the rectification
/// stage. Antiderivative of `|x|` is `x·|x|/2`.
///
/// Based on the Octavia pedal designed by Roger Mayer for Jimi Hendrix (1967).
/// See `docs/reference/hendrix-signal-chain.md` for circuit analysis and
/// validation data from SPICE simulations.
pub struct OctaviaKernel { ... }
```

Rules:
- Document the algorithm and the math: what the code computes and why
- Cite the inspiration in one line: who, what, when
- Link to `docs/reference/hendrix-signal-chain.md` for depth; don't inline circuit analysis into rustdoc
- Every public method gets a `///` doc comment (existing Sonido convention)

### Layer 2: EFFECTS_REFERENCE.md

This layer is user-facing: what the effect sounds like, a parameters table and usage examples, with no circuit theory.

Example entry:
```markdown
## Octavia

Rectifier-based octave-up effect inspired by the Octavia pedal designed by
Roger Mayer for Jimi Hendrix (1967). Doubles the input frequency via
full-wave rectification, producing a bright octave-up tone with fuzz character.

| Parameter | Range | Default | Unit | Description |
|-----------|-------|---------|------|-------------|
| Drive | 1.0–20.0 | 6.0 | — | Pre-rectification gain |
| Bias | -0.5–0.5 | -0.1 | — | Asymmetry control |
| Tone | 500–8000 | 3500 | Hz | Post-rect lowpass cutoff |
| Mix | 0.0–1.0 | 0.7 | % | Dry/wet blend |

**Usage**: Chain after Fuzz for the classic "Purple Haze" tone.
Effective on single-note lines; chords produce dense intermodulation.
```

Rules:
- At most one sentence of provenance ("inspired by X")
- Full parameter table with ranges, defaults, units
- Practical usage notes: what it sounds good on, what to chain it with
- No DSP math, no circuit diagrams, no SPICE references

### Layer 3: docs/reference/hendrix-signal-chain.md (already exists)

This is the research source: SPICE comparisons, validation data, chain interaction notes, and cross-references to the Hendrix Systems Lab repo. It holds the detailed background.

Rules:
- Update this doc when adding validation results (e.g., spectral comparison plots, SNR measurements against SPICE output)
- Don't duplicate into effect-level docs; link from rustdoc instead
- This doc is for developers, not users

### What NOT to create:
- No per-effect "inspiration" markdown files
- No "references" appendix in EFFECTS_REFERENCE.md
- No ADR unless there's a genuine architectural fork (Octavia-as-standalone-vs-Distortion-mode qualifies; "we modeled a Fuzz Face" does not)

---

## Architecture Notes

### Effect Registration

Both new effects follow the kernel workflow in `docs/EFFECT_SDK.md`: a UID appended to `EFFECT_UIDS` and a `register_builtin_effects()` entry in `crates/sonido-registry/src/lib.rs`, plus a `CATALOG` entry in `crates/sonido-plugin/src/bundle.rs`:

```rust
// In register_builtin_effects()
self.register(
    EffectDescriptor {
        id: "octavia",
        name: "Octavia",
        short_name: "OCTV",
        description: "Rectifier octave-up",
        category: EffectCategory::Distortion,
        param_count: 0,
    },
    |sr| Box::new(Adapter::new(OctaviaKernel::new(sr), sr)),
);
```

Category is `EffectCategory::Distortion` for both (they appear in the same UI group), even though Octavia's mechanism is distinct.

### Kernel Implementation

Each effect is an `XxxParams` (`KernelParams`) + `XxxKernel` (`DspKernel`) pair, deployed through `Adapter`. Both are dual-mono (no cross-channel interaction), so `is_true_stereo()` keeps its `false` default.

Use the `kernel_params!` macro for the parameter table. Give continuous parameters (drive, tone, mix) a `SmoothingStyle` (`Fast` for drive, `Slow` for the tone filter, `Standard` for mix); Mode (Si/Ge) is stepped, so `SmoothingStyle::None`.

### Preset Chain: "Hendrix Rig"

After implementing both effects, create a preset that demonstrates the historically correct chain order:

```
Fuzz (Ge mode, Fuzz=25dB) → Octavia (Drive=6, Tone=3500) → Wah (freq=auto) → Phaser (am_depth=0.15)
```

This can be a TOML factory preset in `crates/sonido-config/src/factory_presets.rs` or a CLI example in the docs.

### Testing

Follow existing Sonido testing conventions:

**Unit tests** (in each effect's module):
- Silence in → silence out
- DC offset handling
- Parameter range clamping
- Sample rate change recalculates coefficients
- `reset()` clears all state

**Golden file regression** (in `tests/`):
- Process a known input signal (sine sweep or the `guitarish_note` from Hendrix Systems Lab)
- Compare against saved reference WAV
- Thresholds per `crates/sonido-effects/tests/regression.rs`: MSE < 1e-6, SNR > 60 dB, spectral correlation > 0.9999

**Validation against SPICE** (optional, documented in `docs/reference/hendrix-signal-chain.md`):
- Compare Sonido's Octavia output against the Hendrix Systems Lab's `oct_time.dat` transient
- Compare Fuzz output against `ff_si_time.dat` / `ff_ge_time.dat`
- Report spectral differences, which measure how close the DSP approximation is to the analog circuit

### Doc-to-Code Mapping Updates

When the effects are implemented, update the mapping table in `docs/DOC_CODE_MAPPING.md`:

| Source Module | Doc Target |
|---|---|
| `crates/sonido-effects/src/kernels/octavia.rs` | `EFFECTS_REFERENCE.md` (Octavia section), `docs/reference/hendrix-signal-chain.md` |
| `crates/sonido-effects/src/kernels/fuzz.rs` | `EFFECTS_REFERENCE.md` (Fuzz section), `docs/reference/hendrix-signal-chain.md` |
| `crates/sonido-effects/src/kernels/phaser.rs` | `EFFECTS_REFERENCE.md` (Phaser section: update it for the AM Depth param) |

---

## Summary

Three implementation items, in suggested order:

1. **Octavia**: new standalone effect, clean algorithm, ADAA-compatible, smallest scope
2. **Phaser AM enhancement**: single parameter addition to existing effect, backward compatible
3. **Fuzz**: new standalone effect, more complex (Si/Ge modes, envelope-follower cleanup)

Document at three layers: rustdoc (algorithm + math), EFFECTS_REFERENCE.md (user-facing), `docs/reference/hendrix-signal-chain.md` (research + validation). Link between layers instead of duplicating.
