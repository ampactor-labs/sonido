//! Real-time audio streaming via cpal.

use crate::{Error, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Host, Stream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Extract device name via `description()` (cpal 0.17+).
pub(crate) fn device_name(device: &Device) -> std::result::Result<String, cpal::DeviceNameError> {
    device.description().map(|d| d.name().to_string())
}

/// Audio device information.
#[derive(Debug, Clone)]
pub struct AudioDevice {
    /// Human-readable device name.
    pub name: String,
    /// Whether the device supports audio input.
    pub is_input: bool,
    /// Whether the device supports audio output.
    pub is_output: bool,
    /// Default sample rate in Hz.
    pub default_sample_rate: u32,
}

/// Stream configuration.
#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Buffer size in frames.
    pub buffer_size: u32,
    /// Input device name (uses default if `None`).
    pub input_device: Option<String>,
    /// Output device name (uses default if `None`).
    pub output_device: Option<String>,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            buffer_size: 512,
            input_device: None,
            output_device: None,
        }
    }
}

/// List all available audio devices.
pub fn list_devices() -> Result<Vec<AudioDevice>> {
    let host = cpal::default_host();
    let mut devices = Vec::new();

    // Input devices
    if let Ok(inputs) = host.input_devices() {
        for device in inputs {
            if let Ok(name) = device_name(&device) {
                let sample_rate = device
                    .default_input_config()
                    .map(|c| c.sample_rate())
                    .unwrap_or(48000);

                // Check if also an output
                let is_output = device.default_output_config().is_ok();

                devices.push(AudioDevice {
                    name,
                    is_input: true,
                    is_output,
                    default_sample_rate: sample_rate,
                });
            }
        }
    }

    // Output-only devices
    if let Ok(outputs) = host.output_devices() {
        for device in outputs {
            if let Ok(name) = device_name(&device) {
                // Skip if already added as input
                if devices.iter().any(|d| d.name == name) {
                    continue;
                }

                let sample_rate = device
                    .default_output_config()
                    .map(|c| c.sample_rate())
                    .unwrap_or(48000);

                devices.push(AudioDevice {
                    name,
                    is_input: false,
                    is_output: true,
                    default_sample_rate: sample_rate,
                });
            }
        }
    }

    Ok(devices)
}

/// Get the default audio device info.
pub fn default_device() -> Result<(Option<AudioDevice>, Option<AudioDevice>)> {
    let host = cpal::default_host();

    let input = host.default_input_device().and_then(|d| {
        device_name(&d).ok().map(|name| AudioDevice {
            name,
            is_input: true,
            is_output: false,
            default_sample_rate: d
                .default_input_config()
                .map(|c| c.sample_rate())
                .unwrap_or(48000),
        })
    });

    let output = host.default_output_device().and_then(|d| {
        device_name(&d).ok().map(|name| AudioDevice {
            name,
            is_input: false,
            is_output: true,
            default_sample_rate: d
                .default_output_config()
                .map(|c| c.sample_rate())
                .unwrap_or(48000),
        })
    });

    Ok((input, output))
}

/// Real-time audio stream with input and output.
pub struct AudioStream {
    #[allow(dead_code)]
    host: Host,
    input_device: Device,
    output_device: Device,
    config: StreamConfig,
    running: Arc<AtomicBool>,
    _input_stream: Option<Stream>,
    _output_stream: Option<Stream>,
}

impl AudioStream {
    /// Create a new audio stream with the given configuration.
    pub fn new(config: StreamConfig) -> Result<Self> {
        let host = cpal::default_host();

        let input_device = match &config.input_device {
            Some(name) => find_input_device(&host, name)?,
            None => host.default_input_device().ok_or(Error::NoDevice)?,
        };

        let output_device = match &config.output_device {
            Some(name) => find_output_device(&host, name)?,
            None => host.default_output_device().ok_or(Error::NoDevice)?,
        };

        Ok(Self {
            host,
            input_device,
            output_device,
            config,
            running: Arc::new(AtomicBool::new(false)),
            _input_stream: None,
            _output_stream: None,
        })
    }

