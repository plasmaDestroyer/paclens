//! Application state for the multi-screen TUI.
//!
//! `App` owns everything the screens draw (dev-notes §5): the `ScanResult`, the
//! resolved `Theme`, which `Screen` is active, the dashboard cursor, the update
//! screen's per-source toggles + cursor, and a transient flash message. Rendering
//! borrows `&App` and never mutates; the event loop is the only mutator.

use std::collections::HashMap;

use crate::model::{ActionPlan, PendingUpdate, ScanResult, Source, SourceId, summarize};
use crate::planner;
use crate::tui::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Dashboard,
    Updates,
}

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
    screen: Screen,
    dash_selected: Option<usize>,
    /// Update screen: per-source toggle (true = included in the plan).
    enabled: HashMap<SourceId, bool>,
    /// Update screen: cursor over `available_sources()`.
    update_cursor: usize,
    /// Transient status line, cleared on the next key.
    flash: Option<String>,
}

impl App {
    pub fn new(scan: ScanResult, theme: Theme) -> Self {
        let dash_selected = if scan.sources.is_empty() {
            None
        } else {
            Some(0)
        };
        let enabled = default_toggles(&scan);
        App {
            scan,
            theme,
            screen: Screen::Dashboard,
            dash_selected,
            enabled,
            update_cursor: 0,
            flash: None,
        }
    }

    /// Swap in a fresh scan (after a refresh), keeping cursors valid.
    pub fn replace_scan(&mut self, scan: ScanResult) {
        self.scan = scan;
        let len = self.scan.sources.len();
        self.dash_selected = match (len, self.dash_selected) {
            (0, _) => None,
            (_, None) => Some(0),
            (n, Some(i)) => Some(i.min(n - 1)),
        };
        self.enabled = default_toggles(&self.scan);
        self.clamp_update_cursor();
        self.flash = None;
    }

    // --- shared ---
    pub fn scan(&self) -> &ScanResult {
        &self.scan
    }
    pub fn screen(&self) -> Screen {
        self.screen
    }
    pub fn total_updates(&self) -> usize {
        self.scan.updates.len()
    }
    pub fn flash(&self) -> Option<&str> {
        self.flash.as_deref()
    }
    pub fn set_flash(&mut self, msg: impl Into<String>) {
        self.flash = Some(msg.into());
    }
    pub fn clear_flash(&mut self) {
        self.flash = None;
    }

    // --- screen navigation ---
    pub fn goto_updates(&mut self) {
        self.screen = Screen::Updates;
        self.clamp_update_cursor();
    }
    pub fn back_to_dashboard(&mut self) {
        self.screen = Screen::Dashboard;
    }

    /// Move the active screen's cursor forward / back.
    pub fn on_next(&mut self) {
        match self.screen {
            Screen::Dashboard => self.select_next(),
            Screen::Updates => self.update_next(),
        }
    }
    pub fn on_prev(&mut self) {
        match self.screen {
            Screen::Dashboard => self.select_prev(),
            Screen::Updates => self.update_prev(),
        }
    }

    // --- dashboard ---
    pub fn selected(&self) -> Option<usize> {
        self.dash_selected
    }

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

    fn select_next(&mut self) {
        let len = self.scan.sources.len();
        if len == 0 {
            return;
        }
        self.dash_selected = Some(match self.dash_selected {
            Some(i) if i + 1 < len => i + 1,
            Some(i) => i,
            None => 0,
        });
    }
    fn select_prev(&mut self) {
        if self.scan.sources.is_empty() {
            return;
        }
        self.dash_selected = Some(match self.dash_selected {
            Some(0) | None => 0,
            Some(i) => i - 1,
        });
    }

    // --- update screen ---
    /// Available sources, in scan order — the toggle/cursor list.
    pub fn available_sources(&self) -> Vec<&Source> {
        self.scan.sources.iter().filter(|s| s.available).collect()
    }

    pub fn update_cursor(&self) -> usize {
        self.update_cursor
    }

    pub fn is_enabled(&self, id: &SourceId) -> bool {
        self.enabled.get(id).copied().unwrap_or(true)
    }

    /// Pending updates for one source (the right pane), in scan order.
    pub fn updates_for(&self, id: &SourceId) -> Vec<&PendingUpdate> {
        self.scan
            .updates
            .iter()
            .filter(|u| &u.source_id == id)
            .collect()
    }

    /// The plan for the currently enabled sources (shared with the CLI via P5).
    pub fn update_plan(&self) -> ActionPlan {
        planner::plan_updates(&self.scan, |id| self.is_enabled(id))
    }

