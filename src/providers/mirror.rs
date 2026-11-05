//! How far behind Arch the mirror pacman asks first for `core` is (#6).
//!
//! Every Arch mirror carries `lastupdate`, a Unix time the master writes when
//! its tree changes and mirrors copy verbatim; Arch's own master serves the
//! same file at [`REFERENCE`]. Two `curl`s through the `CommandRunner` seam,
//! like the news feed, and no HTTP crate. What the gap means is the
//! analyzer's call; this only reads the two numbers.

use serde::{Deserialize, Serialize};

use super::CommandRunner;

/// The master's own marker, the one tier 1 mirrors sync against.
pub const REFERENCE: &str = "https://rsync.archlinux.org/lastupdate";

/// The two markers, as read. `None` is a read that gave no usable number:
/// unreachable, an HTTP error, or a body that is not a Unix time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorCheck {
    /// The first `core` server, as `pacman-conf` expands it.
    pub server: String,
    pub mirror: Option<i64>,
    pub reference: Option<i64>,
}

/// Read both markers. `None` when there is no `core` server to check.
pub fn check(runner: &dyn CommandRunner, timeout_secs: u64) -> Option<MirrorCheck> {
    let out = runner
        .run("pacman-conf", &["--repo=core", "Server"])
        .ok()
        .filter(|o| o.exit_code == 0)?;
    let server = out.stdout.lines().next()?.trim().to_string();
    if server.is_empty() {
        return None;
    }
    // ponytail: sequential; offline both fail on DNS at once, online each
    // is one tiny file. Two timeouts is the worst case.
    let mirror = crate::analyzer::mirror::base(&server)
        .and_then(|b| fetch(runner, &format!("{b}lastupdate"), timeout_secs));
    let reference = fetch(runner, REFERENCE, timeout_secs);
    Some(MirrorCheck {
        server,
        mirror,
        reference,
    })
}

fn fetch(runner: &dyn CommandRunner, url: &str, timeout_secs: u64) -> Option<i64> {
    let timeout = timeout_secs.max(1).to_string();
    let out = runner
        .run(
            "curl",
            &["-fsSL", "--max-time", &timeout, "--max-filesize", "64", url],
        )
        .ok()
        .filter(|o| o.exit_code == 0)?;
    parse_stamp(&out.stdout)
}

/// One decimal Unix time and nothing else. An error page, a stray field or
/// a number from before 2001 is no answer rather than a huge gap.
pub fn parse_stamp(body: &str) -> Option<i64> {
    let s = body.trim();
    if s.is_empty() || s.len() > 12 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok().filter(|&t| t >= 1_000_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::test_support::MockRunner;

    const SERVERS: &str = include_str!("../../tests/fixtures/pacman/pacman-conf-core-server.txt");
    const CURL: &str = "curl -fsSL --max-time 10 --max-filesize 64";

    #[test]
    fn a_stamp_is_one_decimal_unix_time() {
        assert_eq!(parse_stamp("1793900867\n"), Some(1_793_900_867));
        assert_eq!(parse_stamp("  1793900867  "), Some(1_793_900_867));
    }

    #[test]
    fn anything_else_is_no_stamp() {
        for bad in [
            "",
            "\n",
            "<html>404</html>",
            "1793900867 1793900000",
            "-1793900867",
            "17939008.67",
            "1",
            "0",
            "9999999999999",
        ] {
            assert_eq!(parse_stamp(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn both_markers_are_read_from_the_first_core_server() {
        let runner = MockRunner::new()
            .with("pacman-conf --repo=core Server", SERVERS, 0)
            .with(
                &format!("{CURL} https://archlinux.cachyos.org/repo/lastupdate"),
                "1793900867\n",
                0,
            )
            .with(&format!("{CURL} {REFERENCE}"), "1793902445\n", 0);
        assert_eq!(
            check(&runner, 10),
            Some(MirrorCheck {
                server: "https://archlinux.cachyos.org/repo/core/os/x86_64".into(),
                mirror: Some(1_793_900_867),
                reference: Some(1_793_902_445),
            })
        );
    }

    #[test]
    fn offline_is_a_check_with_no_numbers() {
        let runner = MockRunner::new().with("pacman-conf --repo=core Server", SERVERS, 0);
        let c = check(&runner, 10).unwrap();
        assert_eq!((c.mirror, c.reference), (None, None));
    }

    #[test]
    fn a_failed_http_read_is_no_number_even_with_a_body() {
        let runner = MockRunner::new()
            .with("pacman-conf --repo=core Server", SERVERS, 0)
            .with(
                &format!("{CURL} https://archlinux.cachyos.org/repo/lastupdate"),
                "1793900867\n",
                22,
            );
        assert_eq!(check(&runner, 10).unwrap().mirror, None);
    }

    #[test]
    fn no_core_server_is_no_check() {
        assert_eq!(check(&MockRunner::new(), 10), None);
        let empty = MockRunner::new().with("pacman-conf --repo=core Server", "", 0);
        assert_eq!(check(&empty, 10), None);
        let failed = MockRunner::new().with("pacman-conf --repo=core Server", SERVERS, 1);
        assert_eq!(check(&failed, 10), None);
    }
}
