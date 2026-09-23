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
use crate::providers::{aur, cargo, flatpak, pacman};

/// Build the update plan from a scan, including only **available** sources that
/// have at least one pending update and pass `is_enabled` (the per-source
/// toggle / `--source` filter). Predicate-based, mirroring `model::summarize`.
pub fn plan_updates(scan: &ScanResult, is_enabled: impl Fn(&SourceId) -> bool) -> ActionPlan {
    plan_for(scan, is_enabled, Coverage::Pending)
}

/// Build a plan that runs every available, enabled source's update command
/// **without having checked what is pending** (user decision 2026-09-17).
///
/// `paclens update` used to scan first, which meant waiting on
/// `checkupdates`, the AUR helper and `flatpak remote-ls` — every one of them
/// a network round trip — to produce a list that pacman then recomputes for
/// itself a second later. Someone who has typed `update` has already decided;
/// the check only delays the thing they asked for.
///
/// P1 is untouched: the exact commands still print before anything runs, and
/// the confirmation still gates them. What is gone is the package list, which
/// was never what P1 asked for — "not a summary of it, the commands".
pub fn plan_full_upgrade(scan: &ScanResult, is_enabled: impl Fn(&SourceId) -> bool) -> ActionPlan {
    plan_for(scan, is_enabled, Coverage::Everything)
}

/// Whether a plan covers what is known to be pending, or simply everything.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Coverage {
    Pending,
    Everything,
}

/// One step as the planner decided it, before it becomes an `ActionStep`.
///
/// A named struct rather than a tuple because `privileged` and `interactive`
/// are both bools: transposed, they would run an AUR build as root and hand
/// pacman's conflict prompt to a thread nobody is watching.
struct Built {
    command: Vec<String>,
    privileged: bool,
    interactive: bool,
    targets: Vec<String>,
    label: String,
}

