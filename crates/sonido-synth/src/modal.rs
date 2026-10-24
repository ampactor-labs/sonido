//! Modal synthesis: struck bars, tines and bells as banks of decaying
//! resonators.
//!
//! A struck object rings at a handful of vibration modes, each a decaying
//! sinusoid with its own frequency, strength and decay time. [`ModalBank`]
//! runs one two-pole resonator per mode,
//!
//! ```text
//! y[n] = b0·x[n] + 2r·cos(ω)·y[n−1] − r²·y[n−2],   b0 = g·sin(ω)
//! ```
//!
//! whose impulse response is `g · rⁿ · sin(ω(n+1))`: amplitude `g` and a
//! 60 dB decay after `T60` seconds when `r = 10^(−3 / (T60 · fs))`. A
//! [`Mallet`] supplies the strike: a short raised-cosine pulse whose length
//! sets how much of the upper modes it excites, so a hard mallet is bright and
//! a soft one is round.
//!
//! The resonators are stored structure-of-arrays in groups of four. On
//! `wasm32` with the `simd128` target feature each group is one `v128`; on
//! other targets it is a `[f32; 4]` that compilers vectorize on their own.
//!
//! Mode tables follow the physics of bars and tines: a free-free bar's
//! overtones sit at 2.756, 5.404, 8.933 and 13.344 times its fundamental, and
//! marimba and vibraphone bars are undercut so their first overtones land on
//! two octaves and three octaves plus a major third (N. H. Fletcher and
//! T. D. Rossing, *The Physics of Musical Instruments*, 2nd ed., 1998, §2.15
//! and §19). The approach is J.-M. Adrien's modal synthesis ("The missing
//! link: modal synthesis", in *Representations of Musical Signals*, MIT
//! Press, 1991).
//!
//! ```rust
//! use sonido_synth::modal::{Mallet, ModalBank, presets};
//!
//! let mut bar: ModalBank<2> = ModalBank::new(48_000.0);
//! bar.set_modes(440.0, presets::MARIMBA);
//! let mut mallet = Mallet::new(48_000.0);
//! mallet.strike(0.8, 0.4);
//! let first: f32 = (0..64).map(|_| bar.process(mallet.next_sample())).sum();
//! assert!(first.abs() > 0.0);
//! ```

use libm::{cosf, expf, sinf};

/// One vibration mode, relative to a fundamental.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mode {
    /// Frequency as a multiple of the fundamental.
    pub ratio: f32,
    /// Amplitude of its ring for a unit strike.
    pub gain: f32,
    /// Time to decay by 60 dB, in seconds.
    pub t60: f32,
}

impl Mode {
    /// A mode at `ratio` times the fundamental.
    pub const fn new(ratio: f32, gain: f32, t60: f32) -> Self {
        Self { ratio, gain, t60 }
    }
}

#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
mod lane {
    use core::arch::wasm32::{
        f32x4, f32x4_abs, f32x4_add, f32x4_extract_lane, f32x4_mul, f32x4_splat, f32x4_sub, v128,
    };

    /// Four resonators' worth of one coefficient or state variable.
    #[derive(Clone, Copy, Debug)]
    pub struct Lane(v128);

    impl Lane {
        #[inline]
        pub fn splat(x: f32) -> Self {
            Self(f32x4_splat(x))
        }
        #[inline]
        pub fn from_array(a: [f32; 4]) -> Self {
            Self(f32x4(a[0], a[1], a[2], a[3]))
        }
        #[inline]
        pub fn to_array(self) -> [f32; 4] {
            [
                f32x4_extract_lane::<0>(self.0),
                f32x4_extract_lane::<1>(self.0),
                f32x4_extract_lane::<2>(self.0),
                f32x4_extract_lane::<3>(self.0),
            ]
        }
        #[inline]
        pub fn mul(self, o: Self) -> Self {
            Self(f32x4_mul(self.0, o.0))
        }
        #[inline]
        pub fn add(self, o: Self) -> Self {
            Self(f32x4_add(self.0, o.0))
        }
        #[inline]
        pub fn sub(self, o: Self) -> Self {
            Self(f32x4_sub(self.0, o.0))
        }
        #[inline]
        pub fn abs(self) -> Self {
            Self(f32x4_abs(self.0))
        }
        #[inline]
        pub fn sum(self) -> f32 {
            let a = self.to_array();
            (a[0] + a[1]) + (a[2] + a[3])
        }
    }
}

