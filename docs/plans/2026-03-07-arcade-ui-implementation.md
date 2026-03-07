# Arcade UI Design System — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Implement the Sonido Arcade UI design system from `docs/plans/2026-03-07-arcade-ui-design-system.md` — CRT phosphor glow, 7-segment displays, void backgrounds, Share Tech Mono font.

**Architecture:** All changes live in `sonido-gui-core` (shared crate). Widgets are reworked one at a time, each step independently compilable. Effect panel code barely changes because all 19 panels delegate to `bridged_knob`/`BypassToggle`/`bridged_combo` — the visual transformation happens at the widget layer.

**Tech Stack:** egui 0.31, pure `Painter` API rendering, Share Tech Mono TTF (bundled via `include_bytes!`).

**Reference:** Read `docs/plans/2026-03-07-arcade-ui-design-system.md` for full color specs, sizing, and component details.

---

## Task 1: SonidoTheme Struct + Font Loading

**Files:**
- Modify: `crates/sonido-gui-core/src/theme.rs`
- Modify: `crates/sonido-gui-core/src/lib.rs`
- Create: `crates/sonido-gui-core/assets/` directory

**Step 1: Download Share Tech Mono font**

```bash
cd crates/sonido-gui-core
mkdir -p assets
curl -L "https://github.com/ArtifexSoftware/google-fonts/raw/main/ofl/sharetechmono/ShareTechMono-Regular.ttf" \
  -o assets/ShareTechMono-Regular.ttf
```

Verify file exists and is ~42KB TTF.

**Step 2: Rewrite `theme.rs`**

Replace the entire file. The new `SonidoTheme` struct replaces `Theme`:

```rust
//! Arcade CRT visual theme for the Sonido GUI.
//!
//! All colors, sizing, glow parameters, and scanline config live in
//! [`SonidoTheme`]. Widgets read this from `egui::Context::data()`.
//! The theme produces a CRT phosphor aesthetic: amber-dominant colors,
//! bloom/glow on active elements, void backgrounds, and scanline textures.

use egui::{Color32, Context, CornerRadius, FontDefinitions, FontFamily, Id, Stroke, Style, Vec2, Visuals, vec2};

/// Complete arcade CRT theme — single source of truth for all visual parameters.
#[derive(Clone, Debug)]
pub struct SonidoTheme {
    /// Color palette.
    pub colors: ThemeColors,
    /// Sizing constants.
    pub sizing: ThemeSizing,
    /// Glow / bloom parameters.
    pub glow: GlowConfig,
    /// Scanline overlay parameters.
    pub scanlines: ScanlineConfig,
    /// Skip bloom + scanlines for performance (WASM fallback).
    pub reduced_fx: bool,
}

/// Phosphor color palette — each color is a "trace" on the CRT.
#[derive(Clone, Debug)]
pub struct ThemeColors {
    /// Brand primary — active knob arcs, headings, panel borders.
    pub amber: Color32,
    /// Signal OK — meter safe zone, bypass-on LED.
    pub green: Color32,
    /// Info / labels — parameter labels, secondary text.
    pub cyan: Color32,
    /// Danger / clip — clipping, error states.
    pub red: Color32,
    /// Modulation — modulation effect category.
    pub magenta: Color32,
    /// Caution — meter hot zone.
    pub yellow: Color32,
    /// Time-based — delay/reverb category.
    pub purple: Color32,
    /// Inactive — knob tracks, ghost segments.
    pub dim: Color32,
    /// Background — the void everything glows out of.
    pub void: Color32,
    /// Primary text.
    pub text_primary: Color32,
    /// Secondary/muted text.
    pub text_secondary: Color32,
}

/// Sizing constants for layout consistency.
#[derive(Clone, Debug)]
pub struct ThemeSizing {
    /// Knob diameter in pixels.
    pub knob_diameter: f32,
    /// Level meter width.
    pub meter_width: f32,
    /// Level meter height.
    pub meter_height: f32,
    /// 7-segment digit width.
    pub led_digit_width: f32,
    /// 7-segment digit height.
    pub led_digit_height: f32,
    /// Gap between 7-segment digits.
    pub led_digit_gap: f32,
    /// Effect panel border corner radius.
    pub panel_border_radius: f32,
    /// Default item spacing (horizontal, vertical).
    pub item_spacing: Vec2,
    /// Horizontal spacing between knobs.
    pub knob_spacing: f32,
    /// Internal padding for panels.
    pub panel_padding: f32,
}

/// Phosphor glow / bloom configuration.
#[derive(Clone, Debug)]
pub struct GlowConfig {
    /// Halo spread in pixels.
    pub bloom_radius: f32,
    /// Halo opacity (0.0 to 1.0).
    pub bloom_alpha: f32,
    /// Ghost / inactive segment opacity (0.0 to 1.0).
    pub ghost_alpha: f32,
    /// Bloom radius multiplier on hover.
    pub hover_bloom_mult: f32,
}

/// Scanline overlay configuration.
#[derive(Clone, Debug)]
pub struct ScanlineConfig {
    /// Pixels between horizontal scanlines.
    pub line_spacing: f32,
    /// Opacity of each scanline (white).
    pub line_opacity: f32,
    /// Whether scanlines are rendered.
    pub enabled: bool,
}

impl Default for SonidoTheme {
    fn default() -> Self {
        Self {
            colors: ThemeColors::default(),
            sizing: ThemeSizing::default(),
            glow: GlowConfig::default(),
            scanlines: ScanlineConfig::default(),
            reduced_fx: false,
        }
    }
}

impl Default for ThemeColors {
    fn default() -> Self {
        Self {
            amber: Color32::from_rgb(255, 184, 51),
            green: Color32::from_rgb(51, 255, 102),
            cyan: Color32::from_rgb(51, 221, 255),
            red: Color32::from_rgb(255, 51, 51),
            magenta: Color32::from_rgb(255, 51, 170),
            yellow: Color32::from_rgb(255, 221, 51),
            purple: Color32::from_rgb(170, 85, 255),
            dim: Color32::from_rgb(42, 42, 53),
            void: Color32::from_rgb(10, 10, 15),
            text_primary: Color32::from_rgb(230, 230, 235),
            text_secondary: Color32::from_rgb(119, 136, 136),
        }
    }
}

impl Default for ThemeSizing {
    fn default() -> Self {
        Self {
            knob_diameter: 60.0,
            meter_width: 24.0,
            meter_height: 120.0,
            led_digit_width: 10.0,
            led_digit_height: 16.0,
            led_digit_gap: 2.0,
            panel_border_radius: 4.0,
            item_spacing: vec2(8.0, 6.0),
            knob_spacing: 16.0,
            panel_padding: 16.0,
        }
    }
}

impl Default for GlowConfig {
    fn default() -> Self {
        Self {
            bloom_radius: 3.0,
            bloom_alpha: 0.20,
            ghost_alpha: 0.05,
            hover_bloom_mult: 1.5,
        }
    }
}

impl Default for ScanlineConfig {
    fn default() -> Self {
        Self {
            line_spacing: 3.0,
            line_opacity: 0.03,
            enabled: true,
        }
    }
}

/// egui temp-data ID for storing the theme.
const THEME_ID: &str = "sonido_theme";

impl SonidoTheme {
    /// Store this theme in the egui context for global access by widgets.
    pub fn install(&self, ctx: &Context) {
        ctx.data_mut(|d| d.insert_temp(Id::new(THEME_ID), self.clone()));
    }

    /// Retrieve the theme from the egui context.
    ///
    /// Returns `SonidoTheme::default()` if no theme was installed.
    pub fn get(ctx: &Context) -> Self {
        ctx.data(|d| d.get_temp::<Self>(Id::new(THEME_ID)))
            .unwrap_or_default()
    }

    /// Load the Share Tech Mono font and register it as both Monospace and Proportional.
    pub fn load_fonts(ctx: &Context) {
        let font_data = include_bytes!("../assets/ShareTechMono-Regular.ttf");
        let mut fonts = FontDefinitions::default();
        fonts.font_data.insert(
            "share_tech_mono".to_owned(),
            egui::FontData::from_static(font_data),
        );
        // Primary for both families — the arcade look uses mono everywhere
        fonts.families
            .get_mut(&FontFamily::Monospace)
            .unwrap()
            .insert(0, "share_tech_mono".to_owned());
        fonts.families
            .get_mut(&FontFamily::Proportional)
            .unwrap()
            .insert(0, "share_tech_mono".to_owned());
        ctx.set_fonts(fonts);
    }

    /// Apply the theme to the egui context (visuals, spacing, fonts).
    ///
    /// Call once at startup, or when the theme changes.
    pub fn apply(&self, ctx: &Context) {
        self.install(ctx);
        Self::load_fonts(ctx);

        let mut style = Style::default();
        let mut visuals = Visuals::dark();

        // Void backgrounds
        visuals.window_fill = self.colors.void;
        visuals.panel_fill = self.colors.void;
        visuals.extreme_bg_color = self.colors.void;
        visuals.faint_bg_color = self.colors.dim;

        // Widget colors — dim base, amber accents
        visuals.widgets.noninteractive.bg_fill = self.colors.void;
        visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, self.colors.text_secondary);
        visuals.widgets.noninteractive.corner_radius = CornerRadius::same(4);

        visuals.widgets.inactive.bg_fill = self.colors.dim;
        visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, self.colors.text_primary);
        visuals.widgets.inactive.corner_radius = CornerRadius::same(4);

        visuals.widgets.hovered.bg_fill = Color32::from_rgb(50, 45, 30);
        visuals.widgets.hovered.fg_stroke = Stroke::new(1.5, self.colors.amber);
        visuals.widgets.hovered.corner_radius = CornerRadius::same(4);

        visuals.widgets.active.bg_fill = Color32::from_rgb(60, 50, 25);
        visuals.widgets.active.fg_stroke = Stroke::new(2.0, self.colors.amber);
        visuals.widgets.active.corner_radius = CornerRadius::same(4);

        // Selection — amber tint
        visuals.selection.bg_fill = self.colors.amber.gamma_multiply(0.2);
        visuals.selection.stroke = Stroke::new(1.0, self.colors.amber);

        visuals.override_text_color = Some(self.colors.text_primary);

        style.visuals = visuals;
        style.spacing.item_spacing = self.sizing.item_spacing;
        style.spacing.window_margin = egui::Margin::same(12);
        style.spacing.button_padding = egui::vec2(8.0, 4.0);

        ctx.set_style(style);
    }

    /// Get meter segment color based on normalized position (0.0 = bottom, 1.0 = top).
    pub fn meter_segment_color(&self, position: f32) -> Color32 {
        if position > 0.95 {
            self.colors.red
        } else if position > 0.7 {
            self.colors.yellow
        } else {
            self.colors.green
        }
    }
}

// === Backward compatibility ===

/// Type alias for the old `Theme` name. Use `SonidoTheme` for new code.
pub type Theme = SonidoTheme;
```

