//! Configuration schema. Mirrors `config.default.toml`.
//!
//! Enum-like fields (`log_level`, `color_theme`, `min_confidence`) are stored as
//! strings so an invalid value never fails deserialization; they are validated
//! through typed accessors that fall back to the default. Call
//! [`Config::validation_warnings`] to surface invalid values to the user.

use serde::Deserialize;
use tracing::level_filters::LevelFilter;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general: General,
    pub sources: Sources,
    pub scan: Scan,
    pub why: Why,
    pub overlap: Overlap,
    pub cleanup: Cleanup,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct General {
    pub cache_ttl: u64,
    /// Which AUR helper to drive. Empty means autodetect, walking `PATH` in
    /// the order paru → yay → pikaur and taking the first hit. A name pins it.
    ///
    /// Empty-means-autodetect rather than recording the detected helper on
    /// first run, because a persisted value goes stale: install paru later and
    /// paclens would keep using the yay it wrote down months ago, for no
    /// reason visible from the outside.
    pub aur_helper: String,
    pub log_keep_count: u32,
    pub log_level: String,
    pub color_theme: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Sources {
    pub pacman: bool,
    pub flatpak: bool,
    pub aur: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Scan {
    pub flatpak_include_system: bool,
    pub flatpak_include_user: bool,
    pub provider_timeout_secs: u64,
    /// Pass `--devel` to paru's update check: compare VCS (-git) packages
    /// against the upstream HEAD instead of the version string. Slower.
    pub aur_devel: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Why {
    pub show_transitive: bool,
    pub max_depth: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Overlap {
    pub ignore: Vec<String>,
    pub min_confidence: String,
    pub extra_mappings: Vec<ExtraMapping>,
}

/// A user-supplied overlap mapping, merged into the bundled map.
#[derive(Debug, Clone, Deserialize)]
pub struct ExtraMapping {
    pub flatpak_id: String,
    pub pacman_name: String,
    #[serde(default)]
    pub alt_names: Vec<String>,
    /// Native profile directories (`~/`-relative) for the migration
    /// advisory, e.g. `["~/.mozilla"]`. Optional — XDG-convention guesses
    /// cover unmapped apps at lower confidence.
    #[serde(default)]
    pub profile_dirs: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Cleanup {
    pub orphan_ignore: Vec<String>,
}

impl Default for General {
    fn default() -> Self {
        Self {
            cache_ttl: 3600,
            aur_helper: String::new(),
            log_keep_count: 10,
            log_level: "info".to_string(),
            color_theme: "dark".to_string(),
        }
    }
}

impl Default for Sources {
    fn default() -> Self {
        Self {
            pacman: true,
            flatpak: true,
            aur: true,
        }
    }
}

impl Default for Scan {
    fn default() -> Self {
        Self {
            flatpak_include_system: true,
            flatpak_include_user: true,
            provider_timeout_secs: 10,
            aur_devel: false,
        }
    }
}

impl Default for Why {
    fn default() -> Self {
        Self {
            show_transitive: true,
            max_depth: 20,
        }
    }
}

impl Default for Overlap {
    fn default() -> Self {
        Self {
            ignore: Vec::new(),
            min_confidence: "Inferred".to_string(),
            extra_mappings: Vec::new(),
        }
    }
}

/// Log verbosity. Defaults to `Info`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
}

impl LogLevel {
    /// Parse a config string, case-insensitively. Returns `None` if unrecognized.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "error" => Some(Self::Error),
            "warn" => Some(Self::Warn),
            "info" => Some(Self::Info),
            "debug" => Some(Self::Debug),
            _ => None,
        }
    }

    pub fn to_level_filter(self) -> LevelFilter {
        match self {
            Self::Error => LevelFilter::ERROR,
            Self::Warn => LevelFilter::WARN,
            Self::Info => LevelFilter::INFO,
            Self::Debug => LevelFilter::DEBUG,
        }
    }
}

/// Color theme. Defaults to `Dark`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorTheme {
    #[default]
    Dark,
    Light,
    None,
}

impl ColorTheme {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

/// Minimum confidence shown in the overlap report. Defaults to `Inferred`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MinConfidence {
    Confirmed,
    #[default]
    Inferred,
    Unknown,
}

impl MinConfidence {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "confirmed" => Some(Self::Confirmed),
            "inferred" => Some(Self::Inferred),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    /// Does a candidate at `confidence` clear this floor? Shared by the CLI
    /// report and the TUI overlap screen (P5 — they must never disagree).
    pub fn allows(self, confidence: crate::model::Confidence) -> bool {
        use crate::model::Confidence;
        let floor = match self {
            MinConfidence::Confirmed => Confidence::Confirmed,
            MinConfidence::Inferred => Confidence::Inferred,
            MinConfidence::Unknown => Confidence::Unknown,
        };
        confidence <= floor // Ord: Confirmed < Inferred < Unknown
    }
}

impl Overlap {
    /// Validated confidence floor, falling back to the default on an invalid value.
    pub fn min_confidence(&self) -> MinConfidence {
        MinConfidence::parse(&self.min_confidence).unwrap_or_default()
    }
}

impl General {
    /// Validated log level, falling back to the default on an invalid value.
    pub fn log_level(&self) -> LogLevel {
        LogLevel::parse(&self.log_level).unwrap_or_default()
    }

    /// Validated color theme, falling back to the default on an invalid value.
    pub fn color_theme(&self) -> ColorTheme {
        ColorTheme::parse(&self.color_theme).unwrap_or_default()
    }
}

impl Config {
    /// Human-readable warnings for values that failed validation and were
    /// replaced with defaults. Empty when everything is valid.
    pub fn validation_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if LogLevel::parse(&self.general.log_level).is_none() {
            warnings.push(format!(
                "general.log_level: invalid value {:?}, using default \"info\"",
                self.general.log_level
            ));
        }
        if ColorTheme::parse(&self.general.color_theme).is_none() {
            warnings.push(format!(
                "general.color_theme: invalid value {:?}, using default \"dark\"",
                self.general.color_theme
            ));
        }
        if MinConfidence::parse(&self.overlap.min_confidence).is_none() {
            warnings.push(format!(
                "overlap.min_confidence: invalid value {:?}, using default \"Inferred\"",
                self.overlap.min_confidence
            ));
        }
        warnings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn general_with_theme(theme: &str) -> General {
        General {
            color_theme: theme.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn color_theme_accessor_parses_known_values() {
        assert_eq!(general_with_theme("light").color_theme(), ColorTheme::Light);
        assert_eq!(general_with_theme("none").color_theme(), ColorTheme::None);
        // case-insensitive
        assert_eq!(general_with_theme("DARK").color_theme(), ColorTheme::Dark);
    }

    #[test]
    fn color_theme_accessor_defaults_on_invalid() {
        assert_eq!(
            general_with_theme("chartreuse").color_theme(),
            ColorTheme::Dark
        );
    }

    #[test]
    fn log_level_accessor_defaults_on_invalid() {
        let g = General {
            log_level: "loud".to_string(),
            ..Default::default()
        };
        assert_eq!(g.log_level(), LogLevel::Info);
    }
}
