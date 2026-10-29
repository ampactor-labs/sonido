# Effects Reference

Complete parameter reference for all Sonido effects.

## Kernel Architecture

All 36 effects have kernel-architecture implementations in `crates/sonido-effects/src/kernels/`. Each effect defines `XxxKernel` (`DspKernel` impl) and `XxxParams` (`KernelParams` impl). The registry creates `Adapter<XxxKernel, SmoothedPolicy>`. All consumers (GUI, CLI, plugins and the pedal firmware, which plays every rig through `PatchPlayer`) use kernel-backed effects transparently.

Every effect is kernel-only; there are no per-effect `Effect` structs.

```rust
// Plugins, editor, CLI and pedal (what the registry builds): Adapter<K, SmoothedPolicy>
let adapter = Adapter::new(DistortionKernel::new(48000.0), 48000.0);

// Single-kernel firmware examples (single_effect.rs): Adapter<K, DirectPolicy>, no smoothing
let adapter = Adapter::new_direct(DistortionKernel::new(48000.0), 48000.0);

// Single-effect test examples only: call the kernel directly
let mut kernel = DistortionKernel::new(48000.0);
let params = DistortionParams::from_knobs(adc_drive, adc_tone, adc_output, adc_shape, adc_mix, adc_dynamics);
let (l, r) = kernel.process_stereo(input_l, input_r, &params);
```

## Effect Aliases

Some effects have alternate names for convenience (resolved by `resolve_effect_name()` in `crates/sonido-graph-dsl/src/effects.rs`, case-insensitive):

| Primary Name | Aliases | Notes |
|-------------|---------|-------|
| `filter` | `lowpass` | Multimode biquad filter |
| `vibrato` | `multivibrato`, `multi_vibrato` | 6-unit tape wow/flutter |
| `tape` | `tapesaturation`, `tape_saturation` | Tape saturation |
| `gate` | `noisegate`, `noise_gate` | Noise gate |
| `wah` | `autowah`, `auto_wah` | Auto-wah/manual wah |
| `preamp` | `cleanpreamp`, `clean_preamp` | Clean preamp |
| `eq` | `parametriceq`, `parametric_eq`, `peq` | 3-band parametric EQ |
| `bitcrusher` | `crusher` | Bit/sample-rate reduction |
| `ringmod` | `ring`, `ring_mod` | Ring modulator |
| `harmonic_habitat` | `habitat`, `harmonic`, `harmonichabitat` | Pitch-aware modal reverb |

Parameter names are matched case-insensitively against each parameter's name and short name, with underscores read as spaces (`room_size` → "Room Size"); a few extra per-effect shorthands (e.g. `lf`/`lg`/`lq` for the EQ, `time` for delay) are listed in `normalize_param_name()` in the same file.

Primary names and aliases work interchangeably in the CLI:

```bash
sonido process in.wav --effect filter --param cutoff=2000
sonido process in.wav --effect lowpass --param cutoff=2000  # Same effect
```

## Output Level Convention

Every effect exposes an `output` level parameter in dB, default 0.0 dB. Most use the range -6 to +6 dB; the exceptions are distortion (-24 to +6 dB), tape (-12 to +12 dB, default -6.0 dB), and drone, glitch, texture, and time_stretch (-60 to +6 dB). Effects with a separate gain-staging control keep it in addition to `output`: preamp `gain` (0 to 40 dB, default 15.0 dB), stage `gain` (-40 to +12 dB), amp `master` (-60 to 0 dB, default -6.0 dB), and compressor `makeup` (0 to 24 dB). `output` is usually the last writable parameter, but not always (distortion, filter, and compressor place other controls after it), and several effects append diagnostic parameters flagged `HIDDEN | READ_ONLY` (compressor `Gain Reduction`, gate `Gate Open`, chorus/flanger/phaser/tremolo `LFO Phase`, tuner `Detected Hz`/`Cents`) that report state for metering and cannot be set.

**Level contract** (`crates/sonido-core/src/level.rs`): at its defaults every effect plays at the loudness of its input, and its mix control does not change that loudness. Each kernel declares `const LEVEL: Level` (`DspKernel::LEVEL`), taken from the generated table in `crates/sonido-effects/src/calibration.rs`. `Level { trim, mix_correlation: Option<f32> }` holds a gain and, for an effect with a mix control, the measured correlation `ρ` between its wet signal and the dry input. Kernels with a mix control blend through `Self::blend` / `Self::blend_stereo`: the wet is multiplied by `trim`, then dry and wet are combined with sine/cosine constant-power gains (`cos(πm/2)`, `sin(πm/2)`) normalized by the blend's power for that correlation, `1/√(1 + ρ·sin(πm))`, so the blend's loudness is the same at every mix setting (`ρ = 0`, as for reverbs and echoes, is the plain constant-power crossfade; `ρ = 1`, as for saturation, keeps amplitude like a linear one). Effects without a mix control apply the trim themselves (`Self::level` / `Self::level_stereo`), before their output level and any safety limiter. Because the trim is fixed, gain-type controls are relative to their defaults: the preamp at its default 15 dB `gain` plays at the input's loudness, and more gain is louder.

The table comes from measurement and is not edited by hand: `cargo run -p sonido-effects --example calibrate --release` plays a reference strummed guitar (`sonido_analysis::signals::reference_guitar`, peaking at `NOMINAL_PEAK_DBFS` = -20 dBFS) through every effect, fully wet when it has a mix control and at its defaults otherwise, measures integrated loudness (LUFS, ITU-R BS.1770-4), and regenerates `calibration.rs`. `crates/sonido-effects/tests/level_contract.rs` checks every effect against it: within 0.5 dB of the dry loudness in that measurement and, for an effect with a mix control, within 1.5 dB at 0, 25, 50, and 75% mix and at its default mix. The looper and tuner are uncalibrated utilities (unity `Level`). Rerun the calibrate example after changing a kernel's sound or defaults. See `docs/DSP_QUALITY_STANDARD.md` for the full quality rules and compliance table.

---

## distortion

Waveshaping distortion with multiple modes.

**Signal flow** (`crates/sonido-effects/src/kernels/distortion.rs`):
```
Input -> [Envelope] -> Drive (gain, dynamic) -> ADAA Waveshaper -> Tone EQ (x makeup) -> Mix -> Soft Limit -> Output Level
```

The distortion applies a static nonlinear transfer function (waveshaper) to the input signal, preceded by a gain stage (drive) to push the signal into the nonlinear region. The `Adapter<K, SmoothedPolicy>` handles per-parameter smoothing via `SmoothingStyle` (Fast for drive, Slow for tone, None for waveshape, Standard for the rest). After the waveshaper the signal is scaled by `1/sqrt(drive)` makeup so higher drive adds saturation without a matching jump in level.

| Parameter | Description | Default | Range |
|-----------|-------------|---------|-------|
| `drive` | Drive amount in dB | 26.0 | 0-40 |
| `tone` | Tone tilt in dB (boost/cut at 1 kHz) | 1.5 | -12 to 12 |
| `output` | Output level in dB | 0.0 | -24 to 6 |
| `shape` | Waveshape type (0=Soft Clip, 1=Hard Clip, 2=Foldback, 3=Asymmetric) | 0 | 0-3 |
| `mix` | Wet/dry mix % | 100.0 | 0-100 |
| `dynamics` | Dynamic drive response (0=static, 100=fully responsive) | 0.0 | 0-100 |

### Waveshape Types and Their Harmonic Character

- **softclip** (`tanh`): Hyperbolic tangent soft clipping. Produces primarily odd harmonics (3rd, 5th, 7th...) with a smooth rolloff. The gentle compression curve means the signal transitions gradually into saturation, similar to a tube amplifier. Odd harmonics are musically consonant (octave + fifth, etc.).
- **hardclip**: Flat clipping at +/-1.0. Produces a dense series of odd harmonics with slower rolloff than tanh, giving a harsher, more aggressive character. Equivalent to transistor clipping stages.
- **foldback**: When the signal exceeds a fixed threshold of 0.8, it folds back toward zero rather than clipping. This creates both even and odd harmonics with a complex, sometimes unpredictable spectrum. The resulting buzzy, metallic timbres are useful for synth processing and experimental sound design.
- **asymmetric**: Different clipping curves for positive (`tanh(x)`) and negative (`tanh(1.5x) / 1.5 × 1.2`) half-cycles. Asymmetric nonlinearities generate even harmonics (2nd, 4th...) in addition to odd harmonics. Even harmonics add warmth and are characteristic of single-ended tube amplifier stages (Class A).

### Tone Filter

The tone control is a peaking EQ biquad (1 kHz, Q 0.7) placed after the waveshaper. The `tone` parameter in dB boosts or cuts around 1 kHz, reducing harsh high-frequency harmonics (negative values) or adding brightness (positive values). The control is needed because nonlinear processing can generate significant energy above the original signal's bandwidth.

**Stereo processing**: In stereo mode (`process_stereo`), each channel has its own independent tone filter state, so the two channels behave as dual mono. A single filter state shared between channels would cause cross-channel artifacts.

**ADAA**: All four waveshape modes use first-order Antiderivative Anti-Aliasing (ADAA, `sonido_core::Adaa1`), which suppresses aliasing from the waveshaper without the CPU cost of oversampling.

**Aliasing note**: For critical applications, wrap the distortion effect in `Oversampled<4, _>` (`sonido_core::Oversampled`) to suppress harmonic aliasing from the nonlinear waveshaper. At 48 kHz base rate, 4x oversampling processes at 192 kHz, keeping generated harmonics below the effective Nyquist.

### Example

```bash
sonido process in.wav --effect distortion \
    --param drive=15 --param tone=-3 --param shape=0
```

---

## compressor

Dynamics compressor with soft knee.

**Architecture** (`crates/sonido-effects/src/kernels/compressor.rs`): A feed-forward design with a sidechain highpass, a dual envelope follower, a soft-knee gain computer, makeup gain, and a parallel mix.

```
Input -> mid (L+R)/2 -> SC HPF -> Dual Envelope (fast + slow) -> Gain Computer -> Gain Reduction
                                                                       |
                                                       Makeup (manual or auto) -> Wet/Dry Mix -> Soft Limit -> Output
```

