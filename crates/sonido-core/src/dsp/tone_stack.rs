//! Interactive 3-band tone stack using peaking EQ biquad filters.
//!
//! [`ToneStack`] models the classic bass/mid/treble tone controls found in
//! guitar amplifiers (Fender, Marshall, Vox) and channel strips. The controls
//! interact subtly — mid cut/boost affects the perceived bass and treble response.
//!
//! # Signal Flow
//!
//! ```text
//! input → low_biquad (100 Hz peak) → mid_biquad (800 Hz peak) → high_biquad (3200 Hz peak) → output
//! ```
//!
//! Each band is a peaking EQ biquad. The 0.0–1.0 knob range maps to ±dB gain
//! centred at 0.5 (flat response). Controls:
//!
//! - **Bass** (100 Hz): ±15 dB, Q = 0.7
//! - **Mid** (800 Hz): ±12 dB, Q = 0.8
//! - **Treble** (3200 Hz): ±15 dB, Q = 0.7
//!
//! # Reference
//!
//! Bristow-Johnson, "Audio EQ Cookbook" — peaking EQ biquad coefficients.
//! Zölzer, "DAFX: Digital Audio Effects", 2nd ed., Chapter 2.

use crate::biquad::{Biquad, peaking_eq_coefficients};
use crate::cached::Cached;

/// Center frequency for the bass band in Hz.
const BASS_HZ: f32 = 100.0;
/// Q factor for bass band — broad peak for shelf-like low-end response.
const BASS_Q: f32 = 0.7;
/// Maximum bass cut/boost in dB.
const BASS_MAX_DB: f32 = 15.0;

/// Center frequency for the mid band in Hz.
const MID_HZ: f32 = 800.0;
/// Q factor for mid band — moderate width.
const MID_Q: f32 = 0.8;
/// Maximum mid cut/boost in dB.
const MID_MAX_DB: f32 = 12.0;

/// Center frequency for the treble band in Hz.
const TREBLE_HZ: f32 = 3200.0;
/// Q factor for treble band — broad peak for shelf-like presence.
const TREBLE_Q: f32 = 0.7;
/// Maximum treble cut/boost in dB.
const TREBLE_MAX_DB: f32 = 15.0;

/// 3-band interactive tone stack using cascaded peaking EQ biquads.
///
/// Controls are specified as normalized 0.0–1.0 values (0.5 = flat).
/// Coefficient recomputation is guarded by [`Cached`] with 1e-4 epsilon —
/// no per-sample trig unless the tone controls actually change.
///
/// # Invariants
///
/// - `sample_rate` must be > 0.0
/// - Control values should be in 0.0–1.0 (clamped internally on set)
pub struct ToneStack {
    /// Sample rate in Hz — required for biquad coefficient calculation.
    sample_rate: f32,

    /// Bass peaking EQ (100 Hz).
    low: Biquad,
    /// Mid peaking EQ (800 Hz).
    mid: Biquad,
    /// Treble peaking EQ (3200 Hz).
    high: Biquad,

    /// Cached bass coefficients, keyed on bass gain dB.
    bass_cache: Cached<[f32; 6]>,
    /// Cached mid coefficients, keyed on mid gain dB.
    mid_cache: Cached<[f32; 6]>,
    /// Cached treble coefficients, keyed on treble gain dB.
    treble_cache: Cached<[f32; 6]>,
}

