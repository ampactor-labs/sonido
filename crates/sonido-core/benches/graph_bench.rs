//! Criterion benchmarks for the DAG routing engine (`sonido-core::graph`).
//!
//! Measures graph overhead independently of DSP cost using a trivial `Gain`
//! effect. Two axes:
//!
//! - **Compile** — topology analysis (Kahn sort + liveness + latency compensation)
//! - **Execute** — `process_block()` throughput at varying block sizes
//!
//! Run with: `cargo bench -p sonido-core -- graph/`
#![allow(missing_docs)]

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use sonido_core::{
    Effect, EffectWithParams, ParamDescriptor, ParameterInfo, graph::ProcessingGraph,
};

const SAMPLE_RATE: f32 = 48000.0;
const BLOCK_SIZE: usize = 256;
const BLOCK_SIZES: &[usize] = &[64, 128, 256, 512, 1024];

// ---------------------------------------------------------------------------
// Trivial Gain effect — isolates graph overhead from DSP cost
// ---------------------------------------------------------------------------

/// Trivial gain effect that multiplies each sample by a constant.
///
/// Used to isolate graph scheduling overhead from DSP processing cost.
/// Implements only `process()` (mono path); stereo defaults to dual-mono.
struct Gain(f32);

impl Effect for Gain {
    fn process(&mut self, input: f32) -> f32 {
        input * self.0
    }

    fn set_sample_rate(&mut self, _sample_rate: f32) {}

    fn reset(&mut self) {}
}

impl ParameterInfo for Gain {
    fn param_count(&self) -> usize {
        0
    }

    fn param_info(&self, _index: usize) -> Option<ParamDescriptor> {
        None
    }

    fn get_param(&self, _index: usize) -> f32 {
        0.0
    }

    fn set_param(&mut self, _index: usize, _value: f32) {}
}

// ---------------------------------------------------------------------------
// Graph constructors
// ---------------------------------------------------------------------------

fn make_linear(n: usize) -> ProcessingGraph {
    let effects: Vec<Box<dyn EffectWithParams + Send>> = (0..n)
        .map(|_| Box::new(Gain(0.9)) as Box<dyn EffectWithParams + Send>)
        .collect();
    ProcessingGraph::linear(effects, SAMPLE_RATE, BLOCK_SIZE).unwrap()
}

fn make_diamond() -> ProcessingGraph {
    let mut graph = ProcessingGraph::new(SAMPLE_RATE, BLOCK_SIZE);
    let input = graph.add_input();
    let split = graph.add_split();
    let a = graph.add_effect(Box::new(Gain(0.8)));
    let b = graph.add_effect(Box::new(Gain(0.7)));
    let merge = graph.add_merge();
    let output = graph.add_output();
    graph.connect(input, split).unwrap();
    graph.connect(split, a).unwrap();
    graph.connect(split, b).unwrap();
    graph.connect(a, merge).unwrap();
    graph.connect(b, merge).unwrap();
    graph.connect(merge, output).unwrap();
    graph.compile().unwrap();
    graph
}

// ---------------------------------------------------------------------------
// Compile benchmarks
// ---------------------------------------------------------------------------