    /// Get the configured sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.config.sample_rate
    }

    /// Get the output device channel count.
    pub fn output_channels(&self) -> u16 {
        self.output_device
            .default_output_config()
            .map(|c| c.channels())
            .unwrap_or(2)
    }

    /// Run the audio stream with a processing callback.
    ///
    /// The callback receives input samples and must fill the output buffer.
    /// This function blocks until the stream is stopped.
    pub fn run<F>(&mut self, mut process: F) -> Result<()>
    where
        F: FnMut(&[f32], &mut [f32]) + Send + 'static,
    {
        use std::sync::mpsc;

        // Get supported configs
        let input_config = self
            .input_device
            .default_input_config()
            .map_err(|e| Error::Stream(e.to_string()))?;

        let output_config = self
            .output_device
            .default_output_config()
            .map_err(|e| Error::Stream(e.to_string()))?;

        // Create channel for passing audio between input and output
        let (tx, rx) = mpsc::sync_channel::<Vec<f32>>(4);

        let running = Arc::clone(&self.running);
        self.running.store(true, Ordering::SeqCst);

        // Input stream - capture audio and send to channel
        let input_running = Arc::clone(&running);
        let input_stream = self
            .input_device
            .build_input_stream(
                &input_config.into(),
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    if input_running.load(Ordering::SeqCst) {
                        let _ = tx.try_send(data.to_vec());
                    }
                },
                |err| eprintln!("Input stream error: {}", err),
                None,
            )
            .map_err(|e| Error::Stream(e.to_string()))?;

        // Output stream - receive processed audio
        let output_running = Arc::clone(&running);
        let mut pending_input: Vec<f32> = Vec::new();
        let output_stream = self
            .output_device
            .build_output_stream(
                &output_config.into(),
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    if !output_running.load(Ordering::SeqCst) {
                        data.fill(0.0);
                        return;
                    }

                    // Collect input samples
                    while let Ok(samples) = rx.try_recv() {
                        pending_input.extend(samples);
                    }

                    // Process if we have enough input
                    if pending_input.len() >= data.len() {
                        let input: Vec<f32> = pending_input.drain(..data.len()).collect();
                        process(&input, data);
                    } else {
                        // Not enough input - output silence
                        data.fill(0.0);
                    }
                },
                |err| eprintln!("Output stream error: {}", err),
                None,
            )
            .map_err(|e| Error::Stream(e.to_string()))?;

        input_stream
            .play()
            .map_err(|e| Error::Stream(e.to_string()))?;
        output_stream
            .play()
            .map_err(|e| Error::Stream(e.to_string()))?;

        self._input_stream = Some(input_stream);
        self._output_stream = Some(output_stream);

        // Block until stopped
        while self.running.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        Ok(())
    }

    /// Run output-only stream (no input).
    pub fn run_output<F>(&mut self, mut generate: F) -> Result<()>
    where
        F: FnMut(&mut [f32]) + Send + 'static,
    {
        let output_config = self
            .output_device
            .default_output_config()
            .map_err(|e| Error::Stream(e.to_string()))?;

        let running = Arc::clone(&self.running);
        self.running.store(true, Ordering::SeqCst);

        let output_running = Arc::clone(&running);
        let output_stream = self
            .output_device
            .build_output_stream(
                &output_config.into(),
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    if output_running.load(Ordering::SeqCst) {
                        generate(data);
                    } else {
                        data.fill(0.0);
                    }
                },
                |err| eprintln!("Output stream error: {}", err),
                None,
            )
            .map_err(|e| Error::Stream(e.to_string()))?;

        output_stream
            .play()
            .map_err(|e| Error::Stream(e.to_string()))?;
        self._output_stream = Some(output_stream);

        while self.running.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        Ok(())
    }

    /// Run the audio stream with a stereo processing callback.
    ///
    /// The callback receives separate left and right channel inputs
    /// and must fill the left and right output buffers.
    /// This function blocks until the stream is stopped.
    ///
    /// Note: The actual stream may be interleaved stereo depending on the
    /// audio device. This method handles deinterleaving input and
    /// interleaving output automatically.
    pub fn run_stereo<F>(&mut self, mut process: F) -> Result<()>
    where
        F: FnMut(&[f32], &[f32], &mut [f32], &mut [f32]) + Send + 'static,
    {
        use std::sync::mpsc;

        // Get supported configs
        let input_config = self
            .input_device
            .default_input_config()
            .map_err(|e| Error::Stream(e.to_string()))?;

        let output_config = self
            .output_device
            .default_output_config()
            .map_err(|e| Error::Stream(e.to_string()))?;

        let input_channels = input_config.channels() as usize;
        let output_channels = output_config.channels() as usize;

        // Create channel for passing audio between input and output
        let (tx, rx) = mpsc::sync_channel::<Vec<f32>>(4);

        let running = Arc::clone(&self.running);
        self.running.store(true, Ordering::SeqCst);

        // Input stream - capture audio and send to channel
        let input_running = Arc::clone(&running);
        let input_stream = self
            .input_device
            .build_input_stream(
                &input_config.into(),
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    if input_running.load(Ordering::SeqCst) {
                        let _ = tx.try_send(data.to_vec());
                    }
                },
                |err| eprintln!("Input stream error: {}", err),
                None,
            )
            .map_err(|e| Error::Stream(e.to_string()))?;

        // Output stream - receive and process stereo audio
        let output_running = Arc::clone(&running);
        let mut pending_input: Vec<f32> = Vec::new();

        let output_stream = self
            .output_device
            .build_output_stream(
                &output_config.into(),
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    if !output_running.load(Ordering::SeqCst) {
                        data.fill(0.0);
                        return;
                    }

                    // Collect input samples
                    while let Ok(samples) = rx.try_recv() {
                        pending_input.extend(samples);
                    }

                    let frames_needed = data.len() / output_channels;
                    let input_samples_needed = frames_needed * input_channels;

                    // Process if we have enough input
                    if pending_input.len() >= input_samples_needed {
                        let input: Vec<f32> = pending_input.drain(..input_samples_needed).collect();

                        // Deinterleave input to separate L/R buffers
                        let (left_in, right_in) = deinterleave(&input, input_channels);

                        // Prepare output buffers
                        let mut left_out = vec![0.0; frames_needed];
                        let mut right_out = vec![0.0; frames_needed];

                        // Process
                        process(&left_in, &right_in, &mut left_out, &mut right_out);

                        // Interleave output back into the data buffer
                        interleave_into(&left_out, &right_out, data, output_channels);
                    } else {
                        // Not enough input - output silence
                        data.fill(0.0);
                    }
                },
                |err| eprintln!("Output stream error: {}", err),
                None,
            )
            .map_err(|e| Error::Stream(e.to_string()))?;

        input_stream
            .play()
            .map_err(|e| Error::Stream(e.to_string()))?;
        output_stream
            .play()
            .map_err(|e| Error::Stream(e.to_string()))?;

        self._input_stream = Some(input_stream);
        self._output_stream = Some(output_stream);

        // Block until stopped
        while self.running.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        Ok(())
    }

    /// Stop the audio stream.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    /// Check if the stream is running.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Get a shared handle to the running flag.
    ///
    /// Store `false` to stop the stream's blocking loop from outside
    /// (e.g., from a signal handler). This is the same flag checked by
    /// `run_output()` / `run()` / `run_stereo()`.
    pub fn running_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.running)
    }
}

