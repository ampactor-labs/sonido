//! Compressor kernel — dynamics processing with soft-knee, sidechain HPF, and lookahead.
//!
//! `CompressorKernel` owns DSP state (envelope followers, sidechain HPF,
//! coefficient caches, gain reduction memory). Parameters are received via
//! `&CompressorParams` each sample. Deployed via
//! [`KernelAdapter`](sonido_core::KernelAdapter) for desktop/plugin, or called
//! directly on embedded targets.
//!
//! # Signal Flow
//!
//! ```text
//! Input → SC HPF (detect only) → Dual Envelope → Gain Computer → Gain Reduction
//!           (mid signal L+R/2)     (fast + slow)       ↓
//!                                               Makeup (auto/manual)
//!                                                       ↓
//!                                               Wet/Dry Mix → Soft Limit → Output
//! ```
//!
//! # Program-Dependent Release
//!
//! Two parallel envelope followers run on the sidechain mid signal:
//! - **Fast** (50 ms fixed release): catches new transients quickly.
//! - **Slow** (user-set release): provides smooth sustained compression.
//!
//! The actual envelope is `max(fast, slow)`, so sustained material uses the
//! slow time constant while new transients are caught by the fast follower
//! even during a slow release cycle.
//!
//! Reference: Giannoulis, Massberg & Reiss, "Digital Dynamic Range Compressor
//! Design — A Tutorial and Analysis", JAES 2012.
//!
//! # Soft-Knee Gain Computer
//!
//! Given overshoot `x = input_dB − threshold_dB` and knee half-width `w = knee_dB / 2`:
//!
//! - `x ≤ −w`:  gain reduction = 0 (below knee)
//! - `x > +w`:  gain reduction = −x × (1 − 1/ratio) (above knee, linear)
//! - `|x| ≤ w`: gain reduction = −((x + w) / knee_dB)² × x × (1 − 1/ratio) (quadratic interpolation)
//!
//! # Auto Makeup
//!
//! When enabled: `makeup_dB = −threshold × (1 − 1/ratio) × 0.5`.
//! The 0.5 factor accounts for the fact that not all signal exceeds threshold.
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! let adapter = KernelAdapter::new(CompressorKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing, ADCs are hardware-filtered)
//! let mut kernel = CompressorKernel::new(48000.0);
//! let params = CompressorParams::from_knobs(thresh, ratio, attack, release, makeup, mix);
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::{
    Biquad, DetectionMode, EnvelopeFollower, ParamDescriptor, ParamFlags, ParamId, ParamScale,
    ParamUnit, fast_db_to_linear, fast_linear_to_db, highpass_coefficients, math::soft_limit,
    wet_dry_mix_stereo,
};

// ── Constants ────────────────────────────────────────────────────────────────

/// Fixed fast-release time for the program-dependent dual-envelope design (ms).
const FAST_RELEASE_MS: f32 = 50.0;

/// Butterworth Q for the sidechain HPF (1/√2 ≈ 0.707).
const SC_HPF_Q: f32 = 0.707;

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`CompressorKernel`].
///
/// All values are in **user-facing units** — the same units shown in GUIs and
/// stored in presets. The kernel converts internally as needed (dB → linear,
/// ms → samples, etc.).
///
/// ## Parameter Table
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `threshold_db` | dB | −60–0 | −18.0 |
/// | 1 | `ratio` | ratio | 1.0–20.0 | 4.0 |
/// | 2 | `attack_ms` | ms | 0.1–100.0 | 10.0 |
/// | 3 | `release_ms` | ms | 10–1000 | 100.0 |
/// | 4 | `makeup_db` | dB | 0–24 | 0.0 |
/// | 5 | `knee_db` | dB | 0–12 | 6.0 |
/// | 6 | `detection` | index | 0–1 | 0 (Peak) |
/// | 7 | `sidechain_freq_hz` | Hz | 20–500 | 80.0 |
/// | 8 | `auto_makeup` | index | 0–1 | 0 (Off) |
/// | 9 | `output_db` | dB | −20–20 | 0.0 |
/// | 10 | `mix_pct` | % | 0–100 | 100.0 |
///
/// ## Smoothing Notes
///
/// - `threshold_db`, `ratio`, `knee_db`: Standard (10 ms) — avoid zipper on automation.
/// - `attack_ms`, `release_ms`: Standard (10 ms) — timing params, smooth transitions.
/// - `makeup_db`, `output_db`: Standard (10 ms) — level faders.
/// - `sidechain_freq_hz`: Slow (20 ms) — filter coefficient recalc, avoid zipper.
/// - `detection`, `auto_makeup`: None — stepped/discrete, snap immediately.
/// - `mix_pct`: Standard (10 ms) — wet/dry blend.
#[derive(Debug, Clone, Copy)]
pub struct CompressorParams {
    /// Compression threshold in decibels.
    ///
    /// Range: −60.0–0.0 dB (default −18.0). The compressor begins attenuating
    /// signals above this level.
    pub threshold_db: f32,

    /// Compression ratio (input change : output change).
    ///
    /// Range: 1.0–20.0 (default 4.0). At 4.0, a 4 dB overshoot produces 1 dB
    /// of output change. At 20.0, the compressor approaches limiting.
    pub ratio: f32,

    /// Envelope attack time in milliseconds.
    ///
    /// Range: 0.1–100.0 ms (default 10.0). Controls how quickly gain reduction
    /// engages when the signal exceeds the threshold. Shorter times sound
    /// "squashed"; longer times let transients through.
    pub attack_ms: f32,

    /// Slow envelope release time in milliseconds.
    ///
    /// Range: 10.0–1000.0 ms (default 100.0). Controls the slow envelope
    /// follower's release. The fast follower always uses 50 ms.
    pub release_ms: f32,

