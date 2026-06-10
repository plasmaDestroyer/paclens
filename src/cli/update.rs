//! `paclens update [--dry-run] [--source <id>]` — show the update plan (spec
//! §11.3). v0.0.5 is dry-run only: it prints exactly what would update and
//! executes nothing (execution arrives in v0.0.6).
//!
//! The plan is built by the shared `crate::planner` and rendered through the
//! shared `Styles`, so the CLI and the TUI update screen never disagree (P5).

use std::path::Path;

use crate::cli::style::Styles;
use crate::config::Config;
use crate::model::{ActionPlan, ScanResult};
use crate::providers::SystemCommandRunner;
use crate::{planner, scanner};

pub fn run(
    config: &Config,
    refresh: bool,
    config_path: Option<&Path>,
    dry_run: bool,
    source: Option<&str>,
    styles: &Styles,
) -> anyhow::Result<()> {
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

    if !dry_run {
        println!(
            "{}",
            styles.dim("(execution arrives in v0.0.6 — re-run with --dry-run to preview)")
        );
    }
    Ok(())
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
}
