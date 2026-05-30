//! Scan orchestration and the scan cache.
//!
//! Detects available providers, runs them, assembles a `ScanResult`, and
//! persists it to the cache ([`cache`]). Never analyzes data (dev-notes §3).
//!
//! Providers run sequentially; concurrent scanning (spec Q5) is a deferred
//! optimization, not required by any milestone.

pub mod cache;

use std::path::Path;

use chrono::Utc;

use crate::config::Config;
use crate::model::{
    CacheSizes, FlatpakScope, Package, PendingUpdate, SCHEMA_VERSION, ScanResult, Source, SourceId,
    SourceKind,
};
use crate::providers::flatpak::FlatpakProvider;
use crate::providers::pacman::PacmanProvider;
use crate::providers::{CommandRunner, Provider};

/// pacman's package cache; its size is reported under cleanup advisories.
const PACMAN_CACHE_DIR: &str = "/var/cache/pacman/pkg/";

/// Return a usable `ScanResult`: a fresh cache hit when possible, otherwise a
/// new scan that is then written back to the cache.
///
/// `refresh` forces a re-scan. A failed cache write is logged but non-fatal —
/// the in-memory result is still returned (spec §15 recovery table).
pub fn load_or_scan(
    runner: &dyn CommandRunner,
    config: &Config,
    refresh: bool,
    config_path: Option<&Path>,
) -> anyhow::Result<ScanResult> {
    let cache = cache::Cache::locate()?;
    if refresh {
        tracing::info!("--refresh: ignoring cache");
    } else if let Some(scan) = cache.read()? {
        match cache::staleness(&scan, cache.path(), config, config_path) {
            None => {
                tracing::info!("using cached scan");
                return Ok(scan);
            }
            Some(reason) => tracing::info!(reason, "cache stale; re-scanning"),
        }
    }

    let scan = scan(runner, config);
    if let Err(err) = cache.write(&scan) {
        tracing::error!(error = %err, "failed to write scan cache; continuing in-memory");
    }
    Ok(scan)
}

/// Run every enabled, available provider and assemble a `ScanResult`.
///
/// Detects provider availability on PATH, then delegates to [`assemble`].
pub fn scan(runner: &dyn CommandRunner, config: &Config) -> ScanResult {
    let pacman_available = PacmanProvider::new(runner).is_available();
    let flatpak_available = FlatpakProvider::new(runner).is_available();
    assemble(runner, config, pacman_available, flatpak_available)
}

/// Assemble a `ScanResult` from the providers, given which binaries are
/// available. Availability is passed in (not probed) so the whole pipeline is
/// hermetically testable with a mock runner.
///
/// Provider failures are isolated: a source that errors is logged and skipped,
/// never aborting the others (dev-notes §3).
fn assemble(
    runner: &dyn CommandRunner,
    config: &Config,
    pacman_available: bool,
    flatpak_available: bool,
) -> ScanResult {
    let now = Utc::now();
    let mut sources = Vec::new();
    let mut packages = Vec::new();
    let mut updates = Vec::new();

    if config.sources.pacman {
        let provider = PacmanProvider::new(runner);
        let available = pacman_available;
        let mut last_scanned = None;
        if available {
            run_provider(&provider, "pacman", &mut packages, &mut updates);
            last_scanned = Some(now);
        } else {
            tracing::info!("pacman not found on PATH; skipping");
        }
        sources.push(Source {
            id: SourceId::pacman(),
            kind: SourceKind::Pacman,
            available,
            last_scanned,
        });
    }

    if config.sources.flatpak {
        let provider = FlatpakProvider::new(runner);
        let available = flatpak_available;
        let last_scanned = available.then_some(now);
        let mut flatpak_updates = Vec::new();
        if available {
            run_provider(&provider, "flatpak", &mut packages, &mut flatpak_updates);
        } else {
            tracing::info!("flatpak not found on PATH; skipping");
        }
        reconcile_flatpak_updates(&mut flatpak_updates, &packages);
        updates.append(&mut flatpak_updates);

        // Flatpak spans two scopes; surface each as its own source per config.
        if config.scan.flatpak_include_user {
            sources.push(Source {
                id: SourceId::flatpak_user(),
                kind: SourceKind::Flatpak {
                    scope: FlatpakScope::User,
                },
                available,
                last_scanned,
            });
        }
        if config.scan.flatpak_include_system {
            sources.push(Source {
                id: SourceId::flatpak_system(),
                kind: SourceKind::Flatpak {
                    scope: FlatpakScope::System,
                },
                available,
                last_scanned,
            });
        }
    }

    ScanResult {
        schema_version: SCHEMA_VERSION,
        scanned_at: now,
        sources,
        packages,
        updates,
        cache_sizes: gather_cache_sizes(runner, config.sources.pacman && pacman_available),
    }
}

