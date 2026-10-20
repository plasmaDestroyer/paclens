//! CLI entry: argument parsing, the init sequence, and subcommand dispatch.

mod cleanup;
pub(crate) mod history;
mod migrate;
mod overlaps;
pub(crate) mod pacman;
mod status;
pub mod style;
mod transcript;
mod update;
mod why;

use std::ffi::OsString;
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
    // Spelled out rather than taken from the Cargo manifest. The manifest
    // description also serves the crates.io listing, where it has to say that
    // this is a binary and not a library — a sentence that means nothing in
    // `--help`.
    about = "A TUI-first pacman + AUR + Flatpak inspection and update tool for Arch Linux.",
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
    Status {
        /// One line for status bars: the cached update count. Exit 0 up to
        /// date, 1 updates pending, 2 unknown. Never scans.
        #[arg(long)]
        check: bool,
    },
    /// Update all sources or a specific one.
    Update {
        /// Print the plan without executing anything.
        #[arg(long)]
        dry_run: bool,
        /// Limit the update to a single source id.
        #[arg(long, value_name = "ID")]
        source: Option<String>,
        /// Run the sources that ask no questions at the same time, while
        /// pacman and the AUR helper take the terminal in turn.
        #[arg(long)]
        parallel: bool,
    },
    /// Explain why a package is installed and what removing it affects.
    Why {
        /// The package name to explain.
        package: String,
    },
    /// List detected Flatpak/native overlaps.
    Overlaps,
    /// Show the migration advisory report for one overlap; --run executes it.
    Migrate {
        /// The overlap to report on: display name, package name, or app id.
        package: String,
        /// Migration direction (default: toward the likely-primary side).
        #[arg(long, value_enum, value_name = "SIDE")]
        to: Option<MigrateTarget>,
        /// Execute the copy plan (backup, then cp -aT; asks y/N first).
        #[arg(long)]
        run: bool,
    },
    /// Print an orphan and cache summary (advisory only).
    Cleanup {
        /// List every orphan, runtime and file instead of the largest few.
        #[arg(long)]
        all: bool,
    },
    /// What past upgrades changed, from pacman's own log.
    History {
        /// One package's history instead of the transaction list.
        #[arg(long, value_name = "NAME")]
        package: Option<String>,
        /// How many transactions to list.
        #[arg(long, default_value_t = 15)]
        limit: usize,
    },
    /// Show the last pacman run as pacman printed it.
    Transcript,
    /// Run pacman under the live view: installs and upgrades (-S) only.
    /// Everything after `pacman` goes to pacman untouched.
    #[command(disable_help_flag = true)]
    Pacman,
}

/// `--to` values for `migrate`, mapped onto the model's `Direction`.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum MigrateTarget {
    Flatpak,
    Native,
}

impl From<MigrateTarget> for crate::model::Direction {
    fn from(t: MigrateTarget) -> Self {
        match t {
            MigrateTarget::Flatpak => crate::model::Direction::ToFlatpak,
            MigrateTarget::Native => crate::model::Direction::ToNative,
        }
    }
}

/// Parse args, load config, initialize logging, and dispatch.
pub fn run() -> ExitCode {
    let (argv, pacman_args) = split_pacman(std::env::args_os().collect());
    let mut cli = Cli::parse_from(argv);
    cli.no_color |= no_color_requested(std::env::var_os("NO_COLOR"));
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
        Command::Status { check: true } => status::check(),
        Command::Status { check: false } => {
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
        Command::Update {
            dry_run,
            source,
            parallel,
        } => {
            let out_styles = Styles::resolve(
                cli.no_color,
                config.general.color_theme(),
                std::io::stdout().is_terminal(),
            );
            report(
                update::run(
                    &config,
                    dry_run,
                    source.as_deref(),
                    parallel,
                    std::io::stdin().is_terminal(),
                    &out_styles,
                ),
                &err_styles,
            )
        }
        Command::Why { package } => {
            let out_styles = Styles::resolve(
                cli.no_color,
                config.general.color_theme(),
                std::io::stdout().is_terminal(),
            );
            report(
                why::run(
                    &config,
                    cli.refresh,
                    config_path.as_deref(),
                    &package,
                    &out_styles,
                ),
                &err_styles,
            )
        }
        Command::Overlaps => {
            let out_styles = Styles::resolve(
                cli.no_color,
                config.general.color_theme(),
                std::io::stdout().is_terminal(),
            );
            report(
                overlaps::run(&config, cli.refresh, config_path.as_deref(), &out_styles),
                &err_styles,
            )
        }
        Command::Migrate { package, to, run } => {
            let out_styles = Styles::resolve(
                cli.no_color,
                config.general.color_theme(),
                std::io::stdout().is_terminal(),
            );
            report(
                migrate::run(
                    &config,
                    cli.refresh,
                    config_path.as_deref(),
                    &package,
                    to.map(Into::into),
                    run,
                    std::io::stdin().is_terminal(),
                    &out_styles,
                ),
                &err_styles,
            )
        }
        Command::History { package, limit } => {
            let out_styles = Styles::resolve(
                cli.no_color,
                config.general.color_theme(),
                std::io::stdout().is_terminal(),
            );
            report(
                history::run(package.as_deref(), limit, &out_styles),
                &err_styles,
            )
        }
        Command::Cleanup { all } => {
            let out_styles = Styles::resolve(
                cli.no_color,
                config.general.color_theme(),
                std::io::stdout().is_terminal(),
            );
            report(
                cleanup::run(
                    &config,
                    cli.refresh,
                    config_path.as_deref(),
                    all,
                    &out_styles,
                ),
                &err_styles,
            )
        }
        Command::Pacman => {
            let out_styles = Styles::resolve(
                cli.no_color,
                config.general.color_theme(),
                std::io::stdout().is_terminal(),
            );
            report(
                pacman::run(
                    &config,
                    &pacman_args,
                    std::io::stdin().is_terminal(),
                    &out_styles,
                ),
                &err_styles,
            )
        }
        Command::Transcript => report(
            crate::scanner::cache::cache_dir()
                .and_then(|dir| transcript::run(&dir, &mut std::io::stdout().lock())),
            &err_styles,
        ),
    }
}

