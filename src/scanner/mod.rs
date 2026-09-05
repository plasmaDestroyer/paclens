//! Scan orchestration and the scan cache.
//!
//! Detects available providers, runs them concurrently on scoped threads
//! (spec Q5 — one lane each for pacman, flatpak, and `du`), assembles a
//! `ScanResult`, and persists it to the cache ([`cache`]). Never analyzes
//! data (design §6).

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
/// Where a kernel's modules live. Gone means the running kernel's were
/// removed by an upgrade — anything not already loaded cannot load (#3).
const MODULES_DIR: &str = "/usr/lib/modules";
/// `uname -r` without a subprocess.
const OSRELEASE: &str = "/proc/sys/kernel/osrelease";

/// Processes still mapping files an upgrade deleted (#4).
///
/// Reads `/proc/<pid>/maps` and `/proc/<pid>/cgroup` directly rather than
/// depending on `lsof` or `needrestart`. Another user's processes are not
/// readable without privilege, and paclens does not take any to look — so
/// this sees the caller's own processes, and the surfaces say so rather than
/// implying the machine was searched.
///
/// ponytail: reads each `maps` file whole and stops at the first mapping that
/// matters — one finding per process is all the report uses.
fn find_stale_processes() -> Vec<crate::analyzer::services::StaleProcess> {
    use crate::analyzer::services::{StaleProcess, mapping_matters, unit_from_cgroup};

    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.parse::<u32>().ok()) else {
            continue; // /proc carries plenty that is not a process
        };
        let dir = entry.path();
        // Unreadable: another user's, or gone between the listing and here.
        let Ok(maps) = std::fs::read_to_string(dir.join("maps")) else {
            continue;
        };
        // The first mapping that matters is all the report uses: one file
        // names the reason, and the rest of a process's maps say the same
        // thing.
        let Some(file) = maps.lines().find_map(|line| {
            let line = line.strip_suffix(" (deleted)")?;
            let mut fields = line.split_whitespace();
            let perms = fields.nth(1)?;
            let path = fields.nth(3)?;
            mapping_matters(perms, path).then(|| path.to_string())
        }) else {
            continue;
        };
        let comm = std::fs::read_to_string(dir.join("comm"))
            .map(|c| c.trim().to_string())
            .unwrap_or_default();
        let (unit, scope) = std::fs::read_to_string(dir.join("cgroup"))
            .ok()
            .and_then(|c| unit_from_cgroup(&c))
            .map_or((None, None), |(u, s)| (Some(u), Some(s)));
        out.push(StaleProcess {
            pid,
            comm,
            unit,
            scope,
            file,
        });
    }
    out
}

/// Walk the config dirs for `.pacnew` / `.pacsave` leftovers (#2).
///
/// Bounded rather than unlimited: `/etc` is shallow, and a runaway walk of a
/// bind-mounted tree would turn a scan into a chore. Unreadable directories
/// are skipped silently — several under `/etc` are root-only by design, and
/// paclens does not elevate to look at configs.
///
/// ponytail: std::fs recursion, no walkdir dependency and no `find`
/// subprocess. Symlinked directories are not followed, which is what keeps
/// this from looping.
fn find_pacfiles(dirs: &[String]) -> Vec<crate::analyzer::pacfiles::PacFile> {
    use crate::analyzer::pacfiles::{PacFile, PacFileKind};
    const MAX_DEPTH: usize = 8;

    fn walk(dir: &Path, depth: usize, out: &mut Vec<PacFile>) {
        if depth > MAX_DEPTH {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return; // unreadable: root-only, or gone since the listing
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                walk(&path, depth + 1, out);
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let found = if name.ends_with(".pacnew") {
                PacFileKind::Pacnew
            } else if name.ends_with(".pacsave") {
                PacFileKind::Pacsave
            } else {
                continue;
            };
            let modified_secs = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs());
            out.push(PacFile {
                path: path.to_string_lossy().into_owned(),
                kind: found,
                modified_secs,
            });
        }
    }

    let mut out = Vec::new();
    for dir in dirs {
        walk(Path::new(dir), 0, &mut out);
    }
    out
}

