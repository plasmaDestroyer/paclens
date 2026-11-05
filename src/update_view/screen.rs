//! Settles terminal output into finished lines, the way a terminal shows them.

/// Feeds pty bytes through a vt100 screen and hands back each line once a
/// `\n` finishes it, as the terminal would show it after every `\r` redraw.
/// Parser height. Independent of the pty size: pacman moves the cursor only
/// relatively, so a tall screen just keeps long wrapped lines whole.
const ROWS: u16 = 1000;

pub struct Settler {
    parser: vt100::Parser,
    cols: u16,
}

impl Settler {
    pub fn new(cols: u16) -> Self {
        Self {
            // ponytail: a line longer than ROWS x cols loses its start;
            // reading vt100 scrollback is the upgrade path.
            parser: vt100::Parser::new(ROWS, cols, 0),
            cols,
        }
    }

    /// Follow the pty's new width, so cursor-relative redraws land where
    /// pacman meant them.
    pub fn resize(&mut self, cols: u16) {
        self.parser.screen_mut().set_size(ROWS, cols);
        self.cols = cols;
    }

    /// Lines completed by a `\n` in `bytes`, trailing whitespace trimmed.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        self.feed_styled(bytes)
            .into_iter()
            .map(|(l, _)| l)
            .collect()
    }

    /// As [`Settler::feed`], each line with its colours too: `(plain, styled)`.
    /// The plain text is what gets classified; the styled one is what is shown.
    pub fn feed_styled(&mut self, bytes: &[u8]) -> Vec<(String, String)> {
        let mut lines = Vec::new();
        let mut segments = bytes.split(|&b| b == b'\n').peekable();
        while let Some(segment) = segments.next() {
            self.parser.process(segment);
            if segments.peek().is_some() {
                lines.push((self.cursor_line(), self.cursor_line_styled()));
                self.parser.process(b"\n");
            }
        }
        lines
    }

    /// The unfinished line, with its colours.
    pub fn partial_styled(&self) -> String {
        self.cursor_line_styled()
    }

    /// [`Self::cursor_line`] with SGR codes for the cells' colours and
    /// weight, ending in a reset; trailing blanks dropped.
    fn cursor_line_styled(&self) -> String {
        let screen = self.parser.screen();
        let (row, _) = screen.cursor_position();
        let mut top = row;
        while top > 0 && screen.row_wrapped(top - 1) {
            top -= 1;
        }
        let mut out = String::new();
        let mut keep = 0; // `out` up to the last visible character
        let mut attrs = Sgr::default();
        for r in top..=row {
            for c in 0..self.cols {
                let Some(cell) = screen.cell(r, c) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let now = Sgr::of(cell);
                if now != attrs {
                    now.write(&mut out);
                    attrs = now;
                }
                let text = cell.contents();
                if text.is_empty() || text == " " {
                    out.push(' ');
                } else {
                    out.push_str(text);
                    keep = out.len();
                }
            }
        }
        out.truncate(keep);
        if attrs != Sgr::default() || out.contains('\u{1b}') {
            out.push_str("\u{1b}[0m");
        }
        out
    }

    /// The unfinished line under the cursor, trimmed, wrapped rows joined.
    pub fn partial(&self) -> String {
        self.cursor_line()
    }

    /// The last `n` non-empty visible rows, trimmed. Rows below the cursor
    /// count too: a cursor move up does not erase them.
    pub fn tail(&self, n: usize) -> Vec<String> {
        let rows: Vec<String> = self.parser.screen().rows(0, self.cols).collect();
        let mut tail: Vec<String> = rows
            .iter()
            .rev()
            .map(|r| r.trim_end())
            .filter(|r| !r.is_empty())
            .take(n)
            .map(str::to_string)
            .collect();
        tail.reverse();
        tail
    }

    /// The cursor row joined with every row that soft-wrapped into it.
    fn cursor_line(&self) -> String {
        let screen = self.parser.screen();
        let (row, _) = screen.cursor_position();
        let mut top = row;
        while top > 0 && screen.row_wrapped(top - 1) {
            top -= 1;
        }
        screen
            .contents_between(top, 0, row, self.cols)
            .trim_end()
            .to_string()
    }
}

/// The look of one cell, as far as the view repeats it.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
struct Sgr {
    fg: vt100::Color,
    bg: vt100::Color,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    inverse: bool,
}

impl Sgr {
    fn of(c: &vt100::Cell) -> Self {
        Self {
            fg: c.fgcolor(),
            bg: c.bgcolor(),
            bold: c.bold(),
            dim: c.dim(),
            italic: c.italic(),
            underline: c.underline(),
            inverse: c.inverse(),
        }
    }

    /// A full reset, then this look: simple and never wrong.
    fn write(&self, out: &mut String) {
        let mut codes = vec!["0".to_string()];
        for (on, code) in [
            (self.bold, "1"),
            (self.dim, "2"),
            (self.italic, "3"),
            (self.underline, "4"),
            (self.inverse, "7"),
        ] {
            if on {
                codes.push(code.into());
            }
        }
        for (color, base) in [(self.fg, 30), (self.bg, 40)] {
            match color {
                vt100::Color::Default => {}
                vt100::Color::Idx(i) if i < 8 => codes.push((base + u16::from(i)).to_string()),
                vt100::Color::Idx(i) if i < 16 => {
                    codes.push((base + 52 + u16::from(i)).to_string())
                }
                vt100::Color::Idx(i) => codes.push(format!("{};5;{i}", base + 8)),
                vt100::Color::Rgb(r, g, b) => codes.push(format!("{};2;{r};{g};{b}", base + 8)),
            }
        }
        out.push_str(&format!("\u{1b}[{}m", codes.join(";")));
    }
}

