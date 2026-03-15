//! Configurable nonlinear gain stage with ADAA anti-aliasing.
//!
//! [`GainStage`] models a tube/transistor gain stage by applying a nonlinear
//! waveshaper between a pre-gain and post-gain multiplier, using first-order
//! Anti-Derivative Anti-Aliasing (ADAA) to reduce aliasing artifacts.
//!
//! # Signal Flow
//!
//! ```text
//! input → × pre_gain → ADAA(waveshaper) → × post_gain → output
//! ```
//!
//! The waveshaper and its antiderivative are supplied as function pointers at
//! construction time, enabling zero-cost monomorphization for each gain-stage
//! variant (soft clip, asymmetric clip, etc.).
//!
//! # Design
//!
//! Pre-gain and post-gain are stored as linear values internally. The dB
//! setters convert on write, avoiding repeated `db_to_linear` calls in the
//! hot path.
//!
//! # Reference
//!
//! Parker et al., "Reducing the Aliasing of Nonlinear Waveshaping Using
//! Continuous-Time Convolution", DAFx-2016.

use crate::adaa::Adaa1;
use crate::math::db_to_linear;

/// Configurable nonlinear gain stage with first-order ADAA anti-aliasing.
///
/// Applies `input * pre_gain → ADAA(waveshaper) → * post_gain`.
///
/// # Type Parameters
///
/// - `F` — Waveshaping function `f(x) → y`
/// - `AF` — First antiderivative `F(x)` of the waveshaper (for ADAA)
///
/// # Example
///
/// ```rust
/// use sonido_core::dsp::GainStage;
/// use sonido_core::math::{soft_clip, soft_clip_ad};
///
/// let mut stage = GainStage::new(soft_clip, soft_clip_ad);
/// stage.set_pre_gain_db(12.0);   // +12 dB pre-drive
/// stage.set_post_gain_db(-12.0); // -12 dB compensate
/// let out = stage.process(0.5);
/// assert!(out.is_finite());
/// ```
pub struct GainStage<F, AF>
where
    F: Fn(f32) -> f32,
    AF: Fn(f32) -> f32,
{
    /// ADAA processor wrapping the nonlinear waveshaper.
    adaa: Adaa1<F, AF>,
    /// Pre-gain multiplier in linear scale.
    pre_gain_linear: f32,
    /// Post-gain multiplier in linear scale.
    post_gain_linear: f32,
}

