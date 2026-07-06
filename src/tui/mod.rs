//! TUI shell: open on the cached scan instantly, scan in the background, and
//! run the multi-screen event loop.
//!
//! Scans run on a worker thread and land through an mpsc channel; the loop
//! polls for key events with a short timeout so the spinner animates and the
//! UI never blocks on a scan (spec §10.1, roadmap v0.0.9). Scan failures flash
//! inline — they never take the TUI down.

mod app;
mod draw;
mod exec;
mod input;
mod theme;

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Context;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

use crate::config::Config;
use crate::executor::{self, InteractiveRunner, UpdateLog};
use crate::model::{ActionPlan, ScanResult};
use crate::providers::SystemCommandRunner;
use crate::scanner;

use app::{App, InputMode};
use input::{
    Action, map_confirm_key, map_dashboard_key, map_exec_key, map_filter_key, map_log_key,
    map_packages_key, map_result_key, map_update_key,
};
use theme::Theme;

/// How long the loop waits for a key before ticking the spinner and checking
/// the scan channel. Also the spinner's frame rate.
const TICK: Duration = Duration::from_millis(120);

/// A scan running on a worker thread; the result arrives on the channel.
struct ScanJob(mpsc::Receiver<anyhow::Result<ScanResult>>);

fn spawn_scan(config: Config) -> ScanJob {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(scanner::scan_and_store(&SystemCommandRunner, &config));
    });
    ScanJob(rx)
}

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

    let mut terminal = ratatui::init();
    let result = (|| {
        // Warm cache → open on it instantly. Cold or --refresh → open on a
        // splash and let the background scan fill it in.
        let cached = if refresh {
            None
        } else {
            scanner::load_cached(config, config_path)?
        };
        let start_scanning = cached.is_none();
        let mut app = App::new(
            cached.unwrap_or_else(ScanResult::empty),
            theme,
            app::AppOptions::from_config(config, executor::sudo::detect()),
        );
        let mut job = None;
        if start_scanning {
            app.set_scanning(true);
            job = Some(spawn_scan(config.clone()));
        }
        run_loop(&mut terminal, &mut app, job, config, config_path)
    })();
    ratatui::restore();
    result
}