#[cfg(not(all(target_arch = "wasm32", target_feature = "simd128")))]
mod lane {
    /// Four resonators' worth of one coefficient or state variable.
    #[derive(Clone, Copy, Debug)]
    pub struct Lane([f32; 4]);

    impl Lane {
        #[inline]
        pub fn splat(x: f32) -> Self {
            Self([x; 4])
        }
        #[inline]
        pub fn from_array(a: [f32; 4]) -> Self {
            Self(a)
        }
        #[inline]
        pub fn to_array(self) -> [f32; 4] {
            self.0
        }
        #[inline]
        pub fn mul(self, o: Self) -> Self {
            Self([
                self.0[0] * o.0[0],
                self.0[1] * o.0[1],
                self.0[2] * o.0[2],
                self.0[3] * o.0[3],
            ])
        }
        #[inline]
        pub fn add(self, o: Self) -> Self {
            Self([
                self.0[0] + o.0[0],
                self.0[1] + o.0[1],
                self.0[2] + o.0[2],
                self.0[3] + o.0[3],
            ])
        }
        #[inline]
        pub fn sub(self, o: Self) -> Self {
            Self([
                self.0[0] - o.0[0],
                self.0[1] - o.0[1],
                self.0[2] - o.0[2],
                self.0[3] - o.0[3],
            ])
        }
        #[inline]
        pub fn abs(self) -> Self {
            Self([
                self.0[0].abs(),
                self.0[1].abs(),
                self.0[2].abs(),
                self.0[3].abs(),
            ])
        }
        #[inline]
        pub fn sum(self) -> f32 {
            let a = self.to_array();
            (a[0] + a[1]) + (a[2] + a[3])
        }
    }
}

use lane::Lane;

/// Ring level below which a bank counts as silent (about −100 dBFS).
const SILENT: f32 = 1e-5;
/// Highest mode frequency kept, as a fraction of the sample rate.
const MAX_FREQ_RATIO: f32 = 0.45;

/// A bank of up to `4 × G` modal resonators.
///
/// Coefficient changes keep the resonators' state, so retuning a ringing
/// bank bends its pitch rather than restarting it.
#[derive(Clone, Debug)]
pub struct ModalBank<const G: usize> {
    a1: [Lane; G],
    a2: [Lane; G],
    b0: [Lane; G],
    y1: [Lane; G],
    y2: [Lane; G],
    groups: usize,
    modes: usize,
    sample_rate: f32,
}

impl<const G: usize> ModalBank<G> {
    /// Maximum number of modes.
    pub const CAPACITY: usize = 4 * G;

    /// An empty bank at `sample_rate`.
    pub fn new(sample_rate: f32) -> Self {
        Self {
            a1: [Lane::splat(0.0); G],
            a2: [Lane::splat(0.0); G],
            b0: [Lane::splat(0.0); G],
            y1: [Lane::splat(0.0); G],
            y2: [Lane::splat(0.0); G],
            groups: 0,
            modes: 0,
            sample_rate,
        }
    }

    /// Tune the bank to `modes` above `fundamental_hz`.
    pub fn set_modes(&mut self, fundamental_hz: f32, modes: &[Mode]) {
        self.set_modes_scaled(fundamental_hz, modes, 1.0, 1.0);
    }

