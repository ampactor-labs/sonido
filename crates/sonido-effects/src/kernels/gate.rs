//! Gate kernel — noise gate with hysteresis, hold time, and sidechain HPF.
//!
//! `GateKernel` owns DSP state (envelope follower, sidechain biquad, gate
//! state machine, exponential coefficients, linear caches). Parameters are
//! received via `&GateParams` each sample. Deployed via
//! [`KernelAdapter`](sonido_core::KernelAdapter) for desktop/plugin, or
//! called directly on embedded targets.
//!
//! # Signal Flow
//!
//! ```text
//! Input ──┬──────────────────────────────────────────── × gain ── × output
//!         │                                                  ▲
//!         └─► SC HPF (Biquad) ─► Envelope ─► GateStateMachine
//! ```
//!
//! Linked-stereo processing: the sidechain uses the average of `|left|` and
//! `|right|`, so both channels are gated by the same gain factor.
//!
//! # Gate State Machine
//!
//! ```text
//!   Closed ──(above open)──► Opening ──(gain ≥ 0.999)──► Open
//!     ▲                          │                          │
//!     │                 (below close)              (below close)
//!     │                          ▼                          ▼
//! (gain ≤ floor+0.001)       Closing ◄─── Holding ◄─── (hold_counter)
//!                                │
//!                        (above open)
//!                                └──► Opening
//! ```
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (adapter handles smoothing)
//! let adapter = KernelAdapter::new(GateKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing)
//! let mut kernel = GateKernel::new(48000.0);
//! let params = GateParams::from_knobs(thresh, atk, rls, hold, range, hyst, sc, out);
//! let (l, r) = kernel.process_stereo(input_l, input_r, &params);
//! ```
//!
//! # References
//!
//! - Giannoulis et al., "Digital Dynamic Range Compressor Design" (2012) —
//!   exponential ballistics and hysteresis design.
//! - Zolzer, "DAFX" (2011), Ch. 4 — gate design with hold time.

use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::{
    Biquad, Cached, EnvelopeFollower, ParamDescriptor, ParamFlags, ParamId, ParamScale, ParamUnit,
    fast_db_to_linear, highpass_coefficients, math::db_to_linear,
};

// ═══════════════════════════════════════════════════════════════════════════
//  Gate state machine
// ═══════════════════════════════════════════════════════════════════════════

/// Internal state of the noise gate envelope.
///
/// Implements hysteresis (separate open/close thresholds) and a hold phase
/// to prevent chatter when the signal hovers near the threshold boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateState {
    /// Gate is fully closed — signal is attenuated to the range floor.
    Closed,
    /// Gate is opening — gain ramps toward 1.0 with exponential attack.
    Opening,
    /// Gate is fully open — signal passes at unity gain.
    Open,
    /// Gate is in hold phase — stays open before starting to close.
    Holding,
    /// Gate is closing — gain ramps toward the floor with exponential release.
    Closing,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`GateKernel`].
///
/// All values are in **user-facing units** — the same units shown in GUIs and
/// stored in presets. The kernel converts internally as needed (dB → linear,
/// ms → samples, etc.).
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `threshold_db` | dB | −80–0 | −40.0 |
/// | 1 | `attack_ms` | ms | 0.1–50 | 1.0 |
/// | 2 | `release_ms` | ms | 10–1000 | 100.0 |
/// | 3 | `hold_ms` | ms | 0–500 | 50.0 |
/// | 4 | `range_db` | dB | −80–0 | −80.0 |
/// | 5 | `hysteresis_db` | dB | 0–12 | 3.0 |
/// | 6 | `sidechain_freq_hz` | Hz | 20–500 | 80.0 |
/// | 7 | `output_db` | dB | −20–+6 | 0.0 |
///
/// # Notes on ParamId assignment
///
/// The classic `Gate` effect uses non-sequential ParamIds to preserve
/// backwards compatibility with automation data. These must be replicated
/// exactly in this kernel:
///
/// - Index 4 (Range) → `ParamId(405)` — **not** 404
/// - Index 7 (Output) → `ParamId(404)` — **not** 408
#[derive(Debug, Clone, Copy)]
pub struct GateParams {
    /// Gate open threshold in decibels.
    ///
    /// Range: −80.0–0.0 dB (default −40.0). The gate opens when the sidechain
    /// signal exceeds this level.
    pub threshold_db: f32,

    /// Attack time in milliseconds.
    ///
    /// Range: 0.1–50.0 ms (default 1.0). Controls how quickly the gate opens
    /// using an exponential (one-pole) curve.
    pub attack_ms: f32,

    /// Release time in milliseconds.
    ///
    /// Range: 10.0–1000.0 ms (default 100.0). Controls how quickly the gate
    /// closes using an exponential (one-pole) curve.
    pub release_ms: f32,

    /// Hold time in milliseconds.
    ///
    /// Range: 0.0–500.0 ms (default 50.0). The gate stays open for this
    /// duration after the signal drops below the close threshold, preventing
    /// chatter on signals that hover near the boundary.
    pub hold_ms: f32,

