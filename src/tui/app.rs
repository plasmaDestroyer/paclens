//! Application state for the multi-screen TUI.
//!
//! `App` owns everything the screens draw (dev-notes §5): the `ScanResult`, the
//! resolved `Theme`, which `Screen` is active, the dashboard cursor, the update
//! screen's per-source toggles + cursor, and a transient flash message. Rendering
//! borrows `&App` and never mutates; the event loop is the only mutator.

use std::collections::HashMap;

use crate::analyzer::{self, DepGraph, WhyReport};
use crate::config::{Config, ExtraMapping};
use crate::executor::ExecutionReport;
use crate::fuzzy;
use crate::model::{ActionPlan, Package, PendingUpdate, ScanResult, Source, SourceId, summarize};
use crate::planner;
use crate::tui::theme::Theme;

/// Startup knobs the `App` keeps for the whole session.
pub struct AppOptions {
    /// `config.why.max_depth`.
    pub why_depth: u32,
    /// The privilege tool detected at startup (spec §13.4), if any.
    pub privilege_tool: Option<&'static str>,
    /// `config.overlap.ignore`, for the dashboard's overlap count.
    pub overlap_ignore: Vec<String>,
    /// `config.overlap.extra_mappings`, same.
    pub extra_mappings: Vec<ExtraMapping>,
}

impl AppOptions {
    pub fn from_config(config: &Config, privilege_tool: Option<&'static str>) -> Self {
        AppOptions {
            why_depth: config.why.max_depth,
            privilege_tool,
            overlap_ignore: config.overlap.ignore.clone(),
            extra_mappings: config.overlap.extra_mappings.clone(),
        }
    }

    /// Deterministic defaults for unit tests.
    #[cfg(test)]
    pub fn test() -> Self {
        AppOptions {
            why_depth: 20,
            privilege_tool: Some("sudo"),
            overlap_ignore: Vec::new(),
            extra_mappings: Vec::new(),
        }
    }
}

/// Which dashboard pane owns ↑/↓: the sources table or the updates preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashPane {
    Sources,
    Updates,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Dashboard,
    Updates,
    /// Per-source package list (spec §10.3), entered with Enter on a dashboard row.
    Packages,
}

/// Which key map is active. The update screen has three: the plan view, the
/// confirm modal on top of it, and the post-execution result view. The package
/// list has two: the list itself and the fuzzy-filter input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Dashboard,
    Updates,
    Confirm,
    Result,
    Packages,
    PackageFilter,
    /// Inline log viewer (over the update screen or result view).
    LogView,
    /// Inline execution console: keys are forwarded to the running command.
    Exec,
}

/// The inline log viewer: file contents + scroll offset.
pub struct LogView {
    pub lines: Vec<String>,
    pub scroll: usize,
}

/// The inline execution console: streamed output; `done` set when the
/// session finished and the report is ready to hand to the result view.
pub struct ExecView {
    pub lines: Vec<String>,
    pub done: Option<ExecutionReport>,
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
    /// Rebuilt with every scan (never serialized) — powers the why pane.
    graph: DepGraph,
    /// Session options captured at startup.
    opts: AppOptions,
    pub theme: Theme,
    screen: Screen,
    dash_selected: Option<usize>,
    /// Update screen: per-source toggle (true = included in the plan).
    enabled: HashMap<SourceId, bool>,
    /// Update screen: cursor over `available_sources()`.
    update_cursor: usize,
    /// Update screen: the confirm modal is open.
    confirming: bool,
    /// Update screen: the last execution's outcome, shown until dismissed.
    report: Option<ExecutionReport>,
    /// Transient status line, cleared on the next key.
    flash: Option<String>,
    /// A blocking re-scan is about to run; the dashboard shows it instead of
    /// the scan age. (True async scanning is the v0.0.9 usability pass.)
    scanning: bool,
    /// Package list: which source's packages are shown.
    pkg_source: Option<SourceId>,
    /// Package list: cursor over `visible_packages()`.
    pkg_cursor: usize,
    /// Package list: the fuzzy filter query.
    pkg_filter: String,
    /// Package list: the filter input line has focus.
    filter_active: bool,
    /// Package list: the why side pane is open.
    why_open: bool,
    /// Spinner animation frame, advanced by the loop's poll tick.
    spinner_frame: usize,
    /// Inline log viewer, over the update screen.
    log_view: Option<LogView>,
    /// Inline execution console, over the update screen.
    exec: Option<ExecView>,
    /// Dashboard: which pane has focus (←/→ or h/l switches).
    dash_focus: DashPane,
    /// Dashboard: scroll offset of the pending-updates pane.
    updates_scroll: usize,
    /// Dashboard system pane: orphan candidates in the current scan.
    orphan_count: usize,
    /// Dashboard system pane: detected overlaps in the current scan.
    overlap_count: usize,
}

