//! Where the view goes (#81): a live region drawn inline under the shell
//! prompt when stdout is a terminal — no alternate screen, so scrollback
//! keeps everything printed above it — or plain appended lines otherwise.

use std::io::{self, Write};
use std::time::Duration;

use crossterm::cursor::{MoveToColumn, MoveUp};
use crossterm::queue;
use crossterm::terminal::{Clear, ClearType};
use unicode_width::UnicodeWidthChar;

use crate::cli::style::Styles;

use super::model::{Outcome, Run};
use super::render::{Delivered, Frame, View, frame};

/// The cursor is hidden while the view draws: a block blinking under the
/// region says "type here" when there is nothing to type. It shows only
/// where pacman waits for an answer.
const HIDE: &str = "\x1b[?25l";
const SHOW: &str = "\x1b[?25h";

/// Shows the cursor again when dropped: every return path and unwinding
/// leave the terminal as they found it.
pub struct CursorGuard;

impl Drop for CursorGuard {
    fn drop(&mut self) {
        let mut out = io::stdout();
        let _ = write!(out, "{SHOW}");
        let _ = out.flush();
    }
}

/// How many terminal rows the last drawn region took. The cursor sits on
/// the row under it, or, `parked`, at the end of its last row (a prompt).
#[derive(Default)]
pub struct Region {
    height: u16,
    parked: bool,
}

impl Region {
    pub fn new() -> Self {
        Self::default()
    }

    /// Erase the last region, print `frame.above` for good, then draw
    /// `frame.region` on a `rows`×`cols` terminal. A region taller than
    /// `rows - 1` keeps its last rows: `MoveUp` cannot reach rows already in
    /// scrollback. A line wider than `cols` counts every row it wraps onto.
    pub fn draw(
        &mut self,
        out: &mut impl Write,
        frame: &Frame,
        rows: u16,
        cols: u16,
    ) -> io::Result<()> {
        // One synchronized update (mode 2026): a terminal that knows it
        // shows the whole frame at once, never half drawn; others ignore it.
        write!(out, "\x1b[?2026h{HIDE}")?;
        self.erase(out)?;
        for l in &frame.above {
            write!(out, "{l}\r\n")?;
        }
        let room = usize::from(rows.saturating_sub(1));
        let (mut used, mut skip) = (0, frame.region.len());
        for l in frame.region.iter().rev() {
            let h = rows_of(l, cols);
            if used + h > room {
                break;
            }
            used += h;
            skip -= 1;
        }
        let mut last = frame.region[skip..].last().cloned();
        for l in &frame.region[skip..] {
            write!(out, "{l}\r\n")?;
        }
        // The last line alone is too tall: its end still shows, so a long
        // prompt keeps its answer choices.
        if skip == frame.region.len()
            && room > 0
            && let Some(whole) = frame.region.last()
        {
            let end = tail_fit(whole, room, cols);
            used = rows_of(&end, cols);
            write!(out, "{end}\r\n")?;
            last = Some(end);
        }
        self.height = u16::try_from(used).unwrap_or(u16::MAX);
        // Waiting for an answer: back up to the end of the question.
        self.parked = false;
        if frame.cursor
            && used > 0
            && let Some(l) = last
        {
            let w = visible(&l).filter_map(|c| c.width()).sum::<usize>();
            let c = usize::from(cols.max(1));
            let col = if w == 0 { 0 } else { (w - 1) % c + 1 };
            queue!(
                out,
                MoveUp(1),
                MoveToColumn(u16::try_from(col).unwrap_or(0))
            )?;
            write!(out, "{SHOW}")?;
            self.parked = true;
        }
        write!(out, "\x1b[?2026l")?;
        out.flush()
    }

    /// Erase the last region and print `lines` for good.
    pub fn close(&mut self, out: &mut impl Write, lines: &[String]) -> io::Result<()> {
        self.erase(out)?;
        self.leave(out, lines)
    }

    /// Keep the last region on screen and print `lines` under it.
    pub fn leave(&mut self, out: &mut impl Write, lines: &[String]) -> io::Result<()> {
        if std::mem::take(&mut self.parked) {
            write!(out, "\r\n")?;
        }
        for l in lines {
            write!(out, "{l}\r\n")?;
        }
        self.height = 0;
        write!(out, "{SHOW}")?;
        out.flush()
    }

    fn erase(&mut self, out: &mut impl Write) -> io::Result<()> {
        // Parked, the cursor is on the region's last row, not under it.
        let up = self.height - u16::from(self.parked && self.height > 0);
        self.parked = false;
        if up > 0 {
            queue!(out, MoveUp(up))?;
        }
        write!(out, "\r")?;
        queue!(out, Clear(ClearType::FromCursorDown))
    }
}

/// The visible characters of `line`, colour escapes left out.
fn visible(line: &str) -> impl Iterator<Item = char> + '_ {
    let mut esc = false;
    line.chars().filter(move |&c| match (esc, c) {
        (false, '\u{1b}') => {
            esc = true;
            false
        }
        (true, c) => {
            esc = !c.is_ascii_alphabetic();
            false
        }
        (false, _) => true,
    })
}

