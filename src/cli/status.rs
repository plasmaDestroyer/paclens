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

fn print_status(scan: &ScanResult) {
    let pacman_installed = scan
        .packages
        .iter()
        .filter(|p| p.source_id == SourceId::pacman())
        .count();
    let pacman_updates = scan
        .updates
        .iter()
        .filter(|u| u.source_id == SourceId::pacman())
        .count();
    let flatpak_installed = scan
        .packages
        .iter()
        .filter(|p| is_flatpak(&p.source_id))
        .count();
    let flatpak_updates = scan
        .updates
        .iter()
        .filter(|u| is_flatpak(&u.source_id))
        .count();

    let pacman_available = scan
        .sources
        .iter()
        .any(|s| s.id == SourceId::pacman() && s.available);
    let flatpak_available = scan
        .sources
        .iter()
        .any(|s| is_flatpak(&s.id) && s.available);

    tracing::info!(
        pacman_installed,
        pacman_updates,
        flatpak_installed,
        flatpak_updates,
        "scan complete"
    );

    print_row("pacman", pacman_available, pacman_installed, pacman_updates);
    print_row(
        "flatpak",
        flatpak_available,
        flatpak_installed,
        flatpak_updates,
    );
    if let Some(bytes) = scan.cache_sizes.pacman_cache_bytes {
        println!("cache    {}", human_bytes(bytes));
    }
    println!("last scan  {}", relative_time(scan.scanned_at));
}

fn print_row(name: &str, available: bool, installed: usize, updates: usize) {
    if available {
        println!("{name:<8} {installed:>5} installed   {updates} updates");
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
