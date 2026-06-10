//! Immediate-mode rendering. Every function here takes `&App` and never mutates
//! it (dev-notes §5): each frame rebuilds the whole UI from current state.
//!
//! Layout: one bordered panel titled "paclens · dashboard" containing a summary
//! line (headline update count + last-scan time), the per-source table, and a
//! footer of keybindings. Below a minimum size the panel is replaced by a terse
//! notice so the frame never renders broken.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Padding, Paragraph, Row, Table, TableState};

use crate::format::relative_time;
use crate::tui::app::App;
use crate::tui::theme::Theme;

const MIN_WIDTH: u16 = 40;
const MIN_HEIGHT: u16 = 10;

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        render_too_small(frame, area, &app.theme);
        return;
    }

    let theme = &app.theme;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(theme.border_set)
        .border_style(theme.border)
        .title(Span::styled(" paclens · dashboard ", theme.title))
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::vertical([
        Constraint::Length(1), // summary
        Constraint::Length(1), // spacer
        Constraint::Min(3),    // table (header + rows)
        Constraint::Length(1), // footer
    ])
    .split(inner);

    render_summary(frame, chunks[0], app);
    render_table(frame, chunks[2], app);
    render_footer(frame, chunks[3], app);
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
        Constraint::Min(14),    // SOURCE (flex)
        Constraint::Length(9),  // INSTALLED
        Constraint::Length(7),  // UPDATES
        Constraint::Length(16), // STATUS
    ];

    let table = Table::new(body, widths)
        .header(header)
        .column_spacing(2)
        .row_highlight_style(theme.selected)
        .highlight_symbol(theme.glyphs.pointer);

    // Build an ephemeral TableState from the App's selection so the render path
    // stays immutable (the App is borrowed `&`, never mutated here).
    let mut state = TableState::default();
    state.select(app.selected());
    frame.render_stateful_widget(table, area, &mut state);
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let g = theme.glyphs;
    let text = format!(
        "q quit {b} {up}/{down} navigate {b} r refresh",
        b = g.bullet,
        up = g.up,
        down = g.down,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(text, theme.dim))),
        area,
    );
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

    fn upd(name: &str, source: SourceId) -> PendingUpdate {
        PendingUpdate {
            package_name: name.to_string(),
            current_version: "1".to_string(),
            available_version: "2".to_string(),
            source_id: source,
        }
    }

    fn sample_scan() -> ScanResult {
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
            packages: vec![pkg("a", SourceId::pacman()), pkg("b", SourceId::pacman())],
            updates: vec![upd("a", SourceId::pacman())],
            cache_sizes: CacheSizes::default(),
        }
    }

    /// Flatten a rendered buffer into newline-separated rows so token presence is
    /// easy to assert (text within a row stays contiguous).
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
    fn renders_title_header_and_every_source_row() {
        let app = App::new(sample_scan(), Theme::none());
        let text = render(&app, 70, 14);
        assert!(text.contains("paclens"), "title missing:\n{text}");
        assert!(text.contains("SOURCE"));
        assert!(text.contains("INSTALLED"));
        assert!(text.contains("UPDATES"));
        assert!(text.contains("STATUS"));
        assert!(text.contains("pacman"));
        assert!(text.contains("flatpak-system"));
    }

    #[test]
    fn renders_headline_update_count_and_footer() {
        let app = App::new(sample_scan(), Theme::none());
        let text = render(&app, 70, 14);
        assert!(
            text.contains("update available"),
            "headline missing:\n{text}"
        );
        assert!(text.contains("quit"));
        assert!(text.contains("refresh"));
    }

    #[test]
    fn unavailable_source_renders_not_available() {
        let app = App::new(sample_scan(), Theme::none());
        let text = render(&app, 70, 14);
        assert!(text.contains("not available"), "status missing:\n{text}");
    }

    #[test]
    fn empty_scan_shows_a_friendly_message_without_panicking() {
        let scan = ScanResult {
            schema_version: SCHEMA_VERSION,
            scanned_at: Utc::now(),
            sources: Vec::new(),
            packages: Vec::new(),
            updates: Vec::new(),
            cache_sizes: CacheSizes::default(),
        };
        let app = App::new(scan, Theme::none());
        let text = render(&app, 70, 14);
        assert!(text.contains("No package sources detected"), "{text}");
        assert!(text.contains("up to date"));
    }

    #[test]
    fn tiny_terminal_shows_the_too_small_notice() {
        let app = App::new(sample_scan(), Theme::none());
        let text = render(&app, 20, 6);
        assert!(text.contains("too small"), "{text}");
    }
}
