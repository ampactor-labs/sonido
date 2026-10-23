//! Every registered effect, at its default parameters, through the registry's
//! `Adapter<K, SmoothedPolicy>` — the exact path the plugins and the editor
//! run. One stereo 256-frame block of a 440 Hz sine at 48 kHz per iteration.
//!
//! `effects_bench` holds the deeper per-effect studies (block sizes,
//! "typical" settings, oversampling, chains); this one keeps coverage
//! complete: a newly registered effect is benchmarked with no extra code.
//!
//! Run: `cargo bench -p sonido-effects --bench all_effects`

#![allow(missing_docs)] // criterion_group! generates undocumented items

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use sonido_registry::EffectRegistry;
use std::time::Duration;

const SAMPLE_RATE: f32 = 48_000.0;
const BLOCK: usize = 256;

fn bench_all_effects(c: &mut Criterion) {
    let registry = EffectRegistry::new();
    let input: Vec<f32> = (0..BLOCK)
        .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / SAMPLE_RATE).sin() * 0.5)
        .collect();
    let mut group = c.benchmark_group("AllEffects");
    group
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(2));
    for desc in registry.all_effects() {
        let mut effect = registry.create(desc.id, SAMPLE_RATE).expect("registered");
        let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
        group.bench_function(desc.id, |b| {
            b.iter(|| {
                effect.process_block_stereo(black_box(&input), black_box(&input), &mut l, &mut r);
                black_box(l[0] + r[0])
            })
        });
    }
    group.finish();
}

criterion_group!(benches, bench_all_effects);
criterion_main!(benches);