    /// Range (floor) in decibels.
    ///
    /// Range: −80.0–0.0 dB (default −80.0). Minimum gain when the gate is
    /// closed. At −80 dB the gate is effectively silent; at −20 dB the signal
    /// is attenuated but still audible (natural drum gating).
    pub range_db: f32,

    /// Hysteresis in decibels.
    ///
    /// Range: 0.0–12.0 dB (default 3.0). Gate opens at `threshold_db` and
    /// closes at `threshold_db − hysteresis_db`. Prevents rapid open/close
    /// cycling when the signal hovers near the threshold.
    pub hysteresis_db: f32,

    /// Sidechain highpass filter cutoff frequency in Hz.
    ///
    /// Range: 20.0–500.0 Hz (default 80.0). Filters the detection path to
    /// prevent low-frequency content (rumble, proximity effect) from keeping
    /// the gate open.
    pub sidechain_freq_hz: f32,

    /// Output level in decibels.
    ///
    /// Range: −20.0–+6.0 dB (default 0.0). Applied after the gate gain.
    pub output_db: f32,

    /// READ_ONLY diagnostic: 1.0 when gate is open/opening, 0.0 when closed.
    ///
    /// Written by the kernel each sample; not user-settable. Exposed at
    /// `KernelParams` index 8 with `ParamFlags::READ_ONLY | ParamFlags::HIDDEN`.
    /// Actual value is provided by `GateKernel::gate_open_diagnostic` field.
    pub gate_open: f32,
}

impl Default for GateParams {
    fn default() -> Self {
        Self {
            threshold_db: -40.0,
            attack_ms: 1.0,
            release_ms: 100.0,
            hold_ms: 50.0,
            range_db: -80.0,
            hysteresis_db: 3.0,
            sidechain_freq_hz: 80.0,
            output_db: 0.0,
            gate_open: 0.0,
        }
    }
}

impl GateParams {
    /// Creates parameters from normalized 0–1 knob readings.
    ///
    /// Curves (logarithmic for frequency/time, linear for percentage) are
    /// derived from [`ParamDescriptor`] — same mapping as GUI and plugin hosts.
    ///
    /// | Argument | Index | Parameter | Range |
    /// |----------|-------|-----------|-------|
    /// | `thresh` | 0 | `threshold_db` | −80–0 dB |
    /// | `atk` | 1 | `attack_ms` | 0.1–50 ms |
    /// | `rls` | 2 | `release_ms` | 10–1000 ms |
    /// | `hold` | 3 | `hold_ms` | 0–500 ms |
    /// | `range` | 4 | `range_db` | −80–0 dB |
    /// | `hyst` | 5 | `hysteresis_db` | 0–12 dB |
    /// | `sc_hpf` | 6 | `sidechain_freq_hz` | 20–500 Hz (log) |
    /// | `out` | 7 | `output_db` | −20–+6 dB |
    #[allow(clippy::too_many_arguments)]
    pub fn from_knobs(
        thresh: f32,
        atk: f32,
        rls: f32,
        hold: f32,
        range: f32,
        hyst: f32,
        sc_hpf: f32,
        out: f32,
    ) -> Self {
        Self::from_normalized(&[thresh, atk, rls, hold, range, hyst, sc_hpf, out])
    }
}