/// The whole fixture recording settled at its recorded width (178 cols):
/// header line skipped, untimed `Script done` footer left out.
#[cfg(test)]
pub(crate) fn fixture_lines() -> Vec<String> {
    let out: &[u8] = include_bytes!("../../tests/fixtures/pacman-run/2026-10-16.out");
    let timing = include_str!("../../tests/fixtures/pacman-run/2026-10-16.timing");
    let len: usize = timing
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1)?.parse::<usize>().ok())
        .sum();
    let start = out.iter().position(|&b| b == b'\n').map_or(0, |i| i + 1);
    Settler::new(178).feed(&out[start..start + len])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styled_line_keeps_pacman_colours() {
        let mut s = Settler::new(80);
        let lines = s.feed_styled(b"\x1b[1;34m::\x1b[0;1m Synchronizing\x1b[0m   \r\n");
        let (plain, styled) = &lines[0];
        assert_eq!(plain, ":: Synchronizing");
        assert_eq!(styled, "\u{1b}[0;1;34m::\u{1b}[0;1m Synchronizing\u{1b}[0m");
    }

    #[test]
    fn styled_line_without_colour_is_the_plain_line() {
        let mut s = Settler::new(80);
        let lines = s.feed_styled(b"plain text\r\n");
        assert_eq!(lines[0].0, lines[0].1);
    }

    const OUT: &[u8] = include_bytes!("../../tests/fixtures/pacman-run/2026-10-16.out");

    #[test]
    fn fixture_is_scrubbed() {
        let s = String::from_utf8_lossy(OUT);
        for bad in ["machineid=", "bootid=", "hostname=", "satyxsing"] {
            assert!(!s.contains(bad), "{bad}");
        }
    }

    #[test]
    fn line_is_settled_on_newline() {
        let mut s = Settler::new(80);
        assert_eq!(s.feed(b"abc\r\n"), vec!["abc"]);
        assert!(s.feed(b"partial").is_empty());
        assert_eq!(s.partial(), "partial");
    }

    #[test]
    fn carriage_return_redraw_settles_final_text() {
        let mut s = Settler::new(80);
        assert_eq!(
            s.feed(b"( 1/2) x  10%\r( 1/2) x 100%\r\n"),
            vec!["( 1/2) x 100%"]
        );
    }

    #[test]
    fn soft_wrapped_line_comes_back_whole() {
        let mut s = Settler::new(20);
        let long = "error: ".to_string() + &"x".repeat(30);
        assert_eq!(s.feed(format!("{long}\r\n").as_bytes()), vec![long]);
    }

    #[test]
    fn line_longer_than_fifty_rows_keeps_its_start() {
        let mut s = Settler::new(20);
        let long = "error: ".to_string() + &"x".repeat(1000);
        assert_eq!(s.feed(format!("{long}\r\n").as_bytes()), vec![long]);
    }

    #[test]
    fn resize_follows_the_new_width() {
        let mut s = Settler::new(20);
        s.resize(40);
        let a = "A".repeat(35);
        assert_eq!(
            s.feed(format!("{a}\r\x1b[2Kdone\r\n").as_bytes()),
            vec!["done"]
        );
    }

    #[test]
    fn bar_redraw_is_visible_in_partial() {
        let mut s = Settler::new(80);
        s.feed(b" Total ( 3/38)  26.1 MiB  [-c  o ]   3%\r Total ( 7/38)  43.2 MiB  [---Co ]   6%");
        assert!(s.partial().ends_with("6%"));
    }

    #[test]
    fn tail_keeps_rows_below_the_cursor() {
        let mut s = Settler::new(80);
        s.feed(b"prompt\r\nerror: failed\r\n\x1b[2A");
        assert_eq!(s.tail(2), vec!["prompt", "error: failed"]);
    }

    #[test]
    fn tail_skips_empty_rows() {
        let mut s = Settler::new(80);
        s.feed(b"a\r\n\r\nb  \r\nc");
        assert_eq!(s.tail(2), vec!["b", "c"]);
        assert_eq!(s.tail(9), vec!["a", "b", "c"]);
    }

    #[test]
    fn fixture_settles_known_lines() {
        let lines = fixture_lines();
        for want in [
            "( 7/13) Restart a running sshd.service",
            "==> root: 1002",
            ":: Running post-transaction hooks...",
        ] {
            assert!(lines.iter().any(|l| l == want), "missing {want:?}");
        }
        // vt100 turns `\t` into cursor movement, so the tab reads back as spaces.
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("foreign") && l.ends_with("wps-office")),
            "missing foreign wps-office"
        );
        assert_eq!(
            lines.iter().filter(|l| l.contains(") upgrading ")).count(),
            38
        );
    }

    #[test]
    fn partial_joins_wrapped_rows() {
        let mut s = Settler::new(20);
        s.feed(format!("error: {}", "0".repeat(30)).as_bytes());
        assert_eq!(s.partial(), format!("error: {}", "0".repeat(30)));
    }
}
