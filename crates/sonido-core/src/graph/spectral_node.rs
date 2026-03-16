//! Spectral processing as a graph primitive.
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
//!       │  complex bins (N/2+1)
//!       ▼
//!   <spectral effects>
//!       │
//!       ▼
//!   [IFFT + overlap-add]
//!       │
//!       ▼
//! time-domain output
//! ```
//!
//! # Latency
//!
//! Processing latency equals `fft_size` samples.  The graph engine's latency
//! compensation inserts a matching delay on all parallel dry paths when a
//! `SpectralNode` is present (ADR-025).
//!
//! # Status
//!
//! Types defined.  FFT/IFFT implementation requires a `no_std`-compatible FFT
//! crate (`microfft` recommended).  Overlap-add and window buffers are stubs.

/// Window function applied to each input frame before the FFT.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowType {
    /// Hann window — good general-purpose choice.  Zero at both ends prevents
    /// discontinuity artefacts.
    #[default]
    Hann,
    /// Hamming window — slightly higher sidelobe suppression than Hann.
    Hamming,
    /// Blackman window — excellent sidelobe attenuation at the cost of a wider
    /// main lobe.
    Blackman,
    /// Rectangular window — no windowing.  Use only for stationary signals.
    Rectangle,
}

/// FFT configuration for a [`SpectralNode`].
///
/// # Constraints
///
/// - `fft_size` must be a power of two in {512, 1024, 2048, 4096}.
/// - `hop_size` must satisfy `hop_size < fft_size`.  Typical: `fft_size / 4`
///   (75 % overlap), which gives the best reconstruction for Hann windows.
/// - `hop_size` must divide evenly into the block size supplied to
///   `SpectralNode::process`.
pub struct SpectralConfig {
    /// FFT frame size in samples.
    ///
    /// Valid values: 512, 1024, 2048, 4096.  Larger sizes give finer frequency
    /// resolution but increase latency.
    pub fft_size: usize,

    /// Hop size in samples (input advance between successive frames).
    ///
    /// Controls the overlap between frames.  `fft_size / 4` gives 75 % overlap,
    /// which is the recommended default for Hann windows.
    ///
    /// Valid range: `[1, fft_size)`.
    pub hop_size: usize,

    /// Window function applied to each input frame.
    pub window: WindowType,
}

impl Default for SpectralConfig {
    fn default() -> Self {
        Self {
            fft_size: 1024,
            hop_size: 256,
            window: WindowType::Hann,
        }
    }
}

/// A spectral processing node in the [`ProcessingGraph`](crate::graph::ProcessingGraph).
///
/// Wraps FFT at input and IFFT at output.  Inner spectral effects receive and
/// return complex frequency-domain bins.
///
/// # Invariants
///
/// - `config.fft_size` is a power of two and ∈ {512, 1024, 2048, 4096}.
/// - `config.hop_size < config.fft_size`.
///
/// # Status
///
/// TODO: Implement with `microfft` or a custom radix-2 FFT.  The overlap-add
/// buffers and window coefficients are placeholders.
pub struct SpectralNode {
    config: SpectralConfig,
    // TODO: FFT/IFFT plan, overlap-add input/output ring buffers (fft_size samples each),
    //       precomputed window coefficients (fft_size f32 values),
    //       complex scratch buffer (fft_size/2 + 1 complex pairs).
}

impl SpectralNode {
    /// Create a new spectral node with the given configuration.
    ///
    /// # Arguments
    ///
    /// * `config` — FFT size, hop size, and window type.
    pub fn new(config: SpectralConfig) -> Self {
        // TODO: allocate overlap-add buffers, precompute window
        Self { config }
    }

    /// Returns the processing latency introduced by this node, in samples.
    ///
    /// Equal to `config.fft_size`.
    pub fn latency_samples(&self) -> usize {
        self.config.fft_size
    }

    /// Returns the configuration used to construct this node.
    pub fn config(&self) -> &SpectralConfig {
        &self.config
    }
}
