//! Immediate-mode rendering. Every function here takes `&App` and never mutates
//! it: each frame rebuilds the whole UI from current state.
//!
//! `draw` dispatches on the active screen after the overlays (exec console,
//! log viewer). The dashboard owns the update flow: quadrant panes with
//! `[✓]`/`[ ]` per-source toggles in the sources table, the selected
//! source's pending updates on the right, and `u` running the plan in the
//! pty console. Below a minimum size the panel is replaced by a terse
//! notice so the frame never renders broken.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Padding, Paragraph, Row, Table, TableState};

use crate::format::relative_time;
use crate::tui::app::{App, MatchField as PkgMatchField, Screen};
use crate::tui::theme::Theme;

const MIN_WIDTH: u16 = 40;
const MIN_HEIGHT: u16 = 10;
/// Below this the package screen stacks the detail pane under the table
/// instead of beside it (design: the pane is always open).
const SIDE_PANE_MIN_WIDTH: u16 = 90;
/// The pane never grows past this: its text is one description and one why
/// report, and the width beyond that reads better as table.
const PANE_MAX_WIDTH: u16 = 64;

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
    // Overlays cover whatever screen is active (the dashboard owns the
    // update flow — the console and log viewer draw on top of it).
    if let Some(view) = app.exec() {
        draw_exec(frame, area, app, view);
        return;
    }
    if let Some(view) = app.log_view() {
        draw_log(frame, area, app, view);
        return;
    }
    match app.screen() {
        Screen::Dashboard => draw_dashboard(frame, area, app),
        Screen::Packages => draw_packages(frame, area, app),
        Screen::Overlaps => draw_overlaps(frame, area, app),
        Screen::Cleanup => draw_cleanup(frame, area, app),
    }
}

/// The pty size the console gets for a `width` × `height` terminal — mirrors
/// `draw_exec`'s layout: borders (2) + footer (1) vertically, borders (2) +
/// horizontal padding (2) across.
pub fn exec_pty_size(width: u16, height: u16) -> (u16, u16) {
    (
        height.saturating_sub(3).max(2),
        width.saturating_sub(4).max(20),
    )
}

/// The inline execution console: the vt100 screen fed by the pty, rendered
/// cell-for-cell with its own colors — a terminal inside the panel. Typed
/// keys pass through to the running command.
fn draw_exec(frame: &mut Frame, area: Rect, app: &App, view: &crate::tui::app::ExecView) {
    let theme = &app.theme;
    let running = view.done.is_none();
    let title = if running {
        format!(" paclens · updating {} ", app.spinner())
    } else {
        " paclens · update finished ".to_string()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(theme.border_set)
        .border_style(if running {
            theme.selected
        } else {
            theme.border
        })
        .title(Span::styled(title, theme.title))
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(inner);

    let screen = view.parser.screen();
    let cursor = (running && !screen.hide_cursor()).then(|| screen.cursor_position());
    frame.render_widget(Paragraph::new(screen_lines(screen, cursor)), chunks[0]);

    let footer = if running {
        keys_line(theme, &[("keys", "pass through"), ("ctrl+c", "interrupt")])
    } else {
        keys_line(theme, &[("any key", "back to the dashboard")])
    };
    frame.render_widget(Paragraph::new(footer), chunks[1]);
}

/// Flatten a vt100 screen into styled lines, grouping same-style runs. The
/// cursor cell renders reversed so prompts (sudo password) show where they
/// are.
fn screen_lines(screen: &vt100::Screen, cursor: Option<(u16, u16)>) -> Vec<Line<'static>> {
    let (rows, cols) = screen.size();
    let mut lines = Vec::with_capacity(rows as usize);
    for r in 0..rows {
        let mut spans: Vec<Span> = Vec::new();
        let mut run = String::new();
        let mut run_style = Style::default();
        for c in 0..cols {
            let (mut style, text) = match screen.cell(r, c) {
                Some(cell) => {
                    let s = cell.contents();
                    (
                        vt_style(cell),
                        if s.is_empty() {
                            " ".to_string()
                        } else {
                            s.to_string()
                        },
                    )
                }
                None => (Style::default(), " ".to_string()),
            };
            if cursor == Some((r, c)) {
                style = style.add_modifier(Modifier::REVERSED);
            }
            if style == run_style {
                run.push_str(&text);
            } else {
                if !run.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut run), run_style));
                }
                run_style = style;
                run = text;
            }
        }
        if !run.is_empty() {
            spans.push(Span::styled(run, run_style));
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// vt100 cell attributes → ratatui style (exact passthrough of the child's
/// own colors; the theme only owns the chrome around the screen).
fn vt_style(cell: &vt100::Cell) -> Style {
    fn color(c: vt100::Color) -> Option<ratatui::style::Color> {
        match c {
            vt100::Color::Default => None,
            vt100::Color::Idx(i) => Some(ratatui::style::Color::Indexed(i)),
            vt100::Color::Rgb(r, g, b) => Some(ratatui::style::Color::Rgb(r, g, b)),
        }
    }
    let mut style = Style::default();
    if let Some(fg) = color(cell.fgcolor()) {
        style = style.fg(fg);
    }
    if let Some(bg) = color(cell.bgcolor()) {
        style = style.bg(bg);
    }
    if cell.bold() {
        style = style.add_modifier(Modifier::BOLD);
    }
    if cell.italic() {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if cell.underline() {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if cell.inverse() {
        style = style.add_modifier(Modifier::REVERSED);
    }
    style
}

/// The inline log viewer: the newest update log, scrollable like a pager.
fn draw_log(frame: &mut Frame, area: Rect, app: &App, view: &crate::tui::app::LogView) {
    let theme = &app.theme;
    let block = panel(theme, " paclens · update log ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(inner);

    let lines: Vec<Line> = view
        .lines
        .iter()
        .map(|l| Line::from(Span::styled(l.clone(), theme.primary)))
        .collect();
    let total = lines.len();
    frame.render_widget(
        Paragraph::new(lines).scroll((view.scroll as u16, 0)),
        chunks[0],
    );

    let g = theme.glyphs;
    let updown = format!("{}/{}", g.up, g.down);
    let mut spans = keys_line(
        theme,
        &[
            (&updown, "scroll"),
            ("pgup/pgdn", "page"),
            ("q/esc", "close"),
        ],
    )
    .spans;
    let below = total.saturating_sub(view.scroll + chunks[0].height as usize);
    if below > 0 {
        spans.push(Span::styled(
            format!("   {} {below} more", g.down),
            theme.dim,
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), chunks[1]);
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

    // A flash (post-update summary, scan failure) rides the bottom border so
    // no layout shifts; the next key press clears it.
    if let Some(flash) = app.flash() {
        let hint = format!(" {flash} ");
        let w = (hint.chars().count() as u16).min(area.width.saturating_sub(4));
        let rect = Rect {
            x: area.x + 2,
            y: area.bottom().saturating_sub(1),
            width: w,
            height: 1,
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(hint, theme.accent))),
            rect,
        );
    }

    if inner.width < 56 || inner.height < 14 {
        draw_dashboard_flat(frame, inner, app);
        return;
    }

    let cols =
        Layout::horizontal([Constraint::Percentage(46), Constraint::Percentage(54)]).split(inner);
    let left = Layout::vertical([Constraint::Min(5), Constraint::Length(8)]).split(cols[0]);
    let right = Layout::vertical([Constraint::Min(5), Constraint::Length(5)]).split(cols[1]);

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
        Paragraph::new(dashboard_key_rows(
            theme,
            keys_inner.width as usize,
            keys_inner.height as usize,
        )),
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
        Paragraph::new(dashboard_key_rows(
            theme,
            chunks[3].width as usize,
            chunks[3].height as usize,
        )),
        chunks[3],
    );
}

/// One styled key-hint line: the key bold, its label dim, dim bullets between.
fn keys_line(theme: &Theme, pairs: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, (key, label)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(
                format!("  {}  ", theme.glyphs.bullet),
                theme.dim,
            ));
        }
        spans.push(Span::styled((*key).to_string(), theme.title));
        spans.push(Span::styled(format!(" {label}"), theme.dim));
    }
    Line::from(spans)
}

/// One key hint: the key as displayed, and what it does.
type KeyHint<'a> = (&'a str, &'a str);

/// Columns between two hints on a row — the padded bullet `keys_line` draws.
const HINT_SEP: usize = 5;

/// Columns one hint occupies: key + space + label.
fn hint_width((key, label): KeyHint<'_>) -> usize {
    key.chars().count() + 1 + label.chars().count()
}

/// Wrap hints across rows of `width` columns, preserving their order. A hint
/// wider than the pane on its own is skipped — there is nothing sensible to
/// render for it.
fn wrap_hints<'a>(hints: &[KeyHint<'a>], width: usize) -> Vec<Vec<KeyHint<'a>>> {
    let mut rows: Vec<Vec<KeyHint<'a>>> = Vec::new();
    let mut row: Vec<KeyHint<'a>> = Vec::new();
    let mut used = 0;
    for &hint in hints {
        let w = hint_width(hint);
        if w > width {
            continue;
        }
        if row.is_empty() {
            row.push(hint);
            used = w;
        } else if used + HINT_SEP + w <= width {
            row.push(hint);
            used += HINT_SEP + w;
        } else {
            rows.push(std::mem::take(&mut row));
            row.push(hint);
            used = w;
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

/// Fit ranked hints into at most `max_rows` rows, dropping the least useful
/// first (highest rank goes first).
///
/// Hints keep their *display* order — only which ones survive depends on the
/// rank — so a narrow pane reads the same way a wide one does, with fewer
/// entries. Nothing is ever clipped: a dropped hint beats half a word.
fn fit_hints<'a>(
    ranked: &[(u8, KeyHint<'a>)],
    width: usize,
    max_rows: usize,
) -> Vec<Vec<KeyHint<'a>>> {
    let mut keep = ranked.to_vec();
    loop {
        let hints: Vec<KeyHint<'a>> = keep.iter().map(|&(_, hint)| hint).collect();
        let rows = wrap_hints(&hints, width);
        if rows.len() <= max_rows {
            return rows;
        }
        let Some(at) = keep
            .iter()
            .enumerate()
            .max_by_key(|(_, (rank, _))| *rank)
            .map(|(i, _)| i)
        else {
            return Vec::new();
        };
        keep.remove(at);
    }
}

/// The dashboard key hints in display order, each with a drop rank.
///
/// `enter update` is rank 0 and never drops — running the update is what the
/// tool is for. `q quit` is next safest to keep, since a user who cannot find
/// the exit is stuck. `L log` goes first: it is the least urgent thing on the
/// screen and the log is reachable again from anywhere.
fn dashboard_hints<'a>(updown: &'a str, leftright: &'a str) -> [(u8, KeyHint<'a>); 10] {
    [
        (4, (updown, "move")),
        (6, (leftright, "pane")),
        (3, ("i", "packages")),
        (2, ("space", "toggle")),
        (0, ("enter", "update")),
        (5, ("r", "refresh")),
        (7, ("o", "overlaps")),
        (8, ("c", "cleanup")),
        (9, ("L", "log")),
        (1, ("q", "quit")),
    ]
}

