//! Crates installed with `cargo install`, from cargo's own record.
//!
//! The inventory is `~/.cargo/.crates.toml`, not `~/.cargo/bin`: most of what
//! is in that directory belongs to rustup (`cargo`, `rustc`, `clippy-driver`,
//! `rust-analyzer`), and claiming those would have paclens offering to
//! `cargo install` things cargo never installed. Cargo's own record is also
//! cheaper than `cargo install --list` — no subprocess (user decision
//! 2026-09-12).
//!
//! `.crates.toml` rather than the newer `.crates2.json` for one reason: it
//! carries the same three facts this needs — the package id, and the binaries
//! each crate installed — and it is TOML, which is already a dependency here.
//! A JSON parser would be a new dependency bought for one file. Cargo writes
//! both, with the same mtime; if it ever stops writing this one the fixture
//! test is what will say so.
//!
//! Everything here lives under `$HOME`, so no step this source produces is
//! ever privileged.
//!
//! Pure over a `&str` (the `staleness_with` pattern): the caller reads the
//! file, this reads the text.

use serde::Deserialize;

use crate::model::{InstallReason, Package, PendingUpdate, SourceId};
use crate::providers::{CommandRunner, ProviderError};

/// The binary that makes the source meaningful — without it nothing here can
/// be updated or removed.
pub const CARGO_BIN: &str = "cargo";
/// `cargo-update`'s binary. Optional, and its absence is what turns update
/// detection off (the same shape as the aur source without a helper).
pub const INSTALL_UPDATE_BIN: &str = "cargo-install-update";

/// Where a crate came from, which decides whether it has an upstream version
/// to be compared against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// From crates.io or another registry: comparable.
    Registry,
    /// `cargo install --path`, or a git checkout. There is no published
    /// version to compare with, so "up to date" is not a claim anyone can
    /// make about it — the same shape as an AUR `-git` package.
    Local,
}

/// `.crates.toml`: one `[v1]` table of package id → the binaries it installed.
#[derive(Deserialize)]
struct CratesToml {
    #[serde(default)]
    v1: std::collections::HashMap<String, Vec<String>>,
}

/// Split `"ripgrep 14.1.1 (registry+https://...)"` into its three parts.
///
/// The key is cargo's own `PackageId` rendering. It has been this shape for
/// years but carries no compatibility promise, so a key that does not parse is
/// skipped rather than guessed at — the alternative is inventing a crate.
fn parse_key(key: &str) -> Option<(String, String, Origin)> {
    let (name, rest) = key.split_once(' ')?;
    let (version, source) = rest.split_once(' ')?;
    let source = source.strip_prefix('(')?.strip_suffix(')')?;
    let origin = if source.starts_with("registry+") {
        Origin::Registry
    } else {
        Origin::Local
    };
    (!name.is_empty() && !version.is_empty())
        .then(|| (name.to_string(), version.to_string(), origin))
}

