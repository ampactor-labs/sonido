//! Stereo loop buffer for looper effects.
//!
//! [`LoopBuffer`] provides fixed-length stereo recording and playback with explicit
//! record/play positions. Unlike [`crate::InterpolatedDelay`], no interpolation is
//! used — the loop length is fixed after recording and sample-accurate playback is
//! preferred.
//!
//! # Design
//!
//! The buffer holds two `Vec<f32>` (left and right channels) allocated once at
//! construction up to `max_samples` capacity. The workflow is:
//!
//! 1. **Record**: `write()` advances `write_pos`, filling the buffer.
//! 2. **Freeze**: `set_loop_end(write_pos)` locks the loop length.
//! 3. **Playback**: `read()` wraps at `loop_end`, delivering the recorded audio.
//! 4. **Overdub**: `read_at_write_pos()` returns the current buffer value at
//!    `write_pos` for feedback mixing before the next `write()` call.
//!
//! # no_std
//!
//! This module is `no_std` compatible. The `Vec` allocations are performed once
//! at construction — no allocations occur in the audio path.

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::vec;
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

/// Stereo loop buffer for recording and playback.
///
/// Unlike [`crate::InterpolatedDelay`], `LoopBuffer` uses explicit record/play positions
/// with no interpolation — appropriate for looper effects where the loop length is fixed
/// after recording.
///
/// # Invariants
///
/// - `write_pos` is always in `[0, max_samples)`.
/// - `read_pos` is always in `[0, loop_end)` when `loop_end > 0`, or `0` otherwise.
/// - `loop_end <= max_samples` at all times.
/// - Both `buf_l` and `buf_r` always have length `max_samples`.
pub struct LoopBuffer {
    /// Left channel sample storage.
    buf_l: Vec<f32>,
    /// Right channel sample storage.
    buf_r: Vec<f32>,
    /// Current write head position.
    write_pos: usize,
    /// Current read head position.
    read_pos: usize,
    /// Loop endpoint — `read()` wraps here. `0` means no loop has been set.
    loop_end: usize,
    /// Maximum number of samples the buffer can hold (allocated size).
    max_samples: usize,
}

impl LoopBuffer {
    /// Allocate a new stereo loop buffer of `max_samples` capacity.
    ///
    /// Both channels are zero-filled. All positions are initialised to 0 and
    /// `loop_end` is set to 0 (no loop set yet).
    ///
    /// # Parameters
    ///
    /// - `max_samples`: Maximum loop length in samples per channel. Memory is
    ///   allocated once here; no allocation occurs in the audio path.
    pub fn new(max_samples: usize) -> Self {
        Self {
            buf_l: vec![0.0; max_samples],
            buf_r: vec![0.0; max_samples],
            write_pos: 0,
            read_pos: 0,
            loop_end: 0,
            max_samples,
        }
    }

    /// Write a stereo sample pair at the current write position and advance.
    ///
    /// `write_pos` increments by 1 after each write. If `write_pos` would exceed
    /// `max_samples - 1` it clamps to `max_samples - 1` (recording stops at buffer
    /// boundary; the caller should stop recording before reaching this limit).
    ///
    /// # Parameters
    ///
    /// - `left`: Left channel sample.
    /// - `right`: Right channel sample.
    pub fn write(&mut self, left: f32, right: f32) {
        self.buf_l[self.write_pos] = left;
        self.buf_r[self.write_pos] = right;
        if self.write_pos + 1 < self.max_samples {
            self.write_pos += 1;
        }
    }

    /// Read a stereo sample pair at the current read position and advance.
    ///
    /// `read_pos` increments after each read and wraps to 0 when it reaches
    /// `loop_end`. If `loop_end` is 0 (no loop set), returns silence `(0.0, 0.0)`.
    ///
    /// # Returns
    ///
    /// `(left, right)` — sample pair at the current read position.
    pub fn read(&mut self) -> (f32, f32) {
        if self.loop_end == 0 {
            return (0.0, 0.0);
        }
        let l = self.buf_l[self.read_pos];
        let r = self.buf_r[self.read_pos];
        self.read_pos += 1;
        if self.read_pos >= self.loop_end {
            self.read_pos = 0;
        }
        (l, r)
    }

    /// Read the buffer at the current write position without advancing either position.
    ///
    /// Used in overdub mode to obtain the existing loop content at `write_pos` so it
    /// can be mixed with the new input before `write()` is called. Calling this method
    /// does **not** advance `write_pos` or `read_pos`.
    ///
    /// # Returns
    ///
    /// `(left, right)` — existing sample at `write_pos`.
    pub fn read_at_write_pos(&self) -> (f32, f32) {
        (self.buf_l[self.write_pos], self.buf_r[self.write_pos])
    }

