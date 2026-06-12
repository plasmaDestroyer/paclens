//! Keyboard input → semantic action, one pure mapping per screen. Pure so they
//! are unit-testable without a terminal; the event loop applies the returned
//! [`Action`] to the `App` (the loop is the only mutator).

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A semantic action produced by a key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    /// Move the active screen's cursor forward.
    Next,
    /// Move the active screen's cursor back.
    Prev,
    Refresh,
    /// Dashboard → open the update screen.
    OpenUpdates,
    /// Update screen → return to the dashboard.
    Back,
    /// Update screen → toggle the selected source.
    Toggle,
    /// Update screen → confirm the plan (opens the confirm modal).
    Confirm,
    /// Confirm modal → run the plan.
    Execute,
    /// Confirm modal → close it without running anything.
    CloseConfirm,
    /// Result view → back to the (refreshed) plan view.
    DismissResult,
    Ignore,
}

fn is_quit(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('q'))
        || (key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')))
}

/// Dashboard key map: nav, refresh, open updates, quit.
pub fn map_dashboard_key(key: KeyEvent) -> Action {
    if is_quit(&key) {
        return Action::Quit;
    }
    match key.code {
        KeyCode::Down | KeyCode::Char('j') => Action::Next,
        KeyCode::Up | KeyCode::Char('k') => Action::Prev,
        KeyCode::Char('r') => Action::Refresh,
        KeyCode::Char('u') => Action::OpenUpdates,
        _ => Action::Ignore,
    }
}

/// Update screen key map: nav, toggle, confirm, back, quit.
pub fn map_update_key(key: KeyEvent) -> Action {
    if is_quit(&key) {
        return Action::Quit;
    }
    match key.code {
        KeyCode::Down | KeyCode::Char('j') => Action::Next,
        KeyCode::Up | KeyCode::Char('k') => Action::Prev,
        KeyCode::Char(' ') => Action::Toggle,
        KeyCode::Enter => Action::Confirm,
        KeyCode::Esc => Action::Back,
        _ => Action::Ignore,
    }
}

/// Confirm modal key map: only an explicit `y` runs the plan; everything that
/// reads as "no" (`n`, `Esc`, even `q`) just closes the modal — a quit-key
/// slip while a sudo-free update is one keypress away should never exit the
/// app. Ctrl-C still quits.
pub fn map_confirm_key(key: KeyEvent) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
        return Action::Quit;
    }
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => Action::Execute,
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q') => {
            Action::CloseConfirm
        }
        _ => Action::Ignore,
    }
}

/// Result view key map: any key dismisses ("press any key to continue");
/// Ctrl-C still quits.
pub fn map_result_key(key: KeyEvent) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
        return Action::Quit;
    }
    Action::DismissResult
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn plain(code: KeyCode) -> KeyEvent {
        key(code, KeyModifiers::NONE)
    }

    #[test]
    fn both_screens_quit_on_q_and_ctrl_c() {
        for map in [map_dashboard_key, map_update_key] {
            assert_eq!(map(plain(KeyCode::Char('q'))), Action::Quit);
            assert_eq!(
                map(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
                Action::Quit
            );
        }
    }

    #[test]
    fn both_screens_navigate_with_arrows_and_jk() {
        for map in [map_dashboard_key, map_update_key] {
            assert_eq!(map(plain(KeyCode::Down)), Action::Next);
            assert_eq!(map(plain(KeyCode::Char('j'))), Action::Next);
            assert_eq!(map(plain(KeyCode::Up)), Action::Prev);
            assert_eq!(map(plain(KeyCode::Char('k'))), Action::Prev);
        }
    }

    #[test]
    fn dashboard_specific_keys() {
        assert_eq!(
            map_dashboard_key(plain(KeyCode::Char('r'))),
            Action::Refresh
        );
        assert_eq!(
            map_dashboard_key(plain(KeyCode::Char('u'))),
            Action::OpenUpdates
        );
        // Update-only keys are ignored on the dashboard.
        assert_eq!(map_dashboard_key(plain(KeyCode::Char(' '))), Action::Ignore);
        assert_eq!(map_dashboard_key(plain(KeyCode::Esc)), Action::Ignore);
        assert_eq!(map_dashboard_key(plain(KeyCode::Enter)), Action::Ignore);
    }

    #[test]
    fn update_specific_keys() {
        assert_eq!(map_update_key(plain(KeyCode::Char(' '))), Action::Toggle);
        assert_eq!(map_update_key(plain(KeyCode::Enter)), Action::Confirm);
        assert_eq!(map_update_key(plain(KeyCode::Esc)), Action::Back);
        // Dashboard-only keys are ignored on the update screen.
        assert_eq!(map_update_key(plain(KeyCode::Char('u'))), Action::Ignore);
        assert_eq!(map_update_key(plain(KeyCode::Char('r'))), Action::Ignore);
    }

    #[test]
    fn unmapped_keys_are_ignored() {
        assert_eq!(map_dashboard_key(plain(KeyCode::Char('x'))), Action::Ignore);
        assert_eq!(map_update_key(plain(KeyCode::Char('x'))), Action::Ignore);
    }

    #[test]
    fn confirm_modal_runs_only_on_an_explicit_y() {
        assert_eq!(map_confirm_key(plain(KeyCode::Char('y'))), Action::Execute);
        assert_eq!(map_confirm_key(plain(KeyCode::Char('Y'))), Action::Execute);
        // Enter must NOT execute — it is what opened the modal.
        assert_eq!(map_confirm_key(plain(KeyCode::Enter)), Action::Ignore);
        assert_eq!(map_confirm_key(plain(KeyCode::Char('x'))), Action::Ignore);
    }

    #[test]
    fn confirm_modal_closes_on_anything_that_reads_as_no() {
        for no in [
            KeyCode::Char('n'),
            KeyCode::Char('N'),
            KeyCode::Esc,
            KeyCode::Char('q'),
        ] {
            assert_eq!(map_confirm_key(plain(no)), Action::CloseConfirm);
        }
    }

    #[test]
    fn result_view_dismisses_on_any_key() {
        for any in [KeyCode::Enter, KeyCode::Esc, KeyCode::Char('q')] {
            assert_eq!(map_result_key(plain(any)), Action::DismissResult);
        }
    }

    #[test]
    fn ctrl_c_quits_from_modal_and_result_too() {
        for map in [map_confirm_key, map_result_key] {
            assert_eq!(
                map(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
                Action::Quit
            );
        }
    }
}
