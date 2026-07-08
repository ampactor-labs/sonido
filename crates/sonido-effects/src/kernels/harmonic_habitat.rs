//! Harmonic Habitat kernel — pitch-aware modal reverb tank.
//!
//! `HarmonicHabitatKernel` is a true-stereo reverb whose late tail is colored by
//! a resonant modal bank. A lightweight input tracker estimates a stable
//! harmonic center, then the modal bank retunes around musically related ratios
//! (open fifth, major, minor, or neutral). The result is a reverb that can behave
//! like a normal room at low settings, or bloom into a tail that follows the
//! player's harmony.

use libm::{ceilf, expf, powf, roundf, sqrtf};
use sonido_core::fast_math::fast_sin_turns;
use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::kernel_params;
use sonido_core::{
    Biquad, InterpolatedDelay, Interpolation, OnePole, ParamDescriptor, ParamFlags, ParamId,
    ParamUnit, fast_db_to_linear, flush_denormal, peaking_eq_coefficients, wet_dry_mix_stereo,
};

// ── Reverb constants ────────────────────────────────────────────────────────

const FDN_TUNINGS_44K: [usize; 8] = [1291, 1433, 1559, 1693, 1789, 1877, 1993, 2137];
const FDN_MOD_RATES: [f32; 8] = [0.23, 0.31, 0.37, 0.43, 0.53, 0.61, 0.71, 0.83];
const FDN_MOD_DEPTH_MS: f32 = 0.22;
const REFERENCE_RATE: f32 = 44_100.0;
const MAX_PREDELAY_MS: f32 = 100.0;
const HADAMARD_SCALE: f32 = 0.353_553_39;

// ── Harmonic tracker / modal bank constants ─────────────────────────────────

const DEFAULT_ROOT_HZ: f32 = 220.0;
const MIN_TRACK_HZ: f32 = 55.0;
const MAX_TRACK_HZ: f32 = 1_200.0;
const TRACK_GATE: f32 = 0.003;
const NUM_MODES: usize = 6;
const MODE_LABELS: &[&str] = &["Neutral", "Major", "Minor", "Open Fifth"];

#[inline]
fn scale_to_rate(samples: usize, target_rate: f32) -> usize {
    (roundf(samples as f32 * target_rate / REFERENCE_RATE) as usize).max(1)
}

#[inline]
fn damping_to_hz(damping: f32) -> f32 {
    180.0 * powf(100.0, 1.0 - damping)
}

#[inline]
fn butterfly_at(buf: &mut [f32; 8], i: usize, j: usize) {
    let sum = buf[i] + buf[j];
    let diff = buf[i] - buf[j];
    buf[i] = sum;
    buf[j] = diff;
}

#[inline]
fn hadamard8(buf: &mut [f32; 8]) {
    butterfly_at(buf, 0, 1);
    butterfly_at(buf, 2, 3);
    butterfly_at(buf, 4, 5);
    butterfly_at(buf, 6, 7);
    butterfly_at(buf, 0, 2);
    butterfly_at(buf, 1, 3);
    butterfly_at(buf, 4, 6);
    butterfly_at(buf, 5, 7);
    butterfly_at(buf, 0, 4);
    butterfly_at(buf, 1, 5);
    butterfly_at(buf, 2, 6);
    butterfly_at(buf, 3, 7);
    for x in buf.iter_mut() {
        *x *= HADAMARD_SCALE;
    }
}

#[inline]
fn decode_mode(mode: f32) -> usize {
    ((mode + 0.5) as usize).min(3)
}

