//! Sonido CLI - Command-line interface for the Sonido DSP framework.

mod commands;
mod effects;
mod graph_dsl;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "sonido")]
#[command(author, version, about = "Sonido DSP Framework CLI", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Process an audio file through effects
    Process(commands::process::ProcessArgs),

    /// Run real-time audio processing
    Realtime(commands::realtime::RealtimeArgs),

    /// Generate test signals
    Generate(commands::generate::GenerateArgs),

    /// Analyze audio files
    Analyze(commands::analyze::AnalyzeArgs),

    /// Compare two audio files (A/B comparison)
    Compare(commands::compare::CompareArgs),

    /// List and manage audio devices
    Devices(commands::devices::DevicesArgs),

    /// List available effects and their parameters
    Effects(commands::effects::EffectsArgs),

    /// Display WAV file information
    Info(commands::info::InfoArgs),

    /// Play an audio file through effects
    Play(commands::play::PlayArgs),

    /// Manage effect presets (list, show, save, delete)
    Presets(commands::presets::PresetsArgs),
}

fn main() -> anyhow::Result<()> {
    use tracing_subscriber::EnvFilter;

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()))
        .init();

    let cli = Cli::parse();

    tracing::debug!(command = ?std::mem::discriminant(&cli.command), "dispatching command");

    match cli.command {
        Commands::Process(args) => commands::process::run(args),
        Commands::Realtime(args) => commands::realtime::run(args),
        Commands::Generate(args) => commands::generate::run(args),
        Commands::Analyze(args) => commands::analyze::run(args),
        Commands::Compare(args) => commands::compare::run(args),
        Commands::Devices(args) => commands::devices::run(args),
        Commands::Effects(args) => commands::effects::run(args),
        Commands::Info(args) => commands::info::run(args),
        Commands::Play(args) => commands::play::run(args),
        Commands::Presets(args) => commands::presets::run(args),
    }
}
