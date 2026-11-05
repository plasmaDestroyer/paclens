//! `paclens pacman <args>`: pacman under the live view (#81), and which
//! pacman arguments the passthrough refuses. Every refusal names a design §3
//! rule or keeps the view honest.

use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::PathBuf;

use crate::cli::history::PACMAN_LOG;
use crate::cli::style::Styles;
use crate::config::Config;
use crate::executor::{self, StepStatus, UpdateLog};
use crate::model::{ActionKind, ActionPlan, ActionStep, Package, SourceId};
use crate::scanner::cache::{Cache, cache_dir};
use crate::update_view::live::{ViewOptions, ViewRunner};

/// Run `pacman <args>` under the live view, as one privileged step. No
/// outer `sudo -v`: sudo caches per tty, and pacman gets its own pty, so a
/// primed timestamp would not reach it and sudo would ask twice.
pub fn run(
    config: &Config,
    args: &[String],
    stdin_is_tty: bool,
    styles: &Styles,
) -> anyhow::Result<()> {
    if let Err(reason) = check(args) {
        anyhow::bail!("refused: {reason} — run pacman directly for this");
    }
    if !stdin_is_tty {
        anyhow::bail!("pacman needs a terminal for its own prompts");
    }
    let plan = ActionPlan {
        steps: vec![ActionStep {
            source_id: SourceId::pacman(),
            kind: ActionKind::Update,
            targets: vec![],
            command: std::iter::once("pacman".to_string())
                .chain(args.iter().cloned())
                .collect(),
            label: "pacman".into(),
            privileged: true,
            interactive: true,
            pacman_db: true,
        }],
    };
    let report = executor::execute(
        &plan,
        &ViewRunner {
            opts: view_options(config, styles, false),
        },
        &mut UpdateLog::open_default()?,
        executor::sudo::detect(),
    );
    outcome(report.steps.first().map(|s| &s.status))
}

/// How the view runs here and in `update`. `primed`: a `sudo -v` on this
/// terminal came first (see [`ViewOptions::primed`]).
pub(crate) fn view_options(config: &Config, styles: &Styles, primed: bool) -> ViewOptions {
    ViewOptions {
        layout: config.update.layout(),
        motif: config.update.motif(),
        styles: styles.clone(),
        live: std::io::stdout().is_terminal(),
        keys: true,
        installed: installed(),
        log: PathBuf::from(PACMAN_LOG),
        transcript_dir: cache_dir().ok(),
        primed,
    }
}

/// The step's end as this command's result.
fn outcome(status: Option<&StepStatus>) -> anyhow::Result<()> {
    match status {
        Some(StepStatus::Skipped { reason }) => anyhow::bail!("pacman did not run: {reason}"),
        Some(StepStatus::Failed { detail }) => {
            anyhow::bail!("pacman failed: {detail} — transcript: paclens transcript")
        }
        _ => Ok(()),
    }
}

/// name → version from the scan cache, for the old version in the plan;
/// empty when there is no cache.
fn installed() -> HashMap<String, String> {
    let scan = Cache::locate().and_then(|c| c.read()).ok().flatten();
    scan.map(|s| pacman_versions(s.packages))
        .unwrap_or_default()
}

/// Only what pacman installed: a cargo `ripgrep` is not pacman's `ripgrep`.
fn pacman_versions(packages: Vec<Package>) -> HashMap<String, String> {
    packages
        .into_iter()
        .filter(|p| p.source_id == SourceId::pacman() || p.source_id == SourceId::aur())
        .map(|p| (p.name, p.version))
        .collect()
}

