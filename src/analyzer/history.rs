//! What a past upgrade actually changed, from `/var/log/pacman.log` (#8).
//!
//! "What changed in the last update" is the question asked at the moment
//! something breaks, and the answer normally lives in a hundred thousand lines
//! of log. Pure local parsing: no network, no inference, nothing to label —
//! the log either says a package was upgraded or it does not.
//!
//! Pure over a `&str` (the `staleness_with` pattern): the caller reads the
//! file, this reads the text. The log is append-only and can reach tens of
//! megabytes, so callers are expected to hand over the tail rather than the
//! whole thing — [`parse`] deliberately tolerates starting mid-file.
//!
//! alpm only. Flatpak keeps no equivalent log, and a surface that showed an
//! empty Flatpak section would be claiming to have looked.

use chrono::{DateTime, FixedOffset};

/// What happened to one package inside a transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Installed,
    Upgraded,
    Removed,
    Reinstalled,
    /// pacman logs a downgrade as `upgraded` with the versions reversed; the
    /// parser cannot tell them apart without comparing versions, so this is
    /// only produced when the log says `downgraded` itself.
    Downgraded,
}

impl EventKind {
    pub fn label(self) -> &'static str {
        match self {
            EventKind::Installed => "installed",
            EventKind::Upgraded => "upgraded",
            EventKind::Removed => "removed",
            EventKind::Reinstalled => "reinstalled",
            EventKind::Downgraded => "downgraded",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "installed" => EventKind::Installed,
            "upgraded" => EventKind::Upgraded,
            "removed" => EventKind::Removed,
            "reinstalled" => EventKind::Reinstalled,
            "downgraded" => EventKind::Downgraded,
            _ => return None,
        })
    }
}

/// One package's line in the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageEvent {
    pub at: DateTime<FixedOffset>,
    pub kind: EventKind,
    pub name: String,
    /// The version before, for an upgrade; `None` for an install.
    pub from: Option<String>,
    /// The version after — for a removal, the version that went away.
    pub to: Option<String>,
}

/// One `transaction started` … `transaction completed` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transaction {
    pub started: DateTime<FixedOffset>,
    /// `None` when the log ends mid-transaction, or the transaction was
    /// interrupted — which is exactly the run worth looking at after a
    /// machine died mid-upgrade, so it is kept rather than dropped.
    pub completed: Option<DateTime<FixedOffset>>,
    pub events: Vec<PackageEvent>,
}

impl Transaction {
    /// Counts by kind, for a one-line summary: (installed, upgraded, removed).
    /// Reinstalls count as installs and downgrades as upgrades — the summary
    /// answers "how much moved", and the detail is a keystroke away.
    pub fn counts(&self) -> (usize, usize, usize) {
        let mut counts = (0, 0, 0);
        for e in &self.events {
            match e.kind {
                EventKind::Installed | EventKind::Reinstalled => counts.0 += 1,
                EventKind::Upgraded | EventKind::Downgraded => counts.1 += 1,
                EventKind::Removed => counts.2 += 1,
            }
        }
        counts
    }
}

/// The timestamp and the rest of a log line: `[2026-09-04T17:12:34+0530] …`.
///
/// The offset form pacman writes has no colon, which `%z` accepts and RFC 3339
/// does not — parsing it as the latter would reject every line on a machine
/// that is not on UTC.
///
/// The offset is **kept** rather than normalised to UTC. pacman logs the wall
/// clock the upgrade happened on, and that is the number the reader
/// remembers: converting an upgrade logged at 17:12+0530 into 11:42Z renders
/// a history nobody recognises. Ordering is unaffected — these compare by
/// instant.
fn split_line(line: &str) -> Option<(DateTime<FixedOffset>, &str)> {
    let rest = line.strip_prefix('[')?;
    let (stamp, rest) = rest.split_once("] ")?;
    let at = DateTime::parse_from_str(stamp, "%Y-%m-%dT%H:%M:%S%z").ok()?;
    Some((at, rest))
}

/// One `[ALPM] upgraded name (a -> b)` line, or `None` for the many lines
/// that are hooks, scriptlets, and pacman's own chatter.
fn parse_event(at: DateTime<FixedOffset>, rest: &str) -> Option<PackageEvent> {
    let rest = rest.strip_prefix("[ALPM] ")?;
    let (word, rest) = rest.split_once(' ')?;
    let kind = EventKind::parse(word)?;
    let (name, versions) = rest.split_once(" (")?;
    let versions = versions.strip_suffix(')')?;
    let (from, to) = match versions.split_once(" -> ") {
        Some((from, to)) => (Some(from.to_string()), Some(to.to_string())),
        None => (None, Some(versions.to_string())),
    };
    Some(PackageEvent {
        at,
        kind,
        name: name.to_string(),
        from,
        to,
    })
}

