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
    /// Package list → return to the dashboard (unwinds why/filter first).
    Back,
    /// Dashboard → toggle the selected source in/out of the update plan.
    Toggle,
    /// Dashboard → run the plan in the pty console (the dashboard IS the
    /// plan view; pacman/sudo ask their own questions in the console).
    Execute,
    /// Update screen / dashboard → open the newest update log inline.
    OpenLog,
    /// Dashboard → focus the sources pane (←/h).
    FocusLeft,
    /// Dashboard → focus the pending-updates pane (→/l).
    FocusRight,
    /// Log viewer → close it.
    CloseLog,
    /// Execution console → pass this key through to the running command.
    ExecKey(KeyEvent),
    /// Execution console, finished → back to the dashboard.
    ExecDismiss,
    /// Dashboard → open the selected source's package list.
    OpenPackages,
    /// Dashboard → open the overlap screen.
    OpenOverlaps,
    /// Dashboard → open the cleanup screen.
    OpenCleanup,
    /// Package list → jump a page of rows.
    NextPage,
    PrevPage,
    /// Package list → focus the fuzzy-filter input.
    StartFilter,
    /// Package list → next sort mode (updates → reason → name → size).
    CycleSort,
    /// Package list → toggle the why side pane.
    ToggleWhy,
    /// Flip the migration report's direction (overlap screen, v0.4).
    FlipDirection,
    /// Run the open migration report's copy plan (overlap screen, v0.5).
    RunMigration,
    /// Remove the source side after a verified migration (v0.5).
    RemoveSource,
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

/// Dashboard key map: nav, toggle, run, inspect, refresh, quit. The dashboard
/// owns the update flow — Space toggles the selected source and Enter runs the
/// plan, because updating is what you open paclens to do (user decision
/// 2026-08-24). `u` stays as an alias. Drilling into a source's package list
/// moved to `i` (info); `d` was avoided because it reads as "delete" in a
/// package tool.
pub fn map_dashboard_key(key: KeyEvent) -> Action {
    if is_quit(&key) {
        return Action::Quit;
    }
    match key.code {
        KeyCode::Down | KeyCode::Char('j') => Action::Next,
        KeyCode::Up | KeyCode::Char('k') => Action::Prev,
        KeyCode::Left | KeyCode::Char('h') => Action::FocusLeft,
        KeyCode::Right | KeyCode::Char('l') => Action::FocusRight,
        KeyCode::Char(' ') => Action::Toggle,
        KeyCode::Char('r') => Action::Refresh,
        KeyCode::Enter | KeyCode::Char('u') => Action::Execute,
        KeyCode::Char('i') | KeyCode::Char('I') => Action::OpenPackages,
        KeyCode::Char('o') | KeyCode::Char('O') => Action::OpenOverlaps,
        KeyCode::Char('c') | KeyCode::Char('C') => Action::OpenCleanup,
        KeyCode::Char('L') => Action::OpenLog,
        _ => Action::Ignore,
    }
}

/// Overlap screen key map: nav, back, quit. Advisory only — no actions.
pub fn map_overlaps_key(key: KeyEvent) -> Action {
    if is_quit(&key) {
        return Action::Quit;
    }
    match key.code {
        KeyCode::Down | KeyCode::Char('j') => Action::Next,
        KeyCode::Up | KeyCode::Char('k') => Action::Prev,
        // Enter/w toggles the migration advisory pane (v0.4).
        KeyCode::Enter | KeyCode::Char('w') | KeyCode::Char('W') => Action::ToggleWhy,
        KeyCode::Char('d') | KeyCode::Char('D') => Action::FlipDirection,
        // v0.5 execution: x runs the copy plan; R (deliberate shift) removes
        // the source after the user verified the target.
        KeyCode::Char('x') | KeyCode::Char('X') => Action::RunMigration,
        KeyCode::Char('R') => Action::RemoveSource,
        KeyCode::Char('L') => Action::OpenLog,
        KeyCode::Esc => Action::Back,
        _ => Action::Ignore,
    }
}