**Step 3: Update `lib.rs` re-export**

Change:
```rust
pub use theme::Theme;
```
to:
```rust
pub use theme::{SonidoTheme, Theme};
```

**Step 4: Update callers**

In `crates/sonido-gui/src/app.rs`, find where `Theme::default().apply(ctx)` is called and change to `SonidoTheme::default().apply(ctx)`. The `Theme` type alias ensures nothing else breaks.

**Step 5: Verify compilation**

```bash
cargo check -p sonido-gui-core && cargo check -p sonido-gui
```

Expected: compiles clean. The `Theme` type alias means all existing code still works.

**Step 6: Run tests**

```bash
cargo test -p sonido-gui-core
```

Expected: all existing tests pass (theme struct tests may need minor updates if they reference old field names, but since `Theme` was only used via `Theme::default().apply()`, tests should pass as-is with the alias).

**Step 7: Commit**

```bash
git add crates/sonido-gui-core/assets/ crates/sonido-gui-core/src/theme.rs crates/sonido-gui-core/src/lib.rs crates/sonido-gui/src/app.rs
git commit -m "feat(gui-core): SonidoTheme struct with arcade CRT color palette + Share Tech Mono font"
```

---

## Task 2: Glow Primitives (`widgets/glow.rs`)

**Files:**
- Create: `crates/sonido-gui-core/src/widgets/glow.rs`
- Modify: `crates/sonido-gui-core/src/widgets/mod.rs`

**Step 1: Create `glow.rs`**

```rust
//! Phosphor bloom and scanline rendering primitives.
//!
//! Every arcade-styled widget calls these functions to produce the CRT glow
//! effect: a sharp element plus a soft halo at reduced alpha. All functions
//! check `theme.reduced_fx` and skip the bloom layer when true.

use egui::{Color32, Painter, Pos2, Rect, Stroke, pos2};

use crate::theme::SonidoTheme;

/// Dim a color to ghost/inactive intensity.
pub fn ghost(color: Color32, theme: &SonidoTheme) -> Color32 {
    color.gamma_multiply(theme.glow.ghost_alpha / 1.0)
}

/// Create a bloom (halo) version of a color.
fn bloom_color(color: Color32, theme: &SonidoTheme) -> Color32 {
    color.gamma_multiply(theme.glow.bloom_alpha)
}

/// Paint a filled circle with phosphor bloom.
pub fn glow_circle(
    painter: &Painter,
    center: Pos2,
    radius: f32,
    color: Color32,
    theme: &SonidoTheme,
) {
    if !theme.reduced_fx {
        painter.circle_filled(center, radius + theme.glow.bloom_radius, bloom_color(color, theme));
    }
    painter.circle_filled(center, radius, color);
}

/// Paint a circle stroke with phosphor bloom.
pub fn glow_circle_stroke(
    painter: &Painter,
    center: Pos2,
    radius: f32,
    color: Color32,
    stroke_width: f32,
    theme: &SonidoTheme,
) {
    if !theme.reduced_fx {
        painter.circle_stroke(
            center,
            radius,
            Stroke::new(stroke_width + theme.glow.bloom_radius * 2.0, bloom_color(color, theme)),
        );
    }
    painter.circle_stroke(center, radius, Stroke::new(stroke_width, color));
}

/// Paint a line segment with phosphor bloom.
pub fn glow_line(
    painter: &Painter,
    start: Pos2,
    end: Pos2,
    color: Color32,
    stroke_width: f32,
    theme: &SonidoTheme,
) {
    if !theme.reduced_fx {
        painter.line_segment(
            [start, end],
            Stroke::new(stroke_width + theme.glow.bloom_radius * 2.0, bloom_color(color, theme)),
        );
    }
    painter.line_segment([start, end], Stroke::new(stroke_width, color));
}

/// Paint a filled rect with phosphor bloom.
pub fn glow_rect(
    painter: &Painter,
    rect: Rect,
    color: Color32,
    corner_radius: f32,
    theme: &SonidoTheme,
) {
    if !theme.reduced_fx {
        let bloomed = rect.expand(theme.glow.bloom_radius);
        painter.rect_filled(bloomed, corner_radius, bloom_color(color, theme));
    }
    painter.rect_filled(rect, corner_radius, color);
}

/// Paint an arc (series of line segments) with phosphor bloom.
///
/// The arc sweeps from `start_angle` to `end_angle` (radians) at the given
/// radius from `center`. Uses 32 segments for smooth appearance.
pub fn glow_arc(
    painter: &Painter,
    center: Pos2,
    radius: f32,
    start_angle: f32,
    end_angle: f32,
    color: Color32,
    stroke_width: f32,
    theme: &SonidoTheme,
) {
    let segments = 32;
    let sweep = end_angle - start_angle;

    let points: Vec<Pos2> = (0..=segments)
        .map(|i| {
            let t = i as f32 / segments as f32;
            let angle = start_angle + t * sweep;
            pos2(center.x + angle.cos() * radius, center.y + angle.sin() * radius)
        })
        .collect();

    // Bloom layer
    if !theme.reduced_fx {
        let bloom_stroke = Stroke::new(
            stroke_width + theme.glow.bloom_radius * 2.0,
            bloom_color(color, theme),
        );
        for window in points.windows(2) {
            painter.line_segment([window[0], window[1]], bloom_stroke);
        }
    }

    // Sharp layer
    let sharp_stroke = Stroke::new(stroke_width, color);
    for window in points.windows(2) {
        painter.line_segment([window[0], window[1]], sharp_stroke);
    }
}

/// Paint scanline texture over a rect.
///
/// Draws faint horizontal lines every `line_spacing` pixels.
pub fn scanlines(painter: &Painter, rect: Rect, theme: &SonidoTheme) {
    if !theme.scanlines.enabled || theme.reduced_fx {
        return;
    }

    let color = Color32::from_white_alpha((theme.scanlines.line_opacity * 255.0) as u8);
    let mut y = rect.top();
    while y < rect.bottom() {
        painter.line_segment(
            [pos2(rect.left(), y), pos2(rect.right(), y)],
            Stroke::new(1.0, color),
        );
        y += theme.scanlines.line_spacing;
    }
}
```

