//! Tiny in-house fuzzy matcher shared by the TUI package filter and the CLI
//! "did you mean" suggestion. Case-insensitive subsequence match with a
//! simple score; no external crate.
//!
//! Scoring favors what a human typing a package name means: consecutive
//! matched characters, matches at the start of the candidate or right after a
//! separator (`-`, `_`, `.`), and shorter candidates on ties.

/// Score for a query that matches; higher is better.
pub type Score = i32;

/// Does `query` match `candidate` as a case-insensitive subsequence?
/// Empty query matches everything with score 0.
pub fn matches(query: &str, candidate: &str) -> Option<Score> {
    let query: Vec<char> = query.chars().flat_map(|c| c.to_lowercase()).collect();
    if query.is_empty() {
        return Some(0);
    }
    let cand: Vec<char> = candidate.chars().flat_map(|c| c.to_lowercase()).collect();

    let mut score: Score = 0;
    let mut qi = 0;
    let mut last_hit: Option<usize> = None;
    for (ci, &c) in cand.iter().enumerate() {
        if qi < query.len() && c == query[qi] {
            score += 1;
            // Bonus: consecutive run.
            if last_hit == Some(ci.wrapping_sub(1)) {
                score += 2;
            }
            // Bonus: start of candidate or start of a word segment.
            if ci == 0 || matches!(cand[ci - 1], '-' | '_' | '.') {
                score += 3;
            }
            last_hit = Some(ci);
            qi += 1;
        }
    }
    if qi < query.len() {
        return None; // not all query chars found in order
    }
    // Tie-break: prefer shorter candidates (less unmatched noise).
    Some(score - cand.len() as Score / 4)
}

/// The best-scoring candidate for `query`, if any matches.
pub fn best_match<'a>(
    query: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    candidates
        .into_iter()
        .filter_map(|c| matches(query, c).map(|s| (s, c)))
        .max_by_key(|&(s, c)| (s, std::cmp::Reverse(c.len())))
        .map(|(_, c)| c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_hits_and_misses() {
        assert!(matches("ffx", "firefox").is_some());
        assert!(matches("calc", "org.gnome.Calculator").is_some());
        assert!(matches("firefox", "firefox").is_some());
        assert!(matches("xyz", "firefox").is_none());
        assert!(matches("firefoxx", "firefox").is_none()); // query longer than hits
    }

    #[test]
    fn case_insensitive() {
        assert!(matches("CALC", "org.gnome.Calculator").is_some());
        assert!(matches("calc", "ORG.GNOME.CALCULATOR").is_some());
    }

    #[test]
    fn empty_query_matches_everything() {
        assert_eq!(matches("", "anything"), Some(0));
    }

    #[test]
    fn exact_and_prefix_beat_scattered() {
        let exact = matches("bash", "bash").unwrap();
        let prefix = matches("bash", "bash-completion").unwrap();
        let scattered = matches("bash", "libbabl-sh-something").unwrap_or(i32::MIN);
        assert!(exact > prefix, "exact {exact} <= prefix {prefix}");
        assert!(
            prefix > scattered,
            "prefix {prefix} <= scattered {scattered}"
        );
    }

    #[test]
    fn word_boundary_bonus_helps_reverse_dns_names() {
        // "calc" hitting the Calculator segment must beat a scatter.
        let boundary = matches("calc", "org.gnome.Calculator").unwrap();
        let scatter = matches("calc", "vlc-plugin-all").unwrap_or(i32::MIN);
        assert!(boundary > scatter);
    }

    #[test]
    fn best_match_picks_the_obvious_candidate() {
        let names = ["firefox", "firefox-developer-edition", "readline", "bash"];
        assert_eq!(best_match("firefx", names), Some("firefox"));
        assert_eq!(best_match("readlin", names), Some("readline"));
        assert_eq!(best_match("zzz", names), None);
    }
}
