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
        CacheSizes, FlatpakScope, InstallReason, Package, PendingUpdate, SCHEMA_VERSION, Source,
        SourceKind,
    };
    use chrono::Utc;

    fn pkg(name: &str, source: SourceId) -> Package {
        Package {
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
        }
    }

    /// A scan with all three sources; flatpak-system is unavailable.
    fn scan() -> ScanResult {
        ScanResult {
            schema_version: SCHEMA_VERSION,
            scanned_at: Utc::now(),
            sources: vec![
                source(SourceId::pacman(), SourceKind::Pacman, true),
                source(
                    SourceId::flatpak_user(),
                    SourceKind::Flatpak {
                        scope: FlatpakScope::User,
                    },
                    true,
                ),
                source(
                    SourceId::flatpak_system(),
                    SourceKind::Flatpak {
                        scope: FlatpakScope::System,
                    },
                    false,
                ),
            ],
            packages: vec![
                pkg("a", SourceId::pacman()),
                pkg("b", SourceId::pacman()),
                pkg("org.x.App", SourceId::flatpak_user()),
                pkg("org.y.App", SourceId::flatpak_system()),
            ],
            updates: vec![
                upd("a", SourceId::pacman()),
                upd("org.x.App", SourceId::flatpak_user()),
            ],
            cache_sizes: CacheSizes::default(),
        }
    }

    #[test]
    fn per_family_groups_both_flatpak_scopes() {
        let flatpak = summarize(&scan(), |id| id.as_str().starts_with("flatpak"));
        assert_eq!(
            flatpak,
            SourceSummary {
                available: true, // flatpak-user is available even though -system is not
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
        let user = summarize(&s, |id| id == &SourceId::flatpak_user());
        assert_eq!(
            user,
            SourceSummary {
                available: true,
                installed: 1,
                updates: 1,
            }
        );
        let system = summarize(&s, |id| id == &SourceId::flatpak_system());
        assert_eq!(
            system,
            SourceSummary {
                available: false,
                installed: 1,
                updates: 0,
            }
        );
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
