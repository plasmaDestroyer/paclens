//! Execution layer: runs pre-built `ActionPlan`s (the *execute* step of P4).
//!
//! Contract (dev-notes §3): the executor never decides what to do — all
//! decisions come from the user via the TUI/CLI. It logs every command before
//! and after execution, and reports exit codes without interpretation (the
//! renderers interpret). Steps it cannot run *yet* (pacman and flatpak-system
//! need sudo, which arrives in v0.1) are never silently dropped: they come back
//! as `Skipped` with an explicit reason.
//!
//! Commands run with inherited stdio in the raw terminal — the user sees and
//! interacts with the tool's own output directly (spec §13.2). The
//! [`StepRunner`] trait is the testing seam, mirroring the providers'
//! `CommandRunner`.

mod log;

use std::path::PathBuf;

use anyhow::Context;

use crate::model::{ActionPlan, ActionStep, SourceId};

pub use log::UpdateLog;

/// Runs one command with inherited stdio, returning its exit code (`None` when
/// the process was terminated by a signal). Injectable for testing.
pub trait StepRunner {
    fn run(&self, argv: &[String]) -> anyhow::Result<Option<i32>>;
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

/// Why a step cannot run in this version, or `None` if it is executable now.
/// v0.0.6 executes Flatpak user-scope only — everything needing sudo waits for
/// the privilege model in v0.1 (roadmap; spec §13).
pub fn skip_reason(step: &ActionStep) -> Option<&'static str> {
    if step.source_id == SourceId::flatpak_user() {
        None
    } else if step.source_id == SourceId::flatpak_system() {
        Some("needs sudo — execution arrives in v0.1")
    } else if step.source_id == SourceId::pacman() {
        Some("execution arrives in v0.1")
    } else {
        Some("not executable in this version")
    }
}

/// Total packages/apps across the steps that would actually run.
pub fn executable_targets(plan: &ActionPlan) -> usize {
    plan.steps
        .iter()
        .filter(|s| skip_reason(s).is_none())
        .map(|s| s.targets.len())
        .sum()
}

/// How many steps would actually run.
pub fn executable_steps(plan: &ActionPlan) -> usize {
    plan.steps
        .iter()
        .filter(|s| skip_reason(s).is_none())
        .count()
}

/// `"3 apps"` / `"1 package"` — the unit the source itself uses.
pub fn target_noun(source_id: &SourceId, count: usize) -> String {
    let unit = match (source_id.as_str().starts_with("flatpak"), count) {
        (true, 1) => "app",
        (true, _) => "apps",
        (false, 1) => "package",
        (false, _) => "packages",
    };
    format!("{count} {unit}")
}

