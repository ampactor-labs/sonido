//! Delay line implementations for time-based effects.
//!
//! Provides circular buffer-based delay lines with interpolation support.
//! These are fundamental building blocks for many audio effects.
//!
//! # Types
//!
//! - [`InterpolatedDelay`] - Heap-allocated, variable-length delay with interpolation
//! - [`FixedDelayLine`] - Stack-allocated, compile-time fixed length
//!
//! # Use Cases
//!
//! | Effect | Delay Range | Modulation |
//! |--------|-------------|------------|
//! | Flanger | 1-10ms | Yes (LFO) |
//! | Chorus | 10-30ms | Yes (LFO) |
//! | Slapback | 50-150ms | No |
//! | Echo/Delay | 100-2000ms | Optional |
//! | Reverb (comb) | 20-100ms | No |
//!
//! # Interpolation
//!
//! For modulated delays (chorus, flanger), use [`InterpolatedDelay::read`]
//! with fractional delay times to avoid zipper noise and aliasing.

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::vec;
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

/// Interpolation method for fractional delay
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Interpolation {
    /// No interpolation (truncate to nearest sample)
    None,
    /// Linear interpolation between two samples
    #[default]
    Linear,
    /// Cubic interpolation (4-point, smoother)
    Cubic,
}

/// Interpolated delay line using a circular buffer (heap-allocated).
///
/// Supports fractional delay times through linear interpolation,
/// allowing smooth modulation of delay time without artifacts.
///
/// # Memory
///
/// The buffer is heap-allocated during construction but never reallocates.
/// No allocations occur during audio processing.
///
/// # Example
///
/// ```rust
/// use sonido_core::InterpolatedDelay;
///
/// // 50ms max delay at 44.1kHz
/// let max_delay_samples = (0.05 * 44100.0) as usize;
/// let mut delay = InterpolatedDelay::new(max_delay_samples);
///
/// // Read with 10.5 sample delay (fractional)
/// let output = delay.read(10.5);
/// delay.write(1.0);
/// ```
#[derive(Debug, Clone)]
pub struct InterpolatedDelay {
    /// Circular buffer storage
    buffer: Vec<f32>,
    /// Write position in buffer
    write_pos: usize,
    /// Interpolation method for fractional delay reads
    interpolation: Interpolation,
}

impl InterpolatedDelay {
    /// Creates a new delay line with the given maximum delay in samples.
    ///
    /// # Arguments
    ///
    /// * `max_delay_samples` - Maximum delay capacity in samples
    ///
    /// # Panics
    ///
    /// Panics if `max_delay_samples` is 0.
    pub fn new(max_delay_samples: usize) -> Self {
        assert!(max_delay_samples > 0, "Delay size must be > 0");

        Self {
            buffer: vec![0.0; max_delay_samples],
            write_pos: 0,
            interpolation: Interpolation::Linear,
        }
    }

    /// Creates a delay line from sample rate and max delay time in seconds.
    pub fn from_time(sample_rate: f32, max_seconds: f32) -> Self {
        let max_samples = (sample_rate * max_seconds) as usize + 1;
        Self::new(max_samples)
    }

    /// Sets the interpolation method for fractional delay reads.
    ///
    /// - [`Interpolation::None`]: Truncate to nearest sample (lowest CPU)
    /// - [`Interpolation::Linear`]: Interpolate between 2 samples (default)
    /// - [`Interpolation::Cubic`]: 4-point cubic interpolation (best quality,
    ///   recommended for modulated effects like chorus/flanger)
    pub fn set_interpolation(&mut self, interp: Interpolation) {
        self.interpolation = interp;
    }