    /// Like [`set_modes`](Self::set_modes), with every gain multiplied by
    /// `gain_scale` and every decay time by `decay_scale` (higher notes of
    /// real instruments decay faster).
    pub fn set_modes_scaled(
        &mut self,
        fundamental_hz: f32,
        modes: &[Mode],
        gain_scale: f32,
        decay_scale: f32,
    ) {
        let mut partials = [(0.0f32, 0.0f32, 0.0f32); 64];
        let mut count = 0;
        for m in modes.iter().take(partials.len()) {
            partials[count] = (
                fundamental_hz * m.ratio,
                m.gain * gain_scale,
                m.t60 * decay_scale,
            );
            count += 1;
        }
        self.set_partials(&partials[..count]);
    }

    /// Tune the bank to explicit partials: `(frequency_hz, gain, t60_s)`.
    ///
    /// Partials above 0.45 × the sample rate, with zero gain, or past the
    /// bank's capacity are dropped.
    pub fn set_partials(&mut self, partials: &[(f32, f32, f32)]) {
        let mut a1 = [[0.0f32; 4]; G];
        let mut a2 = [[0.0f32; 4]; G];
        let mut b0 = [[0.0f32; 4]; G];
        let mut count = 0usize;
        let nyquist_cut = self.sample_rate * MAX_FREQ_RATIO;
        for &(freq, gain, t60) in partials {
            if count >= Self::CAPACITY {
                break;
            }
            if !(freq > 0.0 && freq < nyquist_cut && gain != 0.0 && t60 > 0.0) {
                continue;
            }
            let w = 2.0 * core::f32::consts::PI * freq / self.sample_rate;
            let r = expf(-6.907_755 / (t60 * self.sample_rate));
            let (g, l) = (count / 4, count % 4);
            a1[g][l] = 2.0 * r * cosf(w);
            a2[g][l] = r * r;
            b0[g][l] = gain * sinf(w);
            count += 1;
        }
        for g in 0..G {
            self.a1[g] = Lane::from_array(a1[g]);
            self.a2[g] = Lane::from_array(a2[g]);
            self.b0[g] = Lane::from_array(b0[g]);
        }
        let groups = count.div_ceil(4);
        // Resonators beyond the new mode count stop ringing at once.
        for g in groups..self.groups {
            self.y1[g] = Lane::splat(0.0);
            self.y2[g] = Lane::splat(0.0);
        }
        self.groups = groups;
        self.modes = count;
    }

    /// Number of active modes.
    pub fn modes(&self) -> usize {
        self.modes
    }

    /// Run one sample of excitation through every mode; return their sum.
    #[inline]
    pub fn process(&mut self, excitation: f32) -> f32 {
        let x = Lane::splat(excitation);
        let mut acc = Lane::splat(0.0);
        for g in 0..self.groups {
            let y = self.b0[g]
                .mul(x)
                .add(self.a1[g].mul(self.y1[g]))
                .sub(self.a2[g].mul(self.y2[g]));
            self.y2[g] = self.y1[g];
            self.y1[g] = y;
            acc = acc.add(y);
        }
        acc.sum()
    }

    /// Process a block, adding `gain ×` the bank's output into `out`.
    ///
    /// `excitation` may be shorter than `out` (including empty); the missing
    /// samples are silence, which is the common case for a ringing bank.
    pub fn process_add(&mut self, excitation: &[f32], out: &mut [f32], gain: f32) {
        for (n, o) in out.iter_mut().enumerate() {
            let x = excitation.get(n).copied().unwrap_or(0.0);
            *o += gain * self.process(x);
        }
    }

    /// The bank's current ring level: the sum of every resonator's last two
    /// output magnitudes.
    pub fn level(&self) -> f32 {
        let mut acc = Lane::splat(0.0);
        for g in 0..self.groups {
            acc = acc.add(self.y1[g].abs()).add(self.y2[g].abs());
        }
        acc.sum()
    }

    /// `true` once the bank has rung down to about −100 dBFS. A silent bank
    /// is also cleared, so its resonators never decay into denormal numbers
    /// (WebAssembly has no flush-to-zero mode to save them).
    pub fn is_silent(&mut self) -> bool {
        if self.level() < SILENT {
            self.reset();
            true
        } else {
            false
        }
    }

