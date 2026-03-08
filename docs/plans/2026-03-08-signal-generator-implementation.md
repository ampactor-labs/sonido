# Signal Generator & Mic Removal Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace mic input with a built-in signal generator (sine, sweep, noise, impulse, saw chord) as the default audio source, so Sonido opens ready to make sound on first spacebar press.

**Architecture:** New `SignalGenerator` struct generates samples per-block in the audio thread. `SourceMode` enum (Generator/File) replaces the old mic/file toggle. Input stream (cpal microphone) is removed entirely. Transport commands extended with generator control variants.

**Tech Stack:** Pure Rust math for signal generation (no new crate dependencies — `fastrand` not needed, simple LCG or xorshift inline suffices for noise). Existing `SonidoTheme` arcade widgets for UI.

---

### Task 1: Create `SignalGenerator` with sine output

**Files:**
- Create: `crates/sonido-gui/src/signal_generator.rs`
- Modify: `crates/sonido-gui/src/lib.rs:6` (add module)

**Step 1: Write the signal generator module with tests**

Create `crates/sonido-gui/src/signal_generator.rs`:

```rust
//! Built-in test signal generator for the audio thread.
//!
//! Generates stereo audio samples on-the-fly without file I/O.
//! Supports multiple signal types useful for testing and tuning effects:
//! sine, frequency sweep, white/pink noise, impulse train, and sawtooth chord.

use std::f64::consts::TAU;

/// Available signal types for the built-in generator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalType {
    /// Pure sine wave at a single frequency.
    Sine,
    /// Logarithmic frequency sweep from start to end Hz.
    Sweep,
    /// Uniform white noise (flat spectrum).
    WhiteNoise,
    /// Pink noise via Voss-McCartney algorithm (−3 dB/octave).
    PinkNoise,
    /// Impulse train — single-sample clicks at a fixed rate.
    Impulse,
    /// Sawtooth chord (root + major 3rd + perfect 5th) via naive saw.
    SawChord,
}

impl SignalType {
    /// Display name for UI.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Sine => "Sine",
            Self::Sweep => "Sweep",
            Self::WhiteNoise => "White Noise",
            Self::PinkNoise => "Pink Noise",
            Self::Impulse => "Impulse",
            Self::SawChord => "Saw Chord",
        }
    }

    /// All signal types in display order.
    pub const ALL: &[Self] = &[
        Self::Sine,
        Self::Sweep,
        Self::WhiteNoise,
        Self::PinkNoise,
        Self::Impulse,
        Self::SawChord,
    ];
}

/// Audio source mode — generator or file playback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceMode {
    /// Built-in signal generator.
    Generator,
    /// WAV file playback.
    File,
}

/// Built-in signal generator for the audio thread.
///
/// Generates stereo samples per-block. All signals are mono (identical L/R)
/// except where stereo decorrelation would be meaningful (noise).
pub struct SignalGenerator {
    signal_type: SignalType,
    /// Continuous phase accumulator (0.0–1.0 range, wraps).
    phase: f64,
    /// Primary frequency in Hz.
    frequency: f32,
    /// Output amplitude (0.0–1.0, linear).
    amplitude: f32,
    /// Device sample rate.
    sample_rate: f32,
    /// Whether the generator is producing output.
    playing: bool,

    // -- Sweep state --
    sweep_start_hz: f32,
    sweep_end_hz: f32,
    sweep_duration_secs: f32,
    sweep_position: f64,
    sweep_loop: bool,

    // -- Impulse state --
    impulse_rate_hz: f32,
    impulse_counter: usize,
    impulse_interval: usize,

    // -- Pink noise (Voss-McCartney, 16 rows) --
    pink_rows: [f32; 16],
    pink_running_sum: f32,
    pink_index: u32,
    /// Simple xorshift64 state for noise generation.
    noise_state: u64,

    // -- Saw chord phases --
    saw_phases: [f64; 3],
}

impl SignalGenerator {
    /// Create a new generator at the given sample rate.
    ///
    /// Defaults to sine 440 Hz, amplitude 0.5 (−6 dB), paused.
    pub fn new(sample_rate: f32) -> Self {
        let impulse_rate_hz = 2.0;
        let impulse_interval = (sample_rate / impulse_rate_hz) as usize;
        Self {
            signal_type: SignalType::Sine,
            phase: 0.0,
            frequency: 440.0,
            amplitude: 0.5,
            sample_rate,
            playing: false,

            sweep_start_hz: 20.0,
            sweep_end_hz: 20000.0,
            sweep_duration_secs: 5.0,
            sweep_position: 0.0,
            sweep_loop: true,

            impulse_rate_hz,
            impulse_counter: 0,
            impulse_interval,

            pink_rows: [0.0; 16],
            pink_running_sum: 0.0,
            pink_index: 0,
            noise_state: 0x1234_5678_9ABC_DEF0,

            saw_phases: [0.0; 3],
        }
    }

    /// Whether the generator is currently playing.
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Set playing state.
    pub fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
    }

    /// Stop and reset phase accumulators to zero.
    pub fn stop(&mut self) {
        self.playing = false;
        self.phase = 0.0;
        self.sweep_position = 0.0;
        self.impulse_counter = 0;
        self.saw_phases = [0.0; 3];
    }

    /// Current signal type.
    pub fn signal_type(&self) -> SignalType {
        self.signal_type
    }

    /// Set the signal type, resetting phase state.
    pub fn set_signal_type(&mut self, t: SignalType) {
        self.signal_type = t;
        self.phase = 0.0;
        self.sweep_position = 0.0;
        self.impulse_counter = 0;
        self.saw_phases = [0.0; 3];
    }

    /// Current frequency in Hz.
    pub fn frequency(&self) -> f32 {
        self.frequency
    }

    /// Set the primary frequency (clamped to 20–20000 Hz).
    pub fn set_frequency(&mut self, hz: f32) {
        self.frequency = hz.clamp(20.0, 20000.0);
    }

    /// Current amplitude (0.0–1.0 linear).
    pub fn amplitude(&self) -> f32 {
        self.amplitude
    }

    /// Set amplitude (clamped to 0.0–1.0).
    pub fn set_amplitude(&mut self, amp: f32) {
        self.amplitude = amp.clamp(0.0, 1.0);
    }

    /// Set sweep parameters.
    pub fn set_sweep_params(&mut self, start_hz: f32, end_hz: f32, duration_secs: f32, looping: bool) {
        self.sweep_start_hz = start_hz.clamp(20.0, 20000.0);
        self.sweep_end_hz = end_hz.clamp(20.0, 20000.0);
        self.sweep_duration_secs = duration_secs.clamp(0.5, 30.0);
        self.sweep_loop = looping;
    }

    /// Set impulse train rate in Hz.
    pub fn set_impulse_rate(&mut self, hz: f32) {
        self.impulse_rate_hz = hz.clamp(0.5, 20.0);
        self.impulse_interval = (self.sample_rate / self.impulse_rate_hz) as usize;
    }

    /// Set the sample rate (e.g., after device change).
    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sample_rate = sr;
        self.impulse_interval = (sr / self.impulse_rate_hz) as usize;
    }

    /// Generate a block of stereo samples into the provided buffers.
    ///
    /// If not playing, fills both buffers with silence.
    pub fn generate(&mut self, left: &mut [f32], right: &mut [f32]) {
        debug_assert_eq!(left.len(), right.len());
        if !self.playing {
            left.fill(0.0);
            right.fill(0.0);
            return;
        }
        let amp = self.amplitude;
        match self.signal_type {
            SignalType::Sine => self.gen_sine(left, right, amp),
            SignalType::Sweep => self.gen_sweep(left, right, amp),
            SignalType::WhiteNoise => self.gen_white_noise(left, right, amp),
            SignalType::PinkNoise => self.gen_pink_noise(left, right, amp),
            SignalType::Impulse => self.gen_impulse(left, right, amp),
            SignalType::SawChord => self.gen_saw_chord(left, right, amp),
        }
    }

    // -- Private generators --

    fn gen_sine(&mut self, left: &mut [f32], right: &mut [f32], amp: f32) {
        let inc = self.frequency as f64 / self.sample_rate as f64;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let s = (self.phase * TAU).sin() as f32 * amp;
            *l = s;
            *r = s;
            self.phase += inc;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }
        }
    }

    fn gen_sweep(&mut self, left: &mut [f32], right: &mut [f32], amp: f32) {
        let sr = self.sample_rate as f64;
        let dur = self.sweep_duration_secs as f64;
        let log_start = (self.sweep_start_hz as f64).ln();
        let log_end = (self.sweep_end_hz as f64).ln();
        let step = 1.0 / (sr * dur);

        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let freq = (log_start + (log_end - log_start) * self.sweep_position).exp();
            let inc = freq / sr;
            let s = (self.phase * TAU).sin() as f32 * amp;
            *l = s;
            *r = s;
            self.phase += inc;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }
            self.sweep_position += step;
            if self.sweep_position >= 1.0 {
                if self.sweep_loop {
                    self.sweep_position = 0.0;
                } else {
                    self.sweep_position = 1.0;
                }
            }
        }
    }

    fn gen_white_noise(&mut self, left: &mut [f32], right: &mut [f32], amp: f32) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            // Decorrelated L/R for white noise
            *l = self.next_noise() * amp;
            *r = self.next_noise() * amp;
        }
    }

    fn gen_pink_noise(&mut self, left: &mut [f32], right: &mut [f32], amp: f32) {
        // Voss-McCartney algorithm: sum of 16 octave-band white noise sources
        // Each row updates at half the rate of the previous → −3 dB/octave rolloff
        let scale = amp / 16.0; // normalize sum of 16 rows
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            // Determine which rows to update (trailing zeros of index)
            let changed = (self.pink_index + 1) ^ self.pink_index;
            self.pink_index = self.pink_index.wrapping_add(1);

            for bit in 0..16u32 {
                if changed & (1 << bit) != 0 {
                    let old = self.pink_rows[bit as usize];
                    let new = self.next_noise();
                    self.pink_rows[bit as usize] = new;
                    self.pink_running_sum += new - old;
                }
            }

            let s = self.pink_running_sum * scale;
            *l = s;
            *r = s;
        }
    }

    fn gen_impulse(&mut self, left: &mut [f32], right: &mut [f32], amp: f32) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let s = if self.impulse_counter == 0 { amp } else { 0.0 };
            *l = s;
            *r = s;
            self.impulse_counter += 1;
            if self.impulse_counter >= self.impulse_interval {
                self.impulse_counter = 0;
            }
        }
    }

    fn gen_saw_chord(&mut self, left: &mut [f32], right: &mut [f32], amp: f32) {
        // Root + major third (5:4 ratio) + perfect fifth (3:2 ratio)
        let root = self.frequency as f64;
        let freqs = [root, root * 5.0 / 4.0, root * 3.0 / 2.0];
        let sr = self.sample_rate as f64;
        let voice_amp = amp / 3.0; // equal mix of 3 voices

        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let mut sum = 0.0f32;
            for (i, &freq) in freqs.iter().enumerate() {
                // Naive sawtooth: 2 * phase - 1 (range −1 to +1)
                let saw = (2.0 * self.saw_phases[i] - 1.0) as f32;
                sum += saw;
                self.saw_phases[i] += freq / sr;
                if self.saw_phases[i] >= 1.0 {
                    self.saw_phases[i] -= 1.0;
                }
            }
            let s = sum * voice_amp;
            *l = s;
            *r = s;
        }
    }

    /// Xorshift64 PRNG producing a sample in −1.0..+1.0.
    fn next_noise(&mut self) -> f32 {
        self.noise_state ^= self.noise_state << 13;
        self.noise_state ^= self.noise_state >> 7;
        self.noise_state ^= self.noise_state << 17;
        // Convert to float in −1.0..+1.0
        (self.noise_state as i64 as f64 / i64::MAX as f64) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_output_bounded() {
        let mut gen = SignalGenerator::new(48000.0);
        gen.set_playing(true);
        let mut left = vec![0.0f32; 4800];
        let mut right = vec![0.0f32; 4800];
        gen.generate(&mut left, &mut right);
        for &s in &left {
            assert!(s.abs() <= 0.5 + 1e-6, "sine sample out of range: {s}");
        }
    }

    #[test]
    fn silence_when_paused() {
        let mut gen = SignalGenerator::new(48000.0);
        // Default: not playing
        let mut left = vec![1.0f32; 100];
        let mut right = vec![1.0f32; 100];
        gen.generate(&mut left, &mut right);
        assert!(left.iter().all(|&s| s == 0.0));
        assert!(right.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn white_noise_has_nonzero_variance() {
        let mut gen = SignalGenerator::new(48000.0);
        gen.set_signal_type(SignalType::WhiteNoise);
        gen.set_playing(true);
        let mut left = vec![0.0f32; 4800];
        let mut right = vec![0.0f32; 4800];
        gen.generate(&mut left, &mut right);
        let variance: f32 = left.iter().map(|s| s * s).sum::<f32>() / left.len() as f32;
        assert!(variance > 0.01, "white noise variance too low: {variance}");
    }

    #[test]
    fn impulse_has_correct_spacing() {
        let mut gen = SignalGenerator::new(48000.0);
        gen.set_signal_type(SignalType::Impulse);
        gen.set_impulse_rate(10.0); // 10 Hz → every 4800 samples
        gen.set_playing(true);
        let mut left = vec![0.0f32; 9600];
        let mut right = vec![0.0f32; 9600];
        gen.generate(&mut left, &mut right);
        let impulse_positions: Vec<usize> = left
            .iter()
            .enumerate()
            .filter(|(_, &s)| s > 0.0)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(impulse_positions[0], 0);
        assert_eq!(impulse_positions[1], 4800);
    }

    #[test]
    fn sweep_loops() {
        let mut gen = SignalGenerator::new(48000.0);
        gen.set_signal_type(SignalType::Sweep);
        gen.set_sweep_params(100.0, 1000.0, 1.0, true);
        gen.set_playing(true);
        // Generate 2 seconds — should loop at least once
        let mut left = vec![0.0f32; 96000];
        let mut right = vec![0.0f32; 96000];
        gen.generate(&mut left, &mut right);
        // Just verify it doesn't crash and produces non-zero output
        let energy: f32 = left.iter().map(|s| s * s).sum();
        assert!(energy > 0.0);
    }

    #[test]
    fn saw_chord_bounded() {
        let mut gen = SignalGenerator::new(48000.0);
        gen.set_signal_type(SignalType::SawChord);
        gen.set_playing(true);
        let mut left = vec![0.0f32; 4800];
        let mut right = vec![0.0f32; 4800];
        gen.generate(&mut left, &mut right);
        for &s in &left {
            assert!(s.abs() <= 0.5 + 1e-6, "saw chord out of range: {s}");
        }
    }

    #[test]
    fn stop_resets_phase() {
        let mut gen = SignalGenerator::new(48000.0);
        gen.set_playing(true);
        let mut left = vec![0.0f32; 1000];
        let mut right = vec![0.0f32; 1000];
        gen.generate(&mut left, &mut right);
        assert!(gen.phase > 0.0);
        gen.stop();
        assert_eq!(gen.phase, 0.0);
        assert!(!gen.is_playing());
    }

    #[test]
    fn pink_noise_bounded() {
        let mut gen = SignalGenerator::new(48000.0);
        gen.set_signal_type(SignalType::PinkNoise);
        gen.set_playing(true);
        let mut left = vec![0.0f32; 48000];
        let mut right = vec![0.0f32; 48000];
        gen.generate(&mut left, &mut right);
        for &s in &left {
            assert!(s.is_finite(), "pink noise produced non-finite: {s}");
            assert!(s.abs() < 2.0, "pink noise out of range: {s}");
        }
    }
}
```

