//! Packages the configured repos can no longer reach (#78).
//!
//! An installed package whose version is *newer* than anything the configured
//! repos offer can never move again: `-Syu` prints `local (…) is newer than
//! (…)` and skips it, every time, silently. Nothing about the machine looks
//! wrong — the source is `ok`, the update count is right — and the packages
//! simply stop receiving updates.
//!
//! Found on the author's machine after the plain `[cachyos]` repos were
//! removed from `pacman.conf`, leaving the `-v3` ones: 1226 packages stranded
//! at CachyOS rebuilds the remaining repos had never heard of.
//!
//! Pure over the scan. Advisory only: re-adding the departed repo and
//! downgrading to what the remaining ones offer are both legitimate answers,
//! and paclens does not pick (design §1).

use std::cmp::Ordering;

use crate::model::{Package, ScanResult};

/// One installed package that outranks every configured repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outranked {
    pub name: String,
    /// What is installed.
    pub installed: String,
    /// The best any configured repo offers, and which repo that is.
    pub offered: String,
    pub repo: String,
    /// Who built the installed one, when anybody says — the same grouping
    /// #77 uses, because "59 packaged by CachyOS" is an explanation where 59
    /// names are a list.
    pub packager: Option<String>,
}

/// Every installed package the configured repos cannot reach, by name.
pub fn outranked(scan: &ScanResult) -> Vec<Outranked> {
    let mut out: Vec<Outranked> = scan.packages.iter().filter_map(stranded).collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn stranded(pkg: &Package) -> Option<Outranked> {
    let (repo, offered) = pkg.repo_version.as_ref()?;
    // Only *newer* counts. Older is an ordinary pending update, and equal was
    // never recorded.
    (crate::analyzer::version::compare(&pkg.version, offered) == Ordering::Greater).then(|| {
        Outranked {
            name: pkg.name.clone(),
            installed: pkg.version.clone(),
            offered: offered.clone(),
            repo: repo.clone(),
            packager: pkg.packager.clone(),
        }
    })
}

/// The same finding as one line: how many, and who built them.
///
/// Grouped by packager because that is the part a person can act on — a
/// hundred packages from one builder is a repo that went away, while three
/// scattered ones are three local decisions.
pub fn summary(stranded: &[Outranked]) -> Option<String> {
    if stranded.is_empty() {
        return None;
    }
    let mut by_packager: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for pkg in stranded {
        *by_packager
            .entry(pkg.packager.as_deref().unwrap_or("an unknown packager"))
            .or_default() += 1;
    }
    let mut groups: Vec<(&str, usize)> = by_packager.into_iter().collect();
    // Biggest first, then by name so the sentence is stable between scans.
    groups.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));

    let who = groups
        .iter()
        .take(2)
        .map(|(packager, count)| format!("{count} by {packager}"))
        .collect::<Vec<_>>()
        .join(", ");
    let rest = groups.len().saturating_sub(2);
    let tail = if rest > 0 {
        format!(", and {rest} more")
    } else {
        String::new()
    };
    Some(format!(
        "{} package{} no configured repo can reach: {who}{tail}",
        stranded.len(),
        if stranded.len() == 1 { "" } else { "s" },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{InstallReason, SourceId};

    fn pkg(name: &str, installed: &str, offer: Option<(&str, &str)>) -> Package {
        Package {
            name: name.to_string(),
            version: installed.to_string(),
            source_id: SourceId::pacman(),
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
            signed: true,
            packager: Some("CachyOS <x@y>".to_string()),
            repo_version: offer.map(|(r, v)| (r.to_string(), v.to_string())),
        }
    }

    fn scan_with(packages: Vec<Package>) -> ScanResult {
        let mut scan = ScanResult::empty();
        scan.packages = packages;
        scan
    }

    #[test]
    fn a_package_newer_than_every_repo_is_stranded() {
        // The shape found in the wild: a CachyOS rebuild left behind when the
        // repo that built it was removed.
        let scan = scan_with(vec![pkg("php", "8.5.10-2", Some(("extra", "8.5.10-1")))]);
        let found = outranked(&scan);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "php");
        assert_eq!(found[0].installed, "8.5.10-2");
        assert_eq!(found[0].offered, "8.5.10-1");
        assert_eq!(found[0].repo, "extra");
    }

    #[test]
    fn an_ordinary_pending_update_is_not_stranded() {
        // The repo offers something newer: that is an update, not a package
        // nothing can reach.
        let scan = scan_with(vec![pkg("bash", "5.3.15-1", Some(("core", "5.3.15-2")))]);
        assert!(outranked(&scan).is_empty());
    }

    #[test]
    fn an_epoch_is_respected_rather_than_the_version_string() {
        // Real pair from the author's machine: the installed one wins on
        // epoch despite the lower version, which a string compare gets wrong.
        let scan = scan_with(vec![pkg(
            "linux-api-headers",
            "1:7.1-1",
            Some(("core", "7.2-1")),
        )]);
        assert_eq!(outranked(&scan).len(), 1);
    }

    #[test]
    fn a_package_no_repo_offers_at_all_is_not_this_finding() {
        // That is a foreign package (#77), a different sentence.
        let scan = scan_with(vec![pkg("timr-bin", "1.0-1", None)]);
        assert!(outranked(&scan).is_empty());
    }

    #[test]
    fn the_summary_groups_by_who_built_them() {
        let mut arch = pkg("go", "2:1.27.1-2", Some(("extra", "2:1.27.1-1")));
        arch.packager = Some("Arch <a@b>".to_string());
        let scan = scan_with(vec![
            pkg("php", "8.5.10-2", Some(("extra", "8.5.10-1"))),
            pkg("sddm", "0.21.0-8", Some(("extra", "0.21.0-7"))),
            arch,
        ]);
        let line = summary(&outranked(&scan)).expect("a finding");
        assert!(
            line.starts_with("3 packages no configured repo can reach"),
            "{line}"
        );
        assert!(line.contains("2 by CachyOS"), "{line}");
        assert!(line.contains("1 by Arch"), "{line}");
    }

    #[test]
    fn nothing_stranded_says_nothing() {
        assert_eq!(summary(&[]), None);
        let scan = scan_with(vec![pkg("bash", "5.3.15-2", None)]);
        assert_eq!(summary(&outranked(&scan)), None);
    }
}
