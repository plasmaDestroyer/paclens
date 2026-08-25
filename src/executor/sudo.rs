//! Privilege escalation model (design §11): check for `sudo`, `doas`,
//! `pkexec` in that order and use the first one found. If none exists,
//! privileged steps are skipped with a clear reason — paclens never guesses,
//! never caches credentials, never runs a privileged daemon (design §11).

const CANDIDATES: [&str; 3] = ["sudo", "doas", "pkexec"];

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
    fn detect_finds_a_tool_on_a_normal_system() {
        // Dev machines running this suite have at least one of the three;
        // the assertion is only that probing PATH does not panic.
        let _ = detect();
    }
}