/// Every long option pacman accepts, and whether it takes a required value.
///
/// Taken from pacman 7.1.0 (CachyOS build, libalpm 16.0.1): `pacman -h` for
/// each operation, `man 8 pacman`, plus `--ask` and `--force`, which the help
/// hides but the binary accepts. The table has to be whole: getopt_long takes
/// any unique prefix, so whether a prefix is ambiguous depends on all of them.
#[rustfmt::skip]
const LONG: &[(&str, bool)] = &[
    // operations
    ("database", false), ("deptest", false), ("files", false), ("help", false),
    ("query", false), ("remove", false), ("sync", false), ("upgrade", false),
    ("version", false),
    // options that take a value
    ("arch", true), ("ask", true), ("assume-installed", true), ("cachedir", true),
    ("color", true), ("config", true), ("dbpath", true), ("gpgdir", true),
    ("hookdir", true), ("ignore", true), ("ignoregroup", true), ("logfile", true),
    ("overwrite", true), ("print-format", true), ("root", true), ("sysroot", true),
    // flags (`--debug` takes an optional value, so only as `--debug=N`)
    ("asdeps", false), ("asexplicit", false), ("cascade", false), ("changelog", false),
    ("check", false), ("clean", false), ("confirm", false), ("dbonly", false),
    ("debug", false), ("deps", false), ("disable-download-timeout", false),
    ("disable-sandbox", false), ("disable-sandbox-filesystem", false),
    ("disable-sandbox-network", false), ("disable-sandbox-syscalls", false),
    ("downloadonly", false), ("explicit", false), ("file", false), ("force", false),
    ("foreign", false), ("groups", false), ("info", false), ("list", false),
    ("machinereadable", false), ("native", false), ("needed", false),
    ("noconfirm", false), ("nodeps", false), ("noprogressbar", false), ("nosave", false),
    ("noscriptlet", false), ("owns", false), ("print", false), ("quiet", false),
    ("recursive", false), ("refresh", false), ("regex", false), ("search", false),
    ("sysupgrade", false), ("unneeded", false), ("unrequired", false),
    ("upgrades", false), ("verbose", false),
];

/// A long option as getopt_long finds it: the exact name, else the only
/// option the name is a prefix of. Unknown and ambiguous both give `None`.
fn resolve(name: &str) -> Option<(&'static str, bool)> {
    if let Some(&opt) = LONG.iter().find(|(n, _)| *n == name) {
        return Some(opt);
    }
    let mut hits = LONG.iter().filter(|(n, _)| n.starts_with(name));
    match (hits.next(), hits.next()) {
        (Some(&opt), None) => Some(opt),
        _ => None,
    }
}

/// `Err(reason)` when pacman would run with arguments the passthrough refuses.
/// The rules apply in a fixed order, so the reason is the first one that holds.
///
/// getopt_long reads options anywhere by default, but with `POSIXLY_CORRECT`
/// set it stops at the first target, so `-Sy foo -u` is a partial upgrade
/// there. Both readings must pass; the permuting one reports first.
pub fn check(args: &[String]) -> Result<(), &'static str> {
    rules(args, false)?;
    rules(args, true)
}

