//! The inline execution console's engine: runs a plan on a worker thread with
//! a **pty**, streaming the raw byte stream to the TUI (which feeds a vt100
//! screen) and forwarding typed keys verbatim — exact passthrough, no
//! terminal suspend (user decision 2026-07-08, design §13).
//!
//! A pty means sudo, doas, pkexec and pacman all see a real terminal: their
//! own prompts, echo control, colors and progress bars work untouched. The
//! password is handled by the child's echo-off, never stored (design §11).
//!
//! Logging records commands and exit codes (design §11); the output stream
//! itself is terminal noise (escape codes, progress redraws) and stays out.

use std::io::{Read, Write};
use std::sync::mpsc;

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use crate::executor::{
    self, ExecutionReport, StepReport, StepStatus, UpdateLog, skip_reason, target_noun,
};
use crate::model::ActionPlan;

/// What the worker streams back to the event loop.
pub enum ExecEvent {
    /// A chunk of raw pty output (escape codes included — vt100 renders it).
    Bytes(Vec<u8>),
    /// The whole session finished; the report is final.
    Done(ExecutionReport),
    /// The session could not even start (e.g. the update log failed to open).
    Failed(String),
}

/// Handles to a running session: events out, forwarded key bytes in.
pub struct ExecSession {
    pub events: mpsc::Receiver<ExecEvent>,
    input: mpsc::Sender<Vec<u8>>,
}

impl ExecSession {
    /// Forward raw bytes (typed keys) to the running command's pty.
    /// Best-effort: after the command exits there is nobody to receive them.
    pub fn forward(&self, bytes: Vec<u8>) {
        let _ = self.input.send(bytes);
    }
}

/// Start the session on a worker thread. `size` is the console viewport in
/// (rows, cols) — the pty is created to match so full-screen output wraps
/// correctly. `log_dir` overrides the default update-log location (the
/// hermetic seam for tests).
pub fn start(
    plan: ActionPlan,
    tool: Option<String>,
    size: (u16, u16),
    log_dir: Option<std::path::PathBuf>,
    sudo_loop: Option<std::time::Duration>,
) -> ExecSession {
    let (event_tx, events) = mpsc::channel();
    let (input, input_rx) = mpsc::channel::<Vec<u8>>();

    std::thread::spawn(move || {
        run_session(plan, tool, size, log_dir, sudo_loop, event_tx, input_rx)
    });

    ExecSession { events, input }
}

