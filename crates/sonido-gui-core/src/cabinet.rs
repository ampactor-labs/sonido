//! The Cabinet: the design system's single-effect layout.
//!
//! Every Sonido effect plugin is the same arcade cabinet, lit in its
//! [`Family`]'s color:
//!
//! ```text
//! ┌ MARQUEE ─ SONIDO / FX·HEAT ─── DISTORTION ─────────── //// (⏻) ┐
//! │ IN ▮▮ │ SCREEN: the family's picture across the glass,   │ OUT ▮▮ │
//! │       │ its labelled axes, readouts                      │        │
//! ├ DECK ──────────────────────────────────────────────────────────┤
//! │ [selectors]                                                    │
//! │  (knob) (knob) (knob) (knob) …  wrapping, centred              │
//! └ FOOTER ─ 48.0 KHZ · LAT 0 ───────────── ONE KERNEL · CRT [ON] ┘
//! ```
//!
//! Rules it keeps (see `docs/DESIGN_SYSTEM.md`):
//! - The power button sits top-right, always; bypass turns the game off
//!   (the screen dims and shows INSERT COIN), it never hides controls.
//! - The screen shows the effect itself: the family's picture (`picture`),
//!   measured from the kernel or the tap, fills the glass, so what a knob
//!   does is visible, not described. No scenery: every line is an axis or
//!   a value.
//! - Values are bright neutral text, labels muted; color carries meaning
//!   (family, level) in short marks, never in body text.
//! - Nothing moves for its own sake; what moves is the signal.

use egui::epaint::Mesh;
use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align, Align2, Color32, FontId, Layout, Pos2, Rect, Response, Sense, Shape, Stroke, Ui,
    UiBuilder, pos2, vec2,
};
use sonido_core::{ParamDescriptor, ParamFlags};

use crate::crt::CrtFrame;
use crate::picture::{
    F_HI, F_LO, FREQ_BINS, IMPULSE_SECONDS, LEVELS_DB, Measured, PictureKind, Probe, bin_freq,
    spectrum,
};
use crate::telemetry::{Peaks, SCOPE_LEN, ScopeFrame, SignalTap};
use crate::theme::{Family, SonidoTheme};
use crate::widgets::{VectorText, bridged_knob_accent, glow};
use crate::{ParamBridge, ParamIndex, SlotIndex};

// ── Layout constants (points) ────────────────────────────────────────────────

const PAD: f32 = 10.0;
const GAP: f32 = 8.0;
const MARQUEE_H: f32 = 54.0;
const FOOTER_H: f32 = 20.0;
const METER_W: f32 = 40.0;
const SCREEN_MIN_H: f32 = 150.0;
const KNOB_D: f32 = 56.0;
const KNOB_CELL: egui::Vec2 = egui::Vec2::new(86.0, 98.0);
const CHIP_H: f32 = 24.0;
/// Height of one selector (label plus control).
pub const SELECTOR_ROW_H: f32 = 50.0;
const CHAR_W: f32 = 7.0;
/// Default cabinet width; height follows the parameter count.
pub const DEFAULT_WIDTH: f32 = 720.0;

// The trigger searches the older half of a 2 × SCOPE_WINDOW read.
const _: () = assert!(SCOPE_WINDOW * 2 <= SCOPE_LEN);

/// Scope frames drawn per trace (≈ 21 ms at 48 kHz: a few cycles of a bass note).
const SCOPE_WINDOW: usize = 1024;

/// What the host can tell the cabinet besides parameters.
#[derive(Clone, Copy)]
pub struct CabinetIo<'a> {
    /// Audio-thread meters and scope, if the host has them.
    pub tap: Option<&'a SignalTap>,
    /// Host sample rate in Hz (0.0 = not running yet).
    pub sample_rate: f32,
    /// Reported latency in samples.
    pub latency: u32,
}

/// What a rig-playing cabinet shows on its screen: the rig and its chain.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RigView {
    /// The rig's name.
    pub name: String,
    /// Its effects in slot order: short name and family.
    pub chain: Vec<(String, Family)>,
    /// What to say when the chain is empty (how to get a rig in).
    pub empty_hint: String,
}

/// A stepped parameter's control style.
#[derive(Clone, Copy, PartialEq)]
enum SelectorKind {
    /// All options side by side (few, short options).
    Segmented,
    /// ◀ current ▶ (many or long options).
    Stepper,
}

fn step_labels(desc: &ParamDescriptor) -> Vec<String> {
    if let Some(labels) = desc.step_labels {
        return labels.iter().map(|s| (*s).to_owned()).collect();
    }
    let n = ((desc.max - desc.min) / desc.step.max(1.0)).round() as usize + 1;
    (0..n)
        .map(|i| format!("{}", desc.min + i as f32 * desc.step.max(1.0)))
        .collect()
}

fn selector_kind(labels: &[String]) -> SelectorKind {
    let chars: usize = labels.iter().map(|l| l.chars().count()).sum();
    if labels.len() <= 4 && chars <= 18 {
        SelectorKind::Segmented
    } else {
        SelectorKind::Stepper
    }
}

/// Width of a selector control (and its slot in the selector row).
pub fn selector_width(desc: &ParamDescriptor) -> f32 {
    let labels = step_labels(desc);
    let control = match selector_kind(&labels) {
        SelectorKind::Segmented => labels
            .iter()
            .map(|l| l.chars().count() as f32 * CHAR_W + 16.0)
            .sum::<f32>(),
        SelectorKind::Stepper => {
            let widest = labels.iter().map(|l| l.chars().count()).max().unwrap_or(1);
            (widest as f32 * CHAR_W + 52.0).min(170.0)
        }
    };
    control.max(desc.short_name.chars().count() as f32 * CHAR_W + 8.0)
}

fn visible(desc: &ParamDescriptor) -> bool {
    !desc.flags.contains(ParamFlags::HIDDEN) && !desc.flags.contains(ParamFlags::READ_ONLY)
}

/// Rows a set of item widths takes when flowed into `width`.
fn flow_rows(widths: impl Iterator<Item = f32>, width: f32) -> usize {
    let mut rows = 0;
    let mut x = f32::INFINITY;
    for w in widths {
        if x + w > width {
            rows += 1;
            x = 0.0;
        }
        x += w + GAP * 2.0;
    }
    rows
}

/// Width the deck lays controls out in, inside a deck `outer` points wide.
fn deck_inner_width(outer: f32) -> f32 {
    outer - 2.0 * PAD
}

/// Height of the control deck for these parameters at inner `width`.
fn deck_height(descs: &[ParamDescriptor], width: f32) -> f32 {
    let selectors = descs
        .iter()
        .filter(|d| visible(d) && d.flags.contains(ParamFlags::STEPPED));
    let selector_rows = flow_rows(selectors.map(selector_width), width);
    let knobs = descs
        .iter()
        .filter(|d| visible(d) && !d.flags.contains(ParamFlags::STEPPED))
        .count();
    let cols = ((width / KNOB_CELL.x).floor() as usize).max(1);
    let knob_rows = knobs.div_ceil(cols);
    selector_rows as f32 * SELECTOR_ROW_H + knob_rows as f32 * KNOB_CELL.y + PAD
}

/// The cabinet's preferred size for an effect with these parameters, in points.
pub fn preferred_size(descs: &[ParamDescriptor]) -> (f32, f32) {
    let width = DEFAULT_WIDTH;
    let deck = deck_height(descs, deck_inner_width(width - 2.0 * PAD));
    let screen = 196.0;
    let height = PAD + MARQUEE_H + GAP + screen + GAP + deck + FOOTER_H + PAD;
    (width, height.round())
}

// ── Meter state ─────────────────────────────────────────────────────────────

/// Ballistics for one stereo meter (all in dBFS).
#[derive(Clone, Copy)]
struct MeterState {
    level: [f32; 2],
    hold: [f32; 2],
    hold_age: [f32; 2],
    clip: bool,
}

const METER_FLOOR: f32 = -54.0;
const METER_CEIL: f32 = 6.0;

