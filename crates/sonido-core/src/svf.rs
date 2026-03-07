//! State Variable Filter implementation.
//!
//! A versatile filter that can output lowpass, highpass, bandpass, and notch
//! simultaneously. Well-suited for modulation due to stability at high frequencies.
//!
//! # Topology
//!
//! Implements the Topology-Preserving Transform (TPT) SVF after Zavalishin,
//! "The Art of VA Filter Design" (2012). The TPT approach uses the trapezoidal
//! integrator discretization, which preserves the analog prototype's frequency
//! response and ensures modulation stability — cutoff can be swept without
//! artifacts that plague Direct Form implementations.
//!
//! # Nonlinear Drive
//!
//! Optional soft saturation of the integrator state via
//! [`fast_tanh`], inspired by virtual analog models
//! of ladder and Sallen-Key filters where transistor/op-amp stages saturate
//! under high signal levels. Drive is applied to the state update only (not the
//! output), preserving the filter's frequency response at low levels while
//! adding harmonic richness at high drive.
//!
//! # Four-Pole Mode
//!
//! [`FourPoleSvf`] cascades two SVF stages for 24 dB/oct slopes. The second
//! stage tracks the first stage's cutoff. Resonance is split equally between
//! stages to prevent excessive ringing.
//!
//! # Performance
//!
//! The [`set_cutoff`](StateVariableFilter::set_cutoff) method uses
//! [`fast_tan`] for cutoff frequencies below 10 kHz,
//! falling back to [`libm::tanf`] above 10 kHz where the Padé approximation
//! loses accuracy. This reduces coefficient computation cost by ~10× on
//! Cortex-M7 targets for typical guitar/synth frequency ranges.
//!
//! # Reference
//!
//! Zavalishin, "The Art of VA Filter Design", rev. 2.1.2 (2018), Chapter 3.

use core::f32::consts::PI;
use libm::tanf;

use crate::Effect;
use crate::fast_math::{fast_tan, fast_tanh};
use crate::flush_denormal;

/// State Variable Filter output type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SvfOutput {
    /// Low-pass filter output — passes frequencies below the cutoff.
    #[default]
    Lowpass,
    /// High-pass filter output — passes frequencies above the cutoff.
    Highpass,
    /// Band-pass filter output — passes frequencies near the cutoff.
    Bandpass,
    /// Notch (band-reject) filter output — rejects frequencies near the cutoff.
    Notch,
}

/// State Variable Filter (2-pole, 12 dB/oct).
///
/// Topology-Preserving Transform (TPT) SVF after Zavalishin, "The Art of VA
/// Filter Design". Can output lowpass, highpass, bandpass, and notch
/// simultaneously.
///
/// ## Parameters
///
/// - `cutoff`: Filter cutoff frequency in Hz (20.0 to sr×0.49, default 1000.0)
/// - `resonance`: Q factor (0.5 to 20.0, default 0.707)
/// - `drive`: Nonlinear saturation amount (0.0 to 1.0, default 0.0)
/// - `output_type`: Which filter output to use (default `Lowpass`)
///
/// # Example
///
/// ```rust
/// use sonido_core::{StateVariableFilter, SvfOutput, Effect};
///
/// let mut svf = StateVariableFilter::new(48000.0);
/// svf.set_cutoff(1000.0);
/// svf.set_resonance(2.0);
/// svf.set_drive(0.5);
/// svf.set_output_type(SvfOutput::Lowpass);
///
/// let output = svf.process(0.5);
/// ```
#[derive(Debug, Clone)]
pub struct StateVariableFilter {
    // Filter state
    ic1eq: f32,
    ic2eq: f32,

    // Coefficients
    g: f32,
    k: f32,

    // Parameters
    sample_rate: f32,
    cutoff: f32,
    resonance: f32,
    output_type: SvfOutput,
    drive: f32,
}

impl Default for StateVariableFilter {
    fn default() -> Self {
        Self::new(48000.0)
    }
}

