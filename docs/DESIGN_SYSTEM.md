# Sonido design system

Every Sonido plugin is the same arcade cabinet, lit in its effect family's color. This document is the spec; the code is the source of truth for values: tokens in [`theme.rs`](../crates/sonido-gui-core/src/theme.rs), the layout in [`cabinet.rs`](../crates/sonido-gui-core/src/cabinet.rs), the display face in [`vector_font.rs`](../crates/sonido-gui-core/src/widgets/vector_font.rs), and the CRT pass in [`egui_bridge/crt.rs`](../crates/sonido-plugin/src/egui_bridge/crt.rs).

![Six cabinets, one per family](img/cabinets.png)

## The idea

A vector-display arcade machine at night (Tempest, Battlezone): thin beams of light with a white-hot core and a colored halo, on a violet-black void, behind curved glass. Three references shaped it:

- **Vector displays.** The whole look comes from lines, bloom and phosphor persistence. The halo carries the hue; the core of the beam is near-white. The near-white core makes a line look lit rather than painted.
- **Synthwave composition.** A low horizon, a perspective grid moving toward you, a cockpit in the foreground (the control deck), primary actions fixed to the edges.
- **The OP-1 rule: show, don't describe.** The screen shows the plugin's own signal live, so what a knob does is visible on the scope.

## Principles

1. **One structure, six lights.** Layout, components and behavior are identical across all 36 effects. Only the family accent changes, so knowing one plugin means knowing all of them.
2. **The screen shows the real signal.** The scope draws the plugin's own input (ghost) and output (beam), from the audio thread. Nothing on the screen is decoration made to look like data.
3. **Fixed edges.** Power is always top-right, the status line is always bottom-left, and meters are always IN on the left and OUT on the right.
4. **Light carries meaning, text carries values.** Values are bright neutral text; labels are muted. Color shows family, level and state, and it appears only in short marks, never in body text.
5. **Readable through the glass.** Curvature, scanlines and aberration apply only inside the screen. Every control and every value sits outside it, so they stay crisp.
6. **Bypass is "game over", not hidden.** A bypassed cabinet dims the screen and blinks INSERT COIN; every control stays visible and live.

## Tokens

### Surfaces and text (Sonido Night)

| Token | Hex | Use | Contrast on `panel` |
|---|---|---|---|
| `void` | `#07060E` | window background, inside the screen | – |
| `panel` | `#100E21` | marquee plate, control deck, meter wells | – |
| `panel_hi` | `#1B1836` | knob bodies, hovered chips | – |
| `line` | `#2E2952` | hairlines, bezels, unlit outlines | 1.4:1 (decorative) |
| `dim` | `#242042` | unlit LED segments, ghost traces | decorative |
| `text_primary` | `#F2EEFF` | values, readouts, the status line | 16.7:1 |
| `text_secondary` | `#ABA5CF` | labels | 8.1:1 |
| `text_dim` | `#7A74A3` | facts and hints nobody needs to read | 4.4:1 |

Text meets WCAG AA (4.5:1) and every label clears AAA (7:1). `text_dim` is only for text the user does not need.

### Families

Each registry category is a family with an accent (its phosphor) and a partner (the second stop of its sunset gradient):

| Family | Category | Accent | Partner | Contrast on `panel` |
|---|---|---|---|---|
| HEAT | Distortion | `#FF6A2B` | `#FF2E88` | 6.6:1 |
| VOLT | Dynamics | `#FFC53D` | `#FF6A2B` | 12.0:1 |
| FLUX | Modulation | `#FF4FD8` | `#7B5CFF` | 6.7:1 |
| ECHO | Time-based | `#A27BFF` | `#36E2FF` | 6.2:1 |
| WAVE | Filter | `#36E2FF` | `#7B5CFF` | 12.2:1 |
| LINE | Utility | `#8CFF5A` | `#36E2FF` | 15.0:1 |
| RIG | the graph player (a whole rig) | `#E2D9FF` | `#FF4FD8` | 14.1:1 |

Every accent clears 3:1 for graphics and 4.5:1 for text. `Family::of_effect(id)` maps a registry ID to its family; RIG is not an effect category and belongs to the graph player alone.

### Signal colors

| Token | Hex | Meaning |
|---|---|---|
| `green` | `#3DFF9A` | meter below −9 dBFS; tuner in tune |
| `yellow` | `#FFE14D` | meter −9 to 0 dBFS |
| `red` | `#FF2D55` | above 0 dBFS; the CLIP latch |

Level is never shown by color alone: segments have position, the CLIP latch shows the word CLIP, and the tuner's needle position is the reading, with green only confirming it.

### Type

| Role | Face | Size | Color |
|---|---|---|---|
| Marquee title | Sonido Vector, slant 0.18 | 22 | family accent |
| Wordmark | Sonido Vector | 13 | `text_primary` |
| Value | Share Tech Mono | 12 | `text_primary` (hot accent while held) |
| Label | Share Tech Mono, uppercase | 10–10.5 | `text_secondary` (accent on hover) |
| Status line | Share Tech Mono | 11 | accent name, `text_primary` value |
| Facts | Share Tech Mono | 10 | `text_dim` |