impl ToneStack {
    /// Create a new tone stack initialized for `sample_rate`.
    ///
    /// All bands start at 0 dB (flat response).
    ///
    /// # Arguments
    /// * `sample_rate` — Audio sample rate in Hz (e.g. 44100.0, 48000.0)
    pub fn new(sample_rate: f32) -> Self {
        let flat_bass = peaking_eq_coefficients(BASS_HZ, BASS_Q, 0.0, sample_rate);
        let flat_mid = peaking_eq_coefficients(MID_HZ, MID_Q, 0.0, sample_rate);
        let flat_treble = peaking_eq_coefficients(TREBLE_HZ, TREBLE_Q, 0.0, sample_rate);

        let to_arr = |(b0, b1, b2, a0, a1, a2)| [b0, b1, b2, a0, a1, a2];

        let mut low = Biquad::new();
        let mut mid = Biquad::new();
        let mut high = Biquad::new();

        let bc = to_arr(flat_bass);
        let mc = to_arr(flat_mid);
        let tc = to_arr(flat_treble);

        low.set_coefficients(bc[0], bc[1], bc[2], bc[3], bc[4], bc[5]);
        mid.set_coefficients(mc[0], mc[1], mc[2], mc[3], mc[4], mc[5]);
        high.set_coefficients(tc[0], tc[1], tc[2], tc[3], tc[4], tc[5]);

        let mut bass_cache = Cached::new(bc, 1);
        bass_cache.update(&[0.0], 1e-4, |_| bc);

        let mut mid_cache = Cached::new(mc, 1);
        mid_cache.update(&[0.0], 1e-4, |_| mc);

        let mut treble_cache = Cached::new(tc, 1);
        treble_cache.update(&[0.0], 1e-4, |_| tc);

        Self {
            sample_rate,
            low,
            mid,
            high,
            bass_cache,
            mid_cache,
            treble_cache,
        }
    }

    /// Set all three tone controls simultaneously.
    ///
    /// Coefficients are updated lazily — this only recalculates filters for
    /// bands where the value has changed beyond 1e-4 dB.
    ///
    /// # Arguments
    ///
    /// * `bass` — Bass knob 0.0–1.0 (0.5 = flat, 0.0 = max cut, 1.0 = max boost)
    /// * `mid` — Mid knob 0.0–1.0
    /// * `treble` — Treble knob 0.0–1.0
    pub fn set_controls(&mut self, bass: f32, mid: f32, treble: f32) {
        let bass_db = (bass.clamp(0.0, 1.0) * 2.0 - 1.0) * BASS_MAX_DB;
        let mid_db = (mid.clamp(0.0, 1.0) * 2.0 - 1.0) * MID_MAX_DB;
        let treble_db = (treble.clamp(0.0, 1.0) * 2.0 - 1.0) * TREBLE_MAX_DB;

        let sr = self.sample_rate;

        let bc = *self.bass_cache.update(&[bass_db], 1e-4, |inputs| {
            let (b0, b1, b2, a0, a1, a2) = peaking_eq_coefficients(BASS_HZ, BASS_Q, inputs[0], sr);
            [b0, b1, b2, a0, a1, a2]
        });
        self.low
            .set_coefficients(bc[0], bc[1], bc[2], bc[3], bc[4], bc[5]);

        let mc = *self.mid_cache.update(&[mid_db], 1e-4, |inputs| {
            let (b0, b1, b2, a0, a1, a2) = peaking_eq_coefficients(MID_HZ, MID_Q, inputs[0], sr);
            [b0, b1, b2, a0, a1, a2]
        });
        self.mid
            .set_coefficients(mc[0], mc[1], mc[2], mc[3], mc[4], mc[5]);

        let tc = *self.treble_cache.update(&[treble_db], 1e-4, |inputs| {
            let (b0, b1, b2, a0, a1, a2) =
                peaking_eq_coefficients(TREBLE_HZ, TREBLE_Q, inputs[0], sr);
            [b0, b1, b2, a0, a1, a2]
        });
        self.high
            .set_coefficients(tc[0], tc[1], tc[2], tc[3], tc[4], tc[5]);
    }

    /// Process a single audio sample through all three EQ bands in cascade.
    ///
    /// Order: bass → mid → treble.
    #[inline]
    pub fn process(&mut self, input: f32) -> f32 {
        let after_low = self.low.process(input);
        let after_mid = self.mid.process(after_low);
        self.high.process(after_mid)
    }