impl App {
    pub fn new(scan: ScanResult, theme: Theme, opts: AppOptions) -> Self {
        let dash_selected = if scan.sources.is_empty() {
            None
        } else {
            Some(0)
        };
        let enabled = default_toggles(&scan);
        let graph = DepGraph::build(&scan);
        let orphan_count = graph.orphans(&scan).len();
        let overlap_count =
            analyzer::detect_overlaps(&scan, &opts.overlap_ignore, &opts.extra_mappings).len();
        App {
            scan,
            graph,
            opts,
            theme,
            screen: Screen::Dashboard,
            dash_selected,
            enabled,
            update_cursor: 0,
            confirming: false,
            report: None,
            flash: None,
            scanning: false,
            pkg_source: None,
            pkg_cursor: 0,
            pkg_filter: String::new(),
            filter_active: false,
            why_open: false,
            spinner_frame: 0,
            log_view: None,
            exec: None,
            dash_focus: DashPane::Sources,
            updates_scroll: 0,
            orphan_count,
            overlap_count,
        }
    }

    /// Swap in a fresh scan (after a refresh), keeping cursors valid.
    pub fn replace_scan(&mut self, scan: ScanResult) {
        self.graph = DepGraph::build(&scan);
        self.orphan_count = self.graph.orphans(&scan).len();
        self.overlap_count =
            analyzer::detect_overlaps(&scan, &self.opts.overlap_ignore, &self.opts.extra_mappings)
                .len();
        self.scan = scan;
        let len = self.scan.sources.len();
        self.dash_selected = match (len, self.dash_selected) {
            (0, _) => None,
            (_, None) => Some(0),
            (n, Some(i)) => Some(i.min(n - 1)),
        };
        self.enabled = default_toggles(&self.scan);
        self.clamp_update_cursor();
        // A fresh scan invalidates an open confirm (the plan may have changed);
        // an existing report stays — the loop sets it *after* the refresh.
        self.confirming = false;
        self.flash = None;
        self.scanning = false;
        self.updates_scroll = 0;
        self.clamp_pkg_cursor();
    }

    // --- shared ---
    pub fn scan(&self) -> &ScanResult {
        &self.scan
    }
    pub fn screen(&self) -> Screen {
        self.screen
    }
    /// The active key map, derived from screen + modal/result state.
    pub fn input_mode(&self) -> InputMode {
        match self.screen {
            Screen::Dashboard => InputMode::Dashboard,
            Screen::Updates if self.exec.is_some() => InputMode::Exec,
            Screen::Updates if self.log_view.is_some() => InputMode::LogView,
            Screen::Updates if self.report.is_some() => InputMode::Result,
            Screen::Updates if self.confirming => InputMode::Confirm,
            Screen::Updates => InputMode::Updates,
            Screen::Packages if self.filter_active => InputMode::PackageFilter,
            Screen::Packages => InputMode::Packages,
        }
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

    pub fn is_scanning(&self) -> bool {
        self.scanning
    }
    pub fn set_scanning(&mut self, scanning: bool) {
        self.scanning = scanning;
    }

    /// Advance the spinner one frame (called on every loop tick).
    pub fn tick(&mut self) {
        self.spinner_frame = self.spinner_frame.wrapping_add(1);
    }
    /// The current spinner glyph from the active theme's frame set.
    pub fn spinner(&self) -> &'static str {
        let frames = self.theme.glyphs.spinner;
        frames[self.spinner_frame % frames.len()]
    }

    // --- screen navigation ---
    pub fn goto_updates(&mut self) {
        self.screen = Screen::Updates;
        self.clamp_update_cursor();
    }
    pub fn back_to_dashboard(&mut self) {
        self.screen = Screen::Dashboard;
    }

