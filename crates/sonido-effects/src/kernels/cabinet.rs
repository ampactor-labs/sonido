//! Cabinet IR simulator kernel — direct convolution with 3 factory impulse responses.
//!
//! `CabinetKernel` simulates guitar speaker cabinet frequency response via
//! short (256-sample) direct time-domain convolution. Three programmatically
//! generated IRs cover clean combo, British stack, and modern high-gain character.
//! Parameters are received via `&CabinetParams` each sample. Deployed via
//! [`KernelAdapter`](sonido_core::KernelAdapter) for desktop/plugin, or called
//! directly on embedded targets.
//!
//! # Signal Flow
//!
//! ```text
//! Input → [circular buffer write] → [IR convolution] → [low cut HPF] → wet/dry mix → output gain
//! ```
//!
//! # Algorithm
//!
//! Each IR is generated in `new()` as exponentially decaying filtered noise,
//! approximating the impulse response character of each cabinet type. Convolution
//! is performed in the time domain: one multiply-accumulate per IR sample per
//! audio sample. At 256 samples this costs ~256 MACs per sample — acceptable
//! for short cab IRs.
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! let adapter = KernelAdapter::new(CabinetKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing, ADCs are hardware-filtered)
//! let mut kernel = CabinetKernel::new(48000.0);
//! let params = CabinetParams::default();
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::{
    Biquad, ParamDescriptor, ParamFlags, ParamId, ParamScale, ParamUnit, fast_db_to_linear,
    highpass_coefficients, wet_dry_mix,
};

/// Number of samples in each factory impulse response (≈5.3 ms at 48 kHz).
const IR_LEN: usize = 256;

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`CabinetKernel`].
///
/// All values in **user-facing units** — the same units shown in GUIs and
/// stored in presets.
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `ir_select` | index | 0–2 | 0 (Clean Combo) |
/// | 1 | `mix_pct` | % | 0–100 | 100.0 |
/// | 2 | `low_cut_hz` | Hz | 20–500 | 80.0 |
/// | 3 | `output_db` | dB | −60–+6 | 0.0 |
#[derive(Debug, Clone, Copy)]
pub struct CabinetParams {
    /// IR selection: 0 = Clean Combo, 1 = British Stack, 2 = Modern High-Gain.
    pub ir_select: f32,
    /// Wet/dry mix in percent.
    ///
    /// Range: 0.0 to 100.0 %. 0 % passes input unchanged; 100 % is fully processed.
    pub mix_pct: f32,
    /// Low-cut high-pass filter frequency in Hz.
    ///
    /// Range: 20–500 Hz. Removes sub-bass rumble from the cabinet output.
    pub low_cut_hz: f32,
    /// Output level in decibels.
    ///
    /// Range: −60.0 to +6.0 dB. Applied after wet/dry mix.
    pub output_db: f32,
}

impl Default for CabinetParams {
    fn default() -> Self {
        Self {
            ir_select: 0.0,
            mix_pct: 100.0,
            low_cut_hz: 80.0,
            output_db: 0.0,
        }
    }
}

/// IR type labels for the `ir_select` parameter.
const IR_LABELS: &[&str] = &["Clean Combo", "British Stack", "Modern High-Gain"];