/// Parse `.crates.toml` into packages, sorted by name.
///
/// The binaries a crate provides land in `provides`: that is what the user
/// types, and what an overlap against a pacman package of the same tool would
/// match on (#18).
pub fn parse_installed(text: &str) -> Result<Vec<Package>, ProviderError> {
    let parsed: CratesToml = toml::from_str(text).map_err(|err| ProviderError::CommandFailed {
        program: "cargo".to_string(),
        exit_code: 0,
        stderr: format!("could not read ~/.cargo/.crates.toml: {err}"),
    })?;

    let mut out: Vec<Package> = parsed
        .v1
        .iter()
        .filter_map(|(key, bins)| {
            let (name, version, origin) = parse_key(key)?;
            Some(Package {
                repo_version: None,
                name,
                version,
                source_id: SourceId::cargo(),
                // Everything cargo installs was asked for by name; nothing is
                // pulled in on another crate's behalf.
                install_reason: InstallReason::Explicit,
                size_bytes: None,
                description: None,
                depends_on: Vec::new(),
                required_by: Vec::new(),
                optional_deps: Vec::new(),
                provides: bins.clone(),
                runtime: false,
                scope: None,
                // "Not from this source's registry" — a path or git install,
                // which has no upstream version to compare against.
                foreign: origin == Origin::Local,
                signed: false,
                packager: None,
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Parse `cargo install-update --list` output.
///
/// ```text
/// Package  Installed  Latest   Needs update
/// ripgrep  14.1.1     14.1.2   Yes
/// eza      0.20.1     0.20.1   No
/// ```
/// Only the `Yes` rows are updates. A crate cargo cannot place shows `No` with
/// a dash for the latest version, which is not an update either.
pub fn parse_updates(stdout: &str) -> Vec<PendingUpdate> {
    stdout
        .lines()
        .skip_while(|line| !line.starts_with("Package"))
        .skip(1)
        .filter_map(|line| {
            let mut cols = line.split_whitespace();
            let name = cols.next()?;
            let current = cols.next()?;
            let latest = cols.next()?;
            let needs = cols.next()?;
            (needs.eq_ignore_ascii_case("yes") && latest != "-").then(|| PendingUpdate {
                package_name: name.to_string(),
                current_version: current.to_string(),
                available_version: latest.to_string(),
                source_id: SourceId::cargo(),
            })
        })
        .collect()
}

/// `cargo install-update -a` — every crate cargo can place, in one go.
///
/// Run through `cargo`, not as the bare binary: `cargo-install-update` is a
/// cargo subcommand, and invoked directly it expects `install-update` as its
/// own first argument ("error: unexpected argument '-a' found"). The PATH
/// probe still looks for the binary — that is what tells us the subcommand
/// exists.
///
/// Never privileged: `cargo install` writes to `$CARGO_HOME`, under the user's
/// own home.
pub fn update_command() -> Vec<String> {
    vec![
        "cargo".to_string(),
        "install-update".to_string(),
        "-a".to_string(),
    ]
}

/// Ask `cargo-update` what is out of date. `Ok(vec![])` when it is not
/// installed — a missing optional tool is not a scan failure (design §6).
pub fn scan_updates(runner: &dyn CommandRunner) -> Result<Vec<PendingUpdate>, ProviderError> {
    if !crate::providers::binary_on_path(INSTALL_UPDATE_BIN) {
        return Ok(Vec::new());
    }
    let out = runner
        .run("cargo", &["install-update", "--list"])
        .map_err(|source| ProviderError::Exec {
            program: "cargo".to_string(),
            source,
        })?;
    if out.exit_code != 0 {
        return Err(ProviderError::CommandFailed {
            program: "cargo install-update --list".to_string(),
            exit_code: out.exit_code,
            stderr: out.stderr,
        });
    }
    Ok(parse_updates(&out.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from a real `~/.cargo/.crates.toml`, with a registry and a git
    /// install added: the author's machine has only path installs, which is
    /// exactly the case that cannot be version-compared.
    const CRATES: &str = include_str!("../../tests/fixtures/cargo/crates.toml");

    #[test]
    fn a_real_record_lists_crates_with_the_binaries_they_installed() {
        let pkgs = parse_installed(CRATES).expect("parses");
        assert_eq!(pkgs.len(), 5);
        // Sorted by name, so the list does not reorder between scans.
        let names: Vec<&str> = pkgs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["eza", "paclens", "ripgrep", "soundviz", "tack"]);

        let rg = pkgs.iter().find(|p| p.name == "ripgrep").expect("ripgrep");
        assert_eq!(rg.version, "14.1.1");
        assert_eq!(rg.provides, ["rg"], "the binary is what the user types");
        assert_eq!(rg.source_id, SourceId::cargo());
        // Everything cargo installs was asked for by name.
        assert_eq!(rg.install_reason, InstallReason::Explicit);
    }

    #[test]
    fn path_and_git_installs_are_flagged_as_having_no_upstream() {
        let pkgs = parse_installed(CRATES).expect("parses");
        let unversionable: Vec<&str> = pkgs
            .iter()
            .filter(|p| p.foreign)
            .map(|p| p.name.as_str())
            .collect();
        // A git checkout has no published version either — same shape as a
        // path install, and as an AUR `-git` package.
        assert_eq!(unversionable, ["eza", "paclens", "soundviz", "tack"]);
        let rg = pkgs.iter().find(|p| p.name == "ripgrep").expect("ripgrep");
        assert!(!rg.foreign, "a registry crate can be compared");
    }

    #[test]
    fn a_key_that_does_not_parse_is_skipped_rather_than_guessed_at() {
        // The package id rendering carries no compatibility promise, so the
        // parser drops what it cannot read instead of inventing a crate.
        let text =
            "[v1]\n\"nonsense\" = [\"x\"]\n\"ok 1.0.0 (registry+https://example)\" = [\"ok\"]\n";
        let pkgs = parse_installed(text).expect("parses");
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "ok");
    }

    #[test]
    fn an_empty_record_is_an_empty_source_not_a_failure() {
        assert!(parse_installed("[v1]\n").expect("parses").is_empty());
    }

    #[test]
    fn a_record_that_is_not_toml_at_all_is_an_error_worth_reporting() {
        // Different from "nothing installed": the file exists and is wrong,
        // which the scanner logs rather than silently showing zero crates.
        assert!(parse_installed("<<<not toml>>>").is_err());
    }

    #[test]
    fn only_the_rows_needing_an_update_count_as_updates() {
        let stdout = "\
Package  Installed  Latest   Needs update
ripgrep  14.1.1     14.1.2   Yes
eza      0.20.1     0.20.1   No
tack     0.8.2      -        No

";
        let ups = parse_updates(stdout);
        assert_eq!(ups.len(), 1);
        assert_eq!(ups[0].package_name, "ripgrep");
        assert_eq!(ups[0].current_version, "14.1.1");
        assert_eq!(ups[0].available_version, "14.1.2");
        assert_eq!(ups[0].source_id, SourceId::cargo());
    }

    #[test]
    fn the_update_command_is_never_privileged() {
        // Everything cargo installs lives under $HOME; the planner declares
        // this step unprivileged and this is the command it declares it for.
        assert_eq!(update_command(), ["cargo", "install-update", "-a"]);
    }
}
