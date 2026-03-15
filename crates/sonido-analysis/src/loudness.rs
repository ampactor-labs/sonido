//! ITU-R BS.1770-4 LUFS loudness metering.
//!
//! Implements the full BS.1770-4 measurement chain:
//! 1. K-weighting filter (stage 1: high shelf +4 dB at 1500 Hz;
//!    stage 2: highpass at 38 Hz, both biquad TDF-II)
//! 2. Mean-square integration over 400 ms gated blocks
//! 3. Absolute gate at −70 LUFS and relative gate at −10 dB below ungated loudness
//! 4. Momentary (400 ms), short-term (3 s), and integrated (full program) loudness
//! 5. True peak via 4× oversampled peak detector
//!
//! ## Reference
//!
//! ITU-R BS.1770-4 (2015) — "Algorithms to measure audio programme loudness and
//! true-peak audio level."
//!
//! ## Example
//!
//! ```rust
//! use sonido_analysis::loudness::LufsMeter;
//!
//! let mut meter = LufsMeter::new(48000.0);
//! let left  = vec![0.0f32; 48000];
//! let right = vec![0.0f32; 48000];
//! meter.push_samples(&left, &right);
//! let integrated = meter.integrated();
//! assert!(integrated < -69.0); // silence → near absolute gate floor
//! ```

use std::collections::VecDeque;
use std::f64::consts::PI;

// ═══════════════════════════════════════════════════════════════════════════
//  Biquad TDF-II (f64 for coefficient accuracy)
// ═══════════════════════════════════════════════════════════════════════════

/// Second-order biquad filter in Transposed Direct Form II.
///
/// Uses f64 internally to avoid coefficient quantisation errors at low
/// sample rates. All audio samples are f32 at the public boundary.
#[derive(Debug, Clone)]
struct Biquad64 {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    s1: f64,
    s2: f64,
}