/// Split `paclens [global flags] pacman <args>`: clap reads up to and
/// including `pacman`, and the rest goes to pacman untouched. clap alone
/// would read `paclens pacman --config /x -Syu` as paclens's own `--config`.
fn split_pacman(argv: Vec<OsString>) -> (Vec<OsString>, Vec<String>) {
    let mut i = 1;
    while let Some(a) = argv.get(i).and_then(|a| a.to_str()) {
        match a {
            "pacman" => {
                let rest = argv[i + 1..]
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect();
                let mut argv = argv;
                argv.truncate(i + 1);
                return (argv, rest);
            }
            "--no-color" | "--debug" | "--refresh" => i += 1,
            "--config" => i += 2,
            _ if a.starts_with("--config=") => i += 1,
            _ => break,
        }
    }
    (argv, Vec::new())
}

/// The `NO_COLOR` convention (no-color.org): set and non-empty means no colour.
fn no_color_requested(value: Option<std::ffi::OsString>) -> bool {
    value.is_some_and(|v| !v.is_empty())
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
    fn no_color_env_needs_a_non_empty_value() {
        assert!(no_color_requested(Some("1".into())));
        assert!(!no_color_requested(Some("".into())), "empty means unset");
        assert!(!no_color_requested(None));
    }

    fn split(args: &[&str]) -> (Cli, Vec<String>) {
        let (argv, rest) = split_pacman(args.iter().map(OsString::from).collect());
        (Cli::try_parse_from(argv).unwrap(), rest)
    }

    #[test]
    fn pacman_args_pass_through_untouched() {
        let (cli, args) = split(&["paclens", "pacman", "-S", "--needed", "foo"]);
        assert!(matches!(cli.command, Some(Command::Pacman)));
        assert_eq!(args, ["-S", "--needed", "foo"]);
    }

    #[test]
    fn pacman_args_after_subcommand_are_not_paclens_flags() {
        for a in [
            &["paclens", "pacman", "--config", "/x", "-Syu"][..],
            &["paclens", "pacman", "-Syu", "--config", "/x"][..],
        ] {
            let (cli, args) = split(a);
            assert!(cli.config.is_none(), "{a:?}");
            assert!(args.windows(2).any(|w| w == ["--config", "/x"]), "{a:?}");
        }
        let (_, args) = split(&["paclens", "pacman", "--help"]);
        assert_eq!(args, ["--help"]);
        let (cli, args) = split(&["paclens", "--no-color", "pacman", "-Syu"]);
        assert!(cli.no_color);
        assert_eq!(args, ["-Syu"]);
        let (cli, args) = split(&["paclens", "--config", "/c", "pacman", "-Syu"]);
        assert_eq!(cli.config.as_deref(), Some(std::path::Path::new("/c")));
        assert_eq!(args, ["-Syu"]);
    }

    #[test]
    fn pacman_as_a_value_is_not_the_subcommand() {
        let (argv, rest) = split_pacman(
            ["paclens", "why", "pacman"]
                .iter()
                .map(OsString::from)
                .collect(),
        );
        assert_eq!(argv.len(), 3);
        assert!(rest.is_empty());
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
        assert!(matches!(cli.command, Some(Command::Status { .. })));
    }

    #[test]
    fn update_flags_parse() {
        let cli =
            Cli::try_parse_from(["paclens", "update", "--dry-run", "--source", "pacman"]).unwrap();
        match cli.command {
            Some(Command::Update {
                dry_run,
                source,
                parallel,
            }) => {
                assert!(dry_run);
                assert_eq!(source.as_deref(), Some("pacman"));
                assert!(!parallel, "sequential unless asked");
            }
            other => panic!("expected update, got {other:?}"),
        }
    }

    #[test]
    fn parallel_is_opt_in() {
        let cli = Cli::try_parse_from(["paclens", "update", "--parallel"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Update { parallel: true, .. })
        ));
    }

    #[test]
    fn why_requires_a_package_argument() {
        assert!(Cli::try_parse_from(["paclens", "why"]).is_err());
        let cli = Cli::try_parse_from(["paclens", "why", "firefox"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Why { package }) if package == "firefox"));
    }

    #[test]
    fn transcript_parses_with_no_arguments() {
        let cli = Cli::try_parse_from(["paclens", "transcript"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Transcript)));
    }

    #[test]
    fn unknown_subcommand_is_rejected() {
        assert!(Cli::try_parse_from(["paclens", "frobnicate"]).is_err());
    }
}
