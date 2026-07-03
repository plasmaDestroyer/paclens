//! The in-memory dependency graph (spec §7), built from one scan's pacman
//! packages — never from pactree, never serialized (rebuilt on every load).
//!
//! Nodes are package names; edges are `package → dependency` weighted with
//! `DependencyEdge`. Virtual names from `Provides` are resolved through an
//! alias map so a dep on `sh` lands on `bash`. All queries run in-memory.

use std::collections::{HashMap, HashSet};

use petgraph::Direction;
use petgraph::graph::{DiGraph, NodeIndex};

use crate::model::{Confidence, DependencyEdge, EdgeKind, InstallReason, ScanResult, SourceId};

pub struct DepGraph {
    graph: DiGraph<String, DependencyEdge>,
    index: HashMap<String, NodeIndex>,
}

impl DepGraph {
    /// Build from the scan's **pacman** packages (the roadmap scopes the graph
    /// to pacman; Flatpak apps are self-contained). Pure — no IO.
    pub fn build(scan: &ScanResult) -> Self {
        let pacman: Vec<_> = scan
            .packages
            .iter()
            .filter(|p| p.source_id == SourceId::pacman())
            .collect();

        // Provides alias map: virtual name → real provider. First provider
        // wins on the rare duplicate — good enough for lookup purposes.
        let mut alias: HashMap<&str, &str> = HashMap::new();
        for pkg in &pacman {
            for provided in &pkg.provides {
                alias.entry(provided.as_str()).or_insert(pkg.name.as_str());
            }
        }

        let mut dep_graph = DepGraph {
            graph: DiGraph::new(),
            index: HashMap::new(),
        };
        for pkg in &pacman {
            let from = dep_graph.get_or_insert(&pkg.name);
            for dep in &pkg.depends_on {
                // Resolve a virtual dep to its real provider; a name that is
                // itself installed is never treated as an alias.
                let target = if dep_graph.index.contains_key(dep.as_str())
                    || pacman.iter().any(|p| &p.name == dep)
                {
                    dep.as_str()
                } else {
                    alias.get(dep.as_str()).copied().unwrap_or(dep.as_str())
                };
                let to = dep_graph.get_or_insert(target);
                dep_graph.graph.add_edge(
                    from,
                    to,
                    DependencyEdge {
                        kind: EdgeKind::Real,
                        confidence: Confidence::Confirmed,
                    },
                );
            }
        }
        dep_graph
    }

    fn get_or_insert(&mut self, name: &str) -> NodeIndex {
        if let Some(&idx) = self.index.get(name) {
            idx
        } else {
            let idx = self.graph.add_node(name.to_string());
            self.index.insert(name.to_string(), idx);
            idx
        }
    }

    /// Only tests assert membership directly today; the TUI/CLI go through
    /// `why()`. Kept because the overlap detector (v0.0.8) needs it.
    #[cfg_attr(not(test), expect(dead_code))]
    pub fn contains(&self, name: &str) -> bool {
        self.index.contains_key(name)
    }

    /// What `name` requires (forward deps), sorted.
    pub fn requires(&self, name: &str) -> Vec<String> {
        self.neighbors(name, Direction::Outgoing)
    }

    /// What requires `name` (reverse deps), sorted.
    pub fn required_by(&self, name: &str) -> Vec<String> {
        self.neighbors(name, Direction::Incoming)
    }