The gain computer (`compute_gain_db()`) implements a soft-knee transfer curve. Below the knee region, no gain reduction occurs. Within the knee (default 6 dB wide), compression increases quadratically. This smooth transition avoids the audible "threshold artifact" of hard-knee compressors. Above the knee, full ratio compression applies.

**Program-dependent release** (`dual_envelope()`): two envelope followers run on the sidechain, a fast one with a fixed 50 ms release and a slow one with the user-set release. The larger of the two drives the gain computer, so new transients are caught even during a long release.

**Stereo linking**: The detector runs on the mid signal `(L+R)/2`, and identical gain reduction is applied to both channels. This preserves the stereo image. Independent per-channel compression would cause phantom center shifts when one channel is louder than the other. `process_stereo_with_sidechain()` replaces the internal detector input with an external key signal (bypassing the sidechain HPF).

**Auto makeup**: when enabled, makeup is `-threshold * (1 - 1/ratio) * 0.5` dB and the `makeup` parameter is ignored.

| Parameter | Description | Default | Range |
|-----------|-------------|---------|-------|
| `threshold` | Threshold in dB | -18.0 | -60 to 0 |
| `ratio` | Compression ratio | 4.0 | 1-20 |
| `attack` | Attack time in ms | 10.0 | 0.1-100 |
| `release` | Release time in ms | 100.0 | 10-1000 |
| `makeup` | Makeup gain in dB | 0.0 | 0-24 |
| `knee` | Knee width in dB | 6.0 | 0-12 |
| `detection` | Detection mode (0=Peak, 1=RMS) | 0 | 0-1 |
| `sc_hpf` | Sidechain HPF frequency in Hz (log scale) | 80.0 | 20-500 |
| `auto_makeup` | Auto makeup gain (0=Off, 1=On) | 0 | 0-1 |
| `output` | Output level in dB | 0.0 | -6 to 6 |
| `mix` | Wet/dry mix % (parallel compression) | 100.0 | 0-100 |
| `gain_reduction` | Current gain reduction in dB (read-only meter) | 0.0 | -60 to 0 |

### Tips

- **Fast attack** (1-5ms): Catches transients, can sound "squashed"
- **Slow attack** (20-50ms): Lets transients through, more natural
- **Fast release** (50-100ms): Pumping effect, good for drums
- **Slow release** (200-500ms): Smooth, transparent compression

### Example

```bash
sonido process in.wav --effect compressor \
    --param threshold=-20 --param ratio=4 --param attack=10 --param release=100
```

---

## chorus

Multi-voice modulated delay chorus with feedback and tempo sync.

**How chorus works**: A chorus effect creates the illusion of multiple instruments playing in unison by mixing the dry signal with copies that have slightly varying pitch. The pitch variation is achieved by modulating a short delay time with an LFO. When a delay time changes over time, it effectively time-stretches or compresses the signal, producing a Doppler-like pitch shift.

**Implementation** (`crates/sonido-effects/src/kernels/chorus.rs`): Four `InterpolatedDelay` lines, each with its own LFO, provide 2-4 modulated voices. The configurable base delay (5-25 ms, default 10 ms) sets the centre of modulation. Following the Boss Dimension D approach, the LFOs never lock together: voice 1 is a sine at the base rate, voice 2 a sine at the base rate offset by 90°, voice 3 a sine at rate x 0.73, and voice 4 a triangle at rate x 1.17. A feedback path recirculates each voice's output for resonant tones.

**Stereo processing**: Voice 1 is fed from the left input and voice 2 from the right; voices 3 and 4 (when enabled) take the mid signal. A pan matrix spreads them: with 2 voices, V1 is 80% L / 20% R and V2 the mirror; with 3, V1 left, V2 right, V3 centre; with 4, V1 hard left, V2 hard right, V3 centre-left, V4 centre-right. The wet mix then passes a soft limiter before the output level.

| Parameter | Description | Default | Range |
|-----------|-------------|---------|-------|
| `rate` | LFO rate in Hz | 0.8 | 0.1-10 |
| `depth` | Modulation depth % | 60.0 | 0-100 |
| `mix` | Wet/dry mix % | 50.0 | 0-100 |
| `voices` | Number of chorus voices | 2 | 2-4 |
| `feedback` | Feedback amount % | 0.0 | 0-70 |
| `base_delay` | Base delay in ms | 10.0 | 5-25 |
| `sync` | Tempo sync on/off | off | off, on |
| `division` | Note division (when synced) | eighth | whole, half, quarter, eighth, etc. |
| `output` | Output level in dB | 0.0 | -6 to 6 |

### Tempo Sync

When `sync=on`, the LFO rate locks to the host tempo at the selected note division. The rate parameter is overridden by the computed Hz value from the current BPM and division. Sync responds to `set_tempo_context()` calls from the host/DAW.

### Tips

- **Subtle chorus**: rate=0.5, depth=30, mix=30
- **Classic chorus**: rate=1.0, depth=50, mix=50
- **Thick chorus**: rate=2.0, depth=70, mix=60, voices=4

### Example

```bash
sonido process in.wav --effect chorus \
    --param rate=1.5 --param depth=60 --param mix=50
```

---

## delay

Feedback delay with optional ping-pong stereo mode.

**Architecture** (`crates/sonido-effects/src/kernels/delay.rs`): Two `InterpolatedDelay` lines (left/right, up to 2 s) whose feedback path runs through a one-pole lowpass (`feedback_lp`), a biquad highpass (`feedback_hp`), and an optional diffuser of two true Schroeder allpasses (13 ms and 7 ms; `AllpassFilter` in `crates/sonido-core/src/allpass.rs`, feedback = diffusion × 0.6) for smeared, tape-like repeats. The allpasses have unity gain at every frequency, so diffusion inside the feedback loop never raises the loop gain. The delay time uses `Slow` (20 ms) smoothing to avoid zipper noise and pitch clicks when changed during playback.

**Ping-pong mode**: When enabled, the feedback path crosses channels: the left delay line's output feeds back into the right delay line, and vice versa. This creates alternating left-right echoes that "bounce" across the stereo field. The effect reports `is_true_stereo() -> true` only when ping-pong is active.

**Gain compensation**: The wet signal is scaled by `(1-feedback)` to exactly cancel comb-filter peak gain at resonance. At fb=0.35 (default), compensation = 0.65; the wet signal at resonance equals the input. At high feedback the wet signal is quieter but the decay tail is longer. The calibrated level trim then sets the wet signal's loudness (see [Output Level Convention](#output-level-convention)), and the `output` parameter provides makeup gain if needed.

**Feedback stability**: The feedback parameter tops out at 95% to prevent runaway oscillation. At feedback=0.95, each echo is 95% of the previous, so the signal decays by ~0.45 dB per repeat. Complete decay below -60 dB takes about 135 repeats (fewer in practice, since the feedback filters also take energy out on every pass).

| Parameter | Description | Default | Range |
|-----------|-------------|---------|-------|
| `time` | Delay time in ms | 380.0 | 1-2000 |
| `feedback` | Feedback amount % | 35.0 | 0-95 |
| `mix` | Wet/dry mix % | 35.0 | 0-100 |
| `ping_pong` | Ping-pong stereo mode (0=off, 1=on) | 0 | 0-1 |
| `feedback_lp` | Feedback lowpass frequency in Hz | 6000.0 | 200-20000 |
| `feedback_hp` | Feedback highpass frequency in Hz | 80.0 | 20-2000 |
| `diffusion` | Diffusion amount % | 0.0 | 0-100 |
| `sync` | Tempo sync on/off | off | off, on |
| `division` | Note division (when synced) | quarter | whole, half, quarter, eighth, etc. |
| `output` | Output level in dB | 0.0 | -6 to 6 |

### Tips

- **Slapback**: time=80-120ms, feedback=20, mix=40
- **Quarter note** (120 BPM): time=500ms
- **Dotted eighth** (120 BPM): time=375ms
- **Long washes**: feedback > 90, where repeats take 65+ echoes to fall 60 dB (feedback is capped at 95%, so the delay never self-oscillates)

### Example

```bash
sonido process in.wav --effect delay \
    --param time=375 --param feedback=50 --param mix=40
```

---

## filter

Multimode RBJ biquad filter with LPF, HPF, BPF, and Notch modes, followed by a soft limiter and output level (`crates/sonido-effects/src/kernels/filter.rs`). Also available as `lowpass` (LPF is the default mode).

| Parameter | Description | Default | Range |
|-----------|-------------|---------|-------|
| `cutoff` | Cutoff/center frequency in Hz | 2200.0 | 20-20000 |
| `resonance` | Resonance (Q factor) | 3.0 | 0.1-20 |
| `output` | Output level in dB | 0.0 | -6 to 6 |
| `type` | Filter mode: 0=LPF, 1=HPF, 2=BPF, 3=Notch | 0 | 0-3 |

### Filter Types

- **LPF** (0): Low-pass. It attenuates frequencies above cutoff and is the standard tone-shaping filter.
- **HPF** (1): High-pass. It attenuates frequencies below cutoff and is useful for removing rumble and mud.
- **BPF** (2): Band-pass. It passes a band around the center frequency and attenuates above and below it. Q controls the width.
- **Notch** (3): Band-reject, the opposite of bandpass. It removes a narrow band at the center frequency.

### Tips

- **Q = 0.707**: Butterworth response, flattest passband (all modes)
- **Q > 1**: Resonant peak at cutoff (LPF/HPF) or narrower band (BPF/Notch)
- **Q > 5**: Strong resonant peak (the output soft limiter keeps it from clipping)

### Example

```bash
sonido process in.wav --effect filter \
    --param cutoff=2000 --param resonance=2.0 --param type=1
```

---

## vibrato

6-unit tape wow/flutter vibrato. Also available as `multivibrato`.

Simulates the complex pitch modulation of analog tape machines with 6 independent modulation units per channel (`crates/sonido-effects/src/kernels/vibrato.rs`), running at 0.13-4.7 Hz with 0.4-2.5 cent depths and averaged into one wet signal. Each unit reads a delay line around a 128-sample base delay, which the kernel reports as latency.

