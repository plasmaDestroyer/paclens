//! Who holds pacman's database lock, or that nobody does (#5).
//!
//! pacman refuses to start while `db.lck` exists and says only "unable to lock
//! database". Whether to wait or to remove the file depends on one question —
//! is a package manager still running? — and this answers it as honestly as an
//! unprivileged process can (design §10, "pacman db lock detection").
//!
//! The evidence, best first:
//! - an open file descriptor on the lock: libalpm keeps it open for the whole
//!   transaction, so this names the holder outright. Only readable for the
//!   caller's own processes, so usually not available for a root pacman.
//! - a running process that takes the lock (pacman, pamac, PackageKit): it
//!   *likely* holds it, but a `pacman -Q` takes no lock and looks the same.
//! - none of these, with every process visible: likely stale from a crashed
//!   run. With `/proc` mounted `hidepid`, other users' processes are hidden
//!   and "none found" proves nothing, so the answer is unknown.
//!
//! Not the lock's age against boot time: `btime` moves when the clock is set
//! after boot, so a live lock taken before an NTP step would read as stale.
//!
//! paclens never removes the lock (design §3, P2); the stale case suggests the
//! `rm` as text.

use crate::model::{ActionPlan, Confidence};

/// Where pacman's lock lives under the default `DBPath`.
pub const LOCK_PATH: &str = "/var/lib/pacman/db.lck";

/// Process names that take the libalpm lock on the system database. Not the
/// AUR helpers: they build as the user and hold nothing until their own
/// `pacman -U` child does, which this list names instead.
///
/// pacman exits when its transaction does, so a running one likely holds the
/// lock.
const RUNS: &[&str] = &["pacman", "pacman-static"];
/// These stay up while idle, so one running says nothing about a stale lock:
/// it *may* hold it. Named only when no pacman is running.
const DAEMONS: &[&str] = &["pamac-daemon", "packagekitd"];

/// One process, as far as it matters to the lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockProc {
    pub pid: u32,
    /// `/proc/<pid>/comm`: readable for every process.
    pub comm: String,
    /// One of its file descriptors points at the lock. Only ever true for a
    /// process whose `fd` directory the caller may read.
    pub holds: bool,
}

/// What the lock means right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbLock {
    /// No lock file: pacman can start.
    Free,
    /// A process holds it, or likely does.
    Held {
        pid: u32,
        name: String,
        confidence: Confidence,
    },
    /// Likely nothing holds it.
    Stale,
    /// Locked, and what holds it could not be looked for: `hidepid`, or a
    /// read of `/proc` that failed.
    Unknown,
}

/// Decide from measured facts. `locked`: the lock file exists. `visible`:
/// other users' processes can be seen (no `hidepid`).
pub fn verdict(locked: bool, visible: bool, procs: &[LockProc]) -> DbLock {
    if !locked {
        return DbLock::Free;
    }
    let held = |p: &LockProc, confidence| DbLock::Held {
        pid: p.pid,
        name: p.comm.clone(),
        confidence,
    };
    if let Some(p) = procs.iter().find(|p| p.holds) {
        return held(p, Confidence::Confirmed);
    }
    for names in [RUNS, DAEMONS] {
        if let Some(p) = procs.iter().find(|p| names.contains(&p.comm.as_str())) {
            return held(p, Confidence::Inferred);
        }
    }
    if visible {
        DbLock::Stale
    } else {
        DbLock::Unknown
    }
}

impl DbLock {
    pub fn is_locked(&self) -> bool {
        !matches!(self, DbLock::Free)
    }

    /// Would this lock stop `plan`? Only a step that takes the lock waits on
    /// it; a flatpak-only plan runs whatever pacman is doing.
    pub fn blocks(&self, plan: &ActionPlan) -> bool {
        self.is_locked() && plan.steps.iter().any(|s| s.pacman_db)
    }

    /// The short value for a dashboard row; `None` when there is nothing to say.
    pub fn label(&self) -> Option<String> {
        Some(match self {
            DbLock::Free => return None,
            // Hedge first, pid left to the sentence: at 80 columns the row
            // keeps about 21 characters, and a cut hedge reads as fact.
            DbLock::Held {
                name,
                confidence: Confidence::Confirmed,
                ..
            } => format!("held by {name}"),
            DbLock::Held { name, .. } if DAEMONS.contains(&name.as_str()) => {
                format!("{name} or stale")
            }
            DbLock::Held { name, .. } => format!("likely {name}"),
            DbLock::Stale => "likely stale".into(),
            DbLock::Unknown => "holder unknown".into(),
        })
    }

