//! Digital up-conversion: a complex baseband signal onto an audio carrier.
//!
//! The mirror of a digital down-converter. A down-converter shifts a band of
//! a real signal to complex baseband, `b(t)`, its amplitude and phase moving
//! slowly; [`Upconverter`] takes that baseband, sampled at a low rate, and
//! plays it onto a carrier at audio rate:
//!
//! ```text
//! y(t) = Re{ b(t) · e^{j·2π·f_c·t} } = I(t)·cos(2π f_c t) − Q(t)·sin(2π f_c t)
//! ```
//!
//! The result is a tone exactly at `f_c` whose swell and shimmer are the
//! band's own motion, in real time: no time compression, unlike audification.
//! It is how a 1–2 Hz band of a seismometer can be heard as a chord tone.
//!
//! The baseband is interpolated with a Catmull-Rom cubic between its samples.
//! The carrier is a rotating phasor, renormalized periodically, so a sample
//! costs a handful of multiplies and no transcendental functions; carrier
//! changes glide with continuous phase.

use libm::{cosf, expf, sinf};

/// Samples between phasor renormalizations and glide updates.
const UPDATE_EVERY: u32 = 32;

/// Streaming up-converter with room for `CAP` baseband samples.
///
/// Feed it baseband samples with [`push`](Self::push) at the baseband rate,
/// on average, and pull one audio sample per [`process`](Self::process). If
/// it runs dry it fades to silence and waits; if it is overfed, `push`
/// refuses.
#[derive(Debug, Clone)]
pub struct Upconverter<const CAP: usize> {
    buf_i: [f32; CAP],
    buf_q: [f32; CAP],
    written: u64,
    read: f64,
    step: f64,
    phasor_c: f32,
    phasor_s: f32,
    rot_c: f32,
    rot_s: f32,
    freq: f32,
    target: f32,
    glide_coeff: f32,
    sample_rate: f32,
    fade: f32,
    fade_step: f32,
    counter: u32,
}

impl<const CAP: usize> Upconverter<CAP> {
    /// An up-converter producing audio at `sample_rate` from baseband
    /// samples arriving at `baseband_rate`.
    pub fn new(sample_rate: f32, baseband_rate: f32) -> Self {
        assert!(CAP >= 8, "the buffer needs room for the interpolator");
        let mut up = Self {
            buf_i: [0.0; CAP],
            buf_q: [0.0; CAP],
            written: 0,
            read: 1.0,
            step: f64::from(baseband_rate) / f64::from(sample_rate),
            phasor_c: 1.0,
            phasor_s: 0.0,
            rot_c: 1.0,
            rot_s: 0.0,
            freq: 0.0,
            target: 0.0,
            glide_coeff: 1.0,
            sample_rate,
            fade: 0.0,
            // About 50 ms to fade in or out.
            fade_step: 1.0 / (0.05 * sample_rate),
            counter: 0,
        };
        up.set_carrier(440.0, 0.0);
        up
    }

    /// Append one baseband sample. Returns `false` (and drops it) when full.
    pub fn push(&mut self, i: f32, q: f32) -> bool {
        let oldest_needed = (self.read as u64).saturating_sub(1);
        if self.written - oldest_needed >= CAP as u64 {
            return false;
        }
        let slot = (self.written % CAP as u64) as usize;
        self.buf_i[slot] = i;
        self.buf_q[slot] = q;
        self.written += 1;
        true
    }

    /// Baseband samples buffered ahead of the read position.
    pub fn buffered(&self) -> usize {
        (self.written as f64 - self.read).max(0.0) as usize
    }

