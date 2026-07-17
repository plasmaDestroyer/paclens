//! The assembled result of a scan (spec §4.7).
//!
//! This is the single source of truth (principle P5): the TUI, `why`, and the
//! overlap detector all read from a `ScanResult`. The dependency graph and
//! overlap results are *not* stored here — they are recomputed from this on
//! load (spec §6.6).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{Package, PendingUpdate, Source};

/// Bump on any breaking change to `ScanResult`. A cache with a mismatched
/// version is discarded and re-scanned (spec §6.5).
/// v2: `Source.accurate_updates` + `Package.runtime` (v0.1.1).
/// v3: `flatpak_profile_sizes` (v0.1.4).
/// v4: flatpak `Package.size_bytes` populated (v0.1.5) — data enrichment;
/// bumped so stale caches rescan instead of showing unknown sizes.
/// v5: foreign packages split into the `aur` source (v0.3).
/// v6: `profile_dir_sizes` — migration advisory probe (v0.4).
/// v7: `CacheSizes` reclaimable + paru build cache (v0.5 cleanup honesty).
pub const SCHEMA_VERSION: u32 = 7;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanResult {
    pub schema_version: u32,
    pub scanned_at: DateTime<Utc>,
    pub sources: Vec<Source>,
    pub packages: Vec<Package>,
    pub updates: Vec<PendingUpdate>,
    pub cache_sizes: CacheSizes,
    /// `~/.var/app/<id>` size per Flatpak app that has one — user data the
    /// overlap tradeoff weighs (spec §9.4 heuristic 2). Apps without a
    /// profile dir are absent.
    #[serde(default)]
    pub flatpak_profile_sizes: std::collections::HashMap<String, u64>,
    /// Sizes of profile directories the migration advisory cares about
    /// (v0.4), keyed by the `~/`-relative path. Only directories that exist
    /// are present. Which paths get probed is decided by the pure
    /// `analyzer::migrate::probe_paths`; the scanner just measures them.
    #[serde(default)]
    pub profile_dir_sizes: std::collections::HashMap<String, u64>,
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
        }
    }
}

/// Cache/disk-usage figures gathered during a scan. Populated in v0.0.3;
/// all `None` until then.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheSizes {
    pub pacman_cache_bytes: Option<u64>,
    /// What `paccache -rk2` would actually free (its dry run) — the honest
    /// number next to the total, which is mostly current-version tarballs
    /// (v0.5 cleanup honesty; dev-notes 2026-07-14). `None` = no paccache.
    #[serde(default)]
    pub pacman_cache_reclaimable_bytes: Option<u64>,
    /// `~/.cache/paru` — AUR build cache (clones + built packages).
    #[serde(default)]
    pub paru_cache_bytes: Option<u64>,
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
