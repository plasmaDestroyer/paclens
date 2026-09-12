//! `paclens status` — print a dashboard summary to stdout.
//!
//! Loads from the scan cache when fresh (else re-scans), then prints a headline
//! (total pending updates) and an aligned per-source table: installed/update
//! counts and availability, followed by the cache size and last-scan time. The
//! orphan/overlap rows arrive with their analyzers (v0.0.7/v0.0.8).
//!
//! The per-source counts and the byte/time formatting are shared with the TUI
//! dashboard (`crate::model::summarize`, `crate::format`) so the two never
//! disagree (principle P5). Coloring goes through the shared `Styles`.

use std::path::Path;

use crate::cli::style::Styles;
use crate::config::Config;
use crate::format::{human_bytes, relative_time};
use crate::model::{ScanResult, SourceId, SourceSummary, summarize};
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

    let pacman = summarize(&scan, |id| id == &SourceId::pacman());
    let flatpak = summarize(&scan, is_flatpak);
    tracing::info!(
        pacman_installed = pacman.installed,
        pacman_updates = pacman.updates,
        flatpak_installed = flatpak.installed,
        flatpak_updates = flatpak.updates,
        "scan complete"
    );

    print!("{}", render_status(&scan, styles));
    Ok(())
}

fn is_flatpak(id: &SourceId) -> bool {
    id == &SourceId::flatpak()
}

/// Build the whole status block. Pure (no IO) so the no-color rendering is
/// deterministic and unit-testable.
fn render_status(scan: &ScanResult, s: &Styles) -> String {
    let total = scan.updates.len();
    let summary = if total == 0 {
        s.summary_ok("up to date")
    } else {
        let plural = if total == 1 { "" } else { "s" };
        s.summary_updates(&format!("{total} update{plural} available"))
    };

    let mut out = String::new();
    out.push_str(&format!(
        "{} {} {}\n\n",
        s.title("paclens"),
        s.dim(s.bullet()),
        summary
    ));

    out.push_str(&s.dim(&format!(
        "  {:<8} {:>9}  {:>7}  {}",
        "SOURCE", "INSTALLED", "UPDATES", "STATUS"
    )));
    out.push('\n');

    // One row per source the scan found, in the order the scan lists them.
    // Naming the sources here instead is how `cargo` came to be scanned,
    // cached, and then invisible: a renderer that enumerates what it knows
    // about cannot show a source added later (#11).
    for source in &scan.sources {
        let id = source.id.clone();
        let summary = summarize(scan, |candidate| candidate == &id);
        // Why a source cannot be updated, when that is more specific than
        // "not found" — one answer, shared with the dashboard (P5).
        let reason = scan.unavailable_reason(&source.id);
        // Flag the row itself whenever there is a note, working or not — a
        // stale pin leaves the aur source fine, and the note alone is easy to
        // read past.
        let warned = scan.source_warning(&source.id);
        if reason.is_some() || warned {
            out.push_str(&render_row_because(
                source.id.as_str(),
                &summary,
                reason,
                warned,
                s,
            ));
        } else {
            out.push_str(&render_row(source.id.as_str(), &summary, s));
        }
        out.push('\n');
    }

    out.push('\n');
    // Why a source is degraded, and what fixes it. Shared with the TUI
    // dashboard so the two can never word it differently (P5).
    for source in &scan.sources {
        if let Some(note) = scan.source_note(&source.id) {
            out.push_str(&s.dim(&format!("  {note}")));
            out.push('\n');
        }
    }
    // Same sentence the dashboard prints, from the same analyzer (#3).
    let reboot = crate::analyzer::reboot_status(scan.kernel.as_ref(), &scan.packages);
    if let Some(note) = reboot.note() {
        let line = format!("  reboot {note}");
        out.push_str(&if reboot.is_required() {
            s.summary_updates(&line)
        } else {
            s.dim(&line)
        });
        out.push('\n');
    }
    let mut meta = Vec::new();
    if let Some(bytes) = scan.cache_sizes.pacman_cache_bytes {
        meta.push(format!("cache {}", human_bytes(bytes)));
    }
    meta.push(format!("last scan {}", relative_time(scan.scanned_at)));
    let sep = format!(" {} ", s.bullet());
    out.push_str(&s.dim(&format!("  {}", meta.join(sep.as_str()))));
    out.push('\n');
    out
}

