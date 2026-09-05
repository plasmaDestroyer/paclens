//! The assembled result of a scan (design §7).
//!
//! This is the single source of truth (principle P5): the TUI, `why`, and the
//! overlap detector all read from a `ScanResult`. The dependency graph and
//! overlap results are *not* stored here — they are recomputed from this on
//! load (design §7).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{Package, PendingUpdate, Source};

/// Bump on any breaking change to `ScanResult`. A cache with a mismatched
/// version is discarded and re-scanned (design §7).
/// v2: `Source.accurate_updates` + `Package.runtime` (0.1.0).
/// v3: `flatpak_profile_sizes` (0.2.0).
/// v4: flatpak `Package.size_bytes` populated (0.2.0) — data enrichment;
/// bumped so stale caches rescan instead of showing unknown sizes.
/// v5: foreign packages split into the `aur` source (0.3.0).
/// v6: `profile_dir_sizes` — migration advisory probe (0.3.0).
/// v7: `CacheSizes` reclaimable + paru build cache (0.3.0, cleanup honesty).
/// v8: `aur_helper` — which helper the scan used, so the planner builds the
/// update step for the one actually installed rather than assuming paru.
/// v9: `aur_cache_bytes` replaces `paru_cache_bytes` — the build cache follows
/// the detected helper instead of always being paru's.
/// v10: `aur_helper` widens from the resolved helper to the whole
/// `HelperChoice`, so the dashboard can say *why* — a stale pin needs the name
/// that was configured, which the resolved value has already thrown away.
pub const SCHEMA_VERSION: u32 = 14;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanResult {
    pub schema_version: u32,
    pub scanned_at: DateTime<Utc>,
    pub sources: Vec<Source>,
    pub packages: Vec<Package>,
    pub updates: Vec<PendingUpdate>,
    pub cache_sizes: CacheSizes,
    /// `~/.var/app/<id>` size per Flatpak app that has one — user data the
    /// overlap tradeoff weighs (design §9 heuristic 2). Apps without a
    /// profile dir are absent.
    #[serde(default)]
    pub flatpak_profile_sizes: std::collections::HashMap<String, u64>,
    /// Sizes of profile directories the migration advisory cares about
    /// (v0.4), keyed by the `~/`-relative path. Only directories that exist
    /// are present. Which paths get probed is decided by the pure
    /// `analyzer::migrate::probe_paths`; the scanner just measures them.
    #[serde(default)]
    pub profile_dir_sizes: std::collections::HashMap<String, u64>,
    /// What the AUR helper resolved to, *and how*. Recorded rather than
    /// re-detected because the planner is pure over a `ScanResult` (P5) — it
    /// cannot look at `PATH`, and a plan that names a helper the scan did not
    /// use would be a plan for a different machine.
    ///
    /// The whole choice is kept, not just the resolved helper: explaining a
    /// stale pin needs the name that was *configured*, and resolving throws
    /// that away.
    #[serde(default)]
    pub aur_helper: crate::providers::aur::HelperChoice,
    /// The kernel this machine is running on, and whether its modules are
    /// still on disk. Two facts read from `/proc` and the filesystem; whether
    /// they mean "reboot required" is the analyzer's call (#3).
    #[serde(default)]
    pub kernel: Option<crate::analyzer::kernel::RunningKernel>,
    /// `.pacnew` / `.pacsave` files found under the config dirs (#2). Paths
    /// and mtimes only — what they mean, and what to do about them, is the
    /// analyzer's.
    #[serde(default)]
    pub pacfiles: Vec<crate::analyzer::pacfiles::PacFile>,
    /// Processes holding files an upgrade replaced (#4). Only the ones this
    /// user can see: reading another user's `/proc/<pid>/maps` needs
    /// privilege, and paclens does not take any to look.
    #[serde(default)]
    pub stale_processes: Vec<crate::analyzer::services::StaleProcess>,
}

impl ScanResult {
    /// A placeholder for the moment between opening the TUI cold and the first
    /// background scan landing. Never cached.
    pub fn empty() -> Self {
        ScanResult {
            schema_version: SCHEMA_VERSION,
            scanned_at: Utc::now(),
            sources: Vec::new(),
            packages: Vec::new(),
            updates: Vec::new(),
            cache_sizes: CacheSizes::default(),
            flatpak_profile_sizes: Default::default(),
            profile_dir_sizes: Default::default(),
            aur_helper: Default::default(),
            kernel: None,
            pacfiles: Vec::new(),
            stale_processes: Vec::new(),
        }
    }
}

/// Cache/disk-usage figures gathered during a scan. Populated in v0.0.3;
/// all `None` until then.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheSizes {
    pub pacman_cache_bytes: Option<u64>,
    /// What `paccache -rk3` would actually free (its dry run) — the honest
    /// number next to the total, which is mostly current-version tarballs
    /// (v0.5 cleanup honesty; dev-notes 2026-07-14). `None` = no paccache.
    #[serde(default)]
    pub pacman_cache_reclaimable_bytes: Option<u64>,
    /// `~/.cache/<helper>` — AUR build cache (clones + built packages) for
    /// whichever helper the scan used. `None` when there is no helper, or the
    /// directory does not exist yet.
    #[serde(default)]
    pub aur_cache_bytes: Option<u64>,
    pub flatpak_unused_runtime_count: Option<u32>,
    pub flatpak_unused_runtime_bytes: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_scan_is_truly_empty_and_current_schema() {
        let s = ScanResult::empty();
        assert_eq!(s.schema_version, SCHEMA_VERSION);
        assert!(s.sources.is_empty());
        assert!(s.packages.is_empty());
        assert!(s.updates.is_empty());
        assert_eq!(s.cache_sizes, CacheSizes::default());
    }
}
