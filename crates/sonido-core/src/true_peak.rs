//! True-peak detection and limiting.
//!
//! A sample-peak limiter lets through the peaks that live *between* samples:
//! a full-scale signal can swing past 0 dBFS once it is reconstructed, so a
//! DAC or a lossy encoder clips what the limiter never saw. ITU-R BS.1770-4
//! Annex 2 measures true peak by oversampling 4× and taking the largest
//! interpolated value; [`TruePeakDetector`] does that, streaming, with a
//! 48-tap Kaiser-windowed sinc interpolator, and [`TruePeakLimiter`] holds a
//! stereo signal under a true-peak ceiling.
//!
//! The limiter's gain is a sliding-window minimum of the gain each detected
//! peak requires, smoothed by a box filter of the same length, with the audio
//! delayed to match. The box-filtered hold is never above the requirement of
//! any peak in its window, so the gain reaches its target by the time the
//! peak arrives, with a linear attack and no overshoot (the construction is
//! the one described in Geraint Luff's "Limiters" notes, Signalsmith Audio).

use libm::{expf, log10f, powf};

/// Interpolation factor.
const FACTOR: usize = 4;
/// Taps per polyphase branch.
const PHASE_TAPS: usize = 12;
/// Total FIR length.
const TAPS: usize = FACTOR * PHASE_TAPS;
/// Kaiser window shape (about 80 dB of sidelobe rejection).
const KAISER_BETA: f64 = 8.0;

/// Zeroth-order modified Bessel function of the first kind, by its series.
fn bessel_i0(x: f64) -> f64 {
    let half = x * 0.5;
    let mut term = 1.0f64;
    let mut sum = 1.0f64;
    let mut k = 1.0f64;
    while term > 1e-12 * sum {
        term *= (half / k) * (half / k);
        sum += term;
        k += 1.0;
    }
    sum
}

/// The polyphase branches of the 4× interpolator, each normalized to unity
/// DC gain.
fn interpolator_phases() -> [[f32; PHASE_TAPS]; FACTOR] {
    let center = (TAPS - 1) as f64 / 2.0;
    let denom = bessel_i0(KAISER_BETA);
    let mut h = [0.0f64; TAPS];
    for (n, tap) in h.iter_mut().enumerate() {
        let t = (n as f64 - center) / FACTOR as f64;
        let sinc = if t.abs() < 1e-12 {
            1.0
        } else {
            let a = core::f64::consts::PI * t;
            libm::sin(a) / a
        };
        let r = (n as f64 - center) / center;
        let w = bessel_i0(KAISER_BETA * libm::sqrt((1.0 - r * r).max(0.0))) / denom;
        *tap = sinc * w;
    }
    let mut phases = [[0.0f32; PHASE_TAPS]; FACTOR];
    for (k, phase) in phases.iter_mut().enumerate() {
        let sum: f64 = (0..PHASE_TAPS).map(|t| h[FACTOR * t + k]).sum();
        for (t, coeff) in phase.iter_mut().enumerate() {
            *coeff = (h[FACTOR * t + k] / sum) as f32;
        }
    }
    phases
}

/// Streaming 4× true-peak detector for one channel.
///
/// Each call to [`process`](Self::process) takes one input sample and returns
/// the largest magnitude among the four interpolated points it completes. The
/// interpolator's group delay is [`TruePeakDetector::LATENCY`] input samples.
#[derive(Debug, Clone)]
pub struct TruePeakDetector {
    phases: [[f32; PHASE_TAPS]; FACTOR],
    history: [f32; PHASE_TAPS],
    pos: usize,
}

impl Default for TruePeakDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl TruePeakDetector {
    /// Group delay of the interpolator, rounded up, in input samples.
    pub const LATENCY: usize = 6;

    /// A detector with clear history.
    pub fn new() -> Self {
        Self {
            phases: interpolator_phases(),
            history: [0.0; PHASE_TAPS],
            pos: 0,
        }
    }