impl KernelParams for CabinetParams {
    const COUNT: usize = 4;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(
                ParamDescriptor::custom("IR Select", "IR", 0.0, 2.0, 0.0)
                    .with_unit(ParamUnit::None)
                    .with_step(1.0)
                    .with_id(ParamId(2200), "cab_ir")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(IR_LABELS),
            ),
            1 => Some(ParamDescriptor::mix().with_id(ParamId(2201), "cab_mix")),
            2 => Some(
                ParamDescriptor::custom("Low Cut", "Low Cut", 20.0, 500.0, 80.0)
                    .with_unit(ParamUnit::Hertz)
                    .with_scale(ParamScale::Logarithmic)
                    .with_id(ParamId(2202), "cab_low_cut"),
            ),
            3 => Some(
                sonido_core::gain::output_param_descriptor().with_id(ParamId(2203), "cab_output"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::None,     // ir_select — stepped enum, snap
            1 => SmoothingStyle::Standard, // mix_pct — 10 ms
            2 => SmoothingStyle::Slow,     // low_cut_hz — filter coeff, 20 ms
            3 => SmoothingStyle::Fast,     // output_db — 5 ms
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.ir_select,
            1 => self.mix_pct,
            2 => self.low_cut_hz,
            3 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.ir_select = value,
            1 => self.mix_pct = value,
            2 => self.low_cut_hz = value,
            3 => self.output_db = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  IR generation
// ═══════════════════════════════════════════════════════════════════════════

/// Generate a factory IR using a seeded PRNG and exponential decay.
///
/// Each IR type has distinct character imposed via a one-pole lowpass filter
/// with different cutoff coefficients:
/// - **Clean Combo** (`ir_type=0`): heavy low-pass (coeff ≈ 0.92), warm and rolled-off
/// - **British Stack** (`ir_type=1`): mild low-pass (coeff ≈ 0.70), mid-forward
/// - **Modern High-Gain** (`ir_type=2`): minimal filtering (coeff ≈ 0.40), aggressive and bright
///
/// The exponential decay `e^(-5t/IR_LEN)` reaches roughly e⁻⁵ ≈ 0.007 at the end,
/// providing a natural cabinet tail.
fn generate_ir(ir_type: u8, sample_rate: f32) -> [f32; IR_LEN] {
    let _ = sample_rate; // IR character is sample-count-based, not Hz-based
    let mut ir = [0.0f32; IR_LEN];

    // Decay exponent controls how quickly the IR dies out
    let decay_rate = 5.0_f32 / IR_LEN as f32;

    // Per-type one-pole lowpass coefficient (smoothing the noise burst)
    let lp_coeff: f32 = match ir_type {
        0 => 0.92, // Clean Combo — very warm, heavy HF rolloff
        1 => 0.70, // British Stack — mid-forward, moderate HF
        _ => 0.40, // Modern High-Gain — brighter, tighter low end
    };

    // A simple xorshift32 PRNG seeded per IR type for reproducible IRs
    let mut rng: u32 = 0x1234_5678u32 ^ (ir_type as u32).wrapping_mul(0x9E37_79B9);
    let mut lp_state = 0.0f32;

    for i in 0..IR_LEN {
        // xorshift32
        rng ^= rng << 13;
        rng ^= rng >> 17;
        rng ^= rng << 5;

        // Convert to [-1, 1]
        let noise = (rng as i32 as f32) / (i32::MAX as f32);

        // One-pole lowpass to shape the spectral character
        lp_state = lp_state * lp_coeff + noise * (1.0 - lp_coeff);

        // Exponential decay envelope
        let decay = libm::expf(-decay_rate * i as f32);

        ir[i] = lp_state * decay;
    }

    // Normalize so the first non-zero sample has magnitude ~0.5
    // (avoids level jumps when switching IRs)
    let peak: f32 = ir.iter().fold(0.0f32, |acc, &x| {
        if libm::fabsf(x) > acc {
            libm::fabsf(x)
        } else {
            acc
        }
    });
    if peak > 1e-6 {
        let scale = 0.5 / peak;
        for s in ir.iter_mut() {
            *s *= scale;
        }
    }

    ir
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP cabinet IR simulator kernel.
///
/// Contains ONLY the mutable state required for audio processing:
/// - Current IR coefficients (regenerated on IR select change)
/// - Circular input buffers for L and R channels
/// - Low-cut HPF biquads for L and R channels
/// - Change-detection sentinels for IR select and low-cut
///
/// No `SmoothedParam`, no atomics, no platform awareness.
///
/// # Invariants
///
/// `write_pos` is always in `[0, IR_LEN)`. The circular buffer semantics
/// rely on modular arithmetic — never advance `write_pos` without wrapping.
pub struct CabinetKernel {
    /// Current impulse response coefficients.
    ir: [f32; IR_LEN],
    /// Circular input buffer for the left channel.
    delay_l: [f32; IR_LEN],
    /// Circular input buffer for the right channel.
    delay_r: [f32; IR_LEN],
    /// Next write position in the circular buffers.
    write_pos: usize,
    /// Low-cut HPF for the left channel.
    low_cut_l: Biquad,
    /// Low-cut HPF for the right channel.
    low_cut_r: Biquad,
    /// Sample rate, stored for HPF coefficient recomputation.
    sample_rate: f32,
    /// Last applied IR select (NaN sentinel triggers initial load).
    last_ir_select: f32,
    /// Last applied low-cut frequency (NaN sentinel triggers initial coefficient computation).
    last_low_cut_hz: f32,
}

impl CabinetKernel {
    /// Create a new cabinet kernel at the given sample rate.
    ///
    /// Initialises all three factory IRs and computes the initial low-cut
    /// HPF coefficients at 80 Hz.
    pub fn new(sample_rate: f32) -> Self {
        let ir = generate_ir(0, sample_rate);

        // Initial HPF at 80 Hz (default low_cut_hz)
        let (b0, b1, b2, a0, a1, a2) = highpass_coefficients(80.0, 0.707, sample_rate);
        let mut low_cut_l = Biquad::new();
        let mut low_cut_r = Biquad::new();
        low_cut_l.set_coefficients(b0, b1, b2, a0, a1, a2);
        low_cut_r.set_coefficients(b0, b1, b2, a0, a1, a2);

        Self {
            ir,
            delay_l: [0.0; IR_LEN],
            delay_r: [0.0; IR_LEN],
            write_pos: 0,
            low_cut_l,
            low_cut_r,
            sample_rate,
            last_ir_select: f32::NAN,
            last_low_cut_hz: f32::NAN,
        }
    }

    /// Update HPF coefficients when low_cut_hz changes.
    #[inline]
    fn update_low_cut(&mut self, low_cut_hz: f32) {
        if (self.last_low_cut_hz - low_cut_hz).abs() > 0.01 {
            let (b0, b1, b2, a0, a1, a2) =
                highpass_coefficients(low_cut_hz, 0.707, self.sample_rate);
            self.low_cut_l.set_coefficients(b0, b1, b2, a0, a1, a2);
            self.low_cut_r.set_coefficients(b0, b1, b2, a0, a1, a2);
            self.last_low_cut_hz = low_cut_hz;
        }
    }

    /// Reload the IR when ir_select changes.
    #[inline]
    fn update_ir(&mut self, ir_select: f32) {
        let new_type = (ir_select + 0.5) as u8;
        let old_type = if self.last_ir_select.is_nan() {
            255 // force reload on first call
        } else {
            (self.last_ir_select + 0.5) as u8
        };
        if new_type != old_type {
            self.ir = generate_ir(new_type, self.sample_rate);
            self.last_ir_select = ir_select;
        }
    }
}

impl DspKernel for CabinetKernel {
    type Params = CabinetParams;

    fn process_stereo(&mut self, left: f32, right: f32, params: &CabinetParams) -> (f32, f32) {
        // ── Coefficient / IR updates ──
        self.update_ir(params.ir_select);
        self.update_low_cut(params.low_cut_hz);

        // ── Write input into circular buffers ──
        self.delay_l[self.write_pos] = left;
        self.delay_r[self.write_pos] = right;

        // ── Direct convolution ──
        let mut conv_l = 0.0f32;
        let mut conv_r = 0.0f32;
        for k in 0..IR_LEN {
            // Circular read: most-recent sample at write_pos, oldest at write_pos+1
            let idx = (self.write_pos + IR_LEN - k) % IR_LEN;
            conv_l += self.ir[k] * self.delay_l[idx];
            conv_r += self.ir[k] * self.delay_r[idx];
        }

        // ── Advance write position ──
        self.write_pos = (self.write_pos + 1) % IR_LEN;

        // ── Low-cut HPF ──
        let filtered_l = self.low_cut_l.process(conv_l);
        let filtered_r = self.low_cut_r.process(conv_r);

        // ── Wet/dry mix → output gain ──
        let mix = params.mix_pct / 100.0;
        let output_gain = fast_db_to_linear(params.output_db);

        let out_l = wet_dry_mix(left, filtered_l, mix) * output_gain;
        let out_r = wet_dry_mix(right, filtered_r, mix) * output_gain;

        (out_l, out_r)
    }

    fn reset(&mut self) {
        self.delay_l = [0.0; IR_LEN];
        self.delay_r = [0.0; IR_LEN];
        self.write_pos = 0;
        self.low_cut_l.clear();
        self.low_cut_r.clear();
        // Force IR and HPF reload on next process call
        self.last_ir_select = f32::NAN;
        self.last_low_cut_hz = f32::NAN;
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        // Force coefficient recomputation at new sample rate
        self.last_ir_select = f32::NAN;
        self.last_low_cut_hz = f32::NAN;
        self.low_cut_l.clear();
        self.low_cut_r.clear();
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_core::kernel::KernelAdapter;
    use sonido_core::{Effect, ParameterInfo};

    #[test]
    fn silence_in_silence_out() {
        let mut kernel = CabinetKernel::new(48000.0);
        let params = CabinetParams::default();

        for _ in 0..512 {
            let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
            assert!(l.abs() < 1e-6, "Expected silence on L, got {l}");
            assert!(r.abs() < 1e-6, "Expected silence on R, got {r}");
        }
    }

    #[test]
    fn impulse_response_nonzero_and_decays() {
        // Feed a unit impulse at 100% wet mix and verify:
        // 1. The first output sample is non-zero (convolution is active)
        // 2. The first sample sign matches the first IR coefficient
        // 3. The output tail decays toward zero
        //
        // Note: exact sample-by-sample comparison against the raw IR is not
        // valid here because the low-cut HPF (even at 20 Hz) introduces
        // cumulative phase/magnitude modification across the IR tail.
        let mut kernel = CabinetKernel::new(48000.0);
        let params = CabinetParams {
            mix_pct: 100.0,
            low_cut_hz: 20.0,
            output_db: 0.0,
            ir_select: 0.0,
        };

        let expected_ir = generate_ir(0, 48000.0);
        let (l0, _) = kernel.process_stereo(1.0, 1.0, &params);

        // First output must be non-zero
        assert!(
            l0.abs() > 1e-4,
            "Impulse response should produce non-zero output, got {l0}"
        );

        // Sign must match IR[0] (overall polarity is preserved)
        assert_eq!(
            l0.signum(),
            expected_ir[0].signum(),
            "First sample sign should match IR[0]={}, got {l0}",
            expected_ir[0]
        );

        // Process the tail — output should eventually decay
        let mut max_after = 0.0f32;
        for _ in 0..IR_LEN {
            let (l, _) = kernel.process_stereo(0.0, 0.0, &params);
            if l.abs() > max_after {
                max_after = l.abs();
            }
        }
        // After IR_LEN silent samples the tail must be substantially smaller
        // than the peak (IR has exponential decay, so this should hold easily)
        let (final_l, _) = kernel.process_stereo(0.0, 0.0, &params);
        assert!(
            final_l.abs() < l0.abs(),
            "Output should decay: initial={l0}, final={final_l}"
        );
    }

    #[test]
    fn ir_select_changes_output_character() {
        // Different IRs should produce different outputs for the same input
        let input = 0.5_f32;
        let mut outputs = [0.0f32; 3];

        for ir in 0..3u32 {
            let mut kernel = CabinetKernel::new(48000.0);
            let params = CabinetParams {
                ir_select: ir as f32,
                mix_pct: 100.0,
                low_cut_hz: 20.0,
                output_db: 0.0,
            };
            let (l, _) = kernel.process_stereo(input, input, &params);
            outputs[ir as usize] = l;
        }

        // At least some IRs should differ
        let all_same = outputs.windows(2).all(|w| (w[0] - w[1]).abs() < 1e-6);
        assert!(
            !all_same,
            "Different IR types should produce different outputs: {:?}",
            outputs
        );
    }

    #[test]
    fn dry_mix_passes_input_through() {
        let mut kernel = CabinetKernel::new(48000.0);
        let params = CabinetParams {
            mix_pct: 0.0,
            output_db: 0.0,
            ..Default::default()
        };

        let (l, r) = kernel.process_stereo(0.7, 0.7, &params);
        assert!(
            (l - 0.7).abs() < 1e-5,
            "Dry mix should pass input: expected 0.7, got {l}"
        );
        assert!(
            (r - 0.7).abs() < 1e-5,
            "Dry mix should pass input: expected 0.7, got {r}"
        );
    }

    #[test]
    fn finite_output_for_all_params() {
        let mut kernel = CabinetKernel::new(48000.0);
        let test_cases = [
            CabinetParams {
                ir_select: 0.0,
                mix_pct: 100.0,
                low_cut_hz: 80.0,
                output_db: 0.0,
            },
            CabinetParams {
                ir_select: 1.0,
                mix_pct: 50.0,
                low_cut_hz: 200.0,
                output_db: -6.0,
            },
            CabinetParams {
                ir_select: 2.0,
                mix_pct: 0.0,
                low_cut_hz: 500.0,
                output_db: 6.0,
            },
        ];

        for p in &test_cases {
            for _ in 0..64 {
                let (l, r) = kernel.process_stereo(0.5, -0.3, p);
                assert!(l.is_finite(), "L is not finite with {:?}", p);
                assert!(r.is_finite(), "R is not finite with {:?}", p);
            }
        }
    }

    #[test]
    fn params_descriptor_count() {
        assert_eq!(CabinetParams::COUNT, 4);
        for i in 0..CabinetParams::COUNT {
            assert!(
                CabinetParams::descriptor(i).is_some(),
                "Missing descriptor at {i}"
            );
        }
        assert!(CabinetParams::descriptor(CabinetParams::COUNT).is_none());
    }

    #[test]
    fn params_ids_correct() {
        assert_eq!(CabinetParams::descriptor(0).unwrap().id, ParamId(2200));
        assert_eq!(CabinetParams::descriptor(1).unwrap().id, ParamId(2201));
        assert_eq!(CabinetParams::descriptor(2).unwrap().id, ParamId(2202));
        assert_eq!(CabinetParams::descriptor(3).unwrap().id, ParamId(2203));
    }

    #[test]
    fn adapter_wraps_as_effect() {
        let mut adapter = KernelAdapter::new(CabinetKernel::new(48000.0), 48000.0);
        adapter.reset();
        let out = adapter.process(0.3);
        assert!(out.is_finite(), "Adapter output must be finite, got {out}");
    }

    #[test]
    fn adapter_param_count() {
        let adapter = KernelAdapter::new(CabinetKernel::new(48000.0), 48000.0);
        assert_eq!(adapter.param_count(), 4);
        assert_eq!(adapter.param_info(0).unwrap().name, "IR Select");
        assert_eq!(adapter.param_info(3).unwrap().name, "Output");
    }
}