fn bench_compile(c: &mut Criterion) {
    let mut group = c.benchmark_group("graph/compile");

    // 5-node linear chain
    group.bench_function("linear_5", |b| {
        b.iter(|| {
            let effects: Vec<Box<dyn EffectWithParams + Send>> = (0..5)
                .map(|_| Box::new(Gain(0.9)) as Box<dyn EffectWithParams + Send>)
                .collect();
            let mut graph = ProcessingGraph::new(SAMPLE_RATE, BLOCK_SIZE);
            let input = graph.add_input();
            let mut prev = input;
            for effect in effects {
                let node = graph.add_effect(effect);
                graph.connect(prev, node).unwrap();
                prev = node;
            }
            let output = graph.add_output();
            graph.connect(prev, output).unwrap();
            black_box(graph.compile().unwrap());
        });
    });

    // 20-node linear chain — exercises sort + liveness at larger scale
    group.bench_function("linear_20", |b| {
        b.iter(|| {
            let effects: Vec<Box<dyn EffectWithParams + Send>> = (0..20)
                .map(|_| Box::new(Gain(0.9)) as Box<dyn EffectWithParams + Send>)
                .collect();
            let mut graph = ProcessingGraph::new(SAMPLE_RATE, BLOCK_SIZE);
            let input = graph.add_input();
            let mut prev = input;
            for effect in effects {
                let node = graph.add_effect(effect);
                graph.connect(prev, node).unwrap();
                prev = node;
            }
            let output = graph.add_output();
            graph.connect(prev, output).unwrap();
            black_box(graph.compile().unwrap());
        });
    });

    // Diamond — split/merge with parallel paths and latency compensation
    group.bench_function("diamond", |b| {
        b.iter(|| {
            let mut graph = ProcessingGraph::new(SAMPLE_RATE, BLOCK_SIZE);
            let input = graph.add_input();
            let split = graph.add_split();
            let a = graph.add_effect(Box::new(Gain(0.8)));
            let b_node = graph.add_effect(Box::new(Gain(0.7)));
            let merge = graph.add_merge();
            let output = graph.add_output();
            graph.connect(input, split).unwrap();
            graph.connect(split, a).unwrap();
            graph.connect(split, b_node).unwrap();
            graph.connect(a, merge).unwrap();
            graph.connect(b_node, merge).unwrap();
            graph.connect(merge, output).unwrap();
            black_box(graph.compile().unwrap());
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Execute benchmarks — fixed block size 256
// ---------------------------------------------------------------------------

fn bench_execute(c: &mut Criterion) {
    let mut group = c.benchmark_group("graph/execute");

    let left_in = vec![0.5f32; BLOCK_SIZE];
    let right_in = vec![0.5f32; BLOCK_SIZE];
    let mut left_out = vec![0.0f32; BLOCK_SIZE];
    let mut right_out = vec![0.0f32; BLOCK_SIZE];

    // 5-node linear chain, 256 samples
    {
        let mut graph = make_linear(5);
        group.bench_function("linear_5_block256", |b| {
            b.iter(|| {
                graph.process_block(
                    black_box(&left_in),
                    black_box(&right_in),
                    &mut left_out,
                    &mut right_out,
                );
                black_box((&left_out, &right_out));
            });
        });
    }

    // 20-node linear chain, 256 samples
    {
        let mut graph = make_linear(20);
        group.bench_function("linear_20_block256", |b| {
            b.iter(|| {
                graph.process_block(
                    black_box(&left_in),
                    black_box(&right_in),
                    &mut left_out,
                    &mut right_out,
                );
                black_box((&left_out, &right_out));
            });
        });
    }

    // Diamond, 256 samples
    {
        let mut graph = make_diamond();
        group.bench_function("diamond_block256", |b| {
            b.iter(|| {
                graph.process_block(
                    black_box(&left_in),
                    black_box(&right_in),
                    &mut left_out,
                    &mut right_out,
                );
                black_box((&left_out, &right_out));
            });
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Block size sweep — 5-node chain across all standard block sizes
// ---------------------------------------------------------------------------

fn bench_block_sweep(c: &mut Criterion) {
    let mut group = c.benchmark_group("graph/block_sweep");

    for &block_size in BLOCK_SIZES {
        let left_in = vec![0.5f32; block_size];
        let right_in = vec![0.5f32; block_size];
        let mut left_out = vec![0.0f32; block_size];
        let mut right_out = vec![0.0f32; block_size];

        let effects: Vec<Box<dyn EffectWithParams + Send>> = (0..5)
            .map(|_| Box::new(Gain(0.9)) as Box<dyn EffectWithParams + Send>)
            .collect();
        let mut graph = ProcessingGraph::linear(effects, SAMPLE_RATE, block_size).unwrap();

        group.bench_with_input(
            BenchmarkId::new("linear_5", block_size),
            &block_size,
            |b, _| {
                b.iter(|| {
                    graph.process_block(
                        black_box(&left_in),
                        black_box(&right_in),
                        &mut left_out,
                        &mut right_out,
                    );
                    black_box((&left_out, &right_out));
                });
            },
        );
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

criterion_group!(benches, bench_compile, bench_execute, bench_block_sweep);
criterion_main!(benches);
