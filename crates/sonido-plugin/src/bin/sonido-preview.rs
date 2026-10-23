//! Open one Sonido plugin's GUI in a window, no DAW needed.
//!
//! ```text
//! cargo run -p sonido-plugin --bin sonido-preview -- distortion
//! cargo run -p sonido-plugin --bin sonido-preview -- reverb --scale 1.5
//! cargo run -p sonido-plugin --bin sonido-preview -- chorus --snapshot chorus.png
//! cargo run -p sonido-plugin --bin sonido-preview -- delay --bypassed --snapshot attract.png
//! cargo run -p sonido-plugin --bin sonido-preview -- graph-player --demo-rig
//! ```
//!
//! `--snapshot <png>` saves the window after `--frames <n>` frames (default
//! 90) and exits.

use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut effect: Option<String> = None;
    let mut scale = 1.0;
    let mut bypassed = false;
    let mut demo_rig = false;
    let mut snapshot: Option<PathBuf> = None;
    let mut frames = 90;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--scale" => scale = args.next().and_then(|v| v.parse().ok()).unwrap_or(1.0),
            "--bypassed" => bypassed = true,
            "--demo-rig" => demo_rig = true,
            "--snapshot" => snapshot = args.next().map(PathBuf::from),
            "--frames" => frames = args.next().and_then(|v| v.parse().ok()).unwrap_or(90),
            id => effect = Some(id.to_owned()),
        }
    }
    let registry = sonido_registry::EffectRegistry::new();
    let Some(effect) =
        effect.filter(|id| id == "graph-player" || registry.descriptor(id).is_some())
    else {
        let ids: Vec<&str> = registry.all_effects().iter().map(|d| d.id).collect();
        eprintln!(
            "usage: sonido-preview <effect> [--scale N] [--bypassed] [--snapshot out.png [--frames N]]"
        );
        eprintln!("effects: {}", ids.join(" "));
        std::process::exit(2);
    };
    let effect: &'static str = Box::leak(effect.into_boxed_str());
    sonido_plugin::preview::run(
        effect,
        scale,
        bypassed,
        demo_rig,
        snapshot.map(|p| (p, frames)),
    );
}
