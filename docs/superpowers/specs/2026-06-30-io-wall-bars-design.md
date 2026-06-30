# I/O Wall-Bars + Graph-UX Pass — Design

**Date:** 2026-06-30
**Status:** Approved, in implementation
**Branch:** `feat/io-wall-bars`

Reworks the graph editor's input/output so they are full-height **wall bars**
welded to the canvas edges — each bar is the in/out **meter**, the **wire
anchor**, and carries its **gain/master knob** at the foot. Replaces the round
pin-dots and the separate side I/O strips, on desktop and phone alike. Bundles
three smaller graph-UX fixes discovered in the phone audit.

## Key enabling fact (and the one forced deviation)

`egui-snarl 0.7.1` keeps its pan/zoom state (`SnarlState`) **private**, but the
`SnarlViewer::draw_background(viewport: &Viewport, …)` hook receives the
publicly-exported `Viewport`, which exposes `scale`, `offset`, `rect`, and
`screen_pos_to_graph()` / `graph_pos_to_screen()`. Capturing that `Viewport`
each frame lets us position the I/O nodes so their pins land on the screen walls
regardless of pan/zoom.

Because the `Viewport` is **read-only** (we cannot write the pan offset back), we
**cannot** spring-back mid-canvas pan. Instead:

- **Zoom** is bounded via `SnarlStyle::min_scale`/`max_scale` (public).
- **I/O is welded to the walls** so it can never leave the viewport — which is
  the actual "I/O scrolled off-screen" complaint.
- Mid-canvas **pan stays free**; snarl's double-click-to-center recovers it.

This is a stronger guarantee than the originally-discussed clamp for the stated
problem (I/O literally cannot leave), at the cost of not auto-springing the
middle content. Accepted as a forced constraint.

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
  zero-margin, so only the pin shows — sitting on the bar. One-frame lag while
  panning (positions feed the next render) is imperceptible.
- Falls back to today's bounding-box anchoring when no viewport is captured yet
  (first frame).

### 3. Wall bars (`app.rs` + reuse `LevelMeter`)
- Drawn as screen-space overlays at the canvas rect's left/right edges, full
  canvas height. Each bar = a vertical `LevelMeter` (input/output peak+rms) with
  the gain (left) / master (right) `Knob` docked at its foot.
- The side I/O strips (`render_io_strip`) and the phone levels row
  (`render_phone_levels`) are **removed**; the central layout becomes a
  full-width canvas with the two overlay bars. Unifies desktop and phone.
- Phone: thinner bars (~16px) + a compact foot knob so the canvas isn't crushed.

### 4. Zoom bounds (`graph_view.rs`)
- `SnarlStyle::min_scale`/`max_scale` set to a tight range so nodes can't be
  zoomed into illegibility or vanishing-point — addressing the "weird resizing"
  report.

### Bundled fixes
- **Deselect-race (Duplicate/Remove):** `show()`'s empty-space deselect
  (`graph_view.rs`) is guarded so a press over a foreground overlay area
  (the touch action bar) no longer deselects the node before the button acts.
- **Add-appends-last:** `add_effect_node` calls `append_before_output` instead
  of `splice_at_nearest`, so the new effect always becomes the sole node feeding
  the Output (absorbing every current Output-feeder). Right-click-on-wire keeps
  its positional splice.

### Focus mode reconciliation
- The earlier focus toggle hid the I/O strips; with strips gone, focus mode now
  hides the morph band + Info View and keeps the thin wall bars (they are the
  I/O and cost little width).

## Out of scope
- Mobile audio crackle — separate profiling track (includes the meter-repaint
  load already flagged).

## Testing
- Pure-function unit tests for the wall→graph pin mapping (given a `Viewport`,
  Input maps to the left edge, Output to the right) and the zoom-bound clamp.
- `append_before_output` merge behavior (feeders → new node → Output) gets an
  explicit test.
- Existing graph/compile/session tests stay green; native + wasm32 compile;
  clippy `-D warnings`; `cargo fmt --all --check` (now hook-enforced).
- Visual correctness (bar alignment, welded pins, phone fit) requires the user
  to run the GUI — flagged, not claimable from here.