    /// Push one sample; return the true-peak magnitude it completes.
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        self.pos = (self.pos + PHASE_TAPS - 1) % PHASE_TAPS;
        self.history[self.pos] = x;
        let mut peak = 0.0f32;
        for phase in &self.phases {
            let mut acc = 0.0f32;
            for (t, &c) in phase.iter().enumerate() {
                acc += c * self.history[(self.pos + t) % PHASE_TAPS];
            }
            peak = peak.max(acc.abs());
        }
        peak
    }

    /// Clear the history.
    pub fn reset(&mut self) {
        self.history = [0.0; PHASE_TAPS];
        self.pos = 0;
    }

    /// The largest true peak of a whole buffer, from a cleared detector,
    /// including the interpolator's tail.
    pub fn measure(samples: &[f32]) -> f32 {
        let mut det = Self::new();
        let mut peak = 0.0f32;
        for &s in samples {
            peak = peak.max(det.process(s));
        }
        for _ in 0..PHASE_TAPS {
            peak = peak.max(det.process(0.0));
        }
        peak
    }
}

/// Extra audio delay that lines the signal up with the detector.
const ALIGN: usize = TruePeakDetector::LATENCY - 1;
/// Capacity of the alignment delay (a power of two above `ALIGN`).
const ALIGN_CAP: usize = 8;

/// Stereo lookahead limiter with a true-peak ceiling.
///
/// `W` is the lookahead in samples (256 is about 5.3 ms at 48 kHz). The
/// channels share one gain, so the stereo image doesn't wander. Latency is
/// [`latency`](Self::latency) samples.
#[derive(Debug, Clone)]
pub struct TruePeakLimiter<const W: usize> {
    det_l: TruePeakDetector,
    det_r: TruePeakDetector,
    ceiling: f32,
    release_coeff: f32,
    sample_rate: f32,
    // Sliding-window minimum of the required gain (monotonic deque).
    min_vals: [f32; W],
    min_idx: [u64; W],
    min_head: usize,
    min_len: usize,
    // Box filter over the hold signal.
    box_vals: [f32; W],
    box_sum: f64,
    box_pos: usize,
    // Audio delay: W samples, then ALIGN more.
    delay_l: [f32; W],
    delay_r: [f32; W],
    delay_pos: usize,
    align_l: [f32; ALIGN_CAP],
    align_r: [f32; ALIGN_CAP],
    align_pos: usize,
    gain: f32,
    n: u64,
}

impl<const W: usize> TruePeakLimiter<W> {
    /// A limiter at `sample_rate` with a ceiling in dBTP and a release time.
    pub fn new(sample_rate: f32, ceiling_db: f32, release_ms: f32) -> Self {
        assert!(W >= 2, "lookahead must be at least 2 samples");
        let mut lim = Self {
            det_l: TruePeakDetector::new(),
            det_r: TruePeakDetector::new(),
            ceiling: 1.0,
            release_coeff: 0.0,
            sample_rate,
            min_vals: [1.0; W],
            min_idx: [0; W],
            min_head: 0,
            min_len: 0,
            box_vals: [1.0; W],
            box_sum: W as f64,
            box_pos: 0,
            delay_l: [0.0; W],
            delay_r: [0.0; W],
            delay_pos: 0,
            align_l: [0.0; ALIGN_CAP],
            align_r: [0.0; ALIGN_CAP],
            align_pos: 0,
            gain: 1.0,
            n: 0,
        };
        lim.set_ceiling_db(ceiling_db);
        lim.set_release_ms(release_ms);
        lim
    }

    /// Total delay through the limiter, in samples.
    pub const fn latency() -> usize {
        W + ALIGN
    }

    /// Set the true-peak ceiling in dBTP (for example −1.0).
    pub fn set_ceiling_db(&mut self, ceiling_db: f32) {
        self.ceiling = powf(10.0, ceiling_db.min(0.0) / 20.0);
    }

    /// Set the release time constant in milliseconds.
    pub fn set_release_ms(&mut self, release_ms: f32) {
        let samples = (release_ms.max(1.0) * 0.001 * self.sample_rate).max(1.0);
        self.release_coeff = 1.0 - expf(-1.0 / samples);
    }

