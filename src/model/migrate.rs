//! Migration advisory types (v0.4): where an overlapping app stores its data
//! on each side, and what a manual migration would involve. Advisory only —
//! paclens never copies, moves, or deletes profile data (that is v0.5's
//! problem, behind backups and confirmations).

// TODO(v0.4): drop once the CLI/TUI renderer commits consume these types.
#![allow(dead_code)]

use crate::model::Confidence;

/// Which side the user would consolidate into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Copy native profile data into the Flatpak sandbox home.
    ToFlatpak,
    /// Copy Flatpak sandbox data out to the native locations.
    ToNative,
}

impl Direction {
    pub fn flipped(self) -> Self {
        match self {
            Direction::ToFlatpak => Direction::ToNative,
            Direction::ToNative => Direction::ToFlatpak,
        }
    }

    /// The side being migrated *to*, for headers ("migrate firefox → flatpak").
    pub fn target(&self) -> &'static str {
        match self {
            Direction::ToFlatpak => "flatpak",
            Direction::ToNative => "native",
        }
    }
}

/// What a profile directory holds, by XDG convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    /// `~/.config/…` ↔ `~/.var/app/<id>/config/…`
    Config,
    /// `~/.local/share/…` ↔ `~/.var/app/<id>/data/…`
    Data,
    /// `~/.cache/…` ↔ `~/.var/app/<id>/cache/…` — regenerates, skip copying.
    Cache,
    /// A non-XDG home dir (e.g. `~/.mozilla`) ↔ the same name under
    /// `~/.var/app/<id>/` (the Flatpak sandbox HOME).
    Profile,
}

impl std::fmt::Display for PathKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PathKind::Config => "config",
            PathKind::Data => "data",
            PathKind::Cache => "cache",
            PathKind::Profile => "profile",
        })
    }
}

/// One native-path ↔ flatpak-path pair. Byte counts come from the scanner's
/// probe (`ScanResult.profile_dir_sizes`); `None` means the directory does
/// not exist.
#[derive(Debug, Clone)]
pub struct PathMapping {
    pub kind: PathKind,
    pub native: String,
    pub flatpak: String,
    pub native_bytes: Option<u64>,
    pub flatpak_bytes: Option<u64>,
    /// `Confirmed` when the pair comes from the curated map's
    /// `profile_dirs`; `Inferred` for XDG-convention guesses.
    pub confidence: Confidence,
}

impl PathMapping {
    /// (from, from_bytes, to, to_bytes) for the given direction.
    pub fn endpoints(&self, direction: Direction) -> (&str, Option<u64>, &str, Option<u64>) {
        match direction {
            Direction::ToFlatpak => (
                &self.native,
                self.native_bytes,
                &self.flatpak,
                self.flatpak_bytes,
            ),
            Direction::ToNative => (
                &self.flatpak,
                self.flatpak_bytes,
                &self.native,
                self.native_bytes,
            ),
        }
    }
}

/// The structured advisory report for one overlap candidate (roadmap v0.4):
/// data locations per side, and warnings covering everything the user must
/// check before touching files. No file ops.
pub struct MigrationReport {
    pub display_name: String,
    pub direction: Direction,
    /// The overlap match confidence — an `Unknown` match caps how much any
    /// of this should be trusted (P3).
    pub confidence: Confidence,
    /// Pairs where at least one side exists on disk.
    pub mappings: Vec<PathMapping>,
    pub warnings: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direction_flips_and_names_its_target() {
        assert_eq!(Direction::ToFlatpak.flipped(), Direction::ToNative);
        assert_eq!(Direction::ToNative.flipped(), Direction::ToFlatpak);
        assert_eq!(Direction::ToFlatpak.target(), "flatpak");
        assert_eq!(Direction::ToNative.target(), "native");
    }

    #[test]
    fn endpoints_swap_with_direction() {
        let m = PathMapping {
            kind: PathKind::Config,
            native: "~/.config/vlc".into(),
            flatpak: "~/.var/app/org.videolan.VLC/config/vlc".into(),
            native_bytes: Some(10),
            flatpak_bytes: None,
            confidence: Confidence::Inferred,
        };
        let (from, fb, to, tb) = m.endpoints(Direction::ToFlatpak);
        assert_eq!(from, "~/.config/vlc");
        assert_eq!(fb, Some(10));
        assert_eq!(to, "~/.var/app/org.videolan.VLC/config/vlc");
        assert_eq!(tb, None);
        let (from, fb, _, tb) = m.endpoints(Direction::ToNative);
        assert_eq!(from, "~/.var/app/org.videolan.VLC/config/vlc");
        assert_eq!(fb, None);
        assert_eq!(tb, Some(10));
    }

    #[test]
    fn path_kinds_display_lowercase() {
        assert_eq!(PathKind::Config.to_string(), "config");
        assert_eq!(PathKind::Data.to_string(), "data");
        assert_eq!(PathKind::Cache.to_string(), "cache");
        assert_eq!(PathKind::Profile.to_string(), "profile");
    }
}
