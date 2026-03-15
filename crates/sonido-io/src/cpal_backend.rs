//! cpal-based audio backend implementation.
//!
//! This module provides [`CpalBackend`], the default [`AudioBackend`] implementation
//! that wraps [cpal](https://crates.io/crates/cpal) for cross-platform audio I/O.
//! It supports ALSA (Linux), CoreAudio (macOS/iOS), WASAPI (Windows), Oboe (Android),
//! and WebAudio (WASM).
//!
//! ## Usage
//!
//! ```rust,ignore
//! use sonido_io::cpal_backend::CpalBackend;
//! use sonido_io::backend::{AudioBackend, BackendStreamConfig};
//!
//! let backend = CpalBackend::new();
//! let devices = backend.list_devices()?;
//!
//! let config = BackendStreamConfig::default();
//! let stream = backend.build_output_stream(
//!     &config,
//!     Box::new(|buffer: &mut [f32]| {
//!         // Fill buffer with audio...
//!         buffer.fill(0.0);
//!     }),
//!     Box::new(|err| eprintln!("Audio error: {}", err)),
//! )?;
//! // Stream plays until `stream` is dropped.
//! ```

use crate::backend::{
    AudioBackend, BackendHost, BackendStreamConfig, ErrorCallback, InputCallback, OutputCallback,
    StreamHandle,
};
use crate::stream::device_name;
use crate::{AudioDevice, Error, Result};
use cpal::Host;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// cpal-based audio backend.
///
/// Wraps the cpal library to provide cross-platform audio device enumeration
/// and stream construction. This is the default backend used by sonido when
/// the `cpal-backend` feature is enabled.
///
/// The backend holds a cpal [`Host`] instance, which represents the connection
/// to the platform's audio system.
pub struct CpalBackend {
    host: Host,
}

impl CpalBackend {
    /// Create a new cpal backend using the platform's default audio host.
    ///
    /// On Linux this is ALSA, on macOS CoreAudio, on Windows WASAPI.
    pub fn new() -> Self {
        tracing::info!(
            host = cpal::default_host().id().name(),
            "cpal backend initialized"
        );
        Self {
            host: cpal::default_host(),
        }
    }

    /// Create a backend for the given [`BackendHost`].
    ///
    /// Returns an error if the requested host is unavailable or the required
    /// feature flag was not compiled in.
    pub fn with_host(host: &BackendHost) -> Result<Self> {
        let cpal_host = match host {
            BackendHost::Default => cpal::default_host(),
            BackendHost::Jack => {
                #[cfg(feature = "jack")]
                {
                    cpal::host_from_id(cpal::HostId::Jack)
                        .map_err(|e| Error::Stream(format!("JACK host unavailable: {e}")))?
                }
                #[cfg(not(feature = "jack"))]
                {
                    return Err(Error::Stream(
                        "JACK backend requires the `jack` feature; \
                         recompile with `--features jack`"
                            .into(),
                    ));
                }
            }
            BackendHost::Asio => {
                #[cfg(feature = "asio")]
                {
                    cpal::host_from_id(cpal::HostId::Asio)
                        .map_err(|e| Error::Stream(format!("ASIO host unavailable: {e}")))?
                }
                #[cfg(not(feature = "asio"))]
                {
                    return Err(Error::Stream(
                        "ASIO backend requires the `asio` feature; \
                         recompile with `--features asio`"
                            .into(),
                    ));
                }
            }
        };
        tracing::info!(host = cpal_host.id().name(), "cpal backend initialized");
        Ok(Self { host: cpal_host })
    }

    /// Find a cpal output device by name, or return the default.
    fn find_output_device(&self, name: Option<&str>) -> Result<cpal::Device> {
        match name {
            Some(search) => {
                let search_lower = search.to_lowercase();
                let devices = self
                    .host
                    .output_devices()
                    .map_err(|e| Error::Stream(e.to_string()))?;

                for device in devices {
                    if let Ok(dev_name) = device_name(&device)
                        && dev_name.to_lowercase().contains(search_lower.as_str())
                    {
                        return Ok(device);
                    }
                }
                Err(Error::DeviceNotFound(format!(
                    "no output device matching '{}'",
                    search
                )))
            }
            None => self.host.default_output_device().ok_or(Error::NoDevice),
        }
    }

