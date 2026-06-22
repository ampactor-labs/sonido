//! One-shot screenshot capture for the editor.
//!
//! `sonido-gui --screenshot out.png` boots the full app, lets it render and
//! settle for a few frames, captures the framebuffer via eframe's screenshot
//! viewport command, writes a PNG, and exits. Used to generate the README hero
//! image — and handy as a visual smoke test.

use std::path::{Path, PathBuf};

use eframe::egui;
use sonido_gui::SonidoApp;

/// Boot the app solely to capture one screenshot to `path`, then exit.
pub fn run(
    path: PathBuf,
    effect: Option<String>,
    sample_rate: f32,
    buffer_size: usize,
    size: [f32; 2],
) -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(size)
            .with_title("Sonido"),
        ..Default::default()
    };
    eframe::run_native(
        "Sonido",
        options,
        Box::new(move |cc| {
            let mut inner =
                SonidoApp::new(cc, effect.as_deref(), Some(sample_rate), Some(buffer_size));
            // Populate a demo chain so the capture shows the node editor in use.
            if effect.is_none() {
                inner.populate_demo();
            }
            Ok(Box::new(ScreenshotApp {
                inner,
                path,
                frame: 0,
                requested: false,
            }))
        }),
    )
}

struct ScreenshotApp {
    inner: SonidoApp,
    path: PathBuf,
    frame: u32,
    requested: bool,
}

impl eframe::App for ScreenshotApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Render the real UI.
        self.inner.update(ctx, frame);
        self.frame += 1;

        // If the screenshot reply has arrived, save it and quit.
        let shot = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(color) = shot {
            save_png(&color, &self.path);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        // Let layout, fonts, and the node graph settle, then request one capture.
        if self.frame >= 12 && !self.requested {
            self.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        ctx.request_repaint();
    }
}

fn save_png(color: &egui::ColorImage, path: &Path) {
    let [w, h] = color.size;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for px in &color.pixels {
        rgba.extend_from_slice(&px.to_array());
    }
    match image::RgbaImage::from_raw(w as u32, h as u32, rgba) {
        Some(buf) => match buf.save(path) {
            Ok(()) => eprintln!("screenshot: wrote {} ({w}x{h})", path.display()),
            Err(e) => eprintln!("screenshot: failed to write {}: {e}", path.display()),
        },
        None => eprintln!("screenshot: bad image dimensions {w}x{h}"),
    }
}
