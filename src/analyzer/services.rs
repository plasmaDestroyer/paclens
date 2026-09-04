//! Processes still running against files an upgrade replaced (design §3, #4).
//!
//! Most upgrades do not need a reboot; they need a handful of services
//! bounced. A running process keeps its old library mapped even after the file
//! is gone, so it works until it tries to load something new — and then fails
//! hours later, far from the upgrade that caused it.
//!
//! Every finding here is **inferred** (P3). A deleted mapping proves the file
//! on disk changed, not that anything is broken: a long-lived process may
//! never touch the missing code, and restarting is a cost of its own. So this
//! reports and suggests, and the suggestion is text.

use serde::{Deserialize, Serialize};

/// Which manager owns the unit, which decides whether restarting it needs
/// privilege — and whether paclens could see it at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnitScope {
    /// `systemctl --user` — the caller's own session, no privilege needed.
    User,
    /// `sudo systemctl` — system-wide.
    System,
}

/// One process the scanner found holding a deleted file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaleProcess {
    pub pid: u32,
    /// `/proc/<pid>/comm` — the short name, which is what a reader recognises.
    pub comm: String,
    /// The systemd unit from the process's cgroup, when it has one. A process
    /// outside any unit (a bare fork, a container) has none, and there is no
    /// command to suggest for it.
    pub unit: Option<String>,
    pub scope: Option<UnitScope>,
}

/// A unit worth restarting, with the processes that say so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleUnit {
    pub unit: String,
    pub scope: UnitScope,
    /// Process names, deduplicated, in the order found.
    pub processes: Vec<String>,
    /// Restarting this one ends the session it belongs to.
    pub session_critical: bool,
}

impl StaleUnit {
    /// The command that restarts it. A user unit needs no privilege; a system
    /// one does, and saying so is part of the suggestion.
    pub fn restart_command(&self) -> String {
        match self.scope {
            UnitScope::User => format!("systemctl --user restart {}", self.unit),
            UnitScope::System => format!("sudo systemctl restart {}", self.unit),
        }
    }
}

/// Units whose restart takes the graphical session — and everything running
/// inside it — down with them. Never suggested casually.
fn is_session_critical(unit: &str) -> bool {
    const CRITICAL: &[&str] = &[
        "display-manager.service",
        "gdm.service",
        "sddm.service",
        "lightdm.service",
        "greetd.service",
        "ly.service",
        "graphical-session.target",
        "dbus-broker.service",
        "dbus.service",
    ];
    // `session-9.scope` is the login session itself, and `user@1000.service`
    // is the whole user manager: both take everything with them.
    unit.starts_with("session-") && unit.ends_with(".scope")
        || unit.starts_with("user@")
        || unit.starts_with("user-") && unit.ends_with(".slice")
        || CRITICAL.contains(&unit)
}

/// The unit and scope named by a cgroup v2 line, e.g.
/// `0::/user.slice/user-1000.slice/user@1000.service/app.slice/foo.service`.
///
/// Only `.service` and `.scope` leaves name something restartable; a process
/// sitting directly in a slice does not.
pub fn unit_from_cgroup(cgroup: &str) -> Option<(String, UnitScope)> {
    let path = cgroup.lines().find_map(|l| l.strip_prefix("0::"))?;
    let scope = if path.contains("/user.slice/") {
        UnitScope::User
    } else {
        UnitScope::System
    };
    let leaf = path.rsplit('/').find(|s| !s.is_empty())?;
    if !(leaf.ends_with(".service") || leaf.ends_with(".scope")) {
        return None;
    }
    Some((leaf.to_string(), scope))
}

/// Does this deleted mapping mean anything? Only files a package owns do.
///
/// `/etc/ld.so.cache (deleted)` is rewritten by every `ldconfig` run and says
/// nothing about the process; `/memfd:`, `/dev/zero` and deleted temp files
/// are noise of the same kind. Package content lives under `/usr`.
pub fn mapping_matters(path: &str) -> bool {
    path.starts_with("/usr/")
}

