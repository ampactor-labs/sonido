//! Spectrum analyzer widget — FFT magnitude display on a log frequency axis.
//!
//! [`SpectrumWidget`] renders FFT magnitude bins as vertical bars on a
//! logarithmic frequency axis (20 Hz – 20 kHz). Each bin has independent
//! peak-hold with exponential decay, producing the characteristic "falling
//! peak" visual of professional spectrum analyzers.
//!
//! # Usage
//!
//! ```ignore
//! // State stored per-frame in the app
//! let mut spectrum = SpectrumState::new(1024);
//!
//! // Feed new FFT magnitudes each frame (externally computed)
//! spectrum.update(&fft_magnitudes);
//!
//! // Render
//! ui.add(SpectrumWidget::new(&spectrum).size(400.0, 200.0));
//! ```

use egui::{Rect, Response, Sense, Stroke, StrokeKind, Ui, Widget, pos2, vec2};

use crate::theme::SonidoTheme;
use crate::widgets::glow;

/// Floor dB level — bins below this are not rendered.
const DB_FLOOR: f32 = -90.0;
/// Ceiling dB level — bins above this are clamped.
const DB_CEIL: f32 = 0.0;
/// Peak-hold decay factor per frame (exponential, ~3 dB/frame at 60 fps).
const PEAK_DECAY: f32 = 0.97;
/// Minimum frequency on the log axis.
const FREQ_MIN: f32 = 20.0;
/// Maximum frequency on the log axis.
const FREQ_MAX: f32 = 20_000.0;

/// Per-bin state for the spectrum analyzer (peak-hold with decay).
#[derive(Clone, Debug)]
pub struct SpectrumState {
    /// Smoothed magnitude per display column, normalized 0.0–1.0 (dB mapped).
    smoothed: Vec<f32>,
    /// Peak-hold level per display column, normalized 0.0–1.0.
    peaks: Vec<f32>,
    /// Number of display columns (independent of FFT size).
    columns: usize,
}

impl SpectrumState {
    /// Create a new spectrum state with the given number of display columns.
    ///
    /// `columns` is the number of visual bars (typically 64–256).
    pub fn new(columns: usize) -> Self {
        Self {
            smoothed: vec![0.0; columns],
            peaks: vec![0.0; columns],
            columns,
        }
    }

    /// Update the state from a new FFT magnitude slice.
    ///
    /// `magnitudes` is a slice of linear magnitude values (not dB) with
    /// length equal to `fft_size / 2` (the positive frequencies only).
    /// `sample_rate` is needed to map bin indices to frequencies.
    /// Bins outside 20 Hz – 20 kHz are ignored.
    pub fn update(&mut self, magnitudes: &[f32], sample_rate: f32) {
        let bin_count = magnitudes.len();
        if bin_count == 0 || self.columns == 0 {
            return;
        }

        let bin_hz = sample_rate / (2.0 * bin_count as f32);

        // Map each display column to a frequency range and take the peak bin.
        let log_min = FREQ_MIN.log10();
        let log_max = FREQ_MAX.log10();

        for col in 0..self.columns {
            let t_lo = col as f32 / self.columns as f32;
            let t_hi = (col + 1) as f32 / self.columns as f32;
            let f_lo = 10.0f32.powf(log_min + t_lo * (log_max - log_min));
            let f_hi = 10.0f32.powf(log_min + t_hi * (log_max - log_min));

            let bin_lo = ((f_lo / bin_hz) as usize).clamp(0, bin_count - 1);
            let bin_hi = ((f_hi / bin_hz) as usize).clamp(bin_lo, bin_count - 1);

            // Peak magnitude across the bin range for this column.
            let mag = magnitudes[bin_lo..=bin_hi]
                .iter()
                .copied()
                .fold(0.0f32, f32::max);

            // Convert to dB and normalize to 0.0–1.0.
            let db = if mag > 1e-10 {
                20.0 * mag.log10()
            } else {
                DB_FLOOR - 1.0
            };
            let normalized = ((db - DB_FLOOR) / (DB_CEIL - DB_FLOOR)).clamp(0.0, 1.0);

            // Smooth: take max with decayed previous (fast rise, slow fall).
            self.smoothed[col] = self.smoothed[col].max(normalized) * PEAK_DECAY;
            self.smoothed[col] = self.smoothed[col].max(normalized);

            // Peak hold: advance peak down if current is lower.
            if normalized > self.peaks[col] {
                self.peaks[col] = normalized;
            } else {
                self.peaks[col] *= PEAK_DECAY;
            }
        }
    }

    /// Reset all smoothed values and peak holds to zero.
    pub fn reset(&mut self) {
        self.smoothed.fill(0.0);
        self.peaks.fill(0.0);
    }

    /// Number of display columns.
    pub fn columns(&self) -> usize {
        self.columns
    }
}

/// Spectrum analyzer widget.
///
/// Displays FFT magnitude as vertical bars on a logarithmic frequency axis.
/// Reads from [`SpectrumState`] which holds the smoothed values and peak holds.
///
/// ## Parameters
/// - `state`: Reference to [`SpectrumState`] holding the current magnitudes.
/// - `width`: Display width in pixels (default 300.0).
/// - `height`: Display height in pixels (default 120.0).
pub struct SpectrumWidget<'a> {
    state: &'a SpectrumState,
    width: f32,
    height: f32,
}

impl<'a> SpectrumWidget<'a> {
    /// Create a new spectrum widget reading from `state`.
    pub fn new(state: &'a SpectrumState) -> Self {
        Self {
            state,
            width: 300.0,
            height: 120.0,
        }
    }

