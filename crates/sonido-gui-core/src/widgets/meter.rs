//! Level meter widgets for audio visualization.

use egui::{Color32, Rect, Response, Sense, Stroke, StrokeKind, Ui, Widget, pos2, vec2};

/// VU-style level meter with peak hold.
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

    fn peak_level_to_color(level: f32) -> Color32 {
        if level > 0.95 {
            Color32::from_rgb(255, 100, 100) // Bright Red
        } else if level > 0.7 {
            Color32::from_rgb(255, 230, 100) // Bright Yellow
        } else {
            Color32::from_rgb(120, 230, 120) // Bright Green
        }
    }

    fn level_to_color(level: f32) -> Color32 {
        if level > 0.95 {
            Color32::from_rgb(220, 60, 60) // Red - clipping
        } else if level > 0.7 {
            Color32::from_rgb(220, 200, 60) // Yellow - hot
        } else {
            Color32::from_rgb(80, 200, 80) // Green - normal
        }
    }
}

impl Widget for LevelMeter {
    fn ui(self, ui: &mut Ui) -> Response {
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

            // Background
            let bg_color = Color32::from_rgb(25, 25, 30);
            painter.rect_filled(meter_rect, 2.0, bg_color);

            // Border
            painter.rect_stroke(
                meter_rect,
                2.0,
                Stroke::new(1.0, Color32::from_rgb(50, 50, 60)),
                StrokeKind::Inside,
            );

            // Inner padding
            let inner = meter_rect.shrink(2.0);

            if self.horizontal {
                // RMS bar (main level)
                let rms_width = (self.rms.min(1.0) * inner.width()).max(0.0);
                if rms_width > 0.0 {
                    let rms_rect = Rect::from_min_size(inner.min, vec2(rms_width, inner.height()));
                    painter.rect_filled(rms_rect, 1.0, Self::level_to_color(self.rms));
                }

                // Peak indicator line
                if self.peak > 0.01 {
                    let peak_x = inner.left() + (self.peak.min(1.0) * inner.width());
                    painter.line_segment(
                        [pos2(peak_x, inner.top()), pos2(peak_x, inner.bottom())],
                        Stroke::new(2.0, Color32::WHITE),
                    );
                }

                // Clip indicator
                if self.peak > 1.0 {
                    let clip_rect = Rect::from_min_size(
                        pos2(inner.right() - 4.0, inner.top()),
                        vec2(4.0, inner.height()),
                    );
                    painter.rect_filled(clip_rect, 0.0, Color32::from_rgb(255, 0, 0));
                }
            } else {
                // Vertical meter (default)
                let rms_height = (self.rms.min(1.0) * inner.height()).max(0.0);
                let peak_height = (self.peak.min(1.0) * inner.height()).max(0.0);

                // RMS bar
                if rms_height > 0.0 {
                    let rms_rect = Rect::from_min_max(
                        pos2(inner.left(), inner.bottom() - rms_height),
                        inner.max,
                    );
                    painter.rect_filled(rms_rect, 1.0, Self::level_to_color(self.rms));
                }

                // Peak bar (on top of RMS)
                if peak_height > rms_height {
                    let peak_rect = Rect::from_min_max(
                        pos2(inner.left(), inner.bottom() - peak_height),
                        pos2(inner.right(), inner.bottom() - rms_height),
                    );
                    painter.rect_filled(peak_rect, 1.0, Self::peak_level_to_color(self.peak));
                }

                // Peak indicator line (thin hold line)
                if self.peak > 0.01 {
                    let peak_y = inner.bottom() - (self.peak.min(1.0) * inner.height());
                    painter.line_segment(
                        [pos2(inner.left(), peak_y), pos2(inner.right(), peak_y)],
                        Stroke::new(1.0, Color32::WHITE.gamma_multiply(0.5)),
                    );
                }

                // Clip indicator at top
                if self.peak > 1.0 {
                    let clip_rect = Rect::from_min_size(inner.min, vec2(inner.width(), 4.0));
                    painter.rect_filled(clip_rect, 0.0, Color32::from_rgb(255, 0, 0));
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
                    Color32::from_rgb(150, 150, 160),
                );
            }
        }

        response
    }
}

/// Gain reduction meter for compressor display.
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
        let size = vec2(self.width, self.height + 18.0);
        let (rect, response) = ui.allocate_exact_size(size, Sense::hover());

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();

            let meter_rect = Rect::from_min_size(rect.min, vec2(self.width, self.height));

            // Background
            painter.rect_filled(meter_rect, 2.0, Color32::from_rgb(25, 25, 30));
            painter.rect_stroke(
                meter_rect,
                2.0,
                Stroke::new(1.0, Color32::from_rgb(50, 50, 60)),
                StrokeKind::Inside,
            );

            let inner = meter_rect.shrink(2.0);

            // GR bar - grows from top down (opposite of level meter)
            let normalized = (self.reduction_db / self.max_reduction).min(1.0);
            let gr_height = normalized * inner.height();

            if gr_height > 0.0 {
                let gr_rect = Rect::from_min_size(inner.min, vec2(inner.width(), gr_height));

                // Orange/amber color for gain reduction
                let color = if self.reduction_db > 12.0 {
                    Color32::from_rgb(220, 100, 50) // Heavy compression
                } else {
                    Color32::from_rgb(220, 160, 50) // Normal
                };

                painter.rect_filled(gr_rect, 0.0, color);
            }

            // Label
            let label_pos = pos2(rect.center().x, meter_rect.bottom() + 4.0);
            painter.text(
                label_pos,
                egui::Align2::CENTER_TOP,
                "GR",
                egui::FontId::proportional(11.0),
                Color32::from_rgb(150, 150, 160),
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
    fn level_to_color_thresholds() {
        let green = LevelMeter::level_to_color(0.5);
        let yellow = LevelMeter::level_to_color(0.8);
        let red = LevelMeter::level_to_color(1.0);
        assert_eq!(green, Color32::from_rgb(80, 200, 80));
        assert_eq!(yellow, Color32::from_rgb(220, 200, 60));
        assert_eq!(red, Color32::from_rgb(220, 60, 60));
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
