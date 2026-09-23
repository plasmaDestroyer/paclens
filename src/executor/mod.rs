//! Execution layer: runs pre-built `ActionPlan`s (the *execute* step of P4).
//!
//! Contract (design §6): the executor never decides what to do — all
//! decisions come from the user via the TUI/CLI. It logs every command before
//! and after execution, and reports exit codes without interpretation (the
//! renderers interpret). Steps that declare themselves privileged run through
//! the detected privilege tool ([`sudo`], design §11); with no tool on PATH they
//! come back as `Skipped` with an explicit reason, never silently dropped.
//!
//! Commands run with inherited stdio in the raw terminal — the user sees and
//! interacts with the tool's own output directly (design §11). The
//! [`StepRunner`] trait is the testing seam, mirroring the providers'
//! `CommandRunner`.

mod log;
pub mod sudo;

use std::path::PathBuf;

use anyhow::Context;

use crate::model::{ActionPlan, ActionStep, SourceId};

pub use log::UpdateLog;

/// Runs one command, returning its exit code (`None` when the process was
/// terminated by a signal). Injectable for testing.
pub trait StepRunner {
    /// Run attached to the terminal: the user sees the tool's own output and
    /// answers its prompts directly (design §11).
    fn run(&self, argv: &[String]) -> anyhow::Result<Option<i32>>;

    /// Run detached from the terminal, returning what it printed.
    ///
    /// Only for steps that declared `interactive: false`. Used by
    /// [`execute_concurrent`], where several steps run at once and sharing one
    /// terminal would shred all of their output.
    fn run_captured(&self, argv: &[String]) -> anyhow::Result<(Option<i32>, String)>;
}

/// The production runner: spawns the command attached to the real terminal.
pub struct InteractiveRunner;

impl StepRunner for InteractiveRunner {
    fn run(&self, argv: &[String]) -> anyhow::Result<Option<i32>> {
        let (program, args) = argv.split_first().context("empty command")?;
        let status = std::process::Command::new(program)
            .args(args)
            .status()
            .with_context(|| format!("failed to launch `{program}`"))?;
        Ok(status.code())
    }

    /// stdin is `/dev/null` on purpose: a step that declared itself
    /// non-interactive and then asks a question gets EOF and fails, rather
    /// than hanging the run on a prompt nobody can see. stdout and stderr come
    /// back concatenated in that order — `Command::output` collects them
    /// separately, so their interleaving is lost; the exact ordering of a
    /// background step's two streams has not been worth a pty for.
    fn run_captured(&self, argv: &[String]) -> anyhow::Result<(Option<i32>, String)> {
        let (program, args) = argv.split_first().context("empty command")?;
        let out = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .with_context(|| format!("failed to launch `{program}`"))?;
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        Ok((out.status.code(), text))
    }
}

/// How one step ended. `Failed` carries the uninterpreted detail (exit code,
/// signal, or launch error); `Skipped` carries the reason it never ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepStatus {
    Succeeded,
    Failed { detail: String },
    Skipped { reason: String },
}

/// One step's outcome, in plan order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepReport {
    pub source_id: SourceId,
    /// The step's display name — see `ActionStep::label`.
    pub label: String,
    /// How many packages/apps the step targeted.
    pub targets: usize,
    pub status: StepStatus,
}

/// The outcome of executing a whole plan, plus where it was logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionReport {
    pub steps: Vec<StepReport>,
    pub log_path: PathBuf,
}

impl ExecutionReport {
    /// Steps that actually ran (succeeded or failed).
    pub fn executed(&self) -> usize {
        self.steps.len() - self.skipped()
    }
    pub fn succeeded(&self) -> usize {
        self.count(|s| matches!(s, StepStatus::Succeeded))
    }
    pub fn failed(&self) -> usize {
        self.count(|s| matches!(s, StepStatus::Failed { .. }))
    }
    pub fn skipped(&self) -> usize {
        self.count(|s| matches!(s, StepStatus::Skipped { .. }))
    }

    fn count(&self, pred: impl Fn(&StepStatus) -> bool) -> usize {
        self.steps.iter().filter(|s| pred(&s.status)).count()
    }
}

/// Does this step need privilege escalation?
///
/// The step says so; nothing here reads its source id (design §13,
/// 2026-09-07). This used to be "privileged unless the id is flatpak-user or
/// aur", which made root the default for every source that did not exist yet —
/// cargo, npm, pipx and rustup are all unprivileged and would all have been
/// wrapped in sudo by a rule nobody remembered to edit. Declaring it at the
/// planner means forgetting produces a missing prompt, not a command run as
/// root.
pub fn needs_privilege(step: &ActionStep) -> bool {
    step.privileged
}

