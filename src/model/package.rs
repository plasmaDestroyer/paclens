//! Installed package representation (design §7).

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
    /// A Flatpak runtime (platform/SDK/theme/driver) rather than an app.
    /// Always false for pacman packages. Spec §4.3 deviation (design §13).
    #[serde(default)]
    pub runtime: bool,
    /// Which flatpak installation this lives in — `None` for anything that is
    /// not a flatpak. Scope rides on the package because flatpak is one
    /// source: one tool updates both installations (design §13, 2026-09-07).
    #[serde(default)]
    pub scope: Option<super::FlatpakScope>,
    /// In no configured sync database (`pacman -Qm`). Not the same as "from
    /// the AUR" — a repo that is removed leaves its packages foreign (#77).
    #[serde(default)]
    pub foreign: bool,
    /// `pacman -Qi`'s "Validated By" named a signature, which means something
    /// built and signed this rather than the local machine. Locally built
    /// packages validate as `None`.
    #[serde(default)]
    pub signed: bool,
    /// Who packaged it, when that is anyone in particular — `Unknown Packager`
    /// is makepkg's default and says only "built here".
    #[serde(default)]
    pub packager: Option<String>,
    /// The best version any configured repo offers, **when it differs from
    /// the installed one** — `(repo, version)`.
    ///
    /// `None` means either that no repo offers this package at all (see
    /// `foreign`) or that the repos offer exactly what is installed, which is
    /// the ordinary case and not worth storing 1,800 times. Whether a
    /// difference is an update or a package the repos can no longer reach is
    /// a comparison, and comparisons belong to the analyzer (#78).
    #[serde(default)]
    pub repo_version: Option<(String, String)>,
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
