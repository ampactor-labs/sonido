//! Parameter handling with smoothing for zipper-free changes.
//!
//! Audio parameters (gain, frequency, etc.) need smooth transitions to avoid
//! audible "zipper noise" when values change. This module provides
//! [`SmoothedParam`] for sample-accurate parameter smoothing.
//!
//! ## Smoothing Methods
//!
//! - **Exponential (one-pole lowpass)**: Natural decay, good for most params
//! - **Linear**: Constant rate of change, good for crossfades
//!
//! ## Usage
//!
//! ```rust
//! use sonido_core::SmoothedParam;
//!
//! let mut gain = SmoothedParam::new(1.0);
//! gain.set_sample_rate(48000.0);
//! gain.set_smoothing_time_ms(10.0); // 10ms smoothing
//!
//! // Set new target - smoothing happens automatically
//! gain.set_target(0.5);
//!
//! // In audio callback, get smoothed value each sample
//! for _ in 0..480 { // 10ms at 48kHz
//!     let smoothed_gain = gain.advance();
//!     // Use smoothed_gain for processing...
//! }
//! ```

use libm::expf;

/// A parameter with built-in smoothing for zipper-free changes.
///
/// Uses exponential smoothing (one-pole lowpass) by default, which provides
/// natural-sounding transitions for most audio parameters.
///
/// # Memory Layout
///
/// Designed to be cache-friendly with all fields in a single 24-byte struct
/// (on 64-bit systems with default alignment).
#[derive(Debug, Clone)]
pub struct SmoothedParam {
    /// Current smoothed value
    current: f32,
    /// Target value we're smoothing towards
    target: f32,
    /// Smoothing coefficient (0 = instant, ~1 = very slow)
    coeff: f32,
    /// Sample rate in Hz
    sample_rate: f32,
    /// Smoothing time in milliseconds
    smoothing_time_ms: f32,
    /// True when current ≈ target — skips interpolation math in `advance()`.
    settled: bool,
}

/// Whether a smoothed value is close enough to its target to stop: within one
/// part in a million of the target's magnitude, and never tighter than 1e-6.
#[inline]
fn close_enough(current: f32, target: f32) -> bool {
    (current - target).abs() <= 1e-6 * target.abs().max(1.0)
}

impl SmoothedParam {
    /// Create a new smoothed parameter with initial value.
    ///
    /// Smoothing is disabled by default (instant changes). Call
    /// [`set_sample_rate`](Self::set_sample_rate) and
    /// [`set_smoothing_time_ms`](Self::set_smoothing_time_ms) to enable.
    ///
    /// # Arguments
    /// * `initial` - Initial parameter value
    pub fn new(initial: f32) -> Self {
        Self {
            current: initial,
            target: initial,
            coeff: 0.0, // No smoothing until configured
            sample_rate: 44100.0,
            smoothing_time_ms: 0.0,
            settled: true,
        }
    }

    /// Create a smoothed parameter with full configuration.
    ///
    /// # Arguments
    /// * `initial` - Initial parameter value
    /// * `sample_rate` - Sample rate in Hz
    /// * `smoothing_time_ms` - Smoothing time constant in milliseconds
    pub fn with_config(initial: f32, sample_rate: f32, smoothing_time_ms: f32) -> Self {
        let mut param = Self::new(initial);
        param.sample_rate = sample_rate;
        param.smoothing_time_ms = smoothing_time_ms;
        param.recalculate_coeff();
        param
    }

    /// 5 ms smoothing — drive, nonlinear gain parameters.
    ///
    /// Fast response avoids audible latency on parameters that affect
    /// waveshaping and saturation characteristics.
    pub fn fast(initial: f32, sample_rate: f32) -> Self {
        Self::with_config(initial, sample_rate, 5.0)
    }

    /// 10 ms smoothing — most parameters (rate, depth, mix, level).
    ///
    /// The default choice for general-purpose parameter smoothing.
    /// Provides a good balance between responsiveness and click-free changes.
    pub fn standard(initial: f32, sample_rate: f32) -> Self {
        Self::with_config(initial, sample_rate, 10.0)
    }

    /// 20 ms smoothing — filter coefficients, EQ, reverb decay.
    ///
    /// Slower response prevents audible "zipper" artifacts on parameters
    /// that directly affect filter coefficients.
    pub fn slow(initial: f32, sample_rate: f32) -> Self {
        Self::with_config(initial, sample_rate, 20.0)
    }

    /// 50 ms smoothing — delay time, predelay (avoids pitch artifacts).
    ///
    /// Very slow response prevents the pitch-shifting artifacts that occur
    /// when delay time changes cause the read pointer to jump.
    pub fn interpolated(initial: f32, sample_rate: f32) -> Self {
        Self::with_config(initial, sample_rate, 50.0)
    }

    /// Set the target value (parameter will smooth towards this).
    ///
    /// The parameter will exponentially approach this value over the
    /// configured smoothing time.
    #[inline]
    pub fn set_target(&mut self, target: f32) {
        self.target = target;
        self.settled = close_enough(self.current, target);
    }

