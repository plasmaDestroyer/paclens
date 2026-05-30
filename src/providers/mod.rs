//! Package source providers (pacman, flatpak) and the command-execution seam.
//!
//! `CommandRunner` is the injectable seam used for testing; `Provider` is the
//! per-source trait. Providers never call sudo and never know about each other
//! (dev-notes §3).
//!
//! Built in v0.0.2 (probing) and v0.0.3 (full `pacman -Qi` parser).

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

/// Runs commands via [`std::process::Command`].
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
        let output = std::process::Command::new(program).args(args).output()?;
        Ok(CommandOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code().unwrap_or(-1),
        })
    }
}

/// A package source. Scanning is always unprivileged; a provider never calls
/// sudo and never knows about other providers.
///
/// The update-related methods (`source_id`, `build_update_command`,
/// `requires_sudo_for_update` from spec §5.1) are added with the executor in
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
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
        .unwrap_or(false)
}

/// Test-only helpers shared by the provider submodule tests.
#[cfg(test)]
pub(crate) mod test_support {
    use super::{CommandOutput, CommandRunner};
    use std::collections::HashMap;

    /// Fixture-backed runner keyed by `"program arg1 arg2"` (dev-notes §8).
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
    fn binary_on_path_rejects_nonsense() {
        assert!(!binary_on_path("paclens-definitely-not-a-real-binary"));
    }

    #[test]
    fn system_runner_captures_stdout_and_zero_exit() {
        let out = SystemCommandRunner.run("echo", &["hello"]).unwrap();
        assert_eq!(out.stdout.trim(), "hello");
        assert_eq!(out.exit_code, 0);
    }

    #[test]
    fn system_runner_reports_nonzero_exit() {
        let out = SystemCommandRunner.run("false", &[]).unwrap();
        assert_ne!(out.exit_code, 0);
    }

    #[test]
    fn system_runner_errors_when_binary_missing() {
        let result = SystemCommandRunner.run("paclens-definitely-not-a-real-binary", &[]);
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
