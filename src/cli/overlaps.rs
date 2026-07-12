//! `paclens overlaps` — list detected Flatpak/native overlaps (spec §11.4).
//!
//! Advisory only (roadmap v0.0.8): no suggested actions, no remove prompts.
//! Rendering goes through the shared `Styles`; the confidence label sits
//! inline with the match method, per the confidence model's rule 2.

use std::path::Path;

use crate::analyzer;
use crate::cli::style::Styles;
use crate::config::Config;
use crate::config::schema::MinConfidence;
use crate::model::{InstallReason, OverlapCandidate, PrimaryHeuristic, ScanResult};
use crate::providers::SystemCommandRunner;
use crate::scanner;

pub fn run(
    config: &Config,
    refresh: bool,
    config_path: Option<&Path>,
    styles: &Styles,
) -> anyhow::Result<()> {
    let runner = SystemCommandRunner::new(config.scan.provider_timeout_secs);
    let scan = scanner::load_or_scan(&runner, config, refresh, config_path)?;
    let overlaps = analyzer::detect_overlaps(
        &scan,
        &config.overlap.ignore,
        &config.overlap.extra_mappings,
    );
    let min = config.overlap.min_confidence();
    print!("{}", render_overlaps(&overlaps, &scan, min, styles));
    Ok(())
}

fn render_overlaps(
    overlaps: &[OverlapCandidate],
    scan: &ScanResult,
    min: MinConfidence,
    s: &Styles,
) -> String {
    let shown: Vec<&OverlapCandidate> = overlaps
        .iter()
        .filter(|o| min.allows(o.confidence))
        .collect();

    let mut out = String::new();
    let headline = if shown.is_empty() {
        s.summary_ok("no overlaps detected")
    } else {
        let n = shown.len();
        s.summary_updates(&format!(
            "{n} overlap{} detected",
            if n == 1 { "" } else { "s" }
        ))
    };
    out.push_str(&format!(
        "{} {} {}\n",
        s.title("paclens"),
        s.dim(s.bullet()),
        headline
    ));

    for o in &shown {
        out.push('\n');
        out.push_str(&render_one(o, scan, s));
    }

    if !shown.is_empty() {
        out.push_str(&format!(
            "\n{}\n",
            s.dim("advisory only — paclens never removes anything")
        ));
    }
    out
}

fn render_one(o: &OverlapCandidate, scan: &ScanResult, s: &Styles) -> String {
    let mut out = String::new();
    out.push_str(&format!("{}\n", s.title(&o.display_name)));

    if let Some(native) = &o.native_package {
        let reason = match install_reason(scan, &native.name) {
            Some(InstallReason::Explicit) => ", explicit",
            Some(InstallReason::Dependency) => ", dependency",
            _ => "",
        };
        out.push_str(&field(
            s,
            "native",
            &format!(
                "{} {}  ({}{reason})",
                native.name, native.version, native.source_id
            ),
        ));
    }
    if let Some(app) = &o.flatpak_app {
        out.push_str(&field(
            s,
            "flatpak",
            &format!("{} {}  ({})", app.name, app.version, app.source_id),
        ));
    }
    out.push_str(&format!(
        "  {}   {} {}\n",
        s.dim(&format!("{:9}", "match:")),
        o.match_method,
        s.dim(&format!("[{}]", o.confidence)),
    ));

    let primary = match o.tradeoff.likely_primary {
        PrimaryHeuristic::Native => "native (explicit install)".to_string(),
        PrimaryHeuristic::Flatpak => "flatpak (profile in use)".to_string(),
        PrimaryHeuristic::Unknown => format!("unknown {}", s.dim("— check both profiles")),
    };
    out.push_str(&field(s, "primary", &primary));
    out.push_str(&field(
        s,
        "tradeoff",
        "native has full system integration; flatpak has sandboxing (portal-gated)",
    ));
    out
}

fn field(s: &Styles, label: &str, value: &str) -> String {
    format!(
        "  {}   {value}\n",
        s.dim(&format!("{:9}", format!("{label}:")))
    )
}

