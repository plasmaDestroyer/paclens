//! `paclens update [--dry-run] [--source <id>]` — show the update plan (spec
//! §11.3) and, since v0.0.6, execute it after a y/N confirmation. v0.0.6 runs
//! Flatpak user-scope only; everything needing sudo is reported as skipped.
//!
//! The plan is built by the shared `crate::planner` and executed by the shared
//! `crate::executor`, so the CLI and the TUI can never disagree (P5). The
//! pipeline is intact (P4): the full plan prints before the prompt, nothing
//! runs without an explicit `y`, and the per-source report hides nothing.

use std::io::Write;
use std::path::Path;

use crate::cli::style::Styles;
use crate::config::Config;
use crate::executor::{self, ExecutionReport, InteractiveRunner, StepStatus, UpdateLog};
use crate::model::{ActionPlan, ScanResult};
use crate::providers::SystemCommandRunner;
use crate::{planner, scanner};

pub fn run(
    config: &Config,
    refresh: bool,
    config_path: Option<&Path>,
    dry_run: bool,
    source: Option<&str>,
    stdin_is_tty: bool,
    styles: &Styles,
) -> anyhow::Result<()> {
    // Executing needs an interactive confirmation, so fail fast (before the
    // scan) when there is no terminal to ask on. Scripts get --dry-run.
    if !dry_run && !stdin_is_tty {
        anyhow::bail!("update needs a terminal to confirm on — use --dry-run to preview the plan");
    }

    let runner = SystemCommandRunner;
    let scan = scanner::load_or_scan(&runner, config, refresh, config_path)?;

    if let Some(requested) = source
        && !scan.sources.iter().any(|s| s.id.as_str() == requested)
    {
        let known: Vec<&str> = scan.sources.iter().map(|s| s.id.as_str()).collect();
        anyhow::bail!(
            "unknown source {requested:?}; known sources: {}",
            known.join(", ")
        );
    }

    let plan = planner::plan_updates(&scan, |id| match source {
        Some(requested) => id.as_str() == requested,
        None => true,
    });

    print!("{}", render_plan(&plan, &scan, styles));

    if dry_run || plan.is_empty() {
        return Ok(());
    }
    execute_flow(&plan, styles)
}

