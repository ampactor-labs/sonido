//! N-dimensional morph space for parameter interpolation.
//!
//! `MorphSpace` stores parameter snapshots as corners of a 1D or 2D space
//! and interpolates between them. This generalizes the A↔B preset morphing
//! (1D case) to XY pad control (2D case with 4 corners).
//!
//! # Morph Curves
//!
//! Each parameter has its own interpolation curve:
//! - [`MorphCurve::Linear`] — arithmetic interpolation (default)
//! - [`MorphCurve::Logarithmic`] — geometric interpolation (frequency params)
//! - [`MorphCurve::Snap`] — snaps at midpoint (STEPPED/enum params)
//!
//! # Example
//!
//! ```rust,ignore
//! use sonido_core::{MorphSpace, MorphCurve};
//!
//! // 1D morphing between two presets
//! let mut ms = MorphSpace::new_1d(param_count);
//! ms.set_snapshot(0, &preset_a_values);
//! ms.set_snapshot(1, &preset_b_values);
//!
//! // Auto-detect curves from parameter descriptors
//! let descriptors: Vec<_> = (0..param_count)
//!     .map(|i| effect.param_info(i))
//!     .collect();
//! ms.auto_curves(&descriptors);
//!
//! // Interpolate at a position
//! let mut output = vec![0.0f32; param_count];
//! ms.interpolate(&[0.5], &mut output); // halfway between presets
//! ```

#[cfg(not(feature = "std"))]
use alloc::{vec, vec::Vec};

use crate::{ParamDescriptor, ParamFlags, ParamScale};

/// Interpolation curve applied per-parameter during morphing.
///
/// Each parameter in a [`MorphSpace`] can use a different curve,
/// allowing frequency params (Logarithmic), enum params (Snap), and
/// continuous params (Linear) to all morph correctly in a single operation.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum MorphCurve {
    /// Arithmetic interpolation: `a + (b - a) * t`.
    ///
    /// Suitable for gain, time, and most continuous parameters.
    Linear,
    /// Geometric (logarithmic) interpolation: `exp(log(a) * (1-t) + log(b) * t)`.
    ///
    /// Produces the geometric mean at `t=0.5`. Suitable for frequency parameters
    /// where perceptual distance is logarithmic (e.g. 100 Hz → 1000 Hz → 10 kHz).
    ///
    /// Falls back to [`Linear`](MorphCurve::Linear) if either value is `<= 0.0`.
    Logarithmic,
    /// Stepped interpolation: snaps at the midpoint (`t = 0.5`).
    ///
    /// `t < 0.5` → `a`, `t >= 0.5` → `b`. Suitable for enum/discrete parameters
    /// (filter type, waveshape selector, etc.).
    Snap,
}

/// N-dimensional morph space for parameter interpolation.
///
/// Stores parameter snapshots at the corners of a 1D line (2 corners) or
/// 2D rectangle (4 corners). The `interpolate()` method produces a blended
/// parameter set at any position within the space.
///
/// # Dimensions
///
/// - **1D** (`new_1d`): 2 corners indexed 0 (A) and 1 (B). Position is `[t]` in `[0.0, 1.0]`.
/// - **2D** (`new_2d`): 4 corners indexed 0 (BL), 1 (BR), 2 (TL), 3 (TR). Position is `[x, y]`.
///
/// # Invariants
///
/// - All snapshots have exactly `param_count` values.
/// - `curves` has exactly `param_count` entries.
/// - `dimensions` is 1 or 2; snapshot count is `2^dimensions`.
pub struct MorphSpace {
    /// Corner parameter arrays. Length == `2^dimensions`, each inner vec has `param_count` values.
    snapshots: Vec<Vec<f32>>,
    /// Number of parameters per snapshot.
    param_count: usize,
    /// Per-parameter interpolation curve. Length == `param_count`.
    curves: Vec<MorphCurve>,
    /// Number of interpolation dimensions: 1 (line) or 2 (rectangle).
    dimensions: u8,
}

impl MorphSpace {
    /// Creates a 1D morph space with 2 snapshot corners, all zeros, all [`MorphCurve::Linear`].
    ///
    /// Use [`set_snapshot`](Self::set_snapshot) to fill corners 0 and 1 with preset values.
    ///
    /// # Parameters
    ///
    /// - `param_count` — number of parameters per snapshot (must match effect parameter count)
    pub fn new_1d(param_count: usize) -> Self {
        Self {
            snapshots: vec![vec![0.0; param_count]; 2],
            param_count,
            curves: vec![MorphCurve::Linear; param_count],
            dimensions: 1,
        }
    }