| Parameter | Description | Default | Range |
|-----------|-------------|---------|-------|
| `depth` | Overall depth % | 50.0 | 0-400 |
| `mix` | Wet/dry mix % | 100.0 | 0-100 |
| `output` | Output level in dB | 0.0 | -6 to 6 |

### Tips

- **Subtle**: depth=50 (default) for gentle movement
- **Medium**: depth=100 for a classic tape sound
- **Heavy**: depth=200 for an obvious warble
- **Extreme**: depth=400 for maximum wow/flutter

### Example

```bash
sonido process in.wav --effect vibrato --param depth=80
```

---

## tape

Tape machine model (`crates/sonido-effects/src/kernels/tape.rs`): wow/flutter delay modulation, hysteresis feedback, drive + bias into an asymmetric exponential ADAA waveshaper, saturation blend, head-bump peaking EQ, level-dependent and static HF rolloff, then a soft limiter and output level.

| Parameter | Description | Default | Range |
|-----------|-------------|---------|-------|
| `drive` | Drive amount in dB | 6.0 | 0-24 |
| `saturation` | Saturation amount % | 30.0 | 0-100 |
| `hf_rolloff` | HF rolloff frequency in Hz | 12000.0 | 1000-20000 |
| `bias` | Tape bias offset | 0.0 | -0.2 to 0.2 |
| `wow` | Wow amount (low-rate pitch modulation) | 0.3 | 0-1 |
| `flutter` | Flutter amount (high-rate pitch modulation) | 0.2 | 0-1 |
| `hysteresis` | Magnetic hysteresis feedback amount | 0.15 | 0-0.5 |
| `head_bump` | Head bump (low-frequency resonance) | 0.3 | 0-1 |
| `bump_freq` | Head bump center frequency in Hz | 80.0 | 40-200 |
| `output` | Output level in dB | -6.0 | -12 to 12 |

### Tips

- **Subtle warmth**: drive=3, saturation=20
- **Tape compression**: drive=6, saturation=30
- **Saturated tape**: drive=12, saturation=70
- **Dark tape**: hf_rolloff=4000 rolls off high frequencies earlier
- **Bias offset**: Small bias values add asymmetric harmonic content
- **Wow/flutter**: wow=0.3, flutter=0.2 for classic tape transport instability
- **Head bump**: head_bump=0.5, bump_freq=80 for a tape machine's low-frequency resonance

### Example

```bash
sonido process in.wav --effect tape \
    --param drive=6 --param saturation=30
```

---

## preamp

Clean preamp/gain stage (`crates/sonido-effects/src/kernels/preamp.rs`): boost-only linear gain, a soft clipper that engages only above +20 dB (10.0 linear) of headroom, a presence EQ, then a soft limiter and output level.

| Parameter | Description | Default | Range |
|-----------|-------------|---------|-------|
| `gain` | Input gain in dB | 15.0 | 0-40 |
| `tone` | Presence EQ in dB (peaking, 3 kHz, Q 0.8) | 0.0 | -12 to 12 |
| `output` | Output level in dB | 0.0 | -6 to 6 |

### Tips

Use before other effects to boost quiet signals, or after to control output level. The preamp has no mix control, so it applies its calibrated level trim itself, before its safety limiter: at the default 15 dB `gain` it plays at the input's loudness, and `gain` above or below 15 dB is a boost or cut relative to that (see [Output Level Convention](#output-level-convention)). The tone parameter adds bite (positive) or darkens the upper mids (negative) before downstream effects.

### Example

```bash
sonido process in.wav --effect preamp --param gain=6
```

---

## reverb

Algorithmic stereo reverb: an 8-line feedback delay network (FDN) with Hadamard mixing, tapped-delay early reflections, and modulated allpass diffusion.

**Topology** (`crates/sonido-effects/src/kernels/reverb.rs`):

```
Input L/R -> Pre-delay L/R -> mono sum -+-> ER tapped delay (14 taps) ------------------------> x er_level -+
                                        +-> 8-line FDN (Hadamard feedback, damping) -> 4 allpasses/ch ------+-> M/S width -> Mix -> Output
```

**FDN**: Eight modulated delay lines whose outputs are mixed by an 8x8 Hadamard matrix (fast Walsh-Hadamard butterfly scaled by 1/sqrt(8), so the mixing is energy-preserving) and fed back. The line lengths reuse Freeverb's comb tunings, specified at 44.1 kHz and scaled to the actual sample rate:

```
FDN lines: 1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617 samples (at 44.1 kHz)
```

Each line's read position is swept by its own sine LFO (0.3-1.3 Hz, 0.3 ms depth) to break up metallic resonances, and each feedback path has a one-pole lowpass. The left output averages the even-numbered lines and the right the odd-numbered lines, so the two channels decorrelate without artificial panning.

**Feedback** (`update_derived()`): Room size and decay (internally 0-1 from the user-facing 0-100%) set the FDN feedback with Freeverb's room constants:
```
scaled_room = 0.28 + room_size * 0.7    (range: 0.28 to 0.98)
feedback = scaled_room + decay * (0.98 - scaled_room)    (clamped to 0.99)
```
Damping maps logarithmically to the per-line lowpass cutoff: `200 * 100^(1 - damping)` Hz, i.e. 20 kHz at 0% (bright) down to 200 Hz at 100% (dark).

**Early reflections**: The mono pre-delayed signal feeds a tapped delay with 14 prime-spaced taps (131-1009 samples at 44.1 kHz). Tap spacing scales with room size by `0.5 + room_size * 1.5`, so the last reflection lands between about 11 ms (room 0%) and 46 ms (room 100%). Even taps go left, odd taps right, each weighted by `0.3 / sqrt(i+1)`.

**Diffusion**: The FDN output passes through four modulated allpasses per channel (feedback 0.6, 0.15 ms modulation depth), using Freeverb's allpass tunings: 556, 441, 341, 225 samples on the left, offset by +23 samples on the right (579, 464, 364, 248).

**Gain compensation:** The FDN output is scaled by `sqrt(1-feedback)`. This moderate compensation keeps the wet level roughly constant as decay grows; single-comb effects (delay, flanger) use the exact `(1-fb)` instead. See `docs/DSP_FUNDAMENTALS.md` (Feedback Resonance Compensation) and ADR-019. The calibrated level trim then sets the wet signal's loudness (see [Output Level Convention](#output-level-convention)).

**Stereo width**: A mid/side matrix scales the side signal of the wet mix. At width=0% both channels receive the mid (mono); at 100% the wet L/R pass unchanged.

| Parameter | Description | Default | Range |
|-----------|-------------|---------|-------|
| `room_size` | Room size % (sets early-reflection spacing and FDN feedback) | 60.0 | 0-100 |
| `decay` | Decay time % (reverb tail length) | 55.0 | 0-100 |
| `damping` | HF damping % (0=bright, 100=dark) | 45.0 | 0-100 |
| `predelay` | Pre-delay time in ms | 20.0 | 0-100 |
| `mix` | Wet/dry mix % | 35.0 | 0-100 |
| `width` | Stereo width % | 100.0 | 0-100 |
| `er_level` | Early reflections level % | 40.0 | 0-100 |
| `output` | Output level in dB | 0.0 | -6 to 6 |

### Tips

- **Small room**: room_size=30, decay=40, damping=60 for a tight, intimate sound
- **Medium room**: room_size=50, decay=60, damping=40 for a balanced, natural sound
- **Large hall**: room_size=80, decay=85, damping=25 for a spacious, epic sound
- **Dark reverb**: damping=70-90 for a muffled, vintage sound
- **Bright reverb**: damping=10-30 for a shimmery, modern sound
- **Early reflections**: er_level=70-100 for a more defined room shape; 0 = tail only
- **Pre-delay**: 10-30ms for clarity, keeps source separate from reverb
- **Latency**: Reverb reports 0 latency samples. Predelay is a musical parameter and does not count as processing latency (per CLAP/VST3 spec)
- **Tail**: Reverb reports `decay × 3 s` of tail (up to 3 s at decay=100%) so hosts keep processing after the input stops

### Example

```bash
# Room reverb
sonido process in.wav --effect reverb \
    --param room_size=50 --param decay=60 --param mix=40

# Large hall
sonido process in.wav --effect reverb \
    --param room_size=80 --param decay=85 --param er_level=70 --param mix=50

# Custom dark hall
sonido process in.wav --effect reverb \
    --param room_size=80 --param decay=90 --param damping=70 --param predelay=25
```

---

## tremolo

Amplitude modulation with multiple waveforms.

| Parameter | Description | Default | Range |
|-----------|-------------|---------|-------|
| `rate` | LFO rate in Hz | 5.0 | 0.5-20 |
| `depth` | Modulation depth % | 50.0 | 0-100 |
| `waveform` | Waveform type (0=Sine, 1=Triangle, 2=Square, 3=SampleHold) | 0 | 0-3 |
| `spread` | Stereo spread % | 0.0 | 0-100 |
| `sync` | Tempo sync on/off | off | off, on |
| `division` | Note division (when synced) | eighth | whole, half, quarter, eighth, etc. |
| `output` | Output level in dB | 0.0 | -6 to 6 |

### Waveform Types

- **sine**: Smooth, classic tremolo sound
- **triangle**: Slightly more aggressive with linear ramps
- **square**: Choppy, on/off effect (helicopter)
- **samplehold**: Random stepped levels, creates rhythmic variations

### Tips

- **Subtle tremolo**: rate=4, depth=30 for gentle pulsing
- **Classic tremolo**: rate=6, depth=50 for a vintage amp sound
- **Choppy tremolo**: rate=8, depth=80, waveform=2 for rhythmic gating

### Example

```bash
sonido process in.wav --effect tremolo \
    --param rate=6 --param depth=50 --param waveform=0
```

---

## gate

Noise gate with threshold and hold.

| Parameter | Description | Default | Range |
|-----------|-------------|---------|-------|
| `threshold` | Threshold in dB | -40.0 | -80 to 0 |
| `attack` | Attack time in ms | 5.0 | 0.1-50 |
| `release` | Release time in ms | 100.0 | 10-1000 |
| `hold` | Hold time in ms | 50.0 | 0-500 |
| `range` | Gate range (maximum attenuation) in dB | -80.0 | -80 to 0 |
| `hysteresis` | Hysteresis margin in dB | 3.0 | 0-12 |
| `sc_hpf` | Sidechain HPF frequency in Hz (log scale) | 80.0 | 20-500 |
| `output` | Output level in dB | 0.0 | -6 to 6 |

