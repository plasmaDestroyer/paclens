//! `paclens history` — what past upgrades actually changed (#8).
//!
//! Reads the tail of `/var/log/pacman.log` and hands the text to the pure
//! parser. Nothing is stored in the scan cache: the log is append-only and
//! can reach tens of megabytes, and a question asked occasionally does not
//! belong in a file written on every scan.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{Context, Result};

use crate::analyzer::history::{self, Transaction};
use crate::analyzer::sessions::{self, Placement, Session};
use crate::cli::style::Styles;

/// Where pacman writes, and how much of it to read.
///
/// 4 MiB is thousands of transactions on a normal machine and a fraction of a
/// second to parse, while a full log on a years-old install can be 50 MB.
/// Reading the tail is what keeps a history question cheap; the parser is
/// built to start mid-file for exactly this reason.
pub const PACMAN_LOG: &str = "/var/log/pacman.log";
pub const TAIL_BYTES: u64 = 4 * 1024 * 1024;

/// Read the last [`TAIL_BYTES`] of a log. The first line is likely cut in
/// half, which the parser drops rather than guesses at.
pub fn read_tail(path: &Path, bytes: u64) -> Result<String> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("failed to read {}", path.display()))?;
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if len > bytes {
        file.seek(SeekFrom::Start(len - bytes))?;
    }
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    // The log is UTF-8 in practice, but a scriptlet can write anything; a
    // lossy read is better than refusing to show the history.
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

pub fn run(package: Option<&str>, limit: usize, styles: &Styles) -> Result<()> {
    let text = read_tail(Path::new(PACMAN_LOG), TAIL_BYTES)?;
    let transactions = history::parse(&text);
    // Only a tail that really cut the log can have cut a session's row off.
    let cut = std::fs::metadata(PACMAN_LOG).is_ok_and(|m| m.len() > TAIL_BYTES);
    let sessions = crate::executor::UpdateLog::default_dir()
        .map(|dir| read_sessions(&dir))
        .unwrap_or_default();
    print!(
        "{}",
        render(&transactions, &sessions, cut, package, limit, styles)
    );
    Ok(())
}

/// paclens's own update logs (design §11), every day's file oldest first,
/// parsed into sessions with their times in local time. A missing or
/// unreadable directory is no sessions: history still shows what alpm did.
fn read_sessions(dir: &Path) -> Vec<Session> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".log") && !n.starts_with("paclens-"))
        .collect();
    names.sort();
    let local =
        |t: chrono::DateTime<chrono::FixedOffset>| t.with_timezone(&chrono::Local).fixed_offset();
    let mut all: Vec<Session> = names
        .iter()
        .filter_map(|n| std::fs::read_to_string(dir.join(n)).ok())
        .flat_map(|text| sessions::parse(&text))
        .map(|mut s| {
            s.started = local(s.started);
            s.ended = s.ended.map(local);
            s
        })
        .collect();
    sessions::mark_crossing(&mut all);
    all
}

/// The line a session gets on its own, claiming no history row.
fn session_line(session: &Session) -> String {
    // Overlap first: in an interleaved group a session's `complete` line may
    // have gone to its partner, so "no end recorded" would be a claim.
    if session.overlapped {
        return "paclens session: outcome Unknown — log interleaved with another session [unknown]"
            .to_string();
    }
    if session.ended.is_none() {
        return "paclens session: outcome Unknown — no end recorded [unknown]".to_string();
    }
    format!(
        "paclens session: {} [confirmed]; no matching pacman history row",
        session.failures().join(", ")
    )
}

