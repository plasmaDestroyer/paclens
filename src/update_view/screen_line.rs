//! One settled pacman screen line → what it means (#81).

#[derive(Debug, Clone, PartialEq)]
pub struct PlanRow {
    pub repo: String,
    pub name: String,
    pub old: Option<String>,
    pub new: String,
    pub net: String,
    pub download: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ScreenEvent {
    SyncStart,
    Db(String),
    UpgradeStart,
    NothingToDo,
    Warning(String),
    Error(String),
    PlanHeader {
        count: usize,
    },
    PlanRow(PlanRow),
    PlanNames(Vec<(String, String)>),
    PlanTotal {
        label: String,
        value: String,
    },
    Prompt(String),
    RetrieveStart,
    DownloadTotal {
        done: usize,
        total: usize,
        pct: u8,
        /// pacman's `363.0 MiB  5.12 MiB/s 00:02:51`: bytes, rate, time left.
        detail: String,
    },
    Check {
        name: String,
        done: usize,
        total: usize,
        pct: u8,
    },
    PreHooks,
    PostHooks,
    Processing,
    Hook {
        index: usize,
        total: usize,
        desc: String,
    },
    Package {
        index: usize,
        total: usize,
        verb: String,
        name: String,
        pct: u8,
    },
    Pacnew(String),
    Optdepends(String),
    Other(String),
}

const VERBS: [&str; 5] = [
    "installing",
    "upgrading",
    "reinstalling",
    "downgrading",
    "removing",
];
const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];

/// pacman's `[Y/n]` / `[y/N]` question, bare or with the answer echoed
/// after it (`[Y/n] y`).
pub fn is_prompt(l: &str) -> bool {
    ["[Y/n]", "[y/N]"].iter().any(|q| {
        l.rfind(q).is_some_and(|i| {
            let answer = l[i + q.len()..].trim();
            answer.len() <= 3 && answer.chars().all(|c| c.is_ascii_alphabetic())
        })
    })
}

/// Could this unfinished line be pacman (or sudo) waiting for an answer?
/// A progress row — download, checks, a hook's bar — ends in `NN%` and is
/// only paused, never asking.
pub fn may_be_question(partial: &str) -> bool {
    let t = partial.trim_end();
    !t.is_empty() && !t.ends_with('%')
}

pub fn classify_screen(line: &str, in_sync: bool) -> ScreenEvent {
    use ScreenEvent::*;
    let t = line.trim();
    if is_prompt(t) {
        return Prompt(t.to_string());
    }
    match t {
        ":: Synchronizing package databases..." => return SyncStart,
        ":: Starting full system upgrade..." => return UpgradeStart,
        ":: Retrieving packages..." => return RetrieveStart,
        ":: Running pre-transaction hooks..." => return PreHooks,
        ":: Running post-transaction hooks..." => return PostHooks,
        ":: Processing package changes..." => return Processing,
        "there is nothing to do" => return NothingToDo,
        _ => {}
    }
    // pacman writes `warning:`; systemd, from a scriptlet, `Warning:`.
    if let Some(w) = t
        .strip_prefix("warning: ")
        .or_else(|| t.strip_prefix("Warning: "))
    {
        return match w.split_once(" installed as ") {
            Some((_, p)) if p.ends_with(".pacnew") => Pacnew(p.to_string()),
            _ => Warning(w.to_string()),
        };
    }
    if let Some(e) = t.strip_prefix("error: ") {
        return Error(e.to_string());
    }
    if in_sync && let Some(db) = sync_db(line) {
        return Db(db.to_string());
    }
    if let Some(p) = t.strip_prefix("Optional dependencies for ") {
        return Optdepends(p.to_string());
    }
    plan_line(t)
        .or_else(|| counted_line(t))
        .unwrap_or_else(|| Other(line.to_string()))
}

/// A real database line during sync: ` core`, ` core is up to date`,
/// ` core downloading...` or ` core … NN%`. Anything else is not one.
fn sync_db(line: &str) -> Option<&str> {
    let t = line.strip_prefix(' ')?.trim();
    let (db, rest) = t.split_once(char::is_whitespace).unwrap_or((t, ""));
    let rest = rest.trim();
    let plausible = !db.is_empty()
        && db
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    let db_form = rest.is_empty()
        || rest == "is up to date"
        || rest == "downloading..."
        || bar_pct(rest).is_some();
    (plausible && db_form).then_some(db)
}