### Tips

- **Quiet signals**: Set threshold just above the noise floor
- **Fast attack** (0.5-2ms): Preserves transients
- **Hold time**: Prevents rapid gate flutter on sustained notes
- **Release**: Longer release (200-500ms) for smoother fade-out

### Example

```bash
sonido process in.wav --effect gate \
    --param threshold=-40 --param attack=1 --param release=100 --param hold=50
```

---

## flanger

Classic flanger with modulated short delay.

**How flanging works**: Flanging is a comb filtering effect created by mixing a signal with a short, time-varying delayed copy. The delay sweeps between ~1-10 ms, producing a series of notches in the frequency spectrum at multiples of 1/delay_time. As the delay changes, the notches sweep through the spectrum, creating the characteristic "jet" or "whoosh" sound.

**Implementation** (`crates/sonido-effects/src/kernels/flanger.rs`): Base delay of 5 ms with up to 5 ms of LFO modulation (total range 1-10 ms). The feedback path feeds the delayed output back into the delay input, which deepens the comb filter notches and creates a more resonant, metallic character. At high feedback values the comb filter rings strongly, producing pitched metallic tones. With `tzf` on, the dry path is delayed by the 5 ms sweep midpoint and the wet delay sweeps through it, giving the through-zero null of tape flanging. The kernel always reports that 5 ms (240 samples at 48 kHz) as latency.

**Gain compensation**: The wet signal is scaled by `(1-|feedback|)` to exactly cancel comb-filter peak gain at resonance frequencies (negative feedback is compensated by its magnitude). At fb=0.5 (default), compensation = 0.5; the wet signal at resonance equals the dry signal, so raising feedback adds resonance rather than level. The calibrated level trim then sets the wet signal's loudness (see [Output Level Convention](#output-level-convention)).

**Stereo**: The right channel LFO is phase-offset by 90 degrees from the left. This means the comb filter notches sweep at different times in each channel, creating a spatial motion effect. Each channel has its own delay line and feedback state.

| Parameter | Description | Default | Range |
|-----------|-------------|---------|-------|
| `rate` | LFO rate in Hz | 0.5 | 0.05-5 |
| `depth` | Modulation depth % | 50.0 | 0-100 |
| `feedback` | Feedback amount % (negative = inverted polarity) | 50.0 | -95 to 95 |
| `mix` | Wet/dry mix % | 50.0 | 0-100 |
| `tzf` | Through-zero flanging on/off | off | off, on |
| `sync` | Tempo sync on/off | off | off, on |
| `division` | Note division (when synced) | eighth | whole, half, quarter, eighth, etc. |
| `output` | Output level in dB | 0.0 | -6 to 6 |

### Flanger vs. Chorus vs. Phaser

All three are modulation effects, but they differ in mechanism:

| Effect | Delay Range | Creates | Characteristic |
|--------|-------------|---------|----------------|
| Flanger | 1-10 ms | Comb filter (evenly-spaced notches) | Metallic, jet sweep |
| Chorus | 5-25 ms (base) | Pitch detuning (Doppler) | Thickening, ensemble |
| Phaser | N/A (allpass) | Unevenly-spaced notches | Organic, swooshing |

### Tips

- **Subtle flanger**: rate=0.2, depth=30, feedback=30
- **Classic flanger**: rate=0.5, depth=50, feedback=50
- **Jet flanger**: rate=0.1, depth=80, feedback=80 for a slow, dramatic sweep
- **Metallic**: high feedback (70%+) creates resonant metallic tones

### Example

```bash
sonido process in.wav --effect flanger \
    --param rate=0.5 --param depth=60 --param feedback=50 --param mix=50
```

---

## phaser

Multi-stage allpass phaser with LFO modulation.

**How phasing works**: A phaser creates notches in the frequency spectrum by mixing the input with a phase-shifted copy of itself. Unlike flanging (which uses comb filters with evenly-spaced notches), phasing uses cascaded first-order allpass filters whose notch positions are unevenly spaced. This produces a more organic, less metallic sound.

**Allpass filter theory** (`crates/sonido-effects/src/kernels/phaser.rs`): Each first-order allpass filter shifts the phase of the signal by up to 180 degrees, with the transition centered at a specific frequency. The coefficient is computed as:
```
a = (tan(pi * fc / fs) - 1) / (tan(pi * fc / fs) + 1)
```
When the allpass-shifted signal is mixed with the original, a notch appears at the frequency where the phase difference equals 180 degrees. Each pair of allpass stages produces one notch, so 4 stages (the default) create 2 notches.

**Gain compensation**: The wet signal is scaled by `(1-feedback)` via `gain::feedback_wet_compensation()`. Although the allpass cascade has unity magnitude (unlike true comb filters), this conservative compensation keeps feedback resonance from raising the wet level. The calibrated level trim then sets the wet signal's loudness (see [Output Level Convention](#output-level-convention)).

**Frequency sweep**: The center frequencies use exponential mapping for a natural-sounding sweep: `freq = min_freq * (max_freq/min_freq)^(lfo * depth)`. This ensures equal time is spent per octave rather than per Hz, matching human pitch perception. The default sweep range is 300 Hz to 3000 Hz. Each stage is slightly offset by a factor of `1 + stage_index * 0.1`, spreading the notches for a richer effect. Allpass coefficients are recomputed every 32 samples, and the left and right channels run independent allpass chains driven by LFOs 90° apart.

| Parameter | Description | Default | Range |
|-----------|-------------|---------|-------|
| `rate` | LFO rate in Hz | 0.45 | 0.05-5 |
| `depth` | Frequency sweep range % | 70.0 | 0-100 |
| `stages` | Number of allpass stages (steps of 2) | 4 | 2-12 |
| `feedback` | Feedback/resonance % | 30.0 | 0-95 |
| `mix` | Wet/dry mix % | 50.0 | 0-100 |
| `min_freq` | Minimum sweep frequency in Hz | 300.0 | 20-2000 |
| `max_freq` | Maximum sweep frequency in Hz | 3000.0 | 200-20000 |
| `sync` | Tempo sync on/off | off | off, on |
| `division` | Note division (when synced) | eighth | whole, half, quarter, eighth, etc. |
| `output` | Output level in dB | 0.0 | -6 to 6 |

### Tips

- **Subtle phaser**: stages=4, depth=40, feedback=30
- **Classic phaser**: stages=6, depth=50, feedback=50
- **Deep phaser**: stages=8-12, depth=70, feedback=60
- **More stages**: Creates more notches, thicker effect
- **High feedback**: Resonant, almost filter-like

### Example

```bash
sonido process in.wav --effect phaser \
    --param rate=0.4 --param depth=60 --param stages=6 --param feedback=50
```

---

## wah

Auto-wah and manual wah with envelope follower. Also available as `autowah`.

**Architecture** (`crates/sonido-effects/src/kernels/wah.rs`): State Variable Filter in bandpass mode with an envelope follower for auto-wah. In auto mode, the envelope follower tracks input amplitude (5 ms attack, 50 ms release) and sweeps the SVF cutoff upward from the base frequency across a 200 to 2000 Hz range proportionally to the sensitivity setting. In manual mode, the frequency parameter directly sets the SVF cutoff.

```
Input → EnvelopeFollower (auto-wah) → Target Frequency → SVF Bandpass → Normalize → Mix (80/20 wet/dry) → Soft Limit → Output Level
```

**Stereo processing**: Dual-mono (`is_true_stereo: false`). Both channels share one envelope follower (linked stereo detection from mid signal) and use identical filter parameters applied to independent SVF states.

**Reference**: Pirkle, "Designing Audio Effect Plugins in C++", Chapter 19.

| Index | Parameter | Range | Default | Unit | Description |
|-------|-----------|-------|---------|------|-------------|
| 0 | `frequency` | 200–2000 | 800.0 | Hz | Base/center frequency (logarithmic scale) |
| 1 | `resonance` | 1–10 | 5.0 | — | SVF bandpass Q factor |
| 2 | `sensitivity` | 0–100 | 50.0 | % | Envelope sensitivity (auto mode sweep range) |
| 3 | `mode` | 0–1 | 0 | — | Wah mode (0=Auto, 1=Manual; stepped) |
| 4 | `output` | -6–6 | 0.0 | dB | Output level |

### Gain Normalization

The wah uses an SVF bandpass filter whose peak gain at the center frequency equals Q. Without compensation, high-resonance settings (Q=5-10) would produce +14 to +20 dB of gain at the center frequency. The implementation normalizes the bandpass output by dividing by Q, so the peak gain stays at unity whatever the resonance setting. The normalized signal is then blended 80% wet / 20% dry (fixed; the wah has no mix control), preserving the body of the original signal as real wah pedals do.

A unity-peak bandpass passes only a narrow band, so this signal is quieter than the input. Overall loudness is set by the level contract instead: the wah applies its calibrated level trim before its safety limiter, so at defaults it plays at the input's loudness (see [Output Level Convention](#output-level-convention)).

### Modes

- **auto**: Envelope follower tracks input level, playing dynamics control wah sweep
- **manual**: Frequency parameter directly controls wah position

### Tips

- **Classic auto-wah**: frequency=600, resonance=6, sensitivity=70
- **Subtle envelope**: sensitivity=30-50 for gentle sweep
- **Aggressive**: sensitivity=80-100 for full range sweep
- **High Q** (8-10): Classic narrow wah tone
- **Low Q** (2-4): Wider, smoother sweep

### Example

```bash
# Auto-wah
sonido process in.wav --effect wah \
    --param frequency=700 --param resonance=6 --param sensitivity=70

# Manual wah (fixed position)
sonido process in.wav --effect wah \
    --param frequency=1200 --param mode=1
```

---

## eq

3-band parametric equalizer. Also available as `parametriceq` or `peq`.