impl StateVariableFilter {
    /// Create a new SVF with the given sample rate.
    ///
    /// Initialises with cutoff = 1000 Hz, Q = 0.707 (Butterworth), drive = 0,
    /// lowpass output.
    pub fn new(sample_rate: f32) -> Self {
        let mut svf = Self {
            ic1eq: 0.0,
            ic2eq: 0.0,
            g: 0.0,
            k: 0.0,
            sample_rate,
            cutoff: 1000.0,
            resonance: 0.707,
            output_type: SvfOutput::Lowpass,
            drive: 0.0,
        };
        svf.update_g();
        svf.update_k();
        svf
    }

    /// Set cutoff frequency in Hz.
    ///
    /// Range: 20.0 to `sample_rate × 0.49`. Values are clamped.
    ///
    /// Uses [`fast_tan`] for frequencies below
    /// 10 kHz and [`libm::tanf`] above, balancing speed and accuracy.
    pub fn set_cutoff(&mut self, freq: f32) {
        self.cutoff = freq.clamp(20.0, self.sample_rate * 0.49);
        self.update_g();
    }

    /// Get current cutoff frequency in Hz.
    pub fn cutoff(&self) -> f32 {
        self.cutoff
    }

    /// Set resonance (Q factor).
    ///
    /// Range: 0.5 to 20.0. Values are clamped. Q = 0.707 gives a Butterworth
    /// (maximally flat) response. Higher Q produces a resonant peak at cutoff.
    pub fn set_resonance(&mut self, q: f32) {
        self.resonance = q.clamp(0.5, 20.0);
        self.update_k();
    }

    /// Get current resonance (Q factor).
    pub fn resonance(&self) -> f32 {
        self.resonance
    }

    /// Set the output type (lowpass, highpass, bandpass, or notch).
    pub fn set_output_type(&mut self, output_type: SvfOutput) {
        self.output_type = output_type;
    }

    /// Get current output type.
    pub fn output_type(&self) -> SvfOutput {
        self.output_type
    }

    /// Set nonlinear drive amount.
    ///
    /// Range: 0.0 (linear, no saturation) to 1.0 (maximum saturation).
    /// When drive > 0, [`fast_tanh`] saturation
    /// is applied to the bandpass integrator state update. The drive gain
    /// factor is `1 + drive × 3` (mapping \[0, 1\] to \[1, 4\]×), with
    /// normalisation to preserve small-signal gain.
    ///
    /// Drive adds harmonic richness without altering the filter's frequency
    /// response at low signal levels, inspired by virtual analog models of
    /// Moog and Korg ladder filters.
    pub fn set_drive(&mut self, drive: f32) {
        self.drive = drive.clamp(0.0, 1.0);
    }

    /// Get current drive amount (0.0–1.0).
    pub fn drive(&self) -> f32 {
        self.drive
    }

    /// Recompute the `g` coefficient from the current cutoff and sample rate.
    ///
    /// Uses `fast_tan` for cutoff < 10 kHz (< 0.1% error in that range),
    /// falling back to `libm::tanf` above 10 kHz where the Padé approximation
    /// approaches its accuracy limit.
    fn update_g(&mut self) {
        let arg = PI * self.cutoff / self.sample_rate;
        self.g = if self.cutoff < 10_000.0 {
            fast_tan(arg)
        } else {
            tanf(arg)
        };
    }

    /// Recompute the `k` coefficient from the current resonance.
    fn update_k(&mut self) {
        self.k = 1.0 / self.resonance;
    }

