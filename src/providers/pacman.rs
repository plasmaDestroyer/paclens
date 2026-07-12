//! pacman provider (spec §5.2).
//!
//! `scan_installed` parses full metadata from a single `pacman -Qi` call (the
//! source of the dependency graph, dev-notes §2.3). `scan_updates` prefers
//! `checkupdates` (pacman-contrib) — it syncs a temp DB copy, so pending
//! updates are accurate even when the local sync DB is stale, which is exactly
//! where `pacman -Qu` silently reports zero. Without pacman-contrib it falls
//! back to `-Qu` and the scanner marks the source's counts as possibly stale.

use std::collections::HashMap;

use crate::model::{InstallReason, Package, PendingUpdate, SourceId};

use super::{CommandRunner, Provider, ProviderError};

pub const PACMAN_BIN: &str = "pacman";
pub const CHECKUPDATES_BIN: &str = "checkupdates";

/// The argv for a full pacman system update (no `--noconfirm`, per spec Q6 /
/// dev-notes decisions: it suppresses conflict resolution). Pure — building the
/// command never runs anything. The privilege prefix (sudo/doas/pkexec) is added
/// by the executor in v0.0.6; pacman updates require it.
pub fn update_command() -> Vec<String> {
    vec![PACMAN_BIN.to_string(), "-Syu".to_string()]
}

pub struct PacmanProvider<'a> {
    runner: &'a dyn CommandRunner,
    /// checkupdates (pacman-contrib) found on PATH at construction.
    checkupdates: bool,
}

impl<'a> PacmanProvider<'a> {
    pub fn new(runner: &'a dyn CommandRunner) -> Self {
        Self::with_checkupdates(runner, super::binary_on_path(CHECKUPDATES_BIN))
    }

    /// The hermetic constructor: checkupdates availability injected.
    pub fn with_checkupdates(runner: &'a dyn CommandRunner, checkupdates: bool) -> Self {
        Self {
            runner,
            checkupdates,
        }
    }

    /// True when update counts come from checkupdates (temp-DB sync) rather
    /// than the possibly-stale local sync DB. The scanner passes availability
    /// straight through; only tests read this back.
    #[cfg_attr(not(test), expect(dead_code))]
    pub fn updates_accurate(&self) -> bool {
        self.checkupdates
    }
}

impl Provider for PacmanProvider<'_> {
    fn is_available(&self) -> bool {
        super::binary_on_path(PACMAN_BIN)
    }

    fn scan_installed(&self) -> Result<Vec<Package>, ProviderError> {
        let out = self
            .runner
            .run(PACMAN_BIN, &["-Qi"])
            .map_err(|source| ProviderError::Exec {
                program: PACMAN_BIN.to_string(),
                source,
            })?;
        if out.exit_code != 0 {
            return Err(ProviderError::CommandFailed {
                program: format!("{PACMAN_BIN} -Qi"),
                exit_code: out.exit_code,
                stderr: out.stderr,
            });
        }
        Ok(parse_qi(&out.stdout))
    }

    fn scan_updates(&self) -> Result<Vec<PendingUpdate>, ProviderError> {
        if self.checkupdates {
            return self.scan_updates_checkupdates();
        }
        tracing::warn!("pacman-contrib not installed; falling back to pacman -Qu (may be stale)");
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

impl PacmanProvider<'_> {
    /// `checkupdates --nocolor`: same `name old -> new` line format as `-Qu`.
    /// Exit 2 is its documented "no updates" code, not an error.
    fn scan_updates_checkupdates(&self) -> Result<Vec<PendingUpdate>, ProviderError> {
        let out = self
            .runner
            .run(CHECKUPDATES_BIN, &["--nocolor"])
            .map_err(|source| ProviderError::Exec {
                program: CHECKUPDATES_BIN.to_string(),
                source,
            })?;
        match out.exit_code {
            0 => Ok(parse_updates(&out.stdout)),
            2 => Ok(Vec::new()),
            code => Err(ProviderError::CommandFailed {
                program: format!("{CHECKUPDATES_BIN} --nocolor"),
                exit_code: code,
                stderr: out.stderr,
            }),
        }
    }
}

