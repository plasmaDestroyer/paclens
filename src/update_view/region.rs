//! The live region drawn inline under the shell prompt (#81): no alternate
//! screen, so scrollback keeps everything printed above it.

use std::io::{self, Write};

use crossterm::cursor::MoveUp;
use crossterm::queue;
use crossterm::terminal::{Clear, ClearType};

use super::render::Frame;

/// How many rows the last drawn region took; the cursor sits on the row
/// under it.
#[derive(Default)]
pub struct Region {
    height: u16,
}

impl Region {
    pub fn new() -> Self {
        Self::default()
    }

    /// Erase the last region, print `frame.above` for good, then draw
    /// `frame.region`. A region taller than `rows - 1` keeps its last rows:
    /// `MoveUp` cannot reach rows already in scrollback.
    pub fn draw(&mut self, out: &mut impl Write, frame: &Frame, rows: u16) -> io::Result<()> {
        self.erase(out)?;
        for l in &frame.above {
            write!(out, "{l}\r\n")?;
        }
        let fit = usize::from(rows.saturating_sub(1));
        let skip = frame.region.len().saturating_sub(fit);
        for l in &frame.region[skip..] {
            write!(out, "{l}\r\n")?;
        }
        self.height = u16::try_from(frame.region.len() - skip).unwrap_or(u16::MAX);
        out.flush()
    }

    /// Erase the last region and print `lines` for good.
    pub fn close(&mut self, out: &mut impl Write, lines: &[String]) -> io::Result<()> {
        self.erase(out)?;
        for l in lines {
            write!(out, "{l}\r\n")?;
        }
        self.height = 0;
        out.flush()
    }

    fn erase(&mut self, out: &mut impl Write) -> io::Result<()> {
        if self.height > 0 {
            queue!(out, MoveUp(self.height))?;
        }
        write!(out, "\r")?;
        queue!(out, Clear(ClearType::FromCursorDown))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(above: &[&str], region: usize) -> Frame {
        Frame {
            above: above.iter().map(|s| s.to_string()).collect(),
            region: (0..region).map(|i| format!("row {i}")).collect(),
        }
    }

    fn draw(r: &mut Region, f: &Frame, rows: u16) -> String {
        let mut out = Vec::new();
        r.draw(&mut out, f, rows).unwrap();
        String::from_utf8(out).unwrap()
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
    fn close_erases_and_prints() {
        let mut r = Region::new();
        draw(&mut r, &frame(&[], 2), 40);
        let mut out = Vec::new();
        r.close(&mut out, &["done".to_string()]).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\x1b[2A") && s.ends_with("done\r\n"), "{s:?}");
        assert_eq!(r.height, 0);
    }
}
