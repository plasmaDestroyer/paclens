//! `paclens update [--dry-run] [--source <id>] [--parallel]` — show the update plan (spec
//! §11.3) and, since v0.0.6, execute it after a Y/n confirmation. v0.0.6 runs
//! Flatpak user-scope only; everything needing sudo is reported as skipped.
//!
//! The plan is built by the shared `crate::planner` and executed by the shared
//! `crate::executor`, so the CLI and the TUI can never disagree (P5). The
//! pipeline is intact (P4): the full plan prints before the prompt, nothing
//! runs without an explicit `y`, and the per-source report hides nothing.

use std::io::Write;

use crate::cli::style::Styles;
use crate::config::Config;
use crate::executor::{self, ExecutionReport, InteractiveRunner, StepStatus, UpdateLog};
use crate::model::{ActionPlan, ActionStep};
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
        anyhow::bail!("update needs a terminal to confirm on — use --dry-run to preview the plan");
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

    print!("{}", render_plan(&plan, styles));

    if dry_run || plan.is_empty() {
        if dry_run && parallel {
            println!(
                "  {}",
                styles.dim(&parallel_note(&plan, executor::sudo::detect()))
            );
        }
        return Ok(());
    }
    execute_flow(&plan, parallel, styles)
}

/// The confirm + execute half of a bare `paclens update`: show the exact
/// commands (P1), announce skips, ask `[Y/n]`, run, report every outcome.
fn execute_flow(plan: &ActionPlan, parallel: bool, styles: &Styles) -> anyhow::Result<()> {
    let tool = executor::sudo::detect();

    for step in &plan.steps {
        match executor::skip_reason(step, tool) {
            Some(reason) => println!(
                "  {}",
                styles.dim(&format!(
                    "{} will be skipped — {reason}",
                    step.source_id.as_str()
                ))
            ),
            None => println!(
                "  {} {}",
                styles.dim(will_run_label(parallel, step, tool)),
                executor::effective_command(step, tool).join(" ")
            ),
        }
    }

    // Counted in commands, not packages: nothing was looked up, so there is no
    // package count to offer. Commands rather than sources, because flatpak
    // contributes two steps and the prompt must match what runs (2026-09-17).
    let sources = executor::executable_steps(plan, tool);
    if sources == 0 {
        println!("\n{}", styles.dim("nothing to execute"));
        return Ok(());
    }

    print!(
        "\n{} {} ",
        styles.summary_updates(&format!(
            "Run {sources} command{}?",
            if sources == 1 { "" } else { "s" },
        )),
        styles.dim("[Y/n]")
    );
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if !accepts(&answer) {
        println!("{}", styles.dim("cancelled — nothing executed"));
        return Ok(());
    }

    println!();
    prime_privilege(plan, tool, styles);

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
fn prime_privilege(plan: &ActionPlan, tool: Option<&str>, s: &Styles) {
    if !worth_priming(plan, tool) {
        return;
    }

    let argv = executor::sudo::prime_command();
    println!(
        "  {}",
        s.dim(&format!("{} (once, for the whole run)", argv.join(" ")))
    );
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
    println!();
}

/// Will something in this plan ask for a password? Pure, so the AUR case can
/// be pinned by a test: a helper escalates on its own, so a plan can need a
/// password without carrying a single privileged step.
///
/// sudo only — `doas` and `pkexec` have no "authenticate without running
/// anything" that a later command then finds satisfied.
fn worth_priming(plan: &ActionPlan, tool: Option<&str>) -> bool {
    tool == Some("sudo")
        && plan.steps.iter().any(|st| {
            executor::skip_reason(st, tool).is_none()
                && (executor::needs_privilege(st) || st.source_id.as_str() == "aur")
        })
}

/// What `--dry-run --parallel` owes the reader: which steps would not wait for
/// each other. Pure, so the wording is pinned by a test.
///
/// A plan where nothing qualifies says so rather than printing nothing —
/// `--parallel` on a pacman-only plan changes exactly nothing, and silence
/// would leave that to be guessed at.
fn parallel_note(plan: &ActionPlan, tool: Option<&str>) -> String {
    let labels: Vec<&str> = plan
        .steps
        .iter()
        .filter(|s| executor::runs_in_background(s, tool))
        .map(|s| s.label.as_str())
        .collect();
    if labels.is_empty() {
        "nothing in this plan can run in parallel".to_string()
    } else {
        format!("in parallel: {}", labels.join(", "))
    }
}

/// What the preview line calls a step.
///
/// `--parallel` changes *when* a step runs, so it has to change what the plan
/// says will happen — P1 is "show exactly what will happen", and "these four
/// start at once while pacman has the terminal" is part of that.
fn will_run_label(parallel: bool, step: &ActionStep, tool: Option<&str>) -> &'static str {
    if parallel && executor::runs_in_background(step, tool) {
        "will run (in parallel):"
    } else {
        "will run:"
    }
}

/// Does this answer to `[Y/n]` mean yes?
///
/// Enter means yes (user decision 2026-09-21): the plan is on screen above
/// the prompt, and someone who typed `update` and read it is answering the
/// question they asked. Anything that is not a yes or an empty line is still
/// a refusal — a typo cancels rather than upgrades the system.
fn accepts(answer: &str) -> bool {
    matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "" | "y" | "yes"
    )
}

