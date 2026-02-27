# DSP Quality Standard

Rules governing gain staging, bypass parity, feedback stability, and parameter
vocabulary for all Sonido effects. Every effect in `sonido-effects` must pass
all seven rules at default parameters before merge.

## Rules

### Rule 1: Peak Ceiling

Default parameters must produce peak output no greater than **-1 dBFS** when
driven with a 0 dBFS input signal.

**Rationale:** EBU R128 true-peak compliance. Intersample peaks can exceed
0 dBFS during D/A conversion; the -1 dB margin prevents clipping in downstream
stages (DAC, broadcast limiter, codec).

**Measurement:** 5 seconds of 0 dBFS sine at 1 kHz, 48 kHz sample rate. Report
peak in dBFS.

### Rule 2: Bypass Parity

Default parameters must produce output within **+/-1 dB RMS** of the dry input
signal level (bypass level).

**Exceptions:**
- Gain-purpose effects (preamp, compressor with makeup gain) are exempt because
  their purpose is to change level.
- Time-varying effects (flanger, phaser, chorus) may exhibit momentary overshoot
  from LFO modulation or comb resonance, but the RMS over a sustained signal
  must stay within tolerance.

**Rationale:** Inserting or bypassing an effect in a chain should not cause
audible level jumps. Users expect unity gain at default settings.

### Rule 3: Output Parameter Contract

Every effect exposes a gain staging parameter as the **last**
`ParameterInfo` index. Most effects use `gain::output_param_descriptor()` from
`crates/sonido-core/src/gain.rs`, which provides:

| Field | Value |
|-------|-------|
| Name | "Output" |
| Unit | dB |
| Range | -20 to +20 dB |
| Default | 0 dB (unity) |
| Smoothing | `SmoothedParam::standard` (10 ms) |

**Naming exceptions** (domain-appropriate names that serve the same purpose):

| Effect | Last Gain Param | Name | Index |
|--------|----------------|------|-------|
| Distortion | Level | "Level" | 2 (not last; "Waveshape" follows) |
| Compressor | Makeup Gain | "Makeup Gain" | 4 (not last; "Knee" follows) |

These effects use domain-standard vocabulary ("level" for distortion output
stages, "makeup" for compression gain restoration) rather than the generic
"output" name. They comply with the spirit of the contract (user-accessible
gain staging) but not the letter (last index).

All other effects use `gain::output_param_descriptor()` at their final index.

**Implementation:** See `crates/sonido-core/src/gain.rs` for the shared
constructor, dB get/set helpers, and `ParamDescriptor`.

### Rule 4: Wet/Dry Contract

The mix parameter must satisfy:

- **mix = 0%** (or 0.0 internally): output is **bit-exact dry** -- no
  processing artifacts, no latency compensation error, no floating-point
  deviation from the input sample.
- **mix = 100%** (or 1.0 internally): output is **pure wet** -- no dry signal
  leakage.

**Implementation:** All effects use `wet_dry_mix()` / `wet_dry_mix_stereo()`
from `crates/sonido-core/src/math.rs`:

```rust
// dry + (wet - dry) * mix
// At mix=0: dry + (wet - dry) * 0 = dry  (exact)
// At mix=1: dry + (wet - dry) * 1 = wet  (exact)
pub fn wet_dry_mix(dry: f32, wet: f32, mix: f32) -> f32 {
    dry + (wet - dry) * mix
}
```

This formulation is algebraically exact at both endpoints (no `1.0 - mix`
multiplication residue).

### Rule 5: Feedback Stability

All feedback loops must converge for every valid parameter combination. No
parameter setting within the declared range may produce unbounded energy growth.

**Enforcement mechanisms:**

| Mechanism | Where | Details |
|-----------|-------|---------|
| Feedback cap at 95% | `ParamDescriptor::feedback()` | `max: 95.0` percent. Applied in `set_feedback()` via `clamp(0.0, 0.95)`. |
| Wet-signal compensation | `gain::feedback_wet_compensation()` | Scales wet by `(1-fb)` for exact peak-gain cancellation in comb topologies. |
| Reverb comb compensation | `reverb.rs:update_comb_params()` | Uses `sqrt(1-fb)` (moderate) — parallel averaging provides additional headroom. |
| Denormal flushing | `flush_denormal()` in `crates/sonido-core/src/math.rs` | Prevents CPU stalls from subnormal float decay in feedback paths. |
| Comb filter damping | `CombFilter::set_damp()` | One-pole lowpass in the feedback path ensures HF energy decays faster than LF. |
| Allpass coefficient bounds | `AllpassFilter` | Coefficient clamped to stable range. |

