//! Delay kernel — stereo/ping-pong delay with diffusion, filtering, and tempo sync.
//!
//! `DelayKernel` owns DSP state (delay lines, feedback filters, diffusion
//! allpasses, tempo manager). Parameters are received via `&DelayParams` each
//! sample. Deployed via [`KernelAdapter`](sonido_core::KernelAdapter) for
//! desktop/plugin, or called directly on embedded targets.
//!
//! # Algorithm
//!
//! Classic feedback delay with a full analog-modelling feedback path:
//!
//! ```text
//! input ──►(+)──► [delay line L] ──► delayed_L ──► wet/dry mix ──► output L
//!           ▲                              │
//!           │    ◄── LP filter ◄───────────┤
//!           │    ◄── HP filter ◄──────────┘
//!           └──── allpass diffusion ◄──────┘
//! ```
//!
//! In ping-pong mode the feedback crosses channels:
//! - L delay receives `left_input + filtered(delayed_R) × feedback`
//! - R delay receives `right_input + filtered(delayed_L) × feedback`
//!
//! Diffusion is a two-stage Schroeder allpass cascade (13 ms and 7 ms prime
//! delays) applied inside the feedback path for smeared, tape-like echoes.
//!
//! Tempo sync overrides the manual delay time with a musical note division
//! at the host BPM — recomputed each time [`DspKernel::set_tempo_context()`]
//! is called.
//!
//! # References
//!
//! - Feedback filtering: standard analog delay modelling (Zölzer, "DAFX" Ch. 6)
//! - Diffusion: Schroeder allpass cascade (prime delay lengths 13 ms, 7 ms)
//! - Tempo sync: musical note divisions via [`NoteDivision`]
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! let adapter = KernelAdapter::new(DelayKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing, ADCs are hardware-filtered)
//! let mut kernel = DelayKernel::new(48000.0);
//! let params = DelayParams::from_knobs(0.5, 0.4, 0.5, 0.0, 1.0, 0.0, 0.0, 0.0, 2.0, 0.5);
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

use libm::{ceilf, logf};
use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::{
    AllpassFilter, Biquad, DIVISION_LABELS, InterpolatedDelay, NoteDivision, OnePole,
    ParamDescriptor, ParamFlags, ParamId, ParamScale, ParamUnit, TempoContext, TempoManager,
    fast_db_to_linear, flush_denormal, highpass_coefficients, index_to_division, wet_dry_mix,
    wet_dry_mix_stereo,
};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum delay time in seconds (2 seconds).
///
/// All delay line allocations use this as the upper bound, regardless of
/// the current `time_ms` parameter value.
const MAX_DELAY_S: f32 = 2.0;

/// Threshold below which cached coefficient values are considered unchanged.
///
/// Avoids recalculating LP/HP Biquad coefficients on every sample when the
/// filter frequency has not moved by a perceptible amount.
const COEFF_CHANGE_THRESHOLD: f32 = 0.01;

/// First diffusion allpass delay in seconds (13 ms prime).
///
/// Chosen as a prime number of milliseconds to minimise periodic alignment
/// with the second stage and the main delay time.
const DIFFUSION_AP1_S: f32 = 0.013;

/// Second diffusion allpass delay in seconds (7 ms prime).
const DIFFUSION_AP2_S: f32 = 0.007;

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`DelayKernel`].
///
/// All values are in **user-facing units** — the same units shown in GUIs
/// and stored in presets. The kernel converts internally as needed.
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `time_ms` | ms | 1–2000 | 300.0 |
/// | 1 | `feedback_pct` | % | 0–95 | 40.0 |
/// | 2 | `mix_pct` | % | 0–100 | 50.0 |
/// | 3 | `ping_pong` | index | 0–1 | 0 (Off) |
/// | 4 | `fb_lp_hz` | Hz | 200–20000 | 20000.0 |
/// | 5 | `fb_hp_hz` | Hz | 20–2000 | 20.0 |
/// | 6 | `diffusion_pct` | % | 0–100 | 0.0 |
/// | 7 | `sync` | index | 0–1 | 0 (Off) |
/// | 8 | `division` | index | 0–11 | 2 (Quarter) |
/// | 9 | `output_db` | dB | −20–6 | 0.0 |
#[derive(Debug, Clone, Copy)]
pub struct DelayParams {
    /// Delay time in milliseconds.
    ///
    /// Range: 1.0 to 2000.0 ms. Ignored when `sync > 0.5`.
    pub time_ms: f32,

    /// Feedback amount in percent.
    ///
    /// Range: 0.0 to 95.0 %. Values above ~70 % produce long, self-oscillating
    /// tails. The 95 % cap prevents unbounded growth.
    pub feedback_pct: f32,

    /// Wet/dry mix in percent.
    ///
    /// Range: 0.0 to 100.0 %. 0 % = fully dry, 100 % = fully wet.
    pub mix_pct: f32,

    /// Ping-pong stereo mode: 0.0 = Off, 1.0 = On.
    ///
    /// When enabled, feedback alternates between left and right channels,
    /// creating a bouncing stereo delay effect. Causes `is_true_stereo()` to
    /// return `true`.
    pub ping_pong: f32,

    /// Feedback lowpass cutoff frequency in Hz.
    ///
    /// Range: 200.0 to 20000.0 Hz. At 20 kHz the filter is effectively
    /// bypassed. Lower values darken each repeat, simulating analog tape
    /// or bucket-brigade HF roll-off.
    pub fb_lp_hz: f32,

