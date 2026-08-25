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
    /// Horizontal navigation, as in the dashboard's pane-focus hint.
    pub left: &'static str,
    pub right: &'static str,
    /// Leading marker for a selected/active row.
    pub pointer: &'static str,
    /// Version-transition arrow (`current → new`).
    pub arrow: &'static str,
    /// Mark inside a `[✓]` / `[ ]` toggle when enabled; also a succeeded step.
    pub check: &'static str,
    /// A failed step. ASCII falls back to `!` since `x` already means checked.
    pub cross: &'static str,
    /// Animation frames for the background-scan spinner.
    pub spinner: &'static [&'static str],
    /// Tree drawing: mid branch, last branch, continuation pipe, blank indent.
    pub tree_branch: &'static str,
    pub tree_last: &'static str,
    pub tree_pipe: &'static str,
    pub tree_blank: &'static str,
}

pub const UNICODE: Glyphs = Glyphs {
    available: "●",
    unavailable: "○",
    bullet: "·",
    up: "↑",
    down: "↓",
    left: "←",
    right: "→",
    pointer: "▶ ",
    arrow: "→",
    check: "✓",
    cross: "✗",
    spinner: &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
    tree_branch: "├─ ",
    tree_last: "└─ ",
    tree_pipe: "│  ",
    tree_blank: "   ",
};

pub const ASCII: Glyphs = Glyphs {
    available: "*",
    unavailable: "-",
    bullet: "-",
    up: "^",
    down: "v",
    left: "<",
    right: ">",
    pointer: "> ",
    arrow: "->",
    check: "x",
    cross: "!",
    spinner: &["|", "/", "-", "\\"],
    tree_branch: "|- ",
    tree_last: "`- ",
    tree_pipe: "|  ",
    tree_blank: "   ",
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
    fn spinner_sets_are_non_empty_and_ascii_safe() {
        assert!(!UNICODE.spinner.is_empty());
        assert!(!ASCII.spinner.is_empty());
        assert!(ASCII.spinner.iter().all(|f| f.is_ascii()));
    }

    #[test]
    fn ascii_cross_is_distinct_from_ascii_check() {
        // `[x]` means an enabled toggle, so a failed step must not also be `x`.
        assert_ne!(ASCII.cross, ASCII.check);
    }

    #[test]
    fn ascii_set_is_entirely_ascii() {
        // The no-color path must never emit a glyph a plain terminal renders
        // as tofu — arrows included.
        for g in [
            ASCII.available,
            ASCII.unavailable,
            ASCII.bullet,
            ASCII.up,
            ASCII.down,
            ASCII.left,
            ASCII.right,
            ASCII.pointer,
            ASCII.arrow,
            ASCII.check,
            ASCII.cross,
            ASCII.tree_branch,
            ASCII.tree_last,
            ASCII.tree_pipe,
            ASCII.tree_blank,
        ] {
            assert!(g.is_ascii(), "non-ascii glyph in the ASCII set: {g:?}");
        }
    }

    #[test]
    fn horizontal_glyphs_are_one_column_in_both_sets() {
        for g in [UNICODE.left, UNICODE.right, ASCII.left, ASCII.right] {
            assert_eq!(g.chars().count(), 1, "{g:?} is not one column");
        }
    }

    #[test]
    fn glyph_values_are_what_we_expect() {
        assert_eq!(UNICODE.available, "●");
        assert_eq!(UNICODE.unavailable, "○");
        assert_eq!(ASCII.available, "*");
        assert_eq!(ASCII.unavailable, "-");
    }
}