/// Parse `pacman -Qi`: blank-line-separated records of `Key : Value` pairs.
fn parse_qi(stdout: &str) -> Vec<Package> {
    let mut packages = Vec::new();
    let mut record: Vec<&str> = Vec::new();
    for line in stdout.lines() {
        if line.trim().is_empty() {
            if let Some(pkg) = parse_record(&record) {
                packages.push(pkg);
            }
            record.clear();
        } else {
            record.push(line);
        }
    }
    if let Some(pkg) = parse_record(&record) {
        packages.push(pkg);
    }
    packages
}

/// Parse one record into a `Package`. Returns `None` if it has no `Name`.
///
/// Field lines start in column 0 as `Key : Value`; a line beginning with
/// whitespace is a continuation of the previous field's value (dev-notes §2.1).
fn parse_record(lines: &[&str]) -> Option<Package> {
    let mut fields: HashMap<String, Vec<String>> = HashMap::new();
    let mut current: Option<String> = None;
    for &line in lines {
        if line.starts_with(|c: char| c.is_whitespace()) {
            if let Some(key) = &current {
                fields
                    .entry(key.clone())
                    .or_default()
                    .push(line.trim().to_string());
            }
        } else if let Some(idx) = line.find(':') {
            let key = line[..idx].trim().to_string();
            let value = line[idx + 1..].trim().to_string();
            fields.entry(key.clone()).or_default().push(value);
            current = Some(key);
        }
    }

    let name = fields.get("Name")?.first()?.clone();
    if name.is_empty() {
        return None;
    }
    let version = first(&fields, "Version").unwrap_or_default();
    let description = fields
        .get("Description")
        .map(|v| v.join(" "))
        .filter(|d| !d.is_empty() && d != "None");

    Some(Package {
        name,
        version,
        source_id: SourceId::pacman(),
        install_reason: first(&fields, "Install Reason")
            .map(|r| parse_reason(&r))
            .unwrap_or(InstallReason::Unknown),
        size_bytes: first(&fields, "Installed Size").and_then(|s| parse_size(&s)),
        description,
        depends_on: parse_pkg_list(fields.get("Depends On")),
        required_by: parse_pkg_list(fields.get("Required By")),
        optional_deps: parse_optional_deps(fields.get("Optional Deps")),
        provides: parse_pkg_list(fields.get("Provides")),
        runtime: false,
    })
}

fn first(fields: &HashMap<String, Vec<String>>, key: &str) -> Option<String> {
    fields.get(key).and_then(|v| v.first()).cloned()
}

fn parse_reason(raw: &str) -> InstallReason {
    if raw.starts_with("Explicitly") {
        InstallReason::Explicit
    } else if raw.contains("dependency") {
        InstallReason::Dependency
    } else {
        InstallReason::Unknown
    }
}

/// Strip a version constraint (`glibc>=2.38`, `sh=5.2`, `libfoo.so=1-64`) down
/// to the bare package name.
fn strip_constraint(token: &str) -> &str {
    let end = token.find(['<', '>', '=']).unwrap_or(token.len());
    &token[..end]
}

/// Parse a space-separated package-name field (`Depends On`, `Required By`,
/// `Provides`), dropping the `None` sentinel and version constraints.
fn parse_pkg_list(field: Option<&Vec<String>>) -> Vec<String> {
    let Some(lines) = field else {
        return Vec::new();
    };
    lines
        .iter()
        .flat_map(|line| line.split_whitespace())
        .filter(|tok| *tok != "None")
        .map(|tok| strip_constraint(tok).to_string())
        .collect()
}

