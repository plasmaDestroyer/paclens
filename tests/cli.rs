//! Integration tests for the `paclens` binary's CLI contract.
//!
//! These run the compiled binary end to end. They stay deterministic by
//! exercising only behavior that does not depend on the host's packages
//! (`--version`, `--help`, not-yet-implemented subcommands), and redirect all
//! XDG dirs into a temp directory so config/cache/log files never touch the
//! real home directory. Scan-dependent behavior is covered by hermetic unit
//! tests with a mock CommandRunner.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Path to the binary under test, provided by Cargo for integration tests.
const BIN: &str = env!("CARGO_BIN_EXE_paclens");

/// A unique temp dir used to sandbox XDG paths for one test.
fn sandbox(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("paclens-it-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create sandbox");
    dir
}

/// Run the binary with XDG dirs pointed at `home`.
fn run(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("XDG_DATA_HOME", home.join("data"))
        .output()
        .expect("run paclens")
}

#[test]
fn version_flag_prints_version_and_succeeds() {
    let home = sandbox("version");
    let out = run(&home, &["--version"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "stdout was: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn help_flag_prints_usage_and_succeeds() {
    let home = sandbox("help");
    let out = run(&home, &["--help"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Usage"), "stdout was: {stdout}");
    // Every planned subcommand should be advertised.
    for sub in ["status", "update", "why", "overlaps", "cleanup"] {
        assert!(stdout.contains(sub), "help missing subcommand: {sub}");
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn unimplemented_subcommand_fails_with_a_clear_message() {
    let home = sandbox("why");
    let out = run(&home, &["why", "firefox"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not implemented"), "stderr was: {stderr}");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn unknown_subcommand_is_rejected() {
    let home = sandbox("unknown");
    let out = run(&home, &["frobnicate"]);
    assert!(!out.status.success());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn status_no_color_is_plain_and_succeeds() {
    // `status` scans the host (or finds nothing on a non-Arch runner) and prints
    // the summary. With --no-color and captured (non-TTY) stdout, the output must
    // carry the headline and contain no ANSI escape codes.
    let home = sandbox("status");
    let out = run(&home, &["status", "--no-color"]);
    assert!(out.status.success(), "status should exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("paclens"), "stdout was: {stdout}");
    assert!(
        !stdout.contains('\u{1b}'),
        "no-color output must not contain ANSI escapes: {stdout:?}"
    );
    let _ = std::fs::remove_dir_all(&home);
}
