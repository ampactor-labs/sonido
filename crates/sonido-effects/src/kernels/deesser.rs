//! De-esser kernel — wideband sibilance reduction via sidechain HPF.
//!
//! `DeesserKernel` uses a wideband (full-signal) approach: a highpass sidechain
//! detects sibilance energy, then gain reduction is applied to the full-band
//! signal. This avoids the "lispy" artifact of frequency-selective de-essing.
//!
//! # Signal Flow
//!
//! ```text
//! Input ──┬──────────────────────────────── × gain ── × output
//!         │                                       ▲
//!         └─► HPF @ freq ─► Envelope ─► Gain Computer ─► clamped gain
//!             (detection)    (fast)
//! ```
//!
//! The gain computer is a simple compressor operating only on the sidechain level:
//! when sibilance exceeds the threshold, the gain applied to the full signal is
//! reduced by `(overshoot × (1 − 1/ratio))`, clamped to a maximum `range_db` of
//! gain reduction to prevent over-damping.
//!
//! # Algorithm
//!
//! 1. Filter input through 2nd-order Butterworth HPF at `freq` (detection only).
//! 2. Envelope follower with fast attack (0.5 ms) and moderate release (20 ms)
//!    tracks sibilance level.
//! 3. Gain computer: if `envelope_db > threshold_db`, compute
//!    `gr_db = -(overshoot × (1 − 1/ratio))`, clamped to `−range_db`.
//! 4. Apply `db_to_linear(gr_db)` as a gain multiplier to the full-band input.
//! 5. Final `output_db` trim.
//!
//! Reference: Giannoulis, Massberg & Reiss, "Digital Dynamic Range Compressor
//! Design — A Tutorial and Analysis", JAES 2012 (gain computer formulation).
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! let adapter = KernelAdapter::new(DeesserKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing)
//! let mut kernel = DeesserKernel::new(48000.0);
//! let params = DeesserParams::default();
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::{
    Biquad, Cached, EnvelopeFollower, ParamDescriptor, ParamId, ParamScale, ParamUnit,
    fast_db_to_linear, fast_linear_to_db, highpass_coefficients, math::flush_denormal,
};

// ── Constants ────────────────────────────────────────────────────────────────

/// Sidechain envelope follower attack time — fast to catch brief consonants (ms).
const ATTACK_MS: f32 = 0.5;

/// Sidechain envelope follower release time — moderate to avoid pumping (ms).
const RELEASE_MS: f32 = 20.0;

/// Butterworth Q for sidechain HPF (1/√2 ≈ 0.707).
const HPF_Q: f32 = core::f32::consts::SQRT_2 * 0.5;

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`DeesserKernel`].
///
/// All values are in **user-facing units**.
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `freq_hz` | Hz | 2000–16000 | 6000.0 |
/// | 1 | `thresh_db` | dB | −60–0 | −20.0 |
/// | 2 | `ratio` | ratio | 1–20 | 8.0 |
/// | 3 | `range_db` | dB | 0–24 | 12.0 |
/// | 4 | `output_db` | dB | −60–+6 | 0.0 |
#[derive(Debug, Clone, Copy)]
pub struct DeesserParams {
    /// Sidechain HPF cutoff frequency in Hz.
    ///
    /// Range: 2000.0–16000.0 Hz (default 6000.0). Frequencies above this point
    /// are detected for sibilance. Higher values target only extreme sibilance;
    /// lower values also catch "ess" consonants.
    pub freq_hz: f32,

    /// Sibilance detection threshold in dB.
    ///
    /// Range: −60.0–0.0 dB (default −20.0). Gain reduction is applied only when
    /// the HPF sidechain level exceeds this threshold.
    pub thresh_db: f32,

    /// Gain reduction ratio.
    ///
    /// Range: 1.0–20.0 (default 8.0). At 8.0, a 8 dB overshoot produces 1 dB
    /// of output overshoot. Higher ratios approach limiting of sibilance.
    pub ratio: f32,

