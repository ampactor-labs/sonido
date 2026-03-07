//! Level meter widgets for audio visualization.
//!
//! Renders segmented LED-bar meters with CRT phosphor glow. Each meter is
//! divided into 16 discrete segments colored via [`SonidoTheme::meter_segment_color`].
//! Active segments use [`glow::glow_rect`] for bloom; inactive segments are
//! drawn at ghost intensity. Peak hold is displayed as a single lit segment
//! with time-fading alpha.

use egui::{Rect, Response, Sense, Stroke, StrokeKind, Ui, Widget, pos2, vec2};

use crate::theme::SonidoTheme;
use crate::widgets::glow;

/// Number of discrete LED segments in each meter.
const SEGMENT_COUNT: usize = 16;

/// Gap between segments in pixels.
const SEGMENT_GAP: f32 = 0.5;

/// VU-style segmented level meter with peak hold.
///
/// ## Parameters
/// - `peak`: Peak level, normalized 0.0–1.5 (clamped). Values > 1.0 trigger clip indicator.
/// - `rms`: RMS level, normalized 0.0–1.5 (clamped). Drives the main bar fill.
/// - `label`: Optional text label drawn below the meter.
/// - `width`: Meter width in pixels (default 24.0).
/// - `height`: Meter height in pixels (default 120.0).
/// - `horizontal`: If true, meter draws left-to-right instead of bottom-to-top.
pub struct LevelMeter {
    peak: f32,
    rms: f32,
    label: String,
    width: f32,
    height: f32,
    horizontal: bool,
}

impl LevelMeter {
    /// Create a new level meter.
    ///
    /// `peak` and `rms` are clamped to 0.0–1.5.
    pub fn new(peak: f32, rms: f32) -> Self {
        Self {
            peak: peak.clamp(0.0, 1.5),
            rms: rms.clamp(0.0, 1.5),
            label: String::new(),
            width: 24.0,
            height: 120.0,
            horizontal: false,
        }
    }

    /// Set the label.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    /// Set dimensions.
    pub fn size(mut self, width: f32, height: f32) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    /// Make horizontal instead of vertical.
    pub fn horizontal(self) -> Self {
        Self {
            horizontal: true,
            ..self
        }
    }

    /// Paint segmented LED meter along the primary axis.
    ///
    /// Segments fill from the origin (bottom for vertical, left for horizontal).
    /// Each segment's color is determined by its normalized position via
    /// [`SonidoTheme::meter_segment_color`]. Active segments get phosphor bloom,
    /// inactive segments are drawn at ghost intensity.
    fn paint_segments(
        &self,
        painter: &egui::Painter,
        inner: Rect,
        theme: &SonidoTheme,
    ) {
        let axis_length = if self.horizontal {
            inner.width()
        } else {
            inner.height()
        };

        let total_gaps = (SEGMENT_COUNT - 1) as f32 * SEGMENT_GAP;
        let seg_size = (axis_length - total_gaps) / SEGMENT_COUNT as f32;
        let level = self.rms.min(1.0);

        // Determine which segment the peak sits on (for peak hold)
        let peak_seg = if self.peak > 0.01 {
            let p = self.peak.min(1.0);
            let idx = (p * SEGMENT_COUNT as f32).ceil() as usize;
            Some(idx.min(SEGMENT_COUNT).saturating_sub(1))
        } else {
            None
        };

        for i in 0..SEGMENT_COUNT {
            // Normalized position of this segment's top edge (0.0 = bottom, 1.0 = top)
            let seg_position = (i as f32 + 1.0) / SEGMENT_COUNT as f32;
            let seg_bottom_pos = i as f32 / SEGMENT_COUNT as f32;

            let color = theme.meter_segment_color(seg_position);

            // Compute segment rect along the primary axis
            let seg_rect = if self.horizontal {
                let x = inner.left() + i as f32 * (seg_size + SEGMENT_GAP);
                Rect::from_min_size(pos2(x, inner.top()), vec2(seg_size, inner.height()))
            } else {
                // Vertical: segment 0 is at the bottom
                let y = inner.bottom() - (i as f32 + 1.0) * seg_size - i as f32 * SEGMENT_GAP;
                Rect::from_min_size(pos2(inner.left(), y), vec2(inner.width(), seg_size))
            };

            let is_active = level > seg_bottom_pos;
            let is_peak = peak_seg == Some(i);

            if is_active {
                // Lit segment with phosphor bloom
                glow::glow_rect(painter, seg_rect, color, 1.0, theme);
            } else if is_peak {
                // Peak hold: single lit segment with reduced alpha (fading hold)
                let peak_color = color.gamma_multiply(0.7);
                glow::glow_rect(painter, seg_rect, peak_color, 1.0, theme);
            } else {
                // Ghost (inactive) segment
                let ghost_color = glow::ghost(color, theme);
                painter.rect_filled(seg_rect, 1.0, ghost_color);
            }
        }
    }
}

