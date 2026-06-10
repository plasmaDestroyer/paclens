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
use ratatui::widgets::{Block, Borders, Cell, Padding, Paragraph, Row, Table, TableState};

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
        Screen::Updates => draw_update(frame, area, app),
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
            let status = if r.available {
                Span::styled(
                    format!("{} available", theme.glyphs.available),
                    theme.success,
                )
            } else {
                Span::styled(
                    format!("{} not available", theme.glyphs.unavailable),
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
        frame.render_widget(
            Paragraph::new("Nothing to update — you're up to date")
                .style(theme.dim)
                .alignment(Alignment::Center),
            inner,
        );
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

    let g = theme.glyphs;
    let footer = format!(
        "space toggle {b} {up}/{down} source {b} q quit",
        b = g.bullet,
        up = g.up,
        down = g.down,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(footer, theme.dim))),
        chunks[4],
    );
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
    fn update_screen_empty_state_when_no_updates() {
        let mut app = App::new(scan_with(Vec::new()), Theme::none());
        app.goto_updates();
        let text = render(&app, 72, 16);
        assert!(text.contains("Nothing to update"), "{text}");
    }

    #[test]
    fn confirm_shows_flash_after_enter() {
        let mut app = App::new(
            scan_with(vec![upd("linux", "1", "2", SourceId::pacman())]),
            Theme::none(),
        );
        app.goto_updates();
        app.set_flash("execution arrives in v0.0.6");
        let text = render(&app, 72, 16);
        assert!(text.contains("execution arrives in v0.0.6"), "{text}");
    }
}
