//! `paclens status` — print a dashboard summary to stdout (spec §11.3).
//!
//! Loads from the scan cache when fresh (else re-scans), then prints per-source
//! installed/update counts, the last-scan time, and the pacman cache size. The
//! orphan/overlap rows arrive with their analyzers (v0.0.7/v0.0.8).

use std::path::Path;

use chrono::Utc;

use crate::config::Config;
use crate::model::{ScanResult, SourceId};
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

/// Per-source counts derived from a `ScanResult`. Pure and testable, separate
/// from the printing.
#[derive(Debug, PartialEq, Eq)]
struct SourceSummary {
    available: bool,
    installed: usize,
    updates: usize,
}

fn summarize(scan: &ScanResult, is_member: impl Fn(&SourceId) -> bool) -> SourceSummary {
    SourceSummary {
        available: scan.sources.iter().any(|s| is_member(&s.id) && s.available),
        installed: scan
            .packages
            .iter()
            .filter(|p| is_member(&p.source_id))
            .count(),
        updates: scan
            .updates
            .iter()
            .filter(|u| is_member(&u.source_id))
            .count(),
    }
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

    print_row("pacman", &pacman);
    print_row("flatpak", &flatpak);
    if let Some(bytes) = scan.cache_sizes.pacman_cache_bytes {
        println!("cache    {}", human_bytes(bytes));
    }
    println!("last scan  {}", relative_time(scan.scanned_at));
}

fn print_row(name: &str, summary: &SourceSummary) {
    if summary.available {
        println!(
            "{name:<8} {:>5} installed   {} updates",
            summary.installed, summary.updates
        );
    } else {
        println!("{name:<8} not available");
    }
}

/// Format a byte count as a human-readable size (binary units).
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

/// A coarse "N minutes ago" rendering of a past timestamp.
fn relative_time(when: chrono::DateTime<Utc>) -> String {
    let secs = Utc::now().signed_duration_since(when).num_seconds().max(0);
    match secs {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{} minutes ago", secs / 60),
        3600..=86399 => format!("{} hours ago", secs / 3600),
        _ => format!("{} days ago", secs / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        CacheSizes, InstallReason, Package, PendingUpdate, SCHEMA_VERSION, SourceKind,
    };
    use chrono::Duration;

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

    fn update(name: &str, source: SourceId) -> PendingUpdate {
        PendingUpdate {
            package_name: name.to_string(),
            current_version: "1".to_string(),
            available_version: "2".to_string(),
            source_id: source,
        }
    }

    fn scan_with(packages: Vec<Package>, updates: Vec<PendingUpdate>) -> ScanResult {
        ScanResult {
            schema_version: SCHEMA_VERSION,
            scanned_at: Utc::now(),
            sources: vec![
                crate::model::Source {
                    id: SourceId::pacman(),
                    kind: SourceKind::Pacman,
                    available: true,
                    last_scanned: None,
                },
                crate::model::Source {
                    id: SourceId::flatpak_user(),
                    kind: SourceKind::Flatpak {
                        scope: crate::model::FlatpakScope::User,
                    },
                    available: true,
                    last_scanned: None,
                },
            ],
            packages,
            updates,
            cache_sizes: CacheSizes::default(),
        }
    }

    #[test]
    fn summarize_counts_per_source_family() {
        let scan = scan_with(
            vec![
                pkg("a", SourceId::pacman()),
                pkg("b", SourceId::pacman()),
                pkg("org.x.App", SourceId::flatpak_user()),
            ],
            vec![update("a", SourceId::pacman())],
        );
        let pacman = summarize(&scan, |id| id == &SourceId::pacman());
        assert_eq!(
            pacman,
            SourceSummary {
                available: true,
                installed: 2,
                updates: 1
            }
        );
        let flatpak = summarize(&scan, is_flatpak);
        assert_eq!(
            flatpak,
            SourceSummary {
                available: true,
                installed: 1,
                updates: 0
            }
        );
    }

    #[test]
    fn human_bytes_picks_the_right_unit() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.00 KiB");
        assert_eq!(human_bytes(1536), "1.50 KiB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.00 MiB");
        assert_eq!(human_bytes(1024u64.pow(3)), "1.00 GiB");
    }

    #[test]
    fn relative_time_buckets_by_magnitude() {
        let now = Utc::now();
        assert_eq!(relative_time(now), "just now");
        assert_eq!(relative_time(now - Duration::minutes(5)), "5 minutes ago");
        assert_eq!(relative_time(now - Duration::hours(3)), "3 hours ago");
        assert_eq!(relative_time(now - Duration::days(2)), "2 days ago");
    }
}