fn install_reason(scan: &ScanResult, name: &str) -> Option<InstallReason> {
    scan.packages
        .iter()
        .find(|p| p.name == name)
        .map(|p| p.install_reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Confidence;
    use crate::config::ColorTheme;
    use crate::model::{
        CacheSizes, MatchMethod, Package, PackageRef, SCHEMA_VERSION, SourceId, Tradeoff,
    };
    use chrono::Utc;

    fn plain() -> Styles {
        Styles::resolve(false, ColorTheme::Dark, false)
    }

    fn scan() -> ScanResult {
        ScanResult {
            schema_version: SCHEMA_VERSION,
            scanned_at: Utc::now(),
            sources: Vec::new(),
            packages: vec![Package {
                name: "firefox".to_string(),
                version: "128.0-1".to_string(),
                source_id: SourceId::pacman(),
                install_reason: InstallReason::Explicit,
                size_bytes: None,
                description: None,
                depends_on: Vec::new(),
                required_by: Vec::new(),
                optional_deps: Vec::new(),
                provides: Vec::new(),
                runtime: false,
            }],
            updates: Vec::new(),
            cache_sizes: CacheSizes::default(),
            flatpak_profile_sizes: Default::default(),
        }
    }

    fn overlap(confidence: Confidence, method: MatchMethod) -> OverlapCandidate {
        OverlapCandidate {
            display_name: "Firefox".to_string(),
            native_package: Some(PackageRef {
                name: "firefox".to_string(),
                version: "128.0-1".to_string(),
                source_id: SourceId::pacman(),
            }),
            flatpak_app: Some(PackageRef {
                name: "org.mozilla.firefox".to_string(),
                version: "128.0".to_string(),
                source_id: SourceId::flatpak_user(),
            }),
            match_method: method,
            confidence,
            tradeoff: Tradeoff {
                native_version: Some("128.0-1".to_string()),
                flatpak_version: Some("128.0".to_string()),
                likely_primary: PrimaryHeuristic::Native,
                ..Tradeoff::default()
            },
        }
    }

    #[test]
    fn renders_the_spec_shape_with_confidence_inline() {
        let overlaps = vec![overlap(Confidence::Confirmed, MatchMethod::KnownMap)];
        let text = render_overlaps(&overlaps, &scan(), MinConfidence::Inferred, &plain());
        assert!(text.contains("1 overlap detected"), "{text}");
        assert!(text.contains("Firefox"), "{text}");
        assert!(
            text.contains("firefox 128.0-1  (pacman, explicit)"),
            "{text}"
        );
        assert!(
            text.contains("org.mozilla.firefox 128.0  (flatpak-user)"),
            "{text}"
        );
        assert!(text.contains("known map [confirmed]"), "{text}");
        assert!(text.contains("native (explicit install)"), "{text}");
        assert!(text.contains("sandboxing"), "{text}");
        assert!(text.contains("advisory only"), "{text}");
        assert!(!text.contains('\u{1b}'));
    }

    #[test]
    fn empty_report_is_a_green_no() {
        let text = render_overlaps(&[], &scan(), MinConfidence::Inferred, &plain());
        assert!(text.contains("no overlaps detected"), "{text}");
        assert!(!text.contains("advisory"), "{text}");
    }

    #[test]
    fn min_confidence_floor_hides_weaker_matches() {
        let overlaps = vec![
            overlap(Confidence::Confirmed, MatchMethod::KnownMap),
            overlap(Confidence::Unknown, MatchMethod::DisplayNameMatch),
        ];
        let default_floor = render_overlaps(&overlaps, &scan(), MinConfidence::Inferred, &plain());
        assert!(
            default_floor.contains("1 overlap detected"),
            "{default_floor}"
        );
        assert!(!default_floor.contains("display-name"), "{default_floor}");

        let everything = render_overlaps(&overlaps, &scan(), MinConfidence::Unknown, &plain());
        assert!(everything.contains("2 overlaps detected"), "{everything}");
        assert!(
            everything.contains("display-name match [unknown]"),
            "{everything}"
        );

        let strict = render_overlaps(&overlaps, &scan(), MinConfidence::Confirmed, &plain());
        assert!(strict.contains("1 overlap detected"), "{strict}");
    }
}