    /// Feedback highpass cutoff frequency in Hz.
    ///
    /// Range: 20.0 to 2000.0 Hz. At 20 Hz the filter is effectively bypassed.
    /// Higher values thin each repeat, preventing bass build-up in long tails.
    pub fb_hp_hz: f32,

    /// Diffusion amount in percent.
    ///
    /// Range: 0.0 to 100.0 %. At 0 % the allpass filters are bypassed.
    /// At 100 %, allpass feedback is 0.6, producing maximum temporal smearing
    /// for a tape-echo or plate-reverb-like repeat texture.
    pub diffusion_pct: f32,

    /// Tempo sync enable: 0.0 = Off, 1.0 = On.
    ///
    /// When enabled, the delay time is derived from the host BPM and the
    /// `division` index, overriding the manual `time_ms` parameter.
    pub sync: f32,

    /// Note division index for tempo sync (0–11).
    ///
    /// Mapped via `index_to_division()`. Only active when `sync > 0.5`.
    /// Default 2 = Quarter note.
    pub division: f32,

    /// Output level in decibels.
    ///
    /// Range: −20.0 to +20.0 dB. Applied after the wet/dry mix.
    pub output_db: f32,
}

impl Default for DelayParams {
    /// Defaults match the classic `Delay` effect's descriptor defaults exactly.
    fn default() -> Self {
        Self {
            time_ms: 300.0,
            feedback_pct: 40.0,
            mix_pct: 50.0,
            ping_pong: 0.0,
            fb_lp_hz: 20000.0,
            fb_hp_hz: 20.0,
            diffusion_pct: 0.0,
            sync: 0.0,
            division: 2.0,
            output_db: 0.0,
        }
    }
}

impl DelayParams {
    /// Creates parameters from normalized 0–1 knob readings.
    ///
    /// Curves (logarithmic for frequency/time, linear for percentage) are
    /// derived from [`ParamDescriptor`] — same mapping as GUI and plugin hosts.
    ///
    /// # Parameters
    ///
    /// - `time`: Delay time knob → 1–2000 ms (logarithmic)
    /// - `feedback`: Feedback knob → 0–95 %
    /// - `mix`: Mix knob → 0–100 %
    /// - `ping_pong`: Toggle switch → 0.0 = Off, 1.0 = On (stepped)
    /// - `fb_lp`: LP cutoff knob → 200–20000 Hz (logarithmic)
    /// - `fb_hp`: HP cutoff knob → 20–2000 Hz (logarithmic)
    /// - `diffusion`: Diffusion knob → 0–100 %
    /// - `sync`: Sync toggle → 0.0 = Off, 1.0 = On (stepped)
    /// - `division`: Division selector → index 0–11 (stepped)
    /// - `output`: Output knob → −20–+6 dB
    #[allow(clippy::too_many_arguments)]
    pub fn from_knobs(
        time: f32,
        feedback: f32,
        mix: f32,
        ping_pong: f32,
        fb_lp: f32,
        fb_hp: f32,
        diffusion: f32,
        sync: f32,
        division: f32,
        output: f32,
    ) -> Self {
        Self::from_normalized(&[
            time, feedback, mix, ping_pong, fb_lp, fb_hp, diffusion, sync, division, output,
        ])
    }
}

