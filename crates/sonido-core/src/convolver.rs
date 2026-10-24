//! Real-time convolution with a long impulse response.
//!
//! [`PartitionedConvolver`] is uniformly partitioned overlap-save (UPOLS):
//! the impulse response is cut into `P` partitions of one block each, every
//! partition's spectrum is computed once, and each new input block's spectrum
//! goes into a frequency-domain delay line. One block of output is then one
//! FFT, `P` complex multiply-accumulates per bin and one inverse FFT, instead
//! of `ir.len()` multiplies per sample. Nothing allocates after construction.
//!
//! Reference: F. Wefers, *Partitioned Convolution Algorithms for Real-Time
//! Auralization* (Logos, 2015), chapter 5.
//!
//! Requires the `spectral` feature.

use rustfft::{Fft, FftPlanner, num_complex::Complex};
use std::sync::Arc;

/// Uniformly partitioned overlap-save convolver for one channel.
pub struct PartitionedConvolver {
    block: usize,
    fft_size: usize,
    fwd: Arc<dyn Fft<f32>>,
    inv: Arc<dyn Fft<f32>>,
    /// Spectra of the IR partitions, `P × fft_size`.
    ir_spectra: Vec<Vec<Complex<f32>>>,
    /// Frequency-domain delay line of input-block spectra, `P × fft_size`.
    fdl: Vec<Vec<Complex<f32>>>,
    fdl_pos: usize,
    /// The previous input block followed by the current one.
    window: Vec<f32>,
    work: Vec<Complex<f32>>,
    acc: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
}

impl core::fmt::Debug for PartitionedConvolver {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PartitionedConvolver")
            .field("block", &self.block)
            .field("partitions", &self.ir_spectra.len())
            .finish_non_exhaustive()
    }
}

impl PartitionedConvolver {
    /// A convolver for `ir`, processing `block_size` samples at a time.
    ///
    /// `block_size` must be at least 1; a power of two keeps the FFTs fast.
    pub fn new(ir: &[f32], block_size: usize) -> Self {
        assert!(block_size > 0, "block size must be positive");
        let fft_size = 2 * block_size;
        let mut planner = FftPlanner::<f32>::new();
        let fwd = planner.plan_fft_forward(fft_size);
        let inv = planner.plan_fft_inverse(fft_size);
        let scratch_len = fwd
            .get_inplace_scratch_len()
            .max(inv.get_inplace_scratch_len());
        let mut scratch = vec![Complex::new(0.0, 0.0); scratch_len];

        let partitions = ir.len().div_ceil(block_size).max(1);
        let mut ir_spectra = Vec::with_capacity(partitions);
        for p in 0..partitions {
            let mut spec = vec![Complex::new(0.0, 0.0); fft_size];
            let start = p * block_size;
            let end = (start + block_size).min(ir.len());
            for (slot, &h) in spec.iter_mut().zip(ir.get(start..end).unwrap_or(&[])) {
                *slot = Complex::new(h, 0.0);
            }
            fwd.process_with_scratch(&mut spec, &mut scratch);
            ir_spectra.push(spec);
        }

        Self {
            block: block_size,
            fft_size,
            fwd,
            inv,
            fdl: vec![vec![Complex::new(0.0, 0.0); fft_size]; partitions],
            ir_spectra,
            fdl_pos: 0,
            window: vec![0.0; fft_size],
            work: vec![Complex::new(0.0, 0.0); fft_size],
            acc: vec![Complex::new(0.0, 0.0); fft_size],
            scratch,
        }
    }

    /// Samples per call to [`process_block`](Self::process_block).
    pub fn block_size(&self) -> usize {
        self.block
    }

    /// Number of IR partitions.
    pub fn partitions(&self) -> usize {
        self.ir_spectra.len()
    }

