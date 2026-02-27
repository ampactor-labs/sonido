//! Audio I/O layer for the Sonido DSP framework.
//!
//! This crate provides:
//!
//! - **WAV file I/O**: [`read_wav`] and [`write_wav`] for loading/saving audio files
//! - **Real-time streaming**: [`AudioStream`] for live audio input/output
//! - **Pluggable audio backends**: [`backend::AudioBackend`] trait for platform abstraction
//! - **Effect processing**: [`GraphEngine`] for applying effect chains to audio via DAG routing
//!
//! ## Audio Backend Architecture
//!
//! The [`backend::AudioBackend`] trait decouples audio streaming from platform APIs.
//! The default [`cpal_backend::CpalBackend`] wraps cpal for desktop platforms. Custom
//! backends can be implemented for embedded targets, testing, or direct platform APIs.
//!
//! ```rust,ignore
//! use sonido_io::cpal_backend::CpalBackend;
//! use sonido_io::backend::{AudioBackend, BackendStreamConfig};
//!
//! let backend = CpalBackend::new();
//! let config = BackendStreamConfig::default();
//!
//! let _stream = backend.build_output_stream(
//!     &config,
//!     Box::new(|buf| buf.fill(0.0)),  // silence
//!     Box::new(|err| eprintln!("{}", err)),
//! )?;
//! ```
//!
//! ## Quick Start (File Processing)
//!
//! ```rust,ignore
//! use sonido_io::{read_wav, write_wav, GraphEngine};
//! use sonido_effects::Reverb;
//!
//! let (samples, spec) = read_wav("input.wav")?;
//! let mut engine = GraphEngine::new_linear(spec.sample_rate as f32, 256);
//! engine.add_effect(Box::new(Reverb::new(spec.sample_rate as f32)));
//! let processed = engine.process_file(&samples, 256);
//! write_wav("output.wav", &processed, spec)?;
//! ```

pub mod backend;
pub mod cpal_backend;
mod graph_engine;
pub(crate) mod stream;
mod wav;

pub use graph_engine::GraphEngine;
pub use stream::{
    AudioDevice, AudioStream, StreamConfig, default_device, find_device_by_index,
    find_device_fuzzy, list_devices,
};
pub use wav::{
    StereoSamples, WavFormat, WavInfo, WavSpec, read_wav, read_wav_info, read_wav_stereo,
    write_wav, write_wav_stereo,
};

/// Error types for audio I/O operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// WAV file read/write error.
    #[error("WAV file error: {0}")]
    Wav(#[from] hound::Error),

    /// Audio stream setup or runtime error.
    #[error("Audio stream error: {0}")]
    Stream(String),

    /// No audio device available on the system.
    #[error("No audio device available")]
    NoDevice,

    /// The requested sample format is not supported.
    #[error("Unsupported sample format: {0}")]
    UnsupportedFormat(String),

    /// The requested audio device was not found.
    #[error("Device not found: {0}")]
    DeviceNotFound(String),

    /// Standard I/O error.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Convenience result type for audio I/O operations.
pub type Result<T> = std::result::Result<T, Error>;
