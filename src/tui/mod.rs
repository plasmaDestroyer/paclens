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
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

use crate::config::Config;
use crate::executor::{self, InteractiveRunner, UpdateLog};
use crate::model::ActionPlan;
use crate::providers::SystemCommandRunner;
use crate::scanner;

use app::{App, InputMode};
use input::{Action, map_confirm_key, map_dashboard_key, map_result_key, map_update_key};
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

        let action = read_action(app.input_mode())?;
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
            Action::Confirm => {
                // Open the modal only when something would actually run; the
                // pipeline's confirm step (P4) is never a no-op.
                let plan = app.update_plan();
                if plan.is_empty() {
                    app.set_flash("nothing selected to update");
                } else if executor::executable_steps(&plan) == 0 {
                    app.set_flash("nothing runnable yet — pacman & system updates arrive in v0.1");
                } else {
                    app.open_confirm();
                }
            }
            Action::CloseConfirm => app.close_confirm(),
            Action::Execute => {
                app.close_confirm();
                let plan = app.update_plan();
                match run_plan_suspended(terminal, &plan) {
                    Ok(report) => {
                        // Refresh first so the plan view behind the result is
                        // already current when the report is dismissed.
                        let scan = scanner::load_or_scan(runner, config, true, config_path)?;
                        app.replace_scan(scan);
                        app.set_report(report);
                    }
                    Err(err) => app.set_flash(format!("update failed: {err:#}")),
                }
            }
            Action::DismissResult => app.dismiss_report(),
            Action::Ignore => {}
        }
    }
}

/// Suspend the TUI, run the plan in the raw terminal (the user sees flatpak's
/// own output and prompts directly — spec §13.2), then restore the TUI.
/// Restore always runs, even when opening the update log failed.
fn run_plan_suspended(
    terminal: &mut DefaultTerminal,
    plan: &ActionPlan,
) -> anyhow::Result<executor::ExecutionReport> {
    disable_raw_mode().context("failed to disable raw mode")?;
    ratatui::crossterm::execute!(std::io::stdout(), LeaveAlternateScreen)
        .context("failed to leave the alternate screen")?;

    let result = (|| {
        let mut log = UpdateLog::open_default()?;
        Ok(executor::execute(plan, &InteractiveRunner, &mut log))
    })();

    enable_raw_mode().context("failed to re-enable raw mode")?;
    ratatui::crossterm::execute!(std::io::stdout(), EnterAlternateScreen)
        .context("failed to re-enter the alternate screen")?;
    terminal
        .clear()
        .context("failed to clear the restored terminal")?;
    result
}

/// Block for the next key press and map it with the active mode's key map.
fn read_action(mode: InputMode) -> anyhow::Result<Action> {
    match event::read().context("failed to read a terminal event")? {
        Event::Key(key) if key.kind == KeyEventKind::Press => Ok(match mode {
            InputMode::Dashboard => map_dashboard_key(key),
            InputMode::Updates => map_update_key(key),
            InputMode::Confirm => map_confirm_key(key),
            InputMode::Result => map_result_key(key),
        }),
        _ => Ok(Action::Ignore),
    }
}