/// Why a step cannot run, or `None` if it is executable. Since v0.1.0 the only
/// blocker is a privileged step with no privilege tool on PATH (design §11:
/// "show error, do not proceed with privileged operations").
pub fn skip_reason(step: &ActionStep, tool: Option<&str>) -> Option<&'static str> {
    if needs_privilege(step) && tool.is_none() {
        Some("no privilege tool found (sudo/doas/pkexec)")
    } else {
        None
    }
}

/// The exact argv that will run: the plan's bare command with the privilege
/// tool prepended when the step needs it. Renderers show this same value
/// before anything runs (P1) — display and execution can never diverge.
pub fn effective_command(step: &ActionStep, tool: Option<&str>) -> Vec<String> {
    match (needs_privilege(step), tool) {
        (true, Some(tool)) => std::iter::once(tool.to_string())
            .chain(step.command.iter().cloned())
            .collect(),
        _ => step.command.clone(),
    }
}

/// Can this step run beside the others under `update --parallel`?
///
/// Three conditions, and all three are the step's own declaration rather than
/// anything read out of its id:
///
/// - it asks the user nothing (`interactive`), so taking its terminal away
///   costs only the live output;
/// - it needs no privilege — design §11 is explicit that paclens never runs a
///   privileged process in the background, and `sudo` reads its password from
///   `/dev/tty` rather than stdin, so a backgrounded one would prompt into a
///   terminal three other steps are writing to;
/// - it is not being skipped for want of a privilege tool.
///
/// The CLI preview and the executor both call this, so what the plan promises
/// and what runs cannot diverge (the same contract as [`effective_command`]).
pub fn runs_in_background(step: &ActionStep, tool: Option<&str>) -> bool {
    !step.interactive && !needs_privilege(step) && skip_reason(step, tool).is_none()
}

/// How many steps would actually run.
pub fn executable_steps(plan: &ActionPlan, tool: Option<&str>) -> usize {
    plan.steps
        .iter()
        .filter(|s| skip_reason(s, tool).is_none())
        .count()
}

/// `"3 flatpaks"` / `"1 package"` — the unit the source itself uses
/// ("flatpaks", not "apps": runtime updates count too).
pub fn target_noun(source_id: &SourceId, count: usize) -> String {
    let s = source_id.as_str();
    let unit = match (s == "flatpak", s == "aur", count) {
        (true, _, 1) => "flatpak",
        (true, _, _) => "flatpaks",
        (_, true, 1) => "AUR package",
        (_, true, _) => "AUR packages",
        (_, _, 1) => "package",
        (_, _, _) => "packages",
    };
    format!("{count} {unit}")
}

/// One step's outcome, plus everything still to be said about it: the log
/// lines in plan order, and whatever a background step printed while the
/// terminal belonged to someone else.
struct StepOutcome {
    status: StepStatus,
    log_lines: Vec<String>,
    output: Option<String>,
}

/// Run one step and classify the result — shared by both execution paths, so
/// the status, the log wording and the tracing events cannot drift apart.
///
/// `capture` is the caller's answer to "does this step have the terminal?",
/// never the step's: [`execute`] gives every step the terminal, while
/// [`execute_concurrent`] only gives it to the ones that declared they need it.
fn run_step(
    step: &ActionStep,
    argv: &[String],
    runner: &impl StepRunner,
    capture: bool,
) -> StepOutcome {
    let cmd = argv.join(" ");
    let doing = match step.kind {
        crate::model::ActionKind::Update => format!(
            "running update ({})",
            target_noun(&step.source_id, step.targets.len())
        ),
        crate::model::ActionKind::Migrate => format!("copying {}", step.targets.join(", ")),
        crate::model::ActionKind::Remove => format!("removing {}", step.targets.join(", ")),
    };
    let mut log_lines = vec![format!("{}: {doing}", step.label)];
    tracing::info!(source = %step.source_id, command = %cmd, "executing update step");

    let (result, output) = if capture {
        match runner.run_captured(argv) {
            Ok((code, text)) => (Ok(code), Some(text)),
            Err(err) => (Err(err), None),
        }
    } else {
        (runner.run(argv), None)
    };

    let status = match result {
        Ok(Some(0)) => {
            log_lines.push(format!("{}: completed, exit 0", step.label));
            StepStatus::Succeeded
        }
        Ok(Some(code)) => {
            log_lines.push(format!("{}: failed, exit {code}", step.label));
            tracing::error!(source = %step.source_id, code, "update step failed");
            StepStatus::Failed {
                detail: format!("exit {code}"),
            }
        }
        Ok(None) => {
            log_lines.push(format!("{}: terminated by signal", step.label));
            tracing::error!(source = %step.source_id, "update step terminated by signal");
            StepStatus::Failed {
                detail: "terminated by signal".to_string(),
            }
        }
        Err(err) => {
            log_lines.push(format!("{}: failed to launch: {err:#}", step.label));
            tracing::error!(source = %step.source_id, %err, "update step failed to launch");
            StepStatus::Failed {
                detail: format!("failed to launch: {err:#}"),
            }
        }
    };
    StepOutcome {
        status,
        log_lines,
        output,
    }
}