/// Render the whole plan block. Pure (no IO) so the no-color output is
/// deterministic and unit-testable.
/// The plan: which sources will run and the exact command for each.
///
/// No package list, because none was looked up — the tools report what they
/// change as they change it. P1 asks for the commands, and they are here
/// (user decision 2026-09-17).
fn render_plan(plan: &ActionPlan, s: &Styles) -> String {
    let srcs = plan.source_count();
    let summary = if plan.is_empty() {
        s.summary_ok("no source can be updated")
    } else {
        let src_word = if srcs == 1 { "source" } else { "sources" };
        s.summary_updates(&format!("updating {srcs} {src_word}"))
    };

    let mut out = format!(
        "{} {} {}\n\n",
        s.title("paclens"),
        s.dim(s.bullet()),
        summary
    );
    let label_w = plan
        .steps
        .iter()
        .map(|step| step.label.len())
        .max()
        .unwrap_or(0);
    for step in &plan.steps {
        out.push_str(&format!(
            "  {}  {}\n",
            s.title(&format!("{:label_w$}", step.label)),
            s.dim(&step.command.join(" "))
        ));
    }

    if plan.requires_sudo {
        out.push_str(&format!("\n  {}\n", s.dim("requires sudo")));
    }
    out
}

/// Render the post-execution report: a headline, one line per step
/// (✓ succeeded / ✗ failed / · skipped), and the log path. Pure for the same
/// reason as `render_plan`.
fn render_report(report: &ExecutionReport, s: &Styles) -> String {
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
    out.push_str(&format!(
        "{} {} {}\n\n",
        s.title("paclens"),
        s.dim(s.bullet()),
        headline
    ));

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
    fn the_preview_says_which_steps_run_at_once() {
        let quiet = plan_step("cargo", false, false);
        let asks = plan_step("pacman", true, true);
        let rooted = plan_step("flatpak · system", false, true);

        assert_eq!(will_run_label(false, &quiet, Some("sudo")), "will run:");
        assert_eq!(
            will_run_label(true, &quiet, Some("sudo")),
            "will run (in parallel):"
        );
        assert_eq!(
            will_run_label(true, &asks, Some("sudo")),
            "will run:",
            "a step that asks questions keeps the terminal"
        );
        assert_eq!(
            will_run_label(true, &rooted, Some("sudo")),
            "will run:",
            "design §11 — nothing privileged runs in the background"
        );
    }

    #[test]
    fn dry_run_names_the_steps_that_would_share_the_run() {
        let plan = ActionPlan {
            created_at: Utc::now(),
            steps: vec![
                plan_step("pacman", true, true),
                plan_step("flatpak · user", false, false),
                plan_step("cargo", false, false),
            ],
            requires_sudo: true,
        };
        assert_eq!(
            parallel_note(&plan, Some("sudo")),
            "in parallel: flatpak · user, cargo"
        );

        let alone = ActionPlan {
            created_at: Utc::now(),
            steps: vec![plan_step("pacman", true, true)],
            requires_sudo: true,
        };
        assert_eq!(
            parallel_note(&alone, Some("sudo")),
            "nothing in this plan can run in parallel",
            "silence would leave the reader to guess"
        );
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
                },
                Source {
                    id: SourceId::flatpak(),
                    kind: SourceKind::Flatpak,
                    available: true,
                    last_scanned: None,
                    accurate_updates: true,
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
        let text = render_plan(&plan, &plain());

        assert!(text.starts_with("paclens · updating"), "{text}");
        assert!(text.contains("pacman -Syu"), "{text}");
        assert!(text.contains("flatpak update --user"), "{text}");
        assert!(text.contains("flatpak update --system"), "{text}");
        assert!(
            text.contains("requires sudo"),
            "pacman is in the plan:\n{text}"
        );
        assert!(!text.contains('\u{1b}'), "no ANSI in the plain styler");
    }

    #[test]
    fn a_source_with_nothing_pending_still_gets_its_command() {
        // The whole point: paclens did not check, so it cannot skip a source
        // for having nothing to do. The tool says that itself, faster.
        let s = scan(Vec::new());
        assert!(
            planner::plan_updates(&s, |_| true).is_empty(),
            "the checked plan has nothing to run"
        );
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
        assert!(plan.requires_sudo, "the system half needs it");
        let text = render_plan(&plan, &plain());
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
        let text = render_plan(&plan, &plain());
        assert!(text.contains("no source can be updated"), "{text}");
        assert!(!text.contains("requires sudo"), "{text}");
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
        });
        let plan = planner::plan_full_upgrade(&s, |id| id.as_str() == "aur");

        assert!(
            !plan.requires_sudo,
            "nothing in an AUR plan runs under sudo"
        );
        assert!(worth_priming(&plan, Some("sudo")));
        assert!(!worth_priming(&plan, Some("doas")), "sudo -v is sudo's own");
        assert!(!worth_priming(&plan, None));
    }

    #[test]
    fn a_plan_that_never_escalates_is_not_worth_priming() {
        let s = scan(Vec::new());
        let plan = planner::plan_full_upgrade(&s, |id| id.as_str() == "cargo");
        assert!(!worth_priming(&plan, Some("sudo")));
    }

    // --- the Y/n answer ---
    #[test]
    fn enter_accepts_and_anything_unrecognised_still_refuses() {
        for yes in ["y", "Y", "yes", "YES", " y \n", "", "\n", "  \n"] {
            assert!(accepts(yes), "{yes:?} should accept");
        }
        for no in ["n", "N", "no", "q", "yep", "sure"] {
            assert!(!accepts(no), "{no:?} should refuse");
        }
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
    fn all_green_report_has_no_failed_segment() {
        let r = report(vec![step(SourceId::flatpak(), 1, StepStatus::Succeeded)]);
        let text = render_report(&r, &plain());
        assert!(
            text.contains("1 source ran · 1 succeeded"),
            "headline missing:\n{text}"
        );
        assert!(!text.contains("failed"), "{text}");
        assert!(text.contains("1 flatpak updated"), "{text}");
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