**Step 2: Add module to lib.rs**

In `crates/sonido-gui/src/lib.rs`, add `pub mod signal_generator;` after the existing modules.

**Step 3: Run tests**

Run: `cargo test -p sonido-gui signal_generator`
Expected: All 8 tests pass.

**Step 4: Commit**

```bash
git add crates/sonido-gui/src/signal_generator.rs crates/sonido-gui/src/lib.rs
git commit -m "feat(gui): add SignalGenerator with 6 signal types"
```

---

### Task 2: Remove mic input from audio processor

**Files:**
- Modify: `crates/sonido-gui/src/audio_processor.rs`
- Modify: `crates/sonido-gui/src/file_player.rs` (remove `SetFileMode` variant)
- Modify: `crates/sonido-gui/src/main.rs` (remove `--input` arg)

**Step 1: Remove `SetFileMode` from `TransportCommand`**

In `crates/sonido-gui/src/file_player.rs`:
- Delete the `SetFileMode(bool)` variant from `TransportCommand` enum (~line 51)

**Step 2: Remove input stream from `build_audio_streams()`**

In `crates/sonido-gui/src/audio_processor.rs`:
- Remove `input_device` lookup (~line 390)
- Remove entire input stream block (~lines 422-496): the `(tx, rx)` channel, `tx_fallback`, `in_ch` computation, input stream build, pre-fill silence
- Remove `input_rx: Receiver<f32>` field from `AudioProcessor` (~line 101)
- Remove `in_ch: usize` field from `AudioProcessor` (~line 104)
- Remove them from the `AudioProcessor` constructor in `build_audio_streams()`

