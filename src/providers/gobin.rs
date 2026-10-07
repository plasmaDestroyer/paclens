//! Binaries installed with `go install` (#16). Go records the module and
//! version inside each binary (`go version -m`), so the inventory needs no
//! state file. Everything lives under `$GOPATH/bin` or `$GOBIN`: never
//! privileged.
//!
//! A binary built from a local checkout carries `(devel)` instead of a
//! version. It has nothing to compare against, is marked `foreign`, and the
//! update step leaves it alone rather than replacing it with whatever
//! `@latest` resolves to.

use crate::model::{InstallReason, Package, PendingUpdate, SourceId};
use crate::providers::{CommandRunner, ProviderError};

pub const GO_BIN: &str = "go";

/// One binary as `go version -m` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoBinary {
    pub bin: String,
    pub path: String,
    pub module: String,
    /// `None` for a `(devel)` build.
    pub version: Option<String>,
}

/// Parse `go version -m <dir>`: a `<file>: goX` header per binary, then
/// tab-indented `path` and `mod` lines among the deps.
pub fn parse_version_m(stdout: &str) -> Vec<GoBinary> {
    let mut out: Vec<GoBinary> = Vec::new();
    for line in stdout.lines() {
        if !line.starts_with(char::is_whitespace) {
            let Some((file, _)) = line.rsplit_once(": ") else {
                continue;
            };
            let bin = file.rsplit('/').next().unwrap_or(file).to_string();
            out.push(GoBinary {
                bin,
                path: String::new(),
                module: String::new(),
                version: None,
            });
            continue;
        }
        let Some(cur) = out.last_mut() else { continue };
        let cols: Vec<&str> = line.split_whitespace().collect();
        match cols.as_slice() {
            ["path", p, ..] => cur.path = p.to_string(),
            ["mod", m, v, ..] => {
                cur.module = m.to_string();
                cur.version = (*v != "(devel)").then(|| v.to_string());
            }
            _ => {}
        }
    }
    out.retain(|b| !b.path.is_empty());
    out
}

fn run(runner: &dyn CommandRunner, args: &[&str]) -> Result<String, ProviderError> {
    let out = runner
        .run(GO_BIN, args)
        .map_err(|source| ProviderError::Exec {
            program: GO_BIN.to_string(),
            source,
        })?;
    if out.exit_code != 0 {
        return Err(ProviderError::CommandFailed {
            program: format!("go {}", args.join(" ")),
            exit_code: out.exit_code,
            stderr: out.stderr,
        });
    }
    Ok(out.stdout)
}

/// Where `go install` puts binaries: `$GOBIN`, else `$GOPATH/bin`.
fn bin_dir(runner: &dyn CommandRunner) -> Result<String, ProviderError> {
    let env = run(runner, &["env", "GOBIN", "GOPATH"])?;
    let mut lines = env.lines().map(str::trim);
    let gobin = lines.next().unwrap_or_default();
    let gopath = lines.next().unwrap_or_default();
    Ok(if gobin.is_empty() {
        format!("{gopath}/bin")
    } else {
        gobin.to_string()
    })
}

pub fn scan(
    runner: &dyn CommandRunner,
) -> Result<(Vec<Package>, Vec<PendingUpdate>), ProviderError> {
    let dir = bin_dir(runner)?;
    if !std::path::Path::new(&dir).is_dir() {
        return Ok(Default::default()); // nothing installed is an empty source
    }
    let binaries = parse_version_m(&run(runner, &["version", "-m", &dir])?);
    let mut packages = Vec::new();
    let mut updates = Vec::new();
    for b in &binaries {
        if let Some(current) = &b.version {
            // One module lookup each; a lookup that fails is no update, not
            // an error for the whole source.
            let target = format!("{}@latest", b.module);
            if let Ok(latest) = run(runner, &["list", "-m", "-f", "{{.Version}}", &target]) {
                let latest = latest.trim();
                if !latest.is_empty() && latest != current {
                    updates.push(PendingUpdate {
                        package_name: b.bin.clone(),
                        current_version: current.clone(),
                        available_version: latest.to_string(),
                        source_id: SourceId::gobin(),
                    });
                }
            }
        }
        packages.push(Package {
            name: b.bin.clone(),
            version: b.version.clone().unwrap_or_else(|| "(devel)".to_string()),
            source_id: SourceId::gobin(),
            install_reason: InstallReason::Explicit,
            size_bytes: None,
            description: Some(b.path.clone()),
            depends_on: Vec::new(),
            required_by: Vec::new(),
            optional_deps: Vec::new(),
            provides: vec![b.bin.clone()],
            runtime: false,
            scope: None,
            foreign: b.version.is_none(),
            signed: false,
            packager: None,
            repo_version: None,
        });
    }
    Ok((packages, updates))
}

/// Reinstall every binary that came from a published module at `@latest`.
/// One fixed command, because `update` runs without a scan; `(devel)`
/// builds are skipped so a local checkout is never overwritten.
pub fn update_command() -> Vec<String> {
    let script = r#"d=$(go env GOBIN); [ -n "$d" ] || d="$(go env GOPATH)/bin"; for b in "$d"/*; do set -- $(go version -m "$b" 2>/dev/null | awk '$1=="path"{p=$2} $1=="mod"{v=$3} END{print p, v}'); [ -n "$1" ] && [ "$2" != "(devel)" ] && go install "$1@latest"; done; true"#;
    vec!["sh".to_string(), "-c".to_string(), script.to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;

    const VERSION_M: &str = "/home/u/go/bin/pomo: go1.26.3\n\tpath\tgithub.com/Bahaaio/pomo\n\tmod\tgithub.com/Bahaaio/pomo\tv1.2.1\th1:x=\n\tdep\tgithub.com/a/b\tv0.1.0\th1:y=\n/home/u/go/bin/hellogo: go1.26.3\n\tpath\tgithub.com/me/hellogo\n\tmod\tgithub.com/me/hellogo\t(devel)\t\n";

    #[test]
    fn version_m_gives_each_binary_its_module_and_version() {
        let b = parse_version_m(VERSION_M);
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].bin, "pomo");
        assert_eq!(b[0].module, "github.com/Bahaaio/pomo");
        assert_eq!(b[0].version.as_deref(), Some("v1.2.1"));
        assert_eq!(b[1].version, None, "(devel) has no version");
    }

    #[test]
    fn a_devel_build_is_never_checked_or_offered() {
        let dir = std::env::temp_dir().join(format!("paclens-gobin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.to_string_lossy().to_string();
        let runner = crate::providers::test_support::MockRunner::new()
            .with("go env GOBIN GOPATH", &format!("{d}\n/unused\n"), 0)
            .with(&format!("go version -m {d}"), VERSION_M, 0)
            .with(
                "go list -m -f {{.Version}} github.com/Bahaaio/pomo@latest",
                "v1.3.0\n",
                0,
            );
        let (p, u) = scan(&runner).expect("scans");
        assert_eq!(p.len(), 2);
        assert!(p[1].foreign, "hellogo is a local build");
        assert_eq!(u.len(), 1);
        assert_eq!(u[0].available_version, "v1.3.0");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_update_skips_devel_builds() {
        let cmd = update_command().join(" ");
        assert!(cmd.contains("\"(devel)\""), "{cmd}");
        assert!(cmd.contains("@latest"), "{cmd}");
    }
}