    /// Creates a 2D morph space with 4 snapshot corners, all zeros, all [`MorphCurve::Linear`].
    ///
    /// Corner layout (for XY pad control):
    /// - Corner 0: bottom-left  (x=0, y=0)
    /// - Corner 1: bottom-right (x=1, y=0)
    /// - Corner 2: top-left     (x=0, y=1)
    /// - Corner 3: top-right    (x=1, y=1)
    ///
    /// # Parameters
    ///
    /// - `param_count` — number of parameters per snapshot (must match effect parameter count)
    pub fn new_2d(param_count: usize) -> Self {
        Self {
            snapshots: vec![vec![0.0; param_count]; 4],
            param_count,
            curves: vec![MorphCurve::Linear; param_count],
            dimensions: 2,
        }
    }

    /// Copies parameter values into a snapshot corner.
    ///
    /// # Parameters
    ///
    /// - `corner` — corner index: 0–1 for 1D, 0–3 for 2D
    /// - `values` — parameter values; must have exactly `param_count` elements
    ///
    /// # Panics
    ///
    /// Panics if `corner >= snapshot_count()` or `values.len() != param_count`.
    pub fn set_snapshot(&mut self, corner: usize, values: &[f32]) {
        assert!(
            corner < self.snapshots.len(),
            "corner {corner} out of range (snapshot_count = {})",
            self.snapshots.len()
        );
        assert_eq!(
            values.len(),
            self.param_count,
            "values.len() ({}) must equal param_count ({})",
            values.len(),
            self.param_count
        );
        self.snapshots[corner].copy_from_slice(values);
    }

    /// Returns the parameter values stored at a snapshot corner.
    ///
    /// # Panics
    ///
    /// Panics if `corner >= snapshot_count()`.
    pub fn snapshot(&self, corner: usize) -> &[f32] {
        &self.snapshots[corner]
    }

    /// Returns the number of snapshot corners: 2 for 1D, 4 for 2D.
    pub fn snapshot_count(&self) -> usize {
        self.snapshots.len()
    }

    /// Returns the number of parameters stored in each snapshot.
    pub fn param_count(&self) -> usize {
        self.param_count
    }

    /// Returns the number of interpolation dimensions: 1 or 2.
    pub fn dimensions(&self) -> u8 {
        self.dimensions
    }

    /// Sets the interpolation curve for a single parameter.
    ///
    /// # Parameters
    ///
    /// - `param_index` — parameter index in `[0, param_count)`
    /// - `curve` — curve to use for this parameter
    ///
    /// # Panics
    ///
    /// Panics if `param_index >= param_count`.
    pub fn set_curve(&mut self, param_index: usize, curve: MorphCurve) {
        self.curves[param_index] = curve;
    }

    /// Sets the same interpolation curve for all parameters.
    pub fn set_all_curves(&mut self, curve: MorphCurve) {
        for c in &mut self.curves {
            *c = curve;
        }
    }

    /// Auto-detects appropriate curves from parameter descriptors.
    ///
    /// Rules applied per parameter (first match wins):
    /// - `ParamFlags::STEPPED` set → [`MorphCurve::Snap`]
    /// - `ParamScale::Logarithmic` → [`MorphCurve::Logarithmic`]
    /// - Otherwise → [`MorphCurve::Linear`]
    ///
    /// If a descriptor is `None` (parameter index out of range for the effect),
    /// the curve defaults to [`MorphCurve::Linear`].
    ///
    /// # Parameters
    ///
    /// - `descriptors` — slice of optional descriptors, one per parameter index.
    ///   May be shorter than `param_count`; missing entries default to Linear.
    pub fn auto_curves(&mut self, descriptors: &[Option<ParamDescriptor>]) {
        for (i, curve) in self.curves.iter_mut().enumerate() {
            let desc = descriptors.get(i).and_then(|d| d.as_ref());
            *curve = match desc {
                Some(d) if d.flags.contains(ParamFlags::STEPPED) => MorphCurve::Snap,
                Some(d) if d.scale == ParamScale::Logarithmic => MorphCurve::Logarithmic,
                _ => MorphCurve::Linear,
            };
        }
    }

