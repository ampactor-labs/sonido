//! Texture kernel — granular ambient pad from input.
//!
//! `TextureKernel` captures input into a circular buffer and plays back multiple
//! overlapping grains with random start positions, pitch variation via playback
//! speed, and Hann-window envelopes. The result is a smeared, ambient texture
//! layer added to the dry signal.
//!
//! # Signal Flow
//!
//! ```text
//! input → [circular capture buffer]
//!       → [up to 20 grain readers, each with random start/pitch/Hann envelope]
//!         → sum of all grains → wet/dry mix → output gain
//! ```
//!
//! # Algorithm
//!
//! - **Capture buffer**: 1-second circular buffer per channel (48 000 samples).
//! - **Grain spawn rate**: `density` grains per second. Each new grain is assigned
//!   a random start position within the last `size_ms` of captured audio (scatter
//!   controls how wide this window is) and a random playback speed offset
//!   (`pitch_var` controls ±semitones variation).
//! - **Grain envelope**: Hann window `0.5 × (1 − cos(2π × phase))`. Phase
//!   advances by `1 / grain_size_samples` per sample.
//! - **Grain count**: at most `MAX_GRAINS` (20) active simultaneously.
//! - A simple xorshift PRNG provides randomness without `std`.
//!
//! # Deployment
//!
//! ```rust,ignore
//! // Desktop / Plugin (via adapter — handles smoothing automatically)
//! let adapter = Adapter::new(TextureKernel::new(48000.0), 48000.0);
//! let mut effect: Box<dyn Effect> = Box::new(adapter);
//!
//! // Embedded / Daisy Seed (direct — no smoothing)
//! let mut kernel = TextureKernel::new(48000.0);
//! let params = TextureParams::default();
//! let (left, right) = kernel.process_stereo(input_l, input_r, &params);
//! ```

extern crate alloc;

use alloc::vec::Vec;
use sonido_core::kernel::{DspKernel, KernelParams, SmoothingStyle};
use sonido_core::{ParamDescriptor, ParamId, ParamUnit, fast_db_to_linear, wet_dry_mix};

// ═══════════════════════════════════════════════════════════════════════════
//  Constants
// ═══════════════════════════════════════════════════════════════════════════

/// Capture buffer length in samples (1 second at 48 kHz, power-of-2 for fast modulo).
const BUF_LEN: usize = 65536; // ~1.36 s at 48 kHz

/// Maximum simultaneous grains.
const MAX_GRAINS: usize = 20;

// ═══════════════════════════════════════════════════════════════════════════
//  Parameters
// ═══════════════════════════════════════════════════════════════════════════

/// Parameter values for [`TextureKernel`].
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `density` | grains/s | 1–20 | 8.0 |
/// | 1 | `size_ms` | ms | 50–500 | 200.0 |
/// | 2 | `scatter` | % | 0–100 | 50.0 |
/// | 3 | `pitch_var` | semitones | 0–24 | 2.0 |
/// | 4 | `mix_pct` | % | 0–100 | 50.0 |
/// | 5 | `output_db` | dB | −60–+6 | 0.0 |
#[derive(Debug, Clone, Copy)]
pub struct TextureParams {
    /// Grain spawn rate in grains per second. Range: 1.0 to 20.0.
    pub density: f32,
    /// Grain size in milliseconds. Range: 50–500 ms.
    pub size_ms: f32,
    /// Scatter: how spread the grain start positions are within the buffer (0–100 %).
    ///
    /// At 0 % all grains start from the same region. At 100 % they span the
    /// entire capture buffer.
    pub scatter: f32,
    /// Maximum pitch variation in semitones (0–24). Each grain gets a random
    /// playback speed in `[−pitch_var, +pitch_var]` semitones.
    pub pitch_var: f32,
    /// Wet/dry mix in percent (0–100 %).
    pub mix_pct: f32,
    /// Output level in decibels (−60–+6 dB).
    pub output_db: f32,
}

