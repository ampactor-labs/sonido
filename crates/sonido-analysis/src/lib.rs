//! Sonido Analysis - Spectral tools for audio DSP reverse engineering
//!
//! This crate provides analysis tools for reverse engineering audio algorithms:
//!
//! - [`fft`] - FFT wrapper with windowing functions
//! - [`spectrum`] - Spectral analysis utilities (including Welch's method)
//! - [`dynamics`] - Dynamics analysis (RMS, crest factor, dynamic range)
//! - [`ir`] - Impulse response capture via sine sweep
//! - [`transfer_fn`] - Transfer function measurement
//! - [`compare`] - A/B comparison tools
//! - [`distortion`] - THD, THD+N, and IMD analysis
//! - [`spectrogram`] - STFT-based time-frequency analysis
//! - [`constant_q`] - Constant-Q transform for pitch-based analysis
//! - [`filterbank`] - Bandpass filter bank for frequency band extraction
//! - [`hilbert`] - Hilbert transform for analytic signals
//! - [`cfc`] - Cross-Frequency Coupling (Phase-Amplitude Coupling) analysis
//! - [`export`] - Export formats (FRD, CSV, PGM) for analysis results
//! - [`lms`] - LMS/NLMS adaptive filters for noise/echo cancellation
//! - [`xcorr`] - Cross-correlation (direct + FFT) with lag estimation
//! - [`ddc`] - Digital down-conversion (NCO + FIR + decimation)
//! - [`phase`] - Phase unwrapping (batch, quality-guided, streaming)
//! - [`mod@resample`] - Rational resampling via polyphase filter (decimate, interpolate, P/Q)
//! - [`loudness`] - ITU-R BS.1770-4 LUFS metering (momentary, short-term, integrated, true peak)
//! - [`pitch`] - YIN pitch detection: `detect_pitch(buffer, sample_rate) -> Option<PitchResult>`
//! - [`stereo`] - Stereo Pearson correlation meter with sliding window
//!
//! ## Target Use Case
//!
//! This crate is designed to help reverse engineer audio algorithms like those
//! in DigiTech audioDNA pedals (Polara, Obscura, Ventura, etc.).
//!
//! ## Example Workflow
//!
//! ```rust,ignore
//! use sonido_analysis::{SineSweep, TransferFunction, spectrum};
//!
//! // 1. Generate test signal
//! let sweep = SineSweep::new(48000.0, 20.0, 20000.0, 2.0);
//!
//! // 2. Record pedal response (external)
//!
//! // 3. Analyze transfer function
//! let tf = TransferFunction::from_sweep(&input, &output, 48000.0);
//!
//! // 4. Compare with implementation
//! let similarity = spectrum::correlation(&original, &implementation);
//! ```
//!
//! ## Distortion Analysis
//!
//! ```rust,ignore
//! use sonido_analysis::distortion::{ThdAnalyzer, generate_test_tone};
//!
//! // Generate test tone
//! let signal = generate_test_tone(48000.0, 1000.0, 1.0, 0.5);
//!
//! // Analyze THD
//! let analyzer = ThdAnalyzer::new(48000.0, 8192);
//! let result = analyzer.analyze(&signal, 1000.0);
//! println!("THD: {:.2}% ({:.1} dB)", result.thd_ratio * 100.0, result.thd_db);
//! ```
//!
//! ## Spectrogram
//!
//! ```rust,ignore
//! use sonido_analysis::spectrogram::StftAnalyzer;
//! use sonido_analysis::fft::Window;
//!
//! let analyzer = StftAnalyzer::new(48000.0, 2048, 512, Window::Hann);
//! let spectrogram = analyzer.analyze(&signal);
//!
//! // Get peak frequency at frame 10
//! let peak = spectrogram.peak_frequency(10);
//! ```

pub mod cfc;
pub mod compare;
pub mod constant_q;
pub mod ddc;
pub mod distortion;
pub mod dynamics;
pub mod export;
pub mod fft;
pub mod filterbank;
pub mod hilbert;
pub mod ir;
pub mod lms;
pub mod loudness;
pub mod phase;
pub mod pitch;
pub mod resample;
pub mod spectrogram;
pub mod spectrum;
pub mod stereo;
pub mod transfer_fn;
pub mod xcorr;

// Re-export main types
pub use cfc::{Comodulogram, PacAnalyzer, PacMethod, PacResult};
pub use compare::{spectral_correlation, spectral_difference};
pub use constant_q::{Chromagram, ConstantQTransform, CqtResult, CqtSpectrogram};
pub use distortion::{ImdAnalyzer, ImdResult, ThdAnalyzer, ThdResult, generate_test_tone};
pub use dynamics::{
    DynamicsAnalysis, analyze_dynamics, crest_factor, crest_factor_db, peak, peak_db, rms, rms_db,
};
pub use fft::{Fft, Window};
pub use filterbank::{FilterBank, FrequencyBand, eeg_bands};
pub use hilbert::HilbertTransform;
pub use ir::{Rt60Estimate, SineSweep, energy_decay_curve, estimate_rt60, trim_ir};
pub use spectrogram::{MelFilterbank, MelSpectrogram, Spectrogram, StftAnalyzer};
pub use spectrum::{coherence, magnitude_spectrum, phase_spectrum, spectral_centroid, welch_psd};
pub use transfer_fn::{Resonance, TransferFunction};

// DSP primitives
pub use ddc::Ddc;
pub use lms::{LmsFilter, NlmsFilter};
pub use loudness::LufsMeter;
pub use phase::{PhaseTracker, unwrap_phase, unwrap_phase_quality, unwrap_phase_tol};
pub use pitch::{PitchResult, detect_pitch};
pub use resample::{decimate, design_lowpass, interpolate, resample};
pub use stereo::StereoCorrelation;
pub use xcorr::{peak_lag, xcorr_direct, xcorr_fft, xcorr_normalized};
