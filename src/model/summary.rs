//! Per-source count derivation from a `ScanResult` (principle P5: one source of
//! truth). Both `paclens status` and the TUI dashboard render these numbers;
//! neither re-derives them.
//!
//! Predicate-based: the caller supplies a membership test, so the same function
//! serves a per-family grouping (pacman vs `flatpak*`, used by `status`) and a
//! per-exact-id grouping (one row per source, used by the dashboard table).

use super::{ScanResult, SourceId};

/// Installed/update counts and availability for a subset of a scan's sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceSummary {
    pub available: bool,
    pub installed: usize,
    pub updates: usize,
}

/// Count installed packages and pending updates whose source matches `is_member`,
/// and report whether any matching source was available at scan time.
pub fn summarize(scan: &ScanResult, is_member: impl Fn(&SourceId) -> bool) -> SourceSummary {
    SourceSummary {
        available: scan.sources.iter().any(|s| is_member(&s.id) && s.available),
        installed: scan
            .packages
            .iter()
            .filter(|p| is_member(&p.source_id))
            .count(),
        updates: scan
            .updates
            .iter()
            .filter(|u| is_member(&u.source_id))
            .count(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        CacheSizes, InstallReason, Package, PendingUpdate, SCHEMA_VERSION, Source, SourceKind,
    };
    use chrono::Utc;

    fn pkg(name: &str, source: SourceId) -> Package {
        Package {
            scope: None,
            name: name.to_string(),
            version: "1".to_string(),
            source_id: source,
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

    fn upd(name: &str, source: SourceId) -> PendingUpdate {
        PendingUpdate {
            package_name: name.to_string(),
            current_version: "1".to_string(),
            available_version: "2".to_string(),
            source_id: source,
        }
    }

    fn source(id: SourceId, kind: SourceKind, available: bool) -> Source {
        Source {
            id,
            kind,
            available,
            last_scanned: None,
            accurate_updates: true,
        }
    }

    /// pacman and flatpak: two packages each, one update each.
    fn scan() -> ScanResult {
        ScanResult {
            schema_version: SCHEMA_VERSION,
            scanned_at: Utc::now(),
            sources: vec![
                source(SourceId::pacman(), SourceKind::Pacman, true),
                source(SourceId::flatpak(), SourceKind::Flatpak, true),
            ],
            packages: vec![
                pkg("a", SourceId::pacman()),
                pkg("b", SourceId::pacman()),
                pkg("org.x.App", SourceId::flatpak()),
                pkg("org.y.App", SourceId::flatpak()),
            ],
            updates: vec![
                upd("a", SourceId::pacman()),
                upd("org.x.App", SourceId::flatpak()),
            ],
            cache_sizes: CacheSizes::default(),
            flatpak_profile_sizes: Default::default(),
            profile_dir_sizes: Default::default(),
            aur_helper: crate::providers::aur::HelperChoice::Detected(
                crate::providers::aur::AurHelper::Paru,
            ),
            kernel: None,
            pacfiles: Vec::new(),
            stale_processes: Vec::new(),
        }
    }

    #[test]
    fn flatpak_counts_both_installations_as_one_source() {
        // User and system are one source: one tool updates them, so one row
        // and one pair of counts (design §13, 2026-09-07). This used to be a
        // prefix match over two ids.
        let flatpak = summarize(&scan(), |id| id == &SourceId::flatpak());
        assert_eq!(
            flatpak,
            SourceSummary {
                available: true,
                installed: 2,
                updates: 1,
            }
        );
    }

    #[test]
    fn per_exact_id_counts_one_source() {
        let s = scan();
        let pacman = summarize(&s, |id| id == &SourceId::pacman());
        assert_eq!(
            pacman,
            SourceSummary {
                available: true,
                installed: 2,
                updates: 1,
            }
        );
        let flatpak = summarize(&s, |id| id == &SourceId::flatpak());
        assert_eq!(
            flatpak,
            SourceSummary {
                available: true,
                installed: 2,
                updates: 1,
            }
        );
    }

    #[test]
    fn a_source_with_no_update_path_reports_no_count_rather_than_zero() {
        // Nothing checked it, so there is no number — "0 updates" would read
        // as "none pending" (design §3). What it has installed is still known.
        let mut s = scan();
        for source in s.sources.iter_mut() {
            source.available = false;
        }
        let summary = summarize(&s, |id| id == &SourceId::flatpak());
        assert!(!summary.available);
        assert_eq!(summary.installed, 2, "what is installed is still known");
    }

    #[test]
    fn empty_scan_summarizes_to_zero_and_unavailable() {
        let empty = ScanResult {
            schema_version: SCHEMA_VERSION,
            scanned_at: Utc::now(),
            sources: Vec::new(),
            packages: Vec::new(),
            updates: Vec::new(),
            cache_sizes: CacheSizes::default(),
            flatpak_profile_sizes: Default::default(),
            profile_dir_sizes: Default::default(),
            aur_helper: crate::providers::aur::HelperChoice::Detected(
                crate::providers::aur::AurHelper::Paru,
            ),
            kernel: None,
            pacfiles: Vec::new(),
            stale_processes: Vec::new(),
        };
        assert_eq!(
            summarize(&empty, |_| true),
            SourceSummary {
                available: false,
                installed: 0,
                updates: 0,
            }
        );
    }
}
