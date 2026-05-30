//! flatpak provider (spec §5.3).
//!
//! Scans installed apps across both user and system scope in one call; each
//! package is tagged with its scoped source id from the `installation` column.
//! Columns are always requested explicitly — flatpak's default column order is
//! not stable across versions (dev-notes §2.2).

use crate::model::{InstallReason, Package, PendingUpdate, SourceId};

use super::{CommandRunner, Provider, ProviderError};

pub const FLATPAK_BIN: &str = "flatpak";

const LIST_COLUMNS: &str = "--columns=application,name,version,origin,installation";
const UPDATE_COLUMNS: &str = "--columns=application,version";

pub struct FlatpakProvider<'a> {
    runner: &'a dyn CommandRunner,
}

impl<'a> FlatpakProvider<'a> {
    pub fn new(runner: &'a dyn CommandRunner) -> Self {
        Self { runner }
    }
}

impl Provider for FlatpakProvider<'_> {
    fn is_available(&self) -> bool {
        super::binary_on_path(FLATPAK_BIN)
    }

    fn scan_installed(&self) -> Result<Vec<Package>, ProviderError> {
        let out = self
            .runner
            .run(FLATPAK_BIN, &["list", "--app", LIST_COLUMNS])
            .map_err(|source| ProviderError::Exec {
                program: FLATPAK_BIN.to_string(),
                source,
            })?;
        if out.exit_code != 0 {
            return Err(ProviderError::CommandFailed {
                program: format!("{FLATPAK_BIN} list --app"),
                exit_code: out.exit_code,
                stderr: out.stderr,
            });
        }
        Ok(parse_list_apps(&out.stdout))
    }

    fn scan_updates(&self) -> Result<Vec<PendingUpdate>, ProviderError> {
        let out = self
            .runner
            .run(
                FLATPAK_BIN,
                &["remote-ls", "--updates", "--app", UPDATE_COLUMNS],
            )
            .map_err(|source| ProviderError::Exec {
                program: FLATPAK_BIN.to_string(),
                source,
            })?;
        if out.exit_code != 0 {
            return Err(ProviderError::CommandFailed {
                program: format!("{FLATPAK_BIN} remote-ls --updates --app"),
                exit_code: out.exit_code,
                stderr: out.stderr,
            });
        }
        Ok(parse_updates(&out.stdout))
    }
}

/// Map the `installation` column to a scoped source id.
fn scope_source_id(installation: &str) -> SourceId {
    match installation.trim() {
        "user" => SourceId::flatpak_user(),
        _ => SourceId::flatpak_system(),
    }
}

/// Parse `flatpak list --app --columns=application,name,version,origin,installation`.
/// Tab-separated; `name` is the display name, `application` is the app id.
fn parse_list_apps(stdout: &str) -> Vec<Package> {
    stdout
        .lines()
        .filter_map(|line| {
            if line.trim().is_empty() {
                return None;
            }
            let mut cols = line.split('\t');
            let app_id = cols.next()?.trim();
            if app_id.is_empty() {
                return None;
            }
            let display_name = cols.next().unwrap_or_default().trim();
            let version = cols.next().unwrap_or_default().trim();
            let _origin = cols.next().unwrap_or_default().trim();
            let installation = cols.next().unwrap_or_default();
            Some(Package {
                name: app_id.to_string(),
                version: version.to_string(),
                source_id: scope_source_id(installation),
                install_reason: InstallReason::Unknown,
                size_bytes: None,
                description: (!display_name.is_empty()).then(|| display_name.to_string()),
                depends_on: Vec::new(),
                required_by: Vec::new(),
                optional_deps: Vec::new(),
                provides: Vec::new(),
            })
        })
        .collect()
}

/// Parse `flatpak remote-ls --updates --app --columns=application,version`.
/// The current version and scope are unknown from this command; the scanner
/// reconciles them against the installed list.
fn parse_updates(stdout: &str) -> Vec<PendingUpdate> {
    stdout
        .lines()
        .filter_map(|line| {
            if line.trim().is_empty() {
                return None;
            }
            let mut cols = line.split('\t');
            let app_id = cols.next()?.trim();
            if app_id.is_empty() {
                return None;
            }
            let available = cols.next().unwrap_or_default().trim();
            Some(PendingUpdate {
                package_name: app_id.to_string(),
                current_version: String::new(),
                available_version: available.to_string(),
                source_id: SourceId::flatpak(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::test_support::MockRunner;

    const LIST_KEY: &str =
        "flatpak list --app --columns=application,name,version,origin,installation";
    const UPDATES_KEY: &str = "flatpak remote-ls --updates --app --columns=application,version";

    const LIST_FIXTURE: &str = include_str!("../../tests/fixtures/flatpak/list_apps.txt");
    const UPDATES_FIXTURE: &str =
        include_str!("../../tests/fixtures/flatpak/remote_ls_updates.txt");

    #[test]
    fn parse_list_apps_fixture_has_expected_count() {
        let runner = MockRunner::new().with(LIST_KEY, LIST_FIXTURE, 0);
        let provider = FlatpakProvider::new(&runner);
        let pkgs = provider.scan_installed().unwrap();
        assert_eq!(pkgs.len(), 3);
        assert_eq!(pkgs[0].name, "org.mozilla.firefox");
    }

    #[test]
    fn parse_updates_fixture_has_expected_count() {
        let runner = MockRunner::new().with(UPDATES_KEY, UPDATES_FIXTURE, 0);
        let provider = FlatpakProvider::new(&runner);
        assert_eq!(provider.scan_updates().unwrap().len(), 2);
    }

    #[test]
    fn parse_list_apps_reads_columns_and_scope() {
        let stdout = "org.mozilla.firefox\tFirefox\t128.0\tflathub\tsystem\n\
                      md.obsidian.Obsidian\tObsidian\t1.6.0\tflathub\tuser\n";
        let runner = MockRunner::new().with(LIST_KEY, stdout, 0);
        let provider = FlatpakProvider::new(&runner);
        let pkgs = provider.scan_installed().unwrap();
        assert_eq!(pkgs.len(), 2);
        assert_eq!(pkgs[0].name, "org.mozilla.firefox");
        assert_eq!(pkgs[0].version, "128.0");
        assert_eq!(pkgs[0].description.as_deref(), Some("Firefox"));
        assert_eq!(pkgs[0].source_id, SourceId::flatpak_system());
        assert_eq!(pkgs[1].source_id, SourceId::flatpak_user());
    }

    #[test]
    fn parse_list_apps_handles_missing_version() {
        let stdout = "org.example.App\tExample\t\tflathub\tuser\n";
        let runner = MockRunner::new().with(LIST_KEY, stdout, 0);
        let provider = FlatpakProvider::new(&runner);
        let pkgs = provider.scan_installed().unwrap();
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].version, "");
    }

    #[test]
    fn empty_list_is_ok_not_error() {
        let runner = MockRunner::new().with(LIST_KEY, "", 0);
        let provider = FlatpakProvider::new(&runner);
        assert_eq!(provider.scan_installed().unwrap().len(), 0);
    }

    #[test]
    fn parse_updates_reads_app_and_version() {
        let stdout = "org.mozilla.firefox\t129.0\n";
        let runner = MockRunner::new().with(UPDATES_KEY, stdout, 0);
        let provider = FlatpakProvider::new(&runner);
        let ups = provider.scan_updates().unwrap();
        assert_eq!(ups.len(), 1);
        assert_eq!(ups[0].package_name, "org.mozilla.firefox");
        assert_eq!(ups[0].available_version, "129.0");
    }
}