/// A step that never ran, with the reason recorded exactly as the report
/// shows it.
fn skipped_step(step: &ActionStep, reason: &str) -> StepOutcome {
    tracing::info!(source = %step.source_id, reason, "update step skipped");
    StepOutcome {
        status: StepStatus::Skipped {
            reason: reason.to_string(),
        },
        log_lines: vec![format!("{}: skipped — {reason}", step.label)],
        output: None,
    }
}

/// Run one step the ordinary way: announce it on the terminal it is about to
/// take over (P1), then hand it over.
fn foreground_step(step: &ActionStep, runner: &impl StepRunner, tool: Option<&str>) -> StepOutcome {
    match skip_reason(step, tool) {
        Some(reason) => skipped_step(step, reason),
        None => {
            let argv = effective_command(step, tool);
            // The TUI is suspended (or we are in plain CLI mode): give the raw
            // terminal a header so the user knows whose output follows (P1).
            println!(":: {}", argv.join(" "));
            run_step(step, &argv, runner, false)
        }
    }
}

/// The session header, identical for both paths.
fn open_session(plan: &ActionPlan, log: &mut UpdateLog, tool: Option<&str>) {
    log.line("update session started");
    let run_ids: Vec<&str> = plan
        .steps
        .iter()
        .filter(|s| skip_reason(s, tool).is_none())
        .map(|s| s.label.as_str())
        .collect();
    log.line(&format!("sources: [{}]", run_ids.join(", ")));
}

/// Drain the outcomes into a report, writing every log line in plan order —
/// whatever order the steps actually finished in.
fn close_session(
    plan: &ActionPlan,
    outcomes: Vec<StepOutcome>,
    log: &mut UpdateLog,
) -> ExecutionReport {
    let steps = plan
        .steps
        .iter()
        .zip(outcomes)
        .map(|(step, outcome)| {
            for line in &outcome.log_lines {
                log.line(line);
            }
            StepReport {
                source_id: step.source_id.clone(),
                label: step.label.clone(),
                targets: step.targets.len(),
                status: outcome.status,
            }
        })
        .collect();

    let report = ExecutionReport {
        steps,
        log_path: log.path().to_path_buf(),
    };
    log.line(&format!(
        "update session complete: {}",
        session_summary(&report)
    ));
    report
}

/// Execute a pre-built plan, step by step, logging every command before and
/// after. One step failing never blocks the next (roadmap behavior rules); a
/// failed launch is reported, not raised. Infallible by design: every outcome
/// lands in the report.
pub fn execute(
    plan: &ActionPlan,
    runner: &impl StepRunner,
    log: &mut UpdateLog,
    tool: Option<&str>,
) -> ExecutionReport {
    open_session(plan, log, tool);
    let outcomes = plan
        .steps
        .iter()
        .map(|step| foreground_step(step, runner, tool))
        .collect();
    close_session(plan, outcomes, log)
}

