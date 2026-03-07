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
        visuals.widgets.noninteractive.fg_stroke =
            Stroke::new(1.0, self.colors.text_secondary);
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