**Step 2: Register module in `mod.rs`**

Add to `crates/sonido-gui-core/src/widgets/mod.rs`:

After `mod morph_bar;` add:
```rust
pub mod glow;
```

Add to re-exports in `lib.rs` — not needed yet since glow functions are called via `crate::widgets::glow::*` from within the crate.

**Step 3: Verify compilation**

```bash
cargo check -p sonido-gui-core
```

**Step 4: Commit**

```bash
git add crates/sonido-gui-core/src/widgets/glow.rs crates/sonido-gui-core/src/widgets/mod.rs
git commit -m "feat(gui-core): glow primitives — bloom circles, arcs, lines, rects, scanlines"
```

---

## Task 3: 7-Segment LED Display (`widgets/led_display.rs`)

**Files:**
- Create: `crates/sonido-gui-core/src/widgets/led_display.rs`
- Modify: `crates/sonido-gui-core/src/widgets/mod.rs`
- Modify: `crates/sonido-gui-core/src/lib.rs`

**Step 1: Create `led_display.rs`**

```rust
//! 7-segment LED display for numeric parameter values.
//!
//! Renders digits as vector-drawn line segments (no font file) with the
//! arcade CRT phosphor glow treatment. Inactive segments render as dim
//! "ghost" traces, mimicking real LED display hardware.

use egui::{Color32, Pos2, Response, Sense, Ui, Widget, pos2, vec2};
use sonido_core::ParamUnit;

use crate::theme::SonidoTheme;
use crate::widgets::glow;

/// Segment bitmask for digits 0-9.
///
/// Bit layout: `0bGFEDCBA` where:
/// - A = top horizontal
/// - B = top-right vertical
/// - C = bottom-right vertical
/// - D = bottom horizontal
/// - E = bottom-left vertical
/// - F = top-left vertical
/// - G = middle horizontal
const DIGIT_SEGMENTS: [u8; 10] = [
    0b0111111, // 0: A B C D E F
    0b0000110, // 1: B C
    0b1011011, // 2: A B D E G
    0b1001111, // 3: A B C D G
    0b1100110, // 4: B C F G
    0b1101101, // 5: A C D F G
    0b1111101, // 6: A C D E F G
    0b0000111, // 7: A B C
    0b1111111, // 8: all
    0b1101111, // 9: A B C D F G
];

/// Maps a character to a segment bitmask, or `None` for unsupported chars.
fn char_segments(c: char) -> Option<u8> {
    match c {
        '0'..='9' => Some(DIGIT_SEGMENTS[(c as u8 - b'0') as usize]),
        '-' => Some(0b1000000),       // G only
        ' ' => Some(0),              // all off (ghost only)
        'd' => Some(0b1011110),       // B C D E G
        'b' => Some(0b1111100),       // C D E F G
        'B' => Some(0b1111100),       // same as b
        'H' => Some(0b1110110),       // B C E F G
        'h' => Some(0b1110100),       // C E F G
        'k' => Some(0b1110100),       // approximation
        'z' => Some(0b1011011),       // same as 2
        ':' => None,                  // special: draw as two dots
        '.' => None,                  // special: draw as single dot
        _ => Some(0),                // unsupported → blank
    }
}

/// 7-segment LED display widget.
///
/// Renders numeric values with phosphor glow and ghost segments.
/// Each digit is drawn as 7 line segments via the painter — no font file.
pub struct LedDisplay {
    text: String,
    color: Option<Color32>,
    digit_count: usize,
    show_ghosts: bool,
}

impl LedDisplay {
    /// Create a new LED display with the given text.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            color: None,
            digit_count: 0, // 0 = auto from text length
            show_ghosts: true,
        }
    }

    /// Set the display color (default: theme amber).
    pub fn color(mut self, color: Color32) -> Self {
        self.color = Some(color);
        self
    }

    /// Set fixed digit count (pads with spaces on the left).
    pub fn digits(mut self, count: usize) -> Self {
        self.digit_count = count;
        self
    }

    /// Format a parameter value with its unit for display.
    pub fn from_value(value: f32, unit: &ParamUnit) -> Self {
        let text = match unit {
            ParamUnit::Decibels => {
                if value >= 0.0 {
                    format!(" {value:.1}dB")
                } else {
                    format!("{value:.1}dB")
                }
            }
            ParamUnit::Hertz => {
                if value >= 1000.0 {
                    format!("{:.1}kHz", value / 1000.0)
                } else {
                    format!("{value:.0}Hz")
                }
            }
            ParamUnit::Milliseconds => {
                if value >= 1000.0 {
                    format!("{:.2} s", value / 1000.0)
                } else {
                    format!("{value:.0}ms")
                }
            }
            ParamUnit::Percent => format!("{value:.0} %"),
            ParamUnit::Ratio => format!("{value:.1}:1"),
            ParamUnit::None => format!("{value:.2}"),
        };
        Self::new(text).digits(7)
    }
}

impl Widget for LedDisplay {
    fn ui(self, ui: &mut Ui) -> Response {
        let theme = SonidoTheme::get(ui.ctx());
        let color = self.color.unwrap_or(theme.colors.amber);
        let ghost_color = glow::ghost(color, &theme);

        let dw = theme.sizing.led_digit_width;
        let dh = theme.sizing.led_digit_height;
        let gap = theme.sizing.led_digit_gap;

        // Determine display text (pad to digit_count if set)
        let display_text = if self.digit_count > 0 && self.text.len() < self.digit_count {
            format!("{:>width$}", self.text, width = self.digit_count)
        } else {
            self.text
        };

        let char_count = display_text.len();
        let total_width = char_count as f32 * (dw + gap) - gap;
        let size = vec2(total_width.max(dw), dh);

        let (rect, response) = ui.allocate_exact_size(size, Sense::hover());

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let mut x = rect.left();

            for ch in display_text.chars() {
                let origin = pos2(x, rect.top());

                match ch {
                    '.' => {
                        // Decimal point: small dot at bottom-right
                        let dot_pos = pos2(x + dw * 0.5, rect.bottom() - 1.0);
                        glow::glow_circle(painter, dot_pos, 1.0, color, &theme);
                        x += gap + 4.0; // narrower than a full digit
                        continue;
                    }
                    ':' => {
                        // Colon: two dots vertically centered
                        let mid_x = x + dw * 0.5;
                        glow::glow_circle(painter, pos2(mid_x, rect.top() + dh * 0.3), 1.0, color, &theme);
                        glow::glow_circle(painter, pos2(mid_x, rect.top() + dh * 0.7), 1.0, color, &theme);
                        x += gap + 6.0;
                        continue;
                    }
                    _ => {}
                }

                let segments = char_segments(ch).unwrap_or(0);
                draw_7seg(painter, origin, dw, dh, segments, color, ghost_color, self.show_ghosts, &theme);
                x += dw + gap;
            }
        }

        response
    }
}

/// Draw a single 7-segment digit at the given origin.
#[allow(clippy::too_many_arguments)]
fn draw_7seg(
    painter: &egui::Painter,
    origin: Pos2,
    w: f32,
    h: f32,
    segments: u8,
    active_color: Color32,
    ghost_color: Color32,
    show_ghosts: bool,
    theme: &SonidoTheme,
) {
    let pad = 1.0; // Padding from digit edges
    let half_h = h / 2.0;
    let stroke_w = 2.0;

    // Segment endpoints: (start, end)
    let seg_lines: [(Pos2, Pos2); 7] = [
        // A: top horizontal
        (pos2(origin.x + pad, origin.y), pos2(origin.x + w - pad, origin.y)),
        // B: top-right vertical
        (pos2(origin.x + w, origin.y + pad), pos2(origin.x + w, origin.y + half_h - pad)),
        // C: bottom-right vertical
        (pos2(origin.x + w, origin.y + half_h + pad), pos2(origin.x + w, origin.y + h - pad)),
        // D: bottom horizontal
        (pos2(origin.x + pad, origin.y + h), pos2(origin.x + w - pad, origin.y + h)),
        // E: bottom-left vertical
        (pos2(origin.x, origin.y + half_h + pad), pos2(origin.x, origin.y + h - pad)),
        // F: top-left vertical
        (pos2(origin.x, origin.y + pad), pos2(origin.x, origin.y + half_h - pad)),
        // G: middle horizontal
        (pos2(origin.x + pad, origin.y + half_h), pos2(origin.x + w - pad, origin.y + half_h)),
    ];

    for (i, &(start, end)) in seg_lines.iter().enumerate() {
        let bit = 1 << i;
        if segments & bit != 0 {
            glow::glow_line(painter, start, end, active_color, stroke_w, theme);
        } else if show_ghosts {
            painter.line_segment([start, end], Stroke::new(stroke_w, ghost_color));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digit_segments_all_defined() {
        for d in 0..10 {
            assert_ne!(DIGIT_SEGMENTS[d], 0, "digit {d} has no segments");
        }
    }

    #[test]
    fn char_segments_digits() {
        assert_eq!(char_segments('0'), Some(0b0111111));
        assert_eq!(char_segments('1'), Some(0b0000110));
        assert_eq!(char_segments('8'), Some(0b1111111));
    }

    #[test]
    fn char_segments_special() {
        assert_eq!(char_segments('-'), Some(0b1000000));
        assert_eq!(char_segments(' '), Some(0));
        assert_eq!(char_segments('.'), None); // special rendering
        assert_eq!(char_segments(':'), None);
    }

    #[test]
    fn from_value_formats() {
        let d = LedDisplay::from_value(3.5, &ParamUnit::Decibels);
        assert!(d.text.contains("3.5"));
        assert!(d.text.contains("dB"));

        let d = LedDisplay::from_value(1200.0, &ParamUnit::Hertz);
        assert!(d.text.contains("1.2"));
        assert!(d.text.contains("kHz"));

        let d = LedDisplay::from_value(50.0, &ParamUnit::Percent);
        assert!(d.text.contains("50"));
    }
}
```