impl Widget for LevelMeter {
    fn ui(self, ui: &mut Ui) -> Response {
        let theme = SonidoTheme::get(ui.ctx());
        let extra_height = if self.label.is_empty() { 0.0 } else { 18.0 };
        let size = if self.horizontal {
            vec2(self.height, self.width + extra_height)
        } else {
            vec2(self.width, self.height + extra_height)
        };

        let (rect, response) = ui.allocate_exact_size(size, Sense::hover());

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();

            // Meter area (excluding label)
            let meter_rect = if self.label.is_empty() {
                rect
            } else if self.horizontal {
                Rect::from_min_size(rect.min, vec2(self.height, self.width))
            } else {
                Rect::from_min_size(rect.min, vec2(self.width, self.height))
            };

            // Background — void
            painter.rect_filled(meter_rect, 2.0, theme.colors.void);

            // Border
            painter.rect_stroke(
                meter_rect,
                2.0,
                Stroke::new(1.0, theme.colors.dim),
                StrokeKind::Inside,
            );

            // Inner padding
            let inner = meter_rect.shrink(2.0);

            // Segmented LED bar
            self.paint_segments(painter, inner, &theme);

            // Clip indicator (when peak > 1.0)
            if self.peak > 1.0 {
                if self.horizontal {
                    let clip_rect = Rect::from_min_size(
                        pos2(inner.right() - 4.0, inner.top()),
                        vec2(4.0, inner.height()),
                    );
                    glow::glow_rect(painter, clip_rect, theme.colors.red, 0.0, &theme);
                } else {
                    let clip_rect = Rect::from_min_size(inner.min, vec2(inner.width(), 4.0));
                    glow::glow_rect(painter, clip_rect, theme.colors.red, 0.0, &theme);
                }
            }

            // Label below
            if !self.label.is_empty() {
                let label_pos = pos2(rect.center().x, meter_rect.bottom() + 4.0);
                painter.text(
                    label_pos,
                    egui::Align2::CENTER_TOP,
                    &self.label,
                    egui::FontId::proportional(11.0),
                    theme.colors.text_secondary,
                );
            }
        }

        response
    }
}

/// Gain reduction meter for compressor display.
///
/// Displays gain reduction as a segmented LED bar that lights top-down in amber.
/// Active segments use phosphor bloom; inactive segments are drawn at ghost intensity.
///
/// ## Parameters
/// - `reduction_db`: Gain reduction in dB (positive values, e.g. 6.0 = 6 dB reduction).
/// - `max_reduction`: Maximum displayed reduction in dB (default 20.0).
/// - `width`: Meter width in pixels (default 24.0).
/// - `height`: Meter height in pixels (default 80.0).
pub struct GainReductionMeter {
    reduction_db: f32,
    max_reduction: f32,
    width: f32,
    height: f32,
}

impl GainReductionMeter {
    /// Create a new gain reduction meter.
    ///
    /// `reduction_db` should be positive (e.g., 6.0 means 6dB of gain reduction).
    pub fn new(reduction_db: f32) -> Self {
        Self {
            reduction_db: reduction_db.max(0.0),
            max_reduction: 20.0,
            width: 24.0,
            height: 80.0,
        }
    }

    /// Set maximum displayed reduction.
    pub fn max_reduction(mut self, max: f32) -> Self {
        self.max_reduction = max;
        self
    }

    /// Set dimensions.
    pub fn size(mut self, width: f32, height: f32) -> Self {
        self.width = width;
        self.height = height;
        self
    }
}

