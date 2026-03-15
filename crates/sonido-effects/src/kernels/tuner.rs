//! Chromatic tuner kernel — YIN pitch detection with READ_ONLY diagnostic params.
//!
//! `TunerKernel` accumulates input into a 1024-sample buffer, runs the YIN
//! pitch detection algorithm when the buffer fills, and exposes the detected
//! frequency and cents deviation as READ_ONLY parameters. Audio is passed
//! through with zero algorithmic latency. An optional mute mode silences the
//! output for silent tuning.
//!
//! Parameters are received via `&TunerParams` each sample. Deployed via
//! [`KernelAdapter`](sonido_core::KernelAdapter) for desktop/plugin, or called
//! directly on embedded targets.
//!
//! # Signal Flow
//!
//! ```text
//! Input → [accumulate into 1024-sample window]
//!       → passthrough (or silence if mute=1)
//!       → output gain
//!
//! When buffer fills:
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
//! let adapter = KernelAdapter::new(TunerKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//! ```

extern crate alloc;

use alloc::vec::Vec;
use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::{ParamDescriptor, ParamFlags, ParamId, ParamUnit, fast_db_to_linear};

/// YIN threshold: lower = more accurate but more misses; 0.15 is a good balance.
const YIN_THRESHOLD: f32 = 0.15;

/// Accumulation buffer length. 1024 samples gives ~21 ms at 48 kHz — enough for 48 Hz (low B).
const BUF_LEN: usize = 1024;

/// Maximum lag to search: limits detection to frequencies above ~80 Hz (BUF_LEN/2 = 512 → 93 Hz).
const TAU_MAX: usize = BUF_LEN / 2;

