# GUI Audio Bug Fixes Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fix 4 GUI audio bugs: file_mode desync on startup, CPU explosion from processing bypassed effects, file data lost on audio restart, and file dialog focus issue on Linux.

**Architecture:** Four independent, targeted fixes in the GUI audio pipeline. Bug 1+3 are state synchronization issues in the audio_processor↔file_player boundary. Bug 2 is a graph processing optimization in sonido-core. Bug 4 is a threading change in the file dialog.

**Tech Stack:** Rust, egui, cpal, rfd, crossbeam-channel

---

### Task 1: Fix file_mode desync on startup (Bug 1)

**Files:**
- Modify: `crates/sonido-gui/src/audio_processor.rs:48`
- Test: Manual verification (audio thread state, no unit test harness for cpal)

The GUI's `FilePlayer` defaults `use_file_input: true` but `FilePlayback::file_mode` defaults to `false`. No `SetFileMode` command is sent on startup, so the audio thread reads mic input even though the GUI shows "File".

**Step 1: Change `FilePlayback::file_mode` default to `true`**

In `crates/sonido-gui/src/audio_processor.rs`, change line 48 from:
```rust
            file_mode: false,
```
to:
```rust
            file_mode: true,
```

This matches the GUI default (`FilePlayer::use_file_input: true` at `file_player.rs:90`).

**Step 2: Verify the fix compiles**

Run: `cargo check -p sonido-gui`
Expected: compiles clean

**Step 3: Commit**

```bash
git add crates/sonido-gui/src/audio_processor.rs
git commit -m "fix(gui): sync FilePlayback::file_mode default with GUI (true)"
```

---

### Task 2: Skip processing for fully-bypassed effects (Bug 2)

**Files:**
- Modify: `crates/sonido-core/src/graph/processing.rs:1086-1132`
- Test: `crates/sonido-core/src/graph/processing.rs` (existing test module at line 1469)

When `bypass_fade` is settled at 0.0 (fully bypassed), the graph still runs `process_block_stereo` on every effect. With 19 bypassed effects in debug mode this causes 175-545% CPU. The fix: when bypass is settled, copy input→output directly without calling the effect.

**Step 1: Write the failing test**

Add this test to the `#[cfg(test)] mod tests` block in `crates/sonido-core/src/graph/processing.rs` (after the existing `test_bypass_crossfade_smooth` test around line 2071):

```rust
    #[test]
    fn test_settled_bypass_skips_processing() {
        // When bypass_fade is settled at 0.0, effect should NOT be called.
        static COUNTER: AtomicUsize = AtomicUsize::new(0);

        let effects: Vec<Box<dyn EffectWithParams + Send>> =
            vec![Box::new(CountingEffect { counter: &COUNTER })];
        let mut graph = ProcessingGraph::linear(effects, 48000.0, 64).unwrap();

        let effect_id = NodeId(1);
        graph.set_bypass(effect_id, true);

        // Snap bypass fade to 0.0 immediately (simulates settled state).
        if let Some(Some(node)) = graph.nodes.get_mut(1) {
            node.bypass_fade.snap_to_target();
        }

        COUNTER.store(0, Ordering::SeqCst);

        let left_in = vec![0.5; 64];
        let right_in = vec![0.25; 64];
        let mut left_out = vec![0.0; 64];
        let mut right_out = vec![0.0; 64];

        graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);

        // Effect should NOT have been called.
        assert_eq!(
            COUNTER.load(Ordering::SeqCst),
            0,
            "settled bypass should skip effect processing"
        );

        // Output should be dry signal (input passed through).
        for (i, &s) in left_out.iter().enumerate() {
            assert!(
                (s - 0.5).abs() < 1e-6,
                "settled bypass left[{i}]: expected 0.5, got {s}"
            );
        }
        for (i, &s) in right_out.iter().enumerate() {
            assert!(
                (s - 0.25).abs() < 1e-6,
                "settled bypass right[{i}]: expected 0.25, got {s}"
            );
        }
    }
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p sonido-core test_settled_bypass_skips_processing`
Expected: FAIL — `COUNTER` will be 1 because the current code always processes.