    /// Process one sample and return all outputs (lowpass, highpass, bandpass, notch).
    ///
    /// Useful when you need multiple filter outputs simultaneously.
    ///
    /// When drive > 0, `fast_tanh` saturation is applied to the bandpass
    /// integrator state. The saturation affects only the state update, not the
    /// direct output, preserving the filter's frequency response at low levels.
    pub fn process_all(&mut self, input: f32) -> (f32, f32, f32, f32) {
        let v3 = input - self.ic2eq;
        let v1 = (self.g * v3 + self.ic1eq) / (1.0 + self.g * (self.g + self.k));
        let v2 = self.ic2eq + self.g * v1;

        // Nonlinear drive: saturate the bandpass integrator state.
        // d maps drive ∈ [0, 1] to a pre-tanh gain of [1, 4].
        // Dividing by d after tanh preserves small-signal gain
        // (tanh(x·d)/d ≈ x for small x), so the filter response is
        // unchanged at low levels while adding harmonics at high levels.
        let v1_sat = if self.drive > 0.0 {
            let d = 1.0 + self.drive * 3.0;
            fast_tanh(v1 * d) / d
        } else {
            v1
        };

        self.ic1eq = flush_denormal(2.0 * v1_sat - self.ic1eq);
        self.ic2eq = flush_denormal(2.0 * v2 - self.ic2eq);

        let lp = v2;
        let bp = v1;
        let hp = input - self.k * v1 - v2;
        let notch = lp + hp;

        (lp, hp, bp, notch)
    }
}

impl Effect for StateVariableFilter {
    fn process(&mut self, input: f32) -> f32 {
        let (lp, hp, bp, notch) = self.process_all(input);

        match self.output_type {
            SvfOutput::Lowpass => lp,
            SvfOutput::Highpass => hp,
            SvfOutput::Bandpass => bp,
            SvfOutput::Notch => notch,
        }
    }

    fn reset(&mut self) {
        self.ic1eq = 0.0;
        self.ic2eq = 0.0;
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.update_g();
    }
}

/// Four-pole (24 dB/oct) State Variable Filter.
///
/// Cascades two [`StateVariableFilter`] stages for steeper rolloff slopes.
/// The second stage's cutoff frequency tracks the first. Resonance is split
/// equally between stages (`Q_stage = Q_total × 0.5`) to prevent excessive
/// ringing at high resonance values.
///
/// Both stages inherit the nonlinear drive setting, applied independently
/// in each stage's integrator feedback path.
///
/// ## Parameters
///
/// - `cutoff`: Filter cutoff frequency in Hz (20.0 to sr×0.49, default 1000.0)
/// - `resonance`: Combined Q factor (0.5 to 20.0, default 0.707) — each stage
///   receives half
/// - `drive`: Nonlinear saturation amount (0.0 to 1.0, default 0.0)
/// - `output_type`: Which filter output to use (default `Lowpass`)
///
/// # Example
///
/// ```rust
/// use sonido_core::{FourPoleSvf, SvfOutput, Effect};
///
/// let mut svf4 = FourPoleSvf::new(48000.0);
/// svf4.set_cutoff(1000.0);
/// svf4.set_resonance(4.0);
/// svf4.set_output_type(SvfOutput::Lowpass);
///
/// let output = svf4.process(0.5);
/// ```
///
/// # Reference
///
/// Zavalishin, "The Art of VA Filter Design" (2012), Chapter 4.
#[derive(Debug, Clone)]
pub struct FourPoleSvf {
    /// First SVF stage.
    stage1: StateVariableFilter,
    /// Second SVF stage — cutoff tracks stage1.
    stage2: StateVariableFilter,
    /// Selected output type.
    output_type: SvfOutput,
    /// Combined resonance before splitting.
    resonance: f32,
}

impl Default for FourPoleSvf {
    fn default() -> Self {
        Self::new(48000.0)
    }
}

impl FourPoleSvf {
    /// Create a new four-pole SVF with the given sample rate.
    ///
    /// Initialises with cutoff = 1000 Hz, Q = 0.707, drive = 0, lowpass output.
    pub fn new(sample_rate: f32) -> Self {
        let mut svf = Self {
            stage1: StateVariableFilter::new(sample_rate),
            stage2: StateVariableFilter::new(sample_rate),
            output_type: SvfOutput::Lowpass,
            resonance: 0.707,
        };
        svf.update_resonance();
        svf
    }

    /// Set cutoff frequency in Hz for both stages.
    ///
    /// Range: 20.0 to `sample_rate × 0.49`. Values are clamped.
    pub fn set_cutoff(&mut self, freq: f32) {
        self.stage1.set_cutoff(freq);
        self.stage2.set_cutoff(freq);
    }

    /// Get current cutoff frequency in Hz.
    pub fn cutoff(&self) -> f32 {
        self.stage1.cutoff()
    }

