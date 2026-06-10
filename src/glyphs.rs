//! Single-character display glyphs shared by the TUI theme (`src/tui/theme.rs`)
//! and the CLI styler (`src/cli/style.rs`), so the two never drift.
//!
//! Unicode by default; ASCII in the no-color path so a `--no-color` terminal (or
//! one without the glyphs) never renders tofu boxes.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyphs {
    pub available: &'static str,
    pub unavailable: &'static str,
    pub bullet: &'static str,
    pub up: &'static str,
    pub down: &'static str,
    /// Leading marker for a selected/active row.
    pub pointer: &'static str,
    /// Version-transition arrow (`current → new`).
    pub arrow: &'static str,
    /// Mark inside a `[✓]` / `[ ]` toggle when enabled.
    pub check: &'static str,
}

pub const UNICODE: Glyphs = Glyphs {
    available: "●",
    unavailable: "○",
    bullet: "·",
    up: "↑",
    down: "↓",
    pointer: "▶ ",
    arrow: "→",
    check: "✓",
};

pub const ASCII: Glyphs = Glyphs {
    available: "*",
    unavailable: "-",
    bullet: "-",
    up: "^",
    down: "v",
    pointer: "> ",
    arrow: "->",
    check: "x",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_and_ascii_sets_differ() {
        assert_ne!(UNICODE.available, ASCII.available);
        assert_ne!(UNICODE.unavailable, ASCII.unavailable);
        assert_ne!(UNICODE.pointer, ASCII.pointer);
    }

    #[test]
    fn glyph_values_are_what_we_expect() {
        assert_eq!(UNICODE.available, "●");
        assert_eq!(UNICODE.unavailable, "○");
        assert_eq!(ASCII.available, "*");
        assert_eq!(ASCII.unavailable, "-");
    }
}