    /// Manual makeup gain in decibels.
    ///
    /// Range: 0.0–24.0 dB (default 0.0). Applied after gain reduction.
    /// Ignored when `auto_makeup` is non-zero.
    pub makeup_db: f32,

    /// Soft-knee width in decibels.
    ///
    /// Range: 0.0–12.0 dB (default 6.0). 0 = hard knee (abrupt onset).
    /// Larger values gradually increase compression near the threshold for
    /// a more transparent sound.
    pub knee_db: f32,

    /// Detection mode: 0.0 = Peak, 1.0 = RMS.
    ///
    /// Peak tracks instantaneous amplitude; RMS averages signal power over
    /// a short window for a smoother, less reactive response. Stepped: only
    /// 0.0 or 1.0 are meaningful.
    pub detection: f32,

    /// Sidechain highpass filter cutoff frequency in Hz.
    ///
    /// Range: 20.0–500.0 Hz (default 80.0). Removes low-frequency content
    /// from the detection path to prevent kick drums or bass from pumping
    /// the compressor. Set near 20 Hz to effectively disable.
    pub sidechain_freq_hz: f32,

    /// Auto makeup gain: 0.0 = Off, 1.0 = On.
    ///
    /// When enabled, makeup gain is computed from threshold and ratio:
    /// `makeup_dB = −threshold × (1 − 1/ratio) × 0.5`. The manual `makeup_db`
    /// field is ignored while auto is active. Stepped: only 0.0 or 1.0.
    pub auto_makeup: f32,

    /// Output level in decibels.
    ///
    /// Range: −20.0–20.0 dB (default 0.0). Final output trim applied after
    /// makeup gain and wet/dry mix.
    pub output_db: f32,

    /// Wet/dry parallel compression blend in percent.
    ///
    /// Range: 0.0–100.0 % (default 100.0). At 0% the signal passes dry
    /// (no compression). At 100% the signal is fully compressed. Intermediate
    /// values blend compressed and dry ("New York" parallel compression).
    pub mix_pct: f32,
}

impl Default for CompressorParams {
    fn default() -> Self {
        Self {
            threshold_db: -18.0,
            ratio: 4.0,
            attack_ms: 10.0,
            release_ms: 100.0,
            makeup_db: 0.0,
            knee_db: 6.0,
            detection: 0.0,
            sidechain_freq_hz: 80.0,
            auto_makeup: 0.0,
            output_db: 0.0,
            mix_pct: 100.0,
        }
    }
}

impl CompressorParams {
    /// Build params directly from hardware knob readings (0.0–1.0 normalized).
    ///
    /// Convenience for embedded targets where ADC values map linearly to
    /// parameter ranges. Arguments correspond to the eleven parameter indices
    /// in order.
    ///
    /// | Argument | Parameter | Mapping |
    /// |----------|-----------|---------|
    /// | `thresh` | `threshold_db` | 0–1 → −60–0 dB |
    /// | `ratio` | `ratio` | 0–1 → 1.0–20.0 |
    /// | `attack` | `attack_ms` | 0–1 → 0.1–100.0 ms |
    /// | `release` | `release_ms` | 0–1 → 10–1000 ms |
    /// | `makeup` | `makeup_db` | 0–1 → 0–24 dB |
    /// | `knee` | `knee_db` | 0–1 → 0–12 dB |
    /// | `detect` | `detection` | 0–1 → 0 or 1 (Peak/RMS, snapped) |
    /// | `sc_freq` | `sidechain_freq_hz` | 0–1 → 20–500 Hz |
    /// | `auto_mu` | `auto_makeup` | 0–1 → 0 or 1 (Off/On, snapped) |
    /// | `output` | `output_db` | 0–1 → −20–20 dB |
    /// | `mix` | `mix_pct` | 0–1 → 0–100 % |
    #[allow(clippy::too_many_arguments)]
    pub fn from_knobs(
        thresh: f32,
        ratio: f32,
        attack: f32,
        release: f32,
        makeup: f32,
        knee: f32,
        detect: f32,
        sc_freq: f32,
        auto_mu: f32,
        output: f32,
        mix: f32,
    ) -> Self {
        Self {
            threshold_db: thresh * 60.0 - 60.0,        // 0–1 → −60–0 dB
            ratio: ratio * 19.0 + 1.0,                 // 0–1 → 1.0–20.0
            attack_ms: attack * 99.9 + 0.1,            // 0–1 → 0.1–100.0 ms
            release_ms: release * 990.0 + 10.0,        // 0–1 → 10–1000 ms
            makeup_db: makeup * 24.0,                  // 0–1 → 0–24 dB
            knee_db: knee * 12.0,                      // 0–1 → 0–12 dB
            detection: libm::floorf(detect * 1.99),    // 0 or 1
            sidechain_freq_hz: sc_freq * 480.0 + 20.0, // 0–1 → 20–500 Hz
            auto_makeup: libm::floorf(auto_mu * 1.99), // 0 or 1
            output_db: output * 40.0 - 20.0,           // 0–1 → −20–20 dB
            mix_pct: mix * 100.0,                      // 0–1 → 0–100 %
        }
    }
}

