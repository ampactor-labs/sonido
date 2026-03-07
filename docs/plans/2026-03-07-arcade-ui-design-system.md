# Sonido Arcade UI Design System

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace the current utilitarian flat UI with a composable, branded design system inspired by retro arcade CRT aesthetics — phosphor glow, 7-segment displays, scanline textures, void backgrounds. The system must work identically across standalone GUI, CLAP/VST3 plugins, and WASM.

**Architecture:** All visual identity lives in `sonido-gui-core` (the shared crate). Widgets read from a `SonidoTheme` struct. No new crates. Effect panel layout code stays mostly unchanged — the transformation happens at the widget and primitive layer.

**Tech Stack:** egui 0.31, pure `Painter` API rendering (no textures, no shaders, no image files). Share Tech Mono font bundled as TTF. 7-segment digit renderer as vector geometry.

---

## 1. Visual Identity

### The Sonido Look

**CRT void** — Every surface sits on near-black (`#0A0A0F`). Elements don't have backgrounds — they *glow* out of the darkness, like phosphor traces on a tube monitor. No flat fills, no cards, no panels with solid backgrounds. Just void and light.

**Phosphor rendering** — Active elements get a 2-layer bloom: the sharp element itself, then a blurred halo at 15-25% alpha in the same color, spread 2-4px. This single effect is what makes everything feel like it's emitting light rather than painted on a surface.

**Scanlines** — Subtle horizontal lines across panel backgrounds. 1px line every 3px at 3% white opacity. Barely visible on static screenshots, adds CRT texture in motion.

**Typography pairing:**
- **7-segment vector renderer** for numeric parameter values (`" 3.5 dB"`, `" 440 Hz"`). Each digit drawn as 7 line segments via `Painter`. Inactive segments render at 5% color (ghost segments visible on real LED displays). Active segments at full color with bloom.
- **Share Tech Mono** (bundled TTF) for all text labels, headings, menus, effect names. Squarish, geometric, machine-stencil quality. Free/open (OFL). Renders well at 10-14px.

### Color System

Each color is a "phosphor trace" with three intensities:
- **Full** — active elements, lit segments, values
- **Dim** — inactive tracks, ghost segments, backgrounds (typically 15-25% of full)
- **Bloom** — halo around active elements (full color at 15-25% alpha, spread 2-4px)

| Role | Name | Hex | Usage |
|------|------|-----|-------|
| Brand / Primary | Amber | `#FFB833` | Active knob arcs, value readouts, headings, brand mark, panel borders, selected states |
| Signal / OK | Green | `#33FF66` | Level meter safe zone, bypass-on LED, signal-present indicators |
| Info / Labels | Cyan | `#33DDFF` | Parameter labels, secondary text, node graph wires, morph A color |
| Danger / Clip | Red | `#FF3333` | Clipping indicators, error states, bypass-off, distortion category |
| Modulation | Magenta | `#FF33AA` | Modulation effect category, LFO indicators |
| Caution / Hot | Yellow | `#FFDD33` | Meter hot zone (70-95%), warnings |
| Time-based | Purple | `#AA55FF` | Delay/reverb effect category in graph |
| Inactive | Dim | `#2A2A35` | Knob tracks, inactive elements, ghost segments |
| Background | Void | `#0A0A0F` | All backgrounds — the darkness everything glows out of |
| Scanline | Scanline | `#FFFFFF` @ 3% | Horizontal texture lines |

### Category Colors (Graph Editor & Plugin Tint)

Each effect category has a signature color used for graph node borders, wire colors, and plugin window border tinting:

| Category | Color | Effects |
|----------|-------|---------|
| Dynamics | Cyan `#33DDFF` | Compressor, Gate, Limiter |
| Distortion | Red `#FF3333` | Distortion, Preamp, Bitcrusher, Tape |
| Modulation | Magenta `#FF33AA` | Chorus, Flanger, Phaser, Tremolo, Vibrato, Ring Mod |
| Filter | Amber `#FFB833` | Filter, Wah, EQ |
| Time-based | Purple `#AA55FF` | Delay, Reverb |
| Utility | Gray `#667788` | Stage |
| Structural | Dim white `#778888` | Input, Output, Split, Merge |

