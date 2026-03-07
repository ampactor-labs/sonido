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
        _ => Some(0),                // unsupported -> blank
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
                        // Decimal point: dot at bottom-right
                        let dot_pos = pos2(x + dw * 0.5, rect.bottom() - 2.0);
                        glow::glow_circle(painter, dot_pos, 1.5, color, &theme);
                        x += gap + 6.0; // narrower than a full digit
                        continue;
                    }
                    ':' => {
                        // Colon: two dots vertically centered
                        let mid_x = x + dw * 0.5;
                        glow::glow_circle(painter, pos2(mid_x, rect.top() + dh * 0.3), 1.5, color, &theme);
                        glow::glow_circle(painter, pos2(mid_x, rect.top() + dh * 0.7), 1.5, color, &theme);
                        x += gap + 8.0;
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
    let pad = 2.0;
    let half_h = h / 2.0;
    let stroke_w = 3.0;

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
            painter.line_segment([start, end], egui::Stroke::new(stroke_w, ghost_color));
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
        assert_eq!(char_segments('.'), None);
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