/// One source's table row, right-aligning the numeric columns. The numbers are
/// padded to the column width *before* styling so ANSI codes never break the
/// alignment.
fn render_row(name: &str, summary: &SourceSummary, s: &Styles) -> String {
    render_row_because(name, summary, None, false, s)
}

/// [`render_row`], with an optional reason replacing the generic "not found"
/// and a `warned` flag that marks the row even when the source still works.
fn render_row_because(
    name: &str,
    summary: &SourceSummary,
    reason: Option<&str>,
    warned: bool,
    s: &Styles,
) -> String {
    let installed = format!("{:>9}", summary.installed);
    // A source with no update path checked nothing, so it has no count to
    // show — "0" there would read as "none pending" (design §3).
    let updates = if summary.available {
        s.updates_count(&format!("{:>7}", summary.updates), summary.updates)
    } else {
        s.dim(&format!("{:>7}", "—"))
    };
    let status = match (summary.available, warned, reason) {
        (true, false, _) => s.available(),
        (true, true, _) => s.warned("ok"),
        (false, true, reason) => s.warned(reason.unwrap_or("not found")),
        // An explained limitation — the source lists fine, it just cannot
        // check for updates. Grey "ok", with the `—` in the updates column
        // and the note carrying the rest.
        (false, false, Some(_)) => s.inactive(),
        (false, false, None) => s.unavailable(),
    };
    format!("  {name:<8} {installed}  {updates}  {status}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ColorTheme;
    use crate::model::{
        CacheSizes, InstallReason, Package, PendingUpdate, SCHEMA_VERSION, Source, SourceKind,
    };
    use chrono::Utc;

    /// Piped styler: Unicode glyphs, no ANSI — deterministic and the prettiest
    /// plain form (what `paclens status | cat` produces).
    fn plain_styles() -> Styles {
        Styles::resolve(false, ColorTheme::Dark, false)
    }

    /// Forced-plain styler: ASCII glyphs, no ANSI (`--no-color`).
    fn ascii_styles() -> Styles {
        Styles::resolve(true, ColorTheme::Dark, true)
    }

    fn pkg(name: &str, source: SourceId) -> Package {
        Package {
            scope: None,
            name: name.to_string(),
            version: "1".to_string(),
            source_id: source,
            install_reason: InstallReason::Unknown,
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

    fn upd(name: &str, source: SourceId) -> PendingUpdate {
        PendingUpdate {
            package_name: name.to_string(),
            current_version: "1".to_string(),
            available_version: "2".to_string(),
            source_id: source,
        }
    }

    fn scan_with(
        packages: Vec<Package>,
        updates: Vec<PendingUpdate>,
        flatpak_ok: bool,
    ) -> ScanResult {
        ScanResult {
            schema_version: SCHEMA_VERSION,
            scanned_at: Utc::now(),
            sources: vec![
                Source {
                    id: SourceId::pacman(),
                    kind: SourceKind::Pacman,
                    available: true,
                    last_scanned: None,
                    accurate_updates: true,
                },
                Source {
                    id: SourceId::flatpak(),
                    kind: SourceKind::Flatpak,
                    available: flatpak_ok,
                    last_scanned: None,
                    accurate_updates: true,
                },
            ],
            packages,
            updates,
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

    #[test]
    fn render_row_shows_name_counts_and_availability() {
        let s = plain_styles();
        let summary = SourceSummary {
            available: true,
            installed: 1568,
            updates: 0,
        };
        let row = render_row("pacman", &summary, &s);
        assert!(row.starts_with("  pacman"), "row was: {row:?}");
        assert!(row.contains("1568"));
        assert!(row.ends_with("● ok"));
    }

    #[test]
    fn forced_plain_row_uses_ascii_glyphs() {
        let s = ascii_styles();
        let summary = SourceSummary {
            available: true,
            installed: 1568,
            updates: 0,
        };
        assert!(render_row("pacman", &summary, &s).ends_with("* ok"));
    }

    #[test]
    fn rows_align_the_status_column_regardless_of_number_width() {
        let s = plain_styles();
        let wide = render_row(
            "pacman",
            &SourceSummary {
                available: true,
                installed: 1568,
                updates: 12,
            },
            &s,
        );
        let narrow = render_row(
            "flatpak",
            &SourceSummary {
                available: true,
                installed: 0,
                updates: 0,
            },
            &s,
        );
        // Despite different number widths, the STATUS column starts at the same
        // offset in both rows.
        assert_eq!(
            wide.find("● ok"),
            narrow.find("● ok"),
            "status column misaligned:\n{wide}\n{narrow}"
        );
    }

    #[test]
    fn render_row_unavailable_uses_the_unavailable_glyph() {
        let s = plain_styles();
        let summary = SourceSummary {
            available: false,
            installed: 0,
            updates: 0,
        };
        let row = render_row("flatpak", &summary, &s);
        assert!(row.ends_with("○ not found"), "row was: {row:?}");
    }

    #[test]
    fn headline_says_up_to_date_when_no_updates() {
        let s = plain_styles();
        let scan = scan_with(vec![pkg("a", SourceId::pacman())], Vec::new(), true);
        let text = render_status(&scan, &s);
        assert!(
            text.starts_with("paclens · up to date"),
            "text was:\n{text}"
        );
        assert!(text.contains("SOURCE"));
        assert!(text.contains("INSTALLED"));
    }

    #[test]
    fn headline_counts_updates_with_correct_plural() {
        let s = plain_styles();
        let one = scan_with(
            vec![pkg("a", SourceId::pacman())],
            vec![upd("a", SourceId::pacman())],
            true,
        );
        assert!(render_status(&one, &s).starts_with("paclens · 1 update available"));

        let many = scan_with(
            Vec::new(),
            vec![
                upd("a", SourceId::pacman()),
                upd("b", SourceId::pacman()),
                upd("c", SourceId::pacman()),
            ],
            true,
        );
        assert!(render_status(&many, &s).starts_with("paclens · 3 updates available"));
    }

    #[test]
    fn render_status_has_no_ansi_in_no_color_mode() {
        let s = plain_styles();
        let scan = scan_with(vec![pkg("a", SourceId::pacman())], Vec::new(), true);
        assert!(!render_status(&scan, &s).contains('\u{1b}'));
    }

    /// A degraded aur source says which capability is missing and what
    /// restores it. "not found" alone is the generic note design §3 rules out
    /// — it tells you nothing about what to do next.
    #[test]
    fn a_missing_helper_reads_as_inactive_and_the_note_names_the_fix() {
        use crate::providers::aur::HelperChoice;
        let mut scan = scan_with(Vec::new(), Vec::new(), true);
        scan.sources.push(Source {
            id: SourceId::aur(),
            kind: SourceKind::Aur,
            available: false,
            last_scanned: None,
            accurate_updates: true,
        });
        scan.aur_helper = HelperChoice::None;
        let out = render_status(&scan, &ascii_styles());
        // The row reads as inactive rather than broken — it lists AUR
        // packages perfectly well, it just cannot check them (2026-09-12).
        let row = out
            .lines()
            .find(|l| l.trim_start().starts_with("aur "))
            .expect("aur row");
        assert!(row.contains("ok"), "row: {row:?}");
        assert!(
            !row.contains("not found"),
            "the source is not missing: {row:?}"
        );
        // No count, because nothing checked.
        assert!(row.contains('—'), "a count was claimed: {row:?}");
        assert!(
            out.contains("install paru, yay or pikaur for update detection"),
            "note missing:\n{out}"
        );
    }

    /// Using a different helper than the one configured is exactly the
    /// unexplained behaviour design §2 rules out, so it is said out loud even
    /// though the source is working.
    #[test]
    fn every_source_the_scan_found_gets_a_row() {
        // The bug this pins: the table named its rows, so `cargo` was
        // scanned, cached, and then invisible. A source added later has to
        // appear without this renderer learning its name.
        let mut scan = scan_with(vec![pkg("a", SourceId::pacman())], Vec::new(), true);
        scan.sources.push(Source {
            id: SourceId::cargo(),
            kind: crate::model::SourceKind::Cargo,
            available: false,
            last_scanned: None,
            accurate_updates: false,
        });
        let text = render_status(&scan, &plain_styles());
        assert!(text.contains("cargo"), "no cargo row:\n{text}");
        // The row says why it cannot update, in the width a table cell has…
        // Inactive, not broken: grey "ok" with no count, and no marker.
        let row = text
            .lines()
            .find(|l| l.trim_start().starts_with("cargo "))
            .expect("cargo row");
        assert!(row.contains("ok"), "row: {row:?}");
        assert!(!row.contains("! "), "marked as a problem: {row:?}");
        assert!(
            !row.contains("not found"),
            "the source is not missing: {row:?}"
        );
        assert!(row.contains('—'), "a count was claimed: {row:?}");
        // …and the note below names the tool and how to get it.
        assert!(
            text.contains("install cargo-update"),
            "the note should name the tool:\n{text}"
        );
    }

    #[test]
    fn a_stale_pin_is_reported_even_though_the_source_works() {
        use crate::providers::aur::{AurHelper, HelperChoice};
        let mut scan = scan_with(Vec::new(), Vec::new(), true);
        scan.sources.push(Source {
            id: SourceId::aur(),
            kind: SourceKind::Aur,
            available: true,
            last_scanned: None,
            accurate_updates: true,
        });
        scan.aur_helper = HelperChoice::FellBack {
            configured: "yay".to_string(),
            to: AurHelper::Paru,
        };
        let out = render_status(&scan, &ascii_styles());
        assert!(out.contains("* ok"), "source should still read ok:\n{out}");
        assert!(out.contains("config asks for yay"), "{out}");
        assert!(out.contains("using paru"), "{out}");
    }

    #[test]
    fn a_stale_running_kernel_is_reported_and_a_current_one_is_silent() {
        use crate::analyzer::kernel::RunningKernel;
        let mut scan = scan_with(
            vec![Package {
                name: "linux-cachyos".to_string(),
                version: "7.2.3-1".to_string(),
                ..pkg("linux-cachyos", SourceId::pacman())
            }],
            Vec::new(),
            true,
        );
        scan.kernel = Some(RunningKernel {
            release: "7.2.2-1-cachyos".to_string(),
            modules_present: true,
        });
        let out = render_status(&scan, &ascii_styles());
        assert!(out.contains("reboot required"), "reboot missing:\n{out}");
        assert!(out.contains("7.2.2-1-cachyos"), "{out}");
        assert!(out.contains("7.2.3-1-cachyos"), "{out}");

        // Running what is installed: no row at all.
        scan.packages[0].version = "7.2.2-1".to_string();
        let out = render_status(&scan, &ascii_styles());
        assert!(
            !out.contains("reboot"),
            "furniture on a healthy system:\n{out}"
        );
    }

    /// Nothing to explain, nothing printed — the note must not become a line
    /// of permanent furniture on a healthy system.
    #[test]
    fn a_working_helper_prints_no_note() {
        use crate::providers::aur::{AurHelper, HelperChoice};
        let mut scan = scan_with(Vec::new(), Vec::new(), true);
        scan.sources.push(Source {
            id: SourceId::aur(),
            kind: SourceKind::Aur,
            available: true,
            last_scanned: None,
            accurate_updates: true,
        });
        scan.aur_helper = HelperChoice::Detected(AurHelper::Paru);
        let out = render_status(&scan, &ascii_styles());
        assert!(!out.contains("aur:"), "unexpected note:\n{out}");
        assert!(!out.contains("no helper"), "{out}");
    }

    /// No aur source configured at all → no aur row, and so no aur note
    /// either, whatever the helper state happens to be.
    #[test]
    fn no_aur_source_means_no_note() {
        use crate::providers::aur::HelperChoice;
        let mut scan = scan_with(Vec::new(), Vec::new(), true);
        scan.aur_helper = HelperChoice::None;
        let out = render_status(&scan, &ascii_styles());
        assert!(!out.contains("aur"), "{out}");
    }

    /// A stale pin leaves the source working, so the row would read a clean
    /// "ok" and the note below it is easy to read straight past. The row
    /// carries the warning marker itself.
    #[test]
    fn a_working_but_degraded_aur_row_is_marked() {
        use crate::providers::aur::{AurHelper, HelperChoice};
        let mut scan = scan_with(Vec::new(), Vec::new(), true);
        scan.sources.push(Source {
            id: SourceId::aur(),
            kind: SourceKind::Aur,
            available: true,
            last_scanned: None,
            accurate_updates: true,
        });
        scan.aur_helper = HelperChoice::FellBack {
            configured: "yay".to_string(),
            to: AurHelper::Paru,
        };
        let out = render_status(&scan, &ascii_styles());
        let row = out
            .lines()
            .find(|l| l.trim_start().starts_with("aur"))
            .expect("aur row");
        assert!(row.ends_with("! ok"), "row was: {row:?}");
        // pacman is healthy and must stay unmarked.
        let pacman = out
            .lines()
            .find(|l| l.trim_start().starts_with("pacman"))
            .expect("pacman row");
        assert!(pacman.ends_with("* ok"), "row was: {pacman:?}");
    }

    /// The marker rides on the note, not on availability: every state that
    /// prints a note marks its row, and every state that does not, does not.
    #[test]
    fn the_marker_means_a_surprise_and_the_note_means_an_explanation() {
        use crate::providers::aur::{AurHelper, HelperChoice};
        // These were coupled: anything with a note got the `!` marker. They
        // are different questions (user decision 2026-09-12). The marker is
        // for a source that *works* but is not what the config asked for —
        // nothing else on the row would say so. A source missing its optional
        // tool reads as unavailable already, and marking it says something
        // went wrong when nothing did.
        let cases = [
            // choice, available, marker, note
            (HelperChoice::Detected(AurHelper::Paru), true, false, false),
            (HelperChoice::Pinned(AurHelper::Yay), true, false, false),
            (
                HelperChoice::FellBack {
                    configured: "yay".to_string(),
                    to: AurHelper::Paru,
                },
                true,
                true,
                true,
            ),
            (HelperChoice::None, false, false, true),
            (
                HelperChoice::ConfiguredMissing {
                    configured: "trizen".to_string(),
                },
                false,
                false,
                true,
            ),
        ];
        for (choice, available, expect_mark, expect_note) in cases {
            let mut scan = scan_with(Vec::new(), Vec::new(), true);
            scan.sources.push(Source {
                id: SourceId::aur(),
                kind: SourceKind::Aur,
                available,
                last_scanned: None,
                accurate_updates: true,
            });
            scan.aur_helper = choice.clone();
            let out = render_status(&scan, &ascii_styles());
            let row = out
                .lines()
                .find(|l| l.trim_start().starts_with("aur "))
                .expect("aur row");
            assert_eq!(row.contains("! "), expect_mark, "{choice:?} row: {row:?}");
            assert_eq!(
                out.contains("aur: "),
                expect_note,
                "{choice:?} note presence"
            );
        }
    }
}