    /// Set the widget dimensions.
    pub fn size(mut self, width: f32, height: f32) -> Self {
        self.width = width;
        self.height = height;
        self
    }
}

impl Widget for SpectrumWidget<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let theme = SonidoTheme::get(ui.ctx());
        let (rect, response) =
            ui.allocate_exact_size(vec2(self.width, self.height), Sense::hover());

        if !ui.is_rect_visible(rect) {
            return response;
        }

        let painter = ui.painter();

        // Background
        painter.rect_filled(rect, 2.0, theme.colors.void);
        painter.rect_stroke(
            rect,
            2.0,
            Stroke::new(1.0, theme.colors.dim),
            StrokeKind::Inside,
        );

        let inner = rect.shrink(2.0);
        let cols = self.state.columns;

        if cols == 0 {
            return response;
        }

        let bar_w = (inner.width() / cols as f32).max(1.0);

        // Draw frequency grid lines (1kHz, 5kHz, 10kHz) at dim intensity.
        let log_min = FREQ_MIN.log10();
        let log_max = FREQ_MAX.log10();
        for &grid_hz in &[100.0f32, 1000.0, 5000.0, 10_000.0] {
            let t = (grid_hz.log10() - log_min) / (log_max - log_min);
            if !(0.0..=1.0).contains(&t) {
                continue;
            }
            let x = inner.left() + t * inner.width();
            painter.line_segment(
                [pos2(x, inner.top()), pos2(x, inner.bottom())],
                Stroke::new(1.0, theme.colors.dim),
            );
        }

        // Draw bars
        for col in 0..cols {
            let level = self.state.smoothed[col];
            let peak = self.state.peaks[col];

            if level < 0.001 && peak < 0.001 {
                continue;
            }

            let x = inner.left() + col as f32 * bar_w;
            let bar_height = inner.height() * level;
            let bar_top = inner.bottom() - bar_height;

            // Bar fill — signal color, intensity-modulated by level.
            if bar_height > 0.5 {
                let bar_rect = Rect::from_min_max(
                    pos2(x, bar_top),
                    pos2((x + bar_w - 1.0).max(x), inner.bottom()),
                );
                let color = if level > 0.95 {
                    theme.colors.red
                } else if level > 0.7 {
                    theme.colors.yellow
                } else {
                    theme.colors.green
                };
                painter.rect_filled(bar_rect, 0.0, color.gamma_multiply(level.max(0.3)));
            }

            // Peak hold line — thin horizontal line at peak position.
            if peak > 0.01 {
                let peak_y = inner.bottom() - inner.height() * peak;
                let peak_color = if peak > 0.95 {
                    theme.colors.red
                } else if peak > 0.7 {
                    theme.colors.yellow
                } else {
                    theme.colors.green
                };
                glow::glow_line(
                    painter,
                    pos2(x, peak_y),
                    pos2((x + bar_w - 1.0).max(x), peak_y),
                    peak_color,
                    1.0,
                    &theme,
                );
            }
        }

        // dB scale labels on right edge.
        let font = egui::FontId::proportional(8.0);
        for &(db, label) in &[(-12.0f32, "-12"), (-24.0, "-24"), (-48.0, "-48")] {
            let normalized = ((db - DB_FLOOR) / (DB_CEIL - DB_FLOOR)).clamp(0.0, 1.0);
            let y = inner.bottom() - inner.height() * normalized;
            painter.line_segment(
                [pos2(inner.right() - 4.0, y), pos2(inner.right(), y)],
                Stroke::new(1.0, theme.colors.dim),
            );
            painter.text(
                pos2(inner.right() - 5.0, y),
                egui::Align2::RIGHT_CENTER,
                label,
                font.clone(),
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
    fn spectrum_state_new() {
        let s = SpectrumState::new(128);
        assert_eq!(s.columns(), 128);
        assert!(s.smoothed.iter().all(|&v| v == 0.0));
        assert!(s.peaks.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn spectrum_state_reset() {
        let mut s = SpectrumState::new(64);
        s.smoothed[0] = 0.5;
        s.peaks[0] = 0.8;
        s.reset();
        assert!(s.smoothed.iter().all(|&v| v == 0.0));
        assert!(s.peaks.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn spectrum_update_silent_stays_zero() {
        let mut s = SpectrumState::new(32);
        let mags = vec![0.0f32; 512];
        s.update(&mags, 48000.0);
        assert!(s.smoothed.iter().all(|&v| v < 0.001));
        assert!(s.peaks.iter().all(|&v| v < 0.001));
    }

    #[test]
    fn spectrum_update_full_scale_normalizes() {
        let mut s = SpectrumState::new(32);
        // 1.0 linear = 0 dB = normalized 1.0
        let mags = vec![1.0f32; 512];
        s.update(&mags, 48000.0);
        // All columns that have bins in 20Hz-20kHz range should be near 1.0.
        let any_high = s.smoothed.iter().any(|&v| v > 0.9);
        assert!(
            any_high,
            "0 dB signal should produce high normalized values"
        );
    }

    #[test]
    fn peak_hold_decays() {
        let mut s = SpectrumState::new(4);
        let mags = vec![1.0f32; 512];
        s.update(&mags, 48000.0);
        let initial_peak = s.peaks[0];
        // Feed silence — peaks should decay.
        for _ in 0..10 {
            s.update(&vec![0.0; 512], 48000.0);
        }
        assert!(s.peaks[0] < initial_peak, "peaks should decay over time");
    }

    #[test]
    fn spectrum_state_empty_magnitudes() {
        let mut s = SpectrumState::new(32);
        s.update(&[], 48000.0); // Should not panic.
        assert!(s.smoothed.iter().all(|&v| v == 0.0));
    }
}
