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
    all: bool,
    styles: &Styles,
) -> anyhow::Result<()> {
    let runner = SystemCommandRunner::new(config.scan.provider_timeout_secs);
    let scan = scanner::load_or_scan(&runner, config, refresh, config_path)?;
    let graph = DepGraph::build(&scan);
    print!(
        "{}",
        render_cleanup_with(
            &scan,
            &graph,
            styles,
            &config.cleanup.diff_prog,
            &config.cleanup.orphan_ignore,
            all,
        )
    );
    Ok(())
}

/// How many names a list shows before "… n more" — `--all` lifts it.
const SHOWN: usize = 5;

/// The whole report: one block per finding, its figure on the first line and
/// what to run right under it, so a command never sits far from the thing it
/// fixes. Every finding gets its first line even when the answer is "none" —
/// a report that only speaks up when something is wrong leaves you wondering
/// whether it looked. Pure (no IO), so the plain rendering is testable.
fn render_cleanup_with(
    scan: &ScanResult,
    graph: &DepGraph,
    s: &Styles,
    diff_prog: &str,
    orphan_ignore: &[String],
    all: bool,
) -> String {
    use crate::analyzer::pacfiles;
    let pacfiles = pacfiles::review_order(&scan.pacfiles);
    let stale = crate::analyzer::stale_units(&scan.stale_processes);
    let unowned = crate::analyzer::provenance::unowned(&scan.packages);
    let diff = pacfiles::diff_program(diff_prog, std::env::var("DIFFPROG").ok().as_deref());
    let size_of = |name: &str| {
        scan.packages
            .iter()
            .find(|p| p.name == name)
            .and_then(|p| p.size_bytes)
    };
    // Largest first: size is what makes an orphan worth a look.
    let mut orphans: Vec<(String, Option<u64>)> = graph
        .orphans_ignoring(scan, orphan_ignore)
        .into_iter()
        .map(|n| {
            let size = size_of(&n);
            (n, size)
        })
        .collect();
    orphans.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let orphan_bytes: u64 = orphans.iter().filter_map(|(_, b)| *b).sum();
    let mut unused: Vec<_> = graph.unused_runtimes(scan);
    unused.sort_by(|a, b| {
        b.size_bytes
            .cmp(&a.size_bytes)
            .then_with(|| a.name.cmp(&b.name))
    });
    let unused_bytes: u64 = unused.iter().filter_map(|p| p.size_bytes).sum();
    let sizes = &scan.cache_sizes;

    let limit = if all { usize::MAX } else { SHOWN };
    let more = |total: usize| (total > limit).then(|| s.dim(&format!("+{} more", total - limit)));
    let mut out = String::new();
    let mut findings = 0;
    let head = |out: &mut String, label: &str, value: String| {
        out.push_str(&format!("{} {value}\n", s.title(&format!("{label:16}"))));
    };
    let detail = |out: &mut String, line: String| out.push_str(&format!("  {line}\n"));
    let command = |out: &mut String, cmd: String| out.push_str(&format!("  $ {cmd}\n"));

    if !all {
        return compact_report(
            CompactInput {
                cache: sizes,
                helper: scan.aur_helper.helper(),
                orphans: (orphans.len(), orphan_bytes),
                unused: (unused.len(), unused_bytes),
                pacfiles: pacfiles.len(),
                stale: &stale,
                unowned: unowned.len(),
                review: pacfiles::review_all_command(&diff),
            },
            s,
        );
    }
    // --- caches ---
    head(
        &mut out,
        "pacman cache",
        pacman_cache_value(
            sizes.pacman_cache_bytes,
            sizes.pacman_cache_reclaimable_bytes,
            s,
        ),
    );
    // Only suggest what would do something: an 11 GiB cache that reclaims
    // nothing carries no command.
    if sizes.pacman_cache_reclaimable_bytes.is_some_and(|b| b > 0) {
        findings += 1;
        command(&mut out, "paccache -rk3".to_string());
    }
    // Named for the helper in use, not paru. No helper, no figure.
    if let (Some(b), Some(helper)) = (sizes.aur_cache_bytes, scan.aur_helper.helper()) {
        findings += 1;
        head(&mut out, &format!("{} cache", helper.bin()), human_bytes(b));
        command(&mut out, helper.clean_command().join(" "));
    }

    // --- orphans ---
    if orphans.is_empty() {
        head(&mut out, "orphans", s.dim("none"));
    } else {
        findings += 1;
        head(
            &mut out,
            "orphans",
            format!(
                "{} {} {}",
                orphans.len(),
                s.bullet(),
                human_bytes(orphan_bytes)
            ),
        );
        let mut names: Vec<String> = orphans
            .iter()
            .take(limit)
            .map(|(n, b)| match b {
                Some(b) => format!("{n} {}", s.dim(&human_bytes(*b))),
                None => n.clone(),
            })
            .collect();
        names.extend(more(orphans.len()));
        detail(&mut out, names.join(", "));
        // Per item, never batched (design §3): read why, then remove one.
        command(&mut out, "paclens why <name>".to_string());
    }

    // --- unused runtimes ---
    if unused.is_empty() {
        head(&mut out, "unused runtimes", s.dim("none"));
    } else {
        findings += 1;
        head(
            &mut out,
            "unused runtimes",
            format!(
                "{} {} {}",
                unused.len(),
                s.bullet(),
                human_bytes(unused_bytes)
            ),
        );
        let mut names: Vec<String> = unused.iter().take(limit).map(|p| p.name.clone()).collect();
        names.extend(more(unused.len()));
        detail(&mut out, names.join(", "));
        command(&mut out, "flatpak uninstall --unused".to_string());
    }

    // --- config leftovers ---
    if pacfiles.is_empty() {
        head(&mut out, "config files", s.dim("none"));
    } else {
        findings += 1;
        head(
            &mut out,
            "config files",
            format!(
                "{} {}",
                pacfiles.len(),
                s.dim("left beside your config by upgrades")
            ),
        );
        for f in pacfiles.iter().take(limit) {
            detail(&mut out, format!("{} {}", f.base(), s.dim(f.kind.label())));
        }
        if let Some(m) = more(pacfiles.len()) {
            detail(&mut out, m);
        }
        command(&mut out, pacfiles::review_all_command(&diff));
    }

    // --- stale services ---
    if stale.is_empty() {
        head(&mut out, "stale services", s.dim("none"));
    } else {
        findings += 1;
        head(
            &mut out,
            "stale services",
            format!(
                "{} {} {}",
                stale.len(),
                s.dim("running replaced files"),
                s.dim("[inferred]")
            ),
        );
        let mut relog = Vec::new();
        for u in &stale {
            let file = u
                .files
                .first()
                .map(|f| s.dim(&format!(" {} {f} replaced", s.bullet())))
                .unwrap_or_default();
            detail(
                &mut out,
                format!(
                    "{} {}{file}",
                    u.unit,
                    s.dim(&format!("({})", u.processes.join(", ")))
                ),
            );
            // A scope cannot be restarted, and a session-critical service
            // must not become a casual command (design §3).
            match u.restart_command() {
                Some(cmd) => command(&mut out, cmd),
                None => relog.push(u.unit.as_str()),
            }
        }
        if !relog.is_empty() {
            detail(
                &mut out,
                s.dim(&format!("log out and back in for {}", relog.join(", "))),
            );
        }
        detail(
            &mut out,
            s.dim("yours only; system services need root to inspect"),
        );
    }

    // --- no repository ---
    if unowned.is_empty() {
        head(&mut out, "no repository", s.dim("none"));
    } else {
        findings += 1;
        head(
            &mut out,
            "no repository",
            format!(
                "{} {}",
                unowned.len(),
                s.dim("in no configured repo, so nothing updates them")
            ),
        );
        for (who, n) in crate::analyzer::provenance::by_packager(&unowned) {
            detail(&mut out, format!("{n} packaged by {who}"));
        }
        let mut names: Vec<String> = unowned.iter().take(limit).map(|p| p.name.clone()).collect();
        names.extend(more(unowned.len()));
        detail(&mut out, s.dim(&names.join(", ")));
    }

    let headline = if findings == 0 {
        s.summary_ok("nothing to clean up")
    } else {
        s.summary_updates(&format!(
            "{findings} finding{}",
            if findings == 1 { "" } else { "s" }
        ))
    };
    format!(
        "{} {} {}\n\n{out}",
        s.title("paclens"),
        s.dim(s.bullet()),
        headline
    )
}

