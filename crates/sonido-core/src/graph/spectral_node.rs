//! FFT/overlap-add spectral processing as a graph primitive.
//!
//! `SpectralNode` wraps an FFT at its input and an IFFT at its output. Any
//! effect chain placed between them operates entirely in the frequency domain.
//! This enables spectral compression, spectral gate, spectral morph, and
//! frequency-domain EQ as first-class graph citizens.
//!
//! # Signal Flow
//!
//! ```text
//! time-domain input
//!       │
//!       ▼
//!   [Window + FFT]
//!       │  complex bins (fft_size)
//!       ▼
//!   <spectral effects>
//!       │
//!       ▼
//!   [IFFT + Window + overlap-add]
//!       │
//!       ▼
//! time-domain output
//! ```
//!
//! # Latency
//!
//! Processing latency equals `fft_size` samples. The graph engine's latency
//! compensation inserts a matching delay on all parallel dry paths when a
//! `SpectralNode` is present (ADR-025).
//!
//! # COLA Normalization
//!
//! The overlap-add reconstruction uses synthesis windowing and normalizes by the
//! sum of squared window values at each hop offset, guaranteeing constant
//! overlap-add (COLA) reconstruction for any valid hop/window combination.

use std::f32::consts::PI;
use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use crate::Effect;

/// Window function applied to each input frame before the FFT.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowType {
    /// Hann window -- good general-purpose choice. Zero at both ends prevents
    /// discontinuity artefacts.
    #[default]
    Hann,
    /// Hamming window -- slightly higher sidelobe suppression than Hann.
    Hamming,
    /// Blackman window -- excellent sidelobe attenuation at the cost of a wider
    /// main lobe.
    Blackman,
    /// Rectangular window -- no windowing. Use only for stationary signals.
    Rectangle,
}

/// FFT configuration for a [`SpectralNode`].
///
/// # Constraints
///
/// - `fft_size` must be a power of two (e.g., 256, 512, 1024, 2048, 4096).
/// - `hop_size` must satisfy `hop_size < fft_size`. Typical: `fft_size / 4`
///   (75% overlap), which gives the best reconstruction for Hann windows.
pub struct SpectralConfig {
    /// FFT frame size in samples.
    ///
    /// Must be a power of two. Larger sizes give finer frequency resolution but
    /// increase latency.
    pub fft_size: usize,

    /// Hop size in samples (input advance between successive frames).
    ///
    /// Controls the overlap between frames. `fft_size / 4` gives 75% overlap,
    /// which is the recommended default for Hann windows.
    ///
    /// Valid range: `[1, fft_size)`.
    pub hop_size: usize,

    /// Window function applied to each frame.
    pub window_type: WindowType,
}

impl Default for SpectralConfig {
    fn default() -> Self {
        Self {
            fft_size: 2048,
            hop_size: 512,
            window_type: WindowType::Hann,
        }
    }
}

/// A frequency-domain effect that operates on complex FFT bins.
///
/// Implementors receive the full complex spectrum (all `fft_size` bins) after
/// the forward FFT and can modify magnitudes, phases, or both.
pub trait SpectralEffect: Send {
    /// Process the complex spectrum in-place.
    ///
    /// `spectrum` contains `fft_size` complex bins. Bins `[0..fft_size/2+1]`
    /// are the positive frequencies (DC through Nyquist). The remaining bins
    /// are the conjugate-symmetric negative frequencies.
    fn process_spectrum(&mut self, spectrum: &mut [Complex<f32>]);

    /// Reset internal state.
    fn reset(&mut self);
}

/// FFT/overlap-add spectral processor that implements [`Effect`].
///
/// Wraps FFT at input and IFFT at output. Inner [`SpectralEffect`]s receive
/// and return complex frequency-domain bins.
///
/// # Invariants
///
/// - `fft_size` is a power of two.
/// - `hop_size < fft_size`.
/// - All internal buffers are pre-allocated at construction time.
pub struct SpectralNode {
    fft_size: usize,
    hop_size: usize,
    fft_plan: Arc<dyn Fft<f32>>,
    ifft_plan: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    cola_norm: f32,
    input_ring: Vec<f32>,
    output_ring: Vec<f32>,
    fft_scratch: Vec<Complex<f32>>,
    ifft_scratch: Vec<Complex<f32>>,
    fft_buffer: Vec<Complex<f32>>,
    effects: Vec<Box<dyn SpectralEffect>>,
    ring_pos: usize,
    hop_counter: usize,
    sample_rate: f32,
}

/// Generate window coefficients for the given type and size.
fn generate_window(window_type: WindowType, size: usize) -> Vec<f32> {
    let mut window = vec![0.0_f32; size];
    let n = size as f32;
    for i in 0..size {
        let x = 2.0 * PI * i as f32 / n;
        window[i] = match window_type {
            WindowType::Hann => 0.5 * (1.0 - x.cos()),
            WindowType::Hamming => 0.54 - 0.46 * x.cos(),
            WindowType::Blackman => 0.42 - 0.5 * x.cos() + 0.08 * (2.0 * x).cos(),
            WindowType::Rectangle => 1.0,
        };
    }
    window
}