impl Default for TextureParams {
    fn default() -> Self {
        Self {
            density: 8.0,
            size_ms: 200.0,
            scatter: 50.0,
            pitch_var: 2.0,
            mix_pct: 50.0,
            output_db: 0.0,
        }
    }
}

impl KernelParams for TextureParams {
    const COUNT: usize = 6;

    fn descriptor(index: usize) -> Option<ParamDescriptor> {
        match index {
            0 => Some(
                ParamDescriptor::custom("Density", "Density", 1.0, 20.0, 8.0)
                    .with_unit(ParamUnit::None)
                    .with_id(ParamId(3400), "tex_density"),
            ),
            1 => Some(
                ParamDescriptor::time_ms("Size", "Size", 50.0, 500.0, 200.0)
                    .with_id(ParamId(3401), "tex_size"),
            ),
            2 => Some(
                ParamDescriptor {
                    name: "Scatter",
                    short_name: "Scatter",
                    ..ParamDescriptor::mix()
                }
                .with_id(ParamId(3402), "tex_scatter"),
            ),
            3 => Some(
                ParamDescriptor::custom("Pitch Var", "Pitch", 0.0, 24.0, 2.0)
                    .with_unit(ParamUnit::None)
                    .with_id(ParamId(3403), "tex_pitch_var"),
            ),
            4 => Some(ParamDescriptor::mix().with_id(ParamId(3404), "tex_mix")),
            5 => Some(
                ParamDescriptor::gain_db("Output", "Out", -60.0, 6.0, 0.0)
                    .with_id(ParamId(3405), "tex_output"),
            ),
            _ => None,
        }
    }

    fn smoothing(index: usize) -> SmoothingStyle {
        match index {
            0 => SmoothingStyle::Slow,     // density — 20 ms
            1 => SmoothingStyle::Slow,     // size_ms — 20 ms
            2 => SmoothingStyle::Standard, // scatter — 10 ms
            3 => SmoothingStyle::Slow,     // pitch_var — 20 ms
            4 => SmoothingStyle::Standard, // mix_pct — 10 ms
            5 => SmoothingStyle::Fast,     // output_db — 5 ms
            _ => SmoothingStyle::Standard,
        }
    }

    fn get(&self, index: usize) -> f32 {
        match index {
            0 => self.density,
            1 => self.size_ms,
            2 => self.scatter,
            3 => self.pitch_var,
            4 => self.mix_pct,
            5 => self.output_db,
            _ => 0.0,
        }
    }

