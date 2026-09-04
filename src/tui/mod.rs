//! TUI shell: open on the cached scan instantly, scan in the background, and
//! run the multi-screen event loop.
//!
//! Scans run on a worker thread and land through an mpsc channel; the loop
//! polls for key events with a short timeout so the spinner animates and the
//! UI never blocks on a scan (roadmap v0.0.9). Scan failures flash
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

use crate::config::Config;
use crate::executor::{self, UpdateLog};
use crate::model::ScanResult;
use crate::planner;
use crate::providers::SystemCommandRunner;
use crate::scanner;

use app::{App, InputMode};
use input::{
    Action, map_cleanup_key, map_dashboard_key, map_exec_key, map_filter_key, map_log_key,
    map_overlaps_key, map_packages_key,
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
        let runner = SystemCommandRunner::new(config.scan.provider_timeout_secs);
        let _ = tx.send(scanner::scan_and_store(&runner, &config));
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
        run_loop(&mut terminal, &mut app, job, config)
    })();
    ratatui::restore();
    result
}

fn run_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    mut job: Option<ScanJob>,
    config: &Config,
) -> anyhow::Result<()> {
    let mut exec_session: Option<exec::ExecSession> = None;
    loop {
        // The scrolloff math needs the package table's viewport; only the
        // loop may mutate, so it feeds the size in before every draw.
        if let Ok(size) = terminal.size() {
            app.set_pkg_viewport(draw::pkg_body_rows(size.height));
        }
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
                    Ok(exec::ExecEvent::Bytes(bytes)) => app.exec_feed(&bytes),
                    Ok(exec::ExecEvent::Done(report)) => {
                        app.exec_feed(b"\r\n\x1b[2mdone - press any key to continue\x1b[0m\r\n");
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
        // A key press dismisses any flash; handlers set fresh ones below.
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
            Action::OpenPackages => app.open_packages(),
            Action::OpenOverlaps => app.open_overlaps(),
            Action::OpenCleanup => app.open_cleanup(),
            Action::Back => match app.screen() {
                app::Screen::Overlaps => app.close_overlaps(),
                app::Screen::Cleanup => app.back_cleanup(),
                _ => app.back_packages(),
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
            Action::CycleSort => app.cycle_sort(),
            Action::ToggleWhy => app.toggle_why(),
            Action::FlipDirection => app.flip_migrate_direction(),
            Action::FilterChar(c) => app.filter_push(c),
            Action::FilterBackspace => app.filter_pop(),
            Action::FilterAccept => app.filter_accept(),
            Action::FilterCancel => app.filter_cancel(),
            Action::Toggle => app.toggle_selected(),
            Action::Execute => {
                // Enter runs directly — the plan view is the confirmation
                // (user decision 2026-07-08); pacman/sudo prompt for
                // themselves inside the pty console.
                let plan = app.update_plan();
                let tool = app.privilege_tool();
                if plan.is_empty() {
                    app.set_flash(if app.total_updates() == 0 {
                        "you're up to date"
                    } else {
                        "nothing selected to update"
                    });
                } else if executor::executable_steps(&plan, tool) == 0 {
                    app.set_flash("no privilege tool found (sudo/doas/pkexec) — cannot update");
                } else {
                    let (rows, cols) = terminal
                        .size()
                        .map(|s| draw::exec_pty_size(s.width, s.height))
                        .unwrap_or((24, 80));
                    app.start_exec(rows, cols, app::ExecKind::Update);
                    exec_session = Some(exec::start(
                        plan,
                        tool.map(String::from),
                        (rows, cols),
                        None,
                        app.sudo_loop(),
                    ));
                }
            }
            Action::RunMigration => {
                if !app.is_migrate_open() {
                    app.set_flash("open the migration report first (enter)");
                } else if let (Some(report), Some(home)) = (
                    app.migration_report(),
                    directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()),
                ) {
                    // The candidate exists whenever the report does.
                    let plan = app
                        .selected_overlap()
                        .map(|c| {
                            let backup = planner::migration_backup_dir(&home, c);
                            let plan = planner::plan_migration(&report, c, &home, &backup);
                            let removal = planner::plan_removal(&report, c);
                            (plan, removal, backup)
                        })
                        .filter(|(plan, _, _)| !plan.is_empty());
                    match plan {
                        None => app.set_flash("nothing to copy — the source side has no data"),
                        Some((plan, removal, backup)) => {
                            app.stage_removal(removal.map(|plan| app::StagedRemoval {
                                plan,
                                backup: backup.display().to_string(),
                            }));
                            let (rows, cols) = terminal
                                .size()
                                .map(|s| draw::exec_pty_size(s.width, s.height))
                                .unwrap_or((24, 80));
                            app.start_exec(rows, cols, app::ExecKind::Migrate);
                            // Copy steps never escalate — no tool.
                            exec_session = Some(exec::start(plan, None, (rows, cols), None, None));
                        }
                    }
                }
            }
            Action::RemoveSource => match app.armed_removal() {
                None => app.set_flash("nothing to remove — run a migration first (x)"),
                Some(staged) => {
                    let tool = app.privilege_tool();
                    if staged.plan.requires_sudo && tool.is_none() {
                        app.set_flash("no privilege tool found (sudo/doas/pkexec) — cannot remove");
                    } else {
                        let plan = staged.plan.clone();
                        let (rows, cols) = terminal
                            .size()
                            .map(|s| draw::exec_pty_size(s.width, s.height))
                            .unwrap_or((24, 80));
                        app.start_exec(rows, cols, app::ExecKind::Removal);
                        exec_session = Some(exec::start(
                            plan,
                            tool.map(String::from),
                            (rows, cols),
                            None,
                            app.sudo_loop(),
                        ));
                    }
                }
            },
            Action::FocusLeft => app.focus_sources(),
            Action::FocusRight => app.focus_updates(),
            Action::ExecKey(key) => {
                if let (Some(session), Some(bytes)) = (&exec_session, input::encode_key(key)) {
                    session.forward(bytes);
                }
            }
            Action::ExecDismiss => {
                if let Some(report) = app.take_exec_report() {
                    // Every console lands on a refreshing screen — no result
                    // modal (user decision 2026-07-08). Migrations return to
                    // the overlap screen for the verify/remove step.
                    if job.is_none() {
                        app.set_scanning(true);
                        job = Some(spawn_scan(config.clone()));
                    }
                    match app.exec_kind() {
                        app::ExecKind::Update => app.finish_update(&report),
                        app::ExecKind::Migrate => app.finish_migration(&report),
                        app::ExecKind::Removal => app.finish_removal(&report),
                    }
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
            Action::ResizePane(delta) => app.resize_pane(delta),
            Action::Ignore => {}
        }
    }
}

/// Read the pending key press and map it with the active mode's key map.
fn read_action(mode: InputMode, exec_done: bool) -> anyhow::Result<Action> {
    match event::read().context("failed to read a terminal event")? {
        Event::Key(key) if key.kind == KeyEventKind::Press => Ok(match mode {
            InputMode::Dashboard => map_dashboard_key(key),
            InputMode::Packages => map_packages_key(key),
            InputMode::Overlaps => map_overlaps_key(key),
            InputMode::Cleanup => map_cleanup_key(key),
            InputMode::PackageFilter => map_filter_key(key),
            InputMode::LogView => map_log_key(key),
            InputMode::Exec => map_exec_key(key, exec_done),
        }),
        _ => Ok(Action::Ignore),
    }
}