/// The whole report. Pure, like the other CLI renderers, so the no-color
/// output is deterministic and testable.
fn render(
    transactions: &[Transaction],
    sessions: &[Session],
    cut: bool,
    package: Option<&str>,
    limit: usize,
    s: &Styles,
) -> String {
    let mut out = String::new();
    if let Some(name) = package {
        return render_package(transactions, name, s);
    }

    // Grouped into runs: an AUR helper installs each package it builds with
    // its own `pacman -U`, so the raw list answers "what did alpm do" where
    // the reader asked "what did I upgrade" (#8).
    let runs = history::runs(transactions);
    let shown = runs.len().min(limit);
    out.push_str(&format!(
        "{}\n",
        if runs.is_empty() {
            s.dim("no transactions in the log tail")
        } else {
            s.summary_ok(&format!("{shown} of {} runs", runs.len()))
        }
    ));

    // paclens's own sessions that failed or cannot be read (#79). When the
    // tail cut the log, one that started before its oldest transaction is
    // left out: part of its window was cut off, so neither "this row" nor "no
    // matching row" can be backed. Only sessions months old, at the tail's
    // edge, are lost; a whole log keeps every one.
    let oldest = transactions.last().map(|tx| tx.started).filter(|_| cut);
    let mut attached: Vec<Vec<String>> = vec![Vec::new(); runs.len()];
    let mut alone: Vec<&Session> = Vec::new();
    for session in sessions
        .iter()
        .filter(|x| x.notable() && oldest.is_none_or(|o| x.started >= o))
    {
        match sessions::place(session, &runs) {
            Placement::Run(i) => attached[i].extend(session.failures()),
            Placement::Alone => alone.push(session),
        }
    }
    // Newest first, like the runs; a lone session prints before the first run
    // older than it.
    alone.sort_by_key(|a| std::cmp::Reverse(a.started));
    let mut alone = alone.into_iter().peekable();
    let lone = |out: &mut String, x: &Session| {
        out.push_str(&format!(
            "  {} {}  {}\n",
            s.bullet(),
            x.started.format("%Y-%m-%d %H:%M"),
            s.warn(&session_line(x))
        ));
    };

    for (i, run) in runs.iter().enumerate().take(limit) {
        let started = history::run_started(run);
        while let Some(x) = alone.next_if(|x| started.is_none_or(|t| x.started > t)) {
            lone(&mut out, x);
        }
        // An interrupted upgrade is the one worth seeing, so it says so
        // rather than being quietly indistinguishable from a clean run.
        let mut notes = Vec::new();
        if run.len() > 1 {
            notes.push(format!("{} transactions", run.len()));
        }
        if run.iter().any(|tx| tx.completed.is_none()) {
            notes.push("did not complete".to_string());
        }
        let note = match notes.is_empty() {
            true => String::new(),
            false => format!(" {}", s.dim(&format!("({})", notes.join(", ")))),
        };
        let when = match history::run_started(run) {
            Some(at) => at.format("%Y-%m-%d %H:%M").to_string(),
            None => String::new(),
        };
        out.push_str(&format!(
            "  {} {}  {}{}\n",
            s.bullet(),
            when,
            history::run_summary(run),
            note
        ));
        for failure in &attached[i] {
            out.push_str(&format!(
                "      {} {}\n",
                s.arrow(),
                s.warn(&format!("{failure} — cause/package Unknown [inferred]"))
            ));
        }
    }
    // Every run shown: the rest of the lone sessions are older than all of them.
    if runs.len() <= limit {
        for x in alone {
            lone(&mut out, x);
        }
    }

    if runs.len() > limit {
        out.push_str(&s.dim(&format!(
            "\n  {} more — paclens history --limit N\n",
            runs.len() - limit
        )));
    }
    out
}