    /// Zero-fill both channels and reset all positions to 0.
    ///
    /// After `clear()`, `loop_end` is also reset to 0 (no loop).
    pub fn clear(&mut self) {
        for s in &mut self.buf_l {
            *s = 0.0;
        }
        for s in &mut self.buf_r {
            *s = 0.0;
        }
        self.write_pos = 0;
        self.read_pos = 0;
        self.loop_end = 0;
    }

    /// Set the loop endpoint (called when recording stops).
    ///
    /// Clamps `end` to `max_samples` to prevent out-of-bounds reads. A value of 0
    /// means "no loop" — `read()` will return silence.
    ///
    /// # Parameters
    ///
    /// - `end`: Number of valid samples in the loop. Typically `write_position()`
    ///   at the moment recording stops.
    pub fn set_loop_end(&mut self, end: usize) {
        self.loop_end = end.min(self.max_samples);
    }

    /// Reset the read position to the start of the loop.
    ///
    /// Used when transitioning from Record→Play so playback starts at sample 0.
    pub fn reset_read_pos(&mut self) {
        self.read_pos = 0;
    }

    /// Reset the write position to the start of the buffer.
    ///
    /// Used when starting a fresh recording.
    pub fn reset_write_pos(&mut self) {
        self.write_pos = 0;
    }

    /// Current write position in samples.
    pub fn write_position(&self) -> usize {
        self.write_pos
    }

    /// Current loop length in samples (0 if no loop has been set).
    pub fn loop_length(&self) -> usize {
        self.loop_end
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    /// A freshly created buffer returns silence before any writes.
    #[test]
    fn new_returns_silence() {
        let mut buf = LoopBuffer::new(1024);
        // loop_end is 0 → read() returns silence
        assert_eq!(buf.read(), (0.0, 0.0));
        assert_eq!(buf.loop_length(), 0);
        assert_eq!(buf.write_position(), 0);
    }

    /// Written samples are readable back after setting loop_end.
    #[test]
    fn write_then_read_roundtrip() {
        let mut buf = LoopBuffer::new(100);
        buf.write(0.1, 0.2);
        buf.write(0.3, 0.4);
        buf.set_loop_end(2);

        let (l0, r0) = buf.read();
        assert!((l0 - 0.1).abs() < 1e-6, "l0={l0}");
        assert!((r0 - 0.2).abs() < 1e-6, "r0={r0}");

        let (l1, r1) = buf.read();
        assert!((l1 - 0.3).abs() < 1e-6, "l1={l1}");
        assert!((r1 - 0.4).abs() < 1e-6, "r1={r1}");
    }

    /// read() wraps at loop_end.
    #[test]
    fn read_wraps_at_loop_end() {
        let mut buf = LoopBuffer::new(10);
        buf.write(1.0, -1.0);
        buf.write(0.5, -0.5);
        buf.set_loop_end(2);

        // Read entire loop twice to verify wrap
        for cycle in 0..2_u32 {
            let (l0, _) = buf.read();
            assert!((l0 - 1.0).abs() < 1e-6, "cycle {cycle}: l0={l0}");
            let (l1, _) = buf.read();
            assert!((l1 - 0.5).abs() < 1e-6, "cycle {cycle}: l1={l1}");
        }
    }

    /// clear() zeros the buffer and resets all positions.
    #[test]
    fn clear_resets_state() {
        let mut buf = LoopBuffer::new(10);
        buf.write(1.0, 1.0);
        buf.write(1.0, 1.0);
        buf.set_loop_end(2);
        buf.clear();

        assert_eq!(buf.loop_length(), 0, "loop_end should be 0 after clear");
        assert_eq!(buf.write_position(), 0, "write_pos should be 0 after clear");
        assert_eq!(buf.read(), (0.0, 0.0), "read after clear should be silence");
    }

    /// read_at_write_pos returns existing content without advancing positions.
    #[test]
    fn read_at_write_pos_does_not_advance() {
        let mut buf = LoopBuffer::new(10);
        buf.write(0.7, 0.8); // write_pos is now 1
        buf.set_loop_end(1);

        // write_pos is 1; buf[1] is 0.0 (unwritten)
        let (l, r) = buf.read_at_write_pos();
        assert!((l - 0.0).abs() < 1e-6, "l={l}");
        assert!((r - 0.0).abs() < 1e-6, "r={r}");

        // write_pos must not have changed
        assert_eq!(buf.write_position(), 1);
    }

    /// set_loop_end clamps to max_samples.
    #[test]
    fn set_loop_end_clamps() {
        let mut buf = LoopBuffer::new(100);
        buf.set_loop_end(999);
        assert_eq!(
            buf.loop_length(),
            100,
            "loop_end should clamp to max_samples"
        );
    }
}