    /// Current gain reduction in dB (0 when idle, negative when limiting).
    pub fn gain_reduction_db(&self) -> f32 {
        20.0 * log10f(self.gain.max(1e-6))
    }

    /// Process one stereo frame.
    #[inline]
    pub fn process(&mut self, left: f32, right: f32) -> (f32, f32) {
        // Detect.
        let peak = self.det_l.process(left).max(self.det_r.process(right));
        let required = if peak > self.ceiling {
            self.ceiling / peak
        } else {
            1.0
        };

        // Sliding-window minimum over the last W required gains. Drop the
        // expired head first, so the push below always has room.
        let n = self.n;
        if self.min_len > 0 && self.min_idx[self.min_head] + (W as u64) <= n {
            self.min_head = (self.min_head + 1) % W;
            self.min_len -= 1;
        }
        while self.min_len > 0 {
            let back = (self.min_head + self.min_len - 1) % W;
            if self.min_vals[back] >= required {
                self.min_len -= 1;
            } else {
                break;
            }
        }
        let slot = (self.min_head + self.min_len) % W;
        self.min_vals[slot] = required;
        self.min_idx[slot] = n;
        self.min_len += 1;
        let hold = self.min_vals[self.min_head];

        // Box filter of the hold, over the same window.
        self.box_sum += f64::from(hold) - f64::from(self.box_vals[self.box_pos]);
        self.box_vals[self.box_pos] = hold;
        self.box_pos = (self.box_pos + 1) % W;
        let smooth = (self.box_sum / W as f64) as f32;

        // Release toward unity, never above the smoothed requirement.
        let released = self.gain + (1.0 - self.gain) * self.release_coeff;
        self.gain = released.min(smooth);
        self.n = n + 1;

        // Delay the audio by W + ALIGN samples and apply the gain.
        let dl = self.delay_l[self.delay_pos];
        let dr = self.delay_r[self.delay_pos];
        self.delay_l[self.delay_pos] = left;
        self.delay_r[self.delay_pos] = right;
        self.delay_pos = (self.delay_pos + 1) % W;
        let read = (self.align_pos + ALIGN_CAP - ALIGN) % ALIGN_CAP;
        self.align_l[self.align_pos] = dl;
        self.align_r[self.align_pos] = dr;
        let out_l = self.align_l[read];
        let out_r = self.align_r[read];
        self.align_pos = (self.align_pos + 1) % ALIGN_CAP;
        (out_l * self.gain, out_r * self.gain)
    }