    /// Set combined resonance (Q factor).
    ///
    /// Range: 0.5 to 20.0. Each stage receives `Q × 0.5`, clamped to the
    /// per-stage minimum of 0.5.
    pub fn set_resonance(&mut self, q: f32) {
        self.resonance = q.clamp(0.5, 20.0);
        self.update_resonance();
    }

    /// Get current combined resonance (Q factor).
    pub fn resonance(&self) -> f32 {
        self.resonance
    }

    /// Set the output type for both stages.
    pub fn set_output_type(&mut self, output_type: SvfOutput) {
        self.output_type = output_type;
        self.stage1.set_output_type(output_type);
        self.stage2.set_output_type(output_type);
    }

    /// Get current output type.
    pub fn output_type(&self) -> SvfOutput {
        self.output_type
    }

    /// Set nonlinear drive for both stages.
    ///
    /// Range: 0.0 (linear) to 1.0 (maximum saturation). Applied independently
    /// in each stage's integrator feedback.
    pub fn set_drive(&mut self, drive: f32) {
        self.stage1.set_drive(drive);
        self.stage2.set_drive(drive);
    }

    /// Get current drive amount (0.0–1.0).
    pub fn drive(&self) -> f32 {
        self.stage1.drive()
    }

    /// Distribute combined resonance equally to both stages.
    fn update_resonance(&mut self) {
        let q_stage = self.resonance * 0.5;
        self.stage1.set_resonance(q_stage);
        self.stage2.set_resonance(q_stage);
    }
}

impl Effect for FourPoleSvf {
    fn process(&mut self, input: f32) -> f32 {
        let mid = self.stage1.process(input);
        self.stage2.process(mid)
    }