**Step 2: Register in `mod.rs`**

Add after `pub mod glow;`:
```rust
pub mod led_display;
```

Add to re-exports:
```rust
pub use led_display::LedDisplay;
```

**Step 3: Add to `lib.rs` re-exports**

Add `LedDisplay` to the `pub use widgets::{...}` line.

**Step 4: Verify**

```bash
cargo check -p sonido-gui-core && cargo test -p sonido-gui-core
```

**Step 5: Commit**

```bash
git add crates/sonido-gui-core/src/widgets/led_display.rs crates/sonido-gui-core/src/widgets/mod.rs crates/sonido-gui-core/src/lib.rs
git commit -m "feat(gui-core): 7-segment LED display widget with ghost segments and phosphor glow"
```

---

## Task 4: Knob Rework (Pointer-on-Void)

**Files:**
- Modify: `crates/sonido-gui-core/src/widgets/knob.rs`

**Step 1: Rewrite knob rendering**

The interaction logic (drag, shift fine-control, double-click reset) stays identical. Only the `if ui.is_rect_visible(rect)` paint block changes:

- Remove the filled knob body circle (`painter.circle_filled(center, radius - 8.0, body_color)`)
- Replace `draw_arc` calls with `glow::glow_arc` calls using theme colors
- Replace the center dot and pointer with glow versions
- Remove the text-based value display (it will be handled by `LedDisplay` in `bridged_knob`)
- Keep the label but use theme cyan color