/// Execute a pre-built plan, step by step, logging every command before and
/// after. One step failing never blocks the next (roadmap behavior rules); a
/// failed launch is reported, not raised. Infallible by design: every outcome
/// lands in the report.
pub fn execute(
    plan: &ActionPlan,
    runner: &impl StepRunner,
    log: &mut UpdateLog,
) -> ExecutionReport {
    log.line("update session started");
    let run_ids: Vec<&str> = plan
        .steps
        .iter()
        .filter(|s| skip_reason(s).is_none())
        .map(|s| s.source_id.as_str())
        .collect();
    log.line(&format!("sources: [{}]", run_ids.join(", ")));

    let mut steps = Vec::new();
    for step in &plan.steps {
        let targets = step.targets.len();

        if let Some(reason) = skip_reason(step) {
            log.line(&format!("{}: skipped — {reason}", step.source_id));
            tracing::info!(source = %step.source_id, reason, "update step skipped");
            steps.push(StepReport {
                source_id: step.source_id.clone(),
                targets,
                status: StepStatus::Skipped {
                    reason: reason.to_string(),
                },
            });
            continue;
        }

        let cmd = step.command.join(" ");
        log.line(&format!(
            "{}: running update ({})",
            step.source_id,
            target_noun(&step.source_id, targets)
        ));
        tracing::info!(source = %step.source_id, command = %cmd, "executing update step");
        // The TUI is suspended (or we are in plain CLI mode): give the raw
        // terminal a header so the user knows whose output follows (P1).
        println!(":: {cmd}");

        let status = match runner.run(&step.command) {
            Ok(Some(0)) => {
                log.line(&format!("{}: completed, exit 0", step.source_id));
                StepStatus::Succeeded
            }
            Ok(Some(code)) => {
                log.line(&format!("{}: failed, exit {code}", step.source_id));
                tracing::error!(source = %step.source_id, code, "update step failed");
                StepStatus::Failed {
                    detail: format!("exit {code}"),
                }
            }
            Ok(None) => {
                log.line(&format!("{}: terminated by signal", step.source_id));
                tracing::error!(source = %step.source_id, "update step terminated by signal");
                StepStatus::Failed {
                    detail: "terminated by signal".to_string(),
                }
            }
            Err(err) => {
                log.line(&format!("{}: failed to launch: {err:#}", step.source_id));
                tracing::error!(source = %step.source_id, %err, "update step failed to launch");
                StepStatus::Failed {
                    detail: format!("failed to launch: {err:#}"),
                }
            }
        };
        steps.push(StepReport {
            source_id: step.source_id.clone(),
            targets,
            status,
        });
    }

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
    }

    fn step(source: SourceId, targets: &[&str], command: &[&str]) -> ActionStep {
        ActionStep {
            source_id: source,
            kind: ActionKind::Update,
            targets: targets.iter().map(|t| t.to_string()).collect(),
            command: command.iter().map(|c| c.to_string()).collect(),
        }
    }

    fn flatpak_user_step() -> ActionStep {
        step(
            SourceId::flatpak_user(),
            &["org.gimp.GIMP", "org.inkscape.Inkscape"],
            &["flatpak", "update", "--user", "--noninteractive"],
        )
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

    // --- skip classification ---
    #[test]
    fn only_flatpak_user_is_executable_in_v006() {
        assert_eq!(skip_reason(&flatpak_user_step()), None);
        let sys = step(SourceId::flatpak_system(), &["a"], &["flatpak"]);
        assert_eq!(
            skip_reason(&sys),
            Some("needs sudo — execution arrives in v0.1")
        );
        let pac = step(SourceId::pacman(), &["a"], &["pacman", "-Syu"]);
        assert_eq!(skip_reason(&pac), Some("execution arrives in v0.1"));
    }

    #[test]
    fn executable_counters_only_count_runnable_steps() {
        let p = plan(vec![
            step(SourceId::pacman(), &["linux", "firefox"], &["pacman"]),
            flatpak_user_step(),
        ]);
        assert_eq!(executable_steps(&p), 1);
        assert_eq!(executable_targets(&p), 2); // the two flatpak apps, not pacman's
    }

    #[test]
    fn target_noun_matches_each_sources_vocabulary() {
        assert_eq!(target_noun(&SourceId::flatpak_user(), 1), "1 app");
        assert_eq!(target_noun(&SourceId::flatpak_system(), 3), "3 apps");
        assert_eq!(target_noun(&SourceId::pacman(), 1), "1 package");
        assert_eq!(target_noun(&SourceId::pacman(), 19), "19 packages");
    }

    // --- execution ---
    #[test]
    fn runs_the_exact_command_and_reports_success() {
        let dir = sandbox("success");
        let mut log = UpdateLog::open_in(&dir).unwrap();
        let runner = ScriptedRunner::new(vec![Ok(Some(0))]);

        let report = execute(&plan(vec![flatpak_user_step()]), &runner, &mut log);

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
        let report = execute(&p, &runner, &mut log);

        // pacman never reached the runner.
        assert_eq!(runner.calls.borrow().len(), 1);
        assert_eq!(
            report.steps[0].status,
            StepStatus::Skipped {
                reason: "execution arrives in v0.1".to_string()
            }
        );
        assert_eq!(report.skipped(), 1);
        assert_eq!(report.executed(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn one_failure_does_not_block_the_next_step() {
        let dir = sandbox("isolate");
        let mut log = UpdateLog::open_in(&dir).unwrap();
        let runner = ScriptedRunner::new(vec![Ok(Some(1)), Ok(Some(0))]);

        // Two runnable steps (synthetic, but the executor must not care).
        let p = plan(vec![flatpak_user_step(), flatpak_user_step()]);
        let report = execute(&p, &runner, &mut log);

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
        let report = execute(&p, &runner, &mut log);

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
        let report = execute(&p, &runner, &mut log);

        let text = log_text(&dir);
        assert!(text.contains("update session started"), "{text}");
        assert!(text.contains("sources: [flatpak-user]"), "{text}");
        assert!(
            text.contains("pacman: skipped — execution arrives in v0.1"),
            "{text}"
        );
        assert!(
            text.contains("flatpak-user: running update (2 apps)"),
            "{text}"
        );
        assert!(text.contains("flatpak-user: completed, exit 0"), "{text}");
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

        execute(&plan(vec![flatpak_user_step()]), &runner, &mut log);

        let text = log_text(&dir);
        assert!(text.contains("flatpak-user: failed, exit 2"), "{text}");
        assert!(
            text.contains("update session complete: 1 of 1 sources failed"),
            "{text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_plan_logs_nothing_to_execute() {
        let dir = sandbox("empty");
        let mut log = UpdateLog::open_in(&dir).unwrap();
        let runner = ScriptedRunner::new(Vec::new());

        let report = execute(&plan(Vec::new()), &runner, &mut log);

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
