//! TUI shell: load a scan, build the `App`, and run the multi-screen event loop.
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

use app::{App, Screen};
use input::{Action, map_dashboard_key, map_update_key};
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

        let action = read_action(app.screen())?;
        // A key press dismisses any flash; `Confirm` sets a fresh one below.
        app.clear_flash();

        match action {
            Action::Quit => return Ok(()),
            Action::Next => app.on_next(),
            Action::Prev => app.on_prev(),
            Action::Refresh => {
                // Synchronous re-scan; blocks briefly. Async spinner is v0.0.9.
                let scan = scanner::load_or_scan(runner, config, true, config_path)?;
                app.replace_scan(scan);
            }
            Action::OpenUpdates => app.goto_updates(),
            Action::Back => app.back_to_dashboard(),
            Action::Toggle => app.toggle_selected(),
            Action::Confirm => app.set_flash("execution arrives in v0.0.6"),
            Action::Ignore => {}
        }
    }
}

/// Block for the next key press and map it with the active screen's key map.
fn read_action(screen: Screen) -> anyhow::Result<Action> {
    match event::read().context("failed to read a terminal event")? {
        Event::Key(key) if key.kind == KeyEventKind::Press => Ok(match screen {
            Screen::Dashboard => map_dashboard_key(key),
            Screen::Updates => map_update_key(key),
        }),
        _ => Ok(Action::Ignore),
    }
}