    /// Set target and immediately snap to it (no smoothing).
    ///
    /// Useful for initialization or when you explicitly want instant changes.
    #[inline]
    pub fn set_immediate(&mut self, value: f32) {
        self.target = value;
        self.current = value;
        self.settled = true;
    }

    /// Update sample rate and recalculate smoothing coefficient.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.recalculate_coeff();
    }

    /// Set smoothing time in milliseconds.
    ///
    /// Typical values:
    /// - 0.0 ms: No smoothing (instant)
    /// - 5-10 ms: Fast, good for gain/pan
    /// - 20-50 ms: Medium, good for filter cutoff
    /// - 100+ ms: Slow, for gradual transitions
    pub fn set_smoothing_time_ms(&mut self, time_ms: f32) {
        self.smoothing_time_ms = time_ms;
        self.recalculate_coeff();
    }

    /// Get the next smoothed value (advances by one sample).
    ///
    /// Call this once per sample in your audio processing loop.
    #[inline]
    pub fn advance(&mut self) -> f32 {
        if self.settled {
            return self.current;
        }
        // One-pole lowpass: y[n] = y[n-1] + coeff * (target - y[n-1])
        // Equivalent to: y[n] = (1-coeff) * y[n-1] + coeff * target
        let next = self.current + self.coeff * (self.target - self.current);
        // Snap once close, or once a step no longer moves the value: in f32 a
        // 10 ms smoother stalls ~240 representable values short of its target,
        // and above ~8 even that is more than 1e-6 away, so without this a
        // parameter could smooth forever (and keep its adapter re-writing
        // every parameter, every sample).
        if next == self.current || close_enough(next, self.target) {
            self.current = self.target;
            self.settled = true;
        } else {
            self.current = next;
        }
        self.current
    }

    /// Get the current smoothed value without advancing.
    #[inline]
    pub fn get(&self) -> f32 {
        self.current
    }

    /// Get the target value.
    #[inline]
    pub fn target(&self) -> f32 {
        self.target
    }

    /// Check if the parameter has reached its target (within epsilon).
    ///
    /// Useful for knowing when smoothing is complete.
    #[inline]
    pub fn is_settled(&self) -> bool {
        self.settled
    }

    /// Skip ahead to the target value immediately.
    ///
    /// Useful for resetting state or when the difference is negligible.
    #[inline]
    pub fn snap_to_target(&mut self) {
        self.current = self.target;
        self.settled = true;
    }

    /// Recalculate the smoothing coefficient from sample rate and smoothing time.
    ///
    /// The coefficient controls the speed of the one-pole lowpass filter that
    /// smooths parameter transitions. The derivation:
    ///
    /// A one-pole lowpass has the difference equation:
    ///   `y[n] = y[n-1] + coeff * (target - y[n-1])`
    ///
    /// This is equivalent to `y[n] = (1-coeff) * y[n-1] + coeff * target`,
    /// a first-order IIR with pole at `(1-coeff)`. The time constant tau
    /// (time to reach 63.2% of target) relates to the coefficient by:
    ///
    ///   `coeff = 1 - exp(-1 / (tau * sample_rate))`
    ///
    /// where `tau = smoothing_time_ms / 1000`. After 5*tau, the parameter
    /// reaches 99.3% of the target -- effectively settled for audio purposes.
    ///
    /// When smoothing_time_ms is 0, coeff is set to 1.0 for instant response.
    fn recalculate_coeff(&mut self) {
        if self.smoothing_time_ms <= 0.0 || self.sample_rate <= 0.0 {
            self.coeff = 1.0; // Instant (no smoothing)
        } else {
            // Time constant in seconds
            let time_constant = self.smoothing_time_ms / 1000.0;
            // Samples per time constant
            let samples = time_constant * self.sample_rate;
            // One-pole coefficient
            self.coeff = 1.0 - expf(-1.0 / samples);
        }
    }
}

impl Default for SmoothedParam {
    fn default() -> Self {
        Self::new(0.0)
    }
}

/// A parameter with linear smoothing (constant rate of change).
///
/// Unlike exponential smoothing, linear smoothing changes at a constant rate.
/// This is useful for crossfades and situations where you want predictable
/// transition times.
#[derive(Debug, Clone)]
pub struct LinearSmoothedParam {
    /// Current value
    current: f32,
    /// Target value
    target: f32,
    /// Increment per sample (can be positive or negative)
    increment: f32,
    /// Samples remaining until target reached
    samples_remaining: u32,
    /// Sample rate in Hz
    sample_rate: f32,
    /// Transition time in milliseconds
    transition_time_ms: f32,
}

impl LinearSmoothedParam {
    /// Create a new linear smoothed parameter.
    pub fn new(initial: f32) -> Self {
        Self {
            current: initial,
            target: initial,
            increment: 0.0,
            samples_remaining: 0,
            sample_rate: 44100.0,
            transition_time_ms: 10.0,
        }
    }

    /// Create with full configuration.
    pub fn with_config(initial: f32, sample_rate: f32, transition_time_ms: f32) -> Self {
        Self {
            current: initial,
            target: initial,
            increment: 0.0,
            samples_remaining: 0,
            sample_rate,
            transition_time_ms,
        }
    }

