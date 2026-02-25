//! Audio buffer pool and delay lines for the DAG routing engine.
//!
//! The [`BufferPool`] manages a set of [`StereoBuffer`]s that are shared across
//! processing steps. Buffer assignment uses liveness analysis (register allocation)
//! to minimize memory: a buffer is "live" from the step that writes it to the last
//! step that reads it, then its slot is freed for reuse.
//!
//! The [`CompensationDelay`] provides fixed-delay ring buffers for latency
//! compensation on parallel paths.

#[cfg(not(feature = "std"))]
use alloc::vec;
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

/// A stereo audio buffer (interleaved left/right channels).
pub struct StereoBuffer {
    /// Left channel samples.
    pub left: Vec<f32>,
    /// Right channel samples.
    pub right: Vec<f32>,
}

impl StereoBuffer {
    /// Creates a new zeroed stereo buffer with the given block size.
    pub fn new(block_size: usize) -> Self {
        Self {
            left: vec![0.0; block_size],
            right: vec![0.0; block_size],
        }
    }

    /// Fills both channels with zeros.
    pub fn clear(&mut self) {
        self.left.fill(0.0);
        self.right.fill(0.0);
    }

    /// Resizes both channels to the given block size, zeroing new samples.
    pub fn resize(&mut self, block_size: usize) {
        self.left.resize(block_size, 0.0);
        self.right.resize(block_size, 0.0);
    }

    /// Returns the number of samples per channel.
    pub fn len(&self) -> usize {
        self.left.len()
    }

    /// Returns true if the buffer has zero length.
    pub fn is_empty(&self) -> bool {
        self.left.is_empty()
    }

    /// Copies contents from another buffer.
    pub fn copy_from(&mut self, other: &StereoBuffer) {
        self.left.copy_from_slice(&other.left);
        self.right.copy_from_slice(&other.right);
    }

    /// Adds another buffer's contents sample-by-sample (mix/accumulate).
    pub fn accumulate_from(&mut self, other: &StereoBuffer) {
        for (dst, src) in self.left.iter_mut().zip(other.left.iter()) {
            *dst += *src;
        }
        for (dst, src) in self.right.iter_mut().zip(other.right.iter()) {
            *dst += *src;
        }
    }
}

/// Pool of reusable stereo audio buffers.
///
/// Buffer slots are assigned during schedule compilation via liveness analysis.
/// The pool is sized to the minimum number of simultaneously live buffers,
/// not one-per-edge. For a 20-node linear chain this yields 2 buffers (ping-pong).
pub struct BufferPool {
    buffers: Vec<StereoBuffer>,
    block_size: usize,
}

impl BufferPool {
    /// Creates a pool with the given number of buffer slots and block size.
    pub fn new(count: usize, block_size: usize) -> Self {
        let buffers = (0..count).map(|_| StereoBuffer::new(block_size)).collect();
        Self {
            buffers,
            block_size,
        }
    }

    /// Returns the number of buffer slots.
    pub fn count(&self) -> usize {
        self.buffers.len()
    }

    /// Returns the block size of each buffer.
    pub fn block_size(&self) -> usize {
        self.block_size
    }

    /// Returns a reference to the buffer at the given index.
    ///
    /// # Panics
    ///
    /// Panics if `idx >= count()`.
    #[inline]
    pub fn get(&self, idx: usize) -> &StereoBuffer {
        &self.buffers[idx]
    }

    /// Returns a mutable reference to the buffer at the given index.
    ///
    /// # Panics
    ///
    /// Panics if `idx >= count()`.
    #[inline]
    pub fn get_mut(&mut self, idx: usize) -> &mut StereoBuffer {
        &mut self.buffers[idx]
    }

    /// Resizes all buffers to a new block size.
    pub fn resize_all(&mut self, block_size: usize) {
        self.block_size = block_size;
        for buf in &mut self.buffers {
            buf.resize(block_size);
        }
    }

    /// Clears all buffers to zero.
    pub fn clear_all(&mut self) {
        for buf in &mut self.buffers {
            buf.clear();
        }
    }
}

/// Fixed-delay stereo ring buffer for latency compensation.
///
/// When parallel paths in the DAG have different latencies, shorter paths
/// need delay insertion to align with the longest path at each merge point.
/// This ring buffer provides that compensation with zero-allocation processing.
pub struct CompensationDelay {
    left: Vec<f32>,
    right: Vec<f32>,
    write_pos: usize,
    delay_samples: usize,
}

impl CompensationDelay {
    /// Creates a new compensation delay line.
    ///
    /// `delay_samples` is the fixed delay to apply. If 0, the delay is a no-op.
    pub fn new(delay_samples: usize) -> Self {
        let len = if delay_samples == 0 { 1 } else { delay_samples };
        Self {
            left: vec![0.0; len],
            right: vec![0.0; len],
            write_pos: 0,
            delay_samples,
        }
    }

    /// Returns the delay in samples.
    pub fn delay_samples(&self) -> usize {
        self.delay_samples
    }

    /// Processes a stereo sample pair through the delay line.
    ///
    /// Returns the delayed output and writes the new input.
    #[inline]
    pub fn process(&mut self, left_in: f32, right_in: f32) -> (f32, f32) {
        if self.delay_samples == 0 {
            return (left_in, right_in);
        }
        let read_pos = self.write_pos;
        let out_l = self.left[read_pos];
        let out_r = self.right[read_pos];
        self.left[self.write_pos] = left_in;
        self.right[self.write_pos] = right_in;
        self.write_pos = (self.write_pos + 1) % self.delay_samples;
        (out_l, out_r)
    }

    /// Processes an entire block through the delay line in-place.
    pub fn process_block_inplace(&mut self, left: &mut [f32], right: &mut [f32]) {
        if self.delay_samples == 0 {
            return;
        }
        for i in 0..left.len() {
            let out_l = self.left[self.write_pos];
            let out_r = self.right[self.write_pos];
            self.left[self.write_pos] = left[i];
            self.right[self.write_pos] = right[i];
            self.write_pos = (self.write_pos + 1) % self.delay_samples;
            left[i] = out_l;
            right[i] = out_r;
        }
    }

    /// Clears the delay line to silence.
    pub fn clear(&mut self) {
        self.left.fill(0.0);
        self.right.fill(0.0);
        self.write_pos = 0;
    }
}
