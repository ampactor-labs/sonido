//! Stereo Widener kernel — M/S width control, Haas delay, and bass mono.
//!
//! `StereoWidenerKernel` owns DSP state (LR4 biquads, Haas delay line,
//! coefficient cache). Parameters are received via `&StereoWidenerParams` each
//! sample. Deployed via [`Adapter`](sonido_core::kernel::Adapter) for
//! desktop/plugin, or called directly on embedded targets.
//!
//! # Signal Flow
//!
//! ```text
//! Input L/R
//!   → M/S encode   (mid = (L+R)×0.5, side = (L−R)×0.5)
//!   → Width scale  (side × width/100)
//!   → M/S decode   (out_L = mid+side, out_R = mid−side)
//!   → Haas delay   (right channel, 0–30 ms, InterpolatedDelay)
//!   → Bass mono    (LR4 crossover, mono-sum below bass_mono_hz)
//!   → Output level
//! ```
//!
//! # M/S Width Algorithm
//!
//! ```text
//! mid  = (L + R) × 0.5
//! side = (L − R) × 0.5 × (width / 100)
//! out_L = mid + side
//! out_R = mid − side
//! ```
//!
//! At `width = 100` (default) this is the identity transform.
//! At `width = 0` both channels collapse to the mono mid signal.
//! At `width = 200` the side content is doubled, exaggerating stereo width.
//!
//! # Haas Effect
//!
//! A short delay (0–30 ms) on the right channel creates a spatial impression
//! without changing panning. Uses [`InterpolatedDelay`] so that smoothed
//! delay time changes from the adapter are artefact-free.
//!
//! Reference: Haas, H. (1951). "The influence of a single echo on the
//! audibility of speech", Acustica.
//!
//! # LR4 Bass Mono Crossover (Linkwitz-Riley 4th-order)
//!
//! Two cascaded Butterworth LP biquads (Q = 1/√2) per channel form the low
//! band. Two cascaded Butterworth HP biquads form the high band. Low bands
//! from L and R are mono-summed; high bands remain stereo.
//!
//! Reference: Linkwitz, "Active Crossover Networks for Noncoincident Drivers",
//! JAES 1976.
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! let adapter = Adapter::new(StereoWidenerKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing, ADCs are hardware-filtered)
//! let mut kernel = StereoWidenerKernel::new(48000.0);
//! let params = StereoWidenerParams::from_knobs(
//!     adc_width, adc_haas, adc_bass_mono, adc_output,
//! );
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

use sonido_core::biquad::{highpass_coefficients, lowpass_coefficients};
use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::math::ms_to_samples;
use sonido_core::{
    Biquad, Cached, InterpolatedDelay, ParamDescriptor, ParamId, ParamScale, ParamUnit,
    fast_db_to_linear,
};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Butterworth Q for Linkwitz-Riley 4th-order crossover.
const BUTTERWORTH_Q: f32 = core::f32::consts::FRAC_1_SQRT_2;

/// Maximum Haas delay in seconds (30 ms).
const MAX_HAAS_SECONDS: f32 = 0.03;

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`StereoWidenerKernel`].
///
/// All values are in **user-facing units** — the same units shown in GUIs and
/// stored in presets. The kernel converts internally as needed.
///
/// ## Parameter Table
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `width_pct` | % | 0–200 | 100.0 |
/// | 1 | `haas_delay_ms` | ms | 0–30 | 0.0 |
/// | 2 | `bass_mono_hz` | Hz | 0–500 | 0.0 |
/// | 3 | `output_db` | dB | −60–6 | 0.0 |
#[derive(Debug, Clone, Copy)]
pub struct StereoWidenerParams {
    /// Stereo width as a percentage.
    ///
    /// Range: 0–200 %. 0 = mono, 100 = original stereo, 200 = doubled side.
    pub width_pct: f32,

    /// Haas delay on the right channel in milliseconds.
    ///
    /// Range: 0.0 to 30.0 ms. Default 0.0 (disabled). A short delay on one
    /// channel creates spatial width without panning.
    pub haas_delay_ms: f32,

    /// Bass mono crossover frequency in Hz.
    ///
    /// Range: 0.0 to 500.0 Hz. Default 0.0 (disabled). Content below this
    /// frequency is mono-summed to keep the stereo image coherent at low
    /// frequencies. 0.0 bypasses the crossover entirely.
    pub bass_mono_hz: f32,