/// Terminal rows `line` takes at `cols` columns. A wide character that does
/// not fit at the end of a row starts the next one, as terminals do.
fn rows_of(line: &str, cols: u16) -> usize {
    let cols = usize::from(cols.max(1));
    let (mut rows, mut col) = (1, 0);
    for w in visible(line).filter_map(|c| c.width()).filter(|&w| w > 0) {
        if col + w > cols {
            rows += 1;
            col = 0;
        }
        col += w;
    }
    rows
}

/// The end of `line` that fits in `room` rows of `cols`, without colour.
fn tail_fit(line: &str, room: usize, cols: u16) -> String {
    let chars: Vec<char> = visible(line).collect();
    let mut start = chars.len();
    while start > 0 {
        let candidate: String = chars[start - 1..].iter().collect();
        if rows_of(&candidate, cols) > room {
            break;
        }
        start -= 1;
    }
    chars[start..].iter().collect()
}

/// The view's output, for the live run and the replay alike.
pub struct Painter {
    live: bool,
    rows: u16,
    cols: u16,
    region: Region,
    sent: Delivered,
    /// Plain mode: the unfinished line already printed while pacman waited.
    shown: String,
    /// Plain mode line end: `\r\n` while raw mode is on — raw mode turns off
    /// the terminal's own `\n` → `\r\n`, and `| cat` lands there.
    eol: &'static str,
}

impl Painter {
    /// `live`: stdout is a terminal of `rows`×`cols`.
    pub fn new(live: bool, rows: u16, cols: u16) -> Self {
        Self {
            live,
            rows,
            cols,
            region: Region::new(),
            sent: Delivered::default(),
            shown: String::new(),
            eol: "\n",
        }
    }

    /// Plain lines end `\r\n`, for while stdin is in raw mode.
    pub fn raw(mut self) -> Self {
        self.eol = "\r\n";
        self
    }

    /// A line pacman finished. Plain mode prints it (less the prompt text
    /// already shown); the live region gets it through the next frame.
    pub fn line(&mut self, out: &mut impl Write, line: &str) -> io::Result<()> {
        if !self.live {
            let rest = line.strip_prefix(self.shown.as_str()).unwrap_or(line);
            write!(out, "{rest}{}", self.eol)?;
            self.shown.clear();
        }
        Ok(())
    }

    /// Plain mode, while pacman waits: its whole unfinished line, printed
    /// once without a newline, so a piped run still shows the question.
    pub fn prompt(&mut self, out: &mut impl Write, partial: &str) -> io::Result<()> {
        if self.live || !self.shown.is_empty() || partial.is_empty() {
            return Ok(());
        }
        write!(out, "{partial}")?;
        self.shown = partial.to_string();
        out.flush()
    }

    /// One frame of the live region (plain mode draws none). `idle` =
    /// pacman's screen tail when it waits for an answer.
    pub fn tick(
        &mut self,
        out: &mut impl Write,
        run: &Run,
        view: &View,
        now: Duration,
        idle: Option<&[String]>,
        s: &Styles,
    ) -> io::Result<()> {
        if !self.live {
            return Ok(());
        }
        let (f, d) = frame(run, view, self.sent, self.cols, self.rows, now, idle, s);
        self.sent = d;
        self.region.draw(out, &f, self.rows, self.cols)?;
        // Terminal progress (OSC 9;4): the running phase's share done.
        if let Some(share) = run.progress(now) {
            write!(out, "\x1b]9;4;1;{share}\x07")?;
            out.flush()?;
        }
        Ok(())
    }

