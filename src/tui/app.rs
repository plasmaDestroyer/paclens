//! Dashboard application state.
//!
//! `App` owns everything the screen draws (dev-notes §5): the `ScanResult`, the
//! resolved `Theme`, and the selected row index. Rendering borrows `&App` and
//! never mutates; the event loop is the only mutator. Selection is a plain
//! `Option<usize>` (not a ratatui `TableState`) so the render path can stay fully
//! immutable — `draw` builds an ephemeral `TableState` from `selected()` each
//! frame.

use crate::model::{ScanResult, summarize};
use crate::tui::theme::Theme;

/// One source's row in the dashboard table — a view-model derived from the scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRow {
    pub id: String,
    pub installed: usize,
    pub updates: usize,
    pub available: bool,
}

pub struct App {
    scan: ScanResult,
    pub theme: Theme,
    selected: Option<usize>,
}

impl App {
    pub fn new(scan: ScanResult, theme: Theme) -> Self {
        let selected = if scan.sources.is_empty() {
            None
        } else {
            Some(0)
        };
        App {
            scan,
            theme,
            selected,
        }
    }

    /// Swap in a fresh scan (after a refresh), keeping the selection valid.
    pub fn replace_scan(&mut self, scan: ScanResult) {
        self.scan = scan;
        let len = self.scan.sources.len();
        self.selected = match (len, self.selected) {
            (0, _) => None,
            (_, None) => Some(0),
            (n, Some(i)) => Some(i.min(n - 1)),
        };
    }

    pub fn scan(&self) -> &ScanResult {
        &self.scan
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// Total pending updates across all sources — the dashboard's headline number.
    pub fn total_updates(&self) -> usize {
        self.scan.updates.len()
    }

    /// One [`SourceRow`] per source, with counts derived via the shared
    /// `summarize` (so the table and `paclens status` always agree).
    pub fn rows(&self) -> Vec<SourceRow> {
        self.scan
            .sources
            .iter()
            .map(|s| {
                let summary = summarize(&self.scan, |id| id == &s.id);
                SourceRow {
                    id: s.id.to_string(),
                    installed: summary.installed,
                    updates: summary.updates,
                    available: s.available,
                }
            })
            .collect()
    }

    /// Move the selection down one row, clamped at the last row. No-op when empty.
    pub fn select_next(&mut self) {
        let len = self.scan.sources.len();
        if len == 0 {
            return;
        }
        self.selected = Some(match self.selected {
            Some(i) if i + 1 < len => i + 1,
            Some(i) => i,
            None => 0,
        });
    }

    /// Move the selection up one row, clamped at the first row. No-op when empty.
    pub fn select_prev(&mut self) {
        let len = self.scan.sources.len();
        if len == 0 {
            return;
        }
        self.selected = Some(match self.selected {
            Some(0) | None => 0,
            Some(i) => i - 1,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        CacheSizes, FlatpakScope, InstallReason, Package, PendingUpdate, SCHEMA_VERSION, Source,
        SourceId, SourceKind,
    };
    use crate::tui::theme::Theme;
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

    fn scan_with_sources(sources: Vec<Source>) -> ScanResult {
        ScanResult {
            schema_version: SCHEMA_VERSION,
            scanned_at: Utc::now(),
            sources,
            packages: vec![
                pkg("a", SourceId::pacman()),
                pkg("b", SourceId::pacman()),
                pkg("org.x.App", SourceId::flatpak_user()),
            ],
            updates: vec![upd("a", SourceId::pacman())],
            cache_sizes: CacheSizes::default(),
        }
    }

    fn three_sources() -> Vec<Source> {
        vec![
            Source {
                id: SourceId::pacman(),
                kind: SourceKind::Pacman,
                available: true,
                last_scanned: None,
            },
            Source {
                id: SourceId::flatpak_user(),
                kind: SourceKind::Flatpak {
                    scope: FlatpakScope::User,
                },
                available: true,
                last_scanned: None,
            },
            Source {
                id: SourceId::flatpak_system(),
                kind: SourceKind::Flatpak {
                    scope: FlatpakScope::System,
                },
                available: false,
                last_scanned: None,
            },
        ]
    }

    fn app() -> App {
        App::new(scan_with_sources(three_sources()), Theme::none())
    }

    #[test]
    fn new_selects_first_row_when_non_empty() {
        assert_eq!(app().selected(), Some(0));
    }

    #[test]
    fn new_selects_nothing_when_empty() {
        let app = App::new(scan_with_sources(Vec::new()), Theme::none());
        assert_eq!(app.selected(), None);
    }

    #[test]
    fn rows_derive_counts_and_availability_per_source() {
        let rows = app().rows();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].id, "pacman");
        assert_eq!(rows[0].installed, 2);
        assert_eq!(rows[0].updates, 1);
        assert!(rows[0].available);
        assert_eq!(rows[1].id, "flatpak-user");
        assert_eq!(rows[1].installed, 1);
        assert_eq!(rows[1].updates, 0);
        assert!(rows[1].available);
        assert_eq!(rows[2].id, "flatpak-system");
        assert_eq!(rows[2].installed, 0);
        assert!(!rows[2].available);
    }

    #[test]
    fn total_updates_is_the_headline_count() {
        assert_eq!(app().total_updates(), 1);
    }

    #[test]
    fn select_next_advances_then_clamps_at_bottom() {
        let mut app = app();
        app.select_next();
        assert_eq!(app.selected(), Some(1));
        app.select_next();
        assert_eq!(app.selected(), Some(2));
        app.select_next(); // already at last row
        assert_eq!(app.selected(), Some(2));
    }

    #[test]
    fn select_prev_retreats_then_clamps_at_top() {
        let mut app = app();
        app.select_next();
        app.select_next();
        app.select_prev();
        assert_eq!(app.selected(), Some(1));
        app.select_prev();
        assert_eq!(app.selected(), Some(0));
        app.select_prev(); // already at first row
        assert_eq!(app.selected(), Some(0));
    }

    #[test]
    fn navigation_is_a_noop_on_an_empty_scan() {
        let mut app = App::new(scan_with_sources(Vec::new()), Theme::none());
        app.select_next();
        assert_eq!(app.selected(), None);
        app.select_prev();
        assert_eq!(app.selected(), None);
    }

    #[test]
    fn replace_scan_clamps_selection_into_range() {
        let mut app = app();
        app.select_next();
        app.select_next(); // selected = 2
        // Replace with a scan that has only one source.
        app.replace_scan(scan_with_sources(vec![Source {
            id: SourceId::pacman(),
            kind: SourceKind::Pacman,
            available: true,
            last_scanned: None,
        }]));
        assert_eq!(app.selected(), Some(0));
    }

    #[test]
    fn replace_scan_clears_selection_when_empty() {
        let mut app = app();
        app.replace_scan(scan_with_sources(Vec::new()));
        assert_eq!(app.selected(), None);
    }
}
