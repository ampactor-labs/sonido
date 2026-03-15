//! Looper kernel — stereo loop recorder and player with overdub.
//!
//! `LooperKernel` owns DSP state (a stereo [`LoopBuffer`] and the cached mode).
//! Parameters are received via `&LooperParams` each sample. Deployed via
//! [`KernelAdapter`](sonido_core::KernelAdapter) for desktop/plugin, or called
//! directly on embedded targets.
//!
//! # State Machine
//!
//! The looper has four modes controlled by `params.mode` (rounded to the nearest
//! integer). Transitions are detected in `process_stereo()` by comparing the
//! incoming rounded mode value against `cached_mode`:
//!
//! ```text
//! Stop ──►Record──►Play──►Overdub
//!  ▲         │      │       │
//!  └─────────┴──────┴───────┘
//! ```
//!
//! Mode constants (matches `STEP_LABELS` and the `mode` parameter):
//! - `0` = **Stop** — no recording, no playback, loop preserved in memory
//! - `1` = **Record** — write input to buffer; pass input through dry
//! - `2` = **Play** — read from buffer; mix with dry via wet/dry mix parameter
//! - `3` = **Overdub** — read + write simultaneously; existing loop decays by
//!   `feedback_pct / 100.0` each pass
//!
//! ## Mode Transition Actions
//!
//! | From | To | Action |
//! |------|----|--------|
//! | Any | Record | `clear()`, `reset_write_pos()` — fresh recording |
//! | Record | Play | `set_loop_end(write_pos)`, `reset_read_pos()` — freeze loop, start playback |
//! | Record | Stop | `set_loop_end(write_pos)` — freeze loop, no playback |
//! | Any | Overdub | Continue reading+writing; feedback applied to existing content |
//! | Overdub | Play | Stop writing, continue reading |
//! | Any | Stop | Halt read/write, loop stays in memory |
//!
//! # Signal Flow
//!
//! ```text
//! Record:   input ──► buffer.write() ──► dry output
//! Play:     buffer.read() ──► wet ──► wet_dry_mix(input, wet, mix) ──► output_gain
//! Overdub:  buffer.read_at_write_pos() × feedback + input ──► buffer.write()
//!           buffer.read() ──► wet ──► wet_dry_mix(input, wet, mix) ──► output_gain
//! Stop:     input ──► dry output (no buffer access)
//! ```
//!
//! # Future Enhancements
//!
//! - **Half-speed** (`half_speed` param): read every other sample. Not yet
//!   implemented — the parameter is reserved and documented for future use.
//! - **Reverse** (`reverse` param): read backwards through the loop. Not yet
//!   implemented — the parameter is reserved for future use.
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! let adapter = KernelAdapter::new(LooperKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing, ADCs are hardware-filtered)
//! let mut kernel = LooperKernel::new(48000.0);
//! let params = LooperParams::default();
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

extern crate alloc;

use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::{
    LoopBuffer, ParamDescriptor, ParamFlags, ParamId, ParamUnit, fast_db_to_linear, flush_denormal,
    wet_dry_mix_stereo,
};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum loop duration in samples per channel at 48 kHz × 60 seconds.
///
/// At 48 000 Hz, 60 s = 2 880 000 samples per channel. This is the allocation
/// upper bound for [`LoopBuffer`]. Embedded targets with limited RAM should
/// reduce this via a feature-gated override.
const MAX_LOOP_SAMPLES: usize = 2_880_000;

/// Mode index: no recording or playback, loop content preserved.
const MODE_STOP: u8 = 0;
/// Mode index: record input into the buffer.
const MODE_RECORD: u8 = 1;
/// Mode index: play back the recorded loop.
const MODE_PLAY: u8 = 2;
/// Mode index: play back while simultaneously recording (with feedback decay).
const MODE_OVERDUB: u8 = 3;