    /// Find a cpal input device by name, or return the default.
    fn find_input_device(&self, name: Option<&str>) -> Result<cpal::Device> {
        match name {
            Some(search) => {
                let search_lower = search.to_lowercase();
                let devices = self
                    .host
                    .input_devices()
                    .map_err(|e| Error::Stream(e.to_string()))?;

                for device in devices {
                    if let Ok(dev_name) = device_name(&device)
                        && dev_name.to_lowercase().contains(&search_lower)
                    {
                        return Ok(device);
                    }
                }
                Err(Error::DeviceNotFound(format!(
                    "no input device matching '{}'",
                    search
                )))
            }
            None => self.host.default_input_device().ok_or(Error::NoDevice),
        }
    }
}

impl Default for CpalBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioBackend for CpalBackend {
    fn name(&self) -> &'static str {
        "cpal"
    }

    fn list_devices(&self) -> Result<Vec<AudioDevice>> {
        // Delegate to existing implementation which uses cpal::default_host()
        // internally. This is consistent since CpalBackend also uses the default host.
        crate::stream::list_devices()
    }

    fn default_output_device(&self) -> Result<Option<AudioDevice>> {
        let (_, output) = crate::stream::default_device()?;
        Ok(output)
    }

    fn default_input_device(&self) -> Result<Option<AudioDevice>> {
        let (input, _) = crate::stream::default_device()?;
        Ok(input)
    }

    fn build_output_stream(
        &self,
        config: &BackendStreamConfig,
        mut callback: OutputCallback,
        mut error_callback: ErrorCallback,
    ) -> Result<StreamHandle> {
        let device = self.find_output_device(config.device_name.as_deref())?;

        let stream_config = cpal::StreamConfig {
            channels: config.channels,
            sample_rate: config.sample_rate,
            buffer_size: cpal::BufferSize::Fixed(config.buffer_size),
        };

        let stream = device
            .build_output_stream(
                &stream_config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    callback(data);
                },
                move |err| {
                    error_callback(&err.to_string());
                },
                None,
            )
            .map_err(|e| Error::Stream(e.to_string()))?;

        stream.play().map_err(|e| Error::Stream(e.to_string()))?;
        tracing::info!(
            channels = config.channels,
            sample_rate = config.sample_rate,
            "output stream started"
        );

        Ok(StreamHandle::new(stream))
    }

    fn build_input_stream(
        &self,
        config: &BackendStreamConfig,
        mut callback: InputCallback,
        mut error_callback: ErrorCallback,
    ) -> Result<StreamHandle> {
        let device = self.find_input_device(config.device_name.as_deref())?;

        let stream_config = cpal::StreamConfig {
            channels: config.channels,
            sample_rate: config.sample_rate,
            buffer_size: cpal::BufferSize::Fixed(config.buffer_size),
        };

        let stream = device
            .build_input_stream(
                &stream_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    callback(data);
                },
                move |err| {
                    error_callback(&err.to_string());
                },
                None,
            )
            .map_err(|e| Error::Stream(e.to_string()))?;

        stream.play().map_err(|e| Error::Stream(e.to_string()))?;
        tracing::info!(
            channels = config.channels,
            sample_rate = config.sample_rate,
            "input stream started"
        );

        Ok(StreamHandle::new(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cpal_backend_name() {
        let backend = CpalBackend::new();
        assert_eq!(backend.name(), "cpal");
    }

    #[test]
    fn test_cpal_backend_list_devices() {
        let backend = CpalBackend::new();
        // Should not panic; device availability depends on the system.
        let result = backend.list_devices();
        assert!(result.is_ok());
    }

    #[test]
    fn test_default_config() {
        let config = BackendStreamConfig::default();
        assert_eq!(config.sample_rate, 48000);
        assert_eq!(config.buffer_size, 512);
        assert_eq!(config.channels, 2);
        assert!(config.device_name.is_none());
    }

    #[test]
    fn test_stream_handle_debug() {
        let handle = StreamHandle::new(42u32);
        let debug_str = format!("{:?}", handle);
        assert!(debug_str.contains("StreamHandle"));
    }
}
