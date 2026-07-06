//! The inline execution console's engine: runs a plan on a worker thread with
//! piped stdio, streaming every output line to the TUI and forwarding typed
//! keys to the running command's stdin — no terminal suspend (user request,
//! deviating from spec §13.2's suspend flow; recorded in dev-notes §7).
//!
//! sudo runs as `sudo -S`, which prompts on stderr and reads the password
//! from stdin — the prompt appears in the console and the typed password is
//! forwarded (never echoed, never stored, spec §13.5). pacman's own prompts
//! read stdin the same way. doas/pkexec cannot read a pipe, so callers keep
//! the suspend path for them.
//!
//! Logging matches `executor::execute` line-for-line (spec §14.4).

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;

use crate::executor::{
    self, ExecutionReport, StepReport, StepStatus, UpdateLog, skip_reason, target_noun,
};
use crate::model::ActionPlan;

/// What the worker streams back to the event loop.
pub enum ExecEvent {
    /// One line of command output (stdout and stderr interleaved).
    Line(String),
    /// The whole session finished; the report is final.
    Done(ExecutionReport),
    /// The session could not even start (e.g. the update log failed to open).
    Failed(String),
}

/// Handles to a running session: events out, forwarded stdin bytes in.
pub struct ExecSession {
    pub events: mpsc::Receiver<ExecEvent>,
    input: mpsc::Sender<Vec<u8>>,
}

impl ExecSession {
    /// Forward raw bytes (typed keys) to the running command's stdin.
    /// Best-effort: after the command exits there is nobody to receive them.
    pub fn forward(&self, bytes: Vec<u8>) {
        let _ = self.input.send(bytes);
    }
}

/// Adapt an effective command for pipe-driven execution: `sudo` becomes
/// `sudo -S -p <prompt>` so the password prompt lands on stderr (visible in
/// the console) and the password is read from the forwarded stdin.
pub fn pipe_command(effective: &[String]) -> Vec<String> {
    let mut argv = effective.to_vec();
    if argv.first().map(String::as_str) == Some("sudo") {
        argv.splice(
            1..1,
            [
                "-S".to_string(),
                "-p".to_string(),
                "[sudo] password (typed keys go to the command): ".to_string(),
            ],
        );
    }
    argv
}

/// Can the inline console drive this tool? Only sudo reads a piped stdin;
/// doas and pkexec need the real terminal (suspend path).
pub fn tool_supports_pipe(tool: Option<&str>) -> bool {
    matches!(tool, None | Some("sudo"))
}

/// Start the session on a worker thread. `log_dir` overrides the default
/// update-log location (the hermetic seam for tests).
pub fn start(
    plan: ActionPlan,
    tool: Option<String>,
    log_dir: Option<std::path::PathBuf>,
) -> ExecSession {
    let (event_tx, events) = mpsc::channel();
    let (input, input_rx) = mpsc::channel::<Vec<u8>>();

    std::thread::spawn(move || run_session(plan, tool, log_dir, event_tx, input_rx));

    ExecSession { events, input }
}

fn run_session(
    plan: ActionPlan,
    tool: Option<String>,
    log_dir: Option<std::path::PathBuf>,
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

    let mut steps = Vec::new();
    for step in &plan.steps {
        let targets = step.targets.len();
        if let Some(reason) = skip_reason(step, tool) {
            log.line(&format!("{}: skipped — {reason}", step.source_id));
            let _ = events.send(ExecEvent::Line(format!(
                ":: {} skipped — {reason}",
                step.source_id
            )));
            steps.push(StepReport {
                source_id: step.source_id.clone(),
                targets,
                status: StepStatus::Skipped {
                    reason: reason.to_string(),
                },
            });
            continue;
        }

        let argv = pipe_command(&executor::effective_command(step, tool));
        let cmd = argv.join(" ");
        log.line(&format!(
            "{}: running update ({})",
            step.source_id,
            target_noun(&step.source_id, targets)
        ));
        tracing::info!(source = %step.source_id, command = %cmd, "executing update step (inline)");
        let _ = events.send(ExecEvent::Line(format!(":: {cmd}")));

        let status = run_step(&argv, &events, &input);
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

/// Spawn one command with piped stdio; pump stdout+stderr lines to `events`
/// and forwarded bytes from `input` to its stdin until it exits.
fn run_step(
    argv: &[String],
    events: &mpsc::Sender<ExecEvent>,
    input: &mpsc::Receiver<Vec<u8>>,
) -> StepStatus {
    let Some((program, args)) = argv.split_first() else {
        return StepStatus::Failed {
            detail: "empty command".to_string(),
        };
    };
    let mut child = match Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            return StepStatus::Failed {
                detail: format!("failed to launch: {err}"),
            };
        }
    };

    let mut pumps = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        pumps.push(spawn_pump(stdout, events.clone()));
    }
    if let Some(stderr) = child.stderr.take() {
        pumps.push(spawn_pump(stderr, events.clone()));
    }
    let mut stdin = child.stdin.take();

    // Drain forwarded keys into the child while it runs.
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {}
            Err(err) => break Err(err),
        }
        match input.recv_timeout(std::time::Duration::from_millis(60)) {
            Ok(bytes) => {
                if let Some(pipe) = &mut stdin {
                    let _ = pipe.write_all(&bytes);
                    let _ = pipe.flush();
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break child.wait(),
        }
    };
    drop(stdin);
    for pump in pumps {
        let _ = pump.join();
    }

    match status {
        Ok(status) => match status.code() {
            Some(0) => StepStatus::Succeeded,
            Some(code) => StepStatus::Failed {
                detail: format!("exit {code}"),
            },
            None => StepStatus::Failed {
                detail: "terminated by signal".to_string(),
            },
        },
        Err(err) => StepStatus::Failed {
            detail: format!("wait failed: {err}"),
        },
    }
}