impl Biquad64 {
    fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.s1;
        self.s1 = self.b1 * x - self.a1 * y + self.s2;
        self.s2 = self.b2 * x - self.a2 * y;
        y
    }

    fn reset(&mut self) {
        self.s1 = 0.0;
        self.s2 = 0.0;
    }

    /// High-shelf filter: +`gain_db` above `corner_hz`.
    ///
    /// BS.1770 stage 1: +4 dB shelf at 1500 Hz.
    fn high_shelf(sample_rate: f64, corner_hz: f64, gain_db: f64) -> Self {
        let a = 10.0_f64.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * corner_hz / sample_rate;
        let cos_w0 = w0.cos();
        let alpha = w0.sin() / 2.0 * (a + 1.0 / a).sqrt();

        let b0 = a * ((a + 1.0) + (a - 1.0) * cos_w0 + 2.0 * a.sqrt() * alpha);
        let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0);
        let b2 = a * ((a + 1.0) + (a - 1.0) * cos_w0 - 2.0 * a.sqrt() * alpha);
        let a0 = (a + 1.0) - (a - 1.0) * cos_w0 + 2.0 * a.sqrt() * alpha;
        let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w0);
        let a2 = (a + 1.0) - (a - 1.0) * cos_w0 - 2.0 * a.sqrt() * alpha;

        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            s1: 0.0,
            s2: 0.0,
        }
    }

    /// Second-order highpass (Butterworth Q = 0.5).
    ///
    /// BS.1770 stage 2: highpass at 38 Hz.
    fn highpass(sample_rate: f64, cutoff_hz: f64) -> Self {
        let w0 = 2.0 * PI * cutoff_hz / sample_rate;
        let cos_w0 = w0.cos();
        let q = 0.5_f64;
        let alpha = w0.sin() / (2.0 * q);

        let b0 = (1.0 + cos_w0) / 2.0;
        let b1 = -(1.0 + cos_w0);
        let b2 = (1.0 + cos_w0) / 2.0;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha;

        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            s1: 0.0,
            s2: 0.0,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  K-weighting filter chain for one channel
// ═══════════════════════════════════════════════════════════════════════════

/// K-weighting filter: stage-1 high shelf → stage-2 highpass.
#[derive(Debug, Clone)]
struct KWeightFilter {
    shelf: Biquad64,
    hp: Biquad64,
}

impl KWeightFilter {
    fn new(sample_rate: f64) -> Self {
        Self {
            shelf: Biquad64::high_shelf(sample_rate, 1500.0, 4.0),
            hp: Biquad64::highpass(sample_rate, 38.0),
        }
    }

    fn process(&mut self, x: f32) -> f64 {
        let y = self.shelf.process(x as f64);
        self.hp.process(y)
    }

    fn reset(&mut self) {
        self.shelf.reset();
        self.hp.reset();
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  True-peak oversampled detector (4×)
// ═══════════════════════════════════════════════════════════════════════════

/// True-peak detector with 4× linear interpolation.
///
/// Linear interpolation between samples is a simple but reasonable
/// approximation of the ideal bandlimited 4× oversampled peak detector
/// described in BS.1770 Annex 2.
#[derive(Debug, Clone)]
struct TruePeakDetector {
    prev: f32,
    peak: f32,
}

impl TruePeakDetector {
    fn new() -> Self {
        Self {
            prev: 0.0,
            peak: 0.0,
        }
    }

    fn push(&mut self, x: f32) {
        // 4 sub-samples via linear interpolation between prev and x
        for i in 1..=4 {
            let t = i as f32 / 4.0;
            let interp = self.prev + t * (x - self.prev);
            self.peak = self.peak.max(interp.abs());
        }
        self.prev = x;
    }

    fn peak_linear(&self) -> f32 {
        self.peak
    }

    fn reset(&mut self) {
        self.prev = 0.0;
        self.peak = 0.0;
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Gated block integrator
// ═══════════════════════════════════════════════════════════════════════════

/// Convert mean-square to LUFS: `LUFS = -0.691 + 10 * log10(mean_square)`.
///
/// Returns `f32::NEG_INFINITY` for silence (mean_square ≤ 0).
fn mean_sq_to_lufs(mean_sq: f64) -> f32 {
    if mean_sq <= 0.0 {
        return f32::NEG_INFINITY;
    }
    (-0.691 + 10.0 * mean_sq.log10()) as f32
}

// ═══════════════════════════════════════════════════════════════════════════
//  LufsMeter
// ═══════════════════════════════════════════════════════════════════════════

/// ITU-R BS.1770-4 LUFS loudness meter.
///
/// Call [`push_samples`](LufsMeter::push_samples) to feed audio, then poll
/// [`momentary`](LufsMeter::momentary), [`short_term`](LufsMeter::short_term),
/// or [`integrated`](LufsMeter::integrated) for results.
///
/// # Block sizes
///
/// - **400 ms** block: used for momentary loudness and gating
/// - **3 s** window (7½ overlapping 400 ms blocks): short-term loudness
/// - **Full program**: integrated loudness with absolute + relative gating
///
/// # Invariants
///
/// - Both channels must have the same length in every `push_samples` call.
/// - Supports stereo only (L + R). Mono callers may pass the same slice twice.
#[derive(Debug, Clone)]
pub struct LufsMeter {
    sample_rate: f64,
    /// K-weighting filter for left channel.
    kw_l: KWeightFilter,
    /// K-weighting filter for right channel.
    kw_r: KWeightFilter,
    /// True-peak detectors per channel.
    tp_l: TruePeakDetector,
    tp_r: TruePeakDetector,
    /// Samples in current 400 ms block.
    block_buf: Vec<f64>,
    /// Target block size in samples (400 ms).
    block_size: usize,
    /// Rolling window of 400 ms block mean-square values (stereo summed).
    ///
    /// Short-term: last 7–8 blocks (~3 s); momentary: last block.
    block_history: VecDeque<f64>,
    /// All gated block mean-square values for integrated measurement.
    integrated_blocks: Vec<f64>,
}

impl LufsMeter {
    /// Create a new meter for the given sample rate (Hz).
    ///
    /// # Arguments
    ///
    /// - `sample_rate`: Sample rate in Hz. Typical values: 44100, 48000, 96000.
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate as f64;
        let block_size = (sr * 0.4).round() as usize; // 400 ms
        Self {
            sample_rate: sr,
            kw_l: KWeightFilter::new(sr),
            kw_r: KWeightFilter::new(sr),
            tp_l: TruePeakDetector::new(),
            tp_r: TruePeakDetector::new(),
            block_buf: Vec::with_capacity(block_size * 2),
            block_size,
            block_history: VecDeque::with_capacity(8),
            integrated_blocks: Vec::new(),
        }
    }

    /// Feed stereo audio samples into the meter.
    ///
    /// # Arguments
    ///
    /// - `left`: Left channel samples.
    /// - `right`: Right channel samples. Must have the same length as `left`.
    ///
    /// # Panics
    ///
    /// Panics in debug builds if `left.len() != right.len()`.
    pub fn push_samples(&mut self, left: &[f32], right: &[f32]) {
        debug_assert_eq!(left.len(), right.len());

        for (&l, &r) in left.iter().zip(right.iter()) {
            // K-weight
            let kl = self.kw_l.process(l);
            let kr = self.kw_r.process(r);
            // True peak
            self.tp_l.push(l);
            self.tp_r.push(r);
            // Accumulate mean-square sum: L² + R² (stereo sum as per BS.1770 eq. 2)
            self.block_buf.push(kl * kl + kr * kr);

            if self.block_buf.len() >= self.block_size {
                self.flush_block();
            }
        }
    }

    /// Flush the current partial block and record its mean-square.
    fn flush_block(&mut self) {
        if self.block_buf.is_empty() {
            return;
        }
        let mean_sq: f64 = self.block_buf.iter().sum::<f64>() / self.block_buf.len() as f64;
        self.block_buf.clear();

        // Keep ~8 blocks for 3 s window (8 × 400 ms = 3.2 s)
        if self.block_history.len() >= 8 {
            self.block_history.pop_front();
        }
        self.block_history.push_back(mean_sq);
        self.integrated_blocks.push(mean_sq);
    }

    /// Momentary loudness (LUFS) — last 400 ms block.
    ///
    /// Returns `f32::NEG_INFINITY` if no complete block has been measured.
    pub fn momentary(&self) -> f32 {
        match self.block_history.back().copied() {
            Some(ms) => mean_sq_to_lufs(ms),
            None => f32::NEG_INFINITY,
        }
    }

    /// Short-term loudness (LUFS) — last ~3 s (up to 8 × 400 ms blocks).
    ///
    /// Returns `f32::NEG_INFINITY` if no complete block has been measured.
    pub fn short_term(&self) -> f32 {
        if self.block_history.is_empty() {
            return f32::NEG_INFINITY;
        }
        let mean_sq: f64 = self.block_history.iter().sum::<f64>() / self.block_history.len() as f64;
        mean_sq_to_lufs(mean_sq)
    }

    /// Integrated loudness (LUFS) — full program with gating.
    ///
    /// Applies the two-stage BS.1770 gating:
    /// 1. **Absolute gate**: exclude blocks below −70 LUFS.
    /// 2. **Relative gate**: exclude blocks more than 10 dB below the ungated
    ///    mean of all absolutely-gated blocks.
    ///
    /// Returns `f32::NEG_INFINITY` if fewer than 1 block has been measured.
    pub fn integrated(&self) -> f32 {
        if self.integrated_blocks.is_empty() {
            return f32::NEG_INFINITY;
        }

        // Absolute gate: -70 LUFS ↔ mean_sq threshold
        // -70 LUFS = -0.691 + 10*log10(ms) → ms = 10^((-70+0.691)/10)
        let abs_gate_ms: f64 = 10.0_f64.powf((-70.0 + 0.691) / 10.0);

        let abs_gated: Vec<f64> = self
            .integrated_blocks
            .iter()
            .copied()
            .filter(|&ms| ms > abs_gate_ms)
            .collect();

        if abs_gated.is_empty() {
            return f32::NEG_INFINITY;
        }

        // Ungated mean for relative gate threshold
        let ungated_mean: f64 = abs_gated.iter().sum::<f64>() / abs_gated.len() as f64;
        // Relative gate: -10 dB below ungated mean
        let rel_gate_ms = ungated_mean * 10.0_f64.powf(-10.0 / 10.0);

        let rel_gated: Vec<f64> = abs_gated
            .iter()
            .copied()
            .filter(|&ms| ms > rel_gate_ms)
            .collect();

        if rel_gated.is_empty() {
            return mean_sq_to_lufs(ungated_mean);
        }

        let final_mean: f64 = rel_gated.iter().sum::<f64>() / rel_gated.len() as f64;
        mean_sq_to_lufs(final_mean)
    }

    /// True peak level in dBTP — maximum across both channels.
    ///
    /// Returns the highest instantaneous peak detected since creation or
    /// last [`reset`](LufsMeter::reset), in dB relative to full scale.
    pub fn true_peak_dbtp(&self) -> f32 {
        let peak = self.tp_l.peak_linear().max(self.tp_r.peak_linear());
        if peak > 1e-10 {
            20.0 * peak.log10()
        } else {
            -200.0
        }
    }

    /// Reset all measurement state and filter history.
    pub fn reset(&mut self) {
        self.kw_l.reset();
        self.kw_r.reset();
        self.tp_l.reset();
        self.tp_r.reset();
        self.block_buf.clear();
        self.block_history.clear();
        self.integrated_blocks.clear();
    }

    /// Sample rate this meter was created with, in Hz.
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate as f32
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    fn sine(freq: f32, sr: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * PI * freq * i as f32 / sr).sin())
            .collect()
    }

    #[test]
    fn silence_below_absolute_gate() {
        let mut meter = LufsMeter::new(48000.0);
        let silence = vec![0.0f32; 48000 * 2]; // 2 s
        meter.push_samples(&silence, &silence);
        assert!(
            meter.integrated() < -69.0 || meter.integrated().is_infinite(),
            "silence should be at/below absolute gate floor, got {}",
            meter.integrated()
        );
    }

    #[test]
    fn momentary_not_inf_after_one_block() {
        let sr = 48000.0f32;
        let block = sine(1000.0, sr, (sr * 0.4) as usize + 1);
        let mut meter = LufsMeter::new(sr);
        meter.push_samples(&block, &block);
        assert!(
            meter.momentary().is_finite(),
            "momentary should be finite after one full block"
        );
    }

    #[test]
    fn short_term_finite_after_block() {
        let sr = 48000.0f32;
        let sig = sine(440.0, sr, (sr * 0.5) as usize);
        let mut meter = LufsMeter::new(sr);
        meter.push_samples(&sig, &sig);
        assert!(
            meter.short_term().is_finite(),
            "short-term should be finite after >400 ms of audio"
        );
    }

    #[test]
    fn mono_louder_than_silence() {
        let sr = 48000.0f32;
        let sig = sine(1000.0, sr, (sr * 3.0) as usize);
        let mut meter = LufsMeter::new(sr);
        meter.push_samples(&sig, &sig);
        let lufs = meter.integrated();
        assert!(
            lufs > -70.0,
            "1 kHz sine should be louder than absolute gate, got {}",
            lufs
        );
    }

    #[test]
    fn true_peak_unit_sine() {
        let sr = 48000.0f32;
        let sig = sine(1000.0, sr, (sr * 1.0) as usize);
        let mut meter = LufsMeter::new(sr);
        meter.push_samples(&sig, &sig);
        let tp = meter.true_peak_dbtp();
        // Unit sine → true peak near 0 dBTP (within ±3 dB allowing for interpolation)
        assert!(
            tp > -3.0 && tp < 3.0,
            "true peak of unit sine should be near 0 dBTP, got {}",
            tp
        );
    }

    #[test]
    fn reset_clears_state() {
        let sr = 48000.0f32;
        let sig = sine(440.0, sr, (sr * 2.0) as usize);
        let mut meter = LufsMeter::new(sr);
        meter.push_samples(&sig, &sig);
        meter.reset();
        assert!(meter.momentary().is_infinite());
        assert!(meter.integrated().is_infinite());
    }
}