/// The dashboard key hints, wrapped to whatever the keys pane actually has.
fn dashboard_key_rows(theme: &Theme, width: usize, max_rows: usize) -> Vec<Line<'static>> {
    let g = theme.glyphs;
    let updown = format!("{}/{}", g.up, g.down);
    let leftright = format!("{}/{}", g.left, g.right);
    fit_hints(&dashboard_hints(&updown, &leftright), width, max_rows)
        .iter()
        .map(|row| keys_line(theme, row))
        .collect()
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
/// dashboard debut (design §13 "dashboard enrichment").
fn render_system_pane(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let pane = subpane(theme, " system ");
    let inner = pane.inner(area);
    frame.render_widget(pane, area);

    let kv = |label: &str, value: Span<'static>| {
        Line::from(vec![
            Span::styled(format!("{} ", theme.glyphs.bullet), theme.dim),
            Span::styled(format!("{:13}", label), theme.dim),
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
    // What `u` will run right now — the plan-level summary (P1: what will
    // happen is always visible before the key is pressed).
    let plan = app.update_plan();
    let plan_span = if plan.is_empty() {
        Span::styled("nothing to run".to_string(), theme.dim)
    } else {
        Span::styled(
            format!(
                "{} packages / {} sources",
                plan.total_targets(),
                plan.source_count()
            ),
            theme.accent,
        )
    };
    // Only a finding gets a row: a machine running what it has installed says
    // nothing, the way a clean cache suggests nothing (#3).
    let reboot = app.reboot_status();
    let mut lines = vec![kv("plan", plan_span)];
    if let Some(label) = reboot.label() {
        lines.push(kv(
            "reboot",
            Span::styled(
                label,
                if reboot.is_required() {
                    theme.accent
                } else {
                    theme.dim
                },
            ),
        ));
    }
    let stale = app.stale_units();
    if !stale.is_empty() {
        lines.push(kv(
            "services",
            Span::styled(format!("{} want restarting", stale.len()), theme.accent),
        ));
    }
    lines.extend([
        kv("pacman cache", cache),
        kv("orphans", count(app.orphan_count())),
        kv("overlaps", count(app.overlap_count())),
        kv("scanned", scanned_span(app)),
    ]);
    if app.stale_update_counts() {
        lines.push(Line::from(Span::styled(
            "stale? install pacman-contrib",
            theme.accent,
        )));
    }
    // Why the aur source is degraded, shown only while it is the selected row
    // (the pane already follows the sources cursor) and in accent rather than
    // dim: it is the one line here that asks the user to do something.
    if let Some(note) = app.selected_aur_note() {
        lines.push(Line::from(Span::styled(note, theme.accent)));
    }
    // Deliberately unwrapped: the pane has exactly one row to spare and the
    // compact note is written to fit it. Wrapping would push the last row out
    // of a fixed-height pane, which is how a clipped half-sentence happens.
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Pending-updates pane: grouped per source, capped per group.
fn render_updates_pane(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let total = app.total_updates();
    // The pane follows the sources cursor: only the selected source's
    // updates show (user decision 2026-07-08).
    let source = app.dash_source();
    let ups = source.map(|s| app.updates_for(&s.id)).unwrap_or_default();
    let title = match source {
        Some(s) if !ups.is_empty() => format!(" pending updates · {} ({}) ", s.id, ups.len()),
        Some(s) => format!(" pending updates · {} ", s.id),
        None => " pending updates ".to_string(),
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
    if ups.is_empty() {
        // Other sources have updates; this one is clean.
        let name = source.map(|s| s.id.to_string()).unwrap_or_default();
        frame.render_widget(
            Paragraph::new(Span::styled(format!("{name} is up to date"), theme.dim))
                .alignment(Alignment::Center),
            centered(inner, inner.width, 1),
        );
        return;
    }

    // The full list — ↑/↓ (j/k) scroll it while the pane has focus.
    let mut lines: Vec<Line> = Vec::new();
    let name_w = ups.iter().map(|u| u.package_name.len()).max().unwrap_or(0);
    for u in &ups {
        let new_version = if u.available_version.is_empty() {
            Span::styled("?", theme.dim)
        } else {
            Span::styled(u.available_version.clone(), theme.accent)
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{:name_w$}", u.package_name), theme.primary),
            Span::raw("  "),
            Span::styled(u.current_version.clone(), theme.dim),
            Span::styled(format!(" {} ", theme.glyphs.arrow), theme.dim),
            new_version,
        ]));
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

    let aur_id = crate::model::SourceId::aur().to_string();
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
        Cell::from(""),
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
            // A source carrying a note is flagged on its own row, whether or
            // not it still works — a stale pin leaves the aur source perfectly
            // functional, and without this the only hint lives behind the
            // cursor. The explanation stays in the system pane; this just says
            // there is one.
            let warned = r.id == aur_id && app.scan().aur_helper.note().is_some();
            // "no helper" rather than the generic "not found", but only when
            // the helper is actually the reason — a missing pacman takes the
            // aur source down too, and that is a different sentence.
            let status = match (r.available, warned) {
                (true, false) => {
                    Span::styled(format!("{} ok", theme.glyphs.available), theme.success)
                }
                (true, true) => Span::styled(format!("{} ok", theme.glyphs.warning), theme.accent),
                (false, warned) => {
                    let reason = if r.id == aur_id && app.scan().aur_helper.helper().is_none() {
                        "no helper"
                    } else {
                        "not found"
                    };
                    let (glyph, style) = if warned {
                        (theme.glyphs.warning, theme.accent)
                    } else {
                        (theme.glyphs.unavailable, theme.unavailable)
                    };
                    Span::styled(format!("{glyph} {reason}"), style)
                }
            };
            let updates = if r.updates > 0 {
                Span::styled(r.updates.to_string(), theme.accent)
            } else {
                Span::styled(r.updates.to_string(), theme.dim)
            };
            // Space toggles the source in/out of the plan; a clean source
            // has nothing to toggle and shows a dim dash.
            let toggle = match r.enabled {
                Some(true) => Span::styled(format!("[{}]", theme.glyphs.check), theme.success),
                Some(false) => Span::styled("[ ]".to_string(), theme.dim),
                None => Span::styled(" - ".to_string(), theme.dim),
            };
            Row::new(vec![
                Cell::from(toggle),
                Cell::from(r.id.clone()),
                Cell::from(Line::from(r.installed.to_string()).alignment(Alignment::Right)),
                Cell::from(Line::from(updates).alignment(Alignment::Right)),
                Cell::from(status),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(3),
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

/// The last row of `area` (for a footer outside a Layout split).
fn bottom_line(area: Rect) -> Rect {
    Rect {
        x: area.x,
        y: area.bottom().saturating_sub(1),
        width: area.width,
        height: 1,
    }
}

// ---------------------------------------------------------------------------
// Overlap screen (roadmap v0.1.4) — advisory only, no actions
// ---------------------------------------------------------------------------

fn draw_overlaps(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let overlaps = app.overlaps();
    let title = if overlaps.is_empty() {
        " paclens · overlaps ".to_string()
    } else {
        format!(" paclens · overlaps ({}) ", overlaps.len())
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(theme.border_set)
        .border_style(theme.border)
        .title(Span::styled(title, theme.title))
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if overlaps.is_empty() {
        frame.render_widget(
            Paragraph::new("No overlaps detected — nothing is installed both ways")
                .style(theme.success)
                .alignment(Alignment::Center),
            centered(inner, inner.width, 1),
        );
        render_overlaps_footer(frame, bottom_line(inner), theme, false, false);
        return;
    }

    let chunks = Layout::vertical([
        Constraint::Min(4),     // candidate list
        Constraint::Length(11), // tradeoff detail pane
        Constraint::Length(1),  // footer
    ])
    .split(inner);

    let header = Row::new(vec![
        Cell::from("NAME"),
        Cell::from("NATIVE"),
        Cell::from("FLATPAK"),
        Cell::from("CONFIDENCE"),
        Cell::from("MATCHED VIA"),
    ])
    .style(theme.header);
    let body: Vec<Row> = overlaps
        .iter()
        .map(|o| {
            let native = o
                .native_package
                .as_ref()
                .map(|p| p.version.clone())
                .unwrap_or_default();
            let flatpak = o
                .flatpak_app
                .as_ref()
                .map(|p| p.version.clone())
                .unwrap_or_default();
            Row::new(vec![
                Cell::from(Span::styled(o.display_name.clone(), theme.primary)),
                Cell::from(Span::styled(native, theme.dim)),
                Cell::from(Span::styled(flatpak, theme.dim)),
                Cell::from(confidence_span(o.confidence, theme)),
                Cell::from(Span::styled(o.match_method.to_string(), theme.dim)),
            ])
        })
        .collect();
    let table = Table::new(
        body,
        [
            Constraint::Min(16),
            Constraint::Length(14),
            Constraint::Length(14),
            Constraint::Length(10),
            Constraint::Min(12),
        ],
    )
    .header(header)
    .column_spacing(2)
    .row_highlight_style(theme.selected)
    .highlight_symbol(theme.glyphs.pointer);
    let mut state = TableState::default();
    state.select(Some(app.overlap_cursor()));
    frame.render_stateful_widget(table, chunks[0], &mut state);

    if app.is_migrate_open()
        && let Some(report) = app.migration_report()
    {
        render_migrate_pane(frame, chunks[1], app, &report);
    } else if let Some(overlap) = app.selected_overlap() {
        render_tradeoff_pane(frame, chunks[1], app, overlap);
    }
    render_overlaps_footer(
        frame,
        chunks[2],
        theme,
        app.is_migrate_open(),
        app.is_removal_armed(),
    );
}

fn confidence_span(confidence: crate::model::Confidence, theme: &Theme) -> Span<'static> {
    use crate::model::Confidence;
    let style = match confidence {
        Confidence::Confirmed => theme.success,
        Confidence::Inferred => theme.accent,
        Confidence::Unknown => theme.error,
    };
    Span::styled(confidence.to_string(), style)
}

/// The design §9 tradeoff table for the selected overlap, plus the primary
/// heuristic and the flatpak profile path/size when one exists.
fn render_tradeoff_pane(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    o: &crate::model::OverlapCandidate,
) {
    use crate::model::PrimaryHeuristic;
    let theme = &app.theme;
    let pane = subpane(theme, " tradeoff ");
    let inner = pane.inner(area);
    frame.render_widget(pane, area);

    let t = &o.tradeoff;
    let native_name = o
        .native_package
        .as_ref()
        .map(|p| p.name.clone())
        .unwrap_or_default();
    let flatpak_name = o
        .flatpak_app
        .as_ref()
        .map(|p| p.name.clone())
        .unwrap_or_default();
    let profile = match (&t.flatpak_profile_path, t.flatpak_profile_size_bytes) {
        (Some(path), Some(bytes)) => {
            format!("{} ({})", path.display(), crate::format::human_bytes(bytes))
        }
        _ => "none found".to_string(),
    };
    let primary = match t.likely_primary {
        PrimaryHeuristic::Native => Span::styled("native — explicitly installed", theme.success),
        PrimaryHeuristic::Flatpak => Span::styled("flatpak — profile has user data", theme.success),
        PrimaryHeuristic::Unknown => Span::styled("unknown — check both", theme.dim),
    };

    let row = |label: &str, native: String, flatpak: String| {
        Line::from(vec![
            Span::styled(format!("{label:13}"), theme.dim),
            Span::styled(format!("{native:27}"), theme.primary),
            Span::styled(flatpak, theme.primary),
        ])
    };
    let lines = vec![
        Line::from(vec![
            Span::styled(format!("{:13}", ""), theme.dim),
            Span::styled(format!("{:27}", "native"), theme.header),
            Span::styled("flatpak", theme.header),
        ]),
        row("package", native_name, flatpak_name),
        row(
            "version",
            t.native_version.clone().unwrap_or_default(),
            t.flatpak_version.clone().unwrap_or_default(),
        ),
        row(
            "updates",
            "pacman (rolling)".to_string(),
            "flatpak remote".to_string(),
        ),
        row("profile", "~/.config, ~/.local/share".to_string(), profile),
        row("sandboxing", "no".to_string(), "yes (portals)".to_string()),
        row(
            "integration",
            "full (dbus, theming)".to_string(),
            "portal-gated".to_string(),
        ),
        Line::from(vec![
            Span::styled(format!("{:13}", "primary"), theme.dim),
            primary,
        ]),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The migration advisory pane (v0.4): Enter on an overlap swaps the
/// tradeoff pane for this. Per mapping two lines (from + to), then the
/// analyzer's warnings; truncated content points at the CLI report.
fn render_migrate_pane(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    report: &crate::model::MigrationReport,
) {
    use crate::model::PathKind;
    let theme = &app.theme;
    let g = theme.glyphs;
    let title = format!(
        " migrate {} {} {} [{}] ",
        report.display_name,
        g.arrow,
        report.direction.target(),
        report.confidence,
    );
    let pane = Block::default()
        .borders(Borders::ALL)
        .border_set(theme.border_set)
        .border_style(theme.border)
        .title(Span::styled(title, theme.header))
        .padding(Padding::horizontal(1));
    let inner = pane.inner(area);
    frame.render_widget(pane, area);

    let mut lines: Vec<Line> = Vec::new();
    if report.mappings.is_empty() {
        lines.push(Line::from(Span::styled(
            "no profile data found on either side — nothing to migrate",
            theme.dim,
        )));
    }
    for m in &report.mappings {
        let (from, from_bytes, to, to_bytes) = m.endpoints(report.direction);
        let mut spans = vec![
            Span::styled(format!("{:9}", m.kind.to_string()), theme.dim),
            Span::styled(from.to_string(), theme.primary),
        ];
        match from_bytes {
            Some(b) => spans.push(Span::styled(
                format!("  {}", crate::format::human_bytes(b)),
                theme.accent,
            )),
            None => spans.push(Span::styled("  (not present)", theme.dim)),
        }
        spans.push(Span::styled(format!("  [{}]", m.confidence), theme.dim));
        lines.push(Line::from(spans));

        let to_span = if m.kind == PathKind::Cache {
            Span::styled("(skip — cache regenerates)", theme.dim)
        } else if let Some(b) = to_bytes {
            Span::styled(
                format!("{to}  (exists, {})", crate::format::human_bytes(b)),
                theme.accent,
            )
        } else {
            Span::styled(to.to_string(), theme.primary)
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{:9}{} ", "", g.arrow), theme.dim),
            to_span,
        ]));
    }
    for w in &report.warnings {
        lines.push(Line::from(vec![
            Span::styled("! ", theme.accent),
            Span::styled(w.clone(), theme.dim),
        ]));
    }

    // No scrolling in a fixed pane: truncate and point at the CLI report.
    let fit = inner.height as usize;
    if lines.len() > fit && fit > 0 {
        lines.truncate(fit - 1);
        lines.push(Line::from(Span::styled(
            format!("… more — run `paclens migrate {}`", report.display_name),
            theme.dim,
        )));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_overlaps_footer(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    migrate_open: bool,
    removal_armed: bool,
) {
    let g = theme.glyphs;
    let updown = format!("{}/{}", g.up, g.down);
    let (mut line, note) = if removal_armed {
        (
            keys_line(
                theme,
                &[("R", "remove source"), ("esc", "keep it"), ("L", "log")],
            ),
            "   verify the target launched first",
        )
    } else if migrate_open {
        (
            keys_line(
                theme,
                &[
                    (&updown, "select"),
                    ("d", "flip direction"),
                    ("x", "run"),
                    ("enter", "close"),
                    ("esc", "back"),
                ],
            ),
            "   backups first — nothing deleted",
        )
    } else {
        (
            keys_line(
                theme,
                &[
                    (&updown, "select"),
                    ("enter", "migrate report"),
                    ("esc", "back"),
                    ("q", "quit"),
                ],
            ),
            "   advisory only — decide, then act yourself",
        )
    };
    line.spans.push(Span::styled(note, theme.dim));
    frame.render_widget(Paragraph::new(line), area);
}

// ---------------------------------------------------------------------------
// Cleanup screen (roadmap v0.1.5) — advisory only, no actions
// ---------------------------------------------------------------------------

fn draw_cleanup(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let orphans = app.cleanup_orphans();
    let title = if orphans.is_empty() {
        " paclens · cleanup ".to_string()
    } else {
        format!(" paclens · cleanup ({} orphans) ", orphans.len())
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(theme.border_set)
        .border_style(theme.border)
        .title(Span::styled(title, theme.title))
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::vertical([Constraint::Min(4), Constraint::Length(1)]).split(inner);
    let panes = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(chunks[0]);

    render_orphan_pane(frame, panes[0], app);
    if app.is_cleanup_why_open() {
        render_cleanup_why_pane(frame, panes[1], app);
    } else {
        render_cache_pane(frame, panes[1], app);
    }

    let g = theme.glyphs;
    let updown = format!("{}/{}", g.up, g.down);
    let mut footer = keys_line(
        theme,
        &[
            (&updown, "select"),
            ("enter", "why"),
            ("esc", "back"),
            ("q", "quit"),
        ],
    );
    footer.spans.push(Span::styled(
        "   advisory only — commands are yours to run",
        theme.dim,
    ));
    frame.render_widget(Paragraph::new(footer), chunks[1]);
}

/// Orphan candidates: dependency-installed, nothing requires them. Enter
/// opens the why pane before any suggestion is trusted.
fn render_orphan_pane(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let pane = subpane(theme, " orphans ");
    let inner = pane.inner(area);
    frame.render_widget(pane, area);

    let orphans = app.cleanup_orphans();
    if orphans.is_empty() {
        frame.render_widget(
            Paragraph::new("no orphan candidates — every dependency has a user")
                .style(theme.success)
                .alignment(Alignment::Center),
            centered(inner, inner.width, 1),
        );
        return;
    }

    let body: Vec<Row> = orphans
        .iter()
        .map(|name| {
            let size = match app.orphan_size(name) {
                Some(b) => Span::styled(crate::format::human_bytes(b), theme.dim),
                None => Span::styled("—".to_string(), theme.dim),
            };
            Row::new(vec![
                Cell::from(Span::styled(name.clone(), theme.primary)),
                Cell::from(Line::from(size).alignment(Alignment::Right)),
            ])
        })
        .collect();
    let table = Table::new(body, [Constraint::Min(16), Constraint::Length(10)])
        .header(
            Row::new(vec![
                Cell::from("NAME"),
                Cell::from(Line::from("SIZE").alignment(Alignment::Right)),
            ])
            .style(theme.header),
        )
        .column_spacing(2)
        .row_highlight_style(theme.selected)
        .highlight_symbol(theme.glyphs.pointer);
    let mut state = TableState::default();
    state.select(Some(app.cleanup_cursor()));
    frame.render_stateful_widget(table, inner, &mut state);
}

/// Cache summary + the suggested commands, shown as copiable advisory text.
fn render_cache_pane(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let pane = subpane(theme, " cache ");
    let inner = pane.inner(area);
    frame.render_widget(pane, area);

    let kv = |label: &str, value: Vec<Span<'static>>| {
        let mut spans = vec![
            Span::styled(format!("{} ", theme.glyphs.bullet), theme.dim),
            Span::styled(format!("{:17}", label), theme.dim),
        ];
        spans.extend(value);
        Line::from(spans)
    };
    let sizes = &app.scan().cache_sizes;
    // Honesty rule (dev-notes 2026-07-14): the total is mostly the
    // current-version tarballs paccache never touches — show what a
    // `paccache -rk3` would actually free next to it.
    let pacman_cache = match (
        sizes.pacman_cache_bytes,
        sizes.pacman_cache_reclaimable_bytes,
    ) {
        (Some(b), Some(0)) => vec![
            Span::styled(crate::format::human_bytes(b), theme.accent),
            Span::styled(" (nothing to reclaim)", theme.dim),
        ],
        (Some(b), Some(r)) => vec![
            Span::styled(crate::format::human_bytes(b), theme.accent),
            Span::styled(
                format!(" ({} reclaimable)", crate::format::human_bytes(r)),
                theme.primary,
            ),
        ],
        (Some(b), None) => vec![Span::styled(crate::format::human_bytes(b), theme.accent)],
        (None, _) => vec![Span::styled("—".to_string(), theme.dim)],
    };
    let (unused_count, unused_bytes) = app.unused_runtime_summary();
    let unused = if unused_count == 0 {
        Span::styled("none".to_string(), theme.dim)
    } else {
        Span::styled(
            format!(
                "{unused_count} ({})",
                crate::format::human_bytes(unused_bytes)
            ),
            theme.accent,
        )
    };
    let orphan_bytes: u64 = app
        .cleanup_orphans()
        .iter()
        .filter_map(|n| app.orphan_size(n))
        .sum();

    let stale = app.stale_units();
    let mut lines = vec![kv("pacman cache", pacman_cache)];
    // Named for the helper actually in use — "paru build cache" on a machine
    // with only yay is a figure attributed to the wrong tool.
    let aur_helper = app.scan().aur_helper.helper();
    if let (Some(b), Some(helper)) = (sizes.aur_cache_bytes, aur_helper) {
        lines.push(kv(
            &format!("{} build cache", helper.bin()),
            vec![Span::styled(crate::format::human_bytes(b), theme.accent)],
        ));
    }
    lines.extend([
        kv("unused runtimes", vec![unused]),
        kv(
            "stale services",
            vec![if stale.is_empty() {
                Span::styled("none".to_string(), theme.dim)
            } else {
                Span::styled(format!("{} [inferred]", stale.len()), theme.accent)
            }],
        ),
        kv(
            "config leftovers",
            vec![if app.pacfiles().is_empty() {
                // A clean row, not an absent section: "none" is the answer
                // the question has on most machines, and it is worth saying.
                Span::styled("none".to_string(), theme.dim)
            } else {
                Span::styled(format!("{}", app.pacfiles().len()), theme.accent)
            }],
        ),
        kv(
            "orphans",
            vec![if app.cleanup_orphans().is_empty() {
                Span::styled("none".to_string(), theme.dim)
            } else {
                Span::styled(
                    format!(
                        "{} ({})",
                        app.cleanup_orphans().len(),
                        crate::format::human_bytes(orphan_bytes)
                    ),
                    theme.accent,
                )
            }],
        ),
        Line::default(),
        Line::from(Span::styled(
            "suggested — review, then run yourself:",
            theme.dim,
        )),
    ]);
    // Only suggest what would actually do something.
    if sizes.pacman_cache_reclaimable_bytes != Some(0) {
        lines.push(Line::from(Span::styled("  paccache -rk3", theme.primary)));
    }
    lines.push(Line::from(Span::styled(
        "  flatpak uninstall --unused",
        theme.primary,
    )));
    if let (Some(_), Some(helper)) = (sizes.aur_cache_bytes, aur_helper) {
        lines.push(Line::from(Span::styled(
            format!("  {}", helper.clean_command().join(" ")),
            theme.primary,
        )));
    }
    // Inferred, so it says so: a deleted mapping proves the file changed,
    // not that anything is broken (#4).
    if !stale.is_empty() {
        for u in stale.iter().take(4) {
            lines.push(Line::from(Span::styled(
                format!("  {}", u.restart_command()),
                theme.primary,
            )));
            if u.session_critical {
                lines.push(Line::from(Span::styled(
                    "  (this ends your session)".to_string(),
                    theme.accent,
                )));
            }
        }
        if stale.len() > 4 {
            lines.push(Line::from(Span::styled(
                format!("  … {} more", stale.len() - 4),
                theme.dim,
            )));
        }
    }

    // Merging a config is genuinely destructive, so this stays where the
    // trust ladder puts it: text you read, then run yourself (#2).
    let pacfiles = app.pacfiles();
    if !pacfiles.is_empty() {
        let diff = app.diff_program();
        lines.push(Line::from(Span::styled(
            format!("  {}", crate::analyzer::pacfiles::review_all_command(&diff)),
            theme.primary,
        )));
        // The newest few by name, so the list says *what* is waiting rather
        // than only how many. The rest are behind pacdiff above.
        for f in pacfiles.iter().take(3) {
            lines.push(Line::from(Span::styled(
                format!("  {} {}", f.kind.label(), f.base()),
                theme.dim,
            )));
        }
        if pacfiles.len() > 3 {
            lines.push(Line::from(Span::styled(
                format!("  … {} more", pacfiles.len() - 3),
                theme.dim,
            )));
        }
    }
    if !app.cleanup_orphans().is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  sudo pacman -Rns {}", app.cleanup_orphans().join(" ")),
            theme.primary,
        )));
        lines.push(Line::from(Span::styled(
            "  (check each orphan's why first — enter on the list)",
            theme.dim,
        )));
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }),
        inner,
    );
}

/// The selected orphan's why report, in the same pane the cache summary
/// uses (Enter toggles between them).
fn render_cleanup_why_pane(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let pane = subpane(theme, " why ");
    let inner = pane.inner(area);
    frame.render_widget(pane, area);
    let Some(report) = app.cleanup_why_report() else {
        frame.render_widget(Paragraph::new("no selection").style(theme.dim), inner);
        return;
    };
    let lines = why_pane_lines(&report, theme, true);
    frame.render_widget(
        Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }),
        inner,
    );
}

