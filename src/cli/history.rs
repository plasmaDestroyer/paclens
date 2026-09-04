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
use crate::cli::style::Styles;

/// Where pacman writes, and how much of it to read.
///
/// 4 MiB is thousands of transactions on a normal machine and a fraction of a
/// second to parse, while a full log on a years-old install can be 50 MB.
/// Reading the tail is what keeps a history question cheap; the parser is
/// built to start mid-file for exactly this reason.
const PACMAN_LOG: &str = "/var/log/pacman.log";
const TAIL_BYTES: u64 = 4 * 1024 * 1024;

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
    print!("{}", render(&transactions, package, limit, styles));
    Ok(())
}

/// The whole report. Pure, like the other CLI renderers, so the no-color
/// output is deterministic and testable.
fn render(transactions: &[Transaction], package: Option<&str>, limit: usize, s: &Styles) -> String {
    let mut out = String::new();
    if let Some(name) = package {
        return render_package(transactions, name, s);
    }

    let shown = transactions.len().min(limit);
    out.push_str(&format!(
        "{} {} {}\n\n",
        s.title("paclens"),
        s.dim(s.bullet()),
        if transactions.is_empty() {
            s.dim("no transactions in the log tail")
        } else {
            s.summary_ok(&format!("{shown} of {} transactions", transactions.len()))
        }
    ));

    for tx in transactions.iter().take(limit) {
        let (installed, upgraded, removed) = tx.counts();
        let mut parts = Vec::new();
        if upgraded > 0 {
            parts.push(format!("{upgraded} upgraded"));
        }
        if installed > 0 {
            parts.push(format!("{installed} installed"));
        }
        if removed > 0 {
            parts.push(format!("{removed} removed"));
        }
        // An interrupted upgrade is the one worth seeing, so it says so
        // rather than being quietly indistinguishable from a clean run.
        let state = match tx.completed {
            Some(_) => String::new(),
            None => format!(" {}", s.dim("(did not complete)")),
        };
        out.push_str(&format!(
            "  {} {}  {}{}\n",
            s.bullet(),
            tx.started.format("%Y-%m-%d %H:%M"),
            parts.join(", "),
            state
        ));
    }

    if transactions.len() > limit {
        out.push_str(&s.dim(&format!(
            "\n  {} more — paclens history --limit N\n",
            transactions.len() - limit
        )));
    }
    if !transactions.is_empty() {
        out.push_str(&s.dim("\n  pacman only — Flatpak keeps no equivalent log\n"));
    }
    out
}

/// One package's history: every line the log has about it, newest first.
fn render_package(transactions: &[Transaction], name: &str, s: &Styles) -> String {
    let events = history::package_history(transactions, name);
    if events.is_empty() {
        return format!(
            "{} {} {}\n",
            s.title("paclens"),
            s.dim(s.bullet()),
            s.dim(&format!(
                "nothing about {name} in the log tail — it may predate it"
            ))
        );
    }
    let mut out = format!(
        "{} {} {}\n\n",
        s.title("paclens"),
        s.dim(s.bullet()),
        s.summary_ok(
            &history::package_summary(transactions, name).unwrap_or_else(|| name.to_string())
        )
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
        let out = render(&txs, None, 10, &ascii());
        assert!(out.contains("2 of 2 transactions"), "{out}");
        let newest = out.find("2026-07-02").expect("newest listed");
        let oldest = out.find("2026-03-14").expect("oldest listed");
        assert!(newest < oldest, "not newest-first:\n{out}");
        assert!(out.contains("1 upgraded, 1 removed"), "{out}");
        assert!(out.contains("2 installed"), "{out}");
        // The interrupted transaction says so.
        assert!(out.contains("did not complete"), "{out}");
        // And it does not pretend to know about Flatpak.
        assert!(out.contains("pacman only"), "{out}");
    }

    #[test]
    fn the_limit_says_what_it_hid() {
        let txs = crate::analyzer::history::parse(LOG);
        let out = render(&txs, None, 1, &ascii());
        assert!(out.contains("1 of 2 transactions"), "{out}");
        assert!(out.contains("1 more"), "{out}");
        assert!(!out.contains("2026-03-14"), "hid the wrong end:\n{out}");
    }

    #[test]
    fn one_packages_history_lists_every_line_about_it() {
        let txs = crate::analyzer::history::parse(LOG);
        let out = render(&txs, Some("firefox"), 10, &ascii());
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
        let out = render(&txs, Some("ghost"), 10, &ascii());
        assert!(out.contains("nothing about ghost"), "{out}");
        // "may predate it" rather than "never installed": a tail is a window,
        // not the whole history.
        assert!(out.contains("predate"), "{out}");
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
