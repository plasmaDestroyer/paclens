//! Does the running kernel still match the installed one? (design §3, #3)
//!
//! A kernel upgrade replaces `/usr/lib/modules/<release>`, so anything not
//! already loaded — a USB driver, nvidia, a filesystem module — fails until
//! the machine reboots. The failure is delayed and reads as hardware trouble,
//! which is exactly the kind of thing paclens exists to say out loud.
//!
//! Pure over `(running release, installed packages)`: the scanner reads
//! `/proc/sys/kernel/osrelease` and whether the modules directory still
//! exists, and nothing here touches the system.

use crate::model::Package;

/// What the running kernel is, as the scanner found it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunningKernel {
    /// `uname -r`, e.g. `7.2.2-1-cachyos`.
    pub release: String,
    /// Does `/usr/lib/modules/<release>` still exist? When it does not, the
    /// running kernel's modules are already gone — the upgrade happened and
    /// anything not yet loaded cannot load.
    pub modules_present: bool,
}

/// The verdict. `Unknown` carries why, because a kernel this cannot identify
/// is a thing to say rather than a thing to guess (P3): a false "reboot
/// required" is worse than no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebootStatus {
    /// Running exactly what is installed.
    UpToDate { release: String },
    /// The installed kernel moved on. `modules_gone` is the stronger case:
    /// the running kernel's modules are no longer on disk.
    Required {
        running: String,
        installed: String,
        package: String,
        modules_gone: bool,
    },
    /// A running kernel this cannot match against anything installed, and
    /// why. A finding: something is odd about this machine's kernels.
    Unknown { reason: String },
    /// The scan recorded no kernel at all — a cache written before the field
    /// existed, or a system without `/proc`. Not a finding about the machine,
    /// so the surfaces stay quiet about it.
    NotRecorded,
}

/// The flavour suffix of a `uname -r` release: the trailing segments that do
/// not start with a digit.
///
/// `7.2.2-1-cachyos` → `-cachyos`, `6.18.48-1-cachyos-lts` → `-cachyos-lts`,
/// `6.16.8-zen1-1-zen` → `-zen`, and mainline `6.16.8-arch1-1` → `""`, whose
/// package is plain `linux`.
fn flavour(release: &str) -> String {
    let segments: Vec<&str> = release.split('-').collect();
    let mut start = segments.len();
    for (i, seg) in segments.iter().enumerate().rev() {
        if seg.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            break;
        }
        start = i;
    }
    if start >= segments.len() {
        return String::new();
    }
    format!("-{}", segments[start..].join("-"))
}

/// The `uname -r` form of an installed kernel package's version.
///
/// pacman writes the mainline kernel as `6.16.8.arch1-1` where `uname`
/// reports `6.16.8-arch1-1`, so the first dot introducing letters becomes a
/// dash; the flavour is then the package's own name minus `linux`. An epoch
/// is not part of a release string.
pub fn uname_form(name: &str, version: &str) -> Option<String> {
    let flavour = name.strip_prefix("linux")?;
    let version = version.rsplit_once(':').map_or(version, |(_, v)| v);
    let mut base = String::with_capacity(version.len());
    let mut dashed = false;
    for (i, c) in version.char_indices() {
        let starts_letters = !dashed
            && c == '.'
            && version[i + 1..]
                .chars()
                .next()
                .is_some_and(|n| n.is_ascii_alphabetic());
        if starts_letters {
            base.push('-');
            dashed = true;
        } else {
            base.push(c);
        }
    }
    Some(format!("{base}{flavour}"))
}

/// Is this the package that owns the running kernel? Decided by the flavour,
/// which is what distinguishes `linux-cachyos` from `linux-cachyos-lts` when
/// both are installed and only one is booted.
fn booted_package_name(release: &str) -> String {
    format!("linux{}", flavour(release))
}

/// Compare what is running against what is installed.
pub fn reboot_status(kernel: Option<&RunningKernel>, packages: &[Package]) -> RebootStatus {
    let Some(kernel) = kernel else {
        return RebootStatus::NotRecorded;
    };
    let wanted = booted_package_name(&kernel.release);
    let Some(pkg) = packages.iter().find(|p| p.name == wanted) else {
        return RebootStatus::Unknown {
            reason: format!("running {}, and no {wanted} is installed", kernel.release),
        };
    };
    let Some(installed) = uname_form(&pkg.name, &pkg.version) else {
        return RebootStatus::Unknown {
            reason: format!("cannot read {}'s version as a release", pkg.name),
        };
    };
    if installed == kernel.release {
        return RebootStatus::UpToDate {
            release: kernel.release.clone(),
        };
    }
    RebootStatus::Required {
        running: kernel.release.clone(),
        installed,
        package: pkg.name.clone(),
        modules_gone: !kernel.modules_present,
    }
}