impl Widget for GainReductionMeter {
    fn ui(self, ui: &mut Ui) -> Response {
        let theme = SonidoTheme::get(ui.ctx());
        let size = vec2(self.width, self.height + 18.0);
        let (rect, response) = ui.allocate_exact_size(size, Sense::hover());

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();

            let meter_rect = Rect::from_min_size(rect.min, vec2(self.width, self.height));

            // Background — void
            painter.rect_filled(meter_rect, 2.0, theme.colors.void);
            painter.rect_stroke(
                meter_rect,
                2.0,
                Stroke::new(1.0, theme.colors.dim),
                StrokeKind::Inside,
            );

            let inner = meter_rect.shrink(2.0);
            let axis_length = inner.height();
            let total_gaps = (SEGMENT_COUNT - 1) as f32 * SEGMENT_GAP;
            let seg_size = (axis_length - total_gaps) / SEGMENT_COUNT as f32;

            // Normalized GR level (0.0 = no reduction, 1.0 = max_reduction)
            let normalized = (self.reduction_db / self.max_reduction).min(1.0);
            let amber = theme.colors.amber;
            let ghost_amber = glow::ghost(amber, &theme);

            // GR segments light top-down: segment 0 = topmost
            for i in 0..SEGMENT_COUNT {
                let seg_position = i as f32 / SEGMENT_COUNT as f32;
                let y = inner.top() + i as f32 * (seg_size + SEGMENT_GAP);
                let seg_rect =
                    Rect::from_min_size(pos2(inner.left(), y), vec2(inner.width(), seg_size));

                let is_active = normalized > seg_position;
                if is_active {
                    glow::glow_rect(painter, seg_rect, amber, 1.0, &theme);
                } else {
                    painter.rect_filled(seg_rect, 1.0, ghost_amber);
                }
            }

            // Label
            let label_pos = pos2(rect.center().x, meter_rect.bottom() + 4.0);
            painter.text(
                label_pos,
                egui::Align2::CENTER_TOP,
                "GR",
                egui::FontId::proportional(11.0),
                theme.colors.text_secondary,
            );
        }

        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_meter_defaults() {
        let meter = LevelMeter::new(0.8, 0.5);
        assert_eq!(meter.peak, 0.8);
        assert_eq!(meter.rms, 0.5);
        assert_eq!(meter.width, 24.0);
        assert_eq!(meter.height, 120.0);
        assert!(!meter.horizontal);
        assert!(meter.label.is_empty());
    }

    #[test]
    fn level_meter_clamps_inputs() {
        let meter = LevelMeter::new(5.0, -1.0);
        assert_eq!(meter.peak, 1.5);
        assert_eq!(meter.rms, 0.0);
    }

    #[test]
    fn level_meter_builder() {
        let meter = LevelMeter::new(0.5, 0.3)
            .label("L")
            .size(32.0, 200.0)
            .horizontal();
        assert_eq!(meter.label, "L");
        assert_eq!(meter.width, 32.0);
        assert_eq!(meter.height, 200.0);
        assert!(meter.horizontal);
    }

    #[test]
    fn meter_segment_color_thresholds() {
        // Verify via theme — colors come from SonidoTheme::meter_segment_color
        let theme = SonidoTheme::default();
        let green = theme.meter_segment_color(0.5);
        let yellow = theme.meter_segment_color(0.8);
        let red = theme.meter_segment_color(1.0);
        assert_eq!(green, theme.colors.green);
        assert_eq!(yellow, theme.colors.yellow);
        assert_eq!(red, theme.colors.red);
    }

    #[test]
    fn segment_count_and_gap() {
        assert_eq!(SEGMENT_COUNT, 16);
        assert!((SEGMENT_GAP - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn gain_reduction_meter_defaults() {
        let meter = GainReductionMeter::new(6.0);
        assert_eq!(meter.reduction_db, 6.0);
        assert_eq!(meter.max_reduction, 20.0);
        assert_eq!(meter.width, 24.0);
        assert_eq!(meter.height, 80.0);
    }

    #[test]
    fn gain_reduction_meter_clamps_negative() {
        let meter = GainReductionMeter::new(-3.0);
        assert_eq!(meter.reduction_db, 0.0);
    }

    #[test]
    fn gain_reduction_meter_builder() {
        let meter = GainReductionMeter::new(10.0)
            .max_reduction(30.0)
            .size(16.0, 60.0);
        assert_eq!(meter.max_reduction, 30.0);
        assert_eq!(meter.width, 16.0);
        assert_eq!(meter.height, 60.0);
    }
}
