//! CLI entry: argument parsing, the init sequence, and subcommand dispatch.

mod status;
mod style;
mod update;

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{ArgAction, Parser, Subcommand};

use crate::config;
use crate::config::ColorTheme;
use crate::logging;
use crate::tui;

use style::Styles;

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
    let stderr_tty = std::io::stderr().is_terminal();

    // Until the config is loaded we don't know the theme, so assume the default
    // for any early error. Color is still gated on --no-color and the TTY check.
    let early_err = Styles::resolve(cli.no_color, ColorTheme::Dark, stderr_tty);

    let loaded = match config::load(cli.config.as_deref()) {
        Ok(loaded) => loaded,
        Err(err) => {
            eprintln!("{} {err:#}", early_err.error("error:"));
            return ExitCode::FAILURE;
        }
    };
    let config = loaded.config;
    let err_styles = Styles::resolve(cli.no_color, config.general.color_theme(), stderr_tty);
    for key in &loaded.unknown_keys {
        eprintln!(
            "{} unknown config key ignored: {key}",
            err_styles.warn("warning:")
        );
    }
    for warning in config.validation_warnings() {
        eprintln!("{} {warning}", err_styles.warn("warning:"));
    }

    if let Err(err) = logging::init(&config, cli.debug) {
        eprintln!(
            "{} failed to initialize logging: {err:#}",
            err_styles.error("error:")
        );
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
        Command::Ui => report(
            tui::run(&config, cli.refresh, config_path.as_deref(), cli.no_color),
            &err_styles,
        ),
        Command::Status => {
            let out_styles = Styles::resolve(
                cli.no_color,
                config.general.color_theme(),
                std::io::stdout().is_terminal(),
            );
            report(
                status::run(&config, cli.refresh, config_path.as_deref(), &out_styles),
                &err_styles,
            )
        }
        Command::Update { dry_run, source } => {
            let out_styles = Styles::resolve(
                cli.no_color,
                config.general.color_theme(),
                std::io::stdout().is_terminal(),
            );
            report(
                update::run(
                    &config,
                    cli.refresh,
                    config_path.as_deref(),
                    dry_run,
                    source.as_deref(),
                    std::io::stdin().is_terminal(),
                    &out_styles,
                ),
                &err_styles,
            )
        }
        Command::Why { .. } => not_implemented("why", &err_styles),
        Command::Overlaps => not_implemented("overlaps", &err_styles),
        Command::Cleanup => not_implemented("cleanup", &err_styles),
    }
}

/// Map a fallible handler to an exit code, printing the error chain on failure.
fn report(result: anyhow::Result<()>, styles: &Styles) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{} {err:#}", styles.error("error:"));
            ExitCode::FAILURE
        }
    }
}

fn not_implemented(name: &str, styles: &Styles) -> ExitCode {
    eprintln!("{} `{name}` is not implemented yet", styles.dim("paclens:"));
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
