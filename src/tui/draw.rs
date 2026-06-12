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
    match app.screen() {
        Screen::Dashboard => draw_dashboard(frame, area, app),
        Screen::Updates => match app.report() {
            Some(report) => draw_result(frame, area, app, report),
            None => draw_update(frame, area, app),
        },
    }
}

// ---------------------------------------------------------------------------
// Dashboard
// ---------------------------------------------------------------------------

fn draw_dashboard(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let block = panel(theme, " paclens · dashboard ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::vertical([
        Constraint::Length(1), // summary
        Constraint::Length(1), // spacer
        Constraint::Min(3),    // table
        Constraint::Length(1), // footer
    ])
    .split(inner);

    render_summary(frame, chunks[0], app);
    render_table(frame, chunks[2], app);

    let g = theme.glyphs;
    let footer = format!(
        "q quit {b} {up}/{down} navigate {b} u update {b} r refresh",
        b = g.bullet,
        up = g.up,
        down = g.down,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(footer, theme.dim))),
        chunks[3],
    );
}

fn render_summary(frame: &mut Frame, area: Rect, app: &App) {
    let cols = Layout::horizontal([Constraint::Min(0), Constraint::Length(24)]).split(area);
    frame.render_widget(
        Paragraph::new(summary_line(app)).alignment(Alignment::Left),
        cols[0],
    );
    frame.render_widget(
        Paragraph::new(scanned_line(app)).alignment(Alignment::Right),
        cols[1],
    );
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
    let theme = &app.theme;
    if app.is_scanning() {
        return Line::from(Span::styled("scanning sources…", theme.accent));
    }
    Line::from(Span::styled(
        format!("scanned {}", relative_time(app.scan().scanned_at)),
        theme.dim,
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
        Cell::from(Line::from("INSTALLED").alignment(Alignment::Right)),
        Cell::from(Line::from("UPDATES").alignment(Alignment::Right)),
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
        Constraint::Min(14),
        Constraint::Length(9),
        Constraint::Length(7),
        Constraint::Length(16),
    ];

    let table = Table::new(body, widths)
        .header(header)
        .column_spacing(2)
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
            "space toggle {b} {up}/{down} source {b} esc back {b} q quit",
            b = g.bullet,
            up = g.up,
            down = g.down,
        )
    } else {
        format!("esc back {b} q quit", b = g.bullet)
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
            Line::from(vec![
                Span::styled(format!("{:name_w$}", u.package_name), theme.primary),
                Span::raw("  "),
                Span::styled(u.current_version.clone(), theme.dim),
                Span::styled(format!(" {} ", theme.glyphs.arrow), theme.dim),
                Span::styled(u.available_version.clone(), theme.accent),
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
// Confirm modal
// ---------------------------------------------------------------------------

/// The centered confirmation overlay (chosen with the user): the question, the
/// exact command(s) that will run (P1), what will be skipped and why, and the
/// y/n key hints. Rendered on top of the update screen.
fn render_confirm_modal(frame: &mut Frame, area: Rect, app: &App, plan: &ActionPlan) {
    let theme = &app.theme;
    let apps = executor::executable_targets(plan);

    let mut body: Vec<Line> = vec![Line::from(Span::styled(
        format!(
            "Update {apps} Flatpak app{}?",
            if apps == 1 { "" } else { "s" }
        ),
        theme.accent,
    ))];
    body.push(Line::default());
    for step in &plan.steps {
        if executor::skip_reason(step).is_none() {
            body.push(Line::from(Span::styled(
                step.command.join(" "),
                theme.primary,
            )));
        }
    }
    let skipped: Vec<Line> = plan
        .steps
        .iter()
        .filter_map(|step| {
            executor::skip_reason(step).map(|reason| {
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
            "press any key to continue",
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
        );
        let text = render(&app, 70, 14);
        assert!(text.contains("paclens"));
        assert!(text.contains("SOURCE"));
        assert!(text.contains("u update"), "footer hint missing:\n{text}");
    }

    #[test]
    fn dashboard_shows_a_scanning_indicator_instead_of_the_scan_age() {
        let mut app = App::new(
            scan_with(vec![upd("linux", "1", "2", SourceId::pacman())]),
            Theme::none(),
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
    fn toggling_off_empties_the_confirm() {
        let mut app = App::new(
            scan_with(vec![upd("linux", "1", "2", SourceId::pacman())]),
            Theme::none(),
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
        let mut app = App::new(scan_with(Vec::new()), Theme::none());
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
        );
        app.goto_updates();
        app.set_flash("nothing selected to update");
        let text = render(&app, 72, 16);
        assert!(text.contains("nothing selected to update"), "{text}");
    }

    // --- confirm modal ---
    #[test]
    fn confirm_modal_shows_question_command_skip_note_and_keys() {
        let mut app = App::new(
            scan_with(vec![
                upd("linux", "1", "2", SourceId::pacman()),
                upd("org.gimp.GIMP", "2.10", "2.12", SourceId::flatpak_user()),
            ]),
            Theme::none(),
        );
        app.goto_updates();
        app.open_confirm();
        let text = render(&app, 76, 20);
        assert!(text.contains("confirm"), "modal title missing:\n{text}");
        assert!(
            text.contains("Update 1 Flatpak app?"),
            "question missing:\n{text}"
        );
        assert!(
            text.contains("flatpak update --user --noninteractive"),
            "exact command missing:\n{text}"
        );
        assert!(
            text.contains("pacman will be skipped"),
            "skip note missing:\n{text}"
        );
        assert!(text.contains("[y] update"), "y hint missing:\n{text}");
        assert!(text.contains("[n] cancel"), "n hint missing:\n{text}");
    }

    #[test]
    fn confirm_modal_pluralizes_and_omits_skip_note_when_nothing_skipped() {
        let mut app = App::new(
            scan_with(vec![
                upd("org.gimp.GIMP", "2.10", "2.12", SourceId::flatpak_user()),
                upd("org.x.Editor", "1.0", "1.1", SourceId::flatpak_user()),
            ]),
            Theme::none(),
        );
        app.goto_updates();
        app.open_confirm();
        let text = render(&app, 76, 20);
        assert!(text.contains("Update 2 Flatpak apps?"), "{text}");
        assert!(!text.contains("will be skipped"), "{text}");
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
        let mut app = App::new(scan_with(Vec::new()), Theme::none());
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
        assert!(text.contains("2 apps updated"), "{text}");
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
        assert!(text.contains("press any key to continue"), "{text}");
    }

    #[test]
    fn all_green_result_has_no_failed_segment() {
        let mut app = App::new(scan_with(Vec::new()), Theme::none());
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
        assert!(text.contains("1 app updated"), "{text}");
    }
}
