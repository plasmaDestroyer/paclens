//! Comparing package versions the way libalpm does.
//!
//! Needed because "newer" is not a string or a number: `1:7.1-1` outranks
//! `7.2-1` (epoch wins), `1.0a` is older than `1.0`, and `1.0` is older than
//! `1.0.1`. Every one of those appears on a real machine — the epoch case was
//! found on the author's, where a departed repo left `linux-api-headers` at
//! `1:7.1-1` while the configured repos offer `7.2-1` (#78).
//!
//! Pure, and deliberately not a dependency: the algorithm is small, the
//! semantics are what pacman's own `vercmp` implements, and a table captured
//! from that binary is what pins it (`tests/fixtures/vercmp/pairs.tsv`).

use std::cmp::Ordering;

/// Compare two full versions (`[epoch:]version[-rel]`) as libalpm does.
pub fn compare(a: &str, b: &str) -> Ordering {
    let (epoch_a, rest_a) = split_epoch(a);
    let (epoch_b, rest_b) = split_epoch(b);
    // An epoch outranks everything after it: that is what it is for.
    match compare_segments(epoch_a, epoch_b) {
        Ordering::Equal => {}
        other => return other,
    }

    let (ver_a, rel_a) = split_rel(rest_a);
    let (ver_b, rel_b) = split_rel(rest_b);
    match compare_segments(ver_a, ver_b) {
        Ordering::Equal => {}
        other => return other,
    }

    // A missing pkgrel compares equal to any: `1.0` and `1.0-1` are the same
    // package as far as an upgrade is concerned, which is what pacman does.
    match (rel_a, rel_b) {
        (Some(x), Some(y)) => compare_segments(x, y),
        _ => Ordering::Equal,
    }
}

/// `1:7.1-1` → (`"1"`, `"7.1-1"`); no colon means epoch zero.
///
/// Follows pacman's `parseEVR`: the epoch is the run of leading digits, and
/// it counts only if a colon follows it. An *empty* run still counts —
/// `:1.0` is epoch zero with the colon consumed, not a version beginning
/// with a colon. Leaving it in changes what the version compares as, which a
/// fuzzed pair caught.
fn split_epoch(version: &str) -> (&str, &str) {
    let digits = version.bytes().take_while(|b| b.is_ascii_digit()).count();
    match version.as_bytes().get(digits) {
        Some(b':') => {
            let epoch = &version[..digits];
            let rest = &version[digits + 1..];
            (if epoch.is_empty() { "0" } else { epoch }, rest)
        }
        _ => ("0", version),
    }
}

/// `7.1-1` → (`"7.1"`, `Some("1")`), splitting at the *last* hyphen: a
/// version may contain hyphens, a pkgrel may not.
fn split_rel(version: &str) -> (&str, Option<&str>) {
    match version.rsplit_once('-') {
        Some((ver, rel)) => (ver, Some(rel)),
        None => (version, None),
    }
}

