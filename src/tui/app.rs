//! Application state for the multi-screen TUI.
//!
//! `App` owns everything the screens draw (dev-notes §5): the `ScanResult`, the
//! resolved `Theme`, which `Screen` is active, the dashboard cursor + update
//! toggles, and a transient flash message. Rendering borrows `&App` and never
//! mutates; the event loop is the only mutator.

use std::collections::HashMap;

use crate::analyzer::{self, DepGraph, WhyReport};
use crate::config::{Config, ExtraMapping};
use crate::executor::ExecutionReport;
use crate::fuzzy;
use crate::model::OverlapCandidate;
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
    /// Per-source package list (spec §10.3), entered with Enter on a dashboard row.
    Packages,
    /// Overlap candidates (spec §10.3, roadmap v0.1.4), entered with `o`.
    Overlaps,
}

/// Package-list sort modes; `s` cycles them in this order (user decision
/// 2026-07-11). Fuzzy filtering overrides the sort while a query is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PkgSort {
    /// Packages with a pending update first, then the rest (default).
    UpdatesFirst,
    /// Grouped by install reason: explicit / dependencies / apps / runtimes.
    Reason,
    /// Plain A–Z.
    Name,
    /// Largest install size first.
    Size,
}

impl PkgSort {
    pub fn label(self) -> &'static str {
        match self {
            PkgSort::UpdatesFirst => "updates",
            PkgSort::Reason => "reason",
            PkgSort::Name => "name",
            PkgSort::Size => "size",
        }
    }
    fn next(self) -> Self {
        match self {
            PkgSort::UpdatesFirst => PkgSort::Reason,
            PkgSort::Reason => PkgSort::Name,
            PkgSort::Name => PkgSort::Size,
            PkgSort::Size => PkgSort::UpdatesFirst,
        }
    }
}

/// One renderable package-list row: a dim section header (group label +
/// count, never selectable) or a package.
pub enum PkgRow<'a> {
    Header(&'static str, usize),
    Pkg(&'a Package),
}

/// Which key map is active. The exec console and log viewer are overlays
/// that outrank the screen (they can cover the dashboard). The package
/// list has two: the list itself and the fuzzy-filter input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Dashboard,
    Packages,
    PackageFilter,
    Overlaps,
    /// Inline log viewer overlay.
    LogView,
    /// Inline execution console: keys pass through to the running command.
    Exec,
}

/// The inline log viewer: file contents + scroll offset.
pub struct LogView {
    pub lines: Vec<String>,
    pub scroll: usize,
}

/// The inline execution console: a vt100 screen fed by the pty's raw output
/// (exact passthrough); `done` set when the session finished.
pub struct ExecView {
    pub parser: vt100::Parser,
    pub done: Option<ExecutionReport>,
}

