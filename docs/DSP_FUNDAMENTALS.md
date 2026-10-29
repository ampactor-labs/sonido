# DSP Fundamentals in Sonido

This document explains the digital signal processing theory behind Sonido's implementations. Each section connects the textbook math to the production Rust code that implements it.

## Table of Contents

- [Sampling Theory and Aliasing](#sampling-theory-and-aliasing)
- [Anti-Aliased Oscillators: PolyBLEP](#anti-aliased-oscillators-polyblep)
- [Oversampling for Nonlinear Processing](#oversampling-for-nonlinear-processing)
- [Digital Filters](#digital-filters)
  - [Biquad (Second-Order IIR)](#biquad-second-order-iir)
  - [State Variable Filter (SVF)](#state-variable-filter-svf)
  - [One-Pole Filters](#one-pole-filters)
- [Delay Lines and Interpolation](#delay-lines-and-interpolation)
- [Reverb: Feedback Delay Network](#reverb-feedback-delay-network)
- [Parameter Smoothing](#parameter-smoothing)
- [Waveshaping and Distortion](#waveshaping-and-distortion)
- [Dynamics Processing](#dynamics-processing)
- [Modulation Effects](#modulation-effects)
- [Tempo Synchronization](#tempo-synchronization)
- [Gain Staging](#gain-staging)
- [Soft Limiting (Safety Stage)](#soft-limiting-safety-stage)
- [Shared DSP Vocabulary](#shared-dsp-vocabulary)
- [Denormal Protection](#denormal-protection)
- [Numerical Considerations](#numerical-considerations)

---

## Sampling Theory and Aliasing

The Nyquist-Shannon theorem states that a continuous signal can be perfectly reconstructed from discrete samples only if the signal contains no frequency components above half the sample rate (the Nyquist frequency). At 48 kHz, this limit is 24 kHz.

**Aliasing** occurs when frequencies above Nyquist are present in a signal. They fold back into the audible range as inharmonic artifacts. This is especially problematic for:

1. **Oscillators** generating non-sinusoidal waveforms (saw, square, pulse) whose Fourier series contain infinite harmonics
2. **Nonlinear processing** (distortion, waveshaping) that creates new harmonics from the input signal

Sonido addresses both cases: PolyBLEP for oscillators and oversampling for nonlinear effects.

---

## Anti-Aliased Oscillators: PolyBLEP

**Source:** `crates/sonido-synth/src/oscillator.rs`

### The Problem with Naive Waveforms

A naive digital sawtooth wave is defined as:

```
saw(t) = 2t - 1,  t in [0, 1)
```

This has an instantaneous discontinuity at `t = 0` where the value jumps from +1 to -1. In the frequency domain, this discontinuity produces harmonics at every integer multiple of the fundamental frequency, extending to infinity. Since a digital system cannot represent frequencies above Nyquist, these harmonics alias back and produce audible artifacts, especially at higher pitches.

### The PolyBLEP Solution

PolyBLEP (Polynomial Band-Limited Step) applies a polynomial correction in a small region around each discontinuity. Computing a true band-limited waveform is expensive, so PolyBLEP approximates one by smoothing only the transition region.

The correction function in Sonido (`poly_blep` in `oscillator.rs`) is a 4th-order, C²-continuous polynomial spanning 2 samples on each side of the discontinuity:

```rust
fn poly_blep(t: f32, dt: f32) -> f32 {
    const A4: f32 = -43.0 / 48.0;
    const A3: f32 = 7.0 / 6.0;
    const A2: f32 = 0.5;
    const A0: f32 = -1.0;
    const C: f32 = -11.0 / 48.0;

    let dt2 = 2.0 * dt;
    if t < dt2 {
        // Within 2 samples past the discontinuity
        let n = t / dt;
        if n < 1.0 {
            let n2 = n * n;
            A4 * n2 * n2 + A3 * n2 * n + A2 * n2 + A0
        } else {
            let u = 2.0 - n;
            let u2 = u * u;
            C * u2 * u2
        }
    } else if t > 1.0 - dt2 {
        // Within 2 samples before: antisymmetric mirror of the branch above
        let n = (1.0 - t) / dt;
        // ... same polynomial in n, negated
    } else {
        0.0
    }
}
```

The parameter `dt` is the phase increment per sample (`frequency / sample_rate`). It sets the width of the correction window, so the window scales with frequency. At higher frequencies (larger `dt`), where aliasing is worst, the correction spans more of the waveform and gives stronger anti-aliasing. The source documents roughly 50 dB of alias suppression, against about 30 dB for the common 2nd-order, 1-sample-window version (Välimäki et al., 2010).

### Waveform Generation

All waveform code is in `generate_sample_at_phase_with_dt(phase, dt)`. `dt` is the nominal `phase_inc` for `advance()`, and the effective (modulated) phase step for `advance_with_pm()`.

**Sawtooth**: The naive sawtooth minus the PolyBLEP correction at phase 0:

```rust
let naive = 2.0 * phase - 1.0;
naive - poly_blep(phase, dt)
```

**Square/Pulse** (`generate_pulse_with_dt`): Two PolyBLEP corrections, one at the rising edge (phase 0) and one at the falling edge (phase = duty cycle):

```rust
let naive = if phase < duty { 1.0 } else { -1.0 };
let blep1 = poly_blep(phase, dt);
let blep2 = poly_blep(rem_euclid_f32(phase - duty + 1.0, 1.0), dt);
naive + blep1 - blep2
```

**Triangle**: Generated by integrating a PolyBLEP-corrected square wave through a leaky integrator. This approach produces a triangle with better anti-aliasing than the naive piecewise-linear formula, because the integration inherently smooths the corrected square wave's remaining discontinuities:

```rust
let blep_square =
    square + poly_blep(phase, dt) - poly_blep(rem_euclid_f32(phase + 0.5, 1.0), dt);
let leak = 1.0 - (self.frequency / self.sample_rate).min(0.1);
self.prev_output = leak * self.prev_output + blep_square * dt * 4.0;
```

The frequency-adaptive `leak` creates a leaky integrator that prevents DC drift while preserving the triangle shape: it approaches 1.0 at low frequencies and is floored at 0.9. The factor `dt * 4.0` scales the integration to maintain correct amplitude.

### Phase Modulation for FM Synthesis

The oscillator supports FM synthesis through phase modulation (`advance_with_pm`). The modulator's output (in radians) is converted to normalized phase and added to the carrier's phase:

```rust
let mod_phase = phase_mod / (2.0 * PI);
let modulated_phase = rem_euclid_f32(self.phase + mod_phase, 1.0);
```

The `rem_euclid_f32` function ensures the modulated phase wraps correctly into [0, 1), using a `no_std`-compatible Euclidean remainder. PolyBLEP then uses the actual modulated phase step (wrap-corrected, never smaller than `phase_inc`) as its window width, so corrections land on the true discontinuity positions.

---

## Oversampling for Nonlinear Processing

**Source:** `crates/sonido-core/src/oversample.rs`

### Why Oversampling

Nonlinear operations (distortion, waveshaping, saturation) generate new harmonic content. If a 5 kHz input is processed through a waveshaper at 48 kHz, the generated harmonics at 10 kHz, 15 kHz, 20 kHz, and 25 kHz would place the 25 kHz component above Nyquist, causing it to alias back to 23 kHz.

Oversampling solves this by temporarily increasing the sample rate before nonlinear processing:

1. **Upsample** the input signal by factor N (insert samples via interpolation)
2. **Process** through the nonlinear effect at the higher rate (harmonics now have more headroom before Nyquist)
3. **Downsample** with an anti-aliasing filter to remove content above the original Nyquist, then decimate back to the original rate

### Implementation: `Oversampled<const FACTOR: usize, E: Effect>`

Sonido uses const generics to make the oversampling factor a compile-time parameter, so the compiler can optimize the loops and eliminate branching.

**Upsampling** uses polyphase windowed-sinc interpolation with a Blackman-Harris window. An 8-sample input history is maintained as a circular buffer. For each output sub-sample, the corresponding polyphase kernel row is convolved with the input history:

```rust
fn upsample_channel(input: f32, history: &mut [f32; UPSAMPLE_TAPS], pos: &mut usize,
                    work: &mut [f32; MAX_OVERSAMPLE_FACTOR], kernel: &[[f32; UPSAMPLE_TAPS]]) {
    // Push new sample into circular history
    history[*pos] = input;
    *pos = (*pos + 1) % UPSAMPLE_TAPS;

    for p in 0..FACTOR {
        let mut sum = 0.0;
        let k = &kernel[p];
        for t in 0..UPSAMPLE_TAPS {
            let idx = (*pos + t) % UPSAMPLE_TAPS;
            sum += history[idx] * k[t];
        }
        work[p] = sum * FACTOR as f32;
    }
}
```

`upsample()` / `upsample_right()` call this per channel.

The polyphase decomposition avoids zero-stuffing (inserting zeros and running a full-rate filter): each sub-sample evaluates only the sinc kernel taps that correspond to its fractional offset. The Blackman-Harris window provides >92 dB sidelobe suppression, so interpolation images stay well below the noise floor.

Sonido uses windowed-sinc interpolation here because linear interpolation causes HF rolloff proportional to `sinc(pi*f/fs)^2`. The rolloff is significant at high audio frequencies, especially with 2x oversampling, where the image frequencies are close to the passband.

**Downsampling** applies a 48-tap Kaiser-windowed sinc FIR anti-aliasing filter followed by decimation. All FACTOR processed samples are written into a circular FIR state buffer (no shifting), and the convolution sum is computed once, at the decimation point:

```rust
fn downsample_channel(state: &mut [f32; FILTER_TAPS], pos: &mut usize,
                      work: &[f32; MAX_OVERSAMPLE_FACTOR], coeffs: &[f32; FILTER_TAPS]) -> f32 {
    for sample in work.iter().take(FACTOR) {
        state[*pos] = *sample;
        *pos = (*pos + 1) % FILTER_TAPS;
    }
    // pos points past the newest sample; walk backwards so coeffs[0] pairs with it
    let mut output = 0.0;
    for j in 0..FILTER_TAPS {
        let idx = (*pos + FILTER_TAPS - 1 - j) % FILTER_TAPS;
        output += state[idx] * coeffs[j];
    }
    output
}
```

### Filter Design

**Downsampling FIR (48-tap, Kaiser-windowed sinc, beta = 8.0):**

Three sets of coefficients are provided for 2x, 4x, and 8x oversampling. The cutoff frequencies are:

| Factor | Normalized Cutoff | Stopband Attenuation |
|--------|------------------|---------------------|
| 2x | 0.45 x oversampled Nyquist | >80 dB |
| 4x | 0.225 x oversampled Nyquist | >80 dB |
| 8x | 0.1125 x oversampled Nyquist | >80 dB |

The Kaiser window beta of 8.0 was chosen to achieve >80 dB stopband attenuation with 48 taps. The relationship between beta and attenuation follows Kaiser's empirical formula: `A = 2.285*(M-1)*pi*df + 7.95` where A is the stopband attenuation in dB. At beta=8.0 with M=48, the achieved attenuation exceeds the 80 dB target across all three coefficient sets. All coefficient sets are symmetric (linear phase), so they preserve waveform shape through the oversampling path.

**Upsampling kernels (Blackman-Harris windowed sinc, 8 taps/phase):**

Each kernel is a `[FACTOR][8]` array of polyphase sub-filters. The sinc function is centered between taps 3 and 4 (fractional center), and each phase evaluates at a different fractional offset. The last phase in each kernel is the identity (integer sample position). Each row is normalized to sum to `1/FACTOR`; the upsample function multiplies by FACTOR to restore unity DC gain.

### Latency

The `Oversampled` wrapper reports its latency as the sum of FIR group delay, sinc interpolation delay, and the inner effect's latency:

```rust
fn latency_samples(&self) -> usize {
    let filter_latency = FILTER_ORDER / 2;   // 47/2 = 23 samples (symmetric FIR)
    let upsample_latency = UPSAMPLE_TAPS / 2; // 8/2 = 4 samples (sinc kernel)
    filter_latency + upsample_latency + self.effect.latency_samples()
}
```

For the 48-tap downsample filter and 8-tap upsample kernel, this adds 27 samples of latency.

### References

- A.V. Oppenheim & R.W. Schafer, "Discrete-Time Signal Processing", Ch. 4 & 7 (multirate signal processing)
- J.F. Kaiser, "Nonrecursive Digital Filter Design Using the I0-sinh Window", Proc. IEEE Int. Symp. Circuits & Systems, 1974 (Kaiser window design)
- F.J. Harris, "On the Use of Windows for Harmonic Analysis with the DFT", Proc. IEEE, 1978 (Blackman-Harris window)

---

## Digital Filters

### Biquad (Second-Order IIR)

**Source:** `crates/sonido-core/src/biquad.rs`

The biquad is the standard general-purpose filter in audio DSP. It implements a second-order IIR (Infinite Impulse Response) filter using the Transposed Direct Form II difference equation:

```
y[n] = b0 * x[n] + s1
s1   = b1 * x[n] - a1 * y[n] + s2
s2   = b2 * x[n] - a2 * y[n]
```

This requires 7 multiply-adds per sample with only 2 state variables (`s1`, `s2`).

Sonido uses TDF-II (`Biquad::process`, which also flushes denormals in the state registers) because it has the best numerical properties of all direct forms in 32-bit floating-point: reduced coefficient quantization error, improved dynamic range, and fewer limit-cycle oscillations with high-Q filters. The state variables hold accumulated future contributions instead of acting as raw delay lines, which keeps intermediate values in a tighter dynamic range. It is the standard topology in professional audio frameworks (JUCE, Faust, RTNeural). For audio-rate modulation, where coefficients change often, the SVF topology is preferred (see below).

### RBJ Audio EQ Cookbook

Coefficient calculation functions implement Robert Bristow-Johnson's Audio EQ Cookbook formulas, the standard reference for biquad filter design in audio. Sonido provides:

- **Lowpass** (`lowpass_coefficients`): Attenuates frequencies above the cutoff
- **Highpass** (`highpass_coefficients`): Attenuates frequencies below the cutoff
- **Bandpass** (`bandpass_coefficients`): Passes a band around the center frequency
- **Notch** (`notch_coefficients`): Rejects a narrow band at the center frequency
- **Peaking EQ** (`peaking_eq_coefficients`): Boosts or cuts around a center frequency

All functions share the same structure: compute the intermediate variables `omega`, `cos(omega)`, `sin(omega)`, and `alpha = sin(omega) / (2*Q)`, then derive the six coefficients. The `Q` parameter controls the filter's bandwidth or resonance.

The coefficient normalization (`Biquad::set_coefficients`) divides all coefficients by `a0`, which is standard practice so the difference equation can omit the `a0` multiplication at runtime:

```rust
let a0_inv = 1.0 / a0;
self.b0 = b0 * a0_inv;
// ...
```

### State Variable Filter (SVF)

**Source:** `crates/sonido-core/src/svf.rs`

The SVF is a different topology that provides lowpass, highpass, bandpass, and notch outputs simultaneously from a single processing step. Sonido implements the Chamberlin SVF with trapezoidal integration for improved stability.

The SVF has two advantages over the biquad:

1. **Modulation stability**: The SVF can have its cutoff frequency modulated at audio rate without the instability that biquad coefficient changes can cause. This makes it ideal for filter sweeps, envelope-following, and LFO modulation.

2. **Multi-output**: A single SVF computation yields all four filter types simultaneously via `process_all()`, useful when an effect needs multiple filter responses from the same signal.

The coefficient computation (`update_g` / `update_k`) uses `tan(pi * fc / fs)` for the frequency warping, which gives better frequency accuracy near Nyquist compared to the bilinear transform approximation. Below 10 kHz it uses the Padé `fast_tan`; above, `libm::tanf`:

```rust
let arg = PI * self.cutoff / self.sample_rate;
self.g = if self.cutoff < 10_000.0 { fast_tan(arg) } else { tanf(arg) };
self.k = 1.0 / self.resonance;
```

The core processing (`process_all`) uses implicit trapezoidal integration:

```rust
let v3 = input - self.ic2eq;
let v1 = (self.g * v3 + self.ic1eq) / (1.0 + self.g * (self.g + self.k));
let v2 = self.ic2eq + self.g * v1;
// v1_sat = v1, or fast_tanh(v1 * d) / d when set_drive() > 0
self.ic1eq = flush_denormal(2.0 * v1_sat - self.ic1eq);
self.ic2eq = flush_denormal(2.0 * v2 - self.ic2eq);
```

The state variables `ic1eq` and `ic2eq` use the "2x trick" for trapezoidal integration: `new_state = 2*output - old_state`. This is equivalent to the standard TPT (topology-preserving transform) formulation by Vadim Zavalishin. The optional drive saturates only the bandpass integrator state, so small-signal response is unchanged. `FourPoleSvf` cascades two stages for 24 dB/oct.

### One-Pole Filters

One-pole lowpass filters appear throughout Sonido as building blocks:

- **Reverb feedback damping** (`OnePole` per FDN line in `reverb.rs`; `CombFilter::set_damp()` in `comb.rs`): A one-pole lowpass in the feedback path simulates frequency-dependent absorption in acoustic spaces. Higher frequencies decay faster, as they do in real rooms.
- **Tone controls and HF rolloff**: `OnePole` lowpasses in kernels such as tape and the spring reverb reduce high-frequency content.
- **Parameter smoothing** (`param.rs`): Exponential smoothing is implemented as a one-pole lowpass.
- **Envelope following**: Level detection uses one-pole filters for attack and release ballistics.

The one-pole lowpass difference equation is:

```
y[n] = y[n-1] + alpha * (x[n] - y[n-1])
```

where `alpha = 1 - exp(-1 / (tau * fs))` and `tau` is the time constant in seconds.

---

## Delay Lines and Interpolation

**Source:** `crates/sonido-core/src/delay.rs`

### Circular Buffers

Both `InterpolatedDelay` and `FixedDelayLine` use circular (ring) buffers. A write pointer advances through the buffer; reading at an offset from the write pointer produces the delayed signal. The modular arithmetic `(write_pos + 1) % buffer.len()` handles the wrap-around.

### Fractional Delay and Interpolation

Many effects require non-integer delay times. A chorus with a 15.7-sample delay, or a comb filter tuned to a specific frequency, cannot use integer-only delays. Interpolation reconstructs values between samples.

**Linear interpolation** (`Interpolation::Linear`, the default): Blends two adjacent samples proportionally to the fractional part:

```
output = buffer[n] + frac * (buffer[n-1] - buffer[n])
```

This is computationally cheap (one multiply, two additions) and adequate for most delay effects.

**Cubic interpolation** (Lagrange 3rd-order): Uses four adjacent samples for a smoother reconstruction. Both `InterpolatedDelay` and `FixedDelayLine` support this via `set_interpolation(Interpolation::Cubic)`:

```rust
let a0 = y3 - y2 - y0 + y1;
let a1 = y0 - y1 - a0;
let a2 = y2 - y0;
let a3 = y1;
a0 * t3 + a1 * t2 + a2 * t + a3
```

Cubic interpolation reduces the high-frequency roll-off inherent in linear interpolation, which matters for modulated delays (chorus, flanger) where the delay time continuously changes.

### Two Implementations

| | `InterpolatedDelay` | `FixedDelayLine<N>` |
|---|---|---|
| Memory | Heap-allocated (`Vec<f32>`) | Stack-allocated (`[f32; N]`) |
| Size | Runtime-determined | Compile-time constant |
| Use case | General effects | Embedded / `no_std` targets |
| Interpolation | None, Linear, or Cubic (default: Linear) | None, Linear, or Cubic |

---

## Reverb: Feedback Delay Network

**Source:** `crates/sonido-effects/src/kernels/reverb.rs`, `crates/sonido-core/src/allpass.rs` (`ModulatedAllpass`), `crates/sonido-core/src/one_pole.rs`

### Algorithm Structure

Sonido's `reverb` kernel is an 8-line feedback delay network (FDN, Jot & Chaigne 1991) with Hadamard feedback mixing, early reflections, and allpass diffusion. It keeps Freeverb's delay tunings and room-size constants but replaces Freeverb's parallel combs with a mixed feedback matrix. Per sample:

```
input -> stereo predelay -> mono sum -+-> ER tapped delay (14 taps) ------------> er_level
                                      +-> 8 FDN delays <-> Hadamard feedback -> 4 allpasses L/R
                                                                                 |
                                           ER + late -> M/S width -> wet/dry -> output gain
```

### FDN Delay Lines

The 8 delay lengths at 44.1 kHz are Freeverb's comb tunings:

```rust
const FDN_TUNINGS_44K: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
```

These values are mutually prime (no common factors), which keeps the lines from reinforcing at common delay multiples; shared factors would give metallic coloration. For other sample rates they are scaled proportionally:

```rust
fn scale_to_rate(samples: usize, target_rate: f32) -> usize {
    (roundf(samples as f32 * target_rate / REFERENCE_RATE) as usize).max(1)
}
```

Each line's read position is modulated by its own sine LFO (`FDN_MOD_RATES` 0.3-1.3 Hz, depth 0.3 ms) to break up residual metallic resonances.

### Hadamard Feedback Matrix

`process_fdn` reads all 8 lines, mixes them with an 8x8 Hadamard matrix (fast Walsh-Hadamard: 3 butterfly stages, scaled by 1/sqrt(8) so the matrix is orthogonal and energy-preserving), damps each mixed signal, and writes it back:

```rust
let damped = self.fdn_damping[i].process(mixed[i]);
self.fdn_delays[i].write(flush_denormal(input + damped * self.feedback));
```

Because the matrix is orthogonal, every line's output feeds every other line, so echo density builds far faster than in independent combs, while the loop gain stays governed by `feedback` alone.

**Damping**: each line has a `OnePole` lowpass in its feedback path. The `damping` parameter maps logarithmically to cutoff, `200 * 100^(1 - damping)` Hz (20 kHz at 0%, 200 Hz at 100%). High frequencies decay faster, mimicking air and wall absorption.

**Stereo output**: L sums the even lines and R the odd lines (each x0.25); both are then scaled by `fdn_compensation`.

### Allpass Diffusion

Each channel runs its FDN output through 4 series `ModulatedAllpass` stages (feedback 0.6, 0.15 ms modulation) on Freeverb's allpass tunings, with the right channel offset by 23 samples for decorrelation:

```rust
const ALLPASS_TUNINGS_44K: [usize; 4] = [556, 441, 341, 225];
const ALLPASS_TUNINGS_44K_R: [usize; 4] = [579, 464, 364, 248];
```

An allpass passes all frequencies at equal amplitude but disperses energy in time, smoothing the late tail. Both `AllpassFilter` and `ModulatedAllpass` are true Schroeder allpasses:

```
v[n] = x[n] + g·v[n−D]        (written to the delay line)
y[n] = v[n−D] − g·v[n]

H(z) = (−g + z⁻ᴰ) / (1 − g·z⁻ᴰ)    |H| = 1 at every frequency
```

Freeverb's shortcut, `y = −x + v[n−D]`, has the response `(−1 + (1+g)·z⁻ᴰ) / (1 − g·z⁻ᴰ)`: up to about 2× gain where `z⁻ᴰ = −1`, even at `g = 0`. After a reverb, that gain is only a coloration. Inside a feedback loop (the delay's diffusion, the plate's tank), it pushes the loop gain above 1.

### Early Reflections and Stereo Width

A 14-tap delay on the mono sum (prime tap positions, ~25 ms window at 44.1 kHz) provides early reflections: even taps feed L, odd taps feed R, gains fall as `0.3 / sqrt(i+1)`, and tap positions scale by `0.5 + room * 1.5`. The `er_level` parameter sets their level.

The `width` parameter (index 5, 0-100%, default 100%) scales the side signal of the combined wet output:

```rust
let mid = (wet_l + wet_r) * 0.5;
let side = (wet_l - wet_r) * 0.5;
let final_l = mid + side * width;
let final_r = mid - side * width;
```

### Feedback Coefficient Calculation

The feedback amount is derived from the `room_size` and `decay` parameters (`update_derived`, recomputed only when room/decay/damping move by more than 0.001):

```rust
let scaled_room = 0.28 + room * 0.7;    // Range: 0.28 to 0.98
self.feedback = (scaled_room + decay * (0.98 - scaled_room)).clamp(0.0, 0.99);
self.fdn_compensation = sqrtf((1.0 - self.feedback).max(0.01));
```

This maps the 0-100% parameters to a feedback range that stays below 1.0 (ensuring stability) while providing musically useful control. At maximum settings (`room=1.0, decay=1.0`), feedback reaches 0.98, producing very long tails without instability. The kernel reports `tail_samples = decay * 3 s`.

### Feedback Resonance Compensation

A feedback comb filter with coefficient `g` has peak gain `1/(1-g)` at resonance frequencies (where the round-trip delay produces constructive interference). At `g=0.5`, peak gain is 2× (+6 dB); at `g=0.9`, peak gain is 10× (+20 dB). Without compensation, the wet signal at resonance exceeds the dry signal for any `g > 0`, violating the -1 dBFS peak ceiling.

**Exact compensation: `c = (1-g)`**

Scaling the wet signal by `(1-g)` exactly cancels peak-gain amplification:

```
output_at_resonance = c × 1/(1-g) = (1-g)/(1-g) = 1.0
```

The compensated wet signal at resonance equals the dry signal, whatever the feedback. This is implemented in `gain::feedback_wet_compensation()` and used by delay, flanger, and phaser, which apply it to the wet signal before the mix. The mix (`blend`, see [Gain Staging](#gain-staging)) then applies the effect's fixed, measured level trim. The compensation keeps the wet level from tracking the feedback knob, and the trim matches the effect's level to its input at its defaults.

| Feedback | Compensation | Peak wet at resonance |
|----------|-------------|----------------------|
| 0.0 | 1.000 | 1.0 |
| 0.3 (phaser default) | 0.700 | 1.0 |
| 0.35 (delay default) | 0.650 | 1.0 |
| 0.5 (flanger default) | 0.500 | 1.0 |
| 0.9 | 0.100 | 1.0 |
| 0.95 | 0.050 | 1.0 |

At low feedback (g=0.1), compensation is 0.9, which is barely any attenuation. At high feedback (g=0.9), compensation is 0.1: the wet signal is quieter, but the decay tail is correspondingly longer. The `output` parameter provides makeup gain if needed.

**Moderate compensation for the reverb FDN: `c = sqrt(1-g)`**

The reverb's 8 mutually prime lines are mixed by an orthogonal matrix and averaged 4 per channel, so energy spreads across many modes instead of concentrating on the teeth of a single comb. Exact `(1-g)` compensation would attenuate more than necessary and leave the wet signal much quieter than the dry.

The reverb uses `sqrt(1-g)` (`fdn_compensation`): the geometric mean of unity and exact compensation.

**Topology distinction:**

| Topology | Peak gain | Compensation | Effects |
|----------|-----------|-------------|---------|
| Single comb (delay, flanger) | `1/(1-g)` | `(1-g)` exact | Delay, Flanger |
| Allpass cascade (phaser) | Unity magnitude | `(1-g)` conservative | Phaser |
| Hadamard FDN (reverb) | spread over 8 mixed lines | `sqrt(1-g)` moderate | Reverb |

The phaser's allpass cascade has unity magnitude at all frequencies, and its resonance is narrowband (3 notch-peak pairs vs hundreds of comb teeth). Exact compensation is conservative but harmless, and it keeps the phaser well within the ceiling.

Static wet-signal compensation is the standard approach for embedded/pedal DSP (FV-1, EQD Afterneath). DAW-grade reverbs (MAX/MSP gen~, Valhalla) sometimes use dynamic AGC, but its per-sample envelope tracking is too expensive for Sonido's embedded targets (Daisy Seed, Hothouse).

Reference: CCRMA PASP, "Feedback Comb Filters", which gives the peak gain as 1/(1-|g|).

Source: `crates/sonido-core/src/gain.rs` (`feedback_wet_compensation`), `crates/sonido-effects/src/kernels/reverb.rs` (`update_derived`)

---

## Parameter Smoothing

**Source:** `crates/sonido-core/src/param.rs`

### The Zipper Noise Problem

When a parameter like gain changes abruptly from one sample to the next, the resulting discontinuity in the output waveform produces an audible click or "zipper noise." This is especially noticeable for parameters that multiply the signal directly (gain, pan, mix levels).

### Exponential Smoothing: `SmoothedParam`

Sonido's primary smoothing strategy is a one-pole lowpass filter applied to the parameter value itself (`SmoothedParam::advance`):

```rust
pub fn advance(&mut self) -> f32 {
    if self.settled {
        return self.current;
    }
    let next = self.current + self.coeff * (self.target - self.current);
    if next == self.current || close_enough(next, self.target) {
        self.current = self.target;
        self.settled = true;
    } else {
        self.current = next;
    }
    self.current
}
```

`close_enough` is a relative test: within one part in a million of the target's magnitude, never tighter than 1e-6.

This is mathematically equivalent to:

```
param[n] = (1 - alpha) * param[n-1] + alpha * target
```

where `alpha = 1 - exp(-1 / (tau * fs))` (`recalculate_coeff`; a smoothing time of 0 gives `coeff = 1.0`, i.e. instant):

```rust
let time_constant = self.smoothing_time_ms / 1000.0;
let samples = time_constant * self.sample_rate;
self.coeff = 1.0 - expf(-1.0 / samples);
```

**Properties:**
- After one time constant, the parameter reaches ~63.2% of the way to the target
- After five time constants, it reaches ~99.3% (effectively settled)
- The approach is asymptotic, so `advance()` snaps to the target and marks the parameter settled (later calls skip the arithmetic) once it is close enough, or once a step no longer changes the value. The second test matters in `f32`: a 10 ms smoother stalls about 240 representable values short of its target, and above about 8 even that gap exceeds 1e-6, so with an absolute threshold alone a parameter could keep smoothing forever

Typical smoothing times: 5-10 ms for gain/pan, 20-50 ms for filter cutoff.

### Linear Smoothing: `LinearSmoothedParam`

For crossfades and situations where a predictable transition time matters, Sonido provides linear smoothing (`LinearSmoothedParam` in `param.rs`). The parameter changes at a constant rate:

```rust
pub fn advance(&mut self) -> f32 {
    if self.samples_remaining > 0 {
        self.current += self.increment;
        self.samples_remaining -= 1;
        if self.samples_remaining == 0 {
            self.current = self.target;  // Snap to exact target
        }
    }
    self.current
}
```

Linear smoothing guarantees the target is reached in exactly the specified time. The final snap to the target value prevents floating-point accumulation error from causing drift.

---

## Waveshaping and Distortion

**Source:** `crates/sonido-effects/src/kernels/distortion.rs`

### Signal Flow

```
Input -> [Envelope] -> Drive (gain, dynamic) -> ADAA Waveshaper -> Tone EQ -> Mix -> Soft Limit -> Output Level
```

The drive stage amplifies the input before waveshaping, pushing more of the signal into the nonlinear region. The tone control is a peaking biquad at 1 kHz (Q 0.7, -12 to +12 dB) after the waveshaper, followed by a `1/√drive` makeup gain that keeps loudness roughly steady as drive rises. A `soft_limit` safety stage and the output level follow the wet/dry mix.

The default parameters are the settings the effect is tuned to sound best at: drive 26 dB, tone +1.5 dB, output 0 dB, mix 100%. A freshly opened plugin and a pedal with its knobs at noon both start there, and the level contract trims the result to the input's loudness (see [Gain Staging](#gain-staging)).

### Waveshaping Functions

Each waveshaping algorithm has a distinct harmonic character determined by the symmetry of its transfer function:

**Symmetric functions** (soft clip, hard clip): Odd-symmetric functions `f(-x) = -f(x)` produce only odd harmonics (3rd, 5th, 7th...). This gives a "hollow" or "buzzy" quality typical of transistor and op-amp distortion.

**Asymmetric functions**: Functions that are not odd-symmetric produce even harmonics (2nd, 4th, 6th...) in addition to odd ones. Even harmonics are perceived as "warm" and "musical," characteristic of vacuum tube distortion.

**Foldback distortion**: When the input exceeds a threshold, the signal folds back instead of clipping. This produces a complex harmonic spectrum with both even and odd harmonics. As drive increases, the wavefolder creates more harmonics, while a clipper just compresses the signal. Like the other three modes, foldback uses first-order ADAA (piecewise parabolic antiderivative) to reduce aliasing artifacts.

**Dynamic Waveshaping**: The `dynamics` parameter modulates drive gain based on input level via an `EnvelopeFollower` (5 ms attack, 50 ms release). When input is quiet, drive is reduced by up to 80%, which keeps the signal in the waveshaper's linear region. This imitates the bias-point shift of a tube amp. At `dynamics=0` (default), no envelope processing occurs (zero overhead).

### Oversampling Integration

The distortion kernel relies on first-order ADAA (`Adaa1`) for its four waveshapers rather than oversampling; no kernel currently wraps itself in `Oversampled`. The wrapper is available for any `Effect`: waveshaping generates harmonics that can alias, and at 4x oversampling (192 kHz, 96 kHz Nyquist) the 5th harmonic of a 5 kHz fundamental (25 kHz) is represented correctly and removed by the decimation filter instead of folding back to 23 kHz.

---

## Dynamics Processing

**Source:** `crates/sonido-effects/src/kernels/compressor.rs`

### Feed-Forward Compressor

The compressor follows a feed-forward topology:

```
Input -> Level Detection -> Gain Computer -> Gain Application -> Output
```

In a feed-forward design, the gain reduction is computed from the input signal level, not the output. This gives more predictable behavior than feedback compressor topologies.

### Gain Computation: Soft Knee

The gain computer uses a soft-knee characteristic (`compute_gain_db` in `compressor.rs`) with three regions, where `overshoot = input_db - threshold_db` and `w = knee_db / 2`:

1. **Below knee** (`overshoot <= -w`): No compression, output equals input
2. **Within knee** (`|overshoot| <= w`): `-((overshoot + w) / knee_db)^2 * overshoot * (1 - 1/ratio)`, compression increases smoothly
3. **Above knee** (`overshoot > w`): `-overshoot * (1 - 1/ratio)`, full compression at the specified ratio

The default threshold is -18 dB at a 4:1 ratio, which engages on most program material without heavy compression. The knee width (default 6 dB) is a configurable parameter (`knee_db`, index 5), so users can set anything from a hard knee (0 dB) to a very soft transition.

The soft knee avoids the abrupt transition at the threshold that a hard-knee compressor has, which makes the compression more transparent.

### Ballistics: Attack and Release

The envelope follower uses separate one-pole filters for attack (level increasing) and release (level decreasing), each with its own time:

- **Attack time**: How quickly the compressor responds to transients
- **Release time**: How quickly compression releases after the signal drops

Fast attack catches transients but can sound "squashed." Slow attack lets transients through, preserving dynamics at the cost of slower response.

---

## Modulation Effects

### LFO Architecture

**Source:** `crates/sonido-core/src/lfo.rs`

LFOs use phase accumulation: a phase variable increments by `frequency / sample_rate` each sample and wraps at 1.0. The phase is then mapped to a waveform value. This approach is computationally efficient and alias-free for sub-audio frequencies.

### ModulationSource Trait

**Source:** `crates/sonido-core/src/modulation.rs`

The `ModulationSource` trait gives modulators a common interface. It is implemented by `Lfo` (sonido-core) and `AudioModSource` (sonido-synth, an envelope follower fed via its own `process()`). `AdsrEnvelope` and `EnvelopeFollower` do not implement it:

```rust
pub trait ModulationSource {
    fn mod_advance(&mut self) -> f32;
    fn is_bipolar(&self) -> bool;
    fn mod_reset(&mut self);
    fn mod_value(&self) -> f32;
}
```

Default methods (`mod_advance_unipolar`, `mod_advance_bipolar`) provide the conversions:
- Bipolar [-1, 1] to unipolar [0, 1]: `(value + 1) * 0.5`
- Unipolar [0, 1] to bipolar [-1, 1]: `value * 2 - 1`

Effects can therefore accept any modulation source, whatever its native range.

### Chorus, Flanger, Phaser, Tremolo

These effects all use an LFO to modulate a delay line, filter, or amplitude parameter:

- **Chorus**: LFO modulates a ~10-30 ms delay time, creating a detuned copy that mixes with the original
- **Flanger**: LFO modulates a shorter delay (~1-10 ms) with feedback, creating the characteristic comb-filter sweep
- **Phaser**: LFO modulates the frequencies of a series of allpass filters, creating notches that sweep through the spectrum
- **Tremolo**: LFO modulates the signal amplitude, creating rhythmic volume pulsing

### Tempo Sync for Modulation Effects

All four modulation effects support tempo sync via `set_tempo_context()`. When the **Sync** parameter is enabled, the LFO rate is overridden by a frequency derived from the host BPM and a selected **Division** (musical note value).

The conversion uses `TempoManager::division_to_hz()`:
```
lfo_hz = (bpm / 60.0) / division.beats()
```

For example, at 120 BPM with an eighth-note division: `(120/60) / 0.5 = 4.0 Hz`.

The computed Hz is clamped to each effect's rate range to prevent extreme values. Shared helpers in `sonido-core/src/tempo.rs` (`DIVISION_LABELS`, `index_to_division()`, `division_to_index()`) map between the 12 supported note divisions and UI-friendly indices.

**Source:** `crates/sonido-effects/src/kernels/{chorus,flanger,phaser,tremolo}.rs`

---

## Tempo Synchronization

**Source:** `crates/sonido-core/src/tempo.rs`

### NoteDivision

Musical note values are represented as an enum with a `beats()` method that returns the duration in quarter-note beats:

| Division | Beats | At 120 BPM |
|----------|-------|------------|
| Whole | 4.0 | 2000 ms |
| Half | 2.0 | 1000 ms |
| DottedHalf | 3.0 | 1500 ms |
| Quarter | 1.0 | 500 ms |
| DottedQuarter | 1.5 | 750 ms |
| TripletQuarter | 2/3 | 333.3 ms |
| Eighth | 0.5 | 250 ms |
| DottedEighth | 0.75 | 375 ms |
| TripletEighth | 1/3 | 166.7 ms |
| Sixteenth | 0.25 | 125 ms |
| TripletSixteenth | 1/6 | 83.3 ms |
| ThirtySecond | 0.125 | 62.5 ms |

Conversion to Hz, milliseconds, or samples is provided:

```rust
pub fn to_hz(&self, bpm: f32) -> f32 {
    let beats_per_second = bpm / 60.0;
    beats_per_second / self.beats()
}
```

### TempoManager

The `TempoManager` tracks musical position using a sample counter and provides:

- **Beat/bar position**: Current position in the musical timeline
- **Phase**: Fractional position within the current beat or bar (useful for syncing LFOs)
- **Division conversion**: Convert note divisions to Hz, ms, or samples at the current tempo

The position counter follows transport control (`play`, `stop`, `reset`), so effects stay synchronized when playback stops and resumes.

---

## Gain Staging

**Source:** `crates/sonido-core/src/gain.rs`, `crates/sonido-core/src/level.rs`

### The Problem

Nonlinear effects (distortion, saturation, wah) generate gain that depends on the input signal and parameter settings. Without explicit output level control, effects can produce unexpectedly hot output that clips downstream processors. At the same time, effects with inherent gain reduction (compressor, gate) need makeup gain to restore levels. And a mix control that crossfades two signals of different loudness, or two signals that partly cancel, changes the level as it turns.

### Universal Output Level

Effects in Sonido expose an `output` parameter (normally the last index in `ParameterInfo`; see `docs/DSP_QUALITY_STANDARD.md` Rule 3 for the exceptions) that provides -6 to +6 dB of post-processing level control (`OUTPUT_MIN_DB` / `OUTPUT_MAX_DB`, chosen so the knob's noon is 0 dB). Distortion, tape, drone, glitch, texture and time-stretch define wider ranges. The output level is applied as a linear gain multiplier at the end of the kernel's processing chain. It is the player's control. The level contract (below) matches an effect to its input's loudness.

The `gain.rs` module provides the shared implementation (`output_param_descriptor()` plus these helpers):

```rust
pub fn output_level_param(sample_rate: f32) -> SmoothedParam {
    SmoothedParam::standard(1.0, sample_rate)  // 0 dB, 10ms smoothing
}

pub fn set_output_level_db(param: &mut SmoothedParam, db: f32) {
    param.set_target(db_to_linear(db.clamp(OUTPUT_MIN_DB, OUTPUT_MAX_DB)));
}
```

The output level is stored internally as linear gain (1.0 = 0 dB) for efficient per-sample multiplication, with dB conversion only at the parameter interface boundary.

### The Level Contract

At its defaults every effect plays at the loudness of its input, and its mix control does not change that loudness. Each kernel declares one measured calibration in its `DspKernel` impl:

```rust
const LEVEL: Level = crate::calibration::DELAY;

pub struct Level {
    pub trim: f32,                    // gain on the wet signal (or the output, without a mix control)
    pub mix_correlation: Option<f32>, // measured wet/dry correlation ρ; None without a mix control
}
```

A kernel with a mix control applies the trim to its wet signal inside `Self::blend` ([below](#level-preserving-mix)). A kernel without one passes its sound through `Self::level` / `Self::level_stereo` where it leaves the processing, before its output level and any safety limiter, so the limiter acts on the final level (a +12 dB trim after the wah's limiter could reach +18 dBFS).

The calibration is measured, not tuned by hand. `cargo run -p sonido-effects --example calibrate --release` regenerates `crates/sonido-effects/src/calibration.rs`. It plays `sonido_analysis::signals::reference_guitar` through every registered effect, fully wet if the effect has a mix control and at its defaults otherwise. That signal is a synthetic strummed open-E chord (one strum a second, 6 s) peaking at `NOMINAL_PEAK_DBFS` = -20 dBFS, the median measured from a humbucker guitar into the Hothouse's line-level converter. `sonido_analysis::loudness::effect_gain_db` measures the change in integrated loudness (ITU-R BS.1770-4 LUFS), and the trim is scaled to cancel it. For an effect with a mix control, the zero-lag correlation of the fully wet output with the input becomes ρ. `crates/sonido-effects/tests/level_contract.rs` fails when an entry is more than 0.5 dB off, or when the mix moves the level more than 1.5 dB at 0, 25, 50 or 75% or at the default mix. Rerun the example after changing a kernel's sound or defaults. The looper and tuner are uncalibrated utilities (unity trim).

### Per-Effect Gain Staging

Selected entries from `calibration.rs` (trim in dB, measured fully wet for an effect with a mix control and at defaults otherwise):

| Effect | Trim | ρ | Notes |
|--------|------|---|-------|
| Distortion | -11.9 dB | 0.989 | 26 dB drive, `1/√drive` makeup, `soft_limit` after the mix |
| Preamp | -15.0 dB | none | 15 dB gain |
| Tape | -5.8 dB | none | 6 dB drive, output -6 dB |
| Wah | +12.2 dB | none | Band-pass normalized by Q (see below) |
| Compressor | +0.0 dB | 1.000 | Threshold -18 dB, 4:1, makeup 0 dB |
| Delay | +3.4 dB | 0.065 | Echoes scaled by `feedback_wet_compensation` |
| Vibrato | +0.0 dB | -0.365 | Default mix 100%; the wet partly cancels the dry, so a part-wet blend gets a boost |
| Looper, Tuner | 0.0 dB | none | Uncalibrated |

### Wah Bandpass Normalization

The SVF bandpass filter has peak gain equal to Q at the center frequency (`update_k` in `svf.rs`: `k = 1/Q`). With the default Q=5, the raw bandpass output would be 5x the input level (+14 dB). The wah kernel normalizes the transfer function directly (`WahKernel::process_stereo`), so Resonance doesn't change the level at the center frequency:

```rust
let safe_q = if resonance > 0.0 { resonance } else { 1.0 };
let normalized_l = filtered_l / safe_q;       // Unity peak gain at any Q
let out_l = normalized_l * 0.8 + left * 0.2;  // 80/20 wet/dry blend for body
```

At the center frequency, `(input * Q) / Q * 0.8 + input * 0.2 = input`, which is unity gain. The normalization is algebraically exact regardless of Q. Unity gain at the center frequency does not give unity level on a guitar, though: at its defaults the wah measures 12.2 dB below its input on the reference guitar, and its level trim (+12.2 dB, applied by the adapter) restores the level.

### Level-Preserving Mix

**Source:** `crates/sonido-core/src/level.rs`, `crates/sonido-core/src/kernel/traits.rs`

Every effect with a mix control mixes through `DspKernel::blend` (or `blend_stereo`), which applies the kernel's `LEVEL`:

```rust
fn blend(dry: f32, wet: f32, mix: f32) -> f32 {
    level::blend(dry, wet, mix, &Self::LEVEL)  // dry·g_d + wet·trim·g_w
}
```

A linear crossfade, `dry·(1−m) + wet·m`, keeps the level only when wet and dry are equally loud and fully correlated. Two signals with correlation ρ at gains `a` and `b` sum to power `a² + b² + 2ρab`. For an uncorrelated wet (a reverb tail, an echo), the linear crossfade at `m = 0.5` has power 0.5, a 3 dB dip. A wet that partly cancels the dry (ρ < 0) dips further.

`mix_gains` starts from the constant-power sine/cosine law and normalizes it by the blend's power at the measured correlation:

```
cos²(πm/2) + sin²(πm/2) + 2ρ·cos(πm/2)·sin(πm/2) = 1 + ρ·sin(πm)

k   = 1/√(1 + ρ·sin(πm))
g_d = k·cos(πm/2)
g_w = k·sin(πm/2)
```

The blend's loudness is then the same at every mix for that correlation:

- **ρ = 0** (reverbs, echoes): `k = 1`, the plain constant-power crossfade.
- **ρ = 1** (saturation): `g_d + g_w = 1`, so amplitude is preserved as in a linear crossfade.
- **ρ < 0** (partly cancelling wets, such as a vibrato's comb): `k > 1`, the boost the blend needs.

Unlike a `√m` law, the sine law has a finite slope at both ends, so the first turn of a mix knob fades the wet in instead of jumping to -20 dB (`√0.01 = 0.1`). Mix 0 and 1 return exactly `(1, 0)` and `(0, 1)`.

`mix_gains` runs per sample in every effect with a mix control, so it uses no transcendentals (libm's `sqrtf` is a software routine on the Cortex-M7). `quarter_sine` is a fifth-order odd polynomial for `sin(πx/2)`, exact at `x = 1` and within 6e-4 elsewhere. `k` is a bit-trick inverse square root refined by two Newton steps (within 5 ppm). Because `k` normalizes by the polynomial gains' actual power, the polynomial's own deviation from constant power cancels too.

The linear helpers `wet_dry_mix()` / `wet_dry_mix_stereo()` (`crates/sonido-core/src/math.rs`, `dry + (wet - dry) * mix`) are plain crossfades. The graph engine uses `wet_dry_mix` for a node's bypass fade (`crates/sonido-core/src/graph/processing.rs`); kernels don't use them.

---

## Soft Limiting (Safety Stage)

**Source:** `crates/sonido-core/src/math.rs`

Soft limiting is a safety measure applied as the final processing stage, before the output level parameter. Its only purpose is to guarantee bounded output under all parameter combinations; it is not meant as a musical effect.

### Algorithm

The limiter uses a knee-based design with hyperbolic tangent compression:

```
          ┌─ x                                      if |x| ≤ threshold
f(x) =   │
          └─ sign(x) · (threshold + headroom · tanh((|x| - threshold) / headroom))   otherwise
```

Where:
- `threshold = ceiling × 0.9` (knee onset at 90% of ceiling)
- `headroom = ceiling - threshold` (10% of ceiling reserved for compression)
- `ceiling` = maximum output magnitude (typically 1.0 for 0 dBFS)

### Transfer Function Properties

- **Transparent below knee:** Signals below 90% of ceiling pass through unmodified
- **Smooth transition:** The tanh function provides a continuously differentiable knee, with no discontinuities that would produce clicks or harmonic artifacts
- **Hard ceiling guarantee:** As `|x| → ∞`, `tanh → 1.0`, so output asymptotically approaches but never exceeds `ceiling`
- **Odd symmetry:** `f(-x) = -f(x)`, so the limiter itself generates no even harmonics

### Usage Pattern

It is applied in effects where nonlinear processing or resonant filters can produce unbounded gain:

```rust
// In the kernel, after DSP and mix, before output level
(soft_limit(mixed_l, 1.0) * output, soft_limit(mixed_r, 1.0) * output)
```

Kernels using this pattern with a 1.0 ceiling: chorus, compressor, distortion, eq, filter, preamp, tape, wah. `harmonic_habitat` limits at 3.0 and `spring_reverb` limits its dispersion chain at 1.5. `soft_limit_stereo()` applies it to a pair. Effects without a resonant or nonlinear gain stage do not need it. In chorus, compressor and distortion the level trim is inside the blend, before the limiter; eq, filter, preamp, tape and wah have no mix control, so the adapter's level trim follows the limiter and scales its ceiling (wah's trim is +12.2 dB).

### Reference

Knee-based soft limiting framework: Giannoulis et al., "Digital Dynamic Range Compressor Design: A Tutorial and Analysis," *J. Audio Eng. Soc.* 60(6), 2012.

---

## Shared DSP Vocabulary

**Source:** `crates/sonido-core/src/gain.rs`, `param.rs`, `param_info.rs`, `one_pole.rs`, `math.rs`

Sonido extracts recurring DSP patterns into a shared vocabulary in `sonido-core`. This removes copy-paste duplication across the 36 effects and gives them consistent conventions.

### SmoothedParam Presets

Effects use named presets with documented meanings for the common smoothing times (`10.0`, `5.0`, `20.0`, `50.0`), so these values do not appear as magic numbers in constructors:

| Preset | Time | Use Case |
|--------|------|----------|
| `SmoothedParam::fast()` | 5 ms | Drive, nonlinear gain (needs fast response) |
| `SmoothedParam::standard()` | 10 ms | Most params (rate, depth, mix, level) |
| `SmoothedParam::slow()` | 20 ms | Filter coefficients, EQ, reverb decay |
| `SmoothedParam::interpolated()` | 50 ms | Delay time, predelay (avoids pitch artifacts) |

### ParamDescriptor Factories

Common parameter descriptors are constructed via factory methods rather than inline struct literals:

- `ParamDescriptor::mix()`: 0-100%, default 50%
- `ParamDescriptor::depth()`: 0-100%, default 50%
- `ParamDescriptor::feedback()`: 0-95%, default 50%
- `ParamDescriptor::time_ms(name, short, min, max, default)`: custom time param
- `ParamDescriptor::gain_db(name, short, min, max, default)`: custom gain param

`.with_default(v)` overrides a factory's default with the value that sounds best on that effect (chorus: `ParamDescriptor::depth().with_default(60.0)`). The descriptor default is what a fresh plugin starts at and what a pedal knob at noon selects, and `XxxParams::default()` must match it (`crates/sonido-effects/tests/default_parity.rs`).

### OnePole Filter

**Source:** `crates/sonido-core/src/one_pole.rs`

A minimal one-pole (6 dB/octave) lowpass filter used for tone controls, HF rolloff, and reverb feedback damping:

```rust
let mut tone = OnePole::new(48000.0, 4000.0);
let filtered = tone.process(input);  // one-pole lowpass at 4 kHz
```

The coefficient `exp(-2*pi*freq/sample_rate)` gives the standard one-pole response with -3 dB at the specified frequency.

---

## Fast Math Approximations

Sonido provides fast mathematical approximations for targets without
hardware transcendental support (Cortex-M7, etc.). These trade full
IEEE 754 precision for predictable cycle counts.

### Function Summary

| Function | Replaces | Use Case | Max Error |
|----------|----------|----------|-----------|
| `fast_log2(x)` | `libm::logf` / log2 | dB conversion, dynamics | < 0.2% relative |
| `fast_exp2(x)` | `libm::expf` / exp2 | dB conversion, dynamics | < 0.2% relative |
| `fast_db_to_linear(db)` | `db_to_linear()` | Gain, level metering | < 0.05 dB |
| `fast_linear_to_db(lin)` | `linear_to_db()` | Gain, level metering | < 0.05 dB |
| `fast_sin_turns(turns)` | `libm::sinf` | LFO modulation | < 0.001 absolute |
| `fast_tan(x)` | `libm::tanf` | Filter coefficients | < 0.2% relative below 7.6 kHz at 48 kHz (< 2% below 15.3 kHz) |

### Implementation Techniques

- **`fast_log2` / `fast_exp2`**: IEEE 754 float bit decomposition (Remez minimax polynomial). ~10-15 Cortex-M7 cycles vs ~150-200 for libm equivalents.
- **`fast_sin_turns`**: Bhaskara I parabolic approximation. Input in turns (0.0-1.0 = full cycle). ~16 cycles vs ~100+ for sinf.
- **`fast_tan`**: Padé [2/1] rational approximation (spurious pole at x ~ 1.58). Accurate for filter coefficients well below Nyquist; the SVF switches to `libm::tanf` above 10 kHz. ~12 cycles.
- **`fast_db_to_linear` / `fast_linear_to_db`**: Composed from fast_exp2/fast_log2 with dB scaling.

### When to Use

Use fast_math functions in hot DSP loops on embedded targets (Cortex-M7) where transcendental calls dominate cycle budget. The error margins are inaudible for all documented use cases (LFO modulation, dynamics, filter coefficients). For offline processing or non-embedded targets, prefer the standard `libm` functions.

Used by `Lfo` (`fast_sin_turns`), the SVF (`fast_tan`), and most effect kernels (`fast_db_to_linear` for output gain); see ADR-020.

### References

- ARM Cortex-M7 Technical Reference Manual (FPU instruction timings)
- Remez exchange algorithm (minimax polynomial fitting)
- Bhaskara I's sine approximation (7th century, parabolic form)
- Padé approximants for rational function approximation

**Implementation:** `crates/sonido-core/src/fast_math.rs`

---

## Denormal Protection

**Source:** `crates/sonido-core/src/math.rs` (`flush_denormal`), `crates/sonido-core/src/dc_blocker.rs`

### The Subnormal Problem

IEEE 754 floating-point numbers have a special category called **subnormal** (or **denormalized**) numbers. These are values between zero and the smallest normal float (~1.175e-38 for f32). Subnormals are mathematically valid, but processing them incurs a severe CPU penalty on most architectures: up to 100x slower than normal float operations on x86 and ARM. The hardware fast path cannot handle subnormals, so they are processed in microcode.

In audio DSP, subnormals arise naturally in feedback loops: comb filters, allpass filters, delay lines with feedback, and IIR filters all produce decaying signals that eventually enter the subnormal range. Without mitigation, a reverb tail fading to silence can cause a large spike in CPU load.

### Flush Strategy: `flush_denormal()`

Sonido uses an explicit value-threshold flush function:

```rust
#[inline(always)]
pub fn flush_denormal(x: f32) -> f32 {
    if x.abs() < 1e-20 { 0.0 } else { x }
}
```

The threshold of 1e-20 is well above the IEEE 754 subnormal range (~1.2e-38) but far below any audible signal level. This provides a comfortable margin: values below 1e-20 are inaudible (at -400 dBFS), and if left alone they would only decay further into the subnormal range.

### Where flush_denormal Is Applied

1. **Comb filter feedback** (`comb.rs`): The `filterstore` variable in the one-pole damping filter, and the `ModulatedComb` feedback write, are flushed each sample, preventing the feedback path from sustaining subnormal values indefinitely.
2. **Biquad state** (`biquad.rs`): The TDF-II state registers `s1` and `s2` are flushed, as IIR filters with poles near the unit circle can produce slowly-decaying subnormals.
3. **SVF integrators** (`svf.rs`): `ic1eq` and `ic2eq` are flushed each sample.
4. **Kernel feedback paths**: e.g. the reverb FDN write, delay and looper feedback writes, phaser feedback sample, plate and spring reverb tanks.

### DC Blocker: `DcBlocker`

**Source:** `crates/sonido-core/src/dc_blocker.rs`

The DC blocker is a first-order highpass filter based on Julius O. Smith's design. Its transfer function is:

```
H(z) = (1 - z^-1) / (1 - R * z^-1)
```

where R is a coefficient close to 1.0 (`DcBlocker::new` targets a 7 Hz cutoff, R ~0.9991 at 48 kHz, via `f_c = (1 - R) / (2 * pi) * f_s`). The DC blocker removes any DC offset that accumulates through nonlinear processing, feedback loops, or asymmetric waveshaping. It is particularly useful after:

- Asymmetric distortion (which generates even harmonics and introduces DC)
- Feedback loops that can accumulate DC through integration
- Compressor makeup gain that can shift the baseline

Reference: Julius O. Smith, "Introduction to Digital Filters with Audio Applications"

### Alternatives Not Chosen

- **FTZ/DAZ CPU flags**: Setting the Flush-To-Zero and Denormals-Are-Zero flags on x86 (MXCSR register) or ARM (FPSCR register) would globally flush subnormals at the hardware level. This was not chosen because: (a) it requires platform-specific code, (b) it affects all floating-point operations in the thread, not just audio, (c) it is unavailable in `no_std` environments without inline assembly, and (d) it may interfere with other libraries running in the same thread.
- **DC offset injection**: Adding a tiny DC offset (~1e-15) to feedback signals prevents values from reaching zero and entering the subnormal range. This was not chosen because it introduces actual DC into the signal path, which can accumulate through cascaded effects and cause headroom issues.

---

## Numerical Considerations

### `no_std` Math

Sonido uses `libm` for all transcendental math functions (`sinf`, `cosf`, `tanf`, `expf`, `powf`, `log10f`, etc.) instead of the standard library's `f32` methods. This enables compilation on embedded targets without an FPU or standard library, at the cost of slightly different rounding behavior compared to hardware FPU implementations.

### Fixed-Point Alternatives

The `FixedDelayLine<const N: usize>` uses const generics for stack allocation, avoiding all heap allocation. This is critical for embedded targets like the Daisy Seed (STM32H750, 480 MHz ARM Cortex-M7) where dynamic allocation is either unavailable or has unpredictable latency.

### DC Stability

Several mechanisms prevent DC offset accumulation:

1. **Leaky integrators**: The triangle oscillator uses a leak of `1 - min(f / fs, 0.1)` instead of `1.0` as the integrator coefficient, so any DC accumulation decays away
2. **Allpass structure**: The Schroeder allpass (`y[n] = v[n−D] − g·v[n]`) has unity gain at every frequency, DC included, so it adds no DC gain to a feedback loop (Freeverb's `y = −x + v[n−D]` shortcut has gain `g/(1−g)` at DC; see [Allpass Diffusion](#allpass-diffusion))
3. **Feedback clamping**: Comb, allpass, and reverb FDN feedback are clamped below 1.0 (maximum 0.99), so energy always decays

### Float Precision

Sonido uses `f32` exclusively. For audio at 48 kHz, 32-bit float provides approximately 24 bits of mantissa precision (144 dB dynamic range), which exceeds the requirements of 24-bit audio converters. Double precision (`f64`) would provide no audible benefit while doubling memory bandwidth and reducing cache efficiency.
