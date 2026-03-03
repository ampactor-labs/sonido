//! Limiter kernel — lookahead brickwall limiter with auto-release.
//!
//! `LimiterKernel` owns DSP state (circular delay buffers, gain reduction
//! envelope, cached coefficients). Parameters are received via `&LimiterParams`
//! each sample. Deployed via [`KernelAdapter`](sonido_core::KernelAdapter) for
//! desktop/plugin, or called directly on embedded targets.
//!
//! # Algorithm
//!
//! 1. **Lookahead buffering**: Input is written to circular delay buffers for L and R.
//!    The audio output is read from the far end of the buffer (delayed by
//!    `lookahead_samples`), so gain reduction is applied before the transient arrives.
//! 2. **Peak detection**: The lookahead window is scanned every sample for the peak
//!    absolute amplitude across both channels (linked stereo: `max(|L|, |R|)`).
//!    This is `O(lookahead_samples)` per sample.
//! 3. **Gain computation**: If `peak > threshold_linear` the required gain factor is
//!    `G = (threshold / peak) * ceiling_linear`. Otherwise `G = ceiling_linear`.
//! 4. **Gain smoothing**: One-pole filter — instant attack (follow gain down immediately
//!    when a new peak requires more reduction), exponential release (follow gain back up
//!    slowly when the peak passes):
//!    - `if target < g[n-1]: g[n] = target`
//!    - `else: g[n] = α · g[n-1] + (1 − α) · target`
//!    where `α = exp(-1 / (release_ms · sr / 1000))`.
//! 5. **Output**: Delayed sample × smoothed gain × output_level.
//!
//! # Stereo Linking
//!
//! Peak detection uses `max(|L|, |R|)` so a transient on either channel causes
//! identical gain reduction on both. This preserves the stereo image under heavy
//! limiting.
//!
//! # Latency
//!
//! The kernel reports `lookahead_samples` as its processing latency. The
//! [`KernelAdapter`](sonido_core::KernelAdapter) forwards this to the host via
//! `Effect::latency_samples()`, enabling the host to compensate for the delay.
//!
//! # References
//!
//! - Giannoulis, Massberg & Reiss, "Digital Dynamic Range Compressor Design — A
//!   Tutorial and Analysis", JAES vol. 60 no. 6, 2012. Sections IV–V cover
//!   attack/release ballistics and the one-pole smoothing approach used here.
//! - Zölzer, "DAFX: Digital Audio Effects" (2nd ed.), Ch. 4 — brickwall limiter
//!   topology with lookahead.
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! let adapter = KernelAdapter::new(LimiterKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing, ADCs are hardware-filtered)
//! let mut kernel = LimiterKernel::new(48000.0);
//! let params = LimiterParams::from_knobs(adc_thresh, adc_ceil, adc_release, adc_look, adc_out);
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

#[cfg(not(feature = "std"))]
extern crate alloc;
#[cfg(not(feature = "std"))]
use alloc::vec;
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

use libm::{expf, fabsf};
use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::{ParamDescriptor, ParamId, fast_db_to_linear, math::db_to_linear};

/// Maximum lookahead in milliseconds — sizes the fixed delay buffers at construction.
///
/// Buffers are allocated once for `MAX_LOOKAHEAD_MS` worth of samples. Changing the
/// lookahead parameter at runtime only changes how far into the buffer the peak
/// scanner looks and how far back the read pointer sits; the buffer itself is not
/// reallocated.
const MAX_LOOKAHEAD_MS: f32 = 10.0;

/// Convert a millisecond duration to a sample count.
///
/// The conversion truncates — the result is a lower bound on the requested time.
#[inline]
fn ms_to_samples(ms: f32, sample_rate: f32) -> usize {
    ((ms * sample_rate) / 1000.0) as usize
}