impl KernelParams for GateParams {
    const COUNT: usize = 9;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            // ── [0] Threshold ───────────────────────────────────────────────
            // ParamId(400), "gate_thresh" — matches classic gate.rs [0]
            0 => Some(
                ParamDescriptor {
                    name: "Threshold",
                    short_name: "Thresh",
                    unit: ParamUnit::Decibels,
                    min: -80.0,
                    max: 0.0,
                    default: -40.0,
                    step: 1.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(400), "gate_thresh"),
            ),
            // ── [1] Attack ──────────────────────────────────────────────────
            // ParamId(401), "gate_attack" — matches classic gate.rs [1]
            1 => Some(
                ParamDescriptor {
                    name: "Attack",
                    short_name: "Atk",
                    unit: ParamUnit::Milliseconds,
                    min: 0.1,
                    max: 50.0,
                    default: 1.0,
                    step: 0.1,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(401), "gate_attack"),
            ),
            // ── [2] Release ─────────────────────────────────────────────────
            // ParamId(402), "gate_release" — matches classic gate.rs [2]
            2 => Some(
                ParamDescriptor::time_ms("Release", "Rel", 10.0, 1000.0, 100.0)
                    .with_id(ParamId(402), "gate_release"),
            ),
            // ── [3] Hold ────────────────────────────────────────────────────
            // ParamId(403), "gate_hold" — matches classic gate.rs [3]
            3 => Some(
                ParamDescriptor::time_ms("Hold", "Hold", 0.0, 500.0, 50.0)
                    .with_id(ParamId(403), "gate_hold"),
            ),
            // ── [4] Range (floor) ───────────────────────────────────────────
            // ParamId(405), "gate_range" — NOTE: 405 not 404; matches classic gate.rs [4]
            4 => Some(
                ParamDescriptor {
                    name: "Range",
                    short_name: "Range",
                    unit: ParamUnit::Decibels,
                    min: -80.0,
                    max: 0.0,
                    default: -80.0,
                    step: 1.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(405), "gate_range"),
            ),
            // ── [5] Hysteresis ──────────────────────────────────────────────
            // ParamId(406), "gate_hysteresis" — matches classic gate.rs [5]
            5 => Some(
                ParamDescriptor {
                    name: "Hysteresis",
                    short_name: "Hyst",
                    unit: ParamUnit::Decibels,
                    min: 0.0,
                    max: 12.0,
                    default: 3.0,
                    step: 0.1,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(406), "gate_hysteresis"),
            ),
            // ── [6] SC HPF Freq ─────────────────────────────────────────────
            // ParamId(407), "gate_sc_hpf", Logarithmic — matches classic gate.rs [6]
            6 => Some(
                ParamDescriptor {
                    name: "SC HPF Freq",
                    short_name: "SC HPF",
                    unit: ParamUnit::Hertz,
                    min: 20.0,
                    max: 500.0,
                    default: 80.0,
                    step: 1.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(407), "gate_sc_hpf")
                .with_scale(ParamScale::Logarithmic),
            ),
            // ── [7] Output ──────────────────────────────────────────────────
            // ParamId(404), "gate_output" — NOTE: 404 not 408; matches classic gate.rs [7]
            7 => Some(
                sonido_core::gain::output_param_descriptor().with_id(ParamId(404), "gate_output"),
            ),
            // ── [8] Gate Open (READ_ONLY diagnostic) ────────────────────────
            // ParamId(408), "gate_open" — 1.0 = gate open, 0.0 = gate closed.
            // Written by GateKernel each sample; not user-settable.
            8 => Some(
                ParamDescriptor::custom("Gate Open", "Open", 0.0, 1.0, 0.0)
                    .with_id(ParamId(408), "gate_open")
                    .with_flags(ParamFlags::READ_ONLY.union(ParamFlags::HIDDEN)),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Standard, // threshold — avoid zipper noise on automation
            1 => SmoothingStyle::Standard, // attack — timing param
            2 => SmoothingStyle::Standard, // release — timing param
            3 => SmoothingStyle::Standard, // hold — timing param
            4 => SmoothingStyle::Standard, // range — gain floor
            5 => SmoothingStyle::Standard, // hysteresis — threshold offset
            6 => SmoothingStyle::Slow,     // sidechain HPF — filter coefficient, avoid zipper
            7 => SmoothingStyle::Standard, // output level
            8 => SmoothingStyle::None,     // gate_open — READ_ONLY diagnostic, no smoothing
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.threshold_db,
            1 => self.attack_ms,
            2 => self.release_ms,
            3 => self.hold_ms,
            4 => self.range_db,
            5 => self.hysteresis_db,
            6 => self.sidechain_freq_hz,
            7 => self.output_db,
            // Index 8 (gate_open): actual value stored on GateKernel, not GateParams.
            // Returns 0.0 placeholder here; consumers read gate_open_diagnostic directly.
            8 => self.gate_open,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.threshold_db = value,
            1 => self.attack_ms = value,
            2 => self.release_ms = value,
            3 => self.hold_ms = value,
            4 => self.range_db = value,
            5 => self.hysteresis_db = value,
            6 => self.sidechain_freq_hz = value,
            7 => self.output_db = value,
            8 => self.gate_open = value, // READ_ONLY: kernel writes this, not user
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP noise gate kernel.
///
/// Contains ONLY mutable state required for audio processing. No `SmoothedParam`,
/// no atomics, no platform awareness.
///
/// # DSP State
///
/// - `envelope_follower` — fast peak detector (0.1 ms attack / 20 ms release)
///   on the sidechain path.
/// - `sidechain_hpf` — biquad HPF that rejects low-frequency content from
///   the detection path before the envelope follower.
/// - `state` / `gain` / `hold_counter` — gate state machine.
/// - `attack_coeff` / `release_coeff` — exponential one-pole coefficients
///   cached from `params.attack_ms` / `params.release_ms`.
/// - `cached_threshold_linear` / `cached_floor_linear` — linearized dB values
///   recomputed only when the corresponding params change.
///
/// # Gate Algorithm
///
/// Exponential (one-pole) attack and release curves:
///
/// - Attack: `gain = 1.0 + coeff × (gain − 1.0)` → approaches 1.0
/// - Release: `gain = floor + coeff × (gain − floor)` → approaches floor
///
/// Coefficient: `coeff = exp(−1 / (time_ms × sample_rate / 1000))`
///
/// Reference: Giannoulis et al., "Digital Dynamic Range Compressor Design" (2012).
pub struct GateKernel {
    /// Sample rate — needed for coefficient recalculation.
    sample_rate: f32,

    /// Fast peak envelope follower on the sidechain path.
    ///
    /// Fixed timing: 0.1 ms attack, 20 ms release. The gate's own
    /// attack/release parameters control the gain ramp, not this follower.
    envelope_follower: EnvelopeFollower,

    /// Sidechain highpass biquad filter.
    ///
    /// Rejects low-frequency content from the detection path.
    /// Coefficients are recomputed when `sidechain_freq_hz` changes.
    sidechain_hpf: Biquad,

    /// Current gate state (Closed, Opening, Open, Holding, Closing).
    state: GateState,

    /// Current gate gain, in the range `[floor, 1.0]`.
    gain: f32,

    /// Hold counter in samples. Decrements while in `Holding` state.
    hold_counter: u32,

    /// Exponential attack coefficient: `exp(−1 / (attack_ms × sr / 1000))`.
    attack_coeff: f32,

    /// Exponential release coefficient: `exp(−1 / (release_ms × sr / 1000))`.
    release_coeff: f32,

    /// Cached `fast_db_to_linear(threshold_db)`. Recomputed when threshold changes.
    cached_threshold_linear: f32,

    /// Cached `threshold_db` value (for close-threshold computation).
    cached_threshold_db: f32,

    /// Cached `db_to_linear(range_db)`. Recomputed when range changes.
    cached_floor_linear: f32,

    /// Cached hysteresis_db for close-threshold computation.
    cached_hysteresis_db: f32,

    /// Change-detector for threshold_db → threshold_linear.
    threshold_cache: Cached<f32>,

    /// Change-detector for range_db → floor_linear.
    range_cache: Cached<f32>,

    /// Change-detector for hysteresis_db.
    hysteresis_cache: Cached<f32>,

    /// Change-detector for attack_ms → attack_coeff.
    attack_cache: Cached<f32>,

    /// Change-detector for release_ms → release_coeff.
    release_cache: Cached<f32>,

    /// Change-detector for sidechain HPF coefficients.
    sc_hpf_cache: Cached<[f32; 6]>,

    /// Diagnostic: 1.0 when gate is open/opening, 0.0 when closed/closing.
    ///
    /// Written each sample in `process_stereo`. Exposed at `GateParams` index 8
    /// with `ParamFlags::READ_ONLY | ParamFlags::HIDDEN`.
    pub gate_open_diagnostic: f32,
}

impl GateKernel {
    /// Create a new gate kernel initialized with default parameters.
    ///
    /// The gate starts in `Closed` state with gain at the range floor (−80 dB).
    /// The sidechain HPF is configured at 80 Hz (default). Attack/release
    /// coefficients are computed for the defaults (1 ms / 100 ms).
    pub fn new(sample_rate: f32) -> Self {
        let defaults = GateParams::default();

        // Initialize envelope follower — fixed fast detection timing
        let mut envelope_follower = EnvelopeFollower::new(sample_rate);
        envelope_follower.set_attack_ms(0.1);
        envelope_follower.set_release_ms(20.0);

        // Initialize sidechain HPF at default 80 Hz
        let mut sidechain_hpf = Biquad::new();
        let (b0, b1, b2, a0, a1, a2) = highpass_coefficients(80.0, 0.707, sample_rate);
        sidechain_hpf.set_coefficients(b0, b1, b2, a0, a1, a2);

        // Compute initial attack/release coefficients
        let attack_coeff = Self::compute_coeff(defaults.attack_ms, sample_rate);
        let release_coeff = Self::compute_coeff(defaults.release_ms, sample_rate);

        // Compute initial threshold and floor caches
        let cached_threshold_linear = fast_db_to_linear(defaults.threshold_db);
        let cached_floor_linear = db_to_linear(defaults.range_db);

        let sc_hpf_coeffs = {
            let (b0, b1, b2, a0, a1, a2) = highpass_coefficients(80.0, 0.707, sample_rate);
            [b0, b1, b2, a0, a1, a2]
        };
        let mut threshold_cache = Cached::new(cached_threshold_linear, 1);
        threshold_cache.update(&[defaults.threshold_db], 1e-3, |inputs| {
            fast_db_to_linear(inputs[0])
        });
        let mut range_cache = Cached::new(cached_floor_linear, 1);
        range_cache.update(&[defaults.range_db], 1e-3, |inputs| db_to_linear(inputs[0]));
        let mut hysteresis_cache = Cached::new(defaults.hysteresis_db, 1);
        hysteresis_cache.update(&[defaults.hysteresis_db], 1e-3, |inputs| inputs[0]);
        let mut attack_cache = Cached::new(attack_coeff, 1);
        attack_cache.update(&[defaults.attack_ms], 1e-3, |inputs| {
            Self::compute_coeff(inputs[0], sample_rate)
        });
        let mut release_cache = Cached::new(release_coeff, 1);
        release_cache.update(&[defaults.release_ms], 1e-2, |inputs| {
            Self::compute_coeff(inputs[0], sample_rate)
        });
        let mut sc_hpf_cache = Cached::new(sc_hpf_coeffs, 1);
        sc_hpf_cache.update(&[defaults.sidechain_freq_hz], 0.5, |inputs| {
            let freq = inputs[0].clamp(20.0, 500.0);
            let (b0, b1, b2, a0, a1, a2) = highpass_coefficients(freq, 0.707, sample_rate);
            [b0, b1, b2, a0, a1, a2]
        });

        Self {
            sample_rate,
            envelope_follower,
            sidechain_hpf,
            state: GateState::Closed,
            gain: cached_floor_linear,
            hold_counter: 0,
            attack_coeff,
            release_coeff,
            cached_threshold_linear,
            cached_threshold_db: defaults.threshold_db,
            cached_floor_linear,
            cached_hysteresis_db: defaults.hysteresis_db,
            threshold_cache,
            range_cache,
            hysteresis_cache,
            attack_cache,
            release_cache,
            sc_hpf_cache,
            gate_open_diagnostic: 0.0,
        }
    }

    /// Compute exponential one-pole smoothing coefficient for a given time constant.
    ///
    /// Formula: `exp(−1 / (time_ms × sample_rate / 1000))`
    ///
    /// Returns 0.0 for zero/negative time (instant response).
    /// Returns a value in `[0, 1)` for positive time (larger = slower).
    ///
    /// Reference: Giannoulis et al., "Digital Dynamic Range Compressor Design" (2012).
    #[inline]
    fn compute_coeff(time_ms: f32, sample_rate: f32) -> f32 {
        let samples = time_ms / 1000.0 * sample_rate;
        if samples > 0.0 {
            libm::expf(-1.0 / samples)
        } else {
            0.0
        }
    }

    /// Update all cached/derived values when params have changed.
    ///
    /// Uses `Cached<T>` instances to detect changes and recompute only what changed.
    #[inline]
    fn update_caches(&mut self, params: &GateParams) {
        self.cached_threshold_linear =
            *self
                .threshold_cache
                .update(&[params.threshold_db], 1e-3, |inputs| {
                    fast_db_to_linear(inputs[0])
                });
        self.cached_threshold_db = params.threshold_db;
        self.cached_floor_linear = *self
            .range_cache
            .update(&[params.range_db], 1e-3, |inputs| db_to_linear(inputs[0]));
        self.cached_hysteresis_db =
            *self
                .hysteresis_cache
                .update(&[params.hysteresis_db], 1e-3, |inputs| inputs[0]);
        self.attack_coeff = *self
            .attack_cache
            .update(&[params.attack_ms], 1e-3, |inputs| {
                Self::compute_coeff(inputs[0], self.sample_rate)
            });
        self.release_coeff = *self
            .release_cache
            .update(&[params.release_ms], 1e-2, |inputs| {
                Self::compute_coeff(inputs[0], self.sample_rate)
            });
        let sc_coeffs = *self
            .sc_hpf_cache
            .update(&[params.sidechain_freq_hz], 0.5, |inputs| {
                let freq = inputs[0].clamp(20.0, 500.0);
                let (b0, b1, b2, a0, a1, a2) = highpass_coefficients(freq, 0.707, self.sample_rate);
                [b0, b1, b2, a0, a1, a2]
            });
        self.sidechain_hpf.set_coefficients(
            sc_coeffs[0],
            sc_coeffs[1],
            sc_coeffs[2],
            sc_coeffs[3],
            sc_coeffs[4],
            sc_coeffs[5],
        );
    }

    /// Advance the gate state machine for one sample.
    ///
    /// Updates `self.state`, `self.gain`, and `self.hold_counter` from the
    /// sidechain `envelope` level and the cached threshold/hysteresis/floor.
    ///
    /// # Hysteresis
    ///
    /// The gate opens when `envelope > threshold`. It closes when
    /// `envelope ≤ threshold − hysteresis_db`. This two-threshold design
    /// prevents rapid cycling when the signal hovers near the threshold.
    ///
    /// # Attack and release curves
    ///
    /// Both use exponential (one-pole) curves:
    /// - Attack:  `gain = 1.0 + attack_coeff × (gain − 1.0)`
    /// - Release: `gain = floor + release_coeff × (gain − floor)`
    ///
    /// # Parameters
    ///
    /// - `envelope`: current envelope amplitude from the HPF sidechain path
    /// - `hold_samples`: hold duration in samples (from `params.hold_ms`)
    /// - `floor`: current floor gain in linear (from `params.range_db` cache)
    #[inline]
    fn advance_gate_state(&mut self, envelope: f32, hold_samples: u32, floor: f32) {
        let threshold_linear = self.cached_threshold_linear;

        // Close threshold = threshold_db - hysteresis_db (in linear via fast approx).
        // Uses cached threshold/hysteresis values updated by update_caches().
        let close_threshold =
            fast_db_to_linear(self.cached_threshold_db - self.cached_hysteresis_db);

        let above_open = envelope > threshold_linear;
        let above_close = envelope > close_threshold;

        match self.state {
            GateState::Closed => {
                // Track floor so range changes apply while gate is closed
                self.gain = floor;
                if above_open {
                    self.state = GateState::Opening;
                }
            }
            GateState::Opening => {
                // Exponential approach toward 1.0
                self.gain = 1.0 + self.attack_coeff * (self.gain - 1.0);
                if self.gain >= 0.999 {
                    self.gain = 1.0;
                    self.state = GateState::Open;
                }
                if !above_close {
                    self.state = GateState::Closing;
                }
            }
            GateState::Open => {
                if !above_close {
                    self.hold_counter = hold_samples;
                    self.state = GateState::Holding;
                }
            }
            GateState::Holding => {
                if above_close {
                    self.state = GateState::Open;
                } else if self.hold_counter > 0 {
                    self.hold_counter -= 1;
                } else {
                    self.state = GateState::Closing;
                }
            }
            GateState::Closing => {
                // Exponential approach toward floor
                self.gain = floor + self.release_coeff * (self.gain - floor);
                if self.gain <= floor + 0.001 {
                    self.gain = floor;
                    self.state = GateState::Closed;
                }
                if above_open {
                    self.state = GateState::Opening;
                }
            }
        }
    }
}

impl DspKernel for GateKernel {
    type Params = GateParams;

    /// Process a single mono sample through the noise gate.
    ///
    /// Sidechain: input → HPF → envelope follower → gate state machine.
    /// Output: `input × gate_gain × output_level`.
    fn process(&mut self, input: f32, params: &GateParams) -> f32 {
        self.update_caches(params);

        let floor = self.cached_floor_linear;

        // Sidechain path: HPF → envelope follower
        let sc_filtered = self.sidechain_hpf.process(input);
        let envelope = self.envelope_follower.process(sc_filtered);

        let hold_samples = ((params.hold_ms / 1000.0) * self.sample_rate) as u32;
        self.advance_gate_state(envelope, hold_samples, floor);

        self.gate_open_diagnostic = match self.state {
            GateState::Open | GateState::Opening | GateState::Holding => 1.0,
            GateState::Closed | GateState::Closing => 0.0,
        };

        let output_gain = fast_db_to_linear(params.output_db);
        input * self.gain * output_gain
    }

    /// Process a stereo sample pair through the noise gate.
    ///
    /// Linked-stereo sidechain: `(|left| + |right|) × 0.5` drives the gate
    /// so both channels are controlled by the same gain factor.
    fn process_stereo(&mut self, left: f32, right: f32, params: &GateParams) -> (f32, f32) {
        self.update_caches(params);

        let floor = self.cached_floor_linear;

        // Linked-stereo sidechain: average channel amplitudes, then HPF + envelope
        let sum = (libm::fabsf(left) + libm::fabsf(right)) * 0.5;
        let sc_filtered = self.sidechain_hpf.process(sum);
        let envelope = self.envelope_follower.process(sc_filtered);

        let hold_samples = ((params.hold_ms / 1000.0) * self.sample_rate) as u32;
        self.advance_gate_state(envelope, hold_samples, floor);

        self.gate_open_diagnostic = match self.state {
            GateState::Open | GateState::Opening | GateState::Holding => 1.0,
            GateState::Closed | GateState::Closing => 0.0,
        };

        let output_gain = fast_db_to_linear(params.output_db);
        (
            left * self.gain * output_gain,
            right * self.gain * output_gain,
        )
    }

    /// Process a stereo sample with an external sidechain signal.
    ///
    /// Uses the external `sc_left`/`sc_right` average as the detection signal
    /// instead of the main input. The internal sidechain HPF is bypassed when
    /// an external sidechain is connected — the external source is already
    /// pre-routed by the caller.
    fn process_stereo_with_sidechain(
        &mut self,
        left: f32,
        right: f32,
        sc_left: f32,
        sc_right: f32,
        params: &GateParams,
    ) -> (f32, f32) {
        self.update_caches(params);

        let floor = self.cached_floor_linear;

        // External sidechain: use average amplitude, bypass the internal HPF.
        let sc_mid = (libm::fabsf(sc_left) + libm::fabsf(sc_right)) * 0.5;
        let envelope = self.envelope_follower.process(sc_mid);

        let hold_samples = ((params.hold_ms / 1000.0) * self.sample_rate) as u32;
        self.advance_gate_state(envelope, hold_samples, floor);

        self.gate_open_diagnostic = match self.state {
            GateState::Open | GateState::Opening | GateState::Holding => 1.0,
            GateState::Closed | GateState::Closing => 0.0,
        };

        let output_gain = fast_db_to_linear(params.output_db);
        (
            left * self.gain * output_gain,
            right * self.gain * output_gain,
        )
    }

    fn reset(&mut self) {
        self.envelope_follower.reset();
        self.sidechain_hpf.clear();
        self.state = GateState::Closed;
        self.gain = self.cached_floor_linear;
        self.hold_counter = 0;
        // Invalidate all caches — force recomputation on next process() call.
        self.threshold_cache.invalidate();
        self.range_cache.invalidate();
        self.hysteresis_cache.invalidate();
        self.attack_cache.invalidate();
        self.release_cache.invalidate();
        self.sc_hpf_cache.invalidate();
        self.gate_open_diagnostic = 0.0;
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.envelope_follower.set_sample_rate(sample_rate);
        // Invalidate time-dependent caches so they recompute at next process()
        self.attack_cache.invalidate();
        self.release_cache.invalidate();
        self.sc_hpf_cache.invalidate();
    }

    fn update_diagnostics(&self, params: &mut GateParams) {
        params.gate_open = self.gate_open_diagnostic;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_core::kernel::KernelAdapter;
    use sonido_core::{Effect, ParameterInfo};

    // ── Kernel unit tests ──────────────────────────────────────────────────

    /// Silence input must produce silence output regardless of gate state.
    #[test]
    fn silence_in_silence_out() {
        let mut kernel = GateKernel::new(48000.0);
        let params = GateParams::default();
        for _ in 0..512 {
            let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
            assert!(l.abs() < 1e-6, "Expected silence, got left={l}");
            assert!(r.abs() < 1e-6, "Expected silence, got right={r}");
        }
    }

    /// No output sample — including during state transitions — may be NaN or ±Inf.
    #[test]
    fn no_nan_or_inf() {
        let mut kernel = GateKernel::new(48000.0);
        let params = GateParams::default();

        // Drive through multiple transition cycles: silence → loud → silence
        for (signal, n) in [(0.0f32, 1000usize), (0.5f32, 2000), (0.0f32, 5000)] {
            for i in 0..n {
                let t = i as f32 / 48000.0;
                let s = signal * libm::sinf(2.0 * core::f32::consts::PI * 440.0 * t);
                let (l, r) = kernel.process_stereo(s, s, &params);
                assert!(
                    !l.is_nan() && !l.is_infinite(),
                    "NaN/Inf at left sample {i}: {l}"
                );
                assert!(
                    !r.is_nan() && !r.is_infinite(),
                    "NaN/Inf at right sample {i}: {r}"
                );
            }
        }
    }

    /// Descriptor count must equal `GateParams::COUNT` and all descriptors must be present.
    #[test]
    fn params_descriptor_count() {
        assert_eq!(GateParams::COUNT, 9);
        for i in 0..GateParams::COUNT {
            assert!(
                GateParams::descriptor(i).is_some(),
                "Missing descriptor for index {i}"
            );
        }
        assert!(
            GateParams::descriptor(9).is_none(),
            "Index 9 should be None"
        );
    }

    /// After warming up with a loud signal the gate should be open
    /// and passing the signal at close to full amplitude.
    ///
    /// Uses a 500 Hz sine with a very low threshold (-60 dB) so the gate is
    /// definitely open after warm-up. Verifies that the gate gain reaches 1.0
    /// by checking peak output over a full cycle equals the peak input.
    #[test]
    fn gate_passes_loud_signal() {
        let sr = 44100.0_f32;
        let mut kernel = GateKernel::new(sr);
        let params = GateParams {
            threshold_db: -60.0, // opens at -60 dB (extremely sensitive)
            attack_ms: 0.1,      // near-instant attack
            release_ms: 10.0,
            hold_ms: 0.0,
            hysteresis_db: 0.0,
            output_db: 0.0,
            range_db: -80.0,
            sidechain_freq_hz: 20.0, // HPF at 20 Hz — barely filters anything
            gate_open: 0.0,
        };

        // Warm up with loud 500 Hz sine (well above -60 dB threshold)
        for i in 0..10000 {
            let t = i as f32 / sr;
            let s = 0.5 * libm::sinf(2.0 * core::f32::consts::PI * 500.0 * t);
            kernel.process_stereo(s, s, &params);
        }

        // Measure peak output over one complete cycle at 500 Hz (88 samples)
        // The gate should be fully open: output/input ratio ≈ 1.0
        let cycle_samples = (sr / 500.0) as usize;
        let mut max_output = 0.0_f32;
        let mut max_input = 0.0_f32;
        for i in 0..cycle_samples {
            let t = (10000 + i) as f32 / sr;
            let s = 0.5 * libm::sinf(2.0 * core::f32::consts::PI * 500.0 * t);
            let (l, _r) = kernel.process_stereo(s, s, &params);
            max_output = max_output.max(l.abs());
            max_input = max_input.max(s.abs());
        }

        // With gate fully open and output_db=0, output peak should ≈ input peak
        let ratio = max_output / max_input;
        assert!(
            ratio > 0.95,
            "Gate should pass loud signal: output/input ratio={ratio:.4} (want >0.95)"
        );
    }

    /// After warming up with a quiet signal below threshold the gate should
    /// be closed and attenuating the signal to near the floor level.
    #[test]
    fn gate_attenuates_quiet_signal() {
        let mut kernel = GateKernel::new(44100.0);
        let params = GateParams {
            threshold_db: -20.0, // opens at -20 dB = 0.1 linear
            attack_ms: 0.1,
            release_ms: 10.0,
            hold_ms: 0.0,
            hysteresis_db: 0.0,
            range_db: -80.0, // virtually silent when closed
            output_db: 0.0,
            ..GateParams::default()
        };

        // Feed very quiet signal well below threshold (-40 dB = 0.01 linear)
        for _ in 0..3000 {
            kernel.process_stereo(0.01, 0.01, &params);
        }

        // Gate should be closed — output should be near floor
        let (l, _r) = kernel.process_stereo(0.01, 0.01, &params);
        assert!(
            l.abs() < 0.001,
            "Gate should attenuate quiet signal to near-silence: output={l:.6}"
        );
    }

    // ── Adapter integration tests ──────────────────────────────────────────

    /// Wrapping in KernelAdapter must produce a functioning Effect.
    #[test]
    fn adapter_wraps_as_effect() {
        let kernel = GateKernel::new(48000.0);
        let mut adapter = KernelAdapter::new(kernel, 48000.0);

        adapter.reset();
        let output = adapter.process(0.0);
        assert!(!output.is_nan(), "Output is NaN");
        assert!(output.is_finite(), "Output is infinite");

        // Process more samples to verify stability
        for _ in 0..1000 {
            let out = adapter.process(0.1);
            assert!(out.is_finite(), "Output became non-finite");
        }
    }

    /// The adapter's ParameterInfo must expose the same descriptors as the
    /// classic Gate effect — including the non-sequential ParamId values.
    #[test]
    fn adapter_param_info_matches() {
        let kernel = GateKernel::new(48000.0);
        let adapter = KernelAdapter::new(kernel, 48000.0);

        // Count (9 = 8 user params + 1 READ_ONLY diagnostic)
        assert_eq!(adapter.param_count(), 9);

        // All present, none past end
        for i in 0..9 {
            assert!(
                adapter.param_info(i).is_some(),
                "Missing param_info for index {i}"
            );
        }
        assert!(adapter.param_info(9).is_none());

        // Names match classic gate.rs
        let expected = [
            "Threshold",
            "Attack",
            "Release",
            "Hold",
            "Range",
            "Hysteresis",
            "SC HPF Freq",
            "Output",
        ];
        for (i, &name) in expected.iter().enumerate() {
            let desc = adapter.param_info(i).unwrap();
            assert_eq!(
                desc.name, name,
                "Index {i}: expected '{name}', got '{}'",
                desc.name
            );
        }

        // ParamId values — critical for automation backwards compatibility.
        // These must match classic gate.rs impl_params! exactly.
        assert_eq!(adapter.param_info(0).unwrap().id, ParamId(400)); // Threshold
        assert_eq!(adapter.param_info(1).unwrap().id, ParamId(401)); // Attack
        assert_eq!(adapter.param_info(2).unwrap().id, ParamId(402)); // Release
        assert_eq!(adapter.param_info(3).unwrap().id, ParamId(403)); // Hold
        assert_eq!(adapter.param_info(4).unwrap().id, ParamId(405)); // Range  (NOTE: 405, not 404)
        assert_eq!(adapter.param_info(5).unwrap().id, ParamId(406)); // Hysteresis
        assert_eq!(adapter.param_info(6).unwrap().id, ParamId(407)); // SC HPF Freq
        assert_eq!(adapter.param_info(7).unwrap().id, ParamId(404)); // Output (NOTE: 404, not 408)

        // String IDs — used by CLAP host preset recall
        assert_eq!(adapter.param_info(0).unwrap().string_id, "gate_thresh");
        assert_eq!(adapter.param_info(4).unwrap().string_id, "gate_range");
        assert_eq!(adapter.param_info(7).unwrap().string_id, "gate_output");
    }

    /// Morphing between two param snapshots must always produce finite audio.
    ///
    /// Exercises all state transitions (open → close → open) at each morph
    /// point to ensure no NaN/Inf can occur at any parameter combination.
    #[test]
    fn morph_produces_valid_output() {
        let open_state = GateParams {
            threshold_db: -60.0, // very sensitive — nearly always open
            attack_ms: 0.1,
            release_ms: 10.0,
            hold_ms: 0.0,
            hysteresis_db: 0.0,
            output_db: 0.0,
            ..GateParams::default()
        };
        let closed_state = GateParams {
            threshold_db: 0.0, // never opens (requires full-scale signal)
            attack_ms: 50.0,
            release_ms: 1000.0,
            hold_ms: 500.0,
            hysteresis_db: 12.0,
            output_db: -10.0,
            ..GateParams::default()
        };

        for i in 0..=10 {
            let t = i as f32 / 10.0;
            let morphed = GateParams::lerp(&open_state, &closed_state, t);

            let mut kernel = GateKernel::new(48000.0);
            for j in 0..100 {
                let s = 0.3 * libm::sinf(j as f32 * 0.1);
                let (l, r) = kernel.process_stereo(s, -s, &morphed);
                assert!(
                    l.is_finite() && r.is_finite(),
                    "Morph at t={t:.1} produced NaN/Inf: l={l}, r={r}"
                );
            }
        }
    }
}
