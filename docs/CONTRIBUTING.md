# Contributing

Guidelines for contributing to the Sonido project.

## Development Setup

```bash
git clone https://github.com/ampactor-labs/sonido
cd sonido
cargo build
cargo test
```

On Linux the GUI, CLI and plugin crates link system libraries (Fedora package
names; Debian/Ubuntu equivalents in parentheses):

| Package | Needed by |
|---------|-----------|
| `alsa-lib-devel` (`libasound2-dev`) | sonido-gui, sonido-cli, sonido-io (cpal's ALSA backend; on PipeWire systems `pipewire-alsa` serves it) |
| `mesa-libGL-devel`, `libX11-devel` (`libgl-dev`, `libx11-dev`) | sonido-plugin (baseview's OpenGL + X11 editor window) |
| `gcc-c++`, `cmake` | Only `scripts/bundle-vst3.sh` / `make plugins-vst3` (builds the clap-wrapper VST3 shim) |

The DSP crates (`sonido-core`, `sonido-effects`, `sonido-synth`, `sonido-registry`,
`sonido-analysis`, `sonido-config`, `sonido-platform`, `sonido-patch`) need none of these.

## Continuous Integration

CI runs on GitHub Actions in the public `ampactor-labs/sonido` repo. Workflows live in
`.github/workflows/`.

### Jobs

`ci.yml` — runs on push and pull request to `main` (and manual dispatch), with
`RUSTFLAGS=-D warnings`:

| Job | Runner | Steps |
|-----|--------|-------|
| `lint` | ubuntu-latest | `cargo fmt --all --check`, `cargo clippy --workspace --all-targets` |
| `test` | ubuntu-latest, macos-latest, windows-latest | `cargo test --workspace` |
| `wasm` | ubuntu-latest | `cargo build --target wasm32-unknown-unknown -p sonido-gui` |

`ci-manual.yml` — manual dispatch only (`gh workflow run ci-manual.yml -f job=<check|bench|coverage|plugin|all>`):

| Job | Timeout | Steps |
|-----|---------|-------|
| `check` | 30 min | fmt, clippy (`--lib --bins --tests --benches -D warnings`), workspace tests, `cargo doc` with `-D warnings`, no_std tests (core, effects, synth, registry, platform, patch), wasm check, cargo-deny |
| `bench` | 45 min | criterion benches for core, effects, synth, analysis; critcmp against the cached previous run |
| `coverage` | none set | cargo-llvm-cov → `lcov.info` artifact |
| `plugin` | 20 min | `cargo test -p sonido-plugin`, `scripts/bundle-clap.sh`, clap-validator 0.3.2 on `dist-clap/sonido.clap` |

`pages.yml` deploys the wasm web demo (push to `main` touching `crates/**`, or manual
dispatch); `release.yml` (on `v*` tags, or manual dispatch as a dry run) builds the CLI,
GUI, CLAP bundle, VST3 shim and factory presets per platform and publishes a GitHub release.

### Infrastructure

- **Caching**: `ci.yml` uses `Swatinem/rust-cache`; `ci-manual.yml` uses the composite action below.
- **Composite action**: `.github/actions/setup-rust/action.yml` — toolchain, sccache (`mozilla-actions/sccache-action`), Linux system deps, and cargo registry/git cache via `actions/cache`.
- **System deps** (Ubuntu): `libasound2-dev` plus the GL/X11 dev stack (`libgl-dev`, `libx11-dev`, `libxkbcommon-dev`, `libwayland-dev`, …); the composite action also installs `mold`. The exact lists are in `ci.yml` (`SYSTEM_DEPS`) and the composite action.
- **Plugin validation**: clap-validator 0.3.2 validates the single `sonido.clap` bundle (manual dispatch).
- **Coverage**: cargo-llvm-cov, artifact upload (no threshold gate, manual dispatch).

### Running CI Checks Locally

```bash
make ci           # fmt, clippy (--lib --bins --tests --benches), workspace tests, docs with -D warnings
make test-nostd   # no_std tests for core, effects, synth, registry, platform
make verify       # fmt, clippy --all-targets, workspace + no_std tests, golden and
                  # extreme-param tests, wasm check, docs with -D warnings
```

`make install-hooks` installs `scripts/pre-push` (fmt check, `cargo check --workspace
--all-targets`, wasm check). Alternatively `git config core.hooksPath .githooks` uses
`.githooks/pre-push`, which runs only the fmt check.

## Git Workflow

Push directly to main for typical DSP work. Use PRs for CI/infra changes or when CI-specific validation is needed (CLAP validator, wasm target, coverage).

## Documentation Protocol

Documentation must stay in sync with code at all times. Every code change that affects
behavior, API surface, or DSP algorithms must include corresponding documentation updates
in the same commit or PR. This section defines exactly what to update and when.

### Documentation-to-Code Mapping

The following table maps each documentation file to the source code it describes.
When you change code in the "Source Files" column, you **must** update the corresponding doc.
The per-source-file version of this table is [DOC_CODE_MAPPING.md](DOC_CODE_MAPPING.md).

| Documentation File | Source Files | What to Update |
|---|---|---|
| `README.md` | Any user-facing feature, effect count, benchmark numbers | Update the affected section |
| `docs/EFFECTS_REFERENCE.md` | `crates/sonido-effects/src/kernels/*.rs` | Add/update effect entry with parameters, ranges, DSP theory |
| `docs/SYNTHESIS.md` | `crates/sonido-synth/src/*.rs` | Update oscillator, envelope, voice, mod matrix docs |
| `docs/ARCHITECTURE.md` | Crate structure, dependency graph | Update crate diagram and dependency descriptions |
| `docs/CLI_GUIDE.md` | `crates/sonido-cli/src/**/*.rs` | Update command examples, add new subcommands |
| `docs/EMBEDDED.md` | `crates/sonido-platform/src/*.rs`, `crates/sonido-daisy/` | Update platform traits, control mapping, firmware docs |
| `docs/reference/biosignal.md` | `crates/sonido-analysis/src/cfc.rs`, `filterbank.rs`, `hilbert.rs` | Update analysis pipeline docs |
| `docs/BENCHMARKS.md` | `crates/*/benches/*.rs` | Update benchmark results after optimization |
| `docs/CHANGELOG.md` | Any user-visible change | Add entry under current version |
| `docs/TESTING.md` | Test infrastructure changes | Update test patterns, CI steps |
| `docs/GUI.md` | `crates/sonido-gui/src/**/*.rs`, `crates/sonido-gui-core/src/**/*.rs` | Update GUI feature docs |
| `docs/DSP_FUNDAMENTALS.md` | Core DSP modules (`biquad.rs`, `svf.rs`, `oversample.rs`, `comb.rs`, `allpass.rs`) | Update theory sections when algorithms change |
| `docs/DESIGN_DECISIONS.md` | Any architectural decision | Add new ADR entry |
| `docs/CONTRIBUTING.md` | CI config, dev workflow changes | Update setup/CI/checklist sections |

### When to Update Docs

| Change Type | Required Documentation Updates |
|---|---|
| New effect | `EFFECTS_REFERENCE.md`, `README.md` features list and effect count, registry entry |
| New CLI command | `CLI_GUIDE.md`, clap help text |
| New public trait/struct | Rustdoc with `///` comments; `ARCHITECTURE.md` if it's a core abstraction |
| New DSP algorithm | `DSP_FUNDAMENTALS.md` theory section, inline `///` explaining the math |
| Parameter range change | `EFFECTS_REFERENCE.md` parameter table, rustdoc table on the params struct |
| Breaking API change | `CHANGELOG.md` with migration notes, code examples in affected docs |
| New crate | `ARCHITECTURE.md` diagram, workspace `Cargo.toml`, `README.md` |
| Bug fix | `CHANGELOG.md` entry |
| Performance improvement | `BENCHMARKS.md` with before/after numbers |
| New analysis feature | `docs/reference/biosignal.md` or `docs/reference/cfc-analysis.md` |
| Architectural decision | `DESIGN_DECISIONS.md` ADR entry |

### Doc Checklist (before PR merge)

#### Inline Documentation (every PR)

- [ ] All new public items have `///` rustdoc comments
- [ ] Rustdoc includes usage example if the API is non-obvious
- [ ] DSP algorithm comments explain the math (what the formula does, not just variable names)
- [ ] Safety invariants documented on any `unsafe` code
- [ ] Parameter ranges documented in the params struct's index/unit/range/default table

#### Markdown Documentation (when applicable)

- [ ] `EFFECTS_REFERENCE.md` updated if any effect was added or modified
- [ ] `CLI_GUIDE.md` updated if CLI commands changed
- [ ] `README.md` features list updated if user-facing feature was added
- [ ] `CHANGELOG.md` entry added for any user-visible change
- [ ] `DSP_FUNDAMENTALS.md` updated if a new DSP algorithm was introduced
- [ ] `DESIGN_DECISIONS.md` updated if an architectural choice was made

#### Verification

- [ ] `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` builds
- [ ] `cargo test --doc` passes (all rustdoc examples compile and run)
- [ ] No stale references to renamed/removed items in any `.md` file
- [ ] Code examples in docs use current API (not deprecated patterns)

### Inline Doc Comment Standards

#### Module-level docs

Every module file should start with a `//!` doc comment explaining purpose and key concepts:

```rust
//! # Biquad Filter
//!
//! Second-order IIR filter using the RBJ Audio EQ Cookbook coefficients.
//! Supports lowpass, highpass, bandpass, notch, and peaking responses.
//!
//! ## Theory
//!
//! The biquad implements the transfer function:
//!   H(z) = (b0 + b1*z^-1 + b2*z^-2) / (1 + a1*z^-1 + a2*z^-2)
//!
//! Coefficients are computed from analog prototypes via the bilinear transform.
//! See: Robert Bristow-Johnson, "Audio EQ Cookbook"
```

#### Struct-level docs

Kernel parameter structs document every index with its unit, range and default
(abridged from `crates/sonido-effects/src/kernels/distortion.rs`):

```rust
/// Parameter values for [`DistortionKernel`].
///
/// All values in **user-facing units** — the same units shown in GUIs and
/// stored in presets. The kernel converts internally as needed.
///
/// | Index | Field | Unit | Range | Default |
/// |-------|-------|------|-------|---------|
/// | 0 | `drive_db` | dB | 0–40 | 8.0 |
/// | 1 | `tone_db` | dB | −12–12 | 0.0 |
/// | ... | | | | |
pub struct DistortionParams { /* ... */ }
```

#### Function-level docs for DSP

Abridged from `crates/sonido-synth/src/oscillator.rs`:

```rust
/// 4th-order PolyBLEP correction (2-sample window on each side of the
/// discontinuity, ~50 dB alias suppression).
///
/// Reference: Välimäki et al., "Antialiasing Oscillators", IEEE Signal
/// Processing Magazine, 2010.
///
/// # Arguments
/// * `t` - Current phase position in [0.0, 1.0)
/// * `dt` - Phase increment per sample (frequency / sample_rate)
///
/// # Returns
/// Correction value to subtract from (or add to) the naive waveform.
/// Returns 0.0 when phase is far from a discontinuity.
fn poly_blep(t: f32, dt: f32) -> f32 { /* ... */ }
```

### Running Doc Checks

```bash
# Build all rustdocs, failing on warnings (as CI does)
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

# Run doc tests
cargo test --doc

# Check for broken internal links (manual)
grep -rn '\[.*\](.*\.md)' docs/ | while read line; do
  file=$(echo "$line" | sed 's/.*(\(.*\.md\)).*/\1/')
  if [ ! -f "docs/$file" ] && [ ! -f "$file" ]; then
    echo "BROKEN: $line"
  fi
done
```

### Adding a New DSP Algorithm

When introducing a new DSP algorithm (filter topology, waveshaper, modulation technique, etc.):

1. **Inline docs**: Add `///` comments explaining the mathematical basis, not just what variables mean
2. **Reference the source**: Cite the paper, textbook, or reference implementation (e.g., "Based on RBJ Audio EQ Cookbook")
3. **Document trade-offs**: Why this algorithm over alternatives? (e.g., "SVF chosen over biquad for modulation stability")
4. **Update DSP_FUNDAMENTALS.md**: Add a theory section if the algorithm introduces a new concept
5. **Update DESIGN_DECISIONS.md**: Add an ADR if the choice has architectural implications

## Commit Guidelines

### Message Format

```
<type>: <short description>

<optional body with details>
```

### Types

| Type | Description |
|------|-------------|
| `feat` | New feature |
| `fix` | Bug fix |
| `docs` | Documentation only |
| `refactor` | Code change that neither fixes nor adds |
| `test` | Adding/updating tests |
| `bench` | Benchmark changes |
| `chore` | Maintenance (deps, CI, configs) |

### Examples

```
feat: Add reverb effect with room size parameter

fix: Correct delay feedback clipping at high values

docs: Add CLI usage examples to README

refactor: Extract common filter code to biquad module

chore: Update cpal to 0.15.3
```

### Rules

- Present tense ("Add feature" not "Added feature")
- No period at end of subject line
- Subject line ≤ 72 characters
- Reference issues: `fix: Resolve delay click (#42)`

## Code Style

### General

- Follow `rustfmt` defaults
- Use `cargo clippy` and address warnings
- Prefer explicit types in public APIs
- Document all public items

### DSP Code

- Use `f32` for audio samples (not `f64`)
- Kernels hold no smoothers: declare a `SmoothingStyle` per parameter in `kernel_params!` and let `Adapter` do the smoothing
- Avoid allocations in audio processing hot paths
- Test for both `std` and `no_std` compatibility

### Testing

```bash
# Run all tests
cargo test

# Test no_std compatibility
make test-nostd

# Run benchmarks
cargo bench
```

### Benchmarks

Effect benchmarks run every block size in `BLOCK_SIZES` (`[64, 128, 256, 512, 1024]`)
through the `bench_effect` helper in `crates/sonido-effects/benches/effects_bench.rs`;
see [BENCHMARKS.md](BENCHMARKS.md#writing-new-benchmarks).

## Pull Request Process

1. Fork and create a feature branch
2. Make your changes
3. Ensure tests pass: `cargo test`
4. Ensure clippy passes: `cargo clippy --workspace --all-targets -- -D warnings`
5. Run `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` and fix any doc warnings
6. Complete the documentation checklist above (inline docs + markdown updates)
7. Run `cargo test --doc` to verify doc examples compile
8. Submit PR with clear description

### PR Title Format

Use the same format as commits:

```
feat: Add reverb effect
fix: Correct compressor attack time calculation
```

## Adding a New Effect

### Code Steps

1. Create the kernel file in `crates/sonido-effects/src/kernels/`
2. Declare the params struct with `kernel_params!` (indexed access, descriptors with stable `ParamId`s, smoothing hints)
3. Implement `DspKernel` for the kernel struct (pure DSP processing, receives `&Params`)
4. Add `from_knobs()` on the params struct for embedded ADC mapping
5. Implement `process_stereo()` if the effect has decorrelated L/R processing
6. Add tests in a `#[cfg(test)] mod tests` block
7. Export from `crates/sonido-effects/src/kernels/mod.rs` and `crates/sonido-effects/src/lib.rs`
8. Register in `crates/sonido-registry/src/lib.rs`: an `EffectDescriptor` plus a factory `|sr| Box::new(Adapter::new(XxxKernel::new(sr), sr))`, and append the next free UID to `EFFECT_UIDS` (append-only)
9. Add a `spec(...)` entry to `CATALOG` in `crates/sonido-plugin/src/bundle.rs` so the plugin bundle exports it as `com.sonido.<id-with-dashes>`

The CLI, GUI and graph DSL discover effects through the registry, so they need no
per-effect code (add aliases in `crates/sonido-graph-dsl/src/effects.rs` if useful).

### Documentation Steps (all required)

10. Add `///` rustdoc on the params and kernel structs explaining the DSP algorithm and parameters
11. Add entry to `docs/EFFECTS_REFERENCE.md` with: description, parameter table, DSP theory, usage example
12. Update `README.md` features list and effect count
13. Add `docs/CHANGELOG.md` entry
14. If the effect introduces a new DSP concept, add a section to `docs/DSP_FUNDAMENTALS.md`

### Kernel Template

A minimal gain kernel. See [EFFECT_SDK.md](EFFECT_SDK.md) and
[KERNEL_ARCHITECTURE.md](KERNEL_ARCHITECTURE.md) for the full contract.

```rust
use sonido_core::kernel::{DspKernel, SmoothingStyle};
use sonido_core::{ParamDescriptor, ParamId, fast_db_to_linear, kernel_params};

#[derive(Debug, Clone, Copy, Default)]
pub struct MyGainParams {
    /// Output gain in dB (−60–12, default 0).
    pub gain_db: f32,
}

kernel_params! {
    MyGainParams, this {
        [0] ParamDescriptor::gain_db("Gain", "Gain", -60.0, 12.0, 0.0)
                .with_id(ParamId(9900), "my_gain"),
            smoothing: SmoothingStyle::Standard,
            get: this.gain_db,
            set: |v| this.gain_db = v;
    }
}

pub struct MyGainKernel;

impl MyGainKernel {
    pub fn new(_sample_rate: f32) -> Self {
        Self
    }
}

impl DspKernel for MyGainKernel {
    type Params = MyGainParams;

    fn process(&mut self, input: f32, params: &MyGainParams) -> f32 {
        input * fast_db_to_linear(params.gain_db)
    }

    fn reset(&mut self) {}

    fn set_sample_rate(&mut self, _sample_rate: f32) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonido_core::Effect;
    use sonido_core::kernel::Adapter;

    #[test]
    fn adapter_output_is_finite() {
        let mut effect = Adapter::new(MyGainKernel::new(48000.0), 48000.0);
        let output = effect.process(0.5);
        assert!(output.is_finite());
    }
}
```

## Issue Reporting

When reporting bugs, include:

- Sonido version (`cargo pkgid sonido-core`)
- OS and version
- Steps to reproduce
- Expected vs actual behavior
- Audio file samples if relevant

## License

Sonido is licensed under AGPL-3.0-or-later, with a commercial license available for proprietary use. See [LICENSING.md](../LICENSING.md) for details.

By contributing, you agree that your contributions are licensed under AGPL-3.0-or-later and that Ampactor Labs retains the right to offer contributions under the commercial license (standard CLA terms).