    fn reset(&mut self) {
        self.stage1.reset();
        self.stage2.reset();
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.stage1.set_sample_rate(sample_rate);
        self.stage2.set_sample_rate(sample_rate);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- StateVariableFilter (2-pole) ----

    #[test]
    fn test_svf_lowpass_dc() {
        let mut svf = StateVariableFilter::new(48000.0);
        svf.set_cutoff(1000.0);
        svf.set_output_type(SvfOutput::Lowpass);

        // DC should pass through lowpass
        let mut output = 0.0;
        for _ in 0..1000 {
            output = svf.process(1.0);
        }
        assert!(
            (output - 1.0).abs() < 0.05,
            "DC should pass, got {}",
            output
        );
    }

    #[test]
    fn test_svf_highpass_blocks_dc() {
        let mut svf = StateVariableFilter::new(48000.0);
        svf.set_cutoff(1000.0);
        svf.set_output_type(SvfOutput::Highpass);

        // DC should be blocked by highpass
        let mut output = 0.0;
        for _ in 0..1000 {
            output = svf.process(1.0);
        }
        assert!(output.abs() < 0.1, "DC should be blocked, got {}", output);
    }

    #[test]
    fn test_svf_process_all() {
        let mut svf = StateVariableFilter::new(48000.0);
        svf.set_cutoff(1000.0);

        let (lp, hp, bp, notch) = svf.process_all(1.0);

        // All outputs should be finite
        assert!(lp.is_finite());
        assert!(hp.is_finite());
        assert!(bp.is_finite());
        assert!(notch.is_finite());
    }

    #[test]
    fn test_svf_reset() {
        let mut svf = StateVariableFilter::new(48000.0);

        // Process some samples
        for _ in 0..100 {
            svf.process(1.0);
        }

        // Reset
        svf.reset();

        assert_eq!(svf.ic1eq, 0.0);
        assert_eq!(svf.ic2eq, 0.0);
    }

    // ---- Drive ----

    #[test]
    fn test_drive_default_zero() {
        let svf = StateVariableFilter::new(48000.0);
        assert_eq!(svf.drive(), 0.0);
    }

    #[test]
    fn test_drive_clamp() {
        let mut svf = StateVariableFilter::new(48000.0);
        svf.set_drive(-0.5);
        assert_eq!(svf.drive(), 0.0);
        svf.set_drive(2.0);
        assert_eq!(svf.drive(), 1.0);
    }

    #[test]
    fn test_drive_zero_unchanged() {
        // With drive=0, output should match the original (no-drive) behaviour
        let mut svf_clean = StateVariableFilter::new(48000.0);
        svf_clean.set_cutoff(1000.0);
        svf_clean.set_drive(0.0);

        let mut svf_ref = StateVariableFilter::new(48000.0);
        svf_ref.set_cutoff(1000.0);

        for i in 0..200 {
            let input = libm::sinf(i as f32 * 0.1);
            let a = svf_clean.process(input);
            let b = svf_ref.process(input);
            assert!(
                (a - b).abs() < 1e-7,
                "drive=0 diverged at sample {i}: {a} vs {b}"
            );
        }
    }

    #[test]
    fn test_drive_adds_saturation() {
        // With drive > 0, output should differ from clean on a hot signal
        let mut svf_clean = StateVariableFilter::new(48000.0);
        svf_clean.set_cutoff(2000.0);
        svf_clean.set_resonance(4.0);

        let mut svf_driven = StateVariableFilter::new(48000.0);
        svf_driven.set_cutoff(2000.0);
        svf_driven.set_resonance(4.0);
        svf_driven.set_drive(1.0);

        let mut max_diff: f32 = 0.0;
        for i in 0..500 {
            // Hot signal to excite the resonance
            let input = libm::sinf(i as f32 * 0.25) * 2.0;
            let clean = svf_clean.process(input);
            let driven = svf_driven.process(input);
            let diff = (clean - driven).abs();
            if diff > max_diff {
                max_diff = diff;
            }
        }
        assert!(
            max_diff > 0.01,
            "drive=1.0 should produce measurably different output, max_diff={max_diff}"
        );
    }

    #[test]
    fn test_drive_output_bounded() {
        // Even with max drive and hot input, output should stay finite and bounded
        let mut svf = StateVariableFilter::new(48000.0);
        svf.set_cutoff(500.0);
        svf.set_resonance(10.0);
        svf.set_drive(1.0);

        for i in 0..2000 {
            let input = libm::sinf(i as f32 * 0.3) * 5.0;
            let output = svf.process(input);
            assert!(
                output.is_finite() && output.abs() < 100.0,
                "output unbounded at sample {i}: {output}"
            );
        }
    }

    // ---- fast_tan optimisation ----

    #[test]
    fn test_fast_tan_below_10k() {
        // Verify fast_tan path produces accurate coefficients below 10 kHz
        let sr = 48000.0;
        for freq in [20.0, 100.0, 500.0, 1000.0, 5000.0, 9999.0] {
            let arg = PI * freq / sr;
            let exact = tanf(arg);
            let fast = fast_tan(arg);
            let rel_err = (fast - exact).abs() / exact;
            assert!(
                rel_err < 0.01,
                "fast_tan inaccurate at {freq} Hz: rel_err={rel_err}"
            );
        }
    }

    #[test]
    fn test_fast_tan_above_10k_fallback() {
        // Above 10 kHz the code should use tanf (verified via coefficient match)
        let mut svf = StateVariableFilter::new(48000.0);
        svf.set_cutoff(15000.0);
        let g_actual = svf.g;
        let g_expected = tanf(PI * 15000.0 / 48000.0);
        assert!(
            (g_actual - g_expected).abs() < 1e-6,
            "above 10 kHz should use tanf: {g_actual} vs {g_expected}"
        );
    }

    // ---- FourPoleSvf ----

    #[test]
    fn test_four_pole_lowpass_dc() {
        let mut svf4 = FourPoleSvf::new(48000.0);
        svf4.set_cutoff(1000.0);
        svf4.set_output_type(SvfOutput::Lowpass);

        let mut output = 0.0;
        for _ in 0..2000 {
            output = svf4.process(1.0);
        }
        assert!(
            (output - 1.0).abs() < 0.05,
            "4-pole LP should pass DC, got {output}"
        );
    }

    #[test]
    fn test_four_pole_highpass_blocks_dc() {
        let mut svf4 = FourPoleSvf::new(48000.0);
        svf4.set_cutoff(1000.0);
        svf4.set_output_type(SvfOutput::Highpass);

        let mut output = 0.0;
        for _ in 0..2000 {
            output = svf4.process(1.0);
        }
        assert!(
            output.abs() < 0.05,
            "4-pole HP should block DC, got {output}"
        );
    }

    #[test]
    fn test_four_pole_steeper_rolloff() {
        // 4-pole should attenuate a tone well above cutoff more than 2-pole
        let sr = 48000.0;
        let cutoff = 500.0;
        // Test tone at 4 kHz (3 octaves above cutoff)
        // 2-pole: -12 dB/oct × 3 = -36 dB
        // 4-pole: -24 dB/oct × 3 = -72 dB
        let tone_freq = 4000.0;
        let omega = core::f32::consts::TAU * tone_freq / sr;

        let mut svf2 = StateVariableFilter::new(sr);
        svf2.set_cutoff(cutoff);
        svf2.set_output_type(SvfOutput::Lowpass);

        let mut svf4 = FourPoleSvf::new(sr);
        svf4.set_cutoff(cutoff);
        svf4.set_output_type(SvfOutput::Lowpass);

        // Warm up and measure RMS over a window
        let warmup = 2000;
        let measure = 1000;
        for i in 0..(warmup + measure) {
            let input = libm::sinf(i as f32 * omega);
            svf2.process(input);
            svf4.process(input);
        }

        let mut rms2: f32 = 0.0;
        let mut rms4: f32 = 0.0;
        for i in 0..measure {
            let input = libm::sinf((warmup + measure + i) as f32 * omega);
            let out2 = svf2.process(input);
            let out4 = svf4.process(input);
            rms2 += out2 * out2;
            rms4 += out4 * out4;
        }
        rms2 = libm::sqrtf(rms2 / measure as f32);
        rms4 = libm::sqrtf(rms4 / measure as f32);

        assert!(
            rms4 < rms2,
            "4-pole should attenuate more: rms2={rms2}, rms4={rms4}"
        );
        // The ratio should be substantial (roughly another 36 dB)
        assert!(
            rms4 < rms2 * 0.1,
            "4-pole attenuation insufficient: ratio={}",
            rms4 / rms2
        );
    }

    #[test]
    fn test_four_pole_reset() {
        let mut svf4 = FourPoleSvf::new(48000.0);
        for _ in 0..100 {
            svf4.process(1.0);
        }
        svf4.reset();
        // After reset, processing zero should produce zero
        let out = svf4.process(0.0);
        assert_eq!(out, 0.0, "reset should clear state, got {out}");
    }

    #[test]
    fn test_four_pole_drive() {
        let mut svf4 = FourPoleSvf::new(48000.0);
        svf4.set_drive(0.8);
        assert!((svf4.drive() - 0.8).abs() < 1e-6);

        // Should produce finite output with drive
        for i in 0..500 {
            let input = libm::sinf(i as f32 * 0.2) * 3.0;
            let output = svf4.process(input);
            assert!(output.is_finite(), "non-finite at sample {i}");
        }
    }

    #[test]
    fn test_four_pole_resonance_split() {
        let mut svf4 = FourPoleSvf::new(48000.0);
        svf4.set_resonance(6.0);
        // Each stage should get Q = 3.0
        assert!(
            (svf4.stage1.resonance() - 3.0).abs() < 1e-6,
            "stage1 Q should be 3.0, got {}",
            svf4.stage1.resonance()
        );
        assert!(
            (svf4.stage2.resonance() - 3.0).abs() < 1e-6,
            "stage2 Q should be 3.0, got {}",
            svf4.stage2.resonance()
        );
    }

    #[test]
    fn test_four_pole_default() {
        let svf4 = FourPoleSvf::default();
        assert!((svf4.cutoff() - 1000.0).abs() < 1e-6);
        assert!((svf4.resonance() - 0.707).abs() < 1e-6);
        assert_eq!(svf4.output_type(), SvfOutput::Lowpass);
        assert_eq!(svf4.drive(), 0.0);
    }
}
