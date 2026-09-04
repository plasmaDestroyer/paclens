//! `.pacnew` / `.pacsave` files left behind by upgrades (design §3, #2).
//!
//! A `.pacnew` either exists or it does not, so every finding here is
//! `Confirmed` by construction — no inference, no network, nothing to label.
//! It is also the chore that quietly rots an Arch install: upstream changes a
//! default, pacman refuses to overwrite the edited file, and the new version
//! sits next to it for years.
//!
//! Pure over what the scanner found. Merging a config is genuinely
//! destructive, so nothing here executes anything: the review command is
//! copiable text, which is where the trust ladder puts it (design §5).

use serde::{Deserialize, Serialize};

/// Which kind of leftover this is. They mean opposite things: a `.pacnew` is
/// the new version waiting to be merged into yours, a `.pacsave` is *your*
/// version left behind when the package was removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PacFileKind {
    /// Upstream's new version; your file was kept.
    Pacnew,
    /// Your version; the package that owned it was removed.
    Pacsave,
}

impl PacFileKind {
    pub fn suffix(self) -> &'static str {
        match self {
            PacFileKind::Pacnew => ".pacnew",
            PacFileKind::Pacsave => ".pacsave",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PacFileKind::Pacnew => "pacnew",
            PacFileKind::Pacsave => "pacsave",
        }
    }
}

/// One leftover file, as the scanner found it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PacFile {
    /// Absolute path of the leftover, e.g. `/etc/pacman.conf.pacnew`.
    pub path: String,
    pub kind: PacFileKind,
    /// Seconds since the epoch, or `None` where it could not be read. Used
    /// only for ordering, so it stays a number rather than a timestamp type.
    pub modified_secs: Option<u64>,
}

impl PacFile {
    /// The config this one sits next to: the path minus the suffix.
    pub fn base(&self) -> &str {
        self.path
            .strip_suffix(self.kind.suffix())
            .unwrap_or(&self.path)
    }
}

/// Findings in review order: newest first, because the ones an upgrade just
/// created are the ones still fresh in mind. Ties fall back to the path so the
/// list never reorders between scans.
pub fn review_order(files: &[PacFile]) -> Vec<&PacFile> {
    let mut out: Vec<&PacFile> = files.iter().collect();
    out.sort_by(|a, b| {
        b.modified_secs
            .cmp(&a.modified_secs)
            .then_with(|| a.path.cmp(&b.path))
    });
    out
}

/// The diff program to suggest: the configured one, else `$DIFFPROG`, else
/// `vimdiff` — which is the order `pacdiff` itself resolves in.
pub fn diff_program(configured: &str, env_diffprog: Option<&str>) -> String {
    let configured = configured.trim();
    if !configured.is_empty() {
        return configured.to_string();
    }
    match env_diffprog.map(str::trim) {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => "vimdiff".to_string(),
    }
}

/// The command that walks all of them, which is what a user with ten of these
/// actually wants. `pacdiff` is part of pacman-contrib, which paclens already
/// requires.
pub fn review_all_command(diff_prog: &str) -> String {
    format!("sudo DIFFPROG={diff_prog} pacdiff")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, kind: PacFileKind, mtime: Option<u64>) -> PacFile {
        PacFile {
            path: path.to_string(),
            kind,
            modified_secs: mtime,
        }
    }

    #[test]
    fn base_is_the_config_the_leftover_sits_next_to() {
        assert_eq!(
            file("/etc/pacman.conf.pacnew", PacFileKind::Pacnew, None).base(),
            "/etc/pacman.conf"
        );
        assert_eq!(
            file(
                "/etc/pacman.d/mirrorlist.pacsave",
                PacFileKind::Pacsave,
                None
            )
            .base(),
            "/etc/pacman.d/mirrorlist"
        );
        // A file whose name does not end in its own suffix is left alone
        // rather than truncated.
        assert_eq!(
            file("/etc/odd", PacFileKind::Pacnew, None).base(),
            "/etc/odd"
        );
    }

    #[test]
    fn review_order_is_newest_first_then_stable_by_path() {
        let files = vec![
            file("/etc/b.conf.pacnew", PacFileKind::Pacnew, Some(100)),
            file("/etc/a.conf.pacnew", PacFileKind::Pacnew, Some(300)),
            file("/etc/c.conf.pacnew", PacFileKind::Pacnew, Some(100)),
            file("/etc/d.conf.pacnew", PacFileKind::Pacnew, None),
        ];
        let paths: Vec<&str> = review_order(&files)
            .iter()
            .map(|f| f.path.as_str())
            .collect();
        assert_eq!(
            paths,
            vec![
                "/etc/a.conf.pacnew",
                "/etc/b.conf.pacnew",
                "/etc/c.conf.pacnew",
                "/etc/d.conf.pacnew"
            ]
        );
    }

    #[test]
    fn diff_program_follows_config_then_env_then_pacdiffs_own_default() {
        assert_eq!(diff_program("meld", Some("delta")), "meld");
        assert_eq!(diff_program("", Some("delta")), "delta");
        assert_eq!(diff_program("", None), "vimdiff");
        // Blank is not a choice — an empty knob means "unset", not "run ''".
        assert_eq!(diff_program("  ", Some("  ")), "vimdiff");
    }

    #[test]
    fn the_review_command_stays_text_and_runs_nothing_unattended() {
        let cmd = review_all_command("meld");
        assert_eq!(cmd, "sudo DIFFPROG=meld pacdiff");
        assert!(
            !cmd.contains("--noconfirm"),
            "nothing here may run unattended"
        );
    }
}
