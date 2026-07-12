//! The AUR side of libalpm (roadmap v0.3): foreign packages and their
//! updates via paru.
//!
//! AUR packages are installed through pacman, so the pacman provider already
//! scanned their full metadata — this module only *identifies* them
//! (`pacman -Qm`, which works without paru) and detects their updates
//! (`paru -Qua`). paru is optional: absent paru means the `aur` source shows
//! as not found and update detection is skipped, but the packages still
//! list under the `aur` source. Not a `Provider` impl on purpose —
//! source-specific logic, no generic shortcuts (P6).
//!
//! paru is **never run under sudo** — it builds as the user and self-elevates
//! for the install step (its own hard requirement).

use std::collections::HashSet;

use crate::model::{PendingUpdate, SourceId};
use crate::providers::{CommandRunner, ProviderError, pacman};

pub const PARU_BIN: &str = "paru";

/// The update step's argv: AUR packages only (`-Sua`); the pacman step owns
/// the repos. No `--noconfirm` — paru's prompts run in the pty console.
pub fn update_command() -> Vec<String> {
    vec![PARU_BIN.to_string(), "-Sua".to_string()]
}

/// Names of foreign (non-repo) packages, via `pacman -Qm`. Needs pacman, not
/// paru. `pacman -Qm` exits 1 with empty output on none — not an error.
pub fn foreign_names(runner: &dyn CommandRunner) -> Result<HashSet<String>, ProviderError> {
    let out = runner
        .run("pacman", &["-Qm"])
        .map_err(|source| ProviderError::Exec {
            program: "pacman".to_string(),
            source,
        })?;
    match out.exit_code {
        0 => Ok(out
            .stdout
            .lines()
            .filter_map(|l| l.split_whitespace().next())
            .map(|n| n.to_string())
            .collect()),
        1 if out.stdout.trim().is_empty() => Ok(HashSet::new()),
        code => Err(ProviderError::CommandFailed {
            program: "pacman -Qm".to_string(),
            exit_code: code,
            stderr: out.stderr,
        }),
    }
}

/// Pending AUR updates via `paru -Qua` — the same `name old -> new` line
/// format as `pacman -Qu`, and the same exit-1-means-none convention.
/// `devel` adds `--devel`: paru compares VCS (`-git`) packages against the
/// upstream HEAD instead of the version string (config `scan.aur_devel`;
/// slower, off by default).
pub fn scan_updates(
    runner: &dyn CommandRunner,
    devel: bool,
) -> Result<Vec<PendingUpdate>, ProviderError> {
    let mut args = vec!["-Qua"];
    if devel {
        args.push("--devel");
    }
    let out = runner
        .run(PARU_BIN, &args)
        .map_err(|source| ProviderError::Exec {
            program: PARU_BIN.to_string(),
            source,
        })?;
    match out.exit_code {
        0 => Ok(pacman::parse_updates_as(&out.stdout, SourceId::aur())),
        1 if out.stdout.trim().is_empty() => Ok(Vec::new()),
        code => Err(ProviderError::CommandFailed {
            program: format!("{PARU_BIN} -Qua"),
            exit_code: code,
            stderr: out.stderr,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::test_support::MockRunner;

    const QM: &str = include_str!("../../tests/fixtures/aur/qm.txt");
    const QUA: &str = include_str!("../../tests/fixtures/aur/qua.txt");

    #[test]
    fn foreign_names_reads_qm() {
        let runner = MockRunner::new().with("pacman -Qm", QM, 0);
        let names = foreign_names(&runner).unwrap();
        assert_eq!(names.len(), 6);
        assert!(names.contains("timr-bin"));
        assert!(names.contains("visual-studio-code-bin"));
    }

    #[test]
    fn no_foreign_packages_is_empty_not_error() {
        let runner = MockRunner::new().with("pacman -Qm", "", 1);
        assert!(foreign_names(&runner).unwrap().is_empty());
    }

    #[test]
    fn qua_updates_parse_with_the_aur_source() {
        let runner = MockRunner::new().with("paru -Qua", QUA, 0);
        let ups = scan_updates(&runner, false).unwrap();
        assert_eq!(ups.len(), 2);
        assert_eq!(ups[0].package_name, "timr-bin");
        assert_eq!(ups[0].current_version, "1.11.0-1");
        assert_eq!(ups[0].available_version, "1.12.0-1");
        assert_eq!(ups[0].source_id, SourceId::aur());
    }

    #[test]
    fn exit_one_with_no_output_means_no_updates() {
        let runner = MockRunner::new().with("paru -Qua", "", 1);
        assert!(scan_updates(&runner, false).unwrap().is_empty());
    }

    #[test]
    fn devel_flag_changes_the_command() {
        let runner = MockRunner::new().with("paru -Qua --devel", QUA, 0);
        assert_eq!(scan_updates(&runner, true).unwrap().len(), 2);
        // Without the mock for plain -Qua, the non-devel call must fail.
        assert!(scan_updates(&runner, false).is_err());
    }

    #[test]
    fn real_failure_is_an_error() {
        let runner = MockRunner::new().with("paru -Qua", "boom", 4);
        assert!(scan_updates(&runner, false).is_err());
    }

    #[test]
    fn update_command_is_aur_only_without_noconfirm() {
        let cmd = update_command();
        assert_eq!(cmd, vec!["paru", "-Sua"]);
        assert!(!cmd.iter().any(|a| a.contains("noconfirm")));
    }
}
