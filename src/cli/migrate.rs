//! `paclens migrate <name>` — the migration advisory report (roadmap v0.4).
//!
//! Read-only: shows where each side of an overlap stores its data and what a
//! manual migration would involve. paclens executes none of it — migration
//! execution is v0.5, behind backups and confirmations.

use std::path::Path;

use crate::analyzer;
use crate::cli::style::Styles;
use crate::config::Config;
use crate::format::human_bytes;
use crate::model::{Direction, MigrationReport, OverlapCandidate, PathKind};
use crate::providers::SystemCommandRunner;
use crate::scanner;

pub fn run(
    config: &Config,
    refresh: bool,
    config_path: Option<&Path>,
    name: &str,
    direction: Option<Direction>,
    styles: &Styles,
) -> anyhow::Result<()> {
    let runner = SystemCommandRunner::new(config.scan.provider_timeout_secs);
    let scan = scanner::load_or_scan(&runner, config, refresh, config_path)?;
    let overlaps = analyzer::detect_overlaps(
        &scan,
        &config.overlap.ignore,
        &config.overlap.extra_mappings,
    );
    let Some(candidate) = find(&overlaps, name) else {
        if overlaps.is_empty() {
            anyhow::bail!(
                "no overlaps detected — `migrate` reports on apps installed both natively and as a Flatpak"
            );
        }
        anyhow::bail!(
            "no overlap matches `{name}` — detected: {}",
            overlaps
                .iter()
                .map(|o| o.display_name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    };
    let report =
        analyzer::migrate::report(&scan, candidate, &config.overlap.extra_mappings, direction);
    print!("{}", render_report(&report, candidate, styles));
    Ok(())
}

/// Match by display name, native package name, or Flatpak app id.
fn find<'a>(overlaps: &'a [OverlapCandidate], name: &str) -> Option<&'a OverlapCandidate> {
    let n = name.to_lowercase();
    overlaps.iter().find(|o| {
        o.display_name.to_lowercase() == n
            || o.native_package
                .as_ref()
                .is_some_and(|p| p.name.to_lowercase() == n)
            || o.flatpak_app
                .as_ref()
                .is_some_and(|p| p.name.to_lowercase() == n)
    })
}

fn render_report(r: &MigrationReport, candidate: &OverlapCandidate, s: &Styles) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{} {} {} {}\n",
        s.title("paclens"),
        s.dim(s.bullet()),
        s.summary_updates(&format!(
            "migrate {} {} {}",
            r.display_name,
            s.arrow(),
            r.direction.target()
        )),
        s.dim(&format!("[{}]", r.confidence)),
    ));

    if r.mappings.is_empty() {
        out.push_str(&format!(
            "\n{}\n",
            s.dim("no profile data found on either side — nothing to migrate")
        ));
        return out;
    }

    out.push('\n');
    for m in &r.mappings {
        let (from, from_bytes, to, to_bytes) = m.endpoints(r.direction);
        let mut from_part = from.to_string();
        if let Some(b) = from_bytes {
            from_part.push_str(&format!("  {}", human_bytes(b)));
        } else {
            from_part.push_str(&format!("  {}", s.dim("(not present)")));
        }
        from_part.push_str(&format!("  {}", s.dim(&format!("[{}]", m.confidence))));
        out.push_str(&field(s, &m.kind.to_string(), &from_part));

        let to_part = if m.kind == PathKind::Cache {
            s.dim("(skip — cache regenerates)")
        } else if let Some(b) = to_bytes {
            format!("{to}  {}", s.warn(&format!("(exists, {})", human_bytes(b))))
        } else {
            to.to_string()
        };
        out.push_str(&field(s, "", &format!("{} {to_part}", s.arrow())));
    }

    out.push_str(&format!("\n{}\n", s.title("manual steps")));
    let mut n = 1;
    let mut step = |text: &str, out: &mut String| {
        out.push_str(&format!("  {}. {text}\n", n));
        n += 1;
    };
    step(&format!("close {} everywhere", r.display_name), &mut out);
    for m in &r.mappings {
        let (from, from_bytes, to, _) = m.endpoints(r.direction);
        if m.kind == PathKind::Cache || from_bytes.is_none() {
            continue;
        }
        step(&format!("cp -a {from} {to}"), &mut out);
    }
    step(
        &format!("launch the {} side and verify your data is there", {
            r.direction.target()
        }),
        &mut out,
    );
    step(&removal_hint(r.direction, candidate), &mut out);

    out.push('\n');
    for w in &r.warnings {
        out.push_str(&format!("  {} {w}\n", s.warn("!")));
    }
    out.push_str(&format!(
        "\n{}\n",
        s.dim("advisory only — paclens copies and removes nothing")
    ));
    out
}

/// The final step: what removing the *source* side would look like — phrased
/// as something to consider, never a command paclens runs.
fn removal_hint(direction: Direction, candidate: &OverlapCandidate) -> String {
    match direction {
        Direction::ToFlatpak => {
            let name = candidate
                .native_package
                .as_ref()
                .map_or("<native package>", |p| p.name.as_str());
            format!(
                "only then consider removing the native package (run `paclens why {name}` first)"
            )
        }
        Direction::ToNative => {
            let id = candidate
                .flatpak_app
                .as_ref()
                .map_or("<app-id>", |p| p.name.as_str());
            format!("only then consider `flatpak uninstall {id}`")
        }
    }
}