/// Gather disk-usage figures. Currently the pacman package cache; flatpak
/// unused-runtime sizing arrives with the cleanup screen (v0.1.5).
fn gather_cache_sizes(runner: &dyn CommandRunner, pacman_available: bool) -> CacheSizes {
    // `du` exits non-zero when a transient root-owned `download-*` subdir is
    // unreadable, but still prints the grand total to stdout — so parse stdout
    // regardless of exit code.
    let pacman_cache_bytes = pacman_available
        .then(|| runner.run("du", &["-sb", PACMAN_CACHE_DIR]))
        .and_then(Result::ok)
        .and_then(|out| parse_du_bytes(&out.stdout));
    CacheSizes {
        pacman_cache_bytes,
        flatpak_unused_runtime_count: None,
        flatpak_unused_runtime_bytes: None,
    }
}

/// `du -sb <dir>` prints `<bytes>\t<path>`; take the leading byte count.
fn parse_du_bytes(stdout: &str) -> Option<u64> {
    stdout.split_whitespace().next()?.parse().ok()
}

/// Run one provider's scans, appending results and logging any failure.
fn run_provider<P: Provider>(
    provider: &P,
    label: &str,
    packages: &mut Vec<Package>,
    updates: &mut Vec<PendingUpdate>,
) {
    match provider.scan_installed() {
        Ok(mut pkgs) => packages.append(&mut pkgs),
        Err(err) => tracing::error!(source = label, error = %err, "scan_installed failed"),
    }
    match provider.scan_updates() {
        Ok(mut ups) => updates.append(&mut ups),
        Err(err) => tracing::error!(source = label, error = %err, "scan_updates failed"),
    }
}

/// Fill in scope + current version for flatpak updates by matching app ids
/// against the installed list. The `remote-ls` command alone provides neither.
fn reconcile_flatpak_updates(updates: &mut [PendingUpdate], installed: &[Package]) {
    for update in updates.iter_mut() {
        if let Some(pkg) = installed
            .iter()
            .find(|p| p.name == update.package_name && is_flatpak(&p.source_id))
        {
            update.source_id = pkg.source_id.clone();
            update.current_version = pkg.version.clone();
        }
    }
}

