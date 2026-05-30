//! pacman provider (spec §5.2).
//!
//! v0.0.2 reads the quick installed list (`pacman -Q`) and the update list
//! (`pacman -Qu`). The full-metadata parser (`pacman -Qi`) arrives in v0.0.3.

use crate::model::{InstallReason, Package, PendingUpdate, SourceId};

use super::{CommandRunner, Provider, ProviderError};

pub const PACMAN_BIN: &str = "pacman";

pub struct PacmanProvider<'a> {
    runner: &'a dyn CommandRunner,
}

impl<'a> PacmanProvider<'a> {
    pub fn new(runner: &'a dyn CommandRunner) -> Self {
        Self { runner }
    }
}

impl Provider for PacmanProvider<'_> {
    fn is_available(&self) -> bool {
        super::binary_on_path(PACMAN_BIN)
    }

    fn scan_installed(&self) -> Result<Vec<Package>, ProviderError> {
        let out = self
            .runner
            .run(PACMAN_BIN, &["-Q"])
            .map_err(|source| ProviderError::Exec {
                program: PACMAN_BIN.to_string(),
                source,
            })?;
        if out.exit_code != 0 {
            return Err(ProviderError::CommandFailed {
                program: format!("{PACMAN_BIN} -Q"),
                exit_code: out.exit_code,
                stderr: out.stderr,
            });
        }
        Ok(parse_query(&out.stdout))
    }

    fn scan_updates(&self) -> Result<Vec<PendingUpdate>, ProviderError> {
        let out = self
            .runner
            .run(PACMAN_BIN, &["-Qu"])
            .map_err(|source| ProviderError::Exec {
                program: PACMAN_BIN.to_string(),
                source,
            })?;
        // `pacman -Qu` exits 1 (with empty stdout) when there are no updates —
        // that is not an error.
        match out.exit_code {
            0 => Ok(parse_updates(&out.stdout)),
            1 if out.stdout.trim().is_empty() => Ok(Vec::new()),
            code => Err(ProviderError::CommandFailed {
                program: format!("{PACMAN_BIN} -Qu"),
                exit_code: code,
                stderr: out.stderr,
            }),
        }
    }
}

/// Parse `pacman -Q`: one `name version` per line.
fn parse_query(stdout: &str) -> Vec<Package> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let name = parts.next()?.to_string();
            let version = parts.next().unwrap_or_default().to_string();
            Some(Package {
                name,
                version,
                source_id: SourceId::pacman(),
                install_reason: InstallReason::Unknown,
                size_bytes: None,
                description: None,
                depends_on: Vec::new(),
                required_by: Vec::new(),
                optional_deps: Vec::new(),
                provides: Vec::new(),
            })
        })
        .collect()
}

/// Parse `pacman -Qu`: one `name current -> available` per line.
fn parse_updates(stdout: &str) -> Vec<PendingUpdate> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let name = parts.next()?.to_string();
            let current = parts.next()?.to_string();
            if parts.next()? != "->" {
                return None;
            }
            let available = parts.next()?.to_string();
            Some(PendingUpdate {
                package_name: name,
                current_version: current,
                available_version: available,
                source_id: SourceId::pacman(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::test_support::MockRunner;

    const Q_SAMPLE: &str = include_str!("../../tests/fixtures/pacman/q_sample.txt");
    const QU_SAMPLE: &str = include_str!("../../tests/fixtures/pacman/qu_sample.txt");
    const QU_EMPTY: &str = include_str!("../../tests/fixtures/pacman/qu_empty.txt");

    #[test]
    fn parse_query_fixture_has_expected_count() {
        let runner = MockRunner::new().with("pacman -Q", Q_SAMPLE, 0);
        let provider = PacmanProvider::new(&runner);
        let pkgs = provider.scan_installed().unwrap();
        assert_eq!(pkgs.len(), 20);
        assert_eq!(pkgs[0].name, "7zip");
        assert_eq!(pkgs[0].version, "26.01-1.1");
    }

    #[test]
    fn parse_updates_fixture_has_expected_count() {
        let runner = MockRunner::new().with("pacman -Qu", QU_SAMPLE, 0);
        let provider = PacmanProvider::new(&runner);
        assert_eq!(provider.scan_updates().unwrap().len(), 4);
    }

    #[test]
    fn empty_update_fixture_yields_none() {
        let runner = MockRunner::new().with("pacman -Qu", QU_EMPTY, 0);
        let provider = PacmanProvider::new(&runner);
        assert_eq!(provider.scan_updates().unwrap().len(), 0);
    }

    #[test]
    fn parse_query_reads_name_and_version() {
        let runner = MockRunner::new().with("pacman -Q", "firefox 128.0-1\nglibc 2.40-1\n", 0);
        let provider = PacmanProvider::new(&runner);
        let pkgs = provider.scan_installed().unwrap();
        assert_eq!(pkgs.len(), 2);
        assert_eq!(pkgs[0].name, "firefox");
        assert_eq!(pkgs[0].version, "128.0-1");
        assert_eq!(pkgs[0].source_id, SourceId::pacman());
        assert_eq!(pkgs[1].name, "glibc");
    }

    #[test]
    fn parse_query_skips_blank_lines() {
        let runner = MockRunner::new().with("pacman -Q", "firefox 128.0-1\n\n\n", 0);
        let provider = PacmanProvider::new(&runner);
        assert_eq!(provider.scan_installed().unwrap().len(), 1);
    }

    #[test]
    fn scan_installed_nonzero_exit_is_error() {
        let runner = MockRunner::new().with("pacman -Q", "", 1);
        let provider = PacmanProvider::new(&runner);
        assert!(provider.scan_installed().is_err());
    }

    #[test]
    fn parse_updates_reads_versions() {
        let runner = MockRunner::new().with(
            "pacman -Qu",
            "firefox 128.0-1 -> 129.0-1\nlinux 6.9.1-1 -> 6.9.2-1\n",
            0,
        );
        let provider = PacmanProvider::new(&runner);
        let ups = provider.scan_updates().unwrap();
        assert_eq!(ups.len(), 2);
        assert_eq!(ups[0].package_name, "firefox");
        assert_eq!(ups[0].current_version, "128.0-1");
        assert_eq!(ups[0].available_version, "129.0-1");
    }

    #[test]
    fn no_updates_exit_one_is_empty_not_error() {
        let runner = MockRunner::new().with("pacman -Qu", "", 1);
        let provider = PacmanProvider::new(&runner);
        assert_eq!(provider.scan_updates().unwrap().len(), 0);
    }

    #[test]
    fn parse_updates_ignores_malformed_line() {
        let runner = MockRunner::new().with("pacman -Qu", "garbage line without arrow\n", 0);
        let provider = PacmanProvider::new(&runner);
        assert_eq!(provider.scan_updates().unwrap().len(), 0);
    }
}
