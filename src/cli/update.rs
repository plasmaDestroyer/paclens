//! `paclens update [--dry-run] [--source <id>] [--parallel]` — run every
//! source's update after one sudo prompt; the tools ask their own questions
//! (design §13, 2026-10-06). `--dry-run` prints the exact commands instead.
//!
//! The plan is built by the shared `crate::planner` and executed by the shared
//! `crate::executor`, so the CLI and the TUI can never disagree (P5).

use crate::cli::style::Styles;
use crate::config::Config;
use crate::executor::{self, ExecutionReport, InteractiveRunner, StepStatus, UpdateLog};
use crate::model::ActionPlan;
use crate::providers::SystemCommandRunner;
use crate::{planner, scanner};

pub fn run(
    config: &Config,
    dry_run: bool,
    source: Option<&str>,
    parallel: bool,
    stdin_is_tty: bool,
    styles: &Styles,
) -> anyhow::Result<()> {
    // Executing needs an interactive confirmation, so fail fast (before the
    // scan) when there is no terminal to ask on. Scripts get --dry-run.
    if !dry_run && !stdin_is_tty {
        anyhow::bail!(
            "update needs a terminal for the tools' own prompts — use --dry-run to preview"
        );
    }

    let runner = SystemCommandRunner::new(config.scan.provider_timeout_secs);
    // No scan: someone who typed `update` has decided, and checking first
    // means waiting on `checkupdates`, the helper and `flatpak remote-ls` —
    // three network round trips — to produce a list the tools recompute a
    // second later anyway (user decision 2026-09-17). Detection is PATH
    // probes only.
    let scan = scanner::detect_sources(&runner, config);

    if let Some(requested) = source
        && !scan.sources.iter().any(|s| s.id.as_str() == requested)
    {
        let known: Vec<&str> = scan.sources.iter().map(|s| s.id.as_str()).collect();
        anyhow::bail!(
            "unknown source {requested:?}; known sources: {}",
            known.join(", ")
        );
    }

    let plan = planner::plan_full_upgrade(&scan, |id| match source {
        Some(requested) => id.as_str() == requested,
        None => true,
    });

    // Typing `update` is the decision: no plan and no [Y/n] of paclens's
    // own. pacman and the helper show what they will change and ask for
    // themselves (design §13, 2026-10-06). `--dry-run` still shows it all.
    if dry_run || plan.is_empty() {
        let tool = executor::sudo::detect();
        print!("{}", render_plan(&plan, tool, parallel, dry_run, styles));
        return Ok(());
    }
    execute_flow(&plan, parallel, config.update.loop_interval(), styles)
}

/// The execute half of a bare `paclens update`: one sudo prompt, run, report.
fn execute_flow(
    plan: &ActionPlan,
    parallel: bool,
    sudo_loop: Option<std::time::Duration>,
    styles: &Styles,
) -> anyhow::Result<()> {
    let tool = executor::sudo::detect();

    // Counted in commands, not packages: nothing was looked up, so there is no
    // package count to offer. Commands rather than sources, because flatpak
    // contributes two steps and the prompt must match what runs (2026-09-17).
    if executor::executable_steps(plan, tool) == 0 {
        println!("\n{}", styles.dim("nothing to execute"));
        return Ok(());
    }

    // A warm timestamp only once there is one to keep warm (#24).
    let _keepalive = (prime_privilege(plan, tool, styles) && sudo_loop.is_some())
        .then(|| sudo_loop.map(executor::sudo::Keepalive::start))
        .flatten();

    let mut log = UpdateLog::open_default()?;
    let report = if parallel {
        executor::execute_concurrent(plan, &InteractiveRunner, &mut log, tool)
    } else {
        executor::execute(plan, &InteractiveRunner, &mut log, tool)
    };

    println!();
    print!("{}", render_report(&report, styles));

    if report.failed() > 0 {
        anyhow::bail!(
            "{} of {} sources failed — see the log above",
            report.failed(),
            report.executed()
        );
    }
    Ok(())
}