    /// Convolve one block. `input` and `output` must both be
    /// [`block_size`](Self::block_size) long.
    ///
    /// Output sample `n` is the convolution at the time of input sample `n`:
    /// block-wise callers get no added latency, and a caller feeding single
    /// samples pays one block of buffering.
    pub fn process_block(&mut self, input: &[f32], output: &mut [f32]) {
        let b = self.block;
        assert!(input.len() == b && output.len() == b, "block size mismatch");

        // Slide the window: previous block, then this one.
        self.window.copy_within(b.., 0);
        self.window[b..].copy_from_slice(input);
        for (w, &x) in self.work.iter_mut().zip(self.window.iter()) {
            *w = Complex::new(x, 0.0);
        }
        self.fwd
            .process_with_scratch(&mut self.work, &mut self.scratch);

        let partitions = self.ir_spectra.len();
        self.fdl[self.fdl_pos].copy_from_slice(&self.work);

        // Accumulate X_{n-p} · H_p over the partitions.
        for a in &mut self.acc {
            *a = Complex::new(0.0, 0.0);
        }
        for p in 0..partitions {
            let slot = (self.fdl_pos + partitions - p) % partitions;
            let x = &self.fdl[slot];
            let h = &self.ir_spectra[p];
            for ((a, &xv), &hv) in self.acc.iter_mut().zip(x.iter()).zip(h.iter()) {
                *a += xv * hv;
            }
        }
        self.fdl_pos = (self.fdl_pos + 1) % partitions;

        self.inv
            .process_with_scratch(&mut self.acc, &mut self.scratch);
        let scale = 1.0 / self.fft_size as f32;
        for (o, a) in output.iter_mut().zip(self.acc[b..].iter()) {
            *o = a.re * scale;
        }
    }

    /// Clear the input history; keep the impulse response.
    pub fn reset(&mut self) {
        for spec in &mut self.fdl {
            for c in spec.iter_mut() {
                *c = Complex::new(0.0, 0.0);
            }
        }
        self.window.fill(0.0);
        self.fdl_pos = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, seed: u32) -> Vec<f32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (s >> 8) as f32 / 8_388_608.0 - 1.0
            })
            .collect()
    }

    fn direct(x: &[f32], h: &[f32]) -> Vec<f32> {
        (0..x.len())
            .map(|n| {
                (0..h.len())
                    .filter(|&k| k <= n)
                    .map(|k| f64::from(h[k]) * f64::from(x[n - k]))
                    .sum::<f64>() as f32
            })
            .collect()
    }

    fn run(x: &[f32], h: &[f32], block: usize) -> Vec<f32> {
        let mut conv = PartitionedConvolver::new(h, block);
        let mut out = vec![0.0; x.len()];
        for (xi, oi) in x.chunks(block).zip(out.chunks_mut(block)) {
            conv.process_block(xi, oi);
        }
        out
    }

    #[test]
    fn matches_direct_convolution() {
        for &(ir_len, block) in &[(1usize, 64usize), (100, 64), (1000, 128), (4096, 256)] {
            let h: Vec<f32> = noise(ir_len, 7)
                .iter()
                .enumerate()
                .map(|(k, v)| v * libm::expf(-(k as f32) / (ir_len as f32 / 4.0 + 1.0)))
                .collect();
            let x = noise(block * 40, 11);
            let fast = run(&x, &h, block);
            let slow = direct(&x, &h);
            let peak = slow.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            for (n, (a, b)) in fast.iter().zip(slow.iter()).enumerate() {
                assert!(
                    (a - b).abs() <= 1e-4 * peak.max(1.0),
                    "ir {ir_len}, block {block}, sample {n}: {a} vs {b}"
                );
            }
        }
    }

    #[test]
    fn an_impulse_reproduces_the_ir() {
        let h = noise(700, 3);
        let mut x = vec![0.0; 1024];
        x[0] = 1.0;
        let out = run(&x, &h, 128);
        for k in 0..700 {
            assert!((out[k] - h[k]).abs() < 1e-5, "tap {k}");
        }
        assert!(out[700..].iter().all(|v| v.abs() < 1e-5));
    }

    #[test]
    fn reset_clears_history() {
        let h = noise(300, 5);
        let mut conv = PartitionedConvolver::new(&h, 64);
        let loud = vec![1.0; 64];
        let mut out = vec![0.0; 64];
        conv.process_block(&loud, &mut out);
        conv.reset();
        let silence = vec![0.0; 64];
        for _ in 0..8 {
            conv.process_block(&silence, &mut out);
            assert!(out.iter().all(|v| v.abs() < 1e-6));
        }
        assert_eq!(conv.partitions(), 5);
    }
}