    /// Reads a delayed sample with the configured interpolation method.
    ///
    /// # Arguments
    ///
    /// * `delay_samples` - Delay time in samples (can be fractional)
    ///
    /// Returns the interpolated sample from the delay line.
    #[inline]
    pub fn read(&self, delay_samples: f32) -> f32 {
        debug_assert!(delay_samples >= 0.0);

        let len = self.buffer.len();
        let delay_clamped = delay_samples.min((len - 1) as f32);

        let delay_int = delay_clamped as usize;
        let frac = delay_clamped - delay_int as f32;

        // read_pos = (write_pos + len - delay_int - 1) % len
        // This points to the sample `delay_int` samples before the last written.
        let read_pos = (self.write_pos + len - delay_int - 1) % len;

        match self.interpolation {
            Interpolation::None => self.buffer[read_pos],

            Interpolation::Linear => {
                let next_pos = if read_pos == 0 { len - 1 } else { read_pos - 1 };
                let a = self.buffer[read_pos];
                let b = self.buffer[next_pos];
                a + (b - a) * frac
            }

            Interpolation::Cubic => {
                // 4-point cubic Lagrange interpolation
                let p0 = if read_pos + 1 >= len { 0 } else { read_pos + 1 };
                let p1 = read_pos;
                let p2 = if read_pos == 0 { len - 1 } else { read_pos - 1 };
                let p3 = match read_pos {
                    0 => len - 2,
                    1 => len - 1,
                    _ => read_pos - 2,
                };

                let y0 = self.buffer[p0];
                let y1 = self.buffer[p1];
                let y2 = self.buffer[p2];
                let y3 = self.buffer[p3];

                let t = frac;
                let t2 = t * t;
                let t3 = t2 * t;

                let a0 = y3 - y2 - y0 + y1;
                let a1 = y0 - y1 - a0;
                let a2 = y2 - y0;

                a0 * t3 + a1 * t2 + a2 * t + y1
            }
        }
    }

    /// Reads a delayed sample at an exact integer position (no interpolation).
    ///
    /// This is faster than [`read()`](Self::read) when the delay time is always
    /// an integer number of samples (e.g., fixed comb/allpass filters in reverb).
    /// Skips all interpolation math — a single array lookup.
    ///
    /// # Arguments
    ///
    /// * `delay_samples` - Delay time in whole samples
    #[inline]
    pub fn read_integer(&self, delay_samples: usize) -> f32 {
        let len = self.buffer.len();
        let delay_clamped = delay_samples.min(len - 1);
        let read_pos = (self.write_pos + len - delay_clamped - 1) % len;
        self.buffer[read_pos]
    }

    /// Writes a sample to the delay line and advances the write position.
    ///
    /// # Arguments
    ///
    /// * `sample` - The sample to write
    #[inline]
    pub fn write(&mut self, sample: f32) {
        self.buffer[self.write_pos] = sample;
        self.write_pos += 1;
        if self.write_pos >= self.buffer.len() {
            self.write_pos = 0;
        }
    }

    /// Combined read and write operation.
    #[inline]
    pub fn read_write(&mut self, sample: f32, delay_samples: f32) -> f32 {
        let output = self.read(delay_samples);
        self.write(sample);
        output
    }

    /// Clears the delay line (sets all samples to 0).
    pub fn clear(&mut self) {
        self.buffer.fill(0.0);
        self.write_pos = 0;
    }

    /// Returns the maximum delay capacity in samples.
    pub fn capacity(&self) -> usize {
        self.buffer.len()
    }
}

/// Fixed-size delay line for embedded systems (no heap allocation).
///
/// Uses const generics for compile-time size specification.
///
/// # Example
///
/// ```rust
/// use sonido_core::FixedDelayLine;
///
/// // Create a delay line with max 4096 samples
/// let mut delay: FixedDelayLine<4096> = FixedDelayLine::new();
///
/// // Write and read with 100 sample delay
/// delay.write(1.0);
/// let output = delay.read(100.0);
/// ```
pub struct FixedDelayLine<const N: usize> {
    buffer: [f32; N],
    write_pos: usize,
    interpolation: Interpolation,
}

impl<const N: usize> FixedDelayLine<N> {
    /// Create a new fixed-size delay line
    pub fn new() -> Self {
        Self {
            buffer: [0.0; N],
            write_pos: 0,
            interpolation: Interpolation::Linear,
        }
    }