/// Step labels for the `mode` parameter — matches mode index constants above.
const MODE_LABELS: &[&str] = &["Stop", "Record", "Play", "Overdub"];

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`LooperKernel`].
///
/// All values are in **user-facing units** — the same units shown in GUIs and
/// stored in presets. The kernel converts internally as needed.
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `mode` | index | 0–3 (Stop/Record/Play/Overdub) | 0.0 |
/// | 1 | `feedback_pct` | % | 0–100 | 80.0 |
/// | 2 | `half_speed` | index | 0–1 | 0.0 |
/// | 3 | `reverse` | index | 0–1 | 0.0 |
/// | 4 | `mix_pct` | % | 0–100 | 100.0 |
/// | 5 | `output_db` | dB | −20–+6 | 0.0 |
#[derive(Debug, Clone, Copy)]
pub struct LooperParams {
    /// Looper mode index.
    ///
    /// Range: 0–3 (STEPPED). Values map to: 0 = Stop, 1 = Record, 2 = Play,
    /// 3 = Overdub. Transitions are detected per-sample in the kernel.
    pub mode: f32,

    /// Overdub feedback decay in percent.
    ///
    /// Range: 0.0 to 100.0 %. In Overdub mode, the existing loop content is
    /// scaled by `feedback_pct / 100.0` before the new input is added, causing
    /// the loop to fade with each pass. At 100 % the loop is preserved indefinitely.
    /// At 0 % only the newest layer survives.
    pub feedback_pct: f32,

    /// Half-speed toggle: 0.0 = Off, 1.0 = On.
    ///
    /// Reserved for future implementation. When enabled, playback will advance
    /// the read position every other sample, halving pitch and doubling duration.
    pub half_speed: f32,

    /// Reverse toggle: 0.0 = Off, 1.0 = On.
    ///
    /// Reserved for future implementation. When enabled, the read position will
    /// advance backwards through the loop.
    pub reverse: f32,

    /// Wet/dry mix in percent.
    ///
    /// Range: 0.0 to 100.0 %. 0 % = fully dry (input passthrough), 100 % = fully wet
    /// (loop only). Active in Play and Overdub modes.
    pub mix_pct: f32,

    /// Output level in decibels.
    ///
    /// Range: −20.0 to +6.0 dB. Applied to the final output after wet/dry mix.
    pub output_db: f32,
}

impl Default for LooperParams {
    /// Default: Stop mode, 80 % feedback, no half-speed/reverse, 100 % wet, 0 dB output.
    fn default() -> Self {
        Self {
            mode: 0.0,
            feedback_pct: 80.0,
            half_speed: 0.0,
            reverse: 0.0,
            mix_pct: 100.0,
            output_db: 0.0,
        }
    }
}

impl LooperParams {
    /// Build params directly from hardware knob readings (0.0–1.0 normalized).
    ///
    /// Convenience constructor for embedded targets where ADC values map
    /// linearly across each parameter's range.
    ///
    /// # Parameters
    ///
    /// - `mode`: ADC reading → 0–3 (Stop/Record/Play/Overdub, stepped)
    /// - `feedback`: ADC reading → 0–100 %
    /// - `half_speed`: ADC reading → 0 or 1 (stepped toggle)
    /// - `reverse`: ADC reading → 0 or 1 (stepped toggle)
    /// - `mix`: ADC reading → 0–100 %
    /// - `output`: ADC reading → −20–+6 dB
    #[allow(clippy::too_many_arguments)]
    pub fn from_knobs(
        mode: f32,
        feedback: f32,
        half_speed: f32,
        reverse: f32,
        mix: f32,
        output: f32,
    ) -> Self {
        Self::from_normalized(&[mode, feedback, half_speed, reverse, mix, output])
    }
}

