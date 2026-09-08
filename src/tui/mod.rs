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
pub use app::{Curve, Placeholder, Scenario};
use input::{
    Action, map_cleanup_key, map_dashboard_key, map_exec_key, map_filter_key, map_history_key,
    map_log_key, map_overlaps_key, map_packages_key,
};
use theme::Theme;

/// How long the loop waits for a key before ticking the spinner and checking
/// the scan channel. Also the spinner's frame rate.
const TICK: Duration = Duration::from_millis(120);

/// The redraw interval while a scan is running. The climbing counts are the
/// only thing on screen that changes between frames, and at 120ms they moved
/// in ~12 visible steps; at 30ms they read as a counter rather than a
/// slideshow. It only applies while scanning, so an idle dashboard still
/// wakes eight times a second rather than thirty.
const SCAN_TICK: Duration = Duration::from_millis(30);

/// What a scan reports back as it runs.
enum ScanEvent {
    /// A partial result: everything up to `phase` is real, the rest is not
    /// on screen yet.
    Progress(Box<ScanResult>),
    Done(Box<anyhow::Result<ScanResult>>),
}

/// A scan running on a worker thread; results arrive on the channel.
struct ScanJob(mpsc::Receiver<ScanEvent>);

fn spawn_scan(config: Config) -> ScanJob {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let runner = SystemCommandRunner::new(config.scan.provider_timeout_secs);
        let _ = tx.send(ScanEvent::Done(Box::new(scanner::scan_and_store(
            &runner, &config,
        ))));
    });
    ScanJob(rx)
}

/// **Temporary — delete once the cold-start option is chosen (#TBD).**
///
/// Replays a cold start against the cached scan so the candidate options can
/// be felt in the real TUI instead of compared as screenshots. The producer
/// is fake; everything downstream — the render, the key gating, the event
/// loop — is the real thing, which is the point.
///
/// Delays are what this machine actually measures: `pacman -Qi` 0.33s,
/// `checkupdates` 1.08s, `paru -Qua` 1.64s, `flatpak remote-ls` 1.30s.
/// Which placeholder the demo should replay, or `None` for no demo.
///
/// Any of the three flags starts it: asking for a curve or a scenario without
/// naming a placeholder used to leave the demo unstarted, which opened the
/// ordinary cached dashboard and looked exactly like the flag doing nothing.
fn demo_requested(
    placeholder: Option<app::Placeholder>,
    curve: Option<app::Curve>,
    scenario: Option<app::Scenario>,
) -> Option<app::Placeholder> {
    if placeholder.is_none() && curve.is_none() && scenario.is_none() {
        return None;
    }
    // The climbing count is the one being tuned, so it is what a bare
    // `--demo-curve` or `--demo-scenario` means.
    Some(placeholder.unwrap_or(app::Placeholder::Count))
}

/// What the demo opens on, before any lane reports.
///
/// `last` and `count` need the previous scan behind them — one shows those
/// numbers, the other climbs toward them, and neither has anything to work
/// with if they are cleared first. The rest open with the sources detected
/// and nothing counted.
fn demo_opening(placeholder: app::Placeholder, cache: &ScanResult) -> ScanResult {
    if matches!(
        placeholder,
        app::Placeholder::Last | app::Placeholder::Count
    ) {
        return cache.clone();
    }
    let mut sources_only = cache.clone();
    sources_only.packages.clear();
    sources_only.updates.clear();
    sources_only
}

/// The cache a scenario starts from — the numbers the climb aims at.
fn demo_cache(scenario: app::Scenario, cache: ScanResult) -> ScanResult {
    use crate::model::SourceId;
    let mut cache = cache;
    match scenario {
        // A handful of packages: the climb has almost nowhere to go, which is
        // the case where a counter is worth less than a plain number.
        app::Scenario::Small => {
            cache.packages.truncate(4);
            cache.updates.truncate(1);
        }
        // The machine lost packages since the last scan, so the estimate
        // overshoots and the truth lands below it.
        app::Scenario::Shrunk => {
            let keep = cache.packages.len() / 4;
            cache
                .packages
                .retain(|p| p.source_id != SourceId::pacman() || keep > 0);
            cache.packages.truncate(keep.max(1));
        }
        _ => {}
    }
    cache
}