fn run_session(
    plan: ActionPlan,
    tool: Option<String>,
    size: (u16, u16),
    log_dir: Option<std::path::PathBuf>,
    sudo_loop: Option<std::time::Duration>,
    events: mpsc::Sender<ExecEvent>,
    input: mpsc::Receiver<Vec<u8>>,
) {
    let opened = match &log_dir {
        Some(dir) => UpdateLog::open_in(dir),
        None => UpdateLog::open_default(),
    };
    let mut log = match opened {
        Ok(log) => log,
        Err(err) => {
            let _ = events.send(ExecEvent::Failed(format!("{err:#}")));
            return;
        }
    };
    let tool = tool.as_deref();

    log.line("update session started");
    let run_ids: Vec<&str> = plan
        .steps
        .iter()
        .filter(|s| skip_reason(s, tool).is_none())
        .map(|s| s.source_id.as_str())
        .collect();
    log.line(&format!("sources: [{}]", run_ids.join(", ")));

    // #24: authenticate once, here, where the reader is looking — then keep
    // the timestamp warm so a long AUR build does not stop for a second
    // prompt in the middle of paru's output. The value lives for the run and
    // its Drop stops the loop on every exit path.
    let privileged = plan
        .steps
        .iter()
        .any(|s| executor::needs_privilege(s) && skip_reason(s, tool).is_none());
    let _keepalive = match sudo_loop {
        Some(interval) if executor::sudo::keepalive_applies(tool, true, privileged) => {
            let argv = executor::sudo::prime_command();
            let _ = events.send(ExecEvent::Bytes(
                format!(
                    "\x1b[1m:: {} \x1b[0m\x1b[2m(once, for the whole run)\x1b[0m\r\n",
                    argv.join(" ")
                )
                .into_bytes(),
            ));
            match run_step(&argv, size, &events, &input) {
                StepStatus::Succeeded => {
                    log.line("sudo timestamp primed; keepalive running");
                    Some(executor::sudo::Keepalive::start(interval))
                }
                _ => {
                    // Not fatal: the steps themselves will ask, exactly as
                    // they do with the loop off.
                    log.line("sudo priming failed; steps will authenticate on their own");
                    None
                }
            }
        }
        _ => None,
    };

    let mut steps = Vec::new();
    for step in &plan.steps {
        let targets = step.targets.len();
        if let Some(reason) = skip_reason(step, tool) {
            log.line(&format!("{}: skipped — {reason}", step.source_id));
            let _ = events.send(ExecEvent::Bytes(
                format!("\x1b[2m:: {} skipped — {reason}\x1b[0m\r\n", step.source_id).into_bytes(),
            ));
            steps.push(StepReport {
                source_id: step.source_id.clone(),
                targets,
                status: StepStatus::Skipped {
                    reason: reason.to_string(),
                },
            });
            continue;
        }

        let argv = executor::effective_command(step, tool);
        let cmd = argv.join(" ");
        log.line(&format!(
            "{}: running update ({})",
            step.source_id,
            target_noun(&step.source_id, targets)
        ));
        tracing::info!(source = %step.source_id, command = %cmd, "executing update step (pty)");
        let _ = events.send(ExecEvent::Bytes(
            format!("\x1b[1m:: {cmd}\x1b[0m\r\n").into_bytes(),
        ));

        let status = run_step(&argv, size, &events, &input);
        match &status {
            StepStatus::Succeeded => log.line(&format!("{}: completed, exit 0", step.source_id)),
            StepStatus::Failed { detail } => {
                log.line(&format!("{}: failed, {detail}", step.source_id));
                tracing::error!(source = %step.source_id, detail, "update step failed");
            }
            StepStatus::Skipped { .. } => {}
        }
        steps.push(StepReport {
            source_id: step.source_id.clone(),
            targets,
            status,
        });
    }

    let report = ExecutionReport {
        steps,
        log_path: log.path().to_path_buf(),
    };
    let failed = report.failed();
    let executed = report.executed();
    log.line(&format!(
        "update session complete: {}",
        if executed == 0 {
            "nothing to execute".to_string()
        } else if failed == 0 {
            "all sources succeeded".to_string()
        } else {
            format!("{failed} of {executed} sources failed")
        }
    ));
    let _ = events.send(ExecEvent::Done(report));
}

/// Spawn one command on a fresh pty; pump its output bytes to `events` and
/// forwarded key bytes from `input` into it until it exits.
/// ponytail: pty size is fixed at spawn — a mid-run terminal resize keeps the
/// old wrap width. Resize plumbing is the upgrade path if it ever bites.
fn run_step(
    argv: &[String],
    (rows, cols): (u16, u16),
    events: &mpsc::Sender<ExecEvent>,
    input: &mpsc::Receiver<Vec<u8>>,
) -> StepStatus {
    let Some((program, args)) = argv.split_first() else {
        return StepStatus::Failed {
            detail: "empty command".to_string(),
        };
    };

    let pty = match native_pty_system().openpty(PtySize {
        rows: rows.max(2),
        cols: cols.max(20),
        pixel_width: 0,
        pixel_height: 0,
    }) {
        Ok(pty) => pty,
        Err(err) => {
            return StepStatus::Failed {
                detail: format!("failed to open a pty: {err}"),
            };
        }
    };

    let mut cmd = CommandBuilder::new(program);
    cmd.args(args);
    let mut child = match pty.slave.spawn_command(cmd) {
        Ok(child) => child,
        Err(err) => {
            return StepStatus::Failed {
                detail: format!("failed to launch: {err}"),
            };
        }
    };
    // The child holds the slave; dropping ours lets the reader see EOF.
    drop(pty.slave);

    let reader = match pty.master.try_clone_reader() {
        Ok(reader) => reader,
        Err(err) => {
            return StepStatus::Failed {
                detail: format!("failed to read the pty: {err}"),
            };
        }
    };
    let mut writer = pty.master.take_writer().ok();
    let pump = spawn_pump(reader, events.clone());

    // Drain forwarded keys into the pty while the child runs.
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {}
            Err(err) => break Err(err),
        }
        match input.recv_timeout(std::time::Duration::from_millis(60)) {
            Ok(bytes) => {
                if let Some(w) = &mut writer {
                    let _ = w.write_all(&bytes);
                    let _ = w.flush();
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                break child.wait().map_err(std::io::Error::other);
            }
        }
    };
    drop(writer);
    drop(pty.master); // closes the pty — unblocks the pump on EOF/EIO
    let _ = pump.join();

    match status {
        Ok(status) => {
            if status.success() {
                StepStatus::Succeeded
            } else {
                StepStatus::Failed {
                    detail: format!("exit {}", status.exit_code()),
                }
            }
        }
        Err(err) => StepStatus::Failed {
            detail: format!("wait failed: {err}"),
        },
    }
}

