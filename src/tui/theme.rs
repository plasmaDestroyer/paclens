//! The single home for all colors, borders, and glyphs (CLAUDE.md convention:
//! "colors live only in `src/tui/theme.rs`").
//!
//! Design intent (chosen with the user): minimal, not a branded palette. We lean
//! on the terminal's own foreground/background and ANSI palette so the dashboard
//! adapts to whatever theme the user runs. Secondary text is dimmed, emphasis is
//! bold, and a single restrained accent marks the one number that matters
//! (pending updates). `--no-color` (or `color_theme = "none"`) drops all color
//! and switches to ASCII box drawing and ASCII glyphs.

use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;

use crate::config::ColorTheme;
use crate::glyphs::{ASCII, Glyphs, UNICODE};

/// ASCII box-drawing set for the no-color path.
const ASCII_BORDER: border::Set<'static> = border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

/// A resolved set of styles, borders, and glyphs for one render pass.
#[derive(Debug, Clone)]
pub struct Theme {
    pub border_set: border::Set<'static>,
    pub glyphs: Glyphs,
    /// Bold title text ("paclens · dashboard").
    pub title: Style,
    /// Default body text.
    pub primary: Style,
    /// Secondary / meta text (timestamps, footer hints).
    pub dim: Style,
    /// The one number that matters: pending updates.
    pub accent: Style,
    /// A source is available.
    pub success: Style,
    /// A source is unavailable (muted, not alarming).
    pub unavailable: Style,
    /// Border lines.
    pub border: Style,
    /// The selected table row.
    pub selected: Style,
    /// The table header row.
    pub header: Style,
}

impl Theme {
    /// Pick a theme from the config value and the `--no-color` flag. `--no-color`
    /// always wins, as does `color_theme = "none"`.
    pub fn resolve(theme: ColorTheme, no_color: bool) -> Self {
        if no_color || theme == ColorTheme::None {
            Self::none()
        } else if theme == ColorTheme::Light {
            Self::light()
        } else {
            Self::dark()
        }
    }

    /// Dark and light currently share one adaptive palette: ANSI named colors and
    /// modifiers, resolved by the terminal's own scheme. This keeps the look
    /// minimal and the variants honest rather than hard-coded RGB; they can
    /// diverge later if a real need appears.
    pub fn dark() -> Self {
        Self::colored()
    }

    pub fn light() -> Self {
        Self::colored()
    }

    fn colored() -> Self {
        Theme {
            border_set: border::ROUNDED,
            glyphs: UNICODE,
            title: Style::new().add_modifier(Modifier::BOLD),
            primary: Style::new(),
            dim: Style::new().add_modifier(Modifier::DIM),
            accent: Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            success: Style::new().fg(Color::Green),
            unavailable: Style::new().add_modifier(Modifier::DIM),
            border: Style::new().add_modifier(Modifier::DIM),
            // Focused row: a distinct hue from accent (updates) and success
            // (available) so the three never blur, plus the `▶` pointer. No
            // reverse-video in colored mode.
            selected: Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            header: Style::new()
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::DIM),
        }
    }

    pub fn none() -> Self {
        Theme {
            border_set: ASCII_BORDER,
            glyphs: ASCII,
            title: Style::new().add_modifier(Modifier::BOLD),
            primary: Style::new(),
            dim: Style::new(),
            accent: Style::new().add_modifier(Modifier::BOLD),
            success: Style::new(),
            unavailable: Style::new(),
            border: Style::new(),
            selected: Style::new().add_modifier(Modifier::REVERSED),
            header: Style::new().add_modifier(Modifier::BOLD),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `resolve` is asserted via the one observable difference between the no-color
    // path and the colored path: ASCII vs Unicode glyphs/borders. (dark and light
    // share an adaptive palette by design, so they are deliberately the same.)

    #[test]
    fn no_color_flag_forces_the_ascii_path_over_any_theme() {
        assert_eq!(Theme::resolve(ColorTheme::Dark, true).glyphs.available, "*");
        assert_eq!(
            Theme::resolve(ColorTheme::Light, true).glyphs.available,
            "*"
        );
    }

    #[test]
    fn none_theme_value_uses_the_ascii_path() {
        let t = Theme::resolve(ColorTheme::None, false);
        assert_eq!(t.glyphs.available, "*");
        assert_eq!(t.border_set.top_left, "+");
    }

    #[test]
    fn colored_themes_use_unicode() {
        let t = Theme::resolve(ColorTheme::Dark, false);
        assert_eq!(t.glyphs.available, "●");
        assert_eq!(t.border_set.top_left, border::ROUNDED.top_left);
        assert_eq!(
            Theme::resolve(ColorTheme::Light, false).glyphs.available,
            "●"
        );
    }

    #[test]
    fn none_uses_ascii_glyphs_and_colored_uses_unicode() {
        assert_eq!(Theme::none().glyphs.available, "*");
        assert_eq!(Theme::dark().glyphs.available, "●");
        assert_eq!(Theme::none().glyphs.pointer, "> ");
        assert_eq!(Theme::dark().glyphs.pointer, "▶ ");
    }

    #[test]
    fn colored_theme_assigns_a_distinct_hue_per_meaning() {
        let t = Theme::dark();
        // Each meaning gets its own suitable color, and no two collide.
        assert_eq!(t.accent.fg, Some(Color::Yellow)); // pending updates
        assert_eq!(t.success.fg, Some(Color::Green)); // available
        assert_eq!(t.selected.fg, Some(Color::Cyan)); // focused row
        assert_ne!(t.accent.fg, t.success.fg);
        assert_ne!(t.accent.fg, t.selected.fg);
        assert_ne!(t.success.fg, t.selected.fg);
    }

    #[test]
    fn no_color_path_carries_no_hues() {
        let t = Theme::none();
        assert_eq!(t.accent.fg, None);
        assert_eq!(t.success.fg, None);
        assert_eq!(t.selected.fg, None);
    }
}