---

## 2. Theme Architecture

### SonidoTheme Struct

Single source of truth. All widgets read from this. Stored in egui `Context::data()` for global access.

```rust
pub struct SonidoTheme {
    pub colors: ThemeColors,
    pub sizing: ThemeSizing,
    pub glow: GlowConfig,
    pub scanlines: ScanlineConfig,
    pub reduced_fx: bool, // Skip bloom + scanlines for performance (WASM fallback)
}

pub struct ThemeColors {
    pub amber: Color32,        // #FFB833 — brand primary
    pub green: Color32,        // #33FF66 — signal OK
    pub cyan: Color32,         // #33DDFF — info/labels
    pub red: Color32,          // #FF3333 — danger/clip
    pub magenta: Color32,      // #FF33AA — modulation
    pub yellow: Color32,       // #FFDD33 — caution
    pub purple: Color32,       // #AA55FF — time-based
    pub dim: Color32,          // #2A2A35 — inactive
    pub void: Color32,         // #0A0A0F — background
    pub text_primary: Color32, // #E6E6EB — main text (Share Tech Mono)
    pub text_secondary: Color32, // #778888 — muted text
}

pub struct ThemeSizing {
    pub knob_diameter: f32,      // 60.0
    pub meter_width: f32,        // 24.0
    pub meter_height: f32,       // 120.0
    pub led_digit_width: f32,    // 10.0
    pub led_digit_height: f32,   // 16.0
    pub led_digit_gap: f32,      // 2.0
    pub panel_border_radius: f32, // 4.0
    pub item_spacing: Vec2,      // (8.0, 6.0)
    pub knob_spacing: f32,       // 16.0
    pub panel_padding: f32,      // 16.0
}

pub struct GlowConfig {
    pub bloom_radius: f32,     // 3.0 — halo spread in pixels
    pub bloom_alpha: f32,      // 0.20 — halo opacity
    pub ghost_alpha: f32,      // 0.05 — inactive segment visibility
    pub hover_bloom_mult: f32, // 1.5 — bloom multiplier on hover
}

pub struct ScanlineConfig {
    pub line_spacing: f32,  // 3.0 — pixels between scanlines
    pub line_opacity: f32,  // 0.03 — white opacity per line
    pub enabled: bool,      // true (false on reduced_fx)
}
```

### Theme Access Pattern

```rust
// At app startup
let theme = SonidoTheme::default();
ctx.data_mut(|d| d.insert_temp(Id::NULL, theme));

// In any widget
let theme = ctx.data(|d| d.get_temp::<SonidoTheme>(Id::NULL).unwrap());
```

No trait, no generic parameter threading, no theme provider widget. Just a struct in egui's temp data store. Simple.

### reduced_fx Flag

When `true`: bloom layers skip (just the sharp element, no halo), scanlines skip, attract mode skips. The glow primitive functions check this flag internally — widget code doesn't branch. We don't build the toggle UI until WASM frame drops are measured. For now it's just `false`.

---

## 3. Glow Primitives (`widgets/glow.rs`)

Reusable painter functions that every widget calls. These are the building blocks of the entire visual system.

