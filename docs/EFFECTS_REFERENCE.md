# Effects Reference

Complete parameter reference for all Sonido effects.

## Kernel Architecture

All 20 effects have kernel-architecture implementations in `crates/sonido-effects/src/kernels/`. Each effect defines `XxxKernel` (`DspKernel` impl) and `XxxParams` (`KernelParams` impl). The registry creates `KernelAdapter<XxxKernel>` — all consumers (GUI, CLI, plugin) use kernel-backed effects transparently.

Classic `Effect` implementations have been removed as of v0.2. All effects are kernel-only.

```rust
// Desktop/plugin: KernelAdapter handles smoothing
let adapter = KernelAdapter::new(DistortionKernel::new(48000.0), 48000.0);

// Embedded: call kernel directly, no smoothing overhead
let mut kernel = DistortionKernel::new(48000.0);
let params = DistortionParams::from_knobs(adc_drive, adc_tone, adc_output, adc_shape, adc_mix, adc_dynamics);
let (l, r) = kernel.process_stereo(input_l, input_r, &params);
```

## Effect Aliases

Some effects have alternate names for convenience:

| Primary Name | Alias | Notes |
|-------------|-------|-------|
| `filter` | `lowpass` | Resonant lowpass filter |
| `vibrato` | `multivibrato` | 6-unit tape wow/flutter |
| `gate` | `noisegate` | Noise gate |
| `wah` | `autowah` | Auto-wah/manual wah |
| `eq` | `parametriceq`, `peq` | 3-band parametric EQ |

Both names work interchangeably in the CLI:

```bash
sonido process in.wav --effect filter --param cutoff=2000
sonido process in.wav --effect lowpass --param cutoff=2000  # Same effect
```

## Output Level Convention

All effects expose an `output` parameter as their last ParameterInfo index (range: -20 to +6 dB, default 0.0 dB). Two exceptions use narrower ranges: tape output (-12 to 12 dB, default -6.0 dB) and preamp gain (-40 to 12 dB, default 0.0 dB). Compressor exposes `makeup` (0 to 24 dB, default 0.0 dB) as a separate gain-staging param in addition to its standard `output`. See `docs/DSP_QUALITY_STANDARD.md` for the full quality rules and compliance table.

---

## distortion

Waveshaping distortion with multiple modes.

**Signal flow** (`crates/sonido-effects/src/kernels/distortion.rs`):
```
Input -> [Envelope] -> Drive (gain, dynamic) -> Waveshaper -> Tone Filter -> Output Level
```

The distortion applies a static nonlinear transfer function (waveshaper) to the input signal, preceded by a gain stage (drive) to push the signal into the nonlinear region. The `KernelAdapter` handles per-parameter smoothing via `SmoothingStyle` (Fast for drive, Standard for others).

| Parameter | Description | Default | Range |
|-----------|-------------|---------|-------|
| `drive` | Drive amount in dB | 8.0 | 0-40 |
| `tone` | Tone tilt in dB (boost/cut at 1 kHz) | 0.0 | -12 to 12 |
| `output` | Output level in dB | 0.0 | -20 to 20 |
| `shape` | Waveshape type (0=SoftClip, 1=HardClip, 2=Foldback, 3=Asymmetric) | 0 | 0-3 |
| `mix` | Wet/dry mix % | 100.0 | 0-100 |
| `dynamics` | Dynamic drive response (0=static, 100=fully responsive) | 0.0 | 0-100 |

### Waveshape Types and Their Harmonic Character

- **softclip** (`tanh`): Hyperbolic tangent soft clipping. Produces primarily odd harmonics (3rd, 5th, 7th...) with a smooth rolloff. The gentle compression curve means the signal transitions gradually into saturation, mimicking tube amplifier behavior. Odd harmonics are musically consonant (octave + fifth, etc.).
- **hardclip**: Flat clipping at +/-1.0. Produces a dense series of odd harmonics with slower rolloff than tanh, giving a harsher, more aggressive character. Equivalent to transistor clipping stages.
- **foldback**: When the signal exceeds a threshold (default 0.8), it folds back toward zero rather than clipping. This creates both even and odd harmonics with a complex, sometimes unpredictable spectral signature. The resulting timbres are buzzy and metallic -- useful for synth processing and experimental sound design.
- **asymmetric**: Different clipping curves for positive and negative half-cycles. Asymmetric nonlinearities generate even harmonics (2nd, 4th...) in addition to odd harmonics. Even harmonics add warmth and are characteristic of single-ended tube amplifier stages (Class A).

