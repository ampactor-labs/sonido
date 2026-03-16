//! Time-stretch kernel — independent pitch and time control via granular synthesis.
//!
//! `TimeStretchKernel` decouples time and pitch using two independently controlled
//! granular read heads. The write pointer advances at `time_ratio` speed (stretching
//! or compressing time), while the read pointer advances at `time_ratio × pitch_ratio`
//! speed (shifting pitch independently of time).
//!
//! # Signal Flow
//!
//! ```text
//! input → [write at time_ratio speed]
//!       → [grains: read at time_ratio × pitch_ratio speed]
//!         → Hann window per grain → sum → output gain
//! ```
//!
//! # Algorithm
//!
//! Based on the granular pitch-shift approach from `pitch_shift.rs`, extended with:
//! - **Time ratio**: adjusts how fast the write pointer advances (write N/time_ratio
//!   samples per output sample effectively; implemented by fractional write accumulation).
//! - **Pitch ratio**: adjusts the read-to-write speed relationship.
//! - **Freeze**: stops the write pointer; all grains read from frozen buffer content.
//! - **Randomize**: adds random scatter to grain start positions for diffuse texture.
//! - **Density**: controls grain overlap (higher = more overlapping Hann windows).
//!
//! # References
//!
//! - Zölzer, "DAFX: Digital Audio Effects" (2nd ed.), Chapter 7 — time-scale modification.
//! - Moulines & Charpentier, "Pitch-synchronous waveform processing" — OLA basis.
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! let adapter = Adapter::new(TimeStretchKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing)
//! let mut kernel = TimeStretchKernel::new(48000.0);
//! let params = TimeStretchParams::default();
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

extern crate alloc;

use alloc::vec::Vec;
use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::{ParamDescriptor, ParamFlags, ParamId, ParamUnit, fast_db_to_linear};

// ═══════════════════════════════════════════════════════════════════════════
//  Constants
// ═══════════════════════════════════════════════════════════════════════════

/// Circular buffer length — ~2.7 s at 48 kHz (power of 2).
const BUF_LEN: usize = 131072;

/// Maximum simultaneous grains.
const MAX_GRAINS: usize = 8;

/// Minimum grain size in samples (10 ms at 48 kHz).
const GRAIN_MIN_SAMPLES: f32 = 480.0;

/// Maximum grain size in samples (100 ms at 48 kHz).
const GRAIN_MAX_SAMPLES: f32 = 4800.0;

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`TimeStretchKernel`].
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `time_ratio` | ratio | 0.25–4.0 | 1.0 |
/// | 1 | `pitch_ratio` | ratio | 0.25–4.0 | 1.0 |
/// | 2 | `grain_size` | ms | 10–100 | 50.0 |
/// | 3 | `density` | % | 0–100 | 50.0 |
/// | 4 | `randomize` | % | 0–100 | 20.0 |
/// | 5 | `freeze` | index | 0–1 (STEPPED) | 0.0 |
/// | 6 | `sync` | index | 0–1 (STEPPED) | 0.0 |
/// | 7 | `output_db` | dB | −60–+6 | 0.0 |
#[derive(Debug, Clone, Copy)]
pub struct TimeStretchParams {
    /// Time stretch ratio. Range: 0.25 (4× faster) to 4.0 (4× slower).
    ///
    /// Values < 1.0 compress time (faster playback), > 1.0 stretch time.
    pub time_ratio: f32,
    /// Pitch shift ratio independent of time. Range: 0.25 to 4.0.
    ///
    /// Values < 1.0 lower pitch, > 1.0 raise pitch. At 1.0 pitch is unchanged.
    pub pitch_ratio: f32,
    /// Grain size in milliseconds. Range: 10–100 ms.
    pub grain_size: f32,
    /// Grain density in percent. Range: 0–100 %. Controls grain overlap count.
    pub density: f32,
    /// Position randomization in percent. Range: 0–100 %.
    ///
    /// At 0 % grains read from the expected position. At 100 % start positions
    /// scatter randomly within ±grain_size, creating diffuse texture.
    pub randomize: f32,
    /// Freeze toggle: 0 = off, 1 = on (STEPPED).
    ///
    /// When enabled, the write pointer stops. Grains continue reading from
    /// the frozen buffer content indefinitely.
    pub freeze: f32,
    /// Sync toggle: 0 = off, 1 = on (STEPPED). Reserved for tempo sync.
    pub sync: f32,
    /// Output level in decibels. Range: −60.0 to +6.0 dB.
    pub output_db: f32,
}

