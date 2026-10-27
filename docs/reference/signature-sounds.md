# Signature Sounds

Brainstorming document for sounds and interactions that differentiate Sonido from
textbook DSP implementations. The goal is effects that make players ask "what IS that?"

---

## The Problem

Sonido has 36 production effects, all well implemented with proper anti-aliasing,
parameter smoothing, golden regression tests, and embedded deployment paths. They
sound correct, but they don't sound distinctive.

Players do not buy a pedal for a correct textbook implementation. The DOD
Rubberneck succeeded because of creative decisions, not because its BBD delay was
technically superior. Its regen knob interaction lets players use self-oscillation
as a feature, and its rubberneck momentary switch bends pitch in real time. The Eventide
H9 is popular because scene morphing turns parameter space into a performance
instrument, and not because of its reverb algorithm.

Sonido has the infrastructure (kernel architecture, DAG routing, preset morphing,
analysis toolkit, synth engine) but hasn't yet used it for sounds that can't be
made any other way.

---

## Design Principles

These principles also inform the [Design Philosophy section of the Roadmap](../ROADMAP.md#design-philosophy).

### Musicality Over Correctness (The Tom Cram Principle)

DigiTech's best products (Whammy, Space Station, DOD reissues) succeeded because
R&D put feel ahead of spec sheets. Tom Cram's team evaluated effects by playing
through them, not by measuring THD. A waveshaper that cleans up when you roll back
the guitar volume is better than one with 0.001% lower distortion. This is the
"volume knob cleanup" test: does the effect respond to playing dynamics the way a
great amp does?

**Implications for Sonido:** Every effect should be evaluated by playing through it
with a real guitar signal (or the built-in signal generator). Golden file tests
ensure correctness; the creative test is whether the effect makes you want to keep
playing.

### Dynamic Response

The best analog circuits change character with input level: their tonal quality,
harmonic content, and feel change along with their volume. A tube amp's transfer
function changes continuously from clean headroom through soft saturation to hard
clipping. Digital effects should use this: envelope followers modulating
waveshaper curves, input dynamics driving effect depth, compression whose gain
moves audibly with the playing.

### Real-Time Expression

A player's physical gestures are part of the sound. Parameters should respond to
gestures such as foot sweeps, knob combinations, and playing dynamics in ways that
create musical results. A wah pedal is the canonical example: the effect comes from
sweeping the filter, not from any one filter setting.

### Unexpected Interaction

The most interesting sounds come from parameters that interact nonlinearly: chorus
depth affecting the feedback path, delay time modulated by the input envelope, or
two controls that create a "sweet zone" where they interact constructively. Design
for these interactions rather than for isolated parameters.

### "What IS That?" Moments

These are sounds that don't fit an existing effect category such as "a delay with
modulation", and that make a player stop and explore. They come from combining
domains that are usually kept apart: synthesis + analysis, spectral + temporal,
adaptive + generative.

### Exploit Digital

Digital processing can do things analog circuits can't: negative delay
(lookahead), infinite freeze without noise buildup, topology changes
mid-performance, spectral manipulation, adaptive learning, and parameter-space
trajectories through preset morphing. None of these has an analog equivalent, so
signature sounds should be built on them.

---

## Candidate Ideas

### 0. Dynamic Waveshaper Response

**Concept:** Distortion that changes character with playing dynamics. Beyond gain
scaling, the waveshaping transfer function itself changes continuously with input
level. Roll back the guitar volume and the clipping softens from hard clip to
gentle saturation. Pick hard and the harmonic content shifts from even to odd
harmonics. This is the "volume knob cleanup" that defines every great tube amp
and the best transistor circuits (DOD 250, Klon Centaur, TS808).

**Existing infrastructure:**
- `DistortionKernel` with 4 waveshaper modes (soft clip, hard clip, foldback, asymmetric)
- `DistortionKernel`'s `dynamics` parameter (0-100%, default 0): an `EnvelopeFollower` (5 ms / 50 ms) reduces drive by up to 80% on quiet input, a first step toward cleanup
- `EnvelopeFollower` for input level tracking
- `KernelParams` for per-sample parameter access
- ADAA anti-aliasing already in the distortion hot path

**What's novel:** Most digital distortions select a fixed transfer function and
apply it at all levels. Dynamic waveshaping blends between transfer functions based
on instantaneous input envelope. The blend coefficient becomes the "feel" parameter:
it sets how strongly the character shifts with dynamics. At maximum sensitivity, you
get tube-like cleanup; at zero, you get the traditional fixed waveshaper.

**Implementation sketch:**
- `DynamicWaveshaper` utility: takes envelope value + blend curve → waveshaper mix coefficient
- Modify `DistortionKernel` to blend between soft-clip and current mode based on envelope
- Extend the existing `dynamics` param (today it scales drive only, not the transfer-function blend); new `response` param (envelope attack/release)
- Envelope follower per-sample, not per-block (responsive to individual pick attacks)
- Optional: asymmetric response (fast attack for pick transient, slow release for sustain)

**Why this is #0:** This is the single most impactful change Sonido can make. It
takes distortion from "technically correct" to "feels like an amp." Every guitarist
evaluates distortion by rolling back the volume knob: if the sound doesn't clean
up, it sounds digital, and if it does, it feels like an amp.

**References:**
- David Yeh, "Digital Implementation of Musical Distortion Circuits" (DAFx-2008): dynamic waveshaping models
- Tom Cram (DigiTech/DOD): "volume knob cleanup" as the primary design target for digital distortion
- Klon Centaur circuit analysis: level-dependent germanium diode clipping crossover

---

### 1. Scene Morphing

**Concept:** An expression pedal sweeps through parameter-space trajectories:
paths through multi-dimensional preset space with curves, waypoints, and hold zones.

**Existing infrastructure:**
- `KernelParams::lerp()` already interpolates between any two parameter snapshots
- `MorphSpace` (1D/2D snapshot interpolation with per-param `MorphCurve`), `ChainMorph`, and `curve_lerp` in `sonido_core::kernel`
- `from_normalized()` / `to_normalized()` for range mapping
- Expression pedal input: `ExpressionInput` in `crates/sonido-daisy/src/expression.rs` (smoothing, deadzone, slew limiting)

**What's novel:** Most morphing is linear A-to-B. Scene morphing with waypoints
creates curved paths through parameter space. A single expression pedal sweep could
move through 5 preset snapshots along a Catmull-Rom spline, creating tonal
transitions that can't be achieved by tweaking individual knobs.

**Implementation sketch:**
- `MorphTrajectory` struct: ordered list of `KernelParams` snapshots + curve type
- Expression pedal position (0.0-1.0) maps to trajectory position
- Per-parameter curve override (some params jump at waypoints, others glide)
- Stepped params snap at midpoints between waypoints (already in `lerp()`)

**References:**
- Eventide H9 scene morphing (2 scenes, linear)
- Chase Bliss Automatone MKII (motorized faders, 4 presets)
- Empress Zoia (multi-preset morph via CV)

---

### 2. Living Topology

**Concept:** Rewiring the DAG graph as a creative instrument. A footswitch or
expression pedal changes the routing in real time: parallel becomes serial, feedback
loops open and close, and the signal splits and recombines at different points.

**Existing infrastructure:**
- `ProcessingGraph` supports atomic schedule swap with ~5ms crossfade (click-free)
- `GraphEngine::new_dag()` for arbitrary topologies
- `GraphCommand::ReplaceTopology` (in `sonido-gui`'s `chain_manager.rs`) for atomic swap
- `sonido-graph-dsl` for topology description

**What's novel:** No analog pedal can rewire its circuit in real time, but a digital
one can. A single footswitch press could change the routing from:
```
guitar -> distortion -> delay -> reverb -> out
```
to:
```
guitar -> [distortion | reverb] -> delay -> out
         (parallel, 50/50 mix)
```
with a click-free crossfade. The change of topology is the effect the listener hears.

**Implementation sketch:**
- `TopologySet`: ordered list of compiled `ProcessingGraph` schedules
- Footswitch cycles through topologies (or expression pedal crossfades)
- Schedule crossfade already works (~5ms, `SmoothedParam`)
- DSL console already parses topology strings; wire it up to the footswitch bank

**References:**
- Empress Zoia (modular routing, but menu-driven not real-time)
- MOD Dwarf (pedalboard routing, but not mid-performance switching)
- Red Panda Tensor (context-dependent routing changes)

---

### 3. Cross-Domain DSP

**Concept:** Repurpose biosignal analysis algorithms for guitar effects. The
`sonido-analysis` crate has PAC (Phase-Amplitude Coupling), adaptive filters
(LMS/NLMS), cross-correlation, Hilbert transform, and spectral analysis. These
were built for EEG/biosignal work but apply directly to audio.

**Existing infrastructure:**
- PAC / comodulogram (`sonido-analysis::cfc`)
- LMS / NLMS adaptive filters (`sonido-analysis::lms`)
- Hilbert transform for instantaneous phase/amplitude (`sonido-analysis::hilbert`)
- Cross-correlation (`sonido-analysis::xcorr`)
- Envelope follower (`sonido-core::envelope`)

**What's novel:** Phase-amplitude coupling as an effect parameter. The guitar's
harmonic content modulates effect parameters in musically meaningful ways:
- Tremolo rate driven by harmonic complexity (play a chord, rate increases)
- Filter sweep correlated with picking dynamics via envelope-phase coupling
- Adaptive EQ that learns playing style over 30 seconds and shapes tone to match

**Sub-ideas:**

**a. PAC Tremolo:** Comodulogram analysis extracts the coupling between low-frequency
rhythm (pick strokes) and high-frequency harmonics. This coupling coefficient
modulates tremolo depth/rate. Soft picking gives a gentle tremolo, and hard
strumming gives intense modulation.

**b. Adaptive Tone:** An LMS filter is trained on the player's signal over N seconds.
The filter converges toward the spectral profile of the playing, then applies
an inverse or complementary EQ, so the tone adapts to the guitar, pickup, and
playing style automatically.

**c. Phase-Coherent Modulation:** A Hilbert transform extracts the instantaneous
phase of the guitar signal. Modulation effects (chorus, flanger, phaser) sync their
LFO phase to the guitar's phase structure rather than a free-running oscillator,
so the modulation stays in step with the music.

**References:**
- Canolty et al., "High Gamma Power Is Phase-Locked to Theta Oscillations" (2006)
- Widrow & Hoff, "Adaptive Switching Circuits" (1960): original LMS
- No known commercial guitar effect uses PAC or adaptive filtering

---

### 4. Space Station 2.0

**Concept:** Guitar pitch detection drives PolyBLEP oscillators through independent
effect chains. ROADMAP.md already lists this as a planned feature. The signature
version adds formant synthesis, chord detection, and harmonizer modes.

**Existing infrastructure:**
- `sonido-synth`: PolyBLEP oscillators, ADSR envelopes, voice manager, mod matrix
- Real-time pitch detection in the `tuner` kernel; `pitch_shift` kernel
- DAG routing for parallel dry/synth paths
- Full effect chain available for synth signal

**What's novel beyond ROADMAP:** The roadmap describes a basic pitch-tracking synth.
The signature version uses the full synth + effects + analysis stack:
- **Formant synthesis**: Vowel filter bank applied to synth output. Guitar triggers
  synth, expression pedal sweeps through vowel shapes (ah-ee-oh-oo).
- **Chord detection**: Multiple pitch detection → chord identification → synth
  voices tuned to chord tones with configurable voicings (drop-2, spread, unison).
- **Harmonizer**: Detected pitch + interval table → parallel synth voices at
  fixed intervals (3rd, 5th, octave). Key-aware if MIDI input provides scale.
- **Freeze + synth**: Spectral freeze captures guitar timbre; synth plays new
  pitches through the frozen spectral envelope.

**References:**
- DigiTech Space Station XP-300 (1996, discontinued, cult following)
- Electro-Harmonix Synth9 / Mel9 / Key9 (guitar-to-synth, analog modeling approach)
- Boss SY-200 (modern guitar synth, polyphonic tracking)

---

### 5. Spectral Effects

**Concept:** FFT-domain effects using `sonido-analysis` infrastructure. ROADMAP.md
already lists them as "Spectral Processing Effects." The signature version treats
spectral operations as performance instruments, not studio tools.

**Existing infrastructure:**
- FFT, STFT, spectral analysis in `sonido-analysis`
- `SpectralNode` / `SpectralEffect` (`crates/sonido-core/src/graph/spectral_node.rs`, `spectral` feature): FFT/overlap-add graph primitive for real-time frequency-domain processing; no effect uses it yet
- DAG routing for parallel spectral/temporal paths

**Signature twists:**

**a. Spectral Freeze + Resynth:** Hold a footswitch to capture an FFT frame.
The frozen spectrum sustains indefinitely (no noise buildup, unlike reverb-based
sustain). While the footswitch is held, play new notes: the pitch changes but the
spectral envelope stays frozen. Release it to return to the live signal.

**b. Spectral Morph:** Capture two spectral snapshots (A and B). An expression pedal
crossfades between them in the frequency domain. At 50%, you hear the spectral
average of A and B, which can sound otherworldly.

**c. Spectral Gate as Instrument:** Use a spectral gate as a creative effect
instead of for noise reduction. Set the threshold so only the loudest harmonics
pass through. Soft playing produces a thin, ghostly tone (fundamental only), and
hard playing lets the full spectrum through. The gate threshold becomes an
expression parameter.

**References:**
- Eventide Blackhole (spectral-ish reverb, infinite decay)
- Red Panda Particle V2 (granular/spectral delay)
- Meris Hedra (pitch-shifting with chromatic quantization)

---

### 6. Adaptive Effects

**Concept:** Effects that learn and adapt to the player's style using the adaptive
filtering infrastructure in `sonido-analysis`.

**Existing infrastructure:**
- LMS / NLMS adaptive filters (`sonido-analysis::lms`)
- Envelope follower (`sonido-core::envelope`)
- Cross-correlation for pattern detection (`sonido-analysis::xcorr`)

**Sub-ideas:**

**a. Style Mirror:** An LMS filter trained on 30 seconds of playing extracts the
spectral "fingerprint" of the player's style. When engaged, it applies a
complementary EQ that makes any guitar sound like that player's tone profile. For
example: "make my Strat sound like it has the spectral profile of my Les Paul
session."

**b. Dynamic Compressor Learning:** Cross-correlation between input envelope and
desired output envelope (set by a reference recording or a simple "target dynamics"
control). The compressor's ratio, threshold, and knee auto-adjust over time to
match the target dynamic profile. As it adapts to the player's dynamic range
over time, its output becomes more expressive.

**c. Rhythm-Aware Effects:** Cross-correlation detects rhythmic periodicity in
playing. Delay time, tremolo rate, and LFO speed auto-lock to detected rhythm.
Unlike tap tempo, this happens continuously and adapts as the tempo changes, so
the effects follow a player who plays rubato.

**References:**
- No known commercial effect uses adaptive filtering for creative purposes
- Academic: "Adaptive Audio Effects", Giannoulis et al. (DAFx 2012)
- Related concept: Eventide H9 HotSwitch (but preset-based, not adaptive)

---

## audioDNA Inspirations

The products analyzed so far through clean-room reverse engineering
(Ventura Vibe, Obscura Delay, Dirty Robot, Polara Reverb) are conservative
designs: well-executed traditional effects.

The creative gap is in products that create new categories:

| Product | What makes it distinctive | Sonido infrastructure needed |
|---------|--------------------------|------------------------------|
| Eventide H9 | Scene morphing, algorithm switching | `KernelParams::lerp()`, topology swap |
| Meris Mercury7 | Algorithmic reverb as instrument (pitch, swell, density as expressive controls) | Reverb kernel + expression mapping |
| Chase Bliss Mood | Micro-looper + reverb interaction, clock division as creative tool | Spectral freeze + DAG routing |
| Red Panda Tensor | Time manipulation (reverse, stretch, random splice) | Spectral + buffer manipulation |
| Meris Hedra | Chromatic pitch shifting with rhythmic quantization | Pitch detection + synth engine |
| Hologram Microcosm | Granular + delay + reverb in unusual combinations | DAG routing + spectral processing |
| Cooper FX Generation Loss | Lo-fi as deliberate aesthetic (wow/flutter, noise, dropout) | Tape kernel extended with failure modes |

These products succeed because they combine standard DSP building blocks in
non-standard ways. Sonido has most of the building blocks, but it lacks the
creative routing and interaction design that turns building blocks into
instruments.

---

## Interaction Patterns

Physical interaction design determines whether a feature is a studio tool or a
performance instrument.

### Foot Gestures

| Gesture | Detection | Musical use |
|---------|-----------|-------------|
| Tap | Single press < 300ms | Toggle, tap tempo |
| Hold | Press > 500ms | Freeze, sustain, secondary function |
| Double-tap | Two presses < 400ms apart | Mode switch, scene jump |
| Heel-toe sweep | Expression pedal continuous | Morph, wah, parameter sweep |
| Toe click | Expression pedal at max + press | Engage/disengage expression target |

### Knob Combinations

| Pattern | Behavior |
|---------|----------|
| Two-knob morph zone | When Knob A and Knob B are both above 70%, a third parameter (interaction) activates |
| Dead zone breakout | Turning a knob past 90% engages a different behavior (e.g., delay self-oscillation) |
| Relative knob | Knob controls rate of change, not absolute value (turn right = parameter increases over time) |

### Expression Pedal Curves

| Curve | Use case |
|-------|----------|
| Linear | Direct parameter control (depth, mix) |
| Logarithmic | Volume, filter cutoff (perceptual linearity) |
| S-curve | Crossfade, morph position (slow at ends, fast in middle) |
| Custom LUT | Arbitrary response for specific effects |
| Hysteresis | Different curve up vs. down (heel-to-toe vs. toe-to-heel) |

The expression pedal is the most underused control in guitar effects. Most
pedals map it linearly to one parameter. With scene morphing, a single sweep
could move through a multi-dimensional parameter space and make tonal changes
that would otherwise require 6 simultaneous knob turns.

---

## Priority Assessment

| Idea | Novelty | Existing infra | Implementation effort | Priority |
|------|---------|----------------|----------------------|----------|
| Dynamic Waveshaper | High | High (distortion kernel, envelope) | Low | **0th** |
| Scene Morphing | Medium | High (`lerp()`, `MorphSpace`, `ExpressionInput`) | Low-Medium | **1st** |
| Living Topology | High | High (DAG, crossfade, DSL) | Medium | **2nd** |
| Cross-Domain DSP | Very High | High (analysis crate) | Medium-High | **3rd** |
| Spectral Effects | Medium | Medium (FFT + `SpectralNode` RT path exist, no effects yet) | High | 4th |
| Space Station 2.0 | Medium | High (synth, DAG, tuner pitch detector) | High (polyphonic detect) | 5th |
| Adaptive Effects | Very High | Medium (LMS exists, needs RT bridge) | High | 6th |

Dynamic waveshaper response is the single change with the most benefit for the
effort: it is low effort (~200-400 LOC), builds on the existing distortion kernel
and envelope follower, and directly addresses the #1 quality gap between digital
and analog distortion. That gap is the "does it clean up when you roll back the
volume?" test that every guitarist applies instinctively.

Scene morphing and living topology give the next most benefit for the effort:
they build on infrastructure that already works and create new kinds of
interaction. Cross-domain DSP is the most novel (no commercial product uses PAC
or adaptive filtering for guitar effects), but it requires bridging the offline
analysis path to real-time processing.

---

## See Also

- [Roadmap](../ROADMAP.md): Planned capabilities and milestones
- [Effects Reference](../EFFECTS_REFERENCE.md): Current 36 effects with parameters
- [Kernel Architecture](../KERNEL_ARCHITECTURE.md): DspKernel/KernelParams patterns
- [Embedded Guide](../EMBEDDED.md): Hardware targets and constraints
- [Architecture](../ARCHITECTURE.md): Crate dependency graph
