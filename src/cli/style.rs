//! Plain-text styling for CLI output, the stdout/stderr counterpart to the TUI's
//! `src/tui/theme.rs`. Both follow one semantic palette (green = available,
//! yellow = pending updates, dim = secondary, bold = emphasis) and the same
//! shared glyphs (`crate::glyphs`).
//!
//! Color is emitted via `crossterm` (already a dependency) only when it makes
//! sense: not `--no-color`, not `color_theme = "none"`, and the stream is a TTY
//! (so piped/redirected output stays clean). When color is off, the no-color
//! glyph set is used too, matching the TUI's `none` theme.

use crossterm::style::{StyledContent, Stylize};

use crate::config::ColorTheme;
use crate::glyphs::{self, Glyphs};

pub struct Styles {
    color: bool,
    glyphs: Glyphs,
}

impl Styles {
    /// Resolve styling from the `--no-color` flag, the configured theme, and
    /// whether the target stream is a terminal.
    ///
    /// Two independent decisions: **color** (ANSI) is emitted only when not
    /// forced plain *and* the stream is a TTY — so piping keeps the output clean.
    /// **ASCII glyphs** are used only when explicitly forced plain (`--no-color`
    /// or `color_theme = "none"`); merely piping keeps the nicer Unicode glyphs,
    /// which any UTF-8 sink renders fine.
    pub fn resolve(no_color: bool, theme: ColorTheme, is_tty: bool) -> Self {
        let force_plain = no_color || theme == ColorTheme::None;
        let color = !force_plain && is_tty;
        let glyphs = if force_plain {
            glyphs::ASCII
        } else {
            glyphs::UNICODE
        };
        Styles { color, glyphs }
    }

    /// The active bullet separator ("·" with Unicode glyphs, "-" in the ASCII path).
    pub fn bullet(&self) -> &'static str {
        self.glyphs.bullet
    }

    /// The active version-transition arrow ("→" Unicode, "->" ASCII).
    pub fn arrow(&self) -> &'static str {
        self.glyphs.arrow
    }

    /// The active success mark ("✓" Unicode, "x" ASCII).
    pub fn check(&self) -> &'static str {
        self.glyphs.check
    }

    /// The active failure mark ("✗" Unicode, "!" ASCII).
    pub fn cross(&self) -> &'static str {
        self.glyphs.cross
    }

    /// Apply `style` only when color is enabled; otherwise return the text plain.
    fn paint(&self, text: &str, style: impl FnOnce(String) -> StyledContent<String>) -> String {
        if self.color {
            style(text.to_string()).to_string()
        } else {
            text.to_string()
        }
    }

    /// Bold emphasis (the "paclens" heading).
    pub fn title(&self, s: &str) -> String {
        self.paint(s, |t| t.bold())
    }

    /// Secondary / meta text (header row, timestamps, cache size).
    pub fn dim(&self, s: &str) -> String {
        self.paint(s, |t| t.dim())
    }

    /// Headline when there is nothing to do.
    pub fn summary_ok(&self, s: &str) -> String {
        self.paint(s, |t| t.green().bold())
    }

    /// Headline / accent for pending updates.
    pub fn summary_updates(&self, s: &str) -> String {
        self.paint(s, |t| t.yellow().bold())
    }

    /// A pre-padded update count: accented when there are any, dim when zero
    /// (the caller pads to the column width so alignment survives styling).
    pub fn updates_count(&self, padded: &str, count: usize) -> String {
        if count > 0 {
            self.paint(padded, |t| t.yellow().bold())
        } else {
            self.dim(padded)
        }
    }

    /// "<glyph> ok" — the source's binary was found at scan time, in green.
    /// (Wording chosen with the user: "available" next to the updates column
    /// read like "updates available".)
    pub fn available(&self) -> String {
        let text = format!("{} ok", self.glyphs.available);
        self.paint(&text, |t| t.green())
    }

    /// "<glyph> not found" — the source's binary is not on PATH, dim.
    pub fn unavailable(&self) -> String {
        let text = format!("{} not found", self.glyphs.unavailable);
        self.dim(&text)
    }

    /// A succeeded step / positive mark, in green (not bold — `summary_ok` is
    /// the headline weight).
    pub fn success(&self, s: &str) -> String {
        self.paint(s, |t| t.green())
    }

    /// An "error:" prefix for stderr messages.
    pub fn error(&self, s: &str) -> String {
        self.paint(s, |t| t.red().bold())
    }

    /// A "warning:" prefix for stderr messages.
    pub fn warn(&self, s: &str) -> String {
        self.paint(s, |t| t.yellow().bold())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() -> Styles {
        // no_color = true forces the no-color path.
        Styles::resolve(true, ColorTheme::Dark, true)
    }

    fn colored() -> Styles {
        Styles::resolve(false, ColorTheme::Dark, true)
    }

    const ESC: char = '\u{1b}';

    #[test]
    fn no_color_returns_plain_text() {
        let s = plain();
        assert_eq!(s.title("paclens"), "paclens");
        assert_eq!(s.error("error:"), "error:");
        assert_eq!(s.warn("warning:"), "warning:");
        assert_eq!(s.summary_ok("up to date"), "up to date");
        assert_eq!(s.updates_count("  0", 0), "  0");
        assert_eq!(s.updates_count("  3", 3), "  3");
        assert_eq!(s.available(), "* ok");
        assert_eq!(s.unavailable(), "- not found");
        assert_eq!(s.success("done"), "done");
        assert_eq!(s.check(), "x");
        assert_eq!(s.cross(), "!");
    }

    #[test]
    fn color_emits_ansi_and_unicode_glyphs() {
        let s = colored();
        assert!(s.error("error:").contains(ESC));
        assert!(s.summary_updates("3 updates available").contains(ESC));
        assert!(s.updates_count("  3", 3).contains(ESC));
        let avail = s.available();
        assert!(avail.contains(ESC));
        assert!(avail.contains('●')); // unicode glyph in color mode
        assert!(s.success("done").contains(ESC));
        assert_eq!(s.check(), "✓");
        assert_eq!(s.cross(), "✗");
    }

    #[test]
    fn zero_updates_are_dim_not_accented() {
        // Both dim; the point is it does not panic and stays plain without color.
        assert_eq!(plain().updates_count("  0", 0), "  0");
        assert!(colored().updates_count("  0", 0).contains(ESC));
    }

    #[test]
    fn piping_drops_color_but_keeps_unicode_glyphs() {
        // Not forced plain, but not a TTY: no ANSI, yet the nicer glyphs stay.
        let s = Styles::resolve(false, ColorTheme::Dark, false);
        assert_eq!(s.error("error:"), "error:");
        assert!(!s.available().contains(ESC));
        assert_eq!(s.available(), "● ok");
        assert_eq!(s.bullet(), "·");
    }

    #[test]
    fn forced_plain_switches_to_ascii_glyphs() {
        // --no-color or color_theme = "none" → ASCII glyphs and no color.
        for s in [
            Styles::resolve(true, ColorTheme::Dark, true),
            Styles::resolve(false, ColorTheme::None, true),
        ] {
            assert_eq!(s.available(), "* ok");
            assert_eq!(s.bullet(), "-");
            assert!(!s.error("error:").contains(ESC));
        }
    }
}
