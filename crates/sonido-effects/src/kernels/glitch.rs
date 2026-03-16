//! Glitch kernel — buffer manipulation: stutter, tape-stop, reverse, shuffle.
//!
//! `GlitchKernel` captures input into a circular buffer and applies one of four
//! destructive playback modes. The effect fires probabilistically at `rate` Hz,
//! controlled by `probability`. Between trigger events the dry signal passes through.
//!
//! # Modes
//!
//! | Value | Name | Behavior |
//! |-------|------|----------|
//! | 0 | Stutter | Repeat a short segment (length = depth %) at rate Hz |
//! | 1 | TapeStop | On trigger: slow playback to zero over depth-controlled time |
//! | 2 | Reverse | On trigger: play buffer backwards for one segment |
//! | 3 | Shuffle | On trigger: jump to a random position in the buffer |
//!
//! # Signal Flow
//!
//! ```text
//! input → [circular buffer] → mode dispatch → wet/dry mix → output gain
//! ```
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! let adapter = Adapter::new(GlitchKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing)
//! let mut kernel = GlitchKernel::new(48000.0);
//! let params = GlitchParams::default();
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

extern crate alloc;

use alloc::vec::Vec;
use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::{
    ParamDescriptor, ParamFlags, ParamId, ParamUnit, fast_db_to_linear, wet_dry_mix,
};

// ═══════════════════════════════════════════════════════════════════════════
//  Constants
// ═══════════════════════════════════════════════════════════════════════════

/// Circular buffer length — ~1.36 s at 48 kHz (power of 2).
const BUF_LEN: usize = 65536;

/// Mode: repeat a short segment.
const MODE_STUTTER: u8 = 0;
/// Mode: slow playback to a stop.
const MODE_TAPE_STOP: u8 = 1;
/// Mode: play buffer in reverse.
const MODE_REVERSE: u8 = 2;
/// Mode: jump to random buffer position.
const MODE_SHUFFLE: u8 = 3;

const MODE_LABELS: &[&str] = &["Stutter", "TapeStop", "Reverse", "Shuffle"];

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`GlitchKernel`].
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `mode` | index | 0–3 (STEPPED) | 0.0 |
/// | 1 | `rate` | Hz | 1–32 | 4.0 |
/// | 2 | `depth` | % | 0–100 | 50.0 |
/// | 3 | `probability` | % | 0–100 | 50.0 |
/// | 4 | `mix_pct` | % | 0–100 | 100.0 |
/// | 5 | `output_db` | dB | −60–+6 | 0.0 |
#[derive(Debug, Clone, Copy)]
pub struct GlitchParams {
    /// Glitch mode (STEPPED). 0=Stutter, 1=TapeStop, 2=Reverse, 3=Shuffle.
    pub mode: f32,
    /// Trigger rate in Hz. Range: 1.0 to 32.0 Hz.
    pub rate: f32,
    /// Effect depth in percent. Range: 0.0 to 100.0 %.
    ///
    /// Controls segment length (Stutter/Reverse), deceleration speed (TapeStop).
    pub depth: f32,
    /// Trigger probability in percent. Range: 0.0 to 100.0 %.
    ///
    /// At 100 % the effect always fires on schedule. Below 100 % triggers
    /// are randomly skipped.
    pub probability: f32,
    /// Wet/dry mix in percent. Range: 0.0 to 100.0 %.
    pub mix_pct: f32,
    /// Output level in decibels. Range: −60.0 to +6.0 dB.
    pub output_db: f32,
}

impl Default for GlitchParams {
    fn default() -> Self {
        Self {
            mode: 0.0,
            rate: 4.0,
            depth: 50.0,
            probability: 50.0,
            mix_pct: 100.0,
            output_db: 0.0,
        }
    }
}

