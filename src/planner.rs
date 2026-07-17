//! The *plan* step (P4): turn a `ScanResult` + a per-source selection into an
//! `ActionPlan`. Pure — same inputs, same output; never runs anything (that is
//! the executor's job, v0.0.6). Both `paclens update --dry-run` and the TUI
//! update screen build their plan here, so they can never disagree (P5).

use std::path::Path;

use chrono::Utc;

use crate::model::{
    ActionKind, ActionPlan, ActionStep, Direction, FlatpakScope, MigrationReport, OverlapCandidate,
    PathKind, PathMapping, ScanResult, SourceId, SourceKind,
};
use crate::providers::{aur, flatpak, pacman};

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
            // paru is never run under sudo — it self-elevates for the
            // install step after building as the user.
            SourceKind::Aur => (aur::update_command(), false),
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

/// The pairs a migration would actually copy: from-side exists, not a cache
/// (regenerates). Shared by the plan builder and [`rollback_lines`] so the
/// two can never disagree about indices.
fn actionable(report: &MigrationReport) -> Vec<(usize, &PathMapping)> {
    report
        .mappings
        .iter()
        .filter(|m| m.kind != PathKind::Cache)
        .filter(|m| m.endpoints(report.direction).1.is_some())
        .enumerate()
        .collect()
}

/// `~/…` → absolute, for the exact argv (display keeps the `~` form).
fn expand(home: &Path, path: &str) -> String {
    home.join(path.trim_start_matches("~/"))
        .display()
        .to_string()
}

/// Backup file name inside the backup dir: index-prefixed leaf, so two
/// targets with the same last component cannot collide.
fn backup_leaf(index: usize, to: &str) -> String {
    let leaf = to.rsplit('/').next().unwrap_or(to);
    format!("{index}-{leaf}")
}

/// Build the migration copy plan (roadmap v0.5): back up every target dir
/// that already exists into `backup_dir`, then `cp -aT` each actionable pair.
/// All commands run as the user — profile data under `~` is user-owned even
/// for system-scope apps — and the plan never contains an `rm`: source data
/// is never deleted (roadmap rule), rollback stays possible.
pub fn plan_migration(
    report: &MigrationReport,
    candidate: &OverlapCandidate,
    home: &Path,
    backup_dir: &Path,
) -> ActionPlan {
    let source_id = target_source_id(report.direction, candidate);
    let step = |targets: Vec<String>, command: Vec<String>| ActionStep {
        source_id: source_id.clone(),
        kind: ActionKind::Migrate,
        targets,
        command,
    };
    let argv = |parts: &[&str]| parts.iter().map(|p| p.to_string()).collect::<Vec<_>>();

    let pairs = actionable(report);
    let mut steps = Vec::new();

    let backups: Vec<&(usize, &PathMapping)> = pairs
        .iter()
        .filter(|(_, m)| m.endpoints(report.direction).3.is_some())
        .collect();
    if !backups.is_empty() {
        let dir = backup_dir.display().to_string();
        steps.push(step(vec![dir.clone()], argv(&["mkdir", "-p", &dir])));
        for (i, m) in &backups {
            let (_, _, to, _) = m.endpoints(report.direction);
            let dest = backup_dir.join(backup_leaf(*i, to)).display().to_string();
            steps.push(step(
                vec![to.to_string()],
                argv(&["cp", "-aT", &expand(home, to), &dest]),
            ));
        }
    }

    for (_, m) in &pairs {
        let (from, _, to, _) = m.endpoints(report.direction);
        let to_abs = expand(home, to);
        if let Some(parent) = Path::new(&to_abs).parent() {
            let parent = parent.display().to_string();
            steps.push(step(vec![to.to_string()], argv(&["mkdir", "-p", &parent])));
        }
        steps.push(step(
            vec![to.to_string()],
            argv(&["cp", "-aT", &expand(home, from), &to_abs]),
        ));
    }

    ActionPlan {
        created_at: Utc::now(),
        steps,
        requires_sudo: false,
    }
}

