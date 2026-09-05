//! The `why` query (design §8, roadmap v0.0.7): why is this package installed,
//! what happens if it is removed, with a cautious verdict.
//!
//! Pure — reads only the scan and the pre-built graph. Decision recorded in
//! design §13: **zero reverse deps over confirmed data ⇒ `likely safe`
//! regardless of install reason** ('s canonical example shows an
//! explicit leaf as likely safe; §7.3's dependency-only wording lost). When
//! data is incomplete the verdict is `unclear` — never guessed (P2/P3).

use crate::analyzer::DepGraph;
use crate::fuzzy;
use crate::model::{Confidence, InstallReason, ScanResult, SourceId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    LikelySafe,
    IsADependency,
    Unclear,
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Verdict::LikelySafe => "likely safe",
            Verdict::IsADependency => "is a dependency",
            Verdict::Unclear => "unclear — check manually",
        })
    }
}

/// The two shapes a `why` answer takes (unified across sources in v0.1.3);
/// an unknown name gets a fuzzy suggestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhyReport {
    Found(WhyDetail),
    NotFound {
        package: String,
        suggestion: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhyDetail {
    pub package: String,
    pub source_id: SourceId,
    /// Flatpak runtime (never true for pacman or flatpak apps).
    pub runtime: bool,
    pub reason: InstallReason,
    /// Direct reverse deps — removing the package breaks these.
    pub required_by: Vec<String>,
    /// Everything transitively above (capped at `max_depth`).
    pub transitive_required_by: Vec<String>,
    /// Hops up to the nearest explicitly installed package (0 = itself).
    pub depth_from_explicit: Option<u32>,
    /// Direct deps that would be orphaned (their only requirer is this).
    pub would_remove: Vec<String>,
    /// Reverse-dep chain as a tree ( / roadmap v0.1.2): each root is
    /// a direct requirer, children are *their* requirers, every edge labeled.
    pub tree: Vec<TreeNode>,
    /// Source-specific warnings (roadmap v0.3): AUR PKGBUILD review, VCS
    /// build-snapshot versions. Rendered verbatim by the CLI and the TUI.
    pub caveats: Vec<String>,
    pub verdict: Verdict,
    pub confidence: Confidence,
}

/// One node of the reverse-dependency tree. `confidence` labels the edge from
/// the parent (pacman edges Confirmed, flatpak app → runtime edges Inferred).
/// `truncated` counts children hidden by the branch cap — shown as
/// "… n more", never silently dropped (P3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeNode {
    pub name: String,
    pub confidence: Confidence,
    pub children: Vec<TreeNode>,
    pub truncated: usize,
}

/// Display caps: the tree explains, it does not enumerate — the transitive
/// list already carries the full set.
const TREE_DEPTH: u32 = 3;
const TREE_BRANCH: usize = 5;

/// Build the reverse-dep tree upward from `name`, cycle-safe along each path.
fn reverse_tree(graph: &DepGraph, name: &str, depth: u32, path: &mut Vec<String>) -> Vec<TreeNode> {
    if depth == 0 {
        return Vec::new();
    }
    let requirers = graph.required_by_edges(name);
    let total = requirers.len();
    path.push(name.to_string());
    let mut nodes: Vec<TreeNode> = Vec::new();
    for (r, confidence) in requirers {
        if nodes.len() == TREE_BRANCH {
            break;
        }
        if path.contains(&r) {
            continue; // cycle back-edge
        }
        let children = reverse_tree(graph, &r, depth - 1, path);
        nodes.push(TreeNode {
            name: r,
            confidence,
            children,
            truncated: 0,
        });
    }
    path.pop();
    if total > TREE_BRANCH
        && let Some(last) = nodes.last_mut()
    {
        last.truncated = total - TREE_BRANCH;
    }
    if total > TREE_BRANCH && nodes.is_empty() {
        // Everything was a cycle back-edge; nothing to attach the count to.
        return nodes;
    }
    nodes
}

/// One renderable tree row: the drawing prefix (built from the caller's
/// glyphs), the package name, the edge confidence, and how many siblings were
/// hidden after this node ("… n more").
pub struct TreeLine {
    pub prefix: String,
    pub name: String,
    pub confidence: Confidence,
    pub truncated: usize,
}

