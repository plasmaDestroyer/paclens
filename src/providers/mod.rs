//! Package source providers (pacman, flatpak) and the command-execution seam.
//!
//! `CommandRunner` is the injectable seam used for testing; `Provider` is the
//! per-source trait. Providers never call sudo and never know about each other
//! (design §6).
//!
//! Built in v0.0.2 (probing) and v0.0.3 (full `pacman -Qi` parser).

pub mod aur;
pub mod flatpak;
pub mod pacman;

use crate::model::{Package, PendingUpdate};

/// Captured result of running a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// The command-execution seam. In production this spawns the real binary
/// ([`SystemCommandRunner`]); in tests a mock returns fixture output.
///
/// An `Err` means the process could not be executed at all (e.g. binary not
/// found). A command that runs but exits non-zero is `Ok` with `exit_code` set
/// — providers decide whether that is a failure.
pub trait CommandRunner: Send + Sync {
    fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput>;
}

/// Runs commands via [`std::process::Command`], killing anything that runs
/// past the timeout (config `scan.provider_timeout_secs`; design §2 — a hung
/// `flatpak remote-ls` on an unreachable remote must never hang paclens).
pub struct SystemCommandRunner {
    timeout: std::time::Duration,
}

impl SystemCommandRunner {
    /// `timeout_secs == 0` disables the timeout.
    pub fn new(timeout_secs: u64) -> Self {
        SystemCommandRunner {
            timeout: std::time::Duration::from_secs(timeout_secs),
        }
    }
}

impl Default for SystemCommandRunner {
    fn default() -> Self {
        SystemCommandRunner::new(10)
    }
}

impl CommandRunner for SystemCommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
        use std::io::Read;
        use std::process::Stdio;

        let mut child = std::process::Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        // Drain the pipes on threads so a chatty child can't deadlock on a
        // full pipe while we wait.
        let drain = |reader: Option<Box<dyn Read + Send>>| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                if let Some(mut r) = reader {
                    let _ = r.read_to_end(&mut buf);
                }
                buf
            })
        };
        let stdout = drain(child.stdout.take().map(|s| Box::new(s) as _));
        let stderr = drain(child.stderr.take().map(|s| Box::new(s) as _));

        let started = std::time::Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if !self.timeout.is_zero() && started.elapsed() > self.timeout {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!(
                        "{program} exceeded the {}s provider timeout",
                        self.timeout.as_secs()
                    ),
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        };

        let stdout = stdout.join().unwrap_or_default();
        let stderr = stderr.join().unwrap_or_default();
        Ok(CommandOutput {
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            exit_code: status.code().unwrap_or(-1),
        })
    }
}

/// A package source. Scanning is always unprivileged; a provider never calls
/// sudo and never knows about other providers.
///
/// The update-related methods (`source_id`, `build_update_command`,
/// `requires_sudo_for_update` from design §10) are added with the executor in
/// v0.0.6 — this milestone only scans.
pub trait Provider {
    /// Is the source's binary present on PATH?
    fn is_available(&self) -> bool;
    /// Installed packages. `Ok(vec![])` when nothing is installed; `Err` only
    /// when the binary exists but the command failed.
    fn scan_installed(&self) -> Result<Vec<Package>, ProviderError>;
    /// Available updates. `Ok(vec![])` when none are pending.
    fn scan_updates(&self) -> Result<Vec<PendingUpdate>, ProviderError>;
}

/// A provider-level failure. App code wraps these in `anyhow` with context.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("failed to execute `{program}`: {source}")]
    Exec {
        program: String,
        #[source]
        source: std::io::Error,
    },
    #[error("`{program}` exited with code {exit_code}: {stderr}")]
    CommandFailed {
        program: String,
        exit_code: i32,
        stderr: String,
    },
}

/// Is `name` an executable file on any `PATH` entry?
pub fn binary_on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| is_executable(&dir.join(name))))
        .unwrap_or(false)
}

