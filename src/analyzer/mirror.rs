//! Whether the mirror pacman asks first for `core` is behind Arch (#6).
//!
//! The gap between two `lastupdate` markers is measured, so the number is
//! shown as it is. What it is *not* is a partial upgrade: a full upgrade from
//! a coherent older mirror leaves a coherent older system. So the sentence
//! says "behind", and stops there (design §13, 2026-11-05).
//!
//! The marker moves on any change to the master's tree, not only to `core`,
//! and pacman falls back to the next server when the first fails; the gap is
//! about that one server, and the sentences name it.

use crate::providers::mirror::MirrorCheck;

/// Under a day is a mirror between syncs, not a stale one: Arch asks tier 2
/// mirrors to sync at least daily.
pub const DAY: i64 = 86_400;

/// What the check found, for every surface to word the same way (P5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing to say: the check did not run, the reference was unreachable
    /// (offline is silent, like the news), or the gap is under a day.
    Quiet,
    /// The reference answered but this server gave no marker to compare.
    Unknown { host: String },
    /// Whole days, rounded down: never more than was measured.
    Behind { host: String, days: i64 },
}

/// The mirror root a `core` server URL sits under: everything before
/// `core/os/<arch>`. Any other layout is not guessed at.
pub fn base(server: &str) -> Option<&str> {
    let (rest, arch) = server.trim_end_matches('/').rsplit_once('/')?;
    let rest = rest.strip_suffix("/core/os")?;
    if arch.is_empty() {
        return None;
    }
    Some(&server[..=rest.len()])
}

/// The host part of a server URL, or the whole URL when it has none.
pub fn host(server: &str) -> &str {
    server
        .split_once("://")
        .and_then(|(_, r)| r.split('/').next())
        .filter(|h| !h.is_empty())
        .unwrap_or(server)
}

pub fn verdict(check: Option<&MirrorCheck>) -> Verdict {
    let Some(c) = check else {
        return Verdict::Quiet;
    };
    let Some(reference) = c.reference else {
        return Verdict::Quiet;
    };
    let host = host(&c.server).to_string();
    match c.mirror {
        None => Verdict::Unknown { host },
        // A mirror ahead of the reference is a clock or a sampling gap,
        // never a negative lag.
        Some(m) if reference - m >= DAY => Verdict::Behind {
            host,
            days: (reference - m) / DAY,
        },
        Some(_) => Verdict::Quiet,
    }
}

fn days(n: i64) -> String {
    format!("{n} day{}", if n == 1 { "" } else { "s" })
}

impl Verdict {
    /// The `status` line.
    pub fn note(&self) -> Option<String> {
        match self {
            Verdict::Quiet => None,
            Verdict::Unknown { host } => Some(format!(
                "core mirror {host}: no lastupdate to compare, staleness unknown"
            )),
            Verdict::Behind { host, days: n } => {
                Some(format!("core mirror {host} is {} behind Arch", days(*n)))
            }
        }
    }

    /// What to run about it: prints a fresher list, writes nothing.
    pub fn hint(&self) -> Option<&'static str> {
        matches!(self, Verdict::Behind { .. })
            .then_some("a fresher list, printed only: reflector --latest 20 --sort rate")
    }

    /// The dashboard row. Only a finding gets one; `unknown` stays in
    /// `status`, or a mirror without the file would hold a row for ever.
    pub fn label(&self) -> Option<String> {
        match self {
            Verdict::Behind { days: n, .. } => Some(format!("{} behind", days(*n))),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER: &str = "https://archlinux.cachyos.org/repo/core/os/x86_64";
    const NOW: i64 = 1_793_902_445;

    fn check(mirror: Option<i64>, reference: Option<i64>) -> MirrorCheck {
        MirrorCheck {
            server: SERVER.into(),
            mirror,
            reference,
        }
    }

    #[test]
    fn base_strips_core_os_arch() {
        assert_eq!(base(SERVER), Some("https://archlinux.cachyos.org/repo/"));
        assert_eq!(
            base("https://geo.mirror.pkgbuild.com/core/os/x86_64/"),
            Some("https://geo.mirror.pkgbuild.com/")
        );
        assert_eq!(
            base("file:///srv/mirror/core/os/x86_64"),
            Some("file:///srv/mirror/")
        );
    }

    #[test]
    fn any_other_layout_has_no_base() {
        for s in [
            "https://x.org/arch/",
            "https://x.org/core/os/",
            "https://x.org/extra/os/x86_64",
            "https://x.org/mycore/os/x86_64",
            "",
        ] {
            assert_eq!(base(s), None, "{s:?}");
        }
    }

    #[test]
    fn host_is_the_authority() {
        assert_eq!(host(SERVER), "archlinux.cachyos.org");
        assert_eq!(
            host("file:///srv/m/core/os/x86_64"),
            "file:///srv/m/core/os/x86_64"
        );
        assert_eq!(host("weird"), "weird");
    }

    #[test]
    fn no_check_or_offline_is_quiet() {
        assert_eq!(verdict(None), Verdict::Quiet);
        assert_eq!(verdict(Some(&check(None, None))), Verdict::Quiet);
        assert_eq!(
            verdict(Some(&check(Some(NOW - 9 * DAY), None))),
            Verdict::Quiet
        );
    }

    #[test]
    fn reference_without_mirror_is_unknown() {
        let v = verdict(Some(&check(None, Some(NOW))));
        assert_eq!(
            v,
            Verdict::Unknown {
                host: "archlinux.cachyos.org".into()
            }
        );
        assert_eq!(
            v.note().unwrap(),
            "core mirror archlinux.cachyos.org: no lastupdate to compare, staleness unknown"
        );
        assert_eq!(v.label(), None);
        assert_eq!(v.hint(), None);
    }

    #[test]
    fn under_a_day_or_ahead_is_quiet() {
        for m in [NOW, NOW - 1578, NOW - DAY + 1, NOW + 5 * DAY] {
            assert_eq!(
                verdict(Some(&check(Some(m), Some(NOW)))),
                Verdict::Quiet,
                "{m}"
            );
        }
    }

    #[test]
    fn a_day_or_more_is_behind_in_whole_days_rounded_down() {
        let at = |gap| verdict(Some(&check(Some(NOW - gap), Some(NOW))));
        assert_eq!(at(DAY).label().unwrap(), "1 day behind");
        assert_eq!(at(2 * DAY - 1).label().unwrap(), "1 day behind");
        assert_eq!(at(3 * DAY + 7).label().unwrap(), "3 days behind");
        assert_eq!(
            at(3 * DAY).note().unwrap(),
            "core mirror archlinux.cachyos.org is 3 days behind Arch"
        );
        assert!(at(DAY).hint().unwrap().contains("reflector"));
    }

    #[test]
    fn sentences_fit_one_80_column_line() {
        // A long real-world host still leaves the line readable.
        let v = Verdict::Behind {
            host: "mirror.sg.cdn-perfprod.com".into(),
            days: 365,
        };
        assert!(v.note().unwrap().len() + 2 <= 80);
        assert!(v.hint().unwrap().len() + 4 <= 80);
        assert!(v.label().unwrap().len() <= 21);
    }
}
