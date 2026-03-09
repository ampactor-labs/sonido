//! Level meter widgets for audio visualization.
//!
//! Provides two meter types:
//!
//! - [`LevelMeter`] — Continuous dual-bar (RMS + peak) meter with dB scale,
//!   styled after Ableton Live's channel meters. The RMS level drives a filled
//!   bar colored by threshold (green/yellow/red via
//!   [`SonidoTheme::meter_segment_color`]), while peak is shown as a thin
//!   horizontal line. An optional latching clip indicator lights at the top
//!   when peak exceeds 0 dBFS.
//!
//! - [`GainReductionMeter`] — Segmented LED-bar meter for compressor gain
//!   reduction display. Lights top-down in amber with phosphor bloom.

use egui::{Rect, Response, Sense, Stroke, StrokeKind, Ui, Widget, pos2, vec2};

use crate::theme::SonidoTheme;
use crate::widgets::glow;

/// Number of discrete LED segments in the gain reduction meter.
const SEGMENT_COUNT: usize = 16;

/// Gap between segments in pixels (gain reduction meter).
const SEGMENT_GAP: f32 = 0.5;

/// dB scale tick marks: (linear level, label text).
///
/// Positions are pre-computed from `10^(dB/20)`:
/// - 0 dB = 1.000
/// - -6 dB = 0.501
/// - -12 dB = 0.251
/// - -18 dB = 0.126
/// - -24 dB = 0.063
const DB_MARKS: &[(f32, &str)] = &[
    (1.000, "0"),
    (0.501, "-6"),
    (0.251, "-12"),
    (0.126, "-18"),
    (0.063, "-24"),
];

/// Continuous dual-bar level meter with dB scale and clip indicator.
///
/// Renders an RMS bar (filled rectangle) and a peak line overlaid on a void
/// background. A dB scale with tick marks is drawn to the left of the bar.
/// When peak exceeds 0 dBFS, a clip indicator circle appears at the top of the
/// meter.
///
/// ## Parameters
/// - `peak`: Peak level, normalized 0.0-1.5 (clamped). Values > 1.0 trigger clip indicator.
/// - `rms`: RMS level, normalized 0.0-1.5 (clamped). Drives the main bar fill.
/// - `label`: Optional text label drawn below the meter.
/// - `width`: Total meter width in pixels (default 24.0). Bar occupies the right ~50%.
/// - `height`: Meter height in pixels (default 120.0).
/// - `horizontal`: If true, meter draws left-to-right instead of bottom-to-top.
/// - `clip_latched`: Optional mutable reference to a clip latch flag. When `Some`, the clip
///   indicator stays lit after peak > 1.0 until the meter is clicked. When `None`, clip
///   indicator blinks momentarily.
pub struct LevelMeter<'a> {
    peak: f32,
    rms: f32,
    label: String,
    width: f32,
    height: f32,
    horizontal: bool,
    clip_latched: Option<&'a mut bool>,
}

