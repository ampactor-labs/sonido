//! Arcade CRT visual theme for the Sonido GUI.
//!
//! All colors, sizing, glow parameters, and scanline config live in
//! [`SonidoTheme`]. Widgets read this from `egui::Context::data()`.
//! The theme produces a CRT phosphor aesthetic: amber-dominant colors,
//! bloom/glow on active elements, void backgrounds, and scanline textures.

use egui::{
    Color32, Context, CornerRadius, FontDefinitions, FontFamily, Id, Stroke, Style, Vec2, Visuals,
    vec2,
};

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
    /// Responsive layout ratios and clamps.
    pub layout: ThemeLayout,
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

/// Responsive layout ratios and clamps.
///
/// All sizing derives from available space using these ratios, with min/max
/// constraints. No hardcoded pixel values in layout code.
#[derive(Clone, Debug)]
pub struct ThemeLayout {
    /// I/O strip width as fraction of window width.
    pub io_strip_ratio: f32,
    /// Minimum I/O strip width in pixels.
    pub io_strip_min: f32,
    /// Maximum I/O strip width in pixels.
    pub io_strip_max: f32,
    /// Graph editor height as fraction of content area.
    pub graph_ratio: f32,
    /// Minimum graph editor height in pixels.
    pub graph_min_h: f32,
    /// Maximum effect panel height as fraction of content area.
    pub panel_max_ratio: f32,
    /// Minimum effect panel height in pixels.
    pub panel_min_h: f32,
    /// Minimum fader width in pixels.
    pub fader_min_w: f32,
    /// Maximum fader width in pixels.
    pub fader_max_w: f32,
    /// Minimum fader height in pixels.
    pub fader_min_h: f32,
    /// Maximum fader height in pixels.
    pub fader_max_h: f32,
}

impl Default for SonidoTheme {
    fn default() -> Self {
        Self {
            colors: ThemeColors::default(),
            sizing: ThemeSizing::default(),
            glow: GlowConfig::default(),
            scanlines: ScanlineConfig::default(),
            layout: ThemeLayout::default(),
            reduced_fx: false,
        }
    }
}

impl Default for ThemeColors {
    fn default() -> Self {
        Self {
            // Polybius arcade CRT phosphors — saturated, high-contrast
            amber: Color32::from_rgb(255, 176, 32),
            green: Color32::from_rgb(32, 255, 96),
            cyan: Color32::from_rgb(32, 210, 255),
            red: Color32::from_rgb(255, 48, 48),
            magenta: Color32::from_rgb(255, 48, 160),
            yellow: Color32::from_rgb(255, 210, 32),
            purple: Color32::from_rgb(160, 80, 255),
            dim: Color32::from_rgb(28, 28, 38),
            void: Color32::from_rgb(4, 4, 8),
            text_primary: Color32::from_rgb(220, 220, 228),
            text_secondary: Color32::from_rgb(100, 115, 120),
        }
    }
}

impl Default for ThemeSizing {
    fn default() -> Self {
        Self {
            knob_diameter: 60.0,
            meter_width: 24.0,
            meter_height: 120.0,
            led_digit_width: 18.0,
            led_digit_height: 28.0,
            led_digit_gap: 3.0,
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
            bloom_alpha: 0.25,
            ghost_alpha: 0.08,
            hover_bloom_mult: 1.6,
        }
    }
}

impl Default for ScanlineConfig {
    fn default() -> Self {
        Self {
            line_spacing: 3.0,
            line_opacity: 0.05,
            enabled: true,
        }
    }
}

impl Default for ThemeLayout {
    fn default() -> Self {
        Self {
            io_strip_ratio: 0.07,
            io_strip_min: 50.0,
            io_strip_max: 80.0,
            graph_ratio: 0.45,
            graph_min_h: 150.0,
            panel_max_ratio: 0.50,
            panel_min_h: 120.0,
            fader_min_w: 32.0,
            fader_max_w: 52.0,
            fader_min_h: 60.0,
            fader_max_h: 120.0,
        }
    }
}

impl ThemeLayout {
    /// Compute I/O strip width from window width.
    pub fn io_strip_width(&self, window_w: f32) -> f32 {
        (window_w * self.io_strip_ratio).clamp(self.io_strip_min, self.io_strip_max)
    }

    /// Compute graph and panel heights from available content height.
    ///
    /// Returns `(graph_h, panel_h)`.
    pub fn split_vertical(&self, content_h: f32, panel_content_h: f32) -> (f32, f32) {
        let panel_max = content_h * self.panel_max_ratio;
        let panel_h = panel_content_h.clamp(self.panel_min_h, panel_max);
        let graph_h = (content_h - panel_h).max(self.graph_min_h);
        (graph_h, panel_h)
    }

    /// Compute fader width for `param_count` params in available width.
    pub fn fader_width(&self, available_w: f32, param_count: usize) -> f32 {
        if param_count == 0 {
            return self.fader_max_w;
        }
        let w = available_w / param_count as f32;
        w.clamp(self.fader_min_w, self.fader_max_w)
    }

    /// Compute fader height from available panel height minus labels.
    pub fn fader_height(&self, panel_inner_h: f32) -> f32 {
        let label_space = 32.0;
        (panel_inner_h - label_space).clamp(self.fader_min_h, self.fader_max_h)
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
            egui::FontData::from_static(font_data).into(),
        );
        // Primary for both families — the arcade look uses mono everywhere
        fonts
            .families
            .get_mut(&FontFamily::Monospace)
            .unwrap()
            .insert(0, "share_tech_mono".to_owned());
        fonts
            .families
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

        visuals.widgets.hovered.bg_fill = Color32::from_rgb(35, 30, 15);
        visuals.widgets.hovered.fg_stroke = Stroke::new(1.5, self.colors.amber);
        visuals.widgets.hovered.corner_radius = CornerRadius::same(4);

        visuals.widgets.active.bg_fill = Color32::from_rgb(45, 38, 12);
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

#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn io_strip_width_clamps() {
        let layout = ThemeLayout::default();
        assert_eq!(layout.io_strip_width(400.0), 50.0); // hits min
        let w = layout.io_strip_width(1000.0);
        assert!((w - 70.0).abs() < 0.1); // proportional
        assert_eq!(layout.io_strip_width(2000.0), 80.0); // hits max
    }

    #[test]
    fn split_vertical_respects_min_graph() {
        let layout = ThemeLayout::default();
        let (graph_h, _) = layout.split_vertical(300.0, 200.0);
        assert!(graph_h >= layout.graph_min_h);
    }

    #[test]
    fn fader_width_distributes_evenly() {
        let layout = ThemeLayout::default();
        let w = layout.fader_width(400.0, 8);
        assert_eq!(w, 50.0); // 400/8 = 50, within [32, 52]
    }

    #[test]
    fn fader_width_clamps_to_min() {
        let layout = ThemeLayout::default();
        let w = layout.fader_width(200.0, 20);
        assert_eq!(w, 32.0);
    }
}
