//! `paclens cleanup` — print the reclaimable-space report to stdout.
//!
//! The headless twin of the TUI cleanup screen, reading the same analyzer
//! output so the two can never disagree about what is reclaimable (P5).
//!
//! **Advisory only.** Every suggestion is copiable text for the reader to run,
//! never something paclens executes — the cleanup screen deliberately has no
//! action keys, and a headless version that quietly gained `--yes` would be a
//! way around that rather than a feature (design §5).

use std::path::Path;

use crate::analyzer::DepGraph;
use crate::cli::style::Styles;
use crate::config::Config;
use crate::format::human_bytes;
use crate::model::ScanResult;
use crate::providers::SystemCommandRunner;
use crate::scanner;

pub fn run(
    config: &Config,
    refresh: bool,
    config_path: Option<&Path>,
    styles: &Styles,
) -> anyhow::Result<()> {
    let runner = SystemCommandRunner::new(config.scan.provider_timeout_secs);
    let scan = scanner::load_or_scan(&runner, config, refresh, config_path)?;
    let graph = DepGraph::build(&scan);
    print!("{}", render_cleanup(&scan, &graph, styles));
    Ok(())
}

/// The whole report. Pure (no IO) so the no-color rendering is deterministic
/// and unit-testable, matching `status::render_status`.
fn render_cleanup(scan: &ScanResult, graph: &DepGraph, s: &Styles) -> String {
    let orphans = graph.orphans(scan);
    let unused: Vec<_> = graph.unused_runtimes(scan);
    let unused_bytes: u64 = unused.iter().filter_map(|p| p.size_bytes).sum();
    let orphan_bytes: u64 = orphans
        .iter()
        .filter_map(|n| {
            scan.packages
                .iter()
                .find(|p| &p.name == n)
                .and_then(|p| p.size_bytes)
        })
        .sum();
    let sizes = &scan.cache_sizes;

    let mut out = String::new();
    // The headline counts things a reader could act on, not bytes: the cache
    // total is mostly current-version tarballs that nothing should remove.
    let actionable = orphans.len() + unused.len();
    let headline = if actionable == 0 {
        s.summary_ok("nothing to clean up")
    } else {
        s.summary_updates(&format!(
            "{actionable} item{} worth reviewing",
            if actionable == 1 { "" } else { "s" }
        ))
    };
    out.push_str(&format!(
        "{} {} {}\n\n",
        s.title("paclens"),
        s.dim(s.bullet()),
        headline
    ));

    // --- sizes ---
    out.push_str(&row(
        "pacman cache",
        &pacman_cache_value(
            sizes.pacman_cache_bytes,
            sizes.pacman_cache_reclaimable_bytes,
            s,
        ),
        s,
    ));
    // Named for the helper in use, not paru — see the same rule in the TUI
    // pane. No helper means no figure at all rather than someone else's.
    if let (Some(b), Some(helper)) = (sizes.aur_cache_bytes, scan.aur_helper.helper()) {
        out.push_str(&row(
            &format!("{} build cache", helper.bin()),
            &human_bytes(b),
            s,
        ));
    }
    out.push_str(&row(
        "unused runtimes",
        &if unused.is_empty() {
            s.dim("none")
        } else {
            format!("{} ({})", unused.len(), human_bytes(unused_bytes))
        },
        s,
    ));
    out.push_str(&row(
        "orphans",
        &if orphans.is_empty() {
            s.dim("none")
        } else {
            format!("{} ({})", orphans.len(), human_bytes(orphan_bytes))
        },
        s,
    ));

    // --- the lists themselves, which the TUI shows on its own rows ---
    if !unused.is_empty() {
        out.push('\n');
        out.push_str(&s.dim("unused runtimes:"));
        out.push('\n');
        for p in &unused {
            let size = p
                .size_bytes
                .map(|b| format!(" ({})", human_bytes(b)))
                .unwrap_or_default();
            out.push_str(&format!("  {} {}{}\n", s.bullet(), p.name, s.dim(&size)));
        }
    }
    if !orphans.is_empty() {
        out.push('\n');
        out.push_str(&s.dim("orphans - installed as dependencies, now required by nothing:"));
        out.push('\n');
        for name in &orphans {
            let size = scan
                .packages
                .iter()
                .find(|p| &p.name == name)
                .and_then(|p| p.size_bytes)
                .map(|b| format!(" ({})", human_bytes(b)))
                .unwrap_or_default();
            out.push_str(&format!("  {} {}{}\n", s.bullet(), name, s.dim(&size)));
        }
    }

    // --- suggestions ---
    let mut suggestions = Vec::new();
    // Only suggest what would actually do something: an 11 GiB cache that
    // reclaims nothing should not carry a command that frees nothing.
    if sizes.pacman_cache_reclaimable_bytes != Some(0) {
        suggestions.push("paccache -rk2".to_string());
    }
    if !unused.is_empty() {
        suggestions.push("flatpak uninstall --unused".to_string());
    }
    if let (Some(_), Some(helper)) = (sizes.aur_cache_bytes, scan.aur_helper.helper()) {
        suggestions.push(helper.clean_command().join(" "));
    }
    if !orphans.is_empty() {
        suggestions.push(format!("sudo pacman -Rns {}", orphans.join(" ")));
    }
    if !suggestions.is_empty() {
        out.push('\n');
        out.push_str(&s.dim("suggested - review, then run yourself:"));
        out.push('\n');
        for c in &suggestions {
            out.push_str(&format!("  {c}\n"));
        }
        if !orphans.is_empty() {
            out.push_str(&s.dim("  (check each orphan first: paclens why <name>)"));
            out.push('\n');
        }
    }
    out
}

