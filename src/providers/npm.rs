//! Global npm packages (#13) — the ones you installed, not pacman's.
//!
//! On Arch the global prefix is `/usr`, shared with packages pacman owns
//! (`npm` itself, `node-gyp`, …). Those are pacman's to update and are left
//! out. What remains is report-only for now: updating needs root under
//! `/usr`, and a blanket `npm update -g` would overwrite pacman's files, so
//! the plan carries no npm step until privilege can be declared per machine.

use crate::model::{InstallReason, Package, PendingUpdate, SourceId};
use crate::providers::{CommandRunner, ProviderError};

pub const NPM_BIN: &str = "npm";

/// `name@version` → `(name, version)`, scoped names (`@scope/name@1.0`) kept.
fn split_spec(spec: &str) -> Option<(&str, &str)> {
    let at = spec.rfind('@').filter(|&i| i > 0)?;
    Some((&spec[..at], &spec[at + 1..]))
}

/// `npm ls -g --depth=0 --parseable --long`: `dir:name@version[:…]` per line
/// (the first line is the prefix itself, with no spec).
pub fn parse_ls(stdout: &str) -> Vec<(String, String, String)> {
    stdout
        .lines()
        .filter_map(|l| {
            let mut cols = l.split(':');
            let dir = cols.next()?;
            let (name, version) = split_spec(cols.next()?)?;
            Some((dir.to_string(), name.to_string(), version.to_string()))
        })
        .collect()
}

/// `npm outdated -g --parseable`: `dir:wanted:current:latest:location`.
pub fn parse_outdated(stdout: &str) -> Vec<(String, String, String)> {
    stdout
        .lines()
        .filter_map(|l| {
            let cols: Vec<&str> = l.split(':').collect();
            let (name, latest) = split_spec(cols.get(3)?)?;
            let (_, current) = split_spec(cols.get(2)?)?;
            Some((name.to_string(), current.to_string(), latest.to_string()))
        })
        .collect()
}

pub fn scan(
    runner: &dyn CommandRunner,
) -> Result<(Vec<Package>, Vec<PendingUpdate>), ProviderError> {
    let out = runner
        .run(NPM_BIN, &["ls", "-g", "--depth=0", "--parseable", "--long"])
        .map_err(|source| ProviderError::Exec {
            program: NPM_BIN.to_string(),
            source,
        })?;
    if out.exit_code != 0 {
        return Err(ProviderError::CommandFailed {
            program: "npm ls -g".to_string(),
            exit_code: out.exit_code,
            stderr: out.stderr,
        });
    }
    let listed = parse_ls(&out.stdout);
    // Which of those directories pacman owns: one `pacman -Qo` over all.
    let dirs: Vec<&str> = listed.iter().map(|(d, _, _)| d.as_str()).collect();
    let mut args = vec!["-Qo"];
    args.extend(&dirs);
    let owned: Vec<String> = runner
        .run("pacman", &args)
        .map(|o| {
            o.stdout
                .lines()
                .filter_map(|l| l.split_once(" is owned by ").map(|(p, _)| p.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let mine: Vec<&(String, String, String)> = listed
        .iter()
        .filter(|(d, _, _)| !owned.iter().any(|o| o.trim_end_matches('/') == d))
        .collect();
    let packages: Vec<Package> = mine
        .iter()
        .map(|(_, name, version)| Package {
            name: name.clone(),
            version: version.clone(),
            source_id: SourceId::npm(),
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
        })
        .collect();
    // `npm outdated` exits 1 when anything is outdated: an answer, not a
    // failure.
    let outdated = runner
        .run(NPM_BIN, &["outdated", "-g", "--parseable"])
        .ok()
        .filter(|o| o.exit_code <= 1)
        .map(|o| parse_outdated(&o.stdout))
        .unwrap_or_default();
    let updates = outdated
        .into_iter()
        .filter(|(n, _, _)| packages.iter().any(|p| &p.name == n))
        .map(|(name, current, latest)| PendingUpdate {
            package_name: name,
            current_version: current,
            available_version: latest,
            source_id: SourceId::npm(),
        })
        .collect();
    Ok((packages, updates))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::test_support::MockRunner;

    const LS: &str = "/usr/lib\n/usr/lib/node_modules/@openai/codex:@openai/codex@0.149.0:undefined\n/usr/lib/node_modules/neovim:neovim@5.4.0:undefined\n/usr/lib/node_modules/npm:npm@12.2.0:undefined\n";

    #[test]
    fn specs_split_at_the_last_at_and_keep_scopes() {
        assert_eq!(
            split_spec("@openai/codex@0.149.0"),
            Some(("@openai/codex", "0.149.0"))
        );
        assert_eq!(split_spec("neovim@5.4.0"), Some(("neovim", "5.4.0")));
        assert_eq!(split_spec("/usr/lib"), None);
    }

    #[test]
    fn pacmans_modules_are_left_out_and_outdated_ones_reported() {
        let runner = MockRunner::new()
            .with("npm ls -g --depth=0 --parseable --long", LS, 0)
            .with(
                "pacman -Qo /usr/lib/node_modules/@openai/codex /usr/lib/node_modules/neovim /usr/lib/node_modules/npm",
                "/usr/lib/node_modules/npm/ is owned by npm 12.2.0-1\n",
                1,
            )
            .with(
                "npm outdated -g --parseable",
                "/usr/lib/node_modules/neovim:neovim@5.5.0:neovim@5.4.0:neovim@5.5.0:global\n",
                1,
            );
        let (p, u) = scan(&runner).expect("scans");
        let names: Vec<_> = p.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["@openai/codex", "neovim"]);
        assert_eq!(u.len(), 1);
        assert_eq!(u[0].available_version, "5.5.0");
    }
}