Key changes in the rendering block:
- Track arc: `glow::glow_arc(painter, center, radius - 2.0, start_angle, end_angle, theme.colors.dim, 4.0, &theme)` — note using `dim` not a ghost, since the track should be visible
- Value arc: `glow::glow_arc(..., theme.colors.amber, 6.0, &theme)`
- No body fill
- Pointer line: `glow::glow_line(painter, center, pointer_end, theme.colors.amber, 2.0, &theme)`
- Center dot: `glow::glow_circle(painter, center, 2.0, theme.colors.amber, &theme)`
- Label: keep but use `theme.colors.cyan` and `FontId::monospace(11.0)`
- Value text: keep as fallback but use `theme.colors.amber` and `FontId::monospace(11.0)`

Also update the `draw_arc` helper to use `glow::glow_arc` internally, or remove it and call `glow::glow_arc` directly. Removing the private `draw_arc` is cleaner.

**Step 2: Verify**

```bash
cargo check -p sonido-gui-core && cargo test -p sonido-gui-core
```

Existing knob test (`test_knob_default_value`) should still pass — it tests data, not rendering.

**Step 3: Commit**

```bash
git add crates/sonido-gui-core/src/widgets/knob.rs
git commit -m "feat(gui-core): arcade knob — pointer-on-void with phosphor glow arcs"
```

---

## Task 5: Bridged Knob — LED Value Display

**Files:**
- Modify: `crates/sonido-gui-core/src/widgets/bridged_knob.rs`

**Step 1: Update `bridged_knob` to show LED display below knob**

After rendering the knob, add a `LedDisplay::from_value()` below it using the parameter's unit and denormalized value. The knob's own value text rendering (in `knob.rs`) can be removed or kept as fallback — the bridged version overrides it.

The change in `bridged_knob()`:
- After `let response = ui.add(knob);`, add an `LedDisplay` below the knob
- Use `ui.vertical()` to wrap the knob + LED display together
- The label color changes to `theme.colors.cyan`

The format closures in `bridged_knob` that produce strings like `"-3.5 dB"` are no longer needed for the knob's built-in text — the `LedDisplay` handles formatting. But we keep them for backward compat since `Knob` still has a value text fallback.

**Step 2: Update morph markers in `bridged_knob_with_morph`**

Change marker tick colors from hardcoded `Color32::from_rgb(70, 130, 220)` / `Color32::from_rgb(220, 140, 50)` to `theme.colors.cyan` / `theme.colors.amber`. Use `glow::glow_line` for the tick marks.

**Step 3: Verify**

```bash
cargo check -p sonido-gui-core && cargo check -p sonido-gui
```

**Step 4: Commit**

```bash
git add crates/sonido-gui-core/src/widgets/bridged_knob.rs
git commit -m "feat(gui-core): bridged knob shows 7-segment LED value display, cyan labels"
```

---

## Task 6: Level Meter Rework (Segmented LED Bar)

**Files:**
- Modify: `crates/sonido-gui-core/src/widgets/meter.rs`

**Step 1: Rewrite `LevelMeter` rendering**

Replace the smooth bar fill with discrete segments:
- 16 segments for a 120px meter (each ~7px tall with 0.5px gap)
- Each segment is a `glow::glow_rect()` when lit, or drawn at ghost alpha when unlit
- Color per segment based on position: green (0-70%), yellow (70-95%), red (95-100%)
- Peak hold: one segment stays lit, fading alpha over frames (use `ui.ctx().request_repaint()` to animate)
- Clip indicator: top segment blinks (use frame count modulo for 4Hz blink)
- Background: void + `glow::scanlines()`

