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
    /// Mark inside a `[✓]` / `[ ]` toggle when enabled; also a succeeded step.
    pub check: &'static str,
    /// A failed step. ASCII falls back to `!` since `x` already means checked.
    pub cross: &'static str,
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
    cross: "✗",
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
    cross: "!",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_and_ascii_sets_differ() {
        assert_ne!(UNICODE.available, ASCII.available);
        assert_ne!(UNICODE.unavailable, ASCII.unavailable);
        assert_ne!(UNICODE.pointer, ASCII.pointer);
        assert_ne!(UNICODE.cross, ASCII.cross);
    }

    #[test]
    fn ascii_cross_is_distinct_from_ascii_check() {
        // `[x]` means an enabled toggle, so a failed step must not also be `x`.
        assert_ne!(ASCII.cross, ASCII.check);
    }

    #[test]
    fn glyph_values_are_what_we_expect() {
        assert_eq!(UNICODE.available, "●");
        assert_eq!(UNICODE.unavailable, "○");
        assert_eq!(ASCII.available, "*");
        assert_eq!(ASCII.unavailable, "-");
    }
}