impl Default for MeterState {
    fn default() -> Self {
        Self {
            level: [METER_FLOOR; 2],
            hold: [METER_FLOOR; 2],
            hold_age: [0.0; 2],
            clip: false,
        }
    }
}

impl MeterState {
    /// Fold one frame of linear peaks in: instant attack, 26 dB/s fall,
    /// peak hold for 1.2 s, clip latch above 0 dBFS until clicked.
    fn update(&mut self, peaks: [f32; 2], dt: f32) {
        for ch in 0..2 {
            let db = if peaks[ch] > 1e-6 {
                20.0 * peaks[ch].log10()
            } else {
                METER_FLOOR
            };
            if peaks[ch] > 1.0 {
                self.clip = true;
            }
            self.level[ch] = db.max(self.level[ch] - 26.0 * dt).max(METER_FLOOR);
            if db >= self.hold[ch] {
                self.hold[ch] = db;
                self.hold_age[ch] = 0.0;
            } else {
                self.hold_age[ch] += dt;
                if self.hold_age[ch] > 1.2 {
                    self.hold[ch] = (self.hold[ch] - 40.0 * dt).max(self.level[ch]);
                }
            }
        }
    }
}

fn meter_fraction(db: f32) -> f32 {
    ((db - METER_FLOOR) / (METER_CEIL - METER_FLOOR)).clamp(0.0, 1.0)
}

// ── Cabinet ─────────────────────────────────────────────────────────────────

/// Per-instance cabinet UI state. Create once per window; call
/// [`Cabinet::show`] every frame.
pub struct Cabinet {
    effect_id: String,
    title: String,
    description: String,
    family: Family,
    meters_in: MeterState,
    meters_out: MeterState,
    scope: Vec<ScopeFrame>,
    last_time: Option<f64>,
    /// User's CRT switch (only shown when the renderer offers the pass).
    crt_on: bool,
    /// Param under the pointer this frame: (full name, formatted value).
    hover: Option<(String, String)>,
    /// Rig shown on the screen (graph player), if any.
    rig: Option<RigView>,
    /// What the screen shows.
    picture: PictureKind,
    /// The private kernel the probe pictures are measured from.
    probe: Probe,
    /// Smoothed input and output peaks (linear), for the level picture's dot.
    level: [f32; 2],
    /// Param values as of this frame, for the probe and the sync label.
    values: Vec<f32>,
    /// The spectrum picture's input and output (dBFS per bin), smoothed
    /// across frames like an analyzer's ballistics so it settles, not flickers.
    spectra: [Vec<f32>; 2],
}

impl Cabinet {
    /// A cabinet for registry effect `effect_id`.
    pub fn new(effect_id: &str) -> Self {
        let registry = sonido_registry::EffectRegistry::new();
        let desc = registry.descriptor(effect_id);
        Self::custom(
            effect_id,
            desc.map_or(effect_id, |d| d.name),
            desc.map_or("", |d| d.description),
            Family::of_effect(effect_id),
        )
    }

    /// A cabinet for something that isn't a single registry effect (the graph
    /// player): its own title, description and family.
    pub fn custom(id: &str, title: &str, description: &str, family: Family) -> Self {
        Self {
            effect_id: id.to_owned(),
            title: title.to_owned(),
            description: description.to_owned(),
            family,
            meters_in: MeterState::default(),
            meters_out: MeterState::default(),
            scope: vec![ScopeFrame::default(); SCOPE_WINDOW * 2],
            last_time: None,
            crt_on: true,
            hover: None,
            rig: None,
            picture: PictureKind::for_effect(id, family),
            probe: Probe::new(id, PictureKind::for_effect(id, family)),
            level: [0.0; 2],
            values: Vec::new(),
            spectra: [vec![-120.0; FREQ_BINS], vec![-120.0; FREQ_BINS]],
        }
    }

    /// Show `rig` on the screen (or nothing, with `None`).
    pub fn set_rig(&mut self, rig: Option<RigView>) {
        self.rig = rig;
    }

    /// The effect family lighting this cabinet.
    pub fn family(&self) -> Family {
        self.family
    }

    /// Draw the whole cabinet into `ui` (give it the full window, no margin).
    pub fn show(
        &mut self,
        ui: &mut Ui,
        bridge: &dyn ParamBridge,
        slot: SlotIndex,
        io: CabinetIo<'_>,
    ) {
        let theme = SonidoTheme::get(ui.ctx());
        let now = ui.input(|i| i.time);
        let dt = self
            .last_time
            .map_or(1.0 / 60.0, |t| (now - t) as f32)
            .clamp(0.0, 0.1);
        self.last_time = Some(now);
        ui.ctx().request_repaint();

        // Telemetry → meter ballistics, the scope frames, the level dot.
        let peaks = io.tap.map_or(Peaks::default(), SignalTap::take_peaks);
        self.meters_in.update(peaks.input, dt);
        self.meters_out.update(peaks.output, dt);
        if let Some(tap) = io.tap {
            tap.read_scope(&mut self.scope);
        }
        let out_peak = peaks.output[0].max(peaks.output[1]);

        if self.picture == PictureKind::Spectrum {
            let sr = if io.sample_rate > 0.0 {
                io.sample_rate
            } else {
                48_000.0
            };
            let newest = &self.scope[self.scope.len() - SCOPE_WINDOW..];
            let input: Vec<f32> = newest.iter().map(|f| f.input).collect();
            let output: Vec<f32> = newest.iter().map(|f| (f.left + f.right) * 0.5).collect();
            for (smooth, now) in self
                .spectra
                .iter_mut()
                .zip([spectrum(&input, sr), spectrum(&output, sr)])
            {
                // Up fast, down slow.
                for (s, n) in smooth.iter_mut().zip(now) {
                    *s += (n - *s) * if n > *s { 0.5 } else { 0.12 };
                }
            }
        }

        let in_peak = peaks.input[0].max(peaks.input[1]);
        for (l, p) in self.level.iter_mut().zip([in_peak, out_peak]) {
            *l += (p - *l) * if p > *l { 0.5 } else { 0.08 };
        }

        let bypassed = bridge.is_bypassed(slot);

        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0.0, theme.colors.void);
        let inner = full.shrink(PAD);

        let descs: Vec<ParamDescriptor> = (0..bridge.param_count(slot))
            .filter_map(|i| bridge.param_descriptor(slot, ParamIndex(i)))
            .collect();
        let deck_h = deck_height(&descs, deck_inner_width(inner.width()));
        self.values.clear();
        self.values
            .extend((0..descs.len()).map(|i| bridge.get(slot, ParamIndex(i))));
        self.probe.update(&self.values, io.sample_rate, now);

        let marquee = Rect::from_min_size(inner.min, vec2(inner.width(), MARQUEE_H));
        let footer = Rect::from_min_max(pos2(inner.left(), inner.bottom() - FOOTER_H), inner.max);
        let screen_top = marquee.bottom() + GAP;
        let screen_h = (footer.top() - GAP - deck_h - GAP - screen_top).max(SCREEN_MIN_H);
        let screen_row = Rect::from_min_size(
            pos2(inner.left(), screen_top),
            vec2(inner.width(), screen_h),
        );
        let deck = Rect::from_min_max(
            pos2(inner.left(), screen_row.bottom() + GAP),
            pos2(inner.right(), footer.top() - GAP),
        );

        let meter_in = Rect::from_min_size(screen_row.min, vec2(METER_W, screen_h));
        let meter_out = Rect::from_min_max(
            pos2(screen_row.right() - METER_W, screen_row.top()),
            screen_row.max,
        );
        let screen = Rect::from_min_max(
            pos2(meter_in.right() + GAP, screen_row.top()),
            pos2(meter_out.left() - GAP, screen_row.bottom()),
        );

