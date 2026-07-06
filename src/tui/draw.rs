//! Immediate-mode rendering. Every function here takes `&App` and never mutates
//! it (dev-notes §5): each frame rebuilds the whole UI from current state.
//!
//! `draw` dispatches on the active screen. The dashboard is a bordered panel with
//! a summary line, the per-source table, and a footer. The update screen is a
//! two-pane bordered panel: a left source list with `[✓]`/`[ ]` toggles and a
//! right pane of the highlighted source's pending updates. Below a minimum size
//! the panel is replaced by a terse notice so the frame never renders broken.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Padding, Paragraph, Row, Table, TableState};

use crate::executor::{self, ExecutionReport, StepStatus};
use crate::format::relative_time;
use crate::model::{ActionPlan, Source};
use crate::tui::app::{App, Screen};
use crate::tui::theme::Theme;

const MIN_WIDTH: u16 = 40;
const MIN_HEIGHT: u16 = 10;

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        render_too_small(frame, area, &app.theme);
        return;
    }
    // Cold start: nothing to show yet — the splash carries the spinner until
    // the first background scan lands.
    if app.is_scanning() && app.scan().sources.is_empty() {
        draw_splash(frame, &app.theme, Some(app.spinner()));
        return;
    }
    match app.screen() {
        Screen::Dashboard => draw_dashboard(frame, area, app),
        Screen::Updates => match app.report() {
            Some(report) => draw_result(frame, area, app, report),
            None => draw_update(frame, area, app),
        },
        Screen::Packages => draw_packages(frame, area, app),
    }
}

// ---------------------------------------------------------------------------
// Dashboard
// ---------------------------------------------------------------------------

/// The linutil-style quadrant dashboard (chosen with the user): sources +
/// system panes on the left, pending updates + keys on the right. Below a
/// comfortable size it falls back to the flat single-table layout.
fn draw_dashboard(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let block = panel(theme, " paclens · dashboard ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < 56 || inner.height < 14 {
        draw_dashboard_flat(frame, inner, app);
        return;
    }

    let cols =
        Layout::horizontal([Constraint::Percentage(46), Constraint::Percentage(54)]).split(inner);
    let left = Layout::vertical([Constraint::Min(5), Constraint::Length(8)]).split(cols[0]);
    let right = Layout::vertical([Constraint::Min(5), Constraint::Length(4)]).split(cols[1]);

    let sources_focused = app.dash_focus() == crate::tui::app::DashPane::Sources;
    let sources_pane = subpane(theme, " sources ").border_style(if sources_focused {
        theme.selected
    } else {
        theme.border
    });
    let sources_inner = sources_pane.inner(left[0]);
    frame.render_widget(sources_pane, left[0]);
    render_table(frame, sources_inner, app);

    render_system_pane(frame, left[1], app);
    render_updates_pane(frame, right[0], app);

    let keys_pane = subpane(theme, " keys ");
    let keys_inner = keys_pane.inner(right[1]);
    frame.render_widget(keys_pane, right[1]);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(dashboard_keys(theme), theme.dim)))
            .wrap(ratatui::widgets::Wrap { trim: true }),
        keys_inner,
    );
}

/// The pre-quadrant layout, kept for small terminals.
fn draw_dashboard_flat(frame: &mut Frame, inner: Rect, app: &App) {
    let theme = &app.theme;
    let chunks = Layout::vertical([
        Constraint::Length(1), // summary
        Constraint::Length(1), // spacer
        Constraint::Min(3),    // table
        Constraint::Length(1), // footer
    ])
    .split(inner);

    let cols = Layout::horizontal([Constraint::Min(0), Constraint::Length(24)]).split(chunks[0]);
    frame.render_widget(
        Paragraph::new(summary_line(app)).alignment(Alignment::Left),
        cols[0],
    );
    frame.render_widget(
        Paragraph::new(scanned_line(app)).alignment(Alignment::Right),
        cols[1],
    );
    render_table(frame, chunks[2], app);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(dashboard_keys(theme), theme.dim))),
        chunks[3],
    );
}

fn dashboard_keys(theme: &Theme) -> String {
    let g = theme.glyphs;
    format!(
        "q quit {b} {up}/{down} move {b} ←/→ pane {b} enter packages {b} u update {b} r refresh",
        b = g.bullet,
        up = g.up,
        down = g.down,
    )
}

/// Bordered inner pane with a dim border and a bold-dim title.
fn subpane(theme: &Theme, title: &'static str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_set(theme.border_set)
        .border_style(theme.border)
        .title(Span::styled(title, theme.header))
        .padding(Padding::horizontal(1))
}

/// System pane: cache size, orphans, overlaps, scan age — the analyzer's
/// dashboard debut (dev-notes §7 "dashboard enrichment").
fn render_system_pane(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let pane = subpane(theme, " system ");
    let inner = pane.inner(area);
    frame.render_widget(pane, area);

    let kv = |label: &str, value: Span<'static>| {
        Line::from(vec![
            Span::styled(format!("{:14}", label), theme.dim),
            value,
        ])
    };
    let count = |n: usize| {
        if n > 0 {
            Span::styled(n.to_string(), theme.accent)
        } else {
            Span::styled("0".to_string(), theme.dim)
        }
    };

    let cache = match app.scan().cache_sizes.pacman_cache_bytes {
        Some(b) => Span::styled(crate::format::human_bytes(b), theme.primary),
        None => Span::styled("—".to_string(), theme.dim),
    };
    let mut lines = vec![
        kv("pacman cache", cache),
        kv("orphans", count(app.orphan_count())),
        kv("overlaps", count(app.overlap_count())),
        kv("scanned", scanned_span(app)),
    ];
    if app.stale_update_counts() {
        lines.push(Line::from(Span::styled(
            "stale? install pacman-contrib",
            theme.accent,
        )));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Pending-updates pane: grouped per source, capped per group.
fn render_updates_pane(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let total = app.total_updates();
    let title = if total == 0 {
        " pending updates ".to_string()
    } else {
        format!(" pending updates ({total}) ")
    };
    let focused = app.dash_focus() == crate::tui::app::DashPane::Updates;
    let pane = Block::default()
        .borders(Borders::ALL)
        .border_set(theme.border_set)
        .border_style(if focused {
            theme.selected
        } else {
            theme.border
        })
        .title(Span::styled(title, theme.header))
        .padding(Padding::horizontal(1));
    let inner = pane.inner(area);
    frame.render_widget(pane, area);

    if total == 0 {
        frame.render_widget(
            Paragraph::new(summary_line(app)).alignment(Alignment::Center),
            centered(inner, inner.width, 1),
        );
        return;
    }

    // The full list — ↑/↓ (j/k) scroll it while the pane has focus.
    let mut lines: Vec<Line> = Vec::new();
    for source in &app.scan().sources {
        let ups = app.updates_for(&source.id);
        if ups.is_empty() {
            continue;
        }
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.push(Line::from(vec![
            Span::styled(source.id.to_string(), theme.title),
            Span::styled(format!("  ({})", ups.len()), theme.dim),
        ]));
        let name_w = ups.iter().map(|u| u.package_name.len()).max().unwrap_or(0);
        for u in &ups {
            let new_version = if u.available_version.is_empty() {
                Span::styled("?", theme.dim)
            } else {
                Span::styled(u.available_version.clone(), theme.accent)
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {:name_w$}", u.package_name), theme.primary),
                Span::raw("  "),
                Span::styled(u.current_version.clone(), theme.dim),
                Span::styled(format!(" {} ", theme.glyphs.arrow), theme.dim),
                new_version,
            ]));
        }
    }
    let below = lines
        .len()
        .saturating_sub(app.updates_scroll() + inner.height as usize);
    frame.render_widget(
        Paragraph::new(lines).scroll((app.updates_scroll() as u16, 0)),
        inner,
    );
    if below > 0 {
        // Scroll indicator on the bottom border.
        let hint = format!(" {} {below} more ", theme.glyphs.down);
        let w = hint.chars().count() as u16;
        if area.width > w + 2 {
            let rect = Rect {
                x: area.x + area.width - w - 2,
                y: area.y + area.height - 1,
                width: w,
                height: 1,
            };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(hint, theme.dim))),
                rect,
            );
        }
    }
}

