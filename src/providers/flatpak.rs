//! flatpak provider (design §10).
//!
//! Scans installed apps across both user and system scope in one call; each
//! package is tagged with its scoped source id from the `installation` column.
//! Columns are always requested explicitly — flatpak's default column order is
//! not stable across versions (design §10).

use crate::model::{FlatpakScope, InstallReason, Package, PendingUpdate, SourceId};

use super::{CommandRunner, Provider, ProviderError};

pub const FLATPAK_BIN: &str = "flatpak";

const LIST_COLUMNS: &str = "--columns=application,name,version,origin,installation,runtime,size";
const UPDATE_COLUMNS: &str = "--columns=application,version";

/// The argv for a scoped Flatpak update (design §10, §13.3). `--noninteractive`
/// suppresses Flatpak's own prompts; paclens gates on its own confirm first.
/// User scope needs no sudo; system scope does (added by the executor in v0.0.6).
/// Pure — building the command never runs anything.
pub fn update_command(scope: FlatpakScope) -> Vec<String> {
    let scope_flag = match scope {
        FlatpakScope::User => "--user",
        FlatpakScope::System => "--system",
    };
    vec![
        FLATPAK_BIN.to_string(),
        "update".to_string(),
        scope_flag.to_string(),
        "--noninteractive".to_string(),
    ]
}

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

    /// Apps *and* runtimes: `flatpak update` updates both, so both belong in
    /// the scan (the user's pending updates are often runtimes — GNOME
    /// Platform, GL drivers, themes). Runtimes can repeat rows (branches /
    /// arches share an ID); dedup on (name, version, source).
    fn scan_installed(&self) -> Result<Vec<Package>, ProviderError> {
        let apps = self.list(&["list", "--app", LIST_COLUMNS], false)?;
        let mut runtimes = self.list(&["list", "--runtime", LIST_COLUMNS], true)?;

        let mut packages = apps;
        packages.append(&mut runtimes);
        packages.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
        packages.dedup_by(|a, b| {
            a.name == b.name && a.version == b.version && a.source_id == b.source_id
        });
        Ok(packages)
    }

    /// No `--app` filter: runtime updates count too — they are what
    /// `flatpak update` will actually install.
    fn scan_updates(&self) -> Result<Vec<PendingUpdate>, ProviderError> {
        let out = self
            .runner
            .run(FLATPAK_BIN, &["remote-ls", "--updates", UPDATE_COLUMNS])
            .map_err(|source| ProviderError::Exec {
                program: FLATPAK_BIN.to_string(),
                source,
            })?;
        if out.exit_code != 0 {
            return Err(ProviderError::CommandFailed {
                program: format!("{FLATPAK_BIN} remote-ls --updates"),
                exit_code: out.exit_code,
                stderr: out.stderr,
            });
        }
        Ok(parse_updates(&out.stdout))
    }
}

impl FlatpakProvider<'_> {
    fn list(&self, args: &[&str], runtime: bool) -> Result<Vec<Package>, ProviderError> {
        let out = self
            .runner
            .run(FLATPAK_BIN, args)
            .map_err(|source| ProviderError::Exec {
                program: FLATPAK_BIN.to_string(),
                source,
            })?;
        if out.exit_code != 0 {
            return Err(ProviderError::CommandFailed {
                program: format!("{FLATPAK_BIN} {}", args.join(" ")),
                exit_code: out.exit_code,
                stderr: out.stderr,
            });
        }
        Ok(parse_list(&out.stdout, runtime))
    }
}

/// Map the `installation` column to a scoped source id.
fn scope_source_id(installation: &str) -> SourceId {
    match installation.trim() {
        "user" => SourceId::flatpak_user(),
        _ => SourceId::flatpak_system(),
    }
}