        self.hover = None;
        self.marquee(ui, marquee, bridge, slot, &theme);
        self.deck(ui, deck, bridge, slot, &descs, &theme);
        self.meter(ui, meter_in, "IN", false, &theme);
        self.meter(ui, meter_out, "OUT", true, &theme);
        self.screen(
            ui,
            screen,
            bridge,
            slot,
            bypassed,
            &descs,
            io.sample_rate,
            &theme,
        );
        self.footer(ui, footer, io, &theme);
    }

    // ── Marquee ──────────────────────────────────────────────────────────

    fn marquee(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        bridge: &dyn ParamBridge,
        slot: SlotIndex,
        theme: &SonidoTheme,
    ) {
        let painter = ui.painter_at(rect.expand(4.0));
        let accent = self.family.accent();
        let partner = self.family.partner();

        // Backlit sign: a chamfered plate with the family color bleeding up
        // from the bottom edge.
        let plate = chamfer(rect, 10.0);
        painter.add(Shape::convex_polygon(
            plate.clone(),
            theme.colors.panel,
            Stroke::NONE,
        ));
        vertical_gradient(
            &painter,
            rect.shrink2(vec2(10.0, 1.0)),
            Color32::TRANSPARENT,
            accent.gamma_multiply(0.16),
        );
        painter.add(Shape::closed_line(
            plate,
            Stroke::new(1.0_f32, accent.gamma_multiply(0.55)),
        ));
        glow::neon_line(
            &painter,
            pos2(rect.left() + 12.0, rect.bottom()),
            pos2(rect.right() - 12.0, rect.bottom()),
            accent,
            1.2,
            theme,
        );

        // Brand block: SONIDO wordmark over the family zone.
        let logo = VectorText::new(13.0, theme.colors.text_primary).stroke(1.6);
        let logo_rect = logo.paint(
            &painter,
            pos2(rect.left() + 18.0, rect.top() + 12.0),
            "SONIDO",
            theme,
        );
        spaced_text(
            &painter,
            pos2(logo_rect.left(), logo_rect.bottom() + 6.0),
            &format!("FX//{}", self.family.zone()),
            9.0,
            accent,
            1.5,
        );

        // Divider slash, then the effect name as a slanted neon title.
        let x = logo_rect.right() + 18.0;
        glow::neon_line(
            &painter,
            pos2(x + 8.0, rect.top() + 10.0),
            pos2(x, rect.bottom() - 10.0),
            partner,
            1.4,
            theme,
        );
        let title = VectorText::new(22.0, accent).slant(0.18).stroke(2.2);
        let title_rect = title.paint(
            &painter,
            pos2(x + 20.0, rect.center().y - 11.0),
            &self.title,
            theme,
        );

        // Power: the one button that's always in the same place.
        let power_c = pos2(rect.right() - 30.0, rect.center().y);
        let power_rect = Rect::from_center_size(power_c, vec2(34.0, 34.0));
        let resp = ui.interact(power_rect, ui.id().with("power"), Sense::click());
        let bypassed = bridge.is_bypassed(slot);
        if resp.clicked() {
            bridge.set_bypassed(slot, !bypassed);
        }
        let on = !bypassed;
        let lit = if on { accent } else { theme.colors.text_dim };
        painter.circle_filled(power_c, 15.0, theme.colors.panel_hi);
        if on {
            glow::glow_circle_stroke(
                &painter,
                power_c,
                15.0,
                accent.gamma_multiply(if resp.hovered() { 1.0 } else { 0.7 }),
                1.5,
                theme,
            );
        } else {
            painter.circle_stroke(
                power_c,
                15.0,
                Stroke::new(
                    1.5_f32,
                    if resp.hovered() {
                        accent
                    } else {
                        theme.colors.line
                    },
                ),
            );
        }
        power_icon(&painter, power_c, 7.0, lit, theme);
        if resp.hovered() {
            self.hover = Some((
                "POWER".into(),
                if on {
                    "ON — click to bypass".into()
                } else {
                    "BYPASSED — click to play".into()
                },
            ));
        }

        // Speed stripes between the title and the power button.
        let stripes_right = power_rect.left() - 14.0;
        let stripes_left = (title_rect.right() + 24.0).max(stripes_right - 110.0);
        if stripes_right - stripes_left > 30.0 {
            let n = 6;
            let w = (stripes_right - stripes_left) / n as f32;
            for i in 0..n {
                let x0 = stripes_left + i as f32 * w;
                let a = 0.15 + 0.85 * (i as f32 / (n - 1) as f32);
                let h = rect.height() * 0.36;
                let y0 = rect.center().y - h / 2.0;
                let pts = vec![
                    pos2(x0 + h * 0.5, y0),
                    pos2(x0 + h * 0.5 + w * 0.45, y0),
                    pos2(x0 + w * 0.45, y0 + h),
                    pos2(x0, y0 + h),
                ];
                let c = lerp_color(partner, accent, i as f32 / (n - 1) as f32).gamma_multiply(a);
                painter.add(Shape::convex_polygon(pts, c, Stroke::NONE));
            }
        }
    }

    // ── Screen ───────────────────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    fn screen(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        bridge: &dyn ParamBridge,
        slot: SlotIndex,
        bypassed: bool,
        descs: &[ParamDescriptor],
        sample_rate: f32,
        theme: &SonidoTheme,
    ) {
        let painter = ui.painter_at(rect);
        let accent = self.family.accent();

        // The glass is the effect's own picture and nothing else: a dark
        // tube, the picture's axes, the picture.
        painter.rect_filled(rect, 0.0, theme.colors.void);
        if self.picture == PictureKind::Tuner {
            self.tuner_hud(&painter, rect, bridge, slot, descs, theme);
        } else {
            // A rig's name and chain sit across the top; the picture below.
            let top = if self.rig.as_ref().is_some_and(|r| !r.chain.is_empty()) {
                70.0
            } else {
                12.0
            };
            let plot = Rect::from_min_max(
                pos2(rect.left() + 46.0, rect.top() + top),
                pos2(rect.right() - 18.0, rect.bottom() - 24.0),
            );
            self.picture(&painter, plot, bypassed, descs, sample_rate, theme);
        }

        if let Some(rig) = &self.rig {
            self.rig_hud(&painter, rect, rect, rig, theme);
        }

        // Diagnostics the kernel publishes (gain reduction, gate state).
        self.readouts(&painter, rect, bridge, slot, descs, theme);

        // Attract mode: bypass is "game over", not a hidden state.
        if bypassed {
            painter.rect_filled(rect, 0.0, theme.colors.void.gamma_multiply(0.55));
            // Lit and still: a blink is fun once and noise the fiftieth time.
            let style = VectorText::new(20.0, accent).stroke(2.0);
            let size = style.size("INSERT COIN");
            style.paint(
                &painter,
                rect.center() - size / 2.0 - vec2(0.0, 10.0),
                "INSERT COIN",
                theme,
            );
            painter.text(
                rect.center() + vec2(0.0, 18.0),
                Align2::CENTER_CENTER,
                "BYPASSED · PRESS POWER",
                FontId::monospace(11.0),
                theme.colors.text_secondary,
            );
        }

        // Glass: hand to the GPU pass if there is one, else draw it here.
        let mut crt = CrtFrame::get(ui.ctx());
        crt.enabled = self.crt_on;
        crt.glass = Some(rect);
        crt.set(ui.ctx());
        if !(crt.available && self.crt_on) {
            glow::scanlines(&painter, rect, theme);
        }
        painter.rect_stroke(
            rect,
            3.0,
            Stroke::new(1.0_f32, theme.colors.line),
            egui::StrokeKind::Outside,
        );
    }

    /// The family's picture across the whole screen, in `plot`: a measured
    /// curve, or the live spectrum or stereo field from the tap. Every line
    /// on it is an axis or a value, and every axis says what it is.
    #[allow(clippy::too_many_lines)]
    fn picture(
        &self,
        painter: &egui::Painter,
        plot: Rect,
        bypassed: bool,
        descs: &[ParamDescriptor],
        sample_rate: f32,
        theme: &SonidoTheme,
    ) {
        let accent = if bypassed {
            theme.colors.text_dim
        } else {
            self.family.accent()
        };
        let partner = self.family.partner();
        // Axis text is read, so it takes the label color; guides stay dim.
        let dim = theme.colors.text_secondary;
        let font = FontId::monospace(10.0);
        let guide = Stroke::new(1.0_f32, theme.colors.text_dim.gamma_multiply(0.35));
        let axis = Stroke::new(1.0_f32, theme.colors.text_dim.gamma_multiply(0.75));
        // Map a value in [lo, hi] to the plot's x or y.
        let fx = |v: f32, lo: f32, hi: f32| plot.left() + (v - lo) / (hi - lo) * plot.width();
        let fy = |v: f32, lo: f32, hi: f32| {
            plot.bottom() - ((v - lo) / (hi - lo)).clamp(0.0, 1.0) * plot.height()
        };
        let log_x = |f: f32| plot.left() + (f / F_LO).ln() / (F_HI / F_LO).ln() * plot.width();
        // A vertical guide at x, labelled under the plot.
        let v_tick = |x: f32, text: &str, stroke: Stroke| {
            painter.line_segment([pos2(x, plot.top()), pos2(x, plot.bottom())], stroke);
            painter.text(
                pos2(x, plot.bottom() + 5.0),
                Align2::CENTER_TOP,
                text,
                font.clone(),
                dim,
            );
        };
        // A horizontal guide at y, labelled left of the plot.
        let h_tick = |y: f32, text: &str, stroke: Stroke| {
            painter.line_segment([pos2(plot.left(), y), pos2(plot.right(), y)], stroke);
            painter.text(
                pos2(plot.left() - 6.0, y),
                Align2::RIGHT_CENTER,
                text,
                font.clone(),
                dim,
            );
        };
        // What an axis measures, at its far end.
        let x_name = |text: &str| {
            painter.text(
                pos2(plot.right(), plot.bottom() - 4.0),
                Align2::RIGHT_BOTTOM,
                text,
                font.clone(),
                dim,
            );
        };
        let y_name = |text: &str| {
            painter.text(
                pos2(plot.left() + 4.0, plot.top()),
                Align2::LEFT_TOP,
                text,
                font.clone(),
                dim,
            );
        };
        let newest = &self.scope[self.scope.len() - SCOPE_WINDOW..];
        let silent = newest
            .iter()
            .all(|f| f.input.abs().max(f.left.abs()).max(f.right.abs()) < 1e-4);
        let no_signal = || {
            painter.text(
                plot.center(),
                Align2::CENTER_CENTER,
                "NO SIGNAL",
                FontId::monospace(12.0),
                dim,
            );
        };
        let mono = |f: &ScopeFrame| (f.left + f.right) * 0.5;

        match (self.picture, self.probe.measured()) {
            (PictureKind::Transfer, Measured::Transfer(curve)) => {
                // Sonido level-matches its effects, so the output is often far
                // under full scale: scale to the curve's own peak, since the
                // shape is the point.
                let top = curve.iter().fold(1e-3_f32, |m, p| m.max(p[1].abs())) * 1.15;
                for (v, text) in [(-1.0, "-1"), (-0.5, "-.5"), (0.5, ".5"), (1.0, "+1")] {
                    v_tick(fx(v, -1.0, 1.0), text, guide);
                }
                v_tick(fx(0.0, -1.0, 1.0), "0", axis);
                h_tick(fy(0.0, -top, top), "0", axis);
                // The signal now, riding the curve: each sample's input
                // against its output.
                if !silent {
                    let trail: Vec<Pos2> = newest
                        .iter()
                        .step_by(2)
                        .map(|f| {
                            pos2(
                                fx(f.input.clamp(-1.0, 1.0), -1.0, 1.0),
                                fy(mono(f), -top, top),
                            )
                        })
                        .collect();
                    painter.add(Shape::line(
                        trail,
                        Stroke::new(1.0_f32, partner.gamma_multiply(0.6)),
                    ));
                }
                let pts: Vec<Pos2> = curve
                    .iter()
                    .map(|p| pos2(fx(p[0], -1.0, 1.0), fy(p[1], -top, top)))
                    .collect();
                glow::neon_polyline(painter, &pts, accent, 2.0, theme);
                x_name("IN");
                y_name("OUT");
            }
            (PictureKind::Level, Measured::Level(curve)) => {
                let lo = LEVELS_DB[0];
                let top = (curve.iter().fold(0.0_f32, |m, &v| m.max(v)) / 6.0).ceil() * 6.0;
                for db in [-48.0_f32, -36.0, -24.0, -12.0, 0.0] {
                    v_tick(fx(db, lo, 0.0), &format!("{db}"), guide);
                    h_tick(fy(db, lo, top), &format!("{db}"), guide);
                }
                // Unity: above it would be gain, under it is reduction.
                painter.line_segment(
                    [
                        pos2(fx(lo, lo, 0.0), fy(lo, lo, top)),
                        pos2(fx(0.0, lo, 0.0), fy(0.0, lo, top)),
                    ],
                    axis,
                );
                let pts: Vec<Pos2> = LEVELS_DB
                    .iter()
                    .zip(curve)
                    .map(|(&i, &o)| pos2(fx(i, lo, 0.0), fy(o, lo, top)))
                    .collect();
                glow::neon_polyline(painter, &pts, accent, 2.0, theme);
                // Where the signal is now.
                let [now_in, now_out] = self.level;
                if now_in > 1e-3 {
                    let to_db = |v: f32| 20.0 * v.max(1e-9).log10();
                    let p = pos2(
                        fx(to_db(now_in).clamp(lo, 0.0), lo, 0.0),
                        fy(to_db(now_out), lo, top),
                    );
                    glow::glow_circle(painter, p, 4.0, theme.colors.text_primary, theme);
                }
                x_name("IN dB");
                y_name("OUT dB");
            }
            (PictureKind::Response, Measured::Response(gains)) => {
                const RANGE: f32 = 24.0;
                for (f, text) in [
                    (50.0, "50"),
                    (100.0, "100"),
                    (200.0, ""),
                    (500.0, "500"),
                    (1000.0, "1k"),
                    (2000.0, ""),
                    (5000.0, "5k"),
                    (10_000.0, "10k"),
                ] {
                    v_tick(log_x(f), text, guide);
                }
                for db in [-12.0_f32, 12.0] {
                    h_tick(fy(db, -RANGE, RANGE), &format!("{db:+}"), guide);
                }
                h_tick(fy(0.0, -RANGE, RANGE), "0", axis);
                let nyquist = self.probe.rate().max(1.0) * 0.5;
                let pts: Vec<Pos2> = (0..FREQ_BINS)
                    .filter(|&k| bin_freq(k) < nyquist)
                    .map(|k| pos2(log_x(bin_freq(k)), fy(gains[k], -RANGE, RANGE)))
                    .collect();
                glow::neon_polyline(painter, &pts, accent, 2.0, theme);
                x_name("Hz");
                y_name("dB");
            }
            (PictureKind::Impulse, Measured::Impulse(env)) => {
                const FLOOR: f32 = -60.0;
                for i in 0..=5 {
                    let s = i as f32 * 0.5;
                    v_tick(fx(s, 0.0, IMPULSE_SECONDS), &format!("{s}"), guide);
                }
                for db in [-40.0_f32, -20.0] {
                    h_tick(fy(db, FLOOR, 0.0), &format!("{db}"), guide);
                }
                h_tick(fy(0.0, FLOOR, 0.0), "0", axis);
                let n = env.len().max(1);
                let x_of = |i: usize| plot.left() + (i as f32 + 0.5) / n as f32 * plot.width();
                for (i, &v) in env.iter().enumerate() {
                    if v <= FLOOR {
                        continue;
                    }
                    painter.line_segment(
                        [
                            pos2(x_of(i), plot.bottom()),
                            pos2(x_of(i), fy(v.min(0.0), FLOOR, 0.0)),
                        ],
                        Stroke::new(1.6_f32, accent.gamma_multiply(0.85)),
                    );
                }
                let tops: Vec<Pos2> = env
                    .iter()
                    .enumerate()
                    .map(|(i, &v)| pos2(x_of(i), fy(v.min(0.0), FLOOR, 0.0)))
                    .collect();
                glow::neon_polyline(painter, &tops, partner, 1.0, theme);
                x_name("s");
                // A synced delay probed without a host runs at the kernel's
                // own default tempo; say so rather than pretend.
                let synced = descs
                    .iter()
                    .zip(&self.values)
                    .any(|(d, &v)| d.string_id.ends_with("_sync") && v > 0.5);
                y_name(if synced {
                    "dB · SYNC @ 120 BPM"
                } else {
                    "dB · IMPULSE RESPONSE"
                });
            }
            (PictureKind::Spectrum, _) => {
                const FLOOR: f32 = -72.0;
                for (f, text) in [
                    (50.0, "50"),
                    (100.0, "100"),
                    (500.0, "500"),
                    (1000.0, "1k"),
                    (5000.0, "5k"),
                    (10_000.0, "10k"),
                ] {
                    v_tick(log_x(f), text, guide);
                }
                for db in [-48.0_f32, -24.0] {
                    h_tick(fy(db, FLOOR, 0.0), &format!("{db}"), guide);
                }
                h_tick(fy(0.0, FLOOR, 0.0), "0", axis);
                if silent {
                    no_signal();
                } else {
                    let sr = if sample_rate > 0.0 {
                        sample_rate
                    } else {
                        48_000.0
                    };
                    let [input, output] = &self.spectra;
                    let line = |db: &[f32]| -> Vec<Pos2> {
                        (0..FREQ_BINS)
                            .filter(|&k| bin_freq(k) < sr * 0.5)
                            .map(|k| pos2(log_x(bin_freq(k)), fy(db[k], FLOOR, 0.0)))
                            .collect()
                    };
                    painter.add(Shape::line(
                        line(input),
                        Stroke::new(1.2_f32, partner.gamma_multiply(0.55)),
                    ));
                    glow::neon_polyline(painter, &line(output), accent, 2.0, theme);
                }
                // Which line is which, in their own colors.
                let key = pos2(plot.left() + 4.0, plot.top());
                let g = painter.text(key, Align2::LEFT_TOP, "IN", font.clone(), partner);
                painter.text(
                    pos2(g.right() + 10.0, key.y),
                    Align2::LEFT_TOP,
                    "OUT",
                    font.clone(),
                    accent,
                );
                // The LFO, where the kernel publishes its phase.
                let phase = descs.iter().zip(&self.values).find_map(|(d, &v)| {
                    (d.flags.contains(ParamFlags::READ_ONLY) && d.name.contains("Phase"))
                        .then_some(v)
                });
                if let Some(ph) = phase {
                    let c = pos2(plot.right() - 14.0, plot.top() + 14.0);
                    painter.circle_stroke(c, 10.0, Stroke::new(1.0_f32, dim));
                    let a = std::f32::consts::TAU * ph - std::f32::consts::FRAC_PI_2;
                    glow::glow_circle(
                        painter,
                        c + vec2(a.cos(), a.sin()) * 10.0,
                        3.0,
                        accent,
                        theme,
                    );
                    painter.text(
                        c - vec2(16.0, 0.0),
                        Align2::RIGHT_CENTER,
                        "LFO",
                        font.clone(),
                        dim,
                    );
                }
                x_name("Hz");
            }
            (PictureKind::Stereo, _) => {
                let c = plot.center();
                let r = plot.height().min(plot.width()) * 0.5;
                for dir in [vec2(-1.0, -1.0), vec2(1.0, -1.0)] {
                    let d = dir.normalized() * r;
                    painter.line_segment([c - d, c + d], guide);
                }
                painter.line_segment([c - vec2(0.0, r), c + vec2(0.0, r)], axis);
                for (at, text, align) in [
                    (
                        c + vec2(-1.0, -1.0).normalized() * r,
                        "L",
                        Align2::RIGHT_BOTTOM,
                    ),
                    (
                        c + vec2(1.0, -1.0).normalized() * r,
                        "R",
                        Align2::LEFT_BOTTOM,
                    ),
                    (c - vec2(0.0, r), "MONO", Align2::CENTER_BOTTOM),
                ] {
                    painter.text(at, align, text, font.clone(), dim);
                }
                if silent {
                    no_signal();
                } else {
                    let peak = newest
                        .iter()
                        .fold(0.0_f32, |m, f| m.max(f.left.abs()).max(f.right.abs()));
                    let gain =
                        (0.9 / peak.max(1e-4)).clamp(1.0, 8.0) * r / std::f32::consts::SQRT_2;
                    // Mid up, side across: mono draws a vertical line.
                    let pts: Vec<Pos2> = newest
                        .iter()
                        .step_by(2)
                        .map(|f| {
                            pos2(
                                c.x + (f.right - f.left) * gain,
                                c.y - (f.left + f.right) * gain,
                            )
                        })
                        .collect();
                    glow::neon_polyline(painter, &pts, accent, 1.2, theme);
                }
            }
            _ => {}
        }
    }

    fn readouts(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        bridge: &dyn ParamBridge,
        slot: SlotIndex,
        descs: &[ParamDescriptor],
        theme: &SonidoTheme,
    ) {
        let mut y = rect.top() + 18.0;
        for (i, d) in descs.iter().enumerate() {
            if !d.flags.contains(ParamFlags::READ_ONLY)
                || d.unit == sonido_core::ParamUnit::None
                    && d.max <= 1.0
                    && d.step_labels.is_none()
                    && d.name.contains("Phase")
            {
                continue;
            }
            if self.effect_id == "tuner" {
                continue;
            }
            let v = bridge.get(slot, ParamIndex(i));
            let text = format!("{} {}", d.short_name.to_uppercase(), d.format_value(v));
            let g =
                painter.layout_no_wrap(text, FontId::monospace(14.0), theme.colors.text_primary);
            let pos = pos2(rect.right() - 22.0 - g.size().x, y);
            painter.rect_filled(
                Rect::from_min_size(pos - vec2(6.0, 2.0), g.size() + vec2(12.0, 4.0)),
                2.0,
                theme.colors.void.gamma_multiply(0.82),
            );
            painter.galley(pos, g, theme.colors.text_primary);
            y += 24.0;
        }
    }

    fn tuner_hud(
        &self,
        painter: &egui::Painter,
        sky: Rect,
        bridge: &dyn ParamBridge,
        slot: SlotIndex,
        descs: &[ParamDescriptor],
        theme: &SonidoTheme,
    ) {
        let read = |needle: &str| {
            descs
                .iter()
                .position(|d| {
                    d.flags.contains(ParamFlags::READ_ONLY)
                        && d.name.to_lowercase().contains(needle)
                })
                .map(|i| bridge.get(slot, ParamIndex(i)))
        };
        let hz = read("hz").unwrap_or(0.0);
        let cents = read("cent").unwrap_or(0.0);
        let reference = descs
            .iter()
            .position(|d| d.name == "Reference")
            .map_or(440.0, |i| bridge.get(slot, ParamIndex(i)));
        let locked = hz > 20.0;
        let note = if locked {
            const NAMES: [&str; 12] = [
                "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
            ];
            let midi = (69.0 + 12.0 * (hz / reference).log2()).round() as i32;
            format!(
                "{}{}",
                NAMES[midi.rem_euclid(12) as usize],
                midi.div_euclid(12) - 1
            )
        } else {
            "--".to_owned()
        };
        let in_tune = locked && cents.abs() < 2.0;
        let color = if in_tune {
            theme.colors.green
        } else {
            self.family.accent()
        };

        // Cents meter: a wide arc of ticks over the note, needle from below.
        let pivot = pos2(sky.center().x, sky.bottom() - 4.0);
        let r = (sky.height() - 14.0).min(sky.width() * 0.32);

        // The note goes big, on a dark plate; plate first, so the needle
        // sweeps over it.
        let style = VectorText::new((sky.height() * 0.34).clamp(22.0, 48.0), color).stroke(2.8);
        let size = style.size(&note);
        let at = pos2(
            sky.center().x - size.x / 2.0,
            pivot.y - r * 0.55 - size.y / 2.0,
        );
        painter.rect_filled(
            Rect::from_min_size(at - vec2(14.0, 10.0), size + vec2(28.0, 34.0)),
            4.0,
            theme.colors.void.gamma_multiply(0.85),
        );

        let span = 1.0_f32; // radians either side of vertical for ±50 cents
        for i in -10..=10 {
            let a = -std::f32::consts::FRAC_PI_2 + i as f32 / 10.0 * span;
            let len = if i == 0 {
                12.0
            } else if i % 5 == 0 {
                9.0
            } else {
                5.0
            };
            let dir = vec2(a.cos(), a.sin());
            let c = if i == 0 {
                theme.colors.green
            } else {
                theme.colors.text_dim
            };
            painter.line_segment(
                [pivot + dir * r, pivot + dir * (r - len)],
                Stroke::new(if i == 0 { 2.0_f32 } else { 1.0_f32 }, c),
            );
        }
        if locked {
            let a = -std::f32::consts::FRAC_PI_2 + (cents / 50.0).clamp(-1.0, 1.0) * span;
            let dir = vec2(a.cos(), a.sin());
            glow::neon_line(
                painter,
                // The outer ring only: the note sits inside it.
                pivot + dir * (r * 0.74),
                pivot + dir * (r - 2.0),
                color,
                2.4,
                theme,
            );
        }

        style.paint(painter, at, &note, theme);
        let detail = if locked {
            format!("{hz:.1} Hz  {cents:+.1}¢")
        } else {
            "play a note".to_owned()
        };
        painter.text(
            pos2(sky.center().x, at.y + size.y + 6.0),
            Align2::CENTER_TOP,
            detail,
            FontId::monospace(12.0),
            theme.colors.text_primary,
        );
    }

    /// The rig's name and chain as lit chips across the top of the sky, or
    /// the empty-rig hint in the middle of the screen.
    fn rig_hud(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        sky: Rect,
        rig: &RigView,
        theme: &SonidoTheme,
    ) {
        if rig.chain.is_empty() {
            painter.rect_filled(rect, 0.0, theme.colors.void.gamma_multiply(0.5));
            let style = VectorText::new(16.0, self.family.accent()).stroke(1.8);
            let title = "NO RIG LOADED";
            let size = style.size(title);
            style.paint(
                painter,
                rect.center() - size / 2.0 - vec2(0.0, 14.0),
                title,
                theme,
            );
            let hint = painter.layout(
                rig.empty_hint.clone(),
                FontId::monospace(11.0),
                theme.colors.text_primary,
                rect.width() - 60.0,
            );
            let at = pos2(rect.center().x - hint.size().x / 2.0, rect.center().y + 8.0);
            painter.rect_filled(
                Rect::from_min_size(at - vec2(8.0, 4.0), hint.size() + vec2(16.0, 8.0)),
                3.0,
                theme.colors.void.gamma_multiply(0.85),
            );
            painter.galley(at, hint, theme.colors.text_primary);
            return;
        }
        let mut x = sky.left() + 22.0;
        let y = sky.top() + 18.0;
        let name = painter.layout_no_wrap(
            rig.name.to_uppercase(),
            FontId::monospace(12.0),
            theme.colors.text_primary,
        );
        let plate = Rect::from_min_size(pos2(x - 6.0, y - 3.0), name.size() + vec2(12.0, 6.0));
        painter.rect_filled(plate, 2.0, theme.colors.void.gamma_multiply(0.85));
        painter.galley(pos2(x, y), name, theme.colors.text_primary);
        let y = y + 22.0;
        for (i, (short, family)) in rig.chain.iter().enumerate() {
            if i > 0 {
                let c = pos2(x - 7.0, y + 9.0);
                painter.add(Shape::convex_polygon(
                    vec![
                        c + vec2(-3.0, -4.0),
                        c + vec2(-3.0, 4.0),
                        c + vec2(3.0, 0.0),
                    ],
                    theme.colors.text_dim,
                    Stroke::NONE,
                ));
            }
            let g = painter.layout_no_wrap(
                short.to_uppercase(),
                FontId::monospace(11.0),
                family.accent(),
            );
            let chip = Rect::from_min_size(pos2(x, y), vec2(g.size().x + 12.0, 18.0));
            if chip.right() > sky.right() - 20.0 {
                break;
            }
            painter.rect_filled(chip, 2.0, theme.colors.void.gamma_multiply(0.85));
            painter.rect_stroke(
                chip,
                2.0,
                Stroke::new(1.0_f32, family.accent().gamma_multiply(0.8)),
                egui::StrokeKind::Inside,
            );
            painter.galley(
                pos2(chip.left() + 6.0, chip.center().y - g.size().y / 2.0),
                g,
                family.accent(),
            );
            x = chip.right() + 14.0;
        }
    }

    // ── Meters ───────────────────────────────────────────────────────────

    fn meter(&mut self, ui: &mut Ui, rect: Rect, label: &str, output: bool, theme: &SonidoTheme) {
        let painter = ui.painter_at(rect.expand(2.0));
        let state = if output {
            &mut self.meters_out
        } else {
            &mut self.meters_in
        };
        painter.rect_filled(rect, 3.0, theme.colors.panel);
        painter.text(
            pos2(rect.center().x, rect.top() + 5.0),
            Align2::CENTER_TOP,
            label,
            FontId::monospace(10.0),
            theme.colors.text_secondary,
        );

        // Clip latch doubles as its reset button.
        let clip_rect = Rect::from_min_size(
            pos2(rect.left() + 4.0, rect.top() + 19.0),
            vec2(rect.width() - 8.0, 13.0),
        );
        let resp = ui.interact(clip_rect, ui.id().with(("clip", output)), Sense::click());
        if resp.clicked() {
            state.clip = false;
        }
        if state.clip {
            painter.rect_filled(clip_rect, 2.0, theme.colors.red);
            painter.text(
                clip_rect.center(),
                Align2::CENTER_CENTER,
                "CLIP",
                FontId::monospace(9.0),
                theme.colors.void,
            );
        } else {
            painter.rect_stroke(
                clip_rect,
                2.0,
                Stroke::new(1.0_f32, theme.colors.line),
                egui::StrokeKind::Inside,
            );
            painter.text(
                clip_rect.center(),
                Align2::CENTER_CENTER,
                "CLIP",
                FontId::monospace(9.0),
                theme.colors.dim,
            );
        }

        let bars = Rect::from_min_max(
            pos2(rect.left() + 6.0, clip_rect.bottom() + 6.0),
            pos2(rect.right() - 6.0, rect.bottom() - 20.0),
        );
        let segs = ((bars.height() / 5.0) as usize).clamp(8, 40);
        let seg_h = bars.height() / segs as f32;
        let bar_w = (bars.width() - 4.0) / 2.0;
        for ch in 0..2 {
            let x = bars.left() + ch as f32 * (bar_w + 4.0);
            let level = meter_fraction(state.level[ch]);
            let hold = meter_fraction(state.hold[ch]);
            for s in 0..segs {
                let f = (s as f32 + 0.5) / segs as f32;
                let db = METER_FLOOR + f * (METER_CEIL - METER_FLOOR);
                let color = if db > 0.0 {
                    theme.colors.red
                } else if db > -9.0 {
                    theme.colors.yellow
                } else {
                    theme.colors.green
                };
                let y1 = bars.bottom() - s as f32 * seg_h;
                let seg = Rect::from_min_max(pos2(x, y1 - seg_h + 1.5), pos2(x + bar_w, y1));
                let is_hold =
                    (f - hold).abs() < 0.5 / segs as f32 && state.hold[ch] > METER_FLOOR + 1.0;
                if f <= level || is_hold {
                    glow::glow_rect(&painter, seg, color, 1.0, theme);
                } else {
                    painter.rect_filled(seg, 1.0, color.gamma_multiply(0.09));
                }
            }
        }
        // 0 dBFS tick.
        let zero_y = bars.bottom() - meter_fraction(0.0) * bars.height();
        painter.line_segment(
            [
                pos2(bars.left() - 3.0, zero_y),
                pos2(bars.right() + 3.0, zero_y),
            ],
            Stroke::new(1.0_f32, theme.colors.text_dim),
        );

        let peak = state.hold[0].max(state.hold[1]);
        let text = if peak <= METER_FLOOR + 0.5 {
            "-inf".to_owned()
        } else {
            format!("{peak:.1}")
        };
        painter.text(
            pos2(rect.center().x, rect.bottom() - 4.0),
            Align2::CENTER_BOTTOM,
            text,
            FontId::monospace(10.0),
            theme.colors.text_primary,
        );
    }

    // ── Deck ─────────────────────────────────────────────────────────────

    fn deck(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        bridge: &dyn ParamBridge,
        slot: SlotIndex,
        descs: &[ParamDescriptor],
        theme: &SonidoTheme,
    ) {
        let painter = ui.painter_at(rect.expand(2.0));
        painter.add(Shape::convex_polygon(
            chamfer(rect, 8.0),
            theme.colors.panel,
            Stroke::new(1.0_f32, theme.colors.line),
        ));
        let accent = self.family.accent();
        let area = Rect::from_min_max(
            pos2(rect.left() + PAD, rect.top() + PAD * 0.6),
            pos2(
                rect.left() + PAD + deck_inner_width(rect.width()),
                rect.bottom(),
            ),
        );

        // Selectors first, flowed left to right.
        let mut x = area.left();
        let mut y = area.top();
        for (i, d) in descs.iter().enumerate() {
            if !visible(d) || !d.flags.contains(ParamFlags::STEPPED) {
                continue;
            }
            let w = selector_width(d);
            if x + w > area.right() && x > area.left() {
                x = area.left();
                y += SELECTOR_ROW_H;
            }
            let cell = Rect::from_min_size(pos2(x, y), vec2(w, SELECTOR_ROW_H));
            if let Some(resp) = selector(ui, cell, bridge, slot, ParamIndex(i), d, accent, theme)
                && resp.hovered()
            {
                self.hover = Some((
                    d.name.to_owned(),
                    d.format_value(bridge.get(slot, ParamIndex(i))),
                ));
            }
            x += w + GAP * 2.0;
        }
        let knobs_top = if x > area.left() || y > area.top() {
            y + SELECTOR_ROW_H
        } else {
            area.top()
        };

        // Knobs: a centred grid.
        let knobs: Vec<usize> = descs
            .iter()
            .enumerate()
            .filter(|(_, d)| visible(d) && !d.flags.contains(ParamFlags::STEPPED))
            .map(|(i, _)| i)
            .collect();
        let cols = ((area.width() / KNOB_CELL.x).floor() as usize).max(1);
        for (row, chunk) in knobs.chunks(cols).enumerate() {
            let row_w = chunk.len() as f32 * KNOB_CELL.x;
            let x0 = area.center().x - row_w / 2.0;
            for (c, &i) in chunk.iter().enumerate() {
                let cell = Rect::from_min_size(
                    pos2(
                        x0 + c as f32 * KNOB_CELL.x,
                        knobs_top + row as f32 * KNOB_CELL.y,
                    ),
                    KNOB_CELL,
                );
                let resp = ui
                    .scope_builder(
                        UiBuilder::new()
                            .max_rect(cell)
                            .layout(Layout::top_down(Align::Center)),
                        |ui| bridged_knob_accent(ui, bridge, slot, ParamIndex(i), accent, KNOB_D),
                    )
                    .inner;
                if resp.hovered() || resp.dragged() {
                    let d = &descs[i];
                    self.hover = Some((
                        d.name.to_owned(),
                        d.format_value(bridge.get(slot, ParamIndex(i))),
                    ));
                }
            }
        }
    }

    // ── Footer ───────────────────────────────────────────────────────────

    fn footer(&mut self, ui: &mut Ui, rect: Rect, io: CabinetIo<'_>, theme: &SonidoTheme) {
        let painter = ui.painter_at(rect);
        let accent = self.family.accent();

        // Right: CRT switch (when the renderer has the pass), then engine facts.
        let crt = CrtFrame::get(ui.ctx());
        let mut right = rect.right();
        if crt.available {
            let chip =
                Rect::from_min_size(pos2(right - 62.0, rect.top()), vec2(62.0, rect.height()));
            let resp = ui.interact(chip, ui.id().with("crt"), Sense::click());
            if resp.clicked() {
                self.crt_on = !self.crt_on;
            }
            let on = self.crt_on;
            painter.rect_stroke(
                chip,
                2.0,
                Stroke::new(
                    1.0_f32,
                    if on || resp.hovered() {
                        accent
                    } else {
                        theme.colors.line
                    },
                ),
                egui::StrokeKind::Inside,
            );
            painter.text(
                chip.center(),
                Align2::CENTER_CENTER,
                if on { "CRT ON" } else { "CRT OFF" },
                FontId::monospace(10.0),
                if on {
                    accent
                } else {
                    theme.colors.text_secondary
                },
            );
            if resp.hovered() {
                self.hover = Some((
                    "CRT".into(),
                    "glass, bloom and phosphor trails — click to toggle".into(),
                ));
            }
            right = chip.left() - 10.0;
        }
        let rate = if io.sample_rate > 0.0 {
            format!("{:.1} kHz", io.sample_rate / 1000.0)
        } else {
            "idle".to_owned()
        };
        let facts = painter.layout_no_wrap(
            format!("{rate} · lat {}", io.latency),
            FontId::monospace(10.0),
            theme.colors.text_dim,
        );
        let facts_x = right - facts.size().x;
        painter.galley(
            pos2(facts_x, rect.center().y - facts.size().y / 2.0),
            facts,
            theme.colors.text_dim,
        );

        // Left: the status line — what's under the pointer, else what this is.
        let (head, body) = match &self.hover {
            Some((name, value)) => (name.to_uppercase(), value.clone()),
            None => (self.title.to_uppercase(), self.description.clone()),
        };
        let mut job = LayoutJob::default();
        job.append(
            &head,
            0.0,
            TextFormat {
                font_id: FontId::monospace(11.0),
                color: accent,
                extra_letter_spacing: 1.0,
                ..Default::default()
            },
        );
        job.append("  ", 0.0, TextFormat::default());
        job.append(
            &body,
            0.0,
            TextFormat {
                font_id: FontId::monospace(11.0),
                color: if self.hover.is_some() {
                    theme.colors.text_primary
                } else {
                    theme.colors.text_secondary
                },
                ..Default::default()
            },
        );
        job.wrap.max_width = (facts_x - rect.left() - 16.0).max(40.0);
        job.wrap.max_rows = 1;
        let g = painter.layout_job(job);
        painter.galley(
            pos2(rect.left() + 2.0, rect.center().y - g.size().y / 2.0),
            g,
            theme.colors.text_primary,
        );
    }
}