fn scanned_span(app: &App) -> Span<'static> {
    let theme = &app.theme;
    if app.is_scanning() {
        return Span::styled(format!("{} scanning sources…", app.spinner()), theme.accent);
    }
    Span::styled(relative_time(app.scan().scanned_at), theme.dim)
}

fn summary_line(app: &App) -> Line<'static> {
    let theme = &app.theme;
    let updates = app.total_updates();
    if updates == 0 {
        Line::from(Span::styled("up to date", theme.success))
    } else {
        let plural = if updates == 1 { "" } else { "s" };
        Line::from(vec![
            Span::styled(updates.to_string(), theme.accent),
            Span::styled(format!(" update{plural} available"), theme.primary),
        ])
    }
}

fn scanned_line(app: &App) -> Line<'static> {
    if app.is_scanning() {
        return Line::from(scanned_span(app));
    }
    Line::from(Span::styled(
        format!("scanned {}", relative_time(app.scan().scanned_at)),
        app.theme.dim,
    ))
}

fn render_table(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let rows = app.rows();

    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new("No package sources detected.")
                .style(theme.dim)
                .alignment(Alignment::Center),
            area,
        );
        return;
    }

    let header = Row::new(vec![
        Cell::from("SOURCE"),
        Cell::from(Line::from("INST").alignment(Alignment::Right)),
        Cell::from(Line::from("UPD").alignment(Alignment::Right)),
        Cell::from("STATUS"),
    ])
    .style(theme.header);

    let body: Vec<Row> = rows
        .iter()
        .map(|r| {
            // "ok / not found", not "available": next to the UPDATES column
            // that read like "updates available" (wording chosen with the user).
            let status = if r.available {
                Span::styled(format!("{} ok", theme.glyphs.available), theme.success)
            } else {
                Span::styled(
                    format!("{} not found", theme.glyphs.unavailable),
                    theme.unavailable,
                )
            };
            let updates = if r.updates > 0 {
                Span::styled(r.updates.to_string(), theme.accent)
            } else {
                Span::styled(r.updates.to_string(), theme.dim)
            };
            Row::new(vec![
                Cell::from(r.id.clone()),
                Cell::from(Line::from(r.installed.to_string()).alignment(Alignment::Right)),
                Cell::from(Line::from(updates).alignment(Alignment::Right)),
                Cell::from(status),
            ])
        })
        .collect();

    let widths = [
        Constraint::Min(13),
        Constraint::Length(5),
        Constraint::Length(4),
        Constraint::Length(12),
    ];

    let table = Table::new(body, widths)
        .header(header)
        .column_spacing(1)
        .row_highlight_style(theme.selected)
        .highlight_symbol(theme.glyphs.pointer);

    let mut state = TableState::default();
    state.select(app.selected());
    frame.render_stateful_widget(table, area, &mut state);
}

// ---------------------------------------------------------------------------
// Update screen (two-pane)
// ---------------------------------------------------------------------------

fn draw_update(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let block = panel(theme, " paclens · update plan ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.total_updates() == 0 {
        // Vertically centered message, but keep the key hints visible — an
        // empty screen must never look like a dead end.
        frame.render_widget(
            Paragraph::new("Nothing to update — you're up to date")
                .style(theme.success)
                .alignment(Alignment::Center),
            centered(inner, inner.width, 1),
        );
        render_update_footer(frame, bottom_line(inner), theme, false);
        return;
    }

    let plan = app.update_plan();
    let sources = app.available_sources();

    let chunks = Layout::vertical([
        Constraint::Length(1), // summary
        Constraint::Length(1), // spacer
        Constraint::Min(3),    // two-pane body
        Constraint::Length(1), // confirm / flash
        Constraint::Length(1), // footer
    ])
    .split(inner);

    frame.render_widget(Paragraph::new(update_summary_line(&plan, theme)), chunks[0]);

    let panes = Layout::horizontal([Constraint::Length(26), Constraint::Min(20)]).split(chunks[2]);
    render_source_pane(frame, panes[0], app, &sources);
    render_package_pane(frame, panes[1], app, &sources);

    render_confirm(frame, chunks[3], app, &plan);
    render_update_footer(frame, chunks[4], theme, true);

    if app.is_confirming() {
        render_confirm_modal(frame, inner, app, &plan);
    }
}

/// The update screen's key hints. `esc back` is always present; the plan-only
/// keys are dropped in the empty state where there is nothing to toggle.
fn render_update_footer(frame: &mut Frame, area: Rect, theme: &Theme, has_plan: bool) {
    let g = theme.glyphs;
    let footer = if has_plan {
        format!(
            "space toggle {b} {up}/{down} source {b} l log {b} esc back {b} q quit",
            b = g.bullet,
            up = g.up,
            down = g.down,
        )
    } else {
        format!("l log {b} esc back {b} q quit", b = g.bullet)
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(footer, theme.dim))),
        area,
    );
}

/// The last row of `area` (for a footer outside a Layout split).
fn bottom_line(area: Rect) -> Rect {
    Rect {
        x: area.x,
        y: area.bottom().saturating_sub(1),
        width: area.width,
        height: 1,
    }
}

fn update_summary_line(plan: &ActionPlan, theme: &Theme) -> Line<'static> {
    let total = plan.total_targets();
    if total == 0 {
        return Line::from(Span::styled("nothing selected", theme.dim));
    }
    let srcs = plan.source_count();
    let pkg_word = if total == 1 { "package" } else { "packages" };
    let src_word = if srcs == 1 { "source" } else { "sources" };
    Line::from(vec![
        Span::styled(total.to_string(), theme.accent),
        Span::styled(format!(" {pkg_word} will update across "), theme.primary),
        Span::styled(srcs.to_string(), theme.accent),
        Span::styled(format!(" {src_word}"), theme.primary),
    ])
}