/// Parse `Optional Deps`: one `name: description [installed]` per line. We keep
/// only the package name; the `None` sentinel yields an empty list.
fn parse_optional_deps(field: Option<&Vec<String>>) -> Vec<String> {
    let Some(lines) = field else {
        return Vec::new();
    };
    lines
        .iter()
        .filter(|line| line.as_str() != "None")
        .filter_map(|line| {
            let name = line.split(':').next()?.trim();
            (!name.is_empty()).then(|| name.to_string())
        })
        .collect()
}

/// Parse a human-readable `Installed Size` ("284.72 MiB") into bytes.
fn parse_size(raw: &str) -> Option<u64> {
    let mut parts = raw.split_whitespace();
    let value: f64 = parts.next()?.parse().ok()?;
    let multiplier = match parts.next().unwrap_or("B") {
        "B" => 1.0,
        "KiB" => 1024.0,
        "MiB" => 1024f64.powi(2),
        "GiB" => 1024f64.powi(3),
        "TiB" => 1024f64.powi(4),
        _ => return None,
    };
    Some((value * multiplier).round() as u64)
}

/// Parse the `name current -> available` line format shared by
/// `pacman -Qu`, `checkupdates` and `paru -Qua`.
pub(crate) fn parse_updates_as(stdout: &str, source: SourceId) -> Vec<PendingUpdate> {
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
                source_id: source.clone(),
            })
        })
        .collect()
}