// ---------------------------------------------------------------------------
// Package list + why side pane
// ---------------------------------------------------------------------------

/// Table body rows of the packages screen for a terminal `height` — mirrors
/// `draw_packages`' layout: borders (2) + summary (1) + table header (1) +
/// filter line (1) + footer (1). The event loop feeds this to
/// `App::set_pkg_viewport` so the scrolloff math knows the viewport.
pub fn pkg_body_rows(height: u16) -> usize {
    (height as usize).saturating_sub(6)
}

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
    summary.extend([
        Span::styled(format!(" {b} ", b = theme.glyphs.bullet), theme.dim),
        Span::styled(format!("sort: {}", app.pkg_sort().label()), theme.dim),
    ]);
    frame.render_widget(Paragraph::new(Line::from(summary)), chunks[0]);

    // The pane is permanent: it carries the description in full, which no
    // column ever could. A landscape terminal puts it beside the table; too
    // narrow for that and the split turns vertical, because a side pane there
    // would leave neither half readable.
    if chunks[1].width >= SIDE_PANE_MIN_WIDTH {
        let geo = pkg_geometry(app, chunks[1].width, true, app.pane_bias());
        let panes = Layout::horizontal([Constraint::Length(geo.table_w), Constraint::Min(20)])
            .split(chunks[1]);
        render_package_table(frame, panes[0], app, &geo);
        render_why_pane(frame, panes[1], app, Borders::LEFT);
    } else {
        let stack = Layout::vertical([Constraint::Min(3), Constraint::Length(7)]).split(chunks[1]);
        let geo = pkg_geometry(app, stack[0].width, false, 0);
        render_package_table(frame, stack[0], app, &geo);
        render_why_pane(frame, stack[1], app, Borders::TOP);
    }

    if show_filter {
        render_filter_line(frame, chunks[2], app);
    }

    let g = theme.glyphs;
    let updown = format!("{}/{}", g.up, g.down);
    let line = if app.is_filter_active() {
        keys_line(
            theme,
            &[("type", "to filter"), ("enter", "apply"), ("esc", "cancel")],
        )
    } else {
        keys_line(
            theme,
            &[
                (&updown, "navigate"),
                ("/", "filter"),
                ("s", "sort"),
                ("[ ]", "pane"),
                ("esc", "back"),
                ("q", "quit"),
            ],
        )
    };
    frame.render_widget(Paragraph::new(line), chunks[3]);
}

