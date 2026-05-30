//! Scan cache (spec §6): the on-disk `ScanResult` and its invalidation rules.
//!
//! The cache is a TOML-serialized `ScanResult` at `~/.cache/paclens/scan.toml`.
//! The dependency graph and overlaps are *not* stored — they are recomputed on
//! load (spec §6.6). Writes are atomic: write `.tmp`, then `rename`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, anyhow};
use chrono::{DateTime, Utc};
use directories::ProjectDirs;

use crate::config::Config;
use crate::model::{SCHEMA_VERSION, ScanResult};

const CACHE_FILENAME: &str = "scan.toml";
const TMP_FILENAME: &str = "scan.toml.tmp";
const PACMAN_DB_DIR: &str = "/var/lib/pacman/local/";

/// A located scan cache. Construct with [`Cache::locate`].
pub struct Cache {
    path: PathBuf,
    tmp: PathBuf,
}

impl Cache {
    /// Resolve `~/.cache/paclens/scan.toml`, create the directory (0700) if
    /// absent, and clear any stale `.tmp` left by an interrupted write.
    pub fn locate() -> anyhow::Result<Self> {
        let dir = ProjectDirs::from("", "", "paclens")
            .ok_or_else(|| anyhow!("could not determine the cache directory for this platform"))?
            .cache_dir()
            .to_path_buf();
        create_dir_private(&dir)?;
        let cache = Self {
            path: dir.join(CACHE_FILENAME),
            tmp: dir.join(TMP_FILENAME),
        };
        if cache.tmp.exists() {
            let _ = fs::remove_file(&cache.tmp);
        }
        Ok(cache)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the cached scan. `Ok(None)` if it is absent or corrupt (a corrupt
    /// cache is logged and treated as a miss, not a hard error).
    pub fn read(&self) -> anyhow::Result<Option<ScanResult>> {
        if !self.path.exists() {
            return Ok(None);
        }
        let text = fs::read_to_string(&self.path)
            .with_context(|| format!("failed to read cache: {}", self.path.display()))?;
        match toml::from_str::<ScanResult>(&text) {
            Ok(scan) => Ok(Some(scan)),
            Err(err) => {
                tracing::warn!(error = %err, "scan cache is corrupt; ignoring it");
                Ok(None)
            }
        }
    }

    /// Atomically write the scan: serialize, write `.tmp`, then `rename`
    /// (dev-notes §2.7).
    pub fn write(&self, scan: &ScanResult) -> anyhow::Result<()> {
        let text = toml::to_string(scan).context("failed to serialize scan cache")?;
        fs::write(&self.tmp, text)
            .with_context(|| format!("failed to write cache temp: {}", self.tmp.display()))?;
        fs::rename(&self.tmp, &self.path)
            .with_context(|| format!("failed to commit cache: {}", self.path.display()))?;
        Ok(())
    }
}

/// Why a cached scan is stale, or `None` if it is still usable. Gathers the
/// filesystem inputs and defers to [`staleness_with`] for the (pure) rules.
/// The `--refresh` flag is handled by the caller, before this is reached.
pub fn staleness(
    scan: &ScanResult,
    cache_path: &Path,
    config: &Config,
    config_path: Option<&Path>,
) -> Option<&'static str> {
    staleness_with(
        scan,
        config.general.cache_ttl,
        Utc::now(),
        mtime(Path::new(PACMAN_DB_DIR)),
        config_path.and_then(mtime),
        mtime(cache_path),
    )
}

/// Pure invalidation logic, checked in the order from spec §6.3. Times are
/// passed in so this is deterministic and unit-testable.
fn staleness_with(
    scan: &ScanResult,
    ttl_secs: u64,
    now: DateTime<Utc>,
    pacman_db_mtime: Option<SystemTime>,
    config_mtime: Option<SystemTime>,
    cache_mtime: Option<SystemTime>,
) -> Option<&'static str> {
    if scan.schema_version != SCHEMA_VERSION {
        return Some("schema version changed");
    }
    if let Some(t) = pacman_db_mtime
        && DateTime::<Utc>::from(t) > scan.scanned_at
    {
        return Some("pacman database changed");
    }
    // `ttl_secs == 0` means "always re-scan" (config docs).
    let age = now.signed_duration_since(scan.scanned_at).num_seconds();
    if ttl_secs == 0 || age < 0 || age as u64 > ttl_secs {
        return Some("cache TTL expired");
    }
    if let (Some(cfg), Some(cache)) = (config_mtime, cache_mtime)
        && cfg > cache
    {
        return Some("config changed");
    }
    None
}