    fn neighbors(&self, name: &str, dir: Direction) -> Vec<String> {
        let Some(&idx) = self.index.get(name) else {
            return Vec::new();
        };
        let mut out: Vec<String> = self
            .graph
            .neighbors_directed(idx, dir)
            .map(|n| self.graph[n].clone())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Everything that transitively requires `name` (full removal-impact set),
    /// DFS over incoming edges, capped at `max_depth` hops. Cycle-safe.
    pub fn transitive_required_by(&self, name: &str, max_depth: u32) -> Vec<String> {
        let Some(&start) = self.index.get(name) else {
            return Vec::new();
        };
        let mut seen: HashSet<NodeIndex> = HashSet::new();
        let mut stack: Vec<(NodeIndex, u32)> = vec![(start, 0)];
        while let Some((node, depth)) = stack.pop() {
            if depth >= max_depth {
                continue;
            }
            for parent in self.graph.neighbors_directed(node, Direction::Incoming) {
                if parent != start && seen.insert(parent) {
                    stack.push((parent, depth + 1));
                }
            }
        }
        let mut out: Vec<String> = seen.into_iter().map(|n| self.graph[n].clone()).collect();
        out.sort();
        out
    }

    /// Shortest hop count from `name` up to the nearest explicitly installed
    /// package (BFS over incoming edges). `Some(0)` when `name` itself is
    /// explicit; `None` when no explicit package reaches it.
    pub fn depth_from_explicit(&self, scan: &ScanResult, name: &str) -> Option<u32> {
        let is_explicit = |n: &str| {
            scan.packages
                .iter()
                .any(|p| p.name == n && p.install_reason == InstallReason::Explicit)
        };
        if is_explicit(name) {
            return Some(0);
        }
        let &start = self.index.get(name)?;
        let mut seen = HashSet::from([start]);
        let mut frontier = vec![start];
        let mut depth = 0u32;
        while !frontier.is_empty() {
            depth += 1;
            let mut next = Vec::new();
            for node in frontier {
                for parent in self.graph.neighbors_directed(node, Direction::Incoming) {
                    if !seen.insert(parent) {
                        continue;
                    }
                    if is_explicit(&self.graph[parent]) {
                        return Some(depth);
                    }
                    next.push(parent);
                }
            }
            frontier = next;
        }
        None
    }

    /// Orphan candidates (spec §7.2): installed as a dependency, nothing
    /// requires them. Replaces `pacman -Qtd`.
    /// Spec §7.2 deliverable; the cleanup screen (v0.0.9+) is its consumer.
    #[cfg_attr(not(test), expect(dead_code))]
    pub fn orphans(&self, scan: &ScanResult) -> Vec<String> {
        let mut out: Vec<String> = scan
            .packages
            .iter()
            .filter(|p| {
                p.source_id == SourceId::pacman()
                    && p.install_reason == InstallReason::Dependency
                    && self.required_by(&p.name).is_empty()
            })
            .map(|p| p.name.clone())
            .collect();
        out.sort();
        out
    }
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

    /// firefox(E) → glibc(D); bash(E) → readline(D) → glibc; bash provides sh;
    /// zsh-ish "scripter"(D) depends on virtual sh; leafdep(D) orphaned;
    /// one flatpak app that must be ignored.
    fn scan() -> ScanResult {
        let mut flatpak = pkg("org.x.App", InstallReason::Unknown, &["glibc"], &[]);
        flatpak.source_id = SourceId::flatpak_user();
        ScanResult {
            schema_version: SCHEMA_VERSION,
            scanned_at: Utc::now(),
            sources: Vec::new(),
            packages: vec![
                pkg("firefox", InstallReason::Explicit, &["glibc"], &[]),
                pkg("bash", InstallReason::Explicit, &["readline"], &["sh"]),
                pkg("readline", InstallReason::Dependency, &["glibc"], &[]),
                pkg("glibc", InstallReason::Dependency, &[], &[]),
                pkg("scripter", InstallReason::Dependency, &["sh"], &[]),
                pkg("leafdep", InstallReason::Dependency, &[], &[]),
                flatpak,
            ],
            updates: Vec::new(),
            cache_sizes: CacheSizes::default(),
        }
    }

    fn graph() -> (ScanResult, DepGraph) {
        let s = scan();
        let g = DepGraph::build(&s);
        (s, g)
    }

    #[test]
    fn builds_nodes_for_pacman_only() {
        let (_, g) = graph();
        assert!(g.contains("firefox"));
        assert!(g.contains("glibc"));
        assert!(!g.contains("org.x.App"), "flatpak apps stay out");
    }

    #[test]
    fn forward_and_reverse_queries_agree() {
        let (_, g) = graph();
        assert_eq!(g.requires("firefox"), vec!["glibc"]);
        assert_eq!(g.requires("bash"), vec!["readline"]);
        let mut rdeps = g.required_by("glibc");
        rdeps.retain(|n| n != "scripter"); // alias edge checked separately
        assert_eq!(rdeps, vec!["firefox", "readline"]);
        assert_eq!(g.required_by("readline"), vec!["bash"]);
    }

    #[test]
    fn virtual_dep_resolves_to_the_real_provider() {
        let (_, g) = graph();
        // scripter depends on virtual "sh" → edge lands on bash.
        assert_eq!(g.requires("scripter"), vec!["bash"]);
        assert!(g.required_by("bash").contains(&"scripter".to_string()));
        assert!(!g.contains("sh"), "no node for the resolved virtual name");
    }

    #[test]
    fn installed_name_is_never_shadowed_by_an_alias() {
        // A package that both exists and is provided by another must resolve
        // to itself.
        let mut s = scan();
        s.packages
            .push(pkg("sh", InstallReason::Dependency, &[], &[]));
        s.packages
            .push(pkg("caller", InstallReason::Explicit, &["sh"], &[]));
        let g = DepGraph::build(&s);
        assert_eq!(g.requires("caller"), vec!["sh"]);
    }

    #[test]
    fn transitive_required_by_walks_up_and_respects_depth() {
        let (_, g) = graph();
        let full = g.transitive_required_by("glibc", 20);
        assert!(full.contains(&"firefox".to_string()));
        assert!(full.contains(&"readline".to_string()));
        assert!(full.contains(&"bash".to_string())); // via readline
        // Depth 1: only direct requirers.
        let shallow = g.transitive_required_by("glibc", 1);
        assert!(shallow.contains(&"firefox".to_string()));
        assert!(!shallow.contains(&"bash".to_string()));
    }

    #[test]
    fn transitive_required_by_survives_cycles() {
        let mut s = scan();
        s.packages
            .push(pkg("a", InstallReason::Dependency, &["b"], &[]));
        s.packages
            .push(pkg("b", InstallReason::Dependency, &["a"], &[]));
        let g = DepGraph::build(&s);
        let up = g.transitive_required_by("a", 20);
        assert_eq!(up, vec!["b"]); // terminates, excludes self
    }

    #[test]
    fn depth_from_explicit_counts_hops() {
        let (s, g) = graph();
        assert_eq!(g.depth_from_explicit(&s, "firefox"), Some(0)); // explicit itself
        assert_eq!(g.depth_from_explicit(&s, "readline"), Some(1)); // bash → readline
        assert_eq!(g.depth_from_explicit(&s, "glibc"), Some(1)); // firefox → glibc
        assert_eq!(g.depth_from_explicit(&s, "leafdep"), None); // nothing reaches it
    }

    #[test]
    fn orphans_are_dependency_installed_with_no_requirers() {
        let (s, g) = graph();
        // leafdep: dependency, nothing requires it. scripter: dependency but
        // nothing requires it either → also an orphan candidate.
        assert_eq!(g.orphans(&s), vec!["leafdep", "scripter"]);
    }

    #[test]
    fn unknown_names_return_empty_not_panic() {
        let (s, g) = graph();
        assert!(g.requires("nope").is_empty());
        assert!(g.required_by("nope").is_empty());
        assert!(g.transitive_required_by("nope", 5).is_empty());
        assert_eq!(g.depth_from_explicit(&s, "nope"), None);
    }
}
