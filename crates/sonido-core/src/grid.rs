//! A software ear for the power grid: the mains hum's frequency, its rate of
//! change and its level, from any audio-rate signal that carries the hum.
//!
//! Every wire in a building radiates the grid's 60 Hz (50 Hz elsewhere) and
//! its harmonics; a guitar pickup, a bare cable on a high-impedance input or a
//! pedal's audio input all hear it. The frequency of that hum is set by every
//! generator on the interconnection together, so a millihertz reading of it
//! is a reading of the whole grid's balance: it sags when demand outruns
//! supply, and its rate of fall says how fast a plant just dropped.
//!
//! The ear runs three narrow bandpasses, one per harmonic (1, 2 and 3), and
//! tracks whichever carries the most energy, because a pickup often hears the
//! third harmonic louder than the fundamental. On the tracked band it measures
//! the period between rising zero crossings with linear interpolation, which
//! on a clean hum resolves well below a millihertz over a second. Crossing
//! times are kept as a sample count plus a fraction, never as one float, so
//! precision does not decay as the count grows.
//!
//! `no_std`: fits the pedal and the desktop alike. The caller feeds samples
//! through [`GridEar::process`] and asks for a [`GridReading`] with
//! [`GridEar::tick`] every 50 ms (every `sample_rate / 20` samples).

use crate::biquad::{Biquad, bandpass_coefficients};
use crate::dc_blocker::DcBlocker;
use libm::{expf, log10f};

/// The harmonics the ear listens on, as multiples of the nominal frequency.
const HARMONICS: [u8; 3] = [1, 2, 3];

/// Cycle readings kept per band: more than one second of the third harmonic
/// at 60 Hz (180 cycles) with room to spare.
const CYCLE_RING: usize = 256;

/// Ticks of `hz_fast` kept for the rate-of-change reading: 300 ms back at 50 ms a tick.
const TICK_RING: usize = 8;

/// The rate of change is read across this many ticks (300 ms): short enough
/// to catch a plant tripping inside half a second, long enough that the
/// cycle-to-cycle jitter of a noisy hum does not read as a fall.
const ROCOF_TICKS: usize = 6;

/// Bandpass Q per harmonic band: a 10 Hz passband at 60 Hz, which keeps the
/// zero crossings clean on a hum with noise and settles in about 100 ms.
const BAND_Q: f32 = 6.0;

/// The hum is declared lost when the tracked band falls this far under the
/// level it held (30 dB, as a power ratio), and found again within 10 dB of it.
const LOST_RATIO: f32 = 1e-3;
const FOUND_RATIO: f32 = 1e-1;

/// Below this mean-square level (-60 dBFS) a band holds no hum, only noise,
/// and its crossings are not the grid's: the ear reports nothing from it.
const FLOOR_ENV: f32 = 1e-6;

/// What the ear heard, asked for once per tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridReading {
    /// The mean frequency of the cycles in the last 100 ms, in Hz, already
    /// divided by the tracked harmonic. The reading a fast controller acts on.
    pub hz_fast: f32,
    /// The mean frequency of every cycle in the last second, in Hz.
    pub hz: f32,
    /// The change of `hz_fast` over the last 300 ms, in Hz per second.
    pub rocof: f32,
    /// The tracked band's level in dBFS.
    pub level_db: f32,
    /// The harmonic being tracked: 1, 2 or 3.
    pub harmonic: u8,
}

struct Band {
    harmonic: u8,
    filter: Biquad,
    /// Mean square of the band's output, a one-pole average over about 200 ms.
    env: f32,
    prev: f32,
    has_cross: bool,
    last_n: u32,
    last_frac: f32,
    freqs: [f32; CYCLE_RING],
    times: [u32; CYCLE_RING],
    head: usize,
    count: usize,
    /// Accepted period range in samples, from the nominal frequency ±15%.
    period_min: f32,
    period_max: f32,
}

impl Band {
    fn new(harmonic: u8, nominal_hz: f32, sample_rate: f32) -> Self {
        let center = nominal_hz * harmonic as f32;
        let (b0, b1, b2, a0, a1, a2) = bandpass_coefficients(center, BAND_Q, sample_rate);
        let mut filter = Biquad::new();
        filter.set_coefficients(b0, b1, b2, a0, a1, a2);
        Self {
            harmonic,
            filter,
            env: 0.0,
            prev: 0.0,
            has_cross: false,
            last_n: 0,
            last_frac: 0.0,
            freqs: [0.0; CYCLE_RING],
            times: [0; CYCLE_RING],
            head: 0,
            count: 0,
            period_min: sample_rate / (center * 1.15),
            period_max: sample_rate / (center * 0.85),
        }
    }

