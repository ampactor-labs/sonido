//! Baseview [`WindowHandler`] implementation for egui rendering.
//!
//! Creates an OpenGL-backed egui context inside a baseview child window.
//! The handler runs the egui frame loop on each `on_frame()` call:
//!
//! 1. Accumulate input events from baseview
//! 2. Run the egui pass (user's update closure draws the UI)
//! 3. Tessellate egui shapes into GPU primitives
//! 4. Render via `egui_glow::Painter`
//! 5. Swap buffers
//!
//! # References
//!
//! - egui rendering: <https://docs.rs/egui_glow>
//! - baseview windowing: <https://github.com/RustAudio/baseview>
//! - Based on egui-baseview by BillyDM (Codeberg), reduced to plugin essentials

use baseview::gl::GlConfig;
use baseview::{
    Event, EventStatus, Size, Window, WindowHandler, WindowOpenOptions, WindowScalePolicy,
};
use egui::{Context, Pos2, RawInput, Rect, Vec2};
use raw_window_handle::HasRawWindowHandle;
use sonido_gui_core::crt::CrtFrame;
use std::sync::Arc;
use std::time::Instant;

use super::crt::{CrtParams, CrtPipeline};
use super::translate;
use crate::gui::PendingResize;

/// Open an egui-rendered child window inside a host-provided parent.
///
/// This is the sole entry point for plugin GUI creation. It:
/// 1. Configures a baseview child window with OpenGL
/// 2. Initializes `egui_glow::Painter` for rendering
/// 3. Runs the user's `build` closure once for setup (fonts, theme)
/// 4. Calls the `update` closure each frame to draw the UI
///
/// Returns a [`baseview::WindowHandle`] — dropping it closes the window.
///
/// # Type parameters
///
/// - `P`: Parent window (must implement `HasRawWindowHandle`)
/// - `S`: User state threaded through build/update closures
///
/// # Arguments
///
/// - `parent` — Host's parent window handle (from CLAP `set_parent`)
/// - `title` — Window title (typically the effect name)
/// - `width`, `height` — Window dimensions in logical pixels
/// - `scale` — DPI scale factor from the host (1.0 = no scaling)
/// - `state` — User state passed to both closures
/// - `build` — One-time setup (e.g., font configuration)
/// - `update` — Per-frame UI rendering
#[allow(clippy::too_many_arguments)]
pub fn open_parented<P, S>(
    parent: &P,
    title: String,
    width: u32,
    height: u32,
    scale: f64,
    pending_resize: Arc<PendingResize>,
    state: S,
    build: impl FnOnce(&Context, &mut S) + Send + 'static,
    update: impl FnMut(&Context, &mut S) + Send + 'static,
) -> baseview::WindowHandle
where
    P: HasRawWindowHandle,
    S: Send + 'static,
{
    let options = window_options(title, width, height, scale);
    baseview::Window::open_parented(parent, options, move |window: &mut Window<'_>| {
        EguiBridgeHandler::new(
            window,
            width,
            height,
            scale,
            pending_resize,
            state,
            build,
            update,
            None,
        )
    })
}

/// Open a top-level egui window and block until it closes (the preview tool).
///
/// Same rendering path as [`open_parented`], CRT pass included. With
/// `snapshot`, the window saves its framebuffer as a PNG after that many
/// frames and closes itself.
#[allow(clippy::too_many_arguments)]
pub fn open_blocking<S>(
    title: String,
    width: u32,
    height: u32,
    scale: f64,
    state: S,
    build: impl FnOnce(&Context, &mut S) + Send + 'static,
    update: impl FnMut(&Context, &mut S) + Send + 'static,
    snapshot: Option<(std::path::PathBuf, u32)>,
) where
    S: Send + 'static,
{
    let options = window_options(title, width, height, scale);
    let pending_resize = Arc::new(PendingResize::new(width, height));
    baseview::Window::open_blocking(options, move |window: &mut Window<'_>| {
        EguiBridgeHandler::new(
            window,
            width,
            height,
            scale,
            pending_resize,
            state,
            build,
            update,
            snapshot,
        )
    });
}