fn render_source_pane(frame: &mut Frame, area: Rect, app: &App, sources: &[&Source]) {
    let theme = &app.theme;
    let g = theme.glyphs;
    let lines: Vec<Line> = sources
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let enabled = app.is_enabled(&s.id);
            let count = app.updates_for(&s.id).len();
            let selected = i == app.update_cursor();

            let toggle = if enabled {
                format!("[{}]", g.check)
            } else {
                "[ ]".to_string()
            };
            let count_span = if count > 0 {
                Span::styled(format!("  {count}"), theme.accent)
            } else {
                Span::styled("  0".to_string(), theme.dim)
            };
            let mut line = Line::from(vec![
                Span::raw(if selected { g.pointer } else { "  " }),
                Span::styled(toggle, if enabled { theme.success } else { theme.dim }),
                Span::raw(" "),
                Span::styled(
                    s.id.to_string(),
                    if enabled { theme.title } else { theme.dim },
                ),
                count_span,
            ]);
            if selected {
                line.style = theme.selected;
            }
            line
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_package_pane(frame: &mut Frame, area: Rect, app: &App, sources: &[&Source]) {
    let theme = &app.theme;
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(theme.border)
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(source) = sources.get(app.update_cursor()).copied() else {
        return;
    };
    let ups = app.updates_for(&source.id);
    if ups.is_empty() {
        frame.render_widget(Paragraph::new("up to date").style(theme.success), inner);
        return;
    }

    let name_w = ups.iter().map(|u| u.package_name.len()).max().unwrap_or(0);
    let lines: Vec<Line> = ups
        .iter()
        .map(|u| {
            // Same rule as the CLI plan: an unknown new version (stale flatpak
            // appstream data) renders as a dim "?", never a dangling arrow.
            let new_version = if u.available_version.is_empty() {
                Span::styled("?", theme.dim)
            } else {
                Span::styled(u.available_version.clone(), theme.accent)
            };
            Line::from(vec![
                Span::styled(format!("{:name_w$}", u.package_name), theme.primary),
                Span::raw("  "),
                Span::styled(u.current_version.clone(), theme.dim),
                Span::styled(format!(" {} ", theme.glyphs.arrow), theme.dim),
                new_version,
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_confirm(frame: &mut Frame, area: Rect, app: &App, plan: &ActionPlan) {
    let theme = &app.theme;
    let line = if let Some(flash) = app.flash() {
        Line::from(Span::styled(flash.to_string(), theme.accent))
    } else if plan.is_empty() {
        Line::from(Span::styled("nothing selected to update", theme.dim))
    } else {
        Line::from(vec![
            Span::styled("[Enter]", theme.accent),
            Span::styled(" update everything    ", theme.primary),
            Span::styled("[Esc]", theme.dim),
            Span::styled(" cancel", theme.dim),
        ])
    };
    frame.render_widget(Paragraph::new(line), area);
}

// ---------------------------------------------------------------------------
// Package list (spec §10.3) + why side pane
// ---------------------------------------------------------------------------

fn draw_packages(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let source = app.pkg_source().map(|s| s.to_string()).unwrap_or_default();
    let title = format!(" paclens · {source} · {} packages ", app.pkg_total());
    // panel() wants 'static; build the block by hand for the dynamic title.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(theme.border_set)
        .border_style(theme.border)
        .title(Span::styled(title, theme.title))
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let show_filter = app.is_filter_active() || !app.pkg_filter().is_empty();
    let chunks = Layout::vertical([
        Constraint::Length(1), // source summary
        Constraint::Min(3),    // table (+ optional why pane)
        Constraint::Length(1), // filter line (may be blank)
        Constraint::Length(1), // footer
    ])
    .split(inner);

    let (explicit, deps, runtimes, bytes) = app.pkg_summary();
    let mut summary = vec![
        Span::styled(explicit.to_string(), theme.accent),
        Span::styled(" explicit ", theme.primary),
        Span::styled(format!("{b} ", b = theme.glyphs.bullet), theme.dim),
        Span::styled(deps.to_string(), theme.accent),
        Span::styled(" dependencies ", theme.primary),
    ];
    if runtimes > 0 {
        summary.extend([
            Span::styled(format!("{b} ", b = theme.glyphs.bullet), theme.dim),
            Span::styled(runtimes.to_string(), theme.accent),
            Span::styled(" runtimes ", theme.primary),
        ]);
    }
    if bytes > 0 {
        summary.extend([
            Span::styled(format!("{b} ", b = theme.glyphs.bullet), theme.dim),
            Span::styled(crate::format::human_bytes(bytes), theme.primary),
            Span::styled(" total", theme.dim),
        ]);
    }
    frame.render_widget(Paragraph::new(Line::from(summary)), chunks[0]);

    if app.is_why_open() {
        let panes =
            Layout::horizontal([Constraint::Min(30), Constraint::Percentage(40)]).split(chunks[1]);
        // Narrow mode: extra columns would crush against the pane; the why
        // report is what matters here.
        render_package_table(frame, panes[0], app, true);
        render_why_pane(frame, panes[1], app);
    } else {
        render_package_table(frame, chunks[1], app, false);
    }

    if show_filter {
        render_filter_line(frame, chunks[2], app);
    }

    let g = theme.glyphs;
    let footer = if app.is_filter_active() {
        "type to filter · enter apply · esc cancel".to_string()
    } else {
        format!(
            "{up}/{down} navigate {b} / filter {b} w why {b} esc back {b} q quit",
            b = g.bullet,
            up = g.up,
            down = g.down,
        )
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(footer, theme.dim))),
        chunks[3],
    );
}

/// `narrow` drops the VERSION/SIZE columns — used when the why pane halves
/// the width, where they would truncate into noise.
fn render_package_table(frame: &mut Frame, area: Rect, app: &App, narrow: bool) {
    let theme = &app.theme;
    let pkgs = app.visible_packages();

    if pkgs.is_empty() {
        let msg = if app.pkg_filter().is_empty() {
            "no packages"
        } else {
            "no matches — esc clears the filter"
        };
        frame.render_widget(
            Paragraph::new(msg)
                .style(theme.dim)
                .alignment(Alignment::Center),
            area,
        );
        return;
    }

    let header = if narrow {
        Row::new(vec![Cell::from("NAME"), Cell::from("REASON")])
    } else {
        Row::new(vec![
            Cell::from("NAME"),
            Cell::from("VERSION"),
            Cell::from("KIND"),
            Cell::from("REASON"),
            Cell::from(Line::from("SIZE").alignment(Alignment::Right)),
            Cell::from("DESCRIPTION"),
        ])
    }
    .style(theme.header);

    let body: Vec<Row> = pkgs
        .iter()
        .map(|p| {
            let reason = match p.install_reason {
                crate::model::InstallReason::Explicit => Span::styled("explicit", theme.primary),
                crate::model::InstallReason::Dependency => Span::styled("dependency", theme.dim),
                crate::model::InstallReason::Unknown => Span::styled("—", theme.dim),
            };
            if narrow {
                return Row::new(vec![Cell::from(p.name.clone()), Cell::from(reason)]);
            }
            let kind = if p.runtime {
                Span::styled("runtime", theme.dim)
            } else if p.source_id.as_str().starts_with("flatpak") {
                Span::styled("app", theme.primary)
            } else {
                Span::styled("—", theme.dim)
            };
            let size = match p.size_bytes {
                Some(b) => Span::styled(crate::format::human_bytes(b), theme.primary),
                None => Span::styled("—".to_string(), theme.dim),
            };
            let description = Span::styled(p.description.clone().unwrap_or_default(), theme.dim);
            Row::new(vec![
                Cell::from(p.name.clone()),
                Cell::from(Span::styled(p.version.clone(), theme.dim)),
                Cell::from(kind),
                Cell::from(reason),
                Cell::from(Line::from(size).alignment(Alignment::Right)),
                Cell::from(description),
            ])
        })
        .collect();

    let widths: &[Constraint] = if narrow {
        &[Constraint::Min(20), Constraint::Length(10)]
    } else {
        &[
            Constraint::Min(20),
            Constraint::Length(12),
            Constraint::Length(7),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Min(10),
        ]
    };
    let table = Table::new(body, widths.to_vec())
        .header(header)
        .column_spacing(2)
        .row_highlight_style(theme.selected)
        .highlight_symbol(theme.glyphs.pointer);

    let mut state = TableState::default();
    state.select(Some(app.pkg_cursor()));
    frame.render_stateful_widget(table, area, &mut state);
}

fn render_filter_line(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let shown = app.visible_packages().len();
    let cursor = if app.is_filter_active() { "▏" } else { "" };
    let line = Line::from(vec![
        Span::styled("/", theme.accent),
        Span::styled(format!("{}{cursor}", app.pkg_filter()), theme.primary),
        Span::styled(format!("   {shown} of {}", app.pkg_total()), theme.dim),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

/// The why side pane: the analyzer's report for the row under the cursor,
/// live — same data the CLI prints (P5).
fn render_why_pane(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(theme.border)
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(report) = app.why_report() else {
        frame.render_widget(Paragraph::new("no selection").style(theme.dim), inner);
        return;
    };
    let lines = why_pane_lines(&report, theme);
    frame.render_widget(
        Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }),
        inner,
    );
}

fn why_pane_lines(report: &crate::analyzer::WhyReport, theme: &Theme) -> Vec<Line<'static>> {
    use crate::analyzer::{Verdict, WhyReport};
    use crate::model::InstallReason;

    let kv = |label: &str, value: String, style| {
        Line::from(vec![
            Span::styled(format!("{:10}", label), theme.dim),
            Span::styled(value, style),
        ])
    };
    let names = |list: &[String]| {
        let mut text = list.iter().take(6).cloned().collect::<Vec<_>>().join(", ");
        if list.is_empty() {
            text = "nothing".to_string();
        } else if list.len() > 6 {
            text.push_str(&format!(" … {} more", list.len() - 6));
        }
        text
    };

    match report {
        WhyReport::Pacman(p) => {
            let mut lines = vec![
                Line::from(Span::styled(format!("why · {}", p.package), theme.title)),
                Line::default(),
            ];
            let reason = match p.reason {
                InstallReason::Explicit => "explicit".to_string(),
                InstallReason::Dependency => match p.depth_from_explicit {
                    Some(d) => format!("dependency · {d} hop{}", plural(d)),
                    None => "dependency".to_string(),
                },
                InstallReason::Unknown => "unknown".to_string(),
            };
            lines.push(kv("reason", reason, theme.primary));
            // "needed by" doubles as the breakage list — removing this
            // package breaks exactly what requires it.
            lines.push(kv("needed by", names(&p.required_by), theme.primary));
            lines.push(kv("orphans", names(&p.would_remove), theme.primary));
            if !p.tree.is_empty() {
                lines.push(Line::default());
                lines.push(Line::from(Span::styled("chain".to_string(), theme.dim)));
                let g = theme.glyphs;
                const TREE_ROWS: usize = 10;
                let rows = crate::analyzer::tree_lines(
                    &p.tree,
                    (g.tree_branch, g.tree_last, g.tree_pipe, g.tree_blank),
                );
                let total = rows.len();
                for row in rows.into_iter().take(TREE_ROWS) {
                    let mut spans = vec![
                        Span::styled(row.prefix, theme.dim),
                        Span::styled(row.name, theme.primary),
                        Span::styled(format!(" [{}]", row.confidence), theme.dim),
                    ];
                    if row.truncated > 0 {
                        spans.push(Span::styled(
                            format!("  … {} more", row.truncated),
                            theme.dim,
                        ));
                    }
                    lines.push(Line::from(spans));
                }
                if total > TREE_ROWS {
                    lines.push(Line::from(Span::styled(
                        format!("… {} more rows", total - TREE_ROWS),
                        theme.dim,
                    )));
                }
            }
            lines.push(Line::default());
            let verdict_style = match p.verdict {
                Verdict::LikelySafe => theme.success,
                Verdict::IsADependency => theme.accent,
                Verdict::Unclear => theme.error,
            };
            lines.push(Line::from(vec![
                Span::styled(p.verdict.to_string(), verdict_style),
                Span::styled(format!("  [{}]", p.confidence), theme.dim),
            ]));
            lines
        }
        WhyReport::Flatpak { package, source_id } => vec![
            Line::from(Span::styled(format!("why · {package}"), theme.title)),
            Line::default(),
            Line::from(Span::styled(
                format!("{source_id} app — self-contained,"),
                theme.primary,
            )),
            Line::from(Span::styled(
                "not part of the pacman dep graph",
                theme.primary,
            )),
            Line::default(),
            Line::from(vec![
                Span::styled("likely safe".to_string(), theme.success),
                Span::styled("  [confirmed]", theme.dim),
            ]),
        ],
        WhyReport::NotFound { .. } => {
            vec![Line::from(Span::styled("no data for this row", theme.dim))]
        }
    }
}

fn plural(n: u32) -> &'static str {
    if n == 1 { "" } else { "s" }
}

// ---------------------------------------------------------------------------
// Confirm modal
// ---------------------------------------------------------------------------

/// The centered confirmation overlay (chosen with the user): the question, the
/// exact command(s) that will run (P1), what will be skipped and why, and the
/// y/n key hints. Rendered on top of the update screen.
fn render_confirm_modal(frame: &mut Frame, area: Rect, app: &App, plan: &ActionPlan) {
    let theme = &app.theme;
    let tool = app.privilege_tool();
    let total = executor::executable_targets(plan, tool);
    let sources = executor::executable_steps(plan, tool);

    let mut body: Vec<Line> = vec![Line::from(Span::styled(
        format!(
            "Update {total} package{} across {sources} source{}?",
            if total == 1 { "" } else { "s" },
            if sources == 1 { "" } else { "s" },
        ),
        theme.accent,
    ))];
    body.push(Line::default());
    for step in &plan.steps {
        if executor::skip_reason(step, tool).is_none() {
            body.push(Line::from(Span::styled(
                executor::effective_command(step, tool).join(" "),
                theme.primary,
            )));
        }
    }
    let skipped: Vec<Line> = plan
        .steps
        .iter()
        .filter_map(|step| {
            executor::skip_reason(step, tool).map(|reason| {
                Line::from(Span::styled(
                    format!("{} will be skipped — {reason}", step.source_id),
                    theme.dim,
                ))
            })
        })
        .collect();
    if !skipped.is_empty() {
        body.push(Line::default());
        body.extend(skipped);
    }
    body.push(Line::default());
    body.push(Line::from(vec![
        Span::styled("[y]", theme.success),
        Span::styled(" update", theme.primary),
        Span::raw("      "),
        Span::styled("[n]", theme.error),
        Span::styled(" cancel", theme.dim),
    ]));

    let width = body.iter().map(Line::width).max().unwrap_or(0) as u16 + 6;
    let height = body.len() as u16 + 2;
    let rect = centered(area, width, height);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(theme.border_set)
        .border_style(theme.border)
        .title(Span::styled(" confirm ", theme.title))
        .padding(Padding::horizontal(2));
    frame.render_widget(Clear, rect);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    frame.render_widget(Paragraph::new(body), inner);
}

/// Center a `width` × `height` box inside `area`, clamped to fit.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

// ---------------------------------------------------------------------------
// Result view
// ---------------------------------------------------------------------------

/// Post-execution result (chosen with the user): one line per step with
/// ✓ / ✗ / · marks, the log path, and a "press any key" footer. The roadmap
/// rule in force: show what succeeded, what failed, never hide it.
fn draw_result(frame: &mut Frame, area: Rect, app: &App, report: &ExecutionReport) {
    let theme = &app.theme;
    let block = panel(theme, " paclens · update result ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::vertical([
        Constraint::Length(1), // headline
        Constraint::Length(1), // spacer
        Constraint::Min(3),    // per-step lines
        Constraint::Length(1), // log path
        Constraint::Length(1), // footer
    ])
    .split(inner);

    frame.render_widget(Paragraph::new(result_headline(report, theme)), chunks[0]);

    let name_w = report
        .steps
        .iter()
        .map(|s| s.source_id.as_str().len())
        .max()
        .unwrap_or(0);
    let lines: Vec<Line> = report
        .steps
        .iter()
        .map(|s| result_line(s, name_w, theme))
        .collect();
    frame.render_widget(Paragraph::new(lines), chunks[2]);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("log: {}", report.log_path.display()),
            theme.dim,
        ))),
        chunks[3],
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "l view log · any other key continues",
            theme.dim,
        ))),
        chunks[4],
    );
}