**Step 3: Simplify `process_buffer()` input source selection**

In `AudioProcessor::process_buffer()` (~lines 222-257), replace the entire mic/file input selection with file-only:

```rust
for _ in 0..frames {
    let (in_l_raw, in_r_raw) = if has_file {
        self.file_pb.next_frame()
    } else {
        (0.0, 0.0)
    };
    let in_l = if in_l_raw.is_finite() { in_l_raw } else { 0.0 };
    let in_r = if in_r_raw.is_finite() { in_r_raw } else { 0.0 };
    raw_left.push(in_l * ig);
    raw_right.push(in_r * ig);
}
```

**Step 4: Remove `SetFileMode` handling**

In `process_buffer()` transport command drain (~line 177), remove the `TransportCommand::SetFileMode` arm.

**Step 5: Remove `file_mode` from `FilePlayback`**

In `FilePlayback` struct: remove `file_mode: bool` field and its default (`true`).
Remove `let file_mode = self.file_pb.file_mode;` from `process_buffer()`.
Remove `let has_file = ...` usage of `file_mode` — just use `!self.file_pb.left.is_empty()`.

**Step 6: Remove `--input` CLI arg**

In `crates/sonido-gui/src/main.rs`:
- Remove `input: Option<String>` field from `Args` struct (~lines 22-23)
- Remove the `if let Some(ref input)` log block (~lines 61-63)