**Architecture** (`crates/sonido-effects/src/kernels/eq.rs`): Three cascaded peaking EQ biquad filters (low, mid, high), each using the RBJ Audio EQ Cookbook formula. Coefficients are recomputed at most every 32 samples (0.67 ms at 48 kHz) when parameter values change, preventing per-sample transcendental math while keeping sweeps audibly smooth. A soft limiter at threshold 1.0 prevents clipping from accumulated band boosts before the output level stage.

```
Input → Low Band Peaking EQ → Mid Band Peaking EQ → High Band Peaking EQ → Soft Limit (1.0) → Output Level
```

**Stereo processing**: Dual-mono (`is_true_stereo: false`). Both channels use identical biquad coefficients applied to independent filter states.

**Reference**: Robert Bristow-Johnson, "Cookbook formulae for audio EQ biquad filter coefficients", 1994.

| Index | Parameter | Range | Default | Unit | Description |
|-------|-----------|-------|---------|------|-------------|
| 0 | `low_freq` | 20–500 | 100.0 | Hz | Low band center frequency (logarithmic scale) |
| 1 | `low_gain` | -12–12 | 0.0 | dB | Low band peaking gain |
| 2 | `low_q` | 0.5–5.0 | 1.0 | — | Low band Q factor (bandwidth control) |
| 3 | `mid_freq` | 200–5000 | 1000.0 | Hz | Mid band center frequency (logarithmic scale) |
| 4 | `mid_gain` | -12–12 | 0.0 | dB | Mid band peaking gain |
| 5 | `mid_q` | 0.5–5.0 | 1.0 | — | Mid band Q factor (bandwidth control) |
| 6 | `high_freq` | 1000–15000 | 5000.0 | Hz | High band center frequency (logarithmic scale) |
| 7 | `high_gain` | -12–12 | 0.0 | dB | High band peaking gain |
| 8 | `high_q` | 0.5–5.0 | 1.0 | — | High band Q factor (bandwidth control) |
| 9 | `output` | -6–6 | 0.0 | dB | Output level |

### Tips

- **Wide Q** (0.5-1): Gentle, musical boosts/cuts
- **Narrow Q** (3-5): Surgical, precise adjustments
- **Bass boost**: low_freq=80, low_gain=4, low_q=0.7
- **Presence boost**: mid_freq=3000, mid_gain=3, mid_q=1.5
- **Air/brilliance**: high_freq=10000, high_gain=3, high_q=0.7
- **Mud cut**: mid_freq=300, mid_gain=-4, mid_q=1.5

### Short Parameter Names

For convenience, you can use abbreviated parameter names:
- `lf`, `lg`, `lq` for low band
- `mf`, `mg`, `mq` for mid band
- `hf`, `hg`, `hq` for high band

### Example

```bash
# Boost bass and highs (smiley face EQ)
sonido process in.wav --effect eq \
    --param low_freq=100 --param low_gain=4 \
    --param high_freq=8000 --param high_gain=3

# Cut mud, add presence
sonido process in.wav --effect eq \
    --param mf=300 --param mg=-4 --param mq=1.5 \
    --param hf=3000 --param hg=2
```

---

## limiter

Brickwall lookahead peak limiter with ceiling control (`crates/sonido-effects/src/kernels/limiter.rs`). A sliding-window peak detector on `max(|L|, |R|)` drives linked gain reduction with instant attack and exponential release; audio is read from the far end of the lookahead buffer, and the kernel reports the lookahead (240 samples at the 5 ms default, 48 kHz) as latency.

| Parameter | Description | Default | Range |
|-----------|-------------|---------|-------|
| `threshold` | Threshold in dB | -6.0 | -30 to 0 |
| `ceiling` | Output ceiling in dB | -0.3 | -30 to 0 |
| `release` | Release time in ms | 100.0 | 10-500 |
| `lookahead` | Lookahead time in ms | 5.0 | 0-10 |
| `output` | Output level in dB | 0.0 | -6 to 6 |

### Tips

- **Lookahead**: Higher values catch transients better but add latency
- **Ceiling**: Set to -0.3 dB for inter-sample peak safety
- **Release**: Faster release can cause pumping on sustained material
- **Use after compressor**: Limiter catches peaks the compressor misses

### Example

```bash
sonido process in.wav --effect limiter \
    --param threshold=-6 --param ceiling=-0.3 --param release=100
```

---

## bitcrusher

Lo-fi bit depth and sample rate reduction with jitter.

| Parameter | Description | Default | Range |
|-----------|-------------|---------|-------|
| `bits` | Bit depth | 8.0 | 2-16 |
| `downsample` | Downsample factor | 4.0 | 1-64 |
| `jitter` | Hold time randomization % | 0.0 | 0-100 |
| `mix` | Wet/dry mix % | 100.0 | 0-100 |
| `output` | Output level in dB | 0.0 | -6 to 6 |

### Tips

- **Low bit depth** (2-4): Extreme digital artifacts
- **Downsample**: Creates aliasing and staircase waveforms
- **Jitter**: Adds analog character to the digital degradation
- **Mix**: Blend with dry signal for parallel processing

### Example

```bash
sonido process in.wav --effect bitcrusher \
    --param bits=4 --param downsample=8 --param mix=80
```

---

## ringmod

Ring modulation with sine, triangle, and square carriers.

| Parameter | Description | Default | Range |
|-----------|-------------|---------|-------|
| `frequency` | Carrier frequency in Hz | 300.0 | 20-2000 |
| `depth` | Modulation depth % | 100.0 | 0-100 |
| `waveform` | Carrier waveform (0=Sine, 1=Triangle, 2=Square) | 0 | 0-2 |
| `mix` | Wet/dry mix % | 50.0 | 0-100 |
| `output` | Output level in dB | 0.0 | -6 to 6 |

### Tips

- **Low frequencies** (20-80 Hz): Tremolo-like amplitude modulation
- **Mid frequencies** (200-500 Hz): Classic metallic ring mod tone
- **High frequencies** (1000+ Hz): Bell-like inharmonic overtones
- **Depth < 100%**: Blends between tremolo and ring modulation
- **Square waveform**: Aggressive on/off gating character

### Example

```bash
sonido process in.wav --effect ringmod \
    --param frequency=440 --param depth=100 --param waveform=0 --param mix=50
```

---

## stage

Signal conditioning and stereo utility effect. Provides gain trimming, phase
inversion, channel routing, M/S stereo width, linear balance, LR4 bass mono
crossover, and Haas temporal stereo delay.

**Signal Flow:**

```text
Input L/R
  → Phase invert (±1)
  → Channel mode (normal / swap / mono L / mono R)
  → Input gain (dB, smoothed)
  → DC block (optional)
  → M/S width (0% mono → 100% normal → 200% wide)
  → Balance (-100% left → 0% center → +100% right)
  → Bass mono (LR4 crossover → mono-sum lows)
  → Haas delay (0–30 ms on one channel)
  → Output level (dB, smoothed)
  → Output L/R
```

Implementation: `crates/sonido-effects/src/kernels/stage.rs`

| Parameter | Description | Default | Range |
|-----------|-------------|---------|-------|
| `gain` | Input trim in dB | 0.0 | -40 to 12 |
| `width` | Stereo width % (0=mono, 100=normal, 200=wide) | 100.0 | 0-200 |
| `balance` | Stereo balance % (-100=left, 0=center, +100=right) | 0.0 | -100 to 100 |
| `phase_l` | Invert left channel polarity | Off | Off/On |
| `phase_r` | Invert right channel polarity | Off | Off/On |
| `channel` | Channel routing mode | Normal | Normal/Swap/Mono L/Mono R |
| `dc_block` | Enable DC blocking filter | Off | Off/On |
| `bass_mono` | Enable LR4 bass mono crossover | Off | Off/On |
| `bass_freq` | Bass mono crossover frequency in Hz | 120.0 | 20-500 |
| `haas` | Haas delay time in ms | 0.0 | 0-30 |
| `haas_side` | Which channel gets the Haas delay | L | L/R |
| `output` | Output level in dB | 0.0 | -6 to 6 |

### M/S Width

Mid-side encoding with adjustable side gain:

```text
mid  = (L + R) × 0.5
side = (L - R) × 0.5 × width
out_l = mid + side
out_r = mid - side
```

### Bass Mono

Linkwitz-Riley 4th-order crossover (two cascaded Butterworth biquads per
channel at Q = 1/√2). Low frequencies below the crossover are mono-summed;
highs remain stereo. Prevents sub-bass phase cancellation on playback systems.

### Haas Delay

A short delay (0 to 30 ms) on one channel creates a spatial impression without
altering frequency content. Named after Helmut Haas (1951).

### Tips

- **Width 0%**: Instant mono fold-down, useful before mono bass processing
- **Bass Mono + 120 Hz**: Standard crossover for club/festival playback
- **Haas 8 to 15 ms**: Natural-sounding stereo widening without M/S artifacts
- **Phase invert one channel**: Check for mono compatibility issues
- **Channel Swap**: Quick A/B for stereo image debugging

### Example

```bash
sonido process in.wav --effect stage \
    --param gain=3 --param width=150 --param bass_mono=1 --param bass_freq=120
```

---

## looper

Stereo looper with record, play, and overdub modes.

**Signal flow** (`crates/sonido-effects/src/kernels/looper.rs`):

```text
Input → [mode switch] → Record: write to LoopBuffer, pass dry
                       → Play: read from LoopBuffer, wet/dry mix
                       → Overdub: read + write (feedback blend), wet/dry mix
                       → Stop: pass dry
```