fn result_headline(report: &ExecutionReport, theme: &Theme) -> Line<'static> {
    let executed = report.executed();
    let src_word = if executed == 1 { "source" } else { "sources" };
    let sep = format!(" {} ", theme.glyphs.bullet);
    let mut spans = vec![
        Span::styled(executed.to_string(), theme.accent),
        Span::styled(format!(" {src_word} ran"), theme.primary),
        Span::styled(sep.clone(), theme.dim),
        Span::styled(report.succeeded().to_string(), theme.success),
        Span::styled(" succeeded", theme.primary),
    ];
    if report.failed() > 0 {
        spans.push(Span::styled(sep, theme.dim));
        spans.push(Span::styled(report.failed().to_string(), theme.error));
        spans.push(Span::styled(" failed", theme.primary));
    }
    Line::from(spans)
}

fn result_line(step: &executor::StepReport, name_w: usize, theme: &Theme) -> Line<'static> {
    let g = theme.glyphs;
    let name = format!("{:name_w$}", step.source_id.as_str());
    match &step.status {
        StepStatus::Succeeded => Line::from(vec![
            Span::styled(format!(" {} ", g.check), theme.success),
            Span::styled(name, theme.title),
            Span::styled(
                format!(
                    "  {} updated",
                    executor::target_noun(&step.source_id, step.targets)
                ),
                theme.primary,
            ),
        ]),
        StepStatus::Failed { detail } => Line::from(vec![
            Span::styled(format!(" {} ", g.cross), theme.error),
            Span::styled(name, theme.title),
            Span::styled(format!("  failed ({detail})"), theme.error),
        ]),
        StepStatus::Skipped { reason } => Line::from(Span::styled(
            format!(" {} {name}  skipped — {reason}", g.bullet),
            theme.dim,
        )),
    }
}