#[inline]
fn mode_ratios(mode: usize) -> [f32; NUM_MODES] {
    match mode {
        // Fundamental-forward, musically neutral over ambiguous harmony.
        0 => [1.0, 1.5, 2.0, 2.25, 3.0, 4.0],
        // 1, 5/4, 3/2, 2, 5/2, 3
        1 => [1.0, 1.25, 1.5, 2.0, 2.5, 3.0],
        // 1, 6/5, 3/2, 2, 12/5, 3
        2 => [1.0, 1.2, 1.5, 2.0, 2.4, 3.0],
        // Power-chord stack.
        _ => [1.0, 1.5, 2.0, 3.0, 4.0, 6.0],
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`HarmonicHabitatKernel`].
#[derive(Debug, Clone, Copy)]
pub struct HarmonicHabitatParams {
    /// Room size as a percentage (0-100%).
    pub room_size_pct: f32,
    /// Decay amount as a percentage (0-100%).
    pub decay_pct: f32,
    /// High-frequency damping (0-100%).
    pub damping_pct: f32,
    /// Predelay in milliseconds.
    pub predelay_ms: f32,
    /// Amount of pitch-aware modal coloration.
    pub harmonicity_pct: f32,
    /// How strongly input tracking drives the modal center.
    pub tracking_pct: f32,
    /// Pitch memory/inertia for the detected harmonic center.
    pub memory_pct: f32,
    /// Modal ratio set: 0=Neutral, 1=Major, 2=Minor, 3=Open Fifth.
    pub mode: f32,
    /// Stereo width percentage.
    pub width_pct: f32,
    /// Wet/dry mix percentage.
    pub mix_pct: f32,
    /// Output level in decibels.
    pub output_db: f32,
}

impl Default for HarmonicHabitatParams {
    fn default() -> Self {
        Self {
            room_size_pct: 55.0,
            decay_pct: 60.0,
            damping_pct: 45.0,
            predelay_ms: 18.0,
            harmonicity_pct: 45.0,
            tracking_pct: 60.0,
            memory_pct: 65.0,
            mode: 3.0,
            width_pct: 100.0,
            mix_pct: 35.0,
            output_db: 0.0,
        }
    }
}

impl HarmonicHabitatParams {
    /// Create parameters from normalized 0-1 hardware knob readings.
    #[allow(clippy::too_many_arguments)]
    pub fn from_knobs(
        room: f32,
        decay: f32,
        damping: f32,
        predelay: f32,
        harmonicity: f32,
        tracking: f32,
        memory: f32,
        mode: f32,
        width: f32,
        mix: f32,
        output: f32,
    ) -> Self {
        Self::from_normalized(&[
            room,
            decay,
            damping,
            predelay,
            harmonicity,
            tracking,
            memory,
            mode,
            width,
            mix,
            output,
        ])
    }
}

kernel_params! {
    HarmonicHabitatParams, this {
        [0] ParamDescriptor {
                name: "Room Size",
                short_name: "Room",
                unit: ParamUnit::Percent,
                min: 0.0,
                max: 100.0,
                default: 55.0,
                step: 1.0,
                ..ParamDescriptor::mix()
            }
            .with_id(ParamId(3600), "hab_room_size"),
            smoothing: SmoothingStyle::Slow,
            get: this.room_size_pct,
            set: |v| this.room_size_pct = v;

        [1] ParamDescriptor {
                name: "Decay",
                short_name: "Decay",
                unit: ParamUnit::Percent,
                min: 0.0,
                max: 100.0,
                default: 60.0,
                step: 1.0,
                ..ParamDescriptor::mix()
            }
            .with_id(ParamId(3601), "hab_decay"),
            smoothing: SmoothingStyle::Slow,
            get: this.decay_pct,
            set: |v| this.decay_pct = v;

        [2] ParamDescriptor {
                name: "Damping",
                short_name: "Damp",
                unit: ParamUnit::Percent,
                min: 0.0,
                max: 100.0,
                default: 45.0,
                step: 1.0,
                ..ParamDescriptor::mix()
            }
            .with_id(ParamId(3602), "hab_damping"),
            smoothing: SmoothingStyle::Slow,
            get: this.damping_pct,
            set: |v| this.damping_pct = v;

        [3] ParamDescriptor::custom("Pre-Delay", "PreDly", 0.0, 100.0, 18.0)
                .with_unit(ParamUnit::Milliseconds)
                .with_step(1.0)
                .with_id(ParamId(3603), "hab_predelay"),
            smoothing: SmoothingStyle::Interpolated,
            get: this.predelay_ms,
            set: |v| this.predelay_ms = v;

        [4] ParamDescriptor {
                name: "Harmonicity",
                short_name: "Harm",
                unit: ParamUnit::Percent,
                min: 0.0,
                max: 100.0,
                default: 45.0,
                step: 1.0,
                ..ParamDescriptor::mix()
            }
            .with_id(ParamId(3604), "hab_harmonicity"),
            smoothing: SmoothingStyle::Standard,
            get: this.harmonicity_pct,
            set: |v| this.harmonicity_pct = v;

        [5] ParamDescriptor {
                name: "Tracking",
                short_name: "Track",
                unit: ParamUnit::Percent,
                min: 0.0,
                max: 100.0,
                default: 60.0,
                step: 1.0,
                ..ParamDescriptor::mix()
            }
            .with_id(ParamId(3605), "hab_tracking"),
            smoothing: SmoothingStyle::Standard,
            get: this.tracking_pct,
            set: |v| this.tracking_pct = v;

        [6] ParamDescriptor {
                name: "Memory",
                short_name: "Memory",
                unit: ParamUnit::Percent,
                min: 0.0,
                max: 100.0,
                default: 65.0,
                step: 1.0,
                ..ParamDescriptor::mix()
            }
            .with_id(ParamId(3606), "hab_memory"),
            smoothing: SmoothingStyle::Slow,
            get: this.memory_pct,
            set: |v| this.memory_pct = v;

        [7] ParamDescriptor::custom("Mode", "Mode", 0.0, 3.0, 3.0)
                .with_unit(ParamUnit::None)
                .with_step(1.0)
                .with_id(ParamId(3607), "hab_mode")
                .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                .with_step_labels(MODE_LABELS),
            smoothing: SmoothingStyle::None,
            get: this.mode,
            set: |v| this.mode = v;

        [8] ParamDescriptor {
                name: "Stereo Width",
                short_name: "Width",
                unit: ParamUnit::Percent,
                min: 0.0,
                max: 100.0,
                default: 100.0,
                step: 1.0,
                ..ParamDescriptor::mix()
            }
            .with_id(ParamId(3608), "hab_width"),
            smoothing: SmoothingStyle::Standard,
            get: this.width_pct,
            set: |v| this.width_pct = v;

        [9] ParamDescriptor::mix().with_id(ParamId(3609), "hab_mix"),
            smoothing: SmoothingStyle::Standard,
            get: this.mix_pct,
            set: |v| this.mix_pct = v;

        [10] sonido_core::gain::output_param_descriptor().with_id(ParamId(3610), "hab_output"),
            smoothing: SmoothingStyle::Fast,
            get: this.output_db,
            set: |v| this.output_db = v;
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Internal tracker / modal bank
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy)]
struct HarmonicTracker {
    sample_rate: f32,
    prev_sample: f32,
    zc_counter: u32,
    env: f32,
    freq_hz: f32,
    confidence: f32,
}

impl HarmonicTracker {
    fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            prev_sample: 0.0,
            zc_counter: 0,
            env: 0.0,
            freq_hz: DEFAULT_ROOT_HZ,
            confidence: 0.0,
        }
    }

    fn reset(&mut self) {
        self.prev_sample = 0.0;
        self.zc_counter = 0;
        self.env = 0.0;
        self.freq_hz = DEFAULT_ROOT_HZ;
        self.confidence = 0.0;
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.reset();
    }

    #[inline]
    fn process(&mut self, input: f32, tracking: f32, memory: f32) -> (f32, f32) {
        let sr = self.sample_rate;
        let abs_in = input.abs();
        let attack = expf(-1.0 / (0.005 * sr));
        let release = expf(-1.0 / (0.080 * sr));
        if abs_in > self.env {
            self.env = abs_in + attack * (self.env - abs_in);
        } else {
            self.env = abs_in + release * (self.env - abs_in);
        }

        self.zc_counter = self.zc_counter.saturating_add(1);
        let min_period = (sr / MAX_TRACK_HZ) as u32;
        let max_period = (sr / MIN_TRACK_HZ) as u32;

        let period = self.zc_counter.min(max_period);

        if self.prev_sample <= 0.0 && input > 0.0 && period >= min_period && self.env > TRACK_GATE {
            let measured = sr / period as f32;
            let memory = memory.clamp(0.0, 1.0);
            let tracking = tracking.clamp(0.0, 1.0);
            let coeff = (0.02 + (1.0 - memory) * 0.18) * tracking;
            self.freq_hz += (measured - self.freq_hz) * coeff;
            self.confidence += (tracking - self.confidence) * 0.08;
            self.zc_counter = 0;
        } else if self.zc_counter > max_period {
            self.zc_counter = max_period;
            self.confidence *= 0.9995;
        } else {
            self.confidence *= 0.999_98;
        }

        self.prev_sample = input;
        (
            self.freq_hz.clamp(MIN_TRACK_HZ, MAX_TRACK_HZ),
            self.confidence.clamp(0.0, 1.0),
        )
    }
}