```rust
/// Paint a filled circle with phosphor bloom.
/// Sharp circle at `color`, plus a larger circle at `color * bloom_alpha`.
pub fn glow_circle(
    painter: &Painter,
    center: Pos2,
    radius: f32,
    color: Color32,
    theme: &SonidoTheme,
);

/// Paint an arc stroke with phosphor bloom.
/// Sharp arc at `stroke_width`, bloom arc at `stroke_width * 2` and bloom_alpha.
pub fn glow_arc(
    painter: &Painter,
    center: Pos2,
    radius: f32,
    start_angle: f32,
    end_angle: f32,
    color: Color32,
    stroke_width: f32,
    theme: &SonidoTheme,
);

/// Paint a line segment with phosphor bloom.
pub fn glow_line(
    painter: &Painter,
    start: Pos2,
    end: Pos2,
    color: Color32,
    stroke_width: f32,
    theme: &SonidoTheme,
);

/// Paint a filled rect with phosphor bloom (for LED meter segments).
pub fn glow_rect(
    painter: &Painter,
    rect: Rect,
    color: Color32,
    corner_radius: f32,
    theme: &SonidoTheme,
);

/// Paint scanline texture over a rect.
/// Horizontal 1px lines every `line_spacing` pixels at `line_opacity`.
pub fn scanlines(painter: &Painter, rect: Rect, theme: &SonidoTheme);

/// Dim a color to ghost/inactive intensity.
pub fn ghost(color: Color32, theme: &SonidoTheme) -> Color32;

/// Brighten a color for hover state (increase bloom_radius).
pub fn hover_bloom(color: Color32, theme: &SonidoTheme) -> Color32;
```

All functions check `theme.reduced_fx` and skip bloom layers when true.

---

## 4. 7-Segment Display (`widgets/led_display.rs`)

### Segment Geometry

Each digit is 7 line segments arranged in the classic figure-8 pattern:

```
 _
|_|
|_|
```

Segments labeled A-G (standard convention):
- A: top horizontal
- B: top-right vertical
- C: bottom-right vertical
- D: bottom horizontal
- E: bottom-left vertical
- F: top-left vertical
- G: middle horizontal

Each segment is a line drawn with `glow_line()`. Active segments use the display's color (amber default). Inactive segments use `ghost()` color — visible at 5% as the faint unlit segments you see on real LED displays.

### Digit Map

```rust
const SEGMENTS: [u8; 10] = [
    0b1110111, // 0: A B C D E F
    0b0010010, // 1: B C
    0b1011101, // 2: A B D E G
    0b1011011, // 3: A B C D G
    0b0111010, // 4: B C F G
    0b1101011, // 5: A C D F G
    0b1101111, // 6: A C D E F G
    0b1010010, // 7: A B C
    0b1111111, // 8: all
    0b1111011, // 9: A B C D F G
];
```

Special characters: `-` (segment G only), `.` (small dot bottom-right), ` ` (all ghost), `d`, `B` (for dB display).

### Public API

```rust
pub struct LedDisplay {
    color: Color32,     // Default: theme.colors.amber
    digit_count: usize, // Default: 6
    show_ghosts: bool,  // Default: true
}

impl LedDisplay {
    pub fn new() -> Self;
    pub fn color(self, color: Color32) -> Self; // builder
    pub fn digits(self, count: usize) -> Self;  // builder

    /// Render a formatted parameter value: " 3.5 dB", " 440 Hz", "120 ms"
    pub fn value(self, ui: &mut Ui, value: f32, unit: &ParamUnit) -> Response;

    /// Render raw text (digits + limited chars only)
    pub fn raw(self, ui: &mut Ui, text: &str) -> Response;
}
```

### Formatting Rules

Automatic formatting based on `ParamUnit` (matches current `BridgedKnob` format logic):
- `Decibels`: `"-3.5dB"` or `" 0.0dB"` — right-aligned, 1 decimal
- `Hertz`: `"1.2kHz"` (>= 1000) or `" 440Hz"` (< 1000)
- `Milliseconds`: `"1.50 s"` (>= 1000) or `" 100ms"` (< 1000)
- `Percent`: `"  50 %"` — integer, right-aligned
- `Ratio`: `" 4.0:1"`
- `None`: `" 0.50"` — 2 decimal places

---

## 5. Widget Redesigns

### Knob (`widgets/knob.rs`)