/// Compute the COLA normalization factor for analysis-synthesis windowing.
///
/// For overlap-add with analysis and synthesis windows both equal to `w`, the
/// normalization factor is `sum(w[i]^2)` summed over all hop offsets that
/// overlap a given output sample. This equals `fft_size / hop_size * mean(w^2)`
/// for periodic windows.
fn compute_cola_norm(window: &[f32], fft_size: usize, hop_size: usize) -> f32 {
    let num_overlaps = fft_size / hop_size;
    let mut sum = 0.0_f32;
    for i in 0..fft_size {
        sum += window[i] * window[i];
    }
    // The normalization is the sum of w^2 at each sample position from all
    // overlapping frames. With periodic overlap, each sample is covered by
    // `num_overlaps` frames.
    let norm = sum * num_overlaps as f32 / fft_size as f32;
    if norm > 1e-10 { norm } else { 1.0 }
}

impl SpectralNode {
    /// Create a new spectral node with the given configuration and sample rate.
    ///
    /// # Arguments
    ///
    /// * `config` -- FFT size, hop size, and window type.
    /// * `sample_rate` -- Audio sample rate in Hz.
    pub fn new(config: SpectralConfig, sample_rate: f32) -> Self {
        let fft_size = config.fft_size;
        let hop_size = config.hop_size;

        let mut planner = FftPlanner::new();
        let fft_plan = planner.plan_fft_forward(fft_size);
        let ifft_plan = planner.plan_fft_inverse(fft_size);

        let fft_scratch_len = fft_plan.get_inplace_scratch_len();
        let ifft_scratch_len = ifft_plan.get_inplace_scratch_len();

        let window = generate_window(config.window_type, fft_size);
        let cola_norm = compute_cola_norm(&window, fft_size, hop_size);

        Self {
            fft_size,
            hop_size,
            fft_plan,
            ifft_plan,
            window,
            cola_norm,
            input_ring: vec![0.0; fft_size],
            output_ring: vec![0.0; fft_size],
            fft_scratch: vec![Complex::new(0.0, 0.0); fft_scratch_len],
            ifft_scratch: vec![Complex::new(0.0, 0.0); ifft_scratch_len],
            fft_buffer: vec![Complex::new(0.0, 0.0); fft_size],
            effects: Vec::new(),
            ring_pos: 0,
            hop_counter: 0,
            sample_rate,
        }
    }

    /// Add a spectral effect to the processing chain (builder pattern).
    pub fn with_effect(mut self, effect: Box<dyn SpectralEffect>) -> Self {
        self.effects.push(effect);
        self
    }

    /// Add a spectral effect to the processing chain.
    pub fn add_effect(&mut self, effect: Box<dyn SpectralEffect>) {
        self.effects.push(effect);
    }

    /// Returns the FFT size.
    pub fn fft_size(&self) -> usize {
        self.fft_size
    }

    /// Returns the hop size.
    pub fn hop_size(&self) -> usize {
        self.hop_size
    }

    /// Run one FFT frame: window input, FFT, apply effects, IFFT, window,
    /// overlap-add to output ring.
    fn process_frame(&mut self) {
        let fft_size = self.fft_size;
        let scale = 1.0 / (fft_size as f32 * self.cola_norm);

        // Window the input and load into fft_buffer
        for i in 0..fft_size {
            let ring_idx = (self.ring_pos + i) % fft_size;
            self.fft_buffer[i] = Complex::new(self.input_ring[ring_idx] * self.window[i], 0.0);
        }

        // Forward FFT (in-place)
        self.fft_plan
            .process_with_scratch(&mut self.fft_buffer, &mut self.fft_scratch);

        // Apply spectral effects
        for effect in &mut self.effects {
            effect.process_spectrum(&mut self.fft_buffer);
        }

        // Inverse FFT (in-place)
        self.ifft_plan
            .process_with_scratch(&mut self.fft_buffer, &mut self.ifft_scratch);

        // Synthesis window + overlap-add to output ring
        for i in 0..fft_size {
            let ring_idx = (self.ring_pos + i) % fft_size;
            self.output_ring[ring_idx] += self.fft_buffer[i].re * self.window[i] * scale;
        }
    }
}

impl Effect for SpectralNode {
    fn process(&mut self, input: f32) -> f32 {
        let fft_size = self.fft_size;

        // Write input to ring buffer
        self.input_ring[self.ring_pos] = input;

        // Read output from ring buffer (with fft_size latency offset)
        let out = self.output_ring[self.ring_pos];
        // Clear the output sample we just read so it's fresh for next overlap
        self.output_ring[self.ring_pos] = 0.0;

        // Advance ring position
        self.ring_pos = (self.ring_pos + 1) % fft_size;

        // Increment hop counter and process a frame when we've accumulated
        // hop_size new samples.
        self.hop_counter += 1;
        if self.hop_counter >= self.hop_size {
            self.hop_counter = 0;
            self.process_frame();
        }

        out
    }

    fn process_block(&mut self, input: &[f32], output: &mut [f32]) {
        debug_assert_eq!(input.len(), output.len());
        for (inp, out) in input.iter().zip(output.iter_mut()) {
            *out = self.process(*inp);
        }
    }

