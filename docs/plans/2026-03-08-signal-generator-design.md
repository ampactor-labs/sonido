# Signal Generator & Mic Removal

**Date:** 2026-03-08
**Status:** Approved

## Problem

Sonido opens silent with no sound source loaded. Users must load a WAV file or enable mic input before hearing anything through the effects. The mic input auto-selects the laptop mic with no device picker, causing instant feedback loops on most machines.

## Design

### Source Model

Two source modes, selectable in the UI:

| Mode | Description | Default |
|------|-------------|---------|
| **Generator** | Built-in test tone synthesis | Yes (startup) |
| **File** | WAV file player (existing) | No |

Mic input is removed entirely until a proper audio device selection UI is implemented.

### Startup Behavior

App opens with Generator selected, sine 440 Hz loaded, **transport paused**. Spacebar starts playback — same play/pause behavior as file mode. User hears sound immediately on first spacebar press.

### Signal Types

| Signal | Algorithm | Controls | Use Case |
|--------|-----------|----------|----------|
| Sine | `sin(2pi * f * t)` | Frequency (20-20kHz, default 440 Hz) | Clean fundamental, distortion character |
| Sweep | Linear or log freq ramp | Start/end Hz, duration (1-30s), loop | Filter shapes, resonance peaks |
| White noise | `fastrand` uniform → centered | — | EQ curves, gate thresholds |
| Pink noise | Voss-McCartney (3-stage) | — | Perceptually flat broadband |
| Impulse train | 1.0 at interval, 0.0 between | Rate in Hz (1-20, default 2) | Reverb tails, delay times |
| Sawtooth chord | PolyBLEP saw, root + 3rd + 5th | Root frequency (default 220 Hz) | Harmonically rich, aliasing test |

All signals are stereo (identical L/R except where noted). Generated at the device sample rate.

### Architecture

#### `SignalGenerator` (new struct, audio thread)

Lives alongside `FilePlayback` in `AudioProcessor`. Generates samples per-block via `fn generate(&mut self, left: &mut [f32], right: &mut [f32])`.

```
AudioProcessor
├── file_pb: FilePlayback      (existing)
├── signal_gen: SignalGenerator (new)
└── source_mode: SourceMode    (new enum: Generator | File)
```

The `process_buffer()` input selection becomes:

```
match source_mode {
    SourceMode::Generator => signal_gen.generate(&mut raw_left, &mut raw_right),
    SourceMode::File => { /* existing file_pb.next_frame() loop */ },
}
```

#### `SignalGenerator` struct

```rust
pub(crate) struct SignalGenerator {
    signal_type: SignalType,
    phase: f64,           // continuous phase accumulator
    frequency: f32,       // primary frequency (Hz)
    amplitude: f32,       // output amplitude (0.0-1.0)
    sample_rate: f32,
    playing: bool,
    // Sweep state
    sweep_start: f32,     // Hz
    sweep_end: f32,       // Hz
    sweep_duration: f32,  // seconds
    sweep_position: f64,  // 0.0-1.0 progress
    sweep_loop: bool,
    // Impulse state
    impulse_rate: f32,    // Hz
    impulse_counter: usize,
    // Pink noise state (Voss-McCartney)
    pink_rows: [f32; 16],
    pink_running_sum: f32,
    pink_index: u32,
    // Saw chord
    saw_phases: [f64; 3], // root, major 3rd, perfect 5th
}
```

#### Transport Commands (extended)

Add variants to `TransportCommand`:

```rust
/// Set the active source mode (generator or file).
SetSourceMode(SourceMode),
/// Change signal generator type.
SetSignalType(SignalType),
/// Set generator frequency in Hz.
SetGeneratorFreq(f32),
/// Set generator amplitude (0.0-1.0).
SetGeneratorAmplitude(f32),
/// Set sweep parameters.
SetSweepParams { start_hz: f32, end_hz: f32, duration_secs: f32, looping: bool },
/// Set impulse train rate in Hz.
SetImpulseRate(f32),
```

Play/Pause/Stop commands apply to both file and generator — they control `FilePlayback.playing` and `SignalGenerator.playing` respectively, based on current source mode.

#### GUI (FilePlayer panel updates)

The existing `FilePlayer` panel gains a source mode toggle at the top:

```
[Generator ▾] [File ▾]     ← two-button toggle (highlight active)
─────────────────────────
Generator mode:             File mode:
  Signal: [Sine ▾]           [Load File]
  Freq:   [====|===] 440 Hz  piano.wav (0:07)
  Amp:    [====|===] -6 dB   [scrubber bar]
  ▶ ⏸ ⏹  [Loop]             ▶ ⏸ ⏹  [Loop]
```

Transport controls (play/pause/stop/loop) are shared — same buttons, same spacebar shortcut. The panel content above changes based on source mode.

### Mic Input Removal

#### What gets removed:
- `input_device` / `input_stream` creation in `build_audio_streams()`
- `input_rx: Receiver<f32>` from `AudioProcessor`
- `in_ch` field from `AudioProcessor`
- Mic-related input draining in `process_buffer()` (the `else if self.in_ch >= 2` branches)
- `TransportCommand::SetFileMode` variant (no longer needed — replaced by `SetSourceMode`)
- `file_mode` field from `FilePlayback`
- `--input` CLI arg from `main.rs`

#### What stays:
- Input gain knob (applies to all sources including generator)
- Input metering (measures generator/file output before effects)

### Wasm Considerations

Signal generator works identically on wasm — no file I/O or device access needed. `fastrand` crate already in workspace (used by tests). PolyBLEP oscillator from `sonido-synth` is no_std compatible but synth crate isn't currently a gui dependency — use simple inline `sin()`/saw math instead to avoid adding the dependency.

### Files Changed

| File | Change |
|------|--------|
| `crates/sonido-gui/src/signal_generator.rs` | **New** — `SignalGenerator`, `SignalType`, `SourceMode` |
| `crates/sonido-gui/src/audio_processor.rs` | Add `SignalGenerator` + `SourceMode` to `AudioProcessor`, remove input stream, simplify `process_buffer()` |
| `crates/sonido-gui/src/file_player.rs` | Add source mode toggle UI, remove `SetFileMode`, add generator control commands |
| `crates/sonido-gui/src/app.rs` | Remove `--input` handling, default source = Generator |
| `crates/sonido-gui/src/main.rs` | Remove `--input` CLI arg |
| `crates/sonido-gui/src/lib.rs` | Add `pub mod signal_generator` |
| `crates/sonido-gui/Cargo.toml` | Add `fastrand` dependency |
