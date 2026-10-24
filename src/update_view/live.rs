//! The live view's run plumbing (#81): which steps get the view, and the
//! last-run transcript it leaves behind.
//!
//! The transcript is written in `script(1)`'s format — a `.out` with a
//! `Script started on …` header and a `.timing` of `<delta> <len>` lines — the
//! same shape as the fixture in `tests/fixtures/pacman-run`, so
//! `examples/replay` plays the last real run. Last run only, mode 0600,
//! replaced each time.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyEventKind};
use crossterm::terminal;

use crate::cli::history::{TAIL_BYTES, read_tail};
use crate::cli::style::Styles;
use crate::config::{Layout, Motif};
use crate::executor::pty::{exit_code, spawn};
use crate::executor::sudo::CANDIDATES;
use crate::executor::{InteractiveRunner, StepRunner};
use crate::tui::input::encode_key;

use super::log_line::{LineBuffer, classify_log};
use super::medians::hook_medians;
use super::model::Run;
use super::region::Painter;
use super::render::{View, candy_enabled, closing};
use super::screen::Settler;
use super::screen_line::may_be_question;

/// The last run's raw pty output, `script(1)` style.
pub const TRANSCRIPT: &str = "pacman-last.out";
/// The `<delta> <len>` timing lines matching [`TRANSCRIPT`].
pub const TRANSCRIPT_TIMING: &str = "pacman-last.timing";
/// The pacman.log lines this run produced.
pub const TRANSCRIPT_LOG: &str = "pacman-last.log";

/// Does this step run a pacman transaction? An escalation tool in front
/// (`sudo pacman …`) is skipped; anything else (`paru`, `flatpak`) is not
/// pacman, whoever runs it.
pub fn is_pacman_transaction(argv: &[String]) -> bool {
    let mut rest = argv.iter();
    let mut program = rest.next();
    if program.is_some_and(|p| CANDIDATES.contains(&p.as_str())) {
        program = rest.next();
    }
    program.is_some_and(|p| p == "pacman")
}

pub struct ViewOptions {
    pub layout: Layout,
    pub motif: Motif,
    pub styles: Styles,
    /// stdout is a TTY: draw the live region. Otherwise print settled lines.
    pub live: bool,
    /// stdin is a TTY: raw mode and key polling only then — crossterm fails
    /// with "Failed to initialize input reader" without one.
    pub keys: bool,
    /// name → version from the scan cache; empty if there is none.
    pub installed: HashMap<String, String>,
    /// pacman.log.
    pub log: PathBuf,
    pub transcript_dir: Option<PathBuf>,
}

/// Runs pacman transactions under the live view, every other step as the
/// plain terminal runner does.
pub struct ViewRunner {
    pub opts: ViewOptions,
}

impl StepRunner for ViewRunner {
    fn run(&self, argv: &[String]) -> anyhow::Result<Option<i32>> {
        if is_pacman_transaction(argv) {
            run(argv, &self.opts, &mut io::stdout())
        } else {
            InteractiveRunner.run(argv)
        }
    }

    fn run_captured(&self, argv: &[String]) -> anyhow::Result<(Option<i32>, String)> {
        InteractiveRunner.run_captured(argv)
    }

    /// The view's own title names the command.
    fn announces(&self, argv: &[String]) -> bool {
        !is_pacman_transaction(argv)
    }
}

const TICK: Duration = Duration::from_millis(50);
/// An unfinished line and no output for this long: pacman waits for an
/// answer.
const IDLE: Duration = Duration::from_millis(300);

/// Raw mode for as long as it lives — restored on every return path and on
/// unwinding.
struct RawGuard;

impl RawGuard {
    fn new() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

/// pacman.log from where it ended when the run started.
struct LogTail {
    file: Option<File>,
    lines: LineBuffer,
}

impl LogTail {
    fn open(path: &Path) -> Self {
        let file = File::open(path).ok().and_then(|mut f| {
            f.seek(SeekFrom::End(0)).ok()?;
            Some(f)
        });
        Self {
            file,
            lines: LineBuffer::new(),
        }
    }