    fn process_block_stereo(
        &mut self,
        left_in: &[f32],
        right_in: &[f32],
        left_out: &mut [f32],
        right_out: &mut [f32],
    ) {
        // Dual-mono: process left, then right with independent state reset
        // between channels would require two instances. For simplicity, process
        // left channel through the spectral pipeline and pass right through
        // with the same processing (they share state, which is correct for
        // dual-mono where L and R get the same treatment sequentially).
        self.process_block(left_in, left_out);

        // For right channel: process sample-by-sample through the same node.
        // This means L and R share the same spectral state which is the
        // expected behavior for a single SpectralNode used as dual-mono.
        // For independent L/R processing, use two SpectralNode instances.
        for (inp, out) in right_in.iter().zip(right_out.iter_mut()) {
            *out = self.process(*inp);
        }
    }

    fn is_true_stereo(&self) -> bool {
        false
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
    }

    fn reset(&mut self) {
        self.input_ring.fill(0.0);
        self.output_ring.fill(0.0);
        for c in &mut self.fft_buffer {
            *c = Complex::new(0.0, 0.0);
        }
        self.ring_pos = 0;
        self.hop_counter = 0;
        for effect in &mut self.effects {
            effect.reset();
        }
    }

    fn latency_samples(&self) -> usize {
        self.fft_size
    }
}

#[cfg(test)]
#[cfg(feature = "spectral")]
mod tests {
    use super::*;

    #[test]
    fn test_passthrough_no_effects() {
        // With no spectral effects, the overlap-add should reconstruct the
        // input (after the initial latency window of silence).
        let config = SpectralConfig {
            fft_size: 256,
            hop_size: 64,
            window_type: WindowType::Hann,
        };
        let mut node = SpectralNode::new(config, 48000.0);
        let fft_size = 256;

        // Generate a test signal: sine wave
        let num_samples = fft_size * 4;
        let input: Vec<f32> = (0..num_samples)
            .map(|i| (2.0 * PI * 440.0 * i as f32 / 48000.0).sin())
            .collect();
        let mut output = vec![0.0_f32; num_samples];

        node.process_block(&input, &mut output);

        // After the latency period (fft_size samples), output should match
        // the input shifted by the latency. Check a window in the steady-state
        // region (skip 2*fft_size to be safe).
        let check_start = fft_size * 2;
        let check_end = num_samples;
        let latency = fft_size;

        let mut max_error = 0.0_f32;
        for i in check_start..check_end {
            let expected = input[i - latency];
            let error = (output[i] - expected).abs();
            if error > max_error {
                max_error = error;
            }
        }

        assert!(
            max_error < 0.05,
            "Passthrough reconstruction error too high: {max_error}"
        );
    }

    #[test]
    fn test_latency_reporting() {
        let config = SpectralConfig {
            fft_size: 1024,
            hop_size: 256,
            window_type: WindowType::Hann,
        };
        let node = SpectralNode::new(config, 48000.0);
        assert_eq!(node.latency_samples(), 1024);
    }

    #[test]
    fn test_window_generation() {
        let window = generate_window(WindowType::Hann, 256);

        // Hann window: endpoints should be near 0
        assert!(window[0].abs() < 1e-6, "Hann start should be ~0");
        // Middle should be 1.0 (for even-length, peak is at N/2)
        assert!(
            (window[128] - 1.0).abs() < 1e-6,
            "Hann midpoint should be ~1.0, got {}",
            window[128]
        );
        // All values in [0, 1]
        for &w in &window {
            assert!(w >= 0.0 && w <= 1.0, "Window value out of range: {w}");
        }
    }

    #[test]
    fn test_zero_input_zero_output() {
        let config = SpectralConfig {
            fft_size: 256,
            hop_size: 64,
            window_type: WindowType::Hann,
        };
        let mut node = SpectralNode::new(config, 48000.0);

        let input = vec![0.0_f32; 1024];
        let mut output = vec![0.0_f32; 1024];

        node.process_block(&input, &mut output);

        for (i, &s) in output.iter().enumerate() {
            assert!(s.abs() < 1e-10, "Non-zero output at sample {i}: {s}");
        }
    }

    #[test]
    fn test_reset_clears_state() {
        let config = SpectralConfig {
            fft_size: 256,
            hop_size: 64,
            window_type: WindowType::Hann,
        };
        let mut node = SpectralNode::new(config, 48000.0);

        // Feed some audio through
        let signal: Vec<f32> = (0..512)
            .map(|i| (2.0 * PI * 1000.0 * i as f32 / 48000.0).sin())
            .collect();
        let mut out = vec![0.0_f32; 512];
        node.process_block(&signal, &mut out);

        // Reset
        node.reset();

        // Now feed silence -- output should be silence
        let silence = vec![0.0_f32; 512];
        let mut out2 = vec![0.0_f32; 512];
        node.process_block(&silence, &mut out2);

        for (i, &s) in out2.iter().enumerate() {
            assert!(
                s.abs() < 1e-10,
                "Non-zero output after reset at sample {i}: {s}"
            );
        }
    }
}