fn spawn_pump(
    reader: impl std::io::Read + Send + 'static,
    events: mpsc::Sender<ExecEvent>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            match line {
                Ok(line) => {
                    if events.send(ExecEvent::Line(line)).is_err() {
                        return;
                    }
                }
                Err(_) => return,
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

    #[test]
    fn pipe_command_adapts_sudo_to_stdin_password() {
        let argv = vec!["sudo".to_string(), "pacman".to_string(), "-Syu".to_string()];
        let piped = pipe_command(&argv);
        assert_eq!(piped[0], "sudo");
        assert_eq!(piped[1], "-S");
        assert_eq!(piped[2], "-p");
        assert_eq!(piped[4], "pacman");
        // Non-sudo commands pass through untouched.
        let plain = vec!["flatpak".to_string(), "update".to_string()];
        assert_eq!(pipe_command(&plain), plain);
    }

    #[test]
    fn only_sudo_or_no_tool_supports_the_pipe() {
        assert!(tool_supports_pipe(Some("sudo")));
        assert!(tool_supports_pipe(None));
        assert!(!tool_supports_pipe(Some("doas")));
        assert!(!tool_supports_pipe(Some("pkexec")));
    }

    /// End-to-end through a real child process: `sh` echoes, reads forwarded
    /// stdin, and exits 0 — output lines and the final report both arrive.
    #[test]
    fn session_streams_output_and_forwards_stdin() {
        let plan = ActionPlan {
            created_at: Utc::now(),
            steps: vec![ActionStep {
                source_id: SourceId::flatpak_user(),
                kind: ActionKind::Update,
                targets: vec!["x".to_string()],
                command: vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "echo hello; read answer; echo got:$answer".to_string(),
                ],
            }],
            requires_sudo: false,
        };
        let dir = std::env::temp_dir().join(format!("paclens-exec-tui-{}", std::process::id()));
        let session = start(plan, None, Some(dir.clone()));

        // Wait for the first output line, then answer the `read`.
        let mut lines = Vec::new();
        loop {
            match session
                .events
                .recv_timeout(std::time::Duration::from_secs(5))
            {
                Ok(ExecEvent::Line(line)) => {
                    if line == "hello" {
                        session.forward(b"world\n".to_vec());
                    }
                    lines.push(line);
                }
                Ok(ExecEvent::Done(report)) => {
                    assert_eq!(report.succeeded(), 1);
                    break;
                }
                Ok(ExecEvent::Failed(err)) => panic!("session failed: {err}"),
                Err(err) => panic!("timed out: {err}; lines so far: {lines:?}"),
            }
        }
        assert!(lines.contains(&"hello".to_string()), "{lines:?}");
        assert!(lines.contains(&"got:world".to_string()), "{lines:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failing_step_reports_its_exit_code() {
        let plan = ActionPlan {
            created_at: Utc::now(),
            steps: vec![ActionStep {
                source_id: SourceId::flatpak_user(),
                kind: ActionKind::Update,
                targets: vec!["x".to_string()],
                command: vec!["sh".to_string(), "-c".to_string(), "exit 3".to_string()],
            }],
            requires_sudo: false,
        };
        let dir = std::env::temp_dir().join(format!("paclens-exec-fail-{}", std::process::id()));
        let session = start(plan, None, Some(dir.clone()));
        loop {
            match session
                .events
                .recv_timeout(std::time::Duration::from_secs(5))
            {
                Ok(ExecEvent::Done(report)) => {
                    assert_eq!(report.failed(), 1);
                    assert_eq!(
                        report.steps[0].status,
                        StepStatus::Failed {
                            detail: "exit 3".to_string()
                        }
                    );
                    break;
                }
                Ok(_) => {}
                Err(err) => panic!("timed out: {err}"),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
