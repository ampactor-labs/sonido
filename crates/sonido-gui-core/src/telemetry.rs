//! Lock-free audio → GUI telemetry: peak meters and a scope ring.
//!
//! [`SignalTap`] is written by the audio thread once per block and read by the
//! GUI once per frame. Every slot is an `AtomicU32` holding `f32` bits, so the
//! audio side never locks or allocates; a GUI read that races a block write
//! sees a partly updated ring, which a scope trace cannot tell apart from the
//! signal itself.
//!
//! The audio thread calls [`SignalTap::input`] (optional) and then
//! [`SignalTap::output`] for each block; `output` advances the ring.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

/// Frames kept in the scope ring: about 43 ms at 48 kHz.
pub const SCOPE_LEN: usize = 2048;

/// Peak magnitudes accumulated since the previous [`SignalTap::take_peaks`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Peaks {
    /// Input peaks, left and right (linear).
    pub input: [f32; 2],
    /// Output peaks, left and right (linear).
    pub output: [f32; 2],
}

/// One scope frame: the mono input beside the stereo output.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScopeFrame {
    /// Input, summed to mono.
    pub input: f32,
    /// Output left channel.
    pub left: f32,
    /// Output right channel.
    pub right: f32,
}

/// Audio-thread → GUI signal tap (see the [module docs](self)).
pub struct SignalTap {
    /// `[in L, in R, out L, out R]` peak magnitudes as `f32` bits.
    peaks: [AtomicU32; 4],
    scope_in: Box<[AtomicU32]>,
    scope_l: Box<[AtomicU32]>,
    scope_r: Box<[AtomicU32]>,
    /// Ring index of the next frame to write.
    head: AtomicUsize,
}

impl Default for SignalTap {
    fn default() -> Self {
        Self::new()
    }
}

fn ring() -> Box<[AtomicU32]> {
    (0..SCOPE_LEN).map(|_| AtomicU32::new(0)).collect()
}

/// Largest finite magnitude in `buf`; non-finite samples are ignored.
fn peak_of(buf: &[f32]) -> f32 {
    buf.iter()
        .map(|s| s.abs())
        .filter(|m| m.is_finite())
        .fold(0.0, f32::max)
}

impl SignalTap {
    /// An empty tap: silent peaks and a zeroed ring.
    pub fn new() -> Self {
        Self {
            peaks: Default::default(),
            scope_in: ring(),
            scope_l: ring(),
            scope_r: ring(),
            head: AtomicUsize::new(0),
        }
    }

    /// Raise a peak slot to `magnitude`. For non-negative floats the IEEE bit
    /// pattern orders like the value, so an integer `fetch_max` is a float max.
    fn raise(&self, slot: usize, magnitude: f32) {
        self.peaks[slot].fetch_max(magnitude.to_bits(), Ordering::Relaxed);
    }

    /// Record a block's input. Call before [`output`](Self::output) for the
    /// same block; it writes at the ring position `output` will then advance.
    pub fn input(&self, left: &[f32], right: &[f32]) {
        self.raise(0, peak_of(left));
        self.raise(1, peak_of(right));
        let head = self.head.load(Ordering::Relaxed);
        for (i, (l, r)) in left.iter().zip(right).enumerate() {
            let mono = (l + r) * 0.5;
            self.scope_in[(head + i) % SCOPE_LEN].store(mono.to_bits(), Ordering::Relaxed);
        }
    }

    /// Record a block's output and advance the ring by its length.
    pub fn output(&self, left: &[f32], right: &[f32]) {
        self.raise(2, peak_of(left));
        self.raise(3, peak_of(right));
        let head = self.head.load(Ordering::Relaxed);
        let n = left.len().min(right.len());
        for i in 0..n {
            let at = (head + i) % SCOPE_LEN;
            self.scope_l[at].store(left[i].to_bits(), Ordering::Relaxed);
            self.scope_r[at].store(right[i].to_bits(), Ordering::Relaxed);
        }
        self.head.store((head + n) % SCOPE_LEN, Ordering::Release);
    }

    /// Read and reset the peaks accumulated since the last call.
    pub fn take_peaks(&self) -> Peaks {
        let take = |slot: usize| f32::from_bits(self.peaks[slot].swap(0, Ordering::Relaxed));
        Peaks {
            input: [take(0), take(1)],
            output: [take(2), take(3)],
        }
    }

    /// Copy the newest `out.len()` frames (at most [`SCOPE_LEN`]) into `out`,
    /// oldest first.
    pub fn read_scope(&self, out: &mut [ScopeFrame]) {
        let n = out.len().min(SCOPE_LEN);
        let head = self.head.load(Ordering::Acquire);
        let start = (head + SCOPE_LEN - n) % SCOPE_LEN;
        let load = |ring: &[AtomicU32], at: usize| f32::from_bits(ring[at].load(Ordering::Relaxed));
        for (i, frame) in out.iter_mut().take(n).enumerate() {
            let at = (start + i) % SCOPE_LEN;
            *frame = ScopeFrame {
                input: load(&self.scope_in, at),
                left: load(&self.scope_l, at),
                right: load(&self.scope_r, at),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peaks_accumulate_and_reset() {
        let tap = SignalTap::new();
        tap.input(&[0.1, -0.5], &[0.2, 0.0]);
        tap.output(&[0.3, 0.0], &[-0.9, 0.4]);
        tap.output(&[0.6], &[0.1]);
        let p = tap.take_peaks();
        assert_eq!(p.input, [0.5, 0.2]);
        assert_eq!(p.output, [0.6, 0.9]);
        assert_eq!(tap.take_peaks(), Peaks::default());
    }

    #[test]
    fn non_finite_samples_do_not_stick_the_meter() {
        let tap = SignalTap::new();
        tap.output(&[f32::NAN, 0.25], &[f32::INFINITY, 0.5]);
        assert_eq!(tap.take_peaks().output, [0.25, 0.5]);
    }

    #[test]
    fn scope_returns_newest_frames_oldest_first_across_the_wrap() {
        let tap = SignalTap::new();
        let block: Vec<f32> = (0..SCOPE_LEN + 3).map(|i| i as f32).collect();
        tap.input(&block, &block);
        tap.output(&block, &block);
        let mut out = [ScopeFrame::default(); 4];
        tap.read_scope(&mut out);
        let last = (SCOPE_LEN + 2) as f32;
        assert_eq!(out[3].left, last);
        assert_eq!(out[0].left, last - 3.0);
    }
}
