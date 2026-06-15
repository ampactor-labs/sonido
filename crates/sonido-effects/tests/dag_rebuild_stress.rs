// The tracking allocator below requires `unsafe` to implement `GlobalAlloc`. The
// workspace denies unsafe_code globally, so opt out for this single test file.
#![allow(unsafe_code)]

//! Host-side stress test for the DAG-rebuild path that the Daisy firmware exercises.
//!
//! The recent Daisy commit history is dense with rebuild-related crashes (DMA overruns,
//! SDRAM OOM on rebuild, spillover-tail leaks). The kernels themselves are stable; the
//! integration surface — `clear_topology` → re-add effects → `compile()` while audio is
//! running — was finding bugs on hardware. This file moves that loop onto the host with
//! a tracking allocator so leaks and bounds violations surface in CI, not on a pedal.
//!
//! Each test treats the audio thread and control thread as the firmware does:
//! mutate topology, then process blocks, then drain `dead_effects` off the audio thread.
//! Invariants checked every block: output is finite, output is bounded. Across rebuilds:
//! `spillover_count` is bounded, `dead_effects` drains cleanly, and the heap returns to
//! a steady-state baseline.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use sonido_core::graph::ProcessingGraph;
use sonido_registry::EffectRegistry;

// ── Tracking allocator ──────────────────────────────────────────────────────
//
// Wraps the system allocator with running totals. Integration-test files compile
// to their own binary, so this allocator is local to the rebuild-stress test
// and can't poison counts in other test files.

struct TrackingAllocator;

static CURRENT_BYTES: AtomicUsize = AtomicUsize::new(0);
static PEAK_BYTES: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            let new = CURRENT_BYTES.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            let mut peak = PEAK_BYTES.load(Ordering::Relaxed);
            while new > peak {
                match PEAK_BYTES.compare_exchange_weak(
                    peak,
                    new,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(observed) => peak = observed,
                }
            }
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) };
        CURRENT_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
    }
}

#[global_allocator]
static GLOBAL: TrackingAllocator = TrackingAllocator;

fn current_bytes() -> usize {
    CURRENT_BYTES.load(Ordering::Relaxed)
}

// ── Test config ─────────────────────────────────────────────────────────────

const SAMPLE_RATE: f32 = 48_000.0;
const BLOCK_SIZE: usize = 128;
const REBUILD_ITERATIONS: usize = 200;
const BLOCKS_BETWEEN_REBUILDS: usize = 8;
const MAX_OUTPUT_ABS: f32 = 8.0;
const MAX_SPILLOVER_TAILS: usize = 64;

// Mix of short-tail and long-tail effects so spillover paths and zero-tail
// paths both run. Reverb/delay/plate exercise the spillover-tail machinery.
const EFFECT_ROTATION: &[&str] = &[
    "distortion",
    "reverb",
    "chorus",
    "delay",
    "phaser",
    "plate_reverb",
    "tremolo",
    "filter",
];

// ── Topologies the firmware switches between live ──────────────────────────

#[derive(Copy, Clone, Debug)]
enum Topology {
    Linear,
    Parallel,
    Fan,
}

const TOPOLOGY_ROTATION: &[Topology] = &[Topology::Linear, Topology::Parallel, Topology::Fan];

// ── Helpers ─────────────────────────────────────────────────────────────────

fn fill_input(buf: &mut [f32], phase: &mut f32, sr: f32) {
    let dphi = 2.0 * std::f32::consts::PI * 220.0 / sr;
    for s in buf.iter_mut() {
        *s = 0.5 * phase.sin();
        *phase += dphi;
        if *phase > std::f32::consts::TAU {
            *phase -= std::f32::consts::TAU;
        }
    }
}

fn assert_block_clean(block: &[f32], context: &str) {
    for (i, &s) in block.iter().enumerate() {
        assert!(
            s.is_finite(),
            "non-finite sample at index {i} ({s}) [{context}]",
        );
        assert!(
            s.abs() <= MAX_OUTPUT_ABS,
            "sample {s} out of bound +/-{MAX_OUTPUT_ABS} at index {i} [{context}]",
        );
    }
}

