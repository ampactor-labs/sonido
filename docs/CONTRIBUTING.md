# Contributing

Guidelines for contributing to the Sonido project.

## Development Setup

```bash
git clone https://github.com/ampactor-labs/sonido
cd sonido
cargo build
cargo test
```

## Continuous Integration

CI runs on GitHub Actions with ubuntu-latest runners under the ampactor-labs org (private repo).

### Jobs

| Job | Timeout | Trigger | Notes |
|-----|---------|---------|-------|
| Lint | 15 min | push + PR | fmt + clippy |
| Test | 20 min | push + PR | Full workspace including plugin unit tests |
| no_std | 15 min | push + PR | 5 no_std crates |
| Wasm | 15 min | push + PR | sonido-gui wasm32 check |
| Benchmarks | 45 min | manual dispatch | criterion + critcmp |
| Coverage | (no limit) | manual dispatch | cargo-llvm-cov |
| Plugin | 20 min | manual dispatch | Build + clap-validator for all 19 plugins |

### Infrastructure

- **Composite action**: `.github/actions/setup-rust/action.yml` — DRY toolchain, sccache, system deps, and cargo registry cache
- **Caching**: sccache via `mozilla-actions/sccache-action` + cargo registry/git cache via `actions/cache`
- **System deps**: libasound2-dev, libudev-dev, mold linker, full x11-rs/GL stack (installed by composite action)
- **Plugin validation**: clap-validator 0.3.2 validates all 19 CLAP plugin binaries (manual dispatch)
- **Coverage**: cargo-llvm-cov, artifact upload (no threshold gate, manual dispatch)

### Running CI Checks Locally

```bash
cargo test --workspace
cargo test --no-default-features -p sonido-core -p sonido-effects -p sonido-synth -p sonido-registry -p sonido-platform
cargo clippy --workspace --lib --bins --tests --benches -- -D warnings
cargo fmt --all -- --check
cargo doc --no-deps --all-features  # check for doc warnings
```

## Git Workflow

Push directly to main for typical DSP work. Use PRs for CI/infra changes or when CI-specific validation is needed (CLAP validator, wasm target, coverage). Solo dev, private repo — PRs add ceremony without review value for typical work.

## Documentation Protocol

Documentation must stay in sync with code at all times. Every code change that affects
behavior, API surface, or DSP algorithms must include corresponding documentation updates
in the same commit or PR. This section defines exactly what to update and when.

### Documentation-to-Code Mapping

The following table maps every documentation file to the source code it describes.
When you change code in the "Source Files" column, you **must** update the corresponding doc.

| Documentation File | Source Files | What to Update |
|---|---|---|
| `CLAUDE.md` (Key Files table) | Any new module or crate | Add row to Key Files table |
| `CLAUDE.md` (Crates table) | Workspace `Cargo.toml`, new crate | Add/update crate row |
| `CLAUDE.md` (Key Patterns) | `effect.rs`, `param.rs`, `modulation.rs`, `tempo.rs` | Update code examples if API changes |
| `README.md` (features list) | Any user-facing feature | Update bullet points |
| `README.md` (Why Sonido table) | Test count, feature additions | Update comparison table |
| `docs/EFFECTS_REFERENCE.md` | `crates/sonido-effects/src/*.rs` | Add/update effect entry with parameters, ranges, DSP theory |
| `docs/SYNTHESIS.md` | `crates/sonido-synth/src/*.rs` | Update oscillator, envelope, voice, mod matrix docs |
| `docs/ARCHITECTURE.md` | Crate structure, dependency graph | Update crate diagram and dependency descriptions |
| `docs/CLI_GUIDE.md` | `crates/sonido-cli/src/**/*.rs` | Update command examples, add new subcommands |
| `docs/HARDWARE.md` | `crates/sonido-platform/src/*.rs` | Update platform traits, control mapping |
| `docs/BIOSIGNAL_ANALYSIS.md` | `crates/sonido-analysis/src/cfc.rs`, `filterbank.rs`, `hilbert.rs` | Update analysis pipeline docs |
| `docs/BENCHMARKS.md` | `benches/*.rs` | Update benchmark results after optimization |
| `docs/CHANGELOG.md` | Any user-visible change | Add entry under current version |
| `docs/TESTING.md` | Test infrastructure changes | Update test patterns, CI steps |
| `docs/GUI.md` | `crates/sonido-gui/src/*.rs` | Update GUI feature docs |
| `docs/DSP_FUNDAMENTALS.md` | Core DSP modules (`biquad.rs`, `svf.rs`, `oversample.rs`, `comb.rs`, `allpass.rs`) | Update theory sections when algorithms change |
| `docs/DESIGN_DECISIONS.md` | Any architectural decision | Add new ADR entry |
| `docs/CONTRIBUTING.md` | CI config, dev workflow changes | Update setup/CI/checklist sections |

