# CLI and Graph DSL Context

On-demand reference for CLI and graph DSL work.

## Adding a New CLI Command

1. **Create command file** `crates/sonido-cli/src/commands/my_command.rs` with:
   - An args struct deriving `clap::Args` (e.g., `pub struct MyCommandArgs { ... }`)
   - A `pub fn run(args: MyCommandArgs) -> anyhow::Result<()>` function
2. **Register module** in `crates/sonido-cli/src/commands/mod.rs`:
   ```rust
   pub mod my_command;
   ```
3. **Add variant** to the `Commands` enum in `crates/sonido-cli/src/main.rs`:
   ```rust
   /// Description of my command
   MyCommand(commands::my_command::MyCommandArgs),
   ```
4. **Add match arm** in `main()`:
   ```rust
   Commands::MyCommand(args) => commands::my_command::run(args),
   ```
5. **Update docs** -- `docs/CLI_GUIDE.md` with syntax, flags, and examples

## Graph DSL Overview

The graph DSL crate (`sonido-graph-dsl`) provides a text-based topology description language for building `ProcessingGraph` instances.

| File | Purpose |
|------|---------|
| `parser.rs` | Parses DSL text into an AST (node declarations, connections, parameters) |
| `builder.rs` | Converts parsed AST into a `ProcessingGraph` via `GraphEngine` API |
| `effects.rs` | Effect factory with alias resolution -- maps DSL effect names to registry entries |
| `serialize.rs` | Serializes a graph topology back to DSL text format |

Pipeline: DSL text -> `parser.rs` (AST) -> `builder.rs` (GraphEngine calls) -> compiled `ProcessingGraph`

## Effect Aliases

The DSL and registry support aliases for backward compatibility:

| Alias | Resolves To |
|-------|-------------|
| `multivibrato` | `vibrato` |
| `tape_saturation` | `tape` |
| `parametric_eq` | `eq` |

## Command Architecture

CLI commands live in `crates/sonido-cli/src/commands/`:

| Module | Purpose |
|--------|---------|
| `analyze` | Spectral analysis and signal inspection |
| `common` | Shared CLI utilities |
| `effects` | List available effects and their parameters |
| `process` | Process WAV files through effect chains |
| `info` | Display crate/version information |
| `devices` | List audio devices |
| `generate` | Generate test signals |
| `presets` | Preset management |
| `play` | Play audio files |
| `compare` | Compare audio files |
| `realtime` | Real-time audio processing |

Entry point: `crates/sonido-cli/src/main.rs` (clap-based command dispatch)

Graph DSL integration: `crates/sonido-cli/src/graph_dsl.rs` bridges DSL parsing into CLI commands.

## Key Files

- **sonido-cli**: `crates/sonido-cli/src/` -- main.rs, commands/ (see table above), graph_dsl.rs
- **sonido-graph-dsl**: `crates/sonido-graph-dsl/src/` -- parser.rs, builder.rs, effects.rs, serialize.rs

See `docs/CLI_GUIDE.md` for user-facing documentation.
