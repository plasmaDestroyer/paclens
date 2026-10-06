//! The Arch news feed (#1), fetched with `curl` through the `CommandRunner`
//! seam — curl ships with pacman's own dependencies, so this adds no HTTP
//! crate, and fixtures drive the tests like every other provider.
//!
//! Offline, or with curl missing, the answer is an empty list: news is
//! advisory, and a scan never fails over it.

use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};

use super::CommandRunner;

pub const FEED: &str = "https://archlinux.org/feeds/news/";

/// One news post: what a surface needs to name it and link to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewsItem {
    pub title: String,
    pub link: String,
    pub published: DateTime<FixedOffset>,
}

/// Fetch and parse the feed. Any failure is an empty list.
pub fn fetch(runner: &dyn CommandRunner, timeout_secs: u64) -> Vec<NewsItem> {
    let timeout = timeout_secs.max(1).to_string();
    match runner.run("curl", &["-fsSL", "--max-time", &timeout, FEED]) {
        Ok(out) if out.exit_code == 0 => parse(&out.stdout),
        _ => Vec::new(),
    }
}

/// The text between `<tag>` and `</tag>` in `s`, if any.
fn between<'a>(s: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let start = s.find(&open)? + open.len();
    let end = s[start..].find(&format!("</{tag}>"))? + start;
    Some(&s[start..end])
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Parse the RSS `<item>`s. An item missing a title, link or readable date
/// is skipped rather than guessed at.
pub fn parse(xml: &str) -> Vec<NewsItem> {
    xml.split("<item>")
        .skip(1)
        .filter_map(|item| {
            let item = item.split("</item>").next()?;
            Some(NewsItem {
                title: unescape(between(item, "title")?),
                link: between(item, "link")?.trim().to_string(),
                published: DateTime::parse_from_rfc2822(between(item, "pubDate")?.trim()).ok()?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED_XML: &str = r#"<rss><channel><title>Arch Linux: Recent news updates</title>
<item><title>Mkinitcpio &gt;=42 requires manual intervention</title><link>https://archlinux.org/news/mk/</link><description>&lt;p&gt;x&lt;/p&gt;</description><pubDate>Tue, 22 Sep 2026 09:09:27 +0000</pubDate></item>
<item><title>No date here</title><link>https://archlinux.org/news/x/</link></item>
</channel></rss>"#;

    #[test]
    fn items_parse_with_entities_decoded_and_undated_ones_skipped() {
        let items = parse(FEED_XML);
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(
            items[0].title,
            "Mkinitcpio >=42 requires manual intervention"
        );
        assert_eq!(items[0].link, "https://archlinux.org/news/mk/");
        assert_eq!(items[0].published.to_rfc3339(), "2026-09-22T09:09:27+00:00");
    }

    #[test]
    fn a_failed_fetch_is_no_news_not_an_error() {
        let runner = crate::providers::test_support::MockRunner::new();
        assert!(fetch(&runner, 10).is_empty());
    }
}
