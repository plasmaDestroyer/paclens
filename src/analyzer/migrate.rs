//! Migration advisory (roadmap v0.4): for an overlap candidate, derive where
//! each side stores its data and assemble a structured, read-only report.
//!
//! Pure — path pairs are derived from names and the curated map; existence
//! and sizes come from the scanner's probe (`ScanResult.profile_dir_sizes`).
//! Nothing here reads the filesystem, and nothing downstream executes any of
//! it: the report is "what you would need to do manually" (roadmap v0.4).

// TODO(v0.4): drop once the scanner/CLI/TUI commits consume these fns.
#![allow(dead_code)]

use crate::config::ExtraMapping;
use crate::model::{
    Confidence, Direction, MigrationReport, OverlapCandidate, PathKind, PathMapping,
    PrimaryHeuristic, ScanResult,
};

use super::overlap::map_entries;

/// Classify a `~/`-relative native path by XDG convention.
fn classify(path: &str) -> PathKind {
    if path.starts_with("~/.config/") {
        PathKind::Config
    } else if path.starts_with("~/.local/share/") {
        PathKind::Data
    } else if path.starts_with("~/.cache/") {
        PathKind::Cache
    } else {
        PathKind::Profile
    }
}

/// The Flatpak counterpart of a native path. XDG dirs remap to the sandbox's
/// `config`/`data`/`cache` (Flatpak points the XDG env vars there); anything
/// else keeps its home-relative name under the sandbox HOME
/// (`~/.mozilla` → `~/.var/app/<id>/.mozilla`).
fn flatpak_counterpart(native: &str, app_id: &str) -> String {
    let base = format!("~/.var/app/{app_id}");
    if let Some(rest) = native.strip_prefix("~/.config/") {
        format!("{base}/config/{rest}")
    } else if let Some(rest) = native.strip_prefix("~/.local/share/") {
        format!("{base}/data/{rest}")
    } else if let Some(rest) = native.strip_prefix("~/.cache/") {
        format!("{base}/cache/{rest}")
    } else {
        format!("{base}/{}", native.trim_start_matches("~/"))
    }
}

/// The native package name plus de-suffixed variants
/// (`visual-studio-code-bin` also tries `visual-studio-code`).
fn guess_names(native: &str) -> Vec<String> {
    let mut names = vec![native.to_string()];
    for suffix in ["-bin", "-git", "-desktop"] {
        if let Some(stripped) = native.strip_suffix(suffix)
            && !stripped.is_empty()
        {
            names.push(stripped.to_string());
        }
    }
    names
}

/// Candidate path pairs for one overlap: the curated map's `profile_dirs`
/// first (`Confirmed`), then XDG-convention guesses from the native package
/// name (`Inferred`). Byte counts stay `None` — this is pure name-based
/// derivation; [`report`] joins in the scanner's measurements.
pub fn candidate_mappings(
    candidate: &OverlapCandidate,
    extra: &[ExtraMapping],
) -> Vec<PathMapping> {
    let (Some(native), Some(app)) = (&candidate.native_package, &candidate.flatpak_app) else {
        return Vec::new();
    };
    let mut out: Vec<PathMapping> = Vec::new();
    let push = |native_path: String, confidence: Confidence, out: &mut Vec<PathMapping>| {
        if out.iter().any(|m| m.native == native_path) {
            return; // first entry wins — Confirmed dirs land before guesses
        }
        out.push(PathMapping {
            kind: classify(&native_path),
            flatpak: flatpak_counterpart(&native_path, &app.name),
            native: native_path,
            native_bytes: None,
            flatpak_bytes: None,
            confidence,
        });
    };
    for entry in map_entries(extra)
        .iter()
        .filter(|e| e.flatpak_id == app.name)
    {
        for dir in &entry.profile_dirs {
            push(dir.clone(), Confidence::Confirmed, &mut out);
        }
    }
    for name in guess_names(&native.name) {
        push(format!("~/.config/{name}"), Confidence::Inferred, &mut out);
        push(
            format!("~/.local/share/{name}"),
            Confidence::Inferred,
            &mut out,
        );
        push(format!("~/.cache/{name}"), Confidence::Inferred, &mut out);
    }
    out
}