struct ModalBank {
    filters_l: [Biquad; NUM_MODES],
    filters_r: [Biquad; NUM_MODES],
    sample_rate: f32,
    cached_root: f32,
    cached_amount: f32,
    cached_mode: usize,
}

impl ModalBank {
    fn new(sample_rate: f32) -> Self {
        let mut bank = Self {
            filters_l: core::array::from_fn(|_| Biquad::new()),
            filters_r: core::array::from_fn(|_| Biquad::new()),
            sample_rate,
            cached_root: -1.0,
            cached_amount: -1.0,
            cached_mode: usize::MAX,
        };
        bank.update(DEFAULT_ROOT_HZ, 0.0, 0);
        bank
    }

    fn reset(&mut self) {
        for filter in &mut self.filters_l {
            filter.clear();
        }
        for filter in &mut self.filters_r {
            filter.clear();
        }
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.cached_root = -1.0;
        self.update(DEFAULT_ROOT_HZ, 0.0, 0);
    }

    fn update(&mut self, root_hz: f32, amount: f32, mode: usize) {
        if (root_hz - self.cached_root).abs() < 0.5
            && (amount - self.cached_amount).abs() < 0.005
            && mode == self.cached_mode
        {
            return;
        }
        self.cached_root = root_hz;
        self.cached_amount = amount;
        self.cached_mode = mode;

        let ratios = mode_ratios(mode);
        let gains = [5.5, 4.5, 3.7, 3.0, 2.4, 1.8];
        let q_base = 2.0 + amount * 8.0;
        let nyquist_safe = self.sample_rate * 0.45;

        for i in 0..NUM_MODES {
            let freq = (root_hz * ratios[i]).clamp(55.0, nyquist_safe);
            let gain_db = gains[i] * amount;
            let q = (q_base + i as f32 * 0.35).min(12.0);
            let coeffs = peaking_eq_coefficients(freq, q, gain_db, self.sample_rate);
            self.filters_l[i]
                .set_coefficients(coeffs.0, coeffs.1, coeffs.2, coeffs.3, coeffs.4, coeffs.5);
            self.filters_r[i]
                .set_coefficients(coeffs.0, coeffs.1, coeffs.2, coeffs.3, coeffs.4, coeffs.5);
        }
    }