    /// Reset all biquad filter states to zero.
    ///
    /// Call when the audio stream is interrupted to prevent artefacts.
    pub fn reset(&mut self) {
        self.low.clear();
        self.mid.clear();
        self.high.clear();
        self.bass_cache.invalidate();
        self.mid_cache.invalidate();
        self.treble_cache.invalidate();
    }

    /// Update sample rate and invalidate all coefficient caches.
    ///
    /// # Arguments
    /// * `sample_rate` — New audio sample rate in Hz
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.bass_cache.invalidate();
        self.mid_cache.invalidate();
        self.treble_cache.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_setting_passes_dc() {
        let mut ts = ToneStack::new(48000.0);
        ts.set_controls(0.5, 0.5, 0.5); // flat
        let mut out = 0.0;
        for _ in 0..1000 {
            out = ts.process(1.0);
        }
        // All-flat cascade should be near unity for DC
        assert!(
            (out - 1.0).abs() < 0.05,
            "flat tone stack should pass DC: got {out}"
        );
    }

    #[test]
    fn output_is_finite() {
        let mut ts = ToneStack::new(48000.0);
        ts.set_controls(0.0, 0.5, 1.0); // extreme settings
        for i in 0..256 {
            let x = libm::sinf(i as f32 * 0.1) * 0.5;
            let y = ts.process(x);
            assert!(y.is_finite(), "output must be finite at sample {i}: {y}");
        }
    }

    #[test]
    fn bass_boost_increases_low_freq_energy() {
        let sr = 48000.0;
        let freq = 80.0; // below bass center

        let mut ts_boost = ToneStack::new(sr);
        ts_boost.set_controls(1.0, 0.5, 0.5); // max bass boost

        let mut ts_flat = ToneStack::new(sr);
        ts_flat.set_controls(0.5, 0.5, 0.5); // flat

        let mut rms_boost = 0.0f32;
        let mut rms_flat = 0.0f32;
        let n = 2000;
        for i in 0..n {
            let x = libm::sinf(2.0 * core::f32::consts::PI * freq * i as f32 / sr) * 0.3;
            let yb = ts_boost.process(x);
            let yf = ts_flat.process(x);
            rms_boost += yb * yb;
            rms_flat += yf * yf;
        }
        rms_boost = libm::sqrtf(rms_boost / n as f32);
        rms_flat = libm::sqrtf(rms_flat / n as f32);

        assert!(
            rms_boost > rms_flat,
            "bass boost should increase low-freq energy: boost={rms_boost}, flat={rms_flat}"
        );
    }

    #[test]
    fn treble_boost_increases_high_freq_energy() {
        let sr = 48000.0;
        let freq = 4000.0; // above treble center

        let mut ts_boost = ToneStack::new(sr);
        ts_boost.set_controls(0.5, 0.5, 1.0); // max treble boost

        let mut ts_flat = ToneStack::new(sr);
        ts_flat.set_controls(0.5, 0.5, 0.5); // flat

        let mut rms_boost = 0.0f32;
        let mut rms_flat = 0.0f32;
        let n = 2000;
        for i in 0..n {
            let x = libm::sinf(2.0 * core::f32::consts::PI * freq * i as f32 / sr) * 0.3;
            let yb = ts_boost.process(x);
            let yf = ts_flat.process(x);
            rms_boost += yb * yb;
            rms_flat += yf * yf;
        }
        rms_boost = libm::sqrtf(rms_boost / n as f32);
        rms_flat = libm::sqrtf(rms_flat / n as f32);

        assert!(
            rms_boost > rms_flat,
            "treble boost should increase high-freq energy: boost={rms_boost}, flat={rms_flat}"
        );
    }

    #[test]
    fn reset_clears_filter_state() {
        let mut ts = ToneStack::new(48000.0);
        ts.set_controls(0.8, 0.3, 0.9);
        for _ in 0..100 {
            ts.process(0.5);
        }
        ts.reset();
        let y = ts.process(0.0);
        assert!(y.abs() < 1e-6, "after reset with silence: got {y}");
    }
}
