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
cargo bench -- --save-baseline main
cargo bench -- --baseline main
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

```bash
# Save current performance as baseline
cargo bench -- --save-baseline before_changes

# Make changes, then compare
cargo bench -- --baseline before_changes
```

## Benchmark Organization

Benchmarks are located in `crates/*/benches/`:

```
crates/sonido-core/benches/core_bench.rs       # Core DSP primitives
crates/sonido-effects/benches/effects_bench.rs  # Effects (mono, stereo, oversampling)
crates/sonido-synth/benches/synth_bench.rs      # Synthesis engine
crates/sonido-analysis/benches/analysis_bench.rs # FFT, spectral analysis
```

## Standard Block Sizes

All benchmarks test these block sizes:

| Block Size | Latency @ 48kHz | Use Case |
|------------|-----------------|----------|
| 64 samples | 1.3 ms | Ultra-low latency / embedded |
| 128 samples | 2.7 ms | Low latency monitoring |
| 256 samples | 5.3 ms | Standard real-time |
| 512 samples | 10.7 ms | Comfortable real-time |
| 1024 samples | 21.3 ms | Offline processing |

## Core Primitive Benchmarks

Per-sample processing cost for foundational DSP building blocks. Measured on
Intel i5-6300U @ 3.0 GHz (turbo).

| Primitive | 256 samples | ns/sample | Notes |
|-----------|-------------|-----------|-------|
| DcBlocker | 1.15 µs | 4.5 | 1st-order highpass, cheapest primitive |
| LFO (Saw) | 1.40 µs | 5.5 | Phase accumulator, no trig |
| LFO (Triangle) | 1.37 µs | 5.4 | Folded saw |
| LFO (Square) | 1.52 µs | 5.9 | Threshold comparison |
| LFO (Sine) | 3.13 µs | 12.2 | libm::sinf per sample |
| LFO (S&H) | 3.12 µs | 12.2 | Random generation + hold |
| Biquad | 2.19 µs | 8.6 | Direct Form II, 5 multiplies |
| InterpolatedDelay | 2.47 µs | 9.6 | Linear interpolation read |
| AllpassFilter | 3.21 µs | 12.5 | 556-sample delay + feedback |
| EnvelopeFollower | 3.40 µs | 13.3 | Attack/release ballistics |
| OnePole | 3.86 µs | 15.1 | 1-pole lowpass |
| SmoothedParam (ramping) | 4.28 µs | 16.7 | Active exponential smoothing |
| SmoothedParam (settled) | 4.35 µs | 17.0 | No short-circuit (optimization target) |
| CombFilter | 6.18 µs | 24.1 | 1557-sample delay + damping LPF |
| SVF | 6.12 µs | 23.9 | 2x state update per sample |

Coefficient calculation overhead:
- Biquad `lowpass_coefficients()`: 24 ns (trig-heavy, call once per param change)
- SVF `set_cutoff()`: 15 ns (lightweight recalc)

## Effect Benchmarks

### Individual Effects (Mono Block Processing)

All 19 effects benchmarked with typical parameter settings. Measured on
Intel i5-6300U @ 3.0 GHz (turbo).

| Effect | Key Parameters | 256 samples | ns/sample |
|--------|----------------|-------------|-----------|
| CleanPreamp | gain=12dB | 2.47 µs | 9.6 |
| LowPassFilter | cutoff=1kHz, Q=0.707 | 3.69 µs | 14.4 |
| Tremolo | rate=5Hz, depth=0.8 | 5.05 µs | 19.7 |
| Delay | time=375ms, fb=0.5 | 5.49 µs | 21.4 |
| TapeSaturation | drive=2.0, sat=0.6 | 6.88 µs | 26.9 |
| Gate | thresh=-40dB, atk=1ms | 8.23 µs | 32.2 |
| Flanger | rate=0.5Hz, depth=0.7 | 11.17 µs | 43.6 |
| Wah | auto, sensitivity=0.7 | 13.61 µs | 53.2 |
| Chorus | rate=2Hz, depth=0.7 | 22.80 µs | 89.1 |
| Distortion | drive=20dB, tone=4kHz | 28.08 µs | 109.7 |
| Reverb | room=0.7, decay=0.8 | 49.22 µs | 192.3 |
| Phaser | rate=1Hz, 6 stages | 78.25 µs | 305.6 |
| Compressor | thresh=-20dB, ratio=4:1 | 80.02 µs | 312.6 |
| ParametricEq | 3-band, typical boosts | 113.06 µs | 441.6 |
| MultiVibrato | depth=1.0 | 113.43 µs | 443.1 |
| Limiter | thresh=-6dB, ceil=-0.3dB | TBD | TBD |
| Bitcrusher | bits=8, ds=4 | TBD | TBD |
| RingMod | freq=440Hz, depth=1.0 | TBD | TBD |
| Stage | gain=6dB, width=120% | TBD | TBD |

