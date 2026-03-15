//! Cached coefficient wrapper with epsilon-threshold change detection.
//!
//! [`Cached<T>`] recomputes a value only when its inputs change beyond an
//! epsilon threshold. This replaces the `NaN`-sentinel coefficient caching
//! pattern used across filter kernels (preamp, distortion, EQ, gate, filter,
//! compressor, tape, stage).
//!
//! # Why not just compare `last_freq != freq`?
//!
//! The NaN-sentinel pattern works but has drawbacks:
//! - Copy-pasted across 8+ kernels with subtle per-kernel variations
//! - Requires manually tracking N `last_xxx` fields per cached computation
//! - `NaN` as sentinel is semantically misleading
//! - No epsilon tolerance (recomputes on floating-point noise)
//!
//! `Cached<T>` centralizes the pattern with proper epsilon comparison.
//!
//! # Example
//!
//! ```rust
//! use sonido_core::Cached;
//!
//! // Cache biquad coefficients keyed on (frequency, Q)
//! let mut cached = Cached::new([0.0f32; 5], 2);
//!
//! // First call always computes
//! let coeffs = cached.update(&[1000.0, 0.707], 1e-6, |inputs| {
//!     // Compute biquad coefficients from frequency and Q
//!     [inputs[0], inputs[1], 0.0, 0.0, 0.0]
//! });
//!
//! // Second call with same inputs — returns cached value, no recompute
//! let coeffs2 = cached.update(&[1000.0, 0.707], 1e-6, |inputs| {
//!     panic!("should not be called");
//! });
//! ```

/// Maximum number of tracked input values.
///
/// Covers all current use cases: EQ per-band (freq, gain, Q = 3 inputs),
/// gate (threshold, range, attack, release, hysteresis, sidechain = 6 → use
/// two Cached instances or increase this). 8 is generous headroom.
const MAX_INPUTS: usize = 8;

/// Caches a computed value, recomputing only when inputs change beyond epsilon.
///
/// Generic over the cached value type `T`. The inputs are tracked as an array
/// of `f32` values (up to 8).
///
/// # Invalidation
///
/// Call [`invalidate()`](Self::invalidate) in `reset()` to force recomputation
/// on the next `update()` call. This replaces `self.last_xxx = f32::NAN`.
///
/// # Thread Safety
///
/// `Cached<T>` is `Send` if `T: Send` (true for all coefficient types).
#[derive(Debug, Clone)]
pub struct Cached<T> {
    /// The cached computed value.
    value: T,
    /// Last input values that produced the cached value.
    last_inputs: [f32; MAX_INPUTS],
    /// Number of input values being tracked (1..=MAX_INPUTS).
    input_count: usize,
    /// Whether the cache has been invalidated (forces recompute on next update).
    invalidated: bool,
}

impl<T> Cached<T> {
    /// Creates a new cache with an initial value and the number of tracked inputs.
    ///
    /// The cache starts invalidated — the first `update()` call will always compute.
    ///
    /// # Panics
    ///
    /// Panics if `input_count` is 0 or exceeds 8.
    pub fn new(initial: T, input_count: usize) -> Self {
        assert!(
            input_count > 0 && input_count <= MAX_INPUTS,
            "input_count must be 1..={MAX_INPUTS}, got {input_count}"
        );
        Self {
            value: initial,
            last_inputs: [f32::NAN; MAX_INPUTS], // NaN ensures first comparison triggers
            input_count,
            invalidated: true,
        }
    }

    /// Returns a reference to the cached value.
    #[inline]
    pub fn get(&self) -> &T {
        &self.value
    }