    /// Interpolates between snapshot corners at the given position.
    ///
    /// Position components must be in `[0.0, 1.0]`. Values outside this range
    /// are not clamped — extrapolation is permitted for creative use.
    ///
    /// # Parameters
    ///
    /// - `position` — interpolation position; length must equal `dimensions()`
    /// - `output` — receives interpolated parameter values; length must be `>= param_count()`
    ///
    /// # Panics
    ///
    /// Panics if `position.len() != dimensions()` or `output.len() < param_count()`.
    pub fn interpolate(&self, position: &[f32], output: &mut [f32]) {
        assert_eq!(
            position.len(),
            self.dimensions as usize,
            "position.len() ({}) must equal dimensions ({})",
            position.len(),
            self.dimensions
        );
        assert!(
            output.len() >= self.param_count,
            "output.len() ({}) must be >= param_count ({})",
            output.len(),
            self.param_count
        );

        match self.dimensions {
            1 => {
                let t = position[0];
                for i in 0..self.param_count {
                    output[i] = curve_lerp(
                        self.snapshots[0][i],
                        self.snapshots[1][i],
                        t,
                        self.curves[i],
                    );
                }
            }
            2 => {
                let x = position[0];
                let y = position[1];
                for i in 0..self.param_count {
                    // Bilinear interpolation:
                    //   corner 0 = BL (x=0, y=0), corner 1 = BR (x=1, y=0)
                    //   corner 2 = TL (x=0, y=1), corner 3 = TR (x=1, y=1)
                    let bottom = curve_lerp(
                        self.snapshots[0][i],
                        self.snapshots[1][i],
                        x,
                        self.curves[i],
                    );
                    let top = curve_lerp(
                        self.snapshots[2][i],
                        self.snapshots[3][i],
                        x,
                        self.curves[i],
                    );
                    output[i] = curve_lerp(bottom, top, y, self.curves[i]);
                }
            }
            _ => unreachable!("dimensions must be 1 or 2"),
        }
    }
}