/// Flatten a reverse-dep tree into drawable rows. `glyphs` = (branch, last,
/// pipe, blank), so the CLI and the TUI render the exact same structure with
/// their own character sets (P5).
pub fn tree_lines(nodes: &[TreeNode], glyphs: (&str, &str, &str, &str)) -> Vec<TreeLine> {
    let mut out = Vec::new();
    flatten(nodes, "", glyphs, &mut out);
    out
}

fn flatten(nodes: &[TreeNode], indent: &str, g: (&str, &str, &str, &str), out: &mut Vec<TreeLine>) {
    let (branch, last, pipe, blank) = g;
    for (i, node) in nodes.iter().enumerate() {
        let is_last = i + 1 == nodes.len();
        out.push(TreeLine {
            prefix: format!("{indent}{}", if is_last { last } else { branch }),
            name: node.name.clone(),
            confidence: node.confidence,
            truncated: node.truncated,
        });
        let child_indent = format!("{indent}{}", if is_last { blank } else { pipe });
        flatten(&node.children, &child_indent, g, out);
    }
}

/// AUR-specific warnings (roadmap v0.3). Advisory wording only — never
/// blocks anything.
fn caveats_for(pkg: &crate::model::Package) -> Vec<String> {
    let mut out = Vec::new();
    if pkg.source_id != SourceId::aur() {
        return out;
    }
    out.push("AUR package — review PKGBUILD changes before updating".to_string());
    const VCS: [&str; 5] = ["-git", "-svn", "-hg", "-bzr", "-cvs"];
    if VCS.iter().any(|s| pkg.name.ends_with(s)) {
        out.push(
            "VCS package — the version is a build snapshot; enable scan.aur_devel \
             to check against the upstream HEAD"
                .to_string(),
        );
    }
    out
}

