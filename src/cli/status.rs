//! `paclens status` — print a dashboard summary to stdout (spec §11.3).
//!
//! Loads from the scan cache when fresh (else re-scans), then prints per-source
//! installed/update counts, the last-scan time, and the pacman cache size. The
//! orphan/overlap rows arrive with their analyzers (v0.0.7/v0.0.8).
//!
//! The per-source counts and the byte/time formatting are shared with the TUI
//! dashboard (`crate::model::summarize`, `crate::format`) so the two never
//! disagree (principle P5).

use std::path::Path;

use crate::config::Config;
use crate::format::{human_bytes, relative_time};
use crate::model::{ScanResult, SourceId, SourceSummary, summarize};
use crate::providers::SystemCommandRunner;
use crate::scanner;

pub fn run(config: &Config, refresh: bool, config_path: Option<&Path>) -> anyhow::Result<()> {
    let runner = SystemCommandRunner;
    let scan = scanner::load_or_scan(&runner, config, refresh, config_path)?;
    print_status(&scan);
    Ok(())
}

fn is_flatpak(id: &SourceId) -> bool {
    id.as_str().starts_with("flatpak")
}

fn print_status(scan: &ScanResult) {
    let pacman = summarize(scan, |id| id == &SourceId::pacman());
    let flatpak = summarize(scan, is_flatpak);

    tracing::info!(
        pacman_installed = pacman.installed,
        pacman_updates = pacman.updates,
        flatpak_installed = flatpak.installed,
        flatpak_updates = flatpak.updates,
        "scan complete"
    );

    println!("{}", format_row("pacman", &pacman));
    println!("{}", format_row("flatpak", &flatpak));
    if let Some(bytes) = scan.cache_sizes.pacman_cache_bytes {
        println!("cache    {}", human_bytes(bytes));
    }
    println!("last scan  {}", relative_time(scan.scanned_at));
}

/// Render one source's status line. Pure, so it is unit-testable without IO.
fn format_row(name: &str, summary: &SourceSummary) -> String {
    if summary.available {
        format!(
            "{name:<8} {:>5} installed   {} updates",
            summary.installed, summary.updates
        )
    } else {
        format!("{name:<8} not available")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_row_available_shows_counts() {
        let summary = SourceSummary {
            available: true,
            installed: 1481,
            updates: 2,
        };
        let line = format_row("pacman", &summary);
        assert!(line.starts_with("pacman"));
        assert!(line.contains("1481 installed"));
        assert!(line.contains("2 updates"));
    }

    #[test]
    fn format_row_unavailable_says_so() {
        let summary = SourceSummary {
            available: false,
            installed: 0,
            updates: 0,
        };
        assert_eq!(format_row("flatpak", &summary), "flatpak  not available");
    }
}
