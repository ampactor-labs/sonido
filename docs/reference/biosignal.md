# Biosignal Analysis with Sonido

While designed for audio, sonido-analysis works for any time-series signal. The mathematics of spectral analysis are timescale-agnostic.

## Cross-Frequency Coupling (CFC) and Phase-Amplitude Coupling (PAC)

Phase-Amplitude Coupling is a phenomenon where the phase of a slow oscillation modulates the amplitude envelope of a faster oscillation. It is observed across many biological systems: theta-gamma coupling during memory encoding in the hippocampus, communication chirps in weakly electric fish, and oscillatory coordination in slime mold networks.

### Mathematical Framework

The PAC analysis pipeline (`crates/sonido-analysis/src/cfc.rs`) implements two established methods:

**Mean Vector Length (MVL)** (Canolty et al., 2006): For each time point, the instantaneous amplitude of the high-frequency band is represented as a vector at the angle of the low-frequency phase. The modulation index is the length of the mean resultant vector:

```
MI = |mean(amplitude * exp(i * phase))|
```

If amplitude is uniformly distributed across all phases, the vectors cancel and MI approaches 0. If amplitude concentrates at a preferred phase, the vectors reinforce and MI is large.

**Kullback-Leibler Divergence** (Tort et al., 2010): Phase is binned into 18 bins of 20 degrees each (`NUM_PHASE_BINS` in `cfc.rs`). The mean amplitude in each bin forms a distribution. The KL divergence between this distribution and a uniform distribution measures coupling strength:

```
MI = KL(P || U) / log(N_bins)
```

The KL method is more robust to signal length and less sensitive to outliers than MVL, but requires binning which loses phase resolution.

### Signal Processing Pipeline

1. **Band extraction** (`crates/sonido-analysis/src/filterbank.rs`): 4th-order Butterworth bandpass filters (two cascaded biquad sections per cutoff) isolate the phase and amplitude bands. The Q values 0.541 and 1.307 are the Butterworth cascade values for 4th-order response, which give a maximally flat passband with steep rolloff.

2. **Hilbert transform** (`crates/sonido-analysis/src/hilbert.rs`): FFT-based computation of the analytic signal. Positive frequencies are doubled, negative frequencies zeroed, then IFFT produces the complex analytic signal. Instantaneous amplitude = magnitude, instantaneous phase = argument.

3. **Coupling computation**: MVL or KL method as described above.

### Comodulogram

The `Comodulogram` (`cfc.rs`) computes PAC for a grid of frequency pairs, producing a 2D matrix where each cell shows coupling strength between a phase frequency and an amplitude frequency. The matrix shows which frequency pairs have the strongest coupling. For example, theta-phase / gamma-amplitude coupling in EEG data typically shows as a hot spot around (6 Hz, 40-80 Hz).

### EEG Frequency Bands

The `filterbank::eeg_bands` module defines standard clinical/research bands:

| Band | Range | Associated State |
|------|-------|-----------------|
| Delta | 0.5-4 Hz | Deep sleep, unconscious processes |
| Theta | 4-8 Hz | Drowsiness, memory encoding |
| Alpha | 8-13 Hz | Relaxed wakefulness, eyes closed |
| Beta | 13-30 Hz | Active thinking, focus |
| Low Gamma | 30-80 Hz | Cognitive processing, perception |
| High Gamma | 80-200 Hz | Fine motor control, sensory processing |

## Electric Fish (Weakly Electric)

Electric Organ Discharge (EOD) signals from gymnotiform and mormyrid fish are ideal candidates because they occupy the audio frequency range (50-1000 Hz).

### Species Identification via Waveform

```bash
# Record EOD through electrodes (standard audio interface works)
# Pulse-type fish have rich harmonics, wave-type are near-sinusoidal

sonido analyze distortion eigenmannia.wav --fft-size 16384
# Wave-type: THD < 5%

sonido analyze distortion gymnotus.wav --fft-size 16384
# Pulse-type: THD > 30% (rich harmonics from sharp waveform)
```

### Jamming Avoidance Response (JAR)

When two wave-type fish detect similar frequencies, they shift apart:

```bash
# High-resolution spectrogram captures frequency tracking
sonido analyze spectrogram two_fish.wav -o jar.csv --fft-size 4096 --hop 128

# Visualize with Python:
# import pandas as pd
# import matplotlib.pyplot as plt
# df = pd.read_csv('jar.csv', index_col=0)
# plt.imshow(df.T, aspect='auto', origin='lower')
```

### Chirp Detection

Communication signals appear as rapid frequency excursions:

```bash
# Fast time resolution for transient chirps
sonido analyze spectrogram courtship.wav -o chirps.csv --fft-size 512 --hop 32
```

### EOD Stability Measurement

```bash
# Welch averaging for precise frequency estimation
sonido analyze spectrum fish.wav --method welch --overlap 0.8 --fft-size 65536 --peaks 3
```

## Slime Mold (Physarum polycephalum)

Slime mold electrical oscillations are extremely slow (0.01-0.1 Hz). To analyze them, treat your sampling interval as a "virtual sample rate."

### Data Preparation

If you sample voltage every 0.5 seconds:
- Your effective "sample rate" = 2.0 Hz
- 2 hours of data = 14,400 samples
- A 50-second oscillation (0.02 Hz) is detectable

```python
# Convert timeseries to WAV
import numpy as np
from scipy.io import wavfile

# Your voltage readings (mV scaled to -1.0 to 1.0)
voltage_data = np.array([...])  # 14400 samples at 0.5s intervals
sample_rate = 2  # 2 samples per second

# Scale to audio range
normalized = voltage_data / np.max(np.abs(voltage_data))
wavfile.write('physarum.wav', sample_rate, normalized.astype(np.float32))
```

### Analysis

```bash
# Spectrum of oscillation frequencies
sonido analyze spectrum physarum.wav --method welch --peaks 5
# Peak at "0.02 Hz" indicates 50-second shuttle streaming rhythm

# Time-frequency analysis
sonido analyze spectrogram physarum.wav -o oscillations.csv --fft-size 128 --hop 16
# Reveals how oscillation frequency changes with stimuli

# Dynamics
sonido analyze dynamics physarum.wav
# Crest factor indicates regularity of oscillations
```

### Multi-Electrode Analysis

For multiple recording sites, compare phase relationships:

```bash
# Record each electrode to separate channel
# Analyze transfer function between sites
sonido analyze transfer electrode1.wav electrode2.wav --group-delay
# Group delay reveals signal propagation direction
```

## Frequency Scale Reference

| Signal | Frequency | Required Sample Rate |
|--------|-----------|---------------------|
| Electric eel pulse | 1-10 Hz | 100+ Hz |
| Pulse-type fish EOD | 20-100 Hz | 1 kHz+ |
| Wave-type fish EOD | 200-600 Hz | 5 kHz+ |
| Slime mold oscillation | 0.01-0.1 Hz | 1 Hz |
| Plant action potential | 0.001-0.01 Hz | 0.1 Hz |

## Tips

1. Normalize your data to -1.0 to 1.0 range before conversion to WAV
2. Use appropriate FFT sizes: longer for slow signals
3. Welch averaging is essential for noisy biological recordings
4. Export spectrograms to CSV for visualization in Python/R/MATLAB
5. Group delay can reveal signal propagation in distributed systems
