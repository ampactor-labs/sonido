# Analysis DSP Context

On-demand reference for the sonido-analysis crate.

## Module Map

| Module | Purpose |
|--------|---------|
| `cfc.rs` | Cross-frequency coupling (CFC) and phase-amplitude coupling (PAC) analysis. Comodulogram generation, surrogate statistics for significance testing. |
| `filterbank.rs` | Bandpass filter bank for frequency decomposition. Used by CFC/PAC pipeline. |
| `hilbert.rs` | Hilbert transform for analytic signal computation. Extracts instantaneous amplitude and phase. |
| `lms.rs` | Least mean squares adaptive filter. System identification and noise cancellation. |
| `xcorr.rs` | Cross-correlation and auto-correlation functions. Lag analysis and similarity measurement. |
| `ddc.rs` | Digital downconversion. Frequency shifting and decimation for narrowband analysis. |
| `phase.rs` | Phase analysis utilities. Phase difference, coherence, phase-locking value (PLV). |
| `resample.rs` | Sample rate conversion. Polyphase resampling for analysis at different rates. |

## Constraints

- **Uses std** (not `no_std`) -- no embedded constraints
- Depends on `rustfft` for FFT operations
- All modules are `std`-only; no `libm` restrictions

## CFC/PAC Analysis

Entry points for cross-frequency coupling:
- Comodulogram API: sweep phase-frequency x amplitude-frequency grid, compute modulation index at each point
- Surrogate statistics: shuffle-based significance testing (z-score against surrogate distribution)
- Phase-amplitude coupling: measures how the phase of a low-frequency oscillation modulates the amplitude of a high-frequency oscillation

## Key Files

`crates/sonido-analysis/src/` -- cfc.rs, filterbank.rs, hilbert.rs, lms.rs, xcorr.rs, ddc.rs, phase.rs, resample.rs

See `docs/BIOSIGNAL_ANALYSIS.md` and `docs/CFC_ANALYSIS.md` for theory and API details.