fn field(s: &Styles, label: &str, value: &str) -> String {
    format!(
        "  {}   {value}\n",
        s.dim(&format!(
            "{:9}",
            format!("{label}{}", if label.is_empty() { "" } else { ":" })
        ))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ColorTheme;
    use crate::model::{
        Confidence, MatchMethod, PackageRef, PathMapping, PrimaryHeuristic, SourceId, Tradeoff,
    };

    fn plain() -> Styles {
        Styles::resolve(false, ColorTheme::Dark, false)
    }

    fn candidate() -> OverlapCandidate {
        OverlapCandidate {
            display_name: "Firefox".to_string(),
            native_package: Some(PackageRef {
                name: "firefox".to_string(),
                version: "141.0-1".to_string(),
                source_id: SourceId::pacman(),
            }),
            flatpak_app: Some(PackageRef {
                name: "org.mozilla.firefox".to_string(),
                version: "141.0".to_string(),
                source_id: SourceId::flatpak_user(),
            }),
            match_method: MatchMethod::KnownMap,
            confidence: Confidence::Confirmed,
            tradeoff: Tradeoff {
                likely_primary: PrimaryHeuristic::Unknown,
                ..Tradeoff::default()
            },
        }
    }

    fn mapping(
        kind: PathKind,
        native: &str,
        flatpak: &str,
        native_bytes: Option<u64>,
        flatpak_bytes: Option<u64>,
        confidence: Confidence,
    ) -> PathMapping {
        PathMapping {
            kind,
            native: native.to_string(),
            flatpak: flatpak.to_string(),
            native_bytes,
            flatpak_bytes,
            confidence,
        }
    }

    fn report(mappings: Vec<PathMapping>) -> MigrationReport {
        MigrationReport {
            display_name: "Firefox".to_string(),
            direction: Direction::ToFlatpak,
            confidence: Confidence::Confirmed,
            mappings,
            warnings: vec!["close Firefox everywhere before copying anything".to_string()],
        }
    }

    #[test]
    fn report_renders_rows_steps_and_warnings() {
        let r = report(vec![
            mapping(
                PathKind::Profile,
                "~/.mozilla",
                "~/.var/app/org.mozilla.firefox/.mozilla",
                Some(1_200_000_000),
                None,
                Confidence::Confirmed,
            ),
            mapping(
                PathKind::Cache,
                "~/.cache/firefox",
                "~/.var/app/org.mozilla.firefox/cache/firefox",
                Some(340_000_000),
                None,
                Confidence::Inferred,
            ),
        ]);
        let text = render_report(&r, &candidate(), &plain());
        assert!(text.contains("migrate Firefox → flatpak"), "{text}");
        assert!(text.contains("[confirmed]"), "{text}");
        assert!(text.contains("~/.mozilla  1.12 GiB"), "{text}");
        assert!(text.contains("(skip — cache regenerates)"), "{text}");
        // Steps: close, one cp (cache skipped), verify, removal hint.
        assert!(text.contains("1. close Firefox everywhere"), "{text}");
        assert!(
            text.contains("2. cp -a ~/.mozilla ~/.var/app/org.mozilla.firefox/.mozilla"),
            "{text}"
        );
        assert!(!text.contains("cp -a ~/.cache/firefox"), "{text}");
        assert!(text.contains("3. launch the flatpak side"), "{text}");
        assert!(text.contains("paclens why firefox"), "{text}");
        assert!(text.contains("! close Firefox"), "{text}");
        assert!(text.contains("advisory only"), "{text}");
    }

    #[test]
    fn existing_target_data_is_flagged_inline() {
        let r = report(vec![mapping(
            PathKind::Profile,
            "~/.mozilla",
            "~/.var/app/org.mozilla.firefox/.mozilla",
            Some(1_200_000_000),
            Some(300_000_000),
            Confidence::Confirmed,
        )]);
        let text = render_report(&r, &candidate(), &plain());
        assert!(text.contains("(exists, 286.10 MiB)"), "{text}");
    }

    #[test]
    fn to_native_swaps_endpoints_and_removal_hint() {
        let mut r = report(vec![mapping(
            PathKind::Config,
            "~/.config/vlc",
            "~/.var/app/org.videolan.VLC/config/vlc",
            None,
            Some(2_048),
            Confidence::Inferred,
        )]);
        r.direction = Direction::ToNative;
        let text = render_report(&r, &candidate(), &plain());
        assert!(
            text.contains("cp -a ~/.var/app/org.videolan.VLC/config/vlc ~/.config/vlc"),
            "{text}"
        );
        assert!(
            text.contains("flatpak uninstall org.mozilla.firefox"),
            "{text}"
        );
    }

    #[test]
    fn empty_mappings_short_circuit() {
        let text = render_report(&report(Vec::new()), &candidate(), &plain());
        assert!(text.contains("nothing to migrate"), "{text}");
        assert!(!text.contains("manual steps"), "{text}");
    }

    #[test]
    fn find_matches_any_of_the_three_names() {
        let overlaps = vec![candidate()];
        assert!(find(&overlaps, "firefox").is_some());
        assert!(find(&overlaps, "Firefox").is_some());
        assert!(find(&overlaps, "org.mozilla.firefox").is_some());
        assert!(find(&overlaps, "chromium").is_none());
    }
}
