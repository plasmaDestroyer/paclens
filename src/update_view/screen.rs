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

    /// Lines completed by a `\n` in `bytes`, trailing whitespace trimmed.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        let mut lines = Vec::new();
        let mut segments = bytes.split(|&b| b == b'\n').peekable();
        while let Some(segment) = segments.next() {
            self.parser.process(segment);
            if segments.peek().is_some() {
                lines.push(self.cursor_line());
                self.parser.process(b"\n");
            }
        }
        lines
    }

    /// The unfinished line under the cursor, trimmed.
    pub fn partial(&self) -> String {
        let screen = self.parser.screen();
        let (row, _) = screen.cursor_position();
        screen
            .contents_between(row, 0, row, self.cols)
            .trim_end()
            .to_string()
    }

    /// The last `n` non-empty visible rows, trimmed, up to the cursor row.
    pub fn tail(&self, n: usize) -> Vec<String> {
        let screen = self.parser.screen();
        let (cursor, _) = screen.cursor_position();
        let rows: Vec<String> = screen
            .rows(0, self.cols)
            .take(usize::from(cursor) + 1)
            .map(|r| r.trim_end().to_string())
            .filter(|r| !r.is_empty())
            .collect();
        rows[rows.len().saturating_sub(n)..].to_vec()
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
    fn bar_redraw_is_visible_in_partial() {
        let mut s = Settler::new(80);
        s.feed(b" Total ( 3/38)  26.1 MiB  [-c  o ]   3%\r Total ( 7/38)  43.2 MiB  [---Co ]   6%");
        assert!(s.partial().ends_with("6%"));
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
}