/// A regular file with at least one execute bit set.
///
/// The mode check is not pedantry: detection decides which AUR helper paclens
/// will hand a plan to, so a non-executable file named `paru` sitting on
/// `PATH` must not answer that question. `is_file()` alone would say yes.
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Test-only helpers shared by the provider submodule tests.
#[cfg(test)]
pub(crate) mod test_support {
    use super::{CommandOutput, CommandRunner};
    use std::collections::HashMap;

    /// Fixture-backed runner keyed by `"program arg1 arg2"` (design §12).
    pub(crate) struct MockRunner {
        responses: HashMap<String, CommandOutput>,
    }

    impl MockRunner {
        pub(crate) fn new() -> Self {
            Self {
                responses: HashMap::new(),
            }
        }

        /// Register stdout + exit code for a `"program args..."` invocation.
        pub(crate) fn with(mut self, key: &str, stdout: &str, exit_code: i32) -> Self {
            self.responses.insert(
                key.to_string(),
                CommandOutput {
                    stdout: stdout.to_string(),
                    stderr: String::new(),
                    exit_code,
                },
            );
            self
        }
    }

    impl CommandRunner for MockRunner {
        fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            let key = format!("{} {}", program, args.join(" "));
            self.responses.get(&key).cloned().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, format!("no mock for: {key}"))
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_on_path_finds_sh() {
        assert!(binary_on_path("sh"));
    }

    #[test]
    fn system_runner_captures_output_and_exit_code() {
        let runner = SystemCommandRunner::default();
        let out = runner
            .run("sh", &["-c", "echo hi; echo err >&2; exit 3"])
            .expect("runs");
        assert_eq!(out.stdout.trim(), "hi");
        assert_eq!(out.stderr.trim(), "err");
        assert_eq!(out.exit_code, 3);
    }

    #[test]
    fn system_runner_kills_a_hung_command_at_the_timeout() {
        let runner = SystemCommandRunner::new(1);
        let started = std::time::Instant::now();
        let err = runner.run("sleep", &["30"]).expect_err("must time out");
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "did not kill promptly"
        );
    }

    #[test]
    fn zero_timeout_means_no_timeout() {
        let runner = SystemCommandRunner::new(0);
        let out = runner
            .run("sh", &["-c", "sleep 0.1; echo done"])
            .expect("runs");
        assert_eq!(out.stdout.trim(), "done");
    }

    #[test]
    fn binary_on_path_rejects_nonsense() {
        assert!(!binary_on_path("paclens-definitely-not-a-real-binary"));
    }

    #[test]
    fn a_non_executable_file_is_not_a_binary() {
        // Detection decides which AUR helper gets handed a plan, so a plain
        // file named like one must not answer that question.
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("paclens-exec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("paru");
        std::fs::File::create(&path)
            .and_then(|mut f| f.write_all(b"not a program"))
            .expect("write");

        assert!(!is_executable(&path), "mode 0644 must not count");

        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        assert!(is_executable(&path), "mode 0755 must count");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_directory_is_not_a_binary() {
        // Directories carry execute bits meaning "traversable"; `/usr/bin/sh`
        // being a dir would not make `sh` runnable.
        assert!(!is_executable(std::path::Path::new("/usr/bin")));
    }

    #[test]
    fn system_runner_captures_stdout_and_zero_exit() {
        let out = SystemCommandRunner::default()
            .run("echo", &["hello"])
            .unwrap();
        assert_eq!(out.stdout.trim(), "hello");
        assert_eq!(out.exit_code, 0);
    }

    #[test]
    fn system_runner_reports_nonzero_exit() {
        let out = SystemCommandRunner::default().run("false", &[]).unwrap();
        assert_ne!(out.exit_code, 0);
    }

    #[test]
    fn system_runner_errors_when_binary_missing() {
        let result =
            SystemCommandRunner::default().run("paclens-definitely-not-a-real-binary", &[]);
        assert!(result.is_err());
    }

    #[test]
    fn provider_error_display_includes_context() {
        let failed = ProviderError::CommandFailed {
            program: "pacman -Qi".to_string(),
            exit_code: 1,
            stderr: "db locked".to_string(),
        };
        let text = failed.to_string();
        assert!(text.contains("pacman -Qi"));
        assert!(text.contains("db locked"));
    }
}