    fn set(&mut self, index: usize, value: f32) {
        match index {
            0 => self.density = value,
            1 => self.size_ms = value,
            2 => self.scatter = value,
            3 => self.pitch_var = value,
            4 => self.mix_pct = value,
            5 => self.output_db = value,
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Grain
// ═══════════════════════════════════════════════════════════════════════════

/// A single granular texture grain.
#[derive(Clone, Copy)]
struct Grain {
    /// Fractional read position in the capture buffer.
    read_pos: f32,
    /// Playback speed (1.0 = normal, <1 = slow/lower pitch, >1 = fast/higher pitch).
    speed: f32,
    /// Grain progress 0.0 → 1.0. Advances by `1 / grain_size_samples` per sample.
    phase: f32,
    /// Whether this grain slot is active.
    active: bool,
}

impl Grain {
    const fn new() -> Self {
        Self {
            read_pos: 0.0,
            speed: 1.0,
            phase: 0.0,
            active: false,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Kernel
// ═══════════════════════════════════════════════════════════════════════════

/// Pure DSP granular texture kernel.
///
/// # Invariants
///
/// - `write_pos` is always in `[0, BUF_LEN)`.
/// - Each active grain's `read_pos` is always in `[0, BUF_LEN)`.
/// - Grain `phase` advances uniformly from 0 to 1; at 1 the grain deactivates.
pub struct TextureKernel {
    /// Circular capture buffer — left channel.
    buf_l: Vec<f32>,
    /// Circular capture buffer — right channel.
    buf_r: Vec<f32>,
    /// Next write position.
    write_pos: usize,
    /// Number of samples written so far, capped at BUF_LEN.
    ///
    /// Used to guard grain spawning: grains only read within written regions.
    samples_written: usize,
    /// Grain state array.
    grains: [Grain; MAX_GRAINS],
    /// Accumulated fractional grain count (samples until next spawn).
    spawn_accum: f32,
    /// xorshift64 PRNG state (never 0).
    rng: u64,
    /// Audio sample rate in Hz.
    sample_rate: f32,
}

impl TextureKernel {
    /// Create a new texture kernel at the given sample rate.
    pub fn new(sample_rate: f32) -> Self {
        let mut buf_l = Vec::with_capacity(BUF_LEN);
        buf_l.resize(BUF_LEN, 0.0);
        let mut buf_r = Vec::with_capacity(BUF_LEN);
        buf_r.resize(BUF_LEN, 0.0);
        Self {
            buf_l,
            buf_r,
            write_pos: 0,
            samples_written: 0,
            grains: [Grain::new(); MAX_GRAINS],
            spawn_accum: 0.0,
            rng: 0xDEAD_BEEF_CAFE_1337,
            sample_rate,
        }
    }

    /// xorshift64 PRNG — returns a value in [0.0, 1.0).
    #[inline]
    fn rand_f32(&mut self) -> f32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        // Map to [0, 1) via bitmask on mantissa
        let bits = (x >> 40) as u32; // 24 bits
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

    /// Spawn a new grain at a position based on scatter and write cursor.
    fn spawn_grain(&mut self, params: &TextureParams) {
        // Find an inactive slot
        let slot = self.grains.iter().position(|g| !g.active);
        let Some(slot) = slot else { return };

        let sr = self.sample_rate;
        let size_samples = (params.size_ms / 1000.0 * sr).clamp(10.0, BUF_LEN as f32 - 1.0);

        // Guard: only spawn if enough audio has been written to fill a grain.
        // This prevents grains from reading unwritten (zero) buffer regions.
        if self.samples_written < size_samples as usize + 1 {
            return;
        }

        // Scatter: random offset within [size_samples, size_samples + scatter_window] behind write head.
        // Clamped to written region so grains always read real audio.
        let max_lookback = (self.samples_written as f32).min(BUF_LEN as f32 - 1.0);
        let scatter_window = ((params.scatter / 100.0) * (max_lookback - size_samples)).max(0.0);
        let offset = self.rand_f32() * scatter_window + size_samples;
        let read_start = (self.write_pos as f32 - offset + BUF_LEN as f32 * 2.0) % BUF_LEN as f32;

        // Pitch variation: random speed in [2^(-pitch_var/12), 2^(+pitch_var/12)]
        let rand_semi = (self.rand_f32() * 2.0 - 1.0) * params.pitch_var;
        let speed = libm::powf(2.0, rand_semi / 12.0);

        self.grains[slot] = Grain {
            read_pos: read_start,
            speed,
            phase: 0.0,
            active: true,
        };
    }
}

impl DspKernel for TextureKernel {
    type Params = TextureParams;

    fn process_stereo(&mut self, left: f32, right: f32, params: &TextureParams) -> (f32, f32) {
        // ── Write input to capture buffer ─────────────────────────────────
        self.buf_l[self.write_pos] = left;
        self.buf_r[self.write_pos] = right;
        self.write_pos = (self.write_pos + 1) % BUF_LEN;
        if self.samples_written < BUF_LEN {
            self.samples_written += 1;
        }

        let sr = self.sample_rate;
        let size_samples = (params.size_ms / 1000.0 * sr).clamp(10.0, BUF_LEN as f32 - 1.0);
        let phase_inc = 1.0 / size_samples;

        // ── Grain spawn ───────────────────────────────────────────────────
        // Accumulate fractional samples until next spawn threshold
        self.spawn_accum += params.density.max(0.1) / sr;
        while self.spawn_accum >= 1.0 {
            self.spawn_grain(params);
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

            grain.read_pos = (grain.read_pos + grain.speed + BUF_LEN as f32) % BUF_LEN as f32;
            grain.phase += phase_inc;

            if grain.phase >= 1.0 {
                grain.active = false;
            }
        }

        // Normalize by active grain count to prevent amplitude buildup
        if active_count > 1 {
            let norm = 1.0 / active_count as f32;
            wet_l *= norm;
            wet_r *= norm;
        }

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
        self.samples_written = 0;
        for grain in self.grains.iter_mut() {
            grain.active = false;
        }
        self.spawn_accum = 0.0;
        self.rng = 0xDEAD_BEEF_CAFE_1337;
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
        let mut kernel = TextureKernel::new(48000.0);
        let params = TextureParams::default();
        for i in 0..4096_u32 {
            let t = i as f32 / 48000.0;
            let s = libm::sinf(2.0 * core::f32::consts::PI * 440.0 * t) * 0.5;
            let (l, r) = kernel.process_stereo(s, s, &params);
            assert!(l.is_finite(), "L not finite at {i}: {l}");
            assert!(r.is_finite(), "R not finite at {i}: {r}");
        }
    }

    #[test]
    fn grains_produce_output_during_active_window() {
        // Grain can only spawn after size_samples of audio is written.
        // Use size_ms=50ms → size_samples=2400. After 2400 samples written,
        // first grain spawns at ~2400 + 2400/20 = ~2520 samples.
        // Grain active for 2400 samples: 2520 to 4920.
        // Measure energy from 3000 to 4500 — grain is active throughout.
        let mut kernel = TextureKernel::new(48000.0);
        let params = TextureParams {
            density: 20.0,
            size_ms: 50.0,
            scatter: 0.0, // no scatter — grain reads right behind write head
            mix_pct: 100.0,
            pitch_var: 0.0, // no pitch variation
            ..TextureParams::default()
        };
        let mut energy = 0.0f32;
        // size_ms=50ms → size_samples=2400. Guard requires 2401 samples written.
        // spawn_accum reaches 1.0 at sample 2400/(20/48000) ≈ 4800 * 2 = wrong
        // Actually: spawn happens when samples_written >= 2401 AND accum >= 1.0.
        // accum reaches 1 after 48000/20 = 2400 samples. But guard blocks until 2401 written.
        // Both conditions met earliest at sample 4799. Grain active 4799–7199.
        // Measure from 5000 to 7000.
        for i in 0..8000_u32 {
            let t = i as f32 / 48000.0;
            let s = libm::sinf(2.0 * core::f32::consts::PI * 220.0 * t) * 0.5;
            let (l, _) = kernel.process_stereo(s, s, &params);
            if (5000..7000).contains(&i) {
                energy += l * l;
            }
        }
        assert!(
            energy > 0.01,
            "Texture grains should produce energy while active: {energy}"
        );
    }

    #[test]
    fn params_descriptor_count() {
        assert_eq!(TextureParams::COUNT, 6);
        for i in 0..TextureParams::COUNT {
            assert!(
                TextureParams::descriptor(i).is_some(),
                "Missing descriptor at {i}"
            );
        }
        assert!(TextureParams::descriptor(TextureParams::COUNT).is_none());
    }

    #[test]
    fn adapter_wraps_as_effect() {
        let mut adapter = Adapter::new(TextureKernel::new(48000.0), 48000.0);
        adapter.reset();
        let out = adapter.process(0.3);
        assert!(out.is_finite(), "Adapter output must be finite, got {out}");
    }
}
