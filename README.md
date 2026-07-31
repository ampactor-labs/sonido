# sonido

An audio DSP kernel written once and run in three places: CLAP plugins in a DAW, a node-graph editor in the browser, and a guitar pedal built on the Electrosmith Daisy Seed (STM32H750, 480 MHz Cortex-M7). The distortion flashed to the pedal is the distortion shipped in the plugin.

[![CI](https://github.com/ampactor-labs/sonido/actions/workflows/ci.yml/badge.svg)](https://github.com/ampactor-labs/sonido/actions/workflows/ci.yml)
[![License: AGPL-3.0 + Commercial](https://img.shields.io/badge/License-AGPL--3.0%20%2B%20Commercial-blue.svg)](LICENSE)

**Status: working, API unstable.** Not published to crates.io, so the git dependency below is the only install. The desktop, plugin, and wasm paths are built and tested on every push; the ARM path is not, and is verified by flashing it by hand.

Try the node-graph editor, compiled to WebAssembly and rebuilt by CI on every push: **[ampactor.dev/sonido](https://ampactor.dev/sonido/)**

![Sonido node-graph editor](docs/img/editor.png)

```toml
[dependencies]
sonido-core = { git = "https://github.com/ampactor-labs/sonido" }
sonido-effects = { git = "https://github.com/ampactor-labs/sonido" }
```

## Measured

Per-effect cost on an Intel i5-6300U at 3.0 GHz turbo, 256-sample mono blocks at 48 kHz, through `Adapter<K, SmoothedPolicy>`, measured with criterion. Reproduce with `cargo bench -p sonido-effects`. The five most expensive measured effects, then the cheapest for scale:

| Effect | 256 samples | ns/sample | One core @ 48 kHz |
|---|---|---|---|
| Vibrato | 113.43 µs | 443.1 | 2.1% |
| Eq (3-band) | 113.06 µs | 441.6 | 2.1% |
| Compressor | 80.02 µs | 312.6 | 1.5% |
| Phaser (6-stage) | 78.25 µs | 305.6 | 1.5% |
| Reverb | 49.22 µs | 192.3 | 0.92% |
| CleanPreamp | 2.47 µs | 9.6 | 0.05% |

Four effects are unmeasured and marked TBD in [docs/BENCHMARKS.md](docs/BENCHMARKS.md): Limiter, Bitcrusher, RingMod, Stage. So the claim is not "every effect fits" but the narrower one: the fifteen measured effects clear real time on a 2015 mobile chip with room left over. The full nineteen-row table, the stereo and oversampling numbers, and the method are in that file.

Sound quality is measured by in-repo tooling, at 48 kHz through a 16384-point Blackman-Harris FFT. Reproduce with `cargo run --release --example dsp_report -p sonido-effects`; full tables in [docs/DSP_MEASUREMENTS.md](docs/DSP_MEASUREMENTS.md).

- Distortion THD tracks drive: 13.19% at 10 dB, 41.43% at 30 dB, 43.20% at 40 dB.
- The soft-clip curve is odd-symmetric where the theory says it should be: at 30 dB drive the third harmonic sits at −7.7 dB while the even harmonics stay buried.
- Reverb decay is a measured Schroeder RT60 of 3.50 s, fit correlation 0.998, not a knob label.

## Usage

The embedded path. The kernel is state plus math: no allocator, no smoothing, no trait objects, and parameters arrive by reference every block.

```rust
use sonido_core::kernel::DspKernel;
use sonido_effects::kernels::{DistortionKernel, DistortionParams};

let mut kernel = DistortionKernel::new(48_000.0);
let params = DistortionParams::from_knobs(
    adc_drive, adc_tone, adc_output, adc_shape, adc_mix, adc_dynamics,
);
let (out_l, out_r) = kernel.process_stereo(in_l, in_r, &params);
```

The desktop path. The registry wraps that same kernel in an adapter owning parameter smoothing and the dynamic effect interface.

```rust
use sonido_core::EffectWithParams;
use sonido_registry::EffectRegistry;

let registry = EffectRegistry::new();
let mut effect = registry.create("distortion", 48_000.0).unwrap();
effect.effect_set_param(0, 15.0);  // drive, dB
let output = effect.process(input_sample);
```

[NEEDS RECEIPT: neither block is compiled by CI. Add `#[cfg(doctest)] #[doc = include_str!("../../README.md")]` to `crates/sonido-core/src/lib.rs` so `cargo test` fails when these drift.]

## The kernel stack

Every effect is three layers, and which layers you get depends on whether you have an allocator.

```
XxxParams            typed params; from_knobs(), lerp(), from_normalized()
    │
XxxKernel            pure DSP; process_stereo(l, r, &Params) -> (l, r)
    │                no allocation, no parameter ownership, no smoothing
Adapter<K, Policy>   desktop only; per-parameter smoothing, trait objects
```

The kernel is the part that runs on the pedal. The adapter is the part that does not: desktop has headroom for smoothing and boxing, a Cortex-M7 servicing a DMA callback does not, so on hardware the callback calls the kernel directly and the ADC's own filtering stands in for smoothing. `XxxParams` doubles as the preset format, which is why morphing between presets is `lerp()` and nothing more.

Nonlinear kernels use first-order ADAA (Parker et al., DAFx-2016); filters follow the RBJ cookbook; the reverb is Schroeder-Moorer via Freeverb. The desktop path wraps nonlinear kernels in `Oversampled<N>` with a 48-tap Kaiser-windowed sinc, raised from 16 taps to clear 80 dB of stopband attenuation (`crates/sonido-core/src/oversample.rs`).

Rigs are DAGs rather than chains, compiled through a Kahn sort with buffer liveness analysis, so a 20-node linear chain runs on two ping-ponged buffers instead of twenty. The same topology is scriptable: `"preamp:gain=6 | distortion:drive=15 | reverb:mix=0.3"`, with `split(...)` for parallel legs.

## Weak spots

**The ARM numbers are estimates, and two effects do not fit.** [docs/BENCHMARKS.md](docs/BENCHMARKS.md) derives Cortex-M7 cost by scaling desktop measurements 25×, from a 3.0/0.48 GHz clock ratio and a 4× architecture penalty, and states its own uncertainty at ±50%. Under that model the 3-band Eq lands at 110.4% of the 48 kHz budget and Vibrato at 110.8%: over. Treat every ARM performance statement here as a hypothesis until the on-device harness is run. [NEEDS RECEIPT: flash `crates/sonido-daisy/examples/bench_kernels.rs` (DWT cycle counters) to the Hothouse and replace the estimate table.]

**Two effects blow the memory budget.** AXI SRAM is 512 KB, roughly 480 KB usable under BOOT_SRAM. The delay's two-second stereo lines cost about 772 KB and the looper's sixty-second buffer costs far more; both must live in SDRAM behind 4–8 wait states. Restricted to effects that fit AXI, the worst three-slot combination is about 144 KB. Per-effect heap is measured from kernel `new()` allocations in [docs/EMBEDDED.md](docs/EMBEDDED.md).

**The ARM target is not in CI.** `sonido-daisy` is excluded from the workspace (`Cargo.toml`) and no workflow targets `thumbv7em-none-eabihf`, so pedal firmware is verified by flashing it on my desk and listening.

**VST3 is unvalidated.** VST3 shims are built from the CLAP binaries via clap-wrapper (`scripts/bundle-vst3.sh`), but only clap-validator runs anywhere in this repo. CLAP is the format with a receipt. [NEEDS RECEIPT: run Steinberg's VST3 validator over the bundles in CI, or the shims stay unclaimed.]

**No crates.io release, no pinned MSRV, and the benchmark job has no regression gate.** It uploads a critcmp comparison for a human to read, so a regression will be visible but will not fail anything.

If you want a mature plugin suite today, buy one. This is one person's DSP framework with an unstable API.

## Verification

There are 1,899 `#[test]` functions in the tree, 1,864 outside the excluded ARM crate. The shape matters more than the count.

Automatic, on every push and pull request ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)): `cargo fmt --all --check`, `cargo clippy --workspace --all-targets`, `cargo test --workspace` on ubuntu, macOS, and Windows, and a `wasm32-unknown-unknown` build of the editor.

The tests worth naming:

- Golden-file regression pins every effect's output against a checked-in reference and fails on MSE above 1e-6, SNR below 60 dB, or spectral correlation under 0.9999, each threshold documented next to the constant that sets it (`crates/sonido-effects/tests/regression.rs:38-65`).
- An aliasing suite fails any nonlinear kernel whose inharmonic energy exceeds 5% of total, which is the number ADAA is supposed to buy (`crates/sonido-effects/tests/aliasing.rs:39`).
- Property tests push every registered effect through randomized parameter sets and require finite, bounded output and a clean reset.
- The registry asserts its own inventory, which is where the number 36 in this README comes from (`crates/sonido-registry/src/lib.rs:867`).

Manual, behind `workflow_dispatch` on `ci-manual.yml`: `--no-default-features` builds for the `no_std` crates, criterion benchmarks, llvm-cov coverage, and clap-validator against every bundled CLAP plugin.

## Also in the box

- **CLI**, 12 subcommands: offline processing through a chain or graph string, live input, signal generation, spectrum and impulse-response analysis. `cargo install --path crates/sonido-cli`
- **GUI** (`cargo run -p sonido-gui --release`): node editor with per-knob macros, whole-rig A/B morph, export to CLAP preset, JSON patch, or a pedal image flashed over DFU.
- **Plugins**: 21 CLAP examples, 20 single-effect plus `sonido-graph-player`, which runs a whole exported rig as one plugin. Tagged releases ship bundles for Linux, macOS, and Windows.
- **Pedal firmware**: `sonido_pedal` on the PedalPCB Hothouse, a 3-slot multi-effect with live routing changes, running 48 kHz in 32-sample blocks (`BLOCK_SIZE` in `crates/sonido-daisy/src/lib.rs`).

`no_std` covers seven crates: six carry `#![cfg_attr(not(feature = "std"), no_std)]` and `sonido-daisy` is unconditional. Math goes through `libm`.

## Where the rest lives

- [docs/GETTING_STARTED.md](docs/GETTING_STARTED.md): install, first chain, first plugin
- [docs/EMBEDDED.md](docs/EMBEDDED.md): Daisy hardware, memory budgets, DFU flashing
- [docs/BENCHMARKS.md](docs/BENCHMARKS.md): full timing tables and the M7 estimation method
- [docs/DSP_MEASUREMENTS.md](docs/DSP_MEASUREMENTS.md): THD, harmonic structure, RT60
- [docs/EFFECTS_REFERENCE.md](docs/EFFECTS_REFERENCE.md): all 36 effects and their parameters
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): the crate graph and what depends on what

## License

AGPL-3.0-or-later, or a commercial license for shipping closed-source. Terms in [LICENSING.md](LICENSING.md).
