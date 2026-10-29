# I/O Wall-Bars + Graph-UX Pass — Design

**Date:** 2026-06-30
**Status:** Implemented
**Branch:** `feat/io-wall-bars`

> **Note (2026-10-22):** Implemented as described. Status was "Approved, in implementation." In code: `GraphView::last_viewport`, `SonidoViewer::draw_background` capture, `pin_io_nodes()`, zoom bounds `min_scale = 0.5` / `max_scale = 1.5`, and `append_before_output` are in `crates/sonido-gui/src/graph_view.rs`; the bars are `render_io_bar` in `crates/sonido-gui/src/app.rs` (a vertical `LevelMeter` with the gain or master `Knob` at its foot, width `IO_BAR_WIDTH`). On Desktop/Tablet the macros have since moved to the header, so focus mode hides the bottom morph band and the Info View.

This pass reworks the graph editor's input/output into full-height wall bars
fixed to the canvas edges. Each bar is the in/out meter and the wire anchor, and
carries its gain/master knob at the foot. The bars replace the round pin-dots
and the separate side I/O strips, on desktop and phone alike. The pass also
includes three smaller graph-UX fixes found in the phone audit.

## Key enabling fact (and the one forced deviation)

`egui-snarl 0.7.1` keeps its pan/zoom state (`SnarlState`) private, but the
`SnarlViewer::draw_background(viewport: &Viewport, …)` hook receives the
publicly-exported `Viewport`, which exposes `scale`, `offset`, `rect`, and
`screen_pos_to_graph()` / `graph_pos_to_screen()`. Capturing that `Viewport`
each frame lets us position the I/O nodes so their pins land on the screen walls
regardless of pan/zoom.

Because the `Viewport` is read-only (we cannot write the pan offset back), we
cannot spring back a mid-canvas pan. Instead:

- Zoom is bounded via `SnarlStyle::min_scale`/`max_scale` (public).
- I/O is fixed to the walls, so it can never leave the viewport. That fixes the
  actual complaint, which was "I/O scrolled off-screen".
- Mid-canvas pan stays unrestricted; snarl's double-click-to-center brings the
  content back.

For the stated problem this is a stronger guarantee than the clamp discussed
originally (I/O cannot leave the viewport at all), at the cost of not springing
the middle content back automatically. This is accepted as a forced constraint.

## Components

### 1. Viewport capture (`graph_view.rs`)
- `GraphView` gains `last_viewport: Option<Viewport>`.
- `SonidoViewer` gains `captured_viewport: &mut Option<Viewport>` and overrides
  `draw_background` to stash `*viewport` then call the default background draw.
- After `show()`, `last_viewport` holds this frame's transform.

### 2. Weld I/O pins to the walls (`graph_view.rs`)
- `pin_io_nodes()` is rewritten: using `last_viewport`, set the Input node's
  graph position to `screen_pos_to_graph(left-wall inner edge, vertical center)`
  and the Output node to the right-wall point. The node frame is already
  zero-margin, so only the pin shows, and it sits on the bar. The one-frame lag
  while panning (positions feed the next render) is imperceptible.
- It falls back to today's bounding-box anchoring when no viewport is captured
  yet (first frame).

### 3. Wall bars (`app.rs` + reuse `LevelMeter`)
- Drawn as screen-space overlays at the canvas rect's left/right edges, full
  canvas height. Each bar is a vertical `LevelMeter` (input/output peak+rms) with
  the gain (left) / master (right) `Knob` docked at its foot.
- The side I/O strips (`render_io_strip`) and the phone levels row
  (`render_phone_levels`) are removed; the central layout becomes a
  full-width canvas with the two overlay bars. Desktop and phone now share this
  layout.
- Phone: thinner bars (~16px) + a compact foot knob so the canvas keeps enough
  width.

### 4. Zoom bounds (`graph_view.rs`)
- `SnarlStyle::min_scale`/`max_scale` set to a tight range so nodes can't be
  zoomed until they are illegible or vanishingly small. This addresses the
  "weird resizing" report.

### Bundled fixes
- **Deselect-race (Duplicate/Remove):** `show()`'s empty-space deselect
  (`graph_view.rs`) is guarded so a press over a foreground overlay area
  (the touch action bar) no longer deselects the node before the button's action
  runs.
- **Add-appends-last:** `add_effect_node` calls `append_before_output` instead
  of `splice_at_nearest`, so the new effect always becomes the sole node feeding
  the Output (every node that currently feeds the Output is rewired to feed the
  new node). Right-click-on-wire keeps its positional splice.

### Focus mode reconciliation
- The earlier focus toggle hid the I/O strips; with strips gone, focus mode now
  hides the morph band + Info View and keeps the thin wall bars (they are the
  I/O and cost little width).

## Out of scope
- Mobile audio crackle: a separate profiling track (includes the meter-repaint
  load already flagged).

## Testing
- Pure-function unit tests for the wall→graph pin mapping (given a `Viewport`,
  Input maps to the left edge, Output to the right) and the zoom-bound clamp.
- `append_before_output` merge behavior (feeders → new node → Output) gets an
  explicit test.
- Existing graph/compile/session tests still pass; native + wasm32 compile;
  clippy `-D warnings`; `cargo fmt --all --check` (now hook-enforced).
- Visual correctness (bar alignment, pins fixed to the walls, phone fit)
  requires the user to run the GUI, so it is flagged here and not claimed as
  verified.
