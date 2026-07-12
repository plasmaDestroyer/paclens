//! `paclens why <package>` — print the dependency explanation (spec §11.4).
//!
//! Reads the scan cache, builds the graph, and renders the analyzer's
//! `WhyReport` through the shared `Styles` so the CLI and the TUI why pane
//! can never disagree (P5). Advisory only — prints, never acts.

use std::path::Path;

use crate::analyzer::{self, DepGraph, Verdict, WhyDetail, WhyReport, tree_lines};
use crate::cli::style::Styles;
use crate::config::Config;
use crate::model::{InstallReason, SourceId};
use crate::providers::SystemCommandRunner;
use crate::scanner;

pub fn run(
    config: &Config,
    refresh: bool,
    config_path: Option<&Path>,
    package: &str,
    styles: &Styles,
) -> anyhow::Result<()> {
    let runner = SystemCommandRunner::new(config.scan.provider_timeout_secs);
    let scan = scanner::load_or_scan(&runner, config, refresh, config_path)?;
    let graph = DepGraph::build(&scan);
    let report = analyzer::why(&scan, &graph, package, config.why.max_depth);

    match &report {
        WhyReport::NotFound {
            package,
            suggestion,
        } => {
            let hint = match suggestion {
                Some(s) => format!(" — did you mean {s:?}?"),
                None => String::new(),
            };
            anyhow::bail!("no installed package named {package:?}{hint}");
        }
        _ => {
            print!(
                "{}",
                render_report(&report, config.why.show_transitive, styles)
            );
            Ok(())
        }
    }
}

/// Render a found report (any source — unified in v0.1.3). Pure for
/// testability.
fn render_report(report: &WhyReport, show_transitive: bool, s: &Styles) -> String {
    match report {
        WhyReport::Found(p) => render_detail(p, show_transitive, s),
        WhyReport::NotFound { .. } => String::new(), // handled by the caller
    }
}

fn render_detail(p: &WhyDetail, show_transitive: bool, s: &Styles) -> String {
    let is_pacman = p.source_id == SourceId::pacman();
    let mut out = String::new();
    out.push_str(&format!("{}\n", s.title(&p.package)));
    out.push_str(&field(s, "source", &p.source_id.to_string()));

    let reason = if !is_pacman {
        if p.runtime {
            "flatpak runtime — shared by the apps that depend on it".to_string()
        } else {
            "flatpak app (self-contained)".to_string()
        }
    } else {
        match p.reason {
            InstallReason::Explicit => "explicitly installed".to_string(),
            InstallReason::Dependency => match p.depth_from_explicit {
                Some(d) => format!(
                    "installed as a dependency ({d} hop{} from an explicit install)",
                    if d == 1 { "" } else { "s" }
                ),
                None => "installed as a dependency (nothing explicit reaches it)".to_string(),
            },
            InstallReason::Unknown => "unknown".to_string(),
        }
    };
    out.push_str(&field(s, "reason", &reason));
    if !is_pacman && !p.runtime {
        out.push_str(&field(
            s,
            "removal",
            &format!("flatpak uninstall {}", p.package),
        ));
    }

    out.push_str(&field(s, "required by", &name_list(&p.required_by, s)));
    if show_transitive && !p.tree.is_empty() {
        out.push_str(&field(s, "chain", ""));
        for line in tree_lines(&p.tree, s.tree_glyphs()) {
            let more = if line.truncated > 0 {
                s.dim(&format!("  … {} more", line.truncated))
            } else {
                String::new()
            };
            out.push_str(&format!(
                "    {}{} {}{more}\n",
                s.dim(&line.prefix),
                line.name,
                s.dim(&format!("[{}]", line.confidence)),
            ));
        }
        if p.transitive_required_by.len() > p.required_by.len() {
            out.push_str(&format!(
                "    {}\n",
                s.dim(&format!(
                    "{} packages depend on this in total",
                    p.transitive_required_by.len()
                ))
            ));
        }
    }
    out.push_str(&field(
        s,
        "would also remove",
        &name_list(&p.would_remove, s),
    ));
    if !p.required_by.is_empty() {
        out.push_str(&field(s, "would break", &name_list(&p.required_by, s)));
    }

    let verdict = match p.verdict {
        Verdict::LikelySafe => s.summary_ok(&p.verdict.to_string()),
        Verdict::IsADependency => s.summary_updates(&p.verdict.to_string()),
        Verdict::Unclear => s.error(&p.verdict.to_string()),
    };
    out.push_str(&format!(
        "  {}   {} {}\n",
        s.dim(&format!("{:18}", "verdict:")),
        verdict,
        s.dim(&format!("[{}]", p.confidence))
    ));
    out
}