### When to Update Docs

| Change Type | Required Documentation Updates |
|---|---|
| New effect | `EFFECTS_REFERENCE.md`, `README.md` features list, `CLAUDE.md` Key Files, registry entry |
| New CLI command | `CLI_GUIDE.md`, clap help text |
| New public trait/struct | Rustdoc with `///` comments, `CLAUDE.md` Key Patterns if it's a core abstraction |
| New DSP algorithm | `DSP_FUNDAMENTALS.md` theory section, inline `///` explaining the math |
| Parameter range change | `EFFECTS_REFERENCE.md` parameter table, rustdoc on setter |
| Breaking API change | `CHANGELOG.md` with migration notes, `CLAUDE.md` pattern update |
| New crate | `CLAUDE.md` Crates table, `ARCHITECTURE.md` diagram, workspace `Cargo.toml` |
| Bug fix | `CHANGELOG.md` entry |
| Performance improvement | `BENCHMARKS.md` with before/after numbers |
| New analysis feature | `BIOSIGNAL_ANALYSIS.md` or `CFC_ANALYSIS.md` |
| Architectural decision | `DESIGN_DECISIONS.md` ADR entry |

### Doc Checklist (before PR merge)

#### Inline Documentation (every PR)

- [ ] All new public items have `///` rustdoc comments
- [ ] Rustdoc includes usage example if the API is non-obvious
- [ ] DSP algorithm comments explain the math (what the formula does, not just variable names)
- [ ] Safety invariants documented on any `unsafe` code
- [ ] Parameter ranges documented on setter methods (e.g., `/// Sets drive in dB. Range: 0.0 to 40.0`)

#### Markdown Documentation (when applicable)

- [ ] `EFFECTS_REFERENCE.md` updated if any effect was added or modified
- [ ] `CLI_GUIDE.md` updated if CLI commands changed
- [ ] `README.md` features list updated if user-facing feature was added
- [ ] `CHANGELOG.md` entry added for any user-visible change
- [ ] `CLAUDE.md` Key Files table updated if new modules were added
- [ ] `DSP_FUNDAMENTALS.md` updated if a new DSP algorithm was introduced
- [ ] `DESIGN_DECISIONS.md` updated if an architectural choice was made

#### Verification

- [ ] `cargo doc --no-deps --all-features` builds without warnings
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
//! Supports lowpass, highpass, bandpass, notch, allpass, peaking, low shelf,
//! and high shelf responses.
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

```rust
/// Freeverb-style reverb with 8 parallel comb filters feeding 4 series allpass filters.
///
/// Based on Jezar's Freeverb algorithm. Each comb filter uses a different delay length
/// (prime-adjacent values) to avoid metallic resonances. The allpass diffusers spread
/// energy across the time domain for a dense, natural tail.
///
/// ## Parameters
/// - `room_size`: Controls comb filter feedback (0.0 to 1.0, default 0.5)
/// - `damping`: Low-pass filtering in feedback path (0.0 to 1.0, default 0.5)
/// - `wet`: Wet signal level (0.0 to 1.0, default 0.33)
/// - `dry`: Dry signal level (0.0 to 1.0, default 1.0)
/// - `width`: Stereo spread (0.0 to 1.0, default 1.0)
pub struct Reverb { /* ... */ }
```

