//! Chromatic tuner kernel — YIN pitch detection with READ_ONLY diagnostic params.
//!
//! `TunerKernel` box-averages the mono-summed input down to ~24 kHz into a
//! 2048-sample ring, runs the YIN pitch detection algorithm on the latest
//! 2048-sample frame every 1024 new samples (50% hop, ~43 ms), and exposes the
//! detected frequency and cents deviation as READ_ONLY parameters. Detection
//! reaches down to ~23 Hz. Audio is passed through with zero algorithmic
//! latency. An optional mute mode silences the
//! output for silent tuning.
//!
//! Parameters are received via `&TunerParams` each sample. Deployed via
//! [`Adapter`](sonido_core::kernel::Adapter) for desktop/plugin, or via `Adapter::new_direct`
//! (no smoothing) on embedded targets.
//!
//! # Signal Flow
//!
//! ```text
//! Input → [mono sum → decimate to ~24 kHz → 2048-sample ring]
//!       → passthrough (or silence if mute=1)
//!       → output gain
//!
//! Every 1024 decimated samples (50% hop):
//!   YIN difference function → CMNDF → threshold search → parabolic interpolation
//!   → Hz → nearest note → cents deviation → stored in detected_hz / cents_deviation
//! ```
//!
//! # YIN Algorithm
//!
//! Reference: A. de Cheveigné and H. Kawahara, "YIN, a fundamental frequency
//! estimator for speech and music", JASA 111(4), 2002.
//!
//! Steps:
//! 1. Compute difference function `d(τ) = Σ (x[j] - x[j+τ])²`
//! 2. Compute CMNDF (Cumulative Mean Normalized Difference Function):
//!    `d'(τ=0)=1`, `d'(τ) = d(τ) / ((1/τ) Σ d(j), j=1..τ)`
//! 3. Find first τ where `d'(τ) < threshold (0.15)` and `d'(τ)` is a local minimum
//! 4. Parabolic interpolation for sub-sample accuracy
//! 5. `f0 = sample_rate / τ_interpolated`
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! let adapter = Adapter::new(TunerKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//! ```

extern crate alloc;

use alloc::vec::Vec;
#[cfg(test)]
use sonido_core::kernel::KernelParams;
use sonido_core::kernel::{DspKernel, SmoothingStyle};
use sonido_core::kernel_params;
use sonido_core::level::Level;
use sonido_core::{ParamDescriptor, ParamFlags, ParamId, ParamScale, ParamUnit, fast_db_to_linear};

/// YIN threshold: lower = more accurate but more misses; 0.15 is a good balance.
const YIN_THRESHOLD: f32 = 0.15;

/// Analysis frame length, in decimated samples.
const BUF_LEN: usize = 2048;

/// Maximum lag to search. The lowest detectable pitch is
/// `analysis_rate / TAU_MAX`: 24 kHz / 1024 ≈ 23 Hz, below a five-string
/// bass's low B (30.9 Hz).
const TAU_MAX: usize = BUF_LEN / 2;

/// Rate YIN runs at. The input is averaged down by an integer factor to land
/// near this, so one 2048-sample frame spans ~85 ms at any host rate —
/// several periods of the lowest bass string — while high notes keep enough
/// samples per period for sub-cent interpolation. (The previous 1024-sample
/// frame at the host rate only reached 48 kHz / 512 = 94 Hz: not even a
/// guitar's low E.)
const ANALYSIS_RATE_HZ: f32 = 24_000.0;

/// New decimated samples between analyses (50% overlap: ~43 ms updates).
const HOP: usize = BUF_LEN / 2;

/// Ring length: the analysis window plus one hop of headroom, so the window
/// under analysis stays intact while the next hop is written.
const RING: usize = BUF_LEN + HOP;

/// Integer decimation factor that brings `sample_rate` nearest to [`ANALYSIS_RATE_HZ`].
fn decimation_for(sample_rate: f32) -> usize {
    (libm::roundf(sample_rate / ANALYSIS_RATE_HZ) as usize).max(1)
}

/// Minimum lag to search: caps detection at `analysis_rate / 2` (~12 kHz).
const TAU_MIN: usize = 2;

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`TunerKernel`].
///
/// Indices 3 and 4 are READ_ONLY + HIDDEN — they are written by the kernel via
/// `update_diagnostics()` and read by GUIs / hosts for display purposes only.
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `reference_hz` | Hz | 415–465 | 440.0 |
/// | 1 | `mute` | index | 0–1 | 0 (off) |
/// | 2 | `output_db` | dB | −6–+6 | 0.0 |
/// | 3 | `detected_hz` | Hz | 0–5000 | 0.0 (READ_ONLY) |
/// | 4 | `cents` | cents | −50–50 | 0.0 (READ_ONLY) |
#[derive(Debug, Clone, Copy)]
pub struct TunerParams {
    /// Reference tuning frequency for A4 in Hz. Range: 415–465 Hz. Default: 440.0.
    pub reference_hz: f32,
    /// Mute toggle: 0.0 = pass audio through, 1.0 = silence output (for silent tuning).
    pub mute: f32,
    /// Output level in decibels. Range: −6.0 to +6.0 dB.
    pub output_db: f32,
    /// Detected fundamental frequency in Hz.
    ///
    /// READ_ONLY — written by the kernel. Updated about every 43 ms
    /// (once per 1024-sample hop of the decimated ~24 kHz signal). Zero until
    /// a pitch is first detected; an unvoiced frame keeps the previous value.
    pub detected_hz: f32,
    /// Deviation from the nearest equal-temperament note in cents.
    ///
    /// READ_ONLY — written by the kernel. Range: −50 to +50 cents.
    /// Positive values mean the pitch is sharp; negative mean flat.
    pub cents: f32,
}

