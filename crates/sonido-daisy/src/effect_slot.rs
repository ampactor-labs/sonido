//! Shared effect slot — bypass crossfade, NaN guard, and knob mapping.
//!
//! `EffectSlot` is the atomic unit of effect management on hardware.
//! `single_effect` uses it directly. `sonido_pedal` uses the standalone
//! utilities (`sanitize_stereo`, `BypassCrossfade`).

extern crate alloc;

use alloc::boxed::Box;
use sonido_core::param::SmoothedParam;
use sonido_core::EffectWithParams;
use sonido_platform::knob_mapping::{self, NULL_KNOB};

/// Control poll decimation: every 15th block ≈ 100 Hz at 48 kHz / 32 samples.
///
/// Derivation: 48 000 Hz / 32 samples = 1 500 blocks/s. 1 500 / 15 = 100 Hz.
/// The [`hothouse`](crate::hothouse) control task refreshes the shared buffer
/// at 100 Hz ([`POLL_INTERVAL_MS`](crate::hothouse::POLL_INTERVAL_MS) = 10 ms)
/// so the callback observes each update exactly once.
/// Coupled to `BLOCK_SIZE` (32) and `SAMPLE_RATE` (48 kHz) in the audio task —
/// changing either requires recalculating this constant.
pub const CONTROL_POLL_EVERY: u16 = 15;

/// Replace NaN/Infinity with silence.
#[inline]
pub fn sanitize_stereo(left: f32, right: f32) -> (f32, f32) {
    (
        if left.is_finite() { left } else { 0.0 },
        if right.is_finite() { right } else { 0.0 },
    )
}

/// Click-free bypass ramp (5 ms crossfade).
///
/// Uses [`SmoothedParam::fast`] (5 ms) to ramp between dry (0.0) and wet (1.0).
/// Call [`advance`](BypassCrossfade::advance) once per sample in the audio loop.
pub struct BypassCrossfade {
    mix: SmoothedParam,
}

impl BypassCrossfade {
    /// Creates a new bypass crossfade, starting active (wet = 1.0).
    pub fn new(sample_rate: f32) -> Self {
        Self {
            mix: SmoothedParam::fast(1.0, sample_rate),
        }
    }

    /// Set active (true = effect on, false = bypassed).
    pub fn set_active(&mut self, active: bool) {
        self.mix.set_target(if active { 1.0 } else { 0.0 });
    }

    /// Returns true if the effect is currently active (target = 1.0).
    pub fn is_active(&self) -> bool {
        self.mix.target() > 0.5
    }

    /// Advance the crossfade by one sample and mix dry/wet.
    ///
    /// Returns the crossfaded output.
    #[inline]
    pub fn advance(&mut self, dry_l: f32, dry_r: f32, wet_l: f32, wet_r: f32) -> (f32, f32) {
        let m = self.mix.advance();
        (
            dry_l + (wet_l - dry_l) * m,
            dry_r + (wet_r - dry_r) * m,
        )
    }
}

/// Single effect with bypass crossfade, NaN guard, and knob mapping.
///
/// Wraps a `Box<dyn EffectWithParams + Send>` with:
/// - Click-free bypass via [`BypassCrossfade`]
/// - NaN/Infinity sanitization via [`sanitize_stereo`]
/// - Hardware knob mapping via [`sonido_platform::knob_mapping`]
pub struct EffectSlot {
    effect: Box<dyn EffectWithParams + Send>,
    effect_id: &'static str,
    bypass: BypassCrossfade,
}

impl EffectSlot {
    /// Creates a new effect slot, starting active.
    pub fn new(
        effect: Box<dyn EffectWithParams + Send>,
        id: &'static str,
        sample_rate: f32,
    ) -> Self {
        Self {
            effect,
            effect_id: id,
            bypass: BypassCrossfade::new(sample_rate),
        }
    }

    /// Set whether the effect is active (true) or bypassed (false).
    pub fn set_active(&mut self, active: bool) {
        self.bypass.set_active(active);
    }

    /// Returns true if the effect is currently active.
    pub fn is_active(&self) -> bool {
        self.bypass.is_active()
    }

    /// Process one stereo sample: effect → NaN guard → bypass crossfade.
    #[inline]
    pub fn process_stereo(&mut self, left: f32, right: f32) -> (f32, f32) {
        let (wet_l, wet_r) = self.effect.process_stereo(left, right);
        let (wet_l, wet_r) = sanitize_stereo(wet_l, wet_r);
        self.bypass.advance(left, right, wet_l, wet_r)
    }

    /// Apply 6 hardware knobs via shared knob_map + noon + biased ADC.
    ///
    /// `knob_values` are normalized 0.0–1.0 from the control buffer.
    /// Unmapped knobs (`NULL_KNOB`) are skipped.
    pub fn apply_knobs(&mut self, knob_values: &[f32; 6]) {
        let Some(map) = knob_mapping::knob_map(self.effect_id) else {
            return;
        };
        for (k, &normalized) in knob_values.iter().enumerate() {
            let param_idx = map[k];
            if param_idx == NULL_KNOB {
                continue;
            }
            let idx = param_idx as usize;
            if let Some(desc) = self.effect.effect_param_info(idx) {
                let val = knob_mapping::knob_to_param(self.effect_id, idx, &desc, normalized);
                self.effect.effect_set_param(idx, val);
            }
        }
    }

    /// Returns the effect's registry ID.
    pub fn effect_id(&self) -> &str {
        self.effect_id
    }

    /// Returns a reference to the inner effect.
    pub fn effect(&self) -> &dyn EffectWithParams {
        &*self.effect
    }

    /// Returns a mutable reference to the inner effect.
    pub fn effect_mut(&mut self) -> &mut (dyn EffectWithParams + Send) {
        &mut *self.effect
    }
}