    /// Updates the cached value if any input changed beyond `epsilon`.
    ///
    /// Compares each input against its last-seen value. If any differs by more
    /// than `epsilon` (or the cache was invalidated), calls `compute` with the
    /// new inputs and caches the result.
    ///
    /// Returns a reference to the (possibly updated) cached value.
    ///
    /// # Arguments
    ///
    /// * `inputs` - Current input values. Length must equal `input_count`.
    /// * `epsilon` - Minimum change threshold. Use `1e-6` for frequency,
    ///   `1e-4` for gain/Q.
    /// * `compute` - Closure that computes the new value from inputs.
    #[inline]
    pub fn update(
        &mut self,
        inputs: &[f32],
        epsilon: f32,
        compute: impl FnOnce(&[f32]) -> T,
    ) -> &T {
        debug_assert_eq!(
            inputs.len(),
            self.input_count,
            "expected {} inputs, got {}",
            self.input_count,
            inputs.len()
        );

        let changed = self.invalidated
            || inputs
                .iter()
                .zip(self.last_inputs[..self.input_count].iter())
                .any(|(new, old)| {
                    // NaN != NaN is true, so invalidated inputs (NaN) always trigger
                    (new - old).abs() > epsilon || new.is_nan() != old.is_nan()
                });

        if changed {
            self.value = compute(inputs);
            self.last_inputs[..self.input_count].copy_from_slice(inputs);
            self.invalidated = false;
        }

        &self.value
    }

    /// Forces the next `update()` call to recompute regardless of input values.
    ///
    /// Call this in `DspKernel::reset()` to replace the old `self.last_xxx = f32::NAN`
    /// pattern.
    #[inline]
    pub fn invalidate(&mut self) {
        self.invalidated = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_update_always_computes() {
        let mut cached = Cached::new(0.0f32, 2);
        let mut computed = false;
        cached.update(&[100.0, 0.5], 1e-6, |_| {
            computed = true;
            42.0
        });
        assert!(computed);
        assert!((cached.get() - 42.0).abs() < 1e-6);
    }

    #[test]
    fn same_inputs_skip_compute() {
        let mut cached = Cached::new(0.0f32, 2);
        cached.update(&[100.0, 0.5], 1e-6, |_| 42.0);

        // Same inputs — should NOT recompute
        let result = cached.update(&[100.0, 0.5], 1e-6, |_| {
            panic!("should not recompute");
        });
        assert!((result - 42.0).abs() < 1e-6);
    }

    #[test]
    fn changed_input_triggers_compute() {
        let mut cached = Cached::new(0.0f32, 2);
        cached.update(&[100.0, 0.5], 1e-6, |_| 42.0);

        // Change first input beyond epsilon
        let result = cached.update(&[200.0, 0.5], 1e-6, |inputs| inputs[0] + inputs[1]);
        assert!((result - 200.5).abs() < 1e-6);
    }

    #[test]
    fn epsilon_threshold_respected() {
        let mut cached = Cached::new(0.0f32, 1);
        cached.update(&[100.0], 1.0, |_| 42.0);

        // Change within epsilon — should NOT recompute
        let result = cached.update(&[100.5], 1.0, |_| {
            panic!("should not recompute");
        });
        assert!((result - 42.0).abs() < 1e-6);

        // Change beyond epsilon — should recompute
        let result = cached.update(&[102.0], 1.0, |inputs| inputs[0]);
        assert!((result - 102.0).abs() < 1e-6);
    }

    #[test]
    fn invalidate_forces_recompute() {
        let mut cached = Cached::new(0.0f32, 1);
        cached.update(&[100.0], 1e-6, |_| 42.0);
        cached.invalidate();

        // Same inputs but invalidated — should recompute
        let mut computed = false;
        cached.update(&[100.0], 1e-6, |_| {
            computed = true;
            99.0
        });
        assert!(computed);
        assert!((cached.get() - 99.0).abs() < 1e-6);
    }

    #[test]
    fn works_with_array_value() {
        let mut cached = Cached::new([0.0f32; 3], 2);
        let result = cached.update(&[1000.0, 0.707], 1e-6, |inputs| {
            [inputs[0], inputs[1], inputs[0] * inputs[1]]
        });
        assert!((result[2] - 707.0).abs() < 0.1);
    }
}