/// Applies a [`MorphCurve`] to interpolate between `a` and `b` at position `t`.
///
/// `t = 0.0` → `a`, `t = 1.0` → `b`.
fn curve_lerp(a: f32, b: f32, t: f32, curve: MorphCurve) -> f32 {
    match curve {
        MorphCurve::Linear => a + (b - a) * t,
        MorphCurve::Logarithmic => {
            if a <= 0.0 || b <= 0.0 {
                // Fall back to linear for non-positive values
                a + (b - a) * t
            } else {
                // Geometric interpolation: exp(log(a) * (1-t) + log(b) * t)
                libm::expf(libm::logf(a) * (1.0 - t) + libm::logf(b) * t)
            }
        }
        MorphCurve::Snap => {
            if t < 0.5 {
                a
            } else {
                b
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::*;

    #[test]
    fn test_1d_endpoints() {
        let mut ms = MorphSpace::new_1d(3);
        ms.set_snapshot(0, &[1.0, 2.0, 3.0]);
        ms.set_snapshot(1, &[10.0, 20.0, 30.0]);
        let mut out = [0.0; 3];
        ms.interpolate(&[0.0], &mut out);
        assert_eq!(out, [1.0, 2.0, 3.0]); // t=0 → snapshot A
        ms.interpolate(&[1.0], &mut out);
        assert_eq!(out, [10.0, 20.0, 30.0]); // t=1 → snapshot B
    }

    #[test]
    fn test_1d_linear_midpoint() {
        let mut ms = MorphSpace::new_1d(2);
        ms.set_snapshot(0, &[0.0, 100.0]);
        ms.set_snapshot(1, &[10.0, 200.0]);
        let mut out = [0.0; 2];
        ms.interpolate(&[0.5], &mut out);
        assert!((out[0] - 5.0).abs() < 1e-6);
        assert!((out[1] - 150.0).abs() < 1e-6);
    }

    #[test]
    fn test_1d_snap_curve() {
        let mut ms = MorphSpace::new_1d(1);
        ms.set_snapshot(0, &[0.0]);
        ms.set_snapshot(1, &[3.0]);
        ms.set_curve(0, MorphCurve::Snap);
        let mut out = [0.0];
        ms.interpolate(&[0.49], &mut out);
        assert_eq!(out[0], 0.0); // below midpoint → A
        ms.interpolate(&[0.51], &mut out);
        assert_eq!(out[0], 3.0); // above midpoint → B
    }

    #[test]
    fn test_1d_logarithmic_curve() {
        let mut ms = MorphSpace::new_1d(1);
        ms.set_snapshot(0, &[100.0]); // 100 Hz
        ms.set_snapshot(1, &[10000.0]); // 10 kHz
        ms.set_curve(0, MorphCurve::Logarithmic);
        let mut out = [0.0];
        ms.interpolate(&[0.5], &mut out);
        // Geometric mean of 100 and 10000 = sqrt(100 * 10000) = 1000
        assert!((out[0] - 1000.0).abs() < 1.0);
    }

    #[test]
    fn test_2d_corners() {
        let mut ms = MorphSpace::new_2d(1);
        ms.set_snapshot(0, &[1.0]); // bottom-left
        ms.set_snapshot(1, &[2.0]); // bottom-right
        ms.set_snapshot(2, &[3.0]); // top-left
        ms.set_snapshot(3, &[4.0]); // top-right
        let mut out = [0.0];
        ms.interpolate(&[0.0, 0.0], &mut out);
        assert!((out[0] - 1.0).abs() < 1e-6); // BL
        ms.interpolate(&[1.0, 0.0], &mut out);
        assert!((out[0] - 2.0).abs() < 1e-6); // BR
        ms.interpolate(&[0.0, 1.0], &mut out);
        assert!((out[0] - 3.0).abs() < 1e-6); // TL
        ms.interpolate(&[1.0, 1.0], &mut out);
        assert!((out[0] - 4.0).abs() < 1e-6); // TR
    }

    #[test]
    fn test_2d_center() {
        let mut ms = MorphSpace::new_2d(1);
        ms.set_snapshot(0, &[0.0]);
        ms.set_snapshot(1, &[10.0]);
        ms.set_snapshot(2, &[20.0]);
        ms.set_snapshot(3, &[30.0]);
        let mut out = [0.0];
        ms.interpolate(&[0.5, 0.5], &mut out);
        // Bilinear: bottom=5, top=25, result=15
        assert!((out[0] - 15.0).abs() < 1e-6);
    }

    #[test]
    fn test_logarithmic_fallback_on_zero() {
        // When a value is 0, should fall back to linear
        let mut ms = MorphSpace::new_1d(1);
        ms.set_snapshot(0, &[0.0]);
        ms.set_snapshot(1, &[10.0]);
        ms.set_curve(0, MorphCurve::Logarithmic);
        let mut out = [0.0];
        ms.interpolate(&[0.5], &mut out);
        // Falls back to linear: 0 + (10-0)*0.5 = 5
        assert!((out[0] - 5.0).abs() < 1e-6);
    }

    #[test]
    fn test_set_all_curves() {
        let mut ms = MorphSpace::new_1d(3);
        ms.set_all_curves(MorphCurve::Snap);
        for i in 0..3 {
            assert_eq!(ms.curves[i], MorphCurve::Snap);
        }
    }

    #[test]
    fn test_snapshot_count_and_dimensions() {
        let ms1 = MorphSpace::new_1d(4);
        assert_eq!(ms1.snapshot_count(), 2);
        assert_eq!(ms1.dimensions(), 1);
        assert_eq!(ms1.param_count(), 4);

        let ms2 = MorphSpace::new_2d(4);
        assert_eq!(ms2.snapshot_count(), 4);
        assert_eq!(ms2.dimensions(), 2);
        assert_eq!(ms2.param_count(), 4);
    }

    #[test]
    fn test_auto_curves() {
        use crate::{ParamDescriptor, ParamFlags};

        let mut ms = MorphSpace::new_1d(3);
        let descriptors = vec![
            // param 0: stepped → Snap
            Some(
                ParamDescriptor::gain_db("Type", "Type", 0.0, 3.0, 0.0)
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED)),
            ),
            // param 1: logarithmic → Logarithmic
            Some(ParamDescriptor::rate_hz(20.0, 20000.0, 1000.0)),
            // param 2: plain linear → Linear
            Some(ParamDescriptor::mix()),
        ];
        ms.auto_curves(&descriptors);
        assert_eq!(ms.curves[0], MorphCurve::Snap);
        assert_eq!(ms.curves[1], MorphCurve::Logarithmic);
        assert_eq!(ms.curves[2], MorphCurve::Linear);
    }

    #[test]
    fn test_snapshot_read_back() {
        let mut ms = MorphSpace::new_1d(3);
        ms.set_snapshot(0, &[1.0, 2.0, 3.0]);
        assert_eq!(ms.snapshot(0), &[1.0, 2.0, 3.0]);
    }
}