// ── Selector widget ─────────────────────────────────────────────────────────

/// A stepped parameter: segmented buttons for a few short options, else a
/// ◀ value ▶ stepper (click the value to advance, scroll to step), with its
/// label above, filling `cell` (see [`selector_width`] and `SELECTOR_ROW_H`).
///
/// Returns the control row's response (secondary clicks land on it, e.g. for
/// a context menu), or `None` for a parameter with no options.
#[allow(clippy::too_many_arguments)]
pub fn selector(
    ui: &mut Ui,
    cell: Rect,
    bridge: &dyn ParamBridge,
    slot: SlotIndex,
    param: ParamIndex,
    desc: &ParamDescriptor,
    accent: Color32,
    theme: &SonidoTheme,
) -> Option<Response> {
    let labels = step_labels(desc);
    if labels.is_empty() {
        return None;
    }
    let step = desc.step.max(1.0);
    let current = (((bridge.get(slot, param) - desc.min) / step)
        .round()
        .max(0.0) as usize)
        .min(labels.len() - 1);
    let set = |i: usize| {
        bridge.begin_set(slot, param);
        bridge.set(slot, param, desc.min + i as f32 * step);
        bridge.end_set(slot, param);
    };
    let painter = ui.painter_at(cell.expand(2.0));
    painter.text(
        cell.left_top(),
        Align2::LEFT_TOP,
        desc.short_name.to_uppercase(),
        FontId::monospace(10.0),
        theme.colors.text_secondary,
    );
    let control_w = cell.width();
    let row = Rect::from_min_size(
        pos2(cell.left(), cell.top() + 14.0),
        vec2(control_w, CHIP_H),
    );
    let whole = ui.interact(row, ui.id().with(("sel", param.0)), Sense::click());

    match selector_kind(&labels) {
        SelectorKind::Segmented => {
            let total: f32 = labels
                .iter()
                .map(|l| l.chars().count() as f32 * CHAR_W + 16.0)
                .sum();
            let mut x = row.left();
            for (i, l) in labels.iter().enumerate() {
                let w = (l.chars().count() as f32 * CHAR_W + 16.0) * row.width() / total;
                let seg = Rect::from_min_size(pos2(x, row.top()), vec2(w - 2.0, row.height()));
                let resp = ui.interact(seg, ui.id().with(("seg", param.0, i)), Sense::click());
                if resp.clicked() && i != current {
                    set(i);
                }
                let on = i == current;
                if on {
                    painter.rect_filled(seg, 2.0, accent.gamma_multiply(0.22));
                    glow::glow_rect(
                        &painter,
                        Rect::from_min_size(
                            pos2(seg.left(), seg.bottom() - 2.0),
                            vec2(seg.width(), 2.0),
                        ),
                        accent,
                        0.0,
                        theme,
                    );
                } else {
                    painter.rect_filled(
                        seg,
                        2.0,
                        if resp.hovered() {
                            theme.colors.panel_hi
                        } else {
                            theme.colors.void
                        },
                    );
                }
                painter.rect_stroke(
                    seg,
                    2.0,
                    Stroke::new(1.0_f32, if on { accent } else { theme.colors.line }),
                    egui::StrokeKind::Inside,
                );
                painter.text(
                    seg.center(),
                    Align2::CENTER_CENTER,
                    l.to_uppercase(),
                    FontId::monospace(11.0),
                    if on {
                        theme.colors.text_primary
                    } else {
                        theme.colors.text_secondary
                    },
                );
                x += w;
            }
        }
        SelectorKind::Stepper => {
            let arrow_w = 20.0;
            let prev = Rect::from_min_size(row.min, vec2(arrow_w, row.height()));
            let next = Rect::from_min_size(
                pos2(row.right() - arrow_w, row.top()),
                vec2(arrow_w, row.height()),
            );
            let mid = Rect::from_min_max(
                pos2(prev.right() + 2.0, row.top()),
                pos2(next.left() - 2.0, row.bottom()),
            );
            let r_prev = ui.interact(prev, ui.id().with(("prev", param.0)), Sense::click());
            let r_next = ui.interact(next, ui.id().with(("next", param.0)), Sense::click());
            let r_mid = ui.interact(mid, ui.id().with(("mid", param.0)), Sense::click());
            let n = labels.len();
            if r_prev.clicked() {
                set((current + n - 1) % n);
            } else if r_next.clicked() || r_mid.clicked() {
                set((current + 1) % n);
            } else if whole.hovered() {
                let scroll = ui.input(|i| i.raw_scroll_delta.y);
                if scroll > 0.0 {
                    set((current + 1).min(n - 1));
                } else if scroll < 0.0 {
                    set(current.saturating_sub(1));
                }
            }
            painter.rect_filled(row, 2.0, theme.colors.void);
            painter.rect_stroke(
                row,
                2.0,
                Stroke::new(
                    1.0_f32,
                    if whole.hovered() {
                        accent
                    } else {
                        theme.colors.line
                    },
                ),
                egui::StrokeKind::Inside,
            );
            for (r, rect, left) in [(&r_prev, prev, true), (&r_next, next, false)] {
                let c = rect.center();
                let s = 4.5;
                let pts = if left {
                    vec![
                        c + vec2(s * 0.6, -s),
                        c + vec2(s * 0.6, s),
                        c + vec2(-s * 0.7, 0.0),
                    ]
                } else {
                    vec![
                        c + vec2(-s * 0.6, -s),
                        c + vec2(-s * 0.6, s),
                        c + vec2(s * 0.7, 0.0),
                    ]
                };
                painter.add(Shape::convex_polygon(
                    pts,
                    if r.hovered() {
                        accent
                    } else {
                        accent.gamma_multiply(0.6)
                    },
                    Stroke::NONE,
                ));
            }
            painter.text(
                mid.center(),
                Align2::CENTER_CENTER,
                labels[current].to_uppercase(),
                FontId::monospace(11.0),
                theme.colors.text_primary,
            );
        }
    }
    Some(whole)
}