/// Compute the one-pole release coefficient.
///
/// `α = exp(-1 / τ)` where `τ = release_ms · sample_rate / 1000`.
///
/// As `τ → 0` the coefficient → 0 (instant release / no memory).
/// As `τ → ∞` the coefficient → 1 (never releases).
/// When `τ < 1.0` (release time shorter than one sample) the coefficient is 0
/// to avoid division by zero.
///
/// Reference: Giannoulis, Massberg & Reiss, JAES 2012, eq. (12).
#[inline]
fn compute_release_coeff(release_ms: f32, sample_rate: f32) -> f32 {
    let tau = release_ms * sample_rate / 1000.0;
    if tau < 1.0 { 0.0 } else { expf(-1.0 / tau) }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`LimiterKernel`].
///
/// All values in **user-facing units** — the same units shown in GUIs and stored
/// in presets. The kernel converts internally as needed.
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `threshold_db` | dB | −30–0 | −6.0 |
/// | 1 | `ceiling_db` | dB | −30–0 | −0.3 |
/// | 2 | `release_ms` | ms | 10–500 | 100.0 |
/// | 3 | `lookahead_ms` | ms | 0–10 | 5.0 |
/// | 4 | `output_db` | dB | −20–+20 | 0.0 |
#[derive(Debug, Clone, Copy)]
pub struct LimiterParams {
    /// Threshold in decibels. Gain reduction engages above this level.
    ///
    /// Range: −30.0 to 0.0 dB.
    pub threshold_db: f32,
    /// Hard brickwall ceiling in decibels. Output never exceeds this level.
    ///
    /// Range: −30.0 to 0.0 dB.
    pub ceiling_db: f32,
    /// Exponential release time constant in milliseconds.
    ///
    /// Range: 10.0 to 500.0 ms. Shorter values release faster (may cause pumping);
    /// longer values are smoother.
    pub release_ms: f32,
    /// Lookahead delay in milliseconds.
    ///
    /// Range: 0.0 to 10.0 ms. Sets the processing latency reported to the host.
    /// A value of 0.0 disables lookahead (brickwall still guaranteed, but one-sample
    /// transient overshoot is possible before gain reduction engages).
    pub lookahead_ms: f32,
    /// Final output level trim in decibels.
    ///
    /// Range: −20.0 to +20.0 dB.
    pub output_db: f32,
}

impl Default for LimiterParams {
    /// Default values matching the descriptor defaults exactly.
    ///
    /// threshold −6 dB, ceiling −0.3 dB, release 100 ms, lookahead 5 ms, output 0 dB.
    fn default() -> Self {
        Self {
            threshold_db: -6.0,
            ceiling_db: -0.3,
            release_ms: 100.0,
            lookahead_ms: 5.0,
            output_db: 0.0,
        }
    }
}

impl LimiterParams {
    /// Build params directly from hardware knob readings (0.0–1.0 normalized).
    ///
    /// Convenience for embedded targets where ADC values map linearly to parameter
    /// ranges. Argument order follows the `KernelParams` index order.
    ///
    /// - `threshold`: 0.0 → −30 dB, 1.0 → 0 dB
    /// - `ceiling`: 0.0 → −30 dB, 1.0 → 0 dB
    /// - `release`: 0.0 → 10 ms, 1.0 → 500 ms
    /// - `lookahead`: 0.0 → 0 ms, 1.0 → 10 ms
    /// - `output`: 0.0 → −20 dB, 1.0 → +20 dB
    pub fn from_knobs(
        threshold: f32,
        ceiling: f32,
        release: f32,
        lookahead: f32,
        output: f32,
    ) -> Self {
        Self {
            threshold_db: threshold * 30.0 - 30.0, // −30–0 dB
            ceiling_db: ceiling * 30.0 - 30.0,     // −30–0 dB
            release_ms: release * 490.0 + 10.0,    // 10–500 ms
            lookahead_ms: lookahead * 10.0,        // 0–10 ms
            output_db: output * 40.0 - 20.0,       // −20–+20 dB
        }
    }
}