impl Default for TunerParams {
    fn default() -> Self {
        Self {
            reference_hz: 440.0,
            mute: 0.0,
            output_db: 0.0,
            detected_hz: 0.0,
            cents: 0.0,
        }
    }
}

kernel_params! {
    TunerParams, this {
        [0] ParamDescriptor::custom("Reference", "Ref", 415.0, 465.0, 440.0)
                .with_unit(ParamUnit::Hertz)
                .with_scale(ParamScale::Logarithmic)
                .with_id(ParamId(2300), "tuner_reference"),
            smoothing: SmoothingStyle::None, // reference_hz — snap
            get: this.reference_hz,
            set: |v| this.reference_hz = v;

        [1] ParamDescriptor::custom("Mute", "Mute", 0.0, 1.0, 0.0)
                .with_unit(ParamUnit::None)
                .with_step(1.0)
                .with_id(ParamId(2301), "tuner_mute")
                .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                .with_step_labels(&["Off", "On"]),
            smoothing: SmoothingStyle::None, // stepped, snap
            get: this.mute,
            set: |v| this.mute = v;

        [2] sonido_core::gain::output_param_descriptor().with_id(ParamId(2302), "tuner_output"),
            smoothing: SmoothingStyle::Fast, // output_db — 5 ms
            get: this.output_db,
            set: |v| this.output_db = v;

        [3] ParamDescriptor::custom("Detected Hz", "Det Hz", 0.0, 5000.0, 0.0)
                .with_unit(ParamUnit::Hertz)
                .with_id(ParamId(2303), "tuner_detected_hz")
                .with_flags(ParamFlags::READ_ONLY.union(ParamFlags::HIDDEN)),
            smoothing: SmoothingStyle::None, // READ_ONLY diagnostic
            get: this.detected_hz,
            set: |v| this.detected_hz = v;

        [4] ParamDescriptor::custom("Cents", "Cents", -50.0, 50.0, 0.0)
                .with_unit(ParamUnit::None)
                .with_id(ParamId(2304), "tuner_cents")
                .with_flags(ParamFlags::READ_ONLY.union(ParamFlags::HIDDEN)),
            smoothing: SmoothingStyle::None, // READ_ONLY diagnostic
            get: this.cents,
            set: |v| this.cents = v;
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  YIN helper functions
// ═══════════════════════════════════════════════════════════════════════════

/// Run the YIN algorithm on `buf` and return detected frequency in Hz, or `None`.
///
/// This is a pure function — no state except the input buffer. The caller owns
/// `scratch`, which must have length `TAU_MAX`.
#[cfg(test)]
fn yin_detect(buf: &[f32], sample_rate: f32, scratch: &mut [f32]) -> Option<f32> {
    debug_assert_eq!(buf.len(), BUF_LEN);
    debug_assert_eq!(scratch.len(), TAU_MAX);
    let mut raw = [0.0f32; 3];
    let mut running_sum = 0.0f32;
    scratch[0] = 1.0;
    for tau in 1..TAU_MAX {
        let d = yin_difference(buf, tau);
        scratch[tau] = yin_normalize(d, tau, &mut running_sum);
    }
    let tau_est = yin_tau_estimate(scratch)?;
    for (k, r) in raw.iter_mut().enumerate() {
        *r = yin_difference(buf, (tau_est + k).saturating_sub(1).max(1));
    }
    yin_interpolate(tau_est, raw, sample_rate)
}

/// YIN step 1 for one lag: the raw difference d(τ) over the integration
/// window. This is the expensive part (one `BUF_LEN - TAU_MAX` dot product
/// per lag), which is why the kernel spreads it across samples.
fn yin_difference(buf: &[f32], tau: usize) -> f32 {
    let w = BUF_LEN - TAU_MAX; // integration window length
    let mut d = 0.0f32;
    for j in 0..w {
        let diff = buf[j] - buf[j + tau];
        d += diff * diff;
    }
    d
}

/// YIN step 2 for lag τ, computed in lag order: the cumulative mean
/// normalized difference d'(τ) = d(τ) / ((1/τ) Σ_{k≤τ} d(k)).
/// `running_sum` carries Σ d between calls.
fn yin_normalize(d: f32, tau: usize, running_sum: &mut f32) -> f32 {
    *running_sum += d;
    if *running_sum > 1e-10 {
        d * (tau as f32) / *running_sum
    } else {
        1.0
    }
}

/// YIN step 3: the first lag whose d'(τ) dips below the threshold at a local
/// minimum, else the global minimum if it is reasonably small.
fn yin_tau_estimate(cmndf: &[f32]) -> Option<usize> {
    for tau in TAU_MIN..TAU_MAX - 1 {
        if cmndf[tau] < YIN_THRESHOLD && cmndf[tau] <= cmndf[tau + 1] {
            return Some(tau);
        }
    }
    let (best_tau, best_val) = (TAU_MIN..TAU_MAX)
        .map(|t| (t, cmndf[t]))
        .fold((TAU_MIN, f32::MAX), |a, b| if b.1 < a.1 { b } else { a });
    (best_val <= 0.35).then_some(best_tau)
}

/// YIN step 4: parabolic interpolation of the period around `tau_est`, on
/// the raw differences `[d(τ-1), d(τ), d(τ+1)]` — not the normalized d'(τ),
/// whose running-mean normalization tilts it and biases the vertex
/// (de Cheveigné & Kawahara 2002, §II.E).
fn yin_interpolate(tau_est: usize, [x0, x1, x2]: [f32; 3], sample_rate: f32) -> Option<f32> {
    let tau_f = if tau_est > 1 && tau_est < TAU_MAX - 1 {
        let denom = x0 - 2.0 * x1 + x2;
        if libm::fabsf(denom) > 1e-10 {
            tau_est as f32 - 0.5 * (x2 - x0) / denom
        } else {
            tau_est as f32
        }
    } else {
        tau_est as f32
    };
    (tau_f >= 1.0).then(|| sample_rate / tau_f)
}

/// Convert a detected frequency to cents deviation from the nearest 12-TET note.
///
/// Returns cents in [-50, 50]. Formula:
/// ```text
/// semitones_from_A4 = 12 * log2(freq / reference_hz)
/// nearest_semitone = round(semitones_from_A4)
/// cents = 100 * (semitones_from_A4 - nearest_semitone)
/// ```
fn hz_to_cents(detected_hz: f32, reference_hz: f32) -> f32 {
    if detected_hz <= 0.0 || reference_hz <= 0.0 {
        return 0.0;
    }
    let semitones = 12.0 * libm::log2f(detected_hz / reference_hz);
    // Round to nearest semitone
    let nearest = libm::roundf(semitones);
    // Cents deviation
    let cents = (semitones - nearest) * 100.0;
    // Clamp to [-50, 50]
    cents.clamp(-50.0, 50.0)
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP chromatic tuner kernel.
///
/// Contains ONLY the mutable state required for audio processing:
/// - A decimator (box average) feeding a doubled ring of ~24 kHz audio,
///   analyzed in place (no frame copy)
/// - YIN raw and normalized difference buffers, filled one lag per decimated
///   sample across each hop, so no audio block carries a whole analysis
/// - Detected Hz and cents deviation (updated when an analysis completes)
///
/// No `SmoothedParam`, no atomics, no platform awareness.
///
/// # Diagnostics
///
/// `detected_hz` and `cents_deviation` are internal state; they are pushed
/// back into params via `update_diagnostics()`, which the
/// [`Adapter`](sonido_core::kernel::Adapter) calls after every processed
/// sample or block.
pub struct TunerKernel {
    /// Ring of decimated mono samples, [`RING`] long, stored twice over
    /// (length `2 * RING`): every sample is written at `i` and `i + RING`,
    /// so any `BUF_LEN` window is one contiguous slice and an analysis reads
    /// the ring in place, with no copy.
    buffer: Vec<f32>,
    /// Next write position in the ring (`0..RING`).
    write_pos: usize,
    /// Decimated samples written since the last analysis.
    since_analysis: usize,
    /// Start (in `buffer`) of the `BUF_LEN` window being analyzed. The ring
    /// is a hop longer than the window, so the next hop's writes never touch
    /// it.
    frame_start: usize,
    /// Input samples averaged per decimated sample.
    decimation: usize,
    /// Running sum and count of the current decimation block.
    decim_sum: f32,
    decim_count: usize,
    /// Last detected fundamental frequency in Hz. Zero if no pitch detected.
    detected_hz: f32,
    /// Last cents deviation from nearest 12-TET note.
    cents_deviation: f32,
    /// Audio sample rate in Hz.
    sample_rate: f32,
    /// d'(τ) of the analysis in progress (length = TAU_MAX).
    yin_scratch: Vec<f32>,
    /// Raw d(τ) of the analysis in progress, kept for interpolation.
    yin_raw: Vec<f32>,
    /// Σ d(τ) so far, for normalizing each lag as it is computed.
    yin_running_sum: f32,
    /// Next lag of the analysis in progress, or `None` when idle. The
    /// difference function is computed [`LAGS_PER_STEP`] lags per decimated
    /// sample, so no single sample (or audio block) carries the whole
    /// analysis.
    next_tau: Option<usize>,
    /// Most lags computed in one `process_stereo` call (tests check the
    /// spread).
    #[cfg(test)]
    peak_lags_per_call: usize,
}

/// Lags of the difference function computed per decimated sample: enough to
/// finish an analysis within one hop.
const LAGS_PER_STEP: usize = TAU_MAX.div_ceil(HOP);

impl TunerKernel {
    /// Create a new tuner kernel at the given sample rate.
    pub fn new(sample_rate: f32) -> Self {
        let mut buffer = Vec::with_capacity(2 * RING);
        buffer.resize(2 * RING, 0.0);
        let mut yin_scratch = Vec::with_capacity(TAU_MAX);
        yin_scratch.resize(TAU_MAX, 0.0);

        Self {
            buffer,
            write_pos: 0,
            since_analysis: 0,
            frame_start: 0,
            decimation: decimation_for(sample_rate),
            decim_sum: 0.0,
            decim_count: 0,
            detected_hz: 0.0,
            cents_deviation: 0.0,
            sample_rate,
            yin_raw: alloc::vec![0.0; TAU_MAX],
            yin_scratch,
            yin_running_sum: 0.0,
            next_tau: None,
            #[cfg(test)]
            peak_lags_per_call: 0,
        }
    }

    /// Start an analysis over the newest `BUF_LEN` samples (no copy: see
    /// [`TunerKernel::buffer`]).
    fn begin_analysis(&mut self) {
        self.frame_start = (self.write_pos + RING - BUF_LEN) % RING;
        self.next_tau = Some(1);
        self.yin_scratch[0] = 1.0;
        self.yin_running_sum = 0.0;
    }

    /// Advance the analysis in progress by up to [`LAGS_PER_STEP`] lags; on
    /// the last lag, finish it and update the reading. Returns lags computed.
    fn step_analysis(&mut self, reference_hz: f32) -> usize {
        let Some(start) = self.next_tau else {
            return 0;
        };
        let end = (start + LAGS_PER_STEP).min(TAU_MAX);
        // Each lag is normalized as it is computed, and its raw value kept,
        // so finishing is a short scan with no further dot products.
        for tau in start..end {
            let frame = &self.buffer[self.frame_start..self.frame_start + BUF_LEN];
            let d = yin_difference(frame, tau);
            self.yin_raw[tau] = d;
            self.yin_scratch[tau] = yin_normalize(d, tau, &mut self.yin_running_sum);
        }
        if end < TAU_MAX {
            self.next_tau = Some(end);
        } else {
            self.next_tau = None;
            let rate = self.sample_rate / self.decimation as f32;
            let hz = yin_tau_estimate(&self.yin_scratch).and_then(|t| {
                let raw = [
                    self.yin_raw[t - 1],
                    self.yin_raw[t],
                    self.yin_raw[(t + 1).min(TAU_MAX - 1)],
                ];
                yin_interpolate(t, raw, rate)
            });
            if let Some(hz) = hz {
                self.detected_hz = hz;
                self.cents_deviation = hz_to_cents(hz, reference_hz);
            }
            // If no pitch detected, preserve previous values (hysteresis)
        }
        end - start
    }
}

impl DspKernel for TunerKernel {
    type Params = TunerParams;
    const LEVEL: Level = crate::calibration::TUNER;

    fn process_stereo(&mut self, left: f32, right: f32, params: &TunerParams) -> (f32, f32) {
        // ── Decimate (box average) into the analysis ring ──
        self.decim_sum += (left + right) * 0.5;
        self.decim_count += 1;
        if self.decim_count >= self.decimation {
            let x = self.decim_sum / self.decimation as f32;
            self.buffer[self.write_pos] = x;
            self.buffer[self.write_pos + RING] = x;
            self.decim_sum = 0.0;
            self.decim_count = 0;
            self.write_pos = (self.write_pos + 1) % RING;
            self.since_analysis += 1;

            // ── Every HOP new samples, start YIN on the last BUF_LEN, then
            //    spread its difference function over the following samples ──
            if self.since_analysis >= HOP {
                self.since_analysis = 0;
                // LAGS_PER_STEP finishes within a hop; finish any straggler.
                while self.next_tau.is_some() {
                    self.step_analysis(params.reference_hz);
                }
                self.begin_analysis();
            }
            let _lags = self.step_analysis(params.reference_hz);
            #[cfg(test)]
            {
                self.peak_lags_per_call = self.peak_lags_per_call.max(_lags);
            }
        }

        // ── Audio output ──
        let output_gain = fast_db_to_linear(params.output_db);
        let muted = params.mute >= 0.5;

        if muted {
            (0.0, 0.0)
        } else {
            Self::level_stereo(left * output_gain, right * output_gain)
        }
    }

    fn update_diagnostics(&self, params: &mut TunerParams) {
        params.detected_hz = self.detected_hz;
        params.cents = self.cents_deviation;
    }

    fn reset(&mut self) {
        for s in self.buffer.iter_mut() {
            *s = 0.0;
        }
        self.write_pos = 0;
        self.since_analysis = 0;
        self.next_tau = None;
        self.decim_sum = 0.0;
        self.decim_count = 0;
        self.detected_hz = 0.0;
        self.cents_deviation = 0.0;
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.decimation = decimation_for(sample_rate);
        self.reset();
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    extern crate alloc;
    use alloc::{vec, vec::Vec};
    use sonido_core::Effect;
    use sonido_core::kernel::Adapter;

    /// Generate a sine wave at the given frequency.
    fn sine_wave(freq_hz: f32, sample_rate: f32, n_samples: usize) -> Vec<f32> {
        (0..n_samples)
            .map(|i| libm::sinf(2.0 * core::f32::consts::PI * freq_hz * i as f32 / sample_rate))
            .collect()
    }

    /// Feed `seconds` of a sine through a fresh kernel; return (Hz, cents).
    fn tune(freq_hz: f32, sample_rate: f32, amplitude: f32, seconds: f32) -> (f32, f32) {
        let mut kernel = TunerKernel::new(sample_rate);
        let mut params = TunerParams::default();
        let n = (sample_rate * seconds) as usize;
        for s in sine_wave(freq_hz, sample_rate, n) {
            kernel.process_stereo(s * amplitude, s * amplitude, &params);
            kernel.update_diagnostics(&mut params);
        }
        (params.detected_hz, params.cents)
    }

    /// The low strings: guitar E2 and A2, bass E1 and five-string B0. Before
    /// decimation, YIN at the host rate bottomed out at 94 Hz and read the
    /// guitar's low E as 93.9 Hz, +27 cents sharp of F#2.
    #[test]
    fn tunes_guitar_and_bass_low_strings() {
        for (name, hz) in [
            ("E2", 82.41_f32),
            ("A2", 110.0),
            ("E1", 41.20),
            ("B0", 30.87),
        ] {
            for sr in [44_100.0_f32, 48_000.0, 96_000.0] {
                let (got, cents) = tune(hz, sr, 0.2, 0.5);
                let err_cents = 1200.0 * libm::log2f(got / hz);
                assert!(
                    err_cents.abs() < 3.0,
                    "{name} at {sr} Hz: read {got} Hz ({err_cents:+.1} cents off, reported {cents:+.1})"
                );
            }
        }
    }

    /// Real-time safety: the analysis is spread over the hop, so no call does
    /// more than LAGS_PER_STEP difference lags (the whole analysis in one
    /// block missed the Daisy's 32-sample deadline 29× over).
    #[test]
    fn analysis_work_is_spread_across_samples() {
        let sr = 48_000.0_f32;
        let mut kernel = TunerKernel::new(sr);
        let params = TunerParams::default();
        for s in sine_wave(110.0, sr, (sr * 0.5) as usize) {
            kernel.process_stereo(s * 0.2, s * 0.2, &params);
        }
        assert!(kernel.detected_hz > 0.0, "still detects");
        const { assert!(LAGS_PER_STEP <= 2) };
        assert_eq!(kernel.peak_lags_per_call, LAGS_PER_STEP);
    }

    /// Decimation must not cost accuracy at the top of the range.
    #[test]
    fn tunes_high_notes_within_a_cent() {
        for hz in [329.63_f32, 659.26, 1318.5] {
            let (got, _) = tune(hz, 48_000.0, 0.2, 0.5);
            let err_cents = 1200.0 * libm::log2f(got / hz);
            assert!(
                err_cents.abs() < 1.0,
                "{hz} Hz read as {got} ({err_cents:+.2} cents)"
            );
        }
    }

    #[test]
    fn detects_440_hz() {
        let sr = 48000.0_f32;
        let signal = sine_wave(440.0, sr, BUF_LEN * 4);

        let mut kernel = TunerKernel::new(sr);
        let mut params = TunerParams::default();

        for &s in &signal {
            kernel.process_stereo(s, s, &params);
            kernel.update_diagnostics(&mut params);
        }

        let detected = params.detected_hz;
        assert!(
            (detected - 440.0).abs() < 5.0,
            "Expected ~440 Hz, got {detected}"
        );

        let cents = params.cents;
        assert!(
            cents.abs() < 10.0,
            "Expected near 0 cents for 440 Hz, got {cents}"
        );
    }

    #[test]
    fn detects_442_hz_as_sharp() {
        let sr = 48000.0_f32;
        // 442 Hz is about +7.85 cents sharp of A4=440
        let signal = sine_wave(442.0, sr, BUF_LEN * 4);

        let mut kernel = TunerKernel::new(sr);
        let mut params = TunerParams::default();

        for &s in &signal {
            kernel.process_stereo(s, s, &params);
            kernel.update_diagnostics(&mut params);
        }

        assert!(
            params.cents > 0.0,
            "442 Hz should be sharp (positive cents), got {}",
            params.cents
        );
    }

    #[test]
    fn silence_produces_no_detection() {
        let sr = 48000.0_f32;
        let mut kernel = TunerKernel::new(sr);
        let mut params = TunerParams::default();

        // Process silence
        for _ in 0..BUF_LEN * 2 {
            kernel.process_stereo(0.0, 0.0, &params);
            kernel.update_diagnostics(&mut params);
        }

        // Should not detect any pitch (or report 0 Hz)
        let detected = params.detected_hz;
        // Either no pitch at all, or extremely low Hz — not a musical frequency
        assert!(
            detected < 20.0 || detected == 0.0,
            "Silence should not produce a musical pitch detection, got {detected} Hz"
        );
    }

    #[test]
    fn mute_silences_output() {
        let mut kernel = TunerKernel::new(48000.0);
        let params = TunerParams {
            mute: 1.0,
            ..Default::default()
        };

        let (l, r) = kernel.process_stereo(0.5, 0.5, &params);
        assert_eq!(l, 0.0, "Muted output must be exactly 0");
        assert_eq!(r, 0.0, "Muted output must be exactly 0");
    }

    #[test]
    fn unmuted_passes_audio_through() {
        let mut kernel = TunerKernel::new(48000.0);
        let params = TunerParams {
            mute: 0.0,
            output_db: 0.0,
            ..Default::default()
        };

        let (l, r) = kernel.process_stereo(0.5, -0.3, &params);
        assert!((l - 0.5).abs() < 1e-5, "Expected passthrough, got {l}");
        assert!((r - (-0.3)).abs() < 1e-5, "Expected passthrough, got {r}");
    }

    #[test]
    fn finite_output_always() {
        let sr = 48000.0_f32;
        let signal = sine_wave(330.0, sr, BUF_LEN * 4);
        let mut kernel = TunerKernel::new(sr);
        let mut params = TunerParams::default();

        for &s in &signal {
            let (l, r) = kernel.process_stereo(s, s, &params);
            kernel.update_diagnostics(&mut params);
            assert!(l.is_finite(), "L output is not finite");
            assert!(r.is_finite(), "R output is not finite");
            assert!(params.detected_hz.is_finite(), "detected_hz is not finite");
            assert!(params.cents.is_finite(), "cents is not finite");
        }
    }

    #[test]
    fn params_descriptor_count() {
        assert_eq!(TunerParams::COUNT, 5);
        for i in 0..TunerParams::COUNT {
            assert!(
                TunerParams::descriptor(i).is_some(),
                "Missing descriptor at {i}"
            );
        }
        assert!(TunerParams::descriptor(TunerParams::COUNT).is_none());
    }

    #[test]
    fn params_ids_correct() {
        assert_eq!(TunerParams::descriptor(0).unwrap().id, ParamId(2300));
        assert_eq!(TunerParams::descriptor(1).unwrap().id, ParamId(2301));
        assert_eq!(TunerParams::descriptor(2).unwrap().id, ParamId(2302));
        assert_eq!(TunerParams::descriptor(3).unwrap().id, ParamId(2303));
        assert_eq!(TunerParams::descriptor(4).unwrap().id, ParamId(2304));
    }

    #[test]
    fn read_only_params_have_correct_flags() {
        let d3 = TunerParams::descriptor(3).unwrap();
        let d4 = TunerParams::descriptor(4).unwrap();
        assert!(
            d3.flags.contains(ParamFlags::READ_ONLY),
            "detected_hz must be READ_ONLY"
        );
        assert!(
            d3.flags.contains(ParamFlags::HIDDEN),
            "detected_hz must be HIDDEN"
        );
        assert!(
            d4.flags.contains(ParamFlags::READ_ONLY),
            "cents must be READ_ONLY"
        );
        assert!(
            d4.flags.contains(ParamFlags::HIDDEN),
            "cents must be HIDDEN"
        );
    }

    #[test]
    fn adapter_wraps_as_effect() {
        let mut adapter = Adapter::new(TunerKernel::new(48000.0), 48000.0);
        adapter.reset();
        let out = adapter.process(0.3);
        assert!(out.is_finite(), "Adapter output must be finite, got {out}");
    }

    #[test]
    fn yin_detects_known_frequency() {
        let sr = 48000.0_f32;
        let freq = 220.0_f32; // A3
        let buf: Vec<f32> = (0..BUF_LEN)
            .map(|i| libm::sinf(2.0 * core::f32::consts::PI * freq * i as f32 / sr))
            .collect();
        let mut scratch = vec![0.0f32; TAU_MAX];
        let detected = yin_detect(&buf, sr, &mut scratch);
        assert!(detected.is_some(), "YIN should detect 220 Hz");
        let hz = detected.unwrap();
        assert!((hz - freq).abs() < 5.0, "Expected ~220 Hz, got {hz}");
    }
}
