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
    /// Update screen / result view → open the newest update log in $PAGER.
    OpenLog,
    /// Dashboard → focus the sources pane (←/h).
    FocusLeft,
    /// Dashboard → focus the pending-updates pane (→/l).
    FocusRight,
    /// Log viewer → close it.
    CloseLog,
    /// Execution console → forward these bytes to the running command.
    ExecInput(char),
    /// Execution console, finished → hand off to the result view.
    ExecDismiss,
    /// Dashboard → open the selected source's package list.
    OpenPackages,
    /// Package list → jump a page of rows.
    NextPage,
    PrevPage,
    /// Package list → focus the fuzzy-filter input.
    StartFilter,
    /// Package list → toggle the why side pane.
    ToggleWhy,
    /// Filter input → append a character.
    FilterChar(char),
    FilterBackspace,
    /// Filter input → keep the query, refocus the list.
    FilterAccept,
    /// Filter input → drop the query.
    FilterCancel,
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
        KeyCode::Left | KeyCode::Char('h') => Action::FocusLeft,
        KeyCode::Right | KeyCode::Char('l') => Action::FocusRight,
        KeyCode::Char('r') => Action::Refresh,
        KeyCode::Char('u') => Action::OpenUpdates,
        KeyCode::Enter => Action::OpenPackages,
        _ => Action::Ignore,
    }
}

/// Package list key map: nav (incl. paging), filter, why pane, back, quit.
pub fn map_packages_key(key: KeyEvent) -> Action {
    if is_quit(&key) {
        return Action::Quit;
    }
    match key.code {
        KeyCode::Down | KeyCode::Char('j') => Action::Next,
        KeyCode::Up | KeyCode::Char('k') => Action::Prev,
        KeyCode::PageDown => Action::NextPage,
        KeyCode::PageUp => Action::PrevPage,
        KeyCode::Char('/') => Action::StartFilter,
        KeyCode::Char('w') | KeyCode::Char('W') => Action::ToggleWhy,
        KeyCode::Esc => Action::Back,
        _ => Action::Ignore,
    }
}

/// Filter input key map: printable chars type into the query (`q` included —
/// only Ctrl-C quits here), Backspace deletes, Enter applies, Esc cancels.
pub fn map_filter_key(key: KeyEvent) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
        return Action::Quit;
    }
    match key.code {
        KeyCode::Esc => Action::FilterCancel,
        KeyCode::Enter => Action::FilterAccept,
        KeyCode::Backspace => Action::FilterBackspace,
        KeyCode::Char(c) => Action::FilterChar(c),
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
        KeyCode::Char('l') | KeyCode::Char('L') => Action::OpenLog,
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

/// Inline log viewer: scroll like a pager, `q`/`Esc` close (as in `less` —
/// a viewer's q must not quit the whole app). Ctrl-C still quits.
pub fn map_log_key(key: KeyEvent) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
        return Action::Quit;
    }
    match key.code {
        KeyCode::Down | KeyCode::Char('j') => Action::Next,
        KeyCode::Up | KeyCode::Char('k') => Action::Prev,
        KeyCode::PageDown => Action::NextPage,
        KeyCode::PageUp => Action::PrevPage,
        KeyCode::Esc | KeyCode::Char('q') => Action::CloseLog,
        _ => Action::Ignore,
    }
}

/// Execution console key map. While the command runs, printable keys and
/// Enter are forwarded to its stdin (sudo password, pacman prompts); when
/// `done`, any key hands off to the result view. Ctrl-C still quits.
pub fn map_exec_key(key: KeyEvent, done: bool) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
        return Action::Quit;
    }
    if done {
        return Action::ExecDismiss;
    }
    match key.code {
        KeyCode::Char(c) => Action::ExecInput(c),
        KeyCode::Enter => Action::ExecInput('\n'),
        _ => Action::Ignore,
    }
}

