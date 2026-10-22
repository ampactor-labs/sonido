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

    /// Returns a shared reference to one buffer and a mutable reference to another.
    ///
    /// Uses `split_at_mut` to satisfy the borrow checker without temporary copies.
    /// This is the RT-safe alternative to copying into intermediate `Vec`s.
    ///
    /// # Panics
    ///
    /// Panics if `read_idx == write_idx` (would alias) or if either index is out of bounds.
    #[inline]
    pub fn get_ref_and_mut(
        &mut self,
        read_idx: usize,
        write_idx: usize,
    ) -> (&StereoBuffer, &mut StereoBuffer) {
        assert_ne!(read_idx, write_idx, "cannot alias buffer slots");
        if read_idx < write_idx {
            let (first, second) = self.buffers.split_at_mut(write_idx);
            (&first[read_idx], &mut second[0])
        } else {
            // read_idx > write_idx: split at read_idx
            // first = [0..read_idx) contains write_idx
            // second = [read_idx..] where second[0] is the read buffer
            let (first, second) = self.buffers.split_at_mut(read_idx);
            (&second[0], &mut first[write_idx])
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

#[cfg(test)]
mod tests {
    extern crate alloc;

    use super::*;

    // ── StereoBuffer ──────────────────────────────────────────────────────────

    #[test]
    fn stereo_buffer_new_has_correct_capacity() {
        let buf = StereoBuffer::new(64);
        assert_eq!(buf.len(), 64);
        assert_eq!(buf.left.len(), 64);
        assert_eq!(buf.right.len(), 64);
    }

    #[test]
    fn stereo_buffer_new_is_zeroed() {
        let buf = StereoBuffer::new(8);
        assert!(buf.left.iter().all(|&s| s == 0.0));
        assert!(buf.right.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn stereo_buffer_is_empty_on_zero_size() {
        let buf = StereoBuffer::new(0);
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn stereo_buffer_clear_zeros_contents() {
        let mut buf = StereoBuffer::new(4);
        buf.left[0] = 1.0;
        buf.right[0] = 2.0;
        buf.clear();
        assert!(buf.left.iter().all(|&s| s == 0.0));
        assert!(buf.right.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn stereo_buffer_copy_from_produces_same_data() {
        let mut src = StereoBuffer::new(4);
        src.left.copy_from_slice(&[1.0, 2.0, 3.0, 4.0]);
        src.right.copy_from_slice(&[5.0, 6.0, 7.0, 8.0]);

        let mut dst = StereoBuffer::new(4);
        dst.copy_from(&src);

        assert_eq!(dst.left, src.left);
        assert_eq!(dst.right, src.right);
    }

    #[test]
    fn stereo_buffer_accumulate_from_adds_samples() {
        let mut a = StereoBuffer::new(4);
        a.left.copy_from_slice(&[1.0, 1.0, 1.0, 1.0]);
        a.right.copy_from_slice(&[2.0, 2.0, 2.0, 2.0]);

        let mut b = StereoBuffer::new(4);
        b.left.copy_from_slice(&[0.5, 0.5, 0.5, 0.5]);
        b.right.copy_from_slice(&[0.25, 0.25, 0.25, 0.25]);

        a.accumulate_from(&b);

        assert!(a.left.iter().all(|&s| (s - 1.5).abs() < 1e-6));
        assert!(a.right.iter().all(|&s| (s - 2.25).abs() < 1e-6));
    }

    #[test]
    fn stereo_buffer_resize_grows_correctly() {
        let mut buf = StereoBuffer::new(4);
        buf.resize(8);
        assert_eq!(buf.len(), 8);
        // New samples must be zero.
        assert_eq!(buf.left[7], 0.0);
        assert_eq!(buf.right[7], 0.0);
    }

    // ── BufferPool ────────────────────────────────────────────────────────────

    #[test]
    fn buffer_pool_new_has_correct_count_and_block_size() {
        let pool = BufferPool::new(3, 128);
        assert_eq!(pool.count(), 3);
        assert_eq!(pool.block_size(), 128);
    }

    #[test]
    fn buffer_pool_buffers_are_zeroed_on_creation() {
        let pool = BufferPool::new(2, 32);
        for i in 0..pool.count() {
            let buf = pool.get(i);
            assert!(buf.left.iter().all(|&s| s == 0.0));
            assert!(buf.right.iter().all(|&s| s == 0.0));
        }
    }

    #[test]
    fn buffer_pool_get_mut_write_then_get_reads_same() {
        let mut pool = BufferPool::new(2, 4);
        pool.get_mut(0).left[0] = 42.0;
        assert_eq!(pool.get(0).left[0], 42.0);
    }

    #[test]
    fn buffer_pool_clear_all_zeros_written_data() {
        let mut pool = BufferPool::new(2, 4);
        pool.get_mut(0).left[0] = 1.0;
        pool.get_mut(1).right[2] = 3.0;
        pool.clear_all();
        assert!(pool.get(0).left.iter().all(|&s| s == 0.0));
        assert!(pool.get(1).right.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn buffer_pool_resize_all_changes_block_size() {
        let mut pool = BufferPool::new(2, 16);
        pool.resize_all(64);
        assert_eq!(pool.block_size(), 64);
        assert_eq!(pool.get(0).len(), 64);
        assert_eq!(pool.get(1).len(), 64);
    }

    #[test]
    fn buffer_pool_get_ref_and_mut_read_lt_write() {
        let mut pool = BufferPool::new(3, 4);
        pool.get_mut(0).left[0] = 7.0;
        let (read_buf, write_buf) = pool.get_ref_and_mut(0, 2);
        assert_eq!(read_buf.left[0], 7.0);
        write_buf.left[0] = 99.0;
        assert_eq!(pool.get(2).left[0], 99.0);
    }

    #[test]
    fn buffer_pool_get_ref_and_mut_read_gt_write() {
        let mut pool = BufferPool::new(3, 4);
        pool.get_mut(2).right[1] = 3.5;
        let (read_buf, write_buf) = pool.get_ref_and_mut(2, 0);
        assert_eq!(read_buf.right[1], 3.5);
        write_buf.right[1] = 11.0;
        assert_eq!(pool.get(0).right[1], 11.0);
    }

    #[test]
    #[should_panic(expected = "cannot alias buffer slots")]
    fn buffer_pool_get_ref_and_mut_same_index_panics() {
        let mut pool = BufferPool::new(2, 4);
        pool.get_ref_and_mut(1, 1);
    }

    // ── CompensationDelay ─────────────────────────────────────────────────────

    #[test]
    fn compensation_delay_zero_is_passthrough() {
        let mut delay = CompensationDelay::new(0);
        let (out_l, out_r) = delay.process(1.0, 2.0);
        assert_eq!(out_l, 1.0);
        assert_eq!(out_r, 2.0);
    }

    #[test]
    fn compensation_delay_delay_samples_accessor() {
        let delay = CompensationDelay::new(4);
        assert_eq!(delay.delay_samples(), 4);
    }

    #[test]
    fn compensation_delay_write_then_read_produces_same_data() {
        // With delay = 1, the output of sample N is what was written at sample N-1.
        // On the first call the buffer is zero, so output is (0,0).
        let mut delay = CompensationDelay::new(1);
        let (first_l, first_r) = delay.process(5.0, 9.0);
        assert_eq!(first_l, 0.0); // initial silence comes out first
        assert_eq!(first_r, 0.0);
        let (second_l, second_r) = delay.process(0.0, 0.0);
        assert_eq!(second_l, 5.0); // previous write comes out
        assert_eq!(second_r, 9.0);
    }

    #[test]
    fn compensation_delay_wrap_around_at_capacity() {
        // Delay of 2: output should be 2 samples behind input.
        let mut delay = CompensationDelay::new(2);
        let inputs_l = [1.0f32, 2.0, 3.0, 4.0, 5.0];
        let inputs_r = [10.0f32, 20.0, 30.0, 40.0, 50.0];
        let mut out_l = [0.0f32; 5];
        let mut out_r = [0.0f32; 5];
        for i in 0..5 {
            let (l, r) = delay.process(inputs_l[i], inputs_r[i]);
            out_l[i] = l;
            out_r[i] = r;
        }
        // First two outputs are the initial zeros; from sample 2 on, the lag is 2.
        assert_eq!(out_l[0], 0.0);
        assert_eq!(out_l[1], 0.0);
        assert_eq!(out_l[2], 1.0);
        assert_eq!(out_l[3], 2.0);
        assert_eq!(out_l[4], 3.0);
        assert_eq!(out_r[2], 10.0);
    }

    #[test]
    fn compensation_delay_clear_resets_state() {
        let mut delay = CompensationDelay::new(2);
        delay.process(1.0, 1.0);
        delay.process(1.0, 1.0);
        delay.clear();
        // After clear, outputs should be silence (zeroed buffer, write_pos = 0).
        let (l, r) = delay.process(0.0, 0.0);
        assert_eq!(l, 0.0);
        assert_eq!(r, 0.0);
    }

    #[test]
    fn compensation_delay_process_block_inplace_zero_is_passthrough() {
        let mut delay = CompensationDelay::new(0);
        let mut left = [1.0f32, 2.0, 3.0];
        let mut right = [4.0f32, 5.0, 6.0];
        delay.process_block_inplace(&mut left, &mut right);
        assert_eq!(left, [1.0, 2.0, 3.0]);
        assert_eq!(right, [4.0, 5.0, 6.0]);
    }

    #[test]
    fn compensation_delay_process_block_inplace_delays_signal() {
        // Delay of 3: the first 3 output samples are zeros, then the inputs appear.
        let mut delay = CompensationDelay::new(3);
        let mut left = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0];
        let mut right = left;
        delay.process_block_inplace(&mut left, &mut right);
        assert_eq!(left[0], 0.0);
        assert_eq!(left[1], 0.0);
        assert_eq!(left[2], 0.0);
        assert_eq!(left[3], 1.0);
        assert_eq!(left[4], 2.0);
        assert_eq!(left[5], 3.0);
    }
}
