//! Expression pedal input with smoothing, deadzone, and slew limiting.
//!
//! Reads a normalized ADC value (0.0–1.0) from an expression pedal or
//! CV input and produces a smoothed, jitter-free control value. Supports
//! calibration, deadzone, and slew rate limiting.
//!
//! # Usage
//!
//! ```rust,ignore
//! let mut expr = ExpressionInput::new();
//!
//! // In control task (50 Hz):
//! let raw = adc.blocking_read(&mut expr_channel) as f32 / 65535.0;
//! expr.update(raw);
//!
//! // In audio callback:
//! let value = expr.value(); // 0.0–1.0, smoothed
//! effect.set_param(target_param, denormalize(value));
//! ```

/// Expression pedal input processor.
///
/// Converts raw ADC readings into a clean 0.0–1.0 control signal with:
/// - **Deadzone**: Ignores values below threshold (prevents ghost signals)
/// - **Range mapping**: Maps calibrated (min, max) to full (0.0, 1.0)
/// - **Slew limiting**: Caps maximum change per update (anti-jitter)
/// - **IIR smoothing**: Low-pass filter for final output
///
/// # Connection Detection
///
/// An unconnected TRS jack floats near 0V. [`is_connected()`](Self::is_connected)
/// returns `true` when the raw value exceeds the deadzone, indicating a
/// physical pedal is plugged in.
pub struct ExpressionInput {
    /// Smoothed output value (0.0–1.0).
    value: f32,
    /// Most recent raw ADC reading (0.0–1.0).
    raw: f32,
    /// Calibrated minimum raw value (toe up / heel down).
    min_raw: f32,
    /// Calibrated maximum raw value (toe down / heel up).
    max_raw: f32,
    /// Values below this threshold are treated as 0.0.
    deadzone: f32,
    /// Maximum change per update call (slew rate limit).
    slew_limit: f32,
}

impl ExpressionInput {
    /// Creates a new expression input with default calibration.
    ///
    /// Defaults: full range (0.0–1.0), deadzone 0.02, slew limit 0.05.
    pub const fn new() -> Self {
        Self {
            value: 0.0,
            raw: 0.0,
            min_raw: 0.0,
            max_raw: 1.0,
            deadzone: 0.02,
            slew_limit: 0.05,
        }
    }

    /// Update with a new raw ADC reading (normalized 0.0–1.0).
    ///
    /// Call at the control poll rate (~50 Hz). Applies deadzone, range
    /// mapping, slew limiting, and IIR smoothing in sequence.
    pub fn update(&mut self, raw_normalized: f32) {
        self.raw = raw_normalized;

        // 1. Deadzone: values below threshold map to 0.0
        let after_deadzone = if raw_normalized < self.deadzone {
            0.0
        } else {
            raw_normalized
        };

        // 2. Range mapping: map calibrated range to 0.0..1.0, clamp
        let range = self.max_raw - self.min_raw;
        let mapped = if range <= 0.0 {
            0.0
        } else {
            let v = (after_deadzone - self.min_raw) / range;
            if v < 0.0 {
                0.0
            } else if v > 1.0 {
                1.0
            } else {
                v
            }
        };

        // 3. Slew limiting: cap rate of change per update call
        let delta = mapped - self.value;
        let slewed = if delta > self.slew_limit {
            self.value + self.slew_limit
        } else if delta < -self.slew_limit {
            self.value - self.slew_limit
        } else {
            mapped
        };

        // 4. IIR smoothing: one-pole low-pass
        self.value = 0.15 * slewed + 0.85 * self.value;
    }

    /// Returns the current smoothed value (0.0–1.0).
    #[inline]
    pub fn value(&self) -> f32 {
        self.value
    }

    /// Returns the most recent raw ADC reading.
    #[inline]
    pub fn raw(&self) -> f32 {
        self.raw
    }

    /// Whether an expression pedal appears to be connected.
    ///
    /// An unconnected TRS jack floats near 0V. Returns `true` when the
    /// raw value exceeds the deadzone threshold.
    #[inline]
    pub fn is_connected(&self) -> bool {
        self.raw > self.deadzone
    }

    /// Set the calibration range.
    ///
    /// Call with the raw values observed at heel-down and toe-down positions.
    pub fn set_range(&mut self, min: f32, max: f32) {
        self.min_raw = min;
        self.max_raw = max;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the expression input with `n` identical updates and return final value.
    fn settle(expr: &mut ExpressionInput, raw: f32, n: usize) -> f32 {
        for _ in 0..n {
            expr.update(raw);
        }
        expr.value()
    }

    #[test]
    fn deadzone_suppresses_small_signals() {
        let mut expr = ExpressionInput::new();
        settle(&mut expr, 0.01, 100); // below deadzone=0.02
        assert!(
            expr.value() < 0.001,
            "value should stay near 0 in deadzone, got {}",
            expr.value()
        );
    }

    #[test]
    fn full_pedal_converges_to_one() {
        let mut expr = ExpressionInput::new();
        settle(&mut expr, 1.0, 200);
        assert!(
            expr.value() > 0.99,
            "expected ~1.0 after many updates, got {}",
            expr.value()
        );
    }

    #[test]
    fn slew_limits_step_response() {
        let mut expr = ExpressionInput::new();
        // One update with full signal — slew_limit=0.05 caps the step
        expr.update(1.0);
        // After 1 update: slewed = 0.0 + 0.05 = 0.05, then IIR: 0.15*0.05 + 0.85*0.0 = 0.0075
        assert!(
            expr.value() < 0.02,
            "single update should be slew-limited, got {}",
            expr.value()
        );
    }

    #[test]
    fn is_connected_above_deadzone() {
        let mut expr = ExpressionInput::new();
        expr.update(0.5);
        assert!(expr.is_connected());
    }

    #[test]
    fn is_connected_below_deadzone() {
        let mut expr = ExpressionInput::new();
        expr.update(0.01); // below 0.02 deadzone
        assert!(!expr.is_connected());
    }

    #[test]
    fn calibration_range_maps_correctly() {
        let mut expr = ExpressionInput::new();
        expr.set_range(0.2, 0.8);
        // After many updates at raw=0.8 (max), value should converge near 1.0
        settle(&mut expr, 0.8, 300);
        assert!(
            expr.value() > 0.95,
            "expected ~1.0 with raw at max_raw, got {}",
            expr.value()
        );
    }

    #[test]
    fn raw_is_stored() {
        let mut expr = ExpressionInput::new();
        expr.update(0.42);
        assert!((expr.raw() - 0.42).abs() < f32::EPSILON);
    }
}
