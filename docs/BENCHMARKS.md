# Benchmarks Guide

This guide covers performance benchmarking for the Sonido DSP framework.

## Quick Start

```bash
# Run all benchmarks
cargo bench

# Run benchmarks for a specific crate
cargo bench -p sonido-effects
cargo bench -p sonido-core

# Run a specific benchmark
cargo bench -- Distortion

# Run with baseline comparison
cargo bench --benches -- --save-baseline main
cargo bench --benches -- --baseline main
```

## Benchmark Framework

Sonido uses [Criterion](https://github.com/bheisler/criterion.rs) for benchmarking, which provides:

- Statistical analysis with confidence intervals
- Automatic outlier detection
- HTML reports with graphs
- Baseline comparisons for regression detection

## Running Benchmarks

### Full Benchmark Suite

```bash
cargo bench
```

Results are saved to `target/criterion/` with HTML reports.

### View Reports

After running benchmarks, open the HTML report:

```bash
# Linux
xdg-open target/criterion/report/index.html

# macOS
open target/criterion/report/index.html

# Windows
start target/criterion/report/index.html
```

### Baseline Comparisons

Compare against a saved baseline to detect regressions:

Criterion flags such as `--save-baseline` need `--benches`: without it, `cargo bench`
also runs each library's libtest harness, which rejects them.

```bash
# Save current performance as baseline
cargo bench --benches -- --save-baseline before_changes

# Make changes, then compare
cargo bench --benches -- --baseline before_changes
```

## Benchmark Organization

Benchmarks are located in `crates/*/benches/`:

```
crates/sonido-core/benches/core_bench.rs          # Core DSP primitives
crates/sonido-core/benches/graph_bench.rs         # DAG routing: compile + process_block overhead
crates/sonido-effects/benches/all_effects.rs      # Every registered effect at defaults (stereo, 256)
crates/sonido-effects/benches/effects_bench.rs    # Effects (mono, stereo, oversampling, chain)
crates/sonido-synth/benches/synth_bench.rs        # Synthesis engine
crates/sonido-analysis/benches/analysis_bench.rs  # FFT, spectral analysis, dynamics, STFT, THD
```

`all_effects.rs` benchmarks every registered effect (all 36) at its default
parameters, so a newly registered effect is covered with no extra code.
`effects_bench.rs` goes deeper on 19 of them (the original set: Distortion,
Compressor, Chorus, Delay, LowPassFilter, Vibrato, Tape, CleanPreamp, Reverb, Flanger,
Phaser, Gate, Tremolo, Wah, Eq, Limiter, Bitcrusher, RingMod, Stage), each wrapped as
`Adapter<XxxKernel>`. It also has stereo groups (`Chorus_Stereo`, `Reverb_Stereo`,
`Phaser_Stereo`, `Flanger_Stereo`, `Delay_Stereo_PingPong`), oversampled distortion
(`Oversampled_{2,4,8}x_Distortion`) and a Preamp → Distortion → Chorus → Delay
`EffectChain` group.

## Standard Block Sizes

The core, graph, effects and synth benchmarks run these block sizes (`BLOCK_SIZES`);
the analysis benchmarks sweep FFT/signal sizes instead:

| Block Size | Latency @ 48kHz | Use Case |
|------------|-----------------|----------|
| 64 samples | 1.3 ms | Ultra-low latency / embedded |
| 128 samples | 2.7 ms | Low latency monitoring |
| 256 samples | 5.3 ms | Standard real-time |
| 512 samples | 10.7 ms | Comfortable real-time |
| 1024 samples | 21.3 ms | Offline processing |

## Results

Measured 2026-10-22 on an Intel i5-6300U (2 cores / 4 threads, 2.4 GHz base, 3.0 GHz turbo), Fedora 45, rustc 1.99, the `bench` profile (fat LTO), on a working tree based on commit `d8abf56`, with the machine otherwise idle. Criterion medians.

### Every effect

`all_effects` runs each registered effect at its default parameters through the registry's `Adapter<K, SmoothedPolicy>` (the plugin and editor path): one stereo 256-frame block of a 440 Hz sine at 48 kHz. "One core" is the share of a single core that real-time processing at 48 kHz takes. The last column is the measured Cortex-M7 cost (next section). Effects marked "(pedal)" are the eight the pedal firmware offers (`PEDAL_EFFECT_IDS`).

| Effect | 256 frames | ns/frame | One core at 48 kHz | Cortex-M7, measured mean |
|---|---|---|---|---|
| tuner | 183.99 µs | 718.7 | 3.45% | 27.5% |
| reverb (pedal) | 179.76 µs | 702.2 | 3.37% | 53.2% |
| cabinet | 92.52 µs | 361.4 | 1.73% | 25.0% |
| amp | 88.61 µs | 346.1 | 1.66% | 34.1% |
| drone | 81.65 µs | 318.9 | 1.53% | 20.1% |
| harmonic_habitat | 81.12 µs | 316.9 | 1.52% | 32.1% |
| tape | 54.84 µs | 214.2 | 1.03% | 25.1% |
| time_stretch | 49.55 µs | 193.6 | 0.93% | 9.9% |
| plate_reverb | 44.93 µs | 175.5 | 0.84% | 15.1% |
| vibrato | 40.41 µs | 157.9 | 0.76% | 16.6% |
| multiband_comp | 33.60 µs | 131.2 | 0.63% | 17.1% |
| bitcrusher (pedal) | 31.81 µs | 124.3 | 0.60% | 9.2% |
| distortion (pedal) | 30.43 µs | 118.9 | 0.57% | 10.2% |
| compressor | 27.26 µs | 106.5 | 0.51% | 13.9% |
| pitch_shift | 24.94 µs | 97.4 | 0.47% | 7.6% |
| texture | 24.06 µs | 94.0 | 0.45% | 3.7% |
| delay (pedal) | 20.97 µs | 81.9 | 0.39% | 7.0% |
| chorus (pedal) | 18.64 µs | 72.8 | 0.35% | 6.8% |
| flanger | 18.10 µs | 70.7 | 0.34% | 6.3% |
| limiter | 13.71 µs | 53.6 | 0.26% | 5.1% |
| phaser (pedal) | 13.64 µs | 53.3 | 0.26% | 7.1% |
| wah | 12.72 µs | 49.7 | 0.24% | 7.9% |
| spring_reverb | 12.33 µs | 48.2 | 0.23% | 5.7% |
| gate | 11.94 µs | 46.6 | 0.22% | 7.1% |
| preamp | 7.75 µs | 30.3 | 0.15% | 3.3% |
| tremolo | 7.61 µs | 29.7 | 0.14% | 4.5% |
| eq | 7.09 µs | 27.7 | 0.13% | 3.6% |
| deesser | 7.08 µs | 27.7 | 0.13% | 3.4% |
| transient_shaper | 6.17 µs | 24.1 | 0.12% | 2.7% |
| stage | 6.03 µs | 23.6 | 0.11% | 3.2% |
| glitch | 6.03 µs | 23.6 | 0.11% | 2.5% |
| filter (pedal) | 5.92 µs | 23.1 | 0.11% | 3.3% |
| shelving_eq | 4.70 µs | 18.4 | 0.09% | 2.5% |
| ringmod (pedal) | 4.21 µs | 16.4 | 0.08% | 2.6% |
| stereo_widener | 3.20 µs | 12.5 | 0.06% | 1.9% |
| looper | 2.78 µs | 10.9 | 0.05% | 1.1% |

The distortion, amp and tape rows predate the soft-clipper fix later the same day (`soft_clip_ad` and `tape_sat_ad` rewritten to stay precise near zero). Re-run alongside the other 33 on a busier machine, those three moved by −8%, +6% and +7% relative to the rest of that run.

All 36 fit easily on the desktop. The heaviest, the tuner (YIN pitch detection over a 2048-sample frame every 1024 samples at a 24 kHz analysis rate) and the reverb (an 8-line FDN with modulated lines and early reflections), each take about 3.4–3.5% of one core.

### Cortex-M7, measured on a Daisy Seed

Measured 2026-10-22 on a Daisy Seed (STM32H750, Cortex-M7 at 480 MHz, the `Performance` clock profile) with `crates/sonido-daisy/examples/bench_kernels.rs`, loaded into AXI SRAM over SWD (`probe-rs run`; the same memory placement as the bootloader's BOOT_SRAM mode), D-cache on, kernel state heap-allocated in SDRAM as on the pedal. Each kernel processes `XxxParams::default()` (equal to its descriptor defaults) on a 220 Hz sine: 8 warm-up blocks, then 128 timed 32-sample stereo blocks, read with the DWT cycle counter. The budget is 320,000 cycles per block (32 samples at 48 kHz). These are bare kernel costs; the firmware adds the codec DMA, the graph and control handling on top, measured in [Embedded](EMBEDDED.md#chain-configurations).

This table is the second full run that day, after the soft-clipper fix. Against the first, the distortion's mean rose 2.4% and its worst block 14% (32,223 → 36,677 cycles), the amp's mean 16% and the tape's 7%; the 33 kernels whose code did not change moved by −8% to +5% (tremolo −8%, gate +5%), which is how far a rebuild alone shifts them through code layout. `sonido_platform::cycle_table` takes the worse of the two runs for the pedal's kernels.

| Kernel | Mean cycles / block | Mean % | Worst block | Worst % | ×25 estimate |
|---|---|---|---|---|---|
| reverb (pedal) | 170,364 | 53.2% | 175,730 | 54.9% | 84% |
| amp | 109,034 | 34.1% | 117,339 | 36.7% | 42% |
| harmonic_habitat | 102,646 | 32.1% | 107,061 | 33.5% | 38% |
| tuner | 88,050 | 27.5% | 199,899 | 62.5% | 86% |
| tape | 80,459 | 25.1% | 95,424 | 29.8% | 26% |
| cabinet | 79,989 | 25.0% | 80,044 | 25.0% | 43% |
| drone | 64,351 | 20.1% | 64,976 | 20.3% | 38% |
| multiband_comp | 54,641 | 17.1% | 55,317 | 17.3% | 16% |
| vibrato | 53,033 | 16.6% | 57,622 | 18.0% | 19% |
| plate_reverb | 48,429 | 15.1% | 50,666 | 15.8% | 21% |
| compressor | 44,578 | 13.9% | 44,693 | 14.0% | 13% |
| distortion (pedal) | 32,555 | 10.2% | 36,677 | 11.5% | 14% |
| time_stretch | 31,683 | 9.9% | 40,888 | 12.8% | 23% |
| bitcrusher (pedal) | 29,474 | 9.2% | 29,551 | 9.2% | 15% |
| wah | 25,233 | 7.9% | 25,280 | 7.9% | 6% |
| pitch_shift | 24,240 | 7.6% | 27,657 | 8.6% | 12% |
| gate | 22,625 | 7.1% | 23,241 | 7.3% | 6% |
| phaser (pedal) | 22,593 | 7.1% | 22,631 | 7.1% | 6% |
| delay (pedal) | 22,359 | 7.0% | 24,445 | 7.6% | 10% |
| chorus (pedal) | 21,659 | 6.8% | 23,209 | 7.3% | 9% |
| flanger | 20,271 | 6.3% | 20,442 | 6.4% | 8% |
| spring_reverb | 18,371 | 5.7% | 20,735 | 6.5% | 6% |
| limiter | 16,396 | 5.1% | 19,284 | 6.0% | 6% |
| tremolo | 14,333 | 4.5% | 14,370 | 4.5% | 4% |
| texture | 11,805 | 3.7% | 12,137 | 3.8% | 11% |
| eq | 11,546 | 3.6% | 11,593 | 3.6% | 3% |
| deesser | 10,869 | 3.4% | 10,925 | 3.4% | 3% |
| preamp | 10,634 | 3.3% | 10,652 | 3.3% | 4% |
| filter (pedal) | 10,517 | 3.3% | 10,557 | 3.3% | 3% |
| stage | 10,325 | 3.2% | 10,365 | 3.2% | 3% |
| transient_shaper | 8,565 | 2.7% | 8,617 | 2.7% | 3% |
| ringmod (pedal) | 8,245 | 2.6% | 8,298 | 2.6% | 2% |
| shelving_eq | 7,965 | 2.5% | 7,999 | 2.5% | 2% |
| glitch | 7,962 | 2.5% | 9,677 | 3.0% | 3% |
| stereo_widener | 6,165 | 1.9% | 6,213 | 1.9% | 2% |
| looper | 3,451 | 1.1% | 3,451 | 1.1% | 1% |

The first run's build, flashed into QSPI over DFU through the Electrosmith bootloader (v6.4) and read over USB serial instead of loaded over SWD, matched its SWD numbers within 1% (reverb mean 170,166 vs 170,298 cycles; tuner worst 200,911 vs 202,253).

Every kernel fits a block on its own, worst block included; the reverb (53% mean, 55% worst) is the heaviest pedal effect. The worst three-slot pedal chain, reverb + distortion + bitcrusher, sums to 72.6% of the budget on mean cost and 75.6% on each kernel's worst block; the whole pedal running it in a Hothouse measured 78.1% mean and 89% at its slowest callback ([Embedded](EMBEDDED.md#chain-configurations)).

The tuner's mean and worst differ because its pitch analysis is periodic. The first on-device run found it doing a whole analysis inside one block: 9,166,077 cycles, 29× that block's budget (an audible dropout every 43 ms on the pedal). It now spreads the analysis one lag per decimated sample across each hop and analyzes its ring in place, which brought the worst block to 63%.

**The ×25 estimate, checked.** The last column is the estimate described below (desktop ns/frame × 25 against the per-sample budget). Its ratio to the measured mean has a median of 1.14 and runs from 0.76 (wah) to 3.13 (tuner): 28 of the 36 kernels land within the ±50% the model claims. Of the eight outside it, all on the pessimistic side, pitch shift, reverb and bitcrusher (1.54–1.62) barely miss; the model overstates the rest by 1.7–3.1×: cabinet 1.7, drone 1.9, time stretch 2.4, texture and tuner 3.1, kernels whose work is periodic, grain-based or convolution. Use the measured table; keep the model for kernels not yet measured.

### Stereo, oversampling, chain

From `effects_bench` (256-frame blocks; mono groups through `process_block`, stereo groups through `process_block_stereo`):

| Comparison | Ratio |
|---|---|
| Chorus / Flanger / Phaser / Reverb, stereo vs mono | 1.01 / 1.02 / 1.00 / 1.00 |
| Delay, ping-pong stereo vs mono | 1.17 |
| Distortion, 2× / 4× / 8× oversampled vs base rate | 2.11 / 3.86 / 7.50 |

Mono costs the same as stereo because the adapter runs each kernel's stereo `process_stereo` either way. Oversampling cost tracks the factor, with the 48-tap Kaiser downsampler on top. The Preamp → Distortion → Chorus → Delay `EffectChain` takes 91.7 µs per 256 frames (1.7% of one core). No shipping path oversamples; `Oversampled<N>` is opt-in.

Run `cargo bench -p sonido-effects` to reproduce (`--bench all_effects` for the table above); reports land in `target/criterion/report/index.html`. Record the CPU, date and commit with any numbers you add here.

For measured cost on the Daisy Seed itself, the firmware example
`crates/sonido-daisy/examples/bench_kernels.rs` reads the Cortex-M7 DWT cycle
counter per kernel and prints the results over USB serial (see
[EMBEDDED.md](EMBEDDED.md)).

## Cortex-M7 Cycle Budget Estimation

Desktop numbers can be scaled to a rough STM32H750 (Daisy Seed) estimate for
planning before measuring on hardware. All 36 kernels are now measured on a
Daisy Seed ([above](#cortex-m7-measured-on-a-daisy-seed)); against those
measurements this model's median error is +14%, and 28 of 36 kernels fall
inside its ±50% (the other eight are overestimates).

### Method

```
Est. cycles/sample = desktop_ns/sample × (desktop_GHz / cortex_GHz) × arch_penalty
                   = desktop_ns/sample × (3.0 / 0.48) × 4
                   = desktop_ns/sample × 25
```

- **Desktop clock**: 3.0 GHz (e.g. i5-6300U turbo); substitute your CPU's clock
- **Architecture penalty**: 4x (Cortex-M7 is dual-issue in-order with no
  floating-point SIMD and small caches, vs x86 out-of-order superscalar)
- **Budget**: At 48 kHz and 480 MHz, one sample = 10,000 cycles. A 64-sample
  buffer (1.3 ms) = 640,000 cycles. Target: <50% CPU = 320,000 cycles/buffer.

### Optimization Paths for Tight Chains

1. Keep the reverb (53% measured) away from other heavy kernels: amp, cabinet, Harmonic Habitat, tape
2. Reduce Phaser stages (cost scales with the number of allpass stages)
3. Use OnePole instead of a biquad for simple filtering
4. Lower the sample rate to 44.1 kHz (~8% more cycles per sample)
5. Oversample only the nonlinear stage that needs it, at the lowest factor that suppresses audible aliasing

### Important Caveats

These estimates have significant uncertainty (±50%):
- The 4x architecture penalty is a rough heuristic. Actual penalty depends on
  memory access patterns, branch prediction, and FPU utilization.
- Cortex-M7 has a single-precision FPU, which helps f32 DSP, but no floating-point SIMD.
- Cache effects differ: the i5-6300U has a 3 MB L3; the STM32H750's Cortex-M7 has
  16 KB I-cache + 16 KB D-cache.
- The Daisy Seed runs at 480 MHz but memory access to AXI SRAM has wait
  states; DTCM is zero-wait-state.
- **Real measurement on hardware is essential.** These estimates guide chain
  planning, not replace profiling.

## Writing New Benchmarks

### Adding an Effect Benchmark

1. Open `crates/sonido-effects/benches/effects_bench.rs`
2. Add a benchmark function:

```rust
fn bench_new_effect(c: &mut Criterion) {
    let mut effect = Adapter::new(NewKernel::new(SAMPLE_RATE), SAMPLE_RATE);
    effect.set_param(0, 1.0); // set typical parameters by index
    bench_effect(c, "NewEffect", effect);
}
```

3. Add to the criterion group:

```rust
criterion_group!(
    benches,
    bench_distortion,
    // ... existing benchmarks
    bench_new_effect,
);
```

### Adding a Core Primitive Benchmark

1. Open `crates/sonido-core/benches/core_bench.rs`
2. Add a per-sample loop benchmark:

```rust
fn bench_new_primitive(c: &mut Criterion) {
    let mut group = c.benchmark_group("NewPrimitive");
    for &block_size in BLOCK_SIZES {
        let input = generate_test_signal(block_size);
        group.bench_with_input(
            BenchmarkId::from_parameter(block_size),
            &block_size,
            |b, _| {
                let mut prim = NewPrimitive::new(SAMPLE_RATE);
                b.iter(|| {
                    for &sample in &input {
                        black_box(prim.process(black_box(sample)));
                    }
                });
            },
        );
    }
    group.finish();
}
```

### Benchmark Helpers

Use `bench_effect` for effects with `process_block`, and `bench_stereo_effect`
for true stereo effects with `process_block_stereo`.

## Profiling

### CPU Profiling with perf (Linux)

The release and bench profiles strip symbols (`strip = "symbols"` in the workspace
`Cargo.toml`), so override that when profiling:

```bash
export CARGO_PROFILE_BENCH_STRIP=none CARGO_PROFILE_BENCH_DEBUG=true

# Profile a specific benchmark
perf record -g cargo bench -p sonido-effects --bench effects_bench -- Reverb --profile-time 10
perf report
```

### Flamegraph

```bash
# Install flamegraph
cargo install flamegraph

# Generate flamegraph for benchmark
CARGO_PROFILE_BENCH_STRIP=none CARGO_PROFILE_BENCH_DEBUG=true \
    cargo flamegraph -p sonido-effects --bench effects_bench -- --bench Reverb
```

### Instruments (macOS)

```bash
# Build the CLI with symbols
CARGO_PROFILE_RELEASE_STRIP=none CARGO_PROFILE_RELEASE_DEBUG=true cargo build --release -p sonido-cli

# Run with Instruments
instruments -t "Time Profiler" target/release/sonido process input.wav output.wav -e reverb
```

## Optimization Guidelines

### DSP Performance Tips

1. **Avoid allocations in process loops**: Pre-allocate buffers
2. **Let the Adapter smooth parameters**: kernels declare a `SmoothingStyle`; settled `SmoothedParam`s short-circuit
3. **Batch operations**: `process_block()` over `process()` when possible
4. **Minimize function calls**: Inline hot paths
5. **Use SIMD when available**: Compiler auto-vectorization helps
6. **Place hot data in DTCM on embedded**: Zero wait-state access at 480 MHz

### Measuring Optimization Impact

```bash
# Before optimization
cargo bench --benches -- Reverb --save-baseline before

# After optimization
cargo bench --benches -- Reverb --baseline before
```

Criterion will show percentage improvement/regression.

## CI Benchmarks

Benchmarks run on-demand via manual dispatch:

```bash
gh workflow run ci-manual.yml -f job=bench
```

The CI bench job runs all 4 crates (core, effects, synth, analysis) and uses `critcmp` for cross-run comparison:

1. **Restore** — `actions/cache` restores the previous `target/criterion/` baseline
2. **Run** — Each crate's benchmarks save results with `--save-baseline current`
3. **Compare** — `critcmp` diffs the cached baseline against the current run
4. **Upload** — Bencher-format text, comparison report, and criterion data stored as artifacts (90-day retention)

The comparison report (`bench-comparison.txt`) shows percentage changes per benchmark group. No regression gate — reporting only for human review. Cache key uses `github.sha`; GitHub LRU-evicts old entries within the 10 GB limit.

### Local comparison

Compare against main branch locally:

```bash
# Compare against main branch
git stash
git checkout main
cargo bench --benches -- --save-baseline main
git checkout -
git stash pop
cargo bench --benches -- --baseline main
```

## Benchmark Reports

### Interpreting Results

Example criterion output (illustrative numbers):

```
Distortion/256          time:   [14.532 us 14.672 us 14.823 us]
                        change: [-2.1234% -0.8756% +0.3892%] (p = 0.18 > 0.05)
                        No change in performance detected.
```

- **time**: [lower bound, estimate, upper bound]
- **change**: Comparison to baseline (if set)
- **p-value**: Statistical confidence (< 0.05 = significant change)

### Outliers

Criterion automatically detects outliers:

```
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
```

High outlier counts may indicate system interference.

## See Also

- [Testing](TESTING.md) - Test guide
- [Contributing](CONTRIBUTING.md) - Development guidelines
- [Architecture](ARCHITECTURE.md) - System design
- [Embedded](EMBEDDED.md) - Embedded target details
