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
    fn source_id_constructors_use_canonical_strings() {
        assert_eq!(SourceId::pacman().as_str(), "pacman");
        assert_eq!(SourceId::aur().as_str(), "aur");
        assert_eq!(SourceId::flatpak().as_str(), "flatpak");
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
