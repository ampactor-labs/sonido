//! Scrolling time-domain waveform display widget.
//!
//! [`WaveformWidget`] renders a scrolling oscilloscope-style waveform using a
//! ring buffer of audio samples. New samples are appended via [`WaveformState::push`];
//! the widget renders the most recent `window_width` worth of samples as a
//! polyline using the Arcade CRT signal color.
//!
//! # Usage
//!
//! ```ignore
//! // State stored per-frame in the app — 2 seconds at 48 kHz.
//! let mut waveform = WaveformState::new(96000);
//!
//! // Push new samples each audio block.
//! waveform.push(&block);
//!
//! // Render — shows the last 50ms.
//! ui.add(WaveformWidget::new(&waveform).window_ms(50.0, 48000.0).size(400.0, 80.0));
//! ```

use egui::{Response, Sense, Stroke, StrokeKind, Ui, Widget, pos2, vec2};

use crate::theme::SonidoTheme;

/// Default ring buffer capacity in samples.
const DEFAULT_CAPACITY: usize = 96_000; // 2 s at 48 kHz

/// Ring buffer of audio samples for waveform display.
#[derive(Clone, Debug)]
pub struct WaveformState {
    /// Circular sample buffer.
    buffer: Vec<f32>,
    /// Write cursor (next write position).
    write: usize,
    /// Total samples ever written (used to compute fill count).
    total: usize,
    /// Buffer capacity.
    capacity: usize,
}

impl Default for WaveformState {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl WaveformState {
    /// Create a new waveform state with the given ring buffer capacity.
    ///
    /// `capacity` is the total number of samples stored (e.g., 96 000 = 2 s at 48 kHz).
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            buffer: vec![0.0f32; capacity],
            write: 0,
            total: 0,
            capacity,
        }
    }

    /// Push a block of samples into the ring buffer.
    pub fn push(&mut self, samples: &[f32]) {
        for &s in samples {
            self.buffer[self.write] = s;
            self.write = (self.write + 1) % self.capacity;
            self.total += 1;
        }
    }

    /// Push a single sample.
    pub fn push_sample(&mut self, sample: f32) {
        self.buffer[self.write] = sample;
        self.write = (self.write + 1) % self.capacity;
        self.total += 1;
    }

    /// Number of samples currently in the buffer (up to capacity).
    pub fn len(&self) -> usize {
        self.total.min(self.capacity)
    }

    /// Returns true if no samples have been written yet.
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// Read the most recent `count` samples in chronological order.
    ///
    /// Returns fewer samples if fewer than `count` have been written.
    pub fn recent(&self, count: usize) -> impl Iterator<Item = f32> + '_ {
        let available = self.len().min(count);
        // Start index: `write - available` wrapped around.
        let start = (self.write + self.capacity - available) % self.capacity;
        (0..available).map(move |i| self.buffer[(start + i) % self.capacity])
    }

    /// Reset — clear all samples.
    pub fn reset(&mut self) {
        self.buffer.fill(0.0);
        self.write = 0;
        self.total = 0;
    }

    /// Buffer capacity in samples.
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

/// Scrolling waveform display widget.
///
/// Renders the most recent samples from [`WaveformState`] as a polyline.
/// The display window is configurable in milliseconds.
///
/// ## Parameters
/// - `state`: Reference to [`WaveformState`].
/// - `window_samples`: Number of samples to display (sets horizontal time window).
/// - `amplitude_scale`: Peak amplitude that fills the display (default 1.0).
/// - `width`: Widget width in pixels (default 300.0).
/// - `height`: Widget height in pixels (default 80.0).
pub struct WaveformWidget<'a> {
    state: &'a WaveformState,
    window_samples: usize,
    amplitude_scale: f32,
    width: f32,
    height: f32,
}

impl<'a> WaveformWidget<'a> {
    /// Create a new waveform widget reading from `state`.
    ///
    /// Uses a default window of 4096 samples.
    pub fn new(state: &'a WaveformState) -> Self {
        Self {
            state,
            window_samples: 4096,
            amplitude_scale: 1.0,
            width: 300.0,
            height: 80.0,
        }
    }

    /// Set the display window as a duration in milliseconds.
    ///
    /// `sample_rate` is used to convert from time to sample count.
    /// Valid range: 1 ms – 10 000 ms.
    pub fn window_ms(mut self, ms: f32, sample_rate: f32) -> Self {
        let ms = ms.clamp(1.0, 10_000.0);
        self.window_samples = ((ms / 1000.0) * sample_rate) as usize;
        self
    }