### Stereo Processing (True Stereo Effects)

True stereo effects process decorrelated L/R channels. Stereo cost vs mono
shows the overhead of dual processing paths.

| Effect | 256 samples (stereo) | ns/sample/ch | Stereo/Mono ratio |
|--------|---------------------|-------------|-------------------|
| Chorus | 14.93 µs | 29.2 | 0.65x (cheaper per-ch) |
| Flanger | 17.69 µs | 34.5 | 1.58x |
| Delay (ping-pong) | 8.35 µs | 16.3 | 1.52x |
| Reverb | 88.67 µs | 173.2 | 1.80x |
| Phaser | 129.54 µs | 253.0 | 1.66x |

### Oversampling (Distortion with Anti-Aliasing)

`Oversampled<N, Distortion>` wrapper cost at 256 samples:

| Factor | 256 samples | ns/sample | vs baseline |
|--------|-------------|-----------|-------------|
| 1x (baseline) | 28.08 µs | 109.7 | 1.0x |
| 2x | 38.29 µs | 149.6 | 1.36x |
| 4x | 42.79 µs | 167.2 | 1.52x |
| 8x | 172.59 µs | 674.2 | 6.14x |

4x oversampling adds only 52% overhead — good value for alias suppression.
8x has diminishing returns due to the 16-tap FIR filter running at 8x rate.

### Effect Chain

Typical guitar chain: Preamp → Distortion → Chorus → Delay

| Block Size | Time | ns/sample |
|------------|------|-----------|
| 64 | 8.13 µs | 127.1 |
| 128 | 18.28 µs | 142.8 |
| 256 | 31.09 µs | 121.5 |
| 512 | 62.02 µs | 121.1 |
| 1024 | 122.04 µs | 119.2 |

## Cortex-M7 Cycle Budget Estimation

Estimated CPU cost on STM32H750 (Daisy Seed) at 480 MHz. These are
**conservative estimates** for planning effect chain depth.

### Method

```
Est. cycles/sample = desktop_ns/sample × (desktop_GHz / cortex_GHz) × arch_penalty
                   = desktop_ns/sample × (3.0 / 0.48) × 4
                   = desktop_ns/sample × 25
```

- **Desktop clock**: i5-6300U @ 3.0 GHz turbo
- **Architecture penalty**: 4x (Cortex-M7 is single-issue, no SIMD, smaller caches
  vs x86 superscalar with out-of-order execution)
- **Budget**: At 48 kHz, one sample = 10,000 cycles. A 64-sample buffer (1.3 ms)
  = 640,000 cycles. Target: <50% CPU = 320,000 cycles/buffer.

### Effect Cost Estimates

| Effect | ns/sample (desktop) | Est. cycles/sample (CM7) | % of 48kHz budget |
|--------|--------------------|--------------------------|--------------------|
| CleanPreamp | 9.6 | 240 | 2.4% |
| LowPassFilter | 14.4 | 360 | 3.6% |
| Tremolo | 19.7 | 493 | 4.9% |
| Delay | 21.4 | 535 | 5.4% |
| TapeSaturation | 26.9 | 673 | 6.7% |
| Gate | 32.2 | 805 | 8.1% |
| Flanger | 43.6 | 1,090 | 10.9% |
| Wah | 53.2 | 1,330 | 13.3% |
| Chorus | 89.1 | 2,228 | 22.3% |
| Distortion | 109.7 | 2,743 | 27.4% |
| Reverb | 192.3 | 4,808 | 48.1% |
| Phaser (6-stage) | 305.6 | 7,640 | 76.4% |
| Compressor | 312.6 | 7,815 | 78.2% |
| ParametricEq (3-band) | 441.6 | 11,040 | 110.4% |
| MultiVibrato | 443.1 | 11,078 | 110.8% |
| Limiter | TBD | TBD | TBD |
| Bitcrusher | TBD | TBD | TBD |
| RingMod | TBD | TBD | TBD |
| Stage | TBD | TBD | TBD |

