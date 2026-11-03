//! A `script` recording (typescript + timing) and its pacman.log, read back
//! for replay (#81).

use std::collections::HashMap;
use std::io::{self, Write};
use std::time::Duration;

use chrono::{DateTime, FixedOffset};

use crate::analyzer::history::split_line;
use crate::cli::style::Styles;

use super::log_line::classify_log;
use super::medians::hook_medians;
use super::model::Run;
use super::region::{CursorGuard, Painter};
use super::render::{View, closing, closing_frames};
use super::screen::Settler;
use super::screen_line::may_be_question;

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

/// A field of the header, e.g. `COLUMNS="178"` → `178`.
pub fn header_field(typescript: &[u8], name: &str) -> Option<String> {
    let header = typescript.split(|&b| b == b'\n').next()?;
    let header = std::str::from_utf8(header).ok()?;
    let key = format!("{name}=\"");
    let at = header
        .match_indices(&key)
        .find(|(i, _)| matches!(header[..*i].chars().last(), Some(' ' | '[')))?
        .0;
    let rest = &header[at + key.len()..];
    Some(rest[..rest.find('"')?].to_string())
}

pub struct Play<'a> {
    pub typescript: &'a [u8],
    pub chunks: &'a [Chunk],
    /// pacman.log lines as `log_schedule` gives them.
    pub log: &'a [(Duration, String)],
    pub view: View,
    /// 1.0 = recorded speed; 0 = no sleeping.
    pub speed: f64,
    pub exit: Option<i32>,
    pub width: u16,
    pub height: u16,
    /// stdout is a terminal: draw the live region; else append plain lines.
    pub live: bool,
    /// Older pacman.log text for hook medians (the live view reads the
    /// system log); empty for none.
    pub history: &'a str,
    /// Shown in the failure block as where the full output is.
    pub transcript: &'a str,
    pub styles: Styles,
}

/// Same idle rule as the live view: an unfinished line and no output for
/// this long means pacman waits for an answer.
const IDLE: Duration = Duration::from_millis(300);
const TICK: Duration = Duration::from_millis(50);

/// Play a recording through the live view into `out`, as if pacman ran now.
pub fn play(out: &mut impl Write, p: &Play) -> io::Result<()> {
    let cols = header_field(p.typescript, "COLUMNS")
        .and_then(|c| c.parse().ok())
        .unwrap_or(p.width);
    let command = header_field(p.typescript, "COMMAND").unwrap_or_else(|| "pacman".into());
    let all_log: String = p.log.iter().map(|(_, l)| format!("{l}\n")).collect();
    let medians = hook_medians(&format!("{}\n{all_log}", p.history));
    let mut run = Run::new(&command, medians, HashMap::new());
    let mut settler = Settler::new(cols);
    let mut painter = Painter::new(p.live, p.height, p.width);
    let _cursor = p.live.then_some(CursorGuard);
    let mut log = p.log.iter().peekable();
    let mut now = Duration::ZERO;
    let end = p.log.last().map_or(Duration::ZERO, |(at, _)| *at);
    let idle_rows = |settler: &Settler| settler.tail(20.min(usize::from(p.height / 2)));
    for c in p.chunks {
        let waiting = may_be_question(&settler.partial());
        let mut waited = Duration::ZERO;
        // At speed 0 nothing watches the ticks: one frame per write, and
        // none for writes closer than a tick, as the live view would.
        let tick = if p.speed > 0.0 { TICK } else { c.delay };
        while waited < c.delay && (p.speed > 0.0 || c.delay >= TICK) {
            let step = tick.min(c.delay - waited);
            if p.speed > 0.0 {
                std::thread::sleep(step.div_f64(p.speed));
            }
            waited += step;
            catch_up(&mut log, &mut run, now + waited);
            run.observe_screen(&settler.tail(40), now + waited);
            let idle = (waiting && waited >= IDLE).then(|| idle_rows(&settler));
            painter.tick(out, &run, &p.view, now + waited, idle.as_deref(), &p.styles)?;
            if idle.is_some() {
                painter.prompt(out, &settler.partial())?;
            }
        }
        now += c.delay;
        for (l, styled) in settler.feed_styled(&c.bytes) {
            run.apply_line_styled(&l, &styled, now);
            painter.line(out, &l)?;
        }
    }
    // A last line the recording never ended with a newline still counts.
    let rest = settler.partial();
    if !rest.is_empty() {
        run.apply_line(&rest, now);
        painter.line(out, &rest)?;
    }
    // Log lines after the last write (pacman's post-transaction hooks race
    // its exit) still land before the end.
    catch_up(&mut log, &mut run, now.max(end));
    painter.tick(out, &run, &p.view, now.max(end), None, &p.styles)?;
    run.finish(p.exit, now);
    let lines = closing(&run, &p.view, p.width, p.transcript, &p.styles);
    let frames = closing_frames(&run, &p.view, p.width, p.transcript, &p.styles);
    painter.close(out, &run, &lines, &frames)
}

