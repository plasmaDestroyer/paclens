//! Rust toolchains managed by rustup (#12). rustup installs them and rustup
//! updates them, so it is one source. Everything lives under `$HOME`: no step
//! it produces is privileged.
//!
//! `rustup toolchain list` says what is installed; `rustup check` says what
//! each has and what is available, and is the network half.

use crate::model::{InstallReason, Package, PendingUpdate, SourceId};
use crate::providers::{CommandRunner, ProviderError};

pub const RUSTUP_BIN: &str = "rustup";

/// The toolchain name without its host triple: `stable-x86_64-unknown-
/// linux-gnu` → `stable`. A name without one is left as it is.
fn short(toolchain: &str) -> &str {
    toolchain
        .find("-x86_64-")
        .or_else(|| toolchain.find("-aarch64-"))
        .map_or(toolchain, |i| &toolchain[..i])
}

/// Parse `rustup toolchain list`: one toolchain per line, maybe followed by
/// `(active, default)`.
pub fn parse_toolchains(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|t| !t.is_empty() && *t != "no")
        .map(|t| short(t).to_string())
        .collect()
}

/// One `rustup check` line: `(name, current, available)`, available `None`
/// when up to date. `rustup` itself appears as a line of its own.
fn parse_check_line(line: &str) -> Option<(String, String, Option<String>)> {
    let (name, rest) = line.split_once(" - ")?;
    let (state, versions) = rest.split_once(':')?;
    let ver = |s: &str| s.split_whitespace().next().map(String::from);
    if state.trim().eq_ignore_ascii_case("update available") {
        let (from, to) = versions.split_once("->")?;
        Some((short(name.trim()).to_string(), ver(from)?, ver(to)))
    } else {
        Some((short(name.trim()).to_string(), ver(versions)?, None))
    }
}

/// The toolchains as packages, with versions from `rustup check` where it
/// named them, and the updates it found.
pub fn assemble(toolchains: &[String], check: &str) -> (Vec<Package>, Vec<PendingUpdate>) {
    let lines: Vec<_> = check.lines().filter_map(parse_check_line).collect();
    let package = |name: &str, version: &str| Package {
        name: name.to_string(),
        version: version.to_string(),
        source_id: SourceId::rustup(),
        // Every toolchain was asked for by name.
        install_reason: InstallReason::Explicit,
        size_bytes: None,
        description: None,
        depends_on: Vec::new(),
        required_by: Vec::new(),
        optional_deps: Vec::new(),
        provides: Vec::new(),
        runtime: false,
        scope: None,
        foreign: false,
        signed: false,
        packager: None,
        repo_version: None,
    };
    let packages = toolchains
        .iter()
        .map(|t| {
            let version = lines
                .iter()
                .find(|(n, _, _)| n == t)
                .map_or(t.as_str(), |(_, v, _)| v.as_str());
            package(t, version)
        })
        .collect();
    let updates = lines
        .iter()
        .filter_map(|(name, from, to)| {
            Some(PendingUpdate {
                package_name: name.clone(),
                current_version: from.clone(),
                available_version: to.clone()?,
                source_id: SourceId::rustup(),
            })
        })
        .collect();
    (packages, updates)
}

/// List the toolchains and check them. `rustup check` exits 100 when updates
/// are available, which is an answer, not a failure.
pub fn scan(
    runner: &dyn CommandRunner,
) -> Result<(Vec<Package>, Vec<PendingUpdate>), ProviderError> {
    let run = |args: &[&str]| {
        runner
            .run(RUSTUP_BIN, args)
            .map_err(|source| ProviderError::Exec {
                program: RUSTUP_BIN.to_string(),
                source,
            })
    };
    let list = run(&["toolchain", "list"])?;
    if list.exit_code != 0 {
        return Err(ProviderError::CommandFailed {
            program: "rustup toolchain list".to_string(),
            exit_code: list.exit_code,
            stderr: list.stderr,
        });
    }
    let check = run(&["check"])?;
    if check.exit_code != 0 && check.exit_code != 100 {
        return Err(ProviderError::CommandFailed {
            program: "rustup check".to_string(),
            exit_code: check.exit_code,
            stderr: check.stderr,
        });
    }
    Ok(assemble(&parse_toolchains(&list.stdout), &check.stdout))
}

/// `rustup update`: every toolchain, and rustup itself. Asks nothing.
pub fn update_command() -> Vec<String> {
    vec![RUSTUP_BIN.to_string(), "update".to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str =
        "stable-x86_64-unknown-linux-gnu (active, default)\n1.92.0-x86_64-unknown-linux-gnu\n";
    const CHECK: &str = "stable-x86_64-unknown-linux-gnu - update available: 1.96.0 (ac68faa20 2026-05-25) -> 1.99.0 (b940084d7 2026-09-28)\n1.92.0-x86_64-unknown-linux-gnu - Up to date : 1.92.0 (ded5c06cf 2025-12-08)\nrustup - update available : 1.29.0 -> 1.29.1\n";

    #[test]
    fn toolchains_list_without_their_host_triple() {
        assert_eq!(parse_toolchains(LIST), vec!["stable", "1.92.0"]);
    }

    #[test]
    fn check_gives_versions_and_updates_including_rustup_itself() {
        let (pkgs, ups) = assemble(&parse_toolchains(LIST), CHECK);
        assert_eq!(pkgs.len(), 2);
        assert_eq!(pkgs[0].name, "stable");
        assert_eq!(pkgs[0].version, "1.96.0");
        assert_eq!(pkgs[1].version, "1.92.0");
        let names: Vec<_> = ups.iter().map(|u| u.package_name.as_str()).collect();
        assert_eq!(names, vec!["stable", "rustup"]);
        assert_eq!(ups[0].available_version, "1.99.0");
    }

    #[test]
    fn exit_100_means_updates_not_failure() {
        let runner = crate::providers::test_support::MockRunner::new()
            .with("rustup toolchain list", LIST, 0)
            .with("rustup check", CHECK, 100);
        let (pkgs, ups) = scan(&runner).expect("100 is an answer");
        assert_eq!((pkgs.len(), ups.len()), (2, 2));
    }
}
