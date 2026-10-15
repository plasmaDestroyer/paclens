//! The same command installed more than once (#18): by pacman in `/usr/bin`
//! and by cargo, brew, go or uv in their own bin directories. `PATH` decides
//! which copy runs, and nothing else on the machine says which.
//!
//! The scanner lists each source's bin directory and asks pacman who owns
//! the `/usr/bin` copies; [`find`] turns those facts into the doubled
//! commands, with every copy in the order `PATH` reaches it.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One command with two or more copies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shadow {
    pub bin: String,
    /// In `PATH` order; copies in a directory `PATH` never reaches come last.
    pub copies: Vec<Copy>,
    /// The directory whose `bin` a shell runs: the first on `PATH` holding
    /// an executable of that name, which need not be any of `copies`.
    pub runs_dir: Option<String>,
}

/// One installed copy of a command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Copy {
    /// The source id: `pacman`, `cargo`, `brew`, `gobin`, `uv`.
    pub source: String,
    /// The package that installed it, where the source says (pacman, uv).
    pub package: Option<String>,
    pub dir: String,
    pub on_path: bool,
}

impl Shadow {
    /// The copy a shell runs; `None` when what runs is none of them.
    pub fn runs(&self) -> Option<&Copy> {
        let dir = self.runs_dir.as_ref()?;
        self.copies.iter().find(|c| &c.dir == dir)
    }

    /// Who runs, in words: a copy, something else on `PATH`, or nothing.
    fn runner(&self) -> String {
        match (self.runs(), &self.runs_dir) {
            (Some(c), _) => c.label(),
            (None, Some(d)) => format!("{d}/{}", self.bin),
            (None, None) => String::new(),
        }
    }
}

impl Copy {
    fn label(&self) -> String {
        match &self.package {
            Some(p) if p != &self.source => format!("{} {p}", self.source),
            _ => self.source.clone(),
        }
    }
}

/// What the scanner gathered for one source's bin directory.
pub struct BinDir {
    pub source: String,
    pub dir: PathBuf,
    /// `(command, package)`; the package is `None` when the source does not
    /// say which one installed it.
    pub bins: Vec<(String, Option<String>)>,
}