/// Apply every log line due by `now`.
fn catch_up<'a>(
    log: &mut std::iter::Peekable<impl Iterator<Item = &'a (Duration, String)>>,
    run: &mut Run,
    now: Duration,
) {
    while let Some((at, line)) = log.next_if(|(at, _)| *at <= now) {
        if let Some((_, ev)) = classify_log(line) {
            run.apply_log(ev, *at);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ColorTheme, Layout, Motif};

    const OUT: &[u8] = include_bytes!("../../tests/fixtures/pacman-run/2026-10-16.out");
    const TIMING: &str = include_str!("../../tests/fixtures/pacman-run/2026-10-16.timing");
    const LOG: &str = include_str!("../../tests/fixtures/pacman-run/2026-10-16.log");

    #[test]
    fn header_fields() {
        assert_eq!(header_field(OUT, "COLUMNS").as_deref(), Some("178"));
        assert_eq!(
            header_field(OUT, "COMMAND").as_deref(),
            Some("sudo pacman -Syu")
        );
        assert_eq!(header_field(OUT, "NOPE"), None);
        assert_eq!(header_field(b"no header", "COLUMNS"), None);
    }

    fn play_fixture(layout: Layout, motif: Motif, exit: Option<i32>, live: bool) -> String {
        let chunks = parse_recording(OUT, TIMING).unwrap();
        let log = log_schedule(LOG, start());
        let mut out = Vec::new();
        let p = Play {
            typescript: OUT,
            chunks: &chunks,
            log: &log,
            view: View {
                layout,
                motif,
                candy: true,
            },
            speed: 0.0,
            exit,
            width: 120,
            height: 40,
            live,
            history: "",
            transcript: "/tmp/last.out",
            styles: Styles::resolve(true, ColorTheme::Dark, false),
        };
        play(&mut out, &p).unwrap();
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn fixture_plays_in_every_layout_and_motif() {
        for layout in Layout::ALL {
            for motif in Motif::ALL {
                let s = play_fixture(layout, motif, Some(0), true);
                assert!(s.contains("38"), "{layout:?} {motif:?}");
                if motif == Motif::Subtle {
                    assert!(s.contains("38 eaten in "), "{layout:?}");
                }
                assert!(!s.contains("\x1b[?1049h"), "{layout:?} {motif:?}");
                assert!(!s.contains("/tmp/last.out"), "{layout:?} {motif:?}");
            }
        }
    }

    #[test]
    fn failed_exit_points_at_the_transcript() {
        let s = play_fixture(Layout::Inline, Motif::Subtle, Some(1), true);
        assert!(s.contains("/tmp/last.out"));
        // The region stays: no erase after the last frame.
        let tail = &s[s.rfind("\x1b[J").unwrap_or(0)..];
        assert!(tail.contains("post-hooks"), "{tail:?}");
    }

    #[test]
    fn final_line_without_newline_is_played() {
        let chunks = [Chunk {
            delay: Duration::ZERO,
            bytes: b"odd line\r\nerror: final failure".to_vec(),
        }];
        let mut out = Vec::new();
        let p = Play {
            typescript: b"Script started on 2026-10-16 11:35:31+05:30 [COMMAND=\"pacman -S x\"]\n",
            chunks: &chunks,
            log: &[],
            view: View {
                layout: Layout::Inline,
                motif: Motif::Subtle,
                candy: false,
            },
            speed: 0.0,
            exit: Some(1),
            width: 80,
            height: 24,
            live: false,
            history: "",
            transcript: "/tmp/last.out",
            styles: Styles::resolve(true, ColorTheme::Dark, false),
        };
        play(&mut out, &p).unwrap();
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("odd line\nerror: final failure\n"), "{s:?}");
    }

    #[test]
    fn plain_replay_has_no_cursor_moves() {
        let s = play_fixture(Layout::Inline, Motif::Subtle, Some(0), false);
        assert!(
            !s.contains('\x1b') && !s.contains('\r'),
            "escape in plain output"
        );
        assert_eq!(
            s.matches(":: Proceed with installation? [Y/n]\n").count(),
            1
        );
        assert!(s.ends_with('\n') && s.contains("38 eaten"));
    }

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
