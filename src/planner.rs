//! The *plan* step (P4): turn a `ScanResult` + a per-source selection into an
//! `ActionPlan`. Pure — same inputs, same output; never runs anything (that is
//! the executor's job, v0.0.6). Both `paclens update --dry-run` and the TUI
//! update screen build their plan here, so they can never disagree (P5).

use chrono::Utc;

use crate::model::{
    ActionKind, ActionPlan, ActionStep, FlatpakScope, ScanResult, SourceId, SourceKind,
};
use crate::providers::{flatpak, pacman};

/// Build the update plan from a scan, including only **available** sources that
/// have at least one pending update and pass `is_enabled` (the per-source
/// toggle / `--source` filter). Predicate-based, mirroring `model::summarize`.
pub fn plan_updates(scan: &ScanResult, is_enabled: impl Fn(&SourceId) -> bool) -> ActionPlan {
    let mut steps = Vec::new();
    let mut requires_sudo = false;

    for source in &scan.sources {
        if !source.available || !is_enabled(&source.id) {
            continue;
        }
        let targets: Vec<String> = scan
            .updates
            .iter()
            .filter(|u| u.source_id == source.id)
            .map(|u| u.package_name.clone())
            .collect();
        if targets.is_empty() {
            continue;
        }
        let (command, needs_sudo) = match &source.kind {
            SourceKind::Pacman => (pacman::update_command(), true),
            SourceKind::Flatpak { scope } => (
                flatpak::update_command(*scope),
                *scope == FlatpakScope::System,
            ),
        };
        requires_sudo |= needs_sudo;
        steps.push(ActionStep {
            source_id: source.id.clone(),
            kind: ActionKind::Update,
            targets,
            command,
        });
    }

    ActionPlan {
        created_at: Utc::now(),
        steps,
        requires_sudo,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CacheSizes, PendingUpdate, SCHEMA_VERSION, Source, SourceKind};

    fn upd(name: &str, source: SourceId) -> PendingUpdate {
        PendingUpdate {
            package_name: name.to_string(),
            current_version: "1".to_string(),
            available_version: "2".to_string(),
            source_id: source,
        }
    }

    fn source(id: SourceId, kind: SourceKind, available: bool) -> Source {
        Source {
            id,
            kind,
            available,
            last_scanned: None,
        }
    }

    /// pacman (2 updates), flatpak-user (1), flatpak-system (0, available).
    fn scan() -> ScanResult {
        ScanResult {
            schema_version: SCHEMA_VERSION,
            scanned_at: Utc::now(),
            sources: vec![
                source(SourceId::pacman(), SourceKind::Pacman, true),
                source(
                    SourceId::flatpak_user(),
                    SourceKind::Flatpak {
                        scope: FlatpakScope::User,
                    },
                    true,
                ),
                source(
                    SourceId::flatpak_system(),
                    SourceKind::Flatpak {
                        scope: FlatpakScope::System,
                    },
                    true,
                ),
            ],
            packages: Vec::new(),
            updates: vec![
                upd("linux", SourceId::pacman()),
                upd("firefox", SourceId::pacman()),
                upd("org.gimp.GIMP", SourceId::flatpak_user()),
            ],
            cache_sizes: CacheSizes::default(),
        }
    }

    fn enable_all(_: &SourceId) -> bool {
        true
    }

    #[test]
    fn one_step_per_enabled_source_with_updates() {
        let plan = plan_updates(&scan(), enable_all);
        // pacman + flatpak-user have updates; flatpak-system has none → 2 steps.
        assert_eq!(plan.source_count(), 2);
        assert_eq!(plan.total_targets(), 3);
        assert_eq!(plan.steps[0].source_id, SourceId::pacman());
        assert_eq!(plan.steps[0].targets, vec!["linux", "firefox"]);
        assert_eq!(plan.steps[0].command, vec!["pacman", "-Syu"]);
        assert_eq!(plan.steps[1].source_id, SourceId::flatpak_user());
        assert_eq!(plan.steps[1].targets, vec!["org.gimp.GIMP"]);
        assert_eq!(
            plan.steps[1].command,
            vec!["flatpak", "update", "--user", "--noninteractive"]
        );
        assert_eq!(plan.steps[0].kind, ActionKind::Update);
    }

    #[test]
    fn pacman_in_the_plan_requires_sudo() {
        assert!(plan_updates(&scan(), enable_all).requires_sudo);
    }

    #[test]
    fn flatpak_user_only_does_not_require_sudo() {
        let plan = plan_updates(&scan(), |id| id == &SourceId::flatpak_user());
        assert_eq!(plan.source_count(), 1);
        assert!(!plan.requires_sudo);
    }

    #[test]
    fn flatpak_system_with_updates_requires_sudo() {
        let mut s = scan();
        s.updates
            .push(upd("org.sys.App", SourceId::flatpak_system()));
        let plan = plan_updates(&s, |id| id == &SourceId::flatpak_system());
        assert_eq!(plan.source_count(), 1);
        assert!(plan.requires_sudo);
        assert_eq!(
            plan.steps[0].command,
            vec!["flatpak", "update", "--system", "--noninteractive"]
        );
    }

    #[test]
    fn predicate_excludes_a_source() {
        let plan = plan_updates(&scan(), |id| id != &SourceId::pacman());
        assert_eq!(plan.source_count(), 1);
        assert_eq!(plan.steps[0].source_id, SourceId::flatpak_user());
        assert!(!plan.requires_sudo);
    }

    #[test]
    fn unavailable_source_is_skipped_even_with_updates() {
        let mut s = scan();
        s.sources[0].available = false; // pacman unavailable
        let plan = plan_updates(&s, enable_all);
        assert_eq!(plan.source_count(), 1);
        assert_eq!(plan.steps[0].source_id, SourceId::flatpak_user());
    }

    #[test]
    fn empty_scan_yields_an_empty_plan() {
        let empty = ScanResult {
            schema_version: SCHEMA_VERSION,
            scanned_at: Utc::now(),
            sources: Vec::new(),
            packages: Vec::new(),
            updates: Vec::new(),
            cache_sizes: CacheSizes::default(),
        };
        let plan = plan_updates(&empty, enable_all);
        assert!(plan.is_empty());
        assert!(!plan.requires_sudo);
        assert_eq!(plan.total_targets(), 0);
    }
}