/// Parse transactions out of a log, newest first.
///
/// Events outside any transaction are dropped: pacman writes package lines
/// only inside one, so a line without a `transaction started` above it means
/// the text began mid-transaction — which is the normal case when the caller
/// hands over a tail.
pub fn parse(log: &str) -> Vec<Transaction> {
    let mut out: Vec<Transaction> = Vec::new();
    let mut current: Option<Transaction> = None;
    for line in log.lines() {
        let Some((at, rest)) = split_line(line) else {
            continue;
        };
        match rest {
            "[ALPM] transaction started" => {
                // A started block with no completed line is kept: an upgrade
                // that died halfway is the one worth seeing.
                if let Some(open) = current.take() {
                    out.push(open);
                }
                current = Some(Transaction {
                    started: at,
                    completed: None,
                    events: Vec::new(),
                });
            }
            "[ALPM] transaction completed" => {
                if let Some(mut open) = current.take() {
                    open.completed = Some(at);
                    out.push(open);
                }
            }
            _ => {
                if let (Some(tx), Some(event)) = (current.as_mut(), parse_event(at, rest)) {
                    tx.events.push(event);
                }
            }
        }
    }
    if let Some(open) = current.take() {
        out.push(open);
    }
    // Empty transactions are pacman's own bookkeeping (a `-Sy` that changed
    // nothing); they carry no answer to "what changed".
    out.retain(|tx| !tx.events.is_empty());
    out.reverse();
    out
}

/// Every event for one package, newest first — the answer to "when did this
/// arrive, and how often has it moved since".
pub fn package_history<'a>(transactions: &'a [Transaction], name: &str) -> Vec<&'a PackageEvent> {
    transactions
        .iter()
        .flat_map(|tx| tx.events.iter())
        .filter(|e| e.name == name)
        .collect()
}