/// Authenticate once, here, where the reader is looking.
///
/// Without this, `pacman -Syu` asks for a password and then paru — which
/// self-elevates, and is never run under sudo — asks again seconds later.
/// One `sudo -v` up front satisfies both, because they share the terminal's
/// sudo timestamp (user decision 2026-09-21).
///
/// Failure is not fatal: the steps ask for themselves, exactly as before.
/// A build long enough to outlive sudo's timeout will still stop for a second
/// prompt — `sudo_loop` in the config is the opt-in for that, and it stays
/// opt-in because keeping the timestamp warm lets anything running as this
/// user use sudo unasked.
fn prime_privilege(plan: &ActionPlan, tool: Option<&str>, s: &Styles) -> bool {
    if !executor::sudo::worth_priming(plan, tool) {
        return false;
    }

    let argv = executor::sudo::prime_command();
    let ok = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .status()
        .map(|st| st.success())
        .unwrap_or(false);
    if !ok {
        println!(
            "  {}",
            s.dim("could not authenticate — each step will ask for itself")
        );
    }
    ok
}

/// The plan, once: each step's label and the exact command that will run,
/// privilege prefix included, so the preview and the run cannot disagree
/// (P1). No package list, because none was looked up (2026-09-17). Under
/// `--parallel` a step that runs beside the others ends in `&`.
fn render_plan(
    plan: &ActionPlan,
    tool: Option<&str>,
    parallel: bool,
    dry_run: bool,
    s: &Styles,
) -> String {
    let n = executor::executable_steps(plan, tool);
    let summary = if plan.is_empty() {
        s.summary_ok("no source can be updated")
    } else {
        let commands = format!("{n} command{}", if n == 1 { "" } else { "s" });
        s.summary_updates(&if dry_run {
            format!("would run {commands}")
        } else {
            format!("update {} {commands}", s.bullet())
        })
    };

    let mut out = format!("{}\n", summary);
    let label_w = plan
        .steps
        .iter()
        .map(|step| step.label.len())
        .max()
        .unwrap_or(0);
    for step in &plan.steps {
        let label = s.title(&format!("{:label_w$}", step.label));
        let line = match executor::skip_reason(step, tool) {
            Some(reason) => s.dim(&format!("skipped — {reason}")),
            None => {
                let mut cmd = executor::effective_command(step, tool).join(" ");
                if parallel && executor::runs_in_background(step, tool) {
                    cmd.push_str(" &");
                }
                cmd
            }
        };
        out.push_str(&format!("  {label}  {line}\n"));
    }
    out
}