**Step 3: Implement the optimization**

In `crates/sonido-core/src/graph/processing.rs`, replace the `ProcessStep::ProcessEffect` handler (lines 1086-1132) with:

```rust
                ProcessStep::ProcessEffect {
                    node_idx,
                    input_buf,
                    output_buf,
                } => {
                    if let Some(Some(node)) = nodes.get_mut(*node_idx) {
                        let bypass_settled = node.bypassed && node.bypass_fade.is_settled();
                        let bypass_fading = !bypass_settled
                            && (node.bypassed || !node.bypass_fade.is_settled());

                        if bypass_settled {
                            // Fully bypassed and fade complete — copy input to output,
                            // skip effect processing entirely (massive CPU savings).
                            if *input_buf != *output_buf {
                                let (inp, out) = pool.get_ref_and_mut(*input_buf, *output_buf);
                                out.left[..len].copy_from_slice(&inp.left[..len]);
                                out.right[..len].copy_from_slice(&inp.right[..len]);
                            }
                            // If input_buf == output_buf, data is already in place.
                        } else {
                            // Phase 1: Save dry signal before effect processing.
                            if bypass_fading {
                                let src = pool.get(*input_buf);
                                node.bypass_buf.left[..len]
                                    .copy_from_slice(&src.left[..len]);
                                node.bypass_buf.right[..len]
                                    .copy_from_slice(&src.right[..len]);
                            }

                            // Phase 2: Process through effect.
                            if let NodeKind::Effect(ref mut effect) = node.kind {
                                if *input_buf == *output_buf {
                                    let buf = pool.get_mut(*input_buf);
                                    effect.process_block_stereo_inplace(
                                        &mut buf.left[..len],
                                        &mut buf.right[..len],
                                    );
                                } else {
                                    let (inp, out) =
                                        pool.get_ref_and_mut(*input_buf, *output_buf);
                                    effect.process_block_stereo(
                                        &inp.left[..len],
                                        &inp.right[..len],
                                        &mut out.left[..len],
                                        &mut out.right[..len],
                                    );
                                }
                            }

                            // Phase 3: Crossfade between dry and wet during fade.
                            if bypass_fading {
                                let out = pool.get_mut(*output_buf);
                                for i in 0..len {
                                    let fade = node.bypass_fade.advance();
                                    out.left[i] = wet_dry_mix(
                                        node.bypass_buf.left[i],
                                        out.left[i],
                                        fade,
                                    );
                                    out.right[i] = wet_dry_mix(
                                        node.bypass_buf.right[i],
                                        out.right[i],
                                        fade,
                                    );
                                }
                            }
                        }
                    }
                }
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p sonido-core test_settled_bypass_skips_processing test_bypass_outputs_dry_signal test_bypass_crossfade_smooth test_bypass test_schedule_swap_single_execution`
Expected: ALL PASS

**Step 5: Run full sonido-core tests to check for regressions**

Run: `cargo test -p sonido-core`
Expected: ALL PASS

**Step 6: Commit**

```bash
git add crates/sonido-core/src/graph/processing.rs
git commit -m "perf(graph): skip effect processing when bypass is fully settled

When bypass_fade is settled at 0.0, copy input→output directly without
calling the effect's process_block_stereo. With 19 bypassed effects this
eliminates ~95% of audio thread CPU in the default GUI configuration."
```

---

### Task 3: Re-sync file data and file_mode on audio restart (Bug 3)

**Files:**
- Modify: `crates/sonido-gui/src/file_player.rs:56-98` (add `resync_transport` method)
- Modify: `crates/sonido-gui/src/app.rs:177-219` (call resync after start_audio)

When audio restarts (buffer size change, preset load), the `AudioProcessor` is recreated with a fresh `FilePlayback` — file data and file_mode are lost. The GUI's `FilePlayer` still thinks a file is loaded, but the audio thread has no data.

**Step 1: Add `resync_transport` method to `FilePlayer`**