fn spawn_pump(
    mut reader: Box<dyn Read + Send>,
    events: mpsc::Sender<ExecEvent>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                // 0 = EOF; Err = pty closed (EIO on Linux) — both are done.
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    if events.send(ExecEvent::Bytes(buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
            }
        }
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::model::{ActionKind, ActionStep, SourceId};
    use chrono::Utc;

    fn plan_for(script: &str) -> ActionPlan {
        ActionPlan {
            created_at: Utc::now(),
            steps: vec![ActionStep {
                source_id: SourceId::flatpak_user(),
                kind: ActionKind::Update,
                targets: vec!["x".to_string()],
                command: vec!["sh".to_string(), "-c".to_string(), script.to_string()],
            }],
            requires_sudo: false,
        }
    }

    fn drain(session: &ExecSession, answer: Option<(&str, &[u8])>) -> (String, ExecutionReport) {
        let mut text = String::new();
        let mut answered = false;
        loop {
            match session
                .events
                .recv_timeout(std::time::Duration::from_secs(10))
            {
                Ok(ExecEvent::Bytes(bytes)) => {
                    text.push_str(&String::from_utf8_lossy(&bytes));
                    if let Some((trigger, reply)) = answer
                        && !answered
                        && text.contains(trigger)
                    {
                        session.forward(reply.to_vec());
                        answered = true;
                    }
                }
                Ok(ExecEvent::Done(report)) => return (text, report),
                Ok(ExecEvent::Failed(err)) => panic!("session failed: {err}"),
                Err(err) => panic!("timed out: {err}; output so far: {text:?}"),
            }
        }
    }

    /// The keepalive must not fire for a run that never escalates — a plan of
    /// flatpak steps asks for no password, so priming one would be a prompt
    /// the run does not need (#24). The privileged path is not exercised here
    /// on purpose: it would ask the machine running the tests for a password.
    #[test]
    fn an_unprivileged_plan_is_never_primed() {
        let dir = std::env::temp_dir().join(format!("paclens-exec-noprime-{}", std::process::id()));
        let session = start(
            plan_for("echo done"),
            Some("sudo".to_string()),
            (24, 80),
            Some(dir.clone()),
            Some(std::time::Duration::from_secs(240)),
        );
        let (text, report) = drain(&session, None);
        assert!(
            !text.contains("sudo -v"),
            "primed a run with nothing privileged in it:\n{text}"
        );
        assert_eq!(report.succeeded(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End-to-end through a real pty: `sh` echoes, reads forwarded input
    /// (which the pty echoes back — a real terminal), and exits 0.
    #[test]
    fn session_streams_pty_output_and_forwards_keys() {
        let dir = std::env::temp_dir().join(format!("paclens-exec-tui-{}", std::process::id()));
        let session = start(
            plan_for("echo hello; read answer; echo got:$answer"),
            None,
            (24, 80),
            Some(dir.clone()),
            None,
        );
        let (text, report) = drain(&session, Some(("hello", b"world\r")));
        assert!(text.contains("hello"), "{text:?}");
        assert!(text.contains("got:world"), "{text:?}");
        assert_eq!(report.succeeded(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commands_see_a_real_terminal() {
        let dir = std::env::temp_dir().join(format!("paclens-exec-tty-{}", std::process::id()));
        let session = start(
            plan_for("if [ -t 0 ] && [ -t 1 ]; then echo IS_A_TTY; fi"),
            None,
            (24, 80),
            Some(dir.clone()),
            None,
        );
        let (text, report) = drain(&session, None);
        assert!(text.contains("IS_A_TTY"), "{text:?}");
        assert_eq!(report.succeeded(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failing_step_reports_its_exit_code() {
        let dir = std::env::temp_dir().join(format!("paclens-exec-fail-{}", std::process::id()));
        let session = start(plan_for("exit 3"), None, (24, 80), Some(dir.clone()), None);
        let (_, report) = drain(&session, None);
        assert_eq!(report.failed(), 1);
        assert_eq!(
            report.steps[0].status,
            StepStatus::Failed {
                detail: "exit 3".to_string()
            }
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