    /// Feeds one sample at index `n`; returns the band's filtered output.
    fn process(&mut self, x: f32, n: u32, env_k: f32, sample_rate: f32) -> f32 {
        let y = self.filter.process(x);
        self.env += env_k * (y * y - self.env);
        if self.prev <= 0.0 && y > 0.0 {
            // The crossing sits between sample n-1 (prev) and sample n (y).
            let frac = self.prev / (self.prev - y);
            let cross_n = n.wrapping_sub(1);
            if self.has_cross {
                let dn = cross_n.wrapping_sub(self.last_n) as f32;
                let period = dn + (frac - self.last_frac);
                if period >= self.period_min && period <= self.period_max {
                    let f = sample_rate / period / self.harmonic as f32;
                    self.freqs[self.head] = f;
                    self.times[self.head] = cross_n;
                    self.head = (self.head + 1) % CYCLE_RING;
                    if self.count < CYCLE_RING {
                        self.count += 1;
                    }
                }
            }
            self.has_cross = true;
            self.last_n = cross_n;
            self.last_frac = frac;
        }
        self.prev = y;
        y
    }

    /// Mean of the newest `k` cycle readings, or `None` with fewer than `k`.
    fn mean_last(&self, k: usize) -> Option<f32> {
        if self.count < k {
            return None;
        }
        let mut sum = 0.0;
        for i in 0..k {
            let idx = (self.head + CYCLE_RING - 1 - i) % CYCLE_RING;
            sum += self.freqs[idx];
        }
        Some(sum / k as f32)
    }

    /// Mean of every cycle reading newer than `span` samples before `now`.
    fn mean_since(&self, now: u32, span: u32) -> Option<f32> {
        let mut sum = 0.0;
        let mut k = 0usize;
        for i in 0..self.count {
            let idx = (self.head + CYCLE_RING - 1 - i) % CYCLE_RING;
            if now.wrapping_sub(self.times[idx]) > span {
                break;
            }
            sum += self.freqs[idx];
            k += 1;
        }
        if k == 0 { None } else { Some(sum / k as f32) }
    }

    /// Age in samples of the oldest cycle reading inside the last `span`
    /// samples: how long the band has been locked, up to `span`.
    fn locked_for(&self, now: u32, span: u32) -> u32 {
        let mut age = 0u32;
        for i in 0..self.count {
            let idx = (self.head + CYCLE_RING - 1 - i) % CYCLE_RING;
            let a = now.wrapping_sub(self.times[idx]);
            if a > span {
                break;
            }
            age = a;
        }
        age
    }

    /// Cycles recorded in the last `span` samples.
    fn recent_count(&self, now: u32, span: u32) -> usize {
        let mut k = 0usize;
        for i in 0..self.count {
            let idx = (self.head + CYCLE_RING - 1 - i) % CYCLE_RING;
            if now.wrapping_sub(self.times[idx]) > span {
                break;
            }
            k += 1;
        }
        k
    }
}

/// The ear: feed it samples, ask it for readings.
pub struct GridEar {
    sample_rate: f32,
    n: u32,
    dc: DcBlocker,
    bands: [Band; 3],
    env_k: f32,
    tracked: usize,
    ticks: [f32; TICK_RING],
    tick_head: usize,
    tick_count: usize,
    /// The level the hum has held, a ten-second average of the tracked band's
    /// mean square, frozen while the hum is lost so silence cannot become the
    /// new normal.
    held_env: f32,
    held_k: f32,
    lost: bool,
}