    /// The end: on exit 0 the region gives way to `lines`; on a failure it
    /// stays, with `lines` under it.
    pub fn close(&mut self, out: &mut impl Write, run: &Run, lines: &[String]) -> io::Result<()> {
        let failed = matches!(run.outcome(), Outcome::Failed(_));
        if !self.live {
            if !self.shown.is_empty() {
                write!(out, "{}", self.eol)?;
            }
            for l in lines {
                write!(out, "{l}{}", self.eol)?;
            }
            return out.flush();
        }
        if failed {
            self.region.leave(out, lines)?;
            write!(out, "\x1b]9;4;2;100\x07")?;
        } else {
            self.region.close(out, lines)?;
            write!(out, "\x1b]9;4;0;0\x07")?;
        }
        out.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(above: &[&str], region: usize) -> Frame {
        Frame {
            above: above.iter().map(|s| s.to_string()).collect(),
            region: (0..region).map(|i| format!("row {i}")).collect(),
            cursor: false,
        }
    }

    fn draw(r: &mut Region, f: &Frame, rows: u16) -> String {
        let mut out = Vec::new();
        r.draw(&mut out, f, rows, 80).unwrap();
        let s = String::from_utf8(out).unwrap();
        // Every frame is one synchronized update; the tests read inside it.
        assert!(
            s.starts_with("\x1b[?2026h") && s.ends_with("\x1b[?2026l"),
            "{s:?}"
        );
        // Each frame opens hiding the cursor.
        s["\x1b[?2026h\x1b[?25l".len()..s.len() - "\x1b[?2026l".len()].to_string()
    }

    #[test]
    fn second_draw_moves_up_over_the_first() {
        let mut r = Region::new();
        let first = draw(&mut r, &frame(&[], 3), 40);
        assert!(!first.contains("\x1b[3A"));
        let second = draw(&mut r, &frame(&[], 1), 40);
        assert!(second.starts_with("\x1b[3A\r\x1b[J"), "{second:?}");
        assert!(!first.contains("\x1b[?1049h") && !second.contains("\x1b[?1049h"));
    }

    #[test]
    fn above_goes_before_region() {
        let mut r = Region::new();
        let s = draw(&mut r, &frame(&["kept"], 1), 40);
        assert!(s.ends_with("kept\r\nrow 0\r\n"), "{s:?}");
    }

    #[test]
    fn tall_region_keeps_its_last_rows() {
        let mut r = Region::new();
        let s = draw(&mut r, &frame(&[], 12), 8);
        assert_eq!(s.matches("\r\n").count(), 7);
        assert!(!s.contains("row 4\r") && s.contains("row 5\r") && s.contains("row 11\r"));
        let s = draw(&mut r, &frame(&[], 0), 8);
        assert!(s.starts_with("\x1b[7A"), "{s:?}");
    }

    #[test]
    fn wrapped_line_counts_every_row() {
        let mut r = Region::new();
        let f = Frame {
            above: vec![],
            region: vec!["x".repeat(25), "\x1b[1mshort\x1b[0m".into()],
            cursor: false,
        };
        let mut out = Vec::new();
        r.draw(&mut out, &f, 40, 10).unwrap();
        assert_eq!(r.height, 4);
        // Only the short line fits under a 3-row terminal.
        r.draw(&mut out, &f, 3, 10).unwrap();
        assert_eq!(r.height, 1);
    }

    #[test]
    fn wide_characters_wrap_whole() {
        assert_eq!(rows_of("界界界", 3), 3);
        assert_eq!(rows_of("abc", 3), 1);
        assert_eq!(rows_of("abcd", 3), 2);
        assert_eq!(rows_of("", 3), 1);
    }

    #[test]
    fn too_tall_last_line_keeps_its_end() {
        let mut r = Region::new();
        let f = Frame {
            above: vec![],
            region: vec![":: Proceed with installation? [Y/n]".into()],
            cursor: false,
        };
        let mut out = Vec::new();
        r.draw(&mut out, &f, 4, 10).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.ends_with("? [Y/n]\r\n\x1b[?2026l"), "{s:?}");
        assert_eq!(r.height, 3);
    }

    #[test]
    fn close_erases_and_leave_keeps() {
        let mut r = Region::new();
        draw(&mut r, &frame(&[], 2), 40);
        let mut out = Vec::new();
        r.close(&mut out, &["done".to_string()]).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.starts_with("\x1b[2A") && s.ends_with("done\r\n\x1b[?25h"),
            "{s:?}"
        );
        assert_eq!(r.height, 0);

        draw(&mut r, &frame(&[], 2), 40);
        let mut out = Vec::new();
        r.leave(&mut out, &["failed".to_string()]).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "failed\r\n\x1b[?25h");
    }

    #[test]
    fn raw_plain_painter_ends_lines_with_crlf() {
        let mut p = Painter::new(false, 24, 80).raw();
        let mut out = Vec::new();
        p.line(&mut out, "one").unwrap();
        assert_eq!(out, b"one\r\n");
    }

    #[test]
    fn plain_painter_appends_and_shows_the_prompt_once() {
        let run = Run::new("pacman -S foo", Default::default(), Default::default());
        let view = View {
            layout: crate::config::Layout::Inline,
            motif: crate::config::Motif::Subtle,
            candy: false,
        };
        let s = Styles::resolve(true, crate::config::ColorTheme::Dark, false);
        let mut p = Painter::new(false, 24, 80);
        let mut out = Vec::new();
        p.line(&mut out, "resolving dependencies...").unwrap();
        let idle = vec![":: Proceed with installation? [Y/n]".to_string()];
        for _ in 0..3 {
            p.tick(&mut out, &run, &view, Duration::ZERO, Some(&idle), &s)
                .unwrap();
            p.prompt(&mut out, &idle[0]).unwrap();
        }
        p.line(&mut out, ":: Proceed with installation? [Y/n] y")
            .unwrap();
        p.close(&mut out, &run, &["end".to_string()]).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "resolving dependencies...\n:: Proceed with installation? [Y/n] y\nend\n"
        );
    }

    #[test]
    fn a_prompt_parks_the_cursor_at_its_end() {
        let mut r = Region::new();
        let mut f = frame(&[], 1);
        f.region.push(":: Proceed? [Y/n] ".into());
        f.cursor = true;
        let s = draw(&mut r, &f, 40);
        // Up to the question's row, after its last character, shown.
        assert!(s.ends_with("[Y/n] \r\n\x1b[1A\x1b[19G\x1b[?25h"), "{s:?}");
        // The next frame erases from the region's top: one row less up.
        let s = draw(&mut r, &frame(&[], 1), 40);
        assert!(s.starts_with("\x1b[1A\r\x1b[J"), "{s:?}");
    }
}