/// Wires `effects` into `graph` according to `topology`, returning the new node IDs.
/// Mirrors the firmware's `wire_topology` shape so the test path stays close to
/// what the pedal actually does.
fn wire(
    graph: &mut ProcessingGraph,
    inp: sonido_core::graph::NodeId,
    out: sonido_core::graph::NodeId,
    effect_nodes: &[sonido_core::graph::NodeId],
    topology: Topology,
) {
    if effect_nodes.is_empty() {
        graph.connect(inp, out).unwrap();
        return;
    }
    match topology {
        Topology::Linear => {
            let mut prev = inp;
            for &n in effect_nodes {
                graph.connect(prev, n).unwrap();
                prev = n;
            }
            graph.connect(prev, out).unwrap();
        }
        Topology::Parallel => {
            let s = graph.add_split();
            let m = graph.add_merge();
            graph.connect(inp, s).unwrap();
            for &n in effect_nodes {
                graph.connect(s, n).unwrap();
                graph.connect(n, m).unwrap();
            }
            graph.connect(m, out).unwrap();
        }
        Topology::Fan => {
            let s = graph.add_split();
            let m = graph.add_merge();
            let first = effect_nodes[0];
            graph.connect(inp, first).unwrap();
            graph.connect(first, s).unwrap();
            for &n in &effect_nodes[1..] {
                graph.connect(s, n).unwrap();
                graph.connect(n, m).unwrap();
            }
            graph.connect(m, out).unwrap();
        }
    }
}

/// Replays the firmware's in-place rebuild: drop spillover tails, clear the
/// topology, populate fresh effects, wire them, and recompile. The crossfade
/// triggered by `compile()` runs concurrently with the next `process_block`.
fn rebuild_in_place(
    graph: &mut ProcessingGraph,
    registry: &EffectRegistry,
    effect_ids: &[&'static str],
    topology: Topology,
) {
    // The firmware toggles spillover off/on around a rebuild to drain stale tails
    // before the new graph fires. Mirror that exactly.
    graph.set_spillover(false);
    graph.set_spillover(true);
    graph.clear_topology();

    let inp = graph.input_id().unwrap();
    let out = graph.output_id().unwrap();

    let mut effect_nodes = Vec::with_capacity(effect_ids.len());
    for id in effect_ids {
        let effect = registry
            .create(id, SAMPLE_RATE)
            .unwrap_or_else(|| panic!("registry missing effect '{id}'"));
        effect_nodes.push(graph.add_effect(effect));
    }

    wire(graph, inp, out, &effect_nodes, topology);
    graph.compile().unwrap();
}

// ── Tests ───────────────────────────────────────────────────────────────────

/// Sanity: a static linear chain processes blocks with finite, bounded output.
/// If this fails, the rest of the file is moot — something basic is broken.
#[test]
fn static_chain_produces_clean_audio() {
    let registry = EffectRegistry::new();
    let mut graph = ProcessingGraph::new(SAMPLE_RATE, BLOCK_SIZE);
    let inp = graph.add_input();
    let out = graph.add_output();
    let nodes: Vec<_> = ["distortion", "reverb", "delay"]
        .iter()
        .map(|id| graph.add_effect(registry.create(id, SAMPLE_RATE).unwrap()))
        .collect();
    wire(&mut graph, inp, out, &nodes, Topology::Linear);
    graph.compile().unwrap();

    let mut left_in = vec![0.0; BLOCK_SIZE];
    let mut right_in = vec![0.0; BLOCK_SIZE];
    let mut left_out = vec![0.0; BLOCK_SIZE];
    let mut right_out = vec![0.0; BLOCK_SIZE];
    let mut phase = 0.0;

    for _ in 0..256 {
        fill_input(&mut left_in, &mut phase, SAMPLE_RATE);
        right_in.copy_from_slice(&left_in);
        graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);
        assert_block_clean(&left_out, "static linear left");
        assert_block_clean(&right_out, "static linear right");
    }
}

/// Rebuild storm: 200 rebuilds across 3 topologies, processing audio between
/// each rebuild. Asserts output stays finite and bounded across schedule swaps,
/// and that spillover tails don't grow without bound (the SDRAM-OOM bug).
#[test]
fn rebuild_storm_keeps_output_finite_and_tails_bounded() {
    let registry = EffectRegistry::new();
    let mut graph = ProcessingGraph::new(SAMPLE_RATE, BLOCK_SIZE);
    graph.add_input();
    graph.add_output();
    graph.compile().unwrap();

    let mut left_in = vec![0.0; BLOCK_SIZE];
    let mut right_in = vec![0.0; BLOCK_SIZE];
    let mut left_out = vec![0.0; BLOCK_SIZE];
    let mut right_out = vec![0.0; BLOCK_SIZE];
    let mut phase = 0.0;
    let mut max_spillover = 0;

    for iter in 0..REBUILD_ITERATIONS {
        // Pick three effects that rotate through the catalog so every rebuild
        // changes the kernel set, not just the topology.
        let ids = [
            EFFECT_ROTATION[iter % EFFECT_ROTATION.len()],
            EFFECT_ROTATION[(iter + 3) % EFFECT_ROTATION.len()],
            EFFECT_ROTATION[(iter + 5) % EFFECT_ROTATION.len()],
        ];
        let topology = TOPOLOGY_ROTATION[iter % TOPOLOGY_ROTATION.len()];
        rebuild_in_place(&mut graph, &registry, &ids, topology);

        // Process several blocks across the schedule swap. The first few blocks
        // carry crossfade state; the spillover machinery feeds the previous
        // effects silence concurrently.
        for blk in 0..BLOCKS_BETWEEN_REBUILDS {
            fill_input(&mut left_in, &mut phase, SAMPLE_RATE);
            right_in.copy_from_slice(&left_in);
            graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);
            let context = format!("iter={iter} blk={blk} topo={topology:?} ids={ids:?}");
            assert_block_clean(&left_out, &context);
            assert_block_clean(&right_out, &context);
            max_spillover = max_spillover.max(graph.spillover_count());
        }

        // Mirror the firmware's harvest-from-control-thread step. Without this
        // dead_effects grows monotonically and the SDRAM OOM bug returns.
        graph.clear_garbage();
    }

    // Spillover tails are produced by removed reverbs/delays/plates and decay
    // over their tail length. With 3-effect rebuilds and bounded tail times,
    // the high-water mark shouldn't approach pathological numbers.
    assert!(
        max_spillover <= MAX_SPILLOVER_TAILS,
        "spillover_count peaked at {max_spillover} (limit {MAX_SPILLOVER_TAILS})",
    );
}