impl<'a> LevelMeter<'a> {
    /// Create a new level meter.
    ///
    /// `peak` and `rms` are clamped to 0.0-1.5.
    pub fn new(peak: f32, rms: f32) -> Self {
        Self {
            peak: peak.clamp(0.0, 1.5),
            rms: rms.clamp(0.0, 1.5),
            label: String::new(),
            width: 24.0,
            height: 120.0,
            horizontal: false,
            clip_latched: None,
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

    /// Attach a clip latch flag for persistent clip indication.
    ///
    /// When provided, the clip indicator stays lit after peak exceeds 1.0
    /// until the user clicks the meter to reset it.
    pub fn clip_latch(mut self, latched: &'a mut bool) -> Self {
        self.clip_latched = Some(latched);
        self
    }
}

impl Widget for LevelMeter<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let theme = SonidoTheme::get(ui.ctx());
        let extra_height = if self.label.is_empty() { 0.0 } else { 18.0 };
        let size = if self.horizontal {
            vec2(self.height, self.width + extra_height)
        } else {
            vec2(self.width, self.height + extra_height)
        };

        let (rect, response) = ui.allocate_exact_size(size, Sense::click());

        // Handle clip latch reset on click
        let mut clip_latched = self.clip_latched;
        if response.clicked() {
            if let Some(ref mut latched) = clip_latched {
                **latched = false;
            }
        }

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

            // Background — void (full meter area)
            painter.rect_filled(meter_rect, 2.0, theme.colors.void);

            if self.horizontal {
                // Simplified horizontal fallback: continuous bar without dB scale
                let inner = meter_rect.shrink(2.0);
                let rms_level = self.rms.min(1.0);
                let peak_level = self.peak.min(1.0);

                // Border around bar area
                painter.rect_stroke(
                    meter_rect,
                    2.0,
                    Stroke::new(1.0, theme.colors.dim),
                    StrokeKind::Inside,
                );

                // RMS bar
                if rms_level > 0.001 {
                    let bar_width = inner.width() * rms_level;
                    let bar_rect = Rect::from_min_size(inner.min, vec2(bar_width, inner.height()));
                    let color = theme.meter_segment_color(rms_level);
                    painter.rect_filled(bar_rect, 0.0, color);
                }

                // Peak line
                if peak_level > 0.01 {
                    let peak_x = inner.left() + inner.width() * peak_level;
                    painter.line_segment(
                        [pos2(peak_x, inner.top()), pos2(peak_x, inner.bottom())],
                        Stroke::new(1.0, theme.colors.text_primary),
                    );
                }
            } else {
                // Vertical layout: dB labels on the left, bar on the right

                let inner = meter_rect.shrink(2.0);

                // Partition: ~55% right for bar, ~45% left for dB labels
                let bar_fraction = 0.55;
                let bar_width = inner.width() * bar_fraction;
                let label_width = inner.width() - bar_width;

                let bar_left = inner.right() - bar_width;
                let bar_rect = Rect::from_min_max(pos2(bar_left, inner.top()), inner.max);

                // Border around bar area only
                painter.rect_stroke(
                    bar_rect,
                    1.0,
                    Stroke::new(1.0, theme.colors.dim),
                    StrokeKind::Inside,
                );

                let bar_inner = bar_rect.shrink(1.0);
                let rms_level = self.rms.min(1.0);
                let peak_level = self.peak.min(1.0);

                // RMS bar — continuous filled rect from bottom to RMS level
                if rms_level > 0.001 {
                    let bar_height = bar_inner.height() * rms_level;
                    let rms_rect = Rect::from_min_max(
                        pos2(bar_inner.left(), bar_inner.bottom() - bar_height),
                        bar_inner.max,
                    );
                    // Color based on highest lit position
                    let color = theme.meter_segment_color(rms_level);
                    painter.rect_filled(rms_rect, 0.0, color);
                }

                // Peak line — 1px horizontal line at peak position
                if peak_level > 0.01 {
                    let peak_y = bar_inner.bottom() - bar_inner.height() * peak_level;
                    painter.line_segment(
                        [
                            pos2(bar_inner.left(), peak_y),
                            pos2(bar_inner.right(), peak_y),
                        ],
                        Stroke::new(1.0, theme.colors.text_primary),
                    );
                }

                // dB scale markings — tick marks + labels on the left side
                let font_size = (bar_width * 0.35).clamp(7.0, 9.0);
                let font_id = egui::FontId::proportional(font_size);
                let tick_right = bar_left - 1.0;
                let tick_len = 3.0;

                for &(linear, label_text) in DB_MARKS {
                    let y = bar_inner.bottom() - bar_inner.height() * linear;
                    // Only draw if within visible range
                    if y >= bar_inner.top() && y <= bar_inner.bottom() {
                        // Tick mark
                        painter.line_segment(
                            [pos2(tick_right - tick_len, y), pos2(tick_right, y)],
                            Stroke::new(1.0, theme.colors.dim),
                        );

                        // Label, right-aligned to the left of the tick
                        let label_x = inner.left() + label_width - tick_len - 2.0;
                        painter.text(
                            pos2(label_x, y),
                            egui::Align2::RIGHT_CENTER,
                            label_text,
                            font_id.clone(),
                            theme.colors.text_secondary,
                        );
                    }
                }

                // Clip indicator — 3px red filled circle at top of meter
                let clip_active = if let Some(ref latched) = clip_latched {
                    // Latch mode: set latch on peak > 1.0, stays until clicked
                    if self.peak > 1.0 {
                        // We already potentially reset it on click above;
                        // the latch write happens below after this check
                        true
                    } else {
                        **latched
                    }
                } else {
                    // No latch: momentary blink at 4 Hz
                    if self.peak > 1.0 {
                        let t = ui.ctx().input(|i| i.time);
                        let blink_on = (t * 4.0) % 1.0 < 0.5;
                        if blink_on {
                            ui.ctx().request_repaint();
                            true
                        } else {
                            ui.ctx().request_repaint();
                            false
                        }
                    } else {
                        false
                    }
                };

                // Update latch state (after click reset above)
                if let Some(latched) = clip_latched {
                    if self.peak > 1.0 {
                        *latched = true;
                    }
                }

                if clip_active {
                    let clip_center = pos2(bar_rect.center().x, bar_inner.top() + 3.0);
                    painter.circle_filled(clip_center, 3.0, theme.colors.red);
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
        assert!(meter.clip_latched.is_none());
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
    fn level_meter_clip_latch_builder() {
        let mut latched = false;
        let meter = LevelMeter::new(0.5, 0.3).clip_latch(&mut latched);
        assert!(meter.clip_latched.is_some());
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
    fn db_marks_are_ordered() {
        // Verify dB marks are ordered from highest to lowest linear level
        for window in DB_MARKS.windows(2) {
            assert!(
                window[0].0 > window[1].0,
                "DB_MARKS should be ordered descending by linear level"
            );
        }
    }

    #[test]
    fn db_marks_cover_expected_range() {
        assert_eq!(DB_MARKS.len(), 5);
        assert_eq!(DB_MARKS[0].1, "0");
        assert_eq!(DB_MARKS[4].1, "-24");
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
