//! Package source identity (design §7, §4.2).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Newtype wrapper for source identifiers.
///
/// Canonical values: `pacman`, `aur`, `flatpak`. **One id per tool that keeps
/// packages up to date** (design §13, 2026-09-07) — flatpak installs it and
/// flatpak updates it, so user and system scope are one source; the scope is a
/// property of the package, and of the step that updates it.
///
/// Ids are flat on purpose. `flatpak-user` and `flatpak-system` used to exist,
/// and seven production sites asked about them by prefix — which is how a
/// source's behaviour came to be read out of its name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceId(pub String);

impl SourceId {
    pub fn pacman() -> Self {
        SourceId("pacman".to_string())
    }

    pub fn aur() -> Self {
        SourceId("aur".to_string())
    }

    pub fn flatpak() -> Self {
        SourceId("flatpak".to_string())
    }

    pub fn cargo() -> Self {
        SourceId("cargo".to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpm_sources_answer_the_dependency_questions_and_flatpak_does_not() {
        let alpm = SourceKind::Pacman.capabilities();
        assert!(alpm.dependency_graph && alpm.install_reason && alpm.orphans);
        assert_eq!(alpm.removal_hint, None, "removal is not a one-liner here");
        assert_eq!(
            SourceKind::Aur.capabilities(),
            alpm,
            "the AUR is libalpm too; it differs in who updates it"
        );

        let flatpak = SourceKind::Flatpak.capabilities();
        assert!(!flatpak.dependency_graph);
        assert!(!flatpak.install_reason);
        assert!(!flatpak.orphans);
        assert_eq!(flatpak.removal_hint, Some("flatpak uninstall"));
    }

    #[test]
    fn an_unknown_source_claims_nothing() {
        let unknown = SourceCapabilities::UNKNOWN;
        assert!(!unknown.dependency_graph);
        assert!(!unknown.install_reason);
        assert!(!unknown.orphans);
        assert_eq!(unknown.removal_hint, None);
    }

    #[test]
    fn source_id_constructors_use_canonical_strings() {
        assert_eq!(SourceId::pacman().as_str(), "pacman");
        assert_eq!(SourceId::aur().as_str(), "aur");
        assert_eq!(SourceId::flatpak().as_str(), "flatpak");
        assert_eq!(SourceId::cargo().as_str(), "cargo");
    }

    #[test]
    fn cargo_has_no_dependency_data_but_does_know_why_a_crate_is_there() {
        // The first source with no dependency edges at all. It still records
        // an install reason — everything was asked for by name — and claiming
        // otherwise made `why` describe a crate as a flatpak app.
        let caps = SourceKind::Cargo.capabilities();
        assert!(!caps.dependency_graph);
        assert!(caps.install_reason);
        assert!(!caps.orphans);
        assert_eq!(caps.removal_hint, Some("cargo uninstall"));
    }

    #[test]
    fn source_id_display_matches_inner() {
        assert_eq!(SourceId::flatpak().to_string(), "flatpak");
    }

    #[test]
    fn one_id_per_tool_that_updates() {
        // Scope is a property of a package, never an identity: there is no
        // `flatpak-user` source to ask about by name (design §13).
        let ids = [SourceId::pacman(), SourceId::aur(), SourceId::flatpak()];
        assert!(
            ids.iter().all(|id| !id.as_str().contains('-')),
            "a scoped id is a predicate waiting to be forgotten"
        );
    }
}

/// A package source, as recorded in a scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub id: SourceId,
    pub kind: SourceKind,
    /// Was the source's binary found on PATH at scan time?
    pub available: bool,
    pub last_scanned: Option<DateTime<Utc>>,
    /// False when update counts came from a possibly-stale local DB (pacman
    /// without pacman-contrib's checkupdates). Spec §4.2 deviation, recorded
    /// in design §13.
    #[serde(default = "default_true")]
    pub accurate_updates: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceKind {
    Pacman,
    /// Foreign (AUR) packages: installed through libalpm like pacman's, but
    /// updated via paru (roadmap v0.3).
    Aur,
    /// Both scopes. One tool updates them, so they are one source; which
    /// scope a package lives in rides on the package (design §13).
    Flatpak,
    /// Crates installed with `cargo install`, living under `$HOME` — never
    /// privileged, and known from cargo's own `.crates2.json` rather than
    /// from whatever happens to be in `~/.cargo/bin` (rustup owns most of
    /// that).
    Cargo,
}

/// What a screen may ask a source about (design §13, 2026-09-07).
///
/// Four questions, because four is what the screens branch on today. They
/// used to be asked as `is_alpm(source_id)` — a predicate standing in for two
/// unrelated facts, which would have had to lie about a source like cargo:
/// real versions and sizes, no install reasons, no dependents.
///
/// A capability is added when a screen already asks it, never in advance. A
/// capability that lies is worse than a predicate that is narrow: claiming an
/// install reason a source does not record makes `why` print "installed as a
/// dependency" about a package with no such concept, which is exactly the
/// confident wrong answer §3 forbids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceCapabilities {
    /// Real dependency edges between installed packages, from the source
    /// itself. Flatpak's app → runtime edge is inferred by the analyzer, not
    /// reported by flatpak, so it does not count.
    pub dependency_graph: bool,
    /// The source knows why a package is installed. True for a source where
    /// every install is explicit by construction (cargo) as well as one that
    /// distinguishes explicit from dependency (pacman) — what it rules out is
    /// a source that records nothing, where "installed as a dependency" would
    /// be a fact nobody has.
    pub install_reason: bool,
    /// A package here can become an orphan: installed as a dependency, with
    /// nothing left requiring it.
    pub orphans: bool,
    /// The one-line command that removes a single package, when the source
    /// has one worth printing. `None` where removal is not a one-liner —
    /// pacman removal goes through the cleanup and migration flows, which
    /// carry their own rules.
    pub removal_hint: Option<&'static str>,
}

impl SourceCapabilities {
    /// What is known about a source that the scan has no row for: nothing.
    /// Conservative on purpose — a package no source claims stays out of the
    /// graph and out of the orphan list rather than being assumed alpm-shaped.
    pub const UNKNOWN: SourceCapabilities = SourceCapabilities {
        dependency_graph: false,
        install_reason: false,
        orphans: false,
        removal_hint: None,
    };
}

impl SourceKind {
    /// The one place a source's kind decides what it can answer.
    pub fn capabilities(&self) -> SourceCapabilities {
        match self {
            // Both are libalpm: real dep data, real install reasons. The AUR
            // differs in who *updates* it, which is a different question.
            SourceKind::Pacman | SourceKind::Aur => SourceCapabilities {
                dependency_graph: true,
                install_reason: true,
                orphans: true,
                removal_hint: None,
            },
            SourceKind::Flatpak => SourceCapabilities {
                dependency_graph: false,
                install_reason: false,
                // A runtime nothing uses is unused, not orphaned: flatpak
                // records no reason, so "installed as a dependency" is not a
                // fact anyone here has.
                orphans: false,
                removal_hint: Some("flatpak uninstall"),
            },
            // No dependency edges between installed crates, and nothing is
            // ever installed on another crate's behalf — so no orphans
            // either. It *does* record an install reason, though: every crate
            // cargo installs was asked for by name, so the answer is always
            // "explicit". Saying otherwise sends `why` down the branch for
            // sources that record nothing, which is flatpak's, and a cargo
            // crate came out described as a self-contained flatpak app.
            SourceKind::Cargo => SourceCapabilities {
                dependency_graph: false,
                install_reason: true,
                orphans: false,
                removal_hint: Some("cargo uninstall"),
            },
        }
    }
}

/// Which flatpak installation a package lives in. A property of the package
/// (and so of the step that updates or removes it), not of the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlatpakScope {
    User,
    System,
}

impl FlatpakScope {
    pub fn label(self) -> &'static str {
        match self {
            FlatpakScope::User => "user",
            FlatpakScope::System => "system",
        }
    }

    /// The scope flag every `flatpak` subcommand takes.
    pub fn flag(self) -> &'static str {
        match self {
            FlatpakScope::User => "--user",
            FlatpakScope::System => "--system",
        }
    }

    /// System-scope changes touch `/var/lib/flatpak`; user-scope ones stay
    /// under `~`. The planner reads this when it declares a step's privilege.
    pub fn needs_privilege(self) -> bool {
        self == FlatpakScope::System
    }
}