/// Deinterleave input samples into separate left and right channels.
/// Handles mono input by duplicating to both channels.
fn deinterleave(interleaved: &[f32], channels: usize) -> (Vec<f32>, Vec<f32>) {
    let frames = interleaved.len() / channels;
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);

    match channels {
        1 => {
            // Mono: duplicate to both channels
            for &sample in interleaved {
                left.push(sample);
                right.push(sample);
            }
        }
        _ => {
            // Stereo or more: take first two channels
            for chunk in interleaved.chunks(channels) {
                left.push(chunk[0]);
                right.push(chunk.get(1).copied().unwrap_or(chunk[0]));
            }
        }
    }

    (left, right)
}

/// Interleave left and right channels into an output buffer.
/// Handles output with different channel counts.
fn interleave_into(left: &[f32], right: &[f32], output: &mut [f32], channels: usize) {
    let frames = left.len().min(right.len());

    match channels {
        1 => {
            // Mono output: mix L+R
            for (i, (l, r)) in left.iter().zip(right.iter()).enumerate() {
                if i < output.len() {
                    output[i] = (l + r) * 0.5;
                }
            }
        }
        2 => {
            // Stereo output: interleave L, R
            for i in 0..frames {
                let idx = i * 2;
                if idx + 1 < output.len() {
                    output[idx] = left[i];
                    output[idx + 1] = right[i];
                }
            }
        }
        _ => {
            // Multi-channel: put L/R in first two channels, silence the rest
            for i in 0..frames {
                let idx = i * channels;
                if idx + channels <= output.len() {
                    output[idx] = left[i];
                    output[idx + 1] = right[i];
                    for c in 2..channels {
                        output[idx + c] = 0.0;
                    }
                }
            }
        }
    }
}

/// Find an input device by exact name, partial name, or index.
///
/// The `name_or_index` can be:
/// - A numeric index (e.g., "0", "1")
/// - An exact device name
/// - A partial device name (case-insensitive fuzzy match)
fn find_input_device(host: &Host, name_or_index: &str) -> Result<Device> {
    let devices: Vec<_> = host
        .input_devices()
        .map_err(|e| Error::Stream(e.to_string()))?
        .collect();

    find_device_from_list(&devices, name_or_index, "input")
}

