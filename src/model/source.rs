//! Package source identity (spec §4.1, §4.2).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Newtype wrapper for source identifiers.
///
/// Canonical per-package values: `pacman`, `flatpak-user`, `flatpak-system`.
/// A bare `flatpak` value is used only as the flatpak provider's logical id
/// (it scans both scopes); individual packages always carry a scoped id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceId(pub String);

impl SourceId {
    pub fn pacman() -> Self {
        SourceId("pacman".to_string())
    }

    pub fn flatpak() -> Self {
        SourceId("flatpak".to_string())
    }

    pub fn flatpak_user() -> Self {
        SourceId("flatpak-user".to_string())
    }

    pub fn flatpak_system() -> Self {
        SourceId("flatpak-system".to_string())
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
        assert_eq!(SourceId::flatpak().as_str(), "flatpak");
        assert_eq!(SourceId::flatpak_user().as_str(), "flatpak-user");
        assert_eq!(SourceId::flatpak_system().as_str(), "flatpak-system");
    }

    #[test]
    fn source_id_display_matches_inner() {
        assert_eq!(SourceId::flatpak_user().to_string(), "flatpak-user");
    }

    #[test]
    fn flatpak_scoped_ids_share_the_flatpak_prefix() {
        assert!(SourceId::flatpak_user().as_str().starts_with("flatpak"));
        assert!(SourceId::flatpak_system().as_str().starts_with("flatpak"));
        assert!(!SourceId::pacman().as_str().starts_with("flatpak"));
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceKind {
    Pacman,
    Flatpak { scope: FlatpakScope },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlatpakScope {
    User,
    System,
}
