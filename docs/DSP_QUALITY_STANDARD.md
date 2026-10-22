# DSP Quality Standard

Rules governing gain staging, bypass parity, feedback stability, parameter
vocabulary, and level calibration for all Sonido effects. Rules 1-7 are the
design targets; `crates/sonido-effects/tests/quality_standard.rs` enforces each
one in the form described under **Automated Enforcement** below, iterating
every effect in the registry. Rule 8, the level contract, is enforced as
written by `crates/sonido-effects/tests/level_contract.rs`. Those tests must
pass before merge.

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
- Gain-purpose effects (preamp, amp, distortion) are not exempt at their
  defaults: Rule 8 calibrates them, like every effect but the looper and tuner
  utilities, to their input's loudness. Their drive and makeup controls change
  level by design once moved from their defaults.
- Time-varying effects (flanger, phaser, chorus) may exhibit momentary overshoot
  from LFO modulation or comb resonance, but the RMS over a sustained signal
  must stay within tolerance.

**Rationale:** Inserting or bypassing an effect in a chain should not cause
audible level jumps. Users expect an effect at its defaults to play at the
loudness of its input.

### Rule 3: Output Parameter Contract

Every effect exposes a dB gain staging parameter, normally as the **last**
`ParameterInfo` index. Most effects use `gain::output_param_descriptor()` from
`crates/sonido-core/src/gain.rs`, which provides:

| Field | Value |
|-------|-------|
| Name / short name | "Output" / "Out" |
| Unit | dB |
| Range | -6 to +6 dB (`OUTPUT_MIN_DB`..`OUTPUT_MAX_DB`; noon = 0 dB) |
| Default | 0 dB (unity) |
| Smoothing | `gain::output_level_param()` = `SmoothedParam::standard` (10 ms) |

**Non-last exceptions** (listed in `RULE3_NON_LAST_EXCEPTIONS`):

| Effect | Gain Param | Index | What follows it |
|--------|-----------|-------|-----------------|
| Distortion | "Output" | 2 | Waveshape, Mix, Dynamics |
| Compressor | "Output" (and "Makeup Gain" at 4) | 9 | Mix, READ_ONLY "Gain Reduction" |
| Filter | "Output" | 2 | Type |
| Chorus, Flanger, Phaser, Tremolo | "Output" | second to last | READ_ONLY "LFO Phase" |
| Gate | "Output" | second to last | READ_ONLY "Gate Open" |
| Tuner | "Output" | 2 | READ_ONLY detected Hz and cents |

For these the test only requires a dB-unit parameter whose name contains
"output", "level", "makeup" or "gain".

A few effects define their own range instead: distortion (-24 to +6 dB), tape
(-12 to +12 dB, default -6 dB), and drone, glitch, texture and time-stretch
(-60 to +6 dB).

The output parameter is the player's control, not a level-matching tool:
matching an effect to its input is Rule 8's measured trim, so don't move an
output default to make an effect match.

**Implementation:** See `crates/sonido-core/src/gain.rs` for the shared
constructor, dB get/set helpers, and `ParamDescriptor`.

### Rule 4: Wet/Dry Contract

The mix parameter must satisfy:

- **mix = 0%** (or 0.0 internally): output equals the dry input -- no
  processing artifacts, no latency compensation error. The blend itself is
  exact at mix = 0; the test allows 5e-4 per sample for smoother residue and
  the output-gain stage.