impl KernelParams for LooperParams {
    const COUNT: usize = 6;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(
                ParamDescriptor::custom("Mode", "Mode", 0.0, 3.0, 0.0)
                    .with_unit(ParamUnit::None)
                    .with_step(1.0)
                    .with_id(ParamId(2000), "looper_mode")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(MODE_LABELS),
            ),
            1 => Some(
                ParamDescriptor {
                    name: "Feedback",
                    short_name: "Feedback",
                    unit: ParamUnit::Percent,
                    min: 0.0,
                    max: 100.0,
                    default: 80.0,
                    step: 1.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(2001), "looper_feedback"),
            ),
            2 => Some(
                ParamDescriptor::custom("Half Speed", "Half Spd", 0.0, 1.0, 0.0)
                    .with_unit(ParamUnit::None)
                    .with_step(1.0)
                    .with_id(ParamId(2002), "looper_half_speed")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(&["Off", "On"]),
            ),
            3 => Some(
                ParamDescriptor::custom("Reverse", "Reverse", 0.0, 1.0, 0.0)
                    .with_unit(ParamUnit::None)
                    .with_step(1.0)
                    .with_id(ParamId(2003), "looper_reverse")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(&["Off", "On"]),
            ),
            4 => Some(ParamDescriptor::mix().with_id(ParamId(2004), "looper_mix")),
            5 => Some(
                sonido_core::gain::output_param_descriptor()
                    .with_id(ParamId(2005), "looper_output"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::None,     // mode — stepped enum, snap immediately
            1 => SmoothingStyle::Standard, // feedback_pct — 10 ms
            2 => SmoothingStyle::None,     // half_speed — stepped toggle, snap
            3 => SmoothingStyle::None,     // reverse — stepped toggle, snap
            4 => SmoothingStyle::Standard, // mix_pct — 10 ms
            5 => SmoothingStyle::Standard, // output_db — 10 ms
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.mode,
            1 => self.feedback_pct,
            2 => self.half_speed,
            3 => self.reverse,
            4 => self.mix_pct,
            5 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.mode = value,
            1 => self.feedback_pct = value,
            2 => self.half_speed = value,
            3 => self.reverse = value,
            4 => self.mix_pct = value,
            5 => self.output_db = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP looper kernel.
///
/// Contains ONLY the mutable state required for audio processing:
///
/// - A [`LoopBuffer`] holding up to 60 s worth of stereo samples
/// - `cached_mode` — the last observed mode, used to detect transitions
///
/// No `SmoothedParam`, no atomics, no platform awareness. The kernel is
/// `Send`-safe because all contained types are `Send`.
///
/// ## Mode Transitions
///
/// Mode transitions are detected by comparing `(params.mode + 0.5) as u8`
/// against `cached_mode` each sample. This keeps the state machine cost
/// to a single integer comparison on the hot path.
///
/// ## Half-Speed and Reverse
///
/// These parameters are structurally present and documented but not yet active.
/// Their indices and `ParamId`s are frozen; implementation will be added in a
/// future release without breaking the parameter contract.
pub struct LooperKernel {
    /// Stereo loop buffer — up to 60 s at 48 kHz.
    buffer: LoopBuffer,
    /// Last observed mode (0–3). Compared each sample to detect transitions.
    cached_mode: u8,
}

impl LooperKernel {
    /// Create a new looper kernel at the given sample rate.
    ///
    /// Allocates a stereo [`LoopBuffer`] large enough for 60 s worth of
    /// samples per channel. Initial mode is Stop (0).
    ///
    /// # Parameters
    ///
    /// - `_sample_rate`: Audio sample rate in Hz. Currently unused (loop length
    ///   is fixed at the compile-time constant `MAX_LOOP_SAMPLES`), but accepted
    ///   for API consistency and future use when dynamic sample-rate scaling is added.
    pub fn new(_sample_rate: f32) -> Self {
        Self {
            buffer: LoopBuffer::new(MAX_LOOP_SAMPLES),
            cached_mode: MODE_STOP,
        }
    }

    /// Decode `params.mode` to a `u8` mode index.
    ///
    /// Uses `(value + 0.5) as u8` for nearest-integer rounding without `libm`,
    /// clamped to the valid range [0, 3].
    #[inline]
    fn decode_mode(value: f32) -> u8 {
        let rounded = (value + 0.5) as u8;
        rounded.min(MODE_OVERDUB)
    }

    /// Apply mode transition side-effects when the mode changes.
    ///
    /// Called once per sample when `new_mode != self.cached_mode`. Each
    /// transition case is documented in the module-level state machine table.
    #[inline]
    fn apply_transition(&mut self, new_mode: u8) {
        match (self.cached_mode, new_mode) {
            // → Record: clear the buffer and start writing from position 0.
            // `clear()` also resets loop_end to 0 so old loop data is discarded.
            (_, MODE_RECORD) => {
                self.buffer.clear();
                self.buffer.reset_write_pos();
            }
            // Record → Play: freeze the loop and begin playback from the start.
            (MODE_RECORD, MODE_PLAY) => {
                let end = self.buffer.write_position();
                self.buffer.set_loop_end(end);
                self.buffer.reset_read_pos();
            }
            // Record → Overdub: freeze the loop and begin simultaneous
            // read+write from the start, same as Record→Play but keeping write active.
            (MODE_RECORD, MODE_OVERDUB) => {
                let end = self.buffer.write_position();
                self.buffer.set_loop_end(end);
                self.buffer.reset_read_pos();
                self.buffer.reset_write_pos();
            }
            // Record → Stop: freeze the loop length; no playback.
            (MODE_RECORD, MODE_STOP) => {
                let end = self.buffer.write_position();
                self.buffer.set_loop_end(end);
            }
            // → Play from Overdub: stop writing, keep reading (no state change needed).
            (MODE_OVERDUB, MODE_PLAY) => {}
            // → Stop from any mode: halt — loop content preserved.
            (_, MODE_STOP) => {}
            // → Overdub from Play: continue reading+writing (state already consistent).
            (_, MODE_OVERDUB) => {}
            // Any unhandled transition: no side-effect.
            _ => {}
        }
        self.cached_mode = new_mode;
    }
}

impl DspKernel for LooperKernel {
    type Params = LooperParams;

    /// Process one stereo sample pair through the looper.
    ///
    /// ## Per-sample steps
    ///
    /// 1. Decode `params.mode` and apply any mode transition side-effects.
    /// 2. Dispatch to the active mode:
    ///    - **Stop**: pass input through unchanged (dry).
    ///    - **Record**: write input to the buffer; pass input through dry.
    ///    - **Play**: read from the buffer; blend wet/dry; apply output gain.
    ///    - **Overdub**: read at write_pos for feedback, mix with input, write
    ///      back; read via `read()` for wet output; blend wet/dry; apply output gain.
    /// 3. Apply output gain from `params.output_db`.
    #[inline]
    fn process_stereo(&mut self, left: f32, right: f32, params: &LooperParams) -> (f32, f32) {
        // ── Mode transition detection ──────────────────────────────────────
        let mode = Self::decode_mode(params.mode);
        if mode != self.cached_mode {
            self.apply_transition(mode);
        }

        // ── Unit conversion ────────────────────────────────────────────────
        let feedback = params.feedback_pct / 100.0; // 0.0 – 1.0
        let mix = params.mix_pct / 100.0; // 0.0 – 1.0
        let output_gain = fast_db_to_linear(params.output_db);

        // ── Mode dispatch ──────────────────────────────────────────────────
        let (out_l, out_r) = match mode {
            MODE_STOP => {
                // No buffer access; pass through dry.
                (left, right)
            }
            MODE_RECORD => {
                // Write input to buffer; output is dry passthrough.
                self.buffer.write(left, right);
                (left, right)
            }
            MODE_PLAY => {
                // Read from loop buffer and mix with dry.
                let (loop_l, loop_r) = self.buffer.read();
                wet_dry_mix_stereo(left, right, loop_l, loop_r, mix)
            }
            _ => {
                // Read existing loop content at write_pos for feedback blend.
                let (existing_l, existing_r) = self.buffer.read_at_write_pos();
                // Mix: new input + decayed existing loop.
                let write_l = flush_denormal(left + existing_l * feedback);
                let write_r = flush_denormal(right + existing_r * feedback);
                self.buffer.write(write_l, write_r);

                // Read the loop at the (now-advanced) read position for output.
                let (loop_l, loop_r) = self.buffer.read();
                wet_dry_mix_stereo(left, right, loop_l, loop_r, mix)
            }
        };

        (out_l * output_gain, out_r * output_gain)
    }

    fn reset(&mut self) {
        self.buffer.clear();
        self.cached_mode = MODE_STOP;
    }

    fn set_sample_rate(&mut self, _sample_rate: f32) {
        // Buffer capacity is fixed at construction. Dynamic sample-rate changes
        // clear the loop to avoid pitch/time artifacts.
        self.buffer.clear();
        self.cached_mode = MODE_STOP;
    }

    /// Looper has zero algorithmic latency.
    fn latency_samples(&self) -> usize {
        0
    }

    /// Tail is the recorded loop length — one full loop cycle of audio remains
    /// in the buffer and will play back when the looper is in Play or Overdub mode.
    ///
    /// Returns 0 if no loop has been recorded yet (`loop_length() == 0`).
    fn tail_samples(&self) -> usize {
        self.buffer.loop_length()
    }

    /// Dual-mono: the same loop is played back on both channels (no decorrelation).
    fn is_true_stereo(&self) -> bool {
        false
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

    // ── Helpers ───────────────────────────────────────────────────────────

    /// Construct params in Record mode.
    fn record_params() -> LooperParams {
        LooperParams {
            mode: MODE_RECORD as f32,
            feedback_pct: 80.0,
            mix_pct: 100.0,
            ..LooperParams::default()
        }
    }

    /// Construct params in Play mode.
    fn play_params() -> LooperParams {
        LooperParams {
            mode: MODE_PLAY as f32,
            feedback_pct: 80.0,
            mix_pct: 100.0,
            ..LooperParams::default()
        }
    }

    /// Construct params in Stop mode.
    fn stop_params() -> LooperParams {
        LooperParams {
            mode: MODE_STOP as f32,
            ..LooperParams::default()
        }
    }

    // ── record → play ─────────────────────────────────────────────────────

    /// Record 100 samples, switch to Play, verify the first 100 playback samples
    /// match the recorded input.
    #[test]
    fn test_record_then_play() {
        let mut kernel = LooperKernel::new(48000.0);

        // Record 100 samples of a simple ramp signal.
        let rec = record_params();
        for i in 0..100_u32 {
            let v = i as f32 / 100.0;
            kernel.process_stereo(v, v, &rec);
        }

        // Switch to Play — first call triggers the Record→Play transition.
        let play = play_params();
        for i in 0..100_u32 {
            let expected = i as f32 / 100.0;
            let (l, r) = kernel.process_stereo(0.0, 0.0, &play);
            assert!(
                (l - expected).abs() < 1e-5,
                "Playback L[{i}]: expected {expected}, got {l}"
            );
            assert!(
                (r - expected).abs() < 1e-5,
                "Playback R[{i}]: expected {expected}, got {r}"
            );
        }
    }

    // ── overdub ───────────────────────────────────────────────────────────

    /// Record a constant signal, then switch to Overdub with 80 % feedback.
    /// Each overdub pass adds new input on top of the decaying old content —
    /// the first playback sample must be larger than a single layer.
    #[test]
    fn test_overdub_accumulates() {
        let mut kernel = LooperKernel::new(48000.0);

        // Record 10 samples of 0.5.
        let rec = record_params();
        for _ in 0..10 {
            kernel.process_stereo(0.5, 0.5, &rec);
        }

        // Overdub mode: add 0.5 on top of existing 0.5 × 0.8 feedback.
        let overdub = LooperParams {
            mode: MODE_OVERDUB as f32,
            feedback_pct: 80.0,
            mix_pct: 100.0,
            ..LooperParams::default()
        };

        // One full pass in overdub — the first sample read back should be
        // the original 0.5 (recorded during the Record pass, not yet overdubbed).
        let (l0, _) = kernel.process_stereo(0.5, 0.5, &overdub);
        // l0 is from the read() inside overdub; first element was written as 0.5
        // then read back — must be finite and nonzero.
        assert!(
            l0.is_finite() && l0.abs() > 1e-6,
            "Overdub first sample should be finite and nonzero, got {l0}"
        );

        // After one complete overdub cycle the buffer should have accumulated:
        // new[i] = input + old[i] × feedback = 0.5 + 0.5 × 0.8 = 0.9
        // Read back on the second cycle.
        for _ in 1..10 {
            kernel.process_stereo(0.5, 0.5, &overdub);
        }
        // Now read one sample; it should be close to the accumulated value.
        let (l1, _) = kernel.process_stereo(0.5, 0.5, &overdub);
        // After multiple passes the value should exceed the original 0.5.
        assert!(
            l1 > 0.5,
            "Accumulated overdub should exceed original layer; got {l1}"
        );
    }

    // ── stop preserves loop ───────────────────────────────────────────────

    /// Record → Play → Stop → Play again — verify the loop is still audible.
    #[test]
    fn test_stop_preserves_loop() {
        let mut kernel = LooperKernel::new(48000.0);

        // Record 50 samples of 0.7.
        let rec = record_params();
        for _ in 0..50 {
            kernel.process_stereo(0.7, 0.7, &rec);
        }

        // Play for a few samples.
        let play = play_params();
        for _ in 0..5 {
            kernel.process_stereo(0.0, 0.0, &play);
        }

        // Stop.
        let stop = stop_params();
        for _ in 0..5 {
            kernel.process_stereo(0.0, 0.0, &stop);
        }

        // Play again — the loop must still be there.
        let (l, _) = kernel.process_stereo(0.0, 0.0, &play);
        assert!(
            l.abs() > 1e-5,
            "Loop should still be audible after Stop→Play, got L={l}"
        );
    }

    // ── mode transitions ──────────────────────────────────────────────────

    /// Verify all key state transitions produce no panics and sensible outputs.
    #[test]
    fn test_mode_transitions() {
        let mut kernel = LooperKernel::new(48000.0);

        // Stop → Record
        let rec = record_params();
        let (l, _) = kernel.process_stereo(0.5, 0.5, &rec);
        assert!(l.is_finite(), "Stop→Record: output must be finite");

        // Record 10 more samples
        for _ in 0..9 {
            kernel.process_stereo(0.5, 0.5, &rec);
        }

        // Record → Play
        let play = play_params();
        let (l, _) = kernel.process_stereo(0.0, 0.0, &play);
        assert!(l.is_finite(), "Record→Play: output must be finite");

        // Play → Overdub
        let overdub = LooperParams {
            mode: MODE_OVERDUB as f32,
            mix_pct: 100.0,
            ..LooperParams::default()
        };
        let (l, _) = kernel.process_stereo(0.1, 0.1, &overdub);
        assert!(l.is_finite(), "Play→Overdub: output must be finite");

        // Overdub → Play
        let (l, _) = kernel.process_stereo(0.0, 0.0, &play);
        assert!(l.is_finite(), "Overdub→Play: output must be finite");

        // Play → Stop
        let stop = stop_params();
        let (l, _) = kernel.process_stereo(0.0, 0.0, &stop);
        assert!(l.is_finite(), "Play→Stop: output must be finite");

        // Stop → Record (fresh recording clears old loop)
        let (l, _) = kernel.process_stereo(0.3, 0.3, &rec);
        assert!(
            l.is_finite(),
            "Stop→Record (second time): output must be finite"
        );

        // Record → Stop (freeze without entering play)
        let (l, _) = kernel.process_stereo(0.3, 0.3, &stop);
        assert!(l.is_finite(), "Record→Stop: output must be finite");
    }

    // ── dry/wet mix ───────────────────────────────────────────────────────

    /// With mix_pct = 0, Play mode must pass the dry input unchanged.
    /// With mix_pct = 100, Play mode must return only the loop (no dry bleed).
    #[test]
    fn test_mix_dry_wet() {
        let mut kernel = LooperKernel::new(48000.0);

        // Record 10 samples of 0.8.
        let rec = record_params();
        for _ in 0..10 {
            kernel.process_stereo(0.8, 0.8, &rec);
        }

        // Play at 0 % mix — output should equal dry input (0.5).
        let dry_play = LooperParams {
            mode: MODE_PLAY as f32,
            mix_pct: 0.0,
            ..LooperParams::default()
        };
        let (l_dry, _) = kernel.process_stereo(0.5, 0.5, &dry_play);
        assert!(
            (l_dry - 0.5).abs() < 1e-5,
            "0% mix should pass dry input: expected 0.5, got {l_dry}"
        );

        // Reset to reproduce the same loop for the wet test.
        kernel.reset();
        let rec = record_params();
        for _ in 0..10 {
            kernel.process_stereo(0.8, 0.8, &rec);
        }

        // Play at 100 % mix with zero dry input — output should be ~0.8 (the loop).
        let wet_play = LooperParams {
            mode: MODE_PLAY as f32,
            mix_pct: 100.0,
            ..LooperParams::default()
        };
        let (l_wet, _) = kernel.process_stereo(0.0, 0.0, &wet_play);
        assert!(
            (l_wet - 0.8).abs() < 1e-5,
            "100% mix should return loop content: expected 0.8, got {l_wet}"
        );
    }

    // ── silence invariant ─────────────────────────────────────────────────

    /// Stop mode with silence in must produce silence out.
    #[test]
    fn silence_in_silence_out_stop() {
        let mut kernel = LooperKernel::new(48000.0);
        let params = stop_params();
        let (l, r) = kernel.process_stereo(0.0, 0.0, &params);
        assert!(l.abs() < 1e-10, "Expected silence on L in Stop, got {l}");
        assert!(r.abs() < 1e-10, "Expected silence on R in Stop, got {r}");
    }

    // ── finite output ─────────────────────────────────────────────────────

    /// Processing 2000 samples through record, play, and overdub must never
    /// produce NaN or infinity.
    #[test]
    fn no_nan_or_inf() {
        let mut kernel = LooperKernel::new(48000.0);

        // Record 500 samples
        let rec = record_params();
        for i in 0..500_u32 {
            let v = (i as f32 / 500.0) * 2.0 - 1.0;
            let (l, r) = kernel.process_stereo(v, -v, &rec);
            assert!(l.is_finite(), "Record L not finite at {i}: {l}");
            assert!(r.is_finite(), "Record R not finite at {i}: {r}");
        }

        // Play 500 samples
        let play = play_params();
        for i in 0..500_u32 {
            let (l, r) = kernel.process_stereo(0.0, 0.0, &play);
            assert!(l.is_finite(), "Play L not finite at {i}: {l}");
            assert!(r.is_finite(), "Play R not finite at {i}: {r}");
        }

        // Overdub 500 samples with 80 % feedback
        let overdub = LooperParams {
            mode: MODE_OVERDUB as f32,
            feedback_pct: 80.0,
            mix_pct: 100.0,
            ..LooperParams::default()
        };
        for i in 0..500_u32 {
            let v = (i as f32 / 500.0) * 0.1;
            let (l, r) = kernel.process_stereo(v, -v, &overdub);
            assert!(l.is_finite(), "Overdub L not finite at {i}: {l}");
            assert!(r.is_finite(), "Overdub R not finite at {i}: {r}");
        }
    }

    // ── parameter count ───────────────────────────────────────────────────

    /// `COUNT` must be 6 and every descriptor index must return `Some`.
    #[test]
    fn params_descriptor_count() {
        assert_eq!(LooperParams::COUNT, 6, "Expected exactly 6 parameters");
        for i in 0..LooperParams::COUNT {
            assert!(
                LooperParams::descriptor(i).is_some(),
                "Missing descriptor at index {i}"
            );
        }
        assert!(
            LooperParams::descriptor(LooperParams::COUNT).is_none(),
            "Descriptor beyond COUNT must be None"
        );
    }

    // ── adapter integration ───────────────────────────────────────────────

    /// The kernel must wrap into a `KernelAdapter` and function as a `dyn Effect`.
    #[test]
    fn adapter_wraps_as_effect() {
        let mut adapter = KernelAdapter::new(LooperKernel::new(48000.0), 48000.0);
        adapter.reset();
        let out = adapter.process(0.3);
        assert!(out.is_finite(), "Adapter output must be finite, got {out}");
    }

    /// The adapter's `ParameterInfo` must expose 6 params with the correct `ParamId`s.
    #[test]
    fn adapter_param_info_matches() {
        let adapter = KernelAdapter::new(LooperKernel::new(48000.0), 48000.0);
        assert_eq!(adapter.param_count(), 6, "Expected 6 params via adapter");

        let p = |i: usize| {
            adapter
                .param_info(i)
                .unwrap_or_else(|| panic!("Missing param {i}"))
        };

        assert_eq!(p(0).id, ParamId(2000), "mode must be ParamId(2000)");
        assert_eq!(p(1).id, ParamId(2001), "feedback must be ParamId(2001)");
        assert_eq!(p(2).id, ParamId(2002), "half_speed must be ParamId(2002)");
        assert_eq!(p(3).id, ParamId(2003), "reverse must be ParamId(2003)");
        assert_eq!(p(4).id, ParamId(2004), "mix must be ParamId(2004)");
        assert_eq!(p(5).id, ParamId(2005), "output must be ParamId(2005)");

        // Mode must be STEPPED with the correct labels
        assert!(
            p(0).flags.contains(ParamFlags::STEPPED),
            "mode must be STEPPED"
        );
        assert_eq!(
            p(0).step_labels,
            Some(MODE_LABELS),
            "mode step labels must match MODE_LABELS"
        );

        // string_ids
        assert_eq!(p(0).string_id, "looper_mode");
        assert_eq!(p(1).string_id, "looper_feedback");
        assert_eq!(p(4).string_id, "looper_mix");
        assert_eq!(p(5).string_id, "looper_output");
    }
}
