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
/// Provider failures are isolated: a source that errors is logged and skipped,
/// never aborting the others (dev-notes §3).
pub fn scan(runner: &dyn CommandRunner, config: &Config) -> ScanResult {
    let now = Utc::now();
    let mut sources = Vec::new();
    let mut packages = Vec::new();
    let mut updates = Vec::new();
    let mut pacman_available = false;

    if config.sources.pacman {
        let provider = PacmanProvider::new(runner);
        let available = provider.is_available();
        pacman_available = available;
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
        let available = provider.is_available();
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
        cache_sizes: gather_cache_sizes(runner, pacman_available),
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
