//! pacman.log lines of one run, sorted into what the live view reacts to.

use chrono::{DateTime, FixedOffset};

use crate::analyzer::history::{PackageEvent, parse_event, split_line};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogEvent {
    /// `[PACMAN] Running 'pacman -Syu'` → `pacman -Syu`.
    RunStart(String),
    /// `[ALPM] running 'x.hook'...` → `x.hook`.
    HookRunning(String),
    /// `[ALPM-SCRIPTLET] …` → the text after the tag.
    Scriptlet(String),
    TxStarted,
    TxCompleted,
    TxFailed,
    Package(PackageEvent),
    Other,
}

/// `None` when the line carries no pacman.log timestamp.
pub fn classify_log(line: &str) -> Option<(DateTime<FixedOffset>, LogEvent)> {
    let (at, rest) = split_line(line)?;
    let event = match rest {
        "[ALPM] transaction started" => LogEvent::TxStarted,
        "[ALPM] transaction completed" => LogEvent::TxCompleted,
        "[ALPM] transaction failed" => LogEvent::TxFailed,
        _ => {
            if let Some(cmd) = rest
                .strip_prefix("[PACMAN] Running '")
                .and_then(|r| r.strip_suffix('\''))
            {
                LogEvent::RunStart(cmd.to_string())
            } else if let Some(hook) = rest
                .strip_prefix("[ALPM] running '")
                .and_then(|r| r.strip_suffix("'..."))
            {
                LogEvent::HookRunning(hook.to_string())
            } else if let Some(text) = rest.strip_prefix("[ALPM-SCRIPTLET] ") {
                LogEvent::Scriptlet(text.to_string())
            } else if let Some(event) = parse_event(at, rest) {
                LogEvent::Package(event)
            } else {
                LogEvent::Other
            }
        }
    };
    Some((at, event))
}

/// Splits tailed bytes into whole lines; a partial last line waits for its `\n`.
#[derive(Debug, Default)]
pub struct LineBuffer {
    pending: Vec<u8>,
}

impl LineBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Lines finished by a `\n` in `bytes`, without the `\n`.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(bytes);
        let Some(last) = self.pending.iter().rposition(|&b| b == b'\n') else {
            return Vec::new();
        };
        let rest = self.pending.split_off(last + 1);
        let done = std::mem::replace(&mut self.pending, rest);
        done[..last]
            .split(|&b| b == b'\n')
            .map(|l| String::from_utf8_lossy(l).into_owned())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../tests/fixtures/pacman-run/2026-10-16.log");

    fn events(log: &str) -> Vec<LogEvent> {
        log.lines()
            .filter_map(classify_log)
            .map(|(_, e)| e)
            .collect()
    }

    fn count(f: impl Fn(&LogEvent) -> bool) -> usize {
        events(FIXTURE).iter().filter(|e| f(e)).count()
    }

    #[test]
    fn fixture_every_line_is_a_log_line() {
        assert_eq!(events(FIXTURE).len(), FIXTURE.lines().count());
    }

    #[test]
    fn fixture_counts_per_kind() {
        assert_eq!(count(|e| matches!(e, LogEvent::RunStart(_))), 1);
        assert_eq!(count(|e| matches!(e, LogEvent::HookRunning(_))), 16);
        assert_eq!(count(|e| matches!(e, LogEvent::Scriptlet(_))), 4);
        assert_eq!(count(|e| matches!(e, LogEvent::TxStarted)), 1);
        assert_eq!(count(|e| matches!(e, LogEvent::TxCompleted)), 1);
        assert_eq!(count(|e| matches!(e, LogEvent::TxFailed)), 0);
        assert_eq!(count(|e| matches!(e, LogEvent::Package(_))), 38);
    }

    #[test]
    fn run_start_carries_command() {
        let (_, e) =
            classify_log("[2026-10-16T11:35:36+0530] [PACMAN] Running 'pacman -Syu'").unwrap();
        assert_eq!(e, LogEvent::RunStart("pacman -Syu".into()));
    }

    #[test]
    fn hook_carries_file_name() {
        let (_, e) = classify_log(
            "[2026-10-16T11:37:23+0530] [ALPM] running '70-openssh-restart-sshd.hook'...",
        )
        .unwrap();
        assert_eq!(
            e,
            LogEvent::HookRunning("70-openssh-restart-sshd.hook".into())
        );
    }

    #[test]
    fn scriptlet_carries_text_with_tabs() {
        let (_, e) =
            classify_log("[2026-10-16T11:37:33+0530] [ALPM-SCRIPTLET] foreign\twps-office")
                .unwrap();
        assert_eq!(e, LogEvent::Scriptlet("foreign\twps-office".into()));
    }

    #[test]
    fn transaction_markers() {
        let at = "[2026-10-16T11:37:05+0530] [ALPM] transaction ";
        let kind = |w: &str| classify_log(&format!("{at}{w}")).unwrap().1;
        assert_eq!(kind("started"), LogEvent::TxStarted);
        assert_eq!(kind("completed"), LogEvent::TxCompleted);
        assert_eq!(kind("failed"), LogEvent::TxFailed);
    }

    #[test]
    fn package_line_parsed_by_history() {
        let (at, e) = classify_log(
            "[2026-10-16T11:37:05+0530] [ALPM] upgraded sqlite (3.53.4-1 -> 3.54.0-1)",
        )
        .unwrap();
        let LogEvent::Package(p) = e else {
            panic!("not a package event: {e:?}");
        };
        assert_eq!(p.name, "sqlite");
        assert_eq!(p.from.as_deref(), Some("3.53.4-1"));
        assert_eq!(p.to.as_deref(), Some("3.54.0-1"));
        assert_eq!(at.to_rfc3339(), "2026-10-16T11:37:05+05:30");
    }

    #[test]
    fn pacman_chatter_is_other() {
        let (_, e) =
            classify_log("[2026-10-16T11:35:36+0530] [PACMAN] synchronizing package lists")
                .unwrap();
        assert_eq!(e, LogEvent::Other);
    }

    #[test]
    fn not_a_log_line_is_none() {
        assert!(classify_log("").is_none());
        assert!(classify_log("warning: something").is_none());
        assert!(classify_log("[garbage] [ALPM] transaction started").is_none());
    }

    #[test]
    fn line_buffer_holds_partial_line() {
        let mut buf = LineBuffer::new();
        assert_eq!(buf.push(b"a\nb"), vec!["a"]);
        assert_eq!(buf.push(b"c\n"), vec!["bc"]);
    }

    #[test]
    fn line_buffer_partial_never_emitted_without_newline() {
        let mut buf = LineBuffer::new();
        assert!(buf.push(b"abc").is_empty());
        assert!(buf.push(b"").is_empty());
        assert_eq!(buf.push(b"\n\nx\n"), vec!["abc", "", "x"]);
    }

    #[test]
    fn line_buffer_utf8_split_across_pushes() {
        let mut buf = LineBuffer::new();
        let bytes = "ᗧ ok\n".as_bytes();
        assert!(buf.push(&bytes[..1]).is_empty());
        assert_eq!(buf.push(&bytes[1..]), vec!["ᗧ ok"]);
    }
}
