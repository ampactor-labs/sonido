//! Run a plugin's GUI outside a host, fed by a synthetic audio thread.
//!
//! Used by the `sonido-preview` binary to iterate on the cabinet without a
//! DAW: the same editor, bridge and CRT pass a host gets, with a background
//! thread pushing a test signal through the real effect at real-time pace so
//! the meters and scope move.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use sonido_patch::{Patch, PatchNode, PatchPlayer};
use sonido_registry::{EffectRegistry, effect_by_uid, effect_uid};

use crate::egui_bridge;
use crate::graph_editor::GraphModel;
use crate::graph_player::GraphShared;
use crate::gui::{EditorModel, EditorState, EffectModel, preferred_window_size};
use crate::shared::SonidoShared;

const SAMPLE_RATE: f32 = 48_000.0;
const BLOCK: usize = 256;

/// Open `effect_id`'s editor (or `"graph-player"`'s) in its own window
/// until it is closed (or, with `snapshot`, until it has saved a PNG after
/// `frames` frames). `demo_rig` preloads the graph player with a sample rig.
pub fn run(
    effect_id: &'static str,
    scale: f64,
    bypassed: bool,
    demo_rig: bool,
    snapshot: Option<(PathBuf, u32)>,
) {
    let running = Arc::new(AtomicBool::new(true));
    let (model, size, audio): (Arc<dyn EditorModel>, _, _) = if effect_id == "graph-player" {
        let shared = GraphShared::new(None, None);
        if demo_rig {
            shared.set_patch(demo_patch());
        }
        if let Some(i) = shared.bypass_index() {
            shared.set_value(i, if bypassed { 1.0 } else { 0.0 });
        }
        let size = preferred_window_size(&GraphModel::deck_descriptors(&shared));
        let audio = {
            let (shared, running) = (shared.clone(), Arc::clone(&running));
            std::thread::spawn(move || graph_audio_loop(&shared, &running))
        };
        (Arc::new(GraphModel::new(shared)), size, audio)
    } else {
        let shared = SonidoShared::new(effect_id, None);
        shared.set_bypassed(bypassed);
        shared.set_sample_rate(SAMPLE_RATE);
        let size = preferred_window_size(shared.descriptors());
        let audio = {
            let (shared, running) = (shared.clone(), Arc::clone(&running));
            std::thread::spawn(move || audio_loop(&shared, &running))
        };
        (Arc::new(EffectModel::new(shared)), size, audio)
    };

    egui_bridge::open_blocking(
        format!("Sonido {effect_id} — preview"),
        size.0,
        size.1,
        scale,
        EditorState::new(model),
        EditorState::build,
        EditorState::update,
        snapshot,
    );
    running.store(false, Ordering::Release);
    let _ = audio.join();
}

/// A four-effect demo rig: distortion, chorus, delay, reverb.
fn demo_patch() -> Patch {
    let node = |id: &str| PatchNode::new(effect_uid(id).expect("registered"), Vec::new());
    Patch::linear_chain(
        "Neon Drive",
        vec![
            node("distortion"),
            node("chorus"),
            node("delay"),
            node("reverb"),
        ],
    )
}

/// One block of the test riff into `l`/`r`, starting at sample `n`.
fn riff(l: &mut [f32], r: &mut [f32], n: &mut u64) {
    let notes = [110.0_f32, 146.83, 164.81, 130.81];
    for (a, b) in l.iter_mut().zip(r.iter_mut()) {
        let t = *n as f32 / SAMPLE_RATE;
        let beat = t * 2.0;
        let freq = notes[(beat as usize) % notes.len()];
        let env = (-(beat.fract()) * 3.0).exp();
        let saw = 2.0 * (t * freq).fract() - 1.0;
        let sub = (t * freq * 0.5 * std::f32::consts::TAU).sin();
        let s = (0.6 * saw + 0.4 * sub) * env * 0.5;
        *a = s;
        *b = s;
        *n += 1;
    }
}

/// Sleep until `n` samples' worth of real time has passed since `start`.
fn pace(start: Instant, n: u64) {
    let due = Duration::from_secs_f64(n as f64 / f64::from(SAMPLE_RATE));
    if let Some(wait) = due.checked_sub(start.elapsed()) {
        std::thread::sleep(wait);
    }
}

/// The graph player's stand-in host: runs the rig and rebuilds the player
/// whenever the patch changes (what a host restart does in a DAW).
fn graph_audio_loop(shared: &GraphShared, running: &AtomicBool) {
    let registry = EffectRegistry::new();
    let build = |patch: &Patch| {
        PatchPlayer::from_patch(patch, SAMPLE_RATE, BLOCK, |uid, sr| {
            let id = effect_by_uid(uid)?;
            Some((registry.create(id, sr)?, id))
        })
        .ok()
    };
    shared.set_sample_rate(SAMPLE_RATE);
    let mut generation = shared.generation();
    let mut player = build(&shared.patch());
    let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
    let (mut n, start) = (0u64, Instant::now());
    while running.load(Ordering::Acquire) {
        if shared.generation() != generation {
            generation = shared.generation();
            player = build(&shared.patch());
        }
        riff(&mut l, &mut r, &mut n);
        shared.tap().input(&l, &r);
        if let Some(p) = player.as_mut() {
            crate::graph_player::apply_params(shared, p);
            p.apply_controls();
            p.process_block_stereo(&mut l, &mut r);
        }
        shared.tap().output(&l, &r);
        pace(start, n);
    }
}

/// The test riff through one effect, into the tap, at real-time pace.
fn audio_loop(shared: &SonidoShared, running: &AtomicBool) {
    let mut effect = EffectRegistry::new()
        .create(shared.effect_id(), SAMPLE_RATE)
        .expect("registered effect");
    let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
    let (mut n, start) = (0u64, Instant::now());
    while running.load(Ordering::Acquire) {
        shared.apply_to_effect(effect.as_mut());
        riff(&mut l, &mut r, &mut n);
        shared.tap().input(&l, &r);
        if !shared.is_bypassed() {
            effect.process_block_stereo_inplace(&mut l, &mut r);
        }
        shared.tap().output(&l, &r);
        // Mirror kernel diagnostics (gain reduction, pitch) the way the
        // plugin's audio thread does.
        for i in 0..shared.param_count() {
            if shared.is_read_only(i) {
                shared.set_value(i, effect.effect_get_param(i));
            }
        }
        pace(start, n);
    }
}