### Core Primitive Estimates

| Primitive | ns/sample (desktop) | Est. cycles/sample (CM7) | % of budget |
|-----------|--------------------|--------------------------|-----------:|
| DcBlocker | 4.5 | 113 | 1.1% |
| Biquad | 8.6 | 215 | 2.2% |
| InterpolatedDelay | 9.6 | 240 | 2.4% |
| AllpassFilter | 12.5 | 313 | 3.1% |
| CombFilter | 24.1 | 603 | 6.0% |
| SVF | 23.9 | 598 | 6.0% |

### Chain Feasibility on Daisy Seed

**Comfortable chains (< 50% CPU at 48kHz):**
- Preamp → Distortion → Chorus → Delay: ~30% CPU
- Preamp → TapeSaturation → Flanger → Delay: ~22% CPU
- Gate → Distortion → Tremolo → Delay: ~20% CPU
- Preamp → Wah → Distortion → Chorus: ~32% CPU

**Tight but feasible (50-80% CPU):**
- Preamp → Distortion → Chorus → Delay → Reverb: ~78% CPU
- Compressor → Distortion → Reverb: ~77% CPU

**Does NOT fit at 48kHz mono:**
- Any chain with ParametricEq or MultiVibrato (>100% alone)
- Any chain with Phaser + Reverb (>124%)
- Compressor + Phaser (>154%)

**Optimization paths for tight chains:**
1. Reduce Phaser stages (6 → 4 saves ~33%)
2. Use OnePole instead of SVF for simple filtering
3. Short-circuit SmoothedParam when settled (not currently implemented)
4. Lower sample rate to 44.1 kHz (gains ~8% headroom)
5. Use 4x oversampling only on Distortion (+52%), not 8x (+514%)

### Important Caveats

These estimates have significant uncertainty (±50%):
- The 4x architecture penalty is a rough heuristic. Actual penalty depends on
  memory access patterns, branch prediction, and FPU utilization.
- Cortex-M7 has an FPU (single-precision) which helps f32 DSP, but no SIMD.
- Cache effects differ: i5 has 3MB L2; CM7 has 16KB I-cache + 16KB D-cache.
- The Daisy Seed runs at 480 MHz but memory access to AXI SRAM has 1-2 wait
  states; DTCM is zero-wait-state.
- **Real measurement on hardware is essential.** These estimates guide chain
  planning, not replace profiling.

## Writing New Benchmarks

### Adding an Effect Benchmark

1. Open `crates/sonido-effects/benches/effects_bench.rs`
2. Add a benchmark function:

```rust
fn bench_new_effect(c: &mut Criterion) {
    let mut effect = NewEffect::new(SAMPLE_RATE);
    // Set typical parameters
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

```bash
# Build with debug symbols
cargo build --release

# Profile a specific benchmark
perf record cargo bench -- Reverb --profile-time 10
perf report
```

### Flamegraph

```bash
# Install flamegraph
cargo install flamegraph

# Generate flamegraph for benchmark
cargo flamegraph --bench effects_bench -- --bench Reverb
```

### Instruments (macOS)

```bash
# Build with debug symbols
cargo build --release

# Run with Instruments
instruments -t "Time Profiler" target/release/sonido-cli process input.wav output.wav -e reverb
```

## Optimization Guidelines

### DSP Performance Tips

1. **Avoid allocations in process loops**: Pre-allocate buffers
2. **Use SmoothedParam**: Prevents zipper noise without per-sample branching
3. **Batch operations**: `process_block()` over `process()` when possible
4. **Minimize function calls**: Inline hot paths
5. **Use SIMD when available**: Compiler auto-vectorization helps
6. **Place hot data in DTCM on embedded**: Zero wait-state access at 480 MHz

### Measuring Optimization Impact

```bash
# Before optimization
cargo bench -- Reverb --save-baseline before

# After optimization
cargo bench -- Reverb --baseline before
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
cargo bench -- --save-baseline main
git checkout -
git stash pop
cargo bench -- --baseline main
```

## Benchmark Reports

### Interpreting Results

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
- [Hardware](HARDWARE.md) - Embedded target details
