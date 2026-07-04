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
/// forces DEBUG and adds a stderr layer. Old tracing logs beyond
/// `config.general.log_keep_count` are pruned on startup (spec §14.3); the
/// per-day update logs are the user's audit trail and are never touched.
pub fn init(config: &Config, debug: bool) -> anyhow::Result<()> {
    let log_dir = log_dir()?;
    std::fs::create_dir_all(&log_dir)
        .with_context(|| format!("failed to create log directory: {}", log_dir.display()))?;

    let filename = format!("paclens-{}.log", Utc::now().format("%Y-%m-%d-%H%M%S"));
    prune_old_logs(&log_dir, config.general.log_keep_count as usize);
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

/// Delete tracing logs beyond the newest `keep` (best-effort: rotation must
/// never block startup). Runs before the new file is created, so `keep`
/// includes the file about to be written.
fn prune_old_logs(dir: &std::path::Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    for name in select_prunable(names, keep.saturating_sub(1)) {
        let _ = std::fs::remove_file(dir.join(name));
    }
}

/// Which tracing-log filenames to delete, keeping the newest `keep`. The
/// timestamp in `paclens-YYYY-MM-DD-HHMMSS.log` sorts lexically, so newest =
/// highest. Pure for testing; update logs (`YYYY-MM-DD.log`) never match.
fn select_prunable(mut names: Vec<String>, keep: usize) -> Vec<String> {
    names.retain(|n| n.starts_with("paclens-") && n.ends_with(".log"));
    names.sort_by(|a, b| b.cmp(a)); // newest first
    names.split_off(keep.min(names.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn prunes_oldest_tracing_logs_beyond_keep() {
        let out = select_prunable(
            names(&[
                "paclens-2026-07-01-100000.log",
                "paclens-2026-07-03-100000.log",
                "paclens-2026-07-02-100000.log",
            ]),
            2,
        );
        assert_eq!(out, vec!["paclens-2026-07-01-100000.log"]);
    }

    #[test]
    fn update_logs_and_strangers_are_never_pruned() {
        let out = select_prunable(
            names(&[
                "2026-07-03.log", // update log (audit trail)
                "notes.txt",      // unrelated
                "paclens-2026-07-01-100000.log",
            ]),
            0,
        );
        assert_eq!(out, vec!["paclens-2026-07-01-100000.log"]);
    }

    #[test]
    fn keeping_more_than_exist_deletes_nothing() {
        let out = select_prunable(names(&["paclens-2026-07-01-100000.log"]), 10);
        assert!(out.is_empty());
    }
}