/// Parses `args` as getopt_long does (stopping at the first target when
/// `posix`) and applies the refusal rules to what it found.
fn rules(args: &[String], posix: bool) -> Result<(), &'static str> {
    let mut longs: Vec<&str> = Vec::new();
    let mut shorts = String::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--" || (posix && (arg == "-" || !arg.starts_with('-'))) {
            break;
        }
        if let Some(body) = arg.strip_prefix("--") {
            let (name, inline) = match body.split_once('=') {
                Some((name, _)) => (name, true),
                None => (body, false),
            };
            let (name, takes_value) = resolve(name).ok_or("unrecognised option")?;
            if takes_value && !inline {
                it.next();
            }
            longs.push(name);
        } else if let Some(letters) = arg.strip_prefix('-').filter(|l| !l.is_empty()) {
            // -b and -r take the rest of the cluster as value, else the next arg.
            match letters.find(['b', 'r']) {
                Some(i) => {
                    shorts.push_str(&letters[..=i]);
                    if i + 1 == letters.len() {
                        it.next();
                    }
                }
                None => shorts.push_str(letters),
            }
        }
    }
    let long = |n: &str| longs.contains(&n);
    let short = |c: char| shorts.contains(c);

    if long("noconfirm") || long("ask") {
        return Err("no silent answers (design §3)");
    }
    if long("overwrite") {
        return Err("--overwrite clobbers files other packages own");
    }
    if short('r')
        || short('b')
        || ["root", "sysroot", "dbpath", "logfile", "config"]
            .iter()
            .any(|n| long(n))
    {
        return Err("the view would watch the wrong system or log");
    }
    let mut ops: Vec<char> = "SRQUDFTVh".chars().filter(|&c| short(c)).collect();
    for (name, op) in [
        ("sync", 'S'),
        ("remove", 'R'),
        ("query", 'Q'),
        ("upgrade", 'U'),
        ("database", 'D'),
        ("files", 'F'),
        ("deptest", 'T'),
        ("version", 'V'),
        ("help", 'h'),
    ] {
        if long(name) {
            ops.push(op);
        }
    }
    if ops.contains(&'R') {
        return Err("removals go through `paclens why` and the dependency report (design §3)");
    }
    ops.sort_unstable();
    ops.dedup();
    if ops != ['S']
        || "csilgpw".chars().any(short)
        || [
            "clean",
            "search",
            "info",
            "list",
            "groups",
            "print",
            "print-format",
            "downloadonly",
        ]
        .iter()
        .any(|n| long(n))
    {
        return Err("only installs and upgrades (-S)");
    }
    if (short('y') || long("refresh")) && !(short('u') || long("sysupgrade")) {
        return Err("-y without -u is a partial upgrade (design §3)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_versions_are_pacman_and_aur_only() {
        let pkg = |name: &str, version: &str, source_id: SourceId| Package {
            name: name.into(),
            version: version.into(),
            source_id,
            install_reason: crate::model::InstallReason::Explicit,
            size_bytes: None,
            description: None,
            depends_on: vec![],
            required_by: vec![],
            optional_deps: vec![],
            provides: vec![],
            runtime: false,
            scope: None,
            foreign: false,
            signed: false,
            packager: None,
            repo_version: None,
        };
        let v = pacman_versions(vec![
            pkg("ripgrep", "14.1.1-1", SourceId::pacman()),
            pkg("paru", "2.0.4-1", SourceId::aur()),
            pkg("ripgrep", "14.1.0", SourceId::cargo()),
        ]);
        assert_eq!(v.get("ripgrep").map(String::as_str), Some("14.1.1-1"));
        assert_eq!(v.get("paru").map(String::as_str), Some("2.0.4-1"));
    }

    #[test]
    fn failure_detail_is_kept() {
        let failed = StepStatus::Failed {
            detail: "failed to open a pty: no ptys".into(),
        };
        let err = outcome(Some(&failed)).unwrap_err().to_string();
        assert!(
            err.contains("no ptys") && err.contains("paclens transcript"),
            "{err}"
        );
        assert!(outcome(Some(&StepStatus::Succeeded)).is_ok());
    }

    fn c(args: &[&str]) -> Result<(), &'static str> {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        check(&args)
    }

    #[test]
    fn refuses_noconfirm() {
        assert!(c(&["-Syu", "--noconfirm"]).is_err());
    }

    #[test]
    fn refuses_ask() {
        assert!(c(&["-S", "--ask=4", "foo"]).is_err());
    }

    #[test]
    fn refuses_refresh_without_sysupgrade() {
        assert!(c(&["-Sy", "foo"]).is_err());
    }

    #[test]
    fn refuses_remove() {
        assert!(c(&["-Rns", "foo"]).is_err());
    }

    #[test]
    fn refuses_overwrite() {
        assert!(c(&["-S", "--overwrite", "*", "foo"]).is_err());
    }

    #[test]
    fn refuses_query() {
        assert!(c(&["-Q"]).is_err());
    }

    #[test]
    fn refuses_search() {
        assert!(c(&["-Ss", "foo"]).is_err());
    }

    #[test]
    fn refuses_info() {
        assert!(c(&["-Si", "foo"]).is_err());
    }

    #[test]
    fn refuses_clean() {
        assert!(c(&["-Sc"]).is_err());
    }

    #[test]
    fn refuses_downloadonly() {
        assert!(c(&["-Sw", "foo"]).is_err());
    }

    #[test]
    fn refuses_upgrade_op() {
        assert!(c(&["-U", "x.pkg.tar.zst"]).is_err());
    }

    #[test]
    fn refuses_database_op() {
        assert!(c(&["-D", "--asdeps", "foo"]).is_err());
    }

    #[test]
    fn refuses_files_op() {
        assert!(c(&["-F", "foo"]).is_err());
    }

    #[test]
    fn refuses_deptest_op() {
        assert!(c(&["-T", "foo"]).is_err());
    }

    #[test]
    fn refuses_root() {
        assert!(c(&["-Syu", "--root", "/mnt"]).is_err());
    }

    #[test]
    fn refuses_short_root() {
        assert!(c(&["-Syu", "-r", "/mnt"]).is_err());
    }

    #[test]
    fn refuses_sysroot() {
        assert!(c(&["-Syu", "--sysroot", "/mnt"]).is_err());
    }

    #[test]
    fn refuses_dbpath() {
        assert!(c(&["-Syu", "--dbpath", "/x"]).is_err());
    }

    #[test]
    fn refuses_short_dbpath() {
        assert!(c(&["-Syu", "-b", "/x"]).is_err());
    }

    #[test]
    fn refuses_logfile() {
        assert!(c(&["-Syu", "--logfile", "/x"]).is_err());
    }

    #[test]
    fn refuses_config() {
        assert!(c(&["-Syu", "--config", "/x"]).is_err());
    }

    #[test]
    fn refuses_refresh_in_split_shorts() {
        assert!(c(&["-S", "-y", "foo"]).is_err());
    }

    #[test]
    fn refuses_abbreviated_noconfirm() {
        assert!(c(&["-Syu", "--nocon"]).is_err());
        assert!(c(&["-Syu", "--noconf"]).is_err());
    }

    #[test]
    fn refuses_abbreviated_overwrite_with_value() {
        assert!(c(&["-S", "--overw=*", "foo"]).is_err());
    }

    #[test]
    fn option_value_is_not_read_as_flags() {
        // `-u` is --ignore's value, so this is -Sy alone: a partial upgrade.
        assert!(c(&["-Sy", "--ignore", "-u"]).is_err());
    }

    #[test]
    fn refuses_unknown_long_option() {
        assert_eq!(c(&["-Syu", "--asdf"]), Err("unrecognised option"));
    }

    #[test]
    fn refuses_ambiguous_prefix() {
        // --as: --asdeps, --asexplicit, --ask, --assume-installed.
        assert_eq!(c(&["-Syu", "--as"]), Err("unrecognised option"));
    }

    #[test]
    fn refuses_list_groups_print() {
        assert!(c(&["-Sl"]).is_err());
        assert!(c(&["-Sg"]).is_err());
        assert!(c(&["-Sp", "foo"]).is_err());
    }

    #[test]
    fn refuses_version_and_help() {
        assert!(c(&["-V"]).is_err());
        assert!(c(&["-h"]).is_err());
        assert!(c(&["-S", "--help"]).is_err());
    }

    #[test]
    fn refuses_no_operation() {
        assert!(c(&[]).is_err());
    }

    #[test]
    fn rules_apply_in_order() {
        assert_eq!(
            c(&["-Rns", "--noconfirm", "foo"]),
            Err("no silent answers (design §3)")
        );
        assert_eq!(
            c(&["-Rns", "--root", "/mnt", "foo"]),
            Err("the view would watch the wrong system or log")
        );
        assert_eq!(c(&["-Q", "-y"]), Err("only installs and upgrades (-S)"));
    }

    #[test]
    fn refuses_what_posixly_correct_reads_as_partial() {
        // Under POSIXLY_CORRECT pacman stops at `foo`, so `-u` is a target.
        assert_eq!(
            c(&["-Sy", "foo", "-u"]),
            Err("-y without -u is a partial upgrade (design §3)")
        );
    }

    #[test]
    fn refuses_noconfirm_after_a_target() {
        assert!(c(&["-S", "foo", "--noconfirm"]).is_err());
    }

    #[test]
    fn allows_option_after_a_target_when_both_readings_pass() {
        assert_eq!(c(&["-S", "foo", "--needed"]), Ok(()));
        assert_eq!(c(&["-Syu", "foo"]), Ok(()));
    }

    #[test]
    fn allows_sysupgrade() {
        assert_eq!(c(&["-Syu"]), Ok(()));
        assert_eq!(c(&["-Syyu"]), Ok(()));
    }

    #[test]
    fn allows_install() {
        assert_eq!(c(&["-S", "foo"]), Ok(()));
        assert_eq!(c(&["-S", "--needed", "foo", "bar"]), Ok(()));
    }

    #[test]
    fn allows_ignore_with_value() {
        assert_eq!(c(&["-Syu", "--ignore", "linux"]), Ok(()));
    }

    #[test]
    fn allows_split_shorts() {
        assert_eq!(c(&["-S", "-y", "-u"]), Ok(()));
    }

    #[test]
    fn allows_color_inline_value() {
        assert_eq!(c(&["-Su", "--color=always"]), Ok(()));
    }

    #[test]
    fn allows_long_operation_and_prefix() {
        assert_eq!(c(&["--sync", "--refresh", "--sysupg"]), Ok(()));
    }

    #[test]
    fn args_after_double_dash_are_targets() {
        assert_eq!(c(&["-S", "--", "--noconfirm"]), Ok(()));
    }

    #[test]
    fn resolve_prefers_exact_match() {
        assert_eq!(resolve("print"), Some(("print", false)));
        assert_eq!(resolve("disable-sandbox"), Some(("disable-sandbox", false)));
        assert_eq!(resolve("print-f"), Some(("print-format", true)));
        assert_eq!(resolve("nocon"), Some(("noconfirm", false)));
        assert_eq!(resolve("as"), None);
        assert_eq!(resolve("zzz"), None);
    }
}