**Visual:**
- **Track arc:** 270-degree arc in `dim` color, 4px stroke. Ghost glow.
- **Value arc:** Drawn in amber (or category color) with bloom — sharp arc at 6px, bloom halo behind at 20% alpha, 10px stroke. Glows brighter as value increases.
- **No knob body.** No filled circle. Just a **pointer line** from center to arc edge — a single bright line on void, like an oscilloscope trace.
- **Center dot:** 3px circle in active color.
- **Below knob:** `LedDisplay` showing current value (7-segment, amber).
- **Below display:** Label text in cyan (Share Tech Mono, 11px).
- **Morph markers** (when morph active): Small circles on the arc ring — cyan dot for A position, amber dot for B position.

**Interaction:** Unchanged — vertical drag, shift for fine, double-click for default reset. Gesture protocol (begin_set/end_set) unchanged.

**Hover:** Bloom radius doubles on the pointer line and value arc. Label brightens to full cyan.

### Level Meter (`widgets/meter.rs`)

**Visual:**
- **Background:** Void with scanline texture.
- **Segmented bar:** 16 discrete horizontal segments (2px tall, 1px gap) instead of smooth fill. Each segment is a `glow_rect()`.
- **Inactive segments:** Ghost visibility (5%).
- **Active segment colors:** Green (bottom 70%), yellow (70-95%), red (top 5%). Each lit segment glows.
- **Peak hold:** Single segment stays lit at peak position, fades over 1.5 seconds (alpha lerp per frame).
- **Clip indicator:** Top segment blinks at 4Hz when peak > 1.0.

**GainReductionMeter:** Same segmented approach, segments light from top down in amber/orange.

### Toggle (`widgets/toggle.rs`)

**BypassToggle:**
- OFF: Dim outline circle, ghost glow.
- ON: Bright green filled circle with 4px bloom halo. Arcade button backlight.
- Hover: Bloom radius increases.

**FootswitchToggle:**
- Dark rectangular body (rounded corners, dim border).
- LED indicator dot on top — green glow when ON, ghost when OFF.
- Chunky, physical-feeling.

### Morph Bar (`widgets/morph_bar.rs`)

- **A button:** Glowing cyan circle when captured, dim ghost when empty.
- **B button:** Glowing amber circle when captured, dim ghost when empty.
- **Slider track:** Row of discrete LED segments (horizontal). Position shown by lit segments. Segments crossfade color from cyan (left/A) to amber (right/B) across the range — per-segment color interpolation using `Color32::lerp()` based on segment position vs. slider position.
- **Disabled state:** All segments ghost until both A and B captured.

---

## 6. Effect Panel Template

### Frame

- **Border:** 1px line in amber with bloom. Corner radius 4px.
- **Interior:** Void + scanline texture.
- **Title:** Effect name in Share Tech Mono (12px, amber), top-left inside border.
- **No background fill** — the void shows through.

### Layout Grid

```
+--- DISTORTION ----------------------------------+
| [*] Active            [| HARD  v]               |  <- toggle + combo (top row)
|                                                  |
|  ~~~~~   ~~~~~   ~~~~~   ~~~~~                   |  <- knob arcs (pointer-on-void)
|  24.0dB   50 %   4.2kHz  -1.0dB                 |  <- 7-segment LED readouts
|  DRIVE    MIX    TONE    OUTPUT                  |  <- cyan labels (Share Tech Mono)
+-------------------------------------------------+
```

Multi-row effects (Reverb, Compressor, Delay) stack knob rows vertically with 12px spacing.

### Combo Boxes

Styled to match: void background, amber text in Share Tech Mono, selected item shown in a small LED display area. Dropdown has void background with amber text items. Highlighted item gets a dim amber glow bar behind it.

### Changes to Effect Panel Code

Minimal. The 19 `effects_ui/*.rs` files already call `bridged_knob()`, `bypass_toggle()`, etc. Those functions produce different visuals now because the underlying widgets changed. Panel layout code (horizontal rows, spacing constants) stays mostly the same — just reads spacing from `SonidoTheme` instead of hardcoded values.

---

## 7. Graph Editor Styling

### Node Bodies

