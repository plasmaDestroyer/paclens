//! Privilege escalation model (design §11): check for `sudo`, `doas`,
//! `pkexec` in that order and use the first one found. If none exists,
//! privileged steps are skipped with a clear reason — paclens never guesses,
//! never caches credentials, never runs a privileged daemon (design §11).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const CANDIDATES: [&str; 3] = ["sudo", "doas", "pkexec"];

/// Will something in this plan ask for a password? Then one `sudo -v`
/// before the first step answers it for the whole run (2026-09-21).
///
/// A helper escalates on its own, so an AUR step needs a password without
/// being privileged. sudo only: `doas` and `pkexec` have no "authenticate
/// without running anything" that a later command then finds satisfied —
/// and no timestamp a keepalive could refresh either.
pub fn worth_priming(plan: &crate::model::ActionPlan, tool: Option<&str>) -> bool {
    tool == Some("sudo")
        && plan.steps.iter().any(|st| {
            super::skip_reason(st, tool).is_none()
                && (st.privileged || st.source_id.as_str() == "aur")
        })
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
    pick(crate::providers::binary_on_path)
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
