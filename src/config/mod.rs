//! Configuration loading.
//!
//! Resolves the config path, creates a default file on first run, and parses
//! TOML into [`Config`]. Parsing is split from IO so it can be unit-tested.

pub mod schema;

pub use schema::{ColorTheme, Config};

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};
use directories::ProjectDirs;

/// The bundled default config, written to disk on first run.
const DEFAULT_CONFIG: &str = include_str!("../../config.default.toml");

/// A parsed config plus any unknown keys that were ignored during parsing.
#[derive(Debug)]
pub struct LoadedConfig {
    pub config: Config,
    pub unknown_keys: Vec<String>,
}

/// Errors that can occur while parsing config text.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("not valid TOML")]
    Parse(#[from] toml::de::Error),
}

/// Parse config text into a [`Config`], collecting any unknown (ignored) keys.
///
/// Pure: no IO. Unknown keys are tolerated (forward compatibility); invalid
/// enum values are tolerated at parse time and handled by typed accessors.
pub fn parse_config(text: &str) -> Result<LoadedConfig, ConfigError> {
    let deserializer = toml::Deserializer::parse(text)?;
    let mut unknown_keys = Vec::new();
    let config =
        serde_ignored::deserialize(deserializer, |path| unknown_keys.push(path.to_string()))?;
    Ok(LoadedConfig {
        config,
        unknown_keys,
    })
}

/// Load config from `explicit_path`, or from the default location (creating a
/// default file there on first run if absent).
pub fn load(explicit_path: Option<&Path>) -> anyhow::Result<LoadedConfig> {
    let path = match explicit_path {
        Some(p) => p.to_path_buf(),
        None => {
            let p = default_config_path()?;
            if !p.exists() {
                write_default_config(&p)?;
            }
            p
        }
    };

    let text = fs::read_to_string(&path)
        .with_context(|| format!("failed to read config file: {}", path.display()))?;

    parse_config(&text).with_context(|| format!("in config file: {}", path.display()))
}

/// `~/.config/paclens/config.toml`, resolved via XDG.
pub fn default_config_path() -> anyhow::Result<PathBuf> {
    let dirs = ProjectDirs::from("", "", "paclens")
        .ok_or_else(|| anyhow!("could not determine the config directory for this platform"))?;
    Ok(dirs.config_dir().join("config.toml"))
}

fn write_default_config(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config directory: {}", parent.display()))?;
    }
    fs::write(path, DEFAULT_CONFIG)
        .with_context(|| format!("failed to write default config: {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema::LogLevel;

    #[test]
    fn empty_input_yields_all_defaults() {
        let loaded = parse_config("").unwrap();
        assert_eq!(loaded.config.general.cache_ttl, 3600);
        assert_eq!(loaded.config.general.log_keep_count, 10);
        assert!(loaded.config.sources.pacman);
        assert!(loaded.config.sources.flatpak);
        assert_eq!(loaded.config.scan.provider_timeout_secs, 10);
        assert_eq!(loaded.config.why.max_depth, 20);
        assert!(loaded.unknown_keys.is_empty());
    }

    #[test]
    fn explicit_value_overrides_default() {
        let loaded = parse_config("[general]\ncache_ttl = 60\n").unwrap();
        assert_eq!(loaded.config.general.cache_ttl, 60);
    }

    #[test]
    fn disabled_source_is_parsed() {
        let loaded = parse_config("[sources]\nflatpak = false\n").unwrap();
        assert!(!loaded.config.sources.flatpak);
    }

    #[test]
    fn unknown_key_is_collected() {
        let loaded = parse_config("[general]\nnonsense = 1\n").unwrap();
        assert_eq!(loaded.unknown_keys, vec!["general.nonsense".to_string()]);
    }

    #[test]
    fn unknown_key_does_not_prevent_parsing() {
        let loaded = parse_config("[general]\nnonsense = 1\ncache_ttl = 99\n").unwrap();
        assert_eq!(loaded.config.general.cache_ttl, 99);
    }

    #[test]
    fn malformed_toml_errors() {
        let result = parse_config("[general\ncache_ttl = ");
        assert!(result.is_err());
    }

    #[test]
    fn extra_mapping_is_parsed() {
        let text =
            "[[overlap.extra_mappings]]\nflatpak_id = \"org.x.App\"\npacman_name = \"app\"\n";
        let loaded = parse_config(text).unwrap();
        assert_eq!(loaded.config.overlap.extra_mappings.len(), 1);
        assert_eq!(loaded.config.overlap.extra_mappings[0].pacman_name, "app");
    }

    #[test]
    fn log_level_parses_known_value() {
        let loaded = parse_config("[general]\nlog_level = \"debug\"\n").unwrap();
        assert_eq!(loaded.config.general.log_level(), LogLevel::Debug);
    }

    #[test]
    fn invalid_log_level_falls_back_to_default() {
        let loaded = parse_config("[general]\nlog_level = \"loud\"\n").unwrap();
        assert_eq!(loaded.config.general.log_level(), LogLevel::Info);
    }

    #[test]
    fn invalid_log_level_produces_validation_warning() {
        let loaded = parse_config("[general]\nlog_level = \"loud\"\n").unwrap();
        let warnings = loaded.config.validation_warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("general.log_level"));
    }

    #[test]
    fn invalid_color_theme_produces_validation_warning() {
        let loaded = parse_config("[general]\ncolor_theme = \"purple\"\n").unwrap();
        let warnings = loaded.config.validation_warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("general.color_theme"));
    }

    #[test]
    fn invalid_min_confidence_produces_validation_warning() {
        let loaded = parse_config("[overlap]\nmin_confidence = \"maybe\"\n").unwrap();
        let warnings = loaded.config.validation_warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("overlap.min_confidence"));
    }

    #[test]
    fn valid_config_has_no_validation_warnings() {
        let loaded = parse_config("").unwrap();
        assert!(loaded.config.validation_warnings().is_empty());
    }

    #[test]
    fn bundled_default_config_parses_cleanly() {
        let loaded = parse_config(DEFAULT_CONFIG).unwrap();
        assert!(
            loaded.unknown_keys.is_empty(),
            "bundled default has keys the schema does not know: {:?}",
            loaded.unknown_keys
        );
        assert!(loaded.config.validation_warnings().is_empty());
    }
}