    /// Move the carrier to `hz`, gliding exponentially with a time constant
    /// of `glide_ms` (0 jumps, keeping the phase continuous).
    pub fn set_carrier(&mut self, hz: f32, glide_ms: f32) {
        let nyquist = self.sample_rate * 0.5;
        self.target = hz.clamp(0.0, nyquist * 0.98);
        if glide_ms <= 0.0 {
            self.freq = self.target;
            self.glide_coeff = 1.0;
            self.update_rotation();
        } else {
            let updates = glide_ms * 0.001 * self.sample_rate / UPDATE_EVERY as f32;
            self.glide_coeff = 1.0 - expf(-1.0 / updates.max(1.0));
        }
    }

    /// The carrier frequency right now, mid-glide or not.
    pub fn carrier(&self) -> f32 {
        self.freq
    }

    fn update_rotation(&mut self) {
        let w = 2.0 * core::f32::consts::PI * self.freq / self.sample_rate;
        self.rot_c = cosf(w);
        self.rot_s = sinf(w);
    }

    fn sample(&self, k: u64) -> (f32, f32) {
        let slot = (k % CAP as u64) as usize;
        (self.buf_i[slot], self.buf_q[slot])
    }

    /// Produce one audio sample.
    #[inline]
    pub fn process(&mut self) -> f32 {
        // Glide and renormalize every UPDATE_EVERY samples.
        self.counter += 1;
        if self.counter >= UPDATE_EVERY {
            self.counter = 0;
            if (self.target - self.freq).abs() > 1e-4 {
                self.freq += (self.target - self.freq) * self.glide_coeff;
                self.update_rotation();
            }
            let mag2 = self.phasor_c * self.phasor_c + self.phasor_s * self.phasor_s;
            let norm = 1.5 - 0.5 * mag2;
            self.phasor_c *= norm;
            self.phasor_s *= norm;
        }

        // Interpolate the baseband if the samples around the read position
        // are here; otherwise fade out and wait.
        let k = self.read as u64;
        let have = k + 2 < self.written;
        let (bi, bq) = if have && k >= 1 {
            let t = (self.read - k as f64) as f32;
            let (i0, q0) = self.sample(k - 1);
            let (i1, q1) = self.sample(k);
            let (i2, q2) = self.sample(k + 1);
            let (i3, q3) = self.sample(k + 2);
            self.read += self.step;
            self.fade = (self.fade + self.fade_step).min(1.0);
            (
                catmull_rom(i0, i1, i2, i3, t),
                catmull_rom(q0, q1, q2, q3, t),
            )
        } else {
            self.fade = (self.fade - self.fade_step).max(0.0);
            let last = self.written.saturating_sub(1);
            self.sample(last)
        };

        let out = (bi * self.phasor_c - bq * self.phasor_s) * self.fade;
        let c = self.phasor_c * self.rot_c - self.phasor_s * self.rot_s;
        let s = self.phasor_c * self.rot_s + self.phasor_s * self.rot_c;
        self.phasor_c = c;
        self.phasor_s = s;
        out
    }

    /// Clear the buffer and phase; keep the rates and the carrier.
    pub fn reset(&mut self) {
        self.buf_i = [0.0; CAP];
        self.buf_q = [0.0; CAP];
        self.written = 0;
        self.read = 1.0;
        self.phasor_c = 1.0;
        self.phasor_s = 0.0;
        self.fade = 0.0;
        self.counter = 0;
        self.freq = self.target;
        self.update_rotation();
    }
}

