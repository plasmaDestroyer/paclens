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
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanResult {
    pub schema_version: u32,
    pub scanned_at: DateTime<Utc>,
    pub sources: Vec<Source>,
    pub packages: Vec<Package>,
    pub updates: Vec<PendingUpdate>,
    pub cache_sizes: CacheSizes,
}

/// Cache/disk-usage figures gathered during a scan. Populated in v0.0.3;
/// all `None` until then.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheSizes {
    pub pacman_cache_bytes: Option<u64>,
    pub flatpak_unused_runtime_count: Option<u32>,
    pub flatpak_unused_runtime_bytes: Option<u64>,
}
