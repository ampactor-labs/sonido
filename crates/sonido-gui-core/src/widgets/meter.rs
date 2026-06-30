//! Level meter widgets for audio visualization.
//!
//! Provides two meter types:
//!
//! - [`LevelMeter`] — Continuous dual-color (RMS + peak) meter with a warped
//!   dB scale, peak-hold, ballistics, and a numeric peak readout, styled and
//!   behaved after Ableton Live's channel meters. The RMS bar is colored by
//!   threshold (green/yellow/red via [`SonidoTheme::meter_segment_color`]); the
//!   headroom band from RMS up to the transient peak is drawn in a lightened
//!   tint; a held-peak cap line hangs at the highest recent peak and slowly
//!   falls. The highest peak in dBFS is printed at the top and turns red past
//!   0 dBFS; clicking the meter resets it.
//!
//! - [`GainReductionMeter`] — Segmented LED-bar meter for compressor gain
//!   reduction display. Lights top-down in amber with phosphor bloom.
//!
//! ## Cross-frame state
//!
//! [`LevelMeter`] is immediate-mode but its ballistics, peak-hold, and held
//! max-dB readout need state that survives between frames. That state lives in
//! egui per-widget-id memory ([`MeterState`]), keyed by the widget's response
//! id — so every meter, anywhere, gets the behavior for free without the caller
//! threading any latch flags.

use egui::{Color32, Rect, Response, Sense, Stroke, StrokeKind, Ui, Widget, pos2, vec2};

use crate::theme::SonidoTheme;
use crate::widgets::glow;

/// Number of discrete LED segments in the gain reduction meter.
const SEGMENT_COUNT: usize = 16;

/// Gap between segments in pixels (gain reduction meter).
const SEGMENT_GAP: f32 = 0.5;

/// Bottom of the displayed dB scale. The scale is linear-in-dB from this floor
/// up to 0 dBFS, which gives the −18→0 dB region the room the eye actually uses
/// (−6 dB sits at ~90 % height, vs ~50 % under a linear-amplitude scale).
const METER_DB_FLOOR: f32 = -60.0;

/// How long (seconds) the peak-hold cap hangs before it begins to fall.
const PEAK_HOLD_TIME: f32 = 1.5;

/// Rate (dB/second) at which the held peak falls once the hold expires.
const PEAK_FALL_DB_PER_SEC: f32 = 12.0;

/// RMS ballistics attack time constant (seconds) — fast rise.
const RMS_ATTACK_TAU: f32 = 0.01;

/// RMS ballistics release time constant (seconds) — slow fall, analog feel.
const RMS_RELEASE_TAU: f32 = 0.3;

/// Height (px) of the numeric readout header above a vertical meter.
const READOUT_H: f32 = 12.0;

/// Width (px) of the numeric readout gutter beside a horizontal meter.
const HORIZ_READOUT_W: f32 = 30.0;

/// dB scale tick marks, in dBFS. Positions are derived through [`db_to_norm`]
/// so the marks, the bar fill, and the peak line all share one mapping.
const DB_MARKS: &[f32] = &[0.0, -6.0, -12.0, -18.0, -24.0, -36.0, -48.0];

/// Convert a linear amplitude (0.0–) to dBFS. Floored to avoid `-inf`.
fn linear_to_db(linear: f32) -> f32 {
    20.0 * linear.max(1e-7).log10()
}

/// Convert dBFS back to linear amplitude.
fn db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Map a dBFS value to a 0.0–1.0 bar position, linear-in-dB across
/// `[METER_DB_FLOOR, 0]`. Values at/above 0 dBFS clamp to 1.0.
fn db_to_norm(db: f32) -> f32 {
    ((db - METER_DB_FLOOR) / (0.0 - METER_DB_FLOOR)).clamp(0.0, 1.0)
}

/// One-pole exponential smoothing toward `target` over a `tau`-second time
/// constant. `tau <= 0` snaps instantly.
fn smooth(current: f32, target: f32, dt: f32, tau: f32) -> f32 {
    if tau <= 0.0 {
        return target;
    }
    let coeff = 1.0 - (-dt / tau).exp();
    current + (target - current) * coeff
}

/// Lighten a color toward white by `t` (0.0 = unchanged, 1.0 = white).
fn lighten(c: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |v: u8| (v as f32 + (255.0 - v as f32) * t) as u8;
    Color32::from_rgb(mix(c.r()), mix(c.g()), mix(c.b()))
}

/// Persistent per-meter state, stored in egui memory keyed by the widget id.
#[derive(Clone, Copy, Debug)]
struct MeterState {
    /// Ballistics-smoothed RMS level (linear).
    smoothed_rms: f32,
    /// Currently held peak level (linear) — the falling cap.
    peak_hold: f32,
    /// Seconds since the held peak was last refreshed.
    hold_age: f32,
    /// Highest peak seen since the last reset, in dBFS (the numeric readout).
    max_peak_db: f32,
}

