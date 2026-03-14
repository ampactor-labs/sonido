# Documentation Verification Workflow

Run this checklist before every commit that touches code.

## Automated Checks

### 1. Rustdoc build (no warnings)

```bash
cargo doc --no-deps --all-features 2>&1 | grep -i warning
```

Zero warnings required. Common fixes:
- Missing `///` on public item -> add doc comment
- Broken intra-doc link -> fix path or use backtick-quoted code reference
- Unresolved link -> check that the referenced item exists and is in scope

### 2. Doc tests compile and run

```bash
cargo test --doc
```

All rustdoc examples must compile and produce expected output. Fix by:
- Adding necessary `use` imports to the example
- Using `# ` prefix to hide setup lines
- Marking non-runnable examples with `no_run` or `ignore`

### 3. Scan for stale markers

```bash
grep -rn "TODO\|FIXME\|XXX\|HACK" docs/ --include="*.md"
```

Resolve or remove any stale markers before committing.

## Structural Checks

### Every new .rs file has module docs

```bash
# Find .rs files without //! module doc on line 1
for f in $(git diff --cached --name-only --diff-filter=A -- '*.rs'); do
    head -1 "$f" | grep -q '^//!' || echo "Missing //! module doc: $f"
done
```

### Every public item has a doc comment

`missing_docs = "warn"` in workspace lints catches this during `cargo check`. Review warnings.

### Parameter setters document ranges

For any `set_*` or `with_*` method on a public type, verify the doc comment includes:
- Valid range (min to max)
- Units (Hz, dB, ms, normalized 0-1)
- Boundary behavior (clamp, error, wrap)

## DSP-Specific Checks

### Algorithm docs cite references

Every DSP function that implements a published algorithm must include:
- Algorithm name and what it does in signal processing terms
- Mathematical formula or transfer function
- Reference source (paper, textbook, cookbook)

Standard references:
- Bristow-Johnson, "Audio EQ Cookbook" (biquad filters)
- Valimaki/Smith, "Principles of Digital Signal Processing" (delay-based effects)
- Jezar's Freeverb (reverb topology)
- Valimaki et al., "Antialiasing Oscillators" (PolyBLEP)
- Zolzer, "DAFX" (general effects)

### Parameter units specified

For every `ParamDescriptor`, verify that `unit` is set appropriately (`Hz`, `Db`, `Ms`, `Percent`, `None`) and ranges match the doc comment.

## Doc-to-Code Mapping

Cross-reference against `docs/DOC_CODE_MAPPING.md`. For every source file you modified, check Column B in the mapping table and update the listed documentation targets.

## Full Checklist

- [ ] `cargo doc --no-deps --all-features` produces no warnings
- [ ] `cargo test --doc` passes
- [ ] Every new public item has `///` doc comment
- [ ] Every new `.rs` file has `//!` module doc comment
- [ ] DSP functions document algorithm and cite reference
- [ ] Parameter setters document valid range and units
- [ ] `docs/EFFECTS_REFERENCE.md` updated if any effect changed
- [ ] `docs/DSP_FUNDAMENTALS.md` updated if any DSP algorithm changed
- [ ] `docs/DESIGN_DECISIONS.md` has ADR for any new architectural choice
- [ ] `README.md` reflects any user-facing changes
- [ ] `docs/CHANGELOG.md` has an entry for the change
- [ ] No stale references to renamed/removed items in any `.md` file
- [ ] `docs/DOC_CODE_MAPPING.md` targets updated for modified source files