/// Result view key map: any key dismisses ("press any key to continue");
/// Ctrl-C still quits.
pub fn map_result_key(key: KeyEvent) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
        return Action::Quit;
    }
    match key.code {
        KeyCode::Char('l') | KeyCode::Char('L') => Action::OpenLog,
        _ => Action::DismissResult,
    }
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
        assert_eq!(
            map_dashboard_key(plain(KeyCode::Enter)),
            Action::OpenPackages
        );
        // Update-only keys are ignored on the dashboard.
        assert_eq!(map_dashboard_key(plain(KeyCode::Char(' '))), Action::Ignore);
        assert_eq!(map_dashboard_key(plain(KeyCode::Esc)), Action::Ignore);
    }

    #[test]
    fn dashboard_pane_focus_keys() {
        for left in [KeyCode::Left, KeyCode::Char('h')] {
            assert_eq!(map_dashboard_key(plain(left)), Action::FocusLeft);
        }
        for right in [KeyCode::Right, KeyCode::Char('l')] {
            assert_eq!(map_dashboard_key(plain(right)), Action::FocusRight);
        }
    }

    #[test]
    fn package_list_keys() {
        assert_eq!(map_packages_key(plain(KeyCode::Char('j'))), Action::Next);
        assert_eq!(map_packages_key(plain(KeyCode::PageDown)), Action::NextPage);
        assert_eq!(map_packages_key(plain(KeyCode::PageUp)), Action::PrevPage);
        assert_eq!(
            map_packages_key(plain(KeyCode::Char('/'))),
            Action::StartFilter
        );
        assert_eq!(
            map_packages_key(plain(KeyCode::Char('w'))),
            Action::ToggleWhy
        );
        assert_eq!(
            map_packages_key(plain(KeyCode::Char('W'))),
            Action::ToggleWhy
        );
        assert_eq!(map_packages_key(plain(KeyCode::Esc)), Action::Back);
        assert_eq!(map_packages_key(plain(KeyCode::Char('q'))), Action::Quit);
        assert_eq!(map_packages_key(plain(KeyCode::Enter)), Action::Ignore);
    }

    #[test]
    fn filter_input_types_instead_of_quitting() {
        assert_eq!(
            map_filter_key(plain(KeyCode::Char('q'))),
            Action::FilterChar('q') // q must TYPE, not quit
        );
        assert_eq!(
            map_filter_key(plain(KeyCode::Char('/'))),
            Action::FilterChar('/')
        );
        assert_eq!(
            map_filter_key(plain(KeyCode::Backspace)),
            Action::FilterBackspace
        );
        assert_eq!(map_filter_key(plain(KeyCode::Enter)), Action::FilterAccept);
        assert_eq!(map_filter_key(plain(KeyCode::Esc)), Action::FilterCancel);
        assert_eq!(
            map_filter_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Action::Quit
        );
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
    fn result_view_dismisses_on_any_key_except_log() {
        for any in [KeyCode::Enter, KeyCode::Esc, KeyCode::Char('q')] {
            assert_eq!(map_result_key(plain(any)), Action::DismissResult);
        }
        assert_eq!(map_result_key(plain(KeyCode::Char('l'))), Action::OpenLog);
    }

    #[test]
    fn update_screen_opens_the_log_on_l() {
        assert_eq!(map_update_key(plain(KeyCode::Char('l'))), Action::OpenLog);
        assert_eq!(map_update_key(plain(KeyCode::Char('L'))), Action::OpenLog);
    }

    #[test]
    fn ctrl_c_quits_from_modal_and_result_too() {
        for map in [map_confirm_key, map_result_key, map_log_key] {
            assert_eq!(
                map(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
                Action::Quit
            );
        }
        assert_eq!(
            map_exec_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL), false),
            Action::Quit
        );
    }

    #[test]
    fn log_viewer_scrolls_and_closes_like_a_pager() {
        assert_eq!(map_log_key(plain(KeyCode::Char('j'))), Action::Next);
        assert_eq!(map_log_key(plain(KeyCode::PageUp)), Action::PrevPage);
        assert_eq!(map_log_key(plain(KeyCode::Char('q'))), Action::CloseLog);
        assert_eq!(map_log_key(plain(KeyCode::Esc)), Action::CloseLog);
    }

    #[test]
    fn exec_console_forwards_keys_while_running_and_dismisses_when_done() {
        assert_eq!(
            map_exec_key(plain(KeyCode::Char('y')), false),
            Action::ExecInput('y')
        );
        assert_eq!(
            map_exec_key(plain(KeyCode::Enter), false),
            Action::ExecInput('\n')
        );
        // q while running is INPUT, not quit — it may be part of a password.
        assert_eq!(
            map_exec_key(plain(KeyCode::Char('q')), false),
            Action::ExecInput('q')
        );
        assert_eq!(map_exec_key(plain(KeyCode::Esc), false), Action::Ignore);
        assert_eq!(
            map_exec_key(plain(KeyCode::Char('x')), true),
            Action::ExecDismiss
        );
        assert_eq!(
            map_exec_key(plain(KeyCode::Enter), true),
            Action::ExecDismiss
        );
    }
}