Replace `GainReductionMeter` similarly but segments light top-down in amber.

Keep all builder methods and tests. Update hardcoded colors to use theme.

**Step 2: Update color threshold tests**

The `level_to_color_thresholds` test uses hardcoded RGB values. Update to use `theme.meter_segment_color()` or remove the test since colors now come from the theme.

**Step 3: Verify**

```bash
cargo check -p sonido-gui-core && cargo test -p sonido-gui-core
```

**Step 4: Commit**

```bash
git add crates/sonido-gui-core/src/widgets/meter.rs
git commit -m "feat(gui-core): segmented LED meter with phosphor glow and peak hold animation"
```

---

## Task 7: Toggle Rework (LED Bloom)

**Files:**
- Modify: `crates/sonido-gui-core/src/widgets/toggle.rs`

**Step 1: Rework `BypassToggle`**

- OFF: `glow::glow_circle_stroke()` in `theme.colors.dim`
- ON: `glow::glow_circle()` in `theme.colors.green` with bloom
- Hover: increase bloom radius by `hover_bloom_mult`
- Label: `FontId::monospace(12.0)`, cyan when active, dim when inactive

**Step 2: Rework `FootswitchToggle`**

- Body: void fill with 1px dim border
- LED dot: `glow::glow_circle()` — green with bloom when ON, ghost when OFF
- Label: `FontId::monospace(10.0)`

**Step 3: Update toggle tests**

Tests check field values, not colors — should pass unchanged.

**Step 4: Verify**

```bash
cargo check -p sonido-gui-core && cargo test -p sonido-gui-core
```

**Step 5: Commit**

```bash
git add crates/sonido-gui-core/src/widgets/toggle.rs
git commit -m "feat(gui-core): arcade toggles — LED bloom on bypass and footswitch"
```

---

## Task 8: Morph Bar Rework (Segment Crossfade)

**Files:**
- Modify: `crates/sonido-gui-core/src/widgets/morph_bar.rs`

**Step 1: Rework snapshot buttons**

- Captured: `glow::glow_circle()` with category color (A = cyan, B = amber)
- Empty: ghost circle outline
- Label: `FontId::monospace(10.0)`

**Step 2: Rework slider**

Replace `egui::Slider` with custom-painted horizontal LED segment bar:
- 20 segments across the available width
- Each segment's color is `Color32::lerp()` from cyan to amber based on its position
- Segments up to slider position are lit, rest are ghost
- Disabled state: all ghost

This requires manual drag handling (allocate rect, track mouse delta) since we're replacing egui's built-in slider.

**Step 3: Verify**

```bash
cargo check -p sonido-gui-core && cargo check -p sonido-gui
```

**Step 4: Commit**

```bash
git add crates/sonido-gui-core/src/widgets/morph_bar.rs
git commit -m "feat(gui-core): morph bar — LED segment crossfade from cyan (A) to amber (B)"
```

---

## Task 9: Effect Panel Frame Template

**Files:**
- Modify: `crates/sonido-gui/src/app.rs` (the effect panel rendering section)

**Step 1: Rework effect panel frame**

In `app.rs`, where effect panels are rendered inside a `Frame`, change:
- Frame fill: `theme.colors.void` (instead of panel_bg)
- Frame stroke: 1px in `theme.colors.amber` — use `glow::glow_rect` or draw the border manually with bloom
- Title: effect name in Share Tech Mono (monospace), `theme.colors.amber`
- Interior: add `glow::scanlines()` over the panel rect

**Step 2: Verify visually**

```bash
cargo run -p sonido-gui
```

Spot-check: panels should have amber-bordered frames on void with scanline texture. Knobs should glow amber, labels should be cyan, meters should be segmented.

**Step 3: Commit**

```bash
git add crates/sonido-gui/src/app.rs
git commit -m "feat(gui): arcade effect panel frames — amber border with glow, void + scanlines"
```

---

## Task 10: App Chrome — Header, Status Bar, I/O Columns

**Files:**
- Modify: `crates/sonido-gui/src/app.rs`

**Step 1: Rework header**

- "SONIDO" text: `FontId::monospace(18.0)`, `theme.colors.amber`, with manually-painted bloom behind it (paint amber text, then paint same text at bloom_alpha offset)
- Preset combo: amber-styled
- Audio status: `glow::glow_circle()` — green when OK, red when error

**Step 2: Rework status bar**

- BYPASS button: large `glow::glow_circle()` in red when active
- Sample rate / buffer size / latency: `LedDisplay` widgets in amber
- CPU meter: `LedDisplay` in green (< 80%), yellow (80-100%), red (> 100%)
- CPU sparkline: draw with `glow::glow_line()` in green — produces oscilloscope trace look

**Step 3: Rework I/O columns**

- "INPUT" / "OUTPUT" labels: `FontId::monospace(12.0)`, `theme.colors.cyan`
- Level meters already reworked in Task 6
- Gain knobs already reworked in Task 4

**Step 4: Verify visually**

```bash
cargo run -p sonido-gui
```

**Step 5: Commit**

```bash
git add crates/sonido-gui/src/app.rs
git commit -m "feat(gui): arcade app chrome — amber header, LED status bar, cyan I/O labels"
```

---

## Task 11: Graph Editor Node Styling

**Files:**
- Modify: `crates/sonido-gui/src/graph_view.rs`

**Step 1: Update node rendering in SnarlViewer**