/// One package's history: every line the log has about it, newest first.
fn render_package(transactions: &[Transaction], name: &str, s: &Styles) -> String {
    let events = history::package_history(transactions, name);
    if events.is_empty() {
        return format!(
            "{}\n",
            s.dim(&format!(
                "nothing about {name} in the log tail — it may predate it"
            ))
        );
    }
    let mut out = format!(
        "{}\n",
        s.summary_ok(&format!(
            "{name} {} {}",
            s.bullet(),
            history::package_summary(transactions, name).unwrap_or_default()
        ))
    );
    for e in events {
        let versions = match (&e.from, &e.to) {
            (Some(from), Some(to)) => format!("{from} -> {to}"),
            (_, Some(to)) => to.clone(),
            _ => String::new(),
        };
        out.push_str(&format!(
            "  {} {}  {:<12} {}\n",
            s.bullet(),
            e.at.format("%Y-%m-%d %H:%M"),
            e.kind.label(),
            s.dim(&versions)
        ));
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::config::ColorTheme;

    fn ascii() -> Styles {
        Styles::resolve(true, ColorTheme::Dark, true)
    }

    const LOG: &str = "\
[2026-03-14T10:00:00+0000] [ALPM] transaction started
[2026-03-14T10:00:01+0000] [ALPM] installed firefox (120.0-1)
[2026-03-14T10:00:01+0000] [ALPM] installed extra (1.0-1)
[2026-03-14T10:00:02+0000] [ALPM] transaction completed
[2026-07-02T10:00:00+0000] [ALPM] transaction started
[2026-07-02T10:00:01+0000] [ALPM] upgraded firefox (120.0-1 -> 122.0-1)
[2026-07-02T10:00:01+0000] [ALPM] removed extra (1.0-1)
";

    #[test]
    fn transactions_list_newest_first_with_counts() {
        let txs = crate::analyzer::history::parse(LOG);
        let out = render(&txs, &[], false, None, 10, &ascii());
        assert!(out.contains("2 of 2 runs"), "{out}");
        let newest = out.find("2026-07-02").expect("newest listed");
        let oldest = out.find("2026-03-14").expect("oldest listed");
        assert!(newest < oldest, "not newest-first:\n{out}");
        assert!(out.contains("1 upgraded, 1 removed"), "{out}");
        assert!(out.contains("2 installed"), "{out}");
        // The interrupted transaction says so.
        assert!(out.contains("did not complete"), "{out}");
    }

    #[test]
    fn a_helper_run_lists_as_one_row_that_says_how_many_transactions() {
        let log = "\
[2026-09-07T20:01:51+0530] [ALPM] transaction started
[2026-09-07T20:01:52+0530] [ALPM] upgraded antigravity (2.11.0-1 -> 2.12.2-1)
[2026-09-07T20:01:52+0530] [ALPM] transaction completed
[2026-09-07T20:02:04+0530] [ALPM] transaction started
[2026-09-07T20:02:05+0530] [ALPM] upgraded pikaur (1.33.3-1 -> 1.34-1)
[2026-09-07T20:02:05+0530] [ALPM] transaction completed
";
        let txs = crate::analyzer::history::parse(log);
        let out = render(&txs, &[], false, None, 10, &ascii());
        assert!(out.contains("1 of 1 runs"), "{out}");
        assert!(out.contains("2 upgraded"), "{out}");
        // The grouping is inferred, so the row says what it grouped.
        assert!(out.contains("(2 transactions)"), "{out}");
    }

    #[test]
    fn the_limit_says_what_it_hid() {
        let txs = crate::analyzer::history::parse(LOG);
        let out = render(&txs, &[], false, None, 1, &ascii());
        assert!(out.contains("1 of 2 runs"), "{out}");
        assert!(out.contains("1 more"), "{out}");
        assert!(!out.contains("2026-03-14"), "hid the wrong end:\n{out}");
    }

    #[test]
    fn one_packages_history_lists_every_line_about_it() {
        let txs = crate::analyzer::history::parse(LOG);
        let out = render(&txs, &[], false, Some("firefox"), 10, &ascii());
        assert!(
            out.contains("installed 2026-03-14"),
            "summary missing:\n{out}"
        );
        assert!(out.contains("120.0-1 -> 122.0-1"), "{out}");
        assert!(!out.contains("extra"), "another package leaked in:\n{out}");
    }

    #[test]
    fn a_package_the_log_does_not_mention_says_so() {
        let txs = crate::analyzer::history::parse(LOG);
        let out = render(&txs, &[], false, Some("ghost"), 10, &ascii());
        assert!(out.contains("nothing about ghost"), "{out}");
        // "may predate it" rather than "never installed": a tail is a window,
        // not the whole history.
        assert!(out.contains("predate"), "{out}");
    }

    fn sessions_of(log: &str) -> Vec<Session> {
        sessions::parse(log)
    }

    const REAL_UPDATE: &str = include_str!("../../tests/fixtures/sessions/aur-failed.update.log");
    const REAL_PACMAN: &str = include_str!("../../tests/fixtures/sessions/aur-failed.pacman.log");

    #[test]
    fn a_failed_step_lands_on_the_run_its_session_holds() {
        let txs = crate::analyzer::history::parse(REAL_PACMAN);
        let out = render(&txs, &sessions_of(REAL_UPDATE), false, None, 10, &ascii());
        let row = out.find("2026-11-19 00:38").expect("row listed");
        let note = out
            .find("aur exited 1 — cause/package Unknown [inferred]")
            .expect("note missing");
        let next = out.find("2026-11-19 01:23").expect("later row listed");
        assert!(next < row && row < note, "note not under its row:\n{out}");
        // No package is named: paru's own words were never kept.
        assert!(!out.contains("brave"), "{out}");
    }

    #[test]
    fn a_failure_with_no_row_gets_its_own_line() {
        // An AUR-only failure: the build died, nothing reached alpm.
        let log = "\
[2026-07-02T12:00:00+0000] [ALPM] transaction started
[2026-07-02T12:00:01+0000] [ALPM] upgraded firefox (120.0-1 -> 122.0-1)
[2026-07-02T12:00:02+0000] [ALPM] transaction completed
";
        let update = "\
[2026-07-02T13:00:00Z INFO] update session started
[2026-07-02T13:00:00Z INFO] aur: running paru -Sua
[2026-07-02T13:04:00Z INFO] aur: failed, exit 1
[2026-07-02T13:04:00Z INFO] update session complete: 1 of 1 sources failed
";
        let txs = crate::analyzer::history::parse(log);
        let out = render(&txs, &sessions_of(update), false, None, 10, &ascii());
        assert!(
            out.contains(
                "2026-07-02 13:00  paclens session: aur exited 1 [confirmed]; no matching pacman history row"
            ),
            "{out}"
        );
        // Newest first: the session (13:00) above the run (12:00).
        assert!(
            out.find("13:00").unwrap() < out.find("12:00").unwrap(),
            "{out}"
        );
        assert!(!out.contains("[inferred]"), "claimed a row:\n{out}");
    }

    #[test]
    fn a_session_with_no_end_says_unknown_never_succeeded() {
        let update = "\
[2026-07-02T13:00:00Z INFO] update session started
[2026-07-02T13:00:00Z INFO] pacman: running sudo pacman -Syu
";
        let txs = crate::analyzer::history::parse(LOG);
        let out = render(&txs, &sessions_of(update), false, None, 10, &ascii());
        assert!(
            out.contains("paclens session: outcome Unknown — no end recorded [unknown]"),
            "{out}"
        );
        assert!(!out.contains("succeeded"), "{out}");
    }

    #[test]
    fn an_interleaved_session_says_interleaved_not_unended() {
        let update = "\
[2026-07-02T13:00:00Z INFO] update session started
[2026-07-02T13:00:01Z INFO] update session started
[2026-07-02T13:00:05Z INFO] update session complete: all sources succeeded
[2026-07-02T13:00:06Z INFO] update session complete: all sources succeeded
";
        let txs = crate::analyzer::history::parse(LOG);
        let out = render(&txs, &sessions_of(update), false, None, 10, &ascii());
        assert_eq!(
            out.matches("log interleaved with another session").count(),
            2,
            "{out}"
        );
        assert!(!out.contains("no end recorded"), "{out}");
    }

    #[test]
    fn clean_sessions_leave_the_output_byte_identical() {
        let update = "\
[2026-07-02T10:00:00Z INFO] update session started
[2026-07-02T10:00:00Z INFO] pacman: running sudo pacman -Syu
[2026-07-02T10:00:05Z INFO] pacman: completed, exit 0
[2026-07-02T10:00:05Z INFO] aur: skipped — no AUR helper found
[2026-07-02T10:00:05Z INFO] update session complete: all sources succeeded
";
        let txs = crate::analyzer::history::parse(LOG);
        assert_eq!(
            render(&txs, &sessions_of(update), false, None, 10, &ascii()),
            render(&txs, &[], false, None, 10, &ascii())
        );
    }

    #[test]
    fn a_session_older_than_a_cut_tail_is_left_out() {
        // The tail starts 2026-03-14; a failure before it may have a row that
        // was cut off, so "no matching row" is not said.
        let update = "\
[2026-01-01T10:00:00Z INFO] update session started
[2026-01-01T10:00:00Z INFO] aur: running paru -Sua
[2026-01-01T10:04:00Z INFO] aur: failed, exit 1
[2026-01-01T10:04:00Z INFO] update session complete: 1 of 1 sources failed
";
        let txs = crate::analyzer::history::parse(LOG);
        let cut = render(&txs, &sessions_of(update), true, None, 10, &ascii());
        assert!(!cut.contains("paclens session"), "{cut}");
        // A whole log (a rotated one, say) cut nothing: the failure shows.
        let whole = render(&txs, &sessions_of(update), false, None, 10, &ascii());
        assert!(whole.contains("paclens session: aur exited 1"), "{whole}");
    }

    #[test]
    fn a_session_straddling_a_cut_tail_claims_no_row() {
        // The tail starts mid-window: a run cut off before 10:00 may have
        // shared the window, so attaching to the 10:20 run is not proven.
        let log = "\
[2026-07-02T10:20:00+0000] [ALPM] transaction started
[2026-07-02T10:20:01+0000] [ALPM] upgraded firefox (120.0-1 -> 122.0-1)
[2026-07-02T10:20:02+0000] [ALPM] transaction completed
";
        let update = "\
[2026-07-02T09:59:00Z INFO] update session started
[2026-07-02T10:30:00Z INFO] aur: failed, exit 1
[2026-07-02T10:30:00Z INFO] update session complete: 1 of 1 sources failed
";
        let txs = crate::analyzer::history::parse(log);
        let cut = render(&txs, &sessions_of(update), true, None, 10, &ascii());
        assert!(!cut.contains("aur exited 1"), "{cut}");
        let whole = render(&txs, &sessions_of(update), false, None, 10, &ascii());
        assert!(
            whole.contains("aur exited 1 — cause/package Unknown [inferred]"),
            "{whole}"
        );
    }

    #[test]
    fn read_sessions_distrusts_windows_crossing_day_files() {
        let dir = std::env::temp_dir().join(format!("paclens-cross-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("2026-11-03.log"),
            "[2026-11-03T23:58:00Z INFO] update session started\n[2026-11-04T00:10:00Z INFO] update session complete: all sources succeeded\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("2026-11-04.log"),
            "[2026-11-04T00:01:00Z INFO] update session started\n[2026-11-04T00:05:00Z INFO] aur: failed, exit 1\n[2026-11-04T00:05:00Z INFO] update session complete: 1 of 1 sources failed\n",
        )
        .unwrap();
        let got = read_sessions(&dir);
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|x| !x.trusted()), "{got:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_lone_session_older_than_the_shown_runs_waits_for_the_limit() {
        let update = "\
[2026-05-01T10:00:00Z INFO] update session started
[2026-05-01T10:00:00Z INFO] aur: running paru -Sua
[2026-05-01T10:04:00Z INFO] aur: failed, exit 1
[2026-05-01T10:04:00Z INFO] update session complete: 1 of 1 sources failed
";
        let txs = crate::analyzer::history::parse(LOG);
        let hidden = render(&txs, &sessions_of(update), false, None, 1, &ascii());
        assert!(!hidden.contains("paclens session"), "{hidden}");
        let shown = render(&txs, &sessions_of(update), false, None, 10, &ascii());
        let at = shown
            .find("2026-05-01")
            .expect("shown under the full limit");
        assert!(
            shown.find("2026-07-02").unwrap() < at && at < shown.find("2026-03-14").unwrap(),
            "out of order:\n{shown}"
        );
    }

    #[test]
    fn read_sessions_reads_every_day_and_skips_tracing_logs() {
        let dir = std::env::temp_dir().join(format!("paclens-sess-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let day = |d: &str| {
            format!(
                "[{d}T10:00:00Z INFO] update session started\n[{d}T10:00:05Z INFO] update session complete: all sources succeeded\n"
            )
        };
        std::fs::write(dir.join("2026-07-02.log"), day("2026-07-02")).unwrap();
        std::fs::write(dir.join("2026-07-01.log"), day("2026-07-01")).unwrap();
        std::fs::write(dir.join("paclens-2026-07-02-100000.log"), day("2026-07-03")).unwrap();
        let got = read_sessions(&dir);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[0].started < got[1].started, "not oldest first");
        assert!(got.iter().all(Session::trusted), "{got:?}");
        assert!(read_sessions(&dir.join("missing")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_tail_reader_returns_the_end_of_a_file() {
        let dir = std::env::temp_dir().join(format!("paclens-hist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pacman.log");
        std::fs::write(&path, "old junk\nthe end\n").unwrap();
        let tail = read_tail(&path, 8).unwrap();
        assert!(tail.ends_with("the end\n"), "{tail:?}");
        assert!(!tail.contains("old junk"), "read more than asked: {tail:?}");
        // A file shorter than the window is returned whole.
        assert_eq!(read_tail(&path, 4096).unwrap(), "old junk\nthe end\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