- **mix = 100%** (or 1.0 internally): output is **pure wet** (scaled by the
  effect's level trim, Rule 8) -- no dry signal leakage.
- **In between**, the level holds (Rule 8).

**Implementation:** Effects with a mix control mix with `Self::blend()` /
`Self::blend_stereo()`, provided by `DspKernel`
(`crates/sonido-core/src/kernel/traits.rs`) over `sonido_core::level::blend`
and `mix_gains` (`crates/sonido-core/src/level.rs`). `mix_gains` returns the
gains `(1, 0)` for mix <= 0 and `(0, 1)` for mix >= 1, so both endpoints are
exact. Between them it applies a constant-power sine/cosine law normalized for
the wet/dry correlation the calibration measured (see Rule 8). No kernel
chooses its own mix law or crossfade.

### Rule 5: Feedback Stability

All feedback loops must converge for every valid parameter combination. No
parameter setting within the declared range may produce unbounded energy growth.

**Enforcement mechanisms:**

| Mechanism | Where | Details |
|-----------|-------|---------|
| Feedback cap at 95% | `ParamDescriptor::feedback()` | `max: 95.0` percent; the `Adapter` clamps every value to the descriptor range before the kernel sees it. |
| Wet-signal compensation | `gain::feedback_wet_compensation()` | Scales wet by `(1-fb)` for exact peak-gain cancellation in comb topologies (delay, flanger, phaser). |
| Reverb FDN compensation | `reverb.rs` `update_derived()` | FDN feedback clamped to `0.0..0.99`; wet scaled by `sqrt(1-fb)` (moderate) -- the 8-line Hadamard mix spreads energy, so exact compensation is unnecessary. |
| Denormal flushing | `flush_denormal()` in `crates/sonido-core/src/math.rs` | Prevents CPU stalls from subnormal float decay in feedback paths. |
| Feedback damping | `OnePole` in the reverb FDN, `CombFilter::set_damp()` in core | One-pole lowpass in the feedback path ensures HF energy decays faster than LF. |
| Comb/allpass coefficient bounds | `CombFilter::set_feedback()`, `AllpassFilter::set_feedback()` | Clamped to `0.0..0.99` and `-0.99..0.99` respectively. |

**Effects with feedback paths tested by Rule 5:** Delay, Flanger, Phaser,
Reverb (8-line FDN), Chorus. Other kernels with internal feedback (looper
overdub, plate and spring reverbs, harmonic habitat) are covered by the
universal Rule 6 bound. The plate and spring reverbs set each loop's feedback
gain from the RT60 their Decay control names;
`crates/sonido-effects/tests/reverb_decay.rs` checks the measured RT60 is
within 25% of it.

### Rule 6: Headroom Budget

Effects are designed for **-18 dBFS nominal** operating level and must
**tolerate 0 dBFS peak** input without clipping, distortion artifacts, or
numerical instability.

**Rationale:** Professional audio convention. -18 dBFS RMS with music-like
crest factor yields peaks near 0 dBFS. Effects that clip internally at 0 dBFS
would distort on transients; effects that assume -18 dBFS nominal can use
the full dynamic range without premature saturation.

**Chain-friendly:** When multiple effects are chained, each should neither
amplify nor attenuate significantly at default settings (Rules 2 and 8), so the
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

Short names are 8 characters or fewer for hardware display compatibility
(all current kernels comply; no test enforces it).

### Rule 8: Level Contract

At its defaults every effect plays at the loudness of its input, and its mix
control does not change that loudness.

**Declaration:** every kernel declares its measured level in its `DspKernel`
impl:

```rust
const LEVEL: Level = crate::calibration::DELAY;
```

`Level` (`crates/sonido-core/src/level.rs`) holds `trim`, a gain, and
`mix_correlation`, the measured correlation of the wet signal with the dry
input (`None` for an effect without a mix control).

- **With a mix control:** the kernel mixes with `Self::blend` /
  `Self::blend_stereo` (Rule 4). The trim scales the wet signal, and the dry
  and wet gains `cos(πm/2)`, `sin(πm/2)` are normalized by the blend's power,
  `1/√(1 + ρ·sin(πm))`, so the loudness is the same at any mix for that
  correlation `ρ`.
- **Without a mix control:** the kernel passes its sound through
  `Self::level` / `Self::level_stereo` where it leaves the processing, before its
  output knob and any safety limiter, so the limiter guards the final level.

**Measured, never hand-tuned:** `crates/sonido-effects/src/calibration.rs` is
generated by

```bash
cargo run -p sonido-effects --example calibrate --release
```

which plays `sonido_analysis::signals::reference_guitar` (a strummed guitar
peaking at `NOMINAL_PEAK_DBFS` = -20 dBFS, the median measured from a
humbucker into the Hothouse) through every registered effect, fully wet when it
has a mix control and at its defaults otherwise, and measures the loudness
change in integrated LUFS (ITU-R BS.1770-4) with
`sonido_analysis::loudness::effect_gain_db`. Run it after changing a kernel's
sound or defaults; the test below fails until you do. Don't pick a mix law, add
a fixed gain, or move the output default to match levels by hand.

**Tolerances** (`level_contract.rs`):

| Check | Condition | Limit |
|-------|-----------|-------|
| Calibration current | Fully wet (mix control), else at defaults | within 0.5 dB of the dry loudness |
| Mix keeps the level | 0, 25, 50, 75% mix and the default mix | within 1.5 dB |

**Exceptions:** the looper and tuner are uncalibrated utilities (unity trim):
the looper's wet signal is its loop, and the tuner passes its input through.

---

## Automated Enforcement

`cargo test -p sonido-effects --test quality_standard` runs these checks on
default parameters at 48 kHz (helpers in `crates/sonido-effects/tests/helpers`):

| Test | Effects | Check |
|------|---------|-------|
| `rule1_peak_ceiling` | all | 1 s of 0 dBFS 1 kHz sine: peak < 4.0 (+12 dBFS). Strict -1 dBFS ceiling only for `limiter`; unity-gain effects peak at 0 dBFS on a full-scale sine. |
| `rule2_bypass_parity` | gate, eq, bitcrusher, tremolo | Output RMS within +/-3 dB of input RMS. |
| `rule3_output_parameter` | all | Last param is dB, or a dB output/level/makeup/gain param exists for the Rule 3 exceptions. |
| `rule4_wet_dry_exactness` | delay, flanger, phaser, reverb, ringmod, bitcrusher, vibrato | Mix = 0 after 100 ms settle: every sample within 5e-4 of input (440 Hz sine). |
| `rule5_feedback_stability` | delay, flanger, phaser, reverb, chorus | Every "feedback"/"decay" param at max, 10 s of 1 kHz sine: all samples finite and < 10.0. |
| `rule6_headroom` | all | 0 dBFS and -18 dBFS sines: all samples finite and < 10.0. |
| `rule7_vocabulary` | all | Non-empty names, `min < max`. |

The stricter targets in Rules 1 and 2 (-1 dBFS ceiling, +/-1 dB parity) are
design goals checked by the manual protocol below, not by this suite. Level at
defaults is also held, as loudness on a guitar signal, by the level contract:

`cargo test -p sonido-effects --test level_contract` checks Rule 8 for every
registered effect but the looper and tuner, with the tolerances in the Rule 8
table. If it reports a calibration out of date, run the calibrate example.

### Delay, Flanger, Phaser: Feedback Compensation

These effects use `gain::feedback_wet_compensation()` — exact `(1-fb)` wet-signal
scaling that cancels comb-filter peak gain at resonance:

The exact compensation guarantees the wet signal at resonance never exceeds
the dry signal, regardless of feedback setting (`feedback_wet_compensation`
clamps feedback to 0.99 and floors the factor at 0.01). It depends on the
feedback parameter and is applied to the wet signal before `blend`, which then
applies the effect's fixed level trim (Rule 8). See `docs/DSP_FUNDAMENTALS.md` (Feedback
Resonance Compensation) and `docs/DESIGN_DECISIONS.md` ADR-019 for the math.