- Node border: 1px in category color with bloom (use `glow::glow_rect` for node rect outline)
- Node interior: void fill
- Node title text: `FontId::monospace(11.0)` in category color
- Selected node: double bloom radius, full-intensity border
- Wire color: source node's category color via `glow::glow_line()`

Update the category color constants to match the design spec (cyan for dynamics, red for distortion, etc.).

**Step 2: Update context menu styling**

- Void background on dropdown
- Category sub-menu items in category colors
- Amber text for effect names

**Step 3: Verify visually**

```bash
cargo run -p sonido-gui
```

**Step 4: Commit**

```bash
git add crates/sonido-gui/src/graph_view.rs
git commit -m "feat(gui): arcade graph editor — category-colored glow nodes and wires"
```

---

## Task 12: Effect Panel Color Updates (Mechanical — 19 files)

**Files:**
- Modify: `crates/sonido-gui-core/src/effects_ui/*.rs` (all 19 files)

**Step 1: Audit all 19 panels for hardcoded colors**

Most panels only call `bridged_knob`, `bypass_toggle`, and `bridged_combo` — which are already updated. But some panels may have:
- Hardcoded `ui.label()` calls with custom colors
- `ui.add_space()` values that need adjustment
- Direct `Color32::from_rgb()` usage

Search for hardcoded colors:
```bash
grep -n "Color32::from_rgb" crates/sonido-gui-core/src/effects_ui/*.rs
grep -n "proportional\|FontId" crates/sonido-gui-core/src/effects_ui/*.rs
```

**Step 2: Replace any hardcoded colors with theme references**

For each panel, replace:
- `Color32::from_rgb(...)` → `SonidoTheme::get(ui.ctx()).colors.xxx`
- `FontId::proportional(...)` → `FontId::monospace(...)`
- `ui.label("Type:")` style labels → use theme cyan color

Most panels won't need changes since they delegate entirely to `bridged_knob` etc.

**Step 3: Verify**

```bash
cargo check -p sonido-gui-core && cargo test -p sonido-gui-core
```

**Step 4: Commit**

```bash
git add crates/sonido-gui-core/src/effects_ui/
git commit -m "refactor(gui-core): effect panels use theme colors instead of hardcoded RGB"
```

---

## Task 13: Documentation Updates

**Files:**
- Modify: `docs/GUI.md`
- Modify: `docs/ARCHITECTURE.md`
- Modify: `docs/CHANGELOG.md`

**Step 1: Update `docs/GUI.md`**

Add a "Design System" section describing:
- Arcade CRT aesthetic, color palette table, typography choices
- Component inventory (knob, meter, toggle, LED display, morph bar, glow primitives)
- `SonidoTheme` struct and how widgets access it

**Step 2: Update `docs/ARCHITECTURE.md`**

Update the sonido-gui-core section to mention the new widget/theme structure.

**Step 3: Add changelog entry**

```markdown
## [Unreleased]

### Added
- Arcade CRT design system: phosphor glow, 7-segment LED displays, void backgrounds
- `SonidoTheme` struct — single source of truth for all visual parameters
- `glow.rs` — reusable phosphor bloom/scanline painting primitives
- `LedDisplay` — 7-segment numeric display widget
- Share Tech Mono font bundled for arcade typography

### Changed
- Knob: pointer-on-void style with glow arcs (removed filled body)
- Level meter: segmented LED bar with peak hold animation
- Toggles: LED bloom on bypass and footswitch indicators
- Morph bar: LED segment crossfade from cyan (A) to amber (B)
- All colors: amber-dominant CRT palette replaces blue-accent flat UI
- All text: Share Tech Mono replaces system proportional font
```

**Step 4: Commit**

```bash
git add docs/GUI.md docs/ARCHITECTURE.md docs/CHANGELOG.md
git commit -m "docs: arcade UI design system — GUI, architecture, changelog"
```

---

## Task 14: Final Visual Smoke Test + Cleanup

**Step 1: Run the standalone GUI**

```bash
cargo run -p sonido-gui --release
```

Check:
- [ ] Void background, no old blue/gray colors visible
- [ ] Knobs show amber arcs with glow, pointer-on-void, LED readout below
- [ ] Labels are cyan Share Tech Mono
- [ ] Meters are segmented with green/yellow/red glow
- [ ] Bypass toggles have green LED bloom
- [ ] "SONIDO" header is amber
- [ ] Status bar shows LED-styled digits
- [ ] Graph editor nodes have category-colored glow borders
- [ ] Scanlines visible on panel backgrounds
- [ ] Effect panels have amber border frames
- [ ] Morph bar has cyan/amber LED segments

**Step 2: WASM compile check**

```bash
cargo check --target wasm32-unknown-unknown -p sonido-gui
```

**Step 3: Plugin compile check**

```bash
cargo check -p sonido-plugin
```

**Step 4: Run all tests**

```bash
cargo test -p sonido-gui-core && cargo test -p sonido-gui
```

**Step 5: Fix any remaining hardcoded colors**

```bash
grep -rn "from_rgb" crates/sonido-gui-core/src/ | grep -v "test" | grep -v "theme.rs"
```

Any remaining `Color32::from_rgb()` outside theme.rs and test code should be replaced with theme references.

**Step 6: Final commit**

```bash
git add -A
git commit -m "fix(gui): cleanup remaining hardcoded colors, verify arcade UI across all targets"
```
