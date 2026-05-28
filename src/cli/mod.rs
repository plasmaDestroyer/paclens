//! CLI entry: argument parsing, the init sequence, and subcommand dispatch.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{ArgAction, Parser, Subcommand};

use crate::config;
use crate::logging;
use crate::tui;

/// A TUI-first pacman + Flatpak inspection and update tool for Arch Linux.
#[derive(Debug, Parser)]
#[command(
    name = "paclens",
    version,
    about,
    long_about = None,
    disable_version_flag = true
)]
pub struct Cli {
    /// Print version and exit.
    #[arg(short = 'v', long = "version", action = ArgAction::Version)]
    version: Option<bool>,

    /// Force a re-scan, ignoring the cache.
    #[arg(long, global = true)]
    pub refresh: bool,

    /// Disable colored output and use ASCII box drawing.
    #[arg(long = "no-color", global = true)]
    pub no_color: bool,

    /// Enable debug-level logging to stderr and the log file.
    #[arg(long, global = true)]
    pub debug: bool,

    /// Use an alternate config file.
    #[arg(long, value_name = "PATH", global = true)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Open the TUI (default when no subcommand is given).
    Ui,
    /// Print a dashboard summary to stdout.
    Status,
    /// Update all sources or a specific one.
    Update {
        /// Print the plan without executing anything.
        #[arg(long)]
        dry_run: bool,
        /// Limit the update to a single source id.
        #[arg(long, value_name = "ID")]
        source: Option<String>,
    },
    /// Explain why a package is installed and what removing it affects.
    Why {
        /// The package name to explain.
        package: String,
    },
    /// List detected Flatpak/native overlaps.
    Overlaps,
    /// Print an orphan and cache summary (advisory only).
    Cleanup,
}

/// Parse args, load config, initialize logging, and dispatch.
pub fn run() -> ExitCode {
    let cli = Cli::parse();

    let config = match config::load(cli.config.as_deref()) {
        Ok(loaded) => {
            for key in &loaded.unknown_keys {
                eprintln!("warning: unknown config key ignored: {key}");
            }
            for warning in loaded.config.validation_warnings() {
                eprintln!("warning: {warning}");
            }
            loaded.config
        }
        Err(err) => {
            eprintln!("error: {err:#}");
            return ExitCode::FAILURE;
        }
    };

    if let Err(err) = logging::init(&config, cli.debug) {
        eprintln!("error: failed to initialize logging: {err:#}");
        return ExitCode::FAILURE;
    }

    let command = cli.command.unwrap_or(Command::Ui);
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        ?command,
        "paclens starting"
    );

    match command {
        Command::Ui => match tui::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("error: {err:#}");
                ExitCode::FAILURE
            }
        },
        Command::Status => not_implemented("status"),
        Command::Update { .. } => not_implemented("update"),
        Command::Why { .. } => not_implemented("why"),
        Command::Overlaps => not_implemented("overlaps"),
        Command::Cleanup => not_implemented("cleanup"),
    }
}

fn not_implemented(name: &str) -> ExitCode {
    eprintln!("paclens: `{name}` is not implemented yet");
    ExitCode::FAILURE
}
