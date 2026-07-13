//! Scan orchestration and the scan cache.
//!
//! Detects available providers, runs them concurrently on scoped threads
//! (spec Q5 — one lane each for pacman, flatpak, and `du`), assembles a
//! `ScanResult`, and persists it to the cache ([`cache`]). Never analyzes
//! data (dev-notes §3).

pub mod cache;

use std::path::Path;

use chrono::Utc;

use crate::config::Config;
use crate::model::{
    CacheSizes, FlatpakScope, Package, PendingUpdate, SCHEMA_VERSION, ScanResult, Source, SourceId,
    SourceKind,
};
use crate::providers::aur;
use crate::providers::flatpak::FlatpakProvider;
use crate::providers::pacman::{self, PacmanProvider};
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
    if refresh {
        tracing::info!("--refresh: ignoring cache");
    } else if let Some(scan) = load_cached(config, config_path)? {
        return Ok(scan);
    }
    scan_and_store(runner, config)
}

/// The instant half: a fresh cache hit or `None`. Never runs a subprocess, so
/// the TUI can open on it immediately and scan in the background.
pub fn load_cached(
    config: &Config,
    config_path: Option<&Path>,
) -> anyhow::Result<Option<ScanResult>> {
    let cache = cache::Cache::locate()?;
    if let Some(scan) = cache.read()? {
        match cache::staleness(&scan, cache.path(), config, config_path) {
            None => {
                tracing::info!("using cached scan");
                return Ok(Some(scan));
            }
            Some(reason) => tracing::info!(reason, "cache stale; re-scanning"),
        }
    }
    Ok(None)
}

/// The slow half: run the providers and write the cache. A failed cache write
/// is logged but non-fatal — the in-memory result is still returned
/// (spec §15 recovery table).
pub fn scan_and_store(runner: &dyn CommandRunner, config: &Config) -> anyhow::Result<ScanResult> {
    let cache = cache::Cache::locate()?;
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
    let checkupdates = crate::providers::binary_on_path(pacman::CHECKUPDATES_BIN);
    let paru = crate::providers::binary_on_path(aur::PARU_BIN);
    let profile_dir =
        directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".var").join("app"));
    assemble(
        runner,
        config,
        pacman_available,
        flatpak_available,
        checkupdates,
        paru,
        profile_dir.as_deref(),
    )
}