impl KernelParams for LimiterParams {
    const COUNT: usize = 5;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(
                ParamDescriptor::gain_db("Threshold", "Thresh", -30.0, 0.0, -6.0)
                    .with_id(ParamId(1600), "lim_thresh"),
            ),
            1 => Some(
                ParamDescriptor::gain_db("Ceiling", "Ceil", -30.0, 0.0, -0.3)
                    .with_id(ParamId(1601), "lim_ceil"),
            ),
            2 => Some(
                ParamDescriptor::time_ms("Release", "Rel", 10.0, 500.0, 100.0)
                    .with_id(ParamId(1602), "lim_release"),
            ),
            3 => Some(
                ParamDescriptor::time_ms("Lookahead", "Look", 0.0, 10.0, 5.0)
                    .with_id(ParamId(1603), "lim_look"),
            ),
            4 => Some(
                sonido_core::gain::output_param_descriptor().with_id(ParamId(1604), "lim_output"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Fast,     // threshold — fast for responsive feel
            1 => SmoothingStyle::Slow,     // ceiling — avoid audible steps in brickwall
            2 => SmoothingStyle::Slow,     // release — coefficient recalc, avoid zipper
            3 => SmoothingStyle::None,     // lookahead — buffer resize, snap only
            4 => SmoothingStyle::Standard, // output level
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.threshold_db,
            1 => self.ceiling_db,
            2 => self.release_ms,
            3 => self.lookahead_ms,
            4 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.threshold_db = value,
            1 => self.ceiling_db = value,
            2 => self.release_ms = value,
            3 => self.lookahead_ms = value,
            4 => self.output_db = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP brickwall lookahead limiter kernel.
///
/// Contains ONLY the mutable state required for audio processing:
/// - Circular delay buffers (L and R) sized for `MAX_LOOKAHEAD_MS`
/// - Current smoothed gain reduction (linear, 1.0 = no reduction)
/// - Cached release coefficient and lookahead sample count computed from params
/// - Write position within the circular buffer
///
/// No `SmoothedParam`, no atomics, no platform awareness.
///
/// ## DSP State
///
/// The circular buffers hold up to `MAX_LOOKAHEAD_MS` worth of input. At each
/// sample:
/// 1. New input is written at `write_pos`.
/// 2. The lookahead window `[write_pos, write_pos + lookahead_samples]` is scanned
///    for the peak absolute value across both channels.
/// 3. Gain reduction is computed and smoothed (instant attack, exponential release).
/// 4. The delayed output sample at `write_pos + lookahead_samples + 1` is read and
///    scaled by the smoothed gain and output level.
/// 5. `write_pos` advances.
///
/// ## Coefficient Caching
///
/// The release coefficient (`α`) and lookahead sample count are cached and
/// recomputed when the corresponding params change beyond a small epsilon. This
/// avoids per-sample `expf` calls during normal playback when params are stable.
pub struct LimiterKernel {
    /// Sample rate in Hz. Stored for coefficient recalculation on `set_sample_rate`.
    sample_rate: f32,

    /// Circular delay buffer for the left channel.
    ///
    /// Length is `max_lookahead_samples`. New input is written at `write_pos`;
    /// output is read at `(write_pos + lookahead_samples + 1) % len`.
    buffer_l: Vec<f32>,

    /// Circular delay buffer for the right channel.
    buffer_r: Vec<f32>,

    /// Current write position within the circular buffers.
    write_pos: usize,

    /// Current smoothed gain reduction (linear).
    ///
    /// Range: (0.0, 1.0]. 1.0 = no gain reduction. Updated per sample by the
    /// one-pole smoother: instant attack (follow down), exponential release
    /// (follow up slowly).
    gain_reduction: f32,

    /// Total buffer length in samples, corresponding to `MAX_LOOKAHEAD_MS`.
    max_lookahead_samples: usize,

    // ── Caches (recomputed when params change) ──
    /// One-pole release coefficient: `exp(-1 / (release_ms · sr / 1000))`.
    ///
    /// Cached to avoid calling `expf` every sample. Recomputed when
    /// `params.release_ms` differs from `last_release_ms` by more than 0.01 ms.
    release_coeff: f32,

    /// Lookahead window length in samples, derived from `params.lookahead_ms`.
    ///
    /// Cached to avoid per-sample floating-point division. Clamped to
    /// `max_lookahead_samples - 1` to prevent buffer overrun.
    lookahead_samples: usize,

    /// Last `release_ms` used to compute `release_coeff`. Tracks when a recompute is needed.
    last_release_ms: f32,

    /// Last `lookahead_ms` used to compute `lookahead_samples`. Tracks when a recompute is needed.
    last_lookahead_ms: f32,
}

impl LimiterKernel {
    /// Create a new limiter kernel initialised for the given sample rate.
    ///
    /// Buffers are allocated once for `MAX_LOOKAHEAD_MS` (10 ms) at the given
    /// sample rate. Default param values are used to prime the caches.
    pub fn new(sample_rate: f32) -> Self {
        let max_samples = ms_to_samples(MAX_LOOKAHEAD_MS, sample_rate);
        let defaults = LimiterParams::default();
        let lookahead_samples =
            ms_to_samples(defaults.lookahead_ms, sample_rate).min(max_samples.saturating_sub(1));
        let release_coeff = compute_release_coeff(defaults.release_ms, sample_rate);

        Self {
            sample_rate,
            buffer_l: vec![0.0; max_samples],
            buffer_r: vec![0.0; max_samples],
            write_pos: 0,
            gain_reduction: 1.0,
            max_lookahead_samples: max_samples,
            release_coeff,
            lookahead_samples,
            last_release_ms: defaults.release_ms,
            last_lookahead_ms: defaults.lookahead_ms,
        }
    }

    /// Scan one circular buffer for the peak absolute value within the lookahead window.
    ///
    /// The window starts at `write_pos` (the freshest sample, just written) and extends
    /// forward by `lookahead_samples` slots. The scan wraps around the circular buffer.
    ///
    /// Complexity: O(`lookahead_samples`).
    #[inline]
    fn scan_peak_mono(&self, buf: &[f32]) -> f32 {
        let len = buf.len();
        let mut peak = 0.0_f32;
        for i in 0..=self.lookahead_samples {
            let s = fabsf(buf[(self.write_pos + i) % len]);
            if s > peak {
                peak = s;
            }
        }
        peak
    }

    /// Compute the target gain factor given the detected peak and current params.
    ///
    /// If `peak > threshold` the gain factor brings the peak down to threshold and
    /// then scales to ceiling. Otherwise just the ceiling attenuation is returned.
    ///
    /// Formula: `G = min(threshold / peak, 1.0) * ceiling_linear` when
    /// `peak > threshold`, else `G = ceiling_linear`.
    ///
    /// The guard `peak > 1e-9` avoids division-by-zero on silence.
    #[inline]
    fn compute_target_gain(peak: f32, threshold_db: f32, ceiling_db: f32) -> f32 {
        let threshold_linear = db_to_linear(threshold_db);
        let ceiling_linear = db_to_linear(ceiling_db);

        if peak > threshold_linear && peak > 1e-9 {
            (threshold_linear / peak) * ceiling_linear
        } else {
            ceiling_linear
        }
    }
}

impl DspKernel for LimiterKernel {
    type Params = LimiterParams;

    /// Process a stereo sample pair through the brickwall lookahead limiter.
    ///
    /// ## Signal Flow
    ///
    /// ```text
    /// Input L/R → Circular Buffer → Peak Scan (lookahead window)
    ///                                     ↓
    ///                             Gain Computation
    ///                                     ↓
    ///                         One-pole Gain Smoother (instant attack, exp release)
    ///                                     ↓
    ///                        Delayed Output × Smoothed Gain × Output Level
    /// ```
    ///
    /// ## Coefficient Caching
    ///
    /// `release_coeff` and `lookahead_samples` are recomputed only when the
    /// corresponding params change beyond a small epsilon (0.01 ms / 0.001 ms
    /// respectively). This keeps the hot path free of expensive transcendentals.
    #[inline]
    fn process_stereo(&mut self, left: f32, right: f32, params: &LimiterParams) -> (f32, f32) {
        // ── Coefficient update (only when params change) ──
        if (params.release_ms - self.last_release_ms).abs() > 0.01 {
            self.release_coeff = compute_release_coeff(params.release_ms, self.sample_rate);
            self.last_release_ms = params.release_ms;
        }
        if (params.lookahead_ms - self.last_lookahead_ms).abs() > 0.001 {
            self.lookahead_samples = ms_to_samples(params.lookahead_ms, self.sample_rate)
                .min(self.max_lookahead_samples.saturating_sub(1));
            self.last_lookahead_ms = params.lookahead_ms;
        }

        let len = self.max_lookahead_samples;

        // ── Write new input into circular buffer ──
        self.buffer_l[self.write_pos] = left;
        self.buffer_r[self.write_pos] = right;

        // ── Peak detection — linked stereo: max(|L|, |R|) ──
        let peak_l = self.scan_peak_mono(&self.buffer_l);
        let peak_r = self.scan_peak_mono(&self.buffer_r);
        let peak = if peak_l > peak_r { peak_l } else { peak_r };

        // ── Target gain: threshold + ceiling limits ──
        let target = Self::compute_target_gain(peak, params.threshold_db, params.ceiling_db);

        // ── One-pole gain smoother ──
        // Instant attack: if a new peak demands more reduction, follow immediately.
        // Exponential release: when the peak passes, recover slowly.
        self.gain_reduction = if target < self.gain_reduction {
            target
        } else {
            self.release_coeff * self.gain_reduction + (1.0 - self.release_coeff) * target
        };

        // ── Read delayed sample from the output end of the buffer ──
        // The read pointer sits `lookahead_samples + 1` ahead of the write pointer
        // (wrapping), which is `lookahead_samples` samples behind the freshest input.
        let read_pos = (self.write_pos + self.lookahead_samples + 1) % len;
        let delayed_l = self.buffer_l[read_pos];
        let delayed_r = self.buffer_r[read_pos];

        // ── Advance write pointer ──
        self.write_pos = (self.write_pos + 1) % len;

        // ── Apply smoothed gain and output level ──
        let output_gain = fast_db_to_linear(params.output_db);
        let g = self.gain_reduction * output_gain;
        (delayed_l * g, delayed_r * g)
    }

    /// Reset all DSP state.
    ///
    /// Clears both delay buffers to silence, resets gain reduction to unity (1.0),
    /// and resets the write position to 0. Coefficient caches are NOT invalidated —
    /// they remain correct for the current sample rate.
    fn reset(&mut self) {
        self.buffer_l.fill(0.0);
        self.buffer_r.fill(0.0);
        self.write_pos = 0;
        self.gain_reduction = 1.0;
    }

    /// Update internal state for a new sample rate.
    ///
    /// Reallocates the circular buffers to the correct size for the new sample rate,
    /// recomputes the release coefficient and lookahead sample count from the cached
    /// `last_*` param values, and resets the write position.
    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;

        let max_samples = ms_to_samples(MAX_LOOKAHEAD_MS, sample_rate);
        self.max_lookahead_samples = max_samples;
        self.buffer_l = vec![0.0; max_samples];
        self.buffer_r = vec![0.0; max_samples];
        self.write_pos = 0;

        // Recompute caches with the stored param values.
        self.release_coeff = compute_release_coeff(self.last_release_ms, sample_rate);
        self.lookahead_samples =
            ms_to_samples(self.last_lookahead_ms, sample_rate).min(max_samples.saturating_sub(1));
    }

    /// Reports the current lookahead window as processing latency in samples.
    ///
    /// This value matches the delay introduced by the circular buffer. The host
    /// must compensate for this latency when aligning the limiter's output with
    /// other tracks.
    fn latency_samples(&self) -> usize {
        self.lookahead_samples
    }

    /// Returns `true` — the limiter uses linked stereo gain reduction.
    ///
    /// Peak detection uses `max(|L|, |R|)`, so both channels receive the same
    /// gain reduction envelope. Cross-channel interaction makes this true stereo.
    fn is_true_stereo(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_core::kernel::KernelAdapter;
    use sonido_core::{Effect, ParameterInfo};

    // ── Kernel unit tests ──

    #[test]
    fn silence_in_silence_out() {
        // A limiter fed silence must produce silence (no DC offsets, no
        // leakage from gain-reduction maths, no uninitialized buffer reads).
        let mut kernel = LimiterKernel::new(48000.0);
        let params = LimiterParams::default();

        // Process enough samples to fill the lookahead buffer.
        for _ in 0..512 {
            let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
            assert!(l.abs() < 1e-6, "Left silence violated: {l}");
            assert!(r.abs() < 1e-6, "Right silence violated: {r}");
        }
    }

    #[test]
    fn no_nan_or_inf() {
        // The limiter must never produce NaN or infinite values regardless of
        // input amplitude or parameter settings.
        let mut kernel = LimiterKernel::new(48000.0);
        let params = LimiterParams::default();

        for i in 0..1024 {
            // Vary input to exercise different gain reduction levels.
            let t = i as f32 * core::f32::consts::PI * 0.01;
            let input = libm::sinf(t) * 2.0; // intentionally louder than ceiling
            let (l, r) = kernel.process_stereo(input, -input, &params);
            assert!(!l.is_nan(), "Left NaN at sample {i}");
            assert!(!r.is_nan(), "Right NaN at sample {i}");
            assert!(l.is_finite(), "Left Inf at sample {i}");
            assert!(r.is_finite(), "Right Inf at sample {i}");
        }
    }

    #[test]
    fn params_descriptor_count() {
        // There must be exactly 5 parameters with valid descriptors.
        assert_eq!(LimiterParams::COUNT, 5);

        for i in 0..LimiterParams::COUNT {
            assert!(
                LimiterParams::descriptor(i).is_some(),
                "Missing descriptor at index {i}"
            );
        }
        assert!(
            LimiterParams::descriptor(LimiterParams::COUNT).is_none(),
            "descriptor() should return None past COUNT"
        );

        // Verify specific fields to catch copy-paste errors.
        let d0 = LimiterParams::descriptor(0).unwrap();
        assert_eq!(d0.name, "Threshold");
        assert_eq!(d0.min, -30.0);
        assert_eq!(d0.max, 0.0);
        assert!((d0.default - (-6.0)).abs() < 1e-5);
        assert_eq!(d0.id, ParamId(1600));

        let d1 = LimiterParams::descriptor(1).unwrap();
        assert_eq!(d1.name, "Ceiling");
        assert!((d1.default - (-0.3)).abs() < 1e-5);
        assert_eq!(d1.id, ParamId(1601));

        let d2 = LimiterParams::descriptor(2).unwrap();
        assert_eq!(d2.name, "Release");
        assert!((d2.default - 100.0).abs() < 1e-5);
        assert_eq!(d2.id, ParamId(1602));

        let d3 = LimiterParams::descriptor(3).unwrap();
        assert_eq!(d3.name, "Lookahead");
        assert!((d3.default - 5.0).abs() < 1e-5);
        assert_eq!(d3.id, ParamId(1603));

        let d4 = LimiterParams::descriptor(4).unwrap();
        assert_eq!(d4.name, "Output");
        assert!((d4.default - 0.0).abs() < 1e-5);
        assert_eq!(d4.id, ParamId(1604));
    }

    #[test]
    fn limits_loud_signal() {
        // After the lookahead buffer has filled, the output must be at or below
        // the ceiling level (the brickwall property).
        let mut kernel = LimiterKernel::new(48000.0);
        let params = LimiterParams {
            threshold_db: -6.0,
            ceiling_db: -0.3,
            output_db: 0.0,
            ..LimiterParams::default()
        };

        // -0.3 dB in linear
        let ceiling_linear = db_to_linear(params.ceiling_db);

        // Feed a full-scale signal for long enough to fill the buffer and let
        // gain reduction settle.
        let mut last_l = 0.0_f32;
        for _ in 0..2048 {
            let (l, _r) = kernel.process_stereo(1.0, 1.0, &params);
            last_l = l;
        }

        assert!(
            fabsf(last_l) <= ceiling_linear + 1e-4,
            "Output {last_l:.6} exceeds ceiling {ceiling_linear:.6}"
        );
    }

    #[test]
    fn quiet_signal_passes_through() {
        // A signal well below threshold should pass through with only the ceiling
        // attenuation applied (no gain reduction beyond the ceiling offset).
        let mut kernel = LimiterKernel::new(48000.0);
        let params = LimiterParams {
            threshold_db: -6.0,
            ceiling_db: -0.3,
            output_db: 0.0,
            ..LimiterParams::default()
        };

        // -30 dB ≈ 0.0316 — well below the -6 dB threshold.
        let quiet = db_to_linear(-30.0);
        let ceiling_linear = db_to_linear(params.ceiling_db);
        let expected = quiet * ceiling_linear;

        let mut last_l = 0.0_f32;
        for _ in 0..2048 {
            let (l, _r) = kernel.process_stereo(quiet, quiet, &params);
            last_l = l;
        }

        assert!(
            (fabsf(last_l) - expected).abs() < 0.01,
            "Quiet signal: expected ~{expected:.6}, got {last_l:.6}"
        );
    }

    #[test]
    fn stereo_linked_gain_reduction() {
        // A loud signal on the left channel must also reduce the right channel.
        // Without linking, the quiet right channel would be unaffected.
        let mut kernel = LimiterKernel::new(48000.0);
        let params = LimiterParams {
            threshold_db: -6.0,
            ceiling_db: -0.3,
            output_db: 0.0,
            ..LimiterParams::default()
        };

        let ceiling_linear = db_to_linear(params.ceiling_db);
        let quiet_in = 0.01_f32;
        // Without linking, quiet right through ceiling would be:
        let unlinked_r = quiet_in * ceiling_linear;

        let mut last_r = 0.0_f32;
        for _ in 0..2048 {
            // Left is loud (full scale), right is quiet.
            let (_l, r) = kernel.process_stereo(1.0, quiet_in, &params);
            last_r = r;
        }

        // The right output must be reduced BELOW the unlinked level, proving
        // the left channel's peak drove gain reduction on the right too.
        assert!(
            fabsf(last_r) < unlinked_r,
            "Right channel {last_r:.6} should be below unlinked level {unlinked_r:.6}"
        );
    }

    #[test]
    fn latency_reports_lookahead() {
        // The kernel's latency_samples() must match the lookahead in samples.
        let kernel = LimiterKernel::new(48000.0);
        // Default lookahead = 5 ms at 48000 Hz = 240 samples.
        let expected = (5.0_f32 * 48000.0 / 1000.0) as usize;
        assert_eq!(kernel.latency_samples(), expected);
    }

    #[test]
    fn reset_clears_state() {
        let mut kernel = LimiterKernel::new(48000.0);
        let params = LimiterParams::default();

        // Drive the limiter hard to fill buffers and push gain reduction down.
        for _ in 0..512 {
            kernel.process_stereo(1.0, 1.0, &params);
        }

        kernel.reset();

        assert!(
            (kernel.gain_reduction - 1.0).abs() < 1e-6,
            "gain_reduction should be 1.0 after reset, got {}",
            kernel.gain_reduction
        );
        assert!(
            kernel.buffer_l.iter().all(|&s| s == 0.0),
            "buffer_l not cleared after reset"
        );
        assert!(
            kernel.buffer_r.iter().all(|&s| s == 0.0),
            "buffer_r not cleared after reset"
        );
        assert_eq!(kernel.write_pos, 0, "write_pos should be 0 after reset");
    }

    // ── Adapter integration tests ──

    #[test]
    fn adapter_wraps_as_effect() {
        // KernelAdapter must expose the limiter as a standard Effect.
        let kernel = LimiterKernel::new(48000.0);
        let mut adapter = KernelAdapter::new(kernel, 48000.0);

        adapter.reset();
        let output = adapter.process(0.3);
        assert!(!output.is_nan(), "Adapter output is NaN");
        assert!(output.is_finite(), "Adapter output is Inf");
    }

    #[test]
    fn adapter_param_info_matches() {
        // The adapter's ParameterInfo must reflect LimiterParams exactly.
        let kernel = LimiterKernel::new(48000.0);
        let adapter = KernelAdapter::new(kernel, 48000.0);

        assert_eq!(adapter.param_count(), 5);

        assert_eq!(adapter.param_info(0).unwrap().name, "Threshold");
        assert_eq!(adapter.param_info(1).unwrap().name, "Ceiling");
        assert_eq!(adapter.param_info(2).unwrap().name, "Release");
        assert_eq!(adapter.param_info(3).unwrap().name, "Lookahead");
        assert_eq!(adapter.param_info(4).unwrap().name, "Output");
        assert!(adapter.param_info(5).is_none());

        // ParamIds must match the classic Limiter effect exactly.
        assert_eq!(adapter.param_info(0).unwrap().id, ParamId(1600));
        assert_eq!(adapter.param_info(1).unwrap().id, ParamId(1601));
        assert_eq!(adapter.param_info(2).unwrap().id, ParamId(1602));
        assert_eq!(adapter.param_info(3).unwrap().id, ParamId(1603));
        assert_eq!(adapter.param_info(4).unwrap().id, ParamId(1604));
    }

    #[test]
    fn morph_produces_valid_output() {
        // lerp() between any two LimiterParams snapshots must produce only
        // finite output — no NaN or Inf at any morph position.
        let transparent = LimiterParams {
            threshold_db: 0.0,
            ceiling_db: 0.0,
            release_ms: 500.0,
            lookahead_ms: 0.0,
            output_db: 0.0,
        };
        let aggressive = LimiterParams {
            threshold_db: -20.0,
            ceiling_db: -6.0,
            release_ms: 10.0,
            lookahead_ms: 10.0,
            output_db: -6.0,
        };

        let mut kernel = LimiterKernel::new(48000.0);

        for i in 0..=10 {
            let t = i as f32 / 10.0;
            let morphed = LimiterParams::lerp(&transparent, &aggressive, t);
            let (l, r) = kernel.process_stereo(0.3, -0.3, &morphed);
            assert!(l.is_finite(), "Left NaN/Inf at morph t={t:.1}: {l}");
            assert!(r.is_finite(), "Right NaN/Inf at morph t={t:.1}: {r}");
            kernel.reset();
        }
    }

    #[test]
    fn from_knobs_maps_range() {
        // from_knobs(0, 0, 0, 0, 0) → minimum values.
        let min = LimiterParams::from_knobs(0.0, 0.0, 0.0, 0.0, 0.0);
        assert!((min.threshold_db - (-30.0)).abs() < 0.01, "min threshold");
        assert!((min.ceiling_db - (-30.0)).abs() < 0.01, "min ceiling");
        assert!((min.release_ms - 10.0).abs() < 0.01, "min release");
        assert!((min.lookahead_ms - 0.0).abs() < 0.01, "min lookahead");
        assert!((min.output_db - (-20.0)).abs() < 0.01, "min output");

        // from_knobs(1, 1, 1, 1, 1) → maximum values.
        let max = LimiterParams::from_knobs(1.0, 1.0, 1.0, 1.0, 1.0);
        assert!((max.threshold_db - 0.0).abs() < 0.01, "max threshold");
        assert!((max.ceiling_db - 0.0).abs() < 0.01, "max ceiling");
        assert!((max.release_ms - 500.0).abs() < 0.01, "max release");
        assert!((max.lookahead_ms - 10.0).abs() < 0.01, "max lookahead");
        assert!((max.output_db - 20.0).abs() < 0.01, "max output");
    }
}