/// Plan header, plan names, plan totals and verbose plan rows.
fn plan_line(t: &str) -> Option<ScreenEvent> {
    if let Some(rest) = t
        .strip_prefix("Packages (")
        .or_else(|| t.strip_prefix("Package ("))
    {
        let (n, rest) = rest.split_once(')')?;
        let count = n.trim().parse().ok()?;
        let rest = rest.trim();
        if rest.starts_with("Old Version") || rest.starts_with("New Version") {
            return Some(ScreenEvent::PlanHeader { count });
        }
        return plan_names(rest).map(ScreenEvent::PlanNames);
    }
    if let Some((label, value)) = t.split_once(':')
        && label.ends_with(" Size")
    {
        return Some(ScreenEvent::PlanTotal {
            label: label.to_string(),
            value: value.trim().to_string(),
        });
    }
    let tok: Vec<&str> = t.split_whitespace().collect();
    let (repo, name) = tok.first()?.split_once('/')?;
    // Sizes are (number, unit) pairs read from the end: net, then download.
    let mut end = tok.len();
    let mut sizes = Vec::new();
    while sizes.len() < 2
        && end >= 3
        && UNITS.contains(&tok[end - 1])
        && tok[end - 2].parse::<f64>().is_ok()
    {
        sizes.push(format!("{} {}", tok[end - 2], tok[end - 1]));
        end -= 2;
    }
    let versions = &tok[1..end];
    let (old, new) = match versions {
        [new] => (None, new.to_string()),
        [old, new] => (Some(old.to_string()), new.to_string()),
        _ => return None,
    };
    let mut sizes = sizes.into_iter().rev();
    let net = sizes.next()?;
    Some(ScreenEvent::PlanRow(PlanRow {
        repo: repo.to_string(),
        name: name.to_string(),
        old,
        new,
        net,
        download: sizes.next(),
    }))
}

/// `name-ver-rel` words → (name, `ver-rel`); `None` if any word is not one.
pub(crate) fn plan_names(words: &str) -> Option<Vec<(String, String)>> {
    words
        .split_whitespace()
        .map(|p| {
            let mut it = p.rsplitn(3, '-');
            let (rel, ver, name) = (it.next()?, it.next()?, it.next()?);
            let rel_ok = rel.starts_with(|c: char| c.is_ascii_digit())
                && rel.chars().all(|c| c.is_ascii_digit() || c == '.');
            (rel_ok && !ver.is_empty() && !name.is_empty())
                .then(|| (name.to_string(), format!("{ver}-{rel}")))
        })
        .collect()
}

/// `( i/N) …` lines (hooks, packages, checks) and ` Total ( i/N) … NN%`.
fn counted_line(t: &str) -> Option<ScreenEvent> {
    let (total_bar, t) = match t.strip_prefix("Total ") {
        Some(rest) => (true, rest),
        None => (false, t),
    };
    let (counts, rest) = t.strip_prefix('(')?.split_once(')')?;
    let (i, n) = counts.split_once('/')?;
    let (index, total): (usize, usize) = (i.trim().parse().ok()?, n.trim().parse().ok()?);
    let pct = bar_pct(rest);
    if total_bar {
        let before_bar = rest.split('[').next().unwrap_or_default();
        return Some(ScreenEvent::DownloadTotal {
            done: index,
            total,
            pct: pct?,
            detail: before_bar.split_whitespace().collect::<Vec<_>>().join(" "),
        });
    }
    let Some(pct) = pct else {
        return Some(ScreenEvent::Hook {
            index,
            total,
            desc: rest.trim().to_string(),
        });
    };
    // Text before the bar: drop "NN%", then a bracketed bar if present.
    let text = rest
        .trim_end()
        .trim_end_matches('%')
        .trim_end_matches(|c: char| c.is_ascii_digit());
    let text = text.trim_end();
    let text = match text
        .strip_suffix(']')
        .and_then(|s| s.rfind('[').map(|i| &s[..i]))
    {
        Some(s) => s,
        None => text,
    }
    .trim();
    let mut words = text.split_whitespace();
    match (words.next(), words.next()) {
        (Some(verb), Some(name)) if VERBS.contains(&verb) => Some(ScreenEvent::Package {
            index,
            total,
            verb: verb.to_string(),
            name: name.to_string(),
            pct,
        }),
        _ => Some(ScreenEvent::Check {
            name: text.to_string(),
            done: index,
            total,
            pct,
        }),
    }
}