**Step 7: Clean up FilePlayer**

In `crates/sonido-gui/src/file_player.rs`:
- Remove `use_file_input: bool` field from `FilePlayer` struct (~line 68)
- Remove `use_file_input: true` from constructor (~line 100)
- Remove `use_file_input()` method (~lines 124-126)
- Remove `render_source_toggle()` method entirely (~lines 310-335)
- In `resync_transport()`, remove the `SetFileMode` send (~lines 161-163)

**Step 8: Compile check and test**

Run: `cargo test -p sonido-gui`
Run: `cargo check --target wasm32-unknown-unknown -p sonido-gui`
Expected: All pass (some app.rs call sites will break — fixed in Task 3).

Note: This task and Task 3 must be committed together since removing `render_source_toggle()` and `use_file_input()` will break app.rs call sites.

**Step 9: Commit** (together with Task 3)

---

### Task 3: Integrate `SignalGenerator` into audio processor

**Files:**
- Modify: `crates/sonido-gui/src/audio_processor.rs`
- Modify: `crates/sonido-gui/src/file_player.rs` (add generator transport commands)

**Step 1: Add generator transport commands**

In `crates/sonido-gui/src/file_player.rs`, add to `TransportCommand` enum:

```rust
/// Set the active source mode (generator or file).
SetSourceMode(crate::signal_generator::SourceMode),
/// Change signal generator type.
SetSignalType(crate::signal_generator::SignalType),
/// Set generator frequency in Hz.
SetGeneratorFreq(f32),
/// Set generator amplitude (0.0-1.0).
SetGeneratorAmplitude(f32),
/// Set sweep parameters.
SetSweepParams {
    start_hz: f32,
    end_hz: f32,
    duration_secs: f32,
    looping: bool,
},
/// Set impulse train rate in Hz.
SetImpulseRate(f32),
```