/// Cleanup screen key map: nav, Enter/w opens the selected orphan's why
/// panel (the roadmap rule: understand before removing), back, quit.
/// Advisory only — no actions.
pub fn map_cleanup_key(key: KeyEvent) -> Action {
    if is_quit(&key) {
        return Action::Quit;
    }
    match key.code {
        KeyCode::Down | KeyCode::Char('j') => Action::Next,
        KeyCode::Up | KeyCode::Char('k') => Action::Prev,
        KeyCode::Enter | KeyCode::Char('w') | KeyCode::Char('W') => Action::ToggleWhy,
        KeyCode::Esc => Action::Back,
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
        KeyCode::Char('s') | KeyCode::Char('S') => Action::CycleSort,
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

/// Execution console key map. While the command runs, EVERY key passes
/// through to the pty — Ctrl-C included (it interrupts the child, exactly
/// like a terminal; quitting the TUI mid-update is not on offer). When
/// `done`, any key returns to the dashboard; Ctrl-C quits again.
pub fn map_exec_key(key: KeyEvent, done: bool) -> Action {
    if done {
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            return Action::Quit;
        }
        return Action::ExecDismiss;
    }
    Action::ExecKey(key)
}

/// Encode a key press as the bytes a terminal would send — the passthrough
/// half of the console. Returns `None` for keys with no byte encoding.
pub fn encode_key(key: KeyEvent) -> Option<Vec<u8>> {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        // Ctrl-A..Ctrl-Z → 0x01..0x1a (Ctrl-C = 0x03 interrupts the child).
        if let KeyCode::Char(c) = key.code {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() {
                return Some(vec![c as u8 - b'a' + 1]);
            }
        }
        return None;
    }
    match key.code {
        KeyCode::Char(c) => {
            let mut buf = [0u8; 4];
            Some(c.encode_utf8(&mut buf).as_bytes().to_vec())
        }
        KeyCode::Enter => Some(b"\r".to_vec()),
        KeyCode::Backspace => Some(vec![0x7f]),
        KeyCode::Tab => Some(b"\t".to_vec()),
        KeyCode::Esc => Some(vec![0x1b]),
        KeyCode::Up => Some(b"\x1b[A".to_vec()),
        KeyCode::Down => Some(b"\x1b[B".to_vec()),
        KeyCode::Right => Some(b"\x1b[C".to_vec()),
        KeyCode::Left => Some(b"\x1b[D".to_vec()),
        KeyCode::Home => Some(b"\x1b[H".to_vec()),
        KeyCode::End => Some(b"\x1b[F".to_vec()),
        KeyCode::Delete => Some(b"\x1b[3~".to_vec()),
        KeyCode::PageUp => Some(b"\x1b[5~".to_vec()),
        KeyCode::PageDown => Some(b"\x1b[6~".to_vec()),
        _ => None,
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
    fn dashboard_quits_on_q_and_ctrl_c() {
        assert_eq!(map_dashboard_key(plain(KeyCode::Char('q'))), Action::Quit);
        assert_eq!(
            map_dashboard_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Action::Quit
        );
    }

    #[test]
    fn dashboard_navigates_with_arrows_and_jk() {
        assert_eq!(map_dashboard_key(plain(KeyCode::Down)), Action::Next);
        assert_eq!(map_dashboard_key(plain(KeyCode::Char('j'))), Action::Next);
        assert_eq!(map_dashboard_key(plain(KeyCode::Up)), Action::Prev);
        assert_eq!(map_dashboard_key(plain(KeyCode::Char('k'))), Action::Prev);
    }

    #[test]
    fn dashboard_specific_keys() {
        assert_eq!(
            map_dashboard_key(plain(KeyCode::Char('r'))),
            Action::Refresh
        );
        // The dashboard owns the update flow: space toggles, enter runs.
        assert_eq!(map_dashboard_key(plain(KeyCode::Char(' '))), Action::Toggle);
        assert_eq!(map_dashboard_key(plain(KeyCode::Enter)), Action::Execute);
        // u stays as an alias for enter.
        assert_eq!(
            map_dashboard_key(plain(KeyCode::Char('u'))),
            Action::Execute
        );
        // The package list moved off enter onto i (info).
        assert_eq!(
            map_dashboard_key(plain(KeyCode::Char('i'))),
            Action::OpenPackages
        );
        assert_eq!(
            map_dashboard_key(plain(KeyCode::Char('I'))),
            Action::OpenPackages
        );
        assert_eq!(
            map_dashboard_key(plain(KeyCode::Char('L'))),
            Action::OpenLog
        );
        assert_eq!(
            map_dashboard_key(plain(KeyCode::Char('o'))),
            Action::OpenOverlaps
        );
        assert_eq!(
            map_dashboard_key(plain(KeyCode::Char('c'))),
            Action::OpenCleanup
        );
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
            map_packages_key(plain(KeyCode::Char('s'))),
            Action::CycleSort
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
    fn overlap_screen_keys() {
        assert_eq!(map_overlaps_key(plain(KeyCode::Char('j'))), Action::Next);
        assert_eq!(map_overlaps_key(plain(KeyCode::Up)), Action::Prev);
        assert_eq!(map_overlaps_key(plain(KeyCode::Esc)), Action::Back);
        assert_eq!(map_overlaps_key(plain(KeyCode::Char('q'))), Action::Quit);
        assert_eq!(map_overlaps_key(plain(KeyCode::Enter)), Action::ToggleWhy);
        assert_eq!(
            map_overlaps_key(plain(KeyCode::Char('w'))),
            Action::ToggleWhy
        );
        assert_eq!(
            map_overlaps_key(plain(KeyCode::Char('d'))),
            Action::FlipDirection
        );
        assert_eq!(
            map_overlaps_key(plain(KeyCode::Char('x'))),
            Action::RunMigration
        );
        assert_eq!(
            map_overlaps_key(plain(KeyCode::Char('R'))),
            Action::RemoveSource
        );
        assert_eq!(map_overlaps_key(plain(KeyCode::Char('L'))), Action::OpenLog);
        // Lowercase r stays unmapped — removal is a deliberate shift-key.
        assert_eq!(map_overlaps_key(plain(KeyCode::Char('r'))), Action::Ignore);
    }

    #[test]
    fn cleanup_screen_keys() {
        assert_eq!(map_cleanup_key(plain(KeyCode::Char('j'))), Action::Next);
        assert_eq!(map_cleanup_key(plain(KeyCode::Enter)), Action::ToggleWhy);
        assert_eq!(
            map_cleanup_key(plain(KeyCode::Char('w'))),
            Action::ToggleWhy
        );
        assert_eq!(map_cleanup_key(plain(KeyCode::Esc)), Action::Back);
        assert_eq!(map_cleanup_key(plain(KeyCode::Char('q'))), Action::Quit);
    }

    #[test]
    fn unmapped_keys_are_ignored() {
        assert_eq!(map_dashboard_key(plain(KeyCode::Char('x'))), Action::Ignore);
    }

    #[test]
    fn ctrl_c_quits_from_the_log_viewer_and_the_finished_console() {
        assert_eq!(
            map_log_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Action::Quit
        );
        assert_eq!(
            map_exec_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL), true),
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
    fn exec_console_passes_every_key_through_while_running() {
        for code in [
            KeyCode::Char('y'),
            KeyCode::Char('q'), // may be part of a password — never quits
            KeyCode::Enter,
            KeyCode::Esc,
        ] {
            assert_eq!(
                map_exec_key(plain(code), false),
                Action::ExecKey(plain(code))
            );
        }
        // Ctrl-C too: it must interrupt the CHILD, not the TUI.
        let ctrl_c = key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(map_exec_key(ctrl_c, false), Action::ExecKey(ctrl_c));
        // Done: any key returns to the dashboard.
        assert_eq!(
            map_exec_key(plain(KeyCode::Char('x')), true),
            Action::ExecDismiss
        );
        assert_eq!(
            map_exec_key(plain(KeyCode::Enter), true),
            Action::ExecDismiss
        );
    }

    #[test]
    fn encode_key_speaks_terminal() {
        assert_eq!(encode_key(plain(KeyCode::Char('a'))), Some(b"a".to_vec()));
        assert_eq!(
            encode_key(plain(KeyCode::Char('ß'))),
            Some("ß".as_bytes().to_vec())
        );
        assert_eq!(encode_key(plain(KeyCode::Enter)), Some(b"\r".to_vec()));
        assert_eq!(encode_key(plain(KeyCode::Backspace)), Some(vec![0x7f]));
        assert_eq!(encode_key(plain(KeyCode::Esc)), Some(vec![0x1b]));
        assert_eq!(encode_key(plain(KeyCode::Up)), Some(b"\x1b[A".to_vec()));
        assert_eq!(
            encode_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(vec![0x03])
        );
        assert_eq!(
            encode_key(key(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            Some(vec![0x04])
        );
        assert_eq!(encode_key(plain(KeyCode::F(5))), None);
    }
}