impl Default for MeterState {
    fn default() -> Self {
        Self {
            smoothed_rms: 0.0,
            peak_hold: 0.0,
            hold_age: 0.0,
            max_peak_db: f32::NEG_INFINITY,
        }
    }
}

impl MeterState {
    /// Advance ballistics, peak-hold, and the held max from this frame's
    /// instantaneous `peak`/`rms` (linear) over `dt` seconds.
    fn advance(&mut self, peak: f32, rms: f32, dt: f32) {
        // RMS ballistics: fast attack, slow release.
        let tau = if rms > self.smoothed_rms {
            RMS_ATTACK_TAU
        } else {
            RMS_RELEASE_TAU
        };
        self.smoothed_rms = smooth(self.smoothed_rms, rms, dt, tau);

        // Peak-hold with delayed fall.
        if peak >= self.peak_hold {
            self.peak_hold = peak;
            self.hold_age = 0.0;
        } else {
            self.hold_age += dt;
            if self.hold_age > PEAK_HOLD_TIME {
                let fallen_db = linear_to_db(self.peak_hold) - PEAK_FALL_DB_PER_SEC * dt;
                self.peak_hold = db_to_linear(fallen_db.max(METER_DB_FLOOR));
            }
        }

        // Held maximum for the numeric readout.
        let peak_db = linear_to_db(peak);
        if peak_db > self.max_peak_db {
            self.max_peak_db = peak_db;
        }
    }

    /// True while the meter still has motion to render (and should repaint).
    fn animating(&self) -> bool {
        self.peak_hold > 1e-4 || self.smoothed_rms > 1e-4
    }
}

/// Continuous dual-color level meter with a warped dB scale, peak-hold,
/// ballistics, and a numeric peak readout.
///
/// ## Parameters
/// - `peak`: Peak level, normalized 0.0–1.5 (clamped). Values > 1.0 are over 0 dBFS.
/// - `rms`: RMS level, normalized 0.0–1.5 (clamped). Drives the smoothed bar fill.
/// - `label`: Optional text label drawn below the meter.
/// - `width`: Bar box width in pixels (default 24.0).
/// - `height`: Bar box height in pixels (default 120.0).
/// - `horizontal`: If true, the bar fills left-to-right instead of bottom-to-top.
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

    /// Set the bar box dimensions (width, height).
    pub fn size(mut self, width: f32, height: f32) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    /// Make horizontal (fills left-to-right) instead of vertical.
    pub fn horizontal(mut self) -> Self {
        self.horizontal = true;
        self
    }

    /// Format the numeric peak readout from a held max in dBFS.
    fn readout_text(max_peak_db: f32) -> String {
        if !max_peak_db.is_finite() || max_peak_db <= METER_DB_FLOOR {
            "-\u{221e}".to_string() // "-∞"
        } else {
            format!("{max_peak_db:.1}")
        }
    }
}