In `crates/sonido-gui/src/file_player.rs`, add this method after `toggle_play_pause` (after line 134):

```rust
    /// Re-send current file_mode and file data to the audio thread.
    ///
    /// Called after audio stream restart (buffer size change, preset load)
    /// because the `AudioProcessor` is recreated with a fresh `FilePlayback`.
    pub fn resync_transport(&mut self) {
        // Always sync the current file_mode
        let _ = self
            .transport_tx
            .send(TransportCommand::SetFileMode(self.use_file_input));

        // Re-send file data and restore playback state
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(ref path) = self.file_path {
            if self.has_file {
                if let Ok((samples, _spec)) = read_wav_stereo(path) {
                    let _ = self.transport_tx.send(TransportCommand::LoadFile {
                        left: samples.left,
                        right: samples.right,
                        sample_rate: self.sample_rate,
                    });
                    let _ = self
                        .transport_tx
                        .send(TransportCommand::SetLoop(self.is_looping));
                    if self.position_secs > 0.0 {
                        let _ = self
                            .transport_tx
                            .send(TransportCommand::Seek(self.position_secs));
                    }
                    if self.is_playing {
                        let _ = self.transport_tx.send(TransportCommand::Play);
                    }
                }
            }
        }
    }
```

**Step 2: Call `resync_transport` after every `start_audio()`**

In `crates/sonido-gui/src/app.rs`, there are 4 places where `start_audio()` is called. After each successful call, add `self.file_player.resync_transport();`. The locations are:

**(a) In `SonidoApp::new()` (line 166):**

Change:
```rust
        // Start audio
        if let Err(e) = app.start_audio() {
            app.audio_error = Some(e);
        }
```
To:
```rust
        // Start audio
        if let Err(e) = app.start_audio() {
            app.audio_error = Some(e);
        }
        app.file_player.resync_transport();
```

**(b) In `set_buffer_size()` (around line 269):**

Change:
```rust
        if let Err(e) = self.start_audio() {
            tracing::error!(
                buffer_size = clamped_size,
                error = %e,
                "failed to restart audio"
            );
        }
```
To:
```rust
        if let Err(e) = self.start_audio() {
            tracing::error!(
                buffer_size = clamped_size,
                error = %e,
                "failed to restart audio"
            );
        }
        self.file_player.resync_transport();
```

**(c) In `apply_preset()` (around line 506):**

Change:
```rust
        // 4. Restart audio with the new chain
        if let Err(e) = self.start_audio() {
            self.audio_error = Some(e);
        }
```
To:
```rust
        // 4. Restart audio with the new chain
        if let Err(e) = self.start_audio() {
            self.audio_error = Some(e);
        }
        self.file_player.resync_transport();
```

**(d) In the retry block in `render_header()` (around line 463):**

Change:
```rust
                if retry {
                    self.stop_audio();
                    match self.start_audio() {
                        Ok(()) => self.audio_error = None,
                        Err(e) => self.audio_error = Some(e),
                    }
                }
```
To:
```rust
                if retry {
                    self.stop_audio();
                    match self.start_audio() {
                        Ok(()) => {
                            self.audio_error = None;
                            self.file_player.resync_transport();
                        }
                        Err(e) => self.audio_error = Some(e),
                    }
                }
```

**Step 3: Verify compilation**

Run: `cargo check -p sonido-gui`
Expected: compiles clean

**Step 4: Commit**

```bash
git add crates/sonido-gui/src/file_player.rs crates/sonido-gui/src/app.rs
git commit -m "fix(gui): resync file data and file_mode after audio restart

AudioProcessor is recreated on buffer size change / preset load, losing
file playback data. FilePlayer::resync_transport() re-sends SetFileMode,
LoadFile, loop state, seek position, and play state after every
start_audio() call."
```

---

### Task 4: Non-blocking file dialog (Bug 4)