// ── Drawing helpers ─────────────────────────────────────────────────────────

/// A rect with its corners cut at 45°: the system's panel shape.
fn chamfer(rect: Rect, c: f32) -> Vec<Pos2> {
    vec![
        pos2(rect.left() + c, rect.top()),
        pos2(rect.right() - c, rect.top()),
        pos2(rect.right(), rect.top() + c),
        pos2(rect.right(), rect.bottom() - c),
        pos2(rect.right() - c, rect.bottom()),
        pos2(rect.left() + c, rect.bottom()),
        pos2(rect.left(), rect.bottom() - c),
        pos2(rect.left(), rect.top() + c),
    ]
}

/// Fill `rect` with a top-to-bottom gradient.
fn vertical_gradient(painter: &egui::Painter, rect: Rect, top: Color32, bottom: Color32) {
    let mut mesh = Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 3, 2);
    painter.add(Shape::mesh(mesh));
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Letter-spaced monospace text, the system's label treatment.
fn spaced_text(
    painter: &egui::Painter,
    pos: Pos2,
    text: &str,
    size: f32,
    color: Color32,
    spacing: f32,
) -> Rect {
    let mut job = LayoutJob::default();
    job.append(
        text,
        0.0,
        TextFormat {
            font_id: FontId::monospace(size),
            color,
            extra_letter_spacing: spacing,
            ..Default::default()
        },
    );
    let g = painter.layout_job(job);
    let rect = Rect::from_min_size(pos, g.size());
    painter.galley(pos, g, color);
    rect
}