impl Widget for LevelMeter {
    fn ui(self, ui: &mut Ui) -> Response {
        let theme = SonidoTheme::get(ui.ctx());
        let label_h = if self.label.is_empty() { 0.0 } else { 18.0 };

        let size = if self.horizontal {
            vec2(self.width + HORIZ_READOUT_W, self.height.max(8.0) + label_h)
        } else {
            vec2(self.width, READOUT_H + self.height + label_h)
        };

        let (rect, response) = ui.allocate_exact_size(size, Sense::click());

        // Pull persistent state, advance it, store it back.
        let id = response.id;
        let mut state = ui.data_mut(|d| d.get_temp::<MeterState>(id).unwrap_or_default());
        let dt = ui.input(|i| i.stable_dt).clamp(0.0, 0.1);
        state.advance(self.peak, self.rms, dt);
        if response.clicked() {
            state.max_peak_db = f32::NEG_INFINITY;
        }
        if state.animating() {
            ui.ctx().request_repaint();
        }
        ui.data_mut(|d| d.insert_temp(id, state));

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();

            // Shared fill/scale quantities.
            let rms_norm = db_to_norm(linear_to_db(state.smoothed_rms));
            let peak_norm = db_to_norm(linear_to_db(self.peak));
            let hold_norm = db_to_norm(linear_to_db(state.peak_hold));
            let rms_color = theme.meter_segment_color(state.smoothed_rms.min(1.0));
            let band_color = lighten(theme.meter_segment_color(self.peak.min(1.0)), 0.45);
            let clipping = state.max_peak_db > 0.0;
            let cap_color = if state.peak_hold > 1.0 {
                theme.colors.red
            } else {
                theme.colors.text_primary
            };
            let readout_color = if clipping {
                theme.colors.red
            } else {
                theme.colors.text_secondary
            };
            let readout = Self::readout_text(state.max_peak_db);

            if self.horizontal {
                // Horizontal: readout in the right gutter, bar fills L→R.
                let bar_rect = Rect::from_min_size(rect.min, vec2(self.width, self.height));
                painter.rect_filled(bar_rect, 2.0, theme.colors.void);
                painter.rect_stroke(
                    bar_rect,
                    2.0,
                    Stroke::new(1.0, theme.colors.dim),
                    StrokeKind::Inside,
                );
                let inner = bar_rect.shrink(1.0);

                // Headroom band (RMS→peak), then RMS region over it.
                if peak_norm > 0.001 {
                    let w = inner.width() * peak_norm;
                    painter.rect_filled(
                        Rect::from_min_size(inner.min, vec2(w, inner.height())),
                        0.0,
                        band_color,
                    );
                }
                if rms_norm > 0.001 {
                    let w = inner.width() * rms_norm;
                    painter.rect_filled(
                        Rect::from_min_size(inner.min, vec2(w, inner.height())),
                        0.0,
                        rms_color,
                    );
                }
                // Held-peak cap (vertical line).
                if hold_norm > 0.001 {
                    let x = inner.left() + inner.width() * hold_norm;
                    painter.line_segment(
                        [pos2(x, inner.top()), pos2(x, inner.bottom())],
                        Stroke::new(1.0, cap_color),
                    );
                }
                // Numeric readout in the gutter.
                painter.text(
                    pos2(bar_rect.right() + 3.0, bar_rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    &readout,
                    egui::FontId::monospace(8.0),
                    readout_color,
                );
            } else {
                // Vertical: readout header on top, dB labels left, bar right.
                let readout_rect = Rect::from_min_size(rect.min, vec2(self.width, READOUT_H));
                painter.text(
                    readout_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    &readout,
                    egui::FontId::monospace(8.0),
                    readout_color,
                );

                let meter_rect = Rect::from_min_size(
                    pos2(rect.left(), rect.top() + READOUT_H),
                    vec2(self.width, self.height),
                );
                painter.rect_filled(meter_rect, 2.0, theme.colors.void);

                let inner = meter_rect.shrink(2.0);
                // ~55% right for the bar, ~45% left for dB labels.
                let bar_width = inner.width() * 0.55;
                let label_width = inner.width() - bar_width;
                let bar_left = inner.right() - bar_width;
                let bar_rect = Rect::from_min_max(pos2(bar_left, inner.top()), inner.max);

                painter.rect_stroke(
                    bar_rect,
                    1.0,
                    Stroke::new(1.0, theme.colors.dim),
                    StrokeKind::Inside,
                );
                let bar_inner = bar_rect.shrink(1.0);

                // Headroom band (RMS→peak), then RMS region over it.
                if peak_norm > 0.001 {
                    let h = bar_inner.height() * peak_norm;
                    painter.rect_filled(
                        Rect::from_min_max(
                            pos2(bar_inner.left(), bar_inner.bottom() - h),
                            bar_inner.max,
                        ),
                        0.0,
                        band_color,
                    );
                }
                if rms_norm > 0.001 {
                    let h = bar_inner.height() * rms_norm;
                    painter.rect_filled(
                        Rect::from_min_max(
                            pos2(bar_inner.left(), bar_inner.bottom() - h),
                            bar_inner.max,
                        ),
                        0.0,
                        rms_color,
                    );
                }
                // Held-peak cap (horizontal line).
                if hold_norm > 0.001 {
                    let y = bar_inner.bottom() - bar_inner.height() * hold_norm;
                    painter.line_segment(
                        [pos2(bar_inner.left(), y), pos2(bar_inner.right(), y)],
                        Stroke::new(1.0, cap_color),
                    );
                }

                // dB scale tick marks + labels on the left.
                let font_size = (bar_width * 0.35).clamp(7.0, 9.0);
                let font_id = egui::FontId::proportional(font_size);
                let tick_right = bar_left - 1.0;
                let tick_len = 3.0;
                for &db in DB_MARKS {
                    let y = bar_inner.bottom() - bar_inner.height() * db_to_norm(db);
                    if y >= bar_inner.top() && y <= bar_inner.bottom() {
                        painter.line_segment(
                            [pos2(tick_right - tick_len, y), pos2(tick_right, y)],
                            Stroke::new(1.0, theme.colors.dim),
                        );
                        let label_x = inner.left() + label_width - tick_len - 2.0;
                        painter.text(
                            pos2(label_x, y),
                            egui::Align2::RIGHT_CENTER,
                            format!("{db:.0}"),
                            font_id.clone(),
                            theme.colors.text_secondary,
                        );
                    }
                }
            }

            // Label below.
            if !self.label.is_empty() {
                let label_pos = pos2(rect.center().x, rect.bottom() - label_h + 4.0);
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
    fn db_to_norm_endpoints() {
        // 0 dBFS pins to the top, the floor to the bottom, over-0 clamps.
        assert!((db_to_norm(0.0) - 1.0).abs() < 1e-6);
        assert!((db_to_norm(METER_DB_FLOOR) - 0.0).abs() < 1e-6);
        assert_eq!(db_to_norm(6.0), 1.0);
        assert_eq!(db_to_norm(-200.0), 0.0);
    }

    #[test]
    fn db_to_norm_warps_top_region() {
        // The whole point: −6 dB sits high (~0.9), not at 0.5 like a
        // linear-amplitude scale, and the order is monotonic.
        let n6 = db_to_norm(-6.0);
        let n12 = db_to_norm(-12.0);
        let n24 = db_to_norm(-24.0);
        assert!(n6 > 0.85, "-6 dB should sit near the top, got {n6}");
        assert!(n6 > n12 && n12 > n24);
    }

    #[test]
    fn db_linear_round_trip() {
        for &db in &[-3.0_f32, -12.0, -24.0, -48.0] {
            let back = linear_to_db(db_to_linear(db));
            assert!((back - db).abs() < 1e-3, "round trip {db} -> {back}");
        }
    }

    #[test]
    fn smooth_attack_is_faster_than_release() {
        // Rising toward 1.0 with the attack tau covers more ground per step
        // than falling toward 0.0 with the (slower) release tau.
        let dt = 1.0 / 60.0;
        let rise = smooth(0.0, 1.0, dt, RMS_ATTACK_TAU);
        let fall = 1.0 - smooth(1.0, 0.0, dt, RMS_RELEASE_TAU);
        assert!(rise > fall, "attack {rise} should outpace release {fall}");
    }

    #[test]
    fn smooth_zero_tau_snaps() {
        assert_eq!(smooth(0.0, 0.42, 0.016, 0.0), 0.42);
    }

    #[test]
    fn peak_hold_holds_then_falls() {
        let mut s = MeterState::default();
        // A transient sets the hold.
        s.advance(1.0, 0.5, 0.016);
        assert!((s.peak_hold - 1.0).abs() < 1e-6);
        // Within the hold window, silence does not drop the cap.
        for _ in 0..10 {
            s.advance(0.0, 0.0, 0.016);
        }
        assert!(
            (s.peak_hold - 1.0).abs() < 1e-3,
            "cap fell during hold window"
        );
        // Well past the hold window, the cap has fallen.
        for _ in 0..200 {
            s.advance(0.0, 0.0, 0.016);
        }
        assert!(s.peak_hold < 0.9, "cap did not fall after hold expired");
    }

    #[test]
    fn max_peak_db_latches_until_reset() {
        let mut s = MeterState::default();
        s.advance(1.2, 0.5, 0.016); // over 0 dBFS
        let held = s.max_peak_db;
        assert!(held > 0.0, "over-0 peak should latch positive dB");
        // Subsequent quieter frames do not lower the held max.
        s.advance(0.1, 0.1, 0.016);
        assert_eq!(s.max_peak_db, held);
    }

    #[test]
    fn readout_text_formats() {
        assert_eq!(LevelMeter::readout_text(f32::NEG_INFINITY), "-\u{221e}");
        assert_eq!(LevelMeter::readout_text(-12.34), "-12.3");
        assert_eq!(LevelMeter::readout_text(1.25), "1.2");
    }

    #[test]
    fn meter_segment_color_thresholds() {
        let theme = SonidoTheme::default();
        assert_eq!(theme.meter_segment_color(0.5), theme.colors.green);
        assert_eq!(theme.meter_segment_color(0.8), theme.colors.yellow);
        assert_eq!(theme.meter_segment_color(1.0), theme.colors.red);
    }

    #[test]
    fn lighten_moves_toward_white() {
        let c = Color32::from_rgb(0, 100, 200);
        let l = lighten(c, 0.5);
        assert!(l.r() > c.r() && l.g() > c.g() && l.b() > c.b());
        assert_eq!(lighten(c, 0.0), c);
        assert_eq!(lighten(c, 1.0), Color32::WHITE);
    }

    #[test]
    fn db_marks_are_ordered() {
        for window in DB_MARKS.windows(2) {
            assert!(
                window[0] > window[1],
                "DB_MARKS should be ordered descending"
            );
        }
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