**Effects with feedback paths:** Delay, Flanger, Phaser, Reverb (8 comb
filters per channel), Chorus.

### Rule 6: Headroom Budget

Effects are designed for **-18 dBFS nominal** operating level and must
**tolerate 0 dBFS peak** input without clipping, distortion artifacts, or
numerical instability.

**Rationale:** Professional audio convention. -18 dBFS RMS with music-like
crest factor yields peaks near 0 dBFS. Effects that clip internally at 0 dBFS
would distort on transients; effects that assume -18 dBFS nominal can use
the full dynamic range without premature saturation.

**Chain-friendly:** When multiple effects are chained, each should neither
amplify nor attenuate significantly at default settings (Rule 2), so the
headroom budget propagates through the chain.

### Rule 7: Parameter Vocabulary

All parameters use consistent units and naming from `ParamUnit` in
`crates/sonido-core/src/param_info.rs`:

| Unit | Suffix | Usage |
|------|--------|-------|
| `Decibels` | " dB" | Gain, threshold, level, output |
| `Hertz` | " Hz" | Frequency, cutoff, rate |
| `Milliseconds` | " ms" | Time, delay, attack, release |
| `Percent` | "%" | Mix, depth, feedback, room size |
| `Ratio` | ":1" | Compression ratio |
| `None` | "" | Waveshape selector, reverb type |

**Naming conventions:**
- `ParamDescriptor::mix()` -- standard 0-100% wet/dry
- `ParamDescriptor::depth()` -- standard 0-100% modulation depth
- `ParamDescriptor::feedback()` -- standard 0-95% with stability cap
- `ParamDescriptor::time_ms()` -- time parameter with custom range
- `ParamDescriptor::gain_db()` -- gain parameter with custom range
- `gain::output_param_descriptor()` -- universal output level

Short names are 8 characters or fewer for hardware display compatibility.

---

## Per-Effect Compliance

Status as of 2026-02-22 against 0 dBFS sine input at 48 kHz, default parameters.

| Effect | Category | R1 Peak | R2 Bypass | R3 Output | R4 Mix | R5 Feedback | R6 Headroom | R7 Vocab |
|--------|----------|---------|-----------|-----------|--------|-------------|-------------|----------|
| Distortion | Distortion | PASS | PASS | PASS* | PASS | N/A | PASS | PASS |
| Tape Saturation | Distortion | PASS | PASS | PASS | PASS | N/A | PASS | PASS |
| Compressor | Dynamics | PASS | PASS** | PASS* | N/A | N/A | PASS | PASS |
| Gate | Dynamics | PASS | PASS | PASS | N/A | N/A | PASS | PASS |
| Chorus | Modulation | PASS | PASS | PASS | PASS | PASS | PASS | PASS |
| Flanger | Modulation | PASS | PASS | PASS | PASS | PASS | PASS | PASS |
| Phaser | Modulation | PASS | PASS | PASS | PASS | PASS | PASS | PASS |
| Tremolo | Modulation | PASS | PASS | PASS | N/A | N/A | PASS | PASS |
| Multi Vibrato | Modulation | PASS | PASS | PASS | PASS | N/A | PASS | PASS |
| Delay | Time-Based | PASS | PASS | PASS | PASS | PASS | PASS | PASS |
| Reverb (Room) | Time-Based | PASS | PASS | PASS | PASS | PASS | PASS | PASS |
| Reverb (Hall) | Time-Based | PASS | PASS | PASS | PASS | PASS | PASS | PASS |
| Low Pass Filter | Filter | PASS | PASS | PASS | PASS | N/A | PASS | PASS |
| Wah | Filter | PASS | PASS | PASS | PASS | N/A | PASS | PASS |
| Parametric EQ | Filter | PASS | PASS | PASS | N/A | N/A | PASS | PASS |
| Clean Preamp | Utility | PASS | PASS** | PASS | N/A | N/A | PASS | PASS |
| Limiter | Dynamics | PASS | PASS** | PASS | N/A | N/A | PASS | PASS |
| Bitcrusher | Distortion | PASS | PASS | PASS | PASS | N/A | PASS | PASS |
| Ring Mod | Modulation | PASS | PASS | PASS | PASS | N/A | PASS | PASS |
| Stage | Utility | PASS | PASS** | PASS | N/A | N/A | PASS | PASS |

**Legend:**
- `*` Exception to Rule 3 naming (see Rule 3 table)
- `**` Exempt from Rule 2 (gain-purpose effect)
- `N/A` Rule does not apply (e.g., no feedback path, no mix control)

### Reverb Hall Preset: Gain Compensation