/// Assemble a `ScanResult` from the providers, given which binaries are
/// available. Availability is passed in (not probed) so the whole pipeline is
/// hermetically testable with a mock runner.
///
/// The three independent lanes — pacman, flatpak, and `du` cache sizing — run
/// on scoped threads (spec Q5): wall time is the slowest lane, not the sum.
/// Provider failures are isolated: a source that errors is logged and skipped,
/// never aborting the others (dev-notes §3).
fn assemble(
    runner: &dyn CommandRunner,
    config: &Config,
    pacman_available: bool,
    flatpak_available: bool,
    checkupdates_available: bool,
    paru_available: bool,
    flatpak_profile_dir: Option<&Path>,
) -> ScanResult {
    let now = Utc::now();
    let scan_pacman = config.sources.pacman && pacman_available;
    let scan_flatpak = config.sources.flatpak && flatpak_available;
    let scan_aur = config.sources.aur && scan_pacman;

    let (
        (mut packages, mut updates),
        (flatpak_packages, mut flatpak_updates, flatpak_profile_sizes),
        cache_sizes,
        (foreign, mut aur_updates),
    ) = std::thread::scope(|s| {
        let pacman_lane = s.spawn(|| {
            if !scan_pacman {
                return (Vec::new(), Vec::new());
            }
            let provider = PacmanProvider::with_checkupdates(runner, checkupdates_available);
            collect_provider(&provider, "pacman")
        });
        let flatpak_lane = s.spawn(|| {
            if !scan_flatpak {
                return (Vec::new(), Vec::new(), Default::default());
            }
            let (pkgs, ups) = collect_provider(&FlatpakProvider::new(runner), "flatpak");
            let sizes = gather_profile_sizes(runner, flatpak_profile_dir, &pkgs);
            (pkgs, ups, sizes)
        });
        let du_lane = s.spawn(|| gather_cache_sizes(runner, scan_pacman));
        let aur_lane = s.spawn(|| {
            if !scan_aur {
                return (std::collections::HashSet::new(), Vec::new());
            }
            let foreign = match aur::foreign_names(runner) {
                Ok(names) => names,
                Err(err) => {
                    tracing::error!(error = %err, "pacman -Qm failed; no aur source");
                    return (std::collections::HashSet::new(), Vec::new());
                }
            };
            let updates = if paru_available {
                match aur::scan_updates(runner, config.scan.aur_devel) {
                    Ok(ups) => ups,
                    Err(err) => {
                        tracing::error!(error = %err, "paru -Qua failed; no aur updates");
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };
            (foreign, updates)
        });
        (
            join_lane(pacman_lane, "pacman"),
            join_lane(flatpak_lane, "flatpak"),
            join_lane(du_lane, "du"),
            join_lane(aur_lane, "aur"),
        )
    });

    if config.sources.pacman && !pacman_available {
        tracing::info!("pacman not found on PATH; skipping");
    }
    if config.sources.flatpak && !flatpak_available {
        tracing::info!("flatpak not found on PATH; skipping");
    }

    let mut sources = Vec::new();
    if config.sources.pacman {
        sources.push(Source {
            id: SourceId::pacman(),
            kind: SourceKind::Pacman,
            available: pacman_available,
            last_scanned: scan_pacman.then_some(now),
            accurate_updates: checkupdates_available,
        });
    }
    if config.sources.aur {
        // Foreign packages list via pacman either way; "available" means the
        // update path (paru) exists — its absence shows as "not found".
        sources.push(Source {
            id: SourceId::aur(),
            kind: SourceKind::Aur,
            available: paru_available && pacman_available,
            last_scanned: scan_aur.then_some(now),
            accurate_updates: true,
        });
    }
    if config.sources.flatpak {
        let last_scanned = scan_flatpak.then_some(now);
        // Flatpak spans two scopes; surface each as its own source per config.
        if config.scan.flatpak_include_user {
            sources.push(Source {
                id: SourceId::flatpak_user(),
                kind: SourceKind::Flatpak {
                    scope: FlatpakScope::User,
                },
                available: flatpak_available,
                last_scanned,
                accurate_updates: true,
            });
        }
        if config.scan.flatpak_include_system {
            sources.push(Source {
                id: SourceId::flatpak_system(),
                kind: SourceKind::Flatpak {
                    scope: FlatpakScope::System,
                },
                available: flatpak_available,
                last_scanned,
                accurate_updates: true,
            });
        }
    }

    // Foreign packages keep their full pacman -Qi metadata but belong to the
    // aur source (v0.3) — everything downstream keys on source_id.
    if scan_aur {
        for pkg in packages.iter_mut().filter(|p| foreign.contains(&p.name)) {
            pkg.source_id = SourceId::aur();
        }
    }
    updates.append(&mut aur_updates);

    packages.extend(flatpak_packages);
    reconcile_flatpak_updates(&mut flatpak_updates, &packages);
    updates.append(&mut flatpak_updates);

    ScanResult {
        schema_version: SCHEMA_VERSION,
        scanned_at: now,
        sources,
        packages,
        updates,
        cache_sizes,
        flatpak_profile_sizes,
        // Filled by the migration-advisory probe below (v0.4).
        profile_dir_sizes: Default::default(),
    }
}

/// Join one scan lane; a panicked lane yields its default (empty) result and
/// an error log rather than poisoning the whole scan.
fn join_lane<T: Default>(handle: std::thread::ScopedJoinHandle<'_, T>, lane: &str) -> T {
    match handle.join() {
        Ok(value) => value,
        Err(_) => {
            tracing::error!(lane, "scan lane panicked; treating as empty");
            T::default()
        }
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

/// Size of `~/.var/app/<id>` per scanned Flatpak app (spec §9.4 heuristic 2:
/// user data weighs into the overlap tradeoff). Apps without a profile dir
/// (du fails or prints nothing) are simply absent. Runtimes have no profile.
fn gather_profile_sizes(
    runner: &dyn CommandRunner,
    base: Option<&Path>,
    packages: &[Package],
) -> std::collections::HashMap<String, u64> {
    let Some(base) = base else {
        return Default::default();
    };
    let mut sizes = std::collections::HashMap::new();
    for app in packages.iter().filter(|p| !p.runtime) {
        let dir = base.join(&app.name);
        let Some(dir) = dir.to_str() else { continue };
        if let Ok(out) = runner.run("du", &["-sb", dir])
            && let Some(bytes) = parse_du_bytes(&out.stdout)
        {
            sizes.insert(app.name.clone(), bytes);
        }
    }
    sizes
}

/// Run one provider's scans, logging any failure and returning what survived.
fn collect_provider<P: Provider>(provider: &P, label: &str) -> (Vec<Package>, Vec<PendingUpdate>) {
    let packages = match provider.scan_installed() {
        Ok(pkgs) => pkgs,
        Err(err) => {
            tracing::error!(source = label, error = %err, "scan_installed failed");
            Vec::new()
        }
    };
    let updates = match provider.scan_updates() {
        Ok(ups) => ups,
        Err(err) => {
            tracing::error!(source = label, error = %err, "scan_updates failed");
            Vec::new()
        }
    };
    (packages, updates)
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
        "flatpak list --app --columns=application,name,version,origin,installation,runtime,size";
    const FP_RUNTIME_KEY: &str = "flatpak list --runtime --columns=application,name,version,origin,installation,runtime,size";
    const FP_UPDATES_KEY: &str = "flatpak remote-ls --updates --columns=application,version";
    const DU_KEY: &str = "du -sb /var/cache/pacman/pkg/";

    /// A runner with every command this pipeline issues stubbed to succeed.
    /// checkupdates is stubbed with the same sample as -Qu (same format), so
    /// tests pass whichever path `assemble` is told to take.
    fn full_runner() -> MockRunner {
        MockRunner::new()
            .with("pacman -Qi", QI_SMALL, 0)
            .with("pacman -Qu", QU_SAMPLE, 0)
            .with("checkupdates --nocolor", QU_SAMPLE, 0)
            .with(DU_KEY, "12345\t/var/cache/pacman/pkg/\n", 0)
            .with(FP_LIST_KEY, FP_LIST, 0)
            .with(FP_RUNTIME_KEY, "", 0)
            .with(FP_UPDATES_KEY, FP_UPDATES, 0)
    }

    #[test]
    fn profile_sizes_measured_per_app_missing_dirs_absent() {
        let runner = full_runner()
            .with(
                "du -sb /home/t/.var/app/org.mozilla.firefox",
                "52428800\t/home/t/.var/app/org.mozilla.firefox\n",
                0,
            )
            .with(
                "du -sb /home/t/.var/app/org.gnome.Calculator",
                "1024\t/home/t/.var/app/org.gnome.Calculator\n",
                0,
            );
        // md.obsidian.Obsidian has no mock → du "fails" → no profile dir.
        let scan = assemble(
            &runner,
            &Config::default(),
            true,
            true,
            true,
            false,
            Some(Path::new("/home/t/.var/app")),
        );
        assert_eq!(
            scan.flatpak_profile_sizes.get("org.mozilla.firefox"),
            Some(&52_428_800)
        );
        assert_eq!(
            scan.flatpak_profile_sizes.get("org.gnome.Calculator"),
            Some(&1024)
        );
        assert!(
            !scan
                .flatpak_profile_sizes
                .contains_key("md.obsidian.Obsidian"),
            "missing dir must be absent"
        );
    }

    #[test]
    fn profile_sizes_empty_without_a_base_dir() {
        let scan = assemble(
            &full_runner(),
            &Config::default(),
            true,
            true,
            true,
            false,
            None,
        );
        assert!(scan.flatpak_profile_sizes.is_empty());
    }

    const QM_FIXTURE: &str = include_str!("../../tests/fixtures/aur/qm.txt");
    const QUA_FIXTURE: &str = include_str!("../../tests/fixtures/aur/qua.txt");

    #[test]
    fn foreign_packages_move_to_the_aur_source_with_updates() {
        // QI_SMALL has firefox/glibc/bash; mark bash foreign.
        let runner = full_runner().with("pacman -Qm", "bash 5.2-1\n", 0).with(
            "paru -Qua",
            "bash 5.2-1 -> 5.3-1\n",
            0,
        );
        let scan = assemble(&runner, &Config::default(), true, true, true, true, None);
        let bash = scan.packages.iter().find(|p| p.name == "bash").unwrap();
        assert_eq!(bash.source_id, SourceId::aur());
        // Non-foreign packages stay pacman.
        let alac = scan
            .packages
            .iter()
            .find(|p| p.name == "alacritty")
            .unwrap();
        assert_eq!(alac.source_id, SourceId::pacman());
        // The aur update landed with its source.
        assert!(
            scan.updates
                .iter()
                .any(|u| u.package_name == "bash" && u.source_id == SourceId::aur()),
            "{:?}",
            scan.updates
                .iter()
                .map(|u| &u.package_name)
                .collect::<Vec<_>>()
        );
        // Source row: available (paru present).
        let aur = scan
            .sources
            .iter()
            .find(|s| s.id == SourceId::aur())
            .unwrap();
        assert!(aur.available);
    }

    #[test]
    fn missing_paru_lists_foreign_packages_but_no_updates() {
        let runner = full_runner().with("pacman -Qm", "bash 5.2-1\n", 0);
        let scan = assemble(&runner, &Config::default(), true, true, true, false, None);
        let bash = scan.packages.iter().find(|p| p.name == "bash").unwrap();
        assert_eq!(
            bash.source_id,
            SourceId::aur(),
            "labeling works without paru"
        );
        assert!(!scan.updates.iter().any(|u| u.source_id == SourceId::aur()));
        let aur = scan
            .sources
            .iter()
            .find(|s| s.id == SourceId::aur())
            .unwrap();
        assert!(!aur.available, "paru missing → aur shows not found");
    }

    #[test]
    fn sources_aur_false_keeps_foreign_under_pacman() {
        let mut config = Config::default();
        config.sources.aur = false;
        let runner = full_runner().with("pacman -Qm", "bash 5.2-1\n", 0);
        let scan = assemble(&runner, &config, true, true, true, true, None);
        let bash = scan.packages.iter().find(|p| p.name == "bash").unwrap();
        assert_eq!(bash.source_id, SourceId::pacman());
        assert!(!scan.sources.iter().any(|s| s.id == SourceId::aur()));
    }

    #[test]
    fn aur_fixtures_parse_end_to_end() {
        let runner =
            full_runner()
                .with("pacman -Qm", QM_FIXTURE, 0)
                .with("paru -Qua", QUA_FIXTURE, 0);
        let scan = assemble(&runner, &Config::default(), true, true, true, true, None);
        // None of the live foreign names exist in QI_SMALL — no relabels,
        // but the updates still land under aur.
        assert_eq!(
            scan.updates
                .iter()
                .filter(|u| u.source_id == SourceId::aur())
                .count(),
            2
        );
    }

    #[test]
    fn assemble_full_pipeline_combines_both_sources() {
        let scan = assemble(
            &full_runner(),
            &Config::default(),
            true,
            true,
            true,
            false,
            None,
        );
        // pacman + aur + flatpak-user + flatpak-system
        assert_eq!(scan.sources.len(), 4);
        // Everything available except aur (no paru in this fixture).
        assert!(
            scan.sources
                .iter()
                .all(|s| s.available || s.id == SourceId::aur())
        );
        // 3 pacman packages + 3 flatpak apps
        assert_eq!(scan.packages.len(), 6);
        // 4 pacman updates + 2 flatpak updates
        assert_eq!(scan.updates.len(), 6);
        assert_eq!(scan.cache_sizes.pacman_cache_bytes, Some(12345));
        assert_eq!(scan.schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn accurate_updates_flag_follows_checkupdates_availability() {
        let with = assemble(
            &full_runner(),
            &Config::default(),
            true,
            true,
            true,
            false,
            None,
        );
        let pacman = with
            .sources
            .iter()
            .find(|s| s.id == SourceId::pacman())
            .unwrap();
        assert!(pacman.accurate_updates);

        let without = assemble(
            &full_runner(),
            &Config::default(),
            true,
            true,
            false,
            false,
            None,
        );
        let pacman = without
            .sources
            .iter()
            .find(|s| s.id == SourceId::pacman())
            .unwrap();
        assert!(!pacman.accurate_updates);
        // Flatpak counts are always from the remote — always accurate.
        assert!(
            without
                .sources
                .iter()
                .filter(|s| s.id != SourceId::pacman())
                .all(|s| s.accurate_updates)
        );
    }

    #[test]
    fn assemble_respects_disabled_pacman_source() {
        let mut config = Config::default();
        config.sources.pacman = false;
        let scan = assemble(&full_runner(), &config, true, true, true, false, None);
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
        let scan = assemble(&full_runner(), &config, true, true, true, false, None);
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
            .with("checkupdates --nocolor", "", 1)
            .with(FP_LIST_KEY, FP_LIST, 0)
            .with(FP_RUNTIME_KEY, "", 0)
            .with(FP_UPDATES_KEY, FP_UPDATES, 0);
        let scan = assemble(&runner, &Config::default(), true, true, true, false, None);
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
        let scan = assemble(
            &full_runner(),
            &Config::default(),
            false,
            false,
            false,
            false,
            None,
        );
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
            runtime: false,
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