    /// Maximum gain reduction in dB.
    ///
    /// Range: 0.0–24.0 dB (default 12.0). Caps how much the gain is reduced.
    /// Prevents over-de-essing on very loud sibilants. 0.0 disables the effect.
    pub range_db: f32,

    /// Output level in dB.
    ///
    /// Range: −60.0–+6.0 dB (default 0.0). Final output trim.
    pub output_db: f32,
}

impl Default for DeesserParams {
    fn default() -> Self {
        Self {
            freq_hz: 6000.0,
            thresh_db: -20.0,
            ratio: 8.0,
            range_db: 12.0,
            output_db: 0.0,
        }
    }
}

impl KernelParams for DeesserParams {
    const COUNT: usize = 5;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            // ── [0] Detection frequency ───────────────────────────────────────
            0 => Some(
                ParamDescriptor::custom("Freq", "Freq", 2000.0, 16000.0, 6000.0)
                    .with_unit(ParamUnit::Hertz)
                    .with_step(10.0)
                    .with_id(ParamId(3000), "ds_freq")
                    .with_scale(ParamScale::Logarithmic),
            ),
            // ── [1] Threshold ─────────────────────────────────────────────────
            1 => Some(
                ParamDescriptor::gain_db("Threshold", "Thresh", -60.0, 0.0, -20.0)
                    .with_id(ParamId(3001), "ds_thresh"),
            ),
            // ── [2] Ratio ─────────────────────────────────────────────────────
            2 => Some(
                ParamDescriptor::custom("Ratio", "Ratio", 1.0, 20.0, 8.0)
                    .with_unit(ParamUnit::Ratio)
                    .with_step(0.1)
                    .with_id(ParamId(3002), "ds_ratio"),
            ),
            // ── [3] Range ─────────────────────────────────────────────────────
            3 => Some(
                ParamDescriptor::gain_db("Range", "Range", 0.0, 24.0, 12.0)
                    .with_id(ParamId(3003), "ds_range"),
            ),
            // ── [4] Output level ──────────────────────────────────────────────
            4 => Some(
                sonido_core::gain::output_param_descriptor().with_id(ParamId(3004), "ds_output"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Slow,     // freq — filter coeff recalc
            1 => SmoothingStyle::Standard, // threshold
            2 => SmoothingStyle::Standard, // ratio
            3 => SmoothingStyle::Standard, // range
            4 => SmoothingStyle::Fast,     // output
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.freq_hz,
            1 => self.thresh_db,
            2 => self.ratio,
            3 => self.range_db,
            4 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.freq_hz = value,
            1 => self.thresh_db = value,
            2 => self.ratio = value,
            3 => self.range_db = value,
            4 => self.output_db = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP de-esser kernel using a wideband sidechain approach.
///
/// Contains ONLY mutable state required for audio processing. No `SmoothedParam`,
/// no atomics, no platform awareness.
///
/// # DSP State
///
/// - `sidechain_hpf`: biquad HPF on the detection path. Coefficients recomputed
///   when `freq_hz` changes.
/// - `envelope`: fast envelope follower on the HPF sidechain output (0.5ms / 20ms).
/// - `hpf_cache`: change-detector for HPF coefficients.
///
/// # Gain Computer
///
/// Given `overshoot = envelope_db - threshold_db`:
/// - `overshoot ≤ 0`: no reduction (pass through at unity gain)
/// - `overshoot > 0`: `gr_db = max(−overshoot × (1 − 1/ratio), −range_db)`
///
/// The `range_db` clamp prevents over-processing loud sibilants.
pub struct DeesserKernel {
    /// Sample rate — needed for coefficient recalculation.
    sample_rate: f32,

    /// Sidechain highpass filter (detection path only).
    ///
    /// Applied before the envelope follower. Coefficients recomputed when
    /// `freq_hz` changes.
    sidechain_hpf: Biquad,

    /// Fast envelope follower on the HPF sidechain path.
    ///
    /// Fixed timing: 0.5 ms attack, 20 ms release. Short attack ensures
    /// transient sibilants are caught; moderate release avoids pumping.
    envelope: EnvelopeFollower,

    /// Change-detector for sidechain HPF coefficients.
    ///
    /// Avoids re-running `highpass_coefficients()` every sample when `freq_hz` is stable.
    hpf_cache: Cached<[f32; 6]>,
}

impl DeesserKernel {
    /// Create a new de-esser kernel at `sample_rate`.
    ///
    /// The sidechain HPF is set to 6000 Hz (default). The envelope follower
    /// uses fixed 0.5 ms attack and 20 ms release.
    pub fn new(sample_rate: f32) -> Self {
        let defaults = DeesserParams::default();

        // Initialize sidechain HPF at default frequency
        let (b0, b1, b2, a0, a1, a2) = highpass_coefficients(defaults.freq_hz, HPF_Q, sample_rate);
        let mut sidechain_hpf = Biquad::new();
        sidechain_hpf.set_coefficients(b0, b1, b2, a0, a1, a2);

        // Initialize envelope follower with fixed fast timing
        let mut envelope = EnvelopeFollower::new(sample_rate);
        envelope.set_attack_ms(ATTACK_MS);
        envelope.set_release_ms(RELEASE_MS);

        // Initialize cache with defaults (first update is a no-op)
        let initial_coeffs = [b0, b1, b2, a0, a1, a2];
        let mut hpf_cache = Cached::new(initial_coeffs, 1);
        hpf_cache.update(&[defaults.freq_hz], 0.5, |inputs| {
            let freq = inputs[0].clamp(2000.0, 16000.0);
            let (b0, b1, b2, a0, a1, a2) = highpass_coefficients(freq, HPF_Q, sample_rate);
            [b0, b1, b2, a0, a1, a2]
        });

        Self {
            sample_rate,
            sidechain_hpf,
            envelope,
            hpf_cache,
        }
    }

    /// Update HPF coefficients if `freq_hz` has changed.
    #[inline]
    fn update_hpf(&mut self, freq_hz: f32) {
        let sr = self.sample_rate;
        let coeffs = *self.hpf_cache.update(&[freq_hz], 0.5, |inputs| {
            let freq = inputs[0].clamp(2000.0, 16000.0);
            let (b0, b1, b2, a0, a1, a2) = highpass_coefficients(freq, HPF_Q, sr);
            [b0, b1, b2, a0, a1, a2]
        });
        self.sidechain_hpf.set_coefficients(
            coeffs[0], coeffs[1], coeffs[2], coeffs[3], coeffs[4], coeffs[5],
        );
    }

    /// Compute gain reduction multiplier from the sidechain detection signal.
    ///
    /// Runs the envelope follower and gain computer.
    ///
    /// - Returns 1.0 when the sidechain is below threshold (no reduction).
    /// - Returns `db_to_linear(gr_db)` clamped to `−range_db` otherwise.
    #[inline]
    fn compute_gain_reduction(
        &mut self,
        detection: f32,
        thresh_db: f32,
        ratio: f32,
        range_db: f32,
    ) -> f32 {
        let amplitude = self.envelope.process(libm::fabsf(detection));
        let amplitude = flush_denormal(amplitude).max(1e-10);
        let envelope_db = fast_linear_to_db(amplitude);
        let overshoot = envelope_db - thresh_db;
        if overshoot > 0.0 {
            let ratio_safe = ratio.max(1.0);
            // Gain reduction in dB, clamped by range_db
            let gr_db = (-overshoot * (1.0 - 1.0 / ratio_safe)).max(-range_db);
            fast_db_to_linear(gr_db)
        } else {
            1.0
        }
    }
}

impl DspKernel for DeesserKernel {
    type Params = DeesserParams;

    /// Process a mono sample through the de-esser.
    ///
    /// The detection path filters the input through the sidechain HPF.
    /// Gain reduction is applied to the full-band signal.
    fn process(&mut self, input: f32, params: &DeesserParams) -> f32 {
        self.update_hpf(params.freq_hz);

        // Sidechain: HPF → envelope → gain computer
        let detection = self.sidechain_hpf.process(input);
        let gr =
            self.compute_gain_reduction(detection, params.thresh_db, params.ratio, params.range_db);

        flush_denormal(input * gr) * fast_db_to_linear(params.output_db)
    }

    /// Process a stereo sample pair through the de-esser.
    ///
    /// Linked-stereo detection: the sidechain HPF processes the mid signal
    /// `(left + right) × 0.5` so both channels receive the same gain reduction,
    /// preserving the stereo image.
    fn process_stereo(&mut self, left: f32, right: f32, params: &DeesserParams) -> (f32, f32) {
        self.update_hpf(params.freq_hz);

        // Sidechain: HPF on the mid (mono) signal
        let mid = (left + right) * 0.5;
        let detection = self.sidechain_hpf.process(mid);

        let gr =
            self.compute_gain_reduction(detection, params.thresh_db, params.ratio, params.range_db);
        let output_gain = fast_db_to_linear(params.output_db);

        (
            flush_denormal(left * gr) * output_gain,
            flush_denormal(right * gr) * output_gain,
        )
    }

    /// Process a stereo sample with an external sidechain signal.
    ///
    /// Uses the external `sc_left`/`sc_right` average as the detection signal
    /// instead of filtering the main input. The internal sidechain HPF is bypassed
    /// when an external sidechain is connected.
    fn process_stereo_with_sidechain(
        &mut self,
        left: f32,
        right: f32,
        sc_left: f32,
        sc_right: f32,
        params: &DeesserParams,
    ) -> (f32, f32) {
        // Use the external sidechain mono average directly as the detection signal.
        // This allows feeding a dedicated HF microphone or a parallel HPF chain.
        let sc_mid = (sc_left + sc_right) * 0.5;
        let gr =
            self.compute_gain_reduction(sc_mid, params.thresh_db, params.ratio, params.range_db);
        let output_gain = fast_db_to_linear(params.output_db);

        (
            flush_denormal(left * gr) * output_gain,
            flush_denormal(right * gr) * output_gain,
        )
    }

    fn reset(&mut self) {
        self.sidechain_hpf.clear();
        self.envelope.reset();
        self.hpf_cache.invalidate();
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.envelope.set_sample_rate(sample_rate);
        self.hpf_cache.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_core::ParameterInfo;
    use sonido_core::kernel::KernelAdapter;

    /// All outputs must be finite — no NaN or Inf under any input.
    #[test]
    fn finite_output() {
        let mut kernel = DeesserKernel::new(48000.0);
        let params = DeesserParams::default();

        for i in 0..4096 {
            let t = i as f32 / 48000.0;
            let s = 0.8 * libm::sinf(2.0 * core::f32::consts::PI * 8000.0 * t);
            let (l, r) = kernel.process_stereo(s, -s, &params);
            assert!(l.is_finite(), "Left output non-finite at sample {i}: {l}");
            assert!(r.is_finite(), "Right output non-finite at sample {i}: {r}");
        }
    }

    /// De-esser must attenuate sibilance above threshold.
    ///
    /// Feeds a HF sine (8 kHz, above the 6 kHz detection frequency) at a level
    /// well above the threshold. After warm-up the de-esser should have reduced
    /// the output below the input amplitude.
    #[test]
    fn deesser_reduces_sibilance() {
        let sr = 48000.0_f32;
        let mut kernel = DeesserKernel::new(sr);

        // Params: 6 kHz HPF, -20 dB threshold, ratio 8, range 24 dB
        let params = DeesserParams {
            freq_hz: 6000.0,
            thresh_db: -20.0,
            ratio: 8.0,
            range_db: 24.0,
            output_db: 0.0,
        };

        // Input: 8 kHz sine at -6 dBFS (above threshold by ~14 dB)
        let freq = 8000.0_f32;
        let amplitude = 0.5_f32; // -6 dBFS ≈ -6 dB above 0 dBFS

        // Warm up for 100 ms so envelope settles
        for i in 0..(sr as usize / 10) {
            let t = i as f32 / sr;
            let s = amplitude * libm::sinf(2.0 * core::f32::consts::PI * freq * t);
            kernel.process(s, &params);
        }

        // Measure output peak over one cycle
        let cycle = (sr / freq) as usize;
        let mut peak_out = 0.0_f32;
        let base = sr as usize / 10;
        for i in 0..cycle {
            let t = (base + i) as f32 / sr;
            let s = amplitude * libm::sinf(2.0 * core::f32::consts::PI * freq * t);
            let out = kernel.process(s, &params);
            peak_out = peak_out.max(libm::fabsf(out));
        }

        assert!(
            peak_out < amplitude,
            "De-esser should reduce sibilance: peak_out={peak_out:.4} >= amplitude={amplitude:.4}"
        );
    }

    /// De-esser must pass low-frequency content unchanged (below detection band).
    ///
    /// Feeds a 100 Hz sine, which is well below the 6 kHz HPF cutoff.
    /// The sidechain should detect no energy and apply no gain reduction.
    #[test]
    fn passes_low_frequencies() {
        let sr = 48000.0_f32;
        let mut kernel = DeesserKernel::new(sr);

        // Very aggressive settings to expose any inadvertent LF attenuation
        let params = DeesserParams {
            freq_hz: 6000.0,
            thresh_db: -60.0, // effectively always active if any HF detected
            ratio: 20.0,
            range_db: 24.0,
            output_db: 0.0,
        };

        let freq = 100.0_f32;
        let amplitude = 0.5_f32;

        // Warm up
        for i in 0..4800 {
            let t = i as f32 / sr;
            let s = amplitude * libm::sinf(2.0 * core::f32::consts::PI * freq * t);
            kernel.process(s, &params);
        }

        // Measure peak output over one cycle at 100 Hz
        let cycle = (sr / freq) as usize;
        let mut peak_out = 0.0_f32;
        for i in 0..cycle {
            let t = (4800 + i) as f32 / sr;
            let s = amplitude * libm::sinf(2.0 * core::f32::consts::PI * freq * t);
            let out = kernel.process(s, &params);
            peak_out = peak_out.max(libm::fabsf(out));
        }

        // LF signal should pass essentially unchanged (allow <3% change)
        let ratio = peak_out / amplitude;
        assert!(
            ratio > 0.97,
            "De-esser attenuated LF signal: output/input={ratio:.4} (want > 0.97)"
        );
    }

    /// KernelAdapter wraps the kernel and exposes the correct parameter count and IDs.
    #[test]
    fn adapter_param_info() {
        let kernel = DeesserKernel::new(48000.0);
        let adapter = KernelAdapter::new(kernel, 48000.0);

        assert_eq!(adapter.param_count(), 5);
        for i in 0..5 {
            assert!(adapter.param_info(i).is_some(), "Missing param {i}");
        }
        assert!(adapter.param_info(5).is_none());

        // ParamId base = 3000
        assert_eq!(adapter.param_info(0).unwrap().id, ParamId(3000)); // freq
        assert_eq!(adapter.param_info(4).unwrap().id, ParamId(3004)); // output

        // String IDs
        assert_eq!(adapter.param_info(0).unwrap().string_id, "ds_freq");
        assert_eq!(adapter.param_info(1).unwrap().string_id, "ds_thresh");
        assert_eq!(adapter.param_info(4).unwrap().string_id, "ds_output");
    }
}