/// The formula a brew link points into: `../Cellar/<formula>/<ver>/bin/x`.
pub fn cellar_formula(target: &Path) -> Option<String> {
    let mut parts = target.iter().skip_while(|p| *p != "Cellar");
    parts.next()?;
    Some(parts.next()?.to_string_lossy().into_owned())
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

/// Every command found in two or more places. `pacman` holds the
/// `/usr/bin` copies pacman owns, as `(command, package)`; a `/usr/bin`
/// file pacman does not own is nobody's and not counted. `path` is `PATH`
/// with each entry resolved, and `exec(dir, bin)` says whether `dir/bin` is
/// an executable a shell would run — it decides the winner over the whole
/// `PATH`, not only over the copies found.
pub fn find(
    dirs: &[BinDir],
    pacman: &[(String, String)],
    path: &[PathBuf],
    exec: impl Fn(&Path, &str) -> bool,
) -> Vec<Shadow> {
    let usr = Path::new("/usr/bin");
    let pos = |d: &Path| path.iter().position(|p| p == d);
    let mut out: Vec<Shadow> = Vec::new();
    let mut add = |bin: &str, source: &str, package: Option<String>, dir: &Path| {
        let copy = Copy {
            source: source.to_string(),
            package,
            dir: dir.to_string_lossy().into_owned(),
            on_path: pos(dir).is_some(),
        };
        match out.iter_mut().find(|s| s.bin == bin) {
            Some(s) if s.copies.iter().any(|c| c.dir == copy.dir) => {}
            Some(s) => s.copies.push(copy),
            None => out.push(Shadow {
                bin: bin.to_string(),
                copies: vec![copy],
                runs_dir: None,
            }),
        }
    };
    for (bin, pkg) in pacman {
        add(bin, "pacman", Some(pkg.clone()), usr);
    }
    for d in dirs {
        for (bin, pkg) in &d.bins {
            add(bin, &d.source, pkg.clone(), &d.dir);
        }
    }
    out.retain(|s| s.copies.len() > 1);
    for s in &mut out {
        s.copies
            .sort_by_key(|c| pos(Path::new(&c.dir)).unwrap_or(usize::MAX));
        s.runs_dir = path
            .iter()
            .find(|d| exec(d, &s.bin))
            .map(|d| d.to_string_lossy().into_owned());
    }
    out.sort_by(|a, b| a.bin.cmp(&b.bin));
    out
}

/// Commands grouped by the same set of copies, in `PATH` order:
/// `(labels, commands, runs)`, largest group first. `runs` is the copy a
/// shell runs, or the path of whatever else it runs, or empty for nothing.
pub fn groups(shadows: &[Shadow]) -> Vec<(Vec<String>, Vec<String>, String)> {
    let mut out: Vec<(Vec<String>, Vec<String>, String)> = Vec::new();
    for s in shadows {
        let labels: Vec<String> = s.copies.iter().map(Copy::label).collect();
        // Something outside the copies runs: per command, so never grouped.
        let runs = if s.runs().is_none() && s.runs_dir.is_some() {
            "elsewhere on PATH".to_string()
        } else {
            s.runner()
        };
        match out.iter_mut().find(|(l, _, r)| *l == labels && *r == runs) {
            Some((_, bins, _)) => bins.push(s.bin.clone()),
            None => out.push((labels, vec![s.bin.clone()], runs)),
        }
    }
    out.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
    out
}

fn list(bins: &[String]) -> String {
    let shown: Vec<&str> = bins.iter().take(4).map(String::as_str).collect();
    let more = if bins.len() > 4 {
        format!(" +{}", bins.len() - 4)
    } else {
        String::new()
    };
    format!("{}{more}", shown.join(", "))
}

/// One line per group: the commands, then the copy that runs and the ones
/// it hides.
pub fn lines(shadows: &[Shadow]) -> Vec<String> {
    groups(shadows)
        .into_iter()
        .map(|(labels, bins, runs)| {
            let n = labels.len();
            match runs.as_str() {
                "" => format!(
                    "{} installed {n} times: {}, none on PATH",
                    list(&bins),
                    labels.join(", ")
                ),
                r if labels.first() == Some(&runs) => format!(
                    "{} installed {n} times: {r} runs, then {}",
                    list(&bins),
                    labels[1..].join(", ")
                ),
                r => format!(
                    "{} installed {n} times: {}; {r} runs first",
                    list(&bins),
                    labels.join(", ")
                ),
            }
        })
        .collect()
}

/// For `why`: this package's commands that another source also installed,
/// one line per set of other copies, saying which copy runs.
pub fn for_package(shadows: &[Shadow], pkg: &crate::model::Package) -> Vec<String> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for s in shadows {
        let mine = |c: &Copy| {
            c.source == pkg.source_id.as_str()
                && match &c.package {
                    Some(p) => *p == pkg.name,
                    None => pkg.provides.contains(&s.bin),
                }
        };
        let Some(me) = s.copies.iter().position(mine) else {
            continue;
        };
        let others: Vec<String> = s
            .copies
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != me)
            .map(|(_, c)| c.label())
            .collect();
        let runs = match (s.runs(), &s.runs_dir) {
            (None, None) => "none on PATH".to_string(),
            (Some(c), _) if *c == s.copies[me] => "this copy runs first".to_string(),
            _ => format!("{} runs first", s.runner()),
        };
        let key = format!("also from {}; {runs}", others.join(", "));
        match out.iter_mut().find(|(k, _)| *k == key) {
            Some((_, bins)) => bins.push(s.bin.clone()),
            None => out.push((key, vec![s.bin.clone()])),
        }
    }
    out.into_iter()
        .map(|(key, bins)| format!("{} {key}", list(&bins)))
        .collect()
}