/// `  label:   value` with the label dimmed and aligned.
fn field(s: &Styles, label: &str, value: &str) -> String {
    format!(
        "  {}   {value}\n",
        s.dim(&format!("{:18}", format!("{label}:")))
    )
}

/// Comma list, capped at 8 names with a dim "… n more"; "nothing" when empty.
fn name_list(names: &[String], s: &Styles) -> String {
    if names.is_empty() {
        return "nothing".to_string();
    }
    let shown: Vec<&str> = names.iter().take(8).map(|n| n.as_str()).collect();
    let mut out = shown.join(", ");
    if names.len() > 8 {
        out.push_str(&s.dim(&format!(" … {} more", names.len() - 8)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::TreeNode;
    use crate::config::ColorTheme;
    use crate::model::{Confidence, SourceId};

    fn plain() -> Styles {
        Styles::resolve(false, ColorTheme::Dark, false)
    }

    fn base() -> WhyDetail {
        WhyDetail {
            package: "firefox".to_string(),
            source_id: SourceId::pacman(),
            runtime: false,
            reason: InstallReason::Explicit,
            required_by: Vec::new(),
            transitive_required_by: Vec::new(),
            depth_from_explicit: Some(0),
            would_remove: Vec::new(),
            tree: Vec::new(),
            verdict: Verdict::LikelySafe,
            confidence: Confidence::Confirmed,
        }
    }

    #[test]
    fn explicit_safe_report_matches_the_spec_shape() {
        let text = render_report(&WhyReport::Found(base()), true, &plain());
        assert!(text.starts_with("firefox\n"), "{text}");
        assert!(text.contains("source:"), "{text}");
        assert!(text.contains("explicitly installed"), "{text}");
        assert!(text.contains("required by"), "{text}");
        assert!(text.contains("nothing"), "{text}");
        assert!(text.contains("likely safe [confirmed]"), "{text}");
        assert!(!text.contains('\u{1b}'));
    }

    #[test]
    fn dependency_report_shows_depth_breakage_and_yellow_verdict() {
        let p = WhyDetail {
            package: "glibc".to_string(),
            reason: InstallReason::Dependency,
            required_by: vec!["firefox".to_string(), "readline".to_string()],
            transitive_required_by: vec![
                "bash".to_string(),
                "firefox".to_string(),
                "readline".to_string(),
            ],
            depth_from_explicit: Some(1),
            would_remove: Vec::new(),
            tree: vec![
                TreeNode {
                    name: "firefox".to_string(),
                    confidence: Confidence::Confirmed,
                    children: Vec::new(),
                    truncated: 0,
                },
                TreeNode {
                    name: "readline".to_string(),
                    confidence: Confidence::Confirmed,
                    children: vec![TreeNode {
                        name: "bash".to_string(),
                        confidence: Confidence::Confirmed,
                        children: Vec::new(),
                        truncated: 0,
                    }],
                    truncated: 0,
                },
            ],
            verdict: Verdict::IsADependency,
            confidence: Confidence::Confirmed,
            ..base()
        };
        let text = render_report(&WhyReport::Found(p), true, &plain());
        assert!(
            text.contains("installed as a dependency (1 hop from an explicit install)"),
            "{text}"
        );
        assert!(text.contains("firefox, readline"), "{text}");
        assert!(text.contains("chain:"), "{text}");
        assert!(text.contains("├─ firefox [confirmed]"), "{text}");
        assert!(text.contains("└─ readline [confirmed]"), "{text}");
        assert!(
            text.contains("   └─ bash [confirmed]"),
            "nested indent missing:\n{text}"
        );
        assert!(
            text.contains("3 packages depend on this in total"),
            "{text}"
        );
        assert!(text.contains("would break"), "{text}");
        assert!(text.contains("is a dependency [confirmed]"), "{text}");
    }

    #[test]
    fn chain_respects_the_config_toggle() {
        let p = WhyDetail {
            required_by: vec!["a".to_string()],
            transitive_required_by: vec!["a".to_string(), "b".to_string()],
            tree: vec![TreeNode {
                name: "a".to_string(),
                confidence: Confidence::Confirmed,
                children: Vec::new(),
                truncated: 0,
            }],
            verdict: Verdict::IsADependency,
            ..base()
        };
        let on = render_report(&WhyReport::Found(p.clone()), true, &plain());
        let off = render_report(&WhyReport::Found(p), false, &plain());
        assert!(on.contains("chain:"), "{on}");
        assert!(!off.contains("chain:"), "{off}");
    }

    #[test]
    fn unclear_report_wears_the_unknown_label() {
        let p = WhyDetail {
            reason: InstallReason::Unknown,
            verdict: Verdict::Unclear,
            confidence: Confidence::Unknown,
            ..base()
        };
        let text = render_report(&WhyReport::Found(p), true, &plain());
        assert!(
            text.contains("unclear — check manually [unknown]"),
            "{text}"
        );
    }

    #[test]
    fn long_lists_are_capped_with_a_more_marker() {
        let names: Vec<String> = (0..12).map(|i| format!("pkg{i}")).collect();
        let text = name_list(&names, &plain());
        assert!(text.contains("pkg7"), "{text}");
        assert!(!text.contains("pkg8,"), "{text}");
        assert!(text.contains("… 4 more"), "{text}");
    }

    #[test]
    fn flatpak_app_report_says_self_contained_with_uninstall_hint() {
        let p = WhyDetail {
            package: "org.gnome.Calculator".to_string(),
            source_id: SourceId::flatpak_user(),
            reason: InstallReason::Unknown,
            would_remove: vec!["org.gnome.Platform".to_string()],
            ..base()
        };
        let text = render_report(&WhyReport::Found(p), true, &plain());
        assert!(text.contains("flatpak app (self-contained)"), "{text}");
        assert!(
            text.contains("flatpak uninstall org.gnome.Calculator"),
            "{text}"
        );
        assert!(text.contains("flatpak-user"), "{text}");
        assert!(text.contains("org.gnome.Platform"), "{text}");
        assert!(!text.contains("unknown"), "no unclear leak: {text}");
    }

    #[test]
    fn flatpak_runtime_report_shows_inferred_dependents() {
        let p = WhyDetail {
            package: "org.gnome.Platform".to_string(),
            source_id: SourceId::flatpak_user(),
            runtime: true,
            reason: InstallReason::Unknown,
            required_by: vec!["org.gnome.Calculator".to_string()],
            tree: vec![TreeNode {
                name: "org.gnome.Calculator".to_string(),
                confidence: Confidence::Inferred,
                children: Vec::new(),
                truncated: 0,
            }],
            verdict: Verdict::IsADependency,
            confidence: Confidence::Inferred,
            ..base()
        };
        let text = render_report(&WhyReport::Found(p), true, &plain());
        assert!(text.contains("flatpak runtime"), "{text}");
        assert!(
            text.contains("org.gnome.Calculator [inferred]"),
            "tree edge label missing: {text}"
        );
        assert!(text.contains("is a dependency [inferred]"), "{text}");
        assert!(
            !text.contains("flatpak uninstall"),
            "no hint for runtimes: {text}"
        );
    }
}