/// VERSION + TYPE + SIZE (+ KIND) plus the spacing between every column.
fn fixed_columns_width(kind_col: bool) -> u16 {
    12 + 10 + 10 + if kind_col { 7 + 2 } else { 0 } + 2 * 4
}

/// NAME takes what its longest entry needs, up to `cap`. Group headers
/// ("pending updates (12)") live in the NAME cell, so they set the floor too —
/// a header cut in half is worse than a wide column.
fn name_column_width(rows: &[crate::tui::app::PkgRow<'_>], cap: u16) -> u16 {
    let longest = rows
        .iter()
        .map(|r| match r {
            crate::tui::app::PkgRow::Pkg(p) => p.name.chars().count(),
            crate::tui::app::PkgRow::Header(label, count) => {
                label.chars().count() + count.to_string().len() + 5
            }
        })
        .max()
        .unwrap_or(20) as u16;
    longest.min(cap).max(12)
}

/// How the package screen divides a width. Computed once and handed to both
/// the split and the table, because two sides deciding it separately is what
/// leaves a dead strip between them.
struct PkgGeometry {
    /// NAME absorbs whatever the fixed columns leave, so the table always
    /// fills its side and nothing sits dead against the divider.
    name_w: u16,
    /// The pane takes whatever is left of the area.
    table_w: u16,
    /// VERSION and SIZE do not fit; the table shows name and reason only.
    narrow: bool,
}

/// `available` is the whole area the table and pane share. Stacked, the table
/// takes all of it; side by side, the table takes what its columns need and
/// the pane the rest, up to the pane's cap.
fn pkg_geometry(app: &App, available: u16, pane: bool, bias: i16) -> PkgGeometry {
    let kind_col = app.pkg_source_is_flatpak();
    let fixed = fixed_columns_width(kind_col) + app.theme.glyphs.pointer.chars().count() as u16;
    let narrow = fixed + 12 > if pane { available / 3 * 2 } else { available };
    if narrow {
        let table_w = if pane { available / 2 } else { available };
        return PkgGeometry {
            name_w: table_w.saturating_sub(12).max(12),
            table_w,
            narrow,
        };
    }

    let name_w = name_column_width(&app.pkg_rows(), available.saturating_sub(fixed));
    let table_w = if pane {
        // The pane's share of what the columns leave, then the reader's own
        // nudge on top of it.
        let leftover = available.saturating_sub(fixed + name_w);
        let pane_w = (i32::from(leftover.min(PANE_MAX_WIDTH)) + i32::from(bias))
            .clamp(24, i32::from(available) / 3 * 2) as u16;
        available.saturating_sub(pane_w)
    } else {
        available
    };
    PkgGeometry {
        name_w: table_w.saturating_sub(fixed).max(12),
        table_w,
        narrow,
    }
}

/// A narrow table carries NAME and VERSION only. TYPE and SIZE (and KIND) go
/// because at that width they truncate into noise, and because a squeezed
/// list is still answering "what do I have" — which the pane, always open,
/// answers at length for the row under the cursor (#57).
fn render_package_table(frame: &mut Frame, area: Rect, app: &App, geo: &PkgGeometry) {
    let theme = &app.theme;
    let rows = app.pkg_rows();
    let narrow = geo.narrow;
    // KIND only says something for Flatpak (app vs runtime); everywhere else
    // it is a column of em dashes (#55).
    let kind_col = !narrow && app.pkg_source_is_flatpak();
    let name_w = geo.name_w;

    if rows.is_empty() {
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
        Row::new(vec![Cell::from("NAME"), Cell::from("VERSION")])
    } else {
        let mut cells = vec![Cell::from("NAME"), Cell::from("VERSION")];
        if kind_col {
            cells.push(Cell::from("KIND"));
        }
        cells.extend([
            Cell::from("TYPE"),
            Cell::from(Line::from("SIZE").alignment(Alignment::Right)),
        ]);
        Row::new(cells)
    }
    .style(theme.header);

    // A pending update reads accented whatever the sort — it is the same
    // fact about the package either way. Under a filter the accent would
    // compete with the match highlighting.
    let accent_pending = app.pkg_filter().is_empty();
    let body: Vec<Row> = rows
        .iter()
        .map(|row| {
            let p = match row {
                crate::tui::app::PkgRow::Header(..) => {
                    // Painted over afterwards, full width: a table row cannot
                    // span columns, and a group header that stops at the NAME
                    // column reads as another row.
                    let mut cells = vec![Cell::from("")];
                    let width = if narrow { 2 } else { 3 + usize::from(kind_col) };
                    cells.resize_with(width, || Cell::from(""));
                    return Row::new(cells);
                }
                crate::tui::app::PkgRow::Pkg(p) => p,
            };
            let name = if accent_pending && app.pkg_pending(&p.name) {
                Span::styled(p.name.clone(), theme.accent)
            } else {
                Span::styled(p.name.clone(), theme.primary)
            };
            let reason = match p.install_reason {
                crate::model::InstallReason::Explicit => Span::styled("explicit", theme.primary),
                crate::model::InstallReason::Dependency => Span::styled("dependency", theme.dim),
                crate::model::InstallReason::Unknown => Span::styled("—", theme.dim),
            };
            if narrow {
                // Version over reason: at this width the question is what you
                // have, not how it got here (#57).
                return Row::new(vec![
                    Cell::from(name),
                    Cell::from(Span::styled(p.version.clone(), theme.dim)),
                ]);
            }
            let size = match p.size_bytes {
                Some(b) => Span::styled(crate::format::human_bytes(b), theme.primary),
                None => Span::styled("—".to_string(), theme.dim),
            };
            let mut cells = vec![
                Cell::from(name),
                Cell::from(Span::styled(p.version.clone(), theme.dim)),
            ];
            if kind_col {
                let kind = if p.runtime {
                    Span::styled("runtime", theme.dim)
                } else {
                    Span::styled("app", theme.primary)
                };
                cells.push(Cell::from(kind));
            }
            cells.extend([
                Cell::from(reason),
                Cell::from(Line::from(size).alignment(Alignment::Right)),
            ]);
            Row::new(cells)
        })
        .collect();

    let widths: Vec<Constraint> = if narrow {
        vec![Constraint::Min(20), Constraint::Length(12)]
    } else {
        let mut w = vec![Constraint::Length(name_w), Constraint::Length(12)];
        if kind_col {
            w.push(Constraint::Length(7));
        }
        w.extend([Constraint::Length(10), Constraint::Length(10)]);
        w
    };
    let table = Table::new(body, widths)
        .header(header)
        .column_spacing(2)
        .row_highlight_style(theme.selected)
        .highlight_symbol(theme.glyphs.pointer);

    // Offset comes from the app's scrolloff math (row space); the selected
    // row is always inside that window, so ratatui keeps the offset as-is.
    let mut state = TableState::default().with_offset(app.pkg_offset());
    state.select(Some(app.pkg_row_index()));
    frame.render_stateful_widget(table, area, &mut state);

    for (i, row) in rows.iter().skip(app.pkg_offset()).enumerate() {
        let y = area.y + 1 + i as u16;
        if y >= area.y + area.height {
            break;
        }
        let crate::tui::app::PkgRow::Header(label, count) = row else {
            continue;
        };
        // The accent means "pending updates" everywhere else in the UI, so it
        // means it here too; every other group is structure, not news. Dimmed
        // either way: a divider that shouts competes with the rows it heads.
        let style = if *label == "pending updates" {
            theme.accent.add_modifier(Modifier::DIM)
        } else {
            theme.header
        };
        let titled = label
            .split(' ')
            .map(|word| {
                let mut c = word.chars();
                match c.next() {
                    Some(first) => format!("{}{}", first.to_uppercase(), c.as_str()),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        let body = format!("{titled} ({count})");
        // Two columns short of the edge, so the rule never butts against the
        // pane divider the way a row's last column does not.
        let rule = "─".repeat((area.width as usize).saturating_sub(body.chars().count() + 8));
        let line = Line::from(vec![
            Span::styled("  ── ", theme.border),
            Span::styled(body, style),
            Span::styled(format!(" {rule}"), theme.border),
        ]);
        frame.render_widget(
            Paragraph::new(line),
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            },
        );
    }
}

fn render_filter_line(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let shown = app.visible_packages().len();
    let cursor = if app.is_filter_active() { "▏" } else { "" };
    let mut spans = vec![
        Span::styled("/", theme.accent),
        Span::styled(format!("{}{cursor}", app.pkg_filter()), theme.primary),
        Span::styled(format!("   {shown} of {}", app.pkg_total()), theme.dim),
    ];
    // A row that matched on its description looks like a row that matched on
    // nothing, so say what the query actually hit (#58).
    let fields = app.filter_fields();
    if fields.len() > 1
        || fields
            .first()
            .is_some_and(|&(f, _)| f != PkgMatchField::Name)
    {
        let by = fields
            .iter()
            .map(|(f, n)| format!("{n} by {}", f.label()))
            .collect::<Vec<_>>()
            .join(", ");
        spans.push(Span::styled(
            format!("   {b} {by}", b = theme.glyphs.bullet),
            theme.dim,
        ));
    }
    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), area);
}

/// The why side pane: the analyzer's report for the row under the cursor,
/// live — same data the CLI prints (P5).
/// The package screen's side pane: what the row is, then why it is here.
/// `borders` is LEFT beside the table, TOP under it.
fn render_why_pane(frame: &mut Frame, area: Rect, app: &App, borders: Borders) {
    let theme = &app.theme;
    let block = Block::default()
        .borders(borders)
        .border_set(theme.border_set)
        .border_style(theme.border)
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(report) = app.why_report() else {
        frame.render_widget(Paragraph::new("no selection").style(theme.dim), inner);
        return;
    };
    // The description leads: it is the one thing the table can no longer show.
    let mut lines = Vec::new();
    if let Some(p) = app.selected_package() {
        lines.push(Line::from(vec![
            Span::styled(p.name.clone(), theme.accent),
            Span::styled(format!("  {}", p.version), theme.dim),
        ]));
        lines.push(Line::from(Span::styled(
            p.description
                .clone()
                .unwrap_or("no description".to_string()),
            theme.primary,
        )));
        lines.push(Line::default());
    }
    lines.extend(why_pane_lines(&report, theme, false));
    frame.render_widget(
        Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }),
        inner,
    );
}