/// Parse `flatpak list --columns=application,name,version,origin,installation`
/// output (apps or runtimes). Tab-separated; `name` is the display name,
/// `application` is the app id.
fn parse_list(stdout: &str, runtime: bool) -> Vec<Package> {
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
            // Apps carry their runtime as "org.gnome.Platform/x86_64/50" —
            // the app's one real dependency (rendered as an Inferred edge,
            // design §8). Runtime rows leave the column empty.
            let runtime_dep = cols
                .next()
                .unwrap_or_default()
                .trim()
                .split('/')
                .next()
                .filter(|r| !r.is_empty())
                .map(|r| r.to_string());
            let size_bytes = parse_flatpak_size(cols.next().unwrap_or_default().trim());
            Some(Package {
                name: app_id.to_string(),
                version: version.to_string(),
                source_id: scope_source_id(installation),
                install_reason: InstallReason::Unknown,
                size_bytes,
                description: (!display_name.is_empty()).then(|| display_name.to_string()),
                depends_on: runtime_dep.into_iter().collect(),
                required_by: Vec::new(),
                optional_deps: Vec::new(),
                provides: Vec::new(),
                runtime,
            })
        })
        .collect()
}

/// Parse flatpak's human size column ("12.7 MB", "658.5 MB", "45 kB") into
/// bytes. Flatpak formats with g_format_size — decimal (1000-based) units.
fn parse_flatpak_size(text: &str) -> Option<u64> {
    let mut parts = text.split_whitespace();
    let number: f64 = parts.next()?.parse().ok()?;
    let unit = parts.next().unwrap_or("bytes");
    let factor: f64 = match unit {
        "bytes" | "byte" | "B" => 1.0,
        "kB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        _ => return None,
    };
    Some((number * factor) as u64)
}