/// One source's row in the dashboard table — a view-model derived from the scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRow {
    pub id: String,
    pub installed: usize,
    pub updates: usize,
    pub available: bool,
    /// Included in the update plan (Space toggles; None = nothing to update).
    pub enabled: Option<bool>,
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
    /// Dashboard: per-source update toggle (true = included in the plan).
    enabled: HashMap<SourceId, bool>,
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
    /// Package list: active sort mode (persists across list opens).
    pkg_sort: PkgSort,
    /// Spinner animation frame, advanced by the loop's poll tick.
    spinner_frame: usize,
    /// Inline log viewer overlay (any screen).
    log_view: Option<LogView>,
    /// Inline execution console overlay (any screen).
    exec: Option<ExecView>,
    /// Dashboard: which pane has focus (←/→ or h/l switches).
    dash_focus: DashPane,
    /// Dashboard: scroll offset of the pending-updates pane.
    updates_scroll: usize,
    /// Package list: first visible row (scrolloff keeps the cursor away from
    /// the viewport edges).
    pkg_offset: usize,
    /// Package list: table body rows currently on screen (set by the event
    /// loop from the terminal size — the render side never mutates).
    pkg_viewport: usize,
    /// Dashboard system pane: orphan candidates in the current scan.
    orphan_count: usize,
    /// Detected overlaps, recomputed per scan (never cached — spec §6.6).
    overlaps: Vec<OverlapCandidate>,
    /// Overlap screen: cursor over `overlaps`.
    overlap_cursor: usize,
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
        let overlaps = analyzer::detect_overlaps(&scan, &opts.overlap_ignore, &opts.extra_mappings);
        App {
            scan,
            graph,
            opts,
            theme,
            screen: Screen::Dashboard,
            dash_selected,
            enabled,
            flash: None,
            scanning: false,
            pkg_source: None,
            pkg_cursor: 0,
            pkg_filter: String::new(),
            filter_active: false,
            why_open: false,
            pkg_sort: PkgSort::UpdatesFirst,
            spinner_frame: 0,
            log_view: None,
            exec: None,
            dash_focus: DashPane::Sources,
            updates_scroll: 0,
            pkg_offset: 0,
            pkg_viewport: 0,
            orphan_count,
            overlaps,
            overlap_cursor: 0,
        }
    }

    /// Swap in a fresh scan (after a refresh), keeping cursors valid.
    pub fn replace_scan(&mut self, scan: ScanResult) {
        self.graph = DepGraph::build(&scan);
        self.orphan_count = self.graph.orphans(&scan).len();
        self.overlaps =
            analyzer::detect_overlaps(&scan, &self.opts.overlap_ignore, &self.opts.extra_mappings);
        self.overlap_cursor = self
            .overlap_cursor
            .min(self.overlaps.len().saturating_sub(1));
        self.scan = scan;
        let len = self.scan.sources.len();
        self.dash_selected = match (len, self.dash_selected) {
            (0, _) => None,
            (_, None) => Some(0),
            (n, Some(i)) => Some(i.min(n - 1)),
        };
        self.enabled = default_toggles(&self.scan);
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
    /// The active key map: overlays first, then the screen.
    pub fn input_mode(&self) -> InputMode {
        // Overlays outrank the screen: the console and the log viewer can
        // cover any screen (the dashboard owns the update flow now).
        if self.exec.is_some() {
            return InputMode::Exec;
        }
        if self.log_view.is_some() {
            return InputMode::LogView;
        }
        match self.screen {
            Screen::Dashboard => InputMode::Dashboard,
            Screen::Packages if self.filter_active => InputMode::PackageFilter,
            Screen::Packages => InputMode::Packages,
            Screen::Overlaps => InputMode::Overlaps,
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

    /// Move the active screen's cursor forward / back. On the dashboard the
    /// focused pane decides: sources cursor vs updates-pane scroll.
    pub fn on_next(&mut self) {
        match (self.screen, self.dash_focus) {
            (Screen::Dashboard, DashPane::Sources) => self.select_next(),
            (Screen::Dashboard, DashPane::Updates) => {
                let max = self.updates_pane_rows().saturating_sub(1);
                self.updates_scroll = (self.updates_scroll + 1).min(max);
            }
            (Screen::Packages, _) => self.pkg_move(1),
            (Screen::Overlaps, _) => {
                let max = self.overlaps.len().saturating_sub(1);
                self.overlap_cursor = (self.overlap_cursor + 1).min(max);
            }
        }
    }
    pub fn on_prev(&mut self) {
        match (self.screen, self.dash_focus) {
            (Screen::Dashboard, DashPane::Sources) => self.select_prev(),
            (Screen::Dashboard, DashPane::Updates) => {
                self.updates_scroll = self.updates_scroll.saturating_sub(1);
            }
            (Screen::Packages, _) => self.pkg_move(-1),
            (Screen::Overlaps, _) => {
                self.overlap_cursor = self.overlap_cursor.saturating_sub(1);
            }
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

    /// Renderable rows of the pending-updates pane — the scroll clamp. The
    /// pane shows only the selected source's updates (one row each).
    fn updates_pane_rows(&self) -> usize {
        match self.dash_source() {
            Some(source) => self.updates_for(&source.id).len(),
            None => 0,
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
                let enabled = (s.available && summary.updates > 0).then(|| self.is_enabled(&s.id));
                SourceRow {
                    id: s.id.to_string(),
                    installed: summary.installed,
                    updates: summary.updates,
                    available: s.available,
                    enabled,
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
        // The updates pane shows the selected source — restart its scroll.
        self.updates_scroll = 0;
    }
    fn select_prev(&mut self) {
        if self.scan.sources.is_empty() {
            return;
        }
        self.dash_selected = Some(match self.dash_selected {
            Some(0) | None => 0,
            Some(i) => i - 1,
        });
        self.updates_scroll = 0;
    }

    /// The source selected on the dashboard (drives the updates pane).
    pub fn dash_source(&self) -> Option<&Source> {
        self.scan.sources.get(self.dash_selected?)
    }

    // --- overlap screen (spec §10.3, v0.1.4) ---
    pub fn open_overlaps(&mut self) {
        self.overlap_cursor = self
            .overlap_cursor
            .min(self.overlaps.len().saturating_sub(1));
        self.screen = Screen::Overlaps;
    }
    pub fn overlaps(&self) -> &[OverlapCandidate] {
        &self.overlaps
    }
    pub fn overlap_cursor(&self) -> usize {
        self.overlap_cursor
    }
    pub fn selected_overlap(&self) -> Option<&OverlapCandidate> {
        self.overlaps.get(self.overlap_cursor)
    }
    /// Esc anywhere non-dashboard unwinds toward the dashboard.
    pub fn close_overlaps(&mut self) {
        self.screen = Screen::Dashboard;
    }

    // --- update toggles (dashboard-owned) ---
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
        self.overlaps.len()
    }

    /// The pacman source scanned via the stale `-Qu` fallback, if so.
    pub fn stale_update_counts(&self) -> bool {
        self.scan
            .sources
            .iter()
            .any(|s| s.available && !s.accurate_updates)
    }

    /// Space on the dashboard: toggle the selected source in/out of the
    /// update plan. Only sources with something to update toggle.
    pub fn toggle_selected(&mut self) {
        let Some(source) = self.dash_source() else {
            return;
        };
        let id = source.id.clone();
        if !source.available || self.updates_for(&id).is_empty() {
            self.set_flash(format!("{id} has nothing to update"));
            return;
        }
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
    /// Open the console with a vt100 screen matching the pty size.
    pub fn start_exec(&mut self, rows: u16, cols: u16) {
        self.exec = Some(ExecView {
            parser: vt100::Parser::new(rows.max(2), cols.max(20), 0),
            done: None,
        });
    }
    /// Feed a chunk of raw pty output into the console's screen.
    pub fn exec_feed(&mut self, bytes: &[u8]) {
        if let Some(view) = &mut self.exec {
            view.parser.process(bytes);
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
    /// Close the console; hand back the final report.
    pub fn take_exec_report(&mut self) -> Option<ExecutionReport> {
        self.exec.take().and_then(|v| v.done)
    }
    /// After the console is dismissed: back to the dashboard with a one-line
    /// summary flash (the result view died with the confirm modal — user
    /// decision 2026-07-08).
    pub fn finish_update(&mut self, report: &ExecutionReport) {
        self.screen = Screen::Dashboard;
        let failed = report.failed();
        let executed = report.executed();
        self.flash = Some(if executed == 0 {
            "nothing was executed".to_string()
        } else if failed == 0 {
            format!(
                "update finished — {executed} source{} succeeded (l for the log)",
                if executed == 1 { "" } else { "s" }
            )
        } else {
            format!("update finished — {failed} of {executed} sources FAILED (l for the log)")
        });
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
        self.pkg_offset = 0;
        self.pkg_filter.clear();
        self.filter_active = false;
        self.why_open = false;
        self.screen = Screen::Packages;
    }

    pub fn pkg_source(&self) -> Option<&SourceId> {
        self.pkg_source.as_ref()
    }
    /// The render side selects by `pkg_row_index()`; tests assert the raw
    /// package-space cursor through this.
    #[cfg_attr(not(test), expect(dead_code))]
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

    /// Does this package (of the open source) have a pending update?
    pub fn pkg_pending(&self, name: &str) -> bool {
        match &self.pkg_source {
            Some(src) => self
                .scan
                .updates
                .iter()
                .any(|u| &u.source_id == src && u.package_name == name),
            None => false,
        }
    }

    /// The install-reason group a package belongs to (`Reason` sort). Flatpak
    /// apps scan with an Unknown reason — they group as "apps", not
    /// "unknown" (that label is a pacman data gap).
    fn reason_group(p: &Package) -> &'static str {
        if p.runtime {
            "runtimes"
        } else {
            match p.install_reason {
                crate::model::InstallReason::Explicit => "explicit",
                crate::model::InstallReason::Dependency => "dependencies",
                crate::model::InstallReason::Unknown => {
                    if p.source_id == SourceId::pacman() {
                        "unknown"
                    } else {
                        "apps"
                    }
                }
            }
        }
    }

    /// The open source's packages in display order, grouped for the active
    /// sort. An empty label means "no header row". Fuzzy filtering overrides
    /// the sort with score order.
    fn grouped(&self) -> Vec<(&'static str, Vec<&Package>)> {
        let Some(source) = &self.pkg_source else {
            return Vec::new();
        };
        let mut pkgs: Vec<&Package> = self
            .scan
            .packages
            .iter()
            .filter(|p| &p.source_id == source)
            .collect();
        if !self.pkg_filter.is_empty() {
            let mut scored: Vec<(fuzzy::Score, &Package)> = pkgs
                .into_iter()
                .filter_map(|p| fuzzy::matches(&self.pkg_filter, &p.name).map(|s| (s, p)))
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
            return vec![("", scored.into_iter().map(|(_, p)| p).collect())];
        }
        pkgs.sort_by(|a, b| a.name.cmp(&b.name));
        match self.pkg_sort {
            PkgSort::Name => vec![("", pkgs)],
            PkgSort::Size => {
                let mut pkgs = pkgs;
                pkgs.sort_by(|a, b| {
                    b.size_bytes
                        .unwrap_or(0)
                        .cmp(&a.size_bytes.unwrap_or(0))
                        .then_with(|| a.name.cmp(&b.name))
                });
                vec![("", pkgs)]
            }
            PkgSort::UpdatesFirst => {
                let (pending, rest): (Vec<_>, Vec<_>) =
                    pkgs.into_iter().partition(|p| self.pkg_pending(&p.name));
                if pending.is_empty() {
                    // Nothing pending: headers would be noise.
                    vec![("", rest)]
                } else {
                    vec![("pending updates", pending), ("up to date", rest)]
                }
            }
            PkgSort::Reason => {
                let mut out = Vec::new();
                for group in ["explicit", "dependencies", "apps", "runtimes", "unknown"] {
                    let members: Vec<&Package> = pkgs
                        .iter()
                        .copied()
                        .filter(|p| Self::reason_group(p) == group)
                        .collect();
                    if !members.is_empty() {
                        out.push((group, members));
                    }
                }
                out
            }
        }
    }

    /// Packages of the open source in display order (the cursor's domain —
    /// headers are a render concern, see `pkg_rows`).
    pub fn visible_packages(&self) -> Vec<&Package> {
        self.grouped().into_iter().flat_map(|(_, v)| v).collect()
    }

    /// Renderable rows: packages in display order with dim section headers
    /// interleaved (only labeled, non-empty groups get one).
    pub fn pkg_rows(&self) -> Vec<PkgRow<'_>> {
        let mut out = Vec::new();
        for (label, members) in self.grouped() {
            if !label.is_empty() {
                out.push(PkgRow::Header(label, members.len()));
            }
            out.extend(members.into_iter().map(PkgRow::Pkg));
        }
        out
    }

    /// The table-row index of the cursor's package (accounts for the header
    /// rows above it) — feeds the table selection and the scrolloff math.
    pub fn pkg_row_index(&self) -> usize {
        let mut pkgs_seen = 0;
        for (i, row) in self.pkg_rows().iter().enumerate() {
            if let PkgRow::Pkg(_) = row {
                if pkgs_seen == self.pkg_cursor {
                    return i;
                }
                pkgs_seen += 1;
            }
        }
        0
    }

    pub fn pkg_sort(&self) -> PkgSort {
        self.pkg_sort
    }

    /// `s`: next sort mode; the cursor snaps home (the list reorders).
    pub fn cycle_sort(&mut self) {
        self.pkg_sort = self.pkg_sort.next();
        self.pkg_cursor = 0;
        self.pkg_offset = 0;
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
            self.pkg_offset = 0;
            return;
        }
        let next = self.pkg_cursor as i64 + delta as i64;
        self.pkg_cursor = next.clamp(0, len as i64 - 1) as usize;
        self.sync_pkg_offset();
    }

    /// Told by the event loop how many table body rows fit — the offset math
    /// needs the viewport, and render fns never mutate.
    pub fn set_pkg_viewport(&mut self, rows: usize) {
        if self.pkg_viewport != rows {
            self.pkg_viewport = rows;
            self.sync_pkg_offset();
        }
    }

    pub fn pkg_offset(&self) -> usize {
        self.pkg_offset
    }

    fn sync_pkg_offset(&mut self) {
        // Row space, not package space — header rows scroll like any other.
        self.pkg_offset = scrolloff(
            self.pkg_offset,
            self.pkg_row_index(),
            self.pkg_rows().len(),
            self.pkg_viewport,
        );
    }

    pub fn start_filter(&mut self) {
        self.filter_active = true;
    }
    pub fn filter_push(&mut self, c: char) {
        self.pkg_filter.push(c);
        self.pkg_cursor = 0; // results reorder; snap to the best hit
        self.pkg_offset = 0;
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
        self.sync_pkg_offset();
    }
}

/// Rows the cursor keeps between itself and the viewport edges (vim
/// `scrolloff`): scrolling down parks it MARGIN rows above the bottom, and
/// reversing walks it up through the view before the list scrolls again.
const SCROLL_MARGIN: usize = 4;

/// Next scroll offset for a cursor list with a scroll margin. Pure: previous
/// offset in, new offset out; clamps to the list bounds so short lists and
/// tiny viewports degrade to no scrolling.
fn scrolloff(offset: usize, cursor: usize, len: usize, viewport: usize) -> usize {
    if viewport == 0 || len <= viewport {
        return 0;
    }
    // A viewport shorter than 2×margin+1 can't honor the full margin; shrink
    // it so the clamp bounds never cross.
    let margin = SCROLL_MARGIN.min(viewport.saturating_sub(1) / 2);
    let min = (cursor + margin + 1).saturating_sub(viewport); // cursor ≥ margin from the bottom
    let max = cursor.saturating_sub(margin); // cursor ≥ margin from the top
    offset.clamp(min, max).min(len - viewport)
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
            flatpak_profile_sizes: Default::default(),
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

    // --- dashboard update toggles ---
    #[test]
    fn source_rows_carry_the_toggle_state() {
        // pacman has the one update → toggled on; flatpak-user is clean and
        // flatpak-system unavailable → nothing to toggle (None).
        let rows = app().rows();
        assert_eq!(rows[0].enabled, Some(true));
        assert_eq!(rows[1].enabled, None);
        assert_eq!(rows[2].enabled, None);
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
    fn toggling_the_selected_source_removes_it_from_the_plan() {
        let mut app = app();
        // Dashboard cursor row 0 = pacman (the source with updates).
        app.toggle_selected();
        assert!(!app.is_enabled(&SourceId::pacman()));
        assert!(app.update_plan().is_empty()); // flatpak-user has no updates
        app.toggle_selected();
        assert!(app.is_enabled(&SourceId::pacman()));
        assert_eq!(app.update_plan().source_count(), 1);
    }

    #[test]
    fn toggling_a_clean_source_flashes_instead() {
        let mut app = app();
        app.on_next(); // row 1 = flatpak-user, no updates
        app.toggle_selected();
        assert!(
            app.is_enabled(&SourceId::flatpak_user()),
            "toggle must not flip"
        );
        let flash = app.flash().expect("explanatory flash");
        assert!(
            flash.contains("flatpak-user has nothing to update"),
            "{flash}"
        );
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

    // --- post-update handoff ---
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
    fn input_mode_tracks_the_screen() {
        let mut app = app();
        assert_eq!(app.input_mode(), InputMode::Dashboard);
        app.open_packages();
        assert_eq!(app.input_mode(), InputMode::Packages);
        app.back_packages();
        assert_eq!(app.input_mode(), InputMode::Dashboard);
    }

    #[test]
    fn finish_update_lands_on_the_dashboard_with_a_summary_flash() {
        let mut app = app();
        app.finish_update(&sample_report());
        assert_eq!(app.screen(), Screen::Dashboard);
        let flash = app.flash().expect("summary flash");
        assert!(flash.contains("1 source succeeded"), "{flash}");
    }

    #[test]
    fn finish_update_calls_out_failures() {
        use crate::executor::{StepReport, StepStatus};
        let mut app = app();
        let report = ExecutionReport {
            steps: vec![
                StepReport {
                    source_id: SourceId::pacman(),
                    targets: 3,
                    status: StepStatus::Failed {
                        detail: "exit 1".to_string(),
                    },
                },
                StepReport {
                    source_id: SourceId::flatpak_user(),
                    targets: 1,
                    status: StepStatus::Succeeded,
                },
            ],
            log_path: std::path::PathBuf::from("/tmp/x.log"),
        };
        app.finish_update(&report);
        let flash = app.flash().expect("summary flash");
        assert!(flash.contains("1 of 2 sources FAILED"), "{flash}");
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

    // --- overlap screen ---
    fn overlap_scan() -> ScanResult {
        let mut s = scan_with_sources(three_sources());
        let mut ff_native = pkg("firefox", SourceId::pacman());
        ff_native.install_reason = InstallReason::Explicit;
        let mut ff_flat = pkg("org.mozilla.firefox", SourceId::flatpak_user());
        ff_flat.description = Some("Firefox".to_string());
        let mut gimp_native = pkg("gimp", SourceId::pacman());
        gimp_native.install_reason = InstallReason::Explicit;
        let mut gimp_flat = pkg("org.gimp.GIMP", SourceId::flatpak_user());
        gimp_flat.description = Some("GIMP".to_string());
        s.packages = vec![ff_native, ff_flat, gimp_native, gimp_flat];
        s
    }

    #[test]
    fn overlaps_recompute_per_scan_and_drive_the_screen() {
        let mut app = App::new(
            scan_with_sources(three_sources()),
            Theme::none(),
            AppOptions::test(),
        );
        assert_eq!(app.overlap_count(), 0);
        app.replace_scan(overlap_scan());
        assert_eq!(app.overlap_count(), 2);

        app.open_overlaps();
        assert_eq!(app.screen(), Screen::Overlaps);
        assert_eq!(app.input_mode(), InputMode::Overlaps);
        // Sorted by display name: Firefox, GIMP.
        assert_eq!(app.selected_overlap().unwrap().display_name, "Firefox");
        app.on_next();
        assert_eq!(app.selected_overlap().unwrap().display_name, "GIMP");
        app.on_next(); // clamped
        assert_eq!(app.overlap_cursor(), 1);
        app.on_prev();
        app.on_prev(); // clamped at 0
        assert_eq!(app.overlap_cursor(), 0);
        app.close_overlaps();
        assert_eq!(app.screen(), Screen::Dashboard);
    }

    #[test]
    fn overlap_cursor_survives_a_shrinking_rescan() {
        let mut app = App::new(overlap_scan(), Theme::none(), AppOptions::test());
        app.open_overlaps();
        app.on_next(); // cursor 1
        app.replace_scan(scan_with_sources(three_sources())); // 0 overlaps
        assert_eq!(app.overlap_cursor(), 0);
        assert!(app.selected_overlap().is_none());
    }

    // --- package-list sort modes ---
    fn sorted_app() -> App {
        // pacman: a (update pending), b explicit, c dependency w/ sizes.
        let mut s = scan_with_sources(three_sources());
        let mut b = pkg("b", SourceId::pacman());
        b.install_reason = InstallReason::Explicit;
        b.size_bytes = Some(10);
        let mut c = pkg("c", SourceId::pacman());
        c.install_reason = InstallReason::Dependency;
        c.size_bytes = Some(99);
        let mut a = pkg("a", SourceId::pacman());
        a.install_reason = InstallReason::Explicit;
        s.packages = vec![a, b, c];
        let mut app = App::new(s, Theme::none(), AppOptions::test());
        app.open_packages();
        app
    }

    fn row_names(app: &App) -> Vec<String> {
        app.pkg_rows()
            .iter()
            .map(|r| match r {
                PkgRow::Header(label, n) => format!("[{label} {n}]"),
                PkgRow::Pkg(p) => p.name.clone(),
            })
            .collect()
    }

    #[test]
    fn updates_first_puts_pending_on_top_with_headers() {
        let app = sorted_app();
        assert_eq!(
            row_names(&app),
            vec!["[pending updates 1]", "a", "[up to date 2]", "b", "c"]
        );
        // Cursor domain is packages only; row index skips the headers.
        let names: Vec<&str> = app
            .visible_packages()
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(names, vec!["a", "b", "c"]);
        assert_eq!(app.pkg_row_index(), 1); // cursor 0 = "a", under a header
        assert!(app.pkg_pending("a"));
        assert!(!app.pkg_pending("b"));
    }

    #[test]
    fn updates_first_drops_headers_when_nothing_is_pending() {
        let mut s = scan_with_sources(three_sources());
        s.packages = vec![pkg("a", SourceId::pacman()), pkg("b", SourceId::pacman())];
        s.updates.clear();
        let mut app = App::new(s, Theme::none(), AppOptions::test());
        app.open_packages();
        assert_eq!(row_names(&app), vec!["a", "b"], "headers must vanish");
    }

    #[test]
    fn reason_sort_groups_by_install_reason() {
        let mut app = sorted_app();
        app.cycle_sort(); // updates → reason
        assert_eq!(app.pkg_sort(), PkgSort::Reason);
        assert_eq!(
            row_names(&app),
            vec!["[explicit 2]", "a", "b", "[dependencies 1]", "c"]
        );
    }

    #[test]
    fn reason_sort_labels_flatpak_apps_and_runtimes() {
        let mut s = scan_with_sources(three_sources());
        let mut rt = pkg("org.gnome.Platform", SourceId::flatpak_user());
        rt.runtime = true;
        s.packages = vec![pkg("org.x.App", SourceId::flatpak_user()), rt];
        let mut app = App::new(s, Theme::none(), AppOptions::test());
        app.on_next(); // flatpak-user
        app.open_packages();
        app.cycle_sort();
        assert_eq!(
            row_names(&app),
            vec![
                "[apps 1]",
                "org.x.App",
                "[runtimes 1]",
                "org.gnome.Platform"
            ]
        );
    }

    #[test]
    fn size_sort_is_largest_first_and_name_sort_is_plain() {
        let mut app = sorted_app();
        app.cycle_sort(); // reason
        app.cycle_sort(); // name
        assert_eq!(app.pkg_sort(), PkgSort::Name);
        assert_eq!(row_names(&app), vec!["a", "b", "c"]);
        app.cycle_sort(); // size
        assert_eq!(app.pkg_sort(), PkgSort::Size);
        // c (99) > b (10) > a (None → last).
        assert_eq!(row_names(&app), vec!["c", "b", "a"]);
        app.cycle_sort(); // wraps to updates-first
        assert_eq!(app.pkg_sort(), PkgSort::UpdatesFirst);
    }

    #[test]
    fn cycle_sort_snaps_the_cursor_home() {
        let mut app = sorted_app();
        app.pkg_move(2);
        assert_eq!(app.pkg_cursor(), 2);
        app.cycle_sort();
        assert_eq!(app.pkg_cursor(), 0);
        assert_eq!(app.pkg_offset(), 0);
    }

    #[test]
    fn filter_overrides_the_sort_without_headers() {
        let mut app = sorted_app();
        app.start_filter();
        app.filter_push('b');
        assert_eq!(row_names(&app), vec!["b"], "no headers while filtering");
    }

    #[test]
    fn scrolloff_counts_header_rows() {
        // 50 packages, one pending → 52 rows. The offset math must run in
        // row space or the cursor drifts by the header count.
        let mut s = scan_with_sources(three_sources());
        s.packages = (0..50)
            .map(|i| pkg(&format!("p{i:02}"), SourceId::pacman()))
            .collect();
        s.updates = vec![upd("p00", SourceId::pacman())];
        let mut app = App::new(s, Theme::none(), AppOptions::test());
        app.open_packages();
        app.set_pkg_viewport(10);
        assert_eq!(app.pkg_rows().len(), 52);
        app.pkg_move(20); // cursor pkg 20 → row 22
        assert_eq!(app.pkg_row_index(), 22);
        // Row 22 must sit 4 rows above the bottom: offset 22-(10-1-4) = 17.
        assert_eq!(app.pkg_offset(), 17);
    }

    // --- scrolloff ---
    #[test]
    fn scrolloff_is_inert_when_everything_fits() {
        assert_eq!(scrolloff(0, 3, 5, 10), 0);
        assert_eq!(scrolloff(7, 3, 5, 10), 0, "stale offset resets");
        assert_eq!(scrolloff(0, 3, 5, 0), 0, "zero viewport");
    }

    #[test]
    fn scrolloff_keeps_a_margin_from_the_bottom_going_down() {
        // 100 rows, 20 visible. Walking down from the top: no scroll until
        // the cursor would come within 4 rows of the bottom edge.
        let mut offset = 0;
        for cursor in 0..=15 {
            offset = scrolloff(offset, cursor, 100, 20);
            assert_eq!(offset, 0, "cursor {cursor} scrolled too early");
        }
        offset = scrolloff(offset, 16, 100, 20);
        assert_eq!(offset, 1, "margin row reached — start scrolling");
        offset = scrolloff(offset, 40, 100, 20);
        assert_eq!(offset, 25, "cursor sits 4 rows above the bottom");
    }

    #[test]
    fn scrolloff_reversing_walks_up_before_scrolling() {
        // Deep in the list, cursor 4 from the bottom (offset 25, cursor 40).
        // Moving up: the view holds still until the cursor is 4 from the top.
        let mut offset = 25;
        for cursor in (30..40).rev() {
            offset = scrolloff(offset, cursor, 100, 20);
            assert_eq!(offset, 25, "cursor {cursor} scrolled too early");
        }
        offset = scrolloff(offset, 28, 100, 20); // 4 from the top → scroll
        assert_eq!(offset, 24);
        offset = scrolloff(offset, 10, 100, 20);
        assert_eq!(offset, 6, "cursor stays 4 rows below the top");
    }

    #[test]
    fn scrolloff_clamps_at_the_list_edges() {
        assert_eq!(scrolloff(0, 99, 100, 20), 80, "end of list");
        assert_eq!(scrolloff(80, 0, 100, 20), 0, "back to the top");
        // Tiny viewport: margin shrinks instead of the bounds crossing.
        assert_eq!(scrolloff(0, 50, 100, 3), 49);
    }

    #[test]
    fn pkg_move_applies_scrolloff_through_the_viewport() {
        let mut s = scan_with_sources(three_sources());
        s.packages = (0..50)
            .map(|i| pkg(&format!("p{i:02}"), SourceId::pacman()))
            .collect();
        let mut app = App::new(s, Theme::none(), AppOptions::test());
        app.open_packages();
        app.set_pkg_viewport(10);
        assert_eq!(app.pkg_offset(), 0);
        app.pkg_move(20);
        // Cursor 20, viewport 10 → cursor 4 rows above the bottom edge.
        assert_eq!(app.pkg_offset(), 15);
        app.pkg_move(-1);
        assert_eq!(app.pkg_offset(), 15, "walks up inside the view first");
        app.pkg_move(-11);
        // Cursor 8 → must sit 4 below the top → offset 4.
        assert_eq!(app.pkg_offset(), 4);
        app.filter_push('p');
        assert_eq!(app.pkg_offset(), 0, "filter snaps the window home");
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
    fn exec_and_log_overlays_outrank_the_screen() {
        // Both overlays fire from the dashboard now.
        let mut app = app();
        app.open_log("a\nb\nc".to_string());
        assert_eq!(app.input_mode(), InputMode::LogView);
        app.log_scroll(5);
        assert_eq!(app.log_view().unwrap().scroll, 2); // clamped to last line
        app.log_scroll(-10);
        assert_eq!(app.log_view().unwrap().scroll, 0);
        app.close_log();
        assert_eq!(app.input_mode(), InputMode::Dashboard);

        app.start_exec(10, 40);
        assert_eq!(app.input_mode(), InputMode::Exec);
        assert!(!app.exec_is_done());
        app.exec_feed(b"out\r\n");
        let screen = app.exec().expect("console open").parser.screen();
        assert!(
            screen.contents().contains("out"),
            "vt100 did not take the feed"
        );
        app.exec_finish(sample_report());
        assert!(app.exec_is_done());
        let report = app.take_exec_report().expect("report");
        assert_eq!(report.succeeded(), 1);
        assert!(app.exec().is_none());
        assert_eq!(app.input_mode(), InputMode::Dashboard);
    }

    #[test]
    fn replace_scan_reinitializes_toggles() {
        let mut app = app();
        app.toggle_selected(); // pacman off
        assert!(!app.is_enabled(&SourceId::pacman()));
        app.replace_scan(scan_with_sources(vec![Source {
            id: SourceId::pacman(),
            kind: SourceKind::Pacman,
            available: true,
            last_scanned: None,
            accurate_updates: true,
        }]));
        assert!(app.is_enabled(&SourceId::pacman())); // toggles reset to on
    }
}