impl KernelParams for DelayParams {
    const COUNT: usize = 10;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(
                ParamDescriptor::time_ms("Delay Time", "Time", 1.0, 2000.0, 300.0)
                    .with_id(ParamId(1100), "dly_time")
                    .with_scale(ParamScale::Logarithmic),
            ),
            1 => Some(
                ParamDescriptor {
                    name: "Feedback",
                    short_name: "Feedback",
                    unit: ParamUnit::Percent,
                    min: 0.0,
                    max: 95.0,
                    default: 40.0,
                    step: 1.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(1101), "dly_feedback"),
            ),
            2 => Some(ParamDescriptor::mix().with_id(ParamId(1102), "dly_mix")),
            3 => Some(
                ParamDescriptor {
                    name: "Ping Pong",
                    short_name: "PngPng",
                    unit: ParamUnit::None,
                    min: 0.0,
                    max: 1.0,
                    default: 0.0,
                    step: 1.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(1103), "dly_ping_pong")
                .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                .with_step_labels(&["Off", "On"]),
            ),
            4 => Some(
                ParamDescriptor::custom("Feedback LP", "Fb LP", 200.0, 20000.0, 20000.0)
                    .with_id(ParamId(1105), "dly_fb_lp")
                    .with_unit(ParamUnit::Hertz)
                    .with_scale(ParamScale::Logarithmic),
            ),
            5 => Some(
                ParamDescriptor::custom("Feedback HP", "Fb HP", 20.0, 2000.0, 20.0)
                    .with_id(ParamId(1106), "dly_fb_hp")
                    .with_unit(ParamUnit::Hertz)
                    .with_scale(ParamScale::Logarithmic),
            ),
            6 => Some(
                ParamDescriptor {
                    name: "Diffusion",
                    short_name: "Diff",
                    unit: ParamUnit::Percent,
                    min: 0.0,
                    max: 100.0,
                    default: 0.0,
                    step: 1.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(1107), "dly_diffusion"),
            ),
            7 => Some(
                ParamDescriptor {
                    name: "Sync",
                    short_name: "Sync",
                    unit: ParamUnit::None,
                    min: 0.0,
                    max: 1.0,
                    default: 0.0,
                    step: 1.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(1108), "dly_sync")
                .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                .with_step_labels(&["Off", "On"]),
            ),
            8 => Some(
                ParamDescriptor {
                    name: "Division",
                    short_name: "Div",
                    unit: ParamUnit::None,
                    min: 0.0,
                    max: 11.0,
                    default: 2.0,
                    step: 1.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(1109), "dly_division")
                .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                .with_step_labels(DIVISION_LABELS),
            ),
            9 => Some(
                sonido_core::gain::output_param_descriptor().with_id(ParamId(1104), "dly_output"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Interpolated, // time_ms — 50 ms prevents pitch-shift clicks
            1 => SmoothingStyle::Standard,     // feedback_pct — 10 ms
            2 => SmoothingStyle::Standard,     // mix_pct — 10 ms
            3 => SmoothingStyle::None,         // ping_pong — stepped toggle, snap
            4 => SmoothingStyle::Slow,         // fb_lp_hz — 20 ms, filter coefficients
            5 => SmoothingStyle::Slow,         // fb_hp_hz — 20 ms, filter coefficients
            6 => SmoothingStyle::Standard,     // diffusion_pct — 10 ms
            7 => SmoothingStyle::None,         // sync — stepped toggle, snap
            8 => SmoothingStyle::None,         // division — stepped enum, snap
            9 => SmoothingStyle::Standard,     // output_db — 10 ms
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.time_ms,
            1 => self.feedback_pct,
            2 => self.mix_pct,
            3 => self.ping_pong,
            4 => self.fb_lp_hz,
            5 => self.fb_hp_hz,
            6 => self.diffusion_pct,
            7 => self.sync,
            8 => self.division,
            9 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.time_ms = value,
            1 => self.feedback_pct = value,
            2 => self.mix_pct = value,
            3 => self.ping_pong = value,
            4 => self.fb_lp_hz = value,
            5 => self.fb_hp_hz = value,
            6 => self.diffusion_pct = value,
            7 => self.sync = value,
            8 => self.division = value,
            9 => self.output_db = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP delay kernel.
///
/// Contains ONLY the mutable state required for audio processing:
///
/// - Two [`InterpolatedDelay`] lines (L/R) with 2-second maximum
/// - One-pole lowpass filters in the feedback path (L/R)
/// - Biquad highpass filters in the feedback path (L/R)
/// - Schroeder allpass diffusers — two per channel (L/R × 2)
/// - A [`TempoManager`] for tempo-synced delay times
/// - Cached filter frequencies to avoid unnecessary coefficient recalculations
/// - Cached `ping_pong` flag for `is_true_stereo()`
///
/// No `SmoothedParam`, no atomics, no platform awareness. The kernel is
/// `Send`-safe because all contained types are `Send`.
///
/// ## Coefficient Caching
///
/// LP/HP filter frequencies and the diffusion amount are cached. Coefficients
/// are only recalculated when the incoming parameter value differs from the
/// cached value by more than `COEFF_CHANGE_THRESHOLD`. This avoids expensive
/// `highpass_coefficients()` calls every sample during steady-state operation.
pub struct DelayKernel {
    /// Interpolated delay line — left channel.
    delay_line_l: InterpolatedDelay,
    /// Interpolated delay line — right channel.
    delay_line_r: InterpolatedDelay,
    /// One-pole lowpass filter in the feedback path — left channel.
    feedback_lp_l: OnePole,
    /// One-pole lowpass filter in the feedback path — right channel.
    feedback_lp_r: OnePole,
    /// Biquad highpass filter in the feedback path — left channel.
    feedback_hp_l: Biquad,
    /// Biquad highpass filter in the feedback path — right channel.
    feedback_hp_r: Biquad,
    /// First Schroeder allpass diffuser — left channel (13 ms prime delay).
    diffusion_ap1_l: AllpassFilter,
    /// Second Schroeder allpass diffuser — left channel (7 ms prime delay).
    diffusion_ap2_l: AllpassFilter,
    /// First Schroeder allpass diffuser — right channel (13 ms prime delay).
    diffusion_ap1_r: AllpassFilter,
    /// Second Schroeder allpass diffuser — right channel (7 ms prime delay).
    diffusion_ap2_r: AllpassFilter,
    /// Tempo manager for BPM-synced delay times.
    tempo: TempoManager,
    /// Current sample rate in Hz.
    sample_rate: f32,
    /// Maximum number of delay samples (= 2 s × sample_rate).
    max_delay_samples: f32,
    /// Cached lowpass cutoff frequency (Hz) — guards coefficient recalculation.
    cached_lp_freq: f32,
    /// Cached highpass cutoff frequency (Hz) — guards coefficient recalculation.
    cached_hp_freq: f32,
    /// Cached diffusion fraction (0–1) — guards allpass feedback recalculation.
    cached_diffusion: f32,
    /// Cached ping-pong state — used by `is_true_stereo()`.
    ///
    /// Updated each sample from `params.ping_pong` so the method always
    /// reflects the most recent processed frame.
    cached_ping_pong: bool,

    /// Cached delay time in samples from the last `process_stereo()` call.
    ///
    /// Used by `tail_samples()` to estimate ring-out duration.
    cached_delay_samples: f32,

    /// Cached feedback fraction (0–0.95) from the last `process_stereo()` call.
    ///
    /// Used by `tail_samples()` to estimate the number of audible repeats.
    cached_feedback: f32,
}

impl DelayKernel {
    /// Create a new delay kernel at the given sample rate.
    ///
    /// Allocates two 2-second delay lines, four allpass diffusers (2 per
    /// channel), and initialises all filters at their bypass frequencies
    /// (LP at 20 kHz, HP at 20 Hz).
    ///
    /// # Parameters
    ///
    /// - `sample_rate`: Audio sample rate in Hz (e.g. 44100.0, 48000.0).
    pub fn new(sample_rate: f32) -> Self {
        let max_delay_samples = ceilf(MAX_DELAY_S * sample_rate) as usize;

        let ap1_samples = (DIFFUSION_AP1_S * sample_rate) as usize;
        let ap2_samples = (DIFFUSION_AP2_S * sample_rate) as usize;

        // Initialise the HP biquad at 20 Hz — effectively bypassed at start.
        let mut feedback_hp_l = Biquad::new();
        let mut feedback_hp_r = Biquad::new();
        let (b0, b1, b2, a0, a1, a2) = highpass_coefficients(20.0, 0.707, sample_rate);
        feedback_hp_l.set_coefficients(b0, b1, b2, a0, a1, a2);
        feedback_hp_r.set_coefficients(b0, b1, b2, a0, a1, a2);

        Self {
            delay_line_l: InterpolatedDelay::new(max_delay_samples),
            delay_line_r: InterpolatedDelay::new(max_delay_samples),
            feedback_lp_l: OnePole::new(sample_rate, 20000.0),
            feedback_lp_r: OnePole::new(sample_rate, 20000.0),
            feedback_hp_l,
            feedback_hp_r,
            diffusion_ap1_l: AllpassFilter::new(ap1_samples.max(1)),
            diffusion_ap2_l: AllpassFilter::new(ap2_samples.max(1)),
            diffusion_ap1_r: AllpassFilter::new(ap1_samples.max(1)),
            diffusion_ap2_r: AllpassFilter::new(ap2_samples.max(1)),
            tempo: TempoManager::new(sample_rate, 120.0),
            sample_rate,
            max_delay_samples: max_delay_samples as f32,
            cached_lp_freq: 20000.0,
            cached_hp_freq: 20.0,
            cached_diffusion: 0.0,
            cached_ping_pong: false,
            cached_delay_samples: 300.0 / 1000.0 * sample_rate, // default 300 ms
            cached_feedback: 0.4,                               // default 40 %
        }
    }

    /// Recompute LP/HP filter coefficients and allpass feedback when parameters
    /// have changed beyond `COEFF_CHANGE_THRESHOLD`.
    ///
    /// Called once per sample inside `process_stereo()`. The threshold check
    /// keeps this practically zero-cost during steady-state operation.
    #[inline]
    fn update_coefficients(&mut self, params: &DelayParams) {
        // ── Lowpass ──
        let lp_hz = params.fb_lp_hz.clamp(200.0, 20000.0);
        if (lp_hz - self.cached_lp_freq).abs() > COEFF_CHANGE_THRESHOLD {
            self.cached_lp_freq = lp_hz;
            self.feedback_lp_l.set_frequency(lp_hz);
            self.feedback_lp_r.set_frequency(lp_hz);
        }

        // ── Highpass ──
        let hp_hz = params.fb_hp_hz.clamp(20.0, 2000.0);
        if (hp_hz - self.cached_hp_freq).abs() > COEFF_CHANGE_THRESHOLD {
            self.cached_hp_freq = hp_hz;
            let (b0, b1, b2, a0, a1, a2) = highpass_coefficients(hp_hz, 0.707, self.sample_rate);
            self.feedback_hp_l.set_coefficients(b0, b1, b2, a0, a1, a2);
            self.feedback_hp_r.set_coefficients(b0, b1, b2, a0, a1, a2);
        }

        // ── Diffusion allpass feedback ──
        let diffusion = params.diffusion_pct / 100.0; // 0–1
        if (diffusion - self.cached_diffusion).abs() > COEFF_CHANGE_THRESHOLD {
            self.cached_diffusion = diffusion;
            let ap_feedback = diffusion * 0.6;
            self.diffusion_ap1_l.set_feedback(ap_feedback);
            self.diffusion_ap2_l.set_feedback(ap_feedback);
            self.diffusion_ap1_r.set_feedback(ap_feedback);
            self.diffusion_ap2_r.set_feedback(ap_feedback);
        }
    }

    /// Compute the effective delay time in samples, honouring tempo sync.
    ///
    /// When `params.sync > 0.5`, the delay time is computed from the current
    /// BPM and the `params.division` index via [`TempoManager::division_to_ms()`].
    /// Otherwise converts `params.time_ms` to samples and clamps to the buffer
    /// range `[1, max_delay_samples − 1]`.
    #[inline]
    fn effective_delay_samples(&self, params: &DelayParams) -> f32 {
        if params.sync > 0.5 {
            let div: NoteDivision = index_to_division(params.division as u8);
            let ms = self.tempo.division_to_ms(div);
            (ms / 1000.0 * self.sample_rate).clamp(1.0, self.max_delay_samples - 1.0)
        } else {
            let samples = (params.time_ms / 1000.0) * self.sample_rate;
            samples.clamp(1.0, self.max_delay_samples - 1.0)
        }
    }

    /// Process the feedback signal through LP → HP → (optional) diffusion — left channel.
    #[inline]
    fn filter_feedback_l(&mut self, signal: f32) -> f32 {
        let lp = self.feedback_lp_l.process(signal);
        let hp = self.feedback_hp_l.process(lp);
        if self.cached_diffusion > 0.0 {
            let d1 = self.diffusion_ap1_l.process(hp);
            self.diffusion_ap2_l.process(d1)
        } else {
            hp
        }
    }

    /// Process the feedback signal through LP → HP → (optional) diffusion — right channel.
    #[inline]
    fn filter_feedback_r(&mut self, signal: f32) -> f32 {
        let lp = self.feedback_lp_r.process(signal);
        let hp = self.feedback_hp_r.process(lp);
        if self.cached_diffusion > 0.0 {
            let d1 = self.diffusion_ap1_r.process(hp);
            self.diffusion_ap2_r.process(d1)
        } else {
            hp
        }
    }
}

impl DspKernel for DelayKernel {
    type Params = DelayParams;

    /// Process one stereo sample pair through the delay effect.
    ///
    /// ## Per-sample steps
    ///
    /// 1. Update LP/HP/diffusion coefficients if parameters have changed.
    /// 2. Resolve effective delay time (manual or tempo-synced).
    /// 3. Read the delayed signal from both delay lines.
    /// 4. Build the feedback signal: `delayed × feedback → LP → HP → diffusion`.
    ///    In ping-pong mode, feedback crosses channels (L delay reads from R's
    ///    delayed signal and vice versa).
    /// 5. Write `input + filtered_feedback` into each delay line (denormal-flushed).
    /// 6. Apply `feedback_wet_compensation()` to the wet signal, then wet/dry mix.
    /// 7. Apply output gain.
    #[inline]
    fn process_stereo(&mut self, left: f32, right: f32, params: &DelayParams) -> (f32, f32) {
        // ── Coefficient update (cached — near zero cost when stable) ──
        self.update_coefficients(params);

        // ── Unit conversion ──
        let feedback = params.feedback_pct / 100.0; // 0–0.95
        let mix = params.mix_pct / 100.0; // 0–1
        let output_gain = fast_db_to_linear(params.output_db);
        let delay_samples = self.effective_delay_samples(params);

        // Track ping-pong for is_true_stereo(), and delay/feedback for tail_samples()
        self.cached_ping_pong = params.ping_pong > 0.5;
        self.cached_delay_samples = delay_samples;
        self.cached_feedback = feedback;

        // ── Read delayed signals ──
        let delayed_l = self.delay_line_l.read(delay_samples);
        let delayed_r = self.delay_line_r.read(delay_samples);

        // ── Feedback path ──
        if self.cached_ping_pong {
            // Ping-pong: L delay receives R's delayed signal, R receives L's.
            let filtered_l = self.filter_feedback_l(delayed_r * feedback);
            let filtered_r = self.filter_feedback_r(delayed_l * feedback);
            self.delay_line_l.write(flush_denormal(left + filtered_l));
            self.delay_line_r.write(flush_denormal(right + filtered_r));
        } else {
            // Standard: each channel feeds back into itself.
            let filtered_l = self.filter_feedback_l(delayed_l * feedback);
            let filtered_r = self.filter_feedback_r(delayed_r * feedback);
            self.delay_line_l.write(flush_denormal(left + filtered_l));
            self.delay_line_r.write(flush_denormal(right + filtered_r));
        }

        // ── Wet/dry mix with feedback level compensation ──
        // `feedback_wet_compensation` attenuates the wet signal when feedback
        // is high, keeping perceived loudness consistent regardless of tail length.
        let comp = sonido_core::gain::feedback_wet_compensation(feedback);
        let (out_l, out_r) =
            wet_dry_mix_stereo(left, right, delayed_l * comp, delayed_r * comp, mix);

        (out_l * output_gain, out_r * output_gain)
    }

    /// Process a single mono sample through the delay.
    ///
    /// Uses the left channel delay line and feedback filter chain only.
    /// Feedback is self-referential (standard mono operation; no ping-pong).
    #[inline]
    fn process(&mut self, input: f32, params: &DelayParams) -> f32 {
        self.update_coefficients(params);

        let feedback = params.feedback_pct / 100.0;
        let mix = params.mix_pct / 100.0;
        let output_gain = fast_db_to_linear(params.output_db);
        let delay_samples = self.effective_delay_samples(params);

        let delayed = self.delay_line_l.read(delay_samples);
        let filtered = self.filter_feedback_l(delayed * feedback);
        self.delay_line_l.write(flush_denormal(input + filtered));

        let comp = sonido_core::gain::feedback_wet_compensation(feedback);
        wet_dry_mix(input, delayed * comp, mix) * output_gain
    }

    fn reset(&mut self) {
        self.delay_line_l.clear();
        self.delay_line_r.clear();
        self.feedback_lp_l.reset();
        self.feedback_lp_r.reset();
        self.feedback_hp_l.clear();
        self.feedback_hp_r.clear();
        self.diffusion_ap1_l.clear();
        self.diffusion_ap2_l.clear();
        self.diffusion_ap1_r.clear();
        self.diffusion_ap2_r.clear();
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;

        // Rebuild delay lines at the new sample rate.
        let max_samples = ceilf(MAX_DELAY_S * sample_rate) as usize;
        self.max_delay_samples = max_samples as f32;
        self.delay_line_l = InterpolatedDelay::new(max_samples);
        self.delay_line_r = InterpolatedDelay::new(max_samples);

        // Rebuild diffusion allpass buffers.
        let ap1 = (DIFFUSION_AP1_S * sample_rate) as usize;
        let ap2 = (DIFFUSION_AP2_S * sample_rate) as usize;
        self.diffusion_ap1_l = AllpassFilter::new(ap1.max(1));
        self.diffusion_ap2_l = AllpassFilter::new(ap2.max(1));
        self.diffusion_ap1_r = AllpassFilter::new(ap1.max(1));
        self.diffusion_ap2_r = AllpassFilter::new(ap2.max(1));

        // Update one-pole filter sample rates and recalculate cached LP frequency.
        self.feedback_lp_l.set_sample_rate(sample_rate);
        self.feedback_lp_r.set_sample_rate(sample_rate);
        self.feedback_lp_l.set_frequency(self.cached_lp_freq);
        self.feedback_lp_r.set_frequency(self.cached_lp_freq);

        // Recalculate HP biquad coefficients for the new sample rate.
        let (b0, b1, b2, a0, a1, a2) =
            highpass_coefficients(self.cached_hp_freq, 0.707, sample_rate);
        self.feedback_hp_l.set_coefficients(b0, b1, b2, a0, a1, a2);
        self.feedback_hp_r.set_coefficients(b0, b1, b2, a0, a1, a2);

        self.tempo.set_sample_rate(sample_rate);
    }

    fn latency_samples(&self) -> usize {
        // The delay effect introduces no processing latency.
        // The delay TIME is a musical parameter, not system latency.
        0
    }

    /// Returns `true` when ping-pong mode is active.
    ///
    /// In ping-pong mode the L and R feedback paths cross channels, producing
    /// genuinely decorrelated stereo — left repeats appear on right and vice
    /// versa. In standard mode the two channels are processed independently
    /// (dual-mono), so `false` is returned.
    fn is_true_stereo(&self) -> bool {
        self.cached_ping_pong
    }

    /// Receive tempo context and store the new BPM.
    ///
    /// The [`TempoManager`] BPM is updated here. Actual delay time computation
    /// for the synced case happens inside `process_stereo()` via
    /// `effective_delay_samples()`, so no per-call recomputation is needed.
    fn set_tempo_context(&mut self, ctx: &TempoContext) {
        self.tempo.set_bpm(ctx.bpm);
    }

    /// Estimated delay tail duration in samples.
    ///
    /// Computes the number of repeats until the feedback falls below −60 dB
    /// (`gain < 0.001`), then returns `delay_time × repeats`. Capped at 10
    /// seconds to prevent unbounded values at near-unity feedback.
    fn tail_samples(&self) -> usize {
        let feedback = self.cached_feedback;
        if feedback < 0.001 {
            return 0;
        }
        // Number of repeats to decay 60 dB: N = log(0.001) / log(feedback)
        let repeats = libm::ceilf(logf(0.001) / logf(feedback)) as usize;
        let tail = self.cached_delay_samples as usize * repeats;
        // Cap at 10 seconds.
        tail.min((10.0 * self.sample_rate) as usize)
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

    // ── Silence invariant ──────────────────────────────────────────────────

    /// Silence in must produce silence out at default params.
    ///
    /// Delay lines initialise to zero. With zero input the wet path is zero,
    /// and the dry path is also zero, so the mix is zero.
    #[test]
    fn silence_in_silence_out() {
        let mut kernel = DelayKernel::new(48000.0);
        let params = DelayParams::default();

        let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
        assert!(l.abs() < 1e-6, "Expected silence on left, got {l}");
        assert!(r.abs() < 1e-6, "Expected silence on right, got {r}");
    }

    // ── Finite output ──────────────────────────────────────────────────────

    /// Processing must not produce NaN or ±Infinity for 2000 samples.
    ///
    /// Drives the kernel with a 440 Hz triangle wave and verifies all outputs
    /// remain IEEE-finite. Feedback is set to 80 % to stress the feedback path.
    #[test]
    fn no_nan_or_inf() {
        let mut kernel = DelayKernel::new(48000.0);
        let params = DelayParams {
            time_ms: 50.0,
            feedback_pct: 80.0,
            mix_pct: 50.0,
            ..DelayParams::default()
        };

        let mut phase: f32 = 0.0;
        let phase_inc: f32 = 440.0 / 48000.0;

        for _ in 0..2000 {
            // Triangle wave — no_std safe, no libm needed.
            let input = if phase < 0.5 {
                4.0 * phase - 1.0
            } else {
                3.0 - 4.0 * phase
            };
            phase += phase_inc;
            if phase >= 1.0 {
                phase -= 1.0;
            }

            let (l, r) = kernel.process_stereo(input, -input, &params);
            assert!(l.is_finite(), "Left output is NaN or Inf: {l}");
            assert!(r.is_finite(), "Right output is NaN or Inf: {r}");
        }
    }

    // ── Parameter count ────────────────────────────────────────────────────

    /// `DelayParams::COUNT` must equal 10 and every descriptor index must be
    /// `Some`, while the index beyond `COUNT` returns `None`.
    #[test]
    fn params_descriptor_count() {
        assert_eq!(DelayParams::COUNT, 10, "Expected exactly 10 parameters");

        for i in 0..DelayParams::COUNT {
            assert!(
                DelayParams::descriptor(i).is_some(),
                "Missing descriptor at index {i}"
            );
        }
        assert!(
            DelayParams::descriptor(DelayParams::COUNT).is_none(),
            "Descriptor beyond COUNT should be None"
        );
    }

    // ── Adapter integration ────────────────────────────────────────────────

    /// The kernel must wrap into a `KernelAdapter` and function as a `dyn Effect`.
    #[test]
    fn adapter_wraps_as_effect() {
        let mut adapter = KernelAdapter::new(DelayKernel::new(48000.0), 48000.0);
        adapter.reset();

        let output = adapter.process(0.3);
        assert!(
            output.is_finite(),
            "Adapter output must be finite, got {output}"
        );
    }

    /// The adapter's `ParameterInfo` must expose the correct count and `ParamId`s,
    /// matching the classic `Delay` effect's parameter contract exactly.
    ///
    /// Note the non-sequential output ID: index 9 carries `ParamId(1104)` while
    /// index 3 (ping_pong) carries `ParamId(1103)`. This matches the original
    /// effect, preserving preset compatibility.
    #[test]
    fn adapter_param_info_matches() {
        let adapter = KernelAdapter::new(DelayKernel::new(48000.0), 48000.0);

        assert_eq!(
            adapter.param_count(),
            DelayParams::COUNT,
            "Adapter param count must match DelayParams::COUNT"
        );

        let p = |i: usize| {
            adapter
                .param_info(i)
                .unwrap_or_else(|| panic!("Missing param {i}"))
        };

        // Verify ParamIds — non-sequential by design (output=1104 at index 9).
        assert_eq!(p(0).id, ParamId(1100), "time_ms must be ParamId(1100)");
        assert_eq!(p(1).id, ParamId(1101), "feedback must be ParamId(1101)");
        assert_eq!(p(2).id, ParamId(1102), "mix must be ParamId(1102)");
        assert_eq!(p(3).id, ParamId(1103), "ping_pong must be ParamId(1103)");
        assert_eq!(p(4).id, ParamId(1105), "fb_lp must be ParamId(1105)");
        assert_eq!(p(5).id, ParamId(1106), "fb_hp must be ParamId(1106)");
        assert_eq!(p(6).id, ParamId(1107), "diffusion must be ParamId(1107)");
        assert_eq!(p(7).id, ParamId(1108), "sync must be ParamId(1108)");
        assert_eq!(p(8).id, ParamId(1109), "division must be ParamId(1109)");
        assert_eq!(
            p(9).id,
            ParamId(1104),
            "output must be ParamId(1104) — non-sequential by design"
        );

        // Verify string IDs
        assert_eq!(p(0).string_id, "dly_time");
        assert_eq!(p(3).string_id, "dly_ping_pong");
        assert_eq!(p(4).string_id, "dly_fb_lp");
        assert_eq!(p(9).string_id, "dly_output");

        // Verify division labels are present
        assert_eq!(
            p(8).step_labels,
            Some(DIVISION_LABELS),
            "Division must have step labels"
        );
    }

    // ── Preset morphing ────────────────────────────────────────────────────

    /// Morphing linearly between two param states must always produce finite output.
    ///
    /// Exercises stepped-param lerp (ping_pong, sync, division snap at t = 0.5)
    /// and continuous-param lerp (time_ms, feedback, mix, etc.).
    #[test]
    fn morph_produces_valid_output() {
        let mut kernel = DelayKernel::new(48000.0);

        let a = DelayParams::default();
        let b = DelayParams {
            time_ms: 800.0,
            feedback_pct: 70.0,
            mix_pct: 80.0,
            ping_pong: 1.0,
            fb_lp_hz: 4000.0,
            fb_hp_hz: 200.0,
            diffusion_pct: 60.0,
            sync: 0.0,
            division: 5.0,
            output_db: -6.0,
        };

        for i in 0..=10 {
            let t = i as f32 / 10.0;
            let morphed = DelayParams::lerp(&a, &b, t);
            let (l, r) = kernel.process_stereo(0.3, -0.3, &morphed);
            assert!(
                l.is_finite() && r.is_finite(),
                "Morph at t={t} produced NaN/Inf: l={l}, r={r}"
            );
            kernel.reset();
        }
    }

    // ── from_knobs range coverage ──────────────────────────────────────────

    /// `from_knobs()` must map normalized 0.0–1.0 inputs to the correct
    /// user-facing parameter ranges at both extremes and the mid-point.
    #[test]
    fn from_knobs_maps_ranges() {
        // Maximum: all knobs at 1.0
        let max = DelayParams::from_knobs(1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0);
        assert!(
            (max.feedback_pct - 95.0).abs() < 0.1,
            "Feedback at 1.0 should be 95 %, got {}",
            max.feedback_pct
        );
        assert!(
            (max.mix_pct - 100.0).abs() < 0.1,
            "Mix at 1.0 should be 100 %, got {}",
            max.mix_pct
        );
        assert!(
            (max.output_db - 6.0).abs() < 0.1,
            "Output at 1.0 should be +6 dB, got {}",
            max.output_db
        );
        assert!(
            (max.ping_pong - 1.0).abs() < 0.01,
            "Ping-pong at 1.0 should be 1.0, got {}",
            max.ping_pong
        );

        // Minimum: all knobs at 0.0
        let min = DelayParams::from_knobs(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert!(
            min.feedback_pct.abs() < 0.01,
            "Feedback at 0.0 should be 0 %, got {}",
            min.feedback_pct
        );
        assert!(
            min.mix_pct.abs() < 0.01,
            "Mix at 0.0 should be 0 %, got {}",
            min.mix_pct
        );
        assert!(
            (min.output_db - (-20.0)).abs() < 0.01,
            "Output at 0.0 should be −20 dB, got {}",
            min.output_db
        );
        assert!(
            min.ping_pong.abs() < 0.01,
            "Ping-pong at 0.0 should be 0.0, got {}",
            min.ping_pong
        );

        // Mid-point: non-stepped params at 0.5
        let mid = DelayParams::from_knobs(0.5, 0.5, 0.5, 0.0, 0.5, 0.5, 0.5, 0.0, 0.5, 0.5);
        assert!(
            (mid.feedback_pct - 47.5).abs() < 0.1,
            "Feedback at 0.5 should be 47.5 %, got {}",
            mid.feedback_pct
        );
        assert!(
            (mid.mix_pct - 50.0).abs() < 0.1,
            "Mix at 0.5 should be 50 %, got {}",
            mid.mix_pct
        );
        assert!(
            (mid.output_db - (-7.0)).abs() < 0.1,
            "Output at 0.5 should be -7 dB, got {}",
            mid.output_db
        );
    }

    // ── Delayed impulse ────────────────────────────────────────────────────

    /// A single impulse must appear in the output after the specified delay time.
    ///
    /// Sends one impulse at 100 ms delay, then reads samples until the echo
    /// appears. At 0 % feedback the amplitude is scaled by
    /// `feedback_wet_compensation(0)` ≈ 1.0, so we threshold at > 0.3.
    #[test]
    fn delayed_impulse_appears() {
        let sr = 44100.0_f32;
        let delay_ms = 100.0_f32;

        let mut kernel = DelayKernel::new(sr);
        let params = DelayParams {
            time_ms: delay_ms,
            feedback_pct: 0.0,
            mix_pct: 100.0, // fully wet
            ..DelayParams::default()
        };

        // Send impulse
        kernel.process_stereo(1.0, 1.0, &params);

        let delay_samples = (delay_ms / 1000.0 * sr) as usize;
        let search_window = delay_samples + 200;

        let mut found_l = false;
        let mut found_r = false;
        for _ in 0..search_window {
            let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
            if l.abs() > 0.3 {
                found_l = true;
            }
            if r.abs() > 0.3 {
                found_r = true;
            }
        }

        assert!(found_l, "Delayed echo must appear on left channel");
        assert!(found_r, "Delayed echo must appear on right channel");
    }

    // ── LP filter darkening ────────────────────────────────────────────────

    /// LP-filtered feedback must produce lower high-frequency energy than
    /// unfiltered feedback over a sustained tail.
    ///
    /// Feeds alternating-polarity samples (high-frequency content) into both
    /// kernels. After priming, collects the tail energy. The aggressively
    /// LP-filtered version must accumulate less total energy.
    #[test]
    fn feedback_lp_darkens() {
        let sr = 48000.0_f32;

        // ── LP-filtered delay ──
        let mut lp_kernel = DelayKernel::new(sr);
        let lp_params = DelayParams {
            time_ms: 50.0,
            feedback_pct: 85.0,
            mix_pct: 100.0,
            fb_lp_hz: 500.0, // aggressive LP cut
            ..DelayParams::default()
        };

        // Feed HF impulse burst (alternating polarity)
        for _ in 0..10 {
            lp_kernel.process_stereo(1.0, 1.0, &lp_params);
            lp_kernel.process_stereo(-1.0, -1.0, &lp_params);
        }

        let mut energy_lp = 0.0_f32;
        for _ in 0..10000 {
            let (l, _) = lp_kernel.process_stereo(0.0, 0.0, &lp_params);
            energy_lp += l * l;
        }

        // ── Unfiltered delay (LP at 20 kHz bypass) ──
        let mut clean_kernel = DelayKernel::new(sr);
        let clean_params = DelayParams {
            time_ms: 50.0,
            feedback_pct: 85.0,
            mix_pct: 100.0,
            fb_lp_hz: 20000.0, // effectively bypassed
            ..DelayParams::default()
        };

        for _ in 0..10 {
            clean_kernel.process_stereo(1.0, 1.0, &clean_params);
            clean_kernel.process_stereo(-1.0, -1.0, &clean_params);
        }

        let mut energy_clean = 0.0_f32;
        for _ in 0..10000 {
            let (l, _) = clean_kernel.process_stereo(0.0, 0.0, &clean_params);
            energy_clean += l * l;
        }

        assert!(
            energy_lp < energy_clean,
            "LP-filtered feedback should have less HF energy: \
             lp={energy_lp:.4}, clean={energy_clean:.4}"
        );
    }
}
