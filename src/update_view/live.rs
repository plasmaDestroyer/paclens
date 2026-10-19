//! The live view's run plumbing (#81): which steps get the view, and the
//! last-run transcript it leaves behind.
//!
//! The transcript is written in `script(1)`'s format — a `.out` with a
//! `Script started on …` header and a `.timing` of `<delta> <len>` lines — the
//! same shape as the fixture in `tests/fixtures/pacman-run`, so
//! `examples/replay` plays the last real run. Last run only, mode 0600,
//! replaced each time.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::Duration;

use crate::executor::sudo::CANDIDATES;

/// The last run's raw pty output, `script(1)` style.
pub const TRANSCRIPT: &str = "pacman-last.out";
/// The `<delta> <len>` timing lines matching [`TRANSCRIPT`].
pub const TRANSCRIPT_TIMING: &str = "pacman-last.timing";
/// The pacman.log lines this run produced.
pub const TRANSCRIPT_LOG: &str = "pacman-last.log";

/// Does this step run a pacman transaction? An escalation tool in front
/// (`sudo pacman …`) is skipped; anything else (`paru`, `flatpak`) is not
/// pacman, whoever runs it.
pub fn is_pacman_transaction(argv: &[String]) -> bool {
    let mut rest = argv.iter();
    let mut program = rest.next();
    if program.is_some_and(|p| CANDIDATES.contains(&p.as_str())) {
        program = rest.next();
    }
    program.is_some_and(|p| p == "pacman")
}

/// The last-run transcript. Writes after [`Transcript::create`] are best
/// effort: a failure is logged and dropped, never allowed to stop the run.
pub struct Transcript {
    out: File,
    timing: File,
    log: File,
    last: Duration,
}

impl Transcript {
    /// Create (or replace) the three files in `dir`, private to the user, and
    /// write the `Script started on …` header naming `command`.
    pub fn create(dir: &Path, command: &str) -> io::Result<Self> {
        let mut out = private_file(&dir.join(TRANSCRIPT))?;
        let timing = private_file(&dir.join(TRANSCRIPT_TIMING))?;
        let log = private_file(&dir.join(TRANSCRIPT_LOG))?;
        let started = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%:z");
        writeln!(out, "Script started on {started} [COMMAND=\"{command}\"]")?;
        Ok(Self {
            out,
            timing,
            log,
            last: Duration::ZERO,
        })
    }

    /// Append pty output that arrived `at` (since the run started).
    pub fn record(&mut self, at: Duration, bytes: &[u8]) {
        let delta = at.saturating_sub(self.last);
        self.last = at;
        let result = self
            .out
            .write_all(bytes)
            .and_then(|()| writeln!(self.timing, "{:.6} {}", delta.as_secs_f64(), bytes.len()));
        if let Err(err) = result {
            tracing::warn!(error = %err, "transcript write failed");
        }
    }

    /// Append one pacman.log line.
    pub fn log_line(&mut self, line: &str) {
        if let Err(err) = writeln!(self.log, "{line}") {
            tracing::warn!(error = %err, "transcript log write failed");
        }
    }
}

/// Open `path` for writing, truncated, mode 0600 — also when it already
/// existed with wider permissions, so a replaced file stays private.
fn private_file(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv<const N: usize>(a: [&str; N]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("paclens-live-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn detects_pacman_steps() {
        assert!(is_pacman_transaction(&argv(["sudo", "pacman", "-Syu"])));
        assert!(is_pacman_transaction(&argv(["pacman", "-S", "foo"])));
        assert!(!is_pacman_transaction(&argv(["paru", "-Sua"])));
        assert!(!is_pacman_transaction(&argv(["sudo", "flatpak", "update"])));
    }

    #[test]
    fn detects_pacman_behind_every_escalation_tool() {
        assert!(is_pacman_transaction(&argv(["doas", "pacman", "-Syu"])));
        assert!(is_pacman_transaction(&argv(["pkexec", "pacman", "-Syu"])));
    }

    #[test]
    fn empty_and_bare_escalation_are_not_pacman() {
        assert!(!is_pacman_transaction(&[]));
        assert!(!is_pacman_transaction(&argv(["sudo"])));
    }

    #[test]
    fn only_one_escalation_tool_is_skipped() {
        assert!(!is_pacman_transaction(&argv(["sudo", "sudo", "pacman"])));
    }

    #[test]
    fn transcript_is_private_and_replaced() {
        let dir = temp_dir("replaced");
        // A leftover with wide permissions must come back private.
        fs::write(dir.join(TRANSCRIPT), "old run").unwrap();
        fs::set_permissions(dir.join(TRANSCRIPT), fs::Permissions::from_mode(0o644)).unwrap();

        let mut first = Transcript::create(&dir, "sudo pacman -Syu").unwrap();
        first.record(Duration::from_millis(10), b"first");
        first.log_line("first log");
        drop(first);
        let mut second = Transcript::create(&dir, "sudo pacman -S foo").unwrap();
        second.record(Duration::from_millis(10), b"second");
        second.log_line("second log");
        drop(second);

        for name in [TRANSCRIPT, TRANSCRIPT_TIMING, TRANSCRIPT_LOG] {
            assert_eq!(mode(&dir.join(name)), 0o600, "{name}");
        }
        let out = fs::read_to_string(dir.join(TRANSCRIPT)).unwrap();
        assert!(out.contains("second") && !out.contains("first") && !out.contains("old run"));
        assert!(out.contains("[COMMAND=\"sudo pacman -S foo\"]"));
        assert_eq!(
            fs::read_to_string(dir.join(TRANSCRIPT_TIMING)).unwrap(),
            "0.010000 6\n"
        );
        assert_eq!(
            fs::read_to_string(dir.join(TRANSCRIPT_LOG)).unwrap(),
            "second log\n"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn transcript_matches_the_script_format() {
        let dir = temp_dir("format");
        let mut t = Transcript::create(&dir, "sudo pacman -Syu").unwrap();
        t.record(Duration::from_micros(226_000), b"[sudo] password for me: ");
        t.record(Duration::from_micros(4_348_530), b"\r\n");
        drop(t);

        let out = fs::read_to_string(dir.join(TRANSCRIPT)).unwrap();
        let (header, body) = out.split_once('\n').unwrap();
        assert!(header.starts_with("Script started on "), "{header}");
        assert!(
            header.ends_with(" [COMMAND=\"sudo pacman -Syu\"]"),
            "{header}"
        );
        // `2026-10-16 11:35:31+05:30`: date, time, numeric offset with a colon.
        let stamp = header
            .trim_start_matches("Script started on ")
            .split(" [")
            .next()
            .unwrap();
        assert_eq!(stamp.len(), 25, "{stamp}");
        assert!(chrono::DateTime::parse_from_str(stamp, "%Y-%m-%d %H:%M:%S%:z").is_ok());
        assert_eq!(body, "[sudo] password for me: \r\n");

        // Deltas since the previous record, six decimals, then byte count.
        assert_eq!(
            fs::read_to_string(dir.join(TRANSCRIPT_TIMING)).unwrap(),
            "0.226000 24\n4.122530 2\n"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn create_fails_when_the_dir_is_missing() {
        let dir = std::env::temp_dir().join(format!("paclens-live-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert!(Transcript::create(&dir, "pacman -Syu").is_err());
    }
}