    fn read(&mut self) -> Vec<String> {
        let mut buf = Vec::new();
        if let Some(f) = &mut self.file {
            let _ = f.read_to_end(&mut buf);
        }
        self.lines.push(&buf)
    }
}

/// Run `argv` on a pty under the live view and return its exit code (`None`
/// for a signal). The child sees a real terminal the size of ours; keys go
/// to it unchanged, Ctrl-C included.
pub fn run(
    argv: &[String],
    opts: &ViewOptions,
    out: &mut (impl Write + Send),
) -> anyhow::Result<Option<i32>> {
    let mut log = LogTail::open(&opts.log);
    let medians = read_tail(&opts.log, TAIL_BYTES)
        .map(|t| hook_medians(&t))
        .unwrap_or_default();
    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    let command = argv.join(" ");
    let mut transcript = opts.transcript_dir.as_ref().and_then(|dir| {
        Transcript::create(dir, &command, cols)
            .map_err(|err| tracing::warn!(error = %err, "no transcript"))
            .ok()
    });
    let view = View {
        layout: opts.layout,
        motif: opts.motif,
        candy: fs::read_to_string("/etc/pacman.conf").is_ok_and(|c| candy_enabled(&c)),
    };

    let mut pty = spawn(argv, rows, cols)?;
    let _raw = opts.keys.then(RawGuard::new).transpose()?;
    let mut reader = std::mem::replace(&mut pty.reader, Box::new(io::empty()));
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });

    let start = Instant::now();
    let mut last_bytes = start;
    let mut settler = Settler::new(cols);
    let mut model = Run::new(&command, medians, opts.installed.clone());
    let mut painter = Painter::new(opts.live, rows, cols);
    if opts.keys {
        painter = painter.raw();
    }
    let status = loop {
        let now = start.elapsed();
        while let Ok(bytes) = rx.try_recv() {
            last_bytes = Instant::now();
            take(
                &bytes,
                now,
                &mut settler,
                &mut model,
                &mut painter,
                &mut transcript,
                out,
            )?;
        }
        read_log(&mut log, &mut model, &mut transcript, now);
        if opts.keys {
            while event::poll(Duration::ZERO)? {
                if let Event::Key(k) = event::read()?
                    && k.kind != KeyEventKind::Release
                    && let (Some(bytes), Some(w)) = (encode_key(k), pty.writer.as_mut())
                {
                    w.write_all(&bytes)?;
                    w.flush()?;
                }
            }
        }
        let exited = pty.child.try_wait()?;
        let idle = (may_be_question(&settler.partial()) && last_bytes.elapsed() >= IDLE)
            .then(|| settler.tail(20.min(usize::from(rows / 2))));
        painter.tick(out, &model, &view, now, idle.as_deref(), &opts.styles)?;
        if idle.is_some() {
            painter.prompt(out, &settler.partial())?;
        }
        if let Some(status) = exited {
            break status;
        }
        std::thread::sleep(TICK);
    };

    // The child is gone: close the pty so the reader sees EOF, take what it
    // still had, then the log to its end — pacman can write `transaction
    // started` and exit between two ticks.
    drop(pty.close());
    let now = start.elapsed();
    while let Ok(bytes) = rx.recv_timeout(Duration::from_millis(200)) {
        take(
            &bytes,
            now,
            &mut settler,
            &mut model,
            &mut painter,
            &mut transcript,
            out,
        )?;
    }
    // A last line pacman never ended with a newline still counts.
    let rest = settler.partial();
    if !rest.is_empty() {
        model.apply_line(&rest, now);
        painter.line(out, &rest)?;
    }
    read_log(&mut log, &mut model, &mut transcript, now);
    // One more frame, so what came in after the last tick goes up too.
    painter.tick(out, &model, &view, now, None, &opts.styles)?;
    let code = exit_code(&status);
    model.finish(code, now);
    let transcript_path = opts
        .transcript_dir
        .as_ref()
        .map(|d| d.join(TRANSCRIPT).display().to_string())
        .unwrap_or_default();
    let lines = closing(&model, &view, cols, &transcript_path, &opts.styles);
    painter.close(out, &model, &lines)?;
    Ok(code)
}