    pub fn toggle_selected(&mut self) {
        let id = {
            let sources = self.available_sources();
            match sources.get(self.update_cursor) {
                Some(s) => s.id.clone(),
                None => return,
            }
        };
        let now = !self.is_enabled(&id);
        self.enabled.insert(id, now);
    }

    fn update_next(&mut self) {
        let len = self.available_sources().len();
        if self.update_cursor + 1 < len {
            self.update_cursor += 1;
        }
    }
    fn update_prev(&mut self) {
        self.update_cursor = self.update_cursor.saturating_sub(1);
    }

    fn clamp_update_cursor(&mut self) {
        let len = self.available_sources().len();
        self.update_cursor = self.update_cursor.min(len.saturating_sub(1));
    }
}

fn default_toggles(scan: &ScanResult) -> HashMap<SourceId, bool> {
    scan.sources.iter().map(|s| (s.id.clone(), true)).collect()
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

    fn app() -> App {
        App::new(scan_with_sources(three_sources()), Theme::none())
    }

    // --- dashboard (unchanged behavior) ---
    #[test]
    fn new_selects_first_row_when_non_empty() {
        assert_eq!(app().selected(), Some(0));
        assert_eq!(app().screen(), Screen::Dashboard);
    }

    #[test]
    fn rows_derive_counts_and_availability_per_source() {
        let rows = app().rows();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].id, "pacman");
        assert_eq!(rows[0].installed, 2);
        assert_eq!(rows[0].updates, 1);
        assert!(rows[2].id == "flatpak-system" && !rows[2].available);
    }

    #[test]
    fn dashboard_navigation_clamps() {
        let mut app = app();
        app.on_next();
        assert_eq!(app.selected(), Some(1));
        app.on_next();
        app.on_next();
        assert_eq!(app.selected(), Some(2)); // clamped at last
        app.on_prev();
        assert_eq!(app.selected(), Some(1));
    }

    // --- screen navigation ---
    #[test]
    fn goto_and_back_switch_screens() {
        let mut app = app();
        app.goto_updates();
        assert_eq!(app.screen(), Screen::Updates);
        app.back_to_dashboard();
        assert_eq!(app.screen(), Screen::Dashboard);
    }

    // --- update screen ---
    #[test]
    fn available_sources_excludes_unavailable() {
        let app = app();
        let names: Vec<&str> = app
            .available_sources()
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(names, vec!["pacman", "flatpak-user"]); // flatpak-system is unavailable
    }

    #[test]
    fn update_cursor_clamps_within_available_sources() {
        let mut app = app();
        app.goto_updates();
        app.on_next(); // 0 -> 1
        assert_eq!(app.update_cursor(), 1);
        app.on_next(); // clamped at 1 (only 2 available)
        assert_eq!(app.update_cursor(), 1);
        app.on_prev();
        app.on_prev(); // clamped at 0
        assert_eq!(app.update_cursor(), 0);
    }

    #[test]
    fn toggles_default_on_and_plan_includes_pacman() {
        let app = app();
        assert!(app.is_enabled(&SourceId::pacman()));
        let plan = app.update_plan();
        assert_eq!(plan.source_count(), 1); // pacman has the one update
        assert_eq!(plan.steps[0].source_id, SourceId::pacman());
    }

    #[test]
    fn toggling_a_source_off_removes_it_from_the_plan() {
        let mut app = app();
        app.goto_updates(); // cursor 0 = pacman
        app.toggle_selected();
        assert!(!app.is_enabled(&SourceId::pacman()));
        assert!(app.update_plan().is_empty()); // flatpak-user has no updates
        app.toggle_selected();
        assert!(app.is_enabled(&SourceId::pacman()));
        assert_eq!(app.update_plan().source_count(), 1);
    }

    #[test]
    fn updates_for_filters_by_source() {
        let app = app();
        assert_eq!(app.updates_for(&SourceId::pacman()).len(), 1);
        assert_eq!(app.updates_for(&SourceId::flatpak_user()).len(), 0);
    }

    #[test]
    fn flash_sets_and_clears() {
        let mut app = app();
        assert!(app.flash().is_none());
        app.set_flash("hello");
        assert_eq!(app.flash(), Some("hello"));
        app.clear_flash();
        assert!(app.flash().is_none());
    }

    #[test]
    fn replace_scan_reinitializes_toggles_and_clamps() {
        let mut app = app();
        app.goto_updates();
        app.on_next(); // cursor 1
        app.toggle_selected();
        app.replace_scan(scan_with_sources(vec![Source {
            id: SourceId::pacman(),
            kind: SourceKind::Pacman,
            available: true,
            last_scanned: None,
        }]));
        assert_eq!(app.update_cursor(), 0);
        assert!(app.is_enabled(&SourceId::pacman())); // toggles reset to on
    }
}
