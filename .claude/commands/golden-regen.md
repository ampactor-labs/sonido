# Golden File Regeneration Workflow

Regenerate golden regression test baselines after intentional DSP algorithm changes.

## When to Use

Only after **intentional** DSP algorithm changes that alter effect output. Do not regenerate to mask unintended regressions.

## Pre-flight: Identify Regressions

Run the regression tests to see which effects failed:

```bash
cargo test --test regression -p sonido-effects
```

Review the failure output. Each test reports: MSE, SNR, and spectral correlation. Understand *why* the output changed before regenerating.

## Single-Effect Regeneration

Regenerate only the specific effect that changed:

```bash
REGENERATE_GOLDEN=1 cargo test --test regression -p sonido-effects -- test_name
```

For example, if you changed the distortion algorithm:
```bash
REGENERATE_GOLDEN=1 cargo test --test regression -p sonido-effects -- distortion
```

## All-Effect Regeneration

Only when a foundational change affects all effects (e.g., mix function, gain staging):

```bash
REGENERATE_GOLDEN=1 cargo test --test regression -p sonido-effects
```

## Quality Verification

After regeneration, run the tests without `REGENERATE_GOLDEN` to confirm all metrics pass:

```bash
cargo test --test regression -p sonido-effects
```

All three metrics must pass for every effect:
- **MSE** < 1e-6 (sample-level accuracy)
- **SNR** > 60 dB (signal quality)
- **Spectral correlation** > 0.9999 (frequency content preserved)

## Listen Check

Process a test signal through the changed effect and listen to the output:

```bash
cargo run -p sonido-cli -- process test_input.wav test_output.wav --effect <effect_name>
```

Verify the output sounds correct and the change is audible as intended.

## Commit Protocol

- Include golden files (`.wav` in `crates/sonido-effects/tests/golden/`) in the **same commit** as the DSP changes
- Mention which effects changed in the commit message
- If SVF coefficients changed, expect cascading golden drift in SVF-using effects (wah, filter)

## Known Gotchas

- **Proptest tolerance**: `all_effects_reset_clears_state` has tolerance 0.02 (tape hysteresis is path-dependent)
- **SVF cascade**: SVF coefficient changes affect ALL SVF-using effects -- check wah and filter too
- Golden files are located at `crates/sonido-effects/tests/golden/`
- Test harness is at `crates/sonido-effects/tests/regression.rs`