/// pty output that arrived `at`: into the transcript, the screen, the model
/// and (plain mode) stdout.
fn take(
    bytes: &[u8],
    at: Duration,
    settler: &mut Settler,
    model: &mut Run,
    painter: &mut Painter,
    transcript: &mut Option<Transcript>,
    out: &mut impl Write,
) -> io::Result<()> {
    if let Some(t) = transcript {
        t.record(at, bytes);
    }
    for l in settler.feed(bytes) {
        model.apply_line(&l, at);
        painter.line(out, &l)?;
    }
    Ok(())
}

fn read_log(log: &mut LogTail, model: &mut Run, transcript: &mut Option<Transcript>, at: Duration) {
    for line in log.read() {
        if let Some(t) = transcript {
            t.log_line(&line);
        }
        if let Some((_, ev)) = classify_log(&line) {
            model.apply_log(ev, at);
        }
    }
}

/// The last-run transcript. Writes after [`Transcript::create`] are best
/// effort: a failure is logged and dropped, never allowed to stop the run.
pub struct Transcript {
    out: File,
    timing: File,
    log: File,
    last: Duration,
}

impl Transcript {
    /// Create (or replace) the three files in `dir`, private to the user, and
    /// write the `Script started on …` header naming `command` and the width.
    pub fn create(dir: &Path, command: &str, cols: u16) -> io::Result<Self> {
        let mut out = private_file(&dir.join(TRANSCRIPT))?;
        let timing = private_file(&dir.join(TRANSCRIPT_TIMING))?;
        let log = private_file(&dir.join(TRANSCRIPT_LOG))?;
        let started = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%:z");
        writeln!(
            out,
            "Script started on {started} [COMMAND=\"{command}\" COLUMNS=\"{cols}\"]"
        )?;
        Ok(Self {
            out,
            timing,
            log,
            last: Duration::ZERO,
        })
    }

    /// Append pty output that arrived `at` (since the run started).
    pub fn record(&mut self, at: Duration, bytes: &[u8]) {
        let delta = at.saturating_sub(self.last);
        self.last = at;
        let result = self
            .out
            .write_all(bytes)
            .and_then(|()| writeln!(self.timing, "{:.6} {}", delta.as_secs_f64(), bytes.len()));
        if let Err(err) = result {
            tracing::warn!(error = %err, "transcript write failed");
        }
    }

    /// Append one pacman.log line.
    pub fn log_line(&mut self, line: &str) {
        if let Err(err) = writeln!(self.log, "{line}") {
            tracing::warn!(error = %err, "transcript log write failed");
        }
    }
}

