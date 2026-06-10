//! `paclens status` — print a dashboard summary to stdout (spec §11.3).
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
    let runner = SystemCommandRunner;
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
    out.push_str(&render_row("flatpak", &flatpak, s));
    out.push('\n');

    out.push('\n');
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
    let installed = format!("{:>9}", summary.installed);
    let updates = s.updates_count(&format!("{:>7}", summary.updates), summary.updates);
    let status = if summary.available {
        s.available()
    } else {
        s.unavailable()
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
                },
                Source {
                    id: SourceId::flatpak_user(),
                    kind: SourceKind::Flatpak {
                        scope: FlatpakScope::User,
                    },
                    available: flatpak_ok,
                    last_scanned: None,
                },
            ],
            packages,
            updates,
            cache_sizes: CacheSizes::default(),
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
        assert!(row.ends_with("● available"));
    }

    #[test]
    fn forced_plain_row_uses_ascii_glyphs() {
        let s = ascii_styles();
        let summary = SourceSummary {
            available: true,
            installed: 1568,
            updates: 0,
        };
        assert!(render_row("pacman", &summary, &s).ends_with("* available"));
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
            wide.find("● available"),
            narrow.find("● available"),
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
        assert!(row.ends_with("○ not available"), "row was: {row:?}");
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
}