    /// Output level in decibels.
    ///
    /// Range: −60.0 to +6.0 dB, default 0.0.
    pub output_db: f32,
}

impl Default for StereoWidenerParams {
    fn default() -> Self {
        Self {
            width_pct: 100.0,
            haas_delay_ms: 0.0,
            bass_mono_hz: 0.0,
            output_db: 0.0,
        }
    }
}

impl StereoWidenerParams {
    /// Creates parameters from normalized 0–1 knob readings.
    ///
    /// | Argument | Index | Parameter | Range |
    /// |----------|-------|-----------|-------|
    /// | `width` | 0 | `width_pct` | 0–200 % |
    /// | `haas` | 1 | `haas_delay_ms` | 0–30 ms |
    /// | `bass_mono` | 2 | `bass_mono_hz` | 0–500 Hz (log) |
    /// | `output` | 3 | `output_db` | −60–+6 dB |
    pub fn from_knobs(width: f32, haas: f32, bass_mono: f32, output: f32) -> Self {
        Self::from_normalized(&[width, haas, bass_mono, output])
    }
}

impl KernelParams for StereoWidenerParams {
    const COUNT: usize = 4;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(
                ParamDescriptor::custom("Width", "Width", 0.0, 200.0, 100.0)
                    .with_unit(ParamUnit::Percent)
                    .with_step(1.0)
                    .with_id(ParamId(3100), "sw_width"),
            ),
            1 => Some(
                ParamDescriptor::custom("Haas Delay", "Haas", 0.0, 30.0, 0.0)
                    .with_unit(ParamUnit::Milliseconds)
                    .with_step(0.1)
                    .with_id(ParamId(3101), "sw_haas_delay"),
            ),
            2 => Some(
                ParamDescriptor {
                    name: "Bass Mono",
                    short_name: "BassMn",
                    unit: ParamUnit::Hertz,
                    min: 0.0,
                    max: 500.0,
                    default: 0.0,
                    step: 1.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(3102), "sw_bass_mono")
                .with_scale(ParamScale::Logarithmic),
            ),
            3 => Some(
                sonido_core::gain::output_param_descriptor().with_id(ParamId(3103), "sw_output"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Standard,     // width_pct — 10 ms
            1 => SmoothingStyle::Interpolated, // haas_delay_ms — 50 ms, smooth delay change
            2 => SmoothingStyle::Slow,         // bass_mono_hz — filter coefficient, 20 ms
            3 => SmoothingStyle::Fast,         // output_db — 5 ms
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.width_pct,
            1 => self.haas_delay_ms,
            2 => self.bass_mono_hz,
            3 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.width_pct = value,
            1 => self.haas_delay_ms = value,
            2 => self.bass_mono_hz = value,
            3 => self.output_db = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP stereo widener kernel.
///
/// Contains ONLY the mutable state required for audio processing:
///
/// - LR4 crossover biquads: `bass_lp[channel][stage]` and `bass_hp[channel][stage]`
/// - [`InterpolatedDelay`] for the right-channel Haas effect
/// - Sample rate and coefficient cache for bass crossover
///
/// No `SmoothedParam`, no atomics, no platform awareness.
/// [`is_true_stereo`](DspKernel::is_true_stereo) returns `true` because L and R
/// are processed through different signal paths (M/S decode produces distinct
/// outputs, Haas delay is right-channel only).
///
/// # Invariants
///
/// The bass crossover is bypassed (no biquad processing) when
/// `bass_mono_hz < 5.0 Hz` to avoid numerical instability at very low
/// corner frequencies.
pub struct StereoWidenerKernel {
    /// Current sample rate in Hz.
    sample_rate: f32,

    /// LR4 lowpass biquads for bass mono crossover — `[channel][stage]`.
    bass_lp: [[Biquad; 2]; 2],

    /// LR4 highpass biquads for bass mono crossover — `[channel][stage]`.
    bass_hp: [[Biquad; 2]; 2],

    /// Haas delay line for the right channel.
    haas_delay: InterpolatedDelay,

    /// Coefficient cache for the LR4 bass crossover, keyed on `bass_mono_hz`.
    bass_cache: Cached<[[f32; 6]; 2]>,
}

impl StereoWidenerKernel {
    /// Create a new stereo widener kernel at the given sample rate.
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate;
        let default_freq = 120.0_f32; // use a sensible initial frequency for filter init

        let compute_crossover = |freq_hz: f32| -> [[f32; 6]; 2] {
            let freq = freq_hz.clamp(20.0, 500.0_f32.min(sr * 0.45));
            let (lb0, lb1, lb2, la0, la1, la2) = lowpass_coefficients(freq, BUTTERWORTH_Q, sr);
            let (hb0, hb1, hb2, ha0, ha1, ha2) = highpass_coefficients(freq, BUTTERWORTH_Q, sr);
            [
                [lb0, lb1, lb2, la0, la1, la2],
                [hb0, hb1, hb2, ha0, ha1, ha2],
            ]
        };

        let initial_coeffs = compute_crossover(default_freq);

        let mut bass_lp: [[Biquad; 2]; 2] =
            core::array::from_fn(|_| core::array::from_fn(|_| Biquad::new()));
        let mut bass_hp: [[Biquad; 2]; 2] =
            core::array::from_fn(|_| core::array::from_fn(|_| Biquad::new()));
        let lp = initial_coeffs[0];
        let hp = initial_coeffs[1];
        for ch in 0..2 {
            for stage in 0..2 {
                bass_lp[ch][stage].set_coefficients(lp[0], lp[1], lp[2], lp[3], lp[4], lp[5]);
                bass_hp[ch][stage].set_coefficients(hp[0], hp[1], hp[2], hp[3], hp[4], hp[5]);
            }
        }

        let mut bass_cache = Cached::new(initial_coeffs, 1);
        bass_cache.update(&[default_freq], 0.01, |inputs| compute_crossover(inputs[0]));

        Self {
            sample_rate,
            bass_lp,
            bass_hp,
            haas_delay: InterpolatedDelay::from_time(sample_rate, MAX_HAAS_SECONDS),
            bass_cache,
        }
    }

    /// Apply cached LR4 crossover coefficients to all 8 biquad filters.
    fn apply_bass_crossover(&mut self, coeffs: [[f32; 6]; 2]) {
        let lp = coeffs[0];
        let hp = coeffs[1];
        for ch in 0..2 {
            for stage in 0..2 {
                self.bass_lp[ch][stage].set_coefficients(lp[0], lp[1], lp[2], lp[3], lp[4], lp[5]);
                self.bass_hp[ch][stage].set_coefficients(hp[0], hp[1], hp[2], hp[3], hp[4], hp[5]);
            }
        }
    }

    /// Process a signal through two cascaded biquad stages (one LR4 half).
    #[inline]
    fn process_lr4(biquads: &mut [Biquad; 2], input: f32) -> f32 {
        let mid = biquads[0].process(input);
        biquads[1].process(mid)
    }
}

impl DspKernel for StereoWidenerKernel {
    type Params = StereoWidenerParams;

    fn process_stereo(
        &mut self,
        left: f32,
        right: f32,
        params: &StereoWidenerParams,
    ) -> (f32, f32) {
        // ── 1. M/S width ──────────────────────────────────────────────────────
        let width = params.width_pct / 100.0;
        let mid = (left + right) * 0.5;
        let side = (left - right) * 0.5 * width;
        let mut l = mid + side;
        let mut r = mid - side;

        // ── 2. Haas delay on right channel ────────────────────────────────────
        let haas_samples = ms_to_samples(params.haas_delay_ms.clamp(0.0, 30.0), self.sample_rate);
        if haas_samples > 0.01 {
            let delayed = self.haas_delay.read(haas_samples);
            self.haas_delay.write(r);
            r = delayed;
        } else {
            // Keep delay line fed to avoid stale reads if delay is later enabled.
            self.haas_delay.write(r);
        }

        // ── 3. Bass mono (LR4 crossover) ──────────────────────────────────────
        // Bypass when bass_mono_hz < 5 Hz (effectively disabled at 0 default).
        if params.bass_mono_hz >= 5.0 {
            let sr = self.sample_rate;
            let coeffs = *self
                .bass_cache
                .update(&[params.bass_mono_hz], 0.01, |inputs| {
                    let freq = inputs[0].clamp(20.0, 500.0_f32.min(sr * 0.45));
                    let (lb0, lb1, lb2, la0, la1, la2) =
                        lowpass_coefficients(freq, BUTTERWORTH_Q, sr);
                    let (hb0, hb1, hb2, ha0, ha1, ha2) =
                        highpass_coefficients(freq, BUTTERWORTH_Q, sr);
                    [
                        [lb0, lb1, lb2, la0, la1, la2],
                        [hb0, hb1, hb2, ha0, ha1, ha2],
                    ]
                });
            self.apply_bass_crossover(coeffs);

            let low_l = Self::process_lr4(&mut self.bass_lp[0], l);
            let low_r = Self::process_lr4(&mut self.bass_lp[1], r);
            let high_l = Self::process_lr4(&mut self.bass_hp[0], l);
            let high_r = Self::process_lr4(&mut self.bass_hp[1], r);
            let mono_low = (low_l + low_r) * 0.5;
            l = mono_low + high_l;
            r = mono_low + high_r;
        }

        // ── 4. Output level ───────────────────────────────────────────────────
        let output_gain = fast_db_to_linear(params.output_db);
        (l * output_gain, r * output_gain)
    }

    fn reset(&mut self) {
        self.haas_delay.clear();
        for ch in 0..2 {
            for stage in 0..2 {
                self.bass_lp[ch][stage].clear();
                self.bass_hp[ch][stage].clear();
            }
        }
        // bass_cache is NOT invalidated — coefficients depend only on frequency.
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.haas_delay = InterpolatedDelay::from_time(sample_rate, MAX_HAAS_SECONDS);
        self.bass_cache.invalidate();
    }

    fn is_true_stereo(&self) -> bool {
        // M/S decode always produces decorrelated L/R; Haas delay is right-only.
        true
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_core::Effect;
    use sonido_core::kernel::Adapter;

    #[test]
    fn silence_in_silence_out() {
        let mut kernel = StereoWidenerKernel::new(48000.0);
        let params = StereoWidenerParams::default();
        let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
        assert!(l.abs() < 1e-6, "Expected silence on left, got {l}");
        assert!(r.abs() < 1e-6, "Expected silence on right, got {r}");
    }

    #[test]
    fn no_nan_or_inf() {
        let mut kernel = StereoWidenerKernel::new(48000.0);
        let params = StereoWidenerParams {
            width_pct: 180.0,
            haas_delay_ms: 15.0,
            bass_mono_hz: 120.0,
            output_db: -3.0,
        };
        for i in 0..1000 {
            let t = i as f32 / 48000.0;
            let s = libm::sinf(2.0 * core::f32::consts::PI * 440.0 * t);
            let (l, r) = kernel.process_stereo(s, -s, &params);
            assert!(l.is_finite(), "Left NaN/Inf at sample {i}: {l}");
            assert!(r.is_finite(), "Right NaN/Inf at sample {i}: {r}");
        }
    }

    #[test]
    fn params_descriptor_count() {
        assert_eq!(StereoWidenerParams::COUNT, 4);

        let d0 = StereoWidenerParams::descriptor(0).expect("index 0 must exist");
        assert_eq!(d0.name, "Width");
        assert!((d0.min - 0.0).abs() < 0.01);
        assert!((d0.max - 200.0).abs() < 0.01);
        assert!((d0.default - 100.0).abs() < 0.01);
        assert_eq!(d0.id, ParamId(3100));
        assert_eq!(d0.string_id, "sw_width");

        let d1 = StereoWidenerParams::descriptor(1).expect("index 1 must exist");
        assert_eq!(d1.name, "Haas Delay");
        assert_eq!(d1.id, ParamId(3101));
        assert_eq!(d1.string_id, "sw_haas_delay");

        let d2 = StereoWidenerParams::descriptor(2).expect("index 2 must exist");
        assert_eq!(d2.name, "Bass Mono");
        assert_eq!(d2.id, ParamId(3102));
        assert_eq!(d2.string_id, "sw_bass_mono");

        let d3 = StereoWidenerParams::descriptor(3).expect("index 3 must exist");
        assert_eq!(d3.name, "Output");
        assert_eq!(d3.id, ParamId(3103));
        assert_eq!(d3.string_id, "sw_output");

        assert!(StereoWidenerParams::descriptor(4).is_none());
    }

    #[test]
    fn adapter_wraps_as_effect() {
        let kernel = StereoWidenerKernel::new(48000.0);
        let mut adapter = Adapter::new(kernel, 48000.0);
        adapter.reset();
        let output = adapter.process(0.3);
        assert!(!output.is_nan(), "adapter.process() returned NaN");
        assert!(output.is_finite(), "adapter.process() returned Inf");
    }

    /// Width = 0 must collapse L and R to identical mono mid signal.
    ///
    /// With width = 0, side = 0 always:
    ///   mid = (L + R) / 2
    ///   out_L = mid + 0 = mid
    ///   out_R = mid − 0 = mid
    #[test]
    fn width_zero_produces_mono() {
        let mut kernel = StereoWidenerKernel::new(48000.0);
        let params = StereoWidenerParams {
            width_pct: 0.0,
            ..Default::default()
        };

        // Warm up
        for _ in 0..1000 {
            kernel.process_stereo(0.3, 0.7, &params);
        }

        let (l, r) = kernel.process_stereo(0.3, 0.7, &params);

        // Both channels should be the mono mid = (0.3 + 0.7) / 2 = 0.5
        assert!(
            (l - 0.5).abs() < 0.01,
            "Width 0 should produce mono mid ≈ 0.5 on left, got {l}"
        );
        assert!(
            (l - r).abs() < 1e-5,
            "Width 0 should make L = R, diff = {}",
            (l - r).abs()
        );
    }

    /// Haas delay must produce a time-shifted right channel.
    ///
    /// With a 5 ms delay on the right channel and an impulse input, the right
    /// channel output at sample 0 should be silent (delay hasn't elapsed yet).
    #[test]
    fn haas_delay_shifts_right_channel() {
        let sr = 48000.0;
        let delay_ms = 5.0_f32;
        let delay_samples = (delay_ms / 1000.0 * sr) as usize;

        let params = StereoWidenerParams {
            haas_delay_ms: delay_ms,
            ..Default::default()
        };

        let mut kernel = StereoWidenerKernel::new(sr);

        // Prime the delay buffer with silence
        for _ in 0..delay_samples {
            kernel.process_stereo(0.0, 0.0, &params);
        }

        // Send an impulse
        let (l_out, r_out) = kernel.process_stereo(1.0, 1.0, &params);

        // Left passes through immediately; right is delayed so should be ~0 here
        assert!(
            l_out.abs() > 0.1,
            "Left channel should pass impulse immediately, got {l_out}"
        );
        assert!(
            r_out.abs() < 0.05,
            "Right channel should be near zero before delay elapses, got {r_out}"
        );
    }

    /// Bass mono crossover should equalize low-frequency L/R difference.
    ///
    /// With `bass_mono_hz = 200 Hz` and a 100 Hz tone with opposite polarity
    /// on each channel, the bass mono stage should reduce the difference
    /// between L and R at the bass frequency.
    #[test]
    fn bass_mono_reduces_low_frequency_difference() {
        let sr = 48000.0;
        let test_freq = 60.0_f32; // well below 200 Hz crossover

        let no_bass_mono = StereoWidenerParams {
            bass_mono_hz: 0.0,
            ..Default::default()
        };
        let with_bass_mono = StereoWidenerParams {
            bass_mono_hz: 200.0,
            ..Default::default()
        };

        let mut kernel_no = StereoWidenerKernel::new(sr);
        let mut kernel_bm = StereoWidenerKernel::new(sr);

        // Warm up
        for i in 0..512 {
            let t = i as f32 / sr;
            let s = libm::sinf(2.0 * core::f32::consts::PI * test_freq * t);
            kernel_no.process_stereo(s, -s, &no_bass_mono);
            kernel_bm.process_stereo(s, -s, &with_bass_mono);
        }

        // Measure L−R difference energy
        let mut diff_no = 0.0_f32;
        let mut diff_bm = 0.0_f32;
        for i in 512..1024 {
            let t = i as f32 / sr;
            let s = libm::sinf(2.0 * core::f32::consts::PI * test_freq * t);
            let (ln, rn) = kernel_no.process_stereo(s, -s, &no_bass_mono);
            let (lb, rb) = kernel_bm.process_stereo(s, -s, &with_bass_mono);
            let d_no = (ln - rn).abs();
            let d_bm = (lb - rb).abs();
            diff_no += d_no;
            diff_bm += d_bm;
        }

        assert!(
            diff_bm < diff_no,
            "Bass mono should reduce L−R difference at {test_freq} Hz: no_mono={diff_no:.4}, with_mono={diff_bm:.4}"
        );
    }
}
