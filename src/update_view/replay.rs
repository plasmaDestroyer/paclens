//! A `script` recording (typescript + timing) and its pacman.log, read back
//! for replay (#81).

use std::time::Duration;

use chrono::{DateTime, FixedOffset};

use crate::analyzer::history::split_line;

/// One timed write of the recorded terminal output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub delay: Duration,
    pub bytes: Vec<u8>,
}

/// Cut the typescript into the writes its timing file (classic util-linux
/// format, `<delay secs> <byte count>` per line) describes. The `Script
/// started` header line is skipped; bytes past the timing total are the
/// untimed `Script done` footer and are dropped.
pub fn parse_recording(typescript: &[u8], timing: &str) -> anyhow::Result<Vec<Chunk>> {
    let mut rest = typescript
        .iter()
        .position(|&b| b == b'\n')
        .map_or(typescript, |i| &typescript[i + 1..]);
    let mut chunks = Vec::new();
    for (i, line) in timing.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let n = i + 1;
        let (secs, len) = line
            .split_once(' ')
            .and_then(|(s, l)| Some((s.parse::<f64>().ok()?, l.trim().parse::<usize>().ok()?)))
            .ok_or_else(|| {
                anyhow::anyhow!("timing line {n}: expected `<secs> <bytes>`: {line:?}")
            })?;
        let delay = Duration::try_from_secs_f64(secs)
            .map_err(|e| anyhow::anyhow!("timing line {n}: {e}"))?;
        anyhow::ensure!(
            len <= rest.len(),
            "timing line {n}: wants {len} bytes, typescript has {} left",
            rest.len()
        );
        let (bytes, tail) = rest.split_at(len);
        rest = tail;
        chunks.push(Chunk {
            delay,
            bytes: bytes.to_vec(),
        });
    }
    Ok(chunks)
}

/// When the recording started, from its `Script started on <stamp> [...]`
/// header.
pub fn script_start(typescript: &[u8]) -> Option<DateTime<FixedOffset>> {
    let header = typescript.split(|&b| b == b'\n').next()?;
    let header = std::str::from_utf8(header).ok()?;
    let rest = header.strip_prefix("Script started on ")?;
    let stamp = rest.split(" [").next()?.trim();
    DateTime::parse_from_str(stamp, "%Y-%m-%d %H:%M:%S%z").ok()
}

/// Each timestamped log line with its offset from `start`. Lines without a
/// pacman.log timestamp are skipped; a line stamped before `start` gets 0.
pub fn log_schedule(log: &str, start: DateTime<FixedOffset>) -> Vec<(Duration, String)> {
    log.lines()
        .filter_map(|line| {
            let (at, _) = split_line(line)?;
            let offset = (at - start).to_std().unwrap_or_default();
            Some((offset, line.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUT: &[u8] = include_bytes!("../../tests/fixtures/pacman-run/2026-10-16.out");
    const TIMING: &str = include_str!("../../tests/fixtures/pacman-run/2026-10-16.timing");
    const LOG: &str = include_str!("../../tests/fixtures/pacman-run/2026-10-16.log");

    fn start() -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339("2026-10-16T11:35:31+05:30").unwrap_or_default()
    }

    #[test]
    fn fixture_chunks_sum_to_timing_total() {
        let chunks = parse_recording(OUT, TIMING).unwrap_or_default();
        let total: usize = chunks.iter().map(|c| c.bytes.len()).sum();
        assert_eq!(total, 2_169_381);
    }

    #[test]
    fn fixture_first_delay() {
        let chunks = parse_recording(OUT, TIMING).unwrap_or_default();
        assert_eq!(
            chunks.first().map(|c| c.delay),
            Some(Duration::from_millis(226))
        );
    }

    #[test]
    fn header_skipped_and_footer_dropped() {
        let ts = b"Script started on x\nabcdeSCRIPT DONE";
        let chunks = parse_recording(ts, "0.5 2\n1.0 3\n").unwrap_or_default();
        assert_eq!(
            chunks,
            vec![
                Chunk {
                    delay: Duration::from_millis(500),
                    bytes: b"ab".to_vec()
                },
                Chunk {
                    delay: Duration::from_secs(1),
                    bytes: b"cde".to_vec()
                },
            ]
        );
    }

    #[test]
    fn malformed_timing_line_names_line_number() {
        let err = parse_recording(b"h\nabc", "0.1 1\nnope\n").err();
        let msg = err.map(|e| e.to_string()).unwrap_or_default();
        assert!(msg.contains("line 2"), "{msg}");
    }

    #[test]
    fn timing_longer_than_typescript_is_err() {
        assert!(parse_recording(b"h\nab", "0.1 5\n").is_err());
    }

    #[test]
    fn fixture_script_start() {
        assert_eq!(script_start(OUT), Some(start()));
    }

    #[test]
    fn script_start_none_without_header() {
        assert_eq!(script_start(b"hello\n"), None);
    }

    #[test]
    fn fixture_log_schedule_offsets() {
        let sched = log_schedule(LOG, start());
        assert_eq!(sched.first().map(|s| s.0), Some(Duration::from_secs(5)));
        let done = sched
            .iter()
            .find(|(_, l)| l.ends_with("[ALPM] transaction completed"))
            .map(|s| s.0);
        assert_eq!(done, Some(Duration::from_secs(110)));
    }

    #[test]
    fn log_schedule_skips_untimed_lines() {
        let log = "garbage\n[2026-10-16T11:35:41+0530] [ALPM] x\n";
        assert_eq!(
            log_schedule(log, start()),
            vec![(
                Duration::from_secs(10),
                "[2026-10-16T11:35:41+0530] [ALPM] x".to_string()
            )]
        );
    }
}