/// Trailing ` NN%` read by structure, whatever glyphs draw the bar.
pub(crate) fn bar_pct(line: &str) -> Option<u8> {
    let s = line.trim_end().strip_suffix('%')?;
    let digits = s.len() - s.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    let (head, num) = s.split_at(s.len() - digits);
    if !(1..=3).contains(&digits) || !(head.is_empty() || head.ends_with(char::is_whitespace)) {
        return None;
    }
    num.parse().ok().filter(|p| *p <= 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemd_capital_warning_is_a_warning() {
        let l = "Warning: The unit file of warp-svc.service changed on disk. Run 'systemctl daemon-reload' to reload units.";
        assert!(
            matches!(classify_screen(l, false), ScreenEvent::Warning(w) if w.starts_with("The unit file"))
        );
    }

    #[test]
    fn paused_progress_rows_are_not_questions() {
        assert!(!may_be_question(
            " Total ( 3/79)   363.0 MiB  5.12 MiB/s 00:02:51 [---C o o] 29%"
        ));
        assert!(!may_be_question(
            "(10/75) checking package integrity   [C o o o ]   0%"
        ));
        assert!(may_be_question(":: Proceed with installation? [Y/n] "));
        assert!(may_be_question("[sudo] password for someone:"));
        assert!(!may_be_question("   "));
    }

    #[test]
    fn classifies_fixture_lines() {
        use ScreenEvent::*;
        assert!(matches!(
            classify_screen(":: Synchronizing package databases...", false),
            SyncStart
        ));
        assert!(matches!(classify_screen(" cachyos-v3", true), Db(d) if d == "cachyos-v3"));
        assert!(matches!(
            classify_screen(
                "Package (38)                Old Version               New Version               Net Change  Download Size",
                false
            ),
            PlanHeader { count: 38 }
        ));
        assert!(
            matches!(classify_screen("extra/luajit                2.1.1791890110+634daf2-1  2.1.1791988261+2a8e017-1    0.00 MiB       0.33 MiB", false),
            PlanRow(r) if r.repo == "extra" && r.name == "luajit" && r.old.as_deref() == Some("2.1.1791890110+634daf2-1"))
        );
        assert!(matches!(
            classify_screen(":: Proceed with installation? [Y/n]", false),
            Prompt(_)
        ));
        assert!(matches!(
            classify_screen(
                " Total (11/38)    87.4 MiB  11.9 MiB/s 00:51 [-------C  o  o ]  12%",
                false
            ),
            DownloadTotal {
                done: 11,
                total: 38,
                pct: 12,
                ref detail,
            } if detail == "87.4 MiB 11.9 MiB/s 00:51"
        ));
        assert!(matches!(
            classify_screen(
                "error: failed retrieving file 'luajit-2.1.1791988261+2a8e017-1-x86_64.pkg.tar.zst' from archlinux.cachyos.org : The requested URL returned error: 404",
                false
            ),
            Error(_)
        ));
        assert!(matches!(
            classify_screen("(38/38) checking keys in keyring    [----] 100%", false),
            Check {
                done: 38,
                total: 38,
                pct: 100,
                ..
            }
        ));
        assert!(matches!(
            classify_screen("( 7/13) Restart a running sshd.service", false),
            Hook {
                index: 7,
                total: 13,
                ..
            }
        ));
        assert!(
            matches!(classify_screen("(28/38) upgrading luajit     [----] 100%", false), Package { index: 28, verb, name, .. } if verb == "upgrading" && name == "luajit")
        );
        assert!(matches!(classify_screen("==> root: 1001", false), Other(_)));
    }

    #[test]
    fn bar_pct_ignores_glyphs() {
        assert_eq!(bar_pct("x [\u{e000}\u{e001}]  42%"), Some(42));
    }

    #[test]
    fn nonverbose_plan_line_gives_names_and_new_versions() {
        assert!(
            matches!(classify_screen("Packages (2) curl-8.23.0-1  zstd-1.5.7-4", false),
            ScreenEvent::PlanNames(v) if v == vec![("curl".into(), "8.23.0-1".into()), ("zstd".into(), "1.5.7-4".into())])
        );
    }

    #[test]
    fn plan_names_needs_a_numeric_pkgrel() {
        assert_eq!(
            plan_names("brave-origin-bin-1:1.97.57-1  qt6-base-6.12.0-3.1"),
            Some(vec![
                ("brave-origin-bin".into(), "1:1.97.57-1".into()),
                ("qt6-base".into(), "6.12.0-3.1".into())
            ])
        );
        assert_eq!(plan_names("not-a-name"), None);
        assert_eq!(plan_names("curl-8.23.0-1 oops"), None);
        assert_eq!(plan_names(""), Some(vec![]));
    }

    #[test]
    fn nothing_to_do() {
        assert!(matches!(
            classify_screen(" there is nothing to do", false),
            ScreenEvent::NothingToDo
        ));
    }

    #[test]
    fn bar_pct_needs_a_trailing_percent() {
        assert_eq!(bar_pct("( 7/13) Restart a running sshd.service"), None);
        assert_eq!(bar_pct("x 100%"), Some(100));
        assert_eq!(bar_pct("x 1000%"), None);
        assert_eq!(bar_pct("x100%"), None);
    }

    #[test]
    fn plan_row_without_old_version_or_download() {
        let ScreenEvent::PlanRow(r) =
            classify_screen("extra/foo   1.0-1    2.00 MiB   0.50 MiB", false)
        else {
            panic!("not a row");
        };
        assert_eq!(
            (r.old, r.new.as_str(), r.net.as_str(), r.download.as_deref()),
            (None, "1.0-1", "2.00 MiB", Some("0.50 MiB"))
        );
        let ScreenEvent::PlanRow(r) =
            classify_screen("core/curl   8.22.0-1   8.23.0-1   -0.01 MiB", false)
        else {
            panic!("not a row");
        };
        assert_eq!(
            (r.old.as_deref(), r.new.as_str(), r.net.as_str(), r.download),
            (Some("8.22.0-1"), "8.23.0-1", "-0.01 MiB", None)
        );
    }

    #[test]
    fn plan_totals() {
        assert!(
            matches!(classify_screen("Total Download Size:    702.91 MiB", false),
            ScreenEvent::PlanTotal { label, value } if label == "Total Download Size" && value == "702.91 MiB")
        );
        assert!(matches!(
            classify_screen("Net Upgrade Size:         1.29 MiB", false),
            ScreenEvent::PlanTotal { .. }
        ));
    }

    #[test]
    fn section_markers() {
        use ScreenEvent::*;
        assert!(matches!(
            classify_screen(":: Starting full system upgrade...", false),
            UpgradeStart
        ));
        assert!(matches!(
            classify_screen(":: Retrieving packages...", false),
            RetrieveStart
        ));
        assert!(matches!(
            classify_screen(":: Running pre-transaction hooks...", false),
            PreHooks
        ));
        assert!(matches!(
            classify_screen(":: Running post-transaction hooks...", false),
            PostHooks
        ));
        assert!(matches!(
            classify_screen(":: Processing package changes...", false),
            Processing
        ));
    }

    #[test]
    fn warnings_pacnew_and_prompts() {
        use ScreenEvent::*;
        assert!(
            matches!(classify_screen("warning: rate-mirrors: local (0.33.0-1) is newer than cachyos (0.31.0-2)", false),
            Warning(w) if w == "rate-mirrors: local (0.33.0-1) is newer than cachyos (0.31.0-2)")
        );
        assert!(
            matches!(classify_screen("warning: /etc/pacman.conf installed as /etc/pacman.conf.pacnew", false),
            Pacnew(p) if p == "/etc/pacman.conf.pacnew")
        );
        assert!(matches!(
            classify_screen(":: Replace foo with extra/bar? [Y/n]", false),
            Prompt(_)
        ));
        assert!(matches!(
            classify_screen(":: Remove anyway? [y/N] ", false),
            Prompt(_)
        ));
        assert!(
            matches!(classify_screen("Optional dependencies for firefox", false), Optdepends(p) if p == "firefox")
        );
    }

    #[test]
    fn db_only_while_syncing() {
        assert!(
            matches!(classify_screen(" core  130.4 KiB  1.2 MiB/s 00:00 [----] 100%", true), ScreenEvent::Db(d) if d == "core")
        );
        assert!(matches!(
            classify_screen(" core", false),
            ScreenEvent::Other(_)
        ));
    }

    #[test]
    fn only_real_database_lines_count_as_sync_output() {
        for db_line in [
            " core",
            " extra  8.2 MiB  12.1 MiB/s 00:01 [####] 100%",
            " core is up to date",
            " multilib downloading...",
        ] {
            assert!(
                matches!(classify_screen(db_line, true), ScreenEvent::Db(_)),
                "{db_line}"
            );
        }
        for other in [
            "unexpected diagnostic detail",
            " unexpected diagnostic detail",
            " c/re",
        ] {
            assert_eq!(
                classify_screen(other, true),
                ScreenEvent::Other(other.to_string())
            );
        }
    }

    #[test]
    fn three_digit_hook_index_and_package_verbs() {
        assert!(
            matches!(classify_screen("(1/3) Marking sshd.service for restart...", false), ScreenEvent::Hook { index: 1, total: 3, desc } if desc == "Marking sshd.service for restart...")
        );
        assert!(matches!(
            classify_screen("( 3/38) installing zstd  [##] 40%", false),
            ScreenEvent::Package { pct: 40, .. }
        ));
        assert!(
            matches!(classify_screen("(38/38) checking package integrity   [--] 100%", false), ScreenEvent::Check { name, .. } if name == "checking package integrity")
        );
    }

    #[test]
    fn fixture_rows_packages_and_hooks_all_classified() {
        let lines = crate::update_view::screen::fixture_lines();
        let (mut rows, mut pkgs, mut hooks) = (0, 0, 0);
        for l in &lines {
            match classify_screen(l, false) {
                ScreenEvent::PlanRow(_) => rows += 1,
                ScreenEvent::Package { .. } => pkgs += 1,
                ScreenEvent::Hook { .. } => hooks += 1,
                _ => {}
            }
        }
        assert_eq!((rows, pkgs, hooks), (38, 38, 16));
    }
}