### Tone Filter

The tone control is a shelving tilt filter placed after the waveshaper. The `tone` parameter in dB boosts or cuts around 1 kHz, taming harsh high-frequency harmonics (negative values) or adding brightness (positive values). This is essential because nonlinear processing can generate significant energy above the original signal's bandwidth.

**Stereo processing**: In stereo mode (`process_stereo`), each channel has its own independent tone filter state. This ensures proper dual-mono behavior -- each channel's filtering history is independent, preventing cross-channel artifacts that would occur if a single filter state were shared between channels.

**ADAA**: All four waveshape modes use first-order Antiderivative Anti-Aliasing (ADAA), which reduces aliasing by approximately 6 dB per octave without the CPU cost of oversampling.

**Aliasing note**: For critical applications, wrap the distortion in `Oversampled<4, Distortion>` to suppress harmonic aliasing from the nonlinear waveshaper. At 48 kHz base rate, 4x oversampling processes at 192 kHz, keeping generated harmonics below the effective Nyquist.

### Example

```bash
sonido process in.wav --effect distortion \
    --param drive=15 --param tone=-3 --param shape=0
```

---

## compressor

Dynamics compressor with soft knee.

**Architecture** (`crates/sonido-effects/src/kernels/compressor.rs`): Feed-forward design with envelope follower, gain computer, and smoothed makeup gain.

```
Input -> Envelope Follower -> Gain Computer -> Gain Reduction -> Output
                                    |
                              Makeup Gain
```

The gain computer implements a soft-knee transfer curve (`compressor.rs:66-79`). Below the knee region, no gain reduction occurs. Within the knee (default 6 dB wide), compression increases quadratically -- this smooth transition avoids the audible "threshold artifact" of hard-knee compressors. Above the knee, full ratio compression applies.

**Stereo linking** (`compressor.rs:187-199`): In stereo mode, the envelope is derived from the sum of both channels `(L+R)/2`, and identical gain reduction is applied to both. This preserves the stereo image -- independent per-channel compression would cause phantom center shifts when one channel is louder than the other.

| Parameter | Description | Default | Range |
|-----------|-------------|---------|-------|
| `threshold` | Threshold in dB | -18.0 | -60 to 0 |
| `ratio` | Compression ratio | 4.0 | 1-20 |
| `attack` | Attack time in ms | 10.0 | 0.1-100 |
| `release` | Release time in ms | 100.0 | 10-1000 |
| `makeup` | Makeup gain in dB | 0.0 | 0-24 |
| `knee` | Knee width in dB | 6.0 | 0-12 |
| `detection` | Detection mode (0=Peak, 1=RMS) | 0 | 0-1 |
| `sidechain_freq` | Sidechain HPF frequency in Hz (log scale) | 80.0 | 20-500 |
| `auto_makeup` | Auto makeup gain (0=Off, 1=On) | 0 | 0-1 |
| `output` | Output level in dB | 0.0 | -20 to 20 |
| `mix` | Wet/dry mix % (parallel compression) | 100.0 | 0-100 |

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

**Implementation** (`crates/sonido-effects/src/kernels/chorus.rs`): Up to four `InterpolatedDelay` lines with independent LFOs provide 2-4 modulated voices. The configurable base delay (5-25 ms, default 15 ms) determines the centre of modulation. Each voice's LFO is phase-offset so the voices move independently, creating a richer effect. A feedback path recirculates the wet signal for resonant tones.

**Stereo processing**: In stereo mode, voices are panned across the stereo field with alternating bias (odd voices left, even voices right). This creates a wide stereo image from a mono source — a classic technique for thickening synth pads and guitar tracks.