impl KernelParams for GlitchParams {
    const COUNT: usize = 6;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(
                ParamDescriptor::custom("Mode", "Mode", 0.0, 3.0, 0.0)
                    .with_unit(ParamUnit::None)
                    .with_step(1.0)
                    .with_id(ParamId(3500), "gl_mode")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(MODE_LABELS),
            ),
            1 => Some(
                ParamDescriptor::custom("Rate", "Rate", 1.0, 32.0, 4.0)
                    .with_unit(ParamUnit::Hertz)
                    .with_id(ParamId(3501), "gl_rate"),
            ),
            2 => Some(
                ParamDescriptor {
                    name: "Depth",
                    short_name: "Depth",
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(3502), "gl_depth"),
            ),
            3 => Some(
                ParamDescriptor {
                    name: "Probability",
                    short_name: "Prob",
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(3503), "gl_probability"),
            ),
            4 => Some(ParamDescriptor::mix().with_id(ParamId(3504), "gl_mix")),
            5 => Some(
                ParamDescriptor::gain_db("Output", "Out", -60.0, 6.0, 0.0)
                    .with_id(ParamId(3505), "gl_output"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::None,     // mode — stepped
            1 => SmoothingStyle::Slow,     // rate — 20 ms
            2 => SmoothingStyle::Standard, // depth — 10 ms
            3 => SmoothingStyle::Standard, // probability — 10 ms
            4 => SmoothingStyle::Standard, // mix_pct — 10 ms
            5 => SmoothingStyle::Fast,     // output_db — 5 ms
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.mode,
            1 => self.rate,
            2 => self.depth,
            3 => self.probability,
            4 => self.mix_pct,
            5 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.mode = value,
            1 => self.rate = value,
            2 => self.depth = value,
            3 => self.probability = value,
            4 => self.mix_pct = value,
            5 => self.output_db = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP buffer glitch kernel.
///
/// # Invariants
///
/// - `write_pos` is always in `[0, BUF_LEN)`.
/// - `read_pos` is always in `[0, BUF_LEN)`.
/// - `tape_speed` is non-negative.
pub struct GlitchKernel {
    /// Circular capture buffer — left channel.
    buf_l: Vec<f32>,
    /// Circular capture buffer — right channel.
    buf_r: Vec<f32>,
    /// Next write position.
    write_pos: usize,
    /// Current read position (may differ from write for glitch effects).
    read_pos: f32,
    /// Current playback direction (+1.0 or −1.0).
    playback_dir: f32,
    /// Tape-stop speed multiplier (1.0 = normal, 0.0 = stopped).
    tape_speed: f32,
    /// Whether a glitch effect is currently active (between trigger and reset).
    glitch_active: bool,
    /// Remaining samples in the current glitch segment.
    segment_remaining: f32,
    /// Segment length in samples (set at trigger time).
    segment_len: f32,
    /// Sample counter for rate-based trigger timing.
    rate_counter: f32,
    /// xorshift64 PRNG state.
    rng: u64,
    /// Audio sample rate in Hz.
    sample_rate: f32,
}

impl GlitchKernel {
    /// Create a new glitch kernel at the given sample rate.
    pub fn new(sample_rate: f32) -> Self {
        let mut buf_l = Vec::with_capacity(BUF_LEN);
        buf_l.resize(BUF_LEN, 0.0);
        let mut buf_r = Vec::with_capacity(BUF_LEN);
        buf_r.resize(BUF_LEN, 0.0);
        Self {
            buf_l,
            buf_r,
            write_pos: 0,
            read_pos: 0.0,
            playback_dir: 1.0,
            tape_speed: 1.0,
            glitch_active: false,
            segment_remaining: 0.0,
            segment_len: 0.0,
            rate_counter: 0.0,
            rng: 0xABCD_EF01_2345_6789,
            sample_rate,
        }
    }

    /// xorshift64 PRNG → value in [0.0, 1.0).
    #[inline]
    fn rand_f32(&mut self) -> f32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        let bits = (x >> 40) as u32;
        bits as f32 / (1u32 << 24) as f32
    }

    /// Decode mode from float param.
    #[inline]
    fn decode_mode(value: f32) -> u8 {
        ((value + 0.5) as u8).min(MODE_SHUFFLE)
    }

    /// Linear interpolated read from circular buffer.
    #[inline]
    fn read_lerp(buf: &[f32], pos: f32) -> f32 {
        let len = BUF_LEN as f32;
        let pos = ((pos % len) + len) % len;
        let i0 = pos as usize;
        let i1 = (i0 + 1) % BUF_LEN;
        let frac = pos - i0 as f32;
        buf[i0] * (1.0 - frac) + buf[i1] * frac
    }
}

impl DspKernel for GlitchKernel {
    type Params = GlitchParams;

    fn process_stereo(&mut self, left: f32, right: f32, params: &GlitchParams) -> (f32, f32) {
        let sr = self.sample_rate;
        let mode = Self::decode_mode(params.mode);

        // ── Write input to capture buffer ─────────────────────────────────
        self.buf_l[self.write_pos] = left;
        self.buf_r[self.write_pos] = right;
        self.write_pos = (self.write_pos + 1) % BUF_LEN;

        // ── Rate trigger ──────────────────────────────────────────────────
        let rate = params.rate.max(0.01);
        self.rate_counter += rate / sr;
        let triggered = if self.rate_counter >= 1.0 {
            self.rate_counter -= 1.0;
            // Probabilistic firing
            self.rand_f32() * 100.0 < params.probability
        } else {
            false
        };

        // Segment length in samples based on depth (10 ms to full rate period)
        let max_seg = sr / rate;
        let seg_len = (params.depth / 100.0 * max_seg).max(1.0);

        // ── On trigger: start new glitch segment ──────────────────────────
        if triggered && !self.glitch_active {
            self.glitch_active = true;
            self.segment_len = seg_len;
            self.segment_remaining = seg_len;

            match mode {
                MODE_STUTTER => {
                    // Lock read position to current write position
                    self.read_pos = self.write_pos as f32;
                    self.playback_dir = 1.0;
                    self.tape_speed = 1.0;
                }
                MODE_TAPE_STOP => {
                    self.read_pos = self.write_pos as f32;
                    self.tape_speed = 1.0;
                    self.playback_dir = 1.0;
                }
                MODE_REVERSE => {
                    self.read_pos = self.write_pos as f32;
                    self.playback_dir = -1.0;
                    self.tape_speed = 1.0;
                }
                _ => {
                    // Shuffle: jump to random position in buffer
                    self.read_pos = self.rand_f32() * BUF_LEN as f32;
                    self.playback_dir = 1.0;
                    self.tape_speed = 1.0;
                }
            }
        }

        // ── Process active glitch segment ─────────────────────────────────
        let (wet_l, wet_r) = if self.glitch_active {
            // Read from buffer
            let wet_l = Self::read_lerp(&self.buf_l, self.read_pos);
            let wet_r = Self::read_lerp(&self.buf_r, self.read_pos);

            // Advance read position based on mode
            match mode {
                MODE_STUTTER => {
                    // Advance, wrap within the frozen segment
                    self.read_pos += self.playback_dir * self.tape_speed;
                    // Wrap read within segment window
                    let seg_start = (self.write_pos as f32 - self.segment_len + BUF_LEN as f32)
                        % BUF_LEN as f32;
                    if self.read_pos >= self.write_pos as f32 + BUF_LEN as f32 {
                        self.read_pos = seg_start;
                    }
                    self.read_pos =
                        ((self.read_pos % BUF_LEN as f32) + BUF_LEN as f32) % BUF_LEN as f32;
                }
                MODE_TAPE_STOP => {
                    // Decelerate: playback speed ramps from 1.0 to 0.0 over the segment.
                    // `decel_speed` is 1.0 at start of segment, 0.0 at end.
                    let decel_speed = self.segment_remaining / self.segment_len.max(1.0);
                    self.read_pos = (self.read_pos + decel_speed + BUF_LEN as f32) % BUF_LEN as f32;
                }
                MODE_REVERSE => {
                    self.read_pos = (self.read_pos - 1.0 + BUF_LEN as f32 * 2.0) % BUF_LEN as f32;
                }
                _ => {
                    // Shuffle: normal forward playback from random start
                    self.read_pos = (self.read_pos + 1.0) % BUF_LEN as f32;
                }
            }

            self.segment_remaining -= 1.0;
            if self.segment_remaining <= 0.0 {
                self.glitch_active = false;
                self.tape_speed = 1.0;
                self.playback_dir = 1.0;
            }

            (wet_l, wet_r)
        } else {
            // No active glitch — pass through dry
            (left, right)
        };

        // ── Wet/dry mix and output gain ───────────────────────────────────
        let mix = params.mix_pct / 100.0;
        let output_gain = fast_db_to_linear(params.output_db);

        let out_l = wet_dry_mix(left, wet_l, mix) * output_gain;
        let out_r = wet_dry_mix(right, wet_r, mix) * output_gain;

        (out_l, out_r)
    }

    fn reset(&mut self) {
        for s in self.buf_l.iter_mut() {
            *s = 0.0;
        }
        for s in self.buf_r.iter_mut() {
            *s = 0.0;
        }
        self.write_pos = 0;
        self.read_pos = 0.0;
        self.playback_dir = 1.0;
        self.tape_speed = 1.0;
        self.glitch_active = false;
        self.segment_remaining = 0.0;
        self.segment_len = 0.0;
        self.rate_counter = 0.0;
        self.rng = 0xABCD_EF01_2345_6789;
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.reset();
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_core::Effect;
    use sonido_core::kernel::Adapter;

    #[test]
    fn finite_output_all_modes() {
        for mode in 0..4_u32 {
            let mut kernel = GlitchKernel::new(48000.0);
            let params = GlitchParams {
                mode: mode as f32,
                rate: 8.0,
                depth: 50.0,
                probability: 100.0,
                mix_pct: 100.0,
                output_db: 0.0,
            };
            for i in 0..2048_u32 {
                let t = i as f32 / 48000.0;
                let s = libm::sinf(2.0 * core::f32::consts::PI * 440.0 * t) * 0.5;
                let (l, r) = kernel.process_stereo(s, s, &params);
                assert!(l.is_finite(), "Mode {mode} L not finite at {i}: {l}");
                assert!(r.is_finite(), "Mode {mode} R not finite at {i}: {r}");
            }
        }
    }

    #[test]
    fn mode_switching_no_panic() {
        let mut kernel = GlitchKernel::new(48000.0);
        // Cycle through all modes rapidly
        for i in 0..512_u32 {
            let mode = (i % 4) as f32;
            let params = GlitchParams {
                mode,
                probability: 100.0,
                ..GlitchParams::default()
            };
            let t = i as f32 / 48000.0;
            let s = libm::sinf(2.0 * core::f32::consts::PI * 330.0 * t) * 0.3;
            let (l, r) = kernel.process_stereo(s, s, &params);
            assert!(l.is_finite(), "Mode switch L not finite at {i}: {l}");
            assert!(r.is_finite(), "Mode switch R not finite at {i}: {r}");
        }
    }

    #[test]
    fn params_descriptor_count() {
        assert_eq!(GlitchParams::COUNT, 6);
        for i in 0..GlitchParams::COUNT {
            assert!(
                GlitchParams::descriptor(i).is_some(),
                "Missing descriptor at {i}"
            );
        }
        assert!(GlitchParams::descriptor(GlitchParams::COUNT).is_none());
    }

    #[test]
    fn adapter_wraps_as_effect() {
        let mut adapter = Adapter::new(GlitchKernel::new(48000.0), 48000.0);
        adapter.reset();
        let out = adapter.process(0.3);
        assert!(out.is_finite(), "Adapter output must be finite, got {out}");
    }
}
