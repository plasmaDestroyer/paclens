//! Logging setup. Writes to a timestamped file under the data directory;
//! `--debug` raises the level to DEBUG and mirrors output to stderr.

use std::path::PathBuf;

use anyhow::{Context, anyhow};
use chrono::Utc;
use directories::ProjectDirs;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::config::Config;

/// Initialize the global tracing subscriber.
///
/// Level comes from `config.general.log_level`, unless `debug` is set, which
/// forces DEBUG and adds a stderr layer.
pub fn init(config: &Config, debug: bool) -> anyhow::Result<()> {
    let log_dir = log_dir()?;
    std::fs::create_dir_all(&log_dir)
        .with_context(|| format!("failed to create log directory: {}", log_dir.display()))?;

    let filename = format!("paclens-{}.log", Utc::now().format("%Y-%m-%d-%H%M%S"));
    let appender = tracing_appender::rolling::never(&log_dir, &filename);

    let level = if debug {
        crate::config::schema::LogLevel::Debug
    } else {
        config.general.log_level()
    };

    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(false)
        .with_writer(appender)
        .with_filter(level.to_level_filter());

    let registry = tracing_subscriber::registry().with(file_layer);

    if debug {
        let stderr_layer = tracing_subscriber::fmt::layer()
            .with_target(false)
            .with_writer(std::io::stderr)
            .with_filter(level.to_level_filter());
        registry.with(stderr_layer).init();
    } else {
        registry.init();
    }

    Ok(())
}

/// `~/.local/share/paclens/logs`, resolved via XDG.
fn log_dir() -> anyhow::Result<PathBuf> {
    let dirs = ProjectDirs::from("", "", "paclens")
        .ok_or_else(|| anyhow!("could not determine the data directory for this platform"))?;
    Ok(dirs.data_dir().join("logs"))
}
