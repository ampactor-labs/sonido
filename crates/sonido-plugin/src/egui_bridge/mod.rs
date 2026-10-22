//! Minimal egui-in-baseview bridge for CLAP plugin GUIs.
//!
//! Replaces the external `egui-baseview` dependency with ~350 lines of focused
//! glue code. Only supports the OpenGL (glow) rendering path and parented
//! windows — the two features sonido plugins actually use.
//!
//! # Architecture
//!
//! ```text
//! DAW parent window (RawWindowHandle)
//!     │
//!     ▼
//! baseview::Window (child window + GL context)
//!     │
//!     ▼
//! EguiBridgeHandler (this module)
//!     ├── translate: baseview events → egui RawInput
//!     ├── frame loop: begin_pass → user UI → end_pass → tessellate
//!     └── render: egui_glow::Painter → OpenGL
//!     │
//!     ▼
//! sonido-gui-core Cabinet (the design system's plugin layout)
//!     │
//!     ▼
//! crt: offscreen scene → phosphor persistence → bloom + glass composite
//! ```
//!
//! # Why not egui-baseview?
//!
//! egui-baseview is a general-purpose bridge (~960 lines) supporting standalone
//! windows, wgpu, clipboard, cursor icons, and full keyboard translation.
//! Plugin UIs need none of that — they are always parented, always OpenGL,
//! mouse-driven with only shift-modifier support. This bridge cuts the
//! dependency and gives sonido full control over its rendering pipeline.

mod crt;
mod handler;
mod snapshot;
mod translate;

pub use handler::{open_blocking, open_parented};