fn spawn_demo(scenario: app::Scenario, cached: ScanResult) -> ScanJob {
    use crate::model::SourceId;
    use std::time::Duration;

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        // Nothing is counted until a source's own commands come back, so a
        // partial scan is the sources with their numbers withheld.
        let staged = |landed: &[SourceId]| {
            let mut s = cached.clone();
            for source in s.sources.iter_mut() {
                if !landed.contains(&source.id) {
                    source.last_scanned = None;
                }
            }
            s.packages.retain(|p| landed.contains(&p.source_id));
            s.updates.retain(|u| landed.contains(&u.source_id));
            Box::new(s)
        };
        let send = |event| tx.send(event).is_ok();
        // Measured on this machine: checkupdates 1.08s, flatpak remote-ls
        // 1.30s, paru -Qua 1.64s. The lanes run concurrently, so those are
        // arrival times, not a sum. A stalled network runs to the provider
        // timeout instead, which is many times the climb's ramp.
        let lanes: Vec<(u64, SourceId)> = match scenario {
            app::Scenario::Slow => vec![
                (6000, SourceId::pacman()),
                (8000, SourceId::flatpak()),
                (10000, SourceId::aur()),
            ],
            _ => vec![
                (1080, SourceId::pacman()),
                (1300, SourceId::flatpak()),
                (1640, SourceId::aur()),
            ],
        };

        if !send(ScanEvent::Progress(staged(&[]))) {
            return;
        }
        // A scan that dies: the rows keep whatever they had, and the
        // dashboard says the scan failed rather than pretending it finished.
        if scenario == app::Scenario::Fail {
            std::thread::sleep(Duration::from_millis(2500));
            let _ = send(ScanEvent::Done(Box::new(Err(anyhow::anyhow!(
                "checkupdates: could not reach any mirror"
            )))));
            return;
        }
        let mut landed: Vec<SourceId> = Vec::new();
        let mut elapsed = 0u64;
        for (at, id) in lanes {
            std::thread::sleep(Duration::from_millis(at - elapsed));
            elapsed = at;
            landed.push(id);
            if !send(ScanEvent::Progress(staged(&landed))) {
                return;
            }
        }
        let _ = send(ScanEvent::Done(Box::new(Ok(cached))));
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
    demo_coldstart: Option<app::Placeholder>,
    demo_curve: Option<app::Curve>,
    demo_scenario: Option<app::Scenario>,
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
        // Temporary: replay a cold start against the cached scan, switching
        // what an uncounted cell shows. Deleted with `spawn_demo` once one
        // placeholder is chosen.
        if let Some(placeholder) = demo_requested(demo_coldstart, demo_curve, demo_scenario) {
            let scenario = demo_scenario.unwrap_or_default();
            let Some(cache) = cached else {
                anyhow::bail!("--demo-coldstart needs a cached scan: run `paclens status` first");
            };
            let cache = demo_cache(scenario, cache);
            let opts = app::AppOptions::from_config(config, executor::sudo::detect());
            // `last` and `count` open holding the previous scan's numbers —
            // one shows them, the other climbs toward them, and neither has
            // anything to work with if they are cleared first. The rest open
            // with the sources detected and nothing counted. Never on the
            // splash, which is the point of the exercise.
            let opening = demo_opening(placeholder, &cache);
            let mut app = App::new(opening, theme, opts);
            app.set_placeholder(placeholder);
            app.set_curve(demo_curve.unwrap_or_default());
            app.set_scanning(true);
            return run_loop(
                &mut terminal,
                &mut app,
                Some(spawn_demo(scenario, cache)),
                config,
            );
        }

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
                Ok(ScanEvent::Progress(scan)) => app.replace_scan_partial(*scan),
                Ok(ScanEvent::Done(result)) => {
                    match *result {
                        Ok(scan) => app.replace_scan(scan), // clears the scanning flag
                        Err(err) => {
                            // The lanes that never reported still have not:
                            // their rows keep last scan's numbers and say so,
                            // rather than turning green on a scan that died.
                            app.fail_scan();
                            app.set_flash(format!("scan failed: {err:#}"));
                        }
                    }
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
        let tick = if app.is_scanning() { SCAN_TICK } else { TICK };
        if !event::poll(tick).context("failed to poll for terminal events")? {
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
            // The package list needs only what is installed, which lands
            // long before any update check comes back.
            Action::OpenPackages => match app.dash_source().map(|s| s.id.clone()) {
                // A source that has reported can be opened while the others
                // are still out — its own list is complete.
                Some(id) if app.source_counted(&id) => app.open_packages(),
                Some(id) => app.set_flash(format!("still counting {id}…")),
                None => {}
            },
            // Both read analyzer output over the full package set; opening
            // them mid-scan would show an answer derived from half a machine.
            Action::OpenOverlaps => {
                if app.scan_settled() {
                    app.open_overlaps()
                } else {
                    app.set_flash("still scanning — overlaps need the whole package list");
                }
            }
            Action::OpenCleanup => {
                if app.scan_settled() {
                    app.open_cleanup()
                } else {
                    app.set_flash("still scanning — cleanup needs the whole package list");
                }
            }
            Action::OpenHistory => app.open_history(),
            Action::Back => match app.screen() {
                app::Screen::Overlaps => app.close_overlaps(),
                app::Screen::Cleanup => app.back_cleanup(),
                app::Screen::History => app.back_history(),
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
            // A source that has reported can be toggled: its own count is
            // final, whatever the other lanes are still doing.
            Action::Toggle => match app.dash_source().map(|s| s.id.clone()) {
                Some(id) if app.source_counted(&id) => app.toggle_selected(),
                Some(id) => app.set_flash(format!("still counting {id}…")),
                None => {}
            },
            Action::Execute => {
                // Enter runs directly — the plan view is the confirmation
                // (user decision 2026-07-08); pacman/sudo prompt for
                // themselves inside the pty console.
                let plan = app.update_plan();
                let tool = app.privilege_tool();
                if !app.scan_settled() {
                    // A plan built from half a scan is a plan for a machine
                    // that does not exist. "You're up to date" here would be
                    // a confident wrong answer (design §3).
                    app.set_flash("still checking for updates — the plan is not complete yet");
                } else if plan.is_empty() {
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
            Action::FocusLeft => app.focus_left(),
            Action::FocusRight => app.focus_right(),
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
            InputMode::History => map_history_key(key),
            InputMode::PackageFilter => map_filter_key(key),
            InputMode::LogView => map_log_key(key),
            InputMode::Exec => map_exec_key(key, exec_done),
        }),
        _ => Ok(Action::Ignore),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Package, SourceId};

    fn cache_with(packages: usize) -> ScanResult {
        let mut scan = ScanResult::empty();
        scan.packages = (0..packages)
            .map(|i| Package {
                name: format!("pkg{i}"),
                version: "1".to_string(),
                source_id: SourceId::pacman(),
                install_reason: crate::model::InstallReason::Explicit,
                size_bytes: None,
                description: None,
                depends_on: Vec::new(),
                required_by: Vec::new(),
                optional_deps: Vec::new(),
                provides: Vec::new(),
                runtime: false,
                scope: None,
                foreign: false,
                signed: true,
                packager: None,
            })
            .collect();
        scan
    }

    #[test]
    fn any_demo_flag_starts_the_replay() {
        // The bug: `--demo-curve smooth` alone left the placeholder unset, so
        // the demo never ran and the ordinary cached dashboard opened —
        // which looks exactly like the flag doing nothing.
        assert_eq!(
            demo_requested(None, Some(app::Curve::Smooth), None),
            Some(app::Placeholder::Count),
            "a curve on its own means the climbing count"
        );
        assert_eq!(
            demo_requested(None, None, Some(app::Scenario::Slow)),
            Some(app::Placeholder::Count),
            "so does a scenario"
        );
        assert_eq!(
            demo_requested(Some(app::Placeholder::Blank), None, None),
            Some(app::Placeholder::Blank),
            "an explicit placeholder wins"
        );
        assert_eq!(demo_requested(None, None, None), None, "no flags, no demo");
    }

    #[test]
    fn the_number_placeholders_open_holding_the_previous_scan() {
        // The bug this pins: opening with the packages cleared left `count`
        // with nothing to climb toward, so the cells stayed blank.
        let cache = cache_with(3);
        for placeholder in [app::Placeholder::Last, app::Placeholder::Count] {
            let opening = demo_opening(placeholder, &cache);
            assert_eq!(
                opening.packages.len(),
                3,
                "{placeholder:?} needs the previous numbers"
            );
        }
        for placeholder in [
            app::Placeholder::Blank,
            app::Placeholder::Spinner,
            app::Placeholder::Dots,
        ] {
            let opening = demo_opening(placeholder, &cache);
            assert!(
                opening.packages.is_empty(),
                "{placeholder:?} shows no numbers before a lane reports"
            );
        }
    }
}
