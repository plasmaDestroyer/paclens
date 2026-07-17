//! `paclens migrate <name>` — the migration advisory report (roadmap v0.4)
//! and, with `--run`, its execution (roadmap v0.5).
//!
//! The report is read-only. `--run` executes exactly the copy plan it just
//! showed — backups first, `cp -aT` per pair, never an `rm` — then prints the
//! rollback instructions and offers the source-side removal behind its own
//! `[y/N]` after the user has verified the target works.

use std::io::Write;
use std::path::Path;

use anyhow::Context;

use crate::analyzer;
use crate::cli::style::Styles;
use crate::config::Config;
use crate::executor::{self, InteractiveRunner, UpdateLog};
use crate::format::human_bytes;
use crate::model::{ActionPlan, Direction, MigrationReport, OverlapCandidate, PathKind};
use crate::planner;
use crate::providers::SystemCommandRunner;
use crate::scanner;

#[allow(clippy::too_many_arguments)]
pub fn run(
    config: &Config,
    refresh: bool,
    config_path: Option<&Path>,
    name: &str,
    direction: Option<Direction>,
    execute: bool,
    stdin_is_tty: bool,
    styles: &Styles,
) -> anyhow::Result<()> {
    let runner = SystemCommandRunner::new(config.scan.provider_timeout_secs);
    let scan = scanner::load_or_scan(&runner, config, refresh, config_path)?;
    let overlaps = analyzer::detect_overlaps(
        &scan,
        &config.overlap.ignore,
        &config.overlap.extra_mappings,
    );
    let Some(candidate) = find(&overlaps, name) else {
        if overlaps.is_empty() {
            anyhow::bail!(
                "no overlaps detected — `migrate` reports on apps installed both natively and as a Flatpak"
            );
        }
        anyhow::bail!(
            "no overlap matches `{name}` — detected: {}",
            overlaps
                .iter()
                .map(|o| o.display_name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    };
    let report =
        analyzer::migrate::report(&scan, candidate, &config.overlap.extra_mappings, direction);
    print!("{}", render_report(&report, candidate, styles));
    if !execute {
        return Ok(());
    }
    execute_flow(&report, candidate, stdin_is_tty, styles)
}

/// The v0.5 execution half: plan → confirm → copy → rollback block → offer
/// the source removal in-flow (user decision 2026-07-14). P4 intact — the
/// exact commands print before anything runs.
fn execute_flow(
    report: &MigrationReport,
    candidate: &OverlapCandidate,
    stdin_is_tty: bool,
    styles: &Styles,
) -> anyhow::Result<()> {
    let home = directories::BaseDirs::new()
        .context("cannot locate a home directory")?
        .home_dir()
        .to_path_buf();
    let backup_dir = planner::migration_backup_dir(&home, candidate);
    let plan = planner::plan_migration(report, candidate, &home, &backup_dir);
    if plan.is_empty() {
        println!(
            "\n{}",
            styles.dim("nothing to copy — the source side has no data")
        );
        return Ok(());
    }
    if !stdin_is_tty {
        anyhow::bail!("`migrate --run` needs an interactive terminal");
    }

    print!("{}", render_exec_plan(&plan, &backup_dir, styles));
    print!(
        "{} {} ",
        styles.summary_updates("Run these commands?"),
        styles.dim("[y/N]")
    );
    std::io::stdout().flush()?;
    if !accepts(&read_answer()?) {
        println!("{}", styles.dim("cancelled — nothing executed"));
        return Ok(());
    }

    println!();
    let mut log = UpdateLog::open_default()?;
    // No privilege tool: migration copy steps never escalate.
    let exec = executor::execute(&plan, &InteractiveRunner, &mut log, None);
    println!();
    print!("{}", render_rollback(report, &backup_dir, styles));
    if exec.failed() > 0 {
        anyhow::bail!(
            "{} of {} steps failed — nothing was deleted; see {}",
            exec.failed(),
            exec.executed(),
            exec.log_path.display()
        );
    }

    // In-flow removal offer (roadmap: user confirms after verifying).
    let Some(removal) = planner::plan_removal(report, candidate) else {
        return Ok(());
    };
    let tool = executor::sudo::detect();
    let removal_cmd = executor::effective_command(&removal.steps[0], tool).join(" ");
    println!(
        "\n{}",
        styles.summary_updates(&format!(
            "now launch {} on the {} side and verify your data is there",
            report.display_name,
            report.direction.target()
        ))
    );
    print!(
        "{} {} ",
        styles.warn(&format!("remove the source side (`{removal_cmd}`)?")),
        styles.dim("[y/N]")
    );
    std::io::stdout().flush()?;
    if !accepts(&read_answer()?) {
        println!(
            "{}",
            styles.dim(&format!("kept — remove later with: {removal_cmd}"))
        );
        return Ok(());
    }
    if executor::skip_reason(&removal.steps[0], tool).is_some() {
        anyhow::bail!("no privilege tool found (sudo/doas/pkexec) — cannot remove");
    }
    println!();
    let exec = executor::execute(&removal, &InteractiveRunner, &mut log, tool);
    if exec.failed() > 0 {
        anyhow::bail!("removal failed — see {}", exec.log_path.display());
    }
    println!(
        "{}",
        styles.success(&format!("done — backup kept at {}", backup_dir.display()))
    );
    Ok(())
}

fn read_answer() -> anyhow::Result<String> {
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(answer)
}

/// Does this answer to `[y/N]` mean yes? Default is no.
fn accepts(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// The exact commands `--run` will execute (P1), pure for testing.
fn render_exec_plan(plan: &ActionPlan, backup_dir: &Path, s: &Styles) -> String {
    let mut out = String::new();
    out.push_str(&format!("\n{}\n", s.title("execution plan")));
    out.push_str(&format!(
        "  {}\n",
        s.dim(&format!("backups land in {}", backup_dir.display()))
    ));
    for step in &plan.steps {
        out.push_str(&format!("  $ {}\n", step.command.join(" ")));
    }
    out.push_str(&format!(
        "  {}\n",
        s.dim("no step deletes anything — source data stays put")
    ));
    out
}

/// The rollback block shown after the copy run, pure for testing.
fn render_rollback(report: &MigrationReport, backup_dir: &Path, s: &Styles) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{}\n",
        s.title("rollback (if anything looks wrong)")
    ));
    for line in planner::rollback_lines(report, backup_dir) {
        out.push_str(&format!("  $ {line}\n"));
    }
    out
}

