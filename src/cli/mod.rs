//! CLI entry: argument parsing, the init sequence, and subcommand dispatch.

mod status;

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

    // The actual config file in use, for cache invalidation on config changes.
    let config_path = cli
        .config
        .clone()
        .or_else(|| config::default_config_path().ok());

    let command = cli.command.unwrap_or(Command::Ui);
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        ?command,
        "paclens starting"
    );

    match command {
        Command::Ui => report(tui::run()),
        Command::Status => report(status::run(&config, cli.refresh, config_path.as_deref())),
        Command::Update { .. } => not_implemented("update"),
        Command::Why { .. } => not_implemented("why"),
        Command::Overlaps => not_implemented("overlaps"),
        Command::Cleanup => not_implemented("cleanup"),
    }
}

/// Map a fallible handler to an exit code, printing the error chain on failure.
fn report(result: anyhow::Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn not_implemented(name: &str) -> ExitCode {
    eprintln!("paclens: `{name}` is not implemented yet");
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        // Catches conflicting args, bad defaults, etc. at test time.
        Cli::command().debug_assert();
    }

    #[test]
    fn no_subcommand_means_ui() {
        let cli = Cli::try_parse_from(["paclens"]).unwrap();
        assert!(cli.command.is_none());
    }

    #[test]
    fn global_flags_parse_before_subcommand() {
        let cli = Cli::try_parse_from(["paclens", "--refresh", "--debug", "status"]).unwrap();
        assert!(cli.refresh);
        assert!(cli.debug);
        assert!(matches!(cli.command, Some(Command::Status)));
    }

    #[test]
    fn update_flags_parse() {
        let cli =
            Cli::try_parse_from(["paclens", "update", "--dry-run", "--source", "pacman"]).unwrap();
        match cli.command {
            Some(Command::Update { dry_run, source }) => {
                assert!(dry_run);
                assert_eq!(source.as_deref(), Some("pacman"));
            }
            other => panic!("expected update, got {other:?}"),
        }
    }

    #[test]
    fn why_requires_a_package_argument() {
        assert!(Cli::try_parse_from(["paclens", "why"]).is_err());
        let cli = Cli::try_parse_from(["paclens", "why", "firefox"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Why { package }) if package == "firefox"));
    }

    #[test]
    fn unknown_subcommand_is_rejected() {
        assert!(Cli::try_parse_from(["paclens", "frobnicate"]).is_err());
    }
}