fn is_flatpak(id: &SourceId) -> bool {
    id == &SourceId::flatpak_user() || id == &SourceId::flatpak_system()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::InstallReason;
    use crate::providers::test_support::MockRunner;

    const QI_SMALL: &str = include_str!("../../tests/fixtures/pacman/qi_small_system.txt");
    const QU_SAMPLE: &str = include_str!("../../tests/fixtures/pacman/qu_sample.txt");
    const FP_LIST: &str = include_str!("../../tests/fixtures/flatpak/list_apps.txt");
    const FP_UPDATES: &str = include_str!("../../tests/fixtures/flatpak/remote_ls_updates.txt");
    const FP_LIST_KEY: &str =
        "flatpak list --app --columns=application,name,version,origin,installation";
    const FP_UPDATES_KEY: &str = "flatpak remote-ls --updates --app --columns=application,version";
    const DU_KEY: &str = "du -sb /var/cache/pacman/pkg/";

    /// A runner with every command this pipeline issues stubbed to succeed.
    fn full_runner() -> MockRunner {
        MockRunner::new()
            .with("pacman -Qi", QI_SMALL, 0)
            .with("pacman -Qu", QU_SAMPLE, 0)
            .with(DU_KEY, "12345\t/var/cache/pacman/pkg/\n", 0)
            .with(FP_LIST_KEY, FP_LIST, 0)
            .with(FP_UPDATES_KEY, FP_UPDATES, 0)
    }

    #[test]
    fn assemble_full_pipeline_combines_both_sources() {
        let scan = assemble(&full_runner(), &Config::default(), true, true);
        // pacman + flatpak-user + flatpak-system
        assert_eq!(scan.sources.len(), 3);
        assert!(scan.sources.iter().all(|s| s.available));
        // 3 pacman packages + 3 flatpak apps
        assert_eq!(scan.packages.len(), 6);
        // 4 pacman updates + 2 flatpak updates
        assert_eq!(scan.updates.len(), 6);
        assert_eq!(scan.cache_sizes.pacman_cache_bytes, Some(12345));
        assert_eq!(scan.schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn assemble_respects_disabled_pacman_source() {
        let mut config = Config::default();
        config.sources.pacman = false;
        let scan = assemble(&full_runner(), &config, true, true);
        assert!(scan.sources.iter().all(|s| s.id != SourceId::pacman()));
        assert!(
            scan.packages
                .iter()
                .all(|p| p.source_id != SourceId::pacman())
        );
        // No pacman source => no pacman cache size gathered.
        assert_eq!(scan.cache_sizes.pacman_cache_bytes, None);
    }

    #[test]
    fn assemble_omits_flatpak_scopes_when_excluded() {
        let mut config = Config::default();
        config.scan.flatpak_include_system = false;
        let scan = assemble(&full_runner(), &config, true, true);
        assert!(
            scan.sources
                .iter()
                .all(|s| s.id != SourceId::flatpak_system())
        );
        assert!(
            scan.sources
                .iter()
                .any(|s| s.id == SourceId::flatpak_user())
        );
    }

    #[test]
    fn assemble_isolates_a_failing_provider() {
        // pacman installed-scan fails; flatpak still succeeds.
        let runner = MockRunner::new()
            .with("pacman -Qi", "", 1)
            .with("pacman -Qu", "", 1)
            .with(FP_LIST_KEY, FP_LIST, 0)
            .with(FP_UPDATES_KEY, FP_UPDATES, 0);
        let scan = assemble(&runner, &Config::default(), true, true);
        assert!(
            scan.packages
                .iter()
                .all(|p| p.source_id != SourceId::pacman())
        );
        assert_eq!(scan.packages.len(), 3); // flatpak apps survived
        assert!(scan.sources.iter().any(|s| s.id == SourceId::pacman()));
    }

    #[test]
    fn assemble_skips_unavailable_binaries() {
        let scan = assemble(&full_runner(), &Config::default(), false, false);
        assert!(scan.packages.is_empty());
        assert!(scan.updates.is_empty());
        assert_eq!(scan.cache_sizes.pacman_cache_bytes, None);
        // Sources are still listed (per config) but marked unavailable.
        assert!(scan.sources.iter().all(|s| !s.available));
    }

    #[test]
    fn parse_du_bytes_reads_leading_field() {
        assert_eq!(
            parse_du_bytes("5986725560\t/var/cache/pacman/pkg/\n"),
            Some(5_986_725_560)
        );
        assert_eq!(parse_du_bytes(""), None);
        assert_eq!(parse_du_bytes("not-a-number /path"), None);
    }

    fn flatpak_pkg(name: &str, version: &str, scope: SourceId) -> Package {
        Package {
            name: name.to_string(),
            version: version.to_string(),
            source_id: scope,
            install_reason: InstallReason::Unknown,
            size_bytes: None,
            description: None,
            depends_on: Vec::new(),
            required_by: Vec::new(),
            optional_deps: Vec::new(),
            provides: Vec::new(),
        }
    }

    #[test]
    fn reconcile_fills_scope_and_current_version() {
        let installed = vec![flatpak_pkg(
            "org.mozilla.firefox",
            "128.0",
            SourceId::flatpak_user(),
        )];
        let mut updates = vec![PendingUpdate {
            package_name: "org.mozilla.firefox".to_string(),
            current_version: String::new(),
            available_version: "129.0".to_string(),
            source_id: SourceId::flatpak(),
        }];
        reconcile_flatpak_updates(&mut updates, &installed);
        assert_eq!(updates[0].source_id, SourceId::flatpak_user());
        assert_eq!(updates[0].current_version, "128.0");
    }

    #[test]
    fn reconcile_leaves_unmatched_update_untouched() {
        let mut updates = vec![PendingUpdate {
            package_name: "org.unknown.App".to_string(),
            current_version: String::new(),
            available_version: "2.0".to_string(),
            source_id: SourceId::flatpak(),
        }];
        reconcile_flatpak_updates(&mut updates, &[]);
        assert_eq!(updates[0].source_id, SourceId::flatpak());
        assert_eq!(updates[0].current_version, "");
    }
}