/// A one-line summary of a package's history: when it arrived, how many times
/// it has moved, and when it last did. `None` when the log knows nothing
/// about it — which a tail routinely will, and which is not the same as "never
/// installed".
pub fn package_summary(transactions: &[Transaction], name: &str) -> Option<String> {
    let events = package_history(transactions, name);
    let (first, last) = (events.last()?, events.first()?);
    let upgrades = events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::Upgraded | EventKind::Downgraded))
        .count();
    let arrived = format!("{} {}", first.kind.label(), first.at.format("%Y-%m-%d"));
    Some(match upgrades {
        0 => arrived,
        1 => format!(
            "{arrived}, upgraded once, last {}",
            last.at.format("%Y-%m-%d")
        ),
        n => format!(
            "{arrived}, upgraded {n} times, last {}",
            last.at.format("%Y-%m-%d")
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from a real machine's `/var/log/pacman.log`.
    const RECENT: &str = include_str!("../../tests/fixtures/pacman-log/recent.log");
    const REMOVAL: &str = include_str!("../../tests/fixtures/pacman-log/removal.log");

    #[test]
    fn a_real_log_tail_parses_into_transactions_newest_first() {
        let txs = parse(RECENT);
        assert!(
            txs.len() >= 2,
            "expected several transactions, got {}",
            txs.len()
        );
        assert!(
            txs[0].started >= txs[1].started,
            "transactions must come newest first"
        );
        assert!(
            txs.iter().all(|t| !t.events.is_empty()),
            "an empty transaction answers nothing and should be dropped"
        );
    }

    #[test]
    fn upgrade_lines_carry_both_versions() {
        let txs = parse(RECENT);
        let event = txs
            .iter()
            .flat_map(|t| t.events.iter())
            .find(|e| e.kind == EventKind::Upgraded)
            .expect("the fixture has an upgrade");
        assert!(event.from.is_some(), "no old version: {event:?}");
        assert!(event.to.is_some(), "no new version: {event:?}");
        assert_ne!(event.from, event.to);
    }

    #[test]
    fn installs_and_removals_are_told_apart() {
        let txs = parse(REMOVAL);
        let kinds: Vec<EventKind> = txs
            .iter()
            .flat_map(|t| t.events.iter())
            .map(|e| e.kind)
            .collect();
        assert!(kinds.contains(&EventKind::Removed), "fixture has a removal");
        let removed = txs
            .iter()
            .flat_map(|t| t.events.iter())
            .find(|e| e.kind == EventKind::Removed)
            .expect("a removal");
        // A removal logs the version that went away, and nothing it became.
        assert!(removed.to.is_some());
        assert!(removed.from.is_none());
    }

    #[test]
    fn the_hand_written_shapes_parse_exactly() {
        let log = "\
[2026-09-04T17:12:32+0530] [ALPM] transaction started
[2026-09-04T17:12:34+0530] [ALPM] upgraded perf (7.2.2-1 -> 7.2.3-1)
[2026-09-04T17:12:34+0530] [ALPM] installed matugen (4.2.0-1.1)
[2026-09-04T17:12:34+0530] [ALPM] removed gone (1.0-1)
[2026-09-04T17:12:35+0530] [ALPM] transaction completed
";
        let txs = parse(log);
        assert_eq!(txs.len(), 1);
        let tx = &txs[0];
        assert!(tx.completed.is_some());
        assert_eq!(tx.counts(), (1, 1, 1));
        assert_eq!(tx.events[0].name, "perf");
        assert_eq!(tx.events[0].from.as_deref(), Some("7.2.2-1"));
        assert_eq!(tx.events[0].to.as_deref(), Some("7.2.3-1"));
        assert_eq!(tx.events[1].to.as_deref(), Some("4.2.0-1.1"));
        assert_eq!(tx.events[1].from, None);
    }

    #[test]
    fn hooks_scriptlets_and_pacman_chatter_are_not_events() {
        let log = "\
[2026-09-04T11:49:47+0530] [ALPM] transaction started
[2026-09-04T11:49:47+0530] [ALPM-SCRIPTLET] ==> root: 451
[2026-09-04T11:49:47+0530] [ALPM] running '10-limine-snapper-lock.hook'...
[2026-09-04T11:49:47+0530] [PACMAN] Running 'pacman -Syu'
[2026-09-04T11:49:48+0530] [ALPM] installed real (1.0-1)
[2026-09-04T11:49:48+0530] [ALPM] transaction completed
";
        let txs = parse(log);
        assert_eq!(txs.len(), 1);
        assert_eq!(txs[0].events.len(), 1, "only the package line is an event");
        assert_eq!(txs[0].events[0].name, "real");
    }

    #[test]
    fn a_truncated_log_keeps_what_it_can() {
        // Starting mid-transaction is normal when reading a tail: the events
        // above the first `started` have no transaction and are dropped.
        let log = "\
[2026-09-04T11:49:47+0530] [ALPM] upgraded orphaned (1.0-1 -> 1.0-2)
[2026-09-04T11:49:47+0530] [ALPM] transaction started
[2026-09-04T11:49:48+0530] [ALPM] installed kept (1.0-1)
";
        let txs = parse(log);
        assert_eq!(txs.len(), 1);
        assert_eq!(txs[0].events[0].name, "kept");
        assert_eq!(
            txs[0].completed, None,
            "an interrupted upgrade is the one worth seeing, not one to drop"
        );
    }

    #[test]
    fn malformed_lines_are_skipped_rather_than_guessed_at() {
        let log = "\
not a log line at all
[garbage] [ALPM] installed x (1.0-1)
[2026-09-04T11:49:47+0530] [ALPM] transaction started
[2026-09-04T11:49:47+0530] [ALPM] upgraded broken (no arrow here)
[2026-09-04T11:49:47+0530] [ALPM] upgraded truncated (
[2026-09-04T11:49:48+0530] [ALPM] installed fine (1.0-1)
";
        let txs = parse(log);
        assert_eq!(txs.len(), 1);
        let names: Vec<&str> = txs[0].events.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["broken", "fine"]);
        // "broken" parses as an install-shaped line with one version string:
        // that is what the log says, and inventing an arrow would be worse.
        assert_eq!(txs[0].events[0].to.as_deref(), Some("no arrow here"));
    }

    #[test]
    fn timestamps_keep_the_clock_the_log_wrote() {
        // Found on real data: normalising to UTC turned an upgrade logged at
        // 17:12 into 11:42, which is not the machine's own history.
        let log = "\
[2026-09-04T17:12:32+0530] [ALPM] transaction started
[2026-09-04T17:12:34+0530] [ALPM] upgraded perf (7.2.2-1 -> 7.2.3-1)
[2026-09-04T17:12:35+0530] [ALPM] transaction completed
";
        let txs = parse(log);
        assert_eq!(txs[0].started.format("%H:%M").to_string(), "17:12");
        assert_eq!(
            txs[0].events[0].at.format("%Y-%m-%d %H:%M").to_string(),
            "2026-09-04 17:12"
        );
    }

    #[test]
    fn a_packages_history_reads_as_a_sentence() {
        let log = "\
[2026-03-14T10:00:00+0000] [ALPM] transaction started
[2026-03-14T10:00:01+0000] [ALPM] installed firefox (120.0-1)
[2026-03-14T10:00:02+0000] [ALPM] transaction completed
[2026-05-02T10:00:00+0000] [ALPM] transaction started
[2026-05-02T10:00:01+0000] [ALPM] upgraded firefox (120.0-1 -> 121.0-1)
[2026-05-02T10:00:02+0000] [ALPM] transaction completed
[2026-07-02T10:00:00+0000] [ALPM] transaction started
[2026-07-02T10:00:01+0000] [ALPM] upgraded firefox (121.0-1 -> 122.0-1)
[2026-07-02T10:00:02+0000] [ALPM] transaction completed
";
        let txs = parse(log);
        let summary = package_summary(&txs, "firefox").expect("history");
        assert!(summary.contains("installed 2026-03-14"), "{summary}");
        assert!(summary.contains("upgraded 2 times"), "{summary}");
        assert!(summary.contains("last 2026-07-02"), "{summary}");
        assert_eq!(package_history(&txs, "firefox").len(), 3);
        // A package the log does not mention is unknown, not "never installed".
        assert_eq!(package_summary(&txs, "nothing"), None);
    }
}