fn run_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    mut job: Option<ScanJob>,
    config: &Config,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    let mut exec_session: Option<exec::ExecSession> = None;
    loop {
        terminal
            .draw(|frame| draw::draw(frame, app))
            .context("failed to draw the terminal frame")?;

        // Land a finished background scan, if any.
        if let Some(active) = &job {
            match active.0.try_recv() {
                Ok(Ok(scan)) => {
                    app.replace_scan(scan); // also clears the scanning flag
                    job = None;
                }
                Ok(Err(err)) => {
                    app.set_scanning(false);
                    app.set_flash(format!("scan failed: {err:#}"));
                    job = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    app.set_scanning(false);
                    app.set_flash("scan worker vanished — press r to retry");
                    job = None;
                }
            }
        }

        // Land streamed execution output, if a session is running.
        if let Some(session) = &exec_session {
            loop {
                match session.events.try_recv() {
                    Ok(exec::ExecEvent::Line(line)) => app.exec_push_line(line),
                    Ok(exec::ExecEvent::Done(report)) => {
                        app.exec_push_line(String::new());
                        app.exec_push_line("done — press any key to continue".to_string());
                        app.exec_finish(report);
                        exec_session = None;
                        break;
                    }
                    Ok(exec::ExecEvent::Failed(err)) => {
                        app.take_exec_report();
                        app.set_flash(format!("update failed: {err}"));
                        exec_session = None;
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        exec_session = None;
                        break;
                    }
                }
            }
        }

        // Wait for a key with a timeout so the spinner keeps animating.
        if !event::poll(TICK).context("failed to poll for terminal events")? {
            app.tick();
            continue;
        }
        let action = read_action(app.input_mode(), app.exec_is_done())?;
        // A key press dismisses any flash; `Confirm` sets a fresh one below.
        app.clear_flash();

        match action {
            Action::Quit => return Ok(()),
            Action::Next => {
                if app.log_view().is_some() {
                    app.log_scroll(1);
                } else {
                    app.on_next();
                }
            }
            Action::Prev => {
                if app.log_view().is_some() {
                    app.log_scroll(-1);
                } else {
                    app.on_prev();
                }
            }
            Action::Refresh => {
                // Background re-scan; the dashboard shows the spinner while
                // the current data stays interactive.
                if job.is_none() {
                    app.set_scanning(true);
                    job = Some(spawn_scan(config.clone()));
                }
            }
            Action::OpenUpdates => app.goto_updates(),
            Action::OpenPackages => app.open_packages(),
            Action::Back => match app.screen() {
                app::Screen::Packages => app.back_packages(),
                _ => app.back_to_dashboard(),
            },
            Action::NextPage => {
                if app.log_view().is_some() {
                    app.log_scroll(20);
                } else {
                    app.pkg_move(20);
                }
            }
            Action::PrevPage => {
                if app.log_view().is_some() {
                    app.log_scroll(-20);
                } else {
                    app.pkg_move(-20);
                }
            }
            Action::StartFilter => app.start_filter(),
            Action::ToggleWhy => app.toggle_why(),
            Action::FilterChar(c) => app.filter_push(c),
            Action::FilterBackspace => app.filter_pop(),
            Action::FilterAccept => app.filter_accept(),
            Action::FilterCancel => app.filter_cancel(),
            Action::Toggle => app.toggle_selected(),
            Action::Confirm => {
                // Open the modal only when something would actually run; the
                // pipeline's confirm step (P4) is never a no-op.
                let plan = app.update_plan();
                if plan.is_empty() {
                    app.set_flash("nothing selected to update");
                } else if executor::executable_steps(&plan, app.privilege_tool()) == 0 {
                    app.set_flash("no privilege tool found (sudo/doas/pkexec) — cannot update");
                } else {
                    app.open_confirm();
                }
            }
            Action::CloseConfirm => app.close_confirm(),
            Action::Execute => {
                app.close_confirm();
                let plan = app.update_plan();
                let tool = app.privilege_tool();
                if exec::tool_supports_pipe(tool) {
                    // Inline console: output streams into the update window;
                    // typed keys (sudo password, pacman prompts) forward to
                    // the running command. No terminal suspend.
                    app.start_exec();
                    exec_session = Some(exec::start(plan, tool.map(String::from), None));
                } else {
                    // doas/pkexec cannot read a piped stdin — suspend for them.
                    match run_plan_suspended(terminal, &plan, tool) {
                        Ok(report) => {
                            let runner = SystemCommandRunner;
                            let scan = scanner::load_or_scan(&runner, config, true, config_path)?;
                            app.replace_scan(scan);
                            app.set_report(report);
                        }
                        Err(err) => app.set_flash(format!("update failed: {err:#}")),
                    }
                }
            }
            Action::DismissResult => app.dismiss_report(),
            Action::FocusLeft => app.focus_sources(),
            Action::FocusRight => app.focus_updates(),
            Action::ExecInput(c) => {
                if let Some(session) = &exec_session {
                    let mut buf = [0u8; 4];
                    session.forward(c.encode_utf8(&mut buf).as_bytes().to_vec());
                }
            }
            Action::ExecDismiss => {
                if let Some(report) = app.take_exec_report() {
                    // Refresh in the background; the result view shows now.
                    if job.is_none() {
                        app.set_scanning(true);
                        job = Some(spawn_scan(config.clone()));
                    }
                    app.set_report(report);
                }
            }
            Action::CloseLog => app.close_log(),
            Action::OpenLog => match UpdateLog::latest_path() {
                Some(path) => match std::fs::read_to_string(&path) {
                    Ok(text) => app.open_log(text),
                    Err(err) => app.set_flash(format!("could not read the log: {err}")),
                },
                None => app.set_flash("no update log yet — nothing has been executed"),
            },
            Action::Ignore => {}
        }
    }
}

/// Suspend the TUI (raw mode off, main screen back), run `f` in the raw
/// terminal, then restore. Restore always runs, even when `f` failed.
fn with_suspended<T>(
    terminal: &mut DefaultTerminal,
    f: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    disable_raw_mode().context("failed to disable raw mode")?;
    ratatui::crossterm::execute!(std::io::stdout(), LeaveAlternateScreen)
        .context("failed to leave the alternate screen")?;

    let result = f();

    enable_raw_mode().context("failed to re-enable raw mode")?;
    ratatui::crossterm::execute!(std::io::stdout(), EnterAlternateScreen)
        .context("failed to re-enter the alternate screen")?;
    terminal
        .clear()
        .context("failed to clear the restored terminal")?;
    result
}

/// Run the plan in the raw terminal (the user sees flatpak/pacman output and
/// the sudo prompt directly — spec §13.2), then restore the TUI.
fn run_plan_suspended(
    terminal: &mut DefaultTerminal,
    plan: &ActionPlan,
    tool: Option<&str>,
) -> anyhow::Result<executor::ExecutionReport> {
    with_suspended(terminal, || {
        let mut log = UpdateLog::open_default()?;
        Ok(executor::execute(plan, &InteractiveRunner, &mut log, tool))
    })
}

/// Read the pending key press and map it with the active mode's key map.
fn read_action(mode: InputMode, exec_done: bool) -> anyhow::Result<Action> {
    match event::read().context("failed to read a terminal event")? {
        Event::Key(key) if key.kind == KeyEventKind::Press => Ok(match mode {
            InputMode::Dashboard => map_dashboard_key(key),
            InputMode::Updates => map_update_key(key),
            InputMode::Confirm => map_confirm_key(key),
            InputMode::Result => map_result_key(key),
            InputMode::Packages => map_packages_key(key),
            InputMode::PackageFilter => map_filter_key(key),
            InputMode::LogView => map_log_key(key),
            InputMode::Exec => map_exec_key(key, exec_done),
        }),
        _ => Ok(Action::Ignore),
    }
}
