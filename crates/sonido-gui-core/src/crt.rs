//! Handshake between a Sonido UI and a host's CRT post-process pass.
//!
//! A renderer that can run the CRT shader (bloom, phosphor persistence,
//! scanlines, shadow mask, curvature) marks [`CrtFrame::available`] before the
//! egui pass; the UI answers with whether the user wants it and which rect is
//! the "glass". Renderers without the pass (eframe, the browser) leave
//! `available` false and the UI falls back to painter-drawn scanlines.

use egui::{Context, Id, Rect};

/// Per-frame CRT state shared through the egui context.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CrtFrame {
    /// Set by the renderer: the GPU pass will run this frame.
    pub available: bool,
    /// Set by the UI: the user wants the effect on.
    pub enabled: bool,
    /// Set by the UI: the rect (in points) treated as the CRT glass —
    /// curvature, scanlines and mask apply inside it; bloom applies everywhere.
    pub glass: Option<Rect>,
}

impl CrtFrame {
    fn id() -> Id {
        Id::new("sonido_crt_frame")
    }

    /// Read this frame's CRT state (default: unavailable).
    pub fn get(ctx: &Context) -> Self {
        ctx.data(|d| d.get_temp::<Self>(Self::id()))
            .unwrap_or_default()
    }

    /// Store this frame's CRT state.
    pub fn set(self, ctx: &Context) {
        ctx.data_mut(|d| d.insert_temp(Self::id(), self));
    }
}
