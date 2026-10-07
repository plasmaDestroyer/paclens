//! Homebrew formulae (#17). brew installs and updates them under its own
//! prefix, as the user, so no step is privileged.
//!
//! Every call runs with `HOMEBREW_NO_AUTO_UPDATE=1`: a scan must not pull a
//! tap refresh as a side effect of looking. `brew upgrade` does its own.

use crate::model::{InstallReason, Package, PendingUpdate, SourceId};
use crate::providers::{CommandRunner, ProviderError};

pub const BREW_BIN: &str = "brew";

/// `brew list --versions`: `name version [version…]`; the last is current.
pub fn parse_list(stdout: &str) -> Vec<Package> {
    stdout
        .lines()
        .filter_map(|l| {
            let mut cols = l.split_whitespace();
            let name = cols.next()?;
            let version = cols.last()?;
            Some(package(name, version))
        })
        .collect()
}

/// `brew outdated --verbose`: `name (installed) < available`, maybe with a
/// `[pinned at …]` note. A pinned formula is not upgraded, so not pending.
pub fn parse_outdated(stdout: &str) -> Vec<PendingUpdate> {
    stdout
        .lines()
        .filter(|l| !l.contains("[pinned"))
        .filter_map(|l| {
            let (name, rest) = l.split_once(" (")?;
            let (current, rest) = rest.split_once(')')?;
            let available = rest.trim().strip_prefix('<')?.trim();
            Some(PendingUpdate {
                package_name: name.trim().to_string(),
                current_version: current.split(", ").last()?.to_string(),
                available_version: available.to_string(),
                source_id: SourceId::brew(),
            })
        })
        .collect()
}

fn package(name: &str, version: &str) -> Package {
    Package {
        name: name.to_string(),
        version: version.to_string(),
        source_id: SourceId::brew(),
        // brew records "on request" only through a second query; until a
        // screen needs it, the reason is not claimed.
        install_reason: InstallReason::Unknown,
        size_bytes: None,
        description: None,
        depends_on: Vec::new(),
        required_by: Vec::new(),
        optional_deps: Vec::new(),
        provides: Vec::new(),
        runtime: false,
        scope: None,
        foreign: false,
        signed: false,
        packager: None,
        repo_version: None,
    }
}

fn brew(runner: &dyn CommandRunner, args: &[&str]) -> Result<String, ProviderError> {
    let mut full = vec![
        "HOMEBREW_NO_AUTO_UPDATE=1",
        "HOMEBREW_NO_ENV_HINTS=1",
        BREW_BIN,
    ];
    full.extend_from_slice(args);
    let out = runner
        .run("env", &full)
        .map_err(|source| ProviderError::Exec {
            program: BREW_BIN.to_string(),
            source,
        })?;
    if out.exit_code != 0 {
        return Err(ProviderError::CommandFailed {
            program: format!("brew {}", args.join(" ")),
            exit_code: out.exit_code,
            stderr: out.stderr,
        });
    }
    Ok(out.stdout)
}

pub fn scan(
    runner: &dyn CommandRunner,
) -> Result<(Vec<Package>, Vec<PendingUpdate>), ProviderError> {
    let installed = parse_list(&brew(runner, &["list", "--versions"])?);
    let updates = parse_outdated(&brew(runner, &["outdated", "--verbose"])?);
    Ok((installed, updates))
}

/// `brew upgrade`: every outdated formula. Asks nothing.
pub fn update_command() -> Vec<String> {
    vec![BREW_BIN.to_string(), "upgrade".to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_takes_the_newest_version_column() {
        let p = parse_list("gcc 15.2.0_1\nxz 5.8.2 5.8.3\n");
        assert_eq!(p.len(), 2);
        assert_eq!((p[1].name.as_str(), p[1].version.as_str()), ("xz", "5.8.3"));
    }

    #[test]
    fn outdated_parses_and_skips_pinned() {
        let u = parse_outdated(
            "gcc (15.2.0_1) < 16.2.0\nxz (5.8.2, 5.8.3) < 5.8.4\nnode (22.1.0) < 23.0.0 [pinned at 22.1.0]\n",
        );
        assert_eq!(u.len(), 2);
        assert_eq!(u[0].package_name, "gcc");
        assert_eq!(u[0].current_version, "15.2.0_1");
        assert_eq!(u[1].current_version, "5.8.3");
        assert_eq!(u[1].available_version, "5.8.4");
    }

    #[test]
    fn scan_never_lets_brew_auto_update() {
        let runner = crate::providers::test_support::MockRunner::new()
            .with(
                "env HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_ENV_HINTS=1 brew list --versions",
                "gcc 15.2.0_1\n",
                0,
            )
            .with(
                "env HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_ENV_HINTS=1 brew outdated --verbose",
                "gcc (15.2.0_1) < 16.2.0\n",
                0,
            );
        let (p, u) = scan(&runner).expect("scans");
        assert_eq!((p.len(), u.len()), (1, 1));
    }
}