fn mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[cfg(unix)]
fn create_dir_private(dir: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("failed to create cache directory: {}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CacheSizes, FlatpakScope, Source, SourceId, SourceKind};

    fn sample_scan(scanned_at: DateTime<Utc>, schema: u32) -> ScanResult {
        ScanResult {
            schema_version: schema,
            scanned_at,
            sources: vec![
                Source {
                    id: SourceId::pacman(),
                    kind: SourceKind::Pacman,
                    available: true,
                    last_scanned: Some(scanned_at),
                },
                Source {
                    id: SourceId::flatpak_user(),
                    kind: SourceKind::Flatpak {
                        scope: FlatpakScope::User,
                    },
                    available: true,
                    last_scanned: Some(scanned_at),
                },
            ],
            packages: Vec::new(),
            updates: Vec::new(),
            cache_sizes: CacheSizes {
                pacman_cache_bytes: Some(5_986_725_560),
                flatpak_unused_runtime_count: None,
                flatpak_unused_runtime_bytes: None,
            },
        }
    }

    #[test]
    fn toml_round_trip_preserves_scan_including_flatpak_scope() {
        let scan = sample_scan(Utc::now(), SCHEMA_VERSION);
        let text = toml::to_string(&scan).expect("serialize");
        let back: ScanResult = toml::from_str(&text).expect("deserialize");
        assert_eq!(scan, back);
    }

    #[test]
    fn fresh_scan_is_not_stale() {
        let now = Utc::now();
        let scan = sample_scan(now, SCHEMA_VERSION);
        assert_eq!(staleness_with(&scan, 3600, now, None, None, None), None);
    }

    #[test]
    fn schema_mismatch_is_stale() {
        let now = Utc::now();
        let scan = sample_scan(now, SCHEMA_VERSION + 1);
        assert_eq!(
            staleness_with(&scan, 3600, now, None, None, None),
            Some("schema version changed")
        );
    }

    #[test]
    fn old_scan_past_ttl_is_stale() {
        let scanned = Utc::now() - chrono::Duration::hours(2);
        let scan = sample_scan(scanned, SCHEMA_VERSION);
        assert_eq!(
            staleness_with(&scan, 3600, Utc::now(), None, None, None),
            Some("cache TTL expired")
        );
    }

    #[test]
    fn zero_ttl_always_stale() {
        let now = Utc::now();
        let scan = sample_scan(now, SCHEMA_VERSION);
        assert_eq!(
            staleness_with(&scan, 0, now, None, None, None),
            Some("cache TTL expired")
        );
    }

    #[test]
    fn newer_pacman_db_is_stale() {
        let scanned = Utc::now() - chrono::Duration::minutes(5);
        let scan = sample_scan(scanned, SCHEMA_VERSION);
        let db_touched = SystemTime::from(Utc::now()); // after scanned_at
        assert_eq!(
            staleness_with(&scan, 3600, Utc::now(), Some(db_touched), None, None),
            Some("pacman database changed")
        );
    }

    #[test]
    fn newer_config_than_cache_is_stale() {
        let now = Utc::now();
        let scan = sample_scan(now, SCHEMA_VERSION);
        let cache_mtime = SystemTime::from(now - chrono::Duration::minutes(10));
        let config_mtime = SystemTime::from(now);
        assert_eq!(
            staleness_with(
                &scan,
                3600,
                now,
                None,
                Some(config_mtime),
                Some(cache_mtime)
            ),
            Some("config changed")
        );
    }

    #[test]
    fn write_then_read_round_trips_on_disk() {
        let dir = std::env::temp_dir().join(format!("paclens-cache-test-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let cache = Cache {
            path: dir.join(CACHE_FILENAME),
            tmp: dir.join(TMP_FILENAME),
        };
        let scan = sample_scan(Utc::now(), SCHEMA_VERSION);
        cache.write(&scan).expect("write");
        let read = cache.read().expect("read").expect("present");
        assert_eq!(read, scan);
        let _ = fs::remove_dir_all(&dir);
    }
}