**Files:**
- Modify: `crates/sonido-gui/src/file_player.rs:56-73` (add channel fields)
- Modify: `crates/sonido-gui/src/file_player.rs:77-98` (init channel in constructor)
- Modify: `crates/sonido-gui/src/file_player.rs:272-288` (spawn dialog on thread)
- Modify: `crates/sonido-gui/src/file_player.rs:272` (poll for result in `ui()`)

`rfd::FileDialog::pick_file()` blocks the GUI thread, preventing egui from processing events. On Linux, this causes the dialog to appear behind the main window.

Fix: spawn the dialog on a background thread and receive the result via a channel, matching the existing wasm pattern.

**Step 1: Add native file result channel to `FilePlayer`**

In `crates/sonido-gui/src/file_player.rs`, add a new field to `FilePlayer` (after line 67, before the wasm fields):

```rust
    /// Receives file path from background file dialog (native only).
    #[cfg(not(target_arch = "wasm32"))]
    native_file_rx: crossbeam_channel::Receiver<PathBuf>,
    #[cfg(not(target_arch = "wasm32"))]
    native_file_tx: Sender<PathBuf>,
```

**Step 2: Initialize the channel in `FilePlayer::new()`**

In the constructor, add before `Self {`:

```rust
        #[cfg(not(target_arch = "wasm32"))]
        let (native_file_tx, native_file_rx) = crossbeam_channel::unbounded();
```

And add the fields inside the `Self { ... }` block:

```rust
            #[cfg(not(target_arch = "wasm32"))]
            native_file_rx,
            #[cfg(not(target_arch = "wasm32"))]
            native_file_tx,
```

**Step 3: Poll for file dialog result in `ui()`**

At the top of the `ui()` method (line 272, inside the method before `ui.horizontal`), add native polling alongside the existing wasm polling:

```rust
        // Check for completed file dialog (native)
        #[cfg(not(target_arch = "wasm32"))]
        if let Ok(path) = self.native_file_rx.try_recv() {
            self.load_file(path);
        }
```

**Step 4: Replace blocking dialog with threaded version**

Replace the native file dialog block (lines 281-288):

```rust
            #[cfg(not(target_arch = "wasm32"))]
            if ui.button("Open").clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("WAV", &["wav"])
                    .pick_file()
            {
                self.load_file(path);
            }
```

With:

```rust
            #[cfg(not(target_arch = "wasm32"))]
            if ui.button("Open").clicked() {
                let tx = self.native_file_tx.clone();
                std::thread::spawn(move || {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("WAV", &["wav"])
                        .pick_file()
                    {
                        let _ = tx.send(path);
                    }
                });
            }
```

**Step 5: Verify compilation**

Run: `cargo check -p sonido-gui`
Expected: compiles clean

**Step 6: Commit**

```bash
git add crates/sonido-gui/src/file_player.rs
git commit -m "fix(gui): non-blocking file dialog on native (fixes focus issue)

rfd::FileDialog::pick_file() blocked the GUI thread, preventing egui
from processing events. On Linux this caused the dialog to appear behind
the main window. Now spawned on a background thread with results sent
via crossbeam channel, matching the existing wasm async pattern."
```

---

### Task 5: Verify all fixes together

**Step 1: Run workspace check**

Run: `cargo check --workspace`
Expected: compiles clean

**Step 2: Run all tests**

Run: `cargo test -p sonido-core -p sonido-effects -p sonido-gui`
Expected: ALL PASS

**Step 3: Run golden regression tests**

Run: `cargo test --test regression -p sonido-effects`
Expected: ALL PASS (graph processing change shouldn't affect golden files — bypassed effects weren't in the test chain)

**Step 4: Manual smoke test**

Run: `cargo run -p sonido-gui`

Verify:
1. On startup with "File" mode: input meter shows silence (not red/mic feedback)
2. Switch to Mic: meters show mic signal, CPU is reasonable
3. Load a WAV file, switch to File, play: audio plays through
4. Change buffer size: file playback survives the restart
5. Click "Open" button: file dialog appears in front of main window
6. CPU meter: should be dramatically lower with all effects bypassed

**Step 5: Final commit (if any touchups needed)**

Only if manual testing reveals issues requiring adjustment.