fn parse_updates(stdout: &str) -> Vec<PendingUpdate> {
    parse_updates_as(stdout, SourceId::pacman())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::test_support::MockRunner;

    const QI_FIREFOX: &str = include_str!("../../tests/fixtures/pacman/qi_firefox.txt");
    const QI_DEP: &str = include_str!("../../tests/fixtures/pacman/qi_dep_package.txt");
    const QI_NONE: &str = include_str!("../../tests/fixtures/pacman/qi_none_fields.txt");
    const QI_SMALL: &str = include_str!("../../tests/fixtures/pacman/qi_small_system.txt");
    const QI_EDGE: &str = include_str!("../../tests/fixtures/pacman/qi_edge_cases.txt");
    const QU_SAMPLE: &str = include_str!("../../tests/fixtures/pacman/qu_sample.txt");
    const QU_EMPTY: &str = include_str!("../../tests/fixtures/pacman/qu_empty.txt");

    fn scan(fixture: &str) -> Vec<Package> {
        let runner = MockRunner::new().with("pacman -Qi", fixture, 0);
        PacmanProvider::new(&runner).scan_installed().unwrap()
    }

    fn find<'a>(pkgs: &'a [Package], name: &str) -> &'a Package {
        pkgs.iter()
            .find(|p| p.name == name)
            .expect("package present")
    }

    #[test]
    fn firefox_is_explicit_with_multiline_optional_deps() {
        let pkgs = scan(QI_FIREFOX);
        assert_eq!(pkgs.len(), 1);
        let firefox = &pkgs[0];
        assert_eq!(firefox.name, "firefox");
        assert_eq!(firefox.install_reason, InstallReason::Explicit);
        assert!(firefox.provides.is_empty(), "Provides was None");
        assert!(firefox.required_by.is_empty(), "Required By was None");
        assert_eq!(firefox.optional_deps.len(), 6);
        assert_eq!(firefox.optional_deps[0], "hunspell-en_US");
        assert!(firefox.depends_on.contains(&"glibc".to_string()));
    }

    #[test]
    fn bash_is_dependency_with_provides_and_stripped_constraints() {
        let bash = scan(QI_DEP).into_iter().find(|p| p.name == "bash").unwrap();
        assert_eq!(bash.install_reason, InstallReason::Dependency);
        assert_eq!(bash.provides, vec!["sh".to_string()]);
        assert!(bash.depends_on.contains(&"readline".to_string()));
        // `libreadline.so=8-64` must lose its version constraint.
        assert!(bash.depends_on.contains(&"libreadline.so".to_string()));
        assert_eq!(bash.optional_deps, vec!["bash-completion".to_string()]);
        assert!(!bash.required_by.is_empty());
    }

    #[test]
    fn none_fields_become_empty_vecs() {
        let pkgs = scan(QI_NONE);
        let leaf = find(&pkgs, "adwaita-cursors");
        assert!(leaf.depends_on.is_empty());
        assert!(leaf.provides.is_empty());
        assert!(leaf.optional_deps.is_empty());
        assert_eq!(leaf.required_by, vec!["adwaita-icon-theme".to_string()]);
    }

    #[test]
    fn small_system_parses_every_record() {
        let pkgs = scan(QI_SMALL);
        assert_eq!(pkgs.len(), 3);
    }

    #[test]
    fn edge_cases_cover_constraints_multiline_desc_and_sizes() {
        let pkgs = scan(QI_EDGE);
        assert_eq!(pkgs.len(), 2);

        let libfoo = find(&pkgs, "libfoo");
        assert_eq!(
            libfoo.provides,
            vec!["libfoo.so".to_string(), "foo-compat".to_string()]
        );
        assert_eq!(
            libfoo.depends_on,
            vec!["glibc".to_string(), "gcc-libs".to_string()]
        );
        assert!(
            libfoo
                .description
                .as_deref()
                .unwrap()
                .contains("second line"),
            "multiline description should be joined"
        );
        assert_eq!(libfoo.size_bytes, Some(956 * 1024));

        let bigpkg = find(&pkgs, "bigpkg");
        assert_eq!(bigpkg.install_reason, InstallReason::Explicit);
        assert_eq!(
            bigpkg.optional_deps,
            vec!["optdep-one".to_string(), "optdep-two".to_string()]
        );
        assert_eq!(
            bigpkg.size_bytes,
            Some((1.23 * 1024f64.powi(3)).round() as u64)
        );
    }

    #[test]
    fn scan_installed_nonzero_exit_is_error() {
        let runner = MockRunner::new().with("pacman -Qi", "", 1);
        assert!(PacmanProvider::new(&runner).scan_installed().is_err());
    }

    /// The `pacman -Qu` fallback path: checkupdates injected as absent.
    fn qu_provider(runner: &MockRunner) -> PacmanProvider<'_> {
        PacmanProvider::with_checkupdates(runner, false)
    }

    #[test]
    fn parse_updates_fixture_has_expected_count() {
        let runner = MockRunner::new().with("pacman -Qu", QU_SAMPLE, 0);
        let ups = qu_provider(&runner).scan_updates().unwrap();
        assert_eq!(ups.len(), 4);
        assert_eq!(ups[0].package_name, "firefox");
        assert_eq!(ups[0].current_version, "128.0-1");
        assert_eq!(ups[0].available_version, "129.0-1");
    }

    #[test]
    fn empty_update_fixture_yields_none() {
        let runner = MockRunner::new().with("pacman -Qu", QU_EMPTY, 0);
        assert_eq!(qu_provider(&runner).scan_updates().unwrap().len(), 0);
    }

    #[test]
    fn no_updates_exit_one_is_empty_not_error() {
        let runner = MockRunner::new().with("pacman -Qu", "", 1);
        assert_eq!(qu_provider(&runner).scan_updates().unwrap().len(), 0);
    }

    #[test]
    fn parse_updates_ignores_malformed_line() {
        let runner = MockRunner::new().with("pacman -Qu", "garbage line without arrow\n", 0);
        assert_eq!(qu_provider(&runner).scan_updates().unwrap().len(), 0);
    }

    // --- checkupdates path ---
    const CHECKUPDATES_FIXTURE: &str =
        include_str!("../../tests/fixtures/pacman/checkupdates_sample.txt");

    #[test]
    fn checkupdates_is_preferred_and_parses_the_same_format() {
        let runner = MockRunner::new().with("checkupdates --nocolor", CHECKUPDATES_FIXTURE, 0);
        let provider = PacmanProvider::with_checkupdates(&runner, true);
        assert!(provider.updates_accurate());
        let ups = provider.scan_updates().unwrap();
        assert_eq!(ups.len(), 6);
        assert_eq!(ups[0].package_name, "accountsservice");
        assert_eq!(ups[0].current_version, "26.26.9-1.1");
        assert_eq!(ups[0].available_version, "26.27.3-1.1");
    }

    #[test]
    fn checkupdates_exit_two_means_no_updates() {
        let runner = MockRunner::new().with("checkupdates --nocolor", "", 2);
        let provider = PacmanProvider::with_checkupdates(&runner, true);
        assert_eq!(provider.scan_updates().unwrap().len(), 0);
    }

    #[test]
    fn checkupdates_other_failures_are_errors() {
        let runner = MockRunner::new().with("checkupdates --nocolor", "", 1);
        let provider = PacmanProvider::with_checkupdates(&runner, true);
        assert!(provider.scan_updates().is_err());
    }

    #[test]
    fn fallback_provider_reports_inaccurate_updates() {
        let runner = MockRunner::new();
        assert!(!qu_provider(&runner).updates_accurate());
    }

    // --- pure helper tests (small + specific) ---

    #[test]
    fn strip_constraint_handles_every_operator() {
        assert_eq!(strip_constraint("glibc"), "glibc");
        assert_eq!(strip_constraint("glibc>=2.38"), "glibc");
        assert_eq!(strip_constraint("sh=5.2"), "sh");
        assert_eq!(strip_constraint("libfoo.so=1-64"), "libfoo.so");
        assert_eq!(strip_constraint("python<4"), "python");
    }

    #[test]
    fn parse_reason_maps_known_strings() {
        assert_eq!(
            parse_reason("Explicitly installed"),
            InstallReason::Explicit
        );
        assert_eq!(
            parse_reason("Installed as a dependency for another package"),
            InstallReason::Dependency
        );
        assert_eq!(parse_reason("something else"), InstallReason::Unknown);
    }

    #[test]
    fn parse_size_handles_each_unit() {
        assert_eq!(parse_size("512.00 B"), Some(512));
        assert_eq!(parse_size("956.00 KiB"), Some(956 * 1024));
        assert_eq!(parse_size("12.00 MiB"), Some(12 * 1024 * 1024));
        assert_eq!(parse_size("1.00 GiB"), Some(1024u64.pow(3)));
    }

    #[test]
    fn parse_size_rejects_garbage() {
        assert_eq!(parse_size("not-a-number MiB"), None);
        assert_eq!(parse_size("12.0 Petabytes"), None);
        assert_eq!(parse_size(""), None);
    }

    #[test]
    fn parse_pkg_list_drops_none_and_strips_constraints() {
        let field = vec!["readline  libreadline.so=8-64  glibc".to_string()];
        assert_eq!(
            parse_pkg_list(Some(&field)),
            vec!["readline", "libreadline.so", "glibc"]
        );
        assert_eq!(
            parse_pkg_list(Some(&vec!["None".to_string()])),
            Vec::<String>::new()
        );
        assert_eq!(parse_pkg_list(None), Vec::<String>::new());
    }

    #[test]
    fn parse_optional_deps_keeps_only_names() {
        let field = vec![
            "hunspell-en_US: Spell checking, American English".to_string(),
            "libnotify: Notification integration [installed]".to_string(),
        ];
        assert_eq!(
            parse_optional_deps(Some(&field)),
            vec!["hunspell-en_US", "libnotify"]
        );
        assert_eq!(
            parse_optional_deps(Some(&vec!["None".to_string()])),
            Vec::<String>::new()
        );
    }

    #[test]
    fn update_command_is_full_system_upgrade_without_noconfirm() {
        assert_eq!(update_command(), vec!["pacman", "-Syu"]);
        assert!(!update_command().iter().any(|a| a == "--noconfirm"));
    }
}