Uses `LoopBuffer` (`crates/sonido-core/src/loop_buffer.rs`) with a fixed 2,880,000-sample capacity per channel (60 s at 48 kHz). Mode transitions are detected per-sample via `params.mode` vs cached state. The looper is an uncalibrated utility (unity `Level`, see [Output Level Convention](#output-level-convention)): its wet signal is whatever was recorded, so its mix is a plain constant-power crossfade with no level trim.

**Parameters:**

| # | Name | Range | Default | Unit | Scale | Smoothing |
|---|------|-------|---------|------|-------|-----------|
| 0 | Mode | 0–3 | 0 (Stop) | — | Stepped | None |
| 1 | Feedback | 0–100 | 80.0 | % | Linear | Standard |
| 2 | Half Speed | 0–1 | 0 | — | Stepped | None |
| 3 | Reverse | 0–1 | 0 | — | Stepped | None |
| 4 | Mix | 0–100 | 50.0 | % | Linear | Standard |
| 5 | Output | −6–+6 | 0.0 | dB | Linear | Standard |

Mode values: 0=Stop, 1=Record, 2=Play, 3=Overdub.

**State transitions:**
- Any → Record: clear buffer, start writing
- Record → Play: freeze loop_end, start reading
- Record → Overdub: freeze loop_end, read+write simultaneously
- Record → Stop: freeze loop_end, no playback
- Play → Overdub: continue reading, also write (input + existing × feedback)
- Overdub → Play: stop writing, continue reading
- Any → Stop: halt, loop content preserved

**CLI:**

```bash
sonido process in.wav --effect looper --param mode=1 --param feedback=80 --param mix=100
```

---

## transient_shaper

Transient shaper with independent attack and sustain envelope control.

**Signal flow** (`crates/sonido-effects/src/kernels/transient_shaper.rs`):

```text
Input ──┬──────────────────────────────────── × gain ──┬── wet/dry mix ── × output
        │                                       ▲       │
        └─► × sensitivity ─► Fast EF (1ms/10ms)        └── (dry path)
                           └─► Slow EF (50ms/200ms)
```

Two envelope followers run in parallel on the sensitivity-scaled input. The fast follower tracks transients; the slow follower tracks the sustained body. Their normalized outputs drive a multiplicative gain modifier: `gain = 1.0 + (attack/100) × transient + (sustain/100) × sustain_env`. Gain is clamped ≥ 0 to prevent sign inversion.

**Parameters:**

| # | Name | Range | Default | Unit | Scale | Smoothing |
|---|------|-------|---------|------|-------|-----------|
| 0 | Attack | −100–+100 | 0.0 | % | Linear | Fast |
| 1 | Sustain | −100–+100 | 0.0 | % | Linear | Fast |
| 2 | Sensitivity | 0–100 | 50.0 | % | Linear | Standard |
| 3 | Mix | 0–100 | 50.0 | % | Linear | Standard |
| 4 | Output | −6–+6 | 0.0 | dB | Linear | Fast |

- **Attack +100%**: doubles transient peaks (snappier drums, percussive attack).
- **Attack −100%**: removes transients (softer, rounder onset).
- **Sustain +100%**: doubles sustained body level (fatter, more present).
- **Sustain −100%**: removes sustain (staccato, gated feel).
- **Sensitivity**: pre-gain for envelope detection. Higher = more responsive to quiet signals.

**CLI:**

```bash
sonido process in.wav --effect transient_shaper --param attack=50 --param sustain=-30
```

---

## amp

Amp simulator with a dual gain stage, an interactive tone stack, and sag.

**Signal flow** (`crates/sonido-effects/src/kernels/amp.rs`):

```text
Input → Preamp (soft clip ADAA) → Bright (+3dB @ 2kHz) → Tone Stack (bass/mid/treble)
      → Presence (4kHz shelf) → Sag envelope → Power Stage (asymmetric clip ADAA)
      → Master → Output
```

Models a valve guitar amplifier using `GainStage` (soft/asymmetric clip with ADAA anti-aliasing) and `ToneStack` (interactive bass/mid/treble). Sag compresses dynamics when gain is high, simulating power supply droop.

**Parameters:**

| # | Name | Range | Default | Unit | Scale | Smoothing |
|---|------|-------|---------|------|-------|-----------|
| 0 | Gain | 0–100 | 50.0 | % | Linear | Fast |
| 1 | Bass | 0–100 | 50.0 | % | Linear | Slow |
| 2 | Mid | 0–100 | 50.0 | % | Linear | Slow |
| 3 | Treble | 0–100 | 50.0 | % | Linear | Slow |
| 4 | Presence | 0–100 | 50.0 | % | Linear | Slow |
| 5 | Sag | 0–100 | 30.0 | % | Linear | Standard |
| 6 | Bright | 0–1 | 0 | — | Stepped | None |
| 7 | Master | −60–0 | −6.0 | dB | Linear | Fast |
| 8 | Output | −6–+6 | 0.0 | dB | Linear | Fast |

**CLI:**

```bash
sonido process in.wav --effect amp --param gain=70 --param bass=60 --param treble=40
```

---

## cabinet

Cabinet IR simulator using direct convolution with 3 factory impulse responses.

**Signal flow** (`crates/sonido-effects/src/kernels/cabinet.rs`):

```text
Input → IR Convolution (256-sample direct) → Low Cut HPF → Mix → Output
```

Simulates guitar speaker cabinet frequency response via short direct time-domain convolution. Three IRs, generated in `new()` as exponentially decaying filtered noise: Clean Combo, British Stack, Modern High-Gain.

**Parameters:**

| # | Name | Range | Default | Unit | Scale | Smoothing |
|---|------|-------|---------|------|-------|-----------|
| 0 | IR Select | 0–2 | 0 | — | Stepped | None |
| 1 | Mix | 0–100 | 50.0 | % | Linear | Standard |
| 2 | Low Cut | 20–500 | 80.0 | Hz | Logarithmic | Slow |
| 3 | Output | −6–+6 | 0.0 | dB | Linear | Fast |

IR labels: "Clean Combo", "British Stack", "Modern High-Gain".

**CLI:**

```bash
sonido process in.wav --effect cabinet --param ir_select=1 --param low_cut=100
```

---

## deesser

De-esser: wideband sibilance reduction via a sidechain HPF.

**Signal flow** (`crates/sonido-effects/src/kernels/deesser.rs`):

```text
Input → HPF @ freq → Envelope → Gain Computer (threshold/ratio) → clamped gain → × Output
```

Uses a wideband approach: a highpass sidechain detects sibilance energy, then gain reduction is applied to the full-band signal. Avoids the "lispy" artifact of frequency-selective de-essing.

**Parameters:**

| # | Name | Range | Default | Unit | Scale | Smoothing |
|---|------|-------|---------|------|-------|-----------|
| 0 | Freq | 2000–16000 | 6000.0 | Hz | Logarithmic | Slow |
| 1 | Threshold | −60–0 | −20.0 | dB | Linear | Standard |
| 2 | Ratio | 1–20 | 8.0 | ratio | Linear | Standard |
| 3 | Range | 0–24 | 12.0 | dB | Linear | Standard |
| 4 | Output | −6–+6 | 0.0 | dB | Linear | Fast |

**CLI:**

```bash
sonido process vocal.wav --effect deesser --param freq=7000 --param threshold=-18
```

---

## drone

Sympathetic resonance generator that produces harmonically related sustaining tones.

**Signal flow** (`crates/sonido-effects/src/kernels/drone.rs`):

```text
Input → Envelope Follower → gate tones → Zero-Crossing Detector
      → 3 Sine Oscillators (root, +7st, +12st) → detune wobble
      → Decay Envelope → add to dry → Output
```

Detects input amplitude and generates harmonically related sine tones (root, perfect fifth, octave) at a pitch estimated from rising zero-crossings (estimates outside 50 to 2000 Hz are ignored). Tones sustain independently after input stops, controlled by the `decay` time constant; the kernel reports `decay × 6.9 × sample_rate` samples (the time to fall 60 dB) as its tail, so hosts keep processing until the drone has died away.

**Parameters:**

| # | Name | Range | Default | Unit | Scale | Smoothing |
|---|------|-------|---------|------|-------|-----------|
| 0 | Root Mix | 0–100 | 30.0 | % | Linear | Standard |
| 1 | Fifth Mix | 0–100 | 20.0 | % | Linear | Standard |
| 2 | Octave Mix | 0–100 | 25.0 | % | Linear | Standard |
| 3 | Detune | 0–50 | 5.0 | cents | Linear | Slow |
| 4 | Decay | 0.1–10 | 3.0 | s | Linear | Slow |
| 5 | Output | −60–+6 | 0.0 | dB | Linear | Fast |

**CLI:**

```bash
sonido process guitar.wav --effect drone --param root_mix=40 --param decay=5
```

---

## glitch

Buffer manipulation: stutter, tape-stop, reverse, shuffle.

**Signal flow** (`crates/sonido-effects/src/kernels/glitch.rs`):

```text
Input → Circular Buffer → Mode Dispatch → Mix → Output
```

Captures input into a circular buffer and applies one of four destructive playback modes. Fires probabilistically at `rate` Hz, controlled by `probability`. Dry signal passes between trigger events.

**Parameters:**

| # | Name | Range | Default | Unit | Scale | Smoothing |
|---|------|-------|---------|------|-------|-----------|
| 0 | Mode | 0–3 | 0 | — | Stepped | None |
| 1 | Rate | 1–32 | 4.0 | Hz | Logarithmic | Slow |
| 2 | Depth | 0–100 | 50.0 | % | Linear | Standard |
| 3 | Probability | 0–100 | 50.0 | % | Linear | Standard |
| 4 | Mix | 0–100 | 50.0 | % | Linear | Standard |
| 5 | Output | −60–+6 | 0.0 | dB | Linear | Fast |

Mode labels: "Stutter", "TapeStop", "Reverse", "Shuffle".

**CLI:**

```bash
sonido process drums.wav --effect glitch --param mode=0 --param rate=8 --param probability=70
```

---

## harmonic_habitat

Pitch-aware modal reverb: an FDN tail colored by a resonant bank tuned to the input's pitch.

**Signal flow** (`crates/sonido-effects/src/kernels/harmonic_habitat.rs`):

```text
Input → Predelay L/R → mono sum ─┬→ 8-line FDN (Hadamard feedback, damping) → wet L/R
                                 └→ Zero-crossing pitch tracker → harmonic center
wet L/R → 6-band modal bank (center × Mode ratios) → M/S Width → Mix → Output → Soft Limit
```

The predelayed input is summed to mono and feeds an 8-line feedback delay network with Hadamard mixing, slowly modulated delay lengths, and a one-pole lowpass per line (cutoff 18 kHz at 0% damping down to 180 Hz at 100%). Room Size and Decay together set the feedback, capped at 0.985. A zero-crossing tracker estimates the input's fundamental (55 to 1200 Hz; 220 Hz before any input and whenever Tracking is 0). Six peaking filters per channel are tuned to that center times the selected Mode's ratios and color the wet signal. Coloration strength is Harmonicity × Tracking, scaled from 25% up to 100% by the tracker's confidence, so with Tracking at 0 the modal bank is bypassed and the effect is a plain FDN reverb.

**Parameters:**

| # | Name | Range | Default | Unit | Scale | Smoothing |
|---|------|-------|---------|------|-------|-----------|
| 0 | Room Size | 0–100 | 55.0 | % | Linear | Slow |
| 1 | Decay | 0–100 | 60.0 | % | Linear | Slow |
| 2 | Damping | 0–100 | 45.0 | % | Linear | Slow |
| 3 | Pre-Delay | 0–100 | 18.0 | ms | Linear | Interpolated |
| 4 | Harmonicity | 0–100 | 45.0 | % | Linear | Standard |
| 5 | Tracking | 0–100 | 60.0 | % | Linear | Standard |
| 6 | Memory | 0–100 | 65.0 | % | Linear | Slow |
| 7 | Mode | 0–3 | 3 | — | Stepped | None |
| 8 | Stereo Width | 0–100 | 100.0 | % | Linear | Standard |
| 9 | Mix | 0–100 | 50.0 | % | Linear | Standard |
| 10 | Output | −6–+6 | 0.0 | dB | Linear | Fast |

Mode labels and modal ratios: "Neutral" (1, 1.5, 2, 2.25, 3, 4), "Major" (1, 5/4, 3/2, 2, 5/2, 3), "Minor" (1, 6/5, 3/2, 2, 12/5, 3), "Open Fifth" (1, 1.5, 2, 3, 4, 6).

- **Memory**: how slowly the harmonic center follows a new pitch (glide time constant from 35 ms at 0% to about 1.2 s at 100%).
- **Stereo Width**: M/S width of the wet signal. 0% = mono tail.

**CLI:**

```bash
sonido process guitar.wav --effect harmonic_habitat --param harmonicity=80 --param tracking=100 --param mode=1
```

---

## multiband_comp

Three-band dynamics with Linkwitz-Riley crossovers.

**Signal flow** (`crates/sonido-effects/src/kernels/multiband_comp.rs`):

```text
Input ─┬─ 2× LP @ low_xover ─────────────────────── [Low Comp]  → × low_gain  ─┐
       ├─ 2× HP @ low_xover → 2× LP @ high_xover ── [Mid Comp]  → × mid_gain  ─┼→ Sum → Output
       └─ 2× HP @ high_xover ────────────────────── [High Comp] → × high_gain ─┘
```

Splits the signal into low, mid, and high bands with Linkwitz-Riley crossovers, each split made of two cascaded Butterworth biquads (Q = 1/√2), i.e. a 4th-order LR slope. Each band is independently compressed (peak envelope, 10 ms attack, 100 ms release, hard knee) and scaled by its own gain before the bands are summed.

**Parameters:**

| # | Name | Range | Default | Unit | Scale | Smoothing |
|---|------|-------|---------|------|-------|-----------|
| 0 | Low Xover | 100–1000 | 250.0 | Hz | Logarithmic | Slow |
| 1 | High Xover | 1000–10000 | 3000.0 | Hz | Logarithmic | Slow |
| 2 | Low Thresh | −60–0 | −20.0 | dB | Linear | Standard |
| 3 | Low Ratio | 1–20 | 4.0 | ratio | Linear | Standard |
| 4 | Mid Thresh | −60–0 | −20.0 | dB | Linear | Standard |
| 5 | Mid Ratio | 1–20 | 4.0 | ratio | Linear | Standard |
| 6 | High Thresh | −60–0 | −20.0 | dB | Linear | Standard |
| 7 | High Ratio | 1–20 | 4.0 | ratio | Linear | Standard |
| 8 | Low Gain | −12–+12 | 0.0 | dB | Linear | Standard |
| 9 | Mid Gain | −12–+12 | 0.0 | dB | Linear | Standard |
| 10 | High Gain | −12–+12 | 0.0 | dB | Linear | Standard |
| 11 | Output | −6–+6 | 0.0 | dB | Linear | Fast |

**CLI:**

```bash
sonido process mix.wav --effect multiband_comp --param low_xover=200 --param high_xover=4000
```

---

## pitch_shift

Granular pitch shifter that crossfades overlapping Hann-windowed grains.

**Signal flow** (`crates/sonido-effects/src/kernels/pitch_shift.rs`):

```text
Input → Circular Delay Buffer → 2/4 Grain Readers (Hann window) → Mix → Output
```

Implements pitch shifting via overlapping grains reading from a circular buffer at modified speed. Pitch ratio = `2^(semitones/12 + cents/1200)`. Quality=0 uses 2 grains, Quality=1 uses 4 grains for smoother output.

**Parameters:**

| # | Name | Range | Default | Unit | Scale | Smoothing |
|---|------|-------|---------|------|-------|-----------|
| 0 | Semitones | −24–+24 | 0.0 | st | Linear | Interpolated |
| 1 | Cents | −50–+50 | 0.0 | cents | Linear | Interpolated |
| 2 | Grain Size | 10–50 | 20.0 | ms | Linear | Slow |
| 3 | Mix | 0–100 | 50.0 | % | Linear | Standard |
| 4 | Quality | 0–1 | 0 | — | Stepped | None |
| 5 | Output | −6–+6 | 0.0 | dB | Linear | Fast |

Quality labels: "Standard", "High".

**CLI:**

```bash
sonido process vocal.wav --effect pitch_shift --param semitones=-5 --param quality=1
```

---

## plate_reverb

Dattorro-inspired plate algorithm with input diffusion and modulated tank.

**Signal flow** (`crates/sonido-effects/src/kernels/plate_reverb.rs`):

```text
Input → mono sum → Bandwidth LP → Pre-delay → 4 Input Diffusers (allpass)
      → Tank: two cross-coupled halves, each [Mod Allpass → Delay → Damp], feedback × decay gain
      → Dattorro output taps: wet_L = tank0 − tank1, wet_R = tank1 − tank0 → Mix → Output
```

Models the classic EMT 140 plate reverb: highly diffuse, colorless tail with smooth decay and natural density. The input is summed to mono, band-limited by a one-pole `bandwidth` filter (200 Hz at 0% up to 20 kHz at 100%), pre-delayed, and smeared by four series input allpasses (feedback 0.1 to 0.85 from `diffusion`). The tank has two halves; each runs a slowly modulated allpass (feedback 0.7) into a delay line with one-pole damping (20 kHz at 0% `damping` down to 200 Hz at 100%), and each half's delay output feeds the other half's input. All allpasses are true Schroeder allpasses (`crates/sonido-core/src/allpass.rs`), unity gain at every frequency, so the decay gain alone sets the tank's loop gain (the damping filters only take away highs).

**Decay** is an RT60 in seconds: the cross-feedback gain per pass is `10^(-3 · loop_len / (decay · sample_rate))`, where `loop_len` is one tank half (the average of the two halves' delay plus allpass lengths), so the tail falls 60 dB in `decay` seconds at any `size` (gain capped at 0.995). `size` scales the tank delays from 0.5× (0%) to 2× (100%).

**Output taps**: Dattorro-style, each channel adds a tap from its own tank half and subtracts one from the other (at 44.1 kHz: L = half 0 at 533 samples − half 1 at 1257; R = half 1 at 889 − half 0 at 1711; scaled by 0.25), so the two channels share the tail's energy but not its waveform. The dry path stays stereo. Reports a tail of `decay × 3 × sample_rate` samples (6 s at the 2 s default), so hosts keep processing while the plate rings out.

**Parameters:**

| # | Name | Range | Default | Unit | Scale | Smoothing |
|---|------|-------|---------|------|-------|-----------|
| 0 | Decay | 0.1–10 | 2.0 | s | Linear | Slow |
| 1 | Damping | 0–100 | 50.0 | % | Linear | Slow |
| 2 | Pre-Delay | 0–100 | 20.0 | ms | Linear | Interpolated |
| 3 | Bandwidth | 0–100 | 80.0 | % | Linear | Slow |
| 4 | Diffusion | 0–100 | 70.0 | % | Linear | Standard |
| 5 | Size | 0–100 | 50.0 | % | Linear | Slow |
| 6 | Mix | 0–100 | 50.0 | % | Linear | Standard |
| 7 | Output | −6–+6 | 0.0 | dB | Linear | Fast |

**CLI:**

```bash
sonido process vocal.wav --effect plate_reverb --param decay=3 --param diffusion=80 --param mix=25
```

---

## shelving_eq

Low shelf + high shelf with output gain.

**Signal flow** (`crates/sonido-effects/src/kernels/shelving_eq.rs`):

```text
Input → Low Shelf (RBJ biquad) → High Shelf (RBJ biquad) → Output
```

Dual-mono topology with two RBJ Audio EQ Cookbook shelving filters. Low shelf boosts/cuts below `low_freq`, high shelf boosts/cuts above `high_freq`. Slope S=1.

**Parameters:**

| # | Name | Range | Default | Unit | Scale | Smoothing |
|---|------|-------|---------|------|-------|-----------|
| 0 | Low Freq | 20–500 | 100.0 | Hz | Logarithmic | Slow |
| 1 | Low Gain | −15–+15 | 0.0 | dB | Linear | Slow |
| 2 | High Freq | 1000–20000 | 8000.0 | Hz | Logarithmic | Slow |
| 3 | High Gain | −15–+15 | 0.0 | dB | Linear | Slow |
| 4 | Output | −6–+6 | 0.0 | dB | Linear | Fast |

**CLI:**

```bash
sonido process mix.wav --effect shelving_eq --param low_gain=3 --param high_gain=-2
```

---

## spring_reverb

Allpass dispersion chain that models the character of a physical spring reverb.

**Signal flow** (`crates/sonido-effects/src/kernels/spring_reverb.rs`):

```text
Input → mono sum → Drip (soft clip) → (+ feedback) → 6 Dispersion Allpasses → Soft Limit
      → One-Pole Damp → Feedback Delay ─┬→ wet → Mix (with mono dry) → Output (L = R)
                                        └→ × decay gain → back to the allpass input
```

Models the distinctive boingy, splashy character of a physical spring reverb unit. Six true Schroeder allpasses (feedback 0.7) in series disperse the signal; `tension` sets their lengths, from twice their base (0%, loose and boingy) to half of it (100%, tight and bright), and the decay keeps its RT60 as the loop length changes. The chain feeds a one-pole damping filter (20 kHz at 0% `damping` down to 200 Hz at 100%) and a feedback delay (4800 samples at 44.1 kHz, about 109 ms, at the 2 s default, scaled by `decay / 2`) whose output is the wet signal and, scaled by the decay gain, recirculates to the chain input. A soft limiter (ceiling 1.5) inside the loop prevents runaway.

**Decay** is an RT60 in seconds: the gain per trip around the loop is `10^(-3 · loop_len / (decay · sample_rate))` (capped at 0.99), where `loop_len` is the allpass chain plus the feedback delay, so the tail falls 60 dB in `decay` seconds.

The input is summed to mono and the output is mono: both channels carry the same dry + wet blend. Reports a tail of `decay × 3 × sample_rate` samples (6 s at the 2 s default). References: Välimäki et al., "Physics-Based Model of a Spring Reverb Unit", DAFx-06 (2006).

**Parameters:**

| # | Name | Range | Default | Unit | Scale | Smoothing |
|---|------|-------|---------|------|-------|-----------|
| 0 | Decay | 0.5–5 | 2.0 | s | Linear | Slow |
| 1 | Tension | 0–100 | 50.0 | % | Linear | Standard |
| 2 | Drip | 0–100 | 40.0 | % | Linear | Standard |
| 3 | Damping | 0–100 | 50.0 | % | Linear | Slow |
| 4 | Mix | 0–100 | 50.0 | % | Linear | Standard |
| 5 | Output | −6–+6 | 0.0 | dB | Linear | Fast |

**CLI:**

```bash
sonido process guitar.wav --effect spring_reverb --param tension=70 --param drip=50
```

---

## stereo_widener

M/S width control, Haas delay, and bass mono.

**Signal flow** (`crates/sonido-effects/src/kernels/stereo_widener.rs`):

```text
Input L/R → M/S Encode → Width Scale → M/S Decode
          → Haas Delay (right channel) → Bass Mono (LR4 crossover) → Output
```

Uses mid-side processing for width scaling, a short interpolated delay on the right channel for spatial depth (Haas effect), and an LR4 Linkwitz-Riley crossover to mono-sum low frequencies below the bass mono cutoff.

**Parameters:**

| # | Name | Range | Default | Unit | Scale | Smoothing |
|---|------|-------|---------|------|-------|-----------|
| 0 | Width | 0–200 | 100.0 | % | Linear | Standard |
| 1 | Haas Delay | 0–30 | 0.0 | ms | Linear | Interpolated |
| 2 | Bass Mono | 0–500 | 0.0 | Hz | Linear | Slow |
| 3 | Output | −6–+6 | 0.0 | dB | Linear | Fast |

- **Width 100%**: unchanged stereo field. **Width 0%**: mono. **Width 200%**: exaggerated stereo (sides doubled).
- **Bass Mono > 0**: mono-sums frequencies below the cutoff to prevent phase issues on large speakers.

**CLI:**

```bash
sonido process mix.wav --effect stereo_widener --param width=130 --param bass_mono=120
```

---

## texture

Granular ambient pad from input.

**Signal flow** (`crates/sonido-effects/src/kernels/texture.rs`):

```text
Input → Circular Capture Buffer → Up to 20 Grain Readers
        (random start, pitch variation, Hann envelope) → Sum → Mix → Output
```

Captures input into a circular buffer and plays back multiple overlapping grains with random start positions and pitch variation. The result is a smeared, ambient texture layer blended with the dry signal.

**Parameters:**

| # | Name | Range | Default | Unit | Scale | Smoothing |
|---|------|-------|---------|------|-------|-----------|
| 0 | Density | 1–20 | 8.0 | grains/s | Linear | Slow |
| 1 | Size | 50–500 | 200.0 | ms | Linear | Slow |
| 2 | Scatter | 0–100 | 50.0 | % | Linear | Standard |
| 3 | Pitch Var | 0–24 | 2.0 | st | Linear | Slow |
| 4 | Mix | 0–100 | 50.0 | % | Linear | Standard |
| 5 | Output | −60–+6 | 0.0 | dB | Linear | Fast |

**CLI:**

```bash
sonido process ambient.wav --effect texture --param density=12 --param size=300 --param pitch_var=5
```

---

## time_stretch

Independent pitch and time control via granular synthesis.

**Signal flow** (`crates/sonido-effects/src/kernels/time_stretch.rs`):

```text
Input → Circular Buffer → 1–8 Grain Readers (Hann windows; count set by Density)
        Write speed: time_ratio | Read speed: time_ratio × pitch_ratio → Output
```

Decouples time and pitch using independently controlled granular read heads. References: Zölzer, "DAFX: Digital Audio Effects" (2nd ed.), Chapter 7.

**Parameters:**

| # | Name | Range | Default | Unit | Scale | Smoothing |
|---|------|-------|---------|------|-------|-----------|
| 0 | Time Ratio | 0.25–4.0 | 1.0 | ratio | Linear | Interpolated |
| 1 | Pitch Ratio | 0.25–4.0 | 1.0 | ratio | Linear | Interpolated |
| 2 | Grain Size | 10–100 | 50.0 | ms | Linear | Slow |
| 3 | Density | 0–100 | 50.0 | % | Linear | Standard |
| 4 | Randomize | 0–100 | 20.0 | % | Linear | Standard |
| 5 | Freeze | 0–1 | 0 | — | Stepped | None |
| 6 | Sync | 0–1 | 0 | — | Stepped | None |
| 7 | Output | −60–+6 | 0.0 | dB | Linear | Fast |

- **Time Ratio < 1.0**: slower playback. **> 1.0**: faster.
- **Pitch Ratio**: independent of time. 0.5 = octave down, 2.0 = octave up.
- **Freeze**: holds the current buffer contents, stops writing.
- **Density**: number of overlapping grains, 1 at 0% up to 8 at 100%.
- **Sync**: snaps the grain length to the power-of-two beat subdivision (1/2, 1/4, 1/8, … of a beat) nearest Grain Size, at the host tempo (120 BPM until the host reports one).

**CLI:**

```bash
sonido process vocal.wav --effect time_stretch --param time_ratio=0.75 --param pitch_ratio=1.0
```

---

## tuner

Chromatic tuner using YIN pitch detection, with diagnostic outputs.

**Signal flow** (`crates/sonido-effects/src/kernels/tuner.rs`):

```text
Input → [mono sum → box-average decimation to ~24 kHz → 2048-sample ring]
      → YIN pitch detection every 1024 decimated samples → Detected Hz / Cents (READ_ONLY)
Input → Mute switch → Output
```

Mono-sums the input, box-averages it down to roughly 24 kHz, and keeps the latest 2048 decimated samples in a ring. YIN runs on that 2048-sample frame every 1024 new samples (50% hop, about 43 ms between updates) with a CMNDF threshold of 0.15 and parabolic interpolation, detecting fundamentals down to about 23 Hz. Detected frequency and cents deviation from the nearest note (relative to `reference`) are exposed as READ_ONLY parameters. Audio passes through with zero algorithmic latency. Reference: de Cheveigné & Kawahara, "YIN, a fundamental frequency estimator", JASA 111(4), 2002.

**Parameters:**

| # | Name | Range | Default | Unit | Scale | Smoothing | Flags |
|---|------|-------|---------|------|-------|-----------|-------|
| 0 | Reference | 415–465 | 440.0 | Hz | Logarithmic | None | — |
| 1 | Mute | 0–1 | 0 | — | Stepped | None | — |
| 2 | Output | −6–+6 | 0.0 | dB | Linear | Fast | — |
| 3 | Detected Hz | 0–5000 | 0.0 | Hz | Linear | None | HIDDEN, READ_ONLY |
| 4 | Cents | −50–+50 | 0.0 | cents | Linear | None | HIDDEN, READ_ONLY |

Mute labels: "Off", "On". Detected Hz and Cents are diagnostic readbacks, and no knob can write them.

**CLI:**

```bash
sonido process guitar.wav --effect tuner --param reference=442
```

---

## Effect Chains

### Chain Syntax

```
effect1:param1=value1,param2=value2|effect2:param=value
```

### Common Chains

**Guitar Crunch**
```bash
--chain "preamp:gain=6|distortion:drive=12,tone=-2"
```

**Tape Echo**
```bash
--chain "tape:drive=4,saturation=40|delay:time=350,feedback=45"
```

**Lush Chorus**
```bash
--chain "compressor:threshold=-18,ratio=3|chorus:rate=1,depth=50|delay:time=30,mix=20"
```

**Lo-Fi**
```bash
--chain "tape:drive=8,saturation=60|filter:cutoff=4000|vibrato:depth=80"
```

**Ambient Wash**
```bash
--chain "delay:time=400,feedback=50,mix=30|reverb:decay=90,room_size=80,mix=60"
```

**Guitar Hall**
```bash
--chain "distortion:drive=10|compressor:threshold=-18|reverb:room_size=80,decay=80,mix=40"
```

**Funk Wah**
```bash
--chain "compressor:threshold=-15,ratio=4|wah:sensitivity=80,resonance=7"
```

**Clean Chorus with EQ**
```bash
--chain "eq:lf=80,lg=3,hf=6000,hg=2|chorus:rate=1,depth=50|reverb:mix=30"
```

**Gated Tremolo**
```bash
--chain "gate:threshold=-45|tremolo:rate=6,depth=70,waveform=2"
```

**80s Phaser Lead**
```bash
--chain "distortion:drive=8|phaser:rate=0.3,stages=8,feedback=60|delay:time=350,feedback=40"
```

**Jet Flanger**
```bash
--chain "compressor:threshold=-20|flanger:rate=0.1,depth=90,feedback=80"
```