The hall preset (room_size=0.8, decay=0.8, damping=0.3) previously peaked at
+11.8 dB above input level. This was fixed by feedback-adaptive comb
compensation — a smooth quadratic curve that scales the comb output inversely
with effective feedback. See `docs/DSP_FUNDAMENTALS.md` for the formula and
`docs/DESIGN_DECISIONS.md` ADR-018 for rationale.

### Delay, Flanger, Phaser: Feedback Compensation

These effects use `gain::feedback_wet_compensation()` — exact `(1-fb)` wet-signal
scaling that cancels comb-filter peak gain at resonance:

| Effect | Before (sqrt) | After (exact) | Feedback | Compensation |
|--------|--------------|---------------|----------|-------------|
| Delay | -0.8 dBFS | -2.0 dBFS | 0.4 | 0.6 |
| Flanger | -0.5 dBFS | -2.1 dBFS | 0.5 | 0.5 |
| Phaser | -1.1 dBFS | -2.6 dBFS | 0.5 | 0.5 |

All three now pass Rule 1 (peak ceiling) with comfortable margin. The exact
compensation guarantees the wet signal at resonance never exceeds the dry signal,
regardless of feedback setting. See `docs/DSP_FUNDAMENTALS.md` (Feedback
Resonance Compensation) and `docs/DESIGN_DECISIONS.md` ADR-019 for the math.

---

## Measurement Protocol

### Rule 1: Peak Ceiling Test

Generate a 0 dBFS, 1 kHz sine for 5 seconds and measure peak output:

```bash
sonido generate tone demos/test_input.wav --freq 1000 --duration 5.0
sonido process demos/test_input.wav --effect <effect>
sonido analyze dynamics demos/test_input_<effect>.wav
```

Pass criterion: reported peak <= -1.0 dBFS.

### Rule 2: Bypass Parity Test

Compare RMS of processed vs. dry signal:

```bash
sonido process demos/test_input.wav --effect <effect>
sonido compare demos/test_input.wav demos/test_input_<effect>.wav --detailed
```

Pass criterion: |RMS_processed - RMS_dry| <= 1.0 dB.

### Rule 3: Output Parameter Audit

Verify the last parameter index is a gain/output control:

```bash
sonido effects <effect>
```

Check that the last listed parameter has unit "dB" and serves as a gain
staging control.

### Rule 4: Wet/Dry Exactness Test

```bash
sonido process demos/test_input.wav --effect <effect> --param mix=0
sonido compare demos/test_input.wav demos/test_input_<effect>.wav --detailed

sonido process demos/test_input.wav --effect <effect> --param mix=1
```

Pass criterion: MSE = 0.0 at mix=0 (bit-exact).

### Rule 5: Feedback Stability Test

Drive effect at maximum feedback with sustained input and verify output
remains bounded:

```bash
sonido generate tone demos/long_tone.wav --freq 1000 --duration 10.0
sonido process demos/long_tone.wav --effect <effect> --param feedback=0.95
sonido analyze dynamics demos/long_tone_<effect>.wav
```

Pass criterion: peak remains finite and does not grow monotonically over the
test duration.

### Rule 6: Headroom Test

Process -18 dBFS and 0 dBFS inputs, verify no clipping or NaN:

```bash
# Nominal level (-18 dBFS)
sonido generate tone demos/nominal.wav --freq 1000 --duration 5.0 --amplitude 0.125
sonido process demos/nominal.wav --effect <effect>
sonido analyze dynamics demos/nominal_<effect>.wav

# Full-scale (0 dBFS)
sonido process demos/test_input.wav --effect <effect>
sonido analyze dynamics demos/test_input_<effect>.wav
```

Pass criterion: no NaN, no Inf, no unexpected clipping artifacts.

### Rule 7: Vocabulary Audit

```bash
sonido effects <effect>
```

Verify parameter names, units, and ranges match the conventions in Rule 7.

---

## Source References

| Component | File |
|-----------|------|
| Output level contract | `crates/sonido-core/src/gain.rs` |
| Wet/dry mix | `crates/sonido-core/src/math.rs` (`wet_dry_mix`, `wet_dry_mix_stereo`) |
| Denormal flushing | `crates/sonido-core/src/math.rs` (`flush_denormal`) |
| Feedback descriptor | `crates/sonido-core/src/param_info.rs` (`ParamDescriptor::feedback`) |
| Parameter units | `crates/sonido-core/src/param_info.rs` (`ParamUnit`) |
| ParameterInfo trait | `crates/sonido-core/src/param_info.rs` |
| Reverb comb feedback | `crates/sonido-effects/src/reverb.rs:298-302` |
| Effect registry | `crates/sonido-registry/src/lib.rs` |