/// Compare two version strings segment by segment, alpm's `rpmvercmp`.
///
/// Both are walked together in runs of digits or of letters. Digit runs
/// compare numerically and outrank letter runs; when one side runs out first
/// it is older, *except* that a remaining alphabetic run makes its side older
/// — which is why `1.0a` precedes `1.0`.
fn compare_segments(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    loop {
        // Separators are skipped *inside* the loop, and only while both sides
        // still have something. Leaving the leftovers unskipped is what makes
        // `1.0+git` newer than `1.0` while `1.0a` is older: the final
        // comparison below looks at the raw remainder, and a separator there
        // means "a later revision" where a letter means "a pre-release".
        if a.is_empty() || b.is_empty() {
            break;
        }
        fn skip(s: &[u8]) -> (usize, &[u8]) {
            let n = s.iter().take_while(|c| !c.is_ascii_alphanumeric()).count();
            (n, &s[n..])
        }
        let (skipped_a, rest_a) = skip(a);
        let (skipped_b, rest_b) = skip(b);
        a = rest_a;
        b = rest_b;
        if a.is_empty() || b.is_empty() {
            break;
        }
        // How *many* separators were skipped is itself a comparison: more of
        // them sorts newer, which is what makes `1..0` outrank `1.0`. Without
        // this the two compare equal, and pacman disagrees.
        if skipped_a != skipped_b {
            return skipped_a.cmp(&skipped_b);
        }

        let numeric = a[0].is_ascii_digit();
        fn run(s: &[u8], numeric: bool) -> (&[u8], &[u8]) {
            let n = s
                .iter()
                .take_while(|c| {
                    if numeric {
                        c.is_ascii_digit()
                    } else {
                        c.is_ascii_alphabetic()
                    }
                })
                .count();
            (&s[..n], &s[n..])
        }
        let (seg_a, rest_a) = run(a, numeric);
        let (seg_b, rest_b) = run(b, numeric);

        // One side has a digit run where the other has letters: digits win,
        // because a numeric segment is always a later revision than a tag.
        if seg_b.is_empty() {
            return if numeric {
                Ordering::Greater
            } else {
                Ordering::Less
            };
        }

        let ordering = if numeric {
            // Leading zeroes carry no value; longer is larger once trimmed.
            fn trim(s: &[u8]) -> &[u8] {
                let n = s.iter().take_while(|c| **c == b'0').count();
                &s[n..]
            }
            let (x, y) = (trim(seg_a), trim(seg_b));
            x.len().cmp(&y.len()).then_with(|| x.cmp(y))
        } else {
            seg_a.cmp(seg_b)
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
        a = rest_a;
        b = rest_b;
    }

    // Whatever is left decides, and a remaining alphabetic run never beats an
    // empty string: `1.0a` is a pre-release of `1.0`, while `1.0+git` is
    // something built on top of it.
    match (a.first(), b.first()) {
        (None, None) => Ordering::Equal,
        (None, Some(c)) if !c.is_ascii_alphabetic() => Ordering::Less,
        (Some(c), _) if c.is_ascii_alphabetic() => Ordering::Less,
        _ => Ordering::Greater,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from pacman's own `vercmp` on a real machine: every version
    /// disagreement present there, a sample of agreeing pairs, and the edge
    /// cases the algorithm exists for.
    const PAIRS: &str = include_str!("../../tests/fixtures/vercmp/pairs.tsv");

    #[test]
    fn every_captured_pair_matches_what_vercmp_said() {
        let mut checked = 0;
        for line in PAIRS.lines() {
            let mut cols = line.split('\t');
            let (a, b, want) = (
                cols.next().expect("a"),
                cols.next().expect("b"),
                cols.next().expect("expected"),
            );
            let want = match want {
                "1" => Ordering::Greater,
                "-1" => Ordering::Less,
                _ => Ordering::Equal,
            };
            assert_eq!(compare(a, b), want, "vercmp {a:?} {b:?}");
            // And the reverse, which vercmp guarantees.
            assert_eq!(compare(b, a), want.reverse(), "vercmp {b:?} {a:?}");
            checked += 1;
        }
        assert!(checked > 100, "only {checked} pairs in the table");
    }

    /// 1500 random short strings over the characters that actually matter
    /// (`0 1 a b . : - + ~ _`), with what `vercmp` answered for each.
    /// Hand-picked cases test what the author thought of; this tests what
    /// they did not — it is how the epoch parsing was found to be wrong for
    /// a version like `:1.0`, where the colon is consumed by an empty epoch.
    const FUZZ: &str = include_str!("../../tests/fixtures/vercmp/fuzz.tsv");

    #[test]
    fn fuzzed_pairs_match_what_vercmp_said() {
        for line in FUZZ.lines() {
            let mut cols = line.split('\t');
            let (a, b, want) = (
                cols.next().expect("a"),
                cols.next().expect("b"),
                cols.next().expect("expected"),
            );
            let want = match want {
                "1" => Ordering::Greater,
                "-1" => Ordering::Less,
                _ => Ordering::Equal,
            };
            assert_eq!(compare(a, b), want, "vercmp {a:?} {b:?}");
        }
    }

    #[test]
    fn an_epoch_outranks_a_higher_version() {
        // Found in the wild: a departed repo left this installed while the
        // configured repos offer 7.2-1, and pacman calls the local one newer.
        assert_eq!(compare("1:7.1-1", "7.2-1"), Ordering::Greater);
        assert_eq!(compare("2:1.0", "1:9.9"), Ordering::Greater);
    }

    #[test]
    fn a_missing_pkgrel_compares_equal_to_any() {
        // `1.0` and `1.0-1` are the same package to an upgrade.
        assert_eq!(compare("1.0", "1.0-1"), Ordering::Equal);
        assert_eq!(compare("1.0-5", "1.0"), Ordering::Equal);
        // But two present pkgrels do compare.
        assert_eq!(compare("1.0-1", "1.0-2"), Ordering::Less);
    }

    #[test]
    fn a_trailing_letter_is_a_pre_release() {
        assert_eq!(compare("1.0a", "1.0"), Ordering::Less);
        assert_eq!(compare("1.0.1", "1.0a"), Ordering::Greater);
    }

    #[test]
    fn comparison_is_reflexive_and_ordering_is_total() {
        for v in ["1.0", "1:2.0-3", "20240101", "1.0a", ""] {
            assert_eq!(compare(v, v), Ordering::Equal, "{v:?}");
        }
    }
}