---

## Measurement Protocol

### Rule 1: Peak Ceiling Test

Generate a 0 dBFS, 1 kHz sine for 5 seconds and measure peak output:

```bash
sonido generate tone demos/test_input.wav --freq 1000 --duration 5.0 --amplitude 1.0
sonido process demos/test_input.wav --effect <effect>
sonido analyze dynamics demos/test_input_<effect>.wav
```

`generate tone` defaults to `--amplitude 0.8`, so pass `1.0` for 0 dBFS.
`process` without an output path writes `<stem>_<effect>.wav` (plus
`_<key>=<value>` per `--param`) next to the input.

Pass criterion: reported "Peak level" <= -1.0 dBFS.

### Rule 2: Bypass Parity Test

Compare RMS of processed vs. dry signal:

```bash
sonido process demos/test_input.wav --effect <effect>
sonido compare demos/test_input.wav demos/test_input_<effect>.wav --detailed
```

Pass criterion: the "Level diff - RMS" line reports <= 1.0 dB in magnitude.

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
sonido compare demos/test_input.wav "demos/test_input_<effect>_mix=0.wav" --detailed

sonido process demos/test_input.wav --effect <effect> --param mix=100
```

Mix is in percent (0-100). Pass criterion: MSE ~ 0 at mix=0.

### Rule 5: Feedback Stability Test

Drive effect at maximum feedback with sustained input and verify output
remains bounded:

```bash
sonido generate tone demos/long_tone.wav --freq 1000 --duration 10.0
sonido process demos/long_tone.wav --effect <effect> --param feedback=95
sonido analyze dynamics "demos/long_tone_<effect>_feedback=95.wav"
```

Pass criterion: peak remains finite and does not grow monotonically over the
test duration.

### Rule 6: Headroom Test

Process -18 dBFS and 0 dBFS inputs, verify no clipping or NaN:

```bash
# Nominal level (-18 dBFS)
sonido generate tone demos/nominal.wav --freq 1000 --duration 5.0 --amplitude 0.126
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

