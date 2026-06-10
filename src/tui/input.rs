//! Keyboard input → semantic action. The mapping is a pure function so it is
//! unit-testable without a terminal; the event loop applies the returned
//! [`Action`] to the `App` (the loop and handlers are the only mutators).

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A semantic action produced by a key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Next,
    Prev,
    Refresh,
    Ignore,
}

/// Map a key press to an [`Action`]. `q` and `Ctrl-C` quit; arrows and `j`/`k`
/// move the selection; `r` refreshes; everything else is ignored.
pub fn map_key(key: KeyEvent) -> Action {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Char('c') if ctrl => Action::Quit,
        KeyCode::Down | KeyCode::Char('j') => Action::Next,
        KeyCode::Up | KeyCode::Char('k') => Action::Prev,
        KeyCode::Char('r') => Action::Refresh,
        _ => Action::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn q_and_ctrl_c_quit() {
        assert_eq!(
            map_key(key(KeyCode::Char('q'), KeyModifiers::NONE)),
            Action::Quit
        );
        assert_eq!(
            map_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Action::Quit
        );
    }

    #[test]
    fn plain_c_is_ignored() {
        assert_eq!(
            map_key(key(KeyCode::Char('c'), KeyModifiers::NONE)),
            Action::Ignore
        );
    }

    #[test]
    fn down_and_j_move_next() {
        assert_eq!(
            map_key(key(KeyCode::Down, KeyModifiers::NONE)),
            Action::Next
        );
        assert_eq!(
            map_key(key(KeyCode::Char('j'), KeyModifiers::NONE)),
            Action::Next
        );
    }

    #[test]
    fn up_and_k_move_prev() {
        assert_eq!(map_key(key(KeyCode::Up, KeyModifiers::NONE)), Action::Prev);
        assert_eq!(
            map_key(key(KeyCode::Char('k'), KeyModifiers::NONE)),
            Action::Prev
        );
    }

    #[test]
    fn r_refreshes() {
        assert_eq!(
            map_key(key(KeyCode::Char('r'), KeyModifiers::NONE)),
            Action::Refresh
        );
    }

    #[test]
    fn unmapped_keys_are_ignored() {
        assert_eq!(
            map_key(key(KeyCode::Char('x'), KeyModifiers::NONE)),
            Action::Ignore
        );
        assert_eq!(
            map_key(key(KeyCode::Esc, KeyModifiers::NONE)),
            Action::Ignore
        );
        assert_eq!(
            map_key(key(KeyCode::Enter, KeyModifiers::NONE)),
            Action::Ignore
        );
    }
}
