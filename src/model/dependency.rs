//! Dependency-edge and confidence types (spec §4.5, §8).
//!
//! `DependencyEdge` is the edge *weight* in the analyzer's petgraph — the
//! endpoints are the graph nodes, so the edge carries only metadata. Every
//! advisory fact paclens emits wears a `Confidence` label, and labels are
//! never promoted: an `Unknown` edge anywhere in a path caps the verdict at
//! `Unknown` (P3).

/// Edge weight in the dependency graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DependencyEdge {
    pub kind: EdgeKind,
    pub confidence: Confidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    /// From pacman dep data. Ground truth.
    Real,
    /// Heuristic. Cross-source or appstream-derived — flatpak app → runtime
    /// grouping edges wear this (spec §7.4, v0.1.3).
    Inferred,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    /// Derived from authoritative source data with no inference.
    Confirmed,
    /// Heuristic derivation, likely correct, basis is stated.
    Inferred,
    /// Tool cannot determine from available data.
    Unknown,
}

impl std::fmt::Display for Confidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Confidence::Confirmed => "confirmed",
            Confidence::Inferred => "inferred",
            Confidence::Unknown => "unknown",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_displays_lowercase_labels() {
        assert_eq!(Confidence::Confirmed.to_string(), "confirmed");
        assert_eq!(Confidence::Inferred.to_string(), "inferred");
        assert_eq!(Confidence::Unknown.to_string(), "unknown");
    }

    #[test]
    fn confidence_orders_from_best_to_worst() {
        // Ord lets "never promote" be `max()`: the worst label in a path wins.
        assert!(Confidence::Confirmed < Confidence::Inferred);
        assert!(Confidence::Inferred < Confidence::Unknown);
    }
}