impl Default for TimeStretchParams {
    fn default() -> Self {
        Self {
            time_ratio: 1.0,
            pitch_ratio: 1.0,
            grain_size: 50.0,
            density: 50.0,
            randomize: 20.0,
            freeze: 0.0,
            sync: 0.0,
            output_db: 0.0,
        }
    }
}

impl KernelParams for TimeStretchParams {
    const COUNT: usize = 8;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(
                ParamDescriptor::custom("Time Ratio", "Time", 0.25, 4.0, 1.0)
                    .with_unit(ParamUnit::None)
                    .with_id(ParamId(3600), "ts_time_ratio"),
            ),
            1 => Some(
                ParamDescriptor::custom("Pitch Ratio", "Pitch", 0.25, 4.0, 1.0)
                    .with_unit(ParamUnit::None)
                    .with_id(ParamId(3601), "ts_pitch_ratio"),
            ),
            2 => Some(
                ParamDescriptor::time_ms("Grain Size", "Grain", 10.0, 100.0, 50.0)
                    .with_id(ParamId(3602), "ts_grain_size"),
            ),
            3 => Some(
                ParamDescriptor {
                    name: "Density",
                    short_name: "Density",
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(3603), "ts_density"),
            ),
            4 => Some(
                ParamDescriptor {
                    name: "Randomize",
                    short_name: "Random",
                    default: 20.0,
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(3604), "ts_randomize"),
            ),
            5 => Some(
                ParamDescriptor::custom("Freeze", "Freeze", 0.0, 1.0, 0.0)
                    .with_unit(ParamUnit::None)
                    .with_step(1.0)
                    .with_id(ParamId(3605), "ts_freeze")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(&["Off", "On"]),
            ),
            6 => Some(
                ParamDescriptor::custom("Sync", "Sync", 0.0, 1.0, 0.0)
                    .with_unit(ParamUnit::None)
                    .with_step(1.0)
                    .with_id(ParamId(3606), "ts_sync")
                    .with_flags(ParamFlags::AUTOMATABLE.union(ParamFlags::STEPPED))
                    .with_step_labels(&["Off", "On"]),
            ),
            7 => Some(
                ParamDescriptor::gain_db("Output", "Out", -60.0, 6.0, 0.0)
                    .with_id(ParamId(3607), "ts_output"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Interpolated, // time_ratio — 50 ms (avoids artifacts)
            1 => SmoothingStyle::Interpolated, // pitch_ratio — 50 ms
            2 => SmoothingStyle::Slow,         // grain_size — 20 ms
            3 => SmoothingStyle::Standard,     // density — 10 ms
            4 => SmoothingStyle::Standard,     // randomize — 10 ms
            5 => SmoothingStyle::None,         // freeze — stepped
            6 => SmoothingStyle::None,         // sync — stepped
            7 => SmoothingStyle::Fast,         // output_db — 5 ms
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.time_ratio,
            1 => self.pitch_ratio,
            2 => self.grain_size,
            3 => self.density,
            4 => self.randomize,
            5 => self.freeze,
            6 => self.sync,
            7 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.time_ratio = value,
            1 => self.pitch_ratio = value,
            2 => self.grain_size = value,
            3 => self.density = value,
            4 => self.randomize = value,
            5 => self.freeze = value,
            6 => self.sync = value,
            7 => self.output_db = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Grain
// ═══════════════════════════════════════════════════════════════════════════

/// A single time-stretch grain.
#[derive(Clone, Copy)]
struct Grain {
    /// Fractional read position in the buffer.
    read_pos: f32,
    /// Grain progress 0.0 → 1.0. Advances by `1 / grain_size_samples` per sample.
    phase: f32,
    /// Whether this grain is active.
    active: bool,
}

impl Grain {
    const fn new() -> Self {
        Self {
            read_pos: 0.0,
            phase: 0.0,
            active: false,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP time-stretch kernel.
///
/// # Invariants
///
/// - `write_pos` is always in `[0, BUF_LEN)` (unless freeze is active).
/// - Each active grain's `read_pos` is always in `[0, BUF_LEN)`.
/// - Grain `phase` advances uniformly; at 1.0 the grain deactivates.
pub struct TimeStretchKernel {
    /// Circular buffer — left channel.
    buf_l: Vec<f32>,
    /// Circular buffer — right channel.
    buf_r: Vec<f32>,
    /// Next write position.
    write_pos: usize,
    /// Fractional write accumulator for sub-sample time ratio handling.
    write_accum: f32,
    /// Grain state array.
    grains: [Grain; MAX_GRAINS],
    /// Fractional spawn accumulator for density-based grain triggering.
    spawn_accum: f32,
    /// xorshift64 PRNG state.
    rng: u64,
    /// Audio sample rate in Hz.
    sample_rate: f32,
}

impl TimeStretchKernel {
    /// Create a new time-stretch kernel at the given sample rate.
    pub fn new(sample_rate: f32) -> Self {
        let mut buf_l = Vec::with_capacity(BUF_LEN);
        buf_l.resize(BUF_LEN, 0.0);
        let mut buf_r = Vec::with_capacity(BUF_LEN);
        buf_r.resize(BUF_LEN, 0.0);
        // Stagger initial grain phases so output begins immediately
        let mut grains = [Grain::new(); MAX_GRAINS];
        for (i, g) in grains.iter_mut().enumerate() {
            g.phase = i as f32 / MAX_GRAINS as f32;
        }
        Self {
            buf_l,
            buf_r,
            write_pos: 0,
            write_accum: 0.0,
            grains,
            spawn_accum: 0.0,
            rng: 0x1234_5678_9ABC_DEF0,
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

    /// Hann window at phase `t` in [0, 1].
    #[inline]
    fn hann(phase: f32) -> f32 {
        0.5 * (1.0 - libm::cosf(2.0 * core::f32::consts::PI * phase))
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

    /// Grain size in samples, clamped to valid range.
    #[inline]
    fn grain_size_samples(grain_ms: f32, sample_rate: f32) -> f32 {
        let s = grain_ms / 1000.0 * sample_rate;
        s.clamp(GRAIN_MIN_SAMPLES, GRAIN_MAX_SAMPLES)
    }

    /// Number of simultaneous grains based on density (1–MAX_GRAINS).
    #[inline]
    fn active_grains(density: f32) -> usize {
        let n = (density / 100.0 * MAX_GRAINS as f32 + 0.5) as usize;
        n.clamp(1, MAX_GRAINS)
    }
}

impl DspKernel for TimeStretchKernel {
    type Params = TimeStretchParams;

    fn process_stereo(&mut self, left: f32, right: f32, params: &TimeStretchParams) -> (f32, f32) {
        let sr = self.sample_rate;
        let frozen = params.freeze >= 0.5;

        // ── Write input to buffer (unless frozen) ─────────────────────────
        if !frozen {
            let time_ratio = params.time_ratio.clamp(0.25, 4.0);
            // Accumulate fractional write advancement
            // time_ratio > 1 → write advances slower than output rate (time stretch)
            // time_ratio < 1 → write advances faster (time compression)
            self.write_accum += 1.0 / time_ratio;
            while self.write_accum >= 1.0 {
                self.buf_l[self.write_pos] = left;
                self.buf_r[self.write_pos] = right;
                self.write_pos = (self.write_pos + 1) % BUF_LEN;
                self.write_accum -= 1.0;
            }
        }

        let grain_size = Self::grain_size_samples(params.grain_size, sr);
        let phase_inc = 1.0 / grain_size;
        let n_grains = Self::active_grains(params.density);
        let write_pos_f = self.write_pos as f32;

        // Read speed = pitch_ratio (relative to write speed, which is already adjusted)
        let read_speed = params.pitch_ratio.clamp(0.25, 4.0);

        // ── Grain spawn ───────────────────────────────────────────────────
        self.spawn_accum += n_grains as f32 / grain_size;
        while self.spawn_accum >= 1.0 {
            // Find inactive slot
            if let Some(slot) = self.grains.iter().position(|g| !g.active) {
                // Base position: one grain_size behind write head
                let rand_scatter =
                    (self.rand_f32() * 2.0 - 1.0) * (params.randomize / 100.0) * grain_size;
                let start_pos = (write_pos_f - grain_size + rand_scatter + BUF_LEN as f32 * 2.0)
                    % BUF_LEN as f32;
                self.grains[slot] = Grain {
                    read_pos: start_pos,
                    phase: 0.0,
                    active: true,
                };
            }
            self.spawn_accum -= 1.0;
        }

        // ── Accumulate grain output ───────────────────────────────────────
        let mut wet_l = 0.0f32;
        let mut wet_r = 0.0f32;
        let mut active_count = 0u32;

        for grain in self.grains.iter_mut() {
            if !grain.active {
                continue;
            }
            active_count += 1;
            let window = Self::hann(grain.phase);

            wet_l += Self::read_lerp(&self.buf_l, grain.read_pos) * window;
            wet_r += Self::read_lerp(&self.buf_r, grain.read_pos) * window;

            // Read pointer advances at pitch_ratio speed
            grain.read_pos = (grain.read_pos + read_speed + BUF_LEN as f32) % BUF_LEN as f32;
            grain.phase += phase_inc;

            if grain.phase >= 1.0 {
                grain.active = false;
            }
        }

        // Normalize by active grain count to prevent buildup
        if active_count > 1 {
            let norm = 1.0 / active_count as f32;
            wet_l *= norm;
            wet_r *= norm;
        }

        // ── Output gain ───────────────────────────────────────────────────
        let output_gain = fast_db_to_linear(params.output_db);
        (wet_l * output_gain, wet_r * output_gain)
    }

    fn reset(&mut self) {
        for s in self.buf_l.iter_mut() {
            *s = 0.0;
        }
        for s in self.buf_r.iter_mut() {
            *s = 0.0;
        }
        self.write_pos = 0;
        self.write_accum = 0.0;
        for (i, g) in self.grains.iter_mut().enumerate() {
            g.active = false;
            g.phase = i as f32 / MAX_GRAINS as f32;
            g.read_pos = 0.0;
        }
        self.spawn_accum = 0.0;
        self.rng = 0x1234_5678_9ABC_DEF0;
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
    fn finite_output() {
        let mut kernel = TimeStretchKernel::new(48000.0);
        let params = TimeStretchParams::default();
        for i in 0..4096_u32 {
            let t = i as f32 / 48000.0;
            let s = libm::sinf(2.0 * core::f32::consts::PI * 440.0 * t) * 0.5;
            let (l, r) = kernel.process_stereo(s, s, &params);
            assert!(l.is_finite(), "L not finite at {i}: {l}");
            assert!(r.is_finite(), "R not finite at {i}: {r}");
        }
    }

    #[test]
    fn freeze_produces_non_zero_output() {
        let mut kernel = TimeStretchKernel::new(48000.0);
        // Prime buffer
        let normal = TimeStretchParams::default();
        for i in 0..4000_u32 {
            let t = i as f32 / 48000.0;
            let s = libm::sinf(2.0 * core::f32::consts::PI * 220.0 * t) * 0.5;
            kernel.process_stereo(s, s, &normal);
        }
        // Freeze and feed silence
        let frozen = TimeStretchParams {
            freeze: 1.0,
            output_db: 0.0,
            ..TimeStretchParams::default()
        };
        let mut energy = 0.0f32;
        for _ in 0..2000 {
            let (l, _) = kernel.process_stereo(0.0, 0.0, &frozen);
            energy += l * l;
        }
        assert!(
            energy > 0.001,
            "Frozen time-stretch should produce non-zero output, energy={energy}"
        );
    }

    #[test]
    fn params_descriptor_count() {
        assert_eq!(TimeStretchParams::COUNT, 8);
        for i in 0..TimeStretchParams::COUNT {
            assert!(
                TimeStretchParams::descriptor(i).is_some(),
                "Missing descriptor at {i}"
            );
        }
        assert!(TimeStretchParams::descriptor(TimeStretchParams::COUNT).is_none());
    }

    #[test]
    fn adapter_wraps_as_effect() {
        let mut adapter = Adapter::new(TimeStretchKernel::new(48000.0), 48000.0);
        adapter.reset();
        let out = adapter.process(0.3);
        assert!(out.is_finite(), "Adapter output must be finite, got {out}");
    }
}