**Sonido Vector** is the system's own display face: a stroke font on a 4 × 6 grid in the manner of the vector arcade games of 1979 to 1983, drawn with neon lines so it blooms like a trace. It is code, not a font file, so it scales to any size and costs nothing to ship. It sets titles only; it is never used for values.

### Shape and light

- **Chamfers, not radii.** Panels cut their corners at 45° (`chamfer()`); chips and screens keep a 2 to 3 pt radius.
- **Neon strokes.** `glow::neon_polyline`: a halo at `bloom_alpha`, the line, then a white-hot core (`hot_core`: 55% toward white). Every lit element uses it, and nothing unlit does.
- **Spacing.** A 10 pt outer pad, 8 pt gaps; knob cells are 86 × 98 pt; selector rows are 50 pt.

## Components

**Cabinet** (`cabinet::Cabinet`): the whole plugin window, top to bottom:

- **Marquee.** The SONIDO wordmark over `FX//<FAMILY>`, a partner-colored slash, the effect name in slanted Sonido Vector, speed stripes graded from partner to accent, and the power button.
- **Screen**, between the IN and OUT meters. A starfield, a banded sun sinking into the horizon (its slits scroll), and a perspective grid whose speed follows output level. The scope overlays them, plus any read-only readouts (e.g. GR on the compressor). The tuner replaces the sun with a big note readout and a ±50 ¢ needle.
- **Deck.** Selectors first, flowed left to right, then a centered grid of knobs.
- **Rig HUD** (graph player only, `Cabinet::set_rig`). The rig's name and its chain as chips in each effect's family color, top-left of the sky. With no rig loaded, the screen shows NO RIG LOADED and where rigs come from.
- **Footer.** The status line on the left: the name and value of whatever is under the pointer, otherwise the effect's description. Engine facts and the CRT switch sit on the right.

`Cabinet::new(effect_id)` lights a cabinet from the registry; `Cabinet::custom(id, title, description, family)` builds one for something that isn't a single effect, as the graph player does. `cabinet::preferred_size(&descriptors)` returns the window size: 720 pt wide, and tall enough to fit the deck with no scrolling.

**Knob** (`Knob`, bound by `bridged_knob_accent`): a 25-segment LED ring over a dark well, with lit segments in the accent and unlit ones in `dim`. A range centered on zero fills from twelve o'clock. Interaction:

- drag vertically; hold Shift for fine control
- mouse wheel, when enabled
- Up/Down while focused
- double-click to reset

The value is shown in the host's own formatting (`ParamDescriptor::format_value`), so the plugin and the DAW always display a value the same way.

**Selector.** For stepped parameters. Up to four short options render as a segmented control, with the active segment filled and underlined in the accent. More or longer options render as a ◀ value ▶ stepper: the arrows step, a click on the value advances, and scrolling steps.

**Meter.** Two segmented bars from −54 to +6 dBFS. Attack is instant and the fall is 26 dB/s, with a 1.2 s peak hold, a 0 dBFS tick and a numeric peak under the bars. A CLIP latch above the bars is also its own reset button.

**Power.** An IEC power glyph in a ring, lit in the accent while the plugin plays and dim when it is bypassed. It sits in the same place in every plugin.

## Motion

Motion is ambient and audio-reactive, and it stops when the plugin is bypassed:

- The grid scrolls at 0.25 + 2.2 × output energy rows per second.
- The sun's slits drift down, and it swells by up to 5% with energy.
- Stars twinkle at individual rates.
- INSERT COIN blinks at 1.2 Hz with a 62% duty cycle.

The UI repaints every frame the host gives it.

## The CRT glass

Hosts with a GPU pass (the plugin's OpenGL bridge) render egui offscreen and composite it:

- **Everywhere:** bloom, sampled from the frame's mipmaps.
- **Inside the screen rect only:**
  - phosphor persistence (each frame keeps the max of now and 0.74 × last), which leaves vector-display trails on moving traces
  - barrel curvature
  - chromatic aberration growing toward the edges
  - scanlines that bright beams bleed over, as on a real tube
  - an aperture-grille mask, a vignette, a slow refresh band and grain

The UI passes the location of the glass to the renderer through `crt::CrtFrame`. Renderers without the pass (the eframe editor, the browser) fall back to painter scanlines.

**CRT OFF** in the footer turns the pass off for anyone who finds it tiring or wants maximum sharpness; the cabinet stays fully usable without it.

## Working on it

```sh
cargo run -p sonido-plugin --bin sonido-preview -- distortion            # any registry id
cargo run -p sonido-plugin --bin sonido-preview -- tuner --scale 2       # HiDPI
cargo run -p sonido-plugin --bin sonido-preview -- delay --bypassed --snapshot out.png
cargo run -p sonido-plugin --bin sonido-preview -- graph-player --demo-rig
```

The preview opens the real plugin editor (same bridge, same CRT pass) in its own window, fed by a background thread that runs a test riff through the real effect, so the meters, scope and readouts move. `--snapshot` saves a PNG and exits.

Adding a component: build it from the tokens above (`SonidoTheme::get(ctx)`, `Family::accent()`), draw lit things with `glow::neon_*`, keep text outside the glass, and check it in at least two families with the preview.