/// Execute a plan with every step that declared `interactive: false` running
/// at the same time, while the interactive ones take the terminal one after
/// another in plan order (design §13, 2026-09-21).
///
/// The split is [`runs_in_background`] — the step's own declaration, never its
/// source id (design §13, 2026-09-07). It has to be: `pacman -Syu` and an AUR
/// helper both ask questions, and they also share pacman's database lock, so
/// neither may run beside anything — while `flatpak update --noninteractive`
/// and `cargo install-update -a` touch nothing pacman owns and answer nothing.
/// A privileged step stays in the foreground whatever it declares (design
/// §11).
///
/// Everything a caller can observe is unchanged from [`execute`]: the same
/// statuses, the same log lines, in plan order. What differs is wall time, and
/// that a background step's output is replayed after the fact instead of
/// printed as it happens.
///
/// No runtime, no pool: one scoped thread per background step, exactly as the
/// scanner runs its provider lanes (design §6). A plan has single-digit steps.
pub fn execute_concurrent(
    plan: &ActionPlan,
    runner: &(impl StepRunner + Sync),
    log: &mut UpdateLog,
    tool: Option<&str>,
) -> ExecutionReport {
    open_session(plan, log, tool);

    let in_background: Vec<bool> = plan
        .steps
        .iter()
        .map(|s| runs_in_background(s, tool))
        .collect();

    let mut indexed: Vec<(usize, StepOutcome)> = std::thread::scope(|scope| {
        let handles: Vec<(usize, _)> = plan
            .steps
            .iter()
            .enumerate()
            .filter(|(i, _)| in_background[*i])
            .map(|(i, step)| {
                let argv = effective_command(step, tool);
                // `&` because that is what it is: announced now, output later.
                println!(":: {} &", argv.join(" "));
                (i, scope.spawn(move || run_step(step, &argv, runner, true)))
            })
            .collect();

        // The terminal belongs to these, one at a time, in plan order — this
        // is where pacman asks its questions and sudo asks for a password.
        let mut done: Vec<(usize, StepOutcome)> = plan
            .steps
            .iter()
            .enumerate()
            .filter(|(i, _)| !in_background[*i])
            .map(|(i, step)| (i, foreground_step(step, runner, tool)))
            .collect();

        for (i, handle) in handles {
            let outcome = handle.join().unwrap_or_else(|_| StepOutcome {
                status: StepStatus::Failed {
                    detail: "panicked".to_string(),
                },
                log_lines: vec![format!("{}: panicked", plan.steps[i].label)],
                output: None,
            });
            done.push((i, outcome));
        }
        done
    });

    indexed.sort_by_key(|(i, _)| *i);
    let outcomes: Vec<StepOutcome> = indexed.into_iter().map(|(_, o)| o).collect();

    // Replay what ran behind the foreground, labelled, in plan order. Held
    // back until now rather than printed on arrival: two steps writing to one
    // terminal at once is how output becomes unreadable.
    for (step, outcome) in plan.steps.iter().zip(&outcomes) {
        if let Some(text) = &outcome.output {
            println!("\n:: {} (ran in parallel)", step.label);
            print!("{text}");
        }
    }

    close_session(plan, outcomes, log)
}