// ---------------------------------------------------------------------------
// Startup splash
// ---------------------------------------------------------------------------

/// The cold-start frame: painted immediately on open and animated while the
/// first background scan runs, so the terminal never looks hung.
pub fn draw_splash(frame: &mut Frame, theme: &Theme, spinner: Option<&str>) {
    let area = frame.area();
    let block = panel(theme, " paclens ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let headline = match spinner {
        Some(glyph) => format!("{glyph} scanning sources…"),
        None => "scanning sources…".to_string(),
    };
    let lines = vec![
        Line::from(Span::styled(headline, theme.accent)).centered(),
        Line::from(Span::styled(
            "first scan reads pacman + flatpak and can take a few seconds",
            theme.dim,
        ))
        .centered(),
    ];
    let rect = centered(inner, inner.width, 2);
    frame.render_widget(Paragraph::new(lines), rect);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled("q quit", theme.dim))),
        bottom_line(inner),
    );
}

// ---------------------------------------------------------------------------
// Shared
// ---------------------------------------------------------------------------

/// The outer bordered panel shared by both screens.
fn panel(theme: &Theme, title: &'static str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_set(theme.border_set)
        .border_style(theme.border)
        .title(Span::styled(title, theme.title))
        .padding(Padding::horizontal(1))
}