    /// Set the target value.
    pub fn set_target(&mut self, target: f32) {
        if (target - self.target).abs() < 1e-9 {
            return; // Same target, no change needed
        }

        self.target = target;

        // Calculate samples for transition
        let samples = (self.transition_time_ms / 1000.0 * self.sample_rate) as u32;
        if samples == 0 {
            self.current = target;
            self.increment = 0.0;
            self.samples_remaining = 0;
        } else {
            self.increment = (target - self.current) / samples as f32;
            self.samples_remaining = samples;
        }
    }

    /// Set value immediately.
    pub fn set_immediate(&mut self, value: f32) {
        self.current = value;
        self.target = value;
        self.increment = 0.0;
        self.samples_remaining = 0;
    }

    /// Update sample rate.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
    }

    /// Set transition time in milliseconds.
    pub fn set_transition_time_ms(&mut self, time_ms: f32) {
        self.transition_time_ms = time_ms;
    }

    /// Get next smoothed value.
    #[inline]
    pub fn advance(&mut self) -> f32 {
        if self.samples_remaining > 0 {
            self.current += self.increment;
            self.samples_remaining -= 1;
            if self.samples_remaining == 0 {
                self.current = self.target; // Snap to exact target
            }
        }
        self.current
    }

    /// Get current value without advancing.
    #[inline]
    pub fn get(&self) -> f32 {
        self.current
    }

    /// Get target value.
    #[inline]
    pub fn target(&self) -> f32 {
        self.target
    }

    /// Check if transition is complete.
    #[inline]
    pub fn is_settled(&self) -> bool {
        self.samples_remaining == 0
    }

    /// Snap to target immediately.
    pub fn snap_to_target(&mut self) {
        self.current = self.target;
        self.increment = 0.0;
        self.samples_remaining = 0;
    }
}

impl Default for LinearSmoothedParam {
    fn default() -> Self {
        Self::new(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_values_settle_exactly() {
        // Regression: with an absolute 1e-6 threshold, a 4 kHz parameter
        // stalled a few f32 steps short and never settled.
        for (from, to) in [
            (4000.0, 4100.0),
            (20_000.0, 3_000.0),
            (0.3, 0.7),
            (-24.0, 6.0),
        ] {
            let mut p = SmoothedParam::standard(from, 48_000.0);
            p.set_target(to);
            let mut n = 0;
            while !p.is_settled() {
                p.advance();
                n += 1;
                assert!(n < 48_000, "{from} -> {to} still smoothing after 1 s");
            }
            assert_eq!(p.get(), to);
            // Re-setting the same target keeps it settled.
            p.set_target(to);
            assert!(p.is_settled());
        }
    }

    #[test]
    fn smoothed_param_instant_when_no_smoothing() {
        let mut param = SmoothedParam::new(1.0);
        param.set_sample_rate(48000.0);
        param.set_smoothing_time_ms(0.0); // No smoothing

        param.set_target(0.5);
        let val = param.advance();
        assert!((val - 0.5).abs() < 1e-6, "Should snap instantly");
    }

    #[test]
    fn smoothed_param_converges() {
        let mut param = SmoothedParam::with_config(0.0, 48000.0, 10.0);
        param.set_target(1.0);

        // Run for 50ms (5x the time constant) - should be very close
        for _ in 0..(48000 * 50 / 1000) {
            param.advance();
        }

        assert!(
            (param.get() - 1.0).abs() < 0.01,
            "Should converge to target, got {}",
            param.get()
        );
    }

    #[test]
    fn smoothed_param_gradual_approach() {
        let mut param = SmoothedParam::with_config(0.0, 48000.0, 10.0);
        param.set_target(1.0);

        // After one time constant (~10ms), should be about 63% of the way
        let samples_for_time_constant = (48000.0 * 0.010) as usize;
        for _ in 0..samples_for_time_constant {
            param.advance();
        }

        // One-pole reaches ~63.2% after one time constant
        let expected = 1.0 - expf(-1.0); // ~0.632
        assert!(
            (param.get() - expected).abs() < 0.05,
            "After one time constant, expected ~{}, got {}",
            expected,
            param.get()
        );
    }

    #[test]
    fn linear_smoothed_param_exact_time() {
        let mut param = LinearSmoothedParam::with_config(0.0, 48000.0, 10.0);
        param.set_target(1.0);

        // Run for exactly 10ms
        let samples = (48000.0 * 0.010) as usize;
        for _ in 0..samples {
            param.advance();
        }

        assert!(
            (param.get() - 1.0).abs() < 1e-5,
            "Should reach target exactly, got {}",
            param.get()
        );
        assert!(param.is_settled());
    }

    #[test]
    fn linear_smoothed_constant_rate() {
        let mut param = LinearSmoothedParam::with_config(0.0, 48000.0, 10.0);
        param.set_target(1.0);

        // After 5ms, should be halfway
        let samples = (48000.0 * 0.005) as usize;
        for _ in 0..samples {
            param.advance();
        }

        assert!(
            (param.get() - 0.5).abs() < 0.01,
            "Should be halfway, got {}",
            param.get()
        );
    }
}