    /// Move the active screen's cursor forward / back. On the dashboard the
    /// focused pane decides: sources cursor vs updates-pane scroll.
    pub fn on_next(&mut self) {
        match (self.screen, self.dash_focus) {
            (Screen::Dashboard, DashPane::Sources) => self.select_next(),
            (Screen::Dashboard, DashPane::Updates) => {
                let max = self.updates_pane_rows().saturating_sub(1);
                self.updates_scroll = (self.updates_scroll + 1).min(max);
            }
            (Screen::Updates, _) => self.update_next(),
            (Screen::Packages, _) => self.pkg_move(1),
        }
    }
    pub fn on_prev(&mut self) {
        match (self.screen, self.dash_focus) {
            (Screen::Dashboard, DashPane::Sources) => self.select_prev(),
            (Screen::Dashboard, DashPane::Updates) => {
                self.updates_scroll = self.updates_scroll.saturating_sub(1);
            }
            (Screen::Updates, _) => self.update_prev(),
            (Screen::Packages, _) => self.pkg_move(-1),
        }
    }

    // --- dashboard pane focus + updates scroll ---
    pub fn dash_focus(&self) -> DashPane {
        self.dash_focus
    }
    pub fn focus_sources(&mut self) {
        self.dash_focus = DashPane::Sources;
    }
    pub fn focus_updates(&mut self) {
        self.dash_focus = DashPane::Updates;
    }
    pub fn updates_scroll(&self) -> usize {
        self.updates_scroll
    }

    /// Total renderable rows of the pending-updates pane (group headers +
    /// update rows + blank separators) — the scroll clamp.
    fn updates_pane_rows(&self) -> usize {
        let mut rows = 0;
        let mut groups = 0;
        for source in &self.scan.sources {
            let n = self
                .scan
                .updates
                .iter()
                .filter(|u| u.source_id == source.id)
                .count();
            if n > 0 {
                groups += 1;
                rows += 1 + n; // header + rows
            }
        }
        rows + groups.max(1) - 1 // blank line between groups
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

    pub fn privilege_tool(&self) -> Option<&'static str> {
        self.opts.privilege_tool
    }

    pub fn orphan_count(&self) -> usize {
        self.orphan_count
    }
    pub fn overlap_count(&self) -> usize {
        self.overlap_count
    }

