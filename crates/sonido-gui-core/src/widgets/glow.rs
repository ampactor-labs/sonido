//! Phosphor bloom and scanline rendering primitives.
//!
//! Every arcade-styled widget calls these functions to produce the CRT glow
//! effect: a sharp element plus a soft halo at reduced alpha. All functions
//! check `theme.reduced_fx` and skip the bloom layer when true.

use egui::{Color32, Painter, Pos2, Rect, Stroke, pos2};

use crate::theme::SonidoTheme;

/// Dim a color to ghost/inactive intensity.
pub fn ghost(color: Color32, theme: &SonidoTheme) -> Color32 {
    color.gamma_multiply(theme.glow.ghost_alpha)
}

/// Create a bloom (halo) version of a color.
fn bloom_color(color: Color32, theme: &SonidoTheme) -> Color32 {
    color.gamma_multiply(theme.glow.bloom_alpha)
}

/// Paint a filled circle with phosphor bloom.
pub fn glow_circle(
    painter: &Painter,
    center: Pos2,
    radius: f32,
    color: Color32,
    theme: &SonidoTheme,
) {
    if !theme.reduced_fx {
        painter.circle_filled(
            center,
            radius + theme.glow.bloom_radius,
            bloom_color(color, theme),
        );
    }
    painter.circle_filled(center, radius, color);
}

/// Paint a circle stroke with phosphor bloom.
pub fn glow_circle_stroke(
    painter: &Painter,
    center: Pos2,
    radius: f32,
    color: Color32,
    stroke_width: f32,
    theme: &SonidoTheme,
) {
    if !theme.reduced_fx {
        painter.circle_stroke(
            center,
            radius,
            Stroke::new(
                stroke_width + theme.glow.bloom_radius * 2.0,
                bloom_color(color, theme),
            ),
        );
    }
    painter.circle_stroke(center, radius, Stroke::new(stroke_width, color));
}

/// Paint a line segment with phosphor bloom.
pub fn glow_line(
    painter: &Painter,
    start: Pos2,
    end: Pos2,
    color: Color32,
    stroke_width: f32,
    theme: &SonidoTheme,
) {
    if !theme.reduced_fx {
        painter.line_segment(
            [start, end],
            Stroke::new(
                stroke_width + theme.glow.bloom_radius * 2.0,
                bloom_color(color, theme),
            ),
        );
    }
    painter.line_segment([start, end], Stroke::new(stroke_width, color));
}

/// Paint a filled rect with phosphor bloom.
pub fn glow_rect(
    painter: &Painter,
    rect: Rect,
    color: Color32,
    corner_radius: f32,
    theme: &SonidoTheme,
) {
    if !theme.reduced_fx {
        let bloomed = rect.expand(theme.glow.bloom_radius);
        painter.rect_filled(bloomed, corner_radius, bloom_color(color, theme));
    }
    painter.rect_filled(rect, corner_radius, color);
}

/// Paint an arc (series of line segments) with phosphor bloom.
///
/// The arc sweeps from `start_angle` to `end_angle` (radians) at the given
/// radius from `center`. Uses 32 segments for smooth appearance.
#[allow(clippy::too_many_arguments)]
pub fn glow_arc(
    painter: &Painter,
    center: Pos2,
    radius: f32,
    start_angle: f32,
    end_angle: f32,
    color: Color32,
    stroke_width: f32,
    theme: &SonidoTheme,
) {
    let segments = 32;
    let sweep = end_angle - start_angle;

    let points: Vec<Pos2> = (0..=segments)
        .map(|i| {
            let t = i as f32 / segments as f32;
            let angle = start_angle + t * sweep;
            pos2(
                center.x + angle.cos() * radius,
                center.y + angle.sin() * radius,
            )
        })
        .collect();

    // Bloom layer
    if !theme.reduced_fx {
        let bloom_stroke = Stroke::new(
            stroke_width + theme.glow.bloom_radius * 2.0,
            bloom_color(color, theme),
        );
        for window in points.windows(2) {
            painter.line_segment([window[0], window[1]], bloom_stroke);
        }
    }

    // Sharp layer
    let sharp_stroke = Stroke::new(stroke_width, color);
    for window in points.windows(2) {
        painter.line_segment([window[0], window[1]], sharp_stroke);
    }
}

/// Paint scanline texture over a rect.
///
/// Draws faint horizontal lines every `line_spacing` pixels.
pub fn scanlines(painter: &Painter, rect: Rect, theme: &SonidoTheme) {
    if !theme.scanlines.enabled || theme.reduced_fx {
        return;
    }

    let color = Color32::from_white_alpha((theme.scanlines.line_opacity * 255.0) as u8);
    let mut y = rect.top();
    while y < rect.bottom() {
        painter.line_segment(
            [pos2(rect.left(), y), pos2(rect.right(), y)],
            Stroke::new(1.0_f32, color),
        );
        y += theme.scanlines.line_spacing;
    }
}

/// The white-hot core of a phosphor trace: the color pushed toward white.
///
/// Real neon and CRT phosphors saturate in the middle of the beam, so the
/// core reads near-white while the halo carries the hue. This is what makes a
/// line look lit rather than painted.
pub fn hot_core(color: Color32) -> Color32 {
    let mix = |c: u8| c + ((255 - c) as f32 * 0.55) as u8;
    Color32::from_rgba_unmultiplied(mix(color.r()), mix(color.g()), mix(color.b()), color.a())
}

/// Paint a neon polyline: a wide colored halo, the line itself, and a
/// white-hot core. With `reduced_fx` only the colored line is drawn.
pub fn neon_polyline(
    painter: &Painter,
    points: &[Pos2],
    color: Color32,
    width: f32,
    theme: &SonidoTheme,
) {
    if points.len() < 2 {
        return;
    }
    if !theme.reduced_fx {
        painter.add(egui::Shape::line(
            points.to_vec(),
            Stroke::new(
                width + theme.glow.bloom_radius * 2.0,
                bloom_color(color, theme),
            ),
        ));
    }
    painter.add(egui::Shape::line(
        points.to_vec(),
        Stroke::new(width, color),
    ));
    if !theme.reduced_fx && width >= 1.2 {
        painter.add(egui::Shape::line(
            points.to_vec(),
            Stroke::new(width * 0.45, hot_core(color)),
        ));
    }
}

/// Paint a single neon segment (see [`neon_polyline`]).
pub fn neon_line(
    painter: &Painter,
    start: Pos2,
    end: Pos2,
    color: Color32,
    width: f32,
    theme: &SonidoTheme,
) {
    neon_polyline(painter, &[start, end], color, width, theme);
}