| Parameter | Description | Default | Range |
|-----------|-------------|---------|-------|
| `rate` | LFO rate in Hz | 1.0 | 0.1-10 |
| `depth` | Modulation depth % | 50.0 | 0-100 |
| `mix` | Wet/dry mix % | 50.0 | 0-100 |
| `voices` | Number of chorus voices | 2 | 2-4 |
| `feedback` | Feedback amount % | 0.0 | 0-70 |
| `base_delay` | Base delay in ms | 15.0 | 5-25 |
| `sync` | Tempo sync on/off | off | off, on |
| `division` | Note division (when synced) | eighth | whole, half, quarter, eighth, etc. |
| `output` | Output level in dB | 0.0 | -20 to 20 |

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

**Architecture** (`crates/sonido-effects/src/kernels/delay.rs`): Two `InterpolatedDelay` lines (left/right) with feedback and smoothed parameter control. The delay time parameter uses 50 ms smoothing to prevent audible pitch artifacts when changing delay time during playback.

**Ping-pong mode** (`delay.rs:129-135`): When enabled, the feedback path crosses channels -- the left delay line's output feeds back into the right delay line, and vice versa. This creates alternating left-right echoes that "bounce" across the stereo field. The effect reports `is_true_stereo() -> true` only when ping-pong is active.

**Gain compensation**: The wet signal is scaled by `(1-feedback)` to exactly cancel comb-filter peak gain at resonance. At fb=0.4 (default), compensation = 0.6; the output at resonance equals the input. At high feedback the wet signal is quieter but the decay tail is longer. The `output` parameter provides makeup gain if needed.

**Feedback stability**: Feedback is clamped to 0.95 maximum to prevent runaway oscillation. At feedback=0.95, each echo is 95% of the previous, so the signal decays by ~0.45 dB per repeat. Complete decay below -60 dB takes approximately 130 repeats.

| Parameter | Description | Default | Range |
|-----------|-------------|---------|-------|
| `time` | Delay time in ms | 300.0 | 1-2000 |
| `feedback` | Feedback amount % | 40.0 | 0-95 |
| `mix` | Wet/dry mix % | 50.0 | 0-100 |
| `ping_pong` | Ping-pong stereo mode (0=off, 1=on) | 0 | 0-1 |
| `feedback_lp` | Feedback lowpass frequency in Hz | 20000.0 | 200-20000 |
| `feedback_hp` | Feedback highpass frequency in Hz | 20.0 | 20-2000 |
| `diffusion` | Diffusion amount % | 0.0 | 0-100 |
| `sync` | Tempo sync on/off | off | off, on |
| `division` | Note division (when synced) | quarter | whole, half, quarter, eighth, etc. |
| `output` | Output level in dB | 0.0 | -20 to 20 |

### Tips

- **Slapback**: time=80-120ms, feedback=20, mix=40
- **Quarter note** (120 BPM): time=500ms
- **Dotted eighth** (120 BPM): time=375ms
- **Self-oscillation**: feedback > 90 (be careful with volume!)

### Example

```bash
sonido process in.wav --effect delay \
    --param time=375 --param feedback=50 --param mix=40
```

---

## filter

Multimode biquad filter — LPF, HPF, BPF, and Notch. Also available as `lowpass` (default mode).

| Parameter | Description | Default | Range |
|-----------|-------------|---------|-------|
| `cutoff` | Cutoff/center frequency in Hz | 1000.0 | 20-20000 |
| `resonance` | Resonance (Q factor) | 0.707 | 0.1-20 |
| `output` | Output level in dB | 0.0 | -20 to 20 |
| `filter_type` | Filter mode: 0=LPF, 1=HPF, 2=BPF, 3=Notch | 0 | 0-3 |

### Filter Types

- **LPF** (0): Low-pass — attenuates frequencies above cutoff. Standard tone-shaping filter.
- **HPF** (1): High-pass — attenuates frequencies below cutoff. Useful for removing rumble and mud.
- **BPF** (2): Band-pass — passes a band around the center frequency, attenuates above and below. Width controlled by Q.
- **Notch** (3): Band-reject — removes a narrow band at the center frequency. Opposite of bandpass.

