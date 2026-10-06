//! Which Arch news posts are about this machine (#1).
//!
//! A post counts when its title names an installed package. That is a match on
//! words, so it is `Inferred` (P3): a miss leaves the post unshown, which is
//! what every other tool's feed reader does anyway, while a wrong match costs
//! a line. Only posts newer than `since` count — the last upgrade, so a post
//! about something already dealt with stops showing.

use chrono::{DateTime, FixedOffset};

use crate::model::ScanResult;
use crate::providers::news::NewsItem;

/// Names too generic to identify a post's subject, though each is a package.
const GENERIC: [&str; 8] = [
    "linux", "arch", "base", "core", "extra", "main", "update", "packages",
];

/// When the last upgrade happened: the newest transaction that upgraded
/// anything. A post older than that was out before the system moved on.
pub fn last_upgrade(
    transactions: &[crate::analyzer::history::Transaction],
) -> Option<DateTime<FixedOffset>> {
    transactions
        .iter()
        .filter(|t| t.counts().1 > 0)
        .map(|t| t.started)
        .max()
}

/// A post about something installed here, and the package it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relevant<'a> {
    pub item: &'a NewsItem,
    pub package: String,
}

/// The posts since `since` (all of them when `None`) whose title names an
/// installed package, newest first.
pub fn relevant(scan: &ScanResult, since: Option<DateTime<FixedOffset>>) -> Vec<Relevant<'_>> {
    let installed: std::collections::HashSet<String> = scan
        .packages
        .iter()
        .map(|p| p.name.to_lowercase())
        .collect();
    let mut out: Vec<Relevant> = scan
        .news
        .iter()
        .filter(|item| since.is_none_or(|s| item.published > s))
        .filter_map(|item| {
            let package = item
                .title
                .split(|c: char| !(c.is_alphanumeric() || "-_.+".contains(c)))
                .map(|w| w.trim_matches('.').to_lowercase())
                .find(|w| {
                    w.len() >= 3 && !GENERIC.contains(&w.as_str()) && installed.contains(w)
                })?;
            Some(Relevant { item, package })
        })
        .collect();
    out.sort_by_key(|r| std::cmp::Reverse(r.item.published));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{InstallReason, Package, SourceId};

    fn item(title: &str, date: &str) -> NewsItem {
        NewsItem {
            title: title.to_string(),
            link: "https://archlinux.org/news/x/".to_string(),
            published: DateTime::parse_from_rfc3339(date).unwrap(),
        }
    }

    fn scan(names: &[&str], news: Vec<NewsItem>) -> ScanResult {
        let mut s = ScanResult::empty();
        s.packages = names
            .iter()
            .map(|n| Package {
                name: n.to_string(),
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
                scope: None,
                foreign: false,
                signed: true,
                packager: None,
                repo_version: None,
            })
            .collect();
        s.news = news;
        s
    }

    #[test]
    fn a_post_naming_an_installed_package_is_relevant_and_others_are_not() {
        let s = scan(
            &["mkinitcpio", "linux", "firefox"],
            vec![
                item(
                    "Mkinitcpio >=42 requires manual intervention",
                    "2026-09-22T09:00:00+00:00",
                ),
                item(
                    "kea >= 1:3.0.3-6 update requires manual intervention",
                    "2026-09-20T09:00:00+00:00",
                ),
                item(
                    "Arch Linux 2026 Leader Election Results",
                    "2026-09-21T09:00:00+00:00",
                ),
            ],
        );
        let r = relevant(&s, None);
        assert_eq!(r.len(), 1, "{r:?}");
        assert_eq!(r[0].package, "mkinitcpio");
    }

    #[test]
    fn a_post_older_than_the_last_upgrade_has_been_dealt_with() {
        let s = scan(
            &["mkinitcpio"],
            vec![item(
                "Mkinitcpio >=42 requires manual intervention",
                "2026-09-22T09:00:00+00:00",
            )],
        );
        let after = DateTime::parse_from_rfc3339("2026-09-23T00:00:00+00:00").ok();
        assert!(relevant(&s, after).is_empty());
        let before = DateTime::parse_from_rfc3339("2026-09-21T00:00:00+00:00").ok();
        assert_eq!(relevant(&s, before).len(), 1);
    }
}