    /// Set interpolation method
    pub fn set_interpolation(&mut self, interp: Interpolation) {
        self.interpolation = interp;
    }

    /// Get the maximum delay in samples
    pub const fn max_delay(&self) -> usize {
        N - 1
    }

    /// Write a sample to the delay line
    #[inline]
    pub fn write(&mut self, sample: f32) {
        self.buffer[self.write_pos] = sample;
        self.write_pos += 1;
        if self.write_pos >= N {
            self.write_pos = 0;
        }
    }

    /// Read from the delay line with fractional delay
    #[inline]
    pub fn read(&self, delay_samples: f32) -> f32 {
        let delay_int = (delay_samples as usize).min(N - 1);
        let frac = delay_samples - delay_int as f32;

        let read_pos = (self.write_pos + N - delay_int - 1) % N;

        match self.interpolation {
            Interpolation::None => self.buffer[read_pos],

            Interpolation::Linear => {
                let next_pos = if read_pos == 0 { N - 1 } else { read_pos - 1 };
                let a = self.buffer[read_pos];
                let b = self.buffer[next_pos];
                a + (b - a) * frac
            }

            Interpolation::Cubic => {
                // 4-point cubic interpolation
                let p0 = if read_pos + 1 >= N { 0 } else { read_pos + 1 };
                let p1 = read_pos;
                let p2 = if read_pos == 0 { N - 1 } else { read_pos - 1 };
                let p3 = match read_pos {
                    0 => N - 2,
                    1 => N - 1,
                    _ => read_pos - 2,
                };

                let y0 = self.buffer[p0];
                let y1 = self.buffer[p1];
                let y2 = self.buffer[p2];
                let y3 = self.buffer[p3];

                let t = frac;
                let t2 = t * t;
                let t3 = t2 * t;

                let a0 = y3 - y2 - y0 + y1;
                let a1 = y0 - y1 - a0;
                let a2 = y2 - y0;
                let a3 = y1;

                a0 * t3 + a1 * t2 + a2 * t + a3
            }
        }
    }

    /// Combined read and write operation
    #[inline]
    pub fn read_write(&mut self, sample: f32, delay_samples: f32) -> f32 {
        let output = self.read(delay_samples);
        self.write(sample);
        output
    }

    /// Clear the delay line
    pub fn clear(&mut self) {
        self.buffer = [0.0; N];
        self.write_pos = 0;
    }
}

impl<const N: usize> Default for FixedDelayLine<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interpolated_delay_basic() {
        let mut delay = InterpolatedDelay::new(10);

        // Write some samples
        for i in 1..=5 {
            delay.write(i as f32);
        }