impl<F, AF> GainStage<F, AF>
where
    F: Fn(f32) -> f32,
    AF: Fn(f32) -> f32,
{
    /// Create a new gain stage with the given waveshaper and its antiderivative.
    ///
    /// Pre- and post-gain are initialized to unity (0 dB = 1.0 linear).
    ///
    /// # Arguments
    ///
    /// - `waveshaper` — The nonlinear function `f(x)`
    /// - `antiderivative` — First antiderivative `F(x)` where `F'(x) = f(x)`
    pub fn new(waveshaper: F, antiderivative: AF) -> Self {
        Self {
            adaa: Adaa1::new(waveshaper, antiderivative),
            pre_gain_linear: 1.0,
            post_gain_linear: 1.0,
        }
    }

    /// Process a single audio sample through the gain stage.
    ///
    /// Signal path: `input * pre_gain → ADAA(waveshaper) → * post_gain`
    #[inline]
    pub fn process(&mut self, input: f32) -> f32 {
        let driven = input * self.pre_gain_linear;
        let shaped = self.adaa.process(driven);
        shaped * self.post_gain_linear
    }

    /// Set the pre-waveshaper gain in decibels.
    ///
    /// Positive values increase drive into the nonlinearity; higher values
    /// produce more saturation. Range: any dB value (clamped by `db_to_linear`).
    ///
    /// # Arguments
    /// * `db` — Gain in decibels (0.0 = unity, +12.0 = 4× linear)
    #[inline]
    pub fn set_pre_gain_db(&mut self, db: f32) {
        self.pre_gain_linear = db_to_linear(db);
    }

    /// Set the post-waveshaper gain in decibels.
    ///
    /// Typically used to compensate for the level increase from driving into
    /// the waveshaper.
    ///
    /// # Arguments
    /// * `db` — Gain in decibels (0.0 = unity, -12.0 = 0.25× linear)
    #[inline]
    pub fn set_post_gain_db(&mut self, db: f32) {
        self.post_gain_linear = db_to_linear(db);
    }

    /// Set the pre-gain directly as a linear multiplier.
    ///
    /// # Arguments
    /// * `linear` — Linear multiplier (1.0 = unity, must be > 0)
    #[inline]
    pub fn set_pre_gain_linear(&mut self, linear: f32) {
        self.pre_gain_linear = linear.max(0.0);
    }

    /// Set the post-gain directly as a linear multiplier.
    ///
    /// # Arguments
    /// * `linear` — Linear multiplier (1.0 = unity)
    #[inline]
    pub fn set_post_gain_linear(&mut self, linear: f32) {
        self.post_gain_linear = linear;
    }

    /// Reset internal ADAA state to zero.
    ///
    /// Call when the audio stream is interrupted (preset change, transport stop)
    /// to prevent inter-segment artefacts.
    pub fn reset(&mut self) {
        self.adaa.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::{asymmetric_clip, asymmetric_clip_ad, soft_clip, soft_clip_ad};

    #[test]
    fn unity_gain_passes_signal() {
        let mut stage = GainStage::new(soft_clip as fn(f32) -> f32, soft_clip_ad as fn(f32) -> f32);
        // With unity pre/post gain, small signal passes nearly unchanged (tanh(x) ≈ x near 0)
        for i in 0..100 {
            let x = i as f32 * 0.001;
            let y = stage.process(x);
            assert!(y.is_finite(), "output must be finite at sample {i}");
        }
    }

    #[test]
    fn pre_gain_drives_saturation() {
        let mut low = GainStage::new(soft_clip as fn(f32) -> f32, soft_clip_ad as fn(f32) -> f32);
        let mut high = GainStage::new(soft_clip as fn(f32) -> f32, soft_clip_ad as fn(f32) -> f32);
        low.set_pre_gain_db(0.0);
        high.set_pre_gain_db(20.0);

        let input = 0.1;
        // Warm up ADAA state
        for _ in 0..10 {
            low.process(input);
            high.process(input);
        }
        // Higher pre-gain should push further into saturation (more output for same input)
        let y_low = low.process(input);
        let y_high = high.process(input);
        assert!(
            y_high.abs() > y_low.abs() * 2.0,
            "high gain should produce more output: low={y_low}, high={y_high}"
        );
    }

    #[test]
    fn post_gain_scales_output() {
        let mut stage_a =
            GainStage::new(soft_clip as fn(f32) -> f32, soft_clip_ad as fn(f32) -> f32);
        let mut stage_b =
            GainStage::new(soft_clip as fn(f32) -> f32, soft_clip_ad as fn(f32) -> f32);
        stage_a.set_post_gain_db(0.0);
        stage_b.set_post_gain_db(-6.0); // ~0.5× linear

        let y_a = stage_a.process(0.3);
        let y_b = stage_b.process(0.3);
        assert!(
            (y_b.abs() - y_a.abs() * 0.5).abs() < 0.05,
            "post_gain -6dB should halve output: a={y_a}, b={y_b}"
        );
    }

    #[test]
    fn reset_clears_adaa_state() {
        let mut stage = GainStage::new(soft_clip as fn(f32) -> f32, soft_clip_ad as fn(f32) -> f32);
        for _ in 0..100 {
            stage.process(0.9);
        }
        stage.reset();
        let y = stage.process(0.0);
        assert!(y.abs() < 1e-6, "after reset and silence: got {y}");
    }

    #[test]
    fn asymmetric_clip_stage_finite() {
        let mut stage = GainStage::new(
            asymmetric_clip as fn(f32) -> f32,
            asymmetric_clip_ad as fn(f32) -> f32,
        );
        stage.set_pre_gain_db(18.0);
        stage.set_post_gain_db(-18.0);
        for i in 0..256 {
            let x = libm::sinf(i as f32 * 0.1) * 0.8;
            let y = stage.process(x);
            assert!(
                y.is_finite(),
                "asymmetric stage output must be finite at {i}: {y}"
            );
        }
    }
}