    /// The pacman source scanned via the stale `-Qu` fallback, if so.
    pub fn stale_update_counts(&self) -> bool {
        self.scan
            .sources
            .iter()
            .any(|s| s.available && !s.accurate_updates)
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

    // --- inline log viewer ---
    pub fn log_view(&self) -> Option<&LogView> {
        self.log_view.as_ref()
    }
    pub fn open_log(&mut self, text: String) {
        self.log_view = Some(LogView {
            lines: text.lines().map(|l| l.to_string()).collect(),
            scroll: 0,
        });
    }
    pub fn close_log(&mut self) {
        self.log_view = None;
    }
    pub fn log_scroll(&mut self, delta: i64) {
        if let Some(view) = &mut self.log_view {
            let max = view.lines.len().saturating_sub(1) as i64;
            view.scroll = (view.scroll as i64 + delta).clamp(0, max) as usize;
        }
    }

    // --- inline execution console ---
    pub fn exec(&self) -> Option<&ExecView> {
        self.exec.as_ref()
    }
    pub fn start_exec(&mut self) {
        self.exec = Some(ExecView {
            lines: Vec::new(),
            done: None,
        });
    }
    pub fn exec_push_line(&mut self, line: String) {
        if let Some(view) = &mut self.exec {
            view.lines.push(line);
        }
    }
    pub fn exec_finish(&mut self, report: ExecutionReport) {
        if let Some(view) = &mut self.exec {
            view.done = Some(report);
        }
    }
    pub fn exec_is_done(&self) -> bool {
        self.exec.as_ref().is_some_and(|v| v.done.is_some())
    }
    /// Close the console; hand back the report for the result view.
    pub fn take_exec_report(&mut self) -> Option<ExecutionReport> {
        self.exec.take().and_then(|v| v.done)
    }

    // --- confirm modal + execution result ---
    pub fn is_confirming(&self) -> bool {
        self.confirming
    }
    pub fn open_confirm(&mut self) {
        self.confirming = true;
    }
    pub fn close_confirm(&mut self) {
        self.confirming = false;
    }

    pub fn report(&self) -> Option<&ExecutionReport> {
        self.report.as_ref()
    }
    pub fn set_report(&mut self, report: ExecutionReport) {
        self.confirming = false;
        self.report = Some(report);
    }
    pub fn dismiss_report(&mut self) {
        self.report = None;
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

    // --- package list ---
    /// Dashboard Enter: open the selected source's package list.
    pub fn open_packages(&mut self) {
        let Some(i) = self.dash_selected else { return };
        let Some(source) = self.scan.sources.get(i) else {
            return;
        };
        self.pkg_source = Some(source.id.clone());
        self.pkg_cursor = 0;
        self.pkg_filter.clear();
        self.filter_active = false;
        self.why_open = false;
        self.screen = Screen::Packages;
    }

    pub fn pkg_source(&self) -> Option<&SourceId> {
        self.pkg_source.as_ref()
    }
    pub fn pkg_cursor(&self) -> usize {
        self.pkg_cursor
    }
    pub fn pkg_filter(&self) -> &str {
        &self.pkg_filter
    }
    pub fn is_filter_active(&self) -> bool {
        self.filter_active
    }
    pub fn is_why_open(&self) -> bool {
        self.why_open
    }

    /// Packages of the open source: name-sorted, or fuzzy-filtered and
    /// score-ordered while a filter query is set.
    pub fn visible_packages(&self) -> Vec<&Package> {
        let Some(source) = &self.pkg_source else {
            return Vec::new();
        };
        let mut pkgs: Vec<&Package> = self
            .scan
            .packages
            .iter()
            .filter(|p| &p.source_id == source)
            .collect();
        if self.pkg_filter.is_empty() {
            pkgs.sort_by(|a, b| a.name.cmp(&b.name));
            return pkgs;
        }
        let mut scored: Vec<(fuzzy::Score, &Package)> = pkgs
            .into_iter()
            .filter_map(|p| fuzzy::matches(&self.pkg_filter, &p.name).map(|s| (s, p)))
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
        scored.into_iter().map(|(_, p)| p).collect()
    }

    /// Total packages of the open source, unfiltered (for the "n of N" line).
    pub fn pkg_total(&self) -> usize {
        match &self.pkg_source {
            Some(source) => self
                .scan
                .packages
                .iter()
                .filter(|p| &p.source_id == source)
                .count(),
            None => 0,
        }
    }

    /// Header summary for the open source: (explicit, dependencies, runtimes,
    /// total size in bytes).
    pub fn pkg_summary(&self) -> (usize, usize, usize, u64) {
        let Some(source) = &self.pkg_source else {
            return (0, 0, 0, 0);
        };
        let mut explicit = 0;
        let mut deps = 0;
        let mut runtimes = 0;
        let mut bytes = 0u64;
        for p in self.scan.packages.iter().filter(|p| &p.source_id == source) {
            match p.install_reason {
                crate::model::InstallReason::Explicit => explicit += 1,
                crate::model::InstallReason::Dependency => deps += 1,
                crate::model::InstallReason::Unknown => {}
            }
            if p.runtime {
                runtimes += 1;
            }
            bytes += p.size_bytes.unwrap_or(0);
        }
        (explicit, deps, runtimes, bytes)
    }

    pub fn selected_package(&self) -> Option<&Package> {
        self.visible_packages().get(self.pkg_cursor).copied()
    }

    /// The why report for the row under the cursor (feeds the side pane).
    pub fn why_report(&self) -> Option<WhyReport> {
        let name = self.selected_package()?.name.clone();
        Some(analyzer::why(
            &self.scan,
            &self.graph,
            &name,
            self.opts.why_depth,
        ))
    }

    /// Move the package cursor by `delta` rows (±1 nav, ±20 page), clamped.
    pub fn pkg_move(&mut self, delta: i32) {
        let len = self.visible_packages().len();
        if len == 0 {
            self.pkg_cursor = 0;
            return;
        }
        let next = self.pkg_cursor as i64 + delta as i64;
        self.pkg_cursor = next.clamp(0, len as i64 - 1) as usize;
    }

    pub fn start_filter(&mut self) {
        self.filter_active = true;
    }
    pub fn filter_push(&mut self, c: char) {
        self.pkg_filter.push(c);
        self.pkg_cursor = 0; // results reorder; snap to the best hit
    }
    pub fn filter_pop(&mut self) {
        self.pkg_filter.pop();
        self.clamp_pkg_cursor();
    }
    /// Enter: keep the query, return focus to the list.
    pub fn filter_accept(&mut self) {
        self.filter_active = false;
    }
    /// Esc while typing: drop the query entirely.
    pub fn filter_cancel(&mut self) {
        self.filter_active = false;
        self.pkg_filter.clear();
        self.clamp_pkg_cursor();
    }

    pub fn toggle_why(&mut self) {
        self.why_open = !self.why_open;
    }

    /// Esc on the list unwinds one layer at a time: why pane → filter → back.
    pub fn back_packages(&mut self) {
        if self.why_open {
            self.why_open = false;
        } else if !self.pkg_filter.is_empty() {
            self.pkg_filter.clear();
            self.clamp_pkg_cursor();
        } else {
            self.screen = Screen::Dashboard;
        }
    }

    fn clamp_pkg_cursor(&mut self) {
        let len = self.visible_packages().len();
        self.pkg_cursor = self.pkg_cursor.min(len.saturating_sub(1));
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
            runtime: false,
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
                accurate_updates: true,
            },
            Source {
                id: SourceId::flatpak_user(),
                kind: SourceKind::Flatpak {
                    scope: FlatpakScope::User,
                },
                available: true,
                last_scanned: None,
                accurate_updates: true,
            },
            Source {
                id: SourceId::flatpak_system(),
                kind: SourceKind::Flatpak {
                    scope: FlatpakScope::System,
                },
                available: false,
                last_scanned: None,
                accurate_updates: true,
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
        App::new(
            scan_with_sources(three_sources()),
            Theme::none(),
            AppOptions::test(),
        )
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
    fn tick_advances_and_wraps_the_spinner() {
        let mut app = app();
        let first = app.spinner();
        app.tick();
        assert_ne!(app.spinner(), first, "frame did not advance");
        let frames = app.theme.glyphs.spinner.len();
        for _ in 1..frames {
            app.tick();
        }
        assert_eq!(app.spinner(), first, "did not wrap around");
    }

    #[test]
    fn scanning_flag_sets_and_is_cleared_by_a_fresh_scan() {
        let mut app = app();
        assert!(!app.is_scanning());
        app.set_scanning(true);
        assert!(app.is_scanning());
        app.replace_scan(scan_with_sources(three_sources()));
        assert!(!app.is_scanning());
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

    // --- confirm modal + result ---
    fn sample_report() -> ExecutionReport {
        use crate::executor::{StepReport, StepStatus};
        ExecutionReport {
            steps: vec![StepReport {
                source_id: SourceId::flatpak_user(),
                targets: 1,
                status: StepStatus::Succeeded,
            }],
            log_path: std::path::PathBuf::from("/tmp/x.log"),
        }
    }

    #[test]
    fn input_mode_tracks_screen_modal_and_result() {
        let mut app = app();
        assert_eq!(app.input_mode(), InputMode::Dashboard);
        app.goto_updates();
        assert_eq!(app.input_mode(), InputMode::Updates);
        app.open_confirm();
        assert_eq!(app.input_mode(), InputMode::Confirm);
        app.set_report(sample_report());
        // A report outranks the (now closed) modal.
        assert_eq!(app.input_mode(), InputMode::Result);
        app.dismiss_report();
        assert_eq!(app.input_mode(), InputMode::Updates);
    }

    #[test]
    fn set_report_closes_the_confirm_modal() {
        let mut app = app();
        app.goto_updates();
        app.open_confirm();
        assert!(app.is_confirming());
        app.set_report(sample_report());
        assert!(!app.is_confirming());
        assert!(app.report().is_some());
    }

    #[test]
    fn close_confirm_returns_to_the_plan_view() {
        let mut app = app();
        app.goto_updates();
        app.open_confirm();
        app.close_confirm();
        assert_eq!(app.input_mode(), InputMode::Updates);
        assert!(app.report().is_none());
    }

    #[test]
    fn replace_scan_closes_an_open_confirm_but_keeps_the_report() {
        let mut app = app();
        app.goto_updates();
        app.open_confirm();
        app.replace_scan(scan_with_sources(three_sources()));
        assert!(!app.is_confirming());

        app.set_report(sample_report());
        app.replace_scan(scan_with_sources(three_sources()));
        assert!(app.report().is_some()); // the loop refreshes, then shows it
    }

    // --- package list ---
    #[test]
    fn enter_on_a_dashboard_row_opens_that_sources_packages() {
        let mut app = app();
        app.on_next(); // select flatpak-user
        app.open_packages();
        assert_eq!(app.screen(), Screen::Packages);
        assert_eq!(app.input_mode(), InputMode::Packages);
        assert_eq!(app.pkg_source(), Some(&SourceId::flatpak_user()));
        let names: Vec<&str> = app
            .visible_packages()
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(names, vec!["org.x.App"]);
    }

    #[test]
    fn visible_packages_sort_by_name_and_cursor_clamps() {
        let mut app = app();
        app.open_packages(); // pacman: a, b
        let names: Vec<&str> = app
            .visible_packages()
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(names, vec!["a", "b"]);
        app.pkg_move(1);
        assert_eq!(app.pkg_cursor(), 1);
        app.pkg_move(5);
        assert_eq!(app.pkg_cursor(), 1); // clamped
        app.pkg_move(-10);
        assert_eq!(app.pkg_cursor(), 0);
        assert_eq!(app.pkg_total(), 2);
    }

    #[test]
    fn fuzzy_filter_narrows_and_snaps_the_cursor() {
        let mut app = app();
        app.open_packages();
        app.pkg_move(1); // cursor on "b"
        app.start_filter();
        assert_eq!(app.input_mode(), InputMode::PackageFilter);
        app.filter_push('a');
        assert_eq!(app.pkg_cursor(), 0); // snapped to best hit
        let names: Vec<&str> = app
            .visible_packages()
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(names, vec!["a"]);
        app.filter_accept();
        assert_eq!(app.input_mode(), InputMode::Packages);
        assert_eq!(app.pkg_filter(), "a");
    }

    #[test]
    fn filter_cancel_drops_the_query_but_accept_keeps_it() {
        let mut app = app();
        app.open_packages();
        app.start_filter();
        app.filter_push('a');
        app.filter_cancel();
        assert_eq!(app.pkg_filter(), "");
        assert_eq!(app.visible_packages().len(), 2);
    }

    #[test]
    fn esc_unwinds_why_then_filter_then_screen() {
        let mut app = app();
        app.open_packages();
        app.start_filter();
        app.filter_push('a');
        app.filter_accept();
        app.toggle_why();
        assert!(app.is_why_open());

        app.back_packages(); // 1: closes the why pane
        assert!(!app.is_why_open());
        assert_eq!(app.pkg_filter(), "a");

        app.back_packages(); // 2: clears the filter
        assert_eq!(app.pkg_filter(), "");
        assert_eq!(app.screen(), Screen::Packages);

        app.back_packages(); // 3: back to the dashboard
        assert_eq!(app.screen(), Screen::Dashboard);
    }

    #[test]
    fn why_report_follows_the_cursor() {
        let mut app = app();
        app.open_packages();
        let report = app.why_report().expect("selected row has a report");
        match report {
            crate::analyzer::WhyReport::Found(p) => assert_eq!(p.package, "a"),
            other => panic!("expected pacman report, got {other:?}"),
        }
    }

    #[test]
    fn exec_and_log_states_drive_the_input_mode() {
        let mut app = app();
        app.goto_updates();
        app.open_log("a\nb\nc".to_string());
        assert_eq!(app.input_mode(), InputMode::LogView);
        app.log_scroll(5);
        assert_eq!(app.log_view().unwrap().scroll, 2); // clamped to last line
        app.log_scroll(-10);
        assert_eq!(app.log_view().unwrap().scroll, 0);
        app.close_log();
        assert_eq!(app.input_mode(), InputMode::Updates);

        app.start_exec();
        assert_eq!(app.input_mode(), InputMode::Exec);
        assert!(!app.exec_is_done());
        app.exec_push_line("out".to_string());
        app.exec_finish(sample_report());
        assert!(app.exec_is_done());
        let report = app.take_exec_report().expect("report");
        assert_eq!(report.succeeded(), 1);
        assert!(app.exec().is_none());
        assert_eq!(app.input_mode(), InputMode::Updates);
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
            accurate_updates: true,
        }]));
        assert_eq!(app.update_cursor(), 0);
        assert!(app.is_enabled(&SourceId::pacman())); // toggles reset to on
    }
}