/// The pacman cache figure with its honest reclaimable number beside it. A
/// large cache that frees nothing says so rather than implying a win.
fn pacman_cache_value(total: Option<u64>, reclaimable: Option<u64>, s: &Styles) -> String {
    match (total, reclaimable) {
        (Some(b), Some(0)) => format!("{} {}", human_bytes(b), s.dim("(nothing to reclaim)")),
        (Some(b), Some(r)) => format!("{} ({} reclaimable)", human_bytes(b), human_bytes(r)),
        (Some(b), None) => human_bytes(b),
        (None, _) => s.dim("-"),
    }
}

fn row(label: &str, value: &str, s: &Styles) -> String {
    format!("  {} {:16} {value}\n", s.dim(s.bullet()), s.dim(label))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ColorTheme;
    use crate::model::{
        CacheSizes, FlatpakScope, InstallReason, Package, SCHEMA_VERSION, Source, SourceId,
        SourceKind,
    };
    use crate::providers::aur::{AurHelper, HelperChoice};
    use chrono::Utc;

    fn ascii() -> Styles {
        Styles::resolve(true, ColorTheme::Dark, true)
    }

    fn pkg(name: &str, source: SourceId, reason: InstallReason, size: Option<u64>) -> Package {
        Package {
            name: name.to_string(),
            version: "1".to_string(),
            source_id: source,
            install_reason: reason,
            size_bytes: size,
            description: None,
            depends_on: Vec::new(),
            required_by: Vec::new(),
            optional_deps: Vec::new(),
            provides: Vec::new(),
            runtime: false,
        }
    }

    fn scan(packages: Vec<Package>, sizes: CacheSizes) -> ScanResult {
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
                    id: SourceId::flatpak_user(),
                    kind: SourceKind::Flatpak {
                        scope: FlatpakScope::User,
                    },
                    available: true,
                    last_scanned: None,
                    accurate_updates: true,
                },
            ],
            packages,
            updates: Vec::new(),
            cache_sizes: sizes,
            flatpak_profile_sizes: Default::default(),
            profile_dir_sizes: Default::default(),
            aur_helper: HelperChoice::Detected(AurHelper::Paru),
            kernel: None,
        }
    }

    fn render(scan: &ScanResult) -> String {
        let graph = DepGraph::build(scan);
        render_cleanup(scan, &graph, &ascii())
    }

    #[test]
    fn a_clean_system_says_so_and_suggests_nothing_destructive() {
        let s = scan(
            vec![pkg(
                "bash",
                SourceId::pacman(),
                InstallReason::Explicit,
                Some(100),
            )],
            CacheSizes {
                pacman_cache_bytes: Some(11_000_000_000),
                pacman_cache_reclaimable_bytes: Some(0),
                ..Default::default()
            },
        );
        let out = render(&s);
        assert!(out.contains("nothing to clean up"), "{out}");
        assert!(out.contains("(nothing to reclaim)"), "{out}");
        // A cache that frees nothing must not carry a command that frees
        // nothing (design §3 — no misleading numbers, and no busywork).
        assert!(!out.contains("paccache"), "{out}");
        assert!(!out.contains("pacman -Rns"), "{out}");
    }

    /// An orphan is a package installed as a dependency that nothing now
    /// requires. It is listed with its size and a removal command, but the
    /// command is text — and it points at `why` first.
    #[test]
    fn orphans_are_listed_with_sizes_and_a_why_first_suggestion() {
        let s = scan(
            vec![
                pkg(
                    "leftover",
                    SourceId::pacman(),
                    InstallReason::Dependency,
                    Some(2048),
                ),
                pkg("bash", SourceId::pacman(), InstallReason::Explicit, None),
            ],
            CacheSizes::default(),
        );
        let out = render(&s);
        assert!(out.contains("1 item worth reviewing"), "{out}");
        assert!(out.contains("leftover"), "{out}");
        assert!(out.contains("2.00 KiB"), "size missing:\n{out}");
        assert!(out.contains("sudo pacman -Rns leftover"), "{out}");
        assert!(out.contains("paclens why"), "why hint missing:\n{out}");
    }

    /// The build-cache row and its clean command follow the detected helper,
    /// exactly as the TUI pane does — and vanish entirely without one.
    #[test]
    fn the_build_cache_row_follows_the_helper() {
        let sizes = CacheSizes {
            aur_cache_bytes: Some(9_000_000_000),
            ..Default::default()
        };
        let mut s = scan(Vec::new(), sizes.clone());
        s.aur_helper = HelperChoice::Detected(AurHelper::Yay);
        let out = render(&s);
        assert!(out.contains("yay build cache"), "{out}");
        assert!(out.contains("yay -Sc --aur"), "{out}");
        assert!(!out.contains("paru"), "{out}");

        let mut s = scan(Vec::new(), sizes);
        s.aur_helper = HelperChoice::None;
        let out = render(&s);
        assert!(!out.contains("build cache"), "{out}");
        assert!(!out.contains("-Sc"), "{out}");
    }

    /// Nothing here may run anything. The report is copiable text, matching
    /// the cleanup screen's deliberate lack of action keys.
    #[test]
    fn every_suggestion_is_text_the_reader_runs() {
        let s = scan(
            vec![pkg(
                "leftover",
                SourceId::pacman(),
                InstallReason::Dependency,
                None,
            )],
            CacheSizes {
                pacman_cache_bytes: Some(1000),
                pacman_cache_reclaimable_bytes: Some(500),
                ..Default::default()
            },
        );
        let out = render(&s);
        assert!(out.contains("review, then run yourself"), "{out}");
        assert!(
            !out.contains("--noconfirm"),
            "a suggestion must still prompt:\n{out}"
        );
    }

    /// `--no-color` output carries no ANSI escapes at all — it is what gets
    /// piped into a file or another program.
    #[test]
    fn no_color_output_is_plain_ascii() {
        let s = scan(
            vec![pkg(
                "leftover",
                SourceId::pacman(),
                InstallReason::Dependency,
                Some(2048),
            )],
            CacheSizes {
                pacman_cache_bytes: Some(1000),
                pacman_cache_reclaimable_bytes: Some(500),
                aur_cache_bytes: Some(50),
                ..Default::default()
            },
        );
        let out = render(&s);
        assert!(!out.contains('\u{1b}'), "ANSI escape in --no-color output");
        assert!(out.is_ascii(), "non-ASCII in --no-color output:\n{out}");
    }
}