/// Match by display name, native package name, or Flatpak app id.
fn find<'a>(overlaps: &'a [OverlapCandidate], name: &str) -> Option<&'a OverlapCandidate> {
    let n = name.to_lowercase();
    overlaps.iter().find(|o| {
        o.display_name.to_lowercase() == n
            || o.native_package
                .as_ref()
                .is_some_and(|p| p.name.to_lowercase() == n)
            || o.flatpak_app
                .as_ref()
                .is_some_and(|p| p.name.to_lowercase() == n)
    })
}

fn render_report(r: &MigrationReport, candidate: &OverlapCandidate, s: &Styles) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{} {} {} {}\n",
        s.title("paclens"),
        s.dim(s.bullet()),
        s.summary_updates(&format!(
            "migrate {} {} {}",
            r.display_name,
            s.arrow(),
            r.direction.target()
        )),
        s.dim(&format!("[{}]", r.confidence)),
    ));

    if r.mappings.is_empty() {
        out.push_str(&format!(
            "\n{}\n",
            s.dim("no profile data found on either side — nothing to migrate")
        ));
        return out;
    }

    out.push('\n');
    for m in &r.mappings {
        let (from, from_bytes, to, to_bytes) = m.endpoints(r.direction);
        let mut from_part = from.to_string();
        if let Some(b) = from_bytes {
            from_part.push_str(&format!("  {}", human_bytes(b)));
        } else {
            from_part.push_str(&format!("  {}", s.dim("(not present)")));
        }
        from_part.push_str(&format!("  {}", s.dim(&format!("[{}]", m.confidence))));
        out.push_str(&field(s, &m.kind.to_string(), &from_part));

        let to_part = if m.kind == PathKind::Cache {
            s.dim("(skip — cache regenerates)")
        } else if let Some(b) = to_bytes {
            format!("{to}  {}", s.warn(&format!("(exists, {})", human_bytes(b))))
        } else {
            to.to_string()
        };
        out.push_str(&field(s, "", &format!("{} {to_part}", s.arrow())));
    }

    out.push_str(&format!("\n{}\n", s.title("manual steps")));
    let mut n = 1;
    let mut step = |text: &str, out: &mut String| {
        out.push_str(&format!("  {}. {text}\n", n));
        n += 1;
    };
    step(&format!("close {} everywhere", r.display_name), &mut out);
    for m in &r.mappings {
        let (from, from_bytes, to, _) = m.endpoints(r.direction);
        if m.kind == PathKind::Cache || from_bytes.is_none() {
            continue;
        }
        step(&format!("cp -aT {from} {to}"), &mut out);
    }
    step(
        &format!("launch the {} side and verify your data is there", {
            r.direction.target()
        }),
        &mut out,
    );
    step(&removal_hint(r.direction, candidate), &mut out);

    out.push('\n');
    for w in &r.warnings {
        out.push_str(&format!("  {} {w}\n", s.warn("!")));
    }
    out.push_str(&format!(
        "\n{}\n",
        s.dim("advisory only — paclens copies and removes nothing")
    ));
    out
}

