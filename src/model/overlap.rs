//! Overlap types (spec §4.6): a pacman package and a Flatpak app that are the
//! same application installed twice. Advisory only — computed by the analyzer
//! on every load, never cached, never acted on.

use std::path::PathBuf;

use crate::model::{Confidence, SourceId};

pub struct OverlapCandidate {
    pub display_name: String,
    pub native_package: Option<PackageRef>,
    pub flatpak_app: Option<PackageRef>,
    pub match_method: MatchMethod,
    pub confidence: Confidence,
    pub tradeoff: Tradeoff,
}

/// Lightweight reference to a package without cloning all data.
pub struct PackageRef {
    pub name: String,
    pub version: String,
    pub source_id: SourceId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchMethod {
    /// Entry in overlap_map.toml.
    KnownMap,
    /// Reversed last component of Flatpak app ID matches pacman name.
    ReverseDnsSuffix,
    /// Flatpak appstream display name matches pacman name.
    DisplayNameMatch,
}

impl std::fmt::Display for MatchMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            MatchMethod::KnownMap => "known map",
            MatchMethod::ReverseDnsSuffix => "reverse-DNS suffix",
            MatchMethod::DisplayNameMatch => "display-name match",
        })
    }
}

/// Spec §4.6 verbatim; `native_profile_path` and `native_is_newer` still
/// wait for a per-app config-path map and a reliable cross-source version
/// comparison. Flatpak profile path/size are live (v0.1.4).
#[derive(Default)]
#[allow(dead_code)]
pub struct Tradeoff {
    pub native_profile_path: Option<PathBuf>,
    pub flatpak_profile_path: Option<PathBuf>,
    pub native_version: Option<String>,
    pub flatpak_version: Option<String>,
    pub native_is_newer: Option<bool>,
    /// Filled once the scanner measures `~/.var/app/<id>` (deferred).
    pub flatpak_profile_size_bytes: Option<u64>,
    pub likely_primary: PrimaryHeuristic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PrimaryHeuristic {
    Native,
    /// A >10 MiB `~/.var/app/<id>` profile — the user has data there
    /// (spec §9.4 heuristic 2, live since v0.1.4).
    Flatpak,
    #[default]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_methods_display_for_the_report() {
        assert_eq!(MatchMethod::KnownMap.to_string(), "known map");
        assert_eq!(
            MatchMethod::ReverseDnsSuffix.to_string(),
            "reverse-DNS suffix"
        );
        assert_eq!(
            MatchMethod::DisplayNameMatch.to_string(),
            "display-name match"
        );
    }

    #[test]
    fn tradeoff_defaults_to_unknown_primary() {
        assert_eq!(
            Tradeoff::default().likely_primary,
            PrimaryHeuristic::Unknown
        );
    }
}