#### Function-level docs for DSP

```rust
/// Compute the PolyBLEP (Polynomal Band-Limited Step) correction.
///
/// Reduces aliasing at waveform discontinuities by subtracting a polynomial
/// approximation of the band-limited step function. The correction is applied
/// within one sample of the discontinuity on each side.
///
/// # Arguments
/// * `t` - Phase position normalized to [0, 1) within the waveform period
/// * `dt` - Phase increment per sample (frequency / sample_rate)
///
/// # Returns
/// Correction value to subtract from the naive waveform sample.
fn poly_blep(t: f32, dt: f32) -> f32 { /* ... */ }
```

### Running Doc Checks

```bash
# Build all rustdocs and check for warnings
cargo doc --no-deps --all-features 2>&1 | grep -i warning

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
- Use `SmoothedParam` for all user-controllable parameters
- Avoid allocations in audio processing hot paths
- Test for both `std` and `no_std` compatibility

### Testing

```bash
# Run all tests
cargo test

# Test no_std compatibility
cargo test --no-default-features -p sonido-core
cargo test --no-default-features -p sonido-effects

# Run benchmarks
cargo bench
```

### Benchmarks

Effects should be benchmarked with multiple block sizes:

```rust
fn bench_effect(c: &mut Criterion) {
    for block_size in [64, 128, 256, 512, 1024] {
        // benchmark code
    }
}
```

## Pull Request Process

1. Fork and create a feature branch
2. Make your changes
3. Ensure tests pass: `cargo test`
4. Ensure clippy passes: `cargo clippy --all-targets`
5. Run `cargo doc --no-deps --all-features` and fix any doc warnings
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

1. Create the effect in `crates/sonido-effects/src/`
2. Implement the `Effect` trait from `sonido-core`
3. Implement the `ParameterInfo` trait for runtime introspection
4. Use `SmoothedParam` for all user-controllable parameters
5. Implement `process_stereo()` if the effect has decorrelated L/R processing
6. Add tests in a `#[cfg(test)] mod tests` block
7. Export from `crates/sonido-effects/src/lib.rs`
8. Register in `crates/sonido-registry/src/lib.rs`
9. Add to CLI in `crates/sonido-cli/src/effects.rs`

### Documentation Steps (all required)

10. Add `///` rustdoc on the struct explaining the DSP algorithm and parameters
11. Add `///` rustdoc on each public method with parameter ranges
12. Add entry to `docs/EFFECTS_REFERENCE.md` with: description, parameter table, DSP theory, usage example
13. Add row to `CLAUDE.md` Key Files table
14. Update `README.md` features list and effect count
15. Add `docs/CHANGELOG.md` entry
16. If the effect introduces a new DSP concept, add a section to `docs/DSP_FUNDAMENTALS.md`

### Effect Template

```rust
use sonido_core::{Effect, SmoothedParam};

pub struct MyEffect {
    sample_rate: f32,
    my_param: SmoothedParam,
    // internal state...
}

impl MyEffect {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            my_param: SmoothedParam::with_config(1.0, sample_rate, 10.0),
        }
    }

    pub fn set_my_param(&mut self, value: f32) {
        self.my_param.set_target(value);
    }
}

impl Effect for MyEffect {
    fn process(&mut self, input: f32) -> f32 {
        let param = self.my_param.advance();  // Use advance() instead of next()
        // process audio...
        input * param
    }

    fn process_block(&mut self, input: &[f32], output: &mut [f32]) {
        for (inp, out) in input.iter().zip(output.iter_mut()) {
            *out = self.process(*inp);
        }
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.my_param.set_sample_rate(sample_rate);
    }

    fn reset(&mut self) {
        self.my_param.snap_to_target();
    }

    fn latency_samples(&self) -> usize {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_processing() {
        let mut effect = MyEffect::new(48000.0);
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
