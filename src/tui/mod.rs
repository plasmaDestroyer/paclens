//! TUI shell. For v0.0.1 this is an empty frame that proves the terminal
//! setup works and quits cleanly. Screens and widgets arrive from v0.0.4.

use anyhow::Context;
use ratatui::DefaultTerminal;
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::{Block, Borders};

/// Open the TUI, run the event loop, and restore the terminal on exit.
///
/// `ratatui::init` installs a panic hook that restores the terminal, so a panic
/// inside the loop will not leave the user's terminal in raw mode.
pub fn run() -> anyhow::Result<()> {
    let mut terminal = ratatui::init();
    let result = run_loop(&mut terminal);
    ratatui::restore();
    result
}

fn run_loop(terminal: &mut DefaultTerminal) -> anyhow::Result<()> {
    loop {
        terminal
            .draw(draw)
            .context("failed to draw the terminal frame")?;
        if read_should_quit()? {
            return Ok(());
        }
    }
}

fn draw(frame: &mut Frame) {
    let block = Block::default().title("paclens").borders(Borders::ALL);
    frame.render_widget(block, frame.area());
}

fn read_should_quit() -> anyhow::Result<bool> {
    match event::read().context("failed to read a terminal event")? {
        Event::Key(key) if key.kind == KeyEventKind::Press => Ok(is_quit(key)),
        _ => Ok(false),
    }
}

fn is_quit(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('q'))
        || (key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn q_quits() {
        assert!(is_quit(key(KeyCode::Char('q'), KeyModifiers::NONE)));
    }

    #[test]
    fn ctrl_c_quits() {
        assert!(is_quit(key(KeyCode::Char('c'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn plain_c_does_not_quit() {
        assert!(!is_quit(key(KeyCode::Char('c'), KeyModifiers::NONE)));
    }

    #[test]
    fn other_keys_do_not_quit() {
        assert!(!is_quit(key(KeyCode::Char('x'), KeyModifiers::NONE)));
        assert!(!is_quit(key(KeyCode::Esc, KeyModifiers::NONE)));
    }
}