/// Memory stability: rebuild the graph many times; once dead_effects is
/// drained and tails decay, the live heap should return to a baseline. A real
/// leak would show up as monotonic growth.
#[test]
fn rebuild_storm_does_not_leak_heap() {
    let registry = EffectRegistry::new();
    let mut graph = ProcessingGraph::new(SAMPLE_RATE, BLOCK_SIZE);
    graph.add_input();
    graph.add_output();
    graph.compile().unwrap();

    let mut left_in = vec![0.0; BLOCK_SIZE];
    let mut right_in = vec![0.0; BLOCK_SIZE];
    let mut left_out = vec![0.0; BLOCK_SIZE];
    let mut right_out = vec![0.0; BLOCK_SIZE];
    let mut phase = 0.0;

    // Warm up first: a few rebuilds cause the buffer pool, scratch buffers,
    // and registry caches to size up. Take the baseline after that point so
    // we measure leak, not first-time allocation.
    let warmup = 20;
    for iter in 0..warmup {
        let ids = [
            EFFECT_ROTATION[iter % EFFECT_ROTATION.len()],
            EFFECT_ROTATION[(iter + 1) % EFFECT_ROTATION.len()],
            EFFECT_ROTATION[(iter + 2) % EFFECT_ROTATION.len()],
        ];
        rebuild_in_place(&mut graph, &registry, &ids, Topology::Linear);
        for _ in 0..32 {
            fill_input(&mut left_in, &mut phase, SAMPLE_RATE);
            right_in.copy_from_slice(&left_in);
            graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);
        }
        graph.clear_garbage();
    }

    // Drain any lingering tails by running enough silent blocks for the longest
    // tail (reverb is a few seconds at 48 kHz; 96000 samples = 750 blocks).
    left_in.fill(0.0);
    right_in.fill(0.0);
    for _ in 0..1024 {
        graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);
        graph.clear_garbage();
    }
    let baseline = current_bytes();

    // Now do a long run and measure the post-drain footprint.
    for iter in 0..REBUILD_ITERATIONS {
        let ids = [
            EFFECT_ROTATION[iter % EFFECT_ROTATION.len()],
            EFFECT_ROTATION[(iter + 3) % EFFECT_ROTATION.len()],
            EFFECT_ROTATION[(iter + 5) % EFFECT_ROTATION.len()],
        ];
        let topology = TOPOLOGY_ROTATION[iter % TOPOLOGY_ROTATION.len()];
        rebuild_in_place(&mut graph, &registry, &ids, topology);
        for _ in 0..BLOCKS_BETWEEN_REBUILDS {
            fill_input(&mut left_in, &mut phase, SAMPLE_RATE);
            right_in.copy_from_slice(&left_in);
            graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);
        }
        graph.clear_garbage();
    }
    let after_storm = current_bytes();
    // Drain tails so any memory still held by spillover queues is released too.
    left_in.fill(0.0);
    right_in.fill(0.0);
    for _ in 0..1024 {
        graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);
        graph.clear_garbage();
    }
    let after_drain = current_bytes();

    // Two checks:
    //
    // 1. **In-flight ceiling** — during the storm, with harvest happening every
    //    iteration, the heap shouldn't drift up unboundedly. This catches the
    //    real failure mode (audio-thread runs out of SDRAM mid-set), not just
    //    "things eventually settle." Threshold is generous to accommodate
    //    spillover tails that haven't decayed yet between rebuilds.
    //
    // 2. **Post-drain delta** — once tails finish decaying and dead_effects is
    //    empty, the heap should land back near baseline. A persistent leak
    //    would survive this drain.
    let in_flight = after_storm.saturating_sub(baseline);
    let after = after_drain.saturating_sub(baseline);
    assert!(
        in_flight < 4_000_000,
        "in-flight heap grew {in_flight} bytes during {REBUILD_ITERATIONS} rebuilds: \
         baseline={baseline} after_storm={after_storm} — possible accumulation in DAG path",
    );
    assert!(
        after < 1_000_000,
        "post-drain heap grew {after} bytes after {REBUILD_ITERATIONS} rebuilds: \
         baseline={baseline} after_drain={after_drain} — possible leak in DAG rebuild path",
    );
}