/// The confirm + execute half of a bare `paclens update`: announce what will
/// be skipped, ask `[y/N]`, run the plan, and report every outcome.
fn execute_flow(plan: &ActionPlan, styles: &Styles) -> anyhow::Result<()> {
    for step in &plan.steps {
        if let Some(reason) = executor::skip_reason(step) {
            println!(
                "  {}",
                styles.dim(&format!(
                    "{} will be skipped — {reason}",
                    step.source_id.as_str()
                ))
            );
        }
    }

    let apps = executor::executable_targets(plan);
    if apps == 0 {
        println!("\n{}", styles.dim("nothing to execute"));
        return Ok(());
    }

    print!(
        "\n{} {} ",
        styles.summary_updates(&format!(
            "Update {apps} Flatpak app{}?",
            if apps == 1 { "" } else { "s" }
        )),
        styles.dim("[y/N]")
    );
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if !accepts(&answer) {
        println!("{}", styles.dim("cancelled — nothing executed"));
        return Ok(());
    }

    println!();
    let mut log = UpdateLog::open_default()?;
    let report = executor::execute(plan, &InteractiveRunner, &mut log);

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

/// Does this answer to `[y/N]` mean yes? Default (empty / anything else) is no.
fn accepts(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Render the whole plan block. Pure (no IO) so the no-color output is
/// deterministic and unit-testable.
fn render_plan(plan: &ActionPlan, scan: &ScanResult, s: &Styles) -> String {
    let total = plan.total_targets();
    let summary = if total == 0 {
        s.summary_ok("nothing to update")
    } else {
        let pkgs = if total == 1 { "package" } else { "packages" };
        let srcs = plan.source_count();
        let src_word = if srcs == 1 { "source" } else { "sources" };
        s.summary_updates(&format!(
            "{total} {pkgs} will update across {srcs} {src_word}"
        ))
    };

    let mut out = String::new();
    out.push_str(&format!(
        "{} {} {}\n",
        s.title("paclens"),
        s.dim(s.bullet()),
        summary
    ));

    for step in &plan.steps {
        let ups: Vec<_> = scan
            .updates
            .iter()
            .filter(|u| u.source_id == step.source_id)
            .collect();
        let name_w = ups.iter().map(|u| u.package_name.len()).max().unwrap_or(0);

        out.push('\n');
        out.push_str(&format!(
            "  {}  {}\n",
            s.title(step.source_id.as_str()),
            s.dim(&format!("({})", ups.len()))
        ));
        for u in ups {
            out.push_str(&format!(
                "     {:name_w$}  {} {} {}\n",
                u.package_name,
                s.dim(&u.current_version),
                s.dim(s.arrow()),
                s.summary_updates(&u.available_version),
            ));
        }
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
        .map(|st| st.source_id.as_str().len())
        .max()
        .unwrap_or(0);
    for st in &report.steps {
        let name = format!("{:name_w$}", st.source_id.as_str());
        let line = match &st.status {
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
        CacheSizes, FlatpakScope, PendingUpdate, SCHEMA_VERSION, Source, SourceId, SourceKind,
    };
    use chrono::Utc;

    /// Piped styler: Unicode glyphs, no ANSI — deterministic for assertions.
    fn plain() -> Styles {
        Styles::resolve(false, ColorTheme::Dark, false)
    }

    fn ascii() -> Styles {
        Styles::resolve(true, ColorTheme::Dark, true)
    }

    fn upd(name: &str, cur: &str, new: &str, source: SourceId) -> PendingUpdate {
        PendingUpdate {
            package_name: name.to_string(),
            current_version: cur.to_string(),
            available_version: new.to_string(),
            source_id: source,
        }
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
                },
                Source {
                    id: SourceId::flatpak_user(),
                    kind: SourceKind::Flatpak {
                        scope: FlatpakScope::User,
                    },
                    available: true,
                    last_scanned: None,
                },
            ],
            packages: Vec::new(),
            updates,
            cache_sizes: CacheSizes::default(),
        }
    }

    #[test]
    fn renders_summary_groups_and_version_transitions() {
        let s = scan(vec![
            upd("linux", "6.9.1", "6.9.2", SourceId::pacman()),
            upd("firefox", "127.0", "127.0.1", SourceId::pacman()),
            upd("org.gimp.GIMP", "2.10", "2.10.1", SourceId::flatpak_user()),
        ]);
        let plan = planner::plan_updates(&s, |_| true);
        let text = render_plan(&plan, &s, &plain());

        assert!(text.starts_with("paclens · 3 packages will update across 2 sources"));
        assert!(text.contains("pacman"));
        assert!(text.contains("flatpak-user"));
        assert!(text.contains("linux"));
        assert!(text.contains("6.9.1 → 6.9.2"));
        assert!(text.contains("requires sudo")); // pacman in plan
        assert!(!text.contains('\u{1b}')); // no ANSI in the plain styler
    }

    #[test]
    fn ascii_styler_uses_ascii_arrow() {
        let s = scan(vec![upd("linux", "6.9.1", "6.9.2", SourceId::pacman())]);
        let plan = planner::plan_updates(&s, |_| true);
        let text = render_plan(&plan, &s, &ascii());
        assert!(text.contains("6.9.1 -> 6.9.2"));
    }

    #[test]
    fn singular_package_and_source_wording() {
        let s = scan(vec![upd(
            "org.gimp.GIMP",
            "2.10",
            "2.10.1",
            SourceId::flatpak_user(),
        )]);
        let plan = planner::plan_updates(&s, |_| true);
        let text = render_plan(&plan, &s, &plain());
        assert!(text.starts_with("paclens · 1 package will update across 1 source"));
        // flatpak-user only → no sudo note.
        assert!(!text.contains("requires sudo"));
    }

    #[test]
    fn empty_plan_says_nothing_to_update() {
        let s = scan(Vec::new());
        let plan = planner::plan_updates(&s, |_| true);
        let text = render_plan(&plan, &s, &plain());
        assert!(text.contains("nothing to update"));
        assert!(!text.contains("requires sudo"));
    }

    // --- the y/N answer ---
    #[test]
    fn only_y_and_yes_accept_case_insensitively() {
        for yes in ["y", "Y", "yes", "YES", " y \n"] {
            assert!(accepts(yes), "{yes:?} should accept");
        }
        for no in ["", "\n", "n", "N", "no", "q", "yep", "sure"] {
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
        StepReport {
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
            step(SourceId::flatpak_user(), 2, StepStatus::Succeeded),
            step(
                SourceId::flatpak_system(),
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
        assert!(
            text.contains("✓ flatpak-user    2 apps updated"),
            "success line missing:\n{text}"
        );
        assert!(
            text.contains("✗ flatpak-system  failed (exit 1)"),
            "failure line missing:\n{text}"
        );
        assert!(
            text.contains("pacman          skipped — execution arrives in v0.1"),
            "skip line missing:\n{text}"
        );
        assert!(
            text.contains("log: /tmp/paclens/2026-06-12.log"),
            "log path missing:\n{text}"
        );
        assert!(!text.contains('\u{1b}'));
    }

    #[test]
    fn all_green_report_has_no_failed_segment() {
        let r = report(vec![step(
            SourceId::flatpak_user(),
            1,
            StepStatus::Succeeded,
        )]);
        let text = render_report(&r, &plain());
        assert!(
            text.contains("1 source ran · 1 succeeded"),
            "headline missing:\n{text}"
        );
        assert!(!text.contains("failed"), "{text}");
        assert!(text.contains("1 app updated"), "{text}");
    }

    #[test]
    fn ascii_report_uses_the_ascii_marks() {
        let r = report(vec![
            step(SourceId::flatpak_user(), 2, StepStatus::Succeeded),
            step(
                SourceId::flatpak_system(),
                1,
                StepStatus::Failed {
                    detail: "exit 1".to_string(),
                },
            ),
        ]);
        let text = render_report(&r, &ascii());
        assert!(text.contains("x flatpak-user"), "{text}");
        assert!(text.contains("! flatpak-system"), "{text}");
        assert!(!text.contains('✓'));
        assert!(!text.contains('✗'));
    }
}
