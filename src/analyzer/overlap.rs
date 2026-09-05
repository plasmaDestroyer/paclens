//! Overlap detection (design §9): find applications installed both natively and
//! as Flatpaks. Pure — reads only the scan plus config-derived inputs; the
//! bundled map ships inside the binary via `include_str!()`.
//!
//! Matching runs in priority order — known map (`Confirmed`) → reverse-DNS
//! suffix (`Inferred`) → display-name match (`Unknown`) — first hit wins, and
//! a generic-name blocklist suppresses false positives. A missed overlap is
//! better than a wrong one (design §10).

use std::collections::HashMap;

use serde::Deserialize;

use crate::config::ExtraMapping;
use crate::model::{
    Confidence, InstallReason, MatchMethod, OverlapCandidate, Package, PackageRef,
    PrimaryHeuristic, ScanResult, Tradeoff,
};

/// Generic pacman names that must never be overlap targets (design §9 +
/// design §10). Shipped in the binary, not configurable.
const BLOCKLIST: [&str; 11] = [
    "base",
    "linux",
    "linux-headers",
    "glibc",
    "gcc",
    "files",
    "core",
    "extra",
    "man",
    "lib",
    "utils",
];

const BUNDLED_MAP: &str = include_str!("../../overlap_map.toml");

#[derive(Deserialize)]
struct MapFile {
    #[serde(default)]
    mapping: Vec<MapEntry>,
}

#[derive(Deserialize)]
pub(crate) struct MapEntry {
    pub(crate) flatpak_id: String,
    pacman_name: String,
    #[serde(default)]
    alt_names: Vec<String>,
    /// Curated native profile directories (`~/`-relative) — the migration
    /// advisory's `Confirmed` tier (v0.4).
    #[serde(default)]
    pub(crate) profile_dirs: Vec<String>,
}

/// The bundled map plus user extras, as raw entries. The overlap matcher
/// flattens this to names; the migration advisory reads `profile_dirs`.
pub(crate) fn map_entries(extra: &[ExtraMapping]) -> Vec<MapEntry> {
    let mut entries = toml::from_str::<MapFile>(BUNDLED_MAP)
        .unwrap_or(MapFile {
            mapping: Vec::new(),
        })
        .mapping;
    entries.extend(extra.iter().map(|e| MapEntry {
        flatpak_id: e.flatpak_id.clone(),
        pacman_name: e.pacman_name.clone(),
        alt_names: e.alt_names.clone(),
        profile_dirs: e.profile_dirs.clone(),
    }));
    entries
}

/// flatpak id → candidate pacman names, bundled map + user extras (extras win
/// by being checked from the same table; duplicates simply append names).
fn known_map(extra: &[ExtraMapping]) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for entry in map_entries(extra) {
        let names = map.entry(entry.flatpak_id).or_default();
        names.push(entry.pacman_name);
        names.extend(entry.alt_names);
    }
    map
}

/// Detect overlaps between pacman packages and Flatpak apps. `ignore` comes
/// from `config.overlap.ignore` (pacman names or Flatpak app IDs).
pub fn detect_overlaps(
    scan: &ScanResult,
    ignore: &[String],
    extra_mappings: &[ExtraMapping],
) -> Vec<OverlapCandidate> {
    // "Native" = anything installed through libalpm — repo or AUR (v0.3).
    let native: HashMap<&str, &Package> = scan
        .packages
        .iter()
        .filter(|p| crate::analyzer::graph::is_alpm(&p.source_id))
        .map(|p| (p.name.as_str(), p))
        .collect();
    let map = known_map(extra_mappings);
    let ignored = |name: &str| ignore.iter().any(|i| i == name);

    let mut out: Vec<OverlapCandidate> = scan
        .packages
        .iter()
        // Runtimes are never overlap candidates (spec 9.1 + 9.3).
        .filter(|p| p.source_id.as_str().starts_with("flatpak") && !p.runtime)
        .filter(|app| !ignored(&app.name))
        .filter_map(|app| {
            let (pacman_pkg, method, confidence) = match_app(app, &native, &map)?;
            if ignored(&pacman_pkg.name) {
                return None;
            }
            let profile_size = scan.flatpak_profile_sizes.get(&app.name).copied();
            Some(candidate(app, pacman_pkg, method, confidence, profile_size))
        })
        .collect();
    out.sort_by(|a, b| a.display_name.cmp(&b.display_name));
    out
}