/// Open `path` for writing, truncated, mode 0600 — also when it already
/// existed with wider permissions, so a replaced file stays private.
fn private_file(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv<const N: usize>(a: [&str; N]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("paclens-live-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn detects_pacman_steps() {
        assert!(is_pacman_transaction(&argv(["sudo", "pacman", "-Syu"])));
        assert!(is_pacman_transaction(&argv(["pacman", "-S", "foo"])));
        assert!(!is_pacman_transaction(&argv(["paru", "-Sua"])));
        assert!(!is_pacman_transaction(&argv(["sudo", "flatpak", "update"])));
    }

    #[test]
    fn detects_pacman_behind_every_escalation_tool() {
        assert!(is_pacman_transaction(&argv(["doas", "pacman", "-Syu"])));
        assert!(is_pacman_transaction(&argv(["pkexec", "pacman", "-Syu"])));
    }

    #[test]
    fn empty_and_bare_escalation_are_not_pacman() {
        assert!(!is_pacman_transaction(&[]));
        assert!(!is_pacman_transaction(&argv(["sudo"])));
    }

    #[test]
    fn only_one_escalation_tool_is_skipped() {
        assert!(!is_pacman_transaction(&argv(["sudo", "sudo", "pacman"])));
    }

    #[test]
    fn transcript_is_private_and_replaced() {
        let dir = temp_dir("replaced");
        // A leftover with wide permissions must come back private.
        fs::write(dir.join(TRANSCRIPT), "old run").unwrap();
        fs::set_permissions(dir.join(TRANSCRIPT), fs::Permissions::from_mode(0o644)).unwrap();

        let mut first = Transcript::create(&dir, "sudo pacman -Syu", 80).unwrap();
        first.record(Duration::from_millis(10), b"first");
        first.log_line("first log");
        drop(first);
        let mut second = Transcript::create(&dir, "sudo pacman -S foo", 80).unwrap();
        second.record(Duration::from_millis(10), b"second");
        second.log_line("second log");
        drop(second);

        for name in [TRANSCRIPT, TRANSCRIPT_TIMING, TRANSCRIPT_LOG] {
            assert_eq!(mode(&dir.join(name)), 0o600, "{name}");
        }
        let out = fs::read_to_string(dir.join(TRANSCRIPT)).unwrap();
        assert!(out.contains("second") && !out.contains("first") && !out.contains("old run"));
        assert!(out.contains("[COMMAND=\"sudo pacman -S foo\" COLUMNS=\"80\"]"));
        assert_eq!(
            fs::read_to_string(dir.join(TRANSCRIPT_TIMING)).unwrap(),
            "0.010000 6\n"
        );
        assert_eq!(
            fs::read_to_string(dir.join(TRANSCRIPT_LOG)).unwrap(),
            "second log\n"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn transcript_matches_the_script_format() {
        let dir = temp_dir("format");
        let mut t = Transcript::create(&dir, "sudo pacman -Syu", 80).unwrap();
        t.record(Duration::from_micros(226_000), b"[sudo] password for me: ");
        t.record(Duration::from_micros(4_348_530), b"\r\n");
        drop(t);

        let out = fs::read_to_string(dir.join(TRANSCRIPT)).unwrap();
        let (header, body) = out.split_once('\n').unwrap();
        assert!(header.starts_with("Script started on "), "{header}");
        assert!(
            header.ends_with(" [COMMAND=\"sudo pacman -Syu\" COLUMNS=\"80\"]"),
            "{header}"
        );
        // `2026-10-16 11:35:31+05:30`: date, time, numeric offset with a colon.
        let stamp = header
            .trim_start_matches("Script started on ")
            .split(" [")
            .next()
            .unwrap();
        assert_eq!(stamp.len(), 25, "{stamp}");
        assert!(chrono::DateTime::parse_from_str(stamp, "%Y-%m-%d %H:%M:%S%:z").is_ok());
        assert_eq!(body, "[sudo] password for me: \r\n");

        // Deltas since the previous record, six decimals, then byte count.
        assert_eq!(
            fs::read_to_string(dir.join(TRANSCRIPT_TIMING)).unwrap(),
            "0.226000 24\n4.122530 2\n"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn create_fails_when_the_dir_is_missing() {
        let dir = std::env::temp_dir().join(format!("paclens-live-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert!(Transcript::create(&dir, "pacman -Syu", 80).is_err());
    }

    fn opts(tag: &str) -> (ViewOptions, std::path::PathBuf) {
        let dir = temp_dir(tag);
        let log = dir.join("pacman.log");
        fs::write(&log, "[2026-10-20T10:00:00+0530] [PACMAN] synchronizing\n").unwrap();
        let o = ViewOptions {
            layout: Layout::Inline,
            motif: Motif::Subtle,
            styles: Styles::resolve(true, crate::config::ColorTheme::Dark, false),
            live: false,
            keys: false,
            installed: HashMap::new(),
            log,
            transcript_dir: Some(dir.clone()),
        };
        (o, dir)
    }

    fn sh(script: &str) -> Vec<String> {
        argv(["sh", "-c", script])
    }

    #[test]
    fn live_run_reports_child_exit_code() {
        let (o, dir) = opts("exit");
        let mut out = Vec::new();
        let script = "printf ':: Synchronizing package databases...\\n core\\n'; exit 3";
        let code = run(&sh(script), &o, &mut out).unwrap();
        assert_eq!(code, Some(3));
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains(" core\n"), "{text:?}");
        assert!(text.contains("pacman exit 3"), "{text:?}");
        assert!(!text.contains('\x1b'), "{text:?}");
        let recorded = fs::read_to_string(dir.join(TRANSCRIPT)).unwrap();
        assert!(recorded.contains(" core"));
    }

    /// A `Write` the test can read while `run` still holds it.
    #[derive(Clone, Default)]
    struct Shared(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn plain_mode_prints_idle_prompt() {
        let (o, _) = opts("idle");
        let out = Shared::default();
        let mut w = out.clone();
        let child = std::thread::spawn(move || {
            run(&sh("printf ':: Proceed? [Y/n] '; sleep 2"), &o, &mut w).unwrap()
        });
        std::thread::sleep(Duration::from_millis(1200));
        let seen = String::from_utf8_lossy(&out.0.lock().unwrap()).into_owned();
        assert!(!child.is_finished());
        assert!(seen.contains(":: Proceed? [Y/n]"), "{seen:?}");
        assert_eq!(child.join().unwrap(), Some(0));
    }

    #[test]
    fn log_lines_written_just_before_exit_are_read() {
        let (o, _) = opts("log");
        // ponytail: the race (log line, exit, between two ticks) cannot be
        // forced from here; this pins the outcome, not the post-exit read.
        let log = o.log.display();
        let script = format!(
            "echo '[2026-10-20T10:00:01+0530] [PACMAN] Running pacman -S foo' >> {log}; \
             sleep 0.2; echo '[2026-10-20T10:00:01+0530] [ALPM] transaction started' >> {log}"
        );
        let mut out = Vec::new();
        assert_eq!(run(&sh(&script), &o, &mut out).unwrap(), Some(0));
        let text = String::from_utf8_lossy(&out);
        assert!(!text.contains("nothing to do"), "{text:?}");
    }

    #[test]
    fn last_line_without_newline_is_kept() {
        for live in [false, true] {
            let (mut o, _) = opts(if live { "tail-live" } else { "tail-plain" });
            o.live = live;
            let mut out = Vec::new();
            let script = "printf 'odd line\\nerror: final failure %0600d' 0; exit 1";
            assert_eq!(run(&sh(script), &o, &mut out).unwrap(), Some(1));
            let text = String::from_utf8_lossy(&out);
            assert!(text.contains("odd line"), "live {live}: {text:?}");
            assert!(
                text.contains("error: final failure 000"),
                "live {live}: {text:?}"
            );
        }
    }

    #[test]
    fn signal_exit_is_reported() {
        let (o, _) = opts("signal");
        let mut out = Vec::new();
        assert_eq!(run(&sh("kill -INT $$"), &o, &mut out).unwrap(), None);
        assert!(String::from_utf8_lossy(&out).contains("signal"));
    }

    #[test]
    fn view_steps_carry_no_executor_header() {
        let (opts, _) = opts("announce");
        let r = ViewRunner { opts };
        assert!(!r.announces(&argv(["sudo", "pacman", "-Syu"])));
        assert!(r.announces(&argv(["paru", "-Sua"])));
    }

    #[test]
    fn non_pacman_steps_go_to_the_terminal_runner() {
        let (opts, _) = opts("runner");
        assert_eq!(ViewRunner { opts }.run(&argv(["true"])).unwrap(), Some(0));
    }
}