**Step 2: Add `SignalGenerator` and `SourceMode` to `AudioProcessor`**

In `crates/sonido-gui/src/audio_processor.rs`:
- Add `use crate::signal_generator::{SignalGenerator, SourceMode};` to imports
- Add fields to `AudioProcessor`:
  ```rust
  signal_gen: SignalGenerator,
  source_mode: SourceMode,
  ```
- Initialize in `build_audio_streams()`:
  ```rust
  signal_gen: SignalGenerator::new(sample_rate),
  source_mode: SourceMode::Generator,
  ```

**Step 3: Handle new transport commands in `process_buffer()`**

Add match arms in the transport command drain:

```rust
TransportCommand::SetSourceMode(mode) => self.source_mode = mode,
TransportCommand::SetSignalType(t) => self.signal_gen.set_signal_type(t),
TransportCommand::SetGeneratorFreq(hz) => self.signal_gen.set_frequency(hz),
TransportCommand::SetGeneratorAmplitude(amp) => self.signal_gen.set_amplitude(amp),
TransportCommand::SetSweepParams { start_hz, end_hz, duration_secs, looping } => {
    self.signal_gen.set_sweep_params(start_hz, end_hz, duration_secs, looping);
}
TransportCommand::SetImpulseRate(hz) => self.signal_gen.set_impulse_rate(hz),
```

Also update Play/Pause/Stop to route to the correct source:

```rust
TransportCommand::Play => {
    match self.source_mode {
        SourceMode::Generator => self.signal_gen.set_playing(true),
        SourceMode::File => self.file_pb.playing = true,
    }
}
TransportCommand::Pause => {
    match self.source_mode {
        SourceMode::Generator => self.signal_gen.set_playing(false),
        SourceMode::File => self.file_pb.playing = false,
    }
}
TransportCommand::Stop => {
    match self.source_mode {
        SourceMode::Generator => self.signal_gen.stop(),
        SourceMode::File => {
            self.file_pb.playing = false;
            self.file_pb.position = 0;
        }
    }
}
```

**Step 4: Update input source selection in `process_buffer()`**

Replace the file-only input loop (from Task 2) with:

```rust
match self.source_mode {
    SourceMode::Generator => {
        self.signal_gen.generate(&mut raw_left, &mut raw_right);
        // Apply input gain
        for (l, r) in raw_left.iter_mut().zip(raw_right.iter_mut()) {
            *l *= ig;
            *r *= ig;
        }
    }
    SourceMode::File => {
        for i in 0..frames {
            let (in_l_raw, in_r_raw) = if has_file {
                self.file_pb.next_frame()
            } else {
                (0.0, 0.0)
            };
            let in_l = if in_l_raw.is_finite() { in_l_raw } else { 0.0 };
            let in_r = if in_r_raw.is_finite() { in_r_raw } else { 0.0 };
            raw_left[i] = in_l * ig;
            raw_right[i] = in_r * ig;
        }
    }
}
```

Note: `raw_left`/`raw_right` must be allocated with length `frames` before this block (change from `Vec::with_capacity` to `vec![0.0; frames]`) since the generator writes directly.

**Step 5: Compile and test**

Run: `cargo check -p sonido-gui`
Expected: Compile errors in app.rs (call sites for removed methods) — fixed in Task 4.

**Step 6: Commit** (together with Task 2 and Task 4)

---

### Task 4: Update app.rs — source toggle UI and spacebar

**Files:**
- Modify: `crates/sonido-gui/src/app.rs`
- Modify: `crates/sonido-gui/src/file_player.rs` (add source mode state + UI)

**Step 1: Add source mode state to `FilePlayer`**

In `FilePlayer` struct, replace `use_file_input: bool` with:

```rust
/// Active audio source mode.
source_mode: SourceMode,
```

Add import: `use crate::signal_generator::{SignalType, SourceMode};`

Initialize as `source_mode: SourceMode::Generator` in the constructor.

Add accessor:
```rust
pub fn source_mode(&self) -> SourceMode {
    self.source_mode
}
```

Add generator state fields to `FilePlayer`:
```rust
/// Current generator signal type (GUI state mirror).
gen_signal_type: SignalType,
/// Current generator frequency in Hz.
gen_frequency: f32,
/// Current generator amplitude (0.0-1.0).
gen_amplitude: f32,
```

Initialize: `gen_signal_type: SignalType::Sine, gen_frequency: 440.0, gen_amplitude: 0.5`

**Step 2: Replace `render_source_toggle` with new source mode toggle**

Add new method to `FilePlayer`:

```rust
/// Render the source mode toggle (Generator / File) for the header bar.
pub fn render_source_toggle(&mut self, ui: &mut Ui) {
    let theme = SonidoTheme::get(ui.ctx());

    let gen_active = self.source_mode == SourceMode::Generator;
    let gen_color = if gen_active { theme.colors.green } else { theme.colors.dim };
    if arcade_led_button(ui, "GEN", gen_color, gen_active, &theme).clicked() && !gen_active {
        self.source_mode = SourceMode::Generator;
        let _ = self.transport_tx.send(TransportCommand::SetSourceMode(SourceMode::Generator));
        // Stop file playback when switching
        self.is_playing = false;
        let _ = self.transport_tx.send(TransportCommand::Stop);
    }

    let file_active = self.source_mode == SourceMode::File;
    let file_color = if file_active { theme.colors.amber } else { theme.colors.dim };
    if arcade_led_button(ui, "FILE", file_color, file_active, &theme).clicked() && !file_active {
        self.source_mode = SourceMode::File;
        let _ = self.transport_tx.send(TransportCommand::SetSourceMode(SourceMode::File));
        // Stop generator when switching
        let _ = self.transport_tx.send(TransportCommand::Stop);
    }
}
```

**Step 3: Update `toggle_play_pause` to work for both modes**

Replace the `has_file` guard:

```rust
pub fn toggle_play_pause(&mut self) {
    match self.source_mode {
        SourceMode::Generator => {
            if self.is_playing {
                self.is_playing = false;
                let _ = self.transport_tx.send(TransportCommand::Pause);
            } else {
                self.is_playing = true;
                let _ = self.transport_tx.send(TransportCommand::Play);
            }
        }
        SourceMode::File => {
            if !self.has_file {
                return;
            }
            if self.is_playing {
                self.is_playing = false;
                let _ = self.transport_tx.send(TransportCommand::Pause);
            } else {
                self.is_playing = true;
                let _ = self.transport_tx.send(TransportCommand::Play);
            }
        }
    }
}
```

**Step 4: Add generator controls to `render_compact`**

In `FilePlayer::render_compact()`, add a branch for generator mode before the file UI:

```rust
if self.source_mode == SourceMode::Generator {
    // Play / Pause
    let (play_sym, play_color) = if self.is_playing {
        ("||", theme.colors.amber)
    } else {
        (">", theme.colors.green)
    };
    if arcade_led_button(ui, play_sym, play_color, self.is_playing, &theme).clicked() {
        self.toggle_play_pause();
    }

    // Signal type combo
    let current_label = self.gen_signal_type.label();
    egui::ComboBox::from_id_salt("gen_signal")
        .selected_text(
            egui::RichText::new(current_label)
                .font(egui::FontId::monospace(10.0))
                .color(theme.colors.text_primary),
        )
        .width(90.0)
        .show_ui(ui, |ui| {
            for &t in SignalType::ALL {
                if ui.selectable_label(self.gen_signal_type == t, t.label()).clicked() {
                    self.gen_signal_type = t;
                    let _ = self.transport_tx.send(TransportCommand::SetSignalType(t));
                }
            }
        });

    // Frequency display (for sine, sweep, impulse, saw chord)
    if matches!(self.gen_signal_type, SignalType::Sine | SignalType::SawChord) {
        ui.label(
            egui::RichText::new(format!("{:.0} Hz", self.gen_frequency))
                .font(egui::FontId::monospace(10.0))
                .color(theme.colors.cyan),
        );
    }
    return;
}
```

**Step 5: Update spacebar handler in app.rs**

In `app.rs`, update the spacebar handler (~line 927-933):

```rust
if no_widget_focused && ctx.input(|i| i.key_pressed(egui::Key::Space)) {
    let can_play = match self.file_player.source_mode() {
        SourceMode::Generator => true,
        SourceMode::File => self.file_player.has_file(),
    };
    if can_play {
        self.file_player.toggle_play_pause();
    }
}
```

Add import at top of app.rs: `use crate::signal_generator::SourceMode;`

**Step 6: Update `resync_transport` in FilePlayer**

Replace the `SetFileMode` send with `SetSourceMode`:

```rust
let _ = self.transport_tx.send(TransportCommand::SetSourceMode(self.source_mode));
```

Also resync generator state:
```rust
let _ = self.transport_tx.send(TransportCommand::SetSignalType(self.gen_signal_type));
let _ = self.transport_tx.send(TransportCommand::SetGeneratorFreq(self.gen_frequency));
let _ = self.transport_tx.send(TransportCommand::SetGeneratorAmplitude(self.gen_amplitude));
```

**Step 7: Fix remaining app.rs references**

- Remove the call to `self.file_player.render_source_toggle(ui)` — wait, we kept that method. It just has new internals. No change needed.
- The `self.file_player.use_file_input()` call (~line 759) needs replacing. Check context and replace with `self.file_player.source_mode() == SourceMode::File` or just always show the file player compact view (it now handles both modes internally via `render_compact`).
- The condition at line 929 (`self.file_player.use_file_input() && self.file_player.has_file()`) is replaced by the Step 5 logic above.

**Step 8: Compile, test, wasm check**

Run: `cargo test -p sonido-gui`
Run: `cargo check --target wasm32-unknown-unknown -p sonido-gui`
Expected: All pass.

**Step 9: Commit Tasks 2+3+4 together**

```bash
git add crates/sonido-gui/src/audio_processor.rs crates/sonido-gui/src/file_player.rs \
       crates/sonido-gui/src/app.rs crates/sonido-gui/src/main.rs
git commit -m "feat(gui): replace mic input with signal generator, add source mode toggle"
```

---

### Task 5: Add generator controls to the full file player panel

**Files:**
- Modify: `crates/sonido-gui/src/file_player.rs`

**Step 1: Add generator controls to `FilePlayer::ui()`**

In the `ui()` method, add a generator-mode branch before the file-mode UI:

```rust
pub fn ui(&mut self, ui: &mut Ui) {
    // ... existing file dialog checks ...

    let theme = SonidoTheme::get(ui.ctx());

    if self.source_mode == SourceMode::Generator {
        self.render_generator_panel(ui, &theme);
        return;
    }

    // ... existing file player UI ...
}
```

**Step 2: Implement `render_generator_panel`**