### Tips

- **Q = 0.707**: Butterworth response, flattest passband (all modes)
- **Q > 1**: Resonant peak at cutoff (LPF/HPF) or narrower band (BPF/Notch)
- **Q > 5**: Strong resonance, almost self-oscillating

### Example

```bash
sonido process in.wav --effect filter \
    --param cutoff=2000 --param resonance=2.0 --param filter_type=1
```

---

## vibrato

6-unit tape wow/flutter vibrato. Also available as `multivibrato`.

Simulates the complex pitch modulation of analog tape machines with 6 independent modulation units at different rates.

| Parameter | Description | Default | Range |
|-----------|-------------|---------|-------|
| `depth` | Overall depth % | 100.0 | 0-400 |
| `mix` | Wet/dry mix % | 100.0 | 0-100 |
| `output` | Output level in dB | 0.0 | -20 to 20 |

### Tips

- **Subtle**: depth=50 - Adds gentle movement
- **Medium**: depth=100 - Classic tape sound
- **Heavy**: depth=200 - Obvious warble effect
- **Extreme**: depth=400 - Maximum wow/flutter

### Example

```bash
sonido process in.wav --effect vibrato --param depth=80
```

---

## tape

Tape saturation with HF rolloff.

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
- **Dark tape**: hf_rolloff=4000 — rolls off high frequencies earlier
- **Bias offset**: Small bias values add asymmetric harmonic content
- **Wow/flutter**: wow=0.3, flutter=0.2 — classic tape transport instability
- **Head bump**: head_bump=0.5, bump_freq=80 — tape machine low-frequency resonance

### Example

```bash
sonido process in.wav --effect tape \
    --param drive=6 --param saturation=30
```

---

## preamp

Clean preamp/gain stage.

| Parameter | Description | Default | Range |
|-----------|-------------|---------|-------|
| `gain` | Input gain in dB | 0.0 | 0-40 |
| `tone` | Tone tilt in dB (boost/cut) | 0.0 | -12 to 12 |
| `output` | Output level in dB | 0.0 | -20 to 20 |

### Tips

Use before other effects to boost quiet signals, or after to control output level. The tone parameter provides a simple tilt EQ for shaping the signal before downstream effects.

### Example

```bash
sonido process in.wav --effect preamp --param gain=6
```

---

## reverb

Freeverb-style algorithmic reverb with 8 parallel comb filters and 4 series allpass filters.

**Freeverb topology** (`crates/sonido-effects/src/kernels/reverb.rs`): The Freeverb algorithm, originally by Jezar at Dreampoint, is one of the most widely used algorithmic reverb designs. The signal flow is:

```
Input -> Pre-delay -> [8 parallel comb filters] -> sum -> [4 series allpass filters] -> Output
```

The 8 comb filters run in parallel and their outputs are summed. Each comb filter has a different delay length, chosen to be mutually prime to avoid reinforcing resonances. The delay lengths are specified at 44.1 kHz (`reverb.rs:14`) and scaled to the actual sample rate:

```
Left:  1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617 samples (at 44.1 kHz)
Right: 1139, 1211, 1300, 1379, 1445, 1514, 1580, 1640 samples
```

The right channel uses slightly offset tunings (`reverb.rs:17`) for stereo decorrelation. This means the left and right reverb tails evolve independently, creating a wide stereo image without artificial panning.

The 4 series allpass filters provide diffusion -- they smear the distinct echoes from the comb filters into a dense, smooth reverb tail. The allpass feedback coefficient is fixed at 0.5 (`reverb.rs:139`).

**Comb filter feedback** (`reverb.rs:292-295`): The feedback coefficient combines room_size and decay parameters (internally normalized to 0-1 from the user-facing 0-100% range):
```
scaled_room = 0.28 + room_size * 0.7    (range: 0.28 to 0.98)
feedback = scaled_room + decay * (0.98 - scaled_room)
```
This mapping ensures the feedback stays below 1.0 (stable) while providing a wide range of decay times. The damping parameter controls a one-pole lowpass filter inside each comb, simulating the frequency-dependent absorption of real room surfaces -- higher damping means more high-frequency absorption per reflection, producing a darker reverb tail.