    /// The whole sentence: what holds the lock and what to do about it.
    /// `tool` is the privilege tool the suggested `rm` runs under. Kept under
    /// 75 columns: the TUI shows it in an 80-column border, and a cut-off
    /// `rm` is worse than none.
    pub fn explain(&self, tool: Option<&str>) -> Option<String> {
        Some(match self {
            DbLock::Free => return None,
            DbLock::Held {
                pid,
                name,
                confidence: Confidence::Confirmed,
            } => format!("pacman db locked by {name} (pid {pid}) — wait for it to finish"),
            DbLock::Held { pid, name, .. } if DAEMONS.contains(&name.as_str()) => {
                format!("pacman db locked; {name} (pid {pid}) may hold it, or it is stale")
            }
            DbLock::Held { pid, name, .. } => {
                format!("pacman db likely held by {name} (pid {pid}) — wait for it")
            }
            DbLock::Unknown => {
                "pacman db locked — holder not identified; check running package tools".into()
            }
            DbLock::Stale => format!(
                "pacman db likely stale; none running? {} rm {LOCK_PATH}",
                tool.unwrap_or("sudo")
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ActionKind, ActionStep, SourceId};

    fn proc(pid: u32, comm: &str, holds: bool) -> LockProc {
        LockProc {
            pid,
            comm: comm.into(),
            holds,
        }
    }

    fn held(pid: u32, name: &str, confidence: Confidence) -> DbLock {
        DbLock::Held {
            pid,
            name: name.into(),
            confidence,
        }
    }

    #[test]
    fn no_lock_file_is_free_whatever_runs() {
        assert_eq!(
            verdict(false, true, &[proc(7, "pacman", true)]),
            DbLock::Free
        );
        assert!(!DbLock::Free.is_locked());
        assert_eq!(DbLock::Free.label(), None);
        assert_eq!(DbLock::Free.explain(None), None);
    }

    #[test]
    fn an_open_descriptor_names_the_holder_outright() {
        let procs = [proc(7, "pacman", false), proc(42, "pamac-daemon", true)];
        let v = verdict(true, true, &procs);
        assert_eq!(v, held(42, "pamac-daemon", Confidence::Confirmed));
        assert_eq!(v.label().as_deref(), Some("held by pamac-daemon"));
    }

    #[test]
    fn any_process_holding_the_descriptor_counts_not_just_known_names() {
        let v = verdict(true, true, &[proc(9, "my-alpm-tool", true)]);
        assert_eq!(v, held(9, "my-alpm-tool", Confidence::Confirmed));
    }

    #[test]
    fn a_running_locker_likely_holds_it() {
        let procs = [proc(3, "bash", false), proc(4242, "pacman", false)];
        let v = verdict(true, true, &procs);
        assert_eq!(v, held(4242, "pacman", Confidence::Inferred));
        assert_eq!(v.label().as_deref(), Some("likely pacman"));
    }

    #[test]
    fn a_running_locker_is_named_even_when_other_processes_are_hidden() {
        // Its own name is evidence; hidden processes only weaken "none found".
        let v = verdict(true, false, &[proc(4242, "pacman", false)]);
        assert_eq!(v, held(4242, "pacman", Confidence::Inferred));
    }

    #[test]
    fn a_running_pacman_outranks_an_idle_daemon() {
        let procs = [proc(10, "packagekitd", false), proc(4242, "pacman", false)];
        assert_eq!(
            verdict(true, true, &procs),
            held(4242, "pacman", Confidence::Inferred)
        );
    }

    #[test]
    fn a_daemon_alone_may_hold_it_and_the_lock_may_be_stale() {
        // Daemons idle for hours: "wait for it" could mean waiting forever.
        let v = verdict(true, true, &[proc(10, "packagekitd", false)]);
        assert_eq!(v, held(10, "packagekitd", Confidence::Inferred));
        assert_eq!(v.label().as_deref(), Some("packagekitd or stale"));
        let text = v.explain(None).unwrap_or_default();
        assert!(text.contains("may hold it, or it is stale"), "{text}");
        assert!(!text.contains("wait for it"), "{text}");
    }

    #[test]
    fn every_label_fits_the_80_column_row() {
        for lock in [
            held(1_234_567, "pamac-daemon", Confidence::Confirmed),
            held(1_234_567, "pamac-daemon", Confidence::Inferred),
            held(1_234_567, "pacman-static", Confidence::Inferred),
            DbLock::Stale,
            DbLock::Unknown,
        ] {
            let label = lock.label().unwrap_or_default();
            assert!(label.chars().count() <= 21, "{label}");
        }
    }

    #[test]
    fn each_known_locker_is_recognised() {
        for name in RUNS.iter().chain(DAEMONS) {
            let v = verdict(true, true, &[proc(1, name, false)]);
            assert!(matches!(v, DbLock::Held { .. }), "{name} not recognised");
        }
    }

    #[test]
    fn an_aur_helper_alone_is_not_blamed() {
        // paru builds as the user and holds nothing until its pacman child runs.
        let procs = [proc(5, "paru", false), proc(6, "yay", false)];
        let v = verdict(true, true, &procs);
        assert_eq!(v, DbLock::Stale);
        assert_eq!(v.label().as_deref(), Some("likely stale"));
    }

    #[test]
    fn nothing_found_among_visible_processes_is_likely_stale() {
        // This machine on 2026-11-05: db.lck since 19:00, no locker running.
        let v = verdict(true, true, &[proc(1, "systemd", false)]);
        assert_eq!(v, DbLock::Stale);
    }

    #[test]
    fn nothing_found_with_processes_hidden_is_unknown_not_stale() {
        let v = verdict(true, false, &[proc(2000, "zsh", false)]);
        assert_eq!(v, DbLock::Unknown);
        assert_eq!(v.label().as_deref(), Some("holder unknown"));
        let text = v.explain(Some("sudo")).unwrap_or_default();
        assert!(text.contains("not identified"), "{text}");
        // Cannot tell, so no removal is suggested.
        assert!(!text.contains("rm "), "{text}");
    }

    #[test]
    fn the_stale_explanation_suggests_the_rm_as_text_under_the_tool() {
        let text = DbLock::Stale.explain(Some("doas")).unwrap_or_default();
        assert!(text.contains("likely stale"), "{text}");
        assert!(
            text.starts_with("pacman db likely stale; none running?"),
            "{text}"
        );
        assert!(text.ends_with("doas rm /var/lib/pacman/db.lck"), "{text}");
        let text = DbLock::Stale.explain(None).unwrap_or_default();
        assert!(text.ends_with("sudo rm /var/lib/pacman/db.lck"), "{text}");
    }

    #[test]
    fn a_held_lock_says_wait_and_never_suggests_removing_it() {
        for confidence in [Confidence::Confirmed, Confidence::Inferred] {
            let text = held(4242, "pacman", confidence)
                .explain(Some("sudo"))
                .unwrap_or_default();
            assert!(text.contains("pacman (pid 4242)"), "{text}");
            assert!(text.contains("wait for it"), "{text}");
            assert!(!text.contains("rm "), "{text}");
        }
    }

    #[test]
    fn every_sentence_fits_an_80_column_flash() {
        // The flash pads one space each side inside a 76-column slot.
        let all = [
            held(1_234_567, "pamac-daemon", Confidence::Confirmed),
            held(1_234_567, "pamac-daemon", Confidence::Inferred),
            held(1_234_567, "pacman-static", Confidence::Inferred),
            // comm is at most 15 bytes, so at most 15 columns.
            held(4_194_304, "123456789012345", Confidence::Confirmed),
            DbLock::Stale,
            DbLock::Unknown,
        ];
        for (lock, tool) in all
            .iter()
            .flat_map(|l| ["sudo", "doas", "pkexec"].map(|t| (l, t)))
        {
            let text = lock.explain(Some(tool)).unwrap_or_default();
            assert!(
                text.chars().count() <= 74,
                "{} > 74: {text}",
                text.chars().count()
            );
        }
    }

    #[test]
    fn only_the_inferred_holder_is_hedged() {
        let sure = held(1, "pacman", Confidence::Confirmed)
            .explain(None)
            .unwrap_or_default();
        let guess = held(1, "pacman", Confidence::Inferred)
            .explain(None)
            .unwrap_or_default();
        assert!(!sure.contains("likely"), "{sure}");
        assert!(guess.contains("likely held by"), "{guess}");
    }

    fn step(pacman_db: bool) -> ActionStep {
        ActionStep {
            source_id: SourceId::flatpak(),
            kind: ActionKind::Update,
            targets: vec![],
            command: vec!["x".into()],
            label: "x".into(),
            privileged: false,
            interactive: false,
            pacman_db,
        }
    }

    #[test]
    fn only_a_plan_that_takes_the_lock_is_blocked() {
        let plan = |steps| ActionPlan { steps };
        let with = plan(vec![step(false), step(true)]);
        let without = plan(vec![step(false)]);
        for lock in [
            DbLock::Stale,
            DbLock::Unknown,
            held(1, "pacman", Confidence::Inferred),
        ] {
            assert!(lock.blocks(&with), "{lock:?}");
            assert!(
                !lock.blocks(&without),
                "{lock:?} blocked a flatpak-only plan"
            );
        }
        assert!(!DbLock::Free.blocks(&with));
    }
}
