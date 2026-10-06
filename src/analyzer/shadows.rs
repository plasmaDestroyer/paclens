//! The same command installed twice: once in `~/.cargo/bin` (a crate, or
//! rustup's toolchain shims) and once by pacman in `/usr/bin` (#18). `PATH`
//! decides which one runs, and nothing else on the machine says which.
//!
//! The scanner records the facts — each doubled binary, the pacman package
//! that owns the other copy, and which directory comes first on `PATH`. This
//! groups them for the surfaces.

use serde::{Deserialize, Serialize};

/// One command present in both places.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shadow {
    pub bin: String,
    /// The pacman package that owns `/usr/bin/<bin>`.
    pub pacman_package: String,
    /// `~/.cargo/bin` comes first on `PATH`, so its copy is the one that runs.
    pub cargo_wins: bool,
}

/// Parse `pacman -Qo <paths>`: `/usr/bin/x is owned by pkg 1.0-1` per line.
/// Unowned paths go to stderr and so never appear here.
pub fn parse_owners(stdout: &str) -> Vec<(String, String)> {
    stdout
        .lines()
        .filter_map(|line| {
            let (path, rest) = line.split_once(" is owned by ")?;
            let bin = path.rsplit('/').next()?.to_string();
            let pkg = rest.split_whitespace().next()?.to_string();
            Some((bin, pkg))
        })
        .collect()
}

/// Group by pacman package: `(package, binaries, cargo_wins)`, largest first.
pub fn by_package(shadows: &[Shadow]) -> Vec<(String, Vec<String>, bool)> {
    let mut out: Vec<(String, Vec<String>, bool)> = Vec::new();
    for s in shadows {
        match out.iter_mut().find(|(p, _, _)| *p == s.pacman_package) {
            Some((_, bins, _)) => bins.push(s.bin.clone()),
            None => out.push((s.pacman_package.clone(), vec![s.bin.clone()], s.cargo_wins)),
        }
    }
    out.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
    out
}

/// One line per pacman package whose commands are doubled.
pub fn lines(shadows: &[Shadow]) -> Vec<String> {
    by_package(shadows)
        .into_iter()
        .map(|(pkg, bins, cargo_wins)| {
            let winner = if cargo_wins {
                "~/.cargo/bin runs first"
            } else {
                "pacman's copy runs first"
            };
            let shown: Vec<&str> = bins.iter().take(4).map(String::as_str).collect();
            let more = if bins.len() > 4 {
                format!(" +{}", bins.len() - 4)
            } else {
                String::new()
            };
            format!(
                "{pkg} also in ~/.cargo/bin: {}{more} — {winner}",
                shown.join(", ")
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owners_parse_and_unowned_paths_are_absent() {
        let out = "/usr/bin/rustc is owned by rust 1:1.99.0-1\n/usr/bin/cargo is owned by rust 1:1.99.0-1\n";
        assert_eq!(
            parse_owners(out),
            vec![
                ("rustc".to_string(), "rust".to_string()),
                ("cargo".to_string(), "rust".to_string())
            ]
        );
    }

    #[test]
    fn doubled_commands_group_per_package_and_name_the_winner() {
        let s = |bin: &str| Shadow {
            bin: bin.to_string(),
            pacman_package: "rust".to_string(),
            cargo_wins: true,
        };
        let l = lines(&[
            s("rustc"),
            s("cargo"),
            s("rustfmt"),
            s("rustdoc"),
            s("rust-gdb"),
        ]);
        assert_eq!(l.len(), 1);
        assert!(
            l[0].starts_with("rust also in ~/.cargo/bin: rustc, cargo, rustfmt, rustdoc +1"),
            "{}",
            l[0]
        );
        assert!(l[0].ends_with("~/.cargo/bin runs first"), "{}", l[0]);
    }
}