impl GridEar {
    /// An ear for a `nominal_hz` grid (60.0 in North America, 50.0 elsewhere)
    /// at `sample_rate`.
    pub fn new(nominal_hz: f32, sample_rate: f32) -> Self {
        Self {
            sample_rate,
            n: 0,
            dc: DcBlocker::new(sample_rate),
            bands: [
                Band::new(HARMONICS[0], nominal_hz, sample_rate),
                Band::new(HARMONICS[1], nominal_hz, sample_rate),
                Band::new(HARMONICS[2], nominal_hz, sample_rate),
            ],
            env_k: 1.0 - expf(-1.0 / (0.05 * sample_rate)),
            tracked: 0,
            ticks: [0.0; TICK_RING],
            tick_head: 0,
            tick_count: 0,
            held_env: 0.0,
            held_k: 1.0 - expf(-0.05 / 10.0),
            lost: false,
        }
    }

    /// True while the hum has fallen 30 dB under the level it held.
    pub fn lost(&self) -> bool {
        self.lost
    }

    /// Feeds one sample. Returns the tracked band's filtered output, which is
    /// the signal a learner can read decimated.
    pub fn process(&mut self, x: f32) -> f32 {
        let x = self.dc.process(x);
        let n = self.n;
        let mut tracked_out = 0.0;
        for (i, band) in self.bands.iter_mut().enumerate() {
            let y = band.process(x, n, self.env_k, self.sample_rate);
            if i == self.tracked {
                tracked_out = y;
            }
        }
        self.n = self.n.wrapping_add(1);
        tracked_out
    }

    /// The harmonic currently tracked: 1, 2 or 3.
    pub fn harmonic(&self) -> u8 {
        self.bands[self.tracked].harmonic
    }

    /// The number of samples fed so far (wraps after about a day at 48 kHz).
    pub fn samples(&self) -> u32 {
        self.n
    }

    /// Called every 50 ms by the owner. Picks the band with the most energy
    /// (switching only when another band carries twice the current one's, so
    /// the choice does not flap), and reads frequency, rate of change and level.
    /// `None` until three cycles have been heard in the last second.
    pub fn tick(&mut self) -> Option<GridReading> {
        // The band to track: the loudest, with hysteresis.
        let current_env = self.bands[self.tracked].env;
        let mut best = self.tracked;
        let mut best_env = current_env * 2.0;
        for (i, band) in self.bands.iter().enumerate() {
            if i != self.tracked && band.env > best_env {
                best = i;
                best_env = band.env;
            }
        }
        if best != self.tracked {
            // A new band's history is not this band's: start the rate reading over.
            self.tick_count = 0;
        }
        self.tracked = best;

        let band = &self.bands[self.tracked];
        let level_db = if band.env > 1e-12 { 10.0 * log10f(band.env) } else { -120.0 };
        // Lost and found, against the level the hum has held.
        if self.lost {
            if band.env > self.held_env * FOUND_RATIO {
                self.lost = false;
            }
        } else if self.held_env > 0.0 && band.env < self.held_env * LOST_RATIO {
            self.lost = true;
        } else if self.held_env == 0.0 {
            if band.env >= FLOOR_ENV {
                self.held_env = band.env;
            }
        } else {
            self.held_env += self.held_k * (band.env - self.held_env);
        }
        if self.lost || band.env < FLOOR_ENV {
            self.tick_count = 0;
            return None;
        }
        let one_second = self.sample_rate as u32;
        // The bandpass rings for its first cycles and the crossings it yields
        // then are not the grid's: a band reports only once it has held a
        // lock for 400 ms, so the first rate reading is never the filter settling.
        let warm = one_second / 10 * 4;
        if band.recent_count(self.n, one_second) < 3 || band.locked_for(self.n, one_second) < warm {
            self.tick_count = 0;
            return None;
        }
        let tenth = one_second / 10;
        let hz_fast = match band.mean_since(self.n, tenth) {
            Some(f) if band.recent_count(self.n, tenth) >= 3 => f,
            _ => band.mean_last(3)?,
        };
        let hz = band.mean_since(self.n, one_second)?;

        self.ticks[self.tick_head] = hz_fast;
        self.tick_head = (self.tick_head + 1) % TICK_RING;
        if self.tick_count < TICK_RING {
            self.tick_count += 1;
        }
        let rocof = if self.tick_count > ROCOF_TICKS {
            let back = (self.tick_head + TICK_RING - 1 - ROCOF_TICKS) % TICK_RING;
            (hz_fast - self.ticks[back]) / (ROCOF_TICKS as f32 * 0.05)
        } else {
            0.0
        };

        Some(GridReading { hz_fast, hz, rocof, level_db, harmonic: band.harmonic })
    }
}