/// The rollback instructions for a migration plan (roadmap v0.5): shown after
/// the run and never executed by paclens. Targets that were backed up restore
/// from the backup; targets the run created fresh just get removed.
pub fn rollback_lines(report: &MigrationReport, backup_dir: &Path) -> Vec<String> {
    let backup = backup_dir.display();
    actionable(report)
        .iter()
        .map(|(i, m)| {
            let (_, _, to, to_bytes) = m.endpoints(report.direction);
            if to_bytes.is_some() {
                format!(
                    "rm -rf {to} && cp -aT {backup}/{} {to}",
                    backup_leaf(*i, to)
                )
            } else {
                format!("rm -rf {to}")
            }
        })
        .collect()
}

/// The source side's removal plan (roadmap v0.5) — built only after the user
/// verified the target works, and always behind its own confirmation. `None`
/// when the candidate is missing that side.
pub fn plan_removal(report: &MigrationReport, candidate: &OverlapCandidate) -> Option<ActionPlan> {
    let (source_id, targets, command) = match report.direction {
        // Migrating to flatpak → the native package goes. pacman does the
        // removing even for AUR packages, so the step is pacman's (and sudo's).
        Direction::ToFlatpak => {
            let p = candidate.native_package.as_ref()?;
            (
                SourceId::pacman(),
                vec![p.name.clone()],
                vec!["pacman".to_string(), "-Rns".to_string(), p.name.clone()],
            )
        }
        // Migrating to native → the flatpak goes; scope flag follows its
        // source. flatpak prompts for confirmation itself (no -y).
        Direction::ToNative => {
            let app = candidate.flatpak_app.as_ref()?;
            let scope = if app.source_id == SourceId::flatpak_system() {
                "--system"
            } else {
                "--user"
            };
            (
                app.source_id.clone(),
                vec![app.name.clone()],
                vec![
                    "flatpak".to_string(),
                    "uninstall".to_string(),
                    scope.to_string(),
                    app.name.clone(),
                ],
            )
        }
    };
    let requires_sudo = source_id != SourceId::flatpak_user();
    Some(ActionPlan {
        created_at: Utc::now(),
        steps: vec![ActionStep {
            source_id,
            kind: ActionKind::Remove,
            targets,
            command,
        }],
        requires_sudo,
    })
}

/// Where a migration's backups land:
/// `~/.local/share/paclens/backups/<native-name>/<timestamp>/`.
pub fn migration_backup_dir(home: &Path, candidate: &OverlapCandidate) -> std::path::PathBuf {
    let name = candidate
        .native_package
        .as_ref()
        .map(|p| p.name.as_str())
        .unwrap_or("app");
    home.join(".local/share/paclens/backups")
        .join(name)
        .join(chrono::Local::now().format("%Y%m%d-%H%M%S").to_string())
}