fn session_summary(report: &ExecutionReport) -> String {
    let executed = report.executed();
    let failed = report.failed();
    if executed == 0 {
        "nothing to execute".to_string()
    } else if failed == 0 {
        "all sources succeeded".to_string()
    } else {
        format!("{failed} of {executed} sources failed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ActionKind, ActionStep};
    use chrono::Utc;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::path::Path;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    /// Returns scripted outcomes in order and records every argv it was given.
    struct ScriptedRunner {
        outcomes: RefCell<VecDeque<anyhow::Result<Option<i32>>>>,
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl ScriptedRunner {
        fn new(outcomes: Vec<anyhow::Result<Option<i32>>>) -> Self {
            ScriptedRunner {
                outcomes: RefCell::new(outcomes.into()),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl StepRunner for ScriptedRunner {
        fn run(&self, argv: &[String]) -> anyhow::Result<Option<i32>> {
            self.calls.borrow_mut().push(argv.to_vec());
            self.outcomes
                .borrow_mut()
                .pop_front()
                .unwrap_or(Ok(Some(0)))
        }

        fn run_captured(&self, argv: &[String]) -> anyhow::Result<(Option<i32>, String)> {
            Ok((self.run(argv)?, String::new()))
        }
    }

    fn step(source: SourceId, targets: &[&str], command: &[&str]) -> ActionStep {
        privileged_step(source, targets, command, true)
    }

    fn privileged_step(
        source: SourceId,
        targets: &[&str],
        command: &[&str],
        privileged: bool,
    ) -> ActionStep {
        ActionStep {
            label: source.to_string(),
            source_id: source,
            kind: ActionKind::Update,
            targets: targets.iter().map(|t| t.to_string()).collect(),
            command: command.iter().map(|c| c.to_string()).collect(),
            privileged,
            // The pacman-shaped default: asks questions, keeps the terminal.
            interactive: true,
        }
    }

    fn flatpak_user_step() -> ActionStep {
        let mut step = privileged_step(
            SourceId::flatpak(),
            &["org.gimp.GIMP", "org.inkscape.Inkscape"],
            &["flatpak", "update", "--user", "--noninteractive"],
            false,
        );
        // `--noninteractive` is right there in the command.
        step.interactive = false;
        step
    }

    fn plan(steps: Vec<ActionStep>) -> ActionPlan {
        ActionPlan {
            created_at: Utc::now(),
            steps,
            requires_sudo: false,
        }
    }

    fn sandbox(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("paclens-exec-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn log_text(dir: &Path) -> String {
        let path = dir.join(format!("{}.log", Utc::now().format("%Y-%m-%d")));
        std::fs::read_to_string(path).unwrap()
    }

    // --- privilege classification ---
    #[test]
    fn only_flatpak_user_runs_unprivileged() {
        assert!(!needs_privilege(&flatpak_user_step()));
        assert!(needs_privilege(&step(
            SourceId::flatpak(),
            &["a"],
            &["flatpak"]
        )));
        assert!(needs_privilege(&step(
            SourceId::pacman(),
            &["a"],
            &["pacman", "-Syu"]
        )));
    }

    #[test]
    fn privileged_steps_skip_only_without_a_tool() {
        let pac = step(SourceId::pacman(), &["a"], &["pacman", "-Syu"]);
        assert_eq!(
            skip_reason(&pac, None),
            Some("no privilege tool found (sudo/doas/pkexec)")
        );
        assert_eq!(skip_reason(&pac, Some("sudo")), None);
        // A user-scope flatpak step never needs one.
        assert_eq!(skip_reason(&flatpak_user_step(), None), None);
    }

    #[test]
    fn effective_command_prepends_the_tool_only_where_needed() {
        let pac = step(SourceId::pacman(), &["a"], &["pacman", "-Syu"]);
        assert_eq!(
            effective_command(&pac, Some("doas")),
            vec!["doas", "pacman", "-Syu"]
        );
        // No tool → bare command (the step will be skipped anyway).
        assert_eq!(effective_command(&pac, None), vec!["pacman", "-Syu"]);
        // Unprivileged step never gets a prefix.
        assert_eq!(
            effective_command(&flatpak_user_step(), Some("sudo"))[0],
            "flatpak"
        );
    }

    #[test]
    fn executable_counters_depend_on_the_tool() {
        let p = plan(vec![
            step(SourceId::pacman(), &["linux", "firefox"], &["pacman"]),
            flatpak_user_step(),
        ]);
        assert_eq!(executable_steps(&p, None), 1);
        assert_eq!(executable_steps(&p, Some("sudo")), 2);
    }

    #[test]
    fn privilege_comes_from_the_step_not_from_its_source_id() {
        // The old rule read the id: "privileged unless flatpak-user or aur",
        // which made root the default for every source not yet written. Both
        // directions are pinned here, ids deliberately at odds with the flag.
        let unprivileged =
            privileged_step(SourceId::pacman(), &["timr-bin"], &["paru", "-Sua"], false);
        assert!(!needs_privilege(&unprivileged));
        assert_eq!(skip_reason(&unprivileged, None), None, "no tool needed");
        assert_eq!(
            effective_command(&unprivileged, Some("sudo")),
            vec!["paru", "-Sua"],
            "no sudo prefix even when a tool exists"
        );

        let privileged = privileged_step(
            SourceId::flatpak(),
            &["org.x.App"],
            &["flatpak", "update", "--system"],
            true,
        );
        assert!(needs_privilege(&privileged));
        assert_eq!(
            effective_command(&privileged, Some("sudo"))[0],
            "sudo",
            "a step that says it needs root gets the tool"
        );
    }

    #[test]
    fn target_noun_matches_each_sources_vocabulary() {
        assert_eq!(target_noun(&SourceId::flatpak(), 1), "1 flatpak");
        assert_eq!(target_noun(&SourceId::aur(), 2), "2 AUR packages");
        assert_eq!(target_noun(&SourceId::aur(), 1), "1 AUR package");
        assert_eq!(target_noun(&SourceId::flatpak(), 3), "3 flatpaks");
        assert_eq!(target_noun(&SourceId::pacman(), 1), "1 package");
        assert_eq!(target_noun(&SourceId::pacman(), 19), "19 packages");
    }

    // --- execution ---
    #[test]
    fn runs_the_exact_command_and_reports_success() {
        let dir = sandbox("success");
        let mut log = UpdateLog::open_in(&dir).unwrap();
        let runner = ScriptedRunner::new(vec![Ok(Some(0))]);

        let report = execute(&plan(vec![flatpak_user_step()]), &runner, &mut log, None);

        assert_eq!(
            runner.calls.borrow().as_slice(),
            &[vec![
                "flatpak".to_string(),
                "update".to_string(),
                "--user".to_string(),
                "--noninteractive".to_string()
            ]]
        );
        assert_eq!(report.steps.len(), 1);
        assert_eq!(report.steps[0].status, StepStatus::Succeeded);
        assert_eq!(report.steps[0].targets, 2);
        assert_eq!(report.succeeded(), 1);
        assert_eq!(report.failed(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skipped_steps_are_reported_not_run() {
        let dir = sandbox("skip");
        let mut log = UpdateLog::open_in(&dir).unwrap();
        let runner = ScriptedRunner::new(vec![Ok(Some(0))]);

        let p = plan(vec![
            step(SourceId::pacman(), &["linux"], &["pacman", "-Syu"]),
            flatpak_user_step(),
        ]);
        // No privilege tool → pacman never reaches the runner.
        let report = execute(&p, &runner, &mut log, None);

        assert_eq!(runner.calls.borrow().len(), 1);
        assert_eq!(
            report.steps[0].status,
            StepStatus::Skipped {
                reason: "no privilege tool found (sudo/doas/pkexec)".to_string()
            }
        );
        assert_eq!(report.skipped(), 1);
        assert_eq!(report.executed(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn privileged_step_runs_with_the_tool_prefix() {
        let dir = sandbox("priv");
        let mut log = UpdateLog::open_in(&dir).unwrap();
        let runner = ScriptedRunner::new(vec![Ok(Some(0)), Ok(Some(0))]);

        let p = plan(vec![
            step(SourceId::pacman(), &["linux"], &["pacman", "-Syu"]),
            flatpak_user_step(),
        ]);
        let report = execute(&p, &runner, &mut log, Some("sudo"));

        let calls = runner.calls.borrow();
        assert_eq!(calls[0], vec!["sudo", "pacman", "-Syu"]);
        assert_eq!(calls[1][0], "flatpak"); // no prefix for user scope
        assert_eq!(report.succeeded(), 2);
        assert_eq!(report.skipped(), 0);
        assert!(
            log_text(&dir).contains("sources: [pacman, flatpak]"),
            "{}",
            log_text(&dir)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn one_failure_does_not_block_the_next_step() {
        let dir = sandbox("isolate");
        let mut log = UpdateLog::open_in(&dir).unwrap();
        let runner = ScriptedRunner::new(vec![Ok(Some(1)), Ok(Some(0))]);

        // Two runnable steps (synthetic, but the executor must not care).
        let p = plan(vec![flatpak_user_step(), flatpak_user_step()]);
        let report = execute(&p, &runner, &mut log, None);

        assert_eq!(runner.calls.borrow().len(), 2);
        assert_eq!(
            report.steps[0].status,
            StepStatus::Failed {
                detail: "exit 1".to_string()
            }
        );
        assert_eq!(report.steps[1].status, StepStatus::Succeeded);
        assert_eq!(report.failed(), 1);
        assert_eq!(report.succeeded(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn signal_and_launch_failures_are_reported_uninterpreted() {
        let dir = sandbox("errors");
        let mut log = UpdateLog::open_in(&dir).unwrap();
        let runner = ScriptedRunner::new(vec![
            Ok(None),
            Err(anyhow::anyhow!("no such binary: flatpak")),
        ]);

        let p = plan(vec![flatpak_user_step(), flatpak_user_step()]);
        let report = execute(&p, &runner, &mut log, None);

        assert_eq!(
            report.steps[0].status,
            StepStatus::Failed {
                detail: "terminated by signal".to_string()
            }
        );
        match &report.steps[1].status {
            StepStatus::Failed { detail } => {
                assert!(detail.contains("no such binary"), "detail: {detail}")
            }
            other => panic!("expected launch failure, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writes_the_spec_format_session_log() {
        let dir = sandbox("logfmt");
        let mut log = UpdateLog::open_in(&dir).unwrap();
        let runner = ScriptedRunner::new(vec![Ok(Some(0))]);

        let p = plan(vec![
            step(SourceId::pacman(), &["linux"], &["pacman", "-Syu"]),
            flatpak_user_step(),
        ]);
        let report = execute(&p, &runner, &mut log, None);

        let text = log_text(&dir);
        assert!(text.contains("update session started"), "{text}");
        assert!(text.contains("sources: [flatpak]"), "{text}");
        assert!(
            text.contains("pacman: skipped — no privilege tool found (sudo/doas/pkexec)"),
            "{text}"
        );
        assert!(
            text.contains("flatpak: running update (2 flatpaks)"),
            "{text}"
        );
        assert!(text.contains("flatpak: completed, exit 0"), "{text}");
        assert!(
            text.contains("update session complete: all sources succeeded"),
            "{text}"
        );
        assert_eq!(
            report.log_path,
            dir.join(format!("{}.log", Utc::now().format("%Y-%m-%d")))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failure_summary_counts_failed_sources_in_the_log() {
        let dir = sandbox("failsum");
        let mut log = UpdateLog::open_in(&dir).unwrap();
        let runner = ScriptedRunner::new(vec![Ok(Some(2))]);

        execute(&plan(vec![flatpak_user_step()]), &runner, &mut log, None);

        let text = log_text(&dir);
        assert!(text.contains("flatpak: failed, exit 2"), "{text}");
        assert!(
            text.contains("update session complete: 1 of 1 sources failed"),
            "{text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- concurrent execution (`update --parallel`) ---

    /// A runner for the concurrent path: `Sync`, records *how* each step ran,
    /// and tracks how many were in flight at once — the only way to assert
    /// that "parallel" means parallel rather than "sequential, eventually".
    ///
    /// Programs whose name starts with `slow` hold for `hold`; everything else
    /// returns at once, so a test can make the last step finish first.
    struct ParallelRunner {
        live: AtomicUsize,
        peak: AtomicUsize,
        calls: Mutex<Vec<(&'static str, Vec<String>)>>,
        /// Any step whose command contains this word exits 1. Empty = none.
        fail: &'static str,
        hold: Duration,
    }

    impl ParallelRunner {
        fn new(hold: Duration, fail: &'static str) -> Self {
            ParallelRunner {
                live: AtomicUsize::new(0),
                peak: AtomicUsize::new(0),
                calls: Mutex::new(Vec::new()),
                fail,
                hold,
            }
        }

        fn code(&self, argv: &[String]) -> Option<i32> {
            let cmd = argv.join(" ");
            Some(if !self.fail.is_empty() && cmd.contains(self.fail) {
                1
            } else {
                0
            })
        }

        /// The programs run through one path, in the order they were called.
        fn via(&self, how: &str) -> Vec<String> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(m, _)| *m == how)
                .filter_map(|(_, argv)| argv.first().cloned())
                .collect()
        }
    }

    impl StepRunner for ParallelRunner {
        fn run(&self, argv: &[String]) -> anyhow::Result<Option<i32>> {
            self.calls.lock().unwrap().push(("terminal", argv.to_vec()));
            Ok(self.code(argv))
        }

        fn run_captured(&self, argv: &[String]) -> anyhow::Result<(Option<i32>, String)> {
            self.calls.lock().unwrap().push(("captured", argv.to_vec()));
            let live = self.live.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(live, Ordering::SeqCst);
            if argv.first().is_some_and(|p| p.starts_with("slow")) {
                std::thread::sleep(self.hold);
            }
            self.live.fetch_sub(1, Ordering::SeqCst);
            Ok((self.code(argv), format!("{} finished\n", argv.join(" "))))
        }
    }

    /// A step that declared it needs nothing from the terminal.
    fn background_step(program: &str) -> ActionStep {
        let mut st = privileged_step(SourceId::flatpak(), &["x"], &[program, "update"], false);
        st.label = program.to_string();
        st.interactive = false;
        st
    }

    #[test]
    fn background_steps_really_do_run_at_the_same_time() {
        let dir = sandbox("par-peak");
        let mut log = UpdateLog::open_in(&dir).unwrap();
        let runner = ParallelRunner::new(Duration::from_millis(150), "");

        let p = plan(vec![
            background_step("slow-a"),
            background_step("slow-b"),
            background_step("slow-c"),
        ]);
        let started = Instant::now();
        let report = execute_concurrent(&p, &runner, &mut log, None);
        let elapsed = started.elapsed();

        assert_eq!(report.succeeded(), 3);
        assert_eq!(
            runner.peak.load(Ordering::SeqCst),
            3,
            "all three should have been in flight at once"
        );
        assert!(
            elapsed < Duration::from_millis(400),
            "3 × 150ms sequentially is 450ms; took {elapsed:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn interactive_steps_keep_the_terminal_to_themselves() {
        let dir = sandbox("par-terminal");
        let mut log = UpdateLog::open_in(&dir).unwrap();
        let runner = ParallelRunner::new(Duration::ZERO, "");

        // pacman declares itself interactive; the other two do not.
        let p = plan(vec![
            step(SourceId::pacman(), &["linux"], &["pacman", "-Syu"]),
            background_step("flatpak"),
            background_step("cargo"),
        ]);
        let report = execute_concurrent(&p, &runner, &mut log, Some("sudo"));

        assert_eq!(report.succeeded(), 3);
        assert_eq!(
            runner.via("terminal"),
            vec!["sudo"],
            "only pacman (behind sudo) got the real terminal"
        );
        assert_eq!(runner.via("captured"), vec!["flatpak", "cargo"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_report_and_the_log_stay_in_plan_order() {
        let dir = sandbox("par-order");
        let mut log = UpdateLog::open_in(&dir).unwrap();
        let runner = ParallelRunner::new(Duration::from_millis(120), "");

        // The first step finishes last. Nothing downstream may notice.
        let p = plan(vec![background_step("slow-first"), background_step("fast")]);
        let report = execute_concurrent(&p, &runner, &mut log, None);

        let labels: Vec<&str> = report.steps.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, vec!["slow-first", "fast"]);

        let text = log_text(&dir);
        let first = text.find("slow-first: completed").expect("slow logged");
        let second = text.find("fast: completed").expect("fast logged");
        assert!(
            first < second,
            "log follows the plan, not the finish line:\n{text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failing_background_step_does_not_take_the_others_with_it() {
        let dir = sandbox("par-fail");
        let mut log = UpdateLog::open_in(&dir).unwrap();
        let runner = ParallelRunner::new(Duration::ZERO, "bad");

        let p = plan(vec![
            background_step("good"),
            background_step("bad"),
            background_step("also-good"),
        ]);
        let report = execute_concurrent(&p, &runner, &mut log, None);

        assert_eq!(report.succeeded(), 2);
        assert_eq!(
            report.steps[1].status,
            StepStatus::Failed {
                detail: "exit 1".to_string()
            }
        );
        assert_eq!(runner.via("captured").len(), 3, "all three still ran");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skipped_steps_stay_skipped_when_running_in_parallel() {
        let dir = sandbox("par-skip");
        let mut log = UpdateLog::open_in(&dir).unwrap();
        let runner = ParallelRunner::new(Duration::ZERO, "");

        // Privileged, no tool: skipped in both paths, and never handed to a
        // thread just because it asks nothing of the terminal.
        let mut privileged_background = background_step("flatpak-system");
        privileged_background.privileged = true;

        let p = plan(vec![privileged_background, background_step("cargo")]);
        let report = execute_concurrent(&p, &runner, &mut log, None);

        assert_eq!(report.skipped(), 1);
        assert_eq!(report.succeeded(), 1);
        assert_eq!(runner.via("captured"), vec!["cargo"]);
        assert!(runner.via("terminal").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_privileged_step_stays_in_the_foreground_however_quiet_it_is() {
        let dir = sandbox("par-priv");
        let mut log = UpdateLog::open_in(&dir).unwrap();
        let runner = ParallelRunner::new(Duration::ZERO, "");

        // flatpak · system: answers its own prompts, but needs root. design
        // §11 — no privileged process in the background, and sudo would ask
        // for a password on a terminal three other steps are writing to.
        let mut system_scope = background_step("flatpak-system");
        system_scope.privileged = true;

        let p = plan(vec![system_scope, background_step("cargo")]);
        let report = execute_concurrent(&p, &runner, &mut log, Some("sudo"));

        assert_eq!(report.succeeded(), 2);
        assert_eq!(runner.via("terminal"), vec!["sudo"]);
        assert_eq!(runner.via("captured"), vec!["cargo"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The production runner, on real processes: the mocks above cannot say
    /// whether `Stdio::null()` and the two-stream capture actually behave.
    #[test]
    fn the_real_runner_captures_both_streams_and_hands_stdin_an_eof() {
        let sh = |script: &str| vec!["sh".to_string(), "-c".to_string(), script.to_string()];

        let (code, text) = InteractiveRunner
            .run_captured(&sh("echo to-stdout; echo to-stderr >&2; exit 3"))
            .unwrap();
        assert_eq!(code, Some(3));
        assert!(text.contains("to-stdout"), "{text}");
        assert!(text.contains("to-stderr"), "{text}");

        // A step that asks a question anyway gets EOF and finishes, rather
        // than hanging a run with no terminal to answer on.
        let (code, text) = InteractiveRunner
            .run_captured(&sh("read answer || echo eof-not-a-hang"))
            .unwrap();
        assert_eq!(code, Some(0));
        assert!(text.contains("eof-not-a-hang"), "{text}");
    }

    #[test]
    fn empty_plan_logs_nothing_to_execute() {
        let dir = sandbox("empty");
        let mut log = UpdateLog::open_in(&dir).unwrap();
        let runner = ScriptedRunner::new(Vec::new());

        let report = execute(&plan(Vec::new()), &runner, &mut log, None);

        assert!(report.steps.is_empty());
        assert!(runner.calls.borrow().is_empty());
        assert!(
            log_text(&dir).contains("update session complete: nothing to execute"),
            "{}",
            log_text(&dir)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
