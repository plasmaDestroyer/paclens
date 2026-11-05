//! How far behind Arch the mirror pacman asks first for `core` is (#6).
//!
//! Two markers, both written by Arch's master on its own clock: `lastsync`,
//! the time it writes every minute and every mirror copies when it syncs, so
//! a mirror's copy says when its tree was taken; and the master's
//! `lastupdate`, the time of its newest change, at [`REFERENCE`] (the master
//! serves no `lastsync` over HTTPS). Two `curl`s through the `CommandRunner`
//! seam, like the news feed, and no HTTP crate. What the gap means is the
//! analyzer's call; this only reads the two numbers.

use serde::{Deserialize, Serialize};

use super::CommandRunner;

/// The master's own marker, the one tier 1 mirrors sync against.
pub const REFERENCE: &str = "https://rsync.archlinux.org/lastupdate";

/// A marker more than this far past the local clock is not a time at all.
const FUTURE_SLACK: i64 = 86_400;

/// The two markers, as read. `None` is a read that gave no usable number:
/// unreachable, an HTTP error, or a body that is not a plausible Unix time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorCheck {
    /// The first `core` server, as `pacman-conf` expands it.
    pub server: String,
    /// The mirror's `lastsync`.
    pub mirror: Option<i64>,
    /// The master's `lastupdate`.
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
    let now = chrono::Utc::now().timestamp();
    let read = |url: &str| fetch(runner, url, timeout_secs).and_then(|b| parse_stamp(&b, now));
    let mirror = base(&server).and_then(|b| read(&format!("{b}lastsync")));
    let reference = read(REFERENCE);
    Some(MirrorCheck {
        server,
        mirror,
        reference,
    })
}

/// The mirror root a `core` server URL sits under: everything before
/// `core/os/<arch>`. Any other layout is not guessed at.
pub fn base(server: &str) -> Option<&str> {
    let (rest, arch) = server.trim_end_matches('/').rsplit_once('/')?;
    let rest = rest.strip_suffix("/core/os")?;
    if arch.is_empty() {
        return None;
    }
    Some(&server[..=rest.len()])
}

/// The body of one small read. A timeout of 0 means none, as it does for
/// every other command (`SystemCommandRunner`).
fn fetch(runner: &dyn CommandRunner, url: &str, timeout_secs: u64) -> Option<String> {
    let timeout = timeout_secs.to_string();
    let mut args = vec!["-fsSL", "--max-filesize", "64"];
    if timeout_secs > 0 {
        args.extend(["--max-time", &timeout]);
    }
    args.push(url);
    let out = runner
        .run("curl", &args)
        .ok()
        .filter(|o| o.exit_code == 0)?;
    Some(out.stdout)
}

/// One decimal Unix time and nothing else. An error page, a stray field, a
/// number from before 2001 or one more than a day past `now` is no answer
/// rather than a huge gap.
pub fn parse_stamp(body: &str, now: i64) -> Option<i64> {
    let s = body.trim();
    if s.is_empty() || s.len() > 12 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse()
        .ok()
        .filter(|&t| t >= 1_000_000_000 && t <= now + FUTURE_SLACK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::test_support::MockRunner;

    const SERVERS: &str = include_str!("../../tests/fixtures/pacman/pacman-conf-core-server.txt");
    const CURL: &str = "curl -fsSL --max-filesize 64 --max-time 10";
    const NOW: i64 = 1_793_903_513;

    #[test]
    fn a_stamp_is_one_decimal_unix_time() {
        assert_eq!(parse_stamp("1793900867\n", NOW), Some(1_793_900_867));
        assert_eq!(parse_stamp("  1793900867  ", NOW), Some(1_793_900_867));
        // A mirror a little ahead of this clock is still a time.
        assert_eq!(
            parse_stamp(&(NOW + 3600).to_string(), NOW),
            Some(NOW + 3600)
        );
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
            "999999999999",
        ] {
            assert_eq!(parse_stamp(bad, NOW), None, "{bad:?}");
        }
    }

    #[test]
    fn more_than_a_day_past_now_is_no_stamp() {
        assert_eq!(parse_stamp(&(NOW + 86_401).to_string(), NOW), None);
        assert_eq!(
            parse_stamp(&(NOW + 86_400).to_string(), NOW),
            Some(NOW + 86_400)
        );
    }

    #[test]
    fn base_strips_core_os_arch() {
        assert_eq!(
            base("https://archlinux.cachyos.org/repo/core/os/x86_64"),
            Some("https://archlinux.cachyos.org/repo/")
        );
        assert_eq!(
            base("https://geo.mirror.pkgbuild.com/core/os/x86_64/"),
            Some("https://geo.mirror.pkgbuild.com/")
        );
        assert_eq!(
            base("file:///srv/mirror/core/os/x86_64"),
            Some("file:///srv/mirror/")
        );
    }

    #[test]
    fn any_other_layout_has_no_base() {
        for s in [
            "https://x.org/arch/",
            "https://x.org/core/os/",
            "https://x.org/extra/os/x86_64",
            "https://x.org/mycore/os/x86_64",
            "",
        ] {
            assert_eq!(base(s), None, "{s:?}");
        }
    }

    #[test]
    fn a_zero_timeout_passes_no_max_time() {
        let runner = MockRunner::new().with(
            &format!("curl -fsSL --max-filesize 64 {REFERENCE}"),
            "1793902445\n",
            0,
        );
        assert_eq!(
            fetch(&runner, REFERENCE, 0).as_deref(),
            Some("1793902445\n")
        );
    }

    #[test]
    fn both_markers_are_read_from_the_first_core_server() {
        let runner = MockRunner::new()
            .with("pacman-conf --repo=core Server", SERVERS, 0)
            .with(
                &format!("{CURL} https://archlinux.cachyos.org/repo/lastsync"),
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
                &format!("{CURL} https://archlinux.cachyos.org/repo/lastsync"),
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