### Rule 8: Level Contract Check

```bash
cargo test -p sonido-effects --test level_contract -- --nocapture
```

Prints one row per effect: its loudness change in dB fully wet (or at
defaults), then at 0, 25, 50 and 75% mix and its default mix. Pass criterion:
the Rule 8 tolerances. To recalibrate after a change:

```bash
cargo run -p sonido-effects --example calibrate --release
```

---

## Source References

| Component | File |
|-----------|------|
| Output parameter contract | `crates/sonido-core/src/gain.rs` |
| Level contract, wet/dry blend | `crates/sonido-core/src/level.rs` (`Level`, `blend`, `mix_gains`, `NOMINAL_PEAK_DBFS`) |
| Kernel blend and level declaration | `crates/sonido-core/src/kernel/traits.rs` (`DspKernel::LEVEL`, `blend`, `blend_stereo`) |
| Trim for kernels without a mix control | `DspKernel::level` / `level_stereo` in `crates/sonido-core/src/kernel/traits.rs` |
| Level calibration table (generated) | `crates/sonido-effects/src/calibration.rs` |
| Level calibration generator | `crates/sonido-effects/examples/calibrate.rs` |
| Reference signal, loudness measurement | `crates/sonido-analysis/src/signals.rs` (`reference_guitar`), `crates/sonido-analysis/src/loudness.rs` (`effect_gain_db`) |
| Denormal flushing | `crates/sonido-core/src/math.rs` (`flush_denormal`) |
| Feedback descriptor | `crates/sonido-core/src/param_info.rs` (`ParamDescriptor::feedback`) |
| Parameter units | `crates/sonido-core/src/param_info.rs` (`ParamUnit`) |
| ParameterInfo trait | `crates/sonido-core/src/param_info.rs` |
| Reverb FDN feedback | `crates/sonido-effects/src/kernels/reverb.rs` (`update_derived`) |
| Effect registry | `crates/sonido-registry/src/lib.rs` |
| Compliance tests | `crates/sonido-effects/tests/quality_standard.rs` |
| Level contract test | `crates/sonido-effects/tests/level_contract.rs` |
| Reverb decay (RT60) tests | `crates/sonido-effects/tests/reverb_decay.rs` |