fn window_options(title: String, width: u32, height: u32, scale: f64) -> WindowOpenOptions {
    WindowOpenOptions {
        title,
        size: Size::new(f64::from(width), f64::from(height)),
        scale: WindowScalePolicy::ScaleFactor(scale),
        gl_config: Some(GlConfig {
            version: (3, 2),
            ..GlConfig::default()
        }),
    }
}

/// Per-frame UI callback: draws the frame against the user state.
type UpdateFn<S> = Box<dyn FnMut(&Context, &mut S) + Send>;

/// Baseview window handler that drives the egui frame loop.
///
/// Created by [`open_parented`], lives for the duration of the plugin GUI window.
/// Each `on_frame` call runs one complete egui frame: input → layout → render.
struct EguiBridgeHandler<S> {
    /// egui context (owns all UI state for this window).
    ctx: Context,
    /// OpenGL function pointers (shared with painter).
    gl: Arc<glow::Context>,
    /// egui OpenGL renderer.
    painter: egui_glow::Painter,
    /// Accumulated input for the next egui frame.
    raw_input: RawInput,
    /// Viewport width in physical pixels.
    physical_width: u32,
    /// Viewport height in physical pixels.
    physical_height: u32,
    /// DPI scale factor.
    scale: f64,
    /// Last known mouse position in logical coordinates.
    mouse_pos: Pos2,
    /// Atomic resize channel from the CLAP host.
    pending_resize: Arc<PendingResize>,
    /// Last logical size applied via `window.resize()`, used to detect changes.
    last_applied_logical: (u32, u32),
    /// User state passed to the update closure.
    state: S,
    /// Per-frame UI callback.
    update_fn: UpdateFn<S>,
    /// The CRT post-process pass, if the driver could build it.
    crt: Option<CrtPipeline>,
    /// Window creation time, for shader animation.
    started: Instant,
    /// Preview tool: write a PNG after N frames, then close.
    snapshot: Option<(std::path::PathBuf, u32)>,
    frames: u32,
}

/// The void every frame clears to (design-system `void`, #07060E).
const VOID: [f32; 3] = [7.0 / 255.0, 6.0 / 255.0, 14.0 / 255.0];

impl<S: Send + 'static> EguiBridgeHandler<S> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        window: &mut Window<'_>,
        width: u32,
        height: u32,
        scale: f64,
        pending_resize: Arc<PendingResize>,
        state: S,
        build: impl FnOnce(&Context, &mut S),
        update: impl FnMut(&Context, &mut S) + Send + 'static,
        snapshot: Option<(std::path::PathBuf, u32)>,
    ) -> Self {
        let gl_context = window
            .gl_context()
            .expect("GL context required for egui bridge");

        #[allow(unsafe_code)]
        // SAFETY: make_current binds the window's own GL context to this
        // thread (the one baseview builds and drives the window on). glow then
        // wraps function pointers loaded from that context, which is valid for
        // the window lifetime.
        let gl = unsafe {
            gl_context.make_current();
            Arc::new(glow::Context::from_loader_function(|s| {
                gl_context.get_proc_address(s)
            }))
        };

        let painter = egui_glow::Painter::new(Arc::clone(&gl), "", None, false)
            .expect("Failed to create egui_glow::Painter");

        let crt = match CrtPipeline::new(&gl) {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!(error = %e, "CRT pass unavailable; rendering plain");
                None
            }
        };

        let mut handler = Self {
            ctx: Context::default(),
            gl,
            painter,
            raw_input: RawInput::default(),
            physical_width: (width as f32 * scale as f32) as u32,
            physical_height: (height as f32 * scale as f32) as u32,
            scale,
            mouse_pos: Pos2::ZERO,
            pending_resize,
            last_applied_logical: (width, height),
            state,
            update_fn: Box::new(update),
            crt,
            started: Instant::now(),
            snapshot,
            frames: 0,
        };

        // Run the one-time build closure (font/theme setup).
        build(&handler.ctx, &mut handler.state);
        handler
    }
}

