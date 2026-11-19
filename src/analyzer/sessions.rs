//! Update sessions read back from paclens's own update log (#79).
//!
//! The update log (design §11) records, per session, a start line, one
//! outcome line per step, and a closing line — commands and exit codes only
//! (§13, 2026-07-08). That is enough to say *that* a step failed, never
//! *what* failed: no package is named here and none is guessed. A history
//! row gets the step's exit code only when the session's whole window
//! contains that row and no other (§13, 2026-11-20).
//!
//! Pure: text in, sessions out.

use chrono::{DateTime, FixedOffset};

use crate::analyzer::history::{self, Run};

/// How one step ended, as the log recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Exit(i32),
    Signal,
    Launch,
    Skipped,
    /// `running …` with no outcome line after it: cancelled, crashed, or the
    /// log was cut. Never read as success.
    Unrecorded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub label: String,
    pub outcome: Outcome,
}

impl Step {
    /// The words for a step worth showing; `None` for a clean or skipped one.
    pub fn failure(&self) -> Option<String> {
        let label = &self.label;
        match self.outcome {
            Outcome::Exit(0) | Outcome::Skipped => None,
            Outcome::Exit(code) => Some(format!("{label} exited {code}")),
            Outcome::Signal => Some(format!("{label} terminated by signal")),
            Outcome::Launch => Some(format!("{label} failed to launch")),
            Outcome::Unrecorded => Some(format!("{label}: no exit recorded")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub started: DateTime<FixedOffset>,
    /// The `update session complete` stamp; `None` when the log never closed
    /// this session.
    pub ended: Option<DateTime<FixedOffset>>,
    /// Another session started while this one was open. The log carries no
    /// session id, so whose step lines are whose cannot be told.
    pub overlapped: bool,
    pub steps: Vec<Step>,
}

impl Session {
    /// A session whose step lines can be trusted: closed, and alone.
    pub fn trusted(&self) -> bool {
        self.ended.is_some() && !self.overlapped
    }

    pub fn failures(&self) -> Vec<String> {
        self.steps.iter().filter_map(Step::failure).collect()
    }

    /// Worth a line in history: a trusted session with a failed step, or a
    /// session whose outcome cannot be known.
    pub fn notable(&self) -> bool {
        !self.trusted() || !self.failures().is_empty()
    }
}

/// `[2026-11-18T19:06:42Z INFO] msg` → (stamp, msg).
fn split(line: &str) -> Option<(DateTime<FixedOffset>, &str)> {
    let rest = line.strip_prefix('[')?;
    let (head, msg) = rest.split_once("] ")?;
    let stamp = head.split_whitespace().next()?;
    Some((DateTime::parse_from_rfc3339(stamp).ok()?, msg))
}

fn outcome(rest: &str) -> Option<Outcome> {
    if rest == "completed, exit 0" {
        return Some(Outcome::Exit(0));
    }
    if let Some(code) = rest.strip_prefix("failed, exit ") {
        return code.parse().ok().map(Outcome::Exit);
    }
    if rest == "terminated by signal" {
        return Some(Outcome::Signal);
    }
    if rest.starts_with("failed to launch") {
        return Some(Outcome::Launch);
    }
    if rest.starts_with("skipped") {
        return Some(Outcome::Skipped);
    }
    None
}

/// Parse one update log file into sessions, oldest first. Lines it does not
/// know are skipped.
///
/// One file at a time: a session writes only to the file it opened, so two
/// sessions can share lines only inside one file. Within it, every session
/// that starts while another start is still unmatched by a `complete` is
/// overlapped, until the markers balance. A session that never closed (a
/// crash) therefore distrusts the rest of that day's file. That is Unknown,
/// not a guess: the log has no session id to tell a crash from a second
/// process.
pub fn parse(text: &str) -> Vec<Session> {
    let mut done: Vec<Session> = Vec::new();
    let mut open: Option<Session> = None;
    let mut unmatched = 0usize;
    for line in text.lines() {
        let Some((at, msg)) = split(line) else {
            continue;
        };
        if msg == "update session started" {
            if let Some(mut prev) = open.take() {
                prev.overlapped = true;
                done.push(prev);
            }
            open = Some(Session {
                started: at,
                ended: None,
                overlapped: unmatched > 0,
                steps: Vec::new(),
            });
            unmatched += 1;
            continue;
        }
        if msg.starts_with("update session complete") {
            unmatched = unmatched.saturating_sub(1);
            if let Some(mut session) = open.take() {
                session.ended = Some(at);
                done.push(session);
            }
            continue;
        }
        let Some(session) = open.as_mut() else {
            continue;
        };
        if msg.starts_with("sources: ") {
            continue;
        }
        let Some((label, rest)) = msg.split_once(": ") else {
            continue;
        };
        match outcome(rest) {
            Some(o) => {
                match session
                    .steps
                    .iter_mut()
                    .rev()
                    .find(|s| s.label == label && s.outcome == Outcome::Unrecorded)
                {
                    Some(step) => step.outcome = o,
                    None => session.steps.push(Step {
                        label: label.to_string(),
                        outcome: o,
                    }),
                }
            }
            None => session.steps.push(Step {
                label: label.to_string(),
                outcome: Outcome::Unrecorded,
            }),
        }
    }
    done.extend(open);
    done
}

/// Where a notable session's line goes.
#[derive(Debug, PartialEq, Eq)]
pub enum Placement {
    /// Index into the runs: this run lies wholly inside the session's window
    /// and no other run touches it. The link is inferred.
    Run(usize),
    /// Its own line: nothing claimed about any row.
    Alone,
}

/// Match a trusted session to the one run its whole window holds. Anything
/// else — no run, two runs, a run that spills past either end (a split or
/// grouped run) — is `Alone`.
pub fn place(session: &Session, runs: &[Run<'_>]) -> Placement {
    let Some(ended) = session.ended.filter(|_| session.trusted()) else {
        return Placement::Alone;
    };
    // A run's newest transaction may never have completed; its last event
    // is then the latest thing alpm logged for it.
    let span = |run: &Run<'_>| {
        let newest = run.first()?;
        let to = newest
            .completed
            .or(newest.events.last().map(|e| e.at))
            .unwrap_or(newest.started);
        Some((history::run_started(run)?, to))
    };
    let mut touching = runs
        .iter()
        .enumerate()
        .filter_map(|(i, r)| span(r).map(|s| (i, s)))
        .filter(|(_, (from, to))| *from <= ended && *to >= session.started);
    match (touching.next(), touching.next()) {
        (Some((i, (from, to))), None) if from >= session.started && to <= ended => {
            Placement::Run(i)
        }
        _ => Placement::Alone,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A real session from this machine (2026-11-18): paru exited 1 after
    /// installing three of its packages.
    const REAL: &str = include_str!("../../tests/fixtures/sessions/aur-failed.update.log");
    const REAL_PACMAN: &str = include_str!("../../tests/fixtures/sessions/aur-failed.pacman.log");

    fn at(s: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(s).unwrap()
    }

    #[test]
    fn the_real_failing_session_parses_with_every_step() {
        let sessions = parse(REAL);
        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert!(s.trusted());
        assert_eq!(s.started, at("2026-11-18T19:06:42Z"));
        assert_eq!(s.ended, Some(at("2026-11-18T19:21:18Z")));
        assert_eq!(s.steps.len(), 9);
        assert_eq!(s.failures(), vec!["aur exited 1".to_string()]);
    }

    #[test]
    fn the_real_session_lands_on_the_run_it_holds() {
        let sessions = parse(REAL);
        let txs = history::parse(REAL_PACMAN);
        let runs = history::runs(&txs);
        let i = match place(&sessions[0], &runs) {
            Placement::Run(i) => i,
            Placement::Alone => panic!("did not attach"),
        };
        // The run inside the window: pacman -Syu at 00:38 local plus the
        // three AUR installs, not the 01:23 one after it.
        assert_eq!(
            history::run_started(runs[i]),
            Some(at("2026-11-19T00:38:25+05:30"))
        );
        assert_eq!(runs[i].len(), 5);
    }

    #[test]
    fn a_clean_session_is_not_notable() {
        let log = "\
[2026-11-03T08:49:08Z INFO] update session started
[2026-11-03T08:49:08Z INFO] sources: [pacman]
[2026-11-03T08:49:13Z INFO] pacman: running sudo pacman -Syu
[2026-11-03T08:49:13Z INFO] pacman: completed, exit 0
[2026-11-03T08:49:13Z INFO] aur: skipped — no AUR helper found
[2026-11-03T08:49:13Z INFO] update session complete: all sources succeeded
";
        let s = parse(log);
        assert_eq!(s.len(), 1);
        assert!(!s[0].notable(), "{s:?}");
    }

    #[test]
    fn a_truncated_session_is_not_trusted_and_nothing_reads_as_success() {
        let log = "\
[2026-11-03T08:49:08Z INFO] update session started
[2026-11-03T08:49:08Z INFO] sources: [pacman, aur]
[2026-11-03T08:49:09Z INFO] pacman: running sudo pacman -Syu
";
        let s = parse(log);
        assert_eq!(s[0].ended, None);
        assert!(!s[0].trusted());
        assert!(s[0].notable());
        assert_eq!(place(&s[0], &[]), Placement::Alone);
    }

    #[test]
    fn a_tui_step_with_no_status_is_unrecorded_not_succeeded() {
        // The TUI logs every `running` line up front and an outcome only for
        // steps the status file recorded; a cancelled run leaves the rest bare.
        let log = "\
[2026-11-05T10:00:00Z INFO] update session started
[2026-11-05T10:00:00Z INFO] sources: [pacman, aur]
[2026-11-05T10:00:00Z INFO] pacman: running sudo pacman -Syu
[2026-11-05T10:00:00Z INFO] aur: running paru -Sua
[2026-11-05T10:03:00Z INFO] pacman: completed, exit 0
[2026-11-05T10:03:00Z INFO] update session complete: all sources succeeded
";
        let s = &parse(log)[0];
        assert!(s.trusted());
        assert_eq!(s.failures(), vec!["aur: no exit recorded".to_string()]);
    }

    #[test]
    fn interleaved_sessions_are_both_distrusted() {
        // Real shape (2026-11-03 08:52): two starts, then two completes.
        let log = "\
[2026-11-03T08:52:26Z INFO] update session started
[2026-11-03T08:52:26Z INFO] update session started
[2026-11-03T08:52:27Z INFO] aur: running paru -Sua
[2026-11-03T08:52:30Z INFO] aur: failed, exit 1
[2026-11-03T08:52:30Z INFO] update session complete: 1 of 1 sources failed
[2026-11-03T08:52:31Z INFO] update session complete: all sources succeeded
";
        let s = parse(log);
        assert_eq!(s.len(), 2);
        assert!(s.iter().all(|x| !x.trusted()), "{s:?}");
    }

    #[test]
    fn a_session_after_an_unbalanced_pair_stays_distrusted() {
        // A and B start; A's complete closes B early; B's failure is logged
        // after C starts. Without a count of unmatched starts, C would be
        // trusted and take B's failure.
        let log = "\
[2026-11-03T10:00:00Z INFO] update session started
[2026-11-03T10:01:00Z INFO] update session started
[2026-11-03T10:02:00Z INFO] update session complete: all sources succeeded
[2026-11-03T10:03:00Z INFO] update session started
[2026-11-03T10:04:00Z INFO] aur: failed, exit 1
[2026-11-03T10:05:00Z INFO] update session complete: all sources succeeded
[2026-11-03T10:06:00Z INFO] update session complete: 1 of 1 sources failed
";
        let s = parse(log);
        assert_eq!(s.len(), 3);
        assert!(s.iter().all(|x| !x.trusted()), "{s:?}");
    }

    #[test]
    fn back_to_back_sessions_in_the_same_second_are_both_trusted() {
        // The file is written in order: B's start after A's complete means
        // every A line is above it, whatever the stamps say.
        let log = "\
[2026-11-03T09:00:00Z INFO] update session started
[2026-11-03T09:00:05Z INFO] update session complete: all sources succeeded
[2026-11-03T09:00:05Z INFO] update session started
[2026-11-03T09:00:09Z INFO] update session complete: all sources succeeded
";
        let s = parse(log);
        assert_eq!(s.len(), 2);
        assert!(s.iter().all(Session::trusted), "{s:?}");
    }

    #[test]
    fn every_failure_kind_has_words_and_no_package() {
        let step = |o| Step {
            label: "cargo".into(),
            outcome: o,
        };
        assert_eq!(step(Outcome::Exit(0)).failure(), None);
        assert_eq!(step(Outcome::Skipped).failure(), None);
        assert_eq!(
            step(Outcome::Exit(101)).failure().as_deref(),
            Some("cargo exited 101")
        );
        assert_eq!(
            step(Outcome::Signal).failure().as_deref(),
            Some("cargo terminated by signal")
        );
        assert_eq!(
            step(Outcome::Launch).failure().as_deref(),
            Some("cargo failed to launch")
        );
    }

    const RUN_LOG: &str = "\
[2026-11-05T15:30:00+0530] [ALPM] transaction started
[2026-11-05T15:30:01+0530] [ALPM] upgraded a (1-1 -> 2-1)
[2026-11-05T15:30:02+0530] [ALPM] transaction completed
";

    fn session(from: &str, to: &str) -> Session {
        Session {
            started: at(from),
            ended: Some(at(to)),
            overlapped: false,
            steps: vec![Step {
                label: "aur".into(),
                outcome: Outcome::Exit(1),
            }],
        }
    }

    #[test]
    fn a_run_inside_the_window_attaches() {
        let txs = history::parse(RUN_LOG);
        let runs = history::runs(&txs);
        let s = session("2026-11-05T09:59:00Z", "2026-11-05T10:05:00Z");
        assert_eq!(place(&s, &runs), Placement::Run(0));
    }

    #[test]
    fn a_run_that_spills_past_the_window_does_not_attach() {
        let txs = history::parse(RUN_LOG);
        let runs = history::runs(&txs);
        // Window ends one second into the run.
        let s = session("2026-11-05T09:59:00Z", "2026-11-05T10:00:01Z");
        assert_eq!(place(&s, &runs), Placement::Alone);
    }

    #[test]
    fn a_run_that_starts_before_the_window_does_not_attach() {
        let txs = history::parse(RUN_LOG);
        let runs = history::runs(&txs);
        // Window opens one second after the run began.
        let s = session("2026-11-05T10:00:01Z", "2026-11-05T10:05:00Z");
        assert_eq!(place(&s, &runs), Placement::Alone);
    }

    #[test]
    fn no_run_in_the_window_is_alone() {
        // An AUR-only failure: nothing reached alpm, so no row exists.
        let txs = history::parse(RUN_LOG);
        let runs = history::runs(&txs);
        let s = session("2026-11-05T12:00:00Z", "2026-11-05T12:05:00Z");
        assert_eq!(place(&s, &runs), Placement::Alone);
    }

    #[test]
    fn two_runs_in_the_window_is_alone() {
        let log = "\
[2026-11-05T15:30:00+0530] [ALPM] transaction started
[2026-11-05T15:30:01+0530] [ALPM] upgraded a (1-1 -> 2-1)
[2026-11-05T15:30:02+0530] [ALPM] transaction completed
[2026-11-05T15:50:00+0530] [ALPM] transaction started
[2026-11-05T15:50:01+0530] [ALPM] upgraded b (1-1 -> 2-1)
[2026-11-05T15:50:02+0530] [ALPM] transaction completed
";
        let txs = history::parse(log);
        let runs = history::runs(&txs);
        assert_eq!(runs.len(), 2, "fixture must split into two runs");
        let s = session("2026-11-05T09:59:00Z", "2026-11-05T10:30:00Z");
        assert_eq!(place(&s, &runs), Placement::Alone);
    }

    #[test]
    fn an_interrupted_run_is_measured_to_its_last_event() {
        // No `completed`: the run still reaches 10:20, past the window.
        let log = "\
[2026-11-05T15:30:00+0530] [ALPM] transaction started
[2026-11-05T15:50:00+0530] [ALPM] upgraded a (1-1 -> 2-1)
";
        let txs = history::parse(log);
        let runs = history::runs(&txs);
        assert_eq!(runs.len(), 1, "the open transaction must be kept");
        let s = session("2026-11-05T09:59:00Z", "2026-11-05T10:05:00Z");
        assert_eq!(place(&s, &runs), Placement::Alone);
    }

    #[test]
    fn an_untrusted_session_never_attaches() {
        let txs = history::parse(RUN_LOG);
        let runs = history::runs(&txs);
        let mut s = session("2026-11-05T09:59:00Z", "2026-11-05T10:05:00Z");
        s.overlapped = true;
        assert_eq!(place(&s, &runs), Placement::Alone);
    }
}