fn plan_for(
    scan: &ScanResult,
    is_enabled: impl Fn(&SourceId) -> bool,
    coverage: Coverage,
) -> ActionPlan {
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
        // With nothing checked there is nothing to skip for: a source with no
        // known updates still gets its command, and the tool says "nothing to
        // do" far faster than paclens could have found that out.
        if targets.is_empty() && coverage == Coverage::Pending {
            continue;
        }
        // Most sources are one step. Flatpak is one source updated by one
        // tool, but its two installations are two commands with different
        // privilege — so a source contributes a *list* of steps (design §13,
        // 2026-09-07).
        let built: Vec<Built> = match &source.kind {
            SourceKind::Pacman => vec![Built {
                command: pacman::update_command(),
                privileged: true,
                // `-Syu` asks which provider to keep, what to replace, how to
                // resolve a conflict. Answering those is the entire reason
                // `--noconfirm` is banned (design §3), so the step owns the
                // terminal.
                interactive: true,
                targets,
                label: source.id.to_string(),
            }],
            // An AUR helper is never run under sudo — it self-elevates for the
            // install step after building as the user.
            //
            // The helper comes from the scan rather than from `PATH`: this
            // function is pure (P5), and a plan naming a helper the scan did
            // not use would be a plan for a different machine. `None` should
            // be unreachable here, since a scan with no helper marks the aur
            // source unavailable and the loop already skipped it — but the
            // plan is what the user is asked to confirm, so it invents nothing.
            SourceKind::Aur => match scan.aur_helper.helper() {
                Some(helper) => vec![Built {
                    command: aur::update_command(helper),
                    privileged: false,
                    // A helper shows PKGBUILD diffs, asks whether to proceed,
                    // and prompts for the password of the install step it
                    // elevates for itself. It owns the terminal too.
                    interactive: true,
                    targets,
                    label: source.id.to_string(),
                }],
                None => continue,
            },
            // One tool, two installations, two commands with different
            // privilege. Which scope an update belongs to is the installed
            // package's answer — `flatpak remote-ls` does not say, and the
            // same app id can legitimately be installed in both, in which
            // case both steps are right.
            //
            // `flatpak update` takes no package names: the command updates a
            // whole installation, so `targets` here is what the plan *shows*
            // and the scope is what it *does*. An update whose package the
            // scan cannot place falls to user scope, the unprivileged half —
            // it must not vanish from a plan the dashboard already counted.
            // Everything cargo installs lives under `$HOME`, so no step it
            // produces is ever privileged. `cargo-update` is what does the
            // updating; without it the source has no update path and the scan
            // records no updates, so this arm is not reached.
            SourceKind::Cargo => vec![Built {
                command: cargo::update_command(),
                privileged: false,
                // `cargo install-update -a` asks nothing; it prints what it
                // rebuilds and exits.
                interactive: false,
                targets,
                label: source.id.to_string(),
            }],
            SourceKind::Flatpak => {
                let scope_of = |name: &String| {
                    let scopes: Vec<FlatpakScope> = scan
                        .packages
                        .iter()
                        .filter(|p| &p.name == name && p.source_id == source.id)
                        .filter_map(|p| p.scope)
                        .collect();
                    if scopes.is_empty() {
                        vec![FlatpakScope::User]
                    } else {
                        scopes
                    }
                };
                [FlatpakScope::User, FlatpakScope::System]
                    .into_iter()
                    .filter_map(|scope| {
                        let scoped: Vec<String> = targets
                            .iter()
                            .filter(|name| scope_of(name).contains(&scope))
                            .cloned()
                            .collect();
                        // Unchecked, both installations get a command: which
                        // one holds an out-of-date app is exactly what was not
                        // looked up. The system half asks for root, and the
                        // user sees that in the plan before confirming.
                        (!scoped.is_empty() || coverage == Coverage::Everything).then(|| {
                            Built {
                                command: flatpak::update_command(scope),
                                privileged: scope.needs_privilege(),
                                // `--noninteractive` is in the command itself:
                                // flatpak answers its own prompts, so nothing
                                // is lost by taking its output away.
                                interactive: false,
                                targets: scoped,
                                label: format!("{} · {}", source.id, scope.label()),
                            }
                        })
                    })
                    .collect()
            }
        };
        for b in built {
            requires_sudo |= b.privileged;
            steps.push(ActionStep {
                source_id: source.id.clone(),
                kind: ActionKind::Update,
                targets: b.targets,
                command: b.command,
                label: b.label,
                privileged: b.privileged,
                interactive: b.interactive,
            });
        }
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
        label: source_id.to_string(),
        source_id: source_id.clone(),
        kind: ActionKind::Migrate,
        targets,
        command,
        // Profile data under `~` is user-owned even for a system-scope app.
        privileged: false,
        // `mkdir -p` and `cp -aT` ask nothing.
        interactive: false,
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
    let (source_id, targets, command, requires_sudo) = match report.direction {
        // Migrating to flatpak → the native package goes. pacman does the
        // removing even for AUR packages, so the step is pacman's (and sudo's).
        Direction::ToFlatpak => {
            let p = candidate.native_package.as_ref()?;
            (
                SourceId::pacman(),
                vec![p.name.clone()],
                vec!["pacman".to_string(), "-Rns".to_string(), p.name.clone()],
                true,
            )
        }
        // Migrating to native → the flatpak goes; the scope flag follows the
        // package, not the source. flatpak prompts for confirmation itself
        // (no -y).
        Direction::ToNative => {
            let app = candidate.flatpak_app.as_ref()?;
            // A flatpak with no recorded scope is a scan that predates the
            // field; user scope is the safe read — it is the unprivileged one.
            let scope = app.scope.unwrap_or(FlatpakScope::User);
            (
                app.source_id.clone(),
                vec![app.name.clone()],
                vec![
                    "flatpak".to_string(),
                    "uninstall".to_string(),
                    scope.flag().to_string(),
                    app.name.clone(),
                ],
                scope.needs_privilege(),
            )
        }
    };
    Some(ActionPlan {
        created_at: Utc::now(),
        steps: vec![ActionStep {
            label: source_id.to_string(),
            source_id,
            kind: ActionKind::Remove,
            targets,
            command,
            privileged: requires_sudo,
            // `pacman -Rns` and `flatpak uninstall` both ask first — neither
            // command carries a "yes to everything" flag, deliberately.
            interactive: true,
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

    fn flatpak_pkg(name: &str, scope: FlatpakScope) -> crate::model::Package {
        crate::model::Package {
            repo_version: None,
            name: name.to_string(),
            version: "1".to_string(),
            source_id: SourceId::flatpak(),
            install_reason: crate::model::InstallReason::Unknown,
            size_bytes: None,
            description: None,
            depends_on: Vec::new(),
            required_by: Vec::new(),
            optional_deps: Vec::new(),
            provides: Vec::new(),
            runtime: false,
            scope: Some(scope),
            foreign: false,
            signed: false,
            packager: None,
        }
    }

    /// pacman (2 updates) and flatpak (1 update, a user-scope app).
    fn scan() -> ScanResult {
        ScanResult {
            schema_version: SCHEMA_VERSION,
            scanned_at: Utc::now(),
            sources: vec![
                source(SourceId::pacman(), SourceKind::Pacman, true),
                source(SourceId::flatpak(), SourceKind::Flatpak, true),
            ],
            packages: vec![flatpak_pkg("org.gimp.GIMP", FlatpakScope::User)],
            updates: vec![
                upd("linux", SourceId::pacman()),
                upd("firefox", SourceId::pacman()),
                upd("org.gimp.GIMP", SourceId::flatpak()),
            ],
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

    fn enable_all(_: &SourceId) -> bool {
        true
    }

    /// A scan with one pending AUR update, driven by `helper`.
    fn aur_scan(helper: Option<crate::providers::aur::AurHelper>) -> ScanResult {
        use crate::providers::aur::HelperChoice;
        let helper = match helper {
            Some(h) => HelperChoice::Detected(h),
            None => HelperChoice::None,
        };
        let mut scan = scan();
        scan.sources
            .push(source(SourceId::aur(), SourceKind::Aur, true));
        scan.updates.push(upd("timr-bin", SourceId::aur()));
        scan.aur_helper = helper;
        scan
    }

    fn aur_step(plan: &ActionPlan) -> Option<ActionStep> {
        plan.steps
            .iter()
            .find(|s| s.source_id == SourceId::aur())
            .cloned()
    }

    #[test]
    fn the_aur_step_uses_the_helper_the_scan_recorded() {
        use crate::providers::aur::AurHelper;
        for (helper, bin) in [
            (AurHelper::Paru, "paru"),
            (AurHelper::Yay, "yay"),
            (AurHelper::Pikaur, "pikaur"),
        ] {
            let plan = plan_updates(&aur_scan(Some(helper)), enable_all);
            let step = aur_step(&plan).expect("aur step");
            assert_eq!(step.command, vec![bin, "-Sua"], "helper {bin}");
            assert_eq!(step.targets, vec!["timr-bin"]);
        }
    }

    #[test]
    fn no_recorded_helper_means_no_aur_step_rather_than_a_guess() {
        // The scan found no helper. Defaulting to paru here would put a
        // command in front of the user for a binary they do not have.
        let plan = plan_updates(&aur_scan(None), enable_all);
        assert!(aur_step(&plan).is_none());
        // The other sources are unaffected — one bad source never aborts the rest.
        assert_eq!(plan.source_count(), 2);
    }

    #[test]
    fn the_aur_step_is_never_privileged() {
        use crate::providers::aur::AurHelper;
        for helper in AurHelper::ALL {
            let plan = plan_updates(&aur_scan(Some(helper)), |id| id == &SourceId::aur());
            assert_eq!(plan.source_count(), 1);
            assert!(
                !plan.requires_sudo,
                "{} must self-elevate, never be run under sudo",
                helper.bin()
            );
        }
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
        assert_eq!(plan.steps[1].source_id, SourceId::flatpak());
        assert_eq!(plan.steps[1].targets, vec!["org.gimp.GIMP"]);
        assert_eq!(
            plan.steps[1].command,
            vec!["flatpak", "update", "--user", "--noninteractive"]
        );
        assert_eq!(plan.steps[0].kind, ActionKind::Update);
    }

    #[test]
    fn a_cargo_step_is_never_privileged() {
        // Everything cargo installs lives under $HOME. This is the first
        // source whose steps are unprivileged by nature rather than by the
        // helper self-elevating, and the old id-sniffing rule would have
        // wrapped it in sudo (design §13, 2026-09-07).
        let mut s = scan();
        s.sources
            .push(source(SourceId::cargo(), SourceKind::Cargo, true));
        s.updates.push(upd("ripgrep", SourceId::cargo()));
        let plan = plan_updates(&s, |id| id == &SourceId::cargo());
        assert_eq!(plan.steps.len(), 1);
        let step = &plan.steps[0];
        assert!(!step.privileged, "cargo must never run under sudo");
        assert_eq!(step.command, ["cargo", "install-update", "-a"]);
        assert!(!plan.requires_sudo);
    }

    #[test]
    fn every_step_declares_its_own_privilege() {
        // The planner is where privilege is decided now (design §13,
        // 2026-09-07). Each step says so on its own, rather than the executor
        // recognising an id and guessing.
        let plan = plan_updates(&scan(), enable_all);
        for step in &plan.steps {
            let expected = match (step.source_id.as_str(), step.command.get(2)) {
                ("pacman", _) => true,
                // The helper self-elevates after building as the user.
                ("aur", _) => false,
                ("flatpak", Some(flag)) => flag == "--system",
                ("cargo", _) => false,
                other => panic!("unexpected step in the plan: {other:?}"),
            };
            assert_eq!(
                step.privileged, expected,
                "{} declared the wrong privilege",
                step.source_id
            );
        }
        assert_eq!(
            plan.requires_sudo,
            plan.steps.iter().any(|s| s.privileged),
            "the plan-level flag must agree with its steps"
        );
    }

    #[test]
    fn pacman_in_the_plan_requires_sudo() {
        assert!(plan_updates(&scan(), enable_all).requires_sudo);
    }

    #[test]
    fn flatpak_user_only_does_not_require_sudo() {
        let plan = plan_updates(&scan(), |id| id == &SourceId::flatpak());
        assert_eq!(plan.source_count(), 1);
        assert!(!plan.requires_sudo);
    }

    #[test]
    fn flatpak_system_with_updates_requires_sudo() {
        // One source, two installations: the scope comes from the installed
        // package, and only the system half asks for root.
        let mut s = scan();
        s.packages
            .push(flatpak_pkg("org.sys.App", FlatpakScope::System));
        s.updates.push(upd("org.sys.App", SourceId::flatpak()));
        let plan = plan_updates(&s, |id| id == &SourceId::flatpak());
        assert_eq!(plan.source_count(), 1, "still one source");
        assert_eq!(plan.steps.len(), 2, "one step per installation with work");
        assert!(plan.requires_sudo);

        let user = &plan.steps[0];
        assert_eq!(
            user.command,
            vec!["flatpak", "update", "--user", "--noninteractive"]
        );
        assert!(!user.privileged, "the user installation needs no root");
        assert_eq!(user.targets, vec!["org.gimp.GIMP"]);

        let system = &plan.steps[1];
        assert_eq!(
            system.command,
            vec!["flatpak", "update", "--system", "--noninteractive"]
        );
        assert!(system.privileged);
        assert_eq!(system.targets, vec!["org.sys.App"]);
    }

    #[test]
    fn a_flatpak_update_the_scan_cannot_place_stays_in_the_plan() {
        // A stale cache can hold an update whose package is not in the scan.
        // Dropping it would leave the plan showing fewer packages than the
        // dashboard counted; it falls to the unprivileged half instead.
        let mut s = scan();
        s.updates.push(upd("org.ghost.App", SourceId::flatpak()));
        let plan = plan_updates(&s, |id| id == &SourceId::flatpak());
        assert_eq!(plan.steps.len(), 1);
        assert!(!plan.steps[0].privileged);
        assert!(plan.steps[0].targets.contains(&"org.ghost.App".to_string()));
    }

    #[test]
    fn an_app_installed_in_both_installations_updates_in_both() {
        let mut s = scan();
        s.packages
            .push(flatpak_pkg("org.gimp.GIMP", FlatpakScope::System));
        let plan = plan_updates(&s, |id| id == &SourceId::flatpak());
        assert_eq!(plan.steps.len(), 2, "both installations hold it");
        assert!(plan.steps.iter().all(|s| s.targets == ["org.gimp.GIMP"]));
    }

    #[test]
    fn predicate_excludes_a_source() {
        let plan = plan_updates(&scan(), |id| id != &SourceId::pacman());
        assert_eq!(plan.source_count(), 1);
        assert_eq!(plan.steps[0].source_id, SourceId::flatpak());
        assert!(!plan.requires_sudo);
    }

    #[test]
    fn unavailable_source_is_skipped_even_with_updates() {
        let mut s = scan();
        s.sources[0].available = false; // pacman unavailable
        let plan = plan_updates(&s, enable_all);
        assert_eq!(plan.source_count(), 1);
        assert_eq!(plan.steps[0].source_id, SourceId::flatpak());
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
            aur_helper: crate::providers::aur::HelperChoice::Detected(
                crate::providers::aur::AurHelper::Paru,
            ),
            kernel: None,
            pacfiles: Vec::new(),
            stale_processes: Vec::new(),
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
                scope: None,
                name: "firefox".to_string(),
                version: "141.0-1".to_string(),
                source_id: SourceId::pacman(),
            }),
            flatpak_app: Some(PackageRef {
                scope: None,
                name: "org.mozilla.firefox".to_string(),
                version: "141.0".to_string(),
                source_id: SourceId::flatpak(),
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
                .all(|s| s.source_id == SourceId::flatpak()),
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
        assert_eq!(step.source_id, SourceId::flatpak());
        assert_eq!(
            step.command,
            vec!["flatpak", "uninstall", "--user", "org.mozilla.firefox"]
        );
        assert!(!crate::executor::needs_privilege(step));
    }

    #[test]
    fn removal_plan_system_flatpak_is_privileged() {
        // The scope flag follows the package, not the source id — there is
        // one flatpak id and it cannot answer this.
        let mut c = candidate();
        c.flatpak_app.as_mut().expect("app").scope = Some(FlatpakScope::System);
        let r = report(Direction::ToNative, Vec::new());
        let plan = plan_removal(&r, &c).expect("plan");
        assert!(plan.requires_sudo);
        assert!(plan.steps[0].privileged);
        assert_eq!(plan.steps[0].command[2], "--system");
    }

    #[test]
    fn removal_plan_missing_side_is_none() {
        let mut c = candidate();
        c.native_package = None;
        let r = report(Direction::ToFlatpak, Vec::new());
        assert!(plan_removal(&r, &c).is_none());
    }

    /// The split `--parallel` runs on, declared per step and never read out
    /// of the source id — the same rule as `privileged` (design §13,
    /// 2026-09-07), with the same failure mode if it breaks: a pacman prompt
    /// handed to a thread with no terminal hangs the whole run.
    #[test]
    fn only_the_sources_that_ask_questions_own_the_terminal() {
        let mut s = scan();
        s.sources
            .push(source(SourceId::aur(), SourceKind::Aur, true));
        s.sources
            .push(source(SourceId::cargo(), SourceKind::Cargo, true));
        let plan = plan_full_upgrade(&s, enable_all);

        let owns = |label: &str| {
            plan.steps
                .iter()
                .find(|st| st.label == label)
                .map(|st| st.interactive)
        };
        assert_eq!(owns("pacman"), Some(true), "-Syu asks about conflicts");
        assert_eq!(owns("aur"), Some(true), "a helper shows diffs and asks");
        assert_eq!(owns("cargo"), Some(false));
        assert_eq!(owns("flatpak · user"), Some(false));
        assert_eq!(owns("flatpak · system"), Some(false));
    }

    #[test]
    fn migrate_steps_never_ask_for_privilege() {
        // Even with a pacman source id, a Migrate step stays unprivileged.
        let step = ActionStep {
            label: "pacman".to_string(),
            source_id: SourceId::pacman(),
            kind: ActionKind::Migrate,
            targets: vec!["~/.config/x".to_string()],
            command: vec!["cp".to_string()],
            privileged: false,
            interactive: false,
        };
        assert!(!crate::executor::needs_privilege(&step));
        assert_eq!(crate::executor::skip_reason(&step, None), None);
    }
}