    /// Stop every mode ringing; keep the tuning.
    pub fn reset(&mut self) {
        self.y1 = [Lane::splat(0.0); G];
        self.y2 = [Lane::splat(0.0); G];
    }
}

/// A mallet: the short pulse that strikes a modal bank.
///
/// The pulse is a raised cosine whose area is the strike's velocity, so a
/// strike excites low modes by about `velocity × gain` whatever the hardness.
/// Hardness sets its length, from 1.5 ms (soft felt) to 0.12 ms (a hard
/// plastic mallet): a raised cosine of length `T` passes little above `2/T`,
/// so shorter pulses reach higher modes.
#[derive(Clone, Debug)]
pub struct Mallet {
    sample_rate: f32,
    len: u32,
    pos: u32,
    peak: f32,
}

impl Mallet {
    /// A mallet at `sample_rate`, at rest.
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            len: 1,
            pos: 1,
            peak: 0.0,
        }
    }

    /// Strike with `velocity` (0–1) and `hardness` (0 soft – 1 hard).
    pub fn strike(&mut self, velocity: f32, hardness: f32) {
        let h = hardness.clamp(0.0, 1.0);
        // 1.5 ms at hardness 0 down to 0.12 ms at hardness 1, geometrically.
        let seconds = 0.0015 * expf(-2.525_729 * h);
        self.len = ((seconds * self.sample_rate) as u32).max(2);
        self.pos = 0;
        self.peak = 2.0 * velocity.max(0.0) / self.len as f32;
    }

    /// The next sample of the pulse (0 once it has passed).
    #[inline]
    pub fn next_sample(&mut self) -> f32 {
        if self.pos >= self.len {
            return 0.0;
        }
        let phase = self.pos as f32 / self.len as f32;
        self.pos += 1;
        self.peak * 0.5 * (1.0 - cosf(2.0 * core::f32::consts::PI * phase))
    }

    /// `true` while the pulse is still sounding.
    pub fn is_active(&self) -> bool {
        self.pos < self.len
    }
}

/// Mode tables.
pub mod presets {
    use super::Mode;

    /// A rosewood marimba bar, undercut so its first overtone sits two
    /// octaves up and its second near three octaves and a major third.
    pub const MARIMBA: &[Mode] = &[
        Mode::new(1.0, 1.0, 1.1),
        Mode::new(3.98, 0.34, 0.45),
        Mode::new(9.9, 0.12, 0.18),
    ];

    /// An aluminum vibraphone bar (motor off), tuned 1 : 4 : 10.
    pub const VIBRAPHONE: &[Mode] = &[
        Mode::new(1.0, 1.0, 3.2),
        Mode::new(4.0, 0.22, 1.1),
        Mode::new(10.0, 0.07, 0.45),
    ];

    /// A steel glockenspiel bar: the untuned overtones of a free-free bar.
    pub const GLOCKENSPIEL: &[Mode] = &[
        Mode::new(1.0, 1.0, 2.6),
        Mode::new(2.756, 0.55, 1.7),
        Mode::new(5.404, 0.36, 1.1),
        Mode::new(8.933, 0.2, 0.7),
        Mode::new(13.344, 0.09, 0.4),
    ];

    /// A celesta: free-free bar overtones damped by its felt hammers.
    pub const CELESTA: &[Mode] = &[
        Mode::new(1.0, 1.0, 1.4),
        Mode::new(2.756, 0.25, 0.6),
        Mode::new(5.404, 0.1, 0.3),
    ];

    /// A kalimba tine: a clamped-free cantilever, overtones at 6.267 and
    /// 17.55 times the fundamental.
    pub const KALIMBA: &[Mode] = &[
        Mode::new(1.0, 1.0, 1.6),
        Mode::new(6.267, 0.18, 0.35),
        Mode::new(17.55, 0.05, 0.12),
    ];

