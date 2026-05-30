//! `paclens status` — print a dashboard summary to stdout (spec §11.3).
//!
//! v0.0.2 scans live and prints installed + update counts per source. The
//! cache-backed path and the orphan/overlap/cache rows arrive in v0.0.3+.

use crate::config::Config;
use crate::model::{ScanResult, SourceId};
use crate::providers::SystemCommandRunner;
use crate::scanner;

pub fn run(config: &Config) -> anyhow::Result<()> {
    let runner = SystemCommandRunner;
    let scan = scanner::scan(&runner, config);
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
}

fn print_row(name: &str, available: bool, installed: usize, updates: usize) {
    if available {
        println!("{name:<8} {installed:>5} installed   {updates} updates");
    } else {
        println!("{name:<8} not available");
    }
}
