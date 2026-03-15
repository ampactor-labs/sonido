//! Pluggable audio backend abstraction.
//!
//! This module defines the [`AudioBackend`] trait, which decouples sonido's audio
//! processing pipeline from any specific platform audio API. The default implementation
//! wraps [cpal](https://crates.io/crates/cpal) (feature `"cpal-backend"`), but the
//! trait is designed so that alternative backends can be swapped in:
//!
//! - **Desktop**: cpal (ALSA, CoreAudio, WASAPI) — the default
//! - **Plugin hosts**: Host-provided buffers (CLAP/VST3) — no backend needed
//! - **Embedded**: DMA/I2S interrupt-driven I/O
//! - **WASM**: WebAudio AudioWorklet
//! - **Testing**: Deterministic mock backend for CI
//!
//! ## Architecture
//!
//! ```text
//! ┌──────────────────────────────────┐
//! │         Application code         │
//! │  (CLI, GUI, embedded main loop)  │
//! └──────────────┬───────────────────┘
//!                │ uses AudioBackend trait
//!                ▼
//! ┌──────────────────────────────────┐
//! │        AudioBackend trait         │
//! │  list_devices / build_streams    │
//! └──────────────┬───────────────────┘
//!                │ implemented by
//!        ┌───────┴────────┐
//!        ▼                ▼
//! ┌─────────────┐  ┌─────────────┐
//! │ CpalBackend │  │ (future)    │
//! │  (default)  │  │ AlsaDirect, │
//! │             │  │ Mock, etc.  │
//! └─────────────┘  └─────────────┘
//! ```
//!
//! ## Design Rationale (ADR-023)
//!
//! The trait uses boxed closures for callbacks rather than generic parameters, making
//! `AudioBackend` object-safe and enabling runtime backend selection. Stream handles
//! are returned as [`StreamHandle`], a type-erased wrapper that automatically stops
//! playback on drop. This keeps platform-specific types out of application code.

use crate::{AudioDevice, Result};

/// Selects the audio host when constructing a backend.
///
/// | Variant | Feature flag | Platform |
/// |---------|-------------|----------|
/// | `Default` | — | platform default (ALSA/CoreAudio/WASAPI) |
/// | `Jack` | `jack` | Linux/macOS JACK2 |
/// | `Asio` | `asio` | Windows ASIO |
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BackendHost {
    /// Use the platform's default audio API.
    #[default]
    Default,
    /// JACK audio server (requires `jack` feature and JACK2 installed).
    Jack,
    /// ASIO low-latency driver on Windows (requires `asio` feature and ASIO SDK).
    Asio,
}

/// Configuration for building an audio stream.
///
/// ## Fields
///
/// - `sample_rate`: Requested sample rate in Hz (default: 48000)
/// - `buffer_size`: Preferred buffer size in frames (default: 512)
/// - `channels`: Number of audio channels (default: 2, stereo)
/// - `device_name`: Optional device name filter (uses default device if `None`)
/// - `host`: Audio host selection (default: platform default)
#[derive(Debug, Clone)]
pub struct BackendStreamConfig {
    /// Requested sample rate in Hz.
    pub sample_rate: u32,
    /// Preferred buffer size in frames.
    pub buffer_size: u32,
    /// Number of audio channels.
    pub channels: u16,
    /// Optional device name (uses system default if `None`).
    pub device_name: Option<String>,
    /// Audio host selection. See [`BackendHost`].
    pub host: BackendHost,
}

impl Default for BackendStreamConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            buffer_size: 512,
            channels: 2,
            device_name: None,
            host: BackendHost::Default,
        }
    }
}

/// Type-erased audio stream handle.
///
/// Wraps a backend-specific stream object. The stream is active while this handle
/// exists; dropping it stops playback/capture. This design ensures RAII cleanup
/// regardless of which backend produced the stream.
///
/// The inner value is `Box<dyn Send>`, keeping backend types out of application code.
pub struct StreamHandle {
    /// The backend-specific stream object, kept alive via RAII.
    _inner: Box<dyn Send>,
}

impl StreamHandle {
    /// Create a new stream handle wrapping a backend-specific stream object.
    ///
    /// The wrapped value is kept alive until this handle is dropped.
    /// The type `T` must be `Send + 'static` so it can be safely moved
    /// between threads.
    pub fn new<T: Send + 'static>(stream: T) -> Self {
        Self {
            _inner: Box::new(stream),
        }
    }
}

impl std::fmt::Debug for StreamHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamHandle").finish_non_exhaustive()
    }
}

/// Audio output callback signature.
///
/// Called by the audio backend on the real-time audio thread. The callback receives
/// a mutable buffer of interleaved f32 samples that it must fill with output audio.
///
/// ## Buffer Layout
///
/// For stereo (2-channel) output, samples are interleaved: `[L0, R0, L1, R1, ...]`.
/// The buffer length is `frames * channels`.
///
/// ## Real-Time Safety
///
/// This callback runs on the audio thread. Implementations must not allocate,
/// lock mutexes, or perform I/O. Use lock-free structures (atomic, ring buffers)
/// to communicate with other threads.
pub type OutputCallback = Box<dyn FnMut(&mut [f32]) + Send>;