/// Every path (both sides, all candidates) the scanner should measure.
/// Sorted and deduplicated so probe order is stable.
pub fn probe_paths(candidates: &[OverlapCandidate], extra: &[ExtraMapping]) -> Vec<String> {
    let mut all: Vec<String> = candidates
        .iter()
        .flat_map(|c| candidate_mappings(c, extra))
        .flat_map(|m| [m.native, m.flatpak])
        .collect();
    all.sort();
    all.dedup();
    all
}

/// Do a pacman and a flatpak version string describe the same upstream
/// release? pacman carries `epoch:upstream-pkgrel`; compare the upstream part.
fn versions_agree(native: &str, flatpak: &str) -> bool {
    let upstream = native.split_once(':').map_or(native, |(_, rest)| rest);
    let upstream = upstream.rsplit_once('-').map_or(upstream, |(v, _)| v);
    native == flatpak || upstream == flatpak
}

/// Assemble the advisory report for one candidate. `direction` overrides the
/// default (toward the likely-primary side; unknown primary defaults to
/// native → flatpak, with a warning saying so).
pub fn report(
    scan: &ScanResult,
    candidate: &OverlapCandidate,
    extra: &[ExtraMapping],
    direction: Option<Direction>,
) -> MigrationReport {
    let sizes = &scan.profile_dir_sizes;
    let mappings: Vec<PathMapping> = candidate_mappings(candidate, extra)
        .into_iter()
        .filter_map(|mut m| {
            m.native_bytes = sizes.get(&m.native).copied();
            m.flatpak_bytes = sizes.get(&m.flatpak).copied();
            // Only pairs with something on disk make the report.
            (m.native_bytes.is_some() || m.flatpak_bytes.is_some()).then_some(m)
        })
        .collect();

    let defaulted = direction.is_none();
    let direction = direction.unwrap_or(match candidate.tradeoff.likely_primary {
        PrimaryHeuristic::Native => Direction::ToNative,
        PrimaryHeuristic::Flatpak | PrimaryHeuristic::Unknown => Direction::ToFlatpak,
    });

    let mut warnings = Vec::new();
    if defaulted && candidate.tradeoff.likely_primary == PrimaryHeuristic::Unknown {
        warnings.push("primary side unclear — defaulting to native → flatpak".to_string());
    }
    warnings.push(format!(
        "close {} everywhere before copying anything",
        candidate.display_name
    ));
    warnings
        .push("backup both sides first — this report is advisory, nothing is verified".to_string());
    if let (Some(n), Some(f)) = (
        &candidate.tradeoff.native_version,
        &candidate.tradeoff.flatpak_version,
    ) && !versions_agree(n, f)
    {
        warnings.push(format!(
            "versions differ (native {n}, flatpak {f}) — profile formats may not roundtrip"
        ));
    }
    if mappings
        .iter()
        .any(|m| m.confidence != Confidence::Confirmed)
    {
        warnings.push(
            "[inferred] paths are XDG-convention guesses — verify before copying".to_string(),
        );
    }
    if mappings.iter().any(|m| {
        let (_, _, _, to_bytes) = m.endpoints(direction);
        to_bytes.is_some()
    }) {
        warnings.push(
            "the target side already has data — copying over it may clobber a live profile"
                .to_string(),
        );
    }

    MigrationReport {
        display_name: candidate.display_name.clone(),
        direction,
        confidence: candidate.confidence,
        mappings,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MatchMethod, PackageRef, SourceId, Tradeoff};

    fn candidate(native: &str, app_id: &str, primary: PrimaryHeuristic) -> OverlapCandidate {
        OverlapCandidate {
            display_name: "Firefox".to_string(),
            native_package: Some(PackageRef {
                name: native.to_string(),
                version: "141.0-1".to_string(),
                source_id: SourceId::pacman(),
            }),
            flatpak_app: Some(PackageRef {
                name: app_id.to_string(),
                version: "141.0".to_string(),
                source_id: SourceId::flatpak_user(),
            }),
            match_method: MatchMethod::KnownMap,
            confidence: Confidence::Confirmed,
            tradeoff: Tradeoff {
                native_version: Some("141.0-1".to_string()),
                flatpak_version: Some("141.0".to_string()),
                likely_primary: primary,
                ..Tradeoff::default()
            },
        }
    }

    fn firefox(primary: PrimaryHeuristic) -> OverlapCandidate {
        candidate("firefox", "org.mozilla.firefox", primary)
    }

    fn scan_with(sizes: &[(&str, u64)]) -> ScanResult {
        let mut scan = ScanResult::empty();
        for (path, bytes) in sizes {
            scan.profile_dir_sizes.insert(path.to_string(), *bytes);
        }
        scan
    }

    #[test]
    fn counterparts_follow_the_sandbox_layout() {
        assert_eq!(
            flatpak_counterpart("~/.config/vlc", "org.videolan.VLC"),
            "~/.var/app/org.videolan.VLC/config/vlc"
        );
        assert_eq!(
            flatpak_counterpart("~/.local/share/krita", "org.kde.krita"),
            "~/.var/app/org.kde.krita/data/krita"
        );
        assert_eq!(
            flatpak_counterpart("~/.cache/mpv", "io.mpv.Mpv"),
            "~/.var/app/io.mpv.Mpv/cache/mpv"
        );
        assert_eq!(
            flatpak_counterpart("~/.mozilla", "org.mozilla.firefox"),
            "~/.var/app/org.mozilla.firefox/.mozilla"
        );
    }

    #[test]
    fn curated_dirs_come_first_and_are_confirmed() {
        let mappings = candidate_mappings(&firefox(PrimaryHeuristic::Unknown), &[]);
        assert_eq!(mappings[0].native, "~/.mozilla");
        assert_eq!(mappings[0].confidence, Confidence::Confirmed);
        assert_eq!(mappings[0].kind, PathKind::Profile);
        // XDG guesses follow at Inferred.
        let guess = mappings
            .iter()
            .find(|m| m.native == "~/.config/firefox")
            .expect("XDG guess present");
        assert_eq!(guess.confidence, Confidence::Inferred);
        assert_eq!(guess.kind, PathKind::Config);
    }

    #[test]
    fn suffix_stripped_guesses_are_added() {
        let c = candidate("visual-studio-code-bin", "com.visualstudio.code", {
            PrimaryHeuristic::Unknown
        });
        let mappings = candidate_mappings(&c, &[]);
        assert!(
            mappings
                .iter()
                .any(|m| m.native == "~/.config/visual-studio-code")
        );
        // The curated dir is still first.
        assert_eq!(mappings[0].native, "~/.config/Code");
    }

    #[test]
    fn extra_mapping_profile_dirs_are_confirmed() {
        let extra = vec![ExtraMapping {
            flatpak_id: "com.custom.App".to_string(),
            pacman_name: "custom-app".to_string(),
            alt_names: Vec::new(),
            profile_dirs: vec!["~/.customapp".to_string()],
        }];
        let c = candidate("custom-app", "com.custom.App", PrimaryHeuristic::Unknown);
        let mappings = candidate_mappings(&c, &extra);
        assert_eq!(mappings[0].native, "~/.customapp");
        assert_eq!(mappings[0].confidence, Confidence::Confirmed);
    }

    #[test]
    fn probe_paths_cover_both_sides_deduplicated() {
        let paths = probe_paths(&[firefox(PrimaryHeuristic::Unknown)], &[]);
        assert!(paths.contains(&"~/.mozilla".to_string()));
        assert!(paths.contains(&"~/.var/app/org.mozilla.firefox/.mozilla".to_string()));
        assert!(paths.contains(&"~/.config/firefox".to_string()));
        let mut deduped = paths.clone();
        deduped.dedup();
        assert_eq!(paths, deduped);
    }

    #[test]
    fn report_keeps_only_pairs_that_exist_on_disk() {
        let scan = scan_with(&[("~/.mozilla", 1_200_000_000)]);
        let r = report(&scan, &firefox(PrimaryHeuristic::Unknown), &[], None);
        assert_eq!(r.mappings.len(), 1);
        assert_eq!(r.mappings[0].native, "~/.mozilla");
        assert_eq!(r.mappings[0].native_bytes, Some(1_200_000_000));
        assert_eq!(r.mappings[0].flatpak_bytes, None);
    }

    #[test]
    fn direction_defaults_toward_the_likely_primary() {
        let scan = scan_with(&[("~/.mozilla", 1)]);
        let r = report(&scan, &firefox(PrimaryHeuristic::Native), &[], None);
        assert_eq!(r.direction, Direction::ToNative);
        let r = report(&scan, &firefox(PrimaryHeuristic::Flatpak), &[], None);
        assert_eq!(r.direction, Direction::ToFlatpak);
        // Unknown primary: default to flatpak, but say so.
        let r = report(&scan, &firefox(PrimaryHeuristic::Unknown), &[], None);
        assert_eq!(r.direction, Direction::ToFlatpak);
        assert!(
            r.warnings
                .iter()
                .any(|w| w.contains("primary side unclear"))
        );
        // An explicit direction is never second-guessed (and never warned about).
        let r = report(
            &scan,
            &firefox(PrimaryHeuristic::Unknown),
            &[],
            Some(Direction::ToNative),
        );
        assert_eq!(r.direction, Direction::ToNative);
        assert!(
            !r.warnings
                .iter()
                .any(|w| w.contains("primary side unclear"))
        );
    }

    #[test]
    fn versions_agree_across_packaging_formats() {
        assert!(versions_agree("141.0-1", "141.0"));
        assert!(versions_agree("1:24.08-2", "24.08"));
        assert!(versions_agree("141.0", "141.0"));
        assert!(!versions_agree("141.0-1", "139.0"));
    }

    #[test]
    fn version_mismatch_warns() {
        let mut c = firefox(PrimaryHeuristic::Unknown);
        c.tradeoff.flatpak_version = Some("139.0".to_string());
        let scan = scan_with(&[("~/.mozilla", 1)]);
        let r = report(&scan, &c, &[], None);
        assert!(r.warnings.iter().any(|w| w.contains("versions differ")));
        // Agreeing versions stay quiet.
        let r = report(&scan, &firefox(PrimaryHeuristic::Unknown), &[], None);
        assert!(!r.warnings.iter().any(|w| w.contains("versions differ")));
    }

    #[test]
    fn existing_target_data_warns_about_clobbering() {
        let scan = scan_with(&[
            ("~/.mozilla", 1_200_000_000),
            ("~/.var/app/org.mozilla.firefox/.mozilla", 300_000_000),
        ]);
        let r = report(&scan, &firefox(PrimaryHeuristic::Unknown), &[], None);
        assert!(r.warnings.iter().any(|w| w.contains("clobber")));
        // No data on the target side → no clobber warning.
        let scan = scan_with(&[("~/.mozilla", 1_200_000_000)]);
        let r = report(&scan, &firefox(PrimaryHeuristic::Unknown), &[], None);
        assert!(!r.warnings.iter().any(|w| w.contains("clobber")));
    }

    #[test]
    fn inferred_paths_get_a_verify_warning() {
        // Only the curated ~/.mozilla exists → no inferred rows → no warning.
        let scan = scan_with(&[("~/.mozilla", 1)]);
        let r = report(&scan, &firefox(PrimaryHeuristic::Unknown), &[], None);
        assert!(!r.warnings.iter().any(|w| w.contains("XDG-convention")));
        // An inferred row appears → the warning appears with it.
        let scan = scan_with(&[("~/.mozilla", 1), ("~/.cache/firefox", 340_000_000)]);
        let r = report(&scan, &firefox(PrimaryHeuristic::Unknown), &[], None);
        assert!(r.warnings.iter().any(|w| w.contains("XDG-convention")));
    }

    #[test]
    fn report_carries_the_match_confidence_and_close_warning() {
        let scan = scan_with(&[("~/.mozilla", 1)]);
        let r = report(&scan, &firefox(PrimaryHeuristic::Unknown), &[], None);
        assert_eq!(r.confidence, Confidence::Confirmed);
        assert!(r.warnings.iter().any(|w| w.contains("close Firefox")));
        assert!(r.warnings.iter().any(|w| w.contains("backup")));
    }

    #[test]
    fn half_candidates_produce_nothing() {
        let mut c = firefox(PrimaryHeuristic::Unknown);
        c.native_package = None;
        assert!(candidate_mappings(&c, &[]).is_empty());
    }
}
