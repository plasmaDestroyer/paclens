//! Python tools installed with `uv tool install` (#14 — the pipx role, filled
//! by uv on the reference machine). uv installs and upgrades them under the
//! user's home, so nothing is privileged.

use crate::model::{InstallReason, Package, PendingUpdate, SourceId};
use crate::providers::{CommandRunner, ProviderError};

pub const UV_BIN: &str = "uv";

/// `uv tool list`: a `name vX.Y [latest: Z]` line per tool, followed by its
/// `- binary` lines. Returns the tools (binaries as `provides`) and, where
/// the `[latest: …]` note is present, the update.
pub fn parse(stdout: &str) -> (Vec<Package>, Vec<PendingUpdate>) {
    let mut packages: Vec<Package> = Vec::new();
    let mut updates = Vec::new();
    for line in stdout.lines() {
        if let Some(bin) = line.strip_prefix("- ") {
            if let Some(last) = packages.last_mut() {
                last.provides.push(bin.trim().to_string());
            }
            continue;
        }
        let mut cols = line.split_whitespace();
        let (Some(name), Some(version)) = (cols.next(), cols.next()) else {
            continue;
        };
        let version = version.trim_start_matches('v');
        if let Some(latest) = line
            .split_once("[latest:")
            .and_then(|(_, r)| r.split(']').next())
        {
            updates.push(PendingUpdate {
                package_name: name.to_string(),
                current_version: version.to_string(),
                available_version: latest.trim().to_string(),
                source_id: SourceId::uv(),
            });
        }
        packages.push(Package {
            name: name.to_string(),
            version: version.to_string(),
            source_id: SourceId::uv(),
            // Every tool was asked for by name.
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
        });
    }
    (packages, updates)
}

/// One call answers both questions: `--outdated` lists every tool and marks
/// the ones with a newer release.
pub fn scan(
    runner: &dyn CommandRunner,
) -> Result<(Vec<Package>, Vec<PendingUpdate>), ProviderError> {
    let out = runner
        .run(UV_BIN, &["tool", "list", "--outdated"])
        .map_err(|source| ProviderError::Exec {
            program: UV_BIN.to_string(),
            source,
        })?;
    if out.exit_code != 0 {
        return Err(ProviderError::CommandFailed {
            program: "uv tool list --outdated".to_string(),
            exit_code: out.exit_code,
            stderr: out.stderr,
        });
    }
    // `--outdated` lists only the tools with updates; the full list is the
    // inventory, so ask for it too and merge.
    let full = runner
        .run(UV_BIN, &["tool", "list"])
        .ok()
        .filter(|o| o.exit_code == 0)
        .map(|o| o.stdout)
        .unwrap_or_default();
    let (packages, _) = parse(&full);
    let (_, updates) = parse(&out.stdout);
    Ok((packages, updates))
}

/// `uv tool upgrade --all`. Asks nothing.
pub fn update_command() -> Vec<String> {
    vec![
        UV_BIN.to_string(),
        "tool".to_string(),
        "upgrade".to_string(),
        "--all".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUTDATED: &str = "hf v1.31.0 [latest: 2.1.1]\n- hf\ngraphifyy v0.9.53 [latest: 0.9.80]\n- graphify\n- graphify-mcp\n";
    const FULL: &str = "git-filter-repo v2.47.0\n- git-filter-repo\nhf v1.31.0\n- hf\n";

    #[test]
    fn tools_carry_their_binaries_and_updates_their_latest() {
        let (p, u) = parse(OUTDATED);
        assert_eq!(p.len(), 2);
        assert_eq!(p[1].provides, vec!["graphify", "graphify-mcp"]);
        assert_eq!(u[0].package_name, "hf");
        assert_eq!(u[0].current_version, "1.31.0");
        assert_eq!(u[0].available_version, "2.1.1");
    }

    #[test]
    fn the_inventory_is_every_tool_not_only_the_outdated_ones() {
        let runner = crate::providers::test_support::MockRunner::new()
            .with(
                "uv tool list --outdated",
                "hf v1.31.0 [latest: 2.1.1]\n- hf\n",
                0,
            )
            .with("uv tool list", FULL, 0);
        let (p, u) = scan(&runner).expect("scans");
        assert_eq!(p.len(), 2, "{p:?}");
        assert_eq!(u.len(), 1);
    }
}