impl RebootStatus {
    /// The one sentence both surfaces print, so the TUI and the CLI can never
    /// word it differently (P5). `None` when there is nothing to say: a
    /// machine running what it has installed is the normal case, and a line
    /// saying so every time is furniture.
    pub fn note(&self) -> Option<String> {
        match self {
            RebootStatus::UpToDate { .. } => None,
            RebootStatus::Required {
                running,
                installed,
                modules_gone,
                ..
            } => Some(if *modules_gone {
                format!(
                    "required (running {running}, installed {installed} — its modules are gone)"
                )
            } else {
                format!("required (running {running}, installed {installed})")
            }),
            RebootStatus::Unknown { reason } => Some(format!("unknown ({reason})")),
            RebootStatus::NotRecorded => None,
        }
    }

    /// The dashboard's version. The system pane has ~26 columns for a value,
    /// which the full sentence does not fit — and a truncated sentence says
    /// less than a short one. Both live here so the two surfaces cannot drift
    /// apart in what they claim, only in how much room they have to say it.
    pub fn label(&self) -> Option<&'static str> {
        match self {
            RebootStatus::UpToDate { .. } => None,
            RebootStatus::Required {
                modules_gone: true, ..
            } => Some("required · modules gone"),
            RebootStatus::Required { .. } => Some("required"),
            RebootStatus::Unknown { .. } => Some("unknown"),
            RebootStatus::NotRecorded => None,
        }
    }

    pub fn is_required(&self) -> bool {
        matches!(self, RebootStatus::Required { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{InstallReason, SourceId};

    fn kernel_pkg(name: &str, version: &str) -> Package {
        Package {
            name: name.to_string(),
            version: version.to_string(),
            source_id: SourceId::pacman(),
            install_reason: InstallReason::Explicit,
            size_bytes: None,
            description: None,
            depends_on: Vec::new(),
            required_by: Vec::new(),
            optional_deps: Vec::new(),
            provides: Vec::new(),
            runtime: false,
            foreign: false,
            signed: true,
            packager: None,
        }
    }

    fn running(release: &str) -> RunningKernel {
        RunningKernel {
            release: release.to_string(),
            modules_present: true,
        }
    }

    #[test]
    fn flavour_is_the_trailing_non_numeric_segments() {
        assert_eq!(flavour("7.2.2-1-cachyos"), "-cachyos");
        assert_eq!(flavour("6.18.48-1-cachyos-lts"), "-cachyos-lts");
        assert_eq!(flavour("6.12.48-1-lts"), "-lts");
        assert_eq!(flavour("6.16.8-zen1-1-zen"), "-zen");
        // Mainline carries no flavour: its package is plain `linux`.
        assert_eq!(flavour("6.16.8-arch1-1"), "");
    }

    #[test]
    fn uname_form_matches_what_uname_actually_prints() {
        // Captured from a real machine: linux-cachyos 7.2.2-1 boots as
        // 7.2.2-1-cachyos.
        assert_eq!(
            uname_form("linux-cachyos", "7.2.2-1").as_deref(),
            Some("7.2.2-1-cachyos")
        );
        assert_eq!(
            uname_form("linux-cachyos-lts", "6.18.48-1").as_deref(),
            Some("6.18.48-1-cachyos-lts")
        );
        // Mainline: pacman's dot before the letters is uname's dash.
        assert_eq!(
            uname_form("linux", "6.16.8.arch1-1").as_deref(),
            Some("6.16.8-arch1-1")
        );
        assert_eq!(
            uname_form("linux-zen", "6.16.8.zen1-1").as_deref(),
            Some("6.16.8-zen1-1-zen")
        );
        assert_eq!(
            uname_form("linux-lts", "6.12.48-1").as_deref(),
            Some("6.12.48-1-lts")
        );
        // An epoch belongs to pacman's ordering, not to a release string.
        assert_eq!(
            uname_form("linux-lts", "1:6.12.48-1").as_deref(),
            Some("6.12.48-1-lts")
        );
        assert_eq!(uname_form("firefox", "155.0-1"), None);
    }

    #[test]
    fn running_what_is_installed_needs_no_reboot() {
        let pkgs = vec![kernel_pkg("linux-cachyos", "7.2.2-1")];
        assert_eq!(
            reboot_status(Some(&running("7.2.2-1-cachyos")), &pkgs),
            RebootStatus::UpToDate {
                release: "7.2.2-1-cachyos".to_string()
            }
        );
    }

    #[test]
    fn an_upgraded_kernel_asks_for_a_reboot() {
        let pkgs = vec![kernel_pkg("linux-cachyos", "7.2.3-1")];
        match reboot_status(Some(&running("7.2.2-1-cachyos")), &pkgs) {
            RebootStatus::Required {
                running,
                installed,
                package,
                modules_gone,
            } => {
                assert_eq!(running, "7.2.2-1-cachyos");
                assert_eq!(installed, "7.2.3-1-cachyos");
                assert_eq!(package, "linux-cachyos");
                assert!(!modules_gone, "the modules dir was reported present");
            }
            other => panic!("expected Required, got {other:?}"),
        }
    }

    #[test]
    fn the_booted_flavour_decides_which_kernel_is_compared() {
        // Both installed, the lts one booted: the -lts package is the one
        // that has to match, and the newer plain kernel is not the question.
        let pkgs = vec![
            kernel_pkg("linux-cachyos", "7.2.2-1"),
            kernel_pkg("linux-cachyos-lts", "6.18.48-1"),
        ];
        assert_eq!(
            reboot_status(Some(&running("6.18.48-1-cachyos-lts")), &pkgs),
            RebootStatus::UpToDate {
                release: "6.18.48-1-cachyos-lts".to_string()
            }
        );
    }

    #[test]
    fn missing_modules_are_the_stronger_case() {
        let pkgs = vec![kernel_pkg("linux", "6.16.9.arch1-1")];
        let kernel = RunningKernel {
            release: "6.16.8-arch1-1".to_string(),
            modules_present: false,
        };
        match reboot_status(Some(&kernel), &pkgs) {
            RebootStatus::Required { modules_gone, .. } => assert!(modules_gone),
            other => panic!("expected Required, got {other:?}"),
        }
    }

    #[test]
    fn only_a_finding_gets_a_sentence() {
        assert_eq!(
            RebootStatus::UpToDate {
                release: "7.2.2-1-cachyos".to_string()
            }
            .note(),
            None,
            "a healthy machine gets no line"
        );
        let required = RebootStatus::Required {
            running: "7.2.2-1-cachyos".to_string(),
            installed: "7.2.3-1-cachyos".to_string(),
            package: "linux-cachyos".to_string(),
            modules_gone: false,
        };
        let note = required.note().expect("a finding says something");
        assert!(note.contains("7.2.2-1-cachyos"), "{note}");
        assert!(note.contains("7.2.3-1-cachyos"), "{note}");
        assert!(required.is_required());
        // The pane's version is short enough to survive its width.
        assert_eq!(required.label(), Some("required"));
        assert!(
            RebootStatus::UpToDate {
                release: "x".to_string()
            }
            .label()
            .is_none()
        );

        let gone = RebootStatus::Required {
            running: "7.2.2-1-cachyos".to_string(),
            installed: "7.2.3-1-cachyos".to_string(),
            package: "linux-cachyos".to_string(),
            modules_gone: true,
        };
        assert!(
            gone.note().is_some_and(|n| n.contains("modules are gone")),
            "the stronger case must say so"
        );
        assert_eq!(gone.label(), Some("required · modules gone"));
    }

    #[test]
    fn an_unidentifiable_kernel_says_so_rather_than_guessing() {
        // A custom kernel nothing owns must never read as "reboot required".
        let pkgs = vec![kernel_pkg("linux-cachyos", "7.2.2-1")];
        match reboot_status(Some(&running("7.3.0-custom")), &pkgs) {
            RebootStatus::Unknown { reason } => assert!(
                reason.contains("linux-custom"),
                "reason must name what it looked for: {reason}"
            ),
            other => panic!("expected Unknown, got {other:?}"),
        }
        // A scan that recorded no kernel is not a finding about the machine:
        // it says nothing rather than "unknown" on every dashboard.
        assert_eq!(reboot_status(None, &pkgs), RebootStatus::NotRecorded);
        assert_eq!(RebootStatus::NotRecorded.label(), None);
        assert_eq!(RebootStatus::NotRecorded.note(), None);
    }
}