/// What the one-line-per-finding report needs.
struct CompactInput<'a> {
    cache: &'a crate::model::CacheSizes,
    helper: Option<crate::providers::aur::AurHelper>,
    orphans: (usize, u64),
    unused: (usize, u64),
    pacfiles: usize,
    stale: &'a [crate::analyzer::StaleUnit],
    unowned: usize,
    review: String,
}

/// The default report: one line per finding — the figure, then the command
/// that acts on it. Clean findings are left out; `--all` lists everything.
fn compact_report(c: CompactInput, s: &Styles) -> String {
    let mut rows: Vec<(String, String, String)> = Vec::new();
    if let Some(b) = c.cache.pacman_cache_bytes {
        let (value, cmd) = match c.cache.pacman_cache_reclaimable_bytes {
            Some(r) if r > 0 => (
                format!("{} {} {} free", human_bytes(b), s.bullet(), human_bytes(r)),
                "paccache -rk3".to_string(),
            ),
            _ => (human_bytes(b), String::new()),
        };
        rows.push(("pacman cache".into(), value, cmd));
    }
    if let (Some(b), Some(h)) = (c.cache.aur_cache_bytes, c.helper) {
        rows.push((
            format!("{} cache", h.bin()),
            human_bytes(b),
            h.clean_command().join(" "),
        ));
    }
    if c.orphans.0 > 0 {
        rows.push((
            "orphans".into(),
            format!(
                "{} {} {}",
                c.orphans.0,
                s.bullet(),
                human_bytes(c.orphans.1)
            ),
            "paclens why <name>".into(),
        ));
    }
    if c.unused.0 > 0 {
        rows.push((
            "runtimes".into(),
            format!(
                "{} unused {} {}",
                c.unused.0,
                s.bullet(),
                human_bytes(c.unused.1)
            ),
            "flatpak uninstall --unused".into(),
        ));
    }
    if c.pacfiles > 0 {
        rows.push(("config files".into(), c.pacfiles.to_string(), c.review));
    }
    if !c.stale.is_empty() {
        let restartable = c
            .stale
            .iter()
            .filter(|u| u.restart_command().is_some())
            .count();
        let cmd = match restartable {
            0 => "log out and back in".to_string(),
            n => format!("{n} restart{} in --all", if n == 1 { "" } else { "s" }),
        };
        rows.push(("services".into(), format!("{} stale", c.stale.len()), cmd));
    }
    if c.unowned > 0 {
        rows.push(("no repository".into(), c.unowned.to_string(), String::new()));
    }
    // Everything but the cache totals is something to act on.
    let findings = rows.iter().filter(|r| !r.2.is_empty()).count();
    let mut out = if findings == 0 {
        s.summary_ok("nothing to clean up")
    } else {
        s.summary_updates(&format!("{findings} to review"))
    };
    out.push('\n');
    let value_w = rows.iter().map(|r| r.1.chars().count()).max().unwrap_or(0);
    for (label, value, cmd) in &rows {
        let pad = value_w - value.chars().count();
        let line = format!(
            "  {} {value}{}  {cmd}",
            s.dim(&format!("{label:14}")),
            " ".repeat(pad),
        );
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out.push_str(&s.dim("  paclens cleanup --all for the lists\n"));
    out
}

/// The pacman cache figure with its honest reclaimable number beside it. A
/// large cache that frees nothing says so rather than implying a win.
fn pacman_cache_value(total: Option<u64>, reclaimable: Option<u64>, s: &Styles) -> String {
    match (total, reclaimable) {
        (Some(b), Some(0)) => format!(
            "{} {}",
            human_bytes(b),
            s.dim(&format!("{} nothing to reclaim", s.bullet()))
        ),
        (Some(b), Some(r)) => format!(
            "{} {} {} reclaimable",
            human_bytes(b),
            s.bullet(),
            human_bytes(r)
        ),
        (Some(b), None) => human_bytes(b),
        (None, _) => s.dim("-"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ColorTheme;
    use crate::model::{
        CacheSizes, InstallReason, Package, SCHEMA_VERSION, Source, SourceId, SourceKind,
    };
    use crate::providers::aur::{AurHelper, HelperChoice};
    use chrono::Utc;

    fn ascii() -> Styles {
        Styles::resolve(true, ColorTheme::Dark, true)
    }

    fn pkg(name: &str, source: SourceId, reason: InstallReason, size: Option<u64>) -> Package {
        Package {
            repo_version: None,
            scope: None,
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
            foreign: false,
            signed: true,
            packager: None,
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
            packages,
            updates: Vec::new(),
            cache_sizes: sizes,
            flatpak_profile_sizes: Default::default(),
            profile_dir_sizes: Default::default(),
            aur_helper: HelperChoice::Detected(AurHelper::Paru),
            kernel: None,
            pacfiles: Vec::new(),
            stale_processes: Vec::new(),
        }
    }

    fn render(scan: &ScanResult) -> String {
        let graph = DepGraph::build(scan);
        render_cleanup_with(scan, &graph, &ascii(), "", &[], true)
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
        assert!(out.contains("nothing to reclaim"), "{out}");
        // A cache that frees nothing must not carry a command that frees
        // nothing (design §3 — no misleading numbers, and no busywork).
        assert!(!out.contains("paccache"), "{out}");
        assert!(!out.contains("pacman -Rns"), "{out}");
    }

    /// An orphan is a package installed as a dependency that nothing now
    /// requires. It is listed with its size and points at `why` for review.
    #[test]
    fn packages_with_no_repository_are_named_with_who_shipped_them() {
        let mut scan = scan(Vec::new(), CacheSizes::default());
        let mut from_cachyos = pkg(
            "cachyos-hello",
            SourceId::pacman(),
            InstallReason::Explicit,
            None,
        );
        from_cachyos.foreign = true;
        from_cachyos.signed = true;
        from_cachyos.packager = Some("CachyOS <admin@cachyos.org>".to_string());
        let mut from_aur = pkg(
            "antigravity",
            SourceId::aur(),
            InstallReason::Explicit,
            None,
        );
        from_aur.foreign = true;
        from_aur.signed = false;
        scan.packages = vec![
            from_cachyos,
            from_aur,
            pkg("firefox", SourceId::pacman(), InstallReason::Explicit, None),
        ];

        let out = render(&scan);
        assert!(out.contains("no repository"), "row missing:\n{out}");
        // The packager is the explanation: 1 name is a mystery, "packaged by
        // CachyOS" is an answer.
        assert!(out.contains("packaged by CachyOS"), "{out}");
        assert!(out.contains("cachyos-hello"), "{out}");
        // A package built here is an AUR package, not a stranded one.
        assert!(!out.contains("antigravity"), "AUR package listed:\n{out}");
    }

    #[test]
    fn a_machine_with_all_its_repos_says_none() {
        let scan = scan(Vec::new(), CacheSizes::default());
        let out = render(&scan);
        assert!(out.contains("no repository"), "the row must exist:\n{out}");
        assert!(!out.contains("packaged by"), "nothing to explain:\n{out}");
    }

    #[test]
    fn stale_services_are_inferred_and_session_critical_ones_are_not_suggested() {
        use crate::analyzer::services::{StaleProcess, UnitScope};
        let mut scan = scan(Vec::new(), CacheSizes::default());
        scan.stale_processes = vec![
            StaleProcess {
                pid: 1,
                comm: "pipewire".to_string(),
                unit: Some("pipewire.service".to_string()),
                scope: Some(UnitScope::User),
                file: "/usr/lib/libc.so.6".to_string(),
            },
            StaleProcess {
                pid: 2,
                comm: "Hyprland".to_string(),
                unit: Some("session-9.scope".to_string()),
                scope: Some(UnitScope::User),
                file: "/usr/lib/libc.so.6".to_string(),
            },
        ];
        let out = render(&scan);
        assert!(out.contains("stale services"), "row missing:\n{out}");
        assert!(out.contains("[inferred]"), "label missing:\n{out}");
        assert!(out.contains("pipewire.service"), "{out}");
        // The file is named, which is what `checkservices` cannot tell you.
        assert!(out.contains(" replaced"), "no file named:\n{out}");
        assert!(out.contains("/usr/lib/libc.so.6"), "{out}");
        // The one that would log you out is listed, warned about, and left
        // out of the commands.
        assert!(
            out.contains("log out and back in"),
            "advice missing:\n{out}"
        );
        assert!(
            out.contains("systemctl --user restart pipewire.service"),
            "suggestion missing:\n{out}"
        );
        assert!(
            !out.contains("restart session-9.scope"),
            "a command that logs you out must not be suggested:\n{out}"
        );
        // And it says what it could not see.
        assert!(out.contains("need root to inspect"), "{out}");
    }

    #[test]
    fn config_leftovers_are_listed_with_their_base_and_a_copiable_command() {
        use crate::analyzer::pacfiles::{PacFile, PacFileKind};
        let mut scan = scan(Vec::new(), CacheSizes::default());
        scan.pacfiles = vec![
            PacFile {
                path: "/etc/pacman.conf.pacnew".to_string(),
                kind: PacFileKind::Pacnew,
                modified_secs: Some(200),
            },
            PacFile {
                path: "/etc/locale.gen.pacnew".to_string(),
                kind: PacFileKind::Pacnew,
                modified_secs: Some(100),
            },
        ];
        let graph = DepGraph::build(&scan);
        let out = render_cleanup_with(&scan, &graph, &ascii(), "meld", &[], true);
        assert!(out.contains("config files"), "row missing:\n{out}");
        // Listed by the config they sit next to, not by the leftover's name.
        assert!(out.contains("/etc/pacman.conf "), "base missing:\n{out}");
        assert!(out.contains("pacnew"), "kind missing:\n{out}");
        // Newest first.
        let first = out.find("/etc/pacman.conf").expect("listed");
        let second = out.find("/etc/locale.gen").expect("listed");
        assert!(first < second, "not newest-first:\n{out}");
        // Copiable text, and nothing that merges anything by itself.
        assert!(
            out.contains("sudo DIFFPROG=meld pacdiff"),
            "suggestion missing:\n{out}"
        );
        assert!(!out.contains("--noconfirm"), "{out}");
    }

    #[test]
    fn no_leftovers_still_gets_a_row() {
        let scan = scan(Vec::new(), CacheSizes::default());
        let out = render(&scan);
        assert!(out.contains("stale services"), "row missing:\n{out}");
        assert!(
            out.contains("config files     none"),
            "the row must say none rather than vanish:\n{out}"
        );
        assert!(!out.contains("pacdiff"), "nothing to suggest:\n{out}");
    }

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
        assert!(out.contains("1 finding"), "{out}");
        assert!(out.contains("leftover"), "{out}");
        assert!(out.contains("2.00 KiB"), "size missing:\n{out}");
        assert!(!out.contains("sudo pacman -Rns"), "{out}");
        assert!(out.contains("paclens why"), "why hint missing:\n{out}");
    }

    #[test]
    fn orphan_ignore_applies_to_cli_count_and_advice() {
        let scan = scan(
            vec![pkg(
                "keep-me",
                SourceId::pacman(),
                InstallReason::Dependency,
                Some(2048),
            )],
            CacheSizes::default(),
        );
        let graph = DepGraph::build(&scan);
        let out = render_cleanup_with(&scan, &graph, &ascii(), "", &["keep-me".to_string()], true);
        assert!(out.contains("orphans"), "{out}");
        assert!(
            !out.contains("keep-me") && !out.contains("paclens why"),
            "{out}"
        );
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
        assert!(out.contains("yay cache"), "{out}");
        assert!(out.contains("yay -Sc --aur"), "{out}");
        assert!(!out.contains("paru"), "{out}");

        let mut s = scan(Vec::new(), sizes);
        s.aur_helper = HelperChoice::None;
        let out = render(&s);
        assert!(
            !out.contains("yay cache") && !out.contains("paru cache"),
            "{out}"
        );
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
        assert!(out.contains("  $ "), "{out}");
        assert!(
            !out.contains("--noconfirm"),
            "a suggestion must still prompt:\n{out}"
        );
    }

    /// `--no-color` output carries no ANSI escapes at all — it is what gets
    /// piped into a file or another program.
    #[test]
    fn the_default_report_is_one_line_per_finding_with_its_command() {
        let mut scan = scan(
            Vec::new(),
            CacheSizes {
                pacman_cache_bytes: Some(1000),
                pacman_cache_reclaimable_bytes: Some(500),
                ..CacheSizes::default()
            },
        );
        scan.pacfiles = vec![crate::analyzer::PacFile {
            path: "/etc/pacman.conf.pacnew".to_string(),
            kind: crate::analyzer::pacfiles::PacFileKind::Pacnew,
            modified_secs: Some(1),
        }];
        let graph = DepGraph::build(&scan);
        let out = render_cleanup_with(&scan, &graph, &ascii(), "meld", &[], false);
        assert!(out.starts_with("2 to review\n"), "{out}");
        let line = |label: &str| {
            out.lines()
                .find(|l| l.trim_start().starts_with(label))
                .unwrap_or_default()
                .to_string()
        };
        assert!(line("pacman cache").ends_with("paccache -rk3"), "{out}");
        assert!(
            line("config files").ends_with("sudo DIFFPROG=meld pacdiff"),
            "{out}"
        );
        // Clean findings say nothing; the lists wait for --all.
        assert!(!out.contains("orphans"), "{out}");
        assert!(!out.contains("/etc/pacman.conf"), "{out}");
        assert!(out.contains("--all"), "{out}");
    }

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