/// Catmull-Rom cubic through four evenly spaced points, at `t` in `[0, 1)`
/// between the middle two.
#[inline]
fn catmull_rom(p0: f32, p1: f32, p2: f32, p3: f32, t: f32) -> f32 {
    let t2 = t * t;
    let t3 = t2 * t;
    0.5 * (2.0 * p1
        + (p2 - p0) * t
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2
        + (3.0 * p1 - p0 - 3.0 * p2 + p3) * t3)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Magnitude of the DFT of `x` at `freq`.
    fn tone_level(x: &[f32], freq: f32, fs: f32) -> f32 {
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (n, &v) in x.iter().enumerate() {
            let a = 2.0 * core::f64::consts::PI * f64::from(freq) * n as f64 / f64::from(fs);
            re += f64::from(v) * libm::cos(a);
            im -= f64::from(v) * libm::sin(a);
        }
        (2.0 * libm::sqrt(re * re + im * im) / x.len() as f64) as f32
    }

    fn feed<const C: usize>(
        up: &mut Upconverter<C>,
        samples: usize,
        f: impl Fn(usize) -> (f32, f32),
    ) {
        for n in 0..samples {
            let (i, q) = f(n);
            assert!(up.push(i, q));
        }
    }

    #[test]
    fn constant_baseband_is_a_tone_at_the_carrier() {
        let fs = 48_000.0;
        let mut up: Upconverter<64> = Upconverter::new(fs, 10.0);
        up.set_carrier(440.0, 0.0);
        feed(&mut up, 40, |_| (0.8, 0.0));
        let out: Vec<f32> = (0..96_000).map(|_| up.process()).collect();
        let steady = &out[9600..86_400];
        let at = tone_level(steady, 440.0, fs);
        // 450 Hz completes a whole number of cycles in the window, so it
        // measures leakage rather than the analysis window's sidelobes.
        let off = tone_level(steady, 450.0, fs);
        assert!((at - 0.8).abs() < 0.02, "level {at}");
        assert!(off < 0.01, "leak {off}");
    }

    #[test]
    fn a_rotating_baseband_shifts_the_tone() {
        // A baseband turning at +2 Hz moves the tone to 442 Hz.
        let fs = 48_000.0;
        let bb_rate = 16.0;
        let mut up: Upconverter<128> = Upconverter::new(fs, bb_rate);
        up.set_carrier(440.0, 0.0);
        feed(&mut up, 100, |n| {
            let a = 2.0 * core::f32::consts::PI * 2.0 * n as f32 / bb_rate;
            (cosf(a), sinf(a))
        });
        let out: Vec<f32> = (0..240_000).map(|_| up.process()).collect();
        let steady = &out[24_000..216_000];
        let at = tone_level(steady, 442.0, fs);
        let orig = tone_level(steady, 440.0, fs);
        assert!(at > 0.9, "442 Hz level {at}");
        assert!(orig < 0.05, "440 Hz level {orig}");
    }

    #[test]
    fn glides_without_jumps() {
        let fs = 48_000.0;
        let mut up: Upconverter<64> = Upconverter::new(fs, 10.0);
        up.set_carrier(220.0, 0.0);
        feed(&mut up, 60, |_| (1.0, 0.0));
        let mut prev = up.process();
        for n in 0..240_000 {
            if n == 48_000 {
                up.set_carrier(330.0, 200.0);
            }
            let s = up.process();
            // The steepest a 1.0-amplitude 330 Hz sine moves per sample.
            assert!((s - prev).abs() < 0.05, "jump at {n}: {prev} -> {s}");
            prev = s;
        }
        assert!((up.carrier() - 330.0).abs() < 0.5, "{}", up.carrier());
    }

    #[test]
    fn starvation_fades_and_recovers() {
        let fs = 48_000.0;
        let mut up: Upconverter<64> = Upconverter::new(fs, 10.0);
        feed(&mut up, 5, |_| (1.0, 0.0));
        let out: Vec<f32> = (0..48_000).map(|_| up.process()).collect();
        let tail_peak = out[40_000..].iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(
            tail_peak < 1e-3,
            "should be silent when dry, got {tail_peak}"
        );
        feed(&mut up, 20, |_| (1.0, 0.0));
        let out: Vec<f32> = (0..24_000).map(|_| up.process()).collect();
        let peak = out[12_000..].iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(peak > 0.9, "should recover, got {peak}");
    }

    #[test]
    fn refuses_when_full() {
        let mut up: Upconverter<16> = Upconverter::new(48_000.0, 10.0);
        let accepted = (0..40).filter(|_| up.push(0.5, 0.0)).count();
        assert!((14..=16).contains(&accepted), "{accepted}");
    }
}