```rust
fn render_generator_panel(&mut self, ui: &mut Ui, theme: &SonidoTheme) {
    ui.horizontal(|ui| {
        // Signal type combo
        egui::ComboBox::from_id_salt("gen_signal_full")
            .selected_text(
                egui::RichText::new(self.gen_signal_type.label())
                    .font(egui::FontId::monospace(11.0))
                    .color(theme.colors.text_primary),
            )
            .width(100.0)
            .show_ui(ui, |ui| {
                for &t in SignalType::ALL {
                    if ui.selectable_label(self.gen_signal_type == t, t.label()).clicked() {
                        self.gen_signal_type = t;
                        let _ = self.transport_tx.send(TransportCommand::SetSignalType(t));
                    }
                }
            });

        ui.add_space(8.0);

        // Frequency control (for applicable types)
        if matches!(
            self.gen_signal_type,
            SignalType::Sine | SignalType::SawChord
        ) {
            ui.label(
                egui::RichText::new("Freq:")
                    .font(egui::FontId::monospace(10.0))
                    .color(theme.colors.text_secondary),
            );
            let mut log_freq = self.gen_frequency.ln();
            let resp = ui.add(
                egui::Slider::new(&mut log_freq, (20.0_f32).ln()..=(20000.0_f32).ln())
                    .show_value(false)
                    .custom_formatter(|v, _| format!("{:.0} Hz", v.exp()))
                    .custom_parser(|s| s.trim_end_matches(" Hz").parse::<f64>().ok().map(|v| v.ln())),
            );
            if resp.changed() {
                self.gen_frequency = log_freq.exp();
                let _ = self
                    .transport_tx
                    .send(TransportCommand::SetGeneratorFreq(self.gen_frequency));
            }
        }

        ui.add_space(8.0);

        // Transport: Play/Pause, Stop, Loop (reuse existing arcade buttons)
        let (play_sym, play_color) = if self.is_playing {
            ("||", theme.colors.amber)
        } else {
            (">", theme.colors.green)
        };
        if arcade_led_button(ui, play_sym, play_color, self.is_playing, theme).clicked() {
            self.toggle_play_pause();
        }

        if arcade_led_button(ui, "[]", theme.colors.red, false, theme).clicked() {
            self.is_playing = false;
            let _ = self.transport_tx.send(TransportCommand::Stop);
        }

        ui.add_space(8.0);

        // Signal info label
        let info = match self.gen_signal_type {
            SignalType::Sine => format!("{:.0} Hz", self.gen_frequency),
            SignalType::Sweep => "20-20k Hz log".to_string(),
            SignalType::WhiteNoise => "Flat spectrum".to_string(),
            SignalType::PinkNoise => "-3 dB/oct".to_string(),
            SignalType::Impulse => "2 Hz clicks".to_string(),
            SignalType::SawChord => format!("{:.0} Hz chord", self.gen_frequency),
        };
        ui.label(
            egui::RichText::new(info)
                .font(egui::FontId::monospace(10.0))
                .color(theme.colors.cyan),
        );
    });
}
```

**Step 3: Test and commit**

Run: `cargo check -p sonido-gui`
Run: `cargo check --target wasm32-unknown-unknown -p sonido-gui`
Expected: Clean.

```bash
git add crates/sonido-gui/src/file_player.rs
git commit -m "feat(gui): add generator controls to file player panel"
```

---

### Task 6: Verify and update docs

**Files:**
- Modify: `docs/GUI.md`
- Modify: `docs/CHANGELOG.md`

**Step 1: Run full verification**

```bash
cargo test -p sonido-gui -p sonido-gui-core
cargo check --target wasm32-unknown-unknown -p sonido-gui
cargo clippy -p sonido-gui -p sonido-gui-core
```

Expected: All pass, no new warnings from our code.

**Step 2: Update GUI.md**

Add signal generator section. Replace any mic input references with generator. Mention source mode toggle (GEN/FILE).

**Step 3: Update CHANGELOG.md**

Add entries under `[Unreleased]`:
- `feat(gui): built-in signal generator — sine, sweep, white/pink noise, impulse, saw chord`
- `feat(gui): generator is default source; spacebar starts playback immediately`
- `remove(gui): mic input removed (will return with proper device selection UI)`
- `fix(gui): sample rate mismatch — effects now initialize at device-detected rate`

**Step 4: Commit**

```bash
git add docs/GUI.md docs/CHANGELOG.md
git commit -m "docs: signal generator, mic removal, sample rate fix"
```

---

## Critical Files Summary

| File | Changes |
|------|---------|
| `crates/sonido-gui/src/signal_generator.rs` | **New** — SignalGenerator, SignalType, SourceMode, 8 tests |
| `crates/sonido-gui/src/audio_processor.rs` | Remove input stream, add SignalGenerator + SourceMode, simplify process_buffer |
| `crates/sonido-gui/src/file_player.rs` | Remove SetFileMode/use_file_input, add generator commands + UI, update toggle_play_pause |
| `crates/sonido-gui/src/app.rs` | Update spacebar handler, remove use_file_input references |
| `crates/sonido-gui/src/main.rs` | Remove --input CLI arg |
| `crates/sonido-gui/src/lib.rs` | Add signal_generator module |
| `docs/GUI.md` | Signal generator docs |
| `docs/CHANGELOG.md` | Changelog entries |
