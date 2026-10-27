# Ableton-Parity GUI Pass — Design

**Date:** 2026-06-30
**Status:** Implemented (Info View = selection-driven v1; per-node/widget hover
and desktop chrome-summon drawer are documented as follow-up work)
**Branch:** `feat/gui-ableton-parity`

> **Note (2026-10-22):** As built, the Info View is in `crates/sonido-gui/src/info_view.rs` (not `sonido-gui-core`), and `InfoPayload` is `{ title, description, params, shortcuts }`: the title and prose come from the registry, the live `(name, value)` pairs from `ParamDescriptor` / `ParamBridge`, not from the `Accessible` trait. It is selection-driven only (no per-widget hover target). Focus mode is `chrome_collapsed` in `app.rs`, toggled by a header button or `F`. The I/O strips this spec edits (`render_io_strip`, `render_phone_levels`) were later replaced by wall bars, see `2026-06-30-io-wall-bars-design.md`. The meter work (`MeterState`, `db_to_norm`, `smooth`, `DB_MARKS`) matches `crates/sonido-gui-core/src/widgets/meter.rs`.

Three GUI features that bring the hosted WASM GUI closer to Ableton Live's
metering, layout flexibility, and contextual help, plus one bug fixed along the
way. The build order is Meters, then Info View, then Layout, because each builds
on the one before.

## Bug already fixed (precursor)

The effect-palette search field took keyboard focus again on every frame in
which nothing else had focus (`app.rs`, palette sheet). On touch tiers this
opened the soft keyboard as soon as a thumb touched the list to scroll it. The
fix limits auto-focus to non-touch breakpoints (`!self.breakpoint.is_compact()`).
Touch users tap the field to type, and scrolling the list no longer opens the
keyboard.

## Common foundation

Every feature needs state that persists between frames in an immediate-mode UI:
peak-hold, ballistics, the held max-dB number, and "what is the Info View
currently showing." Today the meter is stateless, and the one piece of state
that exists (`clip_latched: [bool; 2]`) is passed through the caller by hand.

The design moves this into egui per-widget-id memory (`ui.data_mut` keyed by
the widget's `Response::id`). Any meter or widget anywhere gets the behavior
without extra code, and the `clip_latched` array that callers passed by hand is
removed (a structural fix).

## 1. Meters (`sonido-gui-core/src/widgets/meter.rs`)

`LevelMeter` is already "styled after Ableton Live's channel meters." This change
makes it behave like one.

- **`MeterState` in egui memory**, keyed by widget id:
  `{ smoothed_rms, peak_hold, hold_age, max_peak_db }`. Updated each frame from
  `ui.input(|i| i.stable_dt)`. It calls `request_repaint` while the values
  settle, so the meter animates.
- **Ballistics**: the RMS bar gets fast-attack / slow-release exponential
  smoothing (attack τ ≈ 10 ms, release τ ≈ 300 ms) instead of the raw per-frame
  `rms`. A pure `smooth(current, target, dt, tau)` helper does this and is
  unit-tested.
- **Peak-hold + fall**: track the max peak; hold ≈ 1.5 s, then fall at
  ≈ 12 dB/s. Drawn as a bright cap line (`text_primary`).
- **Dual-color peak/RMS**: fill `0→RMS` in the level color
  (`meter_segment_color(rms)`), then `RMS→peak` in a lighter shade of that color
  (the transient/headroom band). The meter then shows the average level, the
  transient peak and the crest-factor gap between them. The cap line marks the
  held peak.
- **Numeric dB readout**: the held `max_peak_db` is printed in a small header
  strip at the top of the meter and turns red once it exceeds 0 dBFS. Clicking
  the meter resets it. The readout replaces the separate external `CLIP`
  button + latch array, which are removed.
- **Warped scale**: replace the linear-amplitude `DB_MARKS` (which leaves little
  room for the low end: −6 dB sat at 50 % height) with a scale that is linear in
  dB from −60 to 0 dB. One `db_to_norm(db)` maps fill, peak, and ticks
  consistently. −6 dB now sits at 90 % height, as on DAW meters. Marks: 0, −6,
  −12, −18, −24, −36, −48. The floor and curve are tunable constants.
- **Horizontal variant**: gets the same scale, dual-color peak/RMS, hold and
  readout. It was a degraded fallback with no scale, no clip and no hold.
- **Out of scope:** `GainReductionMeter` (amber segmented, different display)
  stays as is.
- **Tests:** `db_to_norm`, ballistics `smooth`, and peak-fall are pure functions,
  so they are unit-tested. Existing meter tests updated (drop `clip_latch`
  builder test).

### Caller changes (`app.rs`)

- `render_io_strip` and `render_phone_levels`: drop the external `CLIP` button
  and the `clip_latched[..]` reads/writes, because the meter handles clip
  display + reset itself.
- Remove the `clip_latched: [bool; 2]` field and its initializer.

## 2. Info View (`sonido-gui-core` + `app.rs`)

Ableton's Info View is a single static-text box. This design goes further: it
combines two description sources the project already has and adds live values.

- **`InfoPayload { name, value, description, shortcuts }`** sourced from the
  existing `Accessible` trait (live `name` + formatted `value` + unit) and
  the registry `EffectDescriptor::description`. AccessKit will read the same
  source, so the visible panel and the screen reader always show the same help
  text (chosen extras: live value + shared accessibility source; no rich
  content).
- **Routing**: a frame-scoped "info target". A hovered widget sets it, and it
  falls back to the currently selected node. The panel renders the target at the
  end of the frame. A helper associates a `Response` with its payload when
  hovered.
- **Shortcuts**: a small static context→shortcuts map, seeded from the
  keyboard-nav handler.
- **Placement**: a collapsible chrome panel, docked bottom-left on
  Desktop/Tablet (where Ableton puts it), in the drawer on Phone. Plain text +
  live value + shortcuts only.

## 3. Full-width-on-demand layout (`app.rs`)

The layout is "full-width on demand," not "full-width forced": chrome stays
dockable but collapses to give the canvas full width when the user asks for it.

- A focus toggle (`chrome_collapsed`: button + keyboard shortcut) collapses
  the macro/morph band, I/O strips, and Info View so the canvas spans full
  width, as in the approved mockup.
- **Reuse:** the Phone `Drawer`/FAB code already built generalizes to all
  breakpoints as the "summon collapsed chrome" mechanism. Most of the work is
  removing an existing Phone-only restriction so it applies at all sizes, rather
  than building new layout.
- Defaults: docked on Desktop/Tablet, hero on Phone (unchanged). The native
  1000×700 window still opens docked.

## Testing strategy

- Meters: pure-function unit tests (`db_to_norm`, `smooth`, peak-fall); existing
  widget tests updated.
- Info View: `InfoPayload` assembly from a mock `Accessible` + registry entry.
- Layout: `chrome_collapsed` toggle state and breakpoint interaction.
- Whole crate compiles (`cargo check -p sonido-gui`) and `cargo test` passes at
  each feature boundary; manual WASM check at the end.