/// Parse `flatpak remote-ls --updates --columns=application,version`.
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
        "flatpak list --app --columns=application,name,version,origin,installation,runtime,size";
    const RUNTIME_KEY: &str = "flatpak list --runtime --columns=application,name,version,origin,installation,runtime,size";
    const UPDATES_KEY: &str = "flatpak remote-ls --updates --columns=application,version";

    const LIST_FIXTURE: &str = include_str!("../../tests/fixtures/flatpak/list_apps.txt");
    const RUNTIME_FIXTURE: &str = include_str!("../../tests/fixtures/flatpak/list_runtimes.txt");
    const UPDATES_FIXTURE: &str =
        include_str!("../../tests/fixtures/flatpak/remote_ls_updates.txt");

    /// Runner with both list calls stubbed (runtimes empty unless overridden).
    fn runner_with_lists(apps: &str, runtimes: &str) -> MockRunner {
        MockRunner::new()
            .with(LIST_KEY, apps, 0)
            .with(RUNTIME_KEY, runtimes, 0)
    }

    #[test]
    fn parse_flatpak_size_reads_decimal_units() {
        assert_eq!(parse_flatpak_size("12.7 MB"), Some(12_700_000));
        assert_eq!(parse_flatpak_size("1.2 GB"), Some(1_200_000_000));
        assert_eq!(parse_flatpak_size("45 kB"), Some(45_000));
        assert_eq!(parse_flatpak_size("512 bytes"), Some(512));
        assert_eq!(parse_flatpak_size(""), None);
        assert_eq!(parse_flatpak_size("weird"), None);
    }

    #[test]
    fn installed_lists_carry_sizes() {
        let runner = runner_with_lists(LIST_FIXTURE, RUNTIME_FIXTURE);
        let provider = FlatpakProvider::new(&runner);
        let pkgs = provider.scan_installed().unwrap();
        let firefox = pkgs
            .iter()
            .find(|p| p.name == "org.mozilla.firefox")
            .unwrap();
        assert_eq!(firefox.size_bytes, Some(241_700_000));
        let gl = pkgs
            .iter()
            .find(|p| p.name == "org.freedesktop.Platform.GL.default")
            .unwrap();
        assert!(gl.runtime);
        assert_eq!(gl.size_bytes, Some(658_500_000));
    }

    #[test]
    fn parse_list_apps_fixture_has_expected_count() {
        let runner = runner_with_lists(LIST_FIXTURE, "");
        let provider = FlatpakProvider::new(&runner);
        let pkgs = provider.scan_installed().unwrap();
        assert_eq!(pkgs.len(), 3);
        assert!(pkgs.iter().all(|p| !p.runtime));
        assert!(pkgs.iter().any(|p| p.name == "org.mozilla.firefox"));
    }

    #[test]
    fn runtimes_are_scanned_flagged_and_deduped() {
        // Real fixture: 9 rows, org.freedesktop.Platform.GL.default repeats
        // with the same version (branch/arch duplicates) → deduped.
        let runner = runner_with_lists("", RUNTIME_FIXTURE);
        let provider = FlatpakProvider::new(&runner);
        let pkgs = provider.scan_installed().unwrap();
        assert!(pkgs.len() < 9, "dupes not collapsed: {}", pkgs.len());
        assert!(pkgs.iter().all(|p| p.runtime));
        assert!(
            pkgs.iter().any(|p| p.name == "org.gnome.Platform"),
            "missing gnome platform"
        );
        let gl: Vec<_> = pkgs
            .iter()
            .filter(|p| p.name == "org.freedesktop.Platform.GL.default")
            .collect();
        assert_eq!(gl.len(), 1, "GL.default should dedup to one");
    }

    #[test]
    fn apps_and_runtimes_merge_into_one_list() {
        let runner = runner_with_lists(LIST_FIXTURE, RUNTIME_FIXTURE);
        let provider = FlatpakProvider::new(&runner);
        let pkgs = provider.scan_installed().unwrap();
        assert!(pkgs.iter().any(|p| !p.runtime));
        assert!(pkgs.iter().any(|p| p.runtime));
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
        let runner = runner_with_lists(stdout, "");
        let provider = FlatpakProvider::new(&runner);
        let pkgs = provider.scan_installed().unwrap();
        assert_eq!(pkgs.len(), 2);
        // scan_installed name-sorts, so obsidian comes first.
        assert_eq!(pkgs[0].name, "md.obsidian.Obsidian");
        assert_eq!(pkgs[0].source_id, SourceId::flatpak_user());
        assert_eq!(pkgs[1].name, "org.mozilla.firefox");
        assert_eq!(pkgs[1].version, "128.0");
        assert_eq!(pkgs[1].description.as_deref(), Some("Firefox"));
        assert_eq!(pkgs[1].source_id, SourceId::flatpak_system());
    }

    #[test]
    fn parse_list_apps_handles_missing_version() {
        let stdout = "org.example.App\tExample\t\tflathub\tuser\n";
        let runner = runner_with_lists(stdout, "");
        let provider = FlatpakProvider::new(&runner);
        let pkgs = provider.scan_installed().unwrap();
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].version, "");
    }

    #[test]
    fn empty_list_is_ok_not_error() {
        let runner = runner_with_lists("", "");
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
        // Scope is unknown from remote-ls; the scanner reconciles it later.
        assert_eq!(ups[0].source_id, SourceId::flatpak());
    }

    #[test]
    fn scope_source_id_maps_installation_column() {
        assert_eq!(scope_source_id("user"), SourceId::flatpak_user());
        assert_eq!(scope_source_id("system"), SourceId::flatpak_system());
        // Anything unexpected defaults to system (conservative).
        assert_eq!(scope_source_id("default"), SourceId::flatpak_system());
        assert_eq!(scope_source_id(" user "), SourceId::flatpak_user());
    }

    #[test]
    fn update_command_is_scoped_and_noninteractive() {
        assert_eq!(
            update_command(FlatpakScope::User),
            vec!["flatpak", "update", "--user", "--noninteractive"]
        );
        assert_eq!(
            update_command(FlatpakScope::System),
            vec!["flatpak", "update", "--system", "--noninteractive"]
        );
    }
}