/// Render the post-execution report: a headline, one line per step
/// (✓ succeeded / ✗ failed / · skipped), and the log path. Pure for the same
/// reason as `render_plan`.
fn render_report(report: &ExecutionReport, s: &Styles) -> String {
    let log = s.dim(&format!("log {}", report.log_path.display()));
    if report.failed() == 0 && report.skipped() == 0 && report.executed() > 0 {
        return format!(
            "{} {} {} {}\n",
            s.success(s.check()),
            s.summary_ok(&format!("{} done", report.executed())),
            s.dim(s.bullet()),
            log
        );
    }
    let executed = report.executed();
    let src_word = if executed == 1 { "source" } else { "sources" };
    let counts = format!(
        "{executed} {src_word} ran {b} {} succeeded",
        report.succeeded(),
        b = s.bullet()
    );
    let headline = if report.failed() == 0 {
        s.summary_ok(&counts)
    } else {
        s.error(&format!(
            "{counts} {b} {} failed",
            report.failed(),
            b = s.bullet()
        ))
    };

    let mut out = String::new();
    out.push_str(&format!("{}\n", headline));

    let name_w = report
        .steps
        .iter()
        .map(|st| st.label.as_str().len())
        .max()
        .unwrap_or(0);
    for st in &report.steps {
        let name = format!("{:name_w$}", st.label.as_str());
        let line = match &st.status {
            // No targets means nothing was counted, not that nothing moved:
            // an unchecked run has no list to count. Say "done" rather than
            // report zero packages (P1).
            StepStatus::Succeeded if st.targets == 0 => {
                format!("  {} {}  done", s.success(s.check()), s.title(&name),)
            }
            StepStatus::Succeeded => format!(
                "  {} {}  {} updated",
                s.success(s.check()),
                s.title(&name),
                executor::target_noun(&st.source_id, st.targets),
            ),
            StepStatus::Failed { detail } => format!(
                "  {} {}  {}",
                s.error(s.cross()),
                s.title(&name),
                s.error(&format!("failed ({detail})")),
            ),
            StepStatus::Skipped { reason } => format!(
                "  {} {}",
                s.bullet(),
                s.dim(&format!("{name}  skipped — {reason}"))
            ),
        };
        out.push_str(&line);
        out.push('\n');
    }

    out.push_str(&format!(
        "\n  {}\n",
        s.dim(&format!("log: {}", report.log_path.display()))
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ColorTheme;
    use crate::model::{
        CacheSizes, PendingUpdate, SCHEMA_VERSION, ScanResult, Source, SourceId, SourceKind,
    };
    use chrono::Utc;

    /// A plan step, spelled out: `(interactive, privileged)` is the pair the
    /// preview turns into a promise about when it runs.
    fn plan_step(label: &str, interactive: bool, privileged: bool) -> crate::model::ActionStep {
        crate::model::ActionStep {
            label: label.to_string(),
            source_id: SourceId::pacman(),
            kind: crate::model::ActionKind::Update,
            targets: Vec::new(),
            command: vec![label.to_string()],
            privileged,
            interactive,
        }
    }

    /// The preview promises when a step runs, not just what it runs (P1), and
    /// it promises it by asking the executor — not by repeating the rule.
    #[test]
    fn the_parallel_preview_marks_the_steps_that_run_at_once() {
        let plan = ActionPlan {
            steps: vec![
                plan_step("pacman", true, true),
                plan_step("flatpak · system", false, true),
                plan_step("cargo", false, false),
            ],
        };
        let text = render_plan(&plan, Some("sudo"), true, true, &plain());
        let line = |label: &str| {
            text.lines()
                .find(|l| l.trim_start().starts_with(label))
                .unwrap_or_default()
                .to_string()
        };
        assert!(line("cargo").ends_with('&'), "{text}");
        assert!(
            !line("pacman").ends_with('&'),
            "a step that asks questions keeps the terminal:\n{text}"
        );
        assert!(
            !line("flatpak").ends_with('&'),
            "design §11 — nothing privileged runs in the background:\n{text}"
        );
        let serial = render_plan(&plan, Some("sudo"), false, true, &plain());
        assert!(!serial.contains('&'), "{serial}");
    }

    /// Piped styler: Unicode glyphs, no ANSI — deterministic for assertions.
    fn plain() -> Styles {
        Styles::resolve(false, ColorTheme::Dark, false)
    }

    fn ascii() -> Styles {
        Styles::resolve(true, ColorTheme::Dark, true)
    }

    fn scan(updates: Vec<PendingUpdate>) -> ScanResult {
        ScanResult {
            schema_version: SCHEMA_VERSION,
            scanned_at: Utc::now(),
            sources: vec![
                Source {
                    id: SourceId::pacman(),
                    kind: SourceKind::Pacman,
                    available: true,
                    last_scanned: None,
                    accurate_updates: true,
                    scan_error: None,
                },
                Source {
                    id: SourceId::flatpak(),
                    kind: SourceKind::Flatpak,
                    available: true,
                    last_scanned: None,
                    accurate_updates: true,
                    scan_error: None,
                },
            ],
            packages: Vec::new(),
            updates,
            cache_sizes: CacheSizes::default(),
            flatpak_profile_sizes: Default::default(),
            profile_dir_sizes: Default::default(),
            aur_helper: crate::providers::aur::HelperChoice::Detected(
                crate::providers::aur::AurHelper::Paru,
            ),
            kernel: None,
            pacfiles: Vec::new(),
            stale_processes: Vec::new(),
        }
    }

    #[test]
    fn an_unchecked_plan_still_has_something_to_execute() {
        // The bug this pins: the confirm gate counted *packages*, and an
        // unchecked plan carries none, so `update` printed "nothing to
        // execute" and ran nothing. Count sources (2026-09-17).
        let s = scan(Vec::new());
        let plan = planner::plan_full_upgrade(&s, |_| true);

        assert!(plan.steps.iter().all(|step| step.targets.is_empty()));
        assert!(
            executor::executable_steps(&plan, Some("sudo")) > 0,
            "an unchecked plan must still be executable"
        );
    }

    #[test]
    fn the_plan_names_each_source_and_the_exact_command_it_will_run() {
        // No package list: nothing was looked up. P1 asks for the commands,
        // and these are them (2026-09-17).
        let s = scan(Vec::new());
        let plan = planner::plan_full_upgrade(&s, |_| true);
        let text = render_plan(&plan, Some("sudo"), false, false, &plain());

        assert!(text.starts_with("update · 3 commands"), "{text}");
        // The privilege prefix is in the command itself, so the preview and
        // the run cannot disagree — and each command appears once.
        assert!(text.contains("sudo pacman -Syu"), "{text}");
        assert_eq!(text.matches("pacman -Syu").count(), 1, "{text}");
        assert!(text.contains("flatpak update --user"), "{text}");
        assert!(text.contains("sudo flatpak update --system"), "{text}");
        assert!(!text.contains('\u{1b}'), "no ANSI in the plain styler");
    }

    #[test]
    fn a_source_with_nothing_pending_still_gets_its_command() {
        // The whole point: paclens did not check, so it cannot skip a source
        // for having nothing to do. The tool says that itself, faster.
        let s = scan(Vec::new());
        let plan = planner::plan_full_upgrade(&s, |_| true);
        assert!(!plan.is_empty(), "the unchecked plan runs anyway");
    }

    #[test]
    fn both_flatpak_installations_are_in_an_unchecked_plan() {
        // Which installation holds an out-of-date app is exactly what was not
        // looked up, so both get a command — and the system one asks for
        // root, which the plan shows before the prompt.
        let s = scan(Vec::new());
        let plan = planner::plan_full_upgrade(&s, |id| id == &SourceId::flatpak());
        assert_eq!(plan.steps.len(), 2, "one per installation");
        assert!(plan.requires_sudo(), "the system half needs it");
        let text = render_plan(&plan, Some("sudo"), false, false, &plain());
        assert!(text.contains("flatpak · user"), "{text}");
        assert!(text.contains("flatpak · system"), "{text}");
    }

    #[test]
    fn a_machine_with_no_usable_source_says_so() {
        let mut s = scan(Vec::new());
        for source in s.sources.iter_mut() {
            source.available = false;
        }
        let plan = planner::plan_full_upgrade(&s, |_| true);
        let text = render_plan(&plan, Some("sudo"), false, false, &plain());
        assert!(text.contains("no source can be updated"), "{text}");
        assert!(!text.contains("sudo"), "{text}");
    }

    #[test]
    fn an_aur_only_plan_is_still_worth_priming() {
        // paru is never run under sudo — it escalates itself — so the plan
        // carries no privileged step and would still stop for a password.
        let mut s = scan(Vec::new());
        s.sources.push(Source {
            id: SourceId::aur(),
            kind: SourceKind::Aur,
            available: true,
            last_scanned: None,
            accurate_updates: true,
            scan_error: None,
        });
        let plan = planner::plan_full_upgrade(&s, |id| id.as_str() == "aur");

        assert!(
            !plan.requires_sudo(),
            "nothing in an AUR plan runs under sudo"
        );
        assert!(executor::sudo::worth_priming(&plan, Some("sudo")));
        assert!(
            !executor::sudo::worth_priming(&plan, Some("doas")),
            "sudo -v is sudo's own"
        );
        assert!(!executor::sudo::worth_priming(&plan, None));
    }

    #[test]
    fn a_plan_that_never_escalates_is_not_worth_priming() {
        let s = scan(Vec::new());
        let plan = planner::plan_full_upgrade(&s, |id| id.as_str() == "cargo");
        assert!(!executor::sudo::worth_priming(&plan, Some("sudo")));
    }

    // --- the post-execution report ---
    use crate::executor::StepReport;
    use std::path::PathBuf;

    fn report(steps: Vec<StepReport>) -> ExecutionReport {
        ExecutionReport {
            steps,
            log_path: PathBuf::from("/tmp/paclens/2026-06-12.log"),
        }
    }

    fn step(source: SourceId, targets: usize, status: StepStatus) -> StepReport {
        labelled_step(source.to_string(), source, targets, status)
    }

    /// A step whose display name is not just its source id — flatpak's two
    /// installations are two steps of one source.
    fn labelled_step(
        label: String,
        source: SourceId,
        targets: usize,
        status: StepStatus,
    ) -> StepReport {
        StepReport {
            label,
            source_id: source,
            targets,
            status,
        }
    }

    #[test]
    fn report_lists_success_failure_and_skip_with_the_log_path() {
        let r = report(vec![
            step(
                SourceId::pacman(),
                3,
                StepStatus::Skipped {
                    reason: "execution arrives in v0.1".to_string(),
                },
            ),
            labelled_step(
                "flatpak · user".to_string(),
                SourceId::flatpak(),
                2,
                StepStatus::Succeeded,
            ),
            labelled_step(
                "flatpak · system".to_string(),
                SourceId::flatpak(),
                1,
                StepStatus::Failed {
                    detail: "exit 1".to_string(),
                },
            ),
        ]);
        let text = render_report(&r, &plain());

        assert!(
            text.contains("2 sources ran · 1 succeeded · 1 failed"),
            "headline missing:\n{text}"
        );
        // One source, two installations: the rows must be told apart, or a
        // green flatpak and a red flatpak say nothing about which half failed.
        let row = |needle: &str| {
            text.lines()
                .find(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("no {needle} row in:\n{text}"))
                .to_string()
        };
        let ok = row("flatpak · user");
        assert!(
            ok.contains('✓') && ok.contains("2 flatpaks updated"),
            "{ok}"
        );
        let failed = row("flatpak · system");
        assert!(
            failed.contains('✗') && failed.contains("failed (exit 1)"),
            "{failed}"
        );
        let skipped = row("pacman");
        assert!(
            skipped.contains("skipped — execution arrives in v0.1"),
            "{skipped}"
        );
        assert!(
            text.contains("log: /tmp/paclens/2026-06-12.log"),
            "log path missing:\n{text}"
        );
        assert!(!text.contains('\u{1b}'));
    }

    #[test]
    fn an_all_green_report_is_one_line() {
        let r = report(vec![step(SourceId::flatpak(), 1, StepStatus::Succeeded)]);
        let text = render_report(&r, &plain());
        // Everything worked: one line, and the log for the detail.
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(text.contains("1 done"), "{text}");
        assert!(text.contains("log /tmp/paclens/2026-06-12.log"), "{text}");
        assert!(!text.contains("failed"), "{text}");
    }

    #[test]
    fn ascii_report_uses_the_ascii_marks() {
        let r = report(vec![
            step(SourceId::flatpak(), 2, StepStatus::Succeeded),
            step(
                SourceId::flatpak(),
                1,
                StepStatus::Failed {
                    detail: "exit 1".to_string(),
                },
            ),
        ]);
        let text = render_report(&r, &ascii());
        assert!(text.contains("x flatpak"), "{text}");
        assert!(text.contains("! flatpak"), "{text}");
        assert!(!text.contains('✓'));
        assert!(!text.contains('✗'));
    }
}
