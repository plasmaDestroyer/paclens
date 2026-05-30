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