        // Read with 3 sample delay
        delay.write(6.0);
        let output = delay.read(3.0);
        assert_eq!(output, 3.0);
    }

    #[test]
    fn test_interpolated_delay_interpolation() {
        let mut delay = InterpolatedDelay::new(10);

        // Write 0, 1, 2, 3
        delay.write(0.0);
        delay.write(1.0);
        delay.write(2.0);
        delay.write(3.0);

        // Read with 1.5 sample delay - should interpolate
        let output = delay.read(1.5);
        assert!((output - 1.5).abs() < 0.01, "Expected ~1.5, got {}", output);
    }

    #[test]
    fn test_interpolated_delay_wrap() {
        let mut delay = InterpolatedDelay::new(4);

        // Fill buffer completely
        delay.write(1.0);
        delay.write(2.0);
        delay.write(3.0);
        delay.write(4.0);

        // Now write_pos wraps to 0
        delay.write(5.0);

        // Read with delay that crosses the wrap boundary
        let output = delay.read(3.0);
        assert_eq!(output, 2.0);
    }

    #[test]
    fn test_fixed_delay_line() {
        let mut delay: FixedDelayLine<128> = FixedDelayLine::new();

        delay.write(1.0);
        for _ in 0..50 {
            delay.write(0.0);
        }

        let output = delay.read(50.0);
        assert_eq!(output, 1.0);
    }

    #[test]
    fn test_fixed_delay_interpolation() {
        let mut delay: FixedDelayLine<128> = FixedDelayLine::new();

        delay.write(0.0);
        delay.write(1.0);

        let output = delay.read(0.5);
        assert!((output - 0.5).abs() < 0.01);
    }

    #[test]
    #[should_panic]
    fn test_delay_zero_size_panics() {
        let _delay = InterpolatedDelay::new(0);
    }

    #[test]
    fn test_interpolated_delay_default_is_linear() {
        let delay = InterpolatedDelay::new(16);
        assert_eq!(delay.interpolation, Interpolation::Linear);
    }

    #[test]
    fn test_interpolated_delay_none_interpolation() {
        let mut delay = InterpolatedDelay::new(16);
        delay.set_interpolation(Interpolation::None);

        // Write a ramp: 0, 1, 2, 3, 4
        for i in 0..5 {
            delay.write(i as f32);
        }

        // Fractional delay should truncate — read at 1.7 should give sample at delay 1
        let output = delay.read(1.7);
        assert_eq!(output, 3.0); // delay 1 = sample index 3 (second-to-last written)
    }

    #[test]
    fn test_interpolated_delay_cubic_smooth() {
        let mut delay = InterpolatedDelay::new(64);
        delay.set_interpolation(Interpolation::Cubic);

        // Write a sinusoidal signal
        for i in 0..32 {
            let sample = libm::sinf(i as f32 * core::f32::consts::TAU / 16.0);
            delay.write(sample);
        }

        // Read at fractional delays — cubic should produce smooth output
        let v1 = delay.read(5.0);
        let v2 = delay.read(5.25);
        let v3 = delay.read(5.5);
        let v4 = delay.read(5.75);
        let v5 = delay.read(6.0);

        // Verify monotonicity or smoothness: differences should be small
        let diffs = [
            (v2 - v1).abs(),
            (v3 - v2).abs(),
            (v4 - v3).abs(),
            (v5 - v4).abs(),
        ];
        for d in &diffs {
            assert!(*d < 1.0, "Jump too large: {d}");
        }
    }

    #[test]
    fn test_interpolated_delay_cubic_vs_linear_accuracy() {
        // For a smooth signal, cubic should be more accurate than linear
        // at fractional delay positions
        let mut delay_lin = InterpolatedDelay::new(64);
        let mut delay_cub = InterpolatedDelay::new(64);
        delay_cub.set_interpolation(Interpolation::Cubic);

        // Write identical low-frequency sine (smooth, well-suited for interpolation)
        for i in 0..32 {
            let sample = libm::sinf(i as f32 * core::f32::consts::TAU / 32.0);
            delay_lin.write(sample);
            delay_cub.write(sample);
        }

        // True value at fractional position via direct sine computation
        // delay=5.5 means 5.5 samples before last written (sample 31)
        // That's sample index 31-5.5 = 25.5, i.e. sin(25.5 * TAU/32)
        let true_val = libm::sinf(25.5 * core::f32::consts::TAU / 32.0);

        let lin_val = delay_lin.read(5.5);
        let cub_val = delay_cub.read(5.5);

        let lin_err = (lin_val - true_val).abs();
        let cub_err = (cub_val - true_val).abs();

        assert!(
            cub_err <= lin_err,
            "Cubic error ({cub_err}) should be <= linear error ({lin_err})"
        );
    }

    #[test]
    fn test_interpolated_delay_cubic_wrap_around() {
        // Verify cubic works when buffer indices wrap around
        let mut delay = InterpolatedDelay::new(8);
        delay.set_interpolation(Interpolation::Cubic);

        // Write enough samples to wrap
        for i in 0..12 {
            delay.write(i as f32);
        }

        // Read near the wrap boundary — should not panic
        let output = delay.read(6.5);
        assert!(output.is_finite());
    }
}