/// Group the processes into units worth restarting, most processes first.
///
/// A process with no unit is dropped rather than listed: there is no command
/// to suggest for it, and a finding a reader cannot act on is noise.
pub fn stale_units(processes: &[StaleProcess]) -> Vec<StaleUnit> {
    let mut out: Vec<StaleUnit> = Vec::new();
    for p in processes {
        let (Some(unit), Some(scope)) = (p.unit.as_ref(), p.scope) else {
            continue;
        };
        match out.iter_mut().find(|u| &u.unit == unit) {
            Some(existing) => {
                if !existing.processes.contains(&p.comm) {
                    existing.processes.push(p.comm.clone());
                }
            }
            None => out.push(StaleUnit {
                unit: unit.clone(),
                scope,
                processes: vec![p.comm.clone()],
                session_critical: is_session_critical(unit),
            }),
        }
    }
    // Session-critical last: they are the ones a reader must not run without
    // thinking, so they do not head the list.
    out.sort_by(|a, b| {
        a.session_critical
            .cmp(&b.session_critical)
            .then_with(|| b.processes.len().cmp(&a.processes.len()))
            .then_with(|| a.unit.cmp(&b.unit))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, comm: &str, cgroup: &str) -> StaleProcess {
        let (unit, scope) = match unit_from_cgroup(cgroup) {
            Some((u, s)) => (Some(u), Some(s)),
            None => (None, None),
        };
        StaleProcess {
            pid,
            comm: comm.to_string(),
            unit,
            scope,
        }
    }

    #[test]
    fn cgroup_lines_name_the_unit_and_who_owns_it() {
        // Captured from a real machine.
        assert_eq!(
            unit_from_cgroup(
                "0::/user.slice/user-1000.slice/user@1000.service/session.slice/pipewire.service"
            ),
            Some(("pipewire.service".to_string(), UnitScope::User))
        );
        assert_eq!(
            unit_from_cgroup("0::/system.slice/NetworkManager.service"),
            Some(("NetworkManager.service".to_string(), UnitScope::System))
        );
        assert_eq!(
            unit_from_cgroup("0::/user.slice/user-1000.slice/session-9.scope"),
            Some(("session-9.scope".to_string(), UnitScope::User))
        );
        // A process sitting in a bare slice names nothing restartable.
        assert_eq!(unit_from_cgroup("0::/user.slice/user-1000.slice"), None);
        assert_eq!(unit_from_cgroup(""), None);
    }

    #[test]
    fn only_package_files_count_as_findings() {
        assert!(mapping_matters("/usr/lib/libcrypto.so.3"));
        assert!(mapping_matters("/usr/bin/Hyprland"));
        // Rewritten by ldconfig on every upgrade; says nothing about a process.
        assert!(!mapping_matters("/etc/ld.so.cache"));
        assert!(!mapping_matters("/memfd:wayland-shm"));
        assert!(!mapping_matters("/tmp/scratch"));
        assert!(!mapping_matters("/home/t/.cache/thing"));
    }

    #[test]
    fn processes_group_by_unit_and_keep_their_names() {
        let procs = vec![
            proc(
                1,
                "pipewire",
                "0::/user.slice/user-1000.slice/user@1000.service/session.slice/pipewire.service",
            ),
            proc(
                2,
                "pipewire-pulse",
                "0::/user.slice/user-1000.slice/user@1000.service/session.slice/pipewire.service",
            ),
            proc(3, "nginx", "0::/system.slice/nginx.service"),
        ];
        let units = stale_units(&procs);
        assert_eq!(units.len(), 2);
        // Two processes beat one, so pipewire heads the list.
        assert_eq!(units[0].unit, "pipewire.service");
        assert_eq!(units[0].processes, vec!["pipewire", "pipewire-pulse"]);
        assert_eq!(units[0].scope, UnitScope::User);
        assert_eq!(
            units[0].restart_command(),
            "systemctl --user restart pipewire.service"
        );
        assert_eq!(
            units[1].restart_command(),
            "sudo systemctl restart nginx.service"
        );
    }

    #[test]
    fn a_process_with_no_unit_is_not_a_finding() {
        let procs = vec![proc(1, "orphaned", "0::/user.slice/user-1000.slice")];
        assert!(
            stale_units(&procs).is_empty(),
            "nothing to suggest, so nothing to say"
        );
    }

    #[test]
    fn session_critical_units_are_marked_and_sorted_last() {
        let procs = vec![
            proc(
                1,
                "Hyprland",
                "0::/user.slice/user-1000.slice/session-9.scope",
            ),
            proc(2, "nginx", "0::/system.slice/nginx.service"),
        ];
        let units = stale_units(&procs);
        assert_eq!(units[0].unit, "nginx.service", "safe ones first");
        assert!(!units[0].session_critical);
        assert_eq!(units[1].unit, "session-9.scope");
        assert!(
            units[1].session_critical,
            "restarting the session scope ends the session"
        );
        assert!(is_session_critical("user@1000.service"));
        assert!(is_session_critical("display-manager.service"));
        assert!(!is_session_critical("pipewire.service"));
    }
}