- **Border:** 1px line in category color with bloom. No solid fill — void interior with scanline texture.
- **Effect name:** Share Tech Mono text inside node, category color.
- **Selected node:** Border at full intensity, bloom radius doubles.
- **Unselected node:** Border at 60% intensity, normal bloom.

### Wires

- 1px lines with bloom in source node's category color.
- Connected wires glow; disconnected stubs are ghost/dim.

### Context Menu (right-click to add node)

- Void background, amber border with bloom.
- Category sub-menus in category colors.
- Effect names in Share Tech Mono.
- Hover: dim glow bar behind item.

### Compatibility Note

`egui_snarl` 0.7.1 uses `SnarlViewer` trait for custom node rendering. The `header`, `show_body`, `connect`, `disconnect` methods give us full control over node appearance. Wire rendering uses `SnarlStyle` which we can customize. No library modification needed.

---

## 8. Main App Layout

### Header Bar

- **Background:** Void (no panel color).
- **"SONIDO"**: Share Tech Mono, 18px, amber with bloom. The brand mark.
- **Preset selector:** LED-display-styled combo box showing preset name in amber segments.
- **Audio status:** Green/red LED dot with bloom (existing indicator, new visual treatment).
- **File source toggle:** Styled as arcade button with LED.
- **Compile button** (multi-effect): Amber border button, Share Tech Mono label.

### Central Area (3-column, unchanged structure)

- **Left column (INPUT):** "INPUT" in cyan Share Tech Mono. Segmented level meter (new style). Input gain knob (new pointer-on-void style).
- **Center column:** Graph editor (upper) + effect panel (lower, scrollable) in multi-effect mode. Effect panel only in single-effect mode.
- **Right column (OUTPUT):** Mirror of input column.

### Status Bar

- **BYPASS button:** Large glowing red LED toggle with bloom.
- **Sample rate:** Amber 7-segment digits (`48000`).
- **Buffer size:** Amber 7-segment dropdown.
- **Latency:** Amber 7-segment value.
- **CPU percentage:** Green 7-segment digits. Color shifts to yellow > 80%, red > 100%.
- **CPU sparkline:** Green phosphor trace (1px `glow_line` segments) — looks like an oscilloscope trace.

### File Player Bar

- **Transport buttons:** Arcade-styled — dark bodies with LED indicators (green = play, amber = pause, ghost = stop).
- **Progress bar:** Horizontal segmented LED bar (like morph slider), fills left-to-right in amber as playback progresses.
- **Duration display:** 7-segment readout showing elapsed / total time.

---

## 9. Single-Effect Plugin Layout

When Sonido runs as a CLAP/VST3 plugin for a single effect (e.g., "Sonido Distortion"):

### Layout

The effect panel fills the entire plugin window. No graph editor, no morph bar, no I/O columns. Generous padding (24px).

```
+============= SONIDO DISTORTION ==============+
‖                                        [*]   ‖  <- brand name + bypass LED
‖                                              ‖
‖    ~~~~~   ~~~~~   ~~~~~   ~~~~~             ‖  <- knobs
‖    24.0dB   50 %   4.2kHz  -1.0dB            ‖  <- LED values
‖    DRIVE    MIX    TONE    OUTPUT            ‖  <- labels
‖                                              ‖
+===============================================+
```

### Frame-as-Meter (Signature Feature)

The left and right edges of the plugin window border function as input/output level meters:

- **Left border:** Input level meter. The 1px amber border is replaced by a column of discrete LED segments (4px wide). Segments light up from bottom in green/yellow/red based on input signal level.
- **Right border:** Output level meter. Same treatment.
- **Top and bottom borders:** Static amber with bloom (non-metered).
- **When no signal:** All border segments at ghost intensity — the frame is visible but dim.
- **When signal flows:** The frame pulses with the audio. The plugin literally glows brighter when you play through it.

This replaces the dedicated I/O meter columns from the standalone app, saving ~200px of width.

### Category Color Tint

Each effect category shifts the border hue toward its category color:

- **Distortion/Preamp/Bitcrusher/Tape:** Border tints red-amber (`#FF8833`)
- **Modulation (Chorus/Flanger/Phaser/Tremolo/Vibrato/RingMod):** Border tints magenta-amber (`#FF7744`)
- **Dynamics (Compressor/Gate/Limiter):** Border tints cyan-amber (`#88CC88`)
- **Filter/Wah/EQ:** Pure amber (brand color, no shift)
- **Time-based (Delay/Reverb):** Border tints purple-amber (`#CC8855`)

The tint is subtle — 70% amber, 30% category color. Enough to differentiate when multiple Sonido plugins are open in a DAW, while keeping the brand cohesion.

### Plugin Window Sizing

Default size based on knob row count:
- 1 row (e.g., Stage): 400 x 200
- 2 rows (e.g., Distortion): 450 x 280
- 3 rows (e.g., Reverb): 500 x 360

Resizable within bounds: min 320x200, max 800x600.

---

## 10. Attract Mode

When no audio signal has passed through (output level below -80 dB) for 5+ seconds:

- **7-segment ghost segments** cycle through a slow dim pattern — each digit's ghost segments brighten slightly in sequence, left to right, then fade. Like digits "breathing." Very slow (one full cycle every 3 seconds), very dim (ghost alpha goes from 5% to 10% and back).
- **Frame-as-meter segments** do a slow chase pattern — one segment at a time lights up at 8% and moves upward, wrapping. Like an idle arcade attract animation.
- **Knob pointer lines** don't animate (would be distracting).

When signal returns (output > -60 dB), attract mode stops immediately and live values take over. No transition — snap to live, like an arcade game starting.

Only active on native builds. Skipped when `reduced_fx` is true. Skipped on WASM.

---

## 11. Font Loading

### Share Tech Mono

- **Source:** Google Fonts, OFL license. Single weight (regular).
- **Bundle:** Include `ShareTechMono-Regular.ttf` in `sonido-gui-core/assets/`.
- **Loading:**
  ```rust
  let font_data = include_bytes!("../assets/ShareTechMono-Regular.ttf");
  let mut fonts = FontDefinitions::default();
  fonts.font_data.insert(
      "share_tech_mono".to_owned(),
      egui::FontData::from_static(font_data),
  );
  fonts.families.get_mut(&FontFamily::Monospace).unwrap()
      .insert(0, "share_tech_mono".to_owned());
  // Also set as proportional default for consistent look
  fonts.families.get_mut(&FontFamily::Proportional).unwrap()
      .insert(0, "share_tech_mono".to_owned());
  ctx.set_fonts(fonts);
  ```
- **Sizes:** 18px headings, 12px labels, 11px secondary text, 10px footer.

### 7-Segment Renderer

No font file. Pure vector geometry via `Painter::line_segment()`. Segment coordinates computed from digit position and `ThemeSizing::led_digit_width/height`. This guarantees pixel-perfect rendering at any DPI with no font rasterization artifacts.

---

## 12. Migration Path

### Phase Strategy

The theme system and glow primitives can be built first, then widgets migrated one at a time. At each step the app compiles and runs — some widgets look "new" while others still look "old" until they're converted.

### Order of Operations

1. `SonidoTheme` struct + `glow.rs` primitives + font loading
2. `led_display.rs` (new widget, no migration needed)
3. `knob.rs` rework (pointer-on-void, LED readout below)
4. `meter.rs` rework (segmented, glow)
5. `toggle.rs` rework (LED bloom)
6. `morph_bar.rs` rework (segment crossfade)
7. Effect panel template (amber border frame, scanlines)
8. `app.rs` layout chrome (header, status bar, I/O columns)
9. Graph editor node styling
10. Plugin frame-as-meter + category tint
11. Attract mode

Each step is independently committable and testable.

---

## 13. Performance Considerations

### Draw Call Budget

