//! The update log writer (spec §14.4, roadmap v0.0.6): a per-day, append-only
//! record of every update session at `~/.local/share/paclens/logs/YYYY-MM-DD.log`.
//!
//! Separate from the tracing log: this file is the user-facing audit trail of
//! what paclens executed and how it exited, in the spec's fixed line format.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};
use chrono::Utc;
use directories::ProjectDirs;

pub struct UpdateLog {
    file: File,
    path: PathBuf,
}

impl UpdateLog {
    /// Open (append, create) today's update log in the default XDG data dir.
    pub fn open_default() -> anyhow::Result<Self> {
        let dirs = ProjectDirs::from("", "", "paclens")
            .ok_or_else(|| anyhow!("could not determine the data directory for this platform"))?;
        Self::open_in(&dirs.data_dir().join("logs"))
    }

    /// Open (append, create) today's update log in `dir`. The injectable seam:
    /// tests point this at a temp dir.
    pub fn open_in(dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create log directory: {}", dir.display()))?;
        let path = dir.join(format!("{}.log", Utc::now().format("%Y-%m-%d")));
        let file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)
            .with_context(|| format!("failed to open update log: {}", path.display()))?;
        Ok(UpdateLog { file, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The newest update log on disk, if any session ever ran (for the TUI's
    /// `L` log viewer). Update logs are named `YYYY-MM-DD.log`, so the lexical
    /// maximum is the newest.
    pub fn latest_path() -> Option<PathBuf> {
        let dirs = ProjectDirs::from("", "", "paclens")?;
        Self::latest_in(&dirs.data_dir().join("logs"))
    }

    fn latest_in(dir: &Path) -> Option<PathBuf> {
        let entries = std::fs::read_dir(dir).ok()?;
        entries
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.ends_with(".log") && !n.starts_with("paclens-"))
            .max()
            .map(|name| dir.join(name))
    }

    /// Append one spec-format line: `[<UTC timestamp> INFO] <msg>`. Best-effort:
    /// a failed write must never abort a running update, so it is only traced.
    pub fn line(&mut self, msg: &str) {
        let stamp = Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
        if let Err(err) = writeln!(self.file, "[{stamp} INFO] {msg}") {
            tracing::warn!(?err, "failed to write to the update log");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("paclens-log-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn creates_a_per_day_log_file_and_writes_spec_format_lines() {
        let dir = sandbox("create");
        let mut log = UpdateLog::open_in(&dir).unwrap();
        log.line("update session started");

        let expected = dir.join(format!("{}.log", Utc::now().format("%Y-%m-%d")));
        assert_eq!(log.path(), expected);
        let text = std::fs::read_to_string(&expected).unwrap();
        assert!(
            text.contains(" INFO] update session started"),
            "bad line: {text}"
        );
        assert!(text.starts_with('['), "missing timestamp bracket: {text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn latest_in_picks_the_newest_update_log_and_ignores_tracing_logs() {
        let dir = sandbox("latest");
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "2026-07-01.log",
            "2026-07-04.log",
            "2026-07-02.log",
            "paclens-2026-07-05-120000.log", // tracing log, not an update log
        ] {
            std::fs::write(dir.join(name), "x").unwrap();
        }
        assert_eq!(UpdateLog::latest_in(&dir), Some(dir.join("2026-07-04.log")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn latest_in_is_none_for_missing_or_empty_dir() {
        let dir = sandbox("latest-empty");
        assert_eq!(UpdateLog::latest_in(&dir), None); // dir doesn't exist
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(UpdateLog::latest_in(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reopening_appends_instead_of_truncating() {
        let dir = sandbox("append");
        UpdateLog::open_in(&dir).unwrap().line("first session");
        UpdateLog::open_in(&dir).unwrap().line("second session");

        let path = dir.join(format!("{}.log", Utc::now().format("%Y-%m-%d")));
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("first session"));
        assert!(text.contains("second session"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