    fn process(&mut self, left: f32, right: f32, amount: f32) -> (f32, f32) {
        if amount <= 0.001 {
            return (left, right);
        }

        let mut modal_l = left;
        let mut modal_r = right;
        for i in 0..NUM_MODES {
            modal_l = self.filters_l[i].process(modal_l);
            modal_r = self.filters_r[i].process(modal_r);
        }

        let dry = 1.0 - amount;
        (
            flush_denormal(left * dry + modal_l * amount),
            flush_denormal(right * dry + modal_r * amount),
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP pitch-aware reverb kernel.
pub struct HarmonicHabitatKernel {
    fdn_delays: [InterpolatedDelay; 8],
    fdn_damping: [OnePole; 8],
    fdn_base_delays: [f32; 8],
    fdn_mod_depth: f32,
    fdn_phases: [f32; 8],
    fdn_phase_incs: [f32; 8],
    predelay_l: InterpolatedDelay,
    predelay_r: InterpolatedDelay,
    tracker: HarmonicTracker,
    modal_bank: ModalBank,
    sample_rate: f32,
    cached_room: f32,
    cached_decay: f32,
    cached_damp: f32,
    feedback: f32,
    compensation: f32,
    root_smooth_hz: f32,
}

impl HarmonicHabitatKernel {
    /// Create a new Harmonic Habitat kernel at `sample_rate`.
    pub fn new(sample_rate: f32) -> Self {
        let mod_depth = FDN_MOD_DEPTH_MS * 0.001 * sample_rate;
        let fdn_delays: [InterpolatedDelay; 8] = core::array::from_fn(|i| {
            let base = scale_to_rate(FDN_TUNINGS_44K[i], sample_rate) as f32;
            let capacity = (base + mod_depth) as usize + 4;
            let mut delay = InterpolatedDelay::new(capacity);
            delay.set_interpolation(Interpolation::Linear);
            delay
        });
        let damping_hz = damping_to_hz(0.45);
        let fdn_damping = core::array::from_fn(|_| OnePole::new(sample_rate, damping_hz));
        let fdn_base_delays =
            core::array::from_fn(|i| scale_to_rate(FDN_TUNINGS_44K[i], sample_rate) as f32);
        let fdn_phase_incs = core::array::from_fn(|i| FDN_MOD_RATES[i] / sample_rate);
        let max_predelay = (ceilf(MAX_PREDELAY_MS * 0.001 * sample_rate) as usize).max(1);

        let mut kernel = Self {
            fdn_delays,
            fdn_damping,
            fdn_base_delays,
            fdn_mod_depth: mod_depth,
            fdn_phases: [0.0; 8],
            fdn_phase_incs,
            predelay_l: InterpolatedDelay::new(max_predelay),
            predelay_r: InterpolatedDelay::new(max_predelay),
            tracker: HarmonicTracker::new(sample_rate),
            modal_bank: ModalBank::new(sample_rate),
            sample_rate,
            cached_room: -1.0,
            cached_decay: -1.0,
            cached_damp: -1.0,
            feedback: 0.0,
            compensation: 1.0,
            root_smooth_hz: DEFAULT_ROOT_HZ,
        };
        kernel.update_derived(0.55, 0.60, 0.45);
        kernel
    }

    #[inline]
    fn update_derived(&mut self, room: f32, decay: f32, damp: f32) {
        if (room - self.cached_room).abs() < 0.001
            && (decay - self.cached_decay).abs() < 0.001
            && (damp - self.cached_damp).abs() < 0.001
        {
            return;
        }

        self.cached_room = room;
        self.cached_decay = decay;
        self.cached_damp = damp;

        let scaled_room = 0.30 + room * 0.66;
        self.feedback = (scaled_room + decay * (0.985 - scaled_room)).clamp(0.0, 0.985);
        self.compensation = sqrtf((1.0 - self.feedback).max(0.012));
        let damping_hz = damping_to_hz(damp);
        for filter in &mut self.fdn_damping {
            filter.set_frequency(damping_hz);
        }
    }

    #[inline]
    fn apply_predelay(line: &mut InterpolatedDelay, input: f32, predelay: f32) -> f32 {
        if predelay > 0.5 {
            line.read_write(input, predelay)
        } else {
            line.write(input);
            input
        }
    }

    #[inline]
    fn process_fdn(&mut self, input: f32) -> (f32, f32) {
        let mut raw = [0.0f32; 8];
        for i in 0..8 {
            let modulated =
                self.fdn_base_delays[i] + self.fdn_mod_depth * fast_sin_turns(self.fdn_phases[i]);
            raw[i] = self.fdn_delays[i].read(modulated);
        }

        let mut mixed = raw;
        hadamard8(&mut mixed);

        for i in 0..8 {
            let damped = self.fdn_damping[i].process(mixed[i]);
            let injected = input * if i % 2 == 0 { 0.82 } else { -0.82 };
            self.fdn_delays[i].write(flush_denormal(injected + damped * self.feedback));

            self.fdn_phases[i] += self.fdn_phase_incs[i];
            if self.fdn_phases[i] >= 1.0 {
                self.fdn_phases[i] -= 1.0;
            }
        }

        let wet_l = (raw[0] + raw[2] + raw[4] + raw[6]) * 0.25 * self.compensation;
        let wet_r = (raw[1] + raw[3] + raw[5] + raw[7]) * 0.25 * self.compensation;
        (wet_l, wet_r)
    }

    #[inline]
    fn update_harmonic_center(&mut self, mono: f32, tracking: f32, memory: f32) -> (f32, f32) {
        let (tracked, confidence) = self.tracker.process(mono, tracking, memory);
        let target = if tracking > 0.001 {
            tracked
        } else {
            DEFAULT_ROOT_HZ
        };
        let tau = 0.035 + memory * 1.2;
        let coeff = 1.0 - expf(-1.0 / (tau * self.sample_rate));
        self.root_smooth_hz += (target - self.root_smooth_hz) * coeff;
        (self.root_smooth_hz, confidence)
    }

    #[cfg(test)]
    fn tracked_frequency_hz(&self) -> f32 {
        self.root_smooth_hz
    }
}

impl DspKernel for HarmonicHabitatKernel {
    type Params = HarmonicHabitatParams;

    fn process_stereo(
        &mut self,
        left: f32,
        right: f32,
        params: &HarmonicHabitatParams,
    ) -> (f32, f32) {
        let room = params.room_size_pct * 0.01;
        let decay = params.decay_pct * 0.01;
        let damp = params.damping_pct * 0.01;
        let predelay_samples = (params.predelay_ms * 0.001 * self.sample_rate)
            .clamp(0.0, MAX_PREDELAY_MS * 0.001 * self.sample_rate);
        let tracking = params.tracking_pct * 0.01;
        let memory = params.memory_pct * 0.01;
        let harmonicity = params.harmonicity_pct * 0.01;
        let width = params.width_pct * 0.01;
        let mix = params.mix_pct * 0.01;
        let output = fast_db_to_linear(params.output_db);
        let mode = decode_mode(params.mode);

        self.update_derived(room, decay, damp);

        let pre_l = Self::apply_predelay(&mut self.predelay_l, left, predelay_samples);
        let pre_r = Self::apply_predelay(&mut self.predelay_r, right, predelay_samples);
        let mono = (pre_l + pre_r) * 0.5;

        let (root_hz, confidence) = self.update_harmonic_center(mono, tracking, memory);
        let modal_amount = (harmonicity * tracking * (0.25 + confidence * 0.75)).clamp(0.0, 1.0);

        let (mut wet_l, mut wet_r) = self.process_fdn(mono);
        self.modal_bank.update(root_hz, modal_amount, mode);
        (wet_l, wet_r) = self.modal_bank.process(wet_l, wet_r, modal_amount);

        let mid = (wet_l + wet_r) * 0.5;
        let side = (wet_l - wet_r) * 0.5;
        let final_l = mid + side * width;
        let final_r = mid - side * width;

        let (out_l, out_r) = wet_dry_mix_stereo(left, right, final_l, final_r, mix);
        (
            sonido_core::math::soft_limit(out_l * output, 3.0),
            sonido_core::math::soft_limit(out_r * output, 3.0),
        )
    }

    fn reset(&mut self) {
        for delay in &mut self.fdn_delays {
            delay.clear();
        }
        for filter in &mut self.fdn_damping {
            filter.reset();
        }
        self.fdn_phases = [0.0; 8];
        self.predelay_l.clear();
        self.predelay_r.clear();
        self.tracker.reset();
        self.modal_bank.reset();
        self.root_smooth_hz = DEFAULT_ROOT_HZ;
        self.cached_room = -1.0;
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        let mod_depth = FDN_MOD_DEPTH_MS * 0.001 * sample_rate;
        self.fdn_mod_depth = mod_depth;
        self.fdn_delays = core::array::from_fn(|i| {
            let base = scale_to_rate(FDN_TUNINGS_44K[i], sample_rate) as f32;
            let capacity = (base + mod_depth) as usize + 4;
            let mut delay = InterpolatedDelay::new(capacity);
            delay.set_interpolation(Interpolation::Linear);
            delay
        });
        self.fdn_base_delays =
            core::array::from_fn(|i| scale_to_rate(FDN_TUNINGS_44K[i], sample_rate) as f32);
        self.fdn_phase_incs = core::array::from_fn(|i| FDN_MOD_RATES[i] / sample_rate);
        for filter in &mut self.fdn_damping {
            filter.set_sample_rate(sample_rate);
        }
        let max_predelay = (ceilf(MAX_PREDELAY_MS * 0.001 * sample_rate) as usize).max(1);
        self.predelay_l = InterpolatedDelay::new(max_predelay);
        self.predelay_r = InterpolatedDelay::new(max_predelay);
        self.tracker.set_sample_rate(sample_rate);
        self.modal_bank.set_sample_rate(sample_rate);
        self.root_smooth_hz = DEFAULT_ROOT_HZ;
        self.fdn_phases = [0.0; 8];
        self.cached_room = -1.0;
    }

    fn is_true_stereo(&self) -> bool {
        true
    }

    fn latency_samples(&self) -> usize {
        0
    }

    fn tail_samples(&self) -> usize {
        ((1.0 + self.cached_decay * 5.0) * self.sample_rate) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_core::kernel::Adapter;
    use sonido_core::{Effect, ParameterInfo};

    fn sine(freq: f32, i: usize, sr: f32) -> f32 {
        libm::sinf(core::f32::consts::TAU * freq * i as f32 / sr)
    }

    #[test]
    fn silence_in_silence_out() {
        let mut kernel = HarmonicHabitatKernel::new(48_000.0);
        let params = HarmonicHabitatParams::default();
        let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
        assert!(l.abs() < 1e-6);
        assert!(r.abs() < 1e-6);
    }

    #[test]
    fn no_nan_or_inf_with_hot_input() {
        let mut kernel = HarmonicHabitatKernel::new(48_000.0);
        let params = HarmonicHabitatParams {
            room_size_pct: 90.0,
            decay_pct: 95.0,
            damping_pct: 10.0,
            harmonicity_pct: 100.0,
            tracking_pct: 100.0,
            mix_pct: 100.0,
            ..Default::default()
        };

        for i in 0..12_000 {
            let x = sine(110.0, i, 48_000.0) * 1.2;
            let (l, r) = kernel.process_stereo(x, x * 0.7, &params);
            assert!(l.is_finite(), "left non-finite at {i}: {l}");
            assert!(r.is_finite(), "right non-finite at {i}: {r}");
            assert!(l.abs() <= 3.01, "left exceeded limiter at {i}: {l}");
            assert!(r.abs() <= 3.01, "right exceeded limiter at {i}: {r}");
        }
    }

    #[test]
    fn params_descriptor_count() {
        assert_eq!(HarmonicHabitatParams::COUNT, 11);
        for i in 0..HarmonicHabitatParams::COUNT {
            assert!(HarmonicHabitatParams::descriptor(i).is_some());
        }
        assert!(HarmonicHabitatParams::descriptor(HarmonicHabitatParams::COUNT).is_none());
    }

    #[test]
    fn mode_has_labels() {
        let desc = HarmonicHabitatParams::descriptor(7).unwrap();
        assert!(desc.flags.contains(ParamFlags::STEPPED));
        assert_eq!(desc.step_labels, Some(MODE_LABELS));
    }

    #[test]
    fn adapter_wraps_as_effect() {
        let mut adapter = Adapter::new(HarmonicHabitatKernel::new(48_000.0), 48_000.0);
        adapter.reset();
        let output = adapter.process(0.3);
        assert!(output.is_finite());
        assert_eq!(adapter.param_count(), HarmonicHabitatParams::COUNT);
    }

    #[test]
    fn steady_sine_retunes_harmonic_center() {
        let mut kernel = HarmonicHabitatKernel::new(48_000.0);
        let params = HarmonicHabitatParams {
            tracking_pct: 100.0,
            memory_pct: 0.0,
            harmonicity_pct: 100.0,
            ..Default::default()
        };

        for i in 0..48_000 {
            let x = sine(330.0, i, 48_000.0) * 0.5;
            let _ = kernel.process_stereo(x, x, &params);
        }

        let tracked = kernel.tracked_frequency_hz();
        assert!(
            (tracked - 330.0).abs() < 35.0,
            "expected tracker near 330 Hz, got {tracked}"
        );
    }

    #[test]
    fn tracking_disabled_stays_near_default_center() {
        let mut kernel = HarmonicHabitatKernel::new(48_000.0);
        let params = HarmonicHabitatParams {
            tracking_pct: 0.0,
            harmonicity_pct: 100.0,
            ..Default::default()
        };

        for i in 0..48_000 {
            let x = sine(440.0, i, 48_000.0) * 0.5;
            let _ = kernel.process_stereo(x, x, &params);
        }

        assert!((kernel.tracked_frequency_hz() - DEFAULT_ROOT_HZ).abs() < 1.0);
    }

    #[test]
    fn reset_clears_tail() {
        let mut kernel = HarmonicHabitatKernel::new(48_000.0);
        let params = HarmonicHabitatParams {
            mix_pct: 100.0,
            ..Default::default()
        };

        let _ = kernel.process_stereo(1.0, 1.0, &params);
        for _ in 0..2_000 {
            let _ = kernel.process_stereo(0.0, 0.0, &params);
        }
        kernel.reset();

        for i in 0..128 {
            let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
            assert!(l.abs() < 1e-6, "left tail after reset at {i}: {l}");
            assert!(r.abs() < 1e-6, "right tail after reset at {i}: {r}");
        }
    }
}
