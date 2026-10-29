# Cross-Frequency Coupling Analysis Guide

Complete guide to Phase-Amplitude Coupling (PAC) analysis in Sonido for biosignal research.

## Overview

Cross-Frequency Coupling (CFC) refers to interactions between neural oscillations at different frequencies. The most studied form is **Phase-Amplitude Coupling (PAC)**, where the phase of a slow oscillation modulates the amplitude of a faster oscillation.

PAC is observed in:
- **EEG/MEG**: Theta-gamma coupling during memory encoding and retrieval
- **Electric fish**: Communication signals and jamming avoidance
- **Slime mold**: Electrical oscillations in Physarum
- **Neural recordings**: Local field potentials in hippocampus, cortex

Sonido provides tools for PAC analysis through the `sonido-analysis` crate, accessible via both Rust API and CLI.

---

## Quick Start

### CLI

```bash
# Analyze theta-gamma coupling
sonido analyze pac recording.wav \
    --phase-low 4 --phase-high 8 \
    --amp-low 30 --amp-high 100 \
    --method mvl \
    --surrogates 200 \
    --output pac_results.json
```

### Rust API

```rust
use sonido_analysis::cfc::{PacAnalyzer, PacMethod};
use sonido_analysis::filterbank::eeg_bands;

// Create analyzer for theta-gamma coupling
let mut analyzer = PacAnalyzer::new(
    1000.0,              // sample rate
    eeg_bands::THETA,    // phase band: 4-8 Hz
    eeg_bands::LOW_GAMMA // amplitude band: 30-80 Hz
);

analyzer.set_method(PacMethod::MeanVectorLength);

let signal = vec![0.0; 10000];  // Your signal
let result = analyzer.analyze(&signal);

println!("Modulation Index: {:.4}", result.modulation_index);
println!("Preferred Phase: {:.2} rad ({:.1} deg)",
    result.preferred_phase, result.preferred_phase_degrees());
```

---

## Theoretical Background

### Phase-Amplitude Coupling

PAC quantifies how the amplitude envelope of a high-frequency oscillation varies systematically with the phase of a low-frequency oscillation.

The analysis pipeline:
1. Bandpass filter the signal into phase and amplitude bands
2. Hilbert transform to extract instantaneous phase and amplitude
3. Compute coupling metric (MVL or KL divergence)

### Modulation Index

The **Modulation Index (MI)** ranges from 0 to 1:
- **MI = 0**: No coupling (amplitude is uniform across all phases)
- **MI = 1**: Perfect coupling (amplitude only occurs at one phase)
- **MI > 0.1**: Often considered significant (with statistical testing)

### Preferred Phase

The **preferred phase** indicates at which phase of the slow oscillation the high-frequency amplitude is maximal. In hippocampal theta-gamma coupling, for example, gamma bursts often peak at a specific phase of theta.

---

## Filter Bank

The filter bank extracts frequency bands using 4th-order Butterworth bandpass filters.

### Predefined EEG Bands

```rust
use sonido_analysis::filterbank::eeg_bands;

// Standard EEG bands
let delta = eeg_bands::DELTA;       // 0.5-4 Hz (deep sleep)
let theta = eeg_bands::THETA;       // 4-8 Hz (memory, drowsiness)
let alpha = eeg_bands::ALPHA;       // 8-13 Hz (relaxed wakefulness)
let beta = eeg_bands::BETA;         // 13-30 Hz (active thinking)
let low_gamma = eeg_bands::LOW_GAMMA;   // 30-80 Hz (perception)
let high_gamma = eeg_bands::HIGH_GAMMA; // 80-200 Hz (fine processing)
```

### Custom Bands

```rust
use sonido_analysis::filterbank::FrequencyBand;

// Custom frequency band
let custom_band = FrequencyBand::new("my_band", 6.0, 10.0);

// Band properties
println!("Center: {:.1} Hz", custom_band.center_hz());
println!("Bandwidth: {:.1} Hz", custom_band.bandwidth());
```

### Multi-Band Extraction

```rust
use sonido_analysis::filterbank::{FilterBank, eeg_bands};

let bands = [eeg_bands::THETA, eeg_bands::ALPHA, eeg_bands::BETA];
let mut bank = FilterBank::new(1000.0, &bands);

let signal = vec![0.0; 10000];
let extracted = bank.extract(&signal);

// extracted[0] = theta band
// extracted[1] = alpha band
// extracted[2] = beta band
```