/// The design §9 pipeline: first match wins.
fn match_app<'a>(
    app: &Package,
    native: &HashMap<&str, &'a Package>,
    map: &HashMap<String, Vec<String>>,
) -> Option<(&'a Package, MatchMethod, Confidence)> {
    // Step 1: known name map — curated, so it bypasses the blocklist.
    if let Some(names) = map.get(&app.name)
        && let Some(pkg) = names.iter().find_map(|n| native.get(n.as_str()))
    {
        return Some((pkg, MatchMethod::KnownMap, Confidence::Confirmed));
    }

    let usable = |name: &str| !BLOCKLIST.contains(&name);

    // Step 2: reverse-DNS suffix (org.mozilla.firefox → firefox).
    let suffix = app.name.rsplit('.').next().unwrap_or("").to_lowercase();
    if usable(&suffix)
        && let Some(pkg) = native.get(suffix.as_str())
    {
        return Some((pkg, MatchMethod::ReverseDnsSuffix, Confidence::Inferred));
    }

    // Step 3: display name (from the scan's flatpak metadata), lowercased,
    // whitespace stripped.
    let display: String = app
        .description
        .as_deref()?
        .to_lowercase()
        .split_whitespace()
        .collect();
    if usable(&display)
        && let Some(pkg) = native.get(display.as_str())
    {
        return Some((pkg, MatchMethod::DisplayNameMatch, Confidence::Unknown));
    }
    None
}

/// Spec §9.4 heuristic 2 threshold: a profile this big means user data.
const PROFILE_DATA_THRESHOLD: u64 = 10 * 1024 * 1024;

fn candidate(
    app: &Package,
    native: &Package,
    match_method: MatchMethod,
    confidence: Confidence,
    profile_size: Option<u64>,
) -> OverlapCandidate {
    // Spec §9.4, in order: explicit native vs unknown flatpak → native;
    // a >10 MiB flatpak profile → flatpak (user has data there); else
    // unknown. Advisory only — never acted on.
    let likely_primary = if native.install_reason == InstallReason::Explicit
        && app.install_reason == InstallReason::Unknown
    {
        PrimaryHeuristic::Native
    } else if profile_size.is_some_and(|b| b > PROFILE_DATA_THRESHOLD) {
        PrimaryHeuristic::Flatpak
    } else {
        PrimaryHeuristic::Unknown
    };

    let display_name = app
        .description
        .clone()
        .unwrap_or_else(|| app.name.rsplit('.').next().unwrap_or(&app.name).to_string());

    OverlapCandidate {
        display_name,
        native_package: Some(package_ref(native)),
        flatpak_app: Some(package_ref(app)),
        match_method,
        confidence,
        tradeoff: Tradeoff {
            native_version: Some(native.version.clone()),
            flatpak_version: Some(app.version.clone()),
            // Display-only path — built from the app id, never read from
            // disk (the analyzer stays pure; the scanner measured the size).
            flatpak_profile_path: profile_size
                .map(|_| std::path::PathBuf::from(format!("~/.var/app/{}", app.name))),
            flatpak_profile_size_bytes: profile_size,
            likely_primary,
            ..Tradeoff::default()
        },
    }
}

