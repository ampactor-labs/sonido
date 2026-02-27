.PHONY: build test bench clean check fmt doc demo walkthrough verify-demos
.PHONY: quick-check verify test-nostd dev-install install plugins ci install-hooks

# Build
build:
	cargo build --release

# Run all tests
test:
	cargo test

# Run benchmarks
bench:
	cargo bench

# Clean build artifacts
clean:
	cargo clean

# Run lints and format check
check:
	cargo clippy --all-targets -- -D warnings
	cargo fmt --check

# Format code
fmt:
	cargo fmt

# Generate documentation
doc:
	cargo doc --no-deps --all-features

# Generate demo WAV files (5 signals + 22 effect demos)
demo:
	./scripts/generate_demos.sh

# Full guided walkthrough (build, effects, analysis, presets)
walkthrough:
	./scripts/walkthrough.sh

# Verify existing demos work
verify-demos:
	./scripts/walkthrough.sh --verify

# Fast daily check (~15s) — format + lint + key crate tests
quick-check:
	cargo fmt --check
	cargo clippy --workspace --all-targets -- -D warnings
	cargo test -p sonido-core -p sonido-effects -p sonido-registry -p sonido-gui --lib

# Full pre-commit verification (~90s)
verify:
	cargo fmt --check
	cargo clippy --workspace --all-targets -- -D warnings
	cargo test --workspace
	cargo test --no-default-features -p sonido-core -p sonido-effects \
		-p sonido-synth -p sonido-registry -p sonido-platform
	cargo test --test regression -p sonido-effects
	cargo test --test extreme_params -p sonido-effects
	RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

# Test no_std compatibility (all no_std crates)
test-nostd:
	cargo test --no-default-features -p sonido-core
	cargo test --no-default-features -p sonido-effects
	cargo test --no-default-features -p sonido-synth
	cargo test --no-default-features -p sonido-registry
	cargo test --no-default-features -p sonido-platform

# Install CLI via debug build symlink (fast iteration)
dev-install:
	cargo build -p sonido-cli
	@mkdir -p $(HOME)/.local/bin
	@ln -sf $(CURDIR)/target/debug/sonido $(HOME)/.local/bin/sonido
	@echo "Linked target/debug/sonido → ~/.local/bin/sonido"
	@echo "Ensure ~/.local/bin is in your PATH"

# Install CLI globally
install:
	cargo install --path crates/sonido-cli

# Build and install CLAP plugins to ~/.clap/
plugins:
	cargo build --release -p sonido-plugin --examples
	@mkdir -p $(HOME)/.clap
	@for f in target/release/examples/libsonido_*.so; do \
		name=$$(basename "$$f" .so | sed 's/^lib//; s/-/_/g'); \
		cp "$$f" "$(HOME)/.clap/$$name.clap"; \
		echo "Installed $$name.clap"; \
	done
	@echo "All plugins installed to ~/.clap/"

# Full local CI check (mirrors remote CI minus no_std/wasm cross-compilation)
ci:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings
	cargo test --workspace
	@echo "All CI checks passed"

# Install local pre-push git hook
install-hooks:
	@cp scripts/pre-push .git/hooks/pre-push
	@chmod +x .git/hooks/pre-push
	@echo "Pre-push hook installed at .git/hooks/pre-push"