impl<S: Send + 'static> WindowHandler for EguiBridgeHandler<S> {
    fn on_frame(&mut self, window: &mut Window<'_>) {
        // Apply any pending resize from the CLAP host before rendering.
        let pending = self.pending_resize.get();
        if pending != self.last_applied_logical {
            window.resize(Size::new(f64::from(pending.0), f64::from(pending.1)));
            self.last_applied_logical = pending;
        }

        let gl_context = window.gl_context().unwrap();
        #[allow(unsafe_code)]
        // SAFETY: make_current binds the OpenGL context to the current thread.
        // This is called once per frame from the baseview window handler, which
        // always runs on the same thread. The context is valid for the window lifetime.
        unsafe {
            gl_context.make_current();
        }

        let ppp = self.scale as f32;
        let logical_w = self.physical_width as f32 / ppp;
        let logical_h = self.physical_height as f32 / ppp;

        // Configure egui input for this frame.
        self.raw_input.screen_rect = Some(Rect::from_min_size(
            Pos2::ZERO,
            Vec2::new(logical_w, logical_h),
        ));

        // Tell the UI whether the CRT pass can run; it answers with the glass.
        CrtFrame {
            available: self.crt.is_some(),
            ..CrtFrame::get(&self.ctx)
        }
        .set(&self.ctx);

        // Run the egui frame: user's update closure draws the UI.
        let full_output = self.ctx.run(self.raw_input.take(), |ctx| {
            (self.update_fn)(ctx, &mut self.state);
        });
        let crt_frame = CrtFrame::get(&self.ctx);

        // Tessellate egui shapes into GPU-ready primitives.
        let primitives = self
            .ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);

        let (w, h) = (self.physical_width as i32, self.physical_height as i32);
        let mut use_crt = crt_frame.enabled && self.crt.is_some();
        if use_crt
            && let Some(crt) = &mut self.crt
            && let Err(e) = crt.begin(&self.gl, w, h, VOID)
        {
            tracing::warn!(error = %e, "CRT target allocation failed; rendering plain");
            crt.destroy(&self.gl);
            self.crt = None;
            use_crt = false;
        }
        if !use_crt {
            #[allow(unsafe_code)]
            // SAFETY: GL context made current above.
            unsafe {
                use glow::HasContext;
                self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                self.gl.viewport(0, 0, w, h);
                self.gl.clear_color(VOID[0], VOID[1], VOID[2], 1.0);
                self.gl
                    .clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
            }
        }

        // Render egui primitives via glow (into the CRT scene when active).
        self.painter.paint_and_update_textures(
            [self.physical_width, self.physical_height],
            full_output.pixels_per_point,
            &primitives,
            &full_output.textures_delta,
        );

        if use_crt && let Some(crt) = &mut self.crt {
            crt.finish(
                &self.gl,
                &CrtParams {
                    glass: crt_frame.glass,
                    logical: Vec2::new(logical_w, logical_h),
                    ppp,
                    time: self.started.elapsed().as_secs_f32(),
                    bezel: VOID,
                },
            );
        }

        self.frames += 1;
        let mut close = false;
        if let Some((path, after)) = &self.snapshot
            && self.frames >= *after
        {
            if let Err(e) = super::snapshot::save_png(&self.gl, w, h, path) {
                eprintln!("snapshot failed: {e}");
            }
            close = true;
        }

        gl_context.swap_buffers();
        if close {
            window.close();
        }
    }

    fn on_event(&mut self, _window: &mut Window<'_>, event: Event) -> EventStatus {
        match event {
            Event::Mouse(mouse_event) => {
                translate::translate_mouse(
                    &mouse_event,
                    self.scale,
                    &mut self.raw_input,
                    &mut self.mouse_pos,
                );
                // Let egui decide if the event was consumed.
                if self.ctx.wants_pointer_input() {
                    EventStatus::Captured
                } else {
                    EventStatus::Ignored
                }
            }

            Event::Keyboard(kb_event) => {
                translate::update_modifiers_from_keyboard(kb_event.modifiers, &mut self.raw_input);
                EventStatus::Ignored
            }

            Event::Window(window_event) => {
                translate::translate_window(
                    &window_event,
                    &mut self.raw_input,
                    &mut self.physical_width,
                    &mut self.physical_height,
                );
                EventStatus::Captured
            }
        }
    }
}

impl<S> Drop for EguiBridgeHandler<S> {
    fn drop(&mut self) {
        if let Some(mut crt) = self.crt.take() {
            crt.destroy(&self.gl);
        }
        self.painter.destroy();
    }
}