/// One line for `status`; `overlaps` carries the detail.
pub fn summary(shadows: &[Shadow]) -> Option<String> {
    let groups = groups(shadows).len();
    (groups > 0).then(|| {
        format!(
            "{} commands installed twice or more, in {groups} group{}: paclens overlaps",
            shadows.len(),
            if groups == 1 { "" } else { "s" }
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(source: &str, d: &str, bins: &[(&str, Option<&str>)]) -> BinDir {
        BinDir {
            source: source.to_string(),
            dir: PathBuf::from(d),
            bins: bins
                .iter()
                .map(|(b, p)| (b.to_string(), p.map(str::to_string)))
                .collect(),
        }
    }

    /// `find`, with an executable `bin` in exactly the copies' directories.
    fn find_t(dirs: &[BinDir], pacman: &[(String, String)], path: &[PathBuf]) -> Vec<Shadow> {
        let has = |d: &Path, b: &str| {
            (d == Path::new("/usr/bin") && pacman.iter().any(|(p, _)| p == b))
                || dirs
                    .iter()
                    .any(|x| x.dir == d && x.bins.iter().any(|(n, _)| n == b))
        };
        find(dirs, pacman, path, has)
    }

    #[test]
    fn something_earlier_on_path_runs_instead_of_any_copy() {
        let s = find(
            &[dir("brew", "/brew/bin", &[("rg", Some("ripgrep"))])],
            &[("rg".to_string(), "ripgrep".to_string())],
            &path(&["/h/.local/bin", "/brew/bin", "/usr/bin"]),
            |_, _| true,
        );
        assert_eq!(s[0].runs(), None);
        assert_eq!(s[0].runs_dir.as_deref(), Some("/h/.local/bin"));
        assert_eq!(
            lines(&s),
            vec![
                "rg installed 2 times: brew ripgrep, pacman ripgrep; elsewhere on PATH runs first"
            ]
        );
    }

    fn path(dirs: &[&str]) -> Vec<PathBuf> {
        dirs.iter().map(PathBuf::from).collect()
    }

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
    fn why_names_the_other_copies_and_the_winner() {
        let s = find_t(
            &[dir(
                "brew",
                "/brew/bin",
                &[("as", Some("binutils")), ("ar", Some("binutils"))],
            )],
            &[
                ("as".to_string(), "binutils".to_string()),
                ("ar".to_string(), "binutils".to_string()),
            ],
            &path(&["/brew/bin", "/usr/bin"]),
        );
        let mut pkg = crate::providers::brew::parse_list("binutils 2.45\n").remove(0);
        pkg.source_id = crate::model::SourceId::pacman();
        assert_eq!(
            for_package(&s, &pkg),
            vec!["ar, as also from brew binutils; brew binutils runs first"]
        );
        pkg.source_id = crate::model::SourceId::brew();
        assert_eq!(
            for_package(&s, &pkg),
            vec!["ar, as also from pacman binutils; this copy runs first"]
        );
    }

    #[test]
    fn a_brew_link_names_its_formula() {
        assert_eq!(
            cellar_formula(Path::new("../Cellar/binutils/2.45/bin/as")).as_deref(),
            Some("binutils")
        );
        assert_eq!(cellar_formula(Path::new("/opt/x/bin/as")), None);
    }

    #[test]
    fn a_command_in_one_place_is_not_a_duplicate() {
        let s = find_t(
            &[dir("cargo", "/h/.cargo/bin", &[("rg", None)])],
            &[("fd".to_string(), "fd".to_string())],
            &path(&["/usr/bin"]),
        );
        assert!(s.is_empty(), "{s:?}");
    }

    #[test]
    fn copies_follow_path_order_and_the_first_runs() {
        let s = find_t(
            &[
                dir("cargo", "/h/.cargo/bin", &[("rg", None)]),
                dir("brew", "/brew/bin", &[("rg", None)]),
            ],
            &[("rg".to_string(), "ripgrep".to_string())],
            &path(&["/h/.cargo/bin", "/usr/bin"]),
        );
        assert_eq!(s.len(), 1);
        let order: Vec<&str> = s[0].copies.iter().map(|c| c.source.as_str()).collect();
        assert_eq!(order, vec!["cargo", "pacman", "brew"]);
        assert_eq!(s[0].runs().map(|c| c.source.as_str()), Some("cargo"));
        assert!(!s[0].copies[2].on_path, "brew is not on PATH");
    }

    #[test]
    fn two_user_sources_without_pacman_still_count() {
        let s = find_t(
            &[
                dir("uv", "/h/.local/bin", &[("ruff", Some("ruff"))]),
                dir("brew", "/brew/bin", &[("ruff", None)]),
            ],
            &[],
            &path(&["/brew/bin", "/h/.local/bin"]),
        );
        assert_eq!(s[0].runs().map(|c| c.source.as_str()), Some("brew"));
    }

    #[test]
    fn nothing_on_path_means_nothing_runs() {
        let s = find_t(
            &[
                dir("cargo", "/a", &[("x", None)]),
                dir("brew", "/b", &[("x", None)]),
            ],
            &[],
            &[],
        );
        assert_eq!(s[0].runs(), None);
        assert_eq!(
            lines(&s),
            vec!["x installed 2 times: cargo, brew, none on PATH"]
        );
    }

    #[test]
    fn lines_group_by_copies_and_name_the_winner() {
        let bins: Vec<(&str, Option<&str>)> = ["rustc", "cargo", "rustfmt", "rustdoc", "rust-gdb"]
            .iter()
            .map(|b| (*b, None))
            .collect();
        let pac: Vec<(String, String)> = bins
            .iter()
            .map(|(b, _)| (b.to_string(), "rust".to_string()))
            .collect();
        let s = find_t(
            &[dir("cargo", "/h/.cargo/bin", &bins)],
            &pac,
            &path(&["/h/.cargo/bin", "/usr/bin"]),
        );
        let l = lines(&s);
        assert_eq!(l.len(), 1);
        assert_eq!(
            summary(&s).as_deref(),
            Some("5 commands installed twice or more, in 1 group: paclens overlaps")
        );
        assert_eq!(
            l[0],
            "cargo, rust-gdb, rustc, rustdoc +1 installed 2 times: cargo runs, then pacman rust"
        );
    }
}