Each bloom effect = 2 painter calls (sharp + halo). A typical effect panel with 6 knobs:
- 6 track arcs (1 call each) = 6
- 6 value arcs with bloom (2 calls each) = 12
- 6 pointer lines with bloom (2 calls each) = 12
- 6 center dots (1 call each) = 6
- 6 LED displays (~42 segments × 2 calls) = ~84
- 6 labels (1 text call each) = 6
- Panel border with bloom = 2
- Scanlines = 1 (single rect with texture)
- **Total: ~129 calls** per panel

Compare: current flat panel = ~40 calls. 3x increase, but egui handles thousands of calls per frame at 60fps on native. WASM may need `reduced_fx` which drops back to ~60 calls.

### Scanline Implementation

Not a texture — a loop of horizontal `line_segment()` calls. For a 400px-tall panel at 3px spacing: ~133 line calls. Alternative: a single semi-transparent striped rect via `Shape::Rect` with a repeating pattern. Profile both approaches.

### Attract Mode

Timer-based, not per-frame computation. A single `Instant` tracks "last signal above threshold." Animation state is one `f32` phase counter advanced per frame when active. Negligible cost.

---

## 14. File Inventory

### New Files

| File | Purpose |
|------|---------|
| `crates/sonido-gui-core/src/widgets/glow.rs` | Phosphor bloom/scanline/ghost painting primitives |
| `crates/sonido-gui-core/src/widgets/led_display.rs` | 7-segment numeric display renderer |
| `crates/sonido-gui-core/assets/ShareTechMono-Regular.ttf` | Bundled monospace font |

### Modified Files

| File | Changes |
|------|---------|
| `crates/sonido-gui-core/src/theme.rs` | Replace 12 constants with `SonidoTheme` struct, color system, font loading |
| `crates/sonido-gui-core/src/widgets/mod.rs` | Add `pub mod glow;` and `pub mod led_display;` |
| `crates/sonido-gui-core/src/widgets/knob.rs` | Pointer-on-void style, LED readout, theme-driven colors |
| `crates/sonido-gui-core/src/widgets/bridged_knob.rs` | Use `LedDisplay` for value, theme colors for label |
| `crates/sonido-gui-core/src/widgets/meter.rs` | Segmented bars, glow, peak hold fade, clip blink |
| `crates/sonido-gui-core/src/widgets/toggle.rs` | LED bloom for bypass, footswitch glow |
| `crates/sonido-gui-core/src/widgets/morph_bar.rs` | Segment crossfade slider, glow buttons |
| `crates/sonido-gui-core/src/effects_ui/*.rs` | Replace hardcoded colors/spacing with theme access (19 files, mechanical) |
| `crates/sonido-gui/src/app.rs` | Header/status bar/I/O chrome restyling |
| `crates/sonido-gui/src/graph_view.rs` | Node border glow, wire bloom, category colors |
| `crates/sonido-gui/src/theme.rs` | Update re-exports if needed |
| `crates/sonido-plugin/src/gui.rs` | Frame-as-meter, category tint |
| `crates/sonido-plugin/src/chain/gui.rs` | Frame-as-meter for chain plugin |

### Documentation Updates (per CLAUDE.md rules)

| Doc | Updates |
|-----|---------|
| `docs/GUI.md` | New design system description, component inventory, color palette |
| `docs/ARCHITECTURE.md` | Update sonido-gui-core section for new widget/theme structure |
| `docs/CHANGELOG.md` | Design system entry |
| `README.md` | Update screenshots/description if applicable |

---

## 15. Non-Goals (YAGNI)

- **Light theme** — Not needed. The arcade aesthetic IS the dark void.
- **Theme customization UI** — No user-facing theme picker. One look. One brand.
- **Custom cursor** — Adds complexity, minimal payoff.
- **Sound effects** — Tempting for arcade vibes, but this is a pro audio tool.
- **Accessibility toggle** — Can add later. Ghost segments provide shape cues beyond color. The mono font is highly legible.
- **Texture atlas / shader effects** — Pure Painter API. No GPU dependencies beyond what egui already requires.
- **Animation framework** — Attract mode + peak hold are simple timer-driven. No easing library needed.