/// Whose files the copy steps touch, for honest labeling in logs/console.
fn target_source_id(direction: Direction, candidate: &OverlapCandidate) -> SourceId {
    let side = match direction {
        Direction::ToFlatpak => candidate.flatpak_app.as_ref(),
        Direction::ToNative => candidate.native_package.as_ref(),
    };
    side.map(|p| p.source_id.clone())
        .unwrap_or_else(SourceId::pacman)
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
            accurate_updates: true,
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
            flatpak_profile_sizes: Default::default(),
            profile_dir_sizes: Default::default(),
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
            flatpak_profile_sizes: Default::default(),
            profile_dir_sizes: Default::default(),
        };
        let plan = plan_updates(&empty, enable_all);
        assert!(plan.is_empty());
        assert!(!plan.requires_sudo);
        assert_eq!(plan.total_targets(), 0);
    }

    // --- migration plans (v0.5) ---
    use crate::model::{Confidence, MatchMethod, PackageRef, Tradeoff};

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
            tradeoff: Tradeoff::default(),
        }
    }

    fn mapping(
        kind: PathKind,
        native: &str,
        flatpak: &str,
        native_bytes: Option<u64>,
        flatpak_bytes: Option<u64>,
    ) -> PathMapping {
        PathMapping {
            kind,
            native: native.to_string(),
            flatpak: flatpak.to_string(),
            native_bytes,
            flatpak_bytes,
            confidence: Confidence::Confirmed,
        }
    }

    fn report(direction: Direction, mappings: Vec<PathMapping>) -> MigrationReport {
        MigrationReport {
            display_name: "Firefox".to_string(),
            direction,
            confidence: Confidence::Confirmed,
            mappings,
            warnings: Vec::new(),
        }
    }

    fn commands(plan: &ActionPlan) -> Vec<String> {
        plan.steps.iter().map(|s| s.command.join(" ")).collect()
    }

    #[test]
    fn migration_plan_backs_up_then_copies_with_absolute_paths() {
        let r = report(
            Direction::ToFlatpak,
            vec![
                // Target exists → must be backed up before the copy.
                mapping(
                    PathKind::Profile,
                    "~/.mozilla",
                    "~/.var/app/org.mozilla.firefox/.mozilla",
                    Some(1_000),
                    Some(500),
                ),
                // Cache: never copied, never backed up.
                mapping(
                    PathKind::Cache,
                    "~/.cache/firefox",
                    "~/.var/app/org.mozilla.firefox/cache/firefox",
                    Some(200),
                    None,
                ),
            ],
        );
        let plan = plan_migration(
            &r,
            &candidate(),
            Path::new("/home/t"),
            Path::new("/home/t/.local/share/paclens/backups/firefox/20260714-120000"),
        );
        assert!(!plan.requires_sudo);
        assert!(plan.steps.iter().all(|s| s.kind == ActionKind::Migrate));
        assert!(
            plan.steps
                .iter()
                .all(|s| s.source_id == SourceId::flatpak_user()),
            "copy steps belong to the target side"
        );
        let cmds = commands(&plan);
        assert_eq!(
            cmds,
            vec![
                "mkdir -p /home/t/.local/share/paclens/backups/firefox/20260714-120000",
                "cp -aT /home/t/.var/app/org.mozilla.firefox/.mozilla \
                 /home/t/.local/share/paclens/backups/firefox/20260714-120000/0-.mozilla",
                "mkdir -p /home/t/.var/app/org.mozilla.firefox",
                "cp -aT /home/t/.mozilla /home/t/.var/app/org.mozilla.firefox/.mozilla",
            ],
            "{cmds:?}"
        );
        // No rm anywhere, ever.
        assert!(cmds.iter().all(|c| !c.contains("rm ")), "{cmds:?}");
    }

    #[test]
    fn migration_plan_skips_backup_when_no_target_exists() {
        let r = report(
            Direction::ToFlatpak,
            vec![mapping(
                PathKind::Config,
                "~/.config/vlc",
                "~/.var/app/org.videolan.VLC/config/vlc",
                Some(1_000),
                None,
            )],
        );
        let plan = plan_migration(&r, &candidate(), Path::new("/home/t"), Path::new("/b"));
        let cmds = commands(&plan);
        assert_eq!(
            cmds,
            vec![
                "mkdir -p /home/t/.var/app/org.videolan.VLC/config",
                "cp -aT /home/t/.config/vlc /home/t/.var/app/org.videolan.VLC/config/vlc",
            ],
            "{cmds:?}"
        );
    }

    #[test]
    fn migration_plan_to_native_swaps_endpoints() {
        let r = report(
            Direction::ToNative,
            vec![mapping(
                PathKind::Config,
                "~/.config/vlc",
                "~/.var/app/org.videolan.VLC/config/vlc",
                None,
                Some(2_000),
            )],
        );
        let plan = plan_migration(&r, &candidate(), Path::new("/home/t"), Path::new("/b"));
        assert!(
            plan.steps.iter().all(|s| s.source_id == SourceId::pacman()),
            "target side is native now"
        );
        assert!(commands(&plan).contains(
            &"cp -aT /home/t/.var/app/org.videolan.VLC/config/vlc /home/t/.config/vlc".to_string()
        ));
    }

    #[test]
    fn migration_plan_is_empty_when_nothing_is_actionable() {
        // Only a cache pair, and a pair whose from-side is missing.
        let r = report(
            Direction::ToFlatpak,
            vec![
                mapping(
                    PathKind::Cache,
                    "~/.cache/x",
                    "~/.var/app/id/cache/x",
                    Some(1),
                    None,
                ),
                mapping(
                    PathKind::Config,
                    "~/.config/x",
                    "~/.var/app/id/config/x",
                    None,
                    Some(1),
                ),
            ],
        );
        let plan = plan_migration(&r, &candidate(), Path::new("/h"), Path::new("/b"));
        assert!(plan.is_empty());
    }

    #[test]
    fn rollback_restores_backed_up_targets_and_removes_fresh_ones() {
        let r = report(
            Direction::ToFlatpak,
            vec![
                mapping(
                    PathKind::Profile,
                    "~/.mozilla",
                    "~/.var/app/org.mozilla.firefox/.mozilla",
                    Some(1_000),
                    Some(500),
                ),
                mapping(
                    PathKind::Config,
                    "~/.config/firefox",
                    "~/.var/app/org.mozilla.firefox/config/firefox",
                    Some(10),
                    None,
                ),
            ],
        );
        let lines = rollback_lines(&r, Path::new("/backups/ff/1"));
        assert_eq!(
            lines,
            vec![
                "rm -rf ~/.var/app/org.mozilla.firefox/.mozilla && \
                 cp -aT /backups/ff/1/0-.mozilla ~/.var/app/org.mozilla.firefox/.mozilla",
                "rm -rf ~/.var/app/org.mozilla.firefox/config/firefox",
            ],
            "{lines:?}"
        );
    }

    #[test]
    fn removal_plan_to_flatpak_removes_native_via_sudo_pacman() {
        let r = report(Direction::ToFlatpak, Vec::new());
        let plan = plan_removal(&r, &candidate()).expect("plan");
        assert!(plan.requires_sudo);
        let step = &plan.steps[0];
        assert_eq!(step.kind, ActionKind::Remove);
        assert_eq!(step.source_id, SourceId::pacman());
        assert_eq!(step.command, vec!["pacman", "-Rns", "firefox"]);
        // Kind Remove keeps the source-based privilege rule.
        assert!(crate::executor::needs_privilege(step));
    }

    #[test]
    fn removal_plan_to_native_uninstalls_the_flatpak_unprivileged() {
        let r = report(Direction::ToNative, Vec::new());
        let plan = plan_removal(&r, &candidate()).expect("plan");
        assert!(!plan.requires_sudo);
        let step = &plan.steps[0];
        assert_eq!(step.source_id, SourceId::flatpak_user());
        assert_eq!(
            step.command,
            vec!["flatpak", "uninstall", "--user", "org.mozilla.firefox"]
        );
        assert!(!crate::executor::needs_privilege(step));
    }

    #[test]
    fn removal_plan_system_flatpak_is_privileged() {
        let mut c = candidate();
        c.flatpak_app.as_mut().unwrap().source_id = SourceId::flatpak_system();
        let r = report(Direction::ToNative, Vec::new());
        let plan = plan_removal(&r, &c).expect("plan");
        assert!(plan.requires_sudo);
        assert_eq!(plan.steps[0].command[2], "--system");
    }

    #[test]
    fn removal_plan_missing_side_is_none() {
        let mut c = candidate();
        c.native_package = None;
        let r = report(Direction::ToFlatpak, Vec::new());
        assert!(plan_removal(&r, &c).is_none());
    }

    #[test]
    fn migrate_steps_never_ask_for_privilege() {
        // Even with a pacman source id, a Migrate step stays unprivileged.
        let step = ActionStep {
            source_id: SourceId::pacman(),
            kind: ActionKind::Migrate,
            targets: vec!["~/.config/x".to_string()],
            command: vec!["cp".to_string()],
        };
        assert!(!crate::executor::needs_privilege(&step));
        assert_eq!(crate::executor::skip_reason(&step, None), None);
    }
}
