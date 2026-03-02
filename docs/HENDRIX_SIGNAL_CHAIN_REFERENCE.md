# Hendrix Signal Chain — Reference for Sonido Development

Source: ["Jimi Hendrix Was a Systems Engineer"](https://spectrum.ieee.org/jimi-hendrix-systems-engineer), Rohan S. Puranik, IEEE Spectrum, Feb 2026.
Companion repo: [github.com/nahorov/Hendrix-Systems-Lab](https://github.com/nahorov/Hendrix-Systems-Lab) — full SPICE netlists + Python DSP pipeline.

## Why This Matters to Sonido

Hendrix's rig is a canonical example of the exact problem Sonido solves: a modular analog signal chain where each stage's nonlinear behavior interacts with every other stage, and the whole is greater than the sum of the parts. The article reverse-engineers this chain with SPICE simulations and Python DSP — tools that map directly onto Sonido's architecture.

The Hendrix Systems Lab repo is also a potential **validation target**: its SPICE-derived waveforms and audio stems could serve as reference signals to test Sonido's effect implementations against physically-grounded circuit models.

---

## Signal Chain Mapping: Hendrix Rig → Sonido

| Hendrix Stage | Function | Sonido Equivalent | Gap / Opportunity |
|---|---|---|---|
| Guitar pickups (6k ohm, 2.5H) | Source with reactive impedance | — | Source impedance modeling not in Sonido; affects interaction with low-impedance pedals |
| **Fuzz Face** (Si or Ge) | Two-transistor feedback amp; soft+hard clip mix | `Distortion` (4 algorithms + ADAA) | Sonido has `SoftClip` (tanh) and `HardClip` — the Fuzz Face uses a *mix* of both (0.55 soft + 0.45 hard). Also: the "cleanup effect" (fuzz character changes with input amplitude) is a nonlinear input-impedance interaction, not just a waveshaper. |
| **Octavia** | Full-wave rectifier → frequency doubling | **Not in Sonido** | `abs(x)` as octave doubler is a distinct waveshaping mode. Not fuzz, not ring mod — it's rectification. Could be a new `Distortion` mode or standalone effect. |
| **Wah-wah** (Vox V847) | Bandpass filter, 300Hz–2kHz sweep | `Wah` effect | Direct match. Sonido's Wah uses SVF; Hendrix's was inductor-capacitor. Compare Q profiles. |
| **Uni-Vibe** | 4-stage allpass + LFO-modulated photoresistors | `Phaser` | Sonido's Phaser is allpass-cascade + LFO — structurally identical. Key difference: Uni-Vibe adds AM "throb" (half-rate amplitude modulation on top of phase modulation). Sonido's Phaser may lack this. |
| **Marshall 100W** (near saturation) | Tube saturation + sustain extension | `Tape` / `CleanPreamp` | `Tape` has soft saturation. But tube amp saturation has specific even/odd harmonic ratios and power supply sag dynamics not modeled. |
| **Room acoustics** (feedback loop) | Guitar ↔ speaker coupling, position-dependent | `Reverb` + feedback path | The acoustic feedback loop is the most interesting part: Hendrix tuned oscillation by physical position. In Sonido terms, this is a `Reverb` whose feedback parameter is modulated by an external control signal. |

---

## Key DSP Insights from the Hendrix Systems Lab

### 1. Octavia — Rectification as Frequency Doubling

The Octavia's core mechanism: `abs(sin(2πft))` maps frequency `f` to `2f` by folding negative half-cycles positive. This is not distortion in the harmonic-addition sense — it's a **frequency transformation** via waveform symmetry.

SPICE model (behavioral):
```spice
BRECT RECT 0 V='abs(V(IN))'
BOUT OUT 0 V='limit(2*V(RECT) - 0.2, -1.5, 1.5)'
```

Python model:
```python
rect = np.abs(x)
out = np.tanh(6 * (rect - 0.1))  # asymmetric saturation on rectified signal
```

**Sonido implementation path**: New `Distortion` variant `Rectify` or standalone `Octavia` effect. The antiderivative of `abs(x)` is `x*|x|/2` — ADAA-compatible. Chain it: Fuzz → Octavia produces the Purple Haze tone.

### 2. Fuzz Face — The Cleanup Effect

The Fuzz Face's input impedance is ~20k ohm — low enough that the guitar's pickup inductance (2.5H) forms a resonant circuit with the pedal's input network. When guitar volume is reduced:
- Lower drive → waveshaper operates in its linear region → sinusoidal output
- Higher drive → clipping → fuzz

This is **not just an amplitude effect** — the frequency response changes too, because the pickup/pedal interaction is reactive (inductance + capacitance). The "cleanup" is a nonlinear, frequency-dependent transition.

**Sonido relevance**: The `EnvelopeFollower` could modulate distortion drive dynamically, but the impedance interaction is deeper. This is a case where modeling the source-load interaction matters. Worth noting for any "amp sim" or "pedal chain" preset work.

### 3. Germanium vs Silicon — Temperature as Parameter

The Hendrix Systems Lab models both Ge and Si Fuzz Face variants. Key differences:

| Parameter | Silicon | Germanium |
|---|---|---|
| Beta (BF) | 200 | 120 |
| Saturation current (IS) | 1e-14 | 5e-9 |
| Junction capacitance (CJE) | 8pF | 20pF |
| Base resistance (RB) | ~0 | 200 ohm |

Germanium: lower gain, softer clipping knee, more HF rolloff (higher junction capacitance), and **temperature-dependent bias drift**. The repo includes temperature sweep analysis (`temp_bias_plot.py`) showing Q-point shift from 0-50°C.

**Sonido relevance**: The Si/Ge distinction maps to different `Distortion` parameter presets. The temperature drift is interesting as a slow modulation source — an LFO-modulated bias offset that changes the clipping symmetry over time, simulating a warming amplifier.

### 4. Uni-Vibe's AM + PM Duality

The Uni-Vibe isn't just a phaser — it's phase modulation (4-stage allpass) **plus** amplitude modulation (the lamp brightness varies the signal level, not just the phase). The AM component runs at half the PM rate, creating a slow "throb" underneath the phase sweep.

Python model of the AM layer:
```python
y *= 0.9 * (1 + 0.15 * np.sin(2 * np.pi * (rate / 2) * t))
```

**Sonido relevance**: Check if `Phaser` includes an AM component. If not, adding a `depth_am` parameter that applies half-rate amplitude modulation would capture the Uni-Vibe character that distinguishes it from a pure phaser.

### 5. Chain Order Matters — Nonlinear Interactions

The default Hendrix chain: `guitar → fuzz → wah → univibe → octavia → amp`

The Hendrix Systems Lab's `--chain` flag lets you reorder stages. This matters because nonlinear stages don't commute:
- Fuzz → Wah: fuzz generates harmonics, wah sweeps across them (Hendrix's typical setup)
- Wah → Fuzz: wah narrows the band first, fuzz distorts only the filtered signal (cleaner, more focused)

**Sonido relevance**: The `ProcessingGraph` DAG engine already supports arbitrary routing. But preset design should be informed by these interaction patterns. A "Hendrix Rig" preset chain with the historically correct order would be a good demo.

### 6. Gain Staging — Normalize Once at End

The Hendrix Systems Lab normalizes audio **only once, at the final output**, preserving realistic gain staging through all nonlinear stages. Intermediate clipping/saturation states are not artificially leveled.

This matches Sonido's gain compensation approach in ADR-019 (feedback gain compensation) and is the correct practice for any chain where nonlinear effects depend on input level.

---

## Validation Opportunities

The Hendrix Systems Lab repo provides SPICE-derived reference data that could validate Sonido's implementations:

| Hendrix Lab Output | Sonido Validation Target |
|---|---|
| Fuzz Face transient waveforms (`ff_si_time.dat`) | `Distortion::SoftClip` + `HardClip` mix output |
| Wah frequency response at 6+ treadle positions | `Wah` frequency sweep comparison |
| Uni-Vibe phase response at 6 LFO positions | `Phaser` allpass cascade response |
| Octavia transient (rectified + shaped) | Future `Rectify` mode |
| Full chain WAV stems (48kHz/24bit) | `ProcessingGraph` chain output |

The SPICE models use physically-grounded component values (real transistor parameters, measured inductances). Comparing Sonido's mathematical models against these would quantify how close the DSP approximations are to the analog circuits.

---

## New Effect Candidates

### Octavia (Rectifier Octave-Up)
- **Algorithm**: `abs(x)` → asymmetric saturation → tone filter
- **ADAA**: Antiderivative of `abs(x)` = `x*|x|/2` — first-order ADAA compatible
- **Parameters**: Drive (pre-rect gain), Tone (post-rect LP cutoff), Mix (dry/wet)
- **Category**: Distortion (but fundamentally different from harmonic distortion)

### Fuzz (Vintage Two-Transistor)
- **Algorithm**: Mixed soft/hard clip with pre-emphasis LP filter
- **Key feature**: "Cleanup" — drive-dependent character transition
- **Parameters**: Fuzz (drive), Volume, Tone, Mode (Si/Ge preset curves)
- **Differs from `Distortion`**: The soft/hard mix ratio and pre-emphasis filter are specific to the Fuzz Face topology

---

## References

- Puranik, R.S. "Jimi Hendrix Was a Systems Engineer." IEEE Spectrum, March 2026.
- [Hendrix Systems Lab](https://github.com/nahorov/Hendrix-Systems-Lab) — SPICE netlists, Python DSP, WAV stems
- Robert Bristow-Johnson, "Audio EQ Cookbook" — filter coefficients used in both Sonido and the wah model
- Valimäki et al., "Digital Audio Effects" — theoretical foundation for Sonido's effect implementations