pub fn why(scan: &ScanResult, graph: &DepGraph, name: &str, max_depth: u32) -> WhyReport {
    let Some(pkg) = scan.packages.iter().find(|p| p.name == name) else {
        let suggestion = fuzzy::best_match(name, scan.packages.iter().map(|p| p.name.as_str()))
            .map(|s| s.to_string());
        return WhyReport::NotFound {
            package: name.to_string(),
            suggestion,
        };
    };

    // AUR packages are libalpm too: real install reasons, real dep data —
    // the pacman verdict rules apply to them (v0.3).
    let is_alpm = crate::analyzer::graph::is_alpm(&pkg.source_id);
    let edges = graph.required_by_edges(name);
    let required_by: Vec<String> = edges.iter().map(|(n, _)| n.clone()).collect();
    let would_remove: Vec<String> = graph
        .requires(name)
        .into_iter()
        .filter(|dep| {
            // Orphanable: pacman dependency-installed, or a flatpak runtime.
            let dep_orphanable = scan.packages.iter().any(|p| {
                &p.name == dep && (p.install_reason == InstallReason::Dependency || p.runtime)
            });
            dep_orphanable && graph.required_by(dep) == vec![name.to_string()]
        })
        .collect();

    // Conservative verdicts (P2). pacman with an unknown install reason means
    // incomplete data → Unclear (flatpaks always scan as Unknown reason — the
    // graph, not libalpm reasons, decides for them). A verdict resting on
    // graph edges wears the worst edge label; a runtime leaf is still only
    // Inferred-safe because the grouping data itself is inferred.
    let worst_edge = edges
        .iter()
        .map(|(_, c)| *c)
        .max()
        .unwrap_or(Confidence::Confirmed);
    let (verdict, confidence) = if is_alpm && pkg.install_reason == InstallReason::Unknown {
        (Verdict::Unclear, Confidence::Unknown)
    } else if required_by.is_empty() {
        let conf = if pkg.runtime {
            Confidence::Inferred
        } else {
            Confidence::Confirmed
        };
        (Verdict::LikelySafe, conf)
    } else {
        (Verdict::IsADependency, worst_edge)
    };

    WhyReport::Found(WhyDetail {
        package: pkg.name.clone(),
        source_id: pkg.source_id.clone(),
        runtime: pkg.runtime,
        caveats: caveats_for(pkg),
        reason: pkg.install_reason,
        transitive_required_by: graph.transitive_required_by(name, max_depth),
        depth_from_explicit: graph.depth_from_explicit(scan, name),
        tree: reverse_tree(graph, name, TREE_DEPTH.min(max_depth), &mut Vec::new()),
        required_by,
        would_remove,
        verdict,
        confidence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CacheSizes, Package, SCHEMA_VERSION};
    use chrono::Utc;

    fn pkg(name: &str, reason: InstallReason, depends: &[&str], provides: &[&str]) -> Package {
        Package {
            name: name.to_string(),
            version: "1".to_string(),
            source_id: SourceId::pacman(),
            install_reason: reason,
            size_bytes: None,
            description: None,
            depends_on: depends.iter().map(|d| d.to_string()).collect(),
            required_by: Vec::new(),
            optional_deps: Vec::new(),
            provides: provides.iter().map(|p| p.to_string()).collect(),
            runtime: false,
            foreign: false,
            signed: true,
            packager: None,
        }
    }

    /// firefox(E)→{glibc, onlyffdep}; bash(E, provides sh)→readline(D)→glibc(D);
    /// scripter(D)→virtual sh; onlyffdep(D) required only by firefox;
    /// mystery(?) reason unknown; flatpak app → runtime, plus an unused
    /// runtime leaf.
    fn scan() -> ScanResult {
        let mut app = pkg(
            "org.gnome.Calculator",
            InstallReason::Unknown,
            &["org.gnome.Platform"],
            &[],
        );
        app.source_id = SourceId::flatpak_user();
        let mut runtime = pkg("org.gnome.Platform", InstallReason::Unknown, &[], &[]);
        runtime.source_id = SourceId::flatpak_user();
        runtime.runtime = true;
        let mut unused_runtime = pkg("org.kde.Platform", InstallReason::Unknown, &[], &[]);
        unused_runtime.source_id = SourceId::flatpak_user();
        unused_runtime.runtime = true;
        ScanResult {
            schema_version: SCHEMA_VERSION,
            scanned_at: Utc::now(),
            sources: Vec::new(),
            packages: vec![
                pkg(
                    "firefox",
                    InstallReason::Explicit,
                    &["glibc", "onlyffdep"],
                    &[],
                ),
                pkg("bash", InstallReason::Explicit, &["readline"], &["sh"]),
                pkg("readline", InstallReason::Dependency, &["glibc"], &[]),
                pkg("glibc", InstallReason::Dependency, &[], &[]),
                pkg("onlyffdep", InstallReason::Dependency, &[], &[]),
                pkg("scripter", InstallReason::Dependency, &["sh"], &[]),
                pkg("mystery", InstallReason::Unknown, &[], &[]),
                app,
                runtime,
                unused_runtime,
            ],
            updates: Vec::new(),
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

    fn report(name: &str) -> WhyReport {
        let s = scan();
        let g = DepGraph::build(&s);
        why(&s, &g, name, 20)
    }

    fn detail(name: &str) -> WhyDetail {
        match report(name) {
            WhyReport::Found(p) => p,
            other => panic!("expected found report, got {other:?}"),
        }
    }

    #[test]
    fn explicit_leaf_is_likely_safe_and_confirmed() {
        let p = detail("firefox");
        assert_eq!(p.reason, InstallReason::Explicit);
        assert!(p.required_by.is_empty());
        assert_eq!(p.verdict, Verdict::LikelySafe);
        assert_eq!(p.confidence, Confidence::Confirmed);
        assert_eq!(p.depth_from_explicit, Some(0));
        // Removing firefox orphans onlyffdep (its only requirer) but NOT
        // glibc (readline also needs it).
        assert_eq!(p.would_remove, vec!["onlyffdep"]);
    }

    #[test]
    fn required_package_is_a_dependency_with_breakage_list() {
        let p = detail("glibc");
        assert_eq!(p.verdict, Verdict::IsADependency);
        assert_eq!(p.required_by, vec!["firefox", "readline"]);
        assert!(
            p.transitive_required_by.contains(&"bash".to_string()),
            "transitive misses bash: {:?}",
            p.transitive_required_by
        );
        assert_eq!(p.depth_from_explicit, Some(1));
    }

    #[test]
    fn virtual_provider_sees_its_consumer() {
        // scripter depends on virtual "sh" → bash must list it.
        let p = detail("bash");
        assert_eq!(p.required_by, vec!["scripter"]);
        assert_eq!(p.verdict, Verdict::IsADependency);
    }

    #[test]
    fn unknown_install_reason_is_unclear_and_unknown() {
        let p = detail("mystery");
        assert_eq!(p.verdict, Verdict::Unclear);
        assert_eq!(p.confidence, Confidence::Unknown);
    }

    #[test]
    fn orphaned_dependency_is_likely_safe() {
        // scripter: dependency-installed, nothing requires it.
        let p = detail("scripter");
        assert_eq!(p.reason, InstallReason::Dependency);
        assert_eq!(p.verdict, Verdict::LikelySafe);
    }

    // --- flatpak (v0.1.3 relationship model) ---
    #[test]
    fn flatpak_app_is_a_confirmed_safe_leaf_that_orphans_its_runtime() {
        let p = detail("org.gnome.Calculator");
        assert_eq!(p.source_id, SourceId::flatpak_user());
        assert!(!p.runtime);
        assert!(p.required_by.is_empty());
        assert_eq!(p.verdict, Verdict::LikelySafe);
        assert_eq!(p.confidence, Confidence::Confirmed);
        // Its runtime's only user is this app → would be left unused.
        assert_eq!(p.would_remove, vec!["org.gnome.Platform"]);
    }

    #[test]
    fn used_runtime_is_a_dependency_with_inferred_confidence() {
        let p = detail("org.gnome.Platform");
        assert!(p.runtime);
        assert_eq!(p.required_by, vec!["org.gnome.Calculator"]);
        assert_eq!(p.verdict, Verdict::IsADependency);
        assert_eq!(p.confidence, Confidence::Inferred, "worst edge label wins");
        // The tree wears the same inferred edge label.
        assert_eq!(p.tree.len(), 1);
        assert_eq!(p.tree[0].name, "org.gnome.Calculator");
        assert_eq!(p.tree[0].confidence, Confidence::Inferred);
    }

    #[test]
    fn unused_runtime_is_only_inferred_safe() {
        // The grouping data is inferred, so "nothing uses it" is too.
        let p = detail("org.kde.Platform");
        assert_eq!(p.verdict, Verdict::LikelySafe);
        assert_eq!(p.confidence, Confidence::Inferred);
    }

    #[test]
    fn flatpak_unknown_reason_never_triggers_unclear() {
        // Flatpaks always scan with an Unknown install reason; the pacman
        // Unclear rule must not fire for them.
        assert_ne!(detail("org.gnome.Calculator").verdict, Verdict::Unclear);
        assert_ne!(detail("org.gnome.Platform").verdict, Verdict::Unclear);
    }

    #[test]
    fn unknown_package_suggests_the_closest_name() {
        match report("firefx") {
            WhyReport::NotFound {
                package,
                suggestion,
            } => {
                assert_eq!(package, "firefx");
                assert_eq!(suggestion.as_deref(), Some("firefox"));
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn hopeless_query_has_no_suggestion() {
        match report("qqqqqqqq") {
            WhyReport::NotFound { suggestion, .. } => assert_eq!(suggestion, None),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    // --- AUR caveats (v0.3) ---
    #[test]
    fn aur_packages_carry_the_pkgbuild_caveat_and_alpm_rules() {
        let mut s = scan();
        let mut foreign = pkg("timr-bin", InstallReason::Explicit, &[], &[]);
        foreign.source_id = SourceId::aur();
        s.packages.push(foreign);
        let g = DepGraph::build(&s);
        let WhyReport::Found(d) = why(&s, &g, "timr-bin", 20) else {
            panic!("expected found")
        };
        assert_eq!(d.caveats.len(), 1);
        assert!(d.caveats[0].contains("review PKGBUILD"), "{:?}", d.caveats);
        assert_eq!(d.verdict, Verdict::LikelySafe); // alpm leaf rules apply
    }

    #[test]
    fn vcs_aur_packages_get_the_snapshot_caveat() {
        let mut s = scan();
        let mut foreign = pkg("paclens-git", InstallReason::Explicit, &[], &[]);
        foreign.source_id = SourceId::aur();
        s.packages.push(foreign);
        let g = DepGraph::build(&s);
        let WhyReport::Found(d) = why(&s, &g, "paclens-git", 20) else {
            panic!("expected found")
        };
        assert_eq!(d.caveats.len(), 2);
        assert!(d.caveats[1].contains("build snapshot"), "{:?}", d.caveats);
    }

    #[test]
    fn aur_unknown_reason_is_unclear_like_pacman() {
        let mut s = scan();
        let mut foreign = pkg("weird-bin", InstallReason::Unknown, &[], &[]);
        foreign.source_id = SourceId::aur();
        s.packages.push(foreign);
        let g = DepGraph::build(&s);
        let WhyReport::Found(d) = why(&s, &g, "weird-bin", 20) else {
            panic!("expected found")
        };
        assert_eq!(d.verdict, Verdict::Unclear);
    }

    #[test]
    fn pacman_and_flatpak_have_no_caveats() {
        let p = detail("firefox");
        assert!(p.caveats.is_empty());
        assert!(detail("org.gnome.Calculator").caveats.is_empty());
    }

    // --- reverse-dep tree ---
    #[test]
    fn tree_nests_requirers_with_confirmed_edges() {
        let p = detail("glibc");
        // Direct requirers as roots (sorted): firefox, readline.
        let names: Vec<&str> = p.tree.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["firefox", "readline"]);
        // readline is required by bash → nested child.
        let readline = &p.tree[1];
        assert_eq!(readline.children.len(), 1);
        assert_eq!(readline.children[0].name, "bash");
        assert!(p.tree.iter().all(|n| n.confidence == Confidence::Confirmed));
    }

    #[test]
    fn tree_is_empty_for_a_leaf() {
        assert!(detail("firefox").tree.is_empty());
    }

    #[test]
    fn tree_survives_cycles() {
        let mut s = scan();
        s.packages
            .push(pkg("a", InstallReason::Dependency, &["b"], &[]));
        s.packages
            .push(pkg("b", InstallReason::Dependency, &["a"], &[]));
        let g = DepGraph::build(&s);
        let report = why(&s, &g, "a", 20);
        let WhyReport::Found(p) = report else {
            panic!("expected found")
        };
        // b requires a; a requires b would loop — cut as a back-edge.
        assert_eq!(p.tree.len(), 1);
        assert_eq!(p.tree[0].name, "b");
        assert!(p.tree[0].children.is_empty());
    }

    #[test]
    fn tree_caps_branches_and_reports_the_hidden_count() {
        let mut s = scan();
        for i in 0..8 {
            s.packages.push(pkg(
                &format!("user{i}"),
                InstallReason::Explicit,
                &["glibc"],
                &[],
            ));
        }
        let g = DepGraph::build(&s);
        let WhyReport::Found(p) = why(&s, &g, "glibc", 20) else {
            panic!("expected found")
        };
        assert_eq!(p.tree.len(), 5, "branch cap");
        let hidden: usize = p.tree.iter().map(|n| n.truncated).sum();
        // 10 requirers total (firefox, readline + 8 users) → 5 hidden.
        assert_eq!(hidden, 5);
    }

    #[test]
    fn tree_lines_flatten_with_correct_prefixes() {
        let p = detail("glibc");
        let rows = tree_lines(&p.tree, ("|- ", "`- ", "|  ", "   "));
        let drawn: Vec<String> = rows
            .iter()
            .map(|r| format!("{}{}", r.prefix, r.name))
            .collect();
        assert_eq!(
            drawn,
            vec![
                "|- firefox",
                "`- readline",
                "   `- bash",
                "      `- scripter"
            ]
        );
    }

    #[test]
    fn verdict_labels_render_for_the_ui() {
        assert_eq!(Verdict::LikelySafe.to_string(), "likely safe");
        assert_eq!(Verdict::IsADependency.to_string(), "is a dependency");
        assert_eq!(Verdict::Unclear.to_string(), "unclear — check manually");
    }
}