/// The final step: what removing the *source* side would look like — phrased
/// as something to consider, never a command paclens runs.
fn removal_hint(direction: Direction, candidate: &OverlapCandidate) -> String {
    match direction {
        Direction::ToFlatpak => {
            let name = candidate
                .native_package
                .as_ref()
                .map_or("<native package>", |p| p.name.as_str());
            format!(
                "only then consider removing the native package (run `paclens why {name}` first)"
            )
        }
        Direction::ToNative => {
            let id = candidate
                .flatpak_app
                .as_ref()
                .map_or("<app-id>", |p| p.name.as_str());
            format!("only then consider `flatpak uninstall {id}`")
        }
    }
}

fn field(s: &Styles, label: &str, value: &str) -> String {
    format!(
        "  {}   {value}\n",
        s.dim(&format!(
            "{:9}",
            format!("{label}{}", if label.is_empty() { "" } else { ":" })
        ))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ColorTheme;
    use crate::model::{
        Confidence, MatchMethod, PackageRef, PathMapping, PrimaryHeuristic, SourceId, Tradeoff,
    };

    fn plain() -> Styles {
        Styles::resolve(false, ColorTheme::Dark, false)
    }

    fn candidate() -> OverlapCandidate {
        OverlapCandidate {
            display_name: "Firefox".to_string(),
            native_package: Some(PackageRef {
                name: "firefox".to_string(),
                version: "141.0-1".to_string(),
                source_id: SourceId::pacman(),
            }),
            flatpak_app: Some(PackageRef {
                name: "org.mozilla.firefox".to_string(),
                version: "141.0".to_string(),
                source_id: SourceId::flatpak_user(),
            }),
            match_method: MatchMethod::KnownMap,
            confidence: Confidence::Confirmed,
            tradeoff: Tradeoff {
                likely_primary: PrimaryHeuristic::Unknown,
                ..Tradeoff::default()
            },
        }
    }

    fn mapping(
        kind: PathKind,
        native: &str,
        flatpak: &str,
        native_bytes: Option<u64>,
        flatpak_bytes: Option<u64>,
        confidence: Confidence,
    ) -> PathMapping {
        PathMapping {
            kind,
            native: native.to_string(),
            flatpak: flatpak.to_string(),
            native_bytes,
            flatpak_bytes,
            confidence,
        }
    }

    fn report(mappings: Vec<PathMapping>) -> MigrationReport {
        MigrationReport {
            display_name: "Firefox".to_string(),
            direction: Direction::ToFlatpak,
            confidence: Confidence::Confirmed,
            mappings,
            warnings: vec!["close Firefox everywhere before copying anything".to_string()],
        }
    }

    #[test]
    fn report_renders_rows_steps_and_warnings() {
        let r = report(vec![
            mapping(
                PathKind::Profile,
                "~/.mozilla",
                "~/.var/app/org.mozilla.firefox/.mozilla",
                Some(1_200_000_000),
                None,
                Confidence::Confirmed,
            ),
            mapping(
                PathKind::Cache,
                "~/.cache/firefox",
                "~/.var/app/org.mozilla.firefox/cache/firefox",
                Some(340_000_000),
                None,
                Confidence::Inferred,
            ),
        ]);
        let text = render_report(&r, &candidate(), &plain());
        assert!(text.contains("migrate Firefox → flatpak"), "{text}");
        assert!(text.contains("[confirmed]"), "{text}");
        assert!(text.contains("~/.mozilla  1.12 GiB"), "{text}");
        assert!(text.contains("(skip — cache regenerates)"), "{text}");
        // Steps: close, one cp (cache skipped), verify, removal hint.
        assert!(text.contains("1. close Firefox everywhere"), "{text}");
        assert!(
            text.contains("2. cp -aT ~/.mozilla ~/.var/app/org.mozilla.firefox/.mozilla"),
            "{text}"
        );
        assert!(!text.contains("cp -aT ~/.cache/firefox"), "{text}");
        assert!(text.contains("3. launch the flatpak side"), "{text}");
        assert!(text.contains("paclens why firefox"), "{text}");
        assert!(text.contains("! close Firefox"), "{text}");
        assert!(text.contains("advisory only"), "{text}");
    }

    #[test]
    fn existing_target_data_is_flagged_inline() {
        let r = report(vec![mapping(
            PathKind::Profile,
            "~/.mozilla",
            "~/.var/app/org.mozilla.firefox/.mozilla",
            Some(1_200_000_000),
            Some(300_000_000),
            Confidence::Confirmed,
        )]);
        let text = render_report(&r, &candidate(), &plain());
        assert!(text.contains("(exists, 286.10 MiB)"), "{text}");
    }

    #[test]
    fn to_native_swaps_endpoints_and_removal_hint() {
        let mut r = report(vec![mapping(
            PathKind::Config,
            "~/.config/vlc",
            "~/.var/app/org.videolan.VLC/config/vlc",
            None,
            Some(2_048),
            Confidence::Inferred,
        )]);
        r.direction = Direction::ToNative;
        let text = render_report(&r, &candidate(), &plain());
        assert!(
            text.contains("cp -aT ~/.var/app/org.videolan.VLC/config/vlc ~/.config/vlc"),
            "{text}"
        );
        assert!(
            text.contains("flatpak uninstall org.mozilla.firefox"),
            "{text}"
        );
    }

    #[test]
    fn empty_mappings_short_circuit() {
        let text = render_report(&report(Vec::new()), &candidate(), &plain());
        assert!(text.contains("nothing to migrate"), "{text}");
        assert!(!text.contains("manual steps"), "{text}");
    }

    #[test]
    fn exec_plan_prints_every_command_and_the_no_delete_promise() {
        let r = report(vec![mapping(
            PathKind::Profile,
            "~/.mozilla",
            "~/.var/app/org.mozilla.firefox/.mozilla",
            Some(1_000),
            Some(500),
            Confidence::Confirmed,
        )]);
        let plan = crate::planner::plan_migration(
            &r,
            &candidate(),
            Path::new("/home/t"),
            Path::new("/home/t/backups/ff/1"),
        );
        let text = render_exec_plan(&plan, Path::new("/home/t/backups/ff/1"), &plain());
        assert!(text.contains("execution plan"), "{text}");
        assert!(
            text.contains("backups land in /home/t/backups/ff/1"),
            "{text}"
        );
        assert!(text.contains("$ mkdir -p /home/t/backups/ff/1"), "{text}");
        assert!(
            text.contains(
                "$ cp -aT /home/t/.var/app/org.mozilla.firefox/.mozilla /home/t/backups/ff/1/0-.mozilla"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "$ cp -aT /home/t/.mozilla /home/t/.var/app/org.mozilla.firefox/.mozilla"
            ),
            "{text}"
        );
        assert!(text.contains("no step deletes anything"), "{text}");
    }

    #[test]
    fn rollback_block_lists_the_restore_commands() {
        let r = report(vec![mapping(
            PathKind::Profile,
            "~/.mozilla",
            "~/.var/app/org.mozilla.firefox/.mozilla",
            Some(1_000),
            Some(500),
            Confidence::Confirmed,
        )]);
        let text = render_rollback(&r, Path::new("/b"), &plain());
        assert!(text.contains("rollback"), "{text}");
        assert!(
            text.contains(
                "$ rm -rf ~/.var/app/org.mozilla.firefox/.mozilla && cp -aT /b/0-.mozilla ~/.var/app/org.mozilla.firefox/.mozilla"
            ),
            "{text}"
        );
    }

    #[test]
    fn accepts_only_yes_answers() {
        assert!(accepts("y\n"));
        assert!(accepts("YES\n"));
        assert!(!accepts("\n"));
        assert!(!accepts("n\n"));
        assert!(!accepts("maybe\n"));
    }

    #[test]
    fn backup_dir_uses_the_native_name() {
        let dir = crate::planner::migration_backup_dir(Path::new("/home/t"), &candidate());
        let s = dir.display().to_string();
        assert!(
            s.starts_with("/home/t/.local/share/paclens/backups/firefox/"),
            "{s}"
        );
    }

    #[test]
    fn find_matches_any_of_the_three_names() {
        let overlaps = vec![candidate()];
        assert!(find(&overlaps, "firefox").is_some());
        assert!(find(&overlaps, "Firefox").is_some());
        assert!(find(&overlaps, "org.mozilla.firefox").is_some());
        assert!(find(&overlaps, "chromium").is_none());
    }
}
