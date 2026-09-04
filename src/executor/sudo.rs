//! Privilege escalation model (design §11): check for `sudo`, `doas`,
//! `pkexec` in that order and use the first one found. If none exists,
//! privileged steps are skipped with a clear reason — paclens never guesses,
//! never caches credentials, never runs a privileged daemon (design §11).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const CANDIDATES: [&str; 3] = ["sudo", "doas", "pkexec"];

/// Does a refresh loop apply to this run (#24)?
///
/// `sudo -v` semantics are sudo's own. `doas` has no timestamp refresh —
/// persistence there is a `persist` option the user sets in `doas.conf` — and
/// `pkexec` authenticates through polkit, which has no timestamp at all. On
/// those the honest answer is to do nothing rather than run a subprocess every
/// four minutes that cannot help.
pub fn keepalive_applies(tool: Option<&str>, enabled: bool, plan_is_privileged: bool) -> bool {
    enabled && plan_is_privileged && matches!(tool, Some("sudo"))
}

/// The command that authenticates without running anything, so the one prompt
/// of the run happens where the reader is looking: before the first step,
/// rather than deep inside a build's output.
pub fn prime_command() -> Vec<String> {
    vec!["sudo".to_string(), "-v".to_string()]
}

/// Keeps the sudo timestamp warm while a run executes, and stops the moment
/// the run does.
///
/// The trade is real and is why this is off by default: for as long as the
/// loop runs, anything running as this user can use sudo without a prompt. It
/// buys exactly one thing — a long AUR build no longer strands the run on a
/// password prompt nobody is watching, because paru's own escalation finds a
/// valid timestamp (paru is never run under sudo itself; decision 2026-07-12).
pub struct Keepalive {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Keepalive {
    /// Refresh every `interval` until dropped. `refresh` returns whether the
    /// timestamp could be refreshed without a prompt; the first failure ends
    /// the loop rather than repeating it — a system with
    /// `timestamp_timeout=0` prompts every time and cannot be kept warm, and
    /// looping there would be a subprocess every four minutes for nothing.
    pub fn start_with(interval: Duration, refresh: impl Fn() -> bool + Send + 'static) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            // Wake often, act rarely: a sleeping thread that checks the flag
            // every 200ms stops with the run instead of up to `interval`
            // after it.
            let tick = Duration::from_millis(200);
            let mut waited = Duration::ZERO;
            while !flag.load(Ordering::Relaxed) {
                std::thread::sleep(tick);
                waited += tick;
                if waited < interval {
                    continue;
                }
                waited = Duration::ZERO;
                if !refresh() {
                    tracing::info!("sudo timestamp could not be refreshed; keepalive stopping");
                    return;
                }
            }
        });
        Keepalive {
            stop,
            handle: Some(handle),
        }
    }

    /// The real thing: `sudo -n -v` refreshes the timestamp and never prompts,
    /// so a lost timestamp ends the loop instead of stealing the terminal.
    pub fn start(interval: Duration) -> Self {
        Self::start_with(interval, || {
            std::process::Command::new("sudo")
                .args(["-n", "-v"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        })
    }
}

impl Drop for Keepalive {
    /// Every exit path — success, failure, interrupt — goes through here,
    /// because the run owns the value.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// The first candidate that `available` accepts — the pure core, with the
/// PATH probe injected for hermetic tests.
pub fn pick(available: impl Fn(&str) -> bool) -> Option<&'static str> {
    CANDIDATES.into_iter().find(|tool| available(tool))
}

/// Probe PATH for the first available privilege tool.
pub fn detect() -> Option<&'static str> {
    pick(on_path)
}

fn on_path(bin: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| is_executable(&dir.join(bin)))
}

fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_in_spec_order() {
        assert_eq!(pick(|_| true), Some("sudo"));
        assert_eq!(pick(|t| t != "sudo"), Some("doas"));
        assert_eq!(pick(|t| t == "pkexec"), Some("pkexec"));
        assert_eq!(pick(|_| false), None);
    }

    #[test]
    fn the_keepalive_is_sudo_only_and_opt_in() {
        assert!(keepalive_applies(Some("sudo"), true, true));
        assert!(
            !keepalive_applies(Some("sudo"), false, true),
            "off by default"
        );
        assert!(
            !keepalive_applies(Some("sudo"), true, false),
            "nothing privileged to keep warm"
        );
        // doas persists via its own config, pkexec through polkit: neither has
        // a timestamp this could refresh.
        assert!(!keepalive_applies(Some("doas"), true, true));
        assert!(!keepalive_applies(Some("pkexec"), true, true));
        assert!(!keepalive_applies(None, true, true));
    }

    #[test]
    fn the_loop_refreshes_until_dropped() {
        use std::sync::atomic::AtomicUsize;
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        let keepalive = Keepalive::start_with(Duration::from_millis(200), move || {
            seen.fetch_add(1, Ordering::Relaxed);
            true
        });
        std::thread::sleep(Duration::from_millis(700));
        drop(keepalive); // joins the thread
        let after_drop = calls.load(Ordering::Relaxed);
        assert!(
            after_drop >= 2,
            "expected repeated refreshes, got {after_drop}"
        );
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(
            calls.load(Ordering::Relaxed),
            after_drop,
            "the loop kept running after the run ended"
        );
    }

    #[test]
    fn a_refusal_ends_the_loop_rather_than_repeating_it() {
        use std::sync::atomic::AtomicUsize;
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        // timestamp_timeout=0: every refresh needs a password, so `sudo -n -v`
        // always fails.
        let keepalive = Keepalive::start_with(Duration::from_millis(200), move || {
            seen.fetch_add(1, Ordering::Relaxed);
            false
        });
        std::thread::sleep(Duration::from_millis(900));
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "one failure is the answer; repeating it is noise"
        );
        drop(keepalive);
    }

    #[test]
    fn detect_finds_a_tool_on_a_normal_system() {
        // Dev machines running this suite have at least one of the three;
        // the assertion is only that probing PATH does not panic.
        let _ = detect();
    }
}
