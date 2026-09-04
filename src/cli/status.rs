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
    id.as_str().starts_with("flatpak")
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

    let pacman = summarize(scan, |id| id == &SourceId::pacman());
    let flatpak = summarize(scan, is_flatpak);
    out.push_str(&render_row("pacman", &pacman, s));
    out.push('\n');
    // The aur row only exists when the source is configured (v0.3).
    let aur_shown = scan.sources.iter().any(|src| src.id == SourceId::aur());
    if aur_shown {
        let aur = summarize(scan, |id| id == &SourceId::aur());
        // "no helper" rather than "not found" — but only when the helper is
        // actually the reason. A missing pacman makes the aur source
        // unavailable too, and that is a different sentence.
        let reason = (scan.aur_helper.helper().is_none()).then_some("no helper");
        // Flag the row itself whenever there is a note, working or not — a
        // stale pin leaves the source fine, and the note alone is easy to read
        // past.
        let warned = scan.aur_helper.note().is_some();
        out.push_str(&render_row_because("aur", &aur, reason, warned, s));
        out.push('\n');
    }
    out.push_str(&render_row("flatpak", &flatpak, s));
    out.push('\n');

    out.push('\n');
    // Why the aur source is degraded, and what fixes it. Shared with the TUI
    // dashboard so the two can never word it differently (P5).
    if aur_shown && let Some(note) = scan.aur_helper.note() {
        out.push_str(&s.dim(&format!("  {note}")));
        out.push('\n');
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
    let updates = s.updates_count(&format!("{:>7}", summary.updates), summary.updates);
    let status = match (summary.available, warned, reason) {
        (true, false, _) => s.available(),
        (true, true, _) => s.warned("ok"),
        (false, true, reason) => s.warned(reason.unwrap_or("not found")),
        (false, false, Some(reason)) => s.unavailable_because(reason),
        (false, false, None) => s.unavailable(),
    };
    format!("  {name:<8} {installed}  {updates}  {status}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ColorTheme;
    use crate::model::{
        CacheSizes, FlatpakScope, InstallReason, Package, PendingUpdate, SCHEMA_VERSION, Source,
        SourceKind,
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
                    id: SourceId::flatpak_user(),
                    kind: SourceKind::Flatpak {
                        scope: FlatpakScope::User,
                    },
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
    fn a_missing_helper_names_itself_and_the_fix() {
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
        assert!(out.contains("no helper"), "status column:\n{out}");
        assert!(
            !out.contains("aur") || !out.contains("- not found"),
            "{out}"
        );
        assert!(
            out.contains("install paru, yay or pikaur for update detection"),
            "note missing:\n{out}"
        );
    }

    /// Using a different helper than the one configured is exactly the
    /// unexplained behaviour design §2 rules out, so it is said out loud even
    /// though the source is working.
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
    fn the_row_marker_and_the_note_agree() {
        use crate::providers::aur::{AurHelper, HelperChoice};
        let cases = [
            (HelperChoice::Detected(AurHelper::Paru), true, false),
            (HelperChoice::Pinned(AurHelper::Yay), true, false),
            (
                HelperChoice::FellBack {
                    configured: "yay".to_string(),
                    to: AurHelper::Paru,
                },
                true,
                true,
            ),
            (HelperChoice::None, false, true),
            (
                HelperChoice::ConfiguredMissing {
                    configured: "trizen".to_string(),
                },
                false,
                true,
            ),
        ];
        for (choice, available, expect_mark) in cases {
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
                expect_mark,
                "{choice:?} note presence should match the marker"
            );
        }
    }
}