---

## Hilbert Transform

The Hilbert transform extracts instantaneous phase and amplitude from a signal.

```rust
use sonido_analysis::hilbert::HilbertTransform;

let hilbert = HilbertTransform::new(4096);  // FFT size

// Get phase and amplitude together (efficient)
let (phase, amplitude) = hilbert.phase_and_amplitude(&signal);

// Or separately
let phase = hilbert.instantaneous_phase(&signal);      // radians, -PI to PI
let amplitude = hilbert.instantaneous_amplitude(&signal);

// Unwrap phase to remove discontinuities
let unwrapped = HilbertTransform::unwrap_phase(&phase);

// Compute instantaneous frequency
let inst_freq = hilbert.instantaneous_frequency(&signal, sample_rate);
```

---

## PAC Analysis

### PacAnalyzer

```rust
use sonido_analysis::cfc::{PacAnalyzer, PacMethod, PacResult};
use sonido_analysis::filterbank::FrequencyBand;

// Define frequency bands
let phase_band = FrequencyBand::new("theta", 4.0, 8.0);
let amp_band = FrequencyBand::new("gamma", 30.0, 80.0);

// Create analyzer
let mut analyzer = PacAnalyzer::new(1000.0, phase_band, amp_band);
analyzer.set_method(PacMethod::MeanVectorLength);

let result = analyzer.analyze(&signal);
```

### PAC Methods

| Method | Description | Use Case |
|--------|-------------|----------|
| `MeanVectorLength` | Canolty et al. 2006. Fast, intuitive. | General purpose, real-time |
| `KullbackLeibler` | Tort et al. 2010. Measures deviation from uniform. | Research, detailed analysis |

### PacResult Fields

| Field | Description |
|-------|-------------|
| `modulation_index` | Coupling strength (0-1) |
| `preferred_phase` | Phase of max amplitude (radians) |
| `preferred_phase_degrees()` | Phase in degrees |
| `mean_amplitude_per_phase` | Amplitude histogram (18 bins x 20 deg) |
| `is_significant(threshold)` | Quick significance check |

### Phase-Amplitude Histogram

The result includes amplitude averaged in 18 phase bins (20 degrees each):

```rust
let result = analyzer.analyze(&signal);

println!("Amplitude by phase:");
for (i, &amp) in result.mean_amplitude_per_phase.iter().enumerate() {
    let phase_start = -180.0 + i as f32 * 20.0;
    println!("  {:>4.0} - {:>4.0} deg: {:.4}", phase_start, phase_start + 20.0, amp);
}
```

---

## Comodulogram

A comodulogram shows PAC across multiple frequency pairs, so you can see which combinations have the strongest coupling.

### CLI

```bash
sonido analyze comodulogram recording.wav \
    --phase-range 2-20 \
    --amp-range 20-200 \
    --phase-step 2 \
    --amp-step 10 \
    --bandwidth 0.5 \
    --output comodulogram.csv
```

### Rust API

```rust
use sonido_analysis::cfc::Comodulogram;

let como = Comodulogram::compute(
    &signal,
    1000.0,              // sample rate
    (2.0, 20.0, 2.0),    // phase: 2-20 Hz, step 2
    (20.0, 200.0, 10.0), // amplitude: 20-200 Hz, step 10
    0.5,                 // bandwidth = 50% of center freq
);

// Find peak coupling
let (peak_phase, peak_amp, peak_mi) = como.peak_coupling();
println!("Peak coupling: {:.1} Hz phase, {:.1} Hz amplitude, MI={:.4}",
    peak_phase, peak_amp, peak_mi);

// Export to CSV for visualization
let csv = como.to_csv();
std::fs::write("comodulogram.csv", csv)?;

// Query specific frequency pair
if let Some(mi) = como.get_coupling(6.0, 50.0) {
    println!("Theta-gamma coupling: {:.4}", mi);
}
```

### Visualization

Export to CSV and visualize with Python:

```python
import pandas as pd
import matplotlib.pyplot as plt
import numpy as np

# Load comodulogram
df = pd.read_csv('comodulogram.csv', index_col='phase_hz')

# Plot
fig, ax = plt.subplots(figsize=(10, 8))
im = ax.imshow(df.values.T, aspect='auto', origin='lower',
               extent=[df.index.min(), df.index.max(),
                       float(df.columns[0]), float(df.columns[-1])],
               cmap='hot')
ax.set_xlabel('Phase Frequency (Hz)')
ax.set_ylabel('Amplitude Frequency (Hz)')
ax.set_title('Phase-Amplitude Coupling Comodulogram')
plt.colorbar(im, label='Modulation Index')
plt.savefig('comodulogram.png', dpi=150)
```

---

## Statistical Testing with Surrogates

PAC can arise from noise and filtering artifacts. Surrogate testing establishes significance:

### CLI

```bash
sonido analyze pac recording.wav \
    --phase-low 4 --phase-high 8 \
    --amp-low 30 --amp-high 100 \
    --surrogates 200 \
    --output pac_results.json
```

### Interpretation

- **p < 0.05**: Significant coupling (observed MI exceeds 95% of surrogates)
- **p < 0.01**: Highly significant coupling
- **200+ surrogates**: Recommended for reliable p-values

### Surrogate Methods

The CLI uses time-shifted surrogates (random circular shift), which preserve the spectral content but destroy phase relationships. This is appropriate for most PAC analyses.

---

## CLI Reference

### pac

Analyze Phase-Amplitude Coupling between two frequency bands.

```bash
sonido analyze pac <INPUT> [OPTIONS]
```

| Option | Description | Default |
|--------|-------------|---------|
| `--phase-low <HZ>` | Phase band lower frequency | 4.0 |
| `--phase-high <HZ>` | Phase band upper frequency | 8.0 |
| `--amp-low <HZ>` | Amplitude band lower frequency | 30.0 |
| `--amp-high <HZ>` | Amplitude band upper frequency | 100.0 |
| `--method <METHOD>` | `mvl` or `kl` | mvl |
| `--surrogates <N>` | Surrogate iterations (0=disabled) | 0 |
| `-o, --output <FILE>` | Output JSON file | - |

**Output includes:**
- Modulation index
- Preferred phase (radians and degrees)
- Phase-amplitude histogram
- p-value (if surrogates > 0)

### comodulogram

Compute coupling across multiple frequency pairs.

```bash
sonido analyze comodulogram <INPUT> -o <OUTPUT.csv> [OPTIONS]
```

| Option | Description | Default |
|--------|-------------|---------|
| `--phase-range <LOW-HIGH>` | Phase frequency range | 2-20 |
| `--amp-range <LOW-HIGH>` | Amplitude frequency range | 20-200 |
| `--phase-step <HZ>` | Phase frequency step | 2.0 |
| `--amp-step <HZ>` | Amplitude frequency step | 10.0 |
| `--bandwidth <RATIO>` | Bandwidth as fraction of center | 0.5 |
| `-o, --output <FILE>` | Output CSV file (required) | - |

### bandpass

Extract a frequency band using bandpass filtering.

```bash
sonido analyze bandpass <INPUT> -o <OUTPUT.wav> --low <HZ> --high <HZ>
```

| Option | Description |
|--------|-------------|
| `--low <HZ>` | Lower cutoff frequency |
| `--high <HZ>` | Upper cutoff frequency |
| `--order <N>` | Filter order (2, 4, or 6) |
| `-o, --output <FILE>` | Output WAV file |

### hilbert

Extract instantaneous phase and amplitude.

```bash
sonido analyze hilbert <INPUT> [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--phase-output <FILE>` | Output WAV for phase |
| `--amp-output <FILE>` | Output WAV for amplitude envelope |
| `--bandpass <LOW-HIGH>` | Optional pre-filtering |

---

## Research Applications

### NOW Model (Justin Riddle)

The NOW (Neural Oscillatory Window) model proposes that consciousness emerges from nested oscillatory dynamics. PAC analysis can quantify:

- **Theta-gamma coupling**: Working memory window
- **Alpha-beta coupling**: Attentional gating
- **Cross-frequency phase-phase coupling**: Information binding

Example analysis pipeline:

```bash
# 1. Extract theta-gamma coupling during task vs rest
sonido analyze pac task_eeg.wav --phase-low 4 --phase-high 8 \
    --amp-low 30 --amp-high 80 --surrogates 500 -o task_pac.json

sonido analyze pac rest_eeg.wav --phase-low 4 --phase-high 8 \
    --amp-low 30 --amp-high 80 --surrogates 500 -o rest_pac.json

# 2. Generate comodulogram to find other coupling patterns
sonido analyze comodulogram task_eeg.wav --phase-range 2-20 \
    --amp-range 20-150 -o task_comod.csv
```

### Electric Fish EOD Analysis

Electric fish modulate their discharge frequency for communication:

```bash
# Analyze frequency modulation patterns
sonido analyze hilbert eod_recording.wav \
    --bandpass 200-800 \
    --amp-output eod_envelope.wav

# Look for modulation of EOD by slower rhythms
sonido analyze pac eod_recording.wav \
    --phase-low 0.1 --phase-high 2 \
    --amp-low 200 --amp-high 600
```

### Physarum (Slime Mold) Oscillations

Slime mold electrical oscillations are slow (0.01-0.1 Hz). Prepare data:

```python
# Convert voltage timeseries to WAV
import numpy as np
from scipy.io import wavfile

voltage_data = np.load('physarum_voltage.npy')  # mV readings
sample_rate = 2  # 2 samples per second (0.5s intervals)

# Normalize to [-1, 1]
normalized = voltage_data / np.max(np.abs(voltage_data))
wavfile.write('physarum.wav', sample_rate, normalized.astype(np.float32))
```

Then analyze:

```bash
# Look for coupling between slow oscillations
sonido analyze pac physarum.wav \
    --phase-low 0.01 --phase-high 0.05 \
    --amp-low 0.05 --amp-high 0.2
```

---

## Best Practices

### Signal Preparation

1. **Adequate duration**: At least 10-20 cycles of the lowest frequency
2. **Artifact rejection**: Remove movement artifacts, line noise
3. **Stationarity**: Analyze epochs where signal properties are stable

### Frequency Band Selection

1. **Non-overlapping bands**: Phase band high < Amplitude band low
2. **Appropriate bandwidth**: Narrower bands = more specific, wider = more robust
3. **Physiological relevance**: Use bands appropriate for your signal type

### Interpretation

1. Always use surrogate testing for significance
2. Report effect size (MI) along with p-value
3. Consider multiple comparisons when testing many frequency pairs
4. Validate with simulations using synthetic coupled signals

### Common Pitfalls

- **Edge effects**: First/last samples affected by filtering
- **Sharp transients**: Can create spurious coupling
- **Sample rate**: Must be > 2x highest amplitude frequency (Nyquist)
- **Epoch length**: Too short = unreliable estimates

---

## API Reference

### Key Types

| Type | Location | Description |
|------|----------|-------------|
| `PacAnalyzer` | `sonido_analysis::cfc` | Main PAC analysis |
| `PacResult` | `sonido_analysis::cfc` | Analysis results |
| `PacMethod` | `sonido_analysis::cfc` | MVL or KL method |
| `Comodulogram` | `sonido_analysis::cfc` | Multi-frequency coupling |
| `FilterBank` | `sonido_analysis::filterbank` | Band extraction |
| `FrequencyBand` | `sonido_analysis::filterbank` | Band specification |
| `HilbertTransform` | `sonido_analysis::hilbert` | Phase/amplitude extraction |

### Key Files

| Component | Location |
|-----------|----------|
| PAC Analysis | `crates/sonido-analysis/src/cfc.rs` |
| Hilbert Transform | `crates/sonido-analysis/src/hilbert.rs` |
| Filter Bank | `crates/sonido-analysis/src/filterbank.rs` |
| CLI Commands | `crates/sonido-cli/src/commands/analyze.rs` |

For complete API documentation:

```bash
cargo doc -p sonido-analysis --open
```

---

## References

1. Canolty RT et al. (2006). High gamma power is phase-locked to theta oscillations in human neocortex. Science 313:1626-1628.

2. Tort AB et al. (2010). Measuring phase-amplitude coupling between neuronal oscillations of different frequencies. J Neurophysiol 104:1195-1210.

3. Aru J et al. (2015). Untangling cross-frequency coupling in neuroscience. Curr Opin Neurobiol 31:51-61.
