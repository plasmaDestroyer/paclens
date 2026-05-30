//! Installed package representation (spec §4.3).

use serde::{Deserialize, Serialize};

use super::SourceId;

/// A single installed package from any source.
///
/// For pacman, all fields are populated from `pacman -Qi` (v0.0.3). For
/// flatpak, `name` holds the application id (the stable identifier used by
/// overlap detection) and the human display name lives in `description`;
/// dependency fields stay empty because flatpak deps are bundled, not
/// cross-referenced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub source_id: SourceId,
    pub install_reason: InstallReason,
    pub size_bytes: Option<u64>,
    pub description: Option<String>,
    /// Direct dependencies (`pacman -Qi` "Depends On"). Empty for flatpak.
    pub depends_on: Vec<String>,
    /// Direct reverse dependencies (`pacman -Qi` "Required By"). Empty for flatpak.
    pub required_by: Vec<String>,
    /// Optional dependencies (informational; not graph edges).
    pub optional_deps: Vec<String>,
    /// Virtual package names this provides (`pacman -Qi` "Provides").
    pub provides: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstallReason {
    /// User installed it directly.
    Explicit,
    /// Installed as a dependency of something else.
    Dependency,
    /// Source does not distinguish (e.g. flatpak).
    Unknown,
}