/// Minimum lag to search: limits detection to frequencies below ~20 kHz.
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
/// | 2 | `output_db` | dB | −60–+6 | 0.0 |
/// | 3 | `detected_hz` | Hz | 0–5000 | 0.0 (READ_ONLY) |
/// | 4 | `cents` | cents | −50–50 | 0.0 (READ_ONLY) |
#[derive(Debug, Clone, Copy)]
pub struct TunerParams {
    /// Reference tuning frequency for A4 in Hz. Range: 415–465 Hz. Default: 440.0.
    pub reference_hz: f32,
    /// Mute toggle: 0.0 = pass audio through, 1.0 = silence output (for silent tuning).
    pub mute: f32,
    /// Output level in decibels. Range: −60.0 to +6.0 dB.
    pub output_db: f32,
    /// Detected fundamental frequency in Hz.
    ///
    /// READ_ONLY — written by the kernel. Updated approximately every 21 ms
    /// (once per 1024-sample analysis window). Zero if no pitch detected.
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

impl KernelParams for TunerParams {
    const COUNT: usize = 5;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(
                ParamDescriptor::custom("Reference", "Ref", 415.0, 465.0, 440.0)
                    .with_unit(ParamUnit::Hertz)
                    .with_id(ParamId(2300), "tuner_reference"),
            ),
            1 => Some(
                ParamDescriptor::custom("Mute", "Mute", 0.0, 1.0, 0.0)
                    .with_unit(ParamUnit::None)
                    .with_step(1.0)
                    .with_id(ParamId(2301), "tuner_mute")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(&["Off", "On"]),
            ),
            2 => Some(
                sonido_core::gain::output_param_descriptor().with_id(ParamId(2302), "tuner_output"),
            ),
            3 => Some(
                ParamDescriptor::custom("Detected Hz", "Det Hz", 0.0, 5000.0, 0.0)
                    .with_unit(ParamUnit::Hertz)
                    .with_id(ParamId(2303), "tuner_detected_hz")
                    .with_flags(ParamFlags::READ_ONLY.union(ParamFlags::HIDDEN)),
            ),
            4 => Some(
                ParamDescriptor::custom("Cents", "Cents", -50.0, 50.0, 0.0)
                    .with_unit(ParamUnit::None)
                    .with_id(ParamId(2304), "tuner_cents")
                    .with_flags(ParamFlags::READ_ONLY.union(ParamFlags::HIDDEN)),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::None, // reference_hz — snap
            1 => SmoothingStyle::None, // mute — stepped, snap
            2 => SmoothingStyle::Fast, // output_db — 5 ms
            3 => SmoothingStyle::None, // detected_hz — READ_ONLY diagnostic
            4 => SmoothingStyle::None, // cents — READ_ONLY diagnostic
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.reference_hz,
            1 => self.mute,
            2 => self.output_db,
            3 => self.detected_hz,
            4 => self.cents,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.reference_hz = value,
            1 => self.mute = value,
            2 => self.output_db = value,
            3 => self.detected_hz = value,
            4 => self.cents = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  YIN helper functions
// ═══════════════════════════════════════════════════════════════════════════

/// Run the YIN algorithm on `buf` and return detected frequency in Hz, or `None`.
///
/// This is a pure function — no state except the input buffer. The caller owns
/// `scratch`, which must have length `TAU_MAX`.
fn yin_detect(buf: &[f32], sample_rate: f32, scratch: &mut [f32]) -> Option<f32> {
    debug_assert_eq!(buf.len(), BUF_LEN);
    debug_assert_eq!(scratch.len(), TAU_MAX);

    let w = BUF_LEN - TAU_MAX; // integration window length

    // ── Step 1 & 2: Difference function + CMNDF ──────────────────────────
    scratch[0] = 1.0;
    let mut running_sum = 0.0f32;

    for tau in 1..TAU_MAX {
        let mut d = 0.0f32;
        for j in 0..w {
            let diff = buf[j] - buf[j + tau];
            d += diff * diff;
        }
        running_sum += d;
        // CMNDF: d'(τ) = d(τ) / ((1/τ) * running_sum)
        scratch[tau] = if running_sum > 1e-10 {
            d * (tau as f32) / running_sum
        } else {
            1.0
        };
    }

    // ── Step 3: Find first dip below threshold ────────────────────────────
    let mut tau_min = None;
    for tau in TAU_MIN..TAU_MAX - 1 {
        if scratch[tau] < YIN_THRESHOLD {
            // Ensure it's a local minimum (not just crossing threshold on the way down)
            if scratch[tau] <= scratch[tau + 1] {
                tau_min = Some(tau);
                break;
            }
        }
    }

    // Fallback: if no dip found, find global minimum in valid range
    let tau_est = if let Some(t) = tau_min {
        t
    } else {
        let mut best_tau = TAU_MIN;
        let mut best_val = scratch[TAU_MIN];
        for tau in TAU_MIN + 1..TAU_MAX {
            if scratch[tau] < best_val {
                best_val = scratch[tau];
                best_tau = tau;
            }
        }
        // Only report if the minimum is reasonably small
        if best_val > 0.35 {
            return None;
        }
        best_tau
    };

    // ── Step 4: Parabolic interpolation ──────────────────────────────────
    let tau_f = if tau_est > 0 && tau_est < TAU_MAX - 1 {
        let x0 = scratch[tau_est - 1];
        let x1 = scratch[tau_est];
        let x2 = scratch[tau_est + 1];
        let denom = x0 - 2.0 * x1 + x2;
        if libm::fabsf(denom) > 1e-10 {
            tau_est as f32 - 0.5 * (x2 - x0) / denom
        } else {
            tau_est as f32
        }
    } else {
        tau_est as f32
    };

    if tau_f < 1.0 {
        return None;
    }

    Some(sample_rate / tau_f)
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
/// - A 1024-sample accumulation buffer for pitch analysis
/// - A YIN scratch buffer (reused each analysis frame)
/// - Detected Hz and cents deviation (updated after each analysis frame)
///
/// No `SmoothedParam`, no atomics, no platform awareness.
///
/// # Diagnostics
///
/// `detected_hz` and `cents_deviation` are internal state; they are pushed
/// back into params via `update_diagnostics()` which is called from
/// `process_stereo()` after each analysis frame.
pub struct TunerKernel {
    /// Accumulation buffer for pitch analysis.
    buffer: Vec<f32>,
    /// Write position in the accumulation buffer.
    write_pos: usize,
    /// Last detected fundamental frequency in Hz. Zero if no pitch detected.
    detected_hz: f32,
    /// Last cents deviation from nearest 12-TET note.
    cents_deviation: f32,
    /// Audio sample rate in Hz.
    sample_rate: f32,
    /// Scratch buffer for YIN computation (length = TAU_MAX).
    yin_scratch: Vec<f32>,
}

impl TunerKernel {
    /// Create a new tuner kernel at the given sample rate.
    pub fn new(sample_rate: f32) -> Self {
        let mut buffer = Vec::with_capacity(BUF_LEN);
        buffer.resize(BUF_LEN, 0.0);
        let mut yin_scratch = Vec::with_capacity(TAU_MAX);
        yin_scratch.resize(TAU_MAX, 0.0);

        Self {
            buffer,
            write_pos: 0,
            detected_hz: 0.0,
            cents_deviation: 0.0,
            sample_rate,
            yin_scratch,
        }
    }

    /// Run YIN on the current buffer and update internal diagnostic state.
    fn run_analysis(&mut self, reference_hz: f32) {
        if let Some(hz) = yin_detect(&self.buffer, self.sample_rate, &mut self.yin_scratch) {
            self.detected_hz = hz;
            self.cents_deviation = hz_to_cents(hz, reference_hz);
        }
        // If no pitch detected, preserve previous values (hysteresis)
    }
}

impl DspKernel for TunerKernel {
    type Params = TunerParams;

    fn process_stereo(&mut self, left: f32, right: f32, params: &TunerParams) -> (f32, f32) {
        // ── Accumulate mono sum into analysis buffer ──
        let mono = (left + right) * 0.5;
        self.buffer[self.write_pos] = mono;
        self.write_pos += 1;

        // ── When buffer is full, run YIN ──
        if self.write_pos >= BUF_LEN {
            self.write_pos = 0;
            self.run_analysis(params.reference_hz);
        }

        // ── Audio output ──
        let output_gain = fast_db_to_linear(params.output_db);
        let muted = params.mute >= 0.5;

        if muted {
            (0.0, 0.0)
        } else {
            (left * output_gain, right * output_gain)
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
        self.detected_hz = 0.0;
        self.cents_deviation = 0.0;
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
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
    use sonido_core::kernel::KernelAdapter;

    /// Generate a sine wave at the given frequency.
    fn sine_wave(freq_hz: f32, sample_rate: f32, n_samples: usize) -> Vec<f32> {
        (0..n_samples)
            .map(|i| libm::sinf(2.0 * core::f32::consts::PI * freq_hz * i as f32 / sample_rate))
            .collect()
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
        let mut adapter = KernelAdapter::new(TunerKernel::new(48000.0), 48000.0);
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
