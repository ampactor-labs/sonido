//! Sonido GUI - Professional DSP effect processor interface.
//!
//! A real-time audio effects application built on the Sonido DSP framework.
//! Compiles for both native (desktop) and wasm32 (browser) targets.

// ── Native entry point ──────────────────────────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
use clap::Parser;
#[cfg(not(target_arch = "wasm32"))]
use eframe::egui;
use sonido_gui::SonidoApp;

/// Sonido DSP GUI application.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Parser, Debug)]
#[command(name = "sonido-gui")]
#[command(about = "Professional DSP effect processor GUI")]
#[command(version)]
struct Args {
    /// Output audio device name (optional, uses default if not specified)
    #[arg(long)]
    output: Option<String>,

    /// Sample rate in Hz (default: 48000)
    #[arg(long, default_value = "48000")]
    sample_rate: u32,

    /// Buffer size in samples (default: 2048)
    #[arg(long, default_value = "2048")]
    buffer_size: u32,

    /// Launch in single-effect mode with the given effect name.
    ///
    /// Shows a simplified UI with only one effect and no chain view.
    /// Effect names match the registry IDs: distortion, reverb, compressor, etc.
    #[arg(long)]
    effect: Option<String>,
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result<()> {
    use tracing_subscriber::EnvFilter;

    // Initialize tracing subscriber; bridge legacy log:: calls from eframe/egui
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    tracing_log::LogTracer::init().ok();

    let args = Args::parse();

    tracing::info!("Starting Sonido GUI");
    tracing::info!(sample_rate = args.sample_rate, "audio config");
    tracing::info!(buffer_size = args.buffer_size, "audio config");

    if let Some(ref output) = args.output {
        tracing::info!(device = %output, "output device");
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 700.0])
            .with_min_inner_size([800.0, 550.0])
            .with_title("Sonido"),
        ..Default::default()
    };

    let effect = args.effect.clone();
    let sample_rate = args.sample_rate as f32;
    let buffer_size = args.buffer_size as usize;
    eframe::run_native(
        "Sonido",
        options,
        Box::new(move |cc| {
            Ok(Box::new(SonidoApp::new(
                cc,
                effect.as_deref(),
                Some(sample_rate),
                Some(buffer_size),
            )))
        }),
    )
}

// ── Wasm entry point ────────────────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
fn main() {
    use wasm_bindgen::JsCast;

    eframe::WebLogger::init(log::LevelFilter::Debug).ok();
    tracing::info!("Sonido GUI starting (wasm)");

    wasm_bindgen_futures::spawn_local(async {
        let document = web_sys::window()
            .expect("no window")
            .document()
            .expect("no document");
        let canvas = document
            .get_element_by_id("sonido_canvas")
            .expect("no canvas element with id 'sonido_canvas'")
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .expect("element is not a canvas");

        eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(|cc| Ok(Box::new(SonidoApp::new(cc, None, None, None)))),
            )
            .await
            .expect("failed to start eframe");
    });
}
