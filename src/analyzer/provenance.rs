//! Where an alpm package actually came from (#77).
//!
//! `pacman -Qm` answers "in no configured sync database", which paclens used
//! to read as "from the AUR". That holds only while the repo set never
//! changes: remove a repo from `pacman.conf` and its packages become foreign
//! without ever having touched the AUR. On the machine this was found on, 63
//! of 91 foreign packages were CachyOS's, signed by CachyOS, and absent from
//! the AUR — unupdatable from anywhere, and reported as healthy AUR packages.
//!
//! The fix is to stop reading identity out of an absence. A package built on
//! this machine validates as `None` because nothing signed it; a package from
//! a repository carries a signature. That is a positive fact about the
//! package rather than a fact about the repo list.

use crate::model::Package;

/// Was this built on this machine? The AUR path leaves both marks: makepkg
/// signs nothing by default, and `Unknown Packager` is its default packager.
///
/// ponytail: a user who both signs their own builds *and* sets PACKAGER in
/// makepkg.conf reads as "from a repo". The report names the packager, so the
/// misreading is visible rather than silent; paru's clone directory is the
/// upgrade path if it ever matters, though `paru -Sc --aur` deletes those.
pub fn built_here(pkg: &Package) -> bool {
    !pkg.signed || pkg.packager.is_none()
}

/// Packages in no configured repository: pacman still manages them, nothing
/// can update them, and nobody says so. Sorted by name so the list is stable
/// between scans.
pub fn unowned(packages: &[Package]) -> Vec<&Package> {
    let mut out: Vec<&Package> = packages
        .iter()
        .filter(|p| p.foreign && !built_here(p))
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Who shipped them, with counts — "63 packaged by CachyOS" is the sentence
/// that explains the whole finding, where 63 names would not.
pub fn by_packager(packages: &[&Package]) -> Vec<(String, usize)> {
    let mut out: Vec<(String, usize)> = Vec::new();
    for p in packages {
        let who = p.packager.clone().unwrap_or_else(|| "unknown".to_string());
        match out.iter_mut().find(|(name, _)| name == &who) {
            Some((_, n)) => *n += 1,
            None => out.push((who, 1)),
        }
    }
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{InstallReason, SourceId};

    fn pkg(name: &str, foreign: bool, signed: bool, packager: Option<&str>) -> Package {
        Package {
            name: name.to_string(),
            version: "1".to_string(),
            source_id: SourceId::pacman(),
            install_reason: InstallReason::Explicit,
            size_bytes: None,
            description: None,
            depends_on: Vec::new(),
            required_by: Vec::new(),
            optional_deps: Vec::new(),
            provides: Vec::new(),
            runtime: false,
            foreign,
            signed,
            packager: packager.map(str::to_string),
        }
    }

    #[test]
    fn a_locally_built_package_is_the_aur_one() {
        // What paru leaves behind: nothing signed it, nobody claimed it.
        assert!(built_here(&pkg("antigravity", true, false, None)));
        // A repo package carries both marks.
        assert!(!built_here(&pkg(
            "cachyos-hello",
            true,
            true,
            Some("CachyOS <admin@cachyos.org>")
        )));
        // Either mark alone is enough to call it local: a signed package
        // nobody claims, or an unsigned one with a name on it.
        assert!(built_here(&pkg("signed-anon", true, true, None)));
        assert!(built_here(&pkg("named-unsigned", true, false, Some("me"))));
    }

    #[test]
    fn unowned_is_foreign_and_not_built_here() {
        let packages = vec![
            pkg("in-a-repo", false, true, Some("Arch")),
            pkg("from-the-aur", true, false, None),
            pkg("cachyos-hello", true, true, Some("CachyOS")),
            pkg("asusctl", true, true, Some("CachyOS")),
        ];
        let found = unowned(&packages);
        let names: Vec<&str> = found.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["asusctl", "cachyos-hello"]);
        assert_eq!(by_packager(&found), vec![("CachyOS".to_string(), 2)]);
    }

    #[test]
    fn a_machine_with_all_its_repos_finds_nothing() {
        // The regression that matters: nothing changes for a normal system.
        let packages = vec![
            pkg("firefox", false, true, Some("Arch")),
            pkg("brave-bin", true, false, None),
        ];
        assert!(unowned(&packages).is_empty());
    }
}