fn render_too_small(frame: &mut Frame, area: Rect, theme: &Theme) {
    frame.render_widget(
        Paragraph::new("terminal too small")
            .style(theme.dim)
            .alignment(Alignment::Center),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        CacheSizes, FlatpakScope, InstallReason, Package, PendingUpdate, SCHEMA_VERSION,
        ScanResult, Source, SourceId, SourceKind,
    };
    use crate::tui::app::AppOptions;
    use crate::tui::theme::Theme;
    use chrono::Utc;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

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

    fn upd(name: &str, cur: &str, new: &str, source: SourceId) -> PendingUpdate {
        PendingUpdate {
            package_name: name.to_string(),
            current_version: cur.to_string(),
            available_version: new.to_string(),
            source_id: source,
        }
    }

    fn scan_with(updates: Vec<PendingUpdate>) -> ScanResult {
        ScanResult {
            schema_version: SCHEMA_VERSION,
            scanned_at: Utc::now(),
            sources: vec![
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
            ],
            packages: vec![pkg("a", SourceId::pacman())],
            updates,
            cache_sizes: CacheSizes::default(),
        }
    }

    fn flatten(buf: &Buffer) -> String {
        let area = buf.area;
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                if let Some(cell) = buf.cell((x, y)) {
                    out.push_str(cell.symbol());
                }
            }
            out.push('\n');
        }
        out
    }

    fn render(app: &App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        flatten(terminal.backend().buffer())
    }

    #[test]
    fn dashboard_renders_with_an_update_hint() {
        let app = App::new(
            scan_with(vec![upd("linux", "1", "2", SourceId::pacman())]),
            Theme::none(),
            AppOptions::test(),
        );
        let text = render(&app, 70, 14);
        assert!(text.contains("paclens"));
        assert!(text.contains("SOURCE"));
        assert!(text.contains("u update"), "footer hint missing:\n{text}");
    }

    // --- quadrant dashboard (rendered at a comfortable size) ---
    #[test]
    fn quadrant_dashboard_shows_all_four_panes() {
        let app = App::new(
            scan_with(vec![
                upd("linux", "6.9.1", "6.9.2", SourceId::pacman()),
                upd("firefox", "127.0", "127.0.1", SourceId::pacman()),
            ]),
            Theme::none(),
            AppOptions::test(),
        );
        let text = render(&app, 96, 24);
        for pane in ["sources", "pending updates (2)", "system", "keys"] {
            assert!(text.contains(pane), "pane {pane} missing:\n{text}");
        }
        assert!(text.contains("orphans"), "{text}");
        assert!(text.contains("overlaps"), "{text}");
        assert!(text.contains("pacman cache"), "{text}");
        // Grouped preview with version transitions.
        assert!(text.contains("6.9.1 -> 6.9.2"), "preview missing:\n{text}");
        assert!(text.contains("q quit"), "keys missing:\n{text}");
    }

    #[test]
    fn quadrant_updates_pane_scrolls_the_full_list() {
        let ups: Vec<PendingUpdate> = (0..30)
            .map(|i| upd(&format!("pkg{i:02}"), "1", "2", SourceId::pacman()))
            .collect();
        let mut app = App::new(scan_with(ups), Theme::none(), AppOptions::test());
        let text = render(&app, 96, 24);
        assert!(text.contains("pending updates (30)"), "{text}");
        assert!(text.contains("pkg00"), "{text}");
        assert!(!text.contains("pkg29"), "everything fit?!\n{text}");
        assert!(text.contains("more "), "scroll indicator missing:\n{text}");

        // Focus the pane and scroll: later rows come into view.
        app.focus_updates();
        for _ in 0..15 {
            app.on_next();
        }
        let text = render(&app, 96, 24);
        assert!(!text.contains("pkg00"), "did not scroll:\n{text}");
        assert!(text.contains("pkg29"), "end not reachable:\n{text}");
    }

    #[test]
    fn dashboard_arrows_follow_the_focused_pane() {
        let ups: Vec<PendingUpdate> = (0..5)
            .map(|i| upd(&format!("pkg{i}"), "1", "2", SourceId::pacman()))
            .collect();
        let mut app = App::new(scan_with(ups), Theme::none(), AppOptions::test());
        // Sources focus: ↓ moves the cursor, scroll stays.
        app.on_next();
        assert_eq!(app.selected(), Some(1));
        assert_eq!(app.updates_scroll(), 0);
        // Updates focus: ↓ scrolls, cursor stays.
        app.focus_updates();
        app.on_next();
        assert_eq!(app.selected(), Some(1));
        assert_eq!(app.updates_scroll(), 1);
        app.on_prev();
        app.on_prev(); // clamped at 0
        assert_eq!(app.updates_scroll(), 0);
        // Back to sources.
        app.focus_sources();
        app.on_next();
        assert_eq!(app.selected(), Some(2));
    }

    #[test]
    fn stale_counts_hint_appears_only_on_the_qu_fallback() {
        let mut s = scan_with(vec![upd("linux", "1", "2", SourceId::pacman())]);
        s.sources[0].accurate_updates = false;
        let app = App::new(s, Theme::none(), AppOptions::test());
        let text = render(&app, 96, 24);
        assert!(
            text.contains("stale? install pacman-contrib"),
            "hint missing:\n{text}"
        );

        let accurate = App::new(
            scan_with(vec![upd("linux", "1", "2", SourceId::pacman())]),
            Theme::none(),
            AppOptions::test(),
        );
        let text = render(&accurate, 96, 24);
        assert!(!text.contains("pacman-contrib"), "{text}");
    }

    #[test]
    fn small_terminal_falls_back_to_the_flat_dashboard() {
        let app = App::new(
            scan_with(vec![upd("linux", "1", "2", SourceId::pacman())]),
            Theme::none(),
            AppOptions::test(),
        );
        let text = render(&app, 50, 12);
        assert!(
            !text.contains("pacman cache"),
            "quadrants leaked in:\n{text}"
        );
        assert!(text.contains("SOURCE"), "{text}");
        assert!(text.contains("1 update available"), "{text}");
    }

    #[test]
    fn dashboard_shows_a_scanning_indicator_instead_of_the_scan_age() {
        let mut app = App::new(
            scan_with(vec![upd("linux", "1", "2", SourceId::pacman())]),
            Theme::none(),
            AppOptions::test(),
        );
        app.set_scanning(true);
        let text = render(&app, 70, 14);
        assert!(
            text.contains("scanning sources"),
            "indicator missing:\n{text}"
        );
        assert!(!text.contains("scanned "), "stale age shown:\n{text}");
    }

    #[test]
    fn update_screen_shows_toggle_summary_and_versions() {
        let mut app = App::new(
            scan_with(vec![
                upd("linux", "6.9.1", "6.9.2", SourceId::pacman()),
                upd("firefox", "127.0", "127.0.1", SourceId::pacman()),
            ]),
            Theme::none(),
            AppOptions::test(),
        );
        app.goto_updates();
        let text = render(&app, 72, 16);
        assert!(text.contains("update plan"), "title missing:\n{text}");
        assert!(text.contains("[x] pacman"), "toggle missing:\n{text}"); // ascii check in none theme
        assert!(
            text.contains("2 packages will update"),
            "summary missing:\n{text}"
        );
        assert!(
            text.contains("6.9.1 -> 6.9.2"),
            "version transition missing:\n{text}"
        );
        assert!(text.contains("[Enter]"), "confirm missing:\n{text}");
        assert!(text.contains("space toggle"), "footer missing:\n{text}");
        assert!(text.contains("esc back"), "back hint missing:\n{text}");
    }

    #[test]
    fn unknown_new_version_renders_as_a_question_mark() {
        let mut app = App::new(
            scan_with(vec![upd(
                "org.gnome.Calculator",
                "49.2",
                "",
                SourceId::pacman(),
            )]),
            Theme::none(),
            AppOptions::test(),
        );
        app.goto_updates();
        let text = render(&app, 72, 16);
        assert!(text.contains("49.2 -> ?"), "dangling arrow:\n{text}");
    }

    #[test]
    fn toggling_off_empties_the_confirm() {
        let mut app = App::new(
            scan_with(vec![upd("linux", "1", "2", SourceId::pacman())]),
            Theme::none(),
            AppOptions::test(),
        );
        app.goto_updates();
        app.toggle_selected(); // pacman off -> plan empty
        let text = render(&app, 72, 16);
        assert!(
            text.contains("[ ] pacman"),
            "untoggled box missing:\n{text}"
        );
        assert!(text.contains("nothing selected"), "{text}");
    }

    #[test]
    fn update_screen_empty_state_still_shows_the_way_back() {
        let mut app = App::new(scan_with(Vec::new()), Theme::none(), AppOptions::test());
        app.goto_updates();
        let text = render(&app, 72, 16);
        assert!(text.contains("Nothing to update"), "{text}");
        assert!(text.contains("esc back"), "back hint missing:\n{text}");
        assert!(text.contains("q quit"), "quit hint missing:\n{text}");
        // The plan-only keys make no sense with nothing to toggle.
        assert!(!text.contains("space toggle"), "{text}");
    }

    #[test]
    fn confirm_line_shows_a_flash() {
        let mut app = App::new(
            scan_with(vec![upd("linux", "1", "2", SourceId::pacman())]),
            Theme::none(),
            AppOptions::test(),
        );
        app.goto_updates();
        app.set_flash("nothing selected to update");
        let text = render(&app, 72, 16);
        assert!(text.contains("nothing selected to update"), "{text}");
    }

    // --- confirm modal ---
    #[test]
    fn confirm_modal_shows_both_effective_commands_with_a_tool() {
        let mut app = App::new(
            scan_with(vec![
                upd("linux", "1", "2", SourceId::pacman()),
                upd("org.gimp.GIMP", "2.10", "2.12", SourceId::flatpak_user()),
            ]),
            Theme::none(),
            AppOptions::test(),
        );
        app.goto_updates();
        app.open_confirm();
        let text = render(&app, 80, 20);
        assert!(text.contains("confirm"), "modal title missing:\n{text}");
        assert!(
            text.contains("Update 2 packages across 2 sources?"),
            "question missing:\n{text}"
        );
        assert!(
            text.contains("sudo pacman -Syu"),
            "privileged command missing:\n{text}"
        );
        assert!(
            text.contains("flatpak update --user --noninteractive"),
            "exact command missing:\n{text}"
        );
        assert!(!text.contains("will be skipped"), "{text}");
        assert!(text.contains("[y] update"), "y hint missing:\n{text}");
        assert!(text.contains("[n] cancel"), "n hint missing:\n{text}");
    }

    #[test]
    fn confirm_modal_skips_privileged_steps_without_a_tool() {
        let mut app = App::new(
            scan_with(vec![
                upd("linux", "1", "2", SourceId::pacman()),
                upd("org.gimp.GIMP", "2.10", "2.12", SourceId::flatpak_user()),
            ]),
            Theme::none(),
            AppOptions {
                privilege_tool: None, // no sudo/doas/pkexec on PATH
                ..AppOptions::test()
            },
        );
        app.goto_updates();
        app.open_confirm();
        let text = render(&app, 80, 20);
        assert!(
            text.contains("Update 1 package across 1 source?"),
            "question missing:\n{text}"
        );
        assert!(
            text.contains("pacman will be skipped — no privilege tool"),
            "skip note missing:\n{text}"
        );
        assert!(!text.contains("sudo pacman"), "{text}");
    }

    // --- result view ---
    use crate::executor::{ExecutionReport, StepReport, StepStatus};
    use std::path::PathBuf;

    fn report() -> ExecutionReport {
        ExecutionReport {
            steps: vec![
                StepReport {
                    source_id: SourceId::flatpak_user(),
                    targets: 2,
                    status: StepStatus::Succeeded,
                },
                StepReport {
                    source_id: SourceId::flatpak_system(),
                    targets: 1,
                    status: StepStatus::Failed {
                        detail: "exit 1".to_string(),
                    },
                },
                StepReport {
                    source_id: SourceId::pacman(),
                    targets: 3,
                    status: StepStatus::Skipped {
                        reason: "execution arrives in v0.1".to_string(),
                    },
                },
            ],
            log_path: PathBuf::from("/tmp/paclens/2026-06-12.log"),
        }
    }

    #[test]
    fn result_view_shows_every_outcome_and_the_log_path() {
        let mut app = App::new(scan_with(Vec::new()), Theme::none(), AppOptions::test());
        app.goto_updates();
        app.set_report(report());
        let text = render(&app, 76, 18);
        assert!(text.contains("update result"), "title missing:\n{text}");
        assert!(text.contains("2 sources ran"), "headline missing:\n{text}");
        assert!(text.contains("1 succeeded"), "{text}");
        assert!(text.contains("1 failed"), "{text}");
        assert!(
            text.contains("x flatpak-user"), // ascii check in the none theme
            "success mark missing:\n{text}"
        );
        assert!(text.contains("2 flatpaks updated"), "{text}");
        assert!(
            text.contains("! flatpak-system"), // ascii cross
            "failure mark missing:\n{text}"
        );
        assert!(text.contains("failed (exit 1)"), "{text}");
        assert!(
            text.contains("pacman          skipped - execution arrives in v0.1")
                || text.contains("skipped"),
            "skip line missing:\n{text}"
        );
        assert!(
            text.contains("log: /tmp/paclens/2026-06-12.log"),
            "log path missing:\n{text}"
        );
        assert!(text.contains("l view log"), "{text}");
    }

    #[test]
    fn all_green_result_has_no_failed_segment() {
        let mut app = App::new(scan_with(Vec::new()), Theme::none(), AppOptions::test());
        app.goto_updates();
        app.set_report(ExecutionReport {
            steps: vec![StepReport {
                source_id: SourceId::flatpak_user(),
                targets: 1,
                status: StepStatus::Succeeded,
            }],
            log_path: PathBuf::from("/tmp/x.log"),
        });
        let text = render(&app, 76, 16);
        assert!(text.contains("1 source ran"), "{text}");
        assert!(text.contains("1 succeeded"), "{text}");
        assert!(!text.contains("failed"), "{text}");
        assert!(text.contains("1 flatpak updated"), "{text}");
    }

    // --- package list ---
    fn rich_pkg(name: &str, reason: InstallReason, size: Option<u64>, depends: &[&str]) -> Package {
        Package {
            install_reason: reason,
            size_bytes: size,
            depends_on: depends.iter().map(|d| d.to_string()).collect(),
            ..pkg(name, SourceId::pacman())
        }
    }

    fn pkg_scan() -> ScanResult {
        let mut s = scan_with(Vec::new());
        s.packages = vec![
            rich_pkg(
                "firefox",
                InstallReason::Explicit,
                Some(240_000_000),
                &["glibc"],
            ),
            rich_pkg("glibc", InstallReason::Dependency, Some(50_000_000), &[]),
            rich_pkg("bash", InstallReason::Explicit, None, &["glibc"]),
        ];
        s
    }

    fn pkg_app() -> App {
        let mut app = App::new(pkg_scan(), Theme::none(), AppOptions::test());
        app.open_packages(); // dashboard row 0 = pacman
        app
    }

    #[test]
    fn package_list_renders_columns_rows_and_footer() {
        let text = render(&pkg_app(), 100, 18);
        assert!(text.contains("pacman"), "title missing:\n{text}");
        assert!(text.contains("3 packages"), "count missing:\n{text}");
        for col in ["NAME", "VERSION", "KIND", "REASON", "SIZE", "DESCRIPTION"] {
            assert!(text.contains(col), "column {col} missing:\n{text}");
        }
        assert!(text.contains("firefox"), "{text}");
        assert!(text.contains("explicit"), "{text}");
        assert!(text.contains("dependency"), "{text}");
        assert!(text.contains("228.88 MiB"), "size missing:\n{text}");
        // Header summary line.
        assert!(text.contains("2 explicit"), "summary missing:\n{text}");
        assert!(text.contains("1 dependencies"), "summary missing:\n{text}");
        assert!(
            text.contains("276.57 MiB total"),
            "summary missing:\n{text}"
        );
        assert!(text.contains("/ filter"), "footer missing:\n{text}");
        assert!(text.contains("w why"), "footer missing:\n{text}");
    }

    #[test]
    fn package_list_marks_runtimes_in_the_kind_column() {
        let mut s = scan_with(Vec::new());
        let mut platform = pkg("org.gnome.Platform", SourceId::flatpak_user());
        platform.runtime = true;
        platform.description = Some("GNOME Platform".to_string());
        let mut calc = pkg("org.gnome.Calculator", SourceId::flatpak_user());
        calc.description = Some("Calculator".to_string());
        s.packages = vec![platform, calc];
        let mut app = App::new(s, Theme::none(), AppOptions::test());
        app.on_next(); // select flatpak-user on the dashboard
        app.open_packages();
        let text = render(&app, 100, 18);
        assert!(text.contains("runtime"), "kind missing:\n{text}");
        assert!(text.contains("app"), "kind missing:\n{text}");
        assert!(
            text.contains("GNOME Platform"),
            "description missing:\n{text}"
        );
        assert!(text.contains("1 runtimes"), "summary missing:\n{text}");
    }

    #[test]
    fn filter_line_shows_query_and_counts() {
        let mut app = pkg_app();
        app.start_filter();
        app.filter_push('f');
        app.filter_push('x');
        let text = render(&app, 78, 18);
        assert!(text.contains("/fx"), "query missing:\n{text}");
        assert!(text.contains("1 of 3"), "counts missing:\n{text}");
        assert!(text.contains("firefox"), "{text}");
        assert!(!text.contains("glibc"), "filtered row leaked:\n{text}");
        assert!(
            text.contains("enter apply"),
            "filter footer missing:\n{text}"
        );
    }

    #[test]
    fn why_pane_shows_live_verdict_for_the_cursor_row() {
        let mut app = pkg_app();
        app.toggle_why();
        // cursor row 0 = bash (name-sorted) → explicit, nothing requires it.
        let text = render(&app, 90, 20);
        assert!(text.contains("why · bash"), "pane title missing:\n{text}");
        assert!(text.contains("likely safe"), "{text}");
        assert!(text.contains("[confirmed]"), "{text}");

        app.on_next(); // firefox
        app.on_next(); // glibc — needed by bash + firefox
        let text = render(&app, 90, 20);
        assert!(text.contains("why · glibc"), "pane must follow:\n{text}");
        assert!(text.contains("is a dependency"), "{text}");
        assert!(text.contains("bash, firefox"), "{text}");
    }

    #[test]
    fn why_pane_draws_the_dependency_chain_with_labels() {
        let mut s = scan_with(Vec::new());
        s.packages = vec![
            rich_pkg("bash", InstallReason::Explicit, None, &["readline"]),
            rich_pkg("readline", InstallReason::Dependency, None, &["glibc"]),
            rich_pkg("glibc", InstallReason::Dependency, None, &[]),
        ];
        let mut app = App::new(s, Theme::none(), AppOptions::test());
        app.open_packages();
        app.pkg_move(1); // glibc (name-sorted: bash, glibc, readline)
        app.toggle_why();
        let text = render(&app, 100, 22);
        assert!(text.contains("why · glibc"), "{text}");
        assert!(text.contains("chain"), "chain section missing:\n{text}");
        assert!(
            text.contains("`- readline [confirmed]"),
            "labeled edge missing:\n{text}"
        );
        assert!(
            text.contains("`- bash [confirmed]"),
            "nested edge missing:\n{text}"
        );
    }

    #[test]
    fn empty_filter_result_says_how_to_recover() {
        let mut app = pkg_app();
        app.start_filter();
        app.filter_push('z');
        app.filter_push('z');
        let text = render(&app, 78, 18);
        assert!(text.contains("no matches"), "{text}");
        assert!(text.contains("0 of 3"), "{text}");
    }

    // --- startup splash ---
    #[test]
    fn splash_shows_scanning_before_any_app_exists() {
        let backend = TestBackend::new(70, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::none();
        terminal
            .draw(|frame| draw_splash(frame, &theme, Some("|")))
            .unwrap();
        let text = flatten(terminal.backend().buffer());
        assert!(text.contains("paclens"), "title missing:\n{text}");
        assert!(
            text.contains("| scanning sources"),
            "spinner + indicator missing:\n{text}"
        );
        assert!(
            text.contains("can take a few seconds"),
            "hint missing:\n{text}"
        );
        assert!(text.contains("q quit"), "splash key hint missing:\n{text}");
    }

    #[test]
    fn cold_start_scanning_draws_the_splash_with_a_spinner() {
        // Scanning + no sources yet = the splash owns the whole frame.
        let mut s = scan_with(Vec::new());
        s.sources.clear();
        s.packages.clear();
        let mut app = App::new(s, Theme::none(), AppOptions::test());
        app.set_scanning(true);
        app.tick();
        let text = render(&app, 70, 12);
        assert!(text.contains("scanning sources"), "{text}");
        assert!(!text.contains("SOURCE"), "dashboard leaked in:\n{text}");
    }

    #[test]
    fn dashboard_scanning_line_carries_the_spinner_glyph() {
        let mut app = App::new(
            scan_with(vec![upd("linux", "1", "2", SourceId::pacman())]),
            Theme::none(),
            AppOptions::test(),
        );
        app.set_scanning(true);
        let text = render(&app, 70, 14);
        // ASCII spinner frame 0 is "|".
        assert!(text.contains("| scanning sources"), "{text}");
    }
}