    /// Clear all state.
    pub fn reset(&mut self) {
        self.det_l.reset();
        self.det_r.reset();
        self.min_len = 0;
        self.min_head = 0;
        self.box_vals = [1.0; W];
        self.box_sum = W as f64;
        self.box_pos = 0;
        self.delay_l = [0.0; W];
        self.delay_r = [0.0; W];
        self.delay_pos = 0;
        self.align_l = [0.0; ALIGN_CAP];
        self.align_r = [0.0; ALIGN_CAP];
        self.align_pos = 0;
        self.gain = 1.0;
        self.n = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::f32::consts::PI;
    use libm::sinf;

    fn db(x: f32) -> f32 {
        20.0 * log10f(x)
    }

    #[test]
    fn phases_have_unity_dc_gain() {
        for phase in interpolator_phases() {
            let sum: f32 = phase.iter().sum();
            assert!((sum - 1.0).abs() < 1e-5, "{sum}");
        }
    }

    #[test]
    fn detector_finds_intersample_peaks() {
        // A sine at fs/4 sampled at 45° phase never hits its crest on a
        // sample: every sample is at ±0.707 of the true amplitude.
        let fs = 48_000.0;
        let f = fs / 4.0;
        let x: Vec<f32> = (0..4800)
            .map(|n| sinf(2.0 * PI * f * n as f32 / fs + PI / 4.0))
            .collect();
        let sample_peak = x.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!((sample_peak - core::f32::consts::FRAC_1_SQRT_2).abs() < 1e-3);
        let tp = TruePeakDetector::measure(&x);
        assert!(db(tp) > -0.5, "true peak {} dB should be near 0", db(tp));
    }

    #[test]
    fn detector_matches_sample_peak_for_slow_signals() {
        let fs = 48_000.0;
        let x: Vec<f32> = (0..9600)
            .map(|n| 0.5 * sinf(2.0 * PI * 100.0 * n as f32 / fs))
            .collect();
        let tp = TruePeakDetector::measure(&x);
        assert!((tp - 0.5).abs() < 0.005, "{tp}");
    }

    fn run_limiter(input: &[(f32, f32)], ceiling_db: f32) -> Vec<(f32, f32)> {
        let mut lim: TruePeakLimiter<256> = TruePeakLimiter::new(48_000.0, ceiling_db, 80.0);
        let mut out: Vec<(f32, f32)> = input.iter().map(|&(l, r)| lim.process(l, r)).collect();
        for _ in 0..TruePeakLimiter::<256>::latency() + 32 {
            out.push(lim.process(0.0, 0.0));
        }
        out
    }

    #[test]
    fn limiter_holds_the_true_peak_ceiling() {
        let fs = 48_000.0;
        // Hard cases: intersample-peaking sines at +6 dB, a square wave and
        // isolated full-scale clicks.
        let mut cases: Vec<Vec<(f32, f32)>> = Vec::new();
        cases.push(
            (0..9600)
                .map(|n| {
                    let s = 2.0 * sinf(2.0 * PI * (fs / 4.0) * n as f32 / fs + PI / 4.0);
                    (s, -s)
                })
                .collect(),
        );
        cases.push(
            (0..9600)
                .map(|n| {
                    let s = if (n / 40) % 2 == 0 { 1.5 } else { -1.5 };
                    (s, s * 0.5)
                })
                .collect(),
        );
        cases.push(
            (0..9600)
                .map(|n| {
                    if n % 1500 == 700 {
                        (3.0, 0.0)
                    } else {
                        (0.0, 0.0)
                    }
                })
                .collect(),
        );
        let ceiling_db = -1.0;
        for (i, case) in cases.iter().enumerate() {
            let out = run_limiter(case, ceiling_db);
            let l: Vec<f32> = out.iter().map(|&(l, _)| l).collect();
            let r: Vec<f32> = out.iter().map(|&(_, r)| r).collect();
            let tp = TruePeakDetector::measure(&l).max(TruePeakDetector::measure(&r));
            assert!(
                db(tp) <= ceiling_db + 0.1,
                "case {i}: output true peak {} dB over the {ceiling_db} dB ceiling",
                db(tp)
            );
        }
    }

    #[test]
    fn limiter_is_transparent_below_the_ceiling() {
        let fs = 48_000.0;
        let input: Vec<(f32, f32)> = (0..4800)
            .map(|n| {
                let s = 0.25 * sinf(2.0 * PI * 440.0 * n as f32 / fs);
                (s, s)
            })
            .collect();
        let out = run_limiter(&input, -1.0);
        let lat = TruePeakLimiter::<256>::latency();
        for (n, &(l, _)) in out.iter().enumerate().skip(lat).take(4000) {
            let expected = input[n - lat].0;
            assert!((l - expected).abs() < 1e-6, "sample {n}: {l} vs {expected}");
        }
    }

    #[test]
    fn limiter_releases_after_a_peak() {
        let mut lim: TruePeakLimiter<128> = TruePeakLimiter::new(48_000.0, -1.0, 50.0);
        for n in 0..2000 {
            let s = if n == 100 { 4.0 } else { 0.0 };
            lim.process(s, s);
        }
        assert!(lim.gain_reduction_db() < -0.5);
        for _ in 0..48_000 {
            lim.process(0.1, 0.1);
        }
        assert!(
            lim.gain_reduction_db() > -0.01,
            "{}",
            lim.gain_reduction_db()
        );
    }
}