impl KernelParams for CompressorParams {
    const COUNT: usize = 11;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            // ── [0] Threshold ────────────────────────────────────────────────
            // ParamId(300), "comp_thresh" — matches classic compressor.rs [0]
            0 => Some(
                ParamDescriptor::gain_db("Threshold", "Thresh", -60.0, 0.0, -18.0)
                    .with_id(ParamId(300), "comp_thresh"),
            ),
            // ── [1] Ratio ────────────────────────────────────────────────────
            // ParamId(301), "comp_ratio" — matches classic compressor.rs [1]
            1 => Some(
                ParamDescriptor::custom("Ratio", "Ratio", 1.0, 20.0, 4.0)
                    .with_unit(ParamUnit::Ratio)
                    .with_step(0.1)
                    .with_id(ParamId(301), "comp_ratio"),
            ),
            // ── [2] Attack ───────────────────────────────────────────────────
            // ParamId(302), "comp_attack" — matches classic compressor.rs [2]
            2 => Some(
                ParamDescriptor::custom("Attack", "Attack", 0.1, 100.0, 10.0)
                    .with_unit(ParamUnit::Milliseconds)
                    .with_step(0.1)
                    .with_id(ParamId(302), "comp_attack"),
            ),
            // ── [3] Release ──────────────────────────────────────────────────
            // ParamId(303), "comp_release" — matches classic compressor.rs [3]
            3 => Some(
                ParamDescriptor::time_ms("Release", "Release", 10.0, 1000.0, 100.0)
                    .with_id(ParamId(303), "comp_release"),
            ),
            // ── [4] Makeup Gain ──────────────────────────────────────────────
            // ParamId(304), "comp_makeup" — matches classic compressor.rs [4]
            4 => Some(
                ParamDescriptor::gain_db("Makeup Gain", "Makeup", 0.0, 24.0, 0.0)
                    .with_id(ParamId(304), "comp_makeup"),
            ),
            // ── [5] Knee ─────────────────────────────────────────────────────
            // ParamId(305), "comp_knee" — matches classic compressor.rs [5]
            5 => Some(
                ParamDescriptor::gain_db("Knee", "Knee", 0.0, 12.0, 6.0)
                    .with_id(ParamId(305), "comp_knee"),
            ),
            // ── [6] Detection mode ───────────────────────────────────────────
            // ParamId(306), "comp_detect" — matches classic compressor.rs [6]
            6 => Some(
                ParamDescriptor::custom("Detection", "Detect", 0.0, 1.0, 0.0)
                    .with_step(1.0)
                    .with_id(ParamId(306), "comp_detect")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(&["Peak", "RMS"]),
            ),
            // ── [7] Sidechain HPF frequency ──────────────────────────────────
            // ParamId(307), "comp_sc_freq" — matches classic compressor.rs [7]
            7 => Some(
                ParamDescriptor::custom("SC HPF Freq", "SC HPF", 20.0, 500.0, 80.0)
                    .with_unit(ParamUnit::Hertz)
                    .with_step(1.0)
                    .with_id(ParamId(307), "comp_sc_freq")
                    .with_scale(ParamScale::Logarithmic),
            ),
            // ── [8] Auto makeup ──────────────────────────────────────────────
            // ParamId(308), "comp_auto_makeup" — matches classic compressor.rs [8]
            8 => Some(
                ParamDescriptor::custom("Auto Makeup", "AutoMU", 0.0, 1.0, 0.0)
                    .with_step(1.0)
                    .with_id(ParamId(308), "comp_auto_makeup")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(&["Off", "On"]),
            ),
            // ── [9] Output level ─────────────────────────────────────────────
            // ParamId(309), "comp_output" — matches classic compressor.rs [9]
            9 => Some(
                sonido_core::gain::output_param_descriptor().with_id(ParamId(309), "comp_output"),
            ),
            // ── [10] Mix ─────────────────────────────────────────────────────
            // ParamId(310), "comp_mix" — matches classic compressor.rs [10]
            10 => Some(
                ParamDescriptor {
                    default: 100.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(310), "comp_mix"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Standard, // threshold — avoid zipper on automation
            1 => SmoothingStyle::Standard, // ratio — gain computer parameter
            2 => SmoothingStyle::Standard, // attack — timing param, smooth transitions
            3 => SmoothingStyle::Standard, // release — timing param, smooth transitions
            4 => SmoothingStyle::Standard, // makeup — level fader
            5 => SmoothingStyle::Standard, // knee — gain computer width
            6 => SmoothingStyle::None,     // detection — stepped/discrete, snap immediately
            7 => SmoothingStyle::Slow,     // sidechain freq — filter coeff, avoid zipper
            8 => SmoothingStyle::None,     // auto_makeup — stepped/discrete, snap immediately
            9 => SmoothingStyle::Standard, // output level — level fader
            10 => SmoothingStyle::Standard, // mix — wet/dry blend
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.threshold_db,
            1 => self.ratio,
            2 => self.attack_ms,
            3 => self.release_ms,
            4 => self.makeup_db,
            5 => self.knee_db,
            6 => self.detection,
            7 => self.sidechain_freq_hz,
            8 => self.auto_makeup,
            9 => self.output_db,
            10 => self.mix_pct,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.threshold_db = value,
            1 => self.ratio = value,
            2 => self.attack_ms = value,
            3 => self.release_ms = value,
            4 => self.makeup_db = value,
            5 => self.knee_db = value,
            6 => self.detection = value,
            7 => self.sidechain_freq_hz = value,
            8 => self.auto_makeup = value,
            9 => self.output_db = value,
            10 => self.mix_pct = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP dynamics compressor kernel.
///
/// Contains ONLY the mutable state required for audio processing. No `SmoothedParam`,
/// no atomics, no platform awareness.
///
/// # DSP State
///
/// - `envelope_follower` — slow envelope (user-set release time).
/// - `fast_envelope` — fast envelope (fixed 50 ms release for program-dependent release).
/// - `sidechain_hpf` — biquad HPF on the detection path.
/// - `last_gain_reduction_db` — last computed gain reduction (always ≤ 0.0 dB).
/// - Coefficient caches (`last_*`) — avoid re-running `expf` / HPF recalc every sample.
///
/// # Program-Dependent Release
///
/// `max(slow_envelope, fast_envelope)` drives the gain computer. When sustained
/// material holds the slow follower high, new transients still trigger the fast
/// follower, preventing transients from passing uncompressed during the slow
/// release cycle.
///
/// Reference: Giannoulis, Massberg & Reiss, "Digital Dynamic Range Compressor
/// Design — A Tutorial and Analysis", JAES 2012.
pub struct CompressorKernel {
    /// Sample rate — needed for HPF and envelope coefficient recalculation.
    sample_rate: f32,

    /// Slow envelope follower (user-configured release time).
    envelope_follower: EnvelopeFollower,

    /// Fast envelope follower (fixed 50 ms release for program-dependent release).
    fast_envelope: EnvelopeFollower,

    /// Sidechain highpass filter (detection path only).
    ///
    /// Applied to the linked-stereo mid signal `(L + R) / 2` before both
    /// envelope followers. Coefficients recomputed when `sidechain_freq_hz` changes.
    sidechain_hpf: Biquad,

    /// Last computed gain reduction in dB (always ≤ 0.0).
    ///
    /// Exposed via the adapter as a metering value.
    last_gain_reduction_db: f32,

    /// Cached `sidechain_freq_hz` — invalidates HPF coefficients when changed.
    last_sidechain_freq_hz: f32,

    /// Cached `attack_ms` — invalidates envelope attack coefficients when changed.
    last_attack_ms: f32,

    /// Cached `release_ms` — invalidates slow envelope release coefficient when changed.
    last_release_ms: f32,

    /// Cached `detection` index — invalidates detection mode when changed.
    last_detection: f32,
}

impl CompressorKernel {
    /// Create a new compressor kernel initialized with default parameters.
    ///
    /// The sidechain HPF is set to 80 Hz (default). Both envelope followers are
    /// configured for peak detection with 10 ms attack. The fast follower uses a
    /// fixed 50 ms release; the slow follower uses 100 ms (default).
    pub fn new(sample_rate: f32) -> Self {
        let defaults = CompressorParams::default();

        // Initialize sidechain HPF at default 80 Hz
        let mut sidechain_hpf = Biquad::new();
        let (b0, b1, b2, a0, a1, a2) =
            highpass_coefficients(defaults.sidechain_freq_hz, SC_HPF_Q, sample_rate);
        sidechain_hpf.set_coefficients(b0, b1, b2, a0, a1, a2);

        // Slow envelope: user-controlled release
        let mut envelope_follower = EnvelopeFollower::new(sample_rate);
        envelope_follower.set_attack_ms(defaults.attack_ms);
        envelope_follower.set_release_ms(defaults.release_ms);

        // Fast envelope: fixed 50 ms release for program-dependent behavior
        let mut fast_envelope = EnvelopeFollower::new(sample_rate);
        fast_envelope.set_attack_ms(defaults.attack_ms);
        fast_envelope.set_release_ms(FAST_RELEASE_MS);

        Self {
            sample_rate,
            envelope_follower,
            fast_envelope,
            sidechain_hpf,
            last_gain_reduction_db: 0.0,
            last_sidechain_freq_hz: defaults.sidechain_freq_hz,
            last_attack_ms: defaults.attack_ms,
            last_release_ms: defaults.release_ms,
            last_detection: defaults.detection,
        }
    }

    /// Update cached state when relevant parameters change.
    ///
    /// Comparisons use small epsilon thresholds to avoid redundant recalculation
    /// while smoothed parameters are advancing. Only recomputes what actually changed:
    /// - HPF coefficients when `sidechain_freq_hz` changes.
    /// - Attack coefficients for both followers when `attack_ms` changes.
    /// - Slow release coefficient when `release_ms` changes.
    /// - Detection mode on both followers when `detection` changes.
    #[inline]
    fn update_caches(&mut self, params: &CompressorParams) {
        if (params.sidechain_freq_hz - self.last_sidechain_freq_hz).abs() > 0.5 {
            let freq = params.sidechain_freq_hz.clamp(20.0, 500.0);
            let (b0, b1, b2, a0, a1, a2) = highpass_coefficients(freq, SC_HPF_Q, self.sample_rate);
            self.sidechain_hpf.set_coefficients(b0, b1, b2, a0, a1, a2);
            self.last_sidechain_freq_hz = params.sidechain_freq_hz;
        }
        if (params.attack_ms - self.last_attack_ms).abs() > 0.001 {
            let attack = params.attack_ms.clamp(0.1, 100.0);
            self.envelope_follower.set_attack_ms(attack);
            self.fast_envelope.set_attack_ms(attack);
            self.last_attack_ms = params.attack_ms;
        }
        if (params.release_ms - self.last_release_ms).abs() > 0.01 {
            let release = params.release_ms.clamp(10.0, 1000.0);
            self.envelope_follower.set_release_ms(release);
            // fast_envelope keeps its fixed FAST_RELEASE_MS
            self.last_release_ms = params.release_ms;
        }
        if (params.detection - self.last_detection).abs() > 0.5 {
            let mode = if params.detection < 0.5 {
                DetectionMode::Peak
            } else {
                DetectionMode::Rms
            };
            self.envelope_follower.set_detection_mode(mode);
            self.fast_envelope.set_detection_mode(mode);
            self.last_detection = params.detection;
        }
    }

    /// Run dual-envelope detection on a mono detection signal.
    ///
    /// Returns the program-dependent envelope: `max(fast, slow)`.
    ///
    /// The slow follower provides smooth, sustained compression. The fast follower
    /// (50 ms release) catches new transients that arrive during the slow release
    /// cycle, preventing them from passing uncompressed.
    ///
    /// Reference: Giannoulis et al., JAES 2012, Section III.
    #[inline]
    fn dual_envelope(&mut self, detection: f32) -> f32 {
        let slow = self.envelope_follower.process(detection);
        let fast = self.fast_envelope.process(detection);
        if fast > slow { fast } else { slow }
    }

    /// Compute gain reduction from the soft-knee gain computer.
    ///
    /// Given `input_db`, `threshold_db`, `ratio`, and `knee_db`:
    ///
    /// Let `overshoot = input_db − threshold_db` and `w = knee_db / 2`:
    ///
    /// - `overshoot ≤ −w`:  no reduction (below knee)
    /// - `overshoot > +w`:  `gain_reduction = −overshoot × (1 − 1/ratio)` (linear region)
    /// - `|overshoot| ≤ w`: `gain_reduction = −((overshoot + w) / knee_db)² × overshoot × (1 − 1/ratio)` (soft knee)
    ///
    /// Returns gain reduction in dB (always ≤ 0.0).
    #[inline]
    fn compute_gain_db(input_db: f32, threshold_db: f32, ratio: f32, knee_db: f32) -> f32 {
        let overshoot = input_db - threshold_db;
        let half_knee = knee_db / 2.0;
        let inv_ratio_complement = 1.0 - 1.0 / ratio;

        if overshoot <= -half_knee {
            0.0
        } else if overshoot > half_knee {
            -(overshoot * inv_ratio_complement)
        } else {
            let knee_factor = (overshoot + half_knee) / knee_db;
            -(knee_factor * knee_factor * overshoot * inv_ratio_complement)
        }
    }

    /// Compute auto makeup gain in linear domain.
    ///
    /// Formula: `makeup_dB = −threshold_db × (1 − 1/ratio) × 0.5`
    ///
    /// The 0.5 factor approximates the fact that not all signal is above threshold,
    /// providing a reasonable average compensation.
    ///
    /// Reference: Giannoulis et al., JAES 2012.
    #[inline]
    fn auto_makeup_linear(threshold_db: f32, ratio: f32) -> f32 {
        let auto_db = -threshold_db * (1.0 - 1.0 / ratio) * 0.5;
        fast_db_to_linear(auto_db)
    }
}

impl DspKernel for CompressorKernel {
    type Params = CompressorParams;

    /// Process a stereo sample pair with linked-stereo compression.
    ///
    /// The detection signal is the mid signal `(L + R) / 2`, so both channels
    /// receive identical gain reduction. This preserves the stereo image — if
    /// L and R were compressed independently, image shift would occur whenever
    /// only one channel exceeds the threshold.
    ///
    /// Signal path:
    /// 1. Mid signal → sidechain HPF → dual envelope → gain computer
    /// 2. Gain reduction (dB) → linear → multiply makeup gain
    /// 3. Apply `comp_gain` to both L and R
    /// 4. Wet/dry blend (`mix_pct`)
    /// 5. Soft limit (ceiling 1.0) → output level
    fn process_stereo(&mut self, left: f32, right: f32, params: &CompressorParams) -> (f32, f32) {
        // ── Update coefficient caches ──────────────────────────────────────
        self.update_caches(params);

        // ── Linked-stereo sidechain detection ─────────────────────────────
        // Mid signal avoids image shift from independent L/R compression
        let mid = (left + right) * 0.5;
        let detection = self.sidechain_hpf.process(mid);
        let envelope = self.dual_envelope(detection);

        // ── Gain computer ─────────────────────────────────────────────────
        let envelope_db = fast_linear_to_db(envelope);
        let gain_reduction_db = Self::compute_gain_db(
            envelope_db,
            params.threshold_db,
            params.ratio,
            params.knee_db,
        );
        self.last_gain_reduction_db = gain_reduction_db;
        let gain_linear = fast_db_to_linear(gain_reduction_db);

        // ── Makeup gain (auto or manual) ──────────────────────────────────
        let makeup = if params.auto_makeup >= 0.5 {
            Self::auto_makeup_linear(params.threshold_db, params.ratio)
        } else {
            fast_db_to_linear(params.makeup_db)
        };

        // ── Apply compression gain ────────────────────────────────────────
        let comp_gain = gain_linear * makeup;
        let comp_l = left * comp_gain;
        let comp_r = right * comp_gain;

        // ── Wet/dry blend ─────────────────────────────────────────────────
        let mix = params.mix_pct / 100.0;
        let (mixed_l, mixed_r) = wet_dry_mix_stereo(left, right, comp_l, comp_r, mix);

        // ── Soft limit → output level ─────────────────────────────────────
        let output = fast_db_to_linear(params.output_db);
        (
            soft_limit(mixed_l, 1.0) * output,
            soft_limit(mixed_r, 1.0) * output,
        )
    }

    fn reset(&mut self) {
        self.envelope_follower.reset();
        self.fast_envelope.reset();
        self.sidechain_hpf.clear();
        self.last_gain_reduction_db = 0.0;
        // Invalidate all caches — force recomputation on next process() call.
        // NaN comparisons always fail (NaN != NaN), so every cache will recompute.
        self.last_sidechain_freq_hz = f32::NAN;
        self.last_attack_ms = f32::NAN;
        self.last_release_ms = f32::NAN;
        self.last_detection = f32::NAN;
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.envelope_follower.set_sample_rate(sample_rate);
        self.fast_envelope.set_sample_rate(sample_rate);
        // Invalidate time-dependent caches so they recompute at next process()
        self.last_sidechain_freq_hz = f32::NAN;
        self.last_attack_ms = f32::NAN;
        self.last_release_ms = f32::NAN;
    }

    /// Compressor uses linked-stereo detection (cross-channel mid signal).
    fn is_true_stereo(&self) -> bool {
        true
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_core::kernel::KernelAdapter;
    use sonido_core::{Effect, ParameterInfo};

    // ── Kernel unit tests ──────────────────────────────────────────────────

    /// Silence input must produce silence output.
    #[test]
    fn silence_in_silence_out() {
        let mut kernel = CompressorKernel::new(48000.0);
        let params = CompressorParams::default();
        for _ in 0..512 {
            let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
            assert!(l.abs() < 1e-6, "Expected silence, got left={l}");
            assert!(r.abs() < 1e-6, "Expected silence, got right={r}");
        }
    }

    /// No output sample may be NaN or ±Inf under any standard input.
    #[test]
    fn no_nan_or_inf() {
        let mut kernel = CompressorKernel::new(48000.0);
        let params = CompressorParams::default();
        for i in 0..1000 {
            let t = i as f32 / 48000.0;
            let s = 0.5 * libm::sinf(2.0 * core::f32::consts::PI * 440.0 * t);
            let (l, r) = kernel.process_stereo(s, -s, &params);
            assert!(l.is_finite(), "NaN/Inf at left sample {i}: {l}");
            assert!(r.is_finite(), "NaN/Inf at right sample {i}: {r}");
        }
    }

    /// Descriptor count must equal `CompressorParams::COUNT` and all must be present.
    #[test]
    fn params_descriptor_count() {
        assert_eq!(CompressorParams::COUNT, 11);
        for i in 0..CompressorParams::COUNT {
            assert!(
                CompressorParams::descriptor(i).is_some(),
                "Missing descriptor for index {i}"
            );
        }
        assert!(
            CompressorParams::descriptor(11).is_none(),
            "Index 11 should return None"
        );
    }

    /// A loud 440 Hz signal well above threshold should be reduced after warm-up.
    #[test]
    fn compresses_loud_signal() {
        let sr = 44100.0_f32;
        let mut kernel = CompressorKernel::new(sr);
        let params = CompressorParams {
            threshold_db: -20.0,
            ratio: 4.0,
            attack_ms: 1.0,
            release_ms: 100.0,
            // No makeup, full wet
            makeup_db: 0.0,
            auto_makeup: 0.0,
            mix_pct: 100.0,
            output_db: 0.0,
            ..CompressorParams::default()
        };

        // Warm up with a loud signal (0.5 ≈ −6 dBFS, well above −20 dB threshold)
        let freq = 440.0;
        for i in 0..2000 {
            let s = 0.5 * libm::sinf(i as f32 * 2.0 * core::f32::consts::PI * freq / sr);
            kernel.process_stereo(s, s, &params);
        }

        // Measure peak output over the next 500 samples — must be below input peak
        let mut max_out = 0.0_f32;
        for i in 2000..2500 {
            let s = 0.5 * libm::sinf(i as f32 * 2.0 * core::f32::consts::PI * freq / sr);
            let (l, _r) = kernel.process_stereo(s, s, &params);
            max_out = max_out.max(l.abs());
        }

        assert!(
            max_out < 0.5,
            "Output should be compressed below input peak 0.5, got {max_out}"
        );
    }

    /// A very quiet signal below threshold should pass through essentially unmodified.
    ///
    /// Default threshold is −18 dB; the test signal is at −60 dB (0.001 linear),
    /// so the compressor should apply near-zero gain reduction.
    #[test]
    fn quiet_signal_passes_through() {
        let mut kernel = CompressorKernel::new(48000.0);
        let params = CompressorParams {
            threshold_db: -18.0,
            ratio: 4.0,
            makeup_db: 0.0,
            auto_makeup: 0.0,
            mix_pct: 100.0,
            output_db: 0.0,
            ..CompressorParams::default()
        };

        let input = 0.001; // ≈ −60 dBFS — well below −18 dB threshold
        let mut max_out = 0.0_f32;
        for i in 0..200 {
            let s = input * libm::sinf(i as f32 * 0.1);
            let (l, _r) = kernel.process_stereo(s, s, &params);
            max_out = max_out.max(l.abs());
        }

        // With soft_limit and no gain reduction, output should match input closely
        assert!(
            max_out <= input + 1e-5,
            "Quiet signal below threshold should pass through: input_peak={input}, output_peak={max_out}"
        );
    }

    /// Auto makeup should increase output amplitude compared to no makeup.
    ///
    /// At threshold=−20, ratio=4, auto makeup ≈ 7.5 dB boost.
    #[test]
    fn auto_makeup_increases_output() {
        let sr = 48000.0_f32;
        let freq = 440.0;

        // Params with no makeup
        let no_makeup = CompressorParams {
            threshold_db: -20.0,
            ratio: 4.0,
            attack_ms: 1.0,
            makeup_db: 0.0,
            auto_makeup: 0.0,
            mix_pct: 100.0,
            output_db: 0.0,
            ..CompressorParams::default()
        };

        // Same but with auto makeup enabled
        let with_auto = CompressorParams {
            auto_makeup: 1.0,
            ..no_makeup
        };

        // Warm up + measure both
        let measure = |params: &CompressorParams| -> f32 {
            let mut k = CompressorKernel::new(sr);
            for i in 0..2000 {
                let s = 0.3 * libm::sinf(i as f32 * 2.0 * core::f32::consts::PI * freq / sr);
                k.process_stereo(s, s, params);
            }
            let mut peak = 0.0_f32;
            for i in 2000..2500 {
                let s = 0.3 * libm::sinf(i as f32 * 2.0 * core::f32::consts::PI * freq / sr);
                let (l, _r) = k.process_stereo(s, s, params);
                peak = peak.max(l.abs());
            }
            peak
        };

        let out_no_mu = measure(&no_makeup);
        let out_auto = measure(&with_auto);

        assert!(
            out_auto > out_no_mu,
            "Auto makeup should produce higher output: without={out_no_mu:.4}, with={out_auto:.4}"
        );
    }

    /// Changing the detection mode (Peak vs. RMS) should produce different output levels.
    ///
    /// RMS mode averages power so it typically compresses sustained signals
    /// more heavily than peak mode for the same threshold.
    #[test]
    fn detection_mode_affects_output() {
        let sr = 48000.0_f32;
        let freq = 440.0;

        let peak_params = CompressorParams {
            threshold_db: -20.0,
            ratio: 8.0,
            attack_ms: 1.0,
            detection: 0.0, // Peak
            mix_pct: 100.0,
            output_db: 0.0,
            makeup_db: 0.0,
            auto_makeup: 0.0,
            ..CompressorParams::default()
        };
        let rms_params = CompressorParams {
            detection: 1.0, // RMS
            ..peak_params
        };

        let measure = |params: &CompressorParams| -> f32 {
            let mut k = CompressorKernel::new(sr);
            for i in 0..2000 {
                let s = 0.3 * libm::sinf(i as f32 * 2.0 * core::f32::consts::PI * freq / sr);
                k.process_stereo(s, s, params);
            }
            let mut peak = 0.0_f32;
            for i in 2000..2500 {
                let s = 0.3 * libm::sinf(i as f32 * 2.0 * core::f32::consts::PI * freq / sr);
                let (l, _r) = k.process_stereo(s, s, params);
                peak = peak.max(l.abs());
            }
            peak
        };

        let out_peak = measure(&peak_params);
        let out_rms = measure(&rms_params);

        // Outputs should differ — detection mode must have an effect
        let diff = (out_peak - out_rms).abs();
        assert!(
            diff > 1e-4,
            "Peak and RMS detection should produce different outputs: peak={out_peak:.4}, rms={out_rms:.4}"
        );
    }

    /// Parallel mix at 0% should pass the dry signal unmodified (within soft_limit tolerance).
    #[test]
    fn parallel_mix_blends() {
        let mut kernel = CompressorKernel::new(48000.0);

        // Fully dry — compressor should not alter the signal
        let dry_params = CompressorParams {
            threshold_db: -6.0, // very aggressive — would squash if wet
            ratio: 20.0,
            attack_ms: 0.1,
            mix_pct: 0.0, // 0% wet = fully dry
            output_db: 0.0,
            makeup_db: 0.0,
            auto_makeup: 0.0,
            ..CompressorParams::default()
        };

        let input = 0.4;
        let mut max_diff = 0.0_f32;
        for i in 0..100 {
            let s = input * libm::sinf(i as f32 * 0.2);
            let (l, _r) = kernel.process_stereo(s, s, &dry_params);
            // soft_limit(input, 1.0) at 0.4 should be very close to 0.4
            let expected = soft_limit(s, 1.0) * fast_db_to_linear(dry_params.output_db);
            max_diff = max_diff.max((l - expected).abs());
        }

        assert!(
            max_diff < 1e-5,
            "At 0% mix output should match dry signal: max_diff={max_diff:.6}"
        );
    }

    /// KernelAdapter must wrap CompressorKernel as a functioning Effect.
    #[test]
    fn adapter_wraps_as_effect() {
        let kernel = CompressorKernel::new(48000.0);
        let mut adapter = KernelAdapter::new(kernel, 48000.0);

        adapter.reset();
        let output = adapter.process(0.3);
        assert!(!output.is_nan(), "Output is NaN");
        assert!(output.is_finite(), "Output is infinite");

        // Process a block to verify stability
        for _ in 0..1000 {
            let out = adapter.process(0.2);
            assert!(
                out.is_finite(),
                "Output became non-finite during sustained processing"
            );
        }
    }

    /// The adapter's ParameterInfo must expose the same descriptors as the classic
    /// Compressor effect — including the exact ParamId values (plugin API contracts).
    #[test]
    fn adapter_param_info_matches() {
        let kernel = CompressorKernel::new(48000.0);
        let adapter = KernelAdapter::new(kernel, 48000.0);

        // Count
        assert_eq!(adapter.param_count(), 11);

        // All present, none past end
        for i in 0..11 {
            assert!(
                adapter.param_info(i).is_some(),
                "Missing param_info for index {i}"
            );
        }
        assert!(adapter.param_info(11).is_none());

        // Names match classic compressor.rs
        let expected_names = [
            "Threshold",
            "Ratio",
            "Attack",
            "Release",
            "Makeup Gain",
            "Knee",
            "Detection",
            "SC HPF Freq",
            "Auto Makeup",
            "Output",
            "Mix",
        ];
        for (i, &name) in expected_names.iter().enumerate() {
            let desc = adapter.param_info(i).unwrap();
            assert_eq!(
                desc.name, name,
                "Index {i}: expected name '{name}', got '{}'",
                desc.name
            );
        }

        // ParamId values — critical for automation backwards compatibility.
        // These must match classic compressor.rs impl_params! exactly.
        assert_eq!(adapter.param_info(0).unwrap().id, ParamId(300)); // Threshold
        assert_eq!(adapter.param_info(1).unwrap().id, ParamId(301)); // Ratio
        assert_eq!(adapter.param_info(2).unwrap().id, ParamId(302)); // Attack
        assert_eq!(adapter.param_info(3).unwrap().id, ParamId(303)); // Release
        assert_eq!(adapter.param_info(4).unwrap().id, ParamId(304)); // Makeup Gain
        assert_eq!(adapter.param_info(5).unwrap().id, ParamId(305)); // Knee
        assert_eq!(adapter.param_info(6).unwrap().id, ParamId(306)); // Detection
        assert_eq!(adapter.param_info(7).unwrap().id, ParamId(307)); // SC HPF Freq
        assert_eq!(adapter.param_info(8).unwrap().id, ParamId(308)); // Auto Makeup
        assert_eq!(adapter.param_info(9).unwrap().id, ParamId(309)); // Output
        assert_eq!(adapter.param_info(10).unwrap().id, ParamId(310)); // Mix

        // String IDs — used by CLAP host preset recall
        assert_eq!(adapter.param_info(0).unwrap().string_id, "comp_thresh");
        assert_eq!(adapter.param_info(1).unwrap().string_id, "comp_ratio");
        assert_eq!(adapter.param_info(7).unwrap().string_id, "comp_sc_freq");
        assert_eq!(adapter.param_info(10).unwrap().string_id, "comp_mix");
    }

    /// Morphing between two param snapshots must always produce finite audio.
    #[test]
    fn morph_produces_valid_output() {
        let a = CompressorParams::default();
        let b = CompressorParams {
            threshold_db: -40.0,
            ratio: 10.0,
            attack_ms: 1.0,
            release_ms: 500.0,
            makeup_db: 12.0,
            knee_db: 2.0,
            mix_pct: 50.0,
            output_db: -3.0,
            ..CompressorParams::default()
        };

        for i in 0..=10 {
            let t = i as f32 / 10.0;
            let morphed = CompressorParams::lerp(&a, &b, t);
            let mut kernel = CompressorKernel::new(48000.0);
            for j in 0..200 {
                let s = 0.3 * libm::sinf(j as f32 * 0.1);
                let (l, r) = kernel.process_stereo(s, -s, &morphed);
                assert!(
                    l.is_finite() && r.is_finite(),
                    "Morph at t={t:.1} produced NaN/Inf: l={l}, r={r}"
                );
            }
        }
    }

    /// `from_knobs` must map normalized 0–1 ADC readings to correct parameter ranges.
    #[test]
    fn from_knobs_maps_ranges() {
        // Mid-point knob positions
        let p = CompressorParams::from_knobs(
            0.5, // thresh → -30.0 dB
            0.5, // ratio → 10.5
            0.5, // attack → 50.05 ms
            0.5, // release → 505.0 ms
            0.5, // makeup → 12.0 dB
            0.5, // knee → 6.0 dB
            0.0, // detect → 0 (Peak)
            0.5, // sc_freq → 260.0 Hz
            0.0, // auto_makeup → 0 (Off)
            0.5, // output → 0.0 dB
            0.5, // mix → 50.0 %
        );

        assert!(
            (p.threshold_db - (-30.0)).abs() < 0.1,
            "threshold: got {}",
            p.threshold_db
        );
        assert!((p.ratio - 10.5).abs() < 0.1, "ratio: got {}", p.ratio);
        assert!(
            (p.attack_ms - 50.05).abs() < 0.5,
            "attack_ms: got {}",
            p.attack_ms
        );
        assert!(
            (p.release_ms - 505.0).abs() < 1.0,
            "release_ms: got {}",
            p.release_ms
        );
        assert!(
            (p.makeup_db - 12.0).abs() < 0.1,
            "makeup_db: got {}",
            p.makeup_db
        );
        assert!((p.knee_db - 6.0).abs() < 0.1, "knee_db: got {}", p.knee_db);
        assert_eq!(p.detection, 0.0, "detection should be 0.0 (Peak)");
        assert!(
            (p.sidechain_freq_hz - 260.0).abs() < 1.0,
            "sc_freq: got {}",
            p.sidechain_freq_hz
        );
        assert_eq!(p.auto_makeup, 0.0, "auto_makeup should be 0.0 (Off)");
        assert!(
            (p.output_db - 0.0).abs() < 0.1,
            "output_db: got {}",
            p.output_db
        );
        assert!((p.mix_pct - 50.0).abs() < 0.1, "mix_pct: got {}", p.mix_pct);

        // Extremes: all-zero knobs → minimums
        let lo =
            CompressorParams::from_knobs(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert!((lo.threshold_db - (-60.0)).abs() < 0.1);
        assert!((lo.ratio - 1.0).abs() < 0.1);
        assert!((lo.attack_ms - 0.1).abs() < 0.01);
        assert!((lo.release_ms - 10.0).abs() < 0.1);

        // Extremes: all-one knobs → maximums
        let hi =
            CompressorParams::from_knobs(1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0);
        assert!((hi.threshold_db - 0.0).abs() < 0.1);
        assert!((hi.ratio - 20.0).abs() < 0.1);
        assert!((hi.attack_ms - 100.0).abs() < 0.1);
        assert!((hi.release_ms - 1000.0).abs() < 0.5);
        assert!((hi.makeup_db - 24.0).abs() < 0.1);
        assert!((hi.knee_db - 12.0).abs() < 0.1);
        assert_eq!(hi.detection, 1.0, "detection should snap to 1.0 at full");
        assert!((hi.sidechain_freq_hz - 500.0).abs() < 1.0);
        assert_eq!(
            hi.auto_makeup, 1.0,
            "auto_makeup should snap to 1.0 at full"
        );
        assert!((hi.output_db - 20.0).abs() < 0.1);
        assert!((hi.mix_pct - 100.0).abs() < 0.1);
    }
}
