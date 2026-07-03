//! The `why` query (spec §7.3, roadmap v0.0.7): why is this package installed,
//! what happens if it is removed, with a cautious verdict.
//!
//! Pure — reads only the scan and the pre-built graph. Decision recorded in
//! dev-notes §7: **zero reverse deps over confirmed data ⇒ `likely safe`
//! regardless of install reason** (spec §11.4's canonical example shows an
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

/// The three shapes a `why` answer takes. Flatpak apps are self-contained and
/// carry no fabricated graph data; an unknown name gets a fuzzy suggestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhyReport {
    Pacman(PacmanWhy),
    Flatpak {
        package: String,
        source_id: SourceId,
    },
    NotFound {
        package: String,
        suggestion: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacmanWhy {
    pub package: String,
    pub reason: InstallReason,
    /// Direct reverse deps — removing the package breaks these.
    pub required_by: Vec<String>,
    /// Everything transitively above (capped at `max_depth`).
    pub transitive_required_by: Vec<String>,
    /// Hops up to the nearest explicitly installed package (0 = itself).
    pub depth_from_explicit: Option<u32>,
    /// Direct deps that would be orphaned (their only requirer is this).
    pub would_remove: Vec<String>,
    pub verdict: Verdict,
    pub confidence: Confidence,
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

    if pkg.source_id != SourceId::pacman() {
        return WhyReport::Flatpak {
            package: pkg.name.clone(),
            source_id: pkg.source_id.clone(),
        };
    }

    let required_by = graph.required_by(name);
    let would_remove: Vec<String> = graph
        .requires(name)
        .into_iter()
        .filter(|dep| {
            let dep_is_dependency = scan
                .packages
                .iter()
                .any(|p| &p.name == dep && p.install_reason == InstallReason::Dependency);
            dep_is_dependency && graph.required_by(dep) == vec![name.to_string()]
        })
        .collect();

    // Conservative verdict (P2): unknown install reason means incomplete data.
    let (verdict, confidence) = if pkg.install_reason == InstallReason::Unknown {
        (Verdict::Unclear, Confidence::Unknown)
    } else if required_by.is_empty() {
        (Verdict::LikelySafe, Confidence::Confirmed)
    } else {
        (Verdict::IsADependency, Confidence::Confirmed)
    };

    WhyReport::Pacman(PacmanWhy {
        package: pkg.name.clone(),
        reason: pkg.install_reason,
        transitive_required_by: graph.transitive_required_by(name, max_depth),
        depth_from_explicit: graph.depth_from_explicit(scan, name),
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
        }
    }

    /// firefox(E)→{glibc, onlyffdep}; bash(E, provides sh)→readline(D)→glibc(D);
    /// scripter(D)→virtual sh; onlyffdep(D) required only by firefox;
    /// mystery(?) reason unknown; one flatpak app.
    fn scan() -> ScanResult {
        let mut flatpak = pkg("org.gnome.Calculator", InstallReason::Unknown, &[], &[]);
        flatpak.source_id = SourceId::flatpak_user();
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
                flatpak,
            ],
            updates: Vec::new(),
            cache_sizes: CacheSizes::default(),
        }
    }

    fn report(name: &str) -> WhyReport {
        let s = scan();
        let g = DepGraph::build(&s);
        why(&s, &g, name, 20)
    }

    fn pacman(name: &str) -> PacmanWhy {
        match report(name) {
            WhyReport::Pacman(p) => p,
            other => panic!("expected pacman report, got {other:?}"),
        }
    }

    #[test]
    fn explicit_leaf_is_likely_safe_and_confirmed() {
        let p = pacman("firefox");
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
        let p = pacman("glibc");
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
        let p = pacman("bash");
        assert_eq!(p.required_by, vec!["scripter"]);
        assert_eq!(p.verdict, Verdict::IsADependency);
    }

    #[test]
    fn unknown_install_reason_is_unclear_and_unknown() {
        let p = pacman("mystery");
        assert_eq!(p.verdict, Verdict::Unclear);
        assert_eq!(p.confidence, Confidence::Unknown);
    }

    #[test]
    fn orphaned_dependency_is_likely_safe() {
        // scripter: dependency-installed, nothing requires it.
        let p = pacman("scripter");
        assert_eq!(p.reason, InstallReason::Dependency);
        assert_eq!(p.verdict, Verdict::LikelySafe);
    }

    #[test]
    fn flatpak_app_gets_the_flatpak_report_not_graph_data() {
        assert_eq!(
            report("org.gnome.Calculator"),
            WhyReport::Flatpak {
                package: "org.gnome.Calculator".to_string(),
                source_id: SourceId::flatpak_user(),
            }
        );
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

    #[test]
    fn verdict_labels_render_for_the_ui() {
        assert_eq!(Verdict::LikelySafe.to_string(), "likely safe");
        assert_eq!(Verdict::IsADependency.to_string(), "is a dependency");
        assert_eq!(Verdict::Unclear.to_string(), "unclear — check manually");
    }
}