/// Repeatedly remove and re-insert tail-bearing effects without giving the
/// tails time to decay. The Daisy SDRAM-OOM bug surfaced exactly this way:
/// each rebuild stacked another reverb tail on the spillover queue, and the
/// queue grew unboundedly because `set_spillover(false)` wasn't being called.
#[test]
fn rapid_reverb_rebuilds_dont_stack_unboundedly() {
    let registry = EffectRegistry::new();
    let mut graph = ProcessingGraph::new(SAMPLE_RATE, BLOCK_SIZE);
    graph.add_input();
    graph.add_output();
    graph.compile().unwrap();

    let mut left_in = vec![0.0; BLOCK_SIZE];
    let mut right_in = vec![0.0; BLOCK_SIZE];
    let mut left_out = vec![0.0; BLOCK_SIZE];
    let mut right_out = vec![0.0; BLOCK_SIZE];
    let mut phase = 0.0;
    let mut max_spillover = 0;

    // 50 rebuilds, only one block of audio between them — far less time than
    // a reverb tail needs to decay.
    for iter in 0..50 {
        rebuild_in_place(
            &mut graph,
            &registry,
            &["reverb", "plate_reverb", "delay"],
            Topology::Linear,
        );
        fill_input(&mut left_in, &mut phase, SAMPLE_RATE);
        right_in.copy_from_slice(&left_in);
        graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);
        assert_block_clean(&left_out, &format!("rapid iter={iter}"));
        assert_block_clean(&right_out, &format!("rapid iter={iter}"));
        max_spillover = max_spillover.max(graph.spillover_count());
        graph.clear_garbage();
    }

    // Because rebuild_in_place toggles spillover off-then-on around clear_topology,
    // the tail queue should be drained on every rebuild. The high-water mark
    // therefore reflects only the tails created during the single block of
    // processing per iteration — bounded by the number of effects per rebuild.
    assert!(
        max_spillover <= MAX_SPILLOVER_TAILS,
        "spillover_count peaked at {max_spillover} during rapid rebuilds (limit {MAX_SPILLOVER_TAILS})",
    );
}

/// Ensure dead_effects always drains to zero after `clear_garbage`. The audio
/// thread pushes Boxes onto this queue; if `clear_garbage` ever leaves entries
/// behind, the firmware's eventual SDRAM exhaustion is just a matter of time.
#[test]
fn clear_garbage_actually_clears() {
    let registry = EffectRegistry::new();
    let mut graph = ProcessingGraph::new(SAMPLE_RATE, BLOCK_SIZE);
    graph.add_input();
    graph.add_output();
    graph.compile().unwrap();

    rebuild_in_place(
        &mut graph,
        &registry,
        &["reverb", "delay", "plate_reverb"],
        Topology::Linear,
    );

    let mut left_in = vec![0.0; BLOCK_SIZE];
    let mut right_in = vec![0.0; BLOCK_SIZE];
    let mut left_out = vec![0.0; BLOCK_SIZE];
    let mut right_out = vec![0.0; BLOCK_SIZE];
    let mut phase = 0.0;

    for _ in 0..16 {
        fill_input(&mut left_in, &mut phase, SAMPLE_RATE);
        right_in.copy_from_slice(&left_in);
        graph.process_block(&left_in, &right_in, &mut left_out, &mut right_out);
    }

    // Force a fresh rebuild that pushes effects onto dead_effects via spillover-disable.
    rebuild_in_place(
        &mut graph,
        &registry,
        &["distortion", "chorus", "phaser"],
        Topology::Parallel,
    );

    // After at least one rebuild we should have non-empty garbage. (The exact
    // count depends on whether tails moved through spillover first; we only
    // care that `clear_garbage` empties whatever is there.)
    graph.clear_garbage();
    assert_eq!(
        graph.dead_effects.len(),
        0,
        "clear_garbage left effects behind"
    );
}