/// The running kernel, as two facts read from the system. Whether they add up
/// to "reboot required" is the analyzer's call (P5) — this only measures, the
/// way it measures cache sizes.
fn read_running_kernel() -> Option<crate::analyzer::kernel::RunningKernel> {
    let release = std::fs::read_to_string(OSRELEASE).ok()?.trim().to_string();
    if release.is_empty() {
        return None;
    }
    let modules_present = Path::new(MODULES_DIR).join(&release).is_dir();
    Some(crate::analyzer::kernel::RunningKernel {
        release,
        modules_present,
    })
}

/// Return a usable `ScanResult`: a fresh cache hit when possible, otherwise a
/// new scan that is then written back to the cache.
///
/// `refresh` forces a re-scan. A failed cache write is logged but non-fatal —
/// the in-memory result is still returned (design §11 recovery table).
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
/// (design §11 recovery table).
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
    let helper = aur::detect(&config.general.aur_helper);
    // Say so when the config asked for something else. A stale pin is not an
    // error, but silently using a different helper than the one configured is
    // exactly the kind of unexplained behaviour design §2 rules out.
    match &helper {
        aur::HelperChoice::FellBack { configured, to } => tracing::warn!(
            configured = %configured,
            using = %to.bin(),
            "configured AUR helper is not installed; falling back to autodetection"
        ),
        aur::HelperChoice::ConfiguredMissing { configured } => tracing::warn!(
            configured = %configured,
            "configured AUR helper is not installed and no other helper was found"
        ),
        _ => {}
    }
    let home_dir = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf());
    assemble(
        runner,
        config,
        pacman_available,
        flatpak_available,
        checkupdates,
        helper,
        home_dir.as_deref(),
    )
}

