//! Sonido Vector: the design system's display face.
//!
//! A stroke font in the spirit of the 1979–83 vector arcade games: every glyph
//! is a few straight beams on a 4 × 6 grid, drawn with [`neon`](super::glow)
//! lines so it blooms like a phosphor trace. It sets the marquee and section
//! titles; body text stays in Share Tech Mono, which reads better small.
//!
//! Lowercase input is drawn as uppercase; characters without a glyph advance
//! as a space.

use egui::{Color32, Painter, Pos2, Rect, Vec2, pos2, vec2};

use crate::theme::SonidoTheme;
use crate::widgets::glow;

/// Glyph cell width in grid units (the glyph itself spans 0..=4).
const ADVANCE: f32 = 6.0;
/// Glyph height in grid units.
const HEIGHT: f32 = 6.0;

type Stroke = &'static [(f32, f32)];

/// Polylines for one character on the 4 × 6 grid (y down), or `None`.
#[allow(clippy::too_many_lines)]
fn glyph(c: char) -> Option<&'static [Stroke]> {
    Some(match c.to_ascii_uppercase() {
        'A' => &[
            &[(0., 6.), (0., 2.), (2., 0.), (4., 2.), (4., 6.)],
            &[(0., 4.), (4., 4.)],
        ],
        'B' => &[
            &[
                (0., 0.),
                (0., 6.),
                (3., 6.),
                (4., 5.),
                (4., 4.),
                (3., 3.),
                (0., 3.),
            ],
            &[(0., 0.), (3., 0.), (4., 1.), (4., 2.), (3., 3.)],
        ],
        'C' => &[&[(4., 0.), (0., 0.), (0., 6.), (4., 6.)]],
        'D' => &[&[
            (0., 0.),
            (0., 6.),
            (2., 6.),
            (4., 4.),
            (4., 2.),
            (2., 0.),
            (0., 0.),
        ]],
        'E' => &[
            &[(4., 0.), (0., 0.), (0., 6.), (4., 6.)],
            &[(0., 3.), (3., 3.)],
        ],
        'F' => &[&[(4., 0.), (0., 0.), (0., 6.)], &[(0., 3.), (3., 3.)]],
        'G' => &[&[
            (4., 1.),
            (4., 0.),
            (0., 0.),
            (0., 6.),
            (4., 6.),
            (4., 3.),
            (2., 3.),
        ]],
        'H' => &[
            &[(0., 0.), (0., 6.)],
            &[(4., 0.), (4., 6.)],
            &[(0., 3.), (4., 3.)],
        ],
        'I' => &[
            &[(0., 0.), (4., 0.)],
            &[(2., 0.), (2., 6.)],
            &[(0., 6.), (4., 6.)],
        ],
        'J' => &[&[(4., 0.), (4., 6.), (1., 6.), (0., 5.), (0., 4.)]],
        'K' => &[&[(0., 0.), (0., 6.)], &[(4., 0.), (0., 3.), (4., 6.)]],
        'L' => &[&[(0., 0.), (0., 6.), (4., 6.)]],
        'M' => &[&[(0., 6.), (0., 0.), (2., 2.), (4., 0.), (4., 6.)]],
        'N' => &[&[(0., 6.), (0., 0.), (4., 6.), (4., 0.)]],
        'O' | '0' => &[&[(0., 0.), (4., 0.), (4., 6.), (0., 6.), (0., 0.)]],
        'P' => &[&[(0., 6.), (0., 0.), (4., 0.), (4., 3.), (0., 3.)]],
        'Q' => &[
            &[(0., 0.), (4., 0.), (4., 4.), (2., 6.), (0., 6.), (0., 0.)],
            &[(2., 4.), (4., 6.)],
        ],
        'R' => &[
            &[(0., 6.), (0., 0.), (4., 0.), (4., 3.), (0., 3.)],
            &[(1., 3.), (4., 6.)],
        ],
        'S' | '5' => &[&[(4., 0.), (0., 0.), (0., 3.), (4., 3.), (4., 6.), (0., 6.)]],
        'T' => &[&[(0., 0.), (4., 0.)], &[(2., 0.), (2., 6.)]],
        'U' => &[&[(0., 0.), (0., 6.), (4., 6.), (4., 0.)]],
        'V' => &[&[(0., 0.), (2., 6.), (4., 0.)]],
        'W' => &[&[(0., 0.), (0., 6.), (2., 4.), (4., 6.), (4., 0.)]],
        'X' => &[&[(0., 0.), (4., 6.)], &[(4., 0.), (0., 6.)]],
        'Y' => &[&[(0., 0.), (2., 2.), (4., 0.)], &[(2., 2.), (2., 6.)]],
        'Z' => &[&[(0., 0.), (4., 0.), (0., 6.), (4., 6.)]],
        '1' => &[&[(1., 1.), (2., 0.), (2., 6.)], &[(1., 6.), (3., 6.)]],
        '2' => &[&[(0., 0.), (4., 0.), (4., 3.), (0., 3.), (0., 6.), (4., 6.)]],
        '3' => &[
            &[(0., 0.), (4., 0.), (4., 6.), (0., 6.)],
            &[(1., 3.), (4., 3.)],
        ],
        '4' => &[&[(0., 0.), (0., 3.), (4., 3.)], &[(4., 0.), (4., 6.)]],
        '6' => &[&[(4., 0.), (0., 0.), (0., 6.), (4., 6.), (4., 3.), (0., 3.)]],
        '7' => &[&[(0., 0.), (4., 0.), (4., 6.)]],
        '8' => &[
            &[(0., 0.), (4., 0.), (4., 6.), (0., 6.), (0., 0.)],
            &[(0., 3.), (4., 3.)],
        ],
        '9' => &[&[(4., 3.), (0., 3.), (0., 0.), (4., 0.), (4., 6.), (0., 6.)]],
        '-' => &[&[(1., 3.), (3., 3.)]],
        '+' => &[&[(2., 1.), (2., 5.)], &[(0., 3.), (4., 3.)]],
        '.' => &[&[(1.6, 6.), (2.4, 6.)]],
        ':' => &[&[(2., 1.4), (2., 2.2)], &[(2., 3.8), (2., 4.6)]],
        '/' => &[&[(0., 6.), (4., 0.)]],
        '>' => &[&[(0., 0.), (4., 3.), (0., 6.)]],
        '<' => &[&[(4., 0.), (0., 3.), (4., 6.)]],
        '!' => &[&[(2., 0.), (2., 4.)], &[(2., 5.4), (2., 6.)]],
        '?' => &[
            &[(0., 1.), (0., 0.), (4., 0.), (4., 3.), (2., 3.), (2., 4.)],
            &[(2., 5.4), (2., 6.)],
        ],
        '\'' => &[&[(2., 0.), (2., 1.6)]],
        _ => return None,
    })
}