**Gain compensation:** The comb output is scaled by `sqrt(1-feedback)` — moderate compensation suited to the parallel-comb topology. The 1/8 averaging across 8 combs provides ~18 dB additional headroom, so the exact `(1-fb)` formula used by single-comb effects (delay, flanger) would be unnecessarily aggressive. See `docs/DSP_FUNDAMENTALS.md` (Feedback Resonance Compensation) and ADR-019.

**Stereo width** (`reverb.rs:396-402`): A mid/side matrix controls stereo width. At width=0%, both channels receive the average (mono). At width=100%, channels are fully independent.

| Parameter | Description | Default | Range |
|-----------|-------------|---------|-------|
| `room_size` | Room size % (affects early reflection density) | 50.0 | 0-100 |
| `decay` | Decay time % (reverb tail length) | 50.0 | 0-100 |
| `damping` | HF damping % (0=bright, 100=dark) | 50.0 | 0-100 |
| `predelay` | Pre-delay time in ms | 10.0 | 0-100 |
| `mix` | Wet/dry mix % | 50.0 | 0-100 |
| `width` | Stereo width % | 100.0 | 0-100 |
| `er_level` | Early reflections level % | 50.0 | 0-100 |
| `output` | Output level in dB | 0.0 | -20 to 20 |

### Tips

- **Small room**: room_size=30, decay=40, damping=60 - Tight, intimate sound
- **Medium room**: room_size=50, decay=60, damping=40 - Balanced, natural
- **Large hall**: room_size=80, decay=85, damping=25 - Spacious, epic
- **Dark reverb**: damping=70-90 - Muffled, vintage sound
- **Bright reverb**: damping=10-30 - Shimmery, modern sound
- **Early reflections**: er_level=70-100 — more defined room shape; 0 = tail only
- **Pre-delay**: 10-30ms for clarity, keeps source separate from reverb
- **Latency**: Reverb reports 0 latency samples. Predelay is a musical parameter, not processing latency (per CLAP/VST3 spec)

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
| `output` | Output level in dB | 0.0 | -20 to 20 |

### Waveform Types

- **sine**: Smooth, classic tremolo sound
- **triangle**: Slightly more aggressive with linear ramps
- **square**: Choppy, on/off effect (helicopter)
- **samplehold**: Random stepped levels, creates rhythmic variations

### Tips

- **Subtle tremolo**: rate=4, depth=30 - Gentle pulsing
- **Classic tremolo**: rate=6, depth=50 - Vintage amp sound
- **Choppy tremolo**: rate=8, depth=80, waveform=2 - Rhythmic gating

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
| `attack` | Attack time in ms | 1.0 | 0.1-50 |
| `release` | Release time in ms | 100.0 | 10-1000 |
| `hold` | Hold time in ms | 50.0 | 0-500 |
| `range` | Gate range (maximum attenuation) in dB | -80.0 | -80 to 0 |
| `hysteresis` | Hysteresis margin in dB | 3.0 | 0-12 |
| `sidechain_freq` | Sidechain HPF frequency in Hz | 80.0 | 20-500 |
| `output` | Output level in dB | 0.0 | -20 to 20 |

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

**Implementation** (`crates/sonido-effects/src/kernels/flanger.rs`): Base delay of 5 ms with up to 5 ms of LFO modulation (total range 1-10 ms). The feedback path feeds the delayed output back into the delay input, which deepens the comb filter notches and creates a more resonant, metallic character. At high feedback values, the comb filter approaches self-oscillation, producing pitched metallic tones.

**Gain compensation**: The wet signal is scaled by `(1-feedback)` to exactly cancel comb-filter peak gain at resonance frequencies. At fb=0.5 (default), compensation = 0.5; the wet signal at resonance equals the dry signal. This keeps the flanger within the -1 dBFS peak ceiling at all feedback settings.

