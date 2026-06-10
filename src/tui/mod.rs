//! TUI shell: load a scan, build the dashboard `App`, and run the event loop.
//!
//! The scan is synchronous — `load_or_scan` runs once before the loop (a warm
//! cache makes this instant). The async scan-with-spinner path (spec §10.1) is
//! deferred to the v0.0.9 usability pass.

mod app;
mod draw;
mod input;
mod theme;

use std::path::Path;

use anyhow::Context;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};

use crate::config::Config;
use crate::providers::SystemCommandRunner;
use crate::scanner;

use app::App;
use input::{Action, map_key};
use theme::Theme;

/// Open the TUI, run the event loop, and restore the terminal on exit.
///
/// `ratatui::init` installs a panic hook that restores the terminal, so a panic
/// inside the loop will not leave the user's terminal in raw mode.
pub fn run(
    config: &Config,
    refresh: bool,
    config_path: Option<&Path>,
    no_color: bool,
) -> anyhow::Result<()> {
    let theme = Theme::resolve(config.general.color_theme(), no_color);
    let runner = SystemCommandRunner;
    let scan = scanner::load_or_scan(&runner, config, refresh, config_path)?;
    let mut app = App::new(scan, theme);

    let mut terminal = ratatui::init();
    let result = run_loop(&mut terminal, &mut app, &runner, config, config_path);
    ratatui::restore();
    result
}

fn run_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    runner: &SystemCommandRunner,
    config: &Config,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    loop {
        terminal
            .draw(|frame| draw::draw(frame, app))
            .context("failed to draw the terminal frame")?;

        match read_action()? {
            Action::Quit => return Ok(()),
            Action::Next => app.select_next(),
            Action::Prev => app.select_prev(),
            Action::Refresh => {
                // Synchronous re-scan; blocks briefly. Async spinner is v0.0.9.
                let scan = scanner::load_or_scan(runner, config, true, config_path)?;
                app.replace_scan(scan);
            }
            Action::Ignore => {}
        }
    }
}

/// Block for the next key press and map it to an [`Action`]. Non-key and
/// non-press events are ignored.
fn read_action() -> anyhow::Result<Action> {
    match event::read().context("failed to read a terminal event")? {
        Event::Key(key) if key.kind == KeyEventKind::Press => Ok(map_key(key)),
        _ => Ok(Action::Ignore),
    }
}