fn package_ref(p: &Package) -> PackageRef {
    PackageRef {
        name: p.name.clone(),
        version: p.version.clone(),
        source_id: p.source_id.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SourceId;
    use crate::model::{CacheSizes, SCHEMA_VERSION};
    use chrono::Utc;

    fn pacman_pkg(name: &str, reason: InstallReason) -> Package {
        Package {
            name: name.to_string(),
            version: "128.0-1".to_string(),
            source_id: SourceId::pacman(),
            install_reason: reason,
            size_bytes: None,
            description: None,
            depends_on: Vec::new(),
            required_by: Vec::new(),
            optional_deps: Vec::new(),
            provides: Vec::new(),
            runtime: false,
            foreign: false,
            signed: true,
            packager: None,
        }
    }

    fn flatpak_app(id: &str, display: Option<&str>) -> Package {
        Package {
            name: id.to_string(),
            version: "128.0".to_string(),
            source_id: SourceId::flatpak_user(),
            install_reason: InstallReason::Unknown,
            size_bytes: None,
            description: display.map(|d| d.to_string()),
            depends_on: Vec::new(),
            required_by: Vec::new(),
            optional_deps: Vec::new(),
            provides: Vec::new(),
            runtime: false,
            foreign: false,
            signed: true,
            packager: None,
        }
    }

    fn scan(packages: Vec<Package>) -> ScanResult {
        ScanResult {
            schema_version: SCHEMA_VERSION,
            scanned_at: Utc::now(),
            sources: Vec::new(),
            packages,
            updates: Vec::new(),
            cache_sizes: CacheSizes::default(),
            flatpak_profile_sizes: Default::default(),
            profile_dir_sizes: Default::default(),
            aur_helper: crate::providers::aur::HelperChoice::Detected(
                crate::providers::aur::AurHelper::Paru,
            ),
            kernel: None,
            pacfiles: Vec::new(),
            stale_processes: Vec::new(),
        }
    }

    fn detect(packages: Vec<Package>) -> Vec<OverlapCandidate> {
        detect_overlaps(&scan(packages), &[], &[])
    }

    #[test]
    fn known_map_match_is_confirmed() {
        let found = detect(vec![
            pacman_pkg("firefox", InstallReason::Explicit),
            flatpak_app("org.mozilla.firefox", Some("Firefox")),
        ]);
        assert_eq!(found.len(), 1);
        let o = &found[0];
        assert_eq!(o.match_method, MatchMethod::KnownMap);
        assert_eq!(o.confidence, Confidence::Confirmed);
        assert_eq!(o.display_name, "Firefox");
        assert_eq!(o.native_package.as_ref().unwrap().name, "firefox");
        assert_eq!(o.flatpak_app.as_ref().unwrap().name, "org.mozilla.firefox");
    }

    #[test]
    fn known_map_alt_names_are_checked() {
        // com.brave.Browser maps to brave-bin with alt brave-browser.
        let found = detect(vec![
            pacman_pkg("brave-browser", InstallReason::Explicit),
            flatpak_app("com.brave.Browser", Some("Brave")),
        ]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].match_method, MatchMethod::KnownMap);
    }

    #[test]
    fn reverse_dns_suffix_is_inferred() {
        let found = detect(vec![
            pacman_pkg("celluloid", InstallReason::Explicit),
            flatpak_app("io.github.celluloid_player.celluloid", None),
        ]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].match_method, MatchMethod::ReverseDnsSuffix);
        assert_eq!(found[0].confidence, Confidence::Inferred);
    }

    #[test]
    fn display_name_match_is_unknown_confidence() {
        let found = detect(vec![
            pacman_pkg("obscuretool", InstallReason::Explicit),
            flatpak_app("com.example.SomeId", Some("Obscure Tool")),
        ]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].match_method, MatchMethod::DisplayNameMatch);
        assert_eq!(found[0].confidence, Confidence::Unknown);
    }

    #[test]
    fn no_native_package_means_no_overlap() {
        assert!(detect(vec![flatpak_app("org.mozilla.firefox", Some("Firefox"))]).is_empty());
    }

    #[test]
    fn blocklist_suppresses_generic_suffix_matches() {
        // A hypothetical app id ending in .base must not match pacman "base".
        let found = detect(vec![
            pacman_pkg("base", InstallReason::Explicit),
            flatpak_app("com.example.base", Some("base")),
        ]);
        assert!(found.is_empty(), "generic name matched: {}", found.len());
    }

    #[test]
    fn config_ignore_suppresses_by_either_name() {
        let s = scan(vec![
            pacman_pkg("firefox", InstallReason::Explicit),
            flatpak_app("org.mozilla.firefox", Some("Firefox")),
        ]);
        assert!(detect_overlaps(&s, &["firefox".to_string()], &[]).is_empty());
        assert!(detect_overlaps(&s, &["org.mozilla.firefox".to_string()], &[]).is_empty());
    }

    #[test]
    fn user_extra_mappings_extend_the_map() {
        let extra = vec![ExtraMapping {
            flatpak_id: "com.custom.App".to_string(),
            pacman_name: "custom-app".to_string(),
            alt_names: Vec::new(),
            profile_dirs: Vec::new(),
        }];
        let s = scan(vec![
            pacman_pkg("custom-app", InstallReason::Explicit),
            flatpak_app("com.custom.App", Some("Custom")),
        ]);
        let found = detect_overlaps(&s, &[], &extra);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].match_method, MatchMethod::KnownMap);
    }

    #[test]
    fn big_flatpak_profile_flips_the_primary_heuristic() {
        // Rule 1 off (native is a dependency); >10 MiB profile → Flatpak.
        let mut s = scan(vec![
            pacman_pkg("firefox", InstallReason::Dependency),
            flatpak_app("org.mozilla.firefox", Some("Firefox")),
        ]);
        s.flatpak_profile_sizes
            .insert("org.mozilla.firefox".to_string(), 52_428_800);
        let found = detect_overlaps(&s, &[], &[]);
        assert_eq!(found[0].tradeoff.likely_primary, PrimaryHeuristic::Flatpak);
        assert_eq!(
            found[0].tradeoff.flatpak_profile_size_bytes,
            Some(52_428_800)
        );
        assert_eq!(
            found[0].tradeoff.flatpak_profile_path.as_deref(),
            Some(std::path::Path::new("~/.var/app/org.mozilla.firefox"))
        );

        // Rule 1 outranks rule 2 (spec order).
        let mut s = scan(vec![
            pacman_pkg("firefox", InstallReason::Explicit),
            flatpak_app("org.mozilla.firefox", Some("Firefox")),
        ]);
        s.flatpak_profile_sizes
            .insert("org.mozilla.firefox".to_string(), 52_428_800);
        let found = detect_overlaps(&s, &[], &[]);
        assert_eq!(found[0].tradeoff.likely_primary, PrimaryHeuristic::Native);

        // A tiny profile is not user data.
        let mut s = scan(vec![
            pacman_pkg("firefox", InstallReason::Dependency),
            flatpak_app("org.mozilla.firefox", Some("Firefox")),
        ]);
        s.flatpak_profile_sizes
            .insert("org.mozilla.firefox".to_string(), 1024);
        let found = detect_overlaps(&s, &[], &[]);
        assert_eq!(found[0].tradeoff.likely_primary, PrimaryHeuristic::Unknown);
    }

    #[test]
    fn primary_heuristic_prefers_explicit_native() {
        let found = detect(vec![
            pacman_pkg("firefox", InstallReason::Explicit),
            flatpak_app("org.mozilla.firefox", Some("Firefox")),
        ]);
        assert_eq!(found[0].tradeoff.likely_primary, PrimaryHeuristic::Native);

        let found = detect(vec![
            pacman_pkg("firefox", InstallReason::Dependency),
            flatpak_app("org.mozilla.firefox", Some("Firefox")),
        ]);
        assert_eq!(found[0].tradeoff.likely_primary, PrimaryHeuristic::Unknown);
    }

    #[test]
    fn tradeoff_carries_both_versions() {
        let found = detect(vec![
            pacman_pkg("firefox", InstallReason::Explicit),
            flatpak_app("org.mozilla.firefox", Some("Firefox")),
        ]);
        let t = &found[0].tradeoff;
        assert_eq!(t.native_version.as_deref(), Some("128.0-1"));
        assert_eq!(t.flatpak_version.as_deref(), Some("128.0"));
    }

    #[test]
    fn known_map_wins_over_suffix() {
        // firefox matches via map (Confirmed), not suffix (Inferred).
        let found = detect(vec![
            pacman_pkg("firefox", InstallReason::Explicit),
            flatpak_app("org.mozilla.firefox", None),
        ]);
        assert_eq!(found[0].confidence, Confidence::Confirmed);
    }

    #[test]
    fn bundled_map_parses() {
        let map = known_map(&[]);
        assert!(map.len() > 10, "bundled map too small: {}", map.len());
        assert!(map.contains_key("org.mozilla.firefox"));
    }

    #[test]
    fn results_sort_by_display_name() {
        let found = detect(vec![
            pacman_pkg("firefox", InstallReason::Explicit),
            pacman_pkg("chromium", InstallReason::Explicit),
            flatpak_app("org.mozilla.firefox", Some("Firefox")),
            flatpak_app("org.chromium.Chromium", Some("Chromium")),
        ]);
        let names: Vec<&str> = found.iter().map(|o| o.display_name.as_str()).collect();
        assert_eq!(names, vec!["Chromium", "Firefox"]);
    }
}