    /// Set the amplitude scale — peak level that fills the display height.
    ///
    /// `scale` must be > 0. Default is 1.0 (full-scale ±1.0).
    pub fn amplitude_scale(mut self, scale: f32) -> Self {
        self.amplitude_scale = scale.max(1e-6);
        self
    }

    /// Set widget dimensions.
    pub fn size(mut self, width: f32, height: f32) -> Self {
        self.width = width;
        self.height = height;
        self
    }
}

impl Widget for WaveformWidget<'_> {
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

        // Zero-crossing guide line.
        let mid_y = inner.center().y;
        painter.line_segment(
            [pos2(inner.left(), mid_y), pos2(inner.right(), mid_y)],
            Stroke::new(1.0, theme.colors.dim),
        );

        // Collect the window's worth of samples.
        let window = self.window_samples.min(self.state.capacity());
        let samples: Vec<f32> = self.state.recent(window).collect();
        let n = samples.len();

        if n < 2 {
            return response;
        }

        // Map samples to screen coordinates.
        let x_scale = inner.width() / (n - 1) as f32;
        let half_h = inner.height() * 0.5;
        let center_y = inner.center().y;
        let amp = self.amplitude_scale;

        let points: Vec<egui::Pos2> = samples
            .iter()
            .enumerate()
            .map(|(i, &s)| {
                let x = inner.left() + i as f32 * x_scale;
                let y = center_y - (s / amp).clamp(-1.0, 1.0) * half_h;
                pos2(x, y)
            })
            .collect();

        // Clip rendering to inner rect to avoid overdraw.
        let clip_rect = painter.clip_rect().intersect(inner);
        let clipped = painter.with_clip_rect(clip_rect);

        // Draw polyline — use glow for the characteristic CRT trace look.
        if !theme.reduced_fx {
            // Bloom pass: wider stroke at low alpha.
            let bloom_color = theme.colors.green.gamma_multiply(theme.glow.bloom_alpha);
            for win in points.windows(2) {
                clipped.line_segment(
                    [win[0], win[1]],
                    Stroke::new(1.0 + theme.glow.bloom_radius * 2.0, bloom_color),
                );
            }
        }

        let signal_color = theme.colors.green;
        for win in points.windows(2) {
            clipped.line_segment([win[0], win[1]], Stroke::new(1.0, signal_color));
        }

        // Clip indicator — draw red border if any sample exceeds ±1.0.
        let clipping = samples.iter().any(|&s| s.abs() > amp);
        if clipping {
            painter.rect_stroke(
                inner,
                2.0,
                Stroke::new(1.0, theme.colors.red),
                StrokeKind::Outside,
            );
        }

        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waveform_state_new() {
        let w = WaveformState::new(1024);
        assert_eq!(w.capacity(), 1024);
        assert_eq!(w.len(), 0);
        assert!(w.is_empty());
    }

    #[test]
    fn waveform_state_push_and_len() {
        let mut w = WaveformState::new(1024);
        w.push(&[1.0, 2.0, 3.0]);
        assert_eq!(w.len(), 3);
        assert!(!w.is_empty());
    }

    #[test]
    fn waveform_state_wraps() {
        let mut w = WaveformState::new(4);
        w.push(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(w.len(), 4); // capped at capacity
    }

    #[test]
    fn waveform_recent_order() {
        let mut w = WaveformState::new(16);
        w.push(&[1.0, 2.0, 3.0]);
        let recents: Vec<f32> = w.recent(3).collect();
        assert_eq!(recents, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn waveform_recent_wraps_correctly() {
        let mut w = WaveformState::new(4);
        w.push(&[1.0, 2.0, 3.0, 4.0, 5.0]); // wraps: [5,2,3,4] with write=1
        let recents: Vec<f32> = w.recent(4).collect();
        assert_eq!(recents, vec![2.0, 3.0, 4.0, 5.0]);
    }

    #[test]
    fn waveform_reset() {
        let mut w = WaveformState::new(16);
        w.push(&[0.5, 0.6]);
        w.reset();
        assert_eq!(w.len(), 0);
        assert!(w.is_empty());
    }

    #[test]
    fn waveform_push_sample() {
        let mut w = WaveformState::new(8);
        w.push_sample(0.42);
        assert_eq!(w.len(), 1);
        let recents: Vec<f32> = w.recent(1).collect();
        assert!((recents[0] - 0.42).abs() < 1e-6);
    }

    #[test]
    fn window_ms_sets_samples() {
        let state = WaveformState::new(96000);
        let w = WaveformWidget::new(&state).window_ms(50.0, 48000.0);
        assert_eq!(w.window_samples, 2400);
    }
}