/// Assemble a `ScanResult` from the providers, given which binaries are
/// available. Availability is passed in (not probed) so the whole pipeline is
/// hermetically testable with a mock runner.
///
/// The three independent lanes — pacman, flatpak, and `du` cache sizing — run
/// on scoped threads (spec Q5): wall time is the slowest lane, not the sum.
/// Provider failures are isolated: a source that errors is logged and skipped,
/// never aborting the others (design §6).
fn assemble(
    runner: &dyn CommandRunner,
    config: &Config,
    pacman_available: bool,
    flatpak_available: bool,
    checkupdates_available: bool,
    aur_helper: aur::HelperChoice,
    home_dir: Option<&Path>,
) -> ScanResult {
    let now = Utc::now();
    let flatpak_profile_dir = home_dir.map(|h| h.join(".var").join("app"));
    let flatpak_profile_dir = flatpak_profile_dir.as_deref();
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
        let du_lane =
            s.spawn(|| gather_cache_sizes(runner, scan_pacman, aur_helper.helper(), home_dir));
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
            let updates = if let Some(helper) = aur_helper.helper() {
                match aur::scan_updates(runner, helper, config.scan.aur_devel) {
                    Ok(ups) => ups,
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            helper = %helper.bin(),
                            "AUR update check failed; no aur updates"
                        );
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
        // update path (a helper) exists — its absence shows as "not found".
        sources.push(Source {
            id: SourceId::aur(),
            kind: SourceKind::Aur,
            available: aur_helper.helper().is_some() && pacman_available,
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
    //
    // Foreign is not the same as from the AUR, though (#77). A repo that is
    // removed from pacman.conf leaves its packages foreign without their ever
    // having touched the AUR, so only the ones built on this machine are
    // relabelled; the rest stay pacman's, which is what still manages them.
    for pkg in packages.iter_mut().filter(|p| foreign.contains(&p.name)) {
        pkg.foreign = true;
        if scan_aur && crate::analyzer::provenance::built_here(pkg) {
            pkg.source_id = SourceId::aur();
        }
    }
    updates.append(&mut aur_updates);

    packages.extend(flatpak_packages);
    reconcile_flatpak_updates(&mut flatpak_updates, &packages);
    updates.append(&mut flatpak_updates);

    let mut scan = ScanResult {
        schema_version: SCHEMA_VERSION,
        scanned_at: now,
        sources,
        packages,
        updates,
        cache_sizes,
        flatpak_profile_sizes,
        profile_dir_sizes: Default::default(),
        aur_helper,
        kernel: read_running_kernel(),
        pacfiles: find_pacfiles(&config.cleanup.config_dirs),
        stale_processes: if config.scan.stale_services {
            find_stale_processes()
        } else {
            Vec::new()
        },
    };

    // v0.4 migration-advisory probe. Which paths matter is pure analyzer
    // logic (overlap candidates → their profile-dir pairs); the scanner only
    // expands `~` and measures. Runs after the lanes join because it needs
    // the assembled package list.
    let candidates = crate::analyzer::detect_overlaps(
        &scan,
        &config.overlap.ignore,
        &config.overlap.extra_mappings,
    );
    let paths = crate::analyzer::migrate::probe_paths(&candidates, &config.overlap.extra_mappings);
    scan.profile_dir_sizes = measure_profile_dirs(runner, home_dir, &paths);
    scan
}

/// Measure the migration advisory's candidate dirs (`~/`-relative paths from
/// the pure probe). Dirs that don't exist (du fails) are simply absent.
/// ponytail: du -sb per dir — a probed Steam library takes seconds; fine
/// because it only runs for apps installed on *both* sides.
fn measure_profile_dirs(
    runner: &dyn CommandRunner,
    home: Option<&Path>,
    paths: &[String],
) -> std::collections::HashMap<String, u64> {
    let Some(home) = home else {
        return Default::default();
    };
    let mut sizes = std::collections::HashMap::new();
    for path in paths {
        let Some(rel) = path.strip_prefix("~/") else {
            continue;
        };
        let abs = home.join(rel);
        let Some(abs) = abs.to_str() else { continue };
        if let Ok(out) = runner.run("du", &["-sb", abs])
            && let Some(bytes) = parse_du_bytes(&out.stdout)
        {
            sizes.insert(path.clone(), bytes);
        }
    }
    sizes
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

/// Gather disk-usage figures: pacman cache total, what paccache would
/// actually reclaim (v0.5 cleanup honesty), and the AUR build cache of
/// whichever helper is in use.
fn gather_cache_sizes(
    runner: &dyn CommandRunner,
    pacman_available: bool,
    aur_helper: Option<aur::AurHelper>,
    home: Option<&Path>,
) -> CacheSizes {
    // `du` exits non-zero when a transient root-owned `download-*` subdir is
    // unreadable, but still prints the grand total to stdout — so parse stdout
    // regardless of exit code.
    let pacman_cache_bytes = pacman_available
        .then(|| runner.run("du", &["-sb", PACMAN_CACHE_DIR]))
        .and_then(Result::ok)
        .and_then(|out| parse_du_bytes(&out.stdout));
    // The dry run mirrors the suggested `paccache -rk3`. paccache missing →
    // command fails → None (the pane shows the total alone).
    let pacman_cache_reclaimable_bytes = pacman_available
        .then(|| runner.run("paccache", &["-dk3"]))
        .and_then(Result::ok)
        .and_then(|out| parse_paccache_saved(&format!("{}\n{}", out.stdout, out.stderr)));
    // All three helpers keep their build cache at `~/.cache/<binary name>`,
    // so the binary name is the directory name. No helper means nothing to
    // measure — an absent number rather than paru's, which would be a figure
    // for a tool the user does not have (design §3).
    let aur_cache_bytes = aur_helper
        .zip(home)
        .map(|(helper, h)| h.join(".cache").join(helper.bin()))
        .and_then(|dir| dir.to_str().map(String::from))
        .and_then(|dir| runner.run("du", &["-sb", &dir]).ok())
        .and_then(|out| parse_du_bytes(&out.stdout));
    CacheSizes {
        pacman_cache_bytes,
        pacman_cache_reclaimable_bytes,
        aur_cache_bytes,
        flatpak_unused_runtime_count: None,
        flatpak_unused_runtime_bytes: None,
    }
}

/// `du -sb <dir>` prints `<bytes>\t<path>`; take the leading byte count.
fn parse_du_bytes(stdout: &str) -> Option<u64> {
    stdout.split_whitespace().next()?.parse().ok()
}

/// paccache's dry run ends in either `==> no candidate packages found for
/// pruning` (nothing to reclaim) or `==> finished dry run: N candidates
/// (disk space saved: 806.97 MiB)`. Anything else is unparseable → `None`.
fn parse_paccache_saved(output: &str) -> Option<u64> {
    if output.contains("no candidate packages found") {
        return Some(0);
    }
    let rest = output.split("disk space saved: ").nth(1)?;
    let mut parts = rest.split_whitespace();
    let number: f64 = parts.next()?.parse().ok()?;
    let unit = parts.next()?.trim_end_matches(')');
    let factor: u64 = match unit {
        "B" => 1,
        "KiB" => 1 << 10,
        "MiB" => 1 << 20,
        "GiB" => 1 << 30,
        "TiB" => 1 << 40,
        _ => return None,
    };
    Some((number * factor as f64) as u64)
}

/// Size of `~/.var/app/<id>` per scanned Flatpak app (design §9 heuristic 2:
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
    use crate::providers::aur::HelperChoice as HC;
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
            HC::None,
            Some(Path::new("/home/t")),
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

    /// A minimal -Qi stanza for firefox, appended to QI_SMALL so the scan
    /// contains an overlap with the FP_LIST org.mozilla.firefox app.
    const FIREFOX_QI: &str = "Name            : firefox\n\
                              Version         : 141.0-1\n\
                              Description     : Fast, Private & Safe Web Browser\n\
                              Depends On      : None\n\
                              Required By     : None\n\
                              Optional Deps   : None\n\
                              Provides        : None\n\
                              Installed Size  : 250.00 MiB\n\
                              Install Reason  : Explicitly installed\n";

    #[test]
    fn migration_probe_measures_existing_profile_dirs() {
        let qi = format!("{QI_SMALL}\n{FIREFOX_QI}");
        let runner = full_runner()
            .with("pacman -Qi", &qi, 0)
            // The curated pair exists on both sides…
            .with("du -sb /home/t/.mozilla", "1200\t/home/t/.mozilla\n", 0)
            .with(
                "du -sb /home/t/.var/app/org.mozilla.firefox/.mozilla",
                "300\t/home/t/.var/app/org.mozilla.firefox/.mozilla\n",
                0,
            )
            // …one XDG guess exists too. Everything unmocked "fails" = absent.
            .with(
                "du -sb /home/t/.cache/firefox",
                "77\t/home/t/.cache/firefox\n",
                0,
            );
        let scan = assemble(
            &runner,
            &Config::default(),
            true,
            true,
            true,
            HC::None,
            Some(Path::new("/home/t")),
        );
        assert_eq!(scan.profile_dir_sizes.get("~/.mozilla"), Some(&1200));
        assert_eq!(
            scan.profile_dir_sizes
                .get("~/.var/app/org.mozilla.firefox/.mozilla"),
            Some(&300)
        );
        assert_eq!(scan.profile_dir_sizes.get("~/.cache/firefox"), Some(&77));
        assert!(
            !scan.profile_dir_sizes.contains_key("~/.config/firefox"),
            "missing dirs must be absent"
        );
    }

    #[test]
    fn migration_probe_skips_without_a_home_dir_or_overlaps() {
        // No home dir → nothing measured even with an overlap present.
        let qi = format!("{QI_SMALL}\n{FIREFOX_QI}");
        let runner = full_runner().with("pacman -Qi", &qi, 0);
        let scan = assemble(
            &runner,
            &Config::default(),
            true,
            true,
            true,
            HC::None,
            None,
        );
        assert!(scan.profile_dir_sizes.is_empty());

        // No overlaps (stock fixtures) → no du probes issued at all.
        let scan = assemble(
            &full_runner(),
            &Config::default(),
            true,
            true,
            true,
            HC::None,
            Some(Path::new("/home/t")),
        );
        assert!(scan.profile_dir_sizes.is_empty());
    }

    #[test]
    fn profile_sizes_empty_without_a_base_dir() {
        let scan = assemble(
            &full_runner(),
            &Config::default(),
            true,
            true,
            true,
            HC::None,
            None,
        );
        assert!(scan.flatpak_profile_sizes.is_empty());
    }

    const QM_FIXTURE: &str = include_str!("../../tests/fixtures/aur/qm.txt");
    /// A package built on this machine, captured from a real system: makepkg
    /// signs nothing and claims nobody, which is what tells it apart from a
    /// package a repository shipped (#77).
    const QI_LOCAL: &str = include_str!("../../tests/fixtures/pacman/qi_local_build.txt");
    const QUA_FIXTURE: &str = include_str!("../../tests/fixtures/aur/qua.txt");

    #[test]
    fn foreign_packages_move_to_the_aur_source_with_updates() {
        // The foreign package has to look built-here, or it is a package from
        // a repo that went away rather than an AUR one (#77).
        let runner = full_runner()
            .with("pacman -Qi", &format!("{QI_SMALL}\n{QI_LOCAL}"), 0)
            .with("pacman -Qm", "antigravity 2.11.0-1\n", 0)
            .with("paru -Qua", "antigravity 2.11.0-1 -> 2.12.0-1\n", 0);
        let scan = assemble(
            &runner,
            &Config::default(),
            true,
            true,
            true,
            HC::Detected(aur::AurHelper::Paru),
            None,
        );
        let local = scan
            .packages
            .iter()
            .find(|p| p.name == "antigravity")
            .unwrap();
        assert_eq!(local.source_id, SourceId::aur());
        assert!(local.foreign, "the -Qm pass marks it");
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
                .any(|u| u.package_name == "antigravity" && u.source_id == SourceId::aur()),
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
    fn the_scan_records_which_helper_it_used_and_calls_that_one() {
        // yay is the helper; the runner only answers `yay -Qua`, so a scan
        // that reached for paru would produce no updates at all.
        let runner = MockRunner::new()
            .with("pacman -Qi", QI_SMALL, 0)
            .with("pacman -Qm", "bash 5.2-1\n", 0)
            .with("yay -Qua", QUA_FIXTURE, 0);
        let scan = assemble(
            &runner,
            &Config::default(),
            true,
            false,
            false,
            HC::Detected(aur::AurHelper::Yay),
            None,
        );
        assert_eq!(scan.aur_helper.helper(), Some(aur::AurHelper::Yay));
        assert!(
            scan.updates.iter().any(|u| u.source_id == SourceId::aur()),
            "yay -Qua should have produced aur updates"
        );
    }

    #[test]
    fn no_helper_is_recorded_as_none_and_the_source_is_unavailable() {
        let runner =
            MockRunner::new()
                .with("pacman -Qi", QI_SMALL, 0)
                .with("pacman -Qm", "bash 5.2-1\n", 0);
        let scan = assemble(
            &runner,
            &Config::default(),
            true,
            false,
            false,
            HC::None,
            None,
        );
        assert_eq!(scan.aur_helper.helper(), None);
        let aur_source = scan
            .sources
            .iter()
            .find(|s| s.id == SourceId::aur())
            .expect("aur source");
        assert!(!aur_source.available, "no helper → aur shows as not found");
        assert!(!scan.updates.iter().any(|u| u.source_id == SourceId::aur()));
    }

    #[test]
    fn missing_paru_lists_foreign_packages_but_no_updates() {
        let runner = full_runner()
            .with("pacman -Qi", &format!("{QI_SMALL}\n{QI_LOCAL}"), 0)
            .with("pacman -Qm", "antigravity 2.11.0-1\n", 0);
        let scan = assemble(
            &runner,
            &Config::default(),
            true,
            true,
            true,
            HC::None,
            None,
        );
        let local = scan
            .packages
            .iter()
            .find(|p| p.name == "antigravity")
            .unwrap();
        assert_eq!(
            local.source_id,
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
        let scan = assemble(
            &runner,
            &config,
            true,
            true,
            true,
            HC::Detected(aur::AurHelper::Paru),
            None,
        );
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
        let scan = assemble(
            &runner,
            &Config::default(),
            true,
            true,
            true,
            HC::Detected(aur::AurHelper::Paru),
            None,
        );
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
            HC::None,
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
            HC::None,
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
            HC::None,
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
        let scan = assemble(&full_runner(), &config, true, true, true, HC::None, None);
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
        let scan = assemble(&full_runner(), &config, true, true, true, HC::None, None);
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
        let scan = assemble(
            &runner,
            &Config::default(),
            true,
            true,
            true,
            HC::None,
            None,
        );
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
            HC::None,
            None,
        );
        assert!(scan.packages.is_empty());
        assert!(scan.updates.is_empty());
        assert_eq!(scan.cache_sizes.pacman_cache_bytes, None);
        // Sources are still listed (per config) but marked unavailable.
        assert!(scan.sources.iter().all(|s| !s.available));
    }

    #[test]
    fn parse_paccache_saved_reads_both_endings() {
        assert_eq!(
            parse_paccache_saved("==> no candidate packages found for pruning\n"),
            Some(0)
        );
        assert_eq!(
            parse_paccache_saved(
                "==> finished dry run: 30 candidates (disk space saved: 806.97 MiB)\n"
            ),
            Some((806.97 * 1024.0 * 1024.0) as u64)
        );
        assert_eq!(
            parse_paccache_saved("==> finished dry run: 2 candidates (disk space saved: 1.20 GiB)"),
            Some((1.2 * 1024.0 * 1024.0 * 1024.0) as u64)
        );
        assert_eq!(parse_paccache_saved("garbage"), None);
        assert_eq!(parse_paccache_saved(""), None);
    }

    #[test]
    fn cache_sizes_include_reclaimable_and_the_aur_build_cache() {
        let runner = full_runner()
            .with(
                "paccache -dk3",
                "==> finished dry run: 3 candidates (disk space saved: 100.00 MiB)\n",
                0,
            )
            .with(
                "du -sb /home/t/.cache/paru",
                "9000000000\t/home/t/.cache/paru\n",
                0,
            );
        let scan = assemble(
            &runner,
            &Config::default(),
            true,
            true,
            true,
            HC::Detected(aur::AurHelper::Paru),
            Some(Path::new("/home/t")),
        );
        assert_eq!(
            scan.cache_sizes.pacman_cache_reclaimable_bytes,
            Some(100 * 1024 * 1024)
        );
        assert_eq!(scan.cache_sizes.aur_cache_bytes, Some(9_000_000_000));

        // paccache or the build cache dir missing → honest None.
        let scan = assemble(
            &full_runner(),
            &Config::default(),
            true,
            true,
            true,
            HC::Detected(aur::AurHelper::Paru),
            None,
        );
        assert_eq!(scan.cache_sizes.pacman_cache_reclaimable_bytes, None);
        assert_eq!(scan.cache_sizes.aur_cache_bytes, None);
    }

    /// The build cache measured is the one belonging to the helper actually in
    /// use. A yay user's `~/.cache/yay` is what counts; paru's directory is not
    /// consulted, and a `du` mock for it going unused proves the point.
    #[test]
    fn the_build_cache_measured_follows_the_detected_helper() {
        let runner = full_runner()
            .with("du -sb /home/t/.cache/yay", "700\t/home/t/.cache/yay\n", 0)
            .with(
                "du -sb /home/t/.cache/paru",
                "9000000000\t/home/t/.cache/paru\n",
                0,
            );
        let scan = assemble(
            &runner,
            &Config::default(),
            true,
            true,
            true,
            HC::Detected(aur::AurHelper::Yay),
            Some(Path::new("/home/t")),
        );
        assert_eq!(scan.cache_sizes.aur_cache_bytes, Some(700));
    }

    /// With no helper there is no build cache to attribute, so the figure is
    /// absent rather than paru's — a number for a tool the user does not have
    /// would be exactly the kind of dishonest total design §3 forbids.
    #[test]
    fn no_helper_means_no_build_cache_figure() {
        let runner = full_runner().with(
            "du -sb /home/t/.cache/paru",
            "9000000000\t/home/t/.cache/paru\n",
            0,
        );
        let scan = assemble(
            &runner,
            &Config::default(),
            true,
            true,
            true,
            HC::None,
            Some(Path::new("/home/t")),
        );
        assert_eq!(scan.cache_sizes.aur_cache_bytes, None);
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
            foreign: false,
            signed: true,
            packager: None,
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