/// `title` prints the `why · <name>` heading. The package screen's pane names
/// the package itself a line above, so it asks for the report without one.
fn why_pane_lines(
    report: &crate::analyzer::WhyReport,
    theme: &Theme,
    title: bool,
) -> Vec<Line<'static>> {
    use crate::analyzer::{Verdict, WhyReport};
    use crate::model::{InstallReason, SourceId};

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
        WhyReport::Found(p) => {
            let mut lines = if title {
                vec![
                    Line::from(Span::styled(format!("why · {}", p.package), theme.title)),
                    Line::default(),
                ]
            } else {
                Vec::new()
            };
            let is_alpm =
                p.source_id == SourceId::pacman() || p.source_id == crate::model::SourceId::aur();
            let reason = if !is_alpm {
                if p.runtime {
                    "flatpak runtime".to_string()
                } else {
                    "flatpak app · self-contained".to_string()
                }
            } else {
                match p.reason {
                    InstallReason::Explicit => "explicit".to_string(),
                    InstallReason::Dependency => match p.depth_from_explicit {
                        Some(d) => format!("dependency · {d} hop{}", plural(d)),
                        None => "dependency".to_string(),
                    },
                    InstallReason::Unknown => "unknown".to_string(),
                }
            };
            lines.push(kv("reason", reason, theme.primary));
            for caveat in &p.caveats {
                lines.push(Line::from(vec![
                    Span::styled(format!("{:10}", "caveat"), theme.dim),
                    Span::styled(caveat.clone(), theme.accent),
                ]));
            }
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
        Paragraph::new(keys_line(theme, &[("q", "quit")])),
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
    use crate::tui::app::{AppOptions, ExecKind};
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
        assert!(
            text.contains("enter update"),
            "footer hint missing:\n{text}"
        );
        assert!(text.contains("i packages"), "footer hint missing:\n{text}");
    }

    // --- key-hint wrapping ---

    /// Columns a wrapped row actually occupies once `keys_line` draws it.
    fn row_width(row: &[KeyHint<'_>]) -> usize {
        row.iter().map(|&h| hint_width(h)).sum::<usize>() + HINT_SEP * row.len().saturating_sub(1)
    }

    fn ranked() -> [(u8, KeyHint<'static>); 10] {
        dashboard_hints("^/v", "</>")
    }

    #[test]
    fn wrapped_rows_never_exceed_the_width() {
        let r = ranked();
        let hints: Vec<KeyHint<'_>> = r.iter().map(|&(_, h)| h).collect();
        for width in 10..=120 {
            for row in wrap_hints(&hints, width) {
                assert!(
                    row_width(&row) <= width,
                    "row {row:?} is {} wide at width {width}",
                    row_width(&row)
                );
            }
        }
    }

    #[test]
    fn fit_hints_respects_max_rows_at_every_width() {
        let r = ranked();
        for width in 6..=140 {
            for max_rows in 1..=4 {
                let rows = fit_hints(&r, width, max_rows);
                assert!(
                    rows.len() <= max_rows,
                    "{} rows at width {width}, max {max_rows}",
                    rows.len()
                );
                for row in &rows {
                    assert!(row_width(row) <= width, "clipped at width {width}: {row:?}");
                }
            }
        }
    }

    #[test]
    fn the_update_key_survives_every_width_that_can_show_anything() {
        let r = ranked();
        for width in 12..=140 {
            let rows = fit_hints(&r, width, 1);
            let shown: Vec<&str> = rows.iter().flatten().map(|&(k, _)| k).collect();
            assert!(
                shown.contains(&"enter"),
                "enter dropped at width {width}: {shown:?}"
            );
        }
    }

    #[test]
    fn the_log_hint_drops_before_quit_and_update() {
        let r = ranked();
        // One narrow row: only the highest-priority hints can survive.
        let rows = fit_hints(&r, 26, 1);
        let shown: Vec<&str> = rows.iter().flatten().map(|&(k, _)| k).collect();
        assert!(!shown.contains(&"L"), "log should have dropped: {shown:?}");
        assert!(shown.contains(&"enter"), "{shown:?}");
    }

    #[test]
    fn a_wide_pane_shows_every_hint() {
        let r = ranked();
        let rows = fit_hints(&r, 48, 3);
        let shown: Vec<&str> = rows.iter().flatten().map(|&(k, _)| k).collect();
        assert_eq!(shown.len(), 10, "not all hints shown: {shown:?}");
    }

    #[test]
    fn hints_keep_their_display_order_as_they_drop() {
        let r = ranked();
        let order: Vec<&str> = r.iter().map(|&(_, (k, _))| k).collect();
        for width in 12..=120 {
            let rows = fit_hints(&r, width, 3);
            let shown: Vec<&str> = rows.iter().flatten().map(|&(k, _)| k).collect();
            let mut expected = order.clone();
            expected.retain(|k| shown.contains(k));
            assert_eq!(shown, expected, "order scrambled at width {width}");
        }
    }

    #[test]
    fn dashboard_key_rows_fit_the_width_they_are_given() {
        // Both glyph sets: the unicode arrows and their ASCII fallbacks are
        // the same column count, so neither may overflow where the other fits.
        for theme in [Theme::none(), Theme::dark()] {
            for width in 8..=140 {
                for max_rows in 1..=3 {
                    let rows = dashboard_key_rows(&theme, width, max_rows);
                    assert!(rows.len() <= max_rows, "too many rows at {width}");
                    for line in &rows {
                        assert!(
                            line.width() <= width,
                            "line {} wide at width {width}: {line:?}",
                            line.width()
                        );
                    }
                }
            }
        }
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
        for pane in ["sources", "pending updates · pacman (2)", "system", "keys"] {
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
        assert!(text.contains("pending updates · pacman (30)"), "{text}");
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
    fn updates_pane_follows_the_selected_source() {
        let mut app = App::new(
            scan_with(vec![
                upd("linux", "6.9.1", "6.9.2", SourceId::pacman()),
                upd("org.x.App", "1.0", "1.1", SourceId::flatpak_user()),
            ]),
            Theme::none(),
            AppOptions::test(),
        );
        // Row 0 = pacman: only its update shows.
        let text = render(&app, 96, 24);
        assert!(text.contains("pending updates · pacman (1)"), "{text}");
        assert!(text.contains("linux"), "{text}");
        assert!(!text.contains("org.x.App"), "other source leaked:\n{text}");

        // Row 1 = flatpak-user: swaps over.
        app.on_next();
        let text = render(&app, 96, 24);
        assert!(
            text.contains("pending updates · flatpak-user (1)"),
            "{text}"
        );
        assert!(text.contains("org.x.App"), "{text}");
        assert!(!text.contains("linux"), "{text}");

        // Row 2 = flatpak-system: nothing pending for it.
        app.on_next();
        let text = render(&app, 96, 24);
        assert!(
            text.contains("flatpak-system is up to date"),
            "clean-source message missing:\n{text}"
        );
        assert!(!text.contains("org.x.App"), "{text}");
    }

    #[test]
    fn switching_sources_resets_the_updates_scroll() {
        let ups: Vec<PendingUpdate> = (0..30)
            .map(|i| upd(&format!("pkg{i:02}"), "1", "2", SourceId::pacman()))
            .collect();
        let mut app = App::new(scan_with(ups), Theme::none(), AppOptions::test());
        app.focus_updates();
        for _ in 0..10 {
            app.on_next();
        }
        assert_eq!(app.updates_scroll(), 10);
        app.focus_sources();
        app.on_next(); // different source selected
        assert_eq!(app.updates_scroll(), 0, "stale scroll survived");
    }

    #[test]
    fn package_list_renders_sort_headers_and_chip() {
        let mut s = scan_with(vec![upd("glibc", "2.39", "2.40", SourceId::pacman())]);
        s.packages = vec![
            rich_pkg("glibc", InstallReason::Dependency, Some(50_000_000), &[]),
            rich_pkg("bash", InstallReason::Explicit, None, &[]),
        ];
        let mut app = App::new(s, Theme::none(), AppOptions::test());
        app.open_packages();
        app.cycle_sort(); // size (the default) → updates
        let text = render(&app, 140, 22);
        assert!(text.contains("sort: updates"), "chip missing:\n{text}");
        assert!(
            text.contains("Pending Updates (1)"),
            "header missing:\n{text}"
        );
        assert!(text.contains("Up To Date (1)"), "{text}");
        assert!(text.contains("s sort"), "footer missing:\n{text}");

        app.cycle_sort(); // reason
        let text = render(&app, 140, 22);
        assert!(text.contains("sort: reason"), "{text}");
        assert!(text.contains("Explicit (1)"), "{text}");
        assert!(text.contains("Dependencies (1)"), "{text}");
    }

    #[test]
    fn package_list_scrolls_with_a_margin_not_pinned_to_the_bottom() {
        let mut s = scan_with(Vec::new());
        s.packages = (0..60)
            .map(|i| rich_pkg(&format!("pkg{i:02}"), InstallReason::Explicit, None, &[]))
            .collect();
        let mut app = App::new(s, Theme::none(), AppOptions::test());
        app.open_packages();
        app.set_pkg_viewport(pkg_body_rows(20)); // 14 body rows
        app.pkg_move(30);
        let text = render(&app, 96, 20);
        // Cursor row 30 sits 4 rows above the bottom: 31..=34 still visible.
        assert!(text.contains("pkg30"), "{text}");
        assert!(text.contains("pkg34"), "margin rows missing:\n{text}");
        assert!(!text.contains("pkg35"), "{text}");

        // Reverse: the cursor walks up through the view before it scrolls,
        // so the same window still shows (top row unchanged).
        app.pkg_move(-5);
        let text = render(&app, 96, 20);
        assert!(text.contains("pkg21"), "window moved too early:\n{text}");
        assert!(text.contains("pkg34"), "{text}");
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
        app.on_prev(); // back to pacman — the pane shows its 5 updates
        assert_eq!(app.selected(), Some(0));
        // Updates focus: ↓ scrolls, cursor stays.
        app.focus_updates();
        app.on_next();
        assert_eq!(app.selected(), Some(0));
        assert_eq!(app.updates_scroll(), 1);
        app.on_prev();
        app.on_prev(); // clamped at 0
        assert_eq!(app.updates_scroll(), 0);
        // Back to sources.
        app.focus_sources();
        app.on_next();
        assert_eq!(app.selected(), Some(1));
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
    fn dashboard_sources_table_carries_the_toggles() {
        let mut app = App::new(
            scan_with(vec![upd("linux", "6.9.1", "6.9.2", SourceId::pacman())]),
            Theme::none(),
            AppOptions::test(),
        );
        let text = render(&app, 96, 24);
        // pacman has updates → toggled on; the clean/unavailable sources
        // have nothing to toggle and show a dash.
        assert!(text.contains("[x] pacman"), "toggle missing:\n{text}");
        assert!(text.contains("-  flatpak-user"), "dash missing:\n{text}");
        assert!(text.contains("space toggle"), "footer missing:\n{text}");
        assert!(text.contains("enter update"), "{text}");
        assert!(text.contains("i packages"), "{text}");

        app.toggle_selected(); // pacman off
        let text = render(&app, 96, 24);
        assert!(
            text.contains("[ ] pacman"),
            "untoggled box missing:\n{text}"
        );
    }

    #[test]
    fn system_pane_shows_what_u_will_run() {
        let app = App::new(
            scan_with(vec![
                upd("linux", "6.9.1", "6.9.2", SourceId::pacman()),
                upd("firefox", "127.0", "127.0.1", SourceId::pacman()),
            ]),
            Theme::none(),
            AppOptions::test(),
        );
        let text = render(&app, 96, 24);
        assert!(
            text.contains("2 packages / 1 sources"),
            "plan line missing:\n{text}"
        );

        let clean = App::new(scan_with(Vec::new()), Theme::none(), AppOptions::test());
        let text = render(&clean, 96, 24);
        assert!(text.contains("nothing to run"), "{text}");
    }

    #[test]
    fn unknown_new_version_renders_as_a_question_mark() {
        let app = App::new(
            scan_with(vec![upd(
                "org.gnome.Calculator",
                "49.2",
                "",
                SourceId::pacman(),
            )]),
            Theme::none(),
            AppOptions::test(),
        );
        // The updates pane renders the transition with a dim ? for the
        // unknown target version.
        let text = render(&app, 96, 24);
        assert!(text.contains("49.2 -> ?"), "dangling arrow:\n{text}");
    }

    use crate::executor::{ExecutionReport, StepReport, StepStatus};
    use std::path::PathBuf;

    fn report() -> ExecutionReport {
        ExecutionReport {
            steps: vec![StepReport {
                source_id: SourceId::flatpak_user(),
                targets: 2,
                status: StepStatus::Succeeded,
            }],
            log_path: PathBuf::from("/tmp/paclens/2026-06-12.log"),
        }
    }

    #[test]
    fn finished_update_flashes_its_summary_on_the_dashboard() {
        let mut app = App::new(
            scan_with(vec![upd("linux", "1", "2", SourceId::pacman())]),
            Theme::none(),
            AppOptions::test(),
        );
        app.finish_update(&report());
        let text = render(&app, 96, 24);
        assert!(text.contains("paclens · dashboard"), "{text}");
        assert!(
            text.contains("update finished — 1 source succeeded"),
            "summary flash missing:\n{text}"
        );
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
        for col in ["NAME", "VERSION", "TYPE", "SIZE"] {
            assert!(text.contains(col), "column {col} missing:\n{text}");
        }
        // KIND is Flatpak-only (#55) — on pacman it was a column of em dashes.
        assert!(!text.contains("KIND"), "KIND on a pacman list:\n{text}");
        // The description lives in the pane, never in a column (#56).
        assert!(!text.contains("DESCRIPTION"), "description column:\n{text}");
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
        assert!(text.contains("[ ] pane"), "footer missing:\n{text}");
    }

    #[test]
    fn the_table_ends_where_its_columns_do_so_the_pane_gets_the_rest() {
        // Wide: the pane stops at its cap and the table keeps the rest, which
        // goes to NAME rather than sitting dead against the divider.
        let text = render(&pkg_app(), 200, 18);
        let header = text
            .lines()
            .find(|l| l.contains("NAME") && l.contains("SIZE"))
            .expect("header row");
        let divider = header.rfind('|').unwrap_or(0);
        assert!(
            divider >= 200 - PANE_MAX_WIDTH as usize - 6,
            "pane grew past its cap:\n{text}"
        );
        // Modest width: no room for the column, so no dead strip either — the
        // divider follows SIZE.
        let text = render(&pkg_app(), 110, 18);
        let header = text
            .lines()
            .find(|l| l.contains("NAME") && l.contains("SIZE"))
            .expect("header row");
        let size_end = header.find("SIZE").unwrap_or(0) + "SIZE".len();
        let gap = header[size_end..]
            .find('|')
            .expect("the pane divider follows the last column");
        assert!(gap <= 2, "dead strip between table and pane:\n{text}");
    }

    #[test]
    fn name_column_takes_what_it_needs_within_its_cap() {
        use crate::tui::app::PkgRow;
        let scan = pkg_scan();
        let rows: Vec<PkgRow<'_>> = scan.packages.iter().map(PkgRow::Pkg).collect();
        // Short names sit on the floor; the cap never pushes below it.
        assert_eq!(name_column_width(&rows, 40), 12);
        assert_eq!(name_column_width(&rows, 5), 12, "never below the floor");
        // A header sets the floor: "• pending updates (9)" is wider than any
        // of these names.
        let mut with_header = vec![PkgRow::Header("pending updates", 9)];
        with_header.extend(scan.packages.iter().map(PkgRow::Pkg));
        assert_eq!(name_column_width(&with_header, 40), 21);
    }

    #[test]
    fn no_dead_strip_between_table_and_pane_at_any_width() {
        // A long name is what pushes the columns past the point where a
        // DESCRIPTION column still fits — the width in between used to fall
        // between the table and the pane and be drawn by neither.
        let mut s = pkg_scan();
        s.packages[0].name = "heroic-games-launcher-bin-extra".to_string();
        for p in &mut s.packages {
            // Real descriptions, so trailing blanks cannot be mistaken for
            // layout: what is measured is where the columns end.
            p.description = Some(
                "a description long enough to fill whatever column it is given here".to_string(),
            );
        }
        let mut app = App::new(s, Theme::none(), AppOptions::test());
        app.open_packages();
        for w in (100..=200).step_by(5) {
            let text = render(&app, w, 12);
            let body = text
                .lines()
                .find(|l| l.contains("228.88 MiB"))
                .expect("a package row");
            let inner = body.get(1..body.len() - 1).expect("inside the border");
            let divider = inner.rfind('|').expect("the pane divider");
            let ends = inner[..divider].trim_end().chars().count();
            assert!(
                divider - ends <= 3,
                "{} dead columns at width {w}:\n{text}",
                divider - ends
            );
        }
    }

    #[test]
    fn a_pending_update_reads_accented_under_the_size_sort() {
        let mut s = pkg_scan();
        s.updates = vec![upd("glibc", "1", "2", SourceId::pacman())];
        let mut app = App::new(s, Theme::dark(), AppOptions::test());
        app.open_packages();
        assert_eq!(app.pkg_sort(), crate::tui::app::PkgSort::Size);
        // Off the pending row: the cursor's own highlight would mask it.
        app.pkg_move(1);
        assert_ne!(
            app.selected_package().map(|p| p.name.as_str()),
            Some("glibc")
        );

        // Read the styles back: the pending row's name carries the accent,
        // an up-to-date one does not.
        let backend = TestBackend::new(120, 16);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, &app)).expect("draw");
        let buf = terminal.backend().buffer().clone();
        let accent = Theme::dark().accent.fg;
        let rows: Vec<String> = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
                    .collect()
            })
            .collect();
        // The pane titles its report with the same name in the same accent,
        // so only look left of the divider.
        let divider = rows
            .iter()
            .find(|l| l.contains("NAME"))
            .and_then(|l| l.rfind('|'))
            .unwrap_or(usize::MAX);
        let name_fg = |needle: &str| {
            for (y, row) in rows.iter().enumerate() {
                match row.find(needle) {
                    Some(col) if col < divider => {
                        return buf.cell((col as u16, y as u16)).and_then(|c| c.style().fg);
                    }
                    _ => {}
                }
            }
            None
        };
        assert_eq!(name_fg("glibc"), accent, "pending row is not accented");
        assert_ne!(name_fg("bash"), accent, "an up-to-date row was accented");
    }

    #[test]
    fn a_cramped_table_keeps_the_version_not_the_reason() {
        let s = pkg_scan();

        let mut app = App::new(s, Theme::none(), AppOptions::test());
        app.open_packages();
        let text = render(&app, 56, 14);
        assert!(text.contains("VERSION"), "version dropped:\n{text}");
        assert!(
            !text.contains("TYPE"),
            "reason survived the squeeze:\n{text}"
        );
        assert!(!text.contains("SIZE"), "size survived the squeeze:\n{text}");
    }

    #[test]
    fn group_headers_are_set_like_headers_not_like_rows() {
        let mut s = pkg_scan();
        s.updates = vec![upd("glibc", "1", "2", SourceId::pacman())];
        let mut app = App::new(s, Theme::dark(), AppOptions::test());
        app.open_packages();

        let backend = TestBackend::new(120, 16);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, &app)).expect("draw");
        let buf = terminal.backend().buffer().clone();
        let style_of = |needle: &str| {
            for y in 0..buf.area.height {
                let row: String = (0..buf.area.width)
                    .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
                    .collect();
                if let Some(col) = row.find(needle) {
                    return buf.cell((col as u16, y)).map(|c| c.style());
                }
            }
            None
        };
        let theme = Theme::dark();
        let bold = |s: Option<Style>| s.is_some_and(|s| s.add_modifier.contains(Modifier::BOLD));
        assert!(
            bold(style_of("Pending Updates")),
            "pending header is not set apart"
        );
        assert_eq!(
            style_of("Pending Updates").and_then(|s| s.fg),
            theme.accent.fg,
            "the pending group is the accent's own meaning"
        );
        assert!(
            bold(style_of("Up To Date")),
            "up-to-date header is not set apart"
        );
    }

    #[test]
    fn the_columns_move_by_one_for_every_column_of_width() {
        // #57: NAME used to follow one rule in a narrow table and another in a
        // full one, so the surviving column landed somewhere else entirely.
        // Inside either mode the layout must now walk, not jump.
        let app = pkg_app();
        let version_at = |w: u16| -> Option<usize> {
            let text = render(&app, w, 10);
            text.lines().find(|l| l.contains("NAME"))?.find("VERSION")
        };
        for band in [50u16..57, 60..80] {
            let mut last: Option<usize> = None;
            for w in band {
                let Some(x) = version_at(w) else { continue };
                if let Some(prev) = last {
                    assert_eq!(
                        x,
                        prev + 1,
                        "VERSION jumped from {prev} to {x} between widths {} and {w}",
                        w - 1
                    );
                }
                last = Some(x);
            }
            assert!(last.is_some(), "VERSION never rendered");
        }
    }

    #[test]
    fn the_filter_line_says_which_field_matched() {
        let mut s = pkg_scan();
        s.packages[1].description = Some("the GNU C library and firefox's floor".to_string());
        let mut app = App::new(s, Theme::none(), AppOptions::test());
        app.open_packages();
        app.start_filter();
        for c in "firefox".chars() {
            app.filter_push(c);
        }
        let text = render(&app, 120, 14);
        assert!(text.contains("2 of 3"), "counts missing:\n{text}");
        assert!(text.contains("1 by name"), "fields missing:\n{text}");
        assert!(text.contains("1 by description"), "fields missing:\n{text}");
    }

    #[test]
    fn a_name_only_filter_does_not_explain_itself() {
        let mut app = pkg_app();
        app.start_filter();
        for c in "fire".chars() {
            app.filter_push(c);
        }
        let text = render(&app, 120, 14);
        assert!(text.contains("1 of 3"), "counts missing:\n{text}");
        assert!(
            !text.contains("by name"),
            "noise on the ordinary case:\n{text}"
        );
    }

    #[test]
    fn the_system_pane_reports_a_kernel_that_needs_a_reboot() {
        use crate::analyzer::kernel::RunningKernel;
        let mut s = scan_with(Vec::new());
        let mut kernel = pkg("linux-cachyos", SourceId::pacman());
        kernel.version = "7.2.3-1".to_string();
        s.packages = vec![kernel];
        s.kernel = Some(RunningKernel {
            release: "7.2.2-1-cachyos".to_string(),
            modules_present: false,
        });
        let app = App::new(s.clone(), Theme::none(), AppOptions::test());
        let text = render(&app, 100, 24);
        assert!(text.contains("reboot"), "no reboot row:\n{text}");
        assert!(
            text.contains("required · modules gone"),
            "the stronger case must reach the pane:\n{text}"
        );

        // Running what is installed: the row is not furniture.
        s.packages[0].version = "7.2.2-1".to_string();
        let app = App::new(s, Theme::none(), AppOptions::test());
        let text = render(&app, 100, 24);
        assert!(
            !text.contains("reboot"),
            "furniture on a healthy system:\n{text}"
        );
    }

    #[test]
    fn the_cleanup_screen_warns_before_it_suggests_a_session_ending_restart() {
        use crate::analyzer::services::{StaleProcess, UnitScope};
        let mut s = scan_with(Vec::new());
        s.stale_processes = vec![
            StaleProcess {
                pid: 1,
                comm: "pipewire".to_string(),
                unit: Some("pipewire.service".to_string()),
                scope: Some(UnitScope::User),
            },
            StaleProcess {
                pid: 2,
                comm: "Hyprland".to_string(),
                unit: Some("session-9.scope".to_string()),
                scope: Some(UnitScope::User),
            },
        ];
        let mut app = App::new(s, Theme::none(), AppOptions::test());
        app.open_cleanup();
        let text = render(&app, 110, 30);
        assert!(text.contains("stale services"), "row missing:\n{text}");
        assert!(text.contains("[inferred]"), "label missing:\n{text}");
        assert!(
            text.contains("systemctl --user restart pipewire.service"),
            "command missing:\n{text}"
        );
        assert!(
            text.contains("ends your session"),
            "the session-critical warning must be next to its command:\n{text}"
        );
    }

    #[test]
    fn the_dashboard_counts_services_wanting_a_restart() {
        use crate::analyzer::services::{StaleProcess, UnitScope};
        let mut s = scan_with(Vec::new());
        s.stale_processes = vec![StaleProcess {
            pid: 1,
            comm: "pipewire".to_string(),
            unit: Some("pipewire.service".to_string()),
            scope: Some(UnitScope::User),
        }];
        let app = App::new(s, Theme::none(), AppOptions::test());
        let text = render(&app, 100, 24);
        assert!(text.contains("want restarting"), "system pane row:\n{text}");

        // Nothing stale: no row, the way the reboot row behaves.
        let app = App::new(scan_with(Vec::new()), Theme::none(), AppOptions::test());
        let text = render(&app, 100, 24);
        assert!(!text.contains("want restarting"), "furniture:\n{text}");
    }

    #[test]
    fn the_cleanup_screen_lists_config_leftovers_as_copiable_text() {
        use crate::analyzer::pacfiles::{PacFile, PacFileKind};
        let mut s = scan_with(Vec::new());
        s.pacfiles = (0..5)
            .map(|i| PacFile {
                path: format!("/etc/conf{i}.conf.pacnew"),
                kind: PacFileKind::Pacnew,
                modified_secs: Some(100 - i),
            })
            .collect();
        let mut app = App::new(s, Theme::none(), AppOptions::test());
        app.open_cleanup();
        let text = render(&app, 110, 26);
        assert!(text.contains("config leftovers"), "row missing:\n{text}");
        assert!(text.contains("pacdiff"), "suggestion missing:\n{text}");
        // Named, not just counted — but capped, with the rest behind pacdiff.
        assert!(text.contains("/etc/conf0.conf"), "{text}");
        assert!(
            text.contains("2 more"),
            "the cap must say what it hid:\n{text}"
        );
        // The screen still has no action keys: this is text you run yourself.
        assert!(!text.contains("--noconfirm"), "{text}");
    }

    #[test]
    fn a_clean_system_still_gets_a_config_leftovers_row() {
        let mut app = App::new(scan_with(Vec::new()), Theme::none(), AppOptions::test());
        app.open_cleanup();
        let text = render(&app, 110, 26);
        assert!(
            text.contains("config leftovers"),
            "the row must say none rather than vanish:\n{text}"
        );
        assert!(!text.contains("pacdiff"), "nothing to suggest:\n{text}");
    }

    #[test]
    fn brackets_move_the_split() {
        fn divider(app: &App) -> usize {
            let text = render(app, 140, 18);
            // The outer panel border is a '|' too — look inside it.
            text.lines()
                .find(|l| l.contains("NAME"))
                .and_then(|l| l.get(1..l.len() - 1))
                .and_then(|l| l.rfind('|'))
                .unwrap_or(0)
        }
        let mut app = pkg_app();
        let start = divider(&app);
        app.resize_pane(6); // ] — a wider pane starts further left
        let wider = divider(&app);
        assert!(
            wider < start,
            "] did not widen the pane: {wider} vs {start}"
        );
        app.resize_pane(-12); // [ — past the start, the other way
        assert!(divider(&app) > wider, "[ did not narrow the pane");
    }

    #[test]
    fn pane_sits_beside_the_table_when_wide_and_under_it_when_narrow() {
        let app = pkg_app();
        // Wide: the pane shares rows with the table, so one line carries both.
        let wide = render(&app, 120, 18);
        assert!(
            wide.lines().any(|l| l.contains("firefox")
                && l.contains("228.88 MiB")
                && l.contains("no description")),
            "pane is not beside the table:\n{wide}"
        );
        // Narrow: it stacks, so no line carries both.
        let narrow = render(&app, 70, 22);
        assert!(
            !narrow
                .lines()
                .any(|l| l.contains("228.88 MiB") && l.contains("no description")),
            "pane is not under the table:\n{narrow}"
        );
        assert!(narrow.contains("no description"), "pane missing:\n{narrow}");
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
        // Flatpak IDs are long: below this the table drops to name + reason.
        let text = render(&app, 130, 18);
        assert!(text.contains("KIND"), "kind column missing:\n{text}");
        assert!(text.contains("runtime"), "kind missing:\n{text}");
        assert!(text.contains("app"), "kind missing:\n{text}");
        // The pane carries the selected row's description (#56).
        assert!(text.contains("Calculator"), "description missing:\n{text}");
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
        // cursor row 0 = firefox (size-sorted) → explicit, nothing requires it.
        let text = render(&app, 110, 20);
        assert!(text.contains("firefox  1"), "pane header missing:\n{text}");
        assert!(text.contains("likely safe"), "{text}");
        assert!(text.contains("[confirmed]"), "{text}");

        app.on_next(); // glibc — needed by bash + firefox
        let text = render(&app, 110, 20);
        assert!(text.contains("glibc  1"), "pane must follow:\n{text}");
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
        let text = render(&app, 100, 22);
        assert!(text.contains("glibc  1"), "pane header missing:\n{text}");
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
    fn why_pane_labels_flatpak_runtime_edges_inferred() {
        let mut s = scan_with(Vec::new());
        let mut app_pkg = pkg("org.x.App", SourceId::flatpak_user());
        app_pkg.depends_on = vec!["org.gnome.Platform".to_string()];
        let mut runtime = pkg("org.gnome.Platform", SourceId::flatpak_user());
        runtime.runtime = true;
        s.packages = vec![app_pkg, runtime];
        let mut app = App::new(s, Theme::none(), AppOptions::test());
        app.on_next(); // select the flatpak-user source
        app.open_packages();
        // name-sorted: org.gnome.Platform is row 0. Wide enough that the
        // pane does not wrap the label being asserted.
        let text = render(&app, 130, 22);
        assert!(
            text.contains("org.gnome.Platform  1"),
            "pane header missing:\n{text}"
        );
        assert!(text.contains("flatpak runtime"), "{text}");
        assert!(
            text.contains("org.x.App [inferred]"),
            "inferred edge label missing:\n{text}"
        );
        assert!(text.contains("is a dependency"), "{text}");
        assert!(text.contains("[inferred]"), "{text}");
    }

    #[test]
    fn why_pane_calls_a_flatpak_app_self_contained() {
        let mut s = scan_with(Vec::new());
        s.packages = vec![pkg("org.x.App", SourceId::flatpak_user())];
        let mut app = App::new(s, Theme::none(), AppOptions::test());
        app.on_next(); // select the flatpak-user source
        app.open_packages();
        let text = render(&app, 100, 22);
        assert!(
            text.contains("org.x.App  1"),
            "pane header missing:\n{text}"
        );
        assert!(text.contains("self-contained"), "{text}");
        assert!(text.contains("likely safe"), "{text}");
        assert!(text.contains("[confirmed]"), "{text}");
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

    // --- overlap screen ---
    fn overlap_scan() -> crate::model::ScanResult {
        let mut s = scan_with(Vec::new());
        let mut native = rich_pkg("firefox", InstallReason::Explicit, None, &[]);
        native.version = "128.0-1".to_string();
        let mut flat = pkg("org.mozilla.firefox", SourceId::flatpak_user());
        flat.description = Some("Firefox".to_string());
        flat.version = "128.0".to_string();
        s.packages = vec![native, flat];
        s
    }

    #[test]
    fn overlap_screen_lists_candidates_with_the_tradeoff_pane() {
        let mut s = overlap_scan();
        s.flatpak_profile_sizes
            .insert("org.mozilla.firefox".to_string(), 52_428_800);
        let mut app = App::new(s, Theme::none(), AppOptions::test());
        app.open_overlaps();
        let text = render(&app, 100, 26);
        assert!(text.contains("overlaps (1)"), "title missing:\n{text}");
        assert!(text.contains("Firefox"), "{text}");
        assert!(text.contains("128.0-1"), "native version missing:\n{text}");
        assert!(text.contains("confirmed"), "confidence missing:\n{text}");
        assert!(text.contains("known map"), "match method missing:\n{text}");
        assert!(text.contains("tradeoff"), "{text}");
        assert!(text.contains("sandboxing"), "{text}");
        assert!(text.contains("pacman (rolling)"), "{text}");
        assert!(
            text.contains("~/.var/app/org.mozilla.firefox (50.00 MiB)"),
            "profile path/size missing:\n{text}"
        );
        assert!(
            text.contains("native — explicitly installed"),
            "primary heuristic missing:\n{text}"
        );
        assert!(text.contains("advisory only"), "{text}");
        assert!(text.contains("esc back"), "{text}");
    }

    #[test]
    fn migrate_pane_replaces_the_tradeoff_on_enter() {
        let mut s = overlap_scan();
        s.profile_dir_sizes
            .insert("~/.mozilla".to_string(), 1_200_000_000);
        let mut app = App::new(s, Theme::none(), AppOptions::test());
        app.open_overlaps();
        app.toggle_why();
        let text = render(&app, 110, 26);
        // Explicit native + unknown flatpak → primary native → to-native.
        assert!(
            text.contains("migrate Firefox -> native [confirmed]"),
            "pane title missing:\n{text}"
        );
        assert!(text.contains("(not present)"), "{text}");
        assert!(
            text.contains("-> ~/.mozilla  (exists, 1.12 GiB)"),
            "target annotation missing:\n{text}"
        );
        assert!(text.contains("! close Firefox everywhere"), "{text}");
        assert!(text.contains("! backup both sides"), "{text}");
        assert!(
            !text.contains("sandboxing"),
            "tradeoff must be gone:\n{text}"
        );
        assert!(text.contains("d flip direction"), "footer keys:\n{text}");
        assert!(text.contains("enter close"), "{text}");
    }

    #[test]
    fn migrate_pane_flips_direction_on_d() {
        let mut s = overlap_scan();
        s.profile_dir_sizes
            .insert("~/.mozilla".to_string(), 1_200_000_000);
        let mut app = App::new(s, Theme::none(), AppOptions::test());
        app.open_overlaps();
        app.toggle_why();
        app.flip_migrate_direction();
        let text = render(&app, 110, 26);
        assert!(
            text.contains("migrate Firefox -> flatpak [confirmed]"),
            "{text}"
        );
        assert!(text.contains("~/.mozilla  1.12 GiB"), "{text}");
        assert!(
            text.contains("-> ~/.var/app/org.mozilla.firefox/.mozilla"),
            "{text}"
        );
    }

    #[test]
    fn migrate_footer_offers_run_then_remove() {
        let mut s = overlap_scan();
        s.profile_dir_sizes
            .insert("~/.mozilla".to_string(), 1_200_000_000);
        let mut app = App::new(s, Theme::none(), AppOptions::test());
        app.open_overlaps();
        app.toggle_why();
        let text = render(&app, 110, 26);
        assert!(text.contains("x run"), "{text}");
        assert!(text.contains("nothing deleted"), "{text}");
        assert!(!text.contains("R remove source"), "{text}");

        // A successful copy run arms R.
        app.stage_removal(Some(crate::tui::app::StagedRemoval {
            plan: crate::model::ActionPlan {
                created_at: chrono::Utc::now(),
                steps: Vec::new(),
                requires_sudo: true,
            },
            backup: "/b".to_string(),
        }));
        app.start_exec(10, 40, ExecKind::Migrate);
        app.exec_finish(crate::executor::ExecutionReport {
            steps: Vec::new(),
            log_path: std::path::PathBuf::from("/tmp/x.log"),
        });
        let report = app.take_exec_report().expect("report");
        app.finish_migration(&report);
        let text = render(&app, 110, 26);
        assert!(text.contains("R remove source"), "{text}");
        assert!(text.contains("esc keep it"), "{text}");
        assert!(text.contains("verify the target launched"), "{text}");
    }

    #[test]
    fn migrate_pane_without_data_says_nothing_to_migrate() {
        let mut app = App::new(overlap_scan(), Theme::none(), AppOptions::test());
        app.open_overlaps();
        app.toggle_why();
        let text = render(&app, 110, 26);
        assert!(text.contains("nothing to migrate"), "{text}");
    }

    #[test]
    fn overlap_screen_without_profile_says_none_found() {
        let mut app = App::new(overlap_scan(), Theme::none(), AppOptions::test());
        app.open_overlaps();
        let text = render(&app, 100, 26);
        assert!(text.contains("none found"), "{text}");
    }

    #[test]
    fn empty_overlap_screen_shows_the_way_back() {
        let mut app = App::new(scan_with(Vec::new()), Theme::none(), AppOptions::test());
        app.open_overlaps();
        let text = render(&app, 90, 20);
        assert!(text.contains("No overlaps detected"), "{text}");
        assert!(text.contains("esc back"), "{text}");
    }

    #[test]
    fn dashboard_hints_the_overlap_screen() {
        let app = App::new(scan_with(Vec::new()), Theme::none(), AppOptions::test());
        let text = render(&app, 96, 24);
        assert!(text.contains("o overlaps"), "hint missing:\n{text}");
    }

    /// The dashboard says the same thing `paclens status` does (P5): which
    /// capability the missing helper costs, and what restores it.
    #[test]
    fn dashboard_names_a_missing_helper_and_the_fix() {
        use crate::providers::aur::HelperChoice;
        let mut s = scan_with(Vec::new());
        s.aur_helper = HelperChoice::None;
        s.sources.push(Source {
            id: SourceId::aur(),
            kind: SourceKind::Aur,
            available: false,
            last_scanned: None,
            accurate_updates: true,
        });
        let mut app = App::new(s, Theme::none(), AppOptions::test());
        // The status column says it regardless of the cursor.
        let text = render(&app, 110, 30);
        assert!(text.contains("no helper"), "status column:\n{text}");
        assert!(
            !text.contains("install paru/yay/pikaur"),
            "note must wait for the aur row to be selected:\n{text}"
        );

        select_aur(&mut app);
        let text = render(&app, 110, 30);
        assert!(
            text.contains("install paru/yay/pikaur"),
            "note missing once aur is selected:\n{text}"
        );
    }

    /// Walk the sources cursor to the aur row.
    fn select_aur(app: &mut App) {
        for _ in 0..app.rows().len() {
            if app.dash_source().map(|s| s.id.clone()) == Some(SourceId::aur()) {
                return;
            }
            app.on_next();
        }
        panic!("no aur row to select");
    }

    /// A stale pin is surfaced on the dashboard too, even though the source is
    /// working — silently using a helper other than the configured one is the
    /// unexplained behaviour design §2 rules out.
    #[test]
    fn dashboard_reports_a_stale_pin() {
        use crate::providers::aur::{AurHelper, HelperChoice};
        let mut s = scan_with(Vec::new());
        s.aur_helper = HelperChoice::FellBack {
            configured: "yay".to_string(),
            to: AurHelper::Paru,
        };
        s.sources.push(Source {
            id: SourceId::aur(),
            kind: SourceKind::Aur,
            available: true,
            last_scanned: None,
            accurate_updates: true,
        });
        let mut app = App::new(s, Theme::none(), AppOptions::test());
        assert!(
            !render(&app, 110, 30).contains("no yay, using paru"),
            "a stale pin must not interrupt the pacman row"
        );

        select_aur(&mut app);
        let text = render(&app, 110, 30);
        assert!(text.contains("no yay, using paru"), "{text}");
    }

    /// The row is marked whatever the cursor is doing. The note is the
    /// explanation and stays behind the cursor; the marker is the reason you
    /// would move the cursor there at all.
    #[test]
    fn a_degraded_aur_row_is_marked_without_selecting_it() {
        use crate::providers::aur::{AurHelper, HelperChoice};
        let mut s = scan_with(Vec::new());
        s.aur_helper = HelperChoice::FellBack {
            configured: "yay".to_string(),
            to: AurHelper::Paru,
        };
        s.sources.push(Source {
            id: SourceId::aur(),
            kind: SourceKind::Aur,
            available: true,
            last_scanned: None,
            accurate_updates: true,
        });
        let app = App::new(s, Theme::none(), AppOptions::test());
        let text = render(&app, 110, 30);
        assert!(source_row(&text, "aur").contains("! ok"));
        // Not selected, so no explanation yet.
        assert!(!text.contains("no yay, using paru"), "{text}");
        // And a healthy source is untouched.
        assert!(source_row(&text, "pacman").contains("* ok"));
    }

    /// The dashboard table row for `name`.
    ///
    /// Anchored past the SOURCE header on purpose: the updates pane is titled
    /// " pending updates · pacman ", which a naive line search happily
    /// mistakes for the pacman row.
    fn source_row<'a>(text: &'a str, name: &str) -> &'a str {
        text.lines()
            .skip_while(|l| !l.contains("SOURCE"))
            .find(|l| l.contains(&format!(" {name} ")))
            .unwrap_or_else(|| panic!("no {name} row in:\n{text}"))
    }

    /// Selecting a healthy source must not carry the aur note along with it —
    /// the note belongs to the row it describes.
    #[test]
    fn the_helper_note_follows_the_cursor_off_the_aur_row() {
        use crate::providers::aur::HelperChoice;
        let mut s = scan_with(Vec::new());
        s.aur_helper = HelperChoice::None;
        s.sources.push(Source {
            id: SourceId::aur(),
            kind: SourceKind::Aur,
            available: false,
            last_scanned: None,
            accurate_updates: true,
        });
        let mut app = App::new(s, Theme::none(), AppOptions::test());
        select_aur(&mut app);
        assert!(render(&app, 110, 30).contains("install paru/yay/pikaur"));

        app.on_prev();
        assert!(
            !render(&app, 110, 30).contains("install paru/yay/pikaur"),
            "note should be gone once another source is selected"
        );
    }

    /// On a healthy system the note is absent — it must not eat a row of the
    /// sources pane permanently.
    #[test]
    fn dashboard_prints_no_note_when_the_helper_is_fine() {
        let app = App::new(scan_with(Vec::new()), Theme::none(), AppOptions::test());
        let text = render(&app, 110, 30);
        assert!(!text.contains("aur:"), "unexpected note:\n{text}");
        assert!(!text.contains("no helper"), "{text}");
    }

    // --- cleanup screen ---

    /// The build-cache row and its suggestion name the helper actually in use.
    /// A yay user reading "paru build cache" would be looking at a figure
    /// attributed to a tool they do not have, and copying a command that would
    /// not run (design §3).
    #[test]
    fn cache_pane_names_the_helper_in_use_not_paru() {
        let mut s = scan_with(Vec::new());
        s.aur_helper =
            crate::providers::aur::HelperChoice::Detected(crate::providers::aur::AurHelper::Yay);
        s.cache_sizes.aur_cache_bytes = Some(9_000_000_000);
        let mut app = App::new(s, Theme::none(), AppOptions::test());
        app.open_cleanup();
        let text = render(&app, 110, 24);
        assert!(text.contains("yay build cache"), "{text}");
        assert!(text.contains("yay -Sc --aur"), "{text}");
        assert!(
            !text.contains("paru"),
            "paru must not appear at all:\n{text}"
        );
    }

    /// No helper means no build-cache row and no clean suggestion — nothing to
    /// attribute, so nothing is claimed.
    #[test]
    fn cache_pane_omits_the_build_cache_without_a_helper() {
        let mut s = scan_with(Vec::new());
        s.aur_helper = crate::providers::aur::HelperChoice::None;
        s.cache_sizes.aur_cache_bytes = Some(9_000_000_000);
        let mut app = App::new(s, Theme::none(), AppOptions::test());
        app.open_cleanup();
        let text = render(&app, 110, 24);
        assert!(!text.contains("build cache"), "{text}");
        assert!(!text.contains("-Sc"), "{text}");
    }

    #[test]
    fn cache_pane_shows_honest_reclaimable_and_paru_cache() {
        let mut s = scan_with(Vec::new());
        s.cache_sizes.pacman_cache_bytes = Some(11_000_000_000);
        s.cache_sizes.pacman_cache_reclaimable_bytes = Some(0);
        s.cache_sizes.aur_cache_bytes = Some(9_000_000_000);
        let mut app = App::new(s, Theme::none(), AppOptions::test());
        app.open_cleanup();
        let text = render(&app, 110, 24);
        assert!(
            text.contains("(nothing to reclaim)"),
            "honesty note missing:\n{text}"
        );
        assert!(
            !text.contains("paccache -rk3"),
            "pointless suggestion must be hidden:\n{text}"
        );
        assert!(text.contains("paru build cache"), "{text}");
        assert!(text.contains("8.38 GiB"), "paru size missing:\n{text}");
        assert!(text.contains("paru -Sc --aur"), "{text}");

        // A real reclaimable number shows inline and keeps the suggestion.
        let mut s = scan_with(Vec::new());
        s.cache_sizes.pacman_cache_bytes = Some(11_000_000_000);
        s.cache_sizes.pacman_cache_reclaimable_bytes = Some(846_000_000);
        let mut app = App::new(s, Theme::none(), AppOptions::test());
        app.open_cleanup();
        let text = render(&app, 110, 24);
        assert!(
            text.contains("(806.81 MiB reclaimable)"),
            "reclaimable missing:\n{text}"
        );
        assert!(text.contains("paccache -rk3"), "{text}");
        assert!(!text.contains("paru build cache"), "no paru row:\n{text}");
        assert!(!text.contains("paru -Sc --aur"), "{text}");
    }

    #[test]
    fn cleanup_screen_shows_orphans_cache_and_suggestions() {
        let mut s = scan_with(Vec::new());
        s.cache_sizes.pacman_cache_bytes = Some(7_600_000_000);
        let mut orphan = rich_pkg("leafdep", InstallReason::Dependency, Some(2_000_000), &[]);
        orphan.version = "1.0-1".to_string();
        let mut rt = pkg("org.kde.Platform", SourceId::flatpak_user());
        rt.runtime = true;
        rt.size_bytes = Some(457_000_000);
        s.packages = vec![orphan, rt];
        let mut app = App::new(s, Theme::none(), AppOptions::test());
        app.open_cleanup();
        let text = render(&app, 100, 24);
        assert!(text.contains("cleanup (1 orphans)"), "title:\n{text}");
        assert!(text.contains("leafdep"), "{text}");
        assert!(text.contains("1.91 MiB"), "orphan size missing:\n{text}");
        assert!(text.contains("pacman cache"), "{text}");
        assert!(text.contains("7.08 GiB"), "cache size missing:\n{text}");
        assert!(text.contains("unused runtimes"), "{text}");
        assert!(text.contains("435.83 MiB"), "runtime size missing:\n{text}");
        assert!(text.contains("paccache -rk3"), "{text}");
        assert!(text.contains("flatpak uninstall --unused"), "{text}");
        assert!(
            text.contains("sudo pacman -Rns leafdep"),
            "orphan command missing:\n{text}"
        );
        assert!(text.contains("advisory only"), "{text}");

        // Enter swaps the right pane to the orphan's why report.
        app.toggle_why();
        let text = render(&app, 100, 24);
        assert!(text.contains("why · leafdep"), "why pane missing:\n{text}");
        assert!(text.contains("likely safe"), "{text}");
        assert!(
            !text.contains("paccache"),
            "cache pane still visible:\n{text}"
        );
    }

    #[test]
    fn empty_cleanup_screen_celebrates() {
        let mut app = App::new(scan_with(Vec::new()), Theme::none(), AppOptions::test());
        app.open_cleanup();
        let text = render(&app, 90, 20);
        assert!(text.contains("no orphan candidates"), "{text}");
        assert!(text.contains("esc back"), "{text}");
    }

    #[test]
    fn dashboard_hints_the_cleanup_screen() {
        let app = App::new(scan_with(Vec::new()), Theme::none(), AppOptions::test());
        let text = render(&app, 96, 24);
        assert!(text.contains("c cleanup"), "hint missing:\n{text}");
    }

    // --- inline exec console + log viewer ---
    #[test]
    fn exec_console_renders_the_vt100_screen_and_swaps_footer_when_done() {
        let mut app = App::new(scan_with(Vec::new()), Theme::none(), AppOptions::test());
        let (rows, cols) = exec_pty_size(90, 20);
        app.start_exec(rows, cols, ExecKind::Update);
        app.exec_feed(b"\x1b[1m:: sudo pacman -Syu\x1b[0m\r\n");
        for i in 0..40 {
            app.exec_feed(format!("downloading package {i}\r\n").as_bytes());
        }
        let text = render(&app, 90, 20);
        assert!(text.contains("updating"), "title missing:\n{text}");
        // The vt100 screen keeps only what a terminal would: the tail.
        assert!(
            text.contains("downloading package 39"),
            "not tailing:\n{text}"
        );
        assert!(
            !text.contains("downloading package 0\n"),
            "old lines should scroll away"
        );
        assert!(
            text.contains("pass through"),
            "running footer missing:\n{text}"
        );

        app.exec_finish(crate::executor::ExecutionReport {
            steps: Vec::new(),
            log_path: PathBuf::from("/tmp/x.log"),
        });
        let text = render(&app, 90, 20);
        assert!(text.contains("update finished"), "{text}");
        assert!(text.contains("back to the dashboard"), "{text}");
    }

    #[test]
    fn exec_console_honors_carriage_return_progress_lines() {
        // pacman redraws progress with bare \r — a real terminal overwrites
        // the line, and so must the console.
        let mut app = App::new(scan_with(Vec::new()), Theme::none(), AppOptions::test());
        app.start_exec(10, 40, ExecKind::Update);
        app.exec_feed(b"progress:  10%\rprogress: 100%\r\n");
        let text = render(&app, 60, 14);
        assert!(text.contains("progress: 100%"), "{text}");
        assert!(
            !text.contains("10%"),
            "stale progress not overwritten:\n{text}"
        );
    }

    #[test]
    fn exec_pty_size_mirrors_the_console_layout() {
        assert_eq!(exec_pty_size(90, 20), (17, 86));
        // Degenerate terminals clamp instead of underflowing.
        assert_eq!(exec_pty_size(4, 2), (2, 20));
    }

    #[test]
    fn log_viewer_scrolls_and_shows_position() {
        let mut app = App::new(scan_with(Vec::new()), Theme::none(), AppOptions::test());
        let text: String = (0..60)
            .map(|i| format!("[2026-07-06 INFO] line {i}\n"))
            .collect();
        app.open_log(text);
        let rendered = render(&app, 90, 20);
        assert!(rendered.contains("update log"), "{rendered}");
        assert!(rendered.contains("line 0"), "{rendered}");
        assert!(rendered.contains("more"), "position missing:\n{rendered}");

        app.log_scroll(30);
        let rendered = render(&app, 90, 20);
        assert!(!rendered.contains("line 0\n"), "did not scroll");
        assert!(rendered.contains("line 30"), "{rendered}");
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