**Stereo** (`flanger.rs:178-220`): The right channel LFO is phase-offset by 90 degrees from the left. This means the comb filter notches sweep at different times in each channel, creating a spatial motion effect. Each channel has its own delay line and feedback state.

| Parameter | Description | Default | Range |
|-----------|-------------|---------|-------|
| `rate` | LFO rate in Hz | 0.5 | 0.05-5 |
| `depth` | Modulation depth % | 35.0 | 0-100 |
| `feedback` | Feedback amount % (negative = inverted polarity) | 50.0 | -95 to 95 |
| `mix` | Wet/dry mix % | 50.0 | 0-100 |
| `tzf` | Through-zero flanging on/off | off | off, on |
| `sync` | Tempo sync on/off | off | off, on |
| `division` | Note division (when synced) | eighth | whole, half, quarter, eighth, etc. |
| `output` | Output level in dB | 0.0 | -20 to 20 |

### Flanger vs. Chorus vs. Phaser

All three are modulation effects, but they differ in mechanism:

| Effect | Delay Range | Creates | Characteristic |
|--------|-------------|---------|----------------|
| Flanger | 1-10 ms | Comb filter (evenly-spaced notches) | Metallic, jet sweep |
| Chorus | 10-25 ms | Pitch detuning (Doppler) | Thickening, ensemble |
| Phaser | N/A (allpass) | Unevenly-spaced notches | Organic, swooshing |

### Tips

- **Subtle flanger**: rate=0.2, depth=30, feedback=30
- **Classic flanger**: rate=0.5, depth=50, feedback=50
- **Jet flanger**: rate=0.1, depth=80, feedback=80 - Slow, dramatic sweep
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

**Allpass filter theory** (`crates/sonido-effects/src/kernels/phaser.rs:112-135`): Each first-order allpass filter shifts the phase of the signal by up to 180 degrees, with the transition centered at a specific frequency. The coefficient is computed as:
```
a = (tan(pi * fc / fs) - 1) / (tan(pi * fc / fs) + 1)
```
When the allpass-shifted signal is mixed with the original, a notch appears at the frequency where the phase difference equals 180 degrees. Each pair of allpass stages produces one notch, so 6 stages (the default) creates 3 notches.

**Gain compensation**: The wet signal is scaled by `(1-feedback)` via `gain::feedback_wet_compensation()`. Although the allpass cascade has unity magnitude (unlike true comb filters), this conservative compensation keeps the phaser well within the -1 dBFS peak ceiling at all feedback settings.

**Frequency sweep** (`phaser.rs:242-245`): The center frequencies use exponential mapping for a natural-sounding sweep: `freq = min_freq * (max_freq/min_freq)^(lfo * depth)`. This ensures equal time is spent per octave rather than per Hz, matching human pitch perception. The default sweep range is 200 Hz to 4000 Hz. Each stage is slightly offset by a factor of `1 + stage_index * 0.1`, spreading the notches for a richer effect.

| Parameter | Description | Default | Range |
|-----------|-------------|---------|-------|
| `rate` | LFO rate in Hz | 0.3 | 0.05-5 |
| `depth` | Frequency sweep range % | 50.0 | 0-100 |
| `stages` | Number of allpass stages | 6 | 2-12 |
| `feedback` | Feedback/resonance % | 50.0 | 0-95 |
| `mix` | Wet/dry mix % | 50.0 | 0-100 |
| `min_freq` | Minimum sweep frequency in Hz | 200.0 | 20-2000 |
| `max_freq` | Maximum sweep frequency in Hz | 4000.0 | 200-20000 |
| `sync` | Tempo sync on/off | off | off, on |
| `division` | Note division (when synced) | eighth | whole, half, quarter, eighth, etc. |
| `output` | Output level in dB | 0.0 | -20 to 20 |

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