/// Audio input callback signature.
///
/// Called by the audio backend on the real-time audio thread with captured input
/// samples. The buffer contains interleaved f32 samples.
///
/// ## Buffer Layout
///
/// Same interleaved layout as [`OutputCallback`]: `[L0, R0, L1, R1, ...]`.
pub type InputCallback = Box<dyn FnMut(&[f32]) + Send>;

/// Error callback signature.
///
/// Called when the audio backend encounters an error during streaming.
/// The callback receives a human-readable error message.
pub type ErrorCallback = Box<dyn FnMut(&str) + Send>;

/// Pluggable audio backend trait.
///
/// Abstracts over platform-specific audio APIs (cpal, ALSA, CoreAudio, WASAPI,
/// AAudio, WebAudio, etc.) to provide a uniform interface for device enumeration
/// and stream construction.
///
/// ## Object Safety
///
/// This trait is object-safe, enabling runtime backend selection via
/// `Box<dyn AudioBackend>`. All callbacks use boxed closures, and stream handles
/// are type-erased.
///
/// ## Implementing a Custom Backend
///
/// ```rust,ignore
/// use sonido_io::backend::{AudioBackend, BackendStreamConfig, StreamHandle,
///                          OutputCallback, InputCallback, ErrorCallback};
/// use sonido_io::{AudioDevice, Result};
///
/// struct MyBackend { /* ... */ }
///
/// impl AudioBackend for MyBackend {
///     fn name(&self) -> &str { "my-backend" }
///
///     fn list_devices(&self) -> Result<Vec<AudioDevice>> {
///         // Enumerate hardware devices
///         todo!()
///     }
///
///     fn default_output_device(&self) -> Result<Option<AudioDevice>> {
///         // Return system default
///         todo!()
///     }
///
///     fn default_input_device(&self) -> Result<Option<AudioDevice>> {
///         todo!()
///     }
///
///     fn build_output_stream(
///         &self,
///         config: &BackendStreamConfig,
///         callback: OutputCallback,
///         error_callback: ErrorCallback,
///     ) -> Result<StreamHandle> {
///         // Set up platform-specific output stream
///         todo!()
///     }
///
///     fn build_input_stream(
///         &self,
///         config: &BackendStreamConfig,
///         callback: InputCallback,
///         error_callback: ErrorCallback,
///     ) -> Result<StreamHandle> {
///         // Set up platform-specific input stream
///         todo!()
///     }
/// }
/// ```
pub trait AudioBackend: Send {
    /// Human-readable name of this backend (e.g., "cpal", "alsa-direct", "mock").
    fn name(&self) -> &str;

    /// List all available audio devices.
    fn list_devices(&self) -> Result<Vec<AudioDevice>>;

    /// Get the default output device, if any.
    fn default_output_device(&self) -> Result<Option<AudioDevice>>;

    /// Get the default input device, if any.
    fn default_input_device(&self) -> Result<Option<AudioDevice>>;

    /// Build an output-only audio stream.
    ///
    /// The `callback` is invoked on the audio thread with a mutable buffer
    /// of interleaved f32 samples that must be filled with output audio.
    ///
    /// The returned [`StreamHandle`] keeps the stream alive. Dropping it stops
    /// playback.
    ///
    /// ## Arguments
    ///
    /// - `config`: Stream configuration (sample rate, buffer size, channels, device)
    /// - `callback`: Called per audio buffer to generate output samples
    /// - `error_callback`: Called when the backend encounters a streaming error
    fn build_output_stream(
        &self,
        config: &BackendStreamConfig,
        callback: OutputCallback,
        error_callback: ErrorCallback,
    ) -> Result<StreamHandle>;

    /// Build an input-only audio stream.
    ///
    /// The `callback` is invoked on the audio thread with a buffer of captured
    /// interleaved f32 samples.
    ///
    /// The returned [`StreamHandle`] keeps the stream alive. Dropping it stops
    /// capture.
    ///
    /// ## Arguments
    ///
    /// - `config`: Stream configuration (sample rate, buffer size, channels, device)
    /// - `callback`: Called per audio buffer with captured input samples
    /// - `error_callback`: Called when the backend encounters a streaming error
    fn build_input_stream(
        &self,
        config: &BackendStreamConfig,
        callback: InputCallback,
        error_callback: ErrorCallback,
    ) -> Result<StreamHandle>;

    /// Query the actual sample rate the backend will use for the given config.
    ///
    /// Some backends may not support the exact requested sample rate and will
    /// use the closest available rate. This method returns what would actually
    /// be used. Default implementation returns the requested rate unchanged.
    fn actual_sample_rate(&self, config: &BackendStreamConfig) -> u32 {
        config.sample_rate
    }
}

/// Create a [`CpalBackend`](crate::cpal_backend::CpalBackend) for the host
/// specified in `config.host`.
///
/// - [`BackendHost::Default`]: platform default (ALSA/CoreAudio/WASAPI)
/// - [`BackendHost::Jack`]: JACK host (requires `jack` feature)
/// - [`BackendHost::Asio`]: ASIO host (requires `asio` feature, Windows only)
///
/// # Errors
///
/// Returns an error if the requested host is not available or the feature flag
/// was not compiled in.
pub fn create_backend(
    config: &BackendStreamConfig,
) -> crate::Result<crate::cpal_backend::CpalBackend> {
    crate::cpal_backend::CpalBackend::with_host(&config.host)
}