/// IEC power symbol as neon strokes.
fn power_icon(painter: &egui::Painter, c: Pos2, r: f32, color: Color32, theme: &SonidoTheme) {
    let pts: Vec<Pos2> = (0..=24)
        .map(|i| {
            let a = -std::f32::consts::FRAC_PI_2
                + 0.65
                + i as f32 / 24.0 * (std::f32::consts::TAU - 1.3);
            c + vec2(a.cos(), a.sin()) * r
        })
        .collect();
    glow::neon_polyline(painter, &pts, color, 1.8, theme);
    glow::neon_line(
        painter,
        c + vec2(0.0, -r - 1.5),
        c + vec2(0.0, -1.0),
        color,
        1.8,
        theme,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_registry::EffectRegistry;

    fn descs(id: &str) -> Vec<ParamDescriptor> {
        let e = EffectRegistry::new().create(id, 48_000.0).unwrap();
        (0..e.effect_param_count())
            .filter_map(|i| e.effect_param_info(i))
            .collect()
    }

    #[test]
    fn preferred_size_fits_every_effect_and_grows_with_params() {
        for d in EffectRegistry::new().all_effects() {
            let (w, h) = preferred_size(&descs(d.id));
            assert_eq!(w, DEFAULT_WIDTH);
            assert!((380.0..=760.0).contains(&h), "{}: {h}", d.id);
        }
        assert!(preferred_size(&descs("stage")).1 > preferred_size(&descs("preamp")).1);
    }

    #[test]
    fn meters_attack_instantly_fall_slowly_and_latch_clip() {
        let mut m = MeterState::default();
        m.update([0.5, 0.0], 0.016);
        assert!((m.level[0] - 20.0 * 0.5f32.log10()).abs() < 0.01);
        m.update([0.0, 0.0], 0.1);
        assert!(m.level[0] < -6.0 && m.level[0] > -9.0);
        assert!(!m.clip);
        m.update([1.2, 0.0], 0.016);
        assert!(m.clip);
    }

    #[test]
    fn long_or_many_options_get_a_stepper() {
        let few = vec!["Off".to_owned(), "On".to_owned()];
        assert!(selector_kind(&few) == SelectorKind::Segmented);
        let many: Vec<String> = (0..12).map(|i| format!("1/{i}")).collect();
        assert!(selector_kind(&many) == SelectorKind::Stepper);
    }
}
