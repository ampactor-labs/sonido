# Ableton-Parity GUI Pass — Design

**Date:** 2026-06-30
**Status:** Implemented (Info View = selection-driven v1; per-node/widget hover
and desktop chrome-summon drawer are the documented fast-follows)
**Branch:** `feat/gui-ableton-parity`

Three GUI features that bring the hosted WASM GUI closer to Ableton Live's
metering, layout flexibility, and contextual help — plus one bug already fixed
along the way. Built in order: **Meters → Info View → Layout**, because each
composes onto the prior.

## Bug already fixed (precursor)

The effect-palette search field re-grabbed focus every frame nothing else was
focused (`app.rs`, palette sheet). On touch tiers this popped the soft keyboard
the instant a thumb rested on the list to scroll. Fixed by gating auto-focus to
non-touch breakpoints (`!self.breakpoint.is_compact()`). Touch users tap the
field to type; the keyboard no longer ambushes a scroll.

## Common foundation

Every feature needs **state that survives between frames** in an immediate-mode
UI: peak-hold, ballistics, the held max-dB number, and "what is the Info View
currently showing." Today the meter is stateless and the one bit of state that
exists (`clip_latched: [bool; 2]`) is hand-threaded through the caller.

The design moves this into **egui per-widget-id memory** (`ui.data_mut` keyed by
the widget's `Response::id`). Any meter or widget anywhere gets the behavior for
free, and the hand-threaded `clip_latched` array is retired (structural fix, not
decoration).

## 1. Meters (`sonido-gui-core/src/widgets/meter.rs`)

`LevelMeter` is already "styled after Ableton Live's channel meters." This makes
it *behave* like one.

- **`MeterState` in egui memory**, keyed by widget id:
  `{ smoothed_rms, peak_hold, hold_age, max_peak_db }`. Updated each frame from
  `ui.input(|i| i.stable_dt)`. `request_repaint` while settling so it animates.
- **Ballistics** — RMS bar gets fast-attack / slow-release exponential
  smoothing (attack τ ≈ 10 ms, release τ ≈ 300 ms) instead of the raw per-frame
  `rms`. Pure `smooth(current, target, dt, tau)` helper, unit-tested.
- **Peak-hold + fall** — track the max peak; hold ≈ 1.5 s, then fall at
  ≈ 12 dB/s. Drawn as a bright cap line (`text_primary`).
- **Dual-color peak/RMS** — fill `0→RMS` in the level color
  (`meter_segment_color(rms)`), then `RMS→peak` in that color *lightened* (the
  transient/headroom band). At a glance: average level + transient peak + the
  crest-factor gap between them. Cap line marks the held peak.
- **Numeric dB readout** — held `max_peak_db` printed in a small header strip at
  the top of the meter; red once it exceeds 0 dBFS. Clicking the meter resets it.
  This folds in (and retires) the separate external `CLIP` button + latch array.
- **Warped scale** — replace the linear-amplitude `DB_MARKS` (which crushes the
  low end: −6 dB sat at 50 % height) with **linear-in-dB over −60 → 0 dB**. One
  `db_to_norm(db)` maps fill, peak, *and* ticks consistently. −6 dB now sits at
  90 % height — the DAW-like feel. Marks: 0, −6, −12, −18, −24, −36, −48.
  Floor / curve are constants, tunable.
- **Horizontal variant reaches parity** (was a degraded fallback: no scale, no
  clip, no hold) — same scale, dual-color, hold, readout.
- **Out of scope:** `GainReductionMeter` (amber segmented, different display)
  stays as-is.
- **Tests:** `db_to_norm`, ballistics `smooth`, and peak-fall are pure functions
  → unit-tested. Existing meter tests updated (drop `clip_latch` builder test).

### Caller changes (`app.rs`)

- `render_io_strip` and `render_phone_levels`: drop the external `CLIP` button
  and the `clip_latched[..]` reads/writes — the meter owns clip display + reset.
- Remove the `clip_latched: [bool; 2]` field and its initializer.

## 2. Info View (`sonido-gui-core` + `app.rs`)

Ableton's Info View is a single static-text box. This beats it by unifying two
description sources the project *already has* and adding live values.

- **`InfoPayload { name, value, description, shortcuts }`** sourced from the
  **existing** `Accessible` trait (live `name` + formatted `value` + unit) and
  the registry `EffectDescriptor::description`. This is the *same* source that
  AccessKit will consume, so help text never drifts between the visible panel
  and the screen reader (chosen extras: **live value** + **shared accessibility
  source**; no rich content).
- **Routing** — a frame-scoped "info target": a hovered widget sets it; falls
  back to the currently selected node. The panel renders the target at end of
  frame. A helper associates a `Response` with its payload when hovered.
- **Shortcuts** — a small static context→shortcuts map, seeded from the
  keyboard-nav handler.
- **Placement** — a collapsible chrome panel, docked bottom-left on
  Desktop/Tablet (Ableton's spot), in the drawer on Phone. Plain text + live
  value + shortcuts only.

## 3. Full-width-on-demand layout (`app.rs`)

"Full-width on demand," not "full-width forced": chrome stays dockable but
collapses to give the canvas full width whenever wanted.

- A **focus toggle** (`chrome_collapsed`: button + keyboard shortcut) collapses
  the macro/morph band, I/O strips, and Info View so the canvas spans full
  width — matching the approved mockup.
- **Key reuse:** the Phone `Drawer`/FAB machinery already built generalizes to
  all breakpoints as the "summon collapsed chrome" mechanism — this is mostly
  lifting an existing gate from Phone-only to all sizes, not net-new layout.
- Defaults: docked on Desktop/Tablet, hero on Phone (unchanged). The native
  1000×700 window still opens docked.

## Testing strategy

- Meters: pure-function unit tests (`db_to_norm`, `smooth`, peak-fall); existing
  widget tests updated.
- Info View: `InfoPayload` assembly from a mock `Accessible` + registry entry.
- Layout: `chrome_collapsed` toggle state and breakpoint interaction.
- Whole crate compiles (`cargo check -p sonido-gui`) and `cargo test` green at
  each feature boundary; manual WASM check at the end.