**Architecture** (`crates/sonido-effects/src/kernels/wah.rs`): State Variable Filter in bandpass mode with an envelope follower for auto-wah. In auto mode, the envelope follower tracks input amplitude (5 ms attack, 50 ms release) and sweeps the SVF cutoff upward from the base frequency across a 200–2000 Hz range proportionally to the sensitivity setting. In manual mode, the frequency parameter directly sets the SVF cutoff.

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
| 4 | `output` | -20–20 | 0.0 | dB | Output level |

### Gain Normalization

The wah uses an SVF bandpass filter whose peak gain at the center frequency equals Q. Without compensation, high-resonance settings (Q=5-10) would produce +14 to +20 dB of gain at the center frequency. The implementation normalizes the bandpass output by dividing by Q (`wah.rs:202`), ensuring unity peak gain regardless of the resonance setting. The normalized signal is then blended 80% wet / 20% dry, preserving the body of the original signal as real wah pedals do.

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
| 9 | `output` | -20–20 | 0.0 | dB | Output level |

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
    --param mf=3000 --param mg=2
```

---

## limiter

Brickwall lookahead peak limiter with ceiling control.

| Parameter | Description | Default | Range |
|-----------|-------------|---------|-------|
| `threshold` | Threshold in dB | -6.0 | -30 to 0 |
| `ceiling` | Output ceiling in dB | -0.3 | -30 to 0 |
| `release` | Release time in ms | 100.0 | 10-500 |
| `lookahead` | Lookahead time in ms | 5.0 | 0-10 |
| `output` | Output level in dB | 0.0 | -20 to 20 |

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
| `downsample` | Downsample factor | 1.0 | 1-64 |
| `jitter` | Hold time randomization % | 0.0 | 0-100 |
| `mix` | Wet/dry mix % | 100.0 | 0-100 |
| `output` | Output level in dB | 0.0 | -20 to 20 |

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
| `frequency` | Carrier frequency in Hz | 220.0 | 20-2000 |
| `depth` | Modulation depth % | 100.0 | 0-100 |
| `waveform` | Carrier waveform (0=Sine, 1=Triangle, 2=Square) | 0 | 0-2 |
| `mix` | Wet/dry mix % | 50.0 | 0-100 |
| `output` | Output level in dB | 0.0 | -20 to 20 |

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
| `haas_side` | Which channel gets the Haas delay | Right | Left/Right |
| `output` | Output level in dB | 0.0 | -20 to 20 |

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

A short delay (0–30 ms) on one channel creates a spatial impression without
altering frequency content. Named after Helmut Haas (1951).

### Tips

- **Width 0%**: Instant mono fold-down — useful before mono bass processing
- **Bass Mono + 120 Hz**: Standard crossover for club/festival playback
- **Haas 8–15 ms**: Natural-sounding stereo widening without M/S artifacts
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

Uses `LoopBuffer` (`crates/sonido-core/src/loop_buffer.rs`) for 60s stereo recording at 48kHz. Mode transitions are detected per-sample via `params.mode` vs cached state.

**Parameters:**

| # | Name | Range | Default | Unit | Scale | Smoothing |
|---|------|-------|---------|------|-------|-----------|
| 0 | Mode | 0–3 | 0 (Stop) | — | Stepped | None |
| 1 | Feedback | 0–100 | 80.0 | % | Linear | Standard |
| 2 | Half Speed | 0–1 | 0 | — | Stepped | None |
| 3 | Reverse | 0–1 | 0 | — | Stepped | None |
| 4 | Mix | 0–100 | 100.0 | % | Linear | Standard |
| 5 | Output | −20–6 | 0.0 | dB | Linear | Standard |

Mode values: 0=Stop, 1=Record, 2=Play, 3=Overdub.

**State transitions:**
- Stop → Record: clear buffer, start writing
- Record → Play: freeze loop_end, start reading
- Record → Overdub: freeze loop_end, read+write simultaneously
- Play → Overdub: continue reading, also write (input + existing × feedback)
- Overdub → Play: stop writing, continue reading
- Any → Stop: halt, loop content preserved

**CLI:**

```bash
sonido process in.wav --effect looper --param mode=1 --param feedback=80 --param mix=100
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