/// Find an output device by exact name, partial name, or index.
///
/// The `name_or_index` can be:
/// - A numeric index (e.g., "0", "1")
/// - An exact device name
/// - A partial device name (case-insensitive fuzzy match)
///
/// The CLI's `--output` and the editor's `--output` both resolve through this.
pub fn find_output_device(host: &Host, name_or_index: &str) -> Result<Device> {
    let devices: Vec<_> = host
        .output_devices()
        .map_err(|e| Error::Stream(e.to_string()))?
        .collect();

    find_device_from_list(&devices, name_or_index, "output")
}

/// Find a device from a list by index, exact name, or fuzzy match.
fn find_device_from_list(devices: &[Device], name_or_index: &str, kind: &str) -> Result<Device> {
    // Try parsing as index first
    if let Ok(index) = name_or_index.parse::<usize>() {
        return devices.get(index).cloned().ok_or_else(|| {
            Error::DeviceNotFound(format!(
                "{} device index {} (only {} devices available)",
                kind,
                index,
                devices.len()
            ))
        });
    }

    // Try exact match
    for device in devices {
        if device_name(device).is_ok_and(|n| n == name_or_index) {
            return Ok(device.clone());
        }
    }

    // Try case-insensitive partial match
    let search_lower = name_or_index.to_lowercase();
    let mut matches: Vec<_> = devices
        .iter()
        .filter_map(|d| {
            device_name(d).ok().and_then(|name| {
                if name.to_lowercase().contains(&search_lower) {
                    Some((d.clone(), name))
                } else {
                    None
                }
            })
        })
        .collect();

    match matches.len() {
        0 => Err(Error::DeviceNotFound(format!(
            "no {} device matching '{}'",
            kind, name_or_index
        ))),
        1 => Ok(matches.remove(0).0),
        _ => {
            // Multiple matches - return first but warn
            let names: Vec<_> = matches.iter().map(|(_, n)| n.as_str()).collect();
            eprintln!(
                "Warning: '{}' matches multiple {} devices: {:?}. Using first match: {}",
                name_or_index, kind, names, names[0]
            );
            Ok(matches.remove(0).0)
        }
    }
}

/// Find a device by partial name match (case-insensitive).
///
/// Returns the first device whose name contains the search string.
/// Useful for user-friendly device selection.
///
/// # Example
///
/// ```rust,ignore
/// use sonido_io::find_device_fuzzy;
///
/// // Find any USB audio device
/// let device = find_device_fuzzy("USB", true)?;  // input
/// let device = find_device_fuzzy("USB", false)?; // output
/// ```
pub fn find_device_fuzzy(search: &str, is_input: bool) -> Result<AudioDevice> {
    let devices = list_devices()?;
    let search_lower = search.to_lowercase();

    let filtered: Vec<_> = devices
        .iter()
        .filter(|d| {
            let matches_type = if is_input { d.is_input } else { d.is_output };
            matches_type && d.name.to_lowercase().contains(&search_lower)
        })
        .collect();

    match filtered.len() {
        0 => Err(Error::DeviceNotFound(format!(
            "no {} device matching '{}'",
            if is_input { "input" } else { "output" },
            search
        ))),
        _ => Ok(filtered[0].clone()),
    }
}

/// Find a device by index.
///
/// # Arguments
///
/// * `index` - Zero-based device index
/// * `is_input` - Whether to search input devices (true) or output devices (false)
///
/// # Example
///
/// ```rust,ignore
/// use sonido_io::find_device_by_index;
///
/// let device = find_device_by_index(0, true)?;  // First input device
/// let device = find_device_by_index(1, false)?; // Second output device
/// ```
pub fn find_device_by_index(index: usize, is_input: bool) -> Result<AudioDevice> {
    let devices = list_devices()?;

    let filtered: Vec<_> = devices
        .iter()
        .filter(|d| if is_input { d.is_input } else { d.is_output })
        .collect();

    filtered.get(index).cloned().cloned().ok_or_else(|| {
        Error::DeviceNotFound(format!(
            "{} device index {} (only {} devices available)",
            if is_input { "input" } else { "output" },
            index,
            filtered.len()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_devices() {
        // This test just verifies the function doesn't panic
        // Actual device availability depends on the system
        let _host = crate::audio_host_lock();
        let result = list_devices();
        assert!(result.is_ok());
    }

    #[test]
    fn test_default_device() {
        let _host = crate::audio_host_lock();
        let result = default_device();
        assert!(result.is_ok());
    }
}