/// How a run of vector text is set.
#[derive(Clone, Copy, Debug)]
pub struct VectorText {
    /// Cap height in points.
    pub height: f32,
    /// Phosphor color (the halo); the core is drawn white-hot from it.
    pub color: Color32,
    /// Beam width in points.
    pub stroke: f32,
    /// Forward slant in grid units per unit of height (0.0 = upright).
    pub slant: f32,
}

impl VectorText {
    /// Upright text at `height` in `color`, with a beam width scaled to size.
    pub fn new(height: f32, color: Color32) -> Self {
        Self {
            height,
            color,
            stroke: (height / 12.0).clamp(1.0, 2.6),
            slant: 0.0,
        }
    }

    /// Lean the glyphs forward, synthwave-marquee style.
    pub fn slant(mut self, slant: f32) -> Self {
        self.slant = slant;
        self
    }

    /// Override the beam width.
    pub fn stroke(mut self, stroke: f32) -> Self {
        self.stroke = stroke;
        self
    }

    /// Size of `text` set in this style, in points.
    pub fn size(&self, text: &str) -> Vec2 {
        let unit = self.height / HEIGHT;
        let n = text.chars().count() as f32;
        let width = if n == 0.0 {
            0.0
        } else {
            (n * ADVANCE - 2.0) * unit
        };
        vec2(width + self.slant * self.height, self.height)
    }

    /// Draw `text` with its top-left corner at `origin`; returns its rect.
    pub fn paint(&self, painter: &Painter, origin: Pos2, text: &str, theme: &SonidoTheme) -> Rect {
        let unit = self.height / HEIGHT;
        let mut pen = origin.x;
        let mut points: Vec<Pos2> = Vec::with_capacity(8);
        for c in text.chars() {
            if let Some(strokes) = glyph(c) {
                for stroke in strokes {
                    points.clear();
                    points.extend(stroke.iter().map(|&(x, y)| {
                        // Slant leans the top of the glyph forward.
                        let lean = (HEIGHT - y) * unit * self.slant;
                        pos2(pen + x * unit + lean, origin.y + y * unit)
                    }));
                    glow::neon_polyline(painter, &points, self.color, self.stroke, theme);
                }
            }
            pen += ADVANCE * unit;
        }
        Rect::from_min_size(origin, self.size(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_effect_name_character_has_a_glyph() {
        for c in "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-+.:/<>!?'".chars() {
            assert!(glyph(c).is_some(), "missing glyph {c:?}");
        }
        // Every glyph point sits on the 4 × 6 grid.
        for c in "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789".chars() {
            for stroke in glyph(c).unwrap() {
                for &(x, y) in *stroke {
                    assert!((0.0..=4.0).contains(&x) && (0.0..=6.0).contains(&y), "{c}");
                }
            }
        }
    }

    #[test]
    fn size_scales_with_height_and_length() {
        let style = VectorText::new(12.0, Color32::WHITE);
        assert_eq!(style.size("").x, 0.0);
        assert_eq!(style.size("A"), vec2(8.0, 12.0));
        assert_eq!(style.size("AB").x, 20.0);
    }
}