    /// A wood block or rim click: three short, inharmonic modes.
    pub const WOODBLOCK: &[Mode] = &[
        Mode::new(1.0, 1.0, 0.09),
        Mode::new(2.57, 0.55, 0.06),
        Mode::new(4.1, 0.3, 0.04),
    ];

    /// A bell whose partials are all chord tones: the hum an octave below,
    /// the prime, the chord's third (`third_ratio`, 1.189 for minor or 1.260
    /// for major in equal temperament), the fifth, the nominal an octave up,
    /// and two higher partials. Lower partials ring longest, as in a cast
    /// bell. Returns the number of partials written to `out` as
    /// `(frequency_hz, gain, t60_s)`.
    pub fn chord_bell(root_hz: f32, third_ratio: f32, out: &mut [(f32, f32, f32)]) -> usize {
        let fifth = 1.498_307_1; // 2^(7/12)
        let table = [
            (0.5, 0.45, 7.0),
            (1.0, 0.6, 5.0),
            (third_ratio, 0.5, 4.0),
            (fifth, 0.32, 3.0),
            (2.0, 1.0, 3.5),
            (2.0 * fifth, 0.35, 1.6),
            (4.0, 0.15, 1.0),
        ];
        let n = table.len().min(out.len());
        for (slot, &(ratio, gain, t60)) in out.iter_mut().zip(table.iter()).take(n) {
            *slot = (root_hz * ratio, gain, t60);
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f32 = 48_000.0;

    /// Goertzel magnitude of `x` at `freq`.
    fn tone(x: &[f32], freq: f32) -> f32 {
        let w = 2.0 * core::f64::consts::PI * f64::from(freq) / f64::from(FS);
        let coeff = 2.0 * libm::cos(w);
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for &v in x {
            let s = f64::from(v) + coeff * s1 - s2;
            s2 = s1;
            s1 = s;
        }
        let power = s1 * s1 + s2 * s2 - coeff * s1 * s2;
        (libm::sqrt(power.max(0.0)) * 2.0 / x.len() as f64) as f32
    }

    fn ring<const G: usize>(bank: &mut ModalBank<G>, samples: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(samples);
        out.push(bank.process(1.0));
        for _ in 1..samples {
            out.push(bank.process(0.0));
        }
        out
    }

    #[test]
    fn a_mode_rings_at_its_frequency_with_its_gain() {
        let mut bank: ModalBank<1> = ModalBank::new(FS);
        bank.set_partials(&[(1000.0, 0.5, 5.0)]);
        let out = ring(&mut bank, 4800);
        let peak = out.iter().take(200).fold(0.0f32, |m, v| m.max(v.abs()));
        assert!((peak - 0.5).abs() < 0.01, "first-cycle amplitude {peak}");
        let at = tone(&out, 1000.0);
        assert!(at > 5.0 * tone(&out, 1050.0) && at > 5.0 * tone(&out, 950.0));
    }

    #[test]
    fn decay_matches_t60() {
        let mut bank: ModalBank<1> = ModalBank::new(FS);
        bank.set_partials(&[(440.0, 1.0, 0.5)]);
        let out = ring(&mut bank, 36_000);
        let env = |t: f32| {
            let n = (t * FS) as usize;
            out[n..n + 220].iter().fold(0.0f32, |m, v| m.max(v.abs()))
        };
        let drop_db = 20.0 * libm::log10f(env(0.5) / env(0.0));
        assert!((drop_db + 60.0).abs() < 1.5, "drop after T60: {drop_db} dB");
    }

    #[test]
    fn matches_a_plain_resonator_per_mode() {
        let modes = [
            (220.0f32, 1.0f32, 1.0f32),
            (607.0, 0.4, 0.6),
            (1190.0, 0.3, 0.3),
            (1966.0, 0.2, 0.2),
            (2937.0, 0.1, 0.1),
        ];
        let mut bank: ModalBank<2> = ModalBank::new(FS);
        bank.set_partials(&modes);
        assert_eq!(bank.modes(), 5);
        let mut naive: Vec<(f32, f32, f32, f32, f32)> = modes
            .iter()
            .map(|&(f, g, t60)| {
                let w = 2.0 * core::f32::consts::PI * f / FS;
                let r = expf(-6.907_755 / (t60 * FS));
                (2.0 * r * cosf(w), r * r, g * sinf(w), 0.0, 0.0)
            })
            .collect();
        for n in 0..2000 {
            let x = if n == 0 { 1.0 } else { 0.0 };
            let mut expected = 0.0;
            for m in &mut naive {
                let y = m.2 * x + m.0 * m.3 - m.1 * m.4;
                m.4 = m.3;
                m.3 = y;
                expected += y;
            }
            let got = bank.process(x);
            assert!(
                (got - expected).abs() < 1e-5,
                "sample {n}: {got} vs {expected}"
            );
        }
    }

    #[test]
    fn drops_modes_it_cannot_play() {
        let mut bank: ModalBank<1> = ModalBank::new(FS);
        bank.set_modes(
            1000.0,
            &[
                Mode::new(1.0, 1.0, 1.0),
                Mode::new(30.0, 1.0, 1.0), // 30 kHz: above Nyquist
                Mode::new(2.0, 0.0, 1.0),  // silent
                Mode::new(3.0, 1.0, 0.5),
                Mode::new(4.0, 1.0, 0.5),
                Mode::new(5.0, 1.0, 0.5),
                Mode::new(6.0, 1.0, 0.5), // past capacity (4)
            ],
        );
        assert_eq!(bank.modes(), 4);
        let out = ring(&mut bank, 1000);
        assert!(out.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn goes_silent_and_clears_itself() {
        let mut bank: ModalBank<2> = ModalBank::new(FS);
        bank.set_modes(880.0, presets::GLOCKENSPIEL);
        let mut mallet = Mallet::new(FS);
        mallet.strike(1.0, 0.8);
        let mut silent_at = None;
        for n in 0..(FS as usize * 6) {
            bank.process(mallet.next_sample());
            // A bank is only silent once its strike is over: the pulse's
            // first samples are near zero by design.
            if n % 480 == 0 && !mallet.is_active() && bank.is_silent() {
                silent_at = Some(n);
                break;
            }
        }
        let n = silent_at.expect("a 2.6 s glockenspiel should be silent within 6 s");
        assert!(n > FS as usize, "silent too soon: {n}");
        assert_eq!(bank.level(), 0.0);
    }

    #[test]
    fn harder_mallets_are_brighter() {
        let brightness = |hardness: f32| {
            let mut bank: ModalBank<2> = ModalBank::new(FS);
            bank.set_modes(500.0, presets::GLOCKENSPIEL);
            let mut mallet = Mallet::new(FS);
            mallet.strike(1.0, hardness);
            let out: Vec<f32> = (0..9600)
                .map(|_| bank.process(mallet.next_sample()))
                .collect();
            tone(&out, 500.0 * 8.933) / tone(&out, 500.0)
        };
        assert!(brightness(1.0) > 3.0 * brightness(0.0));
    }

    #[test]
    fn chord_bells_stay_in_the_chord() {
        let mut partials = [(0.0f32, 0.0f32, 0.0f32); 8];
        let n = presets::chord_bell(220.0, 1.259_921, &mut partials);
        assert_eq!(n, 7);
        let freqs: Vec<f32> = partials[..n].iter().map(|p| p.0).collect();
        // A major chord's pitch classes: A, C♯, E.
        for f in freqs {
            let semis = 12.0 * libm::log2f(f / 220.0);
            let pc = (semis.round() as i32).rem_euclid(12);
            assert!([0, 4, 7].contains(&pc), "{f} Hz is pitch class {pc}");
            assert!(
                (semis - semis.round()).abs() < 0.01,
                "{f} Hz is out of tune"
            );
        }
    }
}
