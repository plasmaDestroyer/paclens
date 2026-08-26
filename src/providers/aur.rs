//! The AUR side of libalpm: foreign packages and their updates via whichever
//! AUR helper is installed.
//!
//! AUR packages are installed through pacman, so the pacman provider already
//! scanned their full metadata — this module only *identifies* them
//! (`pacman -Qm`, which works with no helper at all) and detects their updates
//! (`<helper> -Qua`). A helper is optional: none installed means the `aur`
//! source shows as not found and update detection is skipped, but the packages
//! still list under the `aur` source. Not a `Provider` impl on purpose —
//! source-specific logic, no generic shortcuts (P6).
//!
//! A helper is **never run under sudo** — every one of them builds as the user
//! and self-elevates for the install step. That was never a paru rule.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::model::{PendingUpdate, SourceId};
use crate::providers::{CommandRunner, ProviderError, pacman};

/// An AUR helper paclens knows how to drive.
///
/// Variant order is detection priority (paru → yay → pikaur), and
/// [`AurHelper::ALL`] depends on it, so do not reorder casually.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AurHelper {
    Paru,
    Yay,
    Pikaur,
}

impl AurHelper {
    /// Every known helper, in detection priority order.
    pub const ALL: [AurHelper; 3] = [AurHelper::Paru, AurHelper::Yay, AurHelper::Pikaur];

    /// The binary name, which is also the config value that pins it.
    pub fn bin(self) -> &'static str {
        match self {
            AurHelper::Paru => "paru",
            AurHelper::Yay => "yay",
            AurHelper::Pikaur => "pikaur",
        }
    }

    /// Parse a config value. Case-insensitive; unknown names are `None`.
    pub fn parse(name: &str) -> Option<AurHelper> {
        let name = name.trim().to_ascii_lowercase();
        AurHelper::ALL.into_iter().find(|h| h.bin() == name)
    }
}

/// What [`choose`] decided, and why — the "why" exists so the caller can say
/// so rather than silently doing something other than what the config asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelperChoice {
    /// Nothing configured; this is the first helper found on `PATH`.
    Detected(AurHelper),
    /// Configured, and present.
    Pinned(AurHelper),
    /// Configured but not installed, so detection ran anyway and found this.
    /// A stale config is not a fatal error (design §13).
    FellBack { configured: String, to: AurHelper },
    /// Configured but not installed, and nothing else is either.
    ConfiguredMissing { configured: String },
    /// Nothing configured and no helper on `PATH`.
    None,
}

impl HelperChoice {
    /// The helper to actually use, if there is one.
    pub fn helper(&self) -> Option<AurHelper> {
        match self {
            HelperChoice::Detected(h) | HelperChoice::Pinned(h) => Some(*h),
            HelperChoice::FellBack { to, .. } => Some(*to),
            HelperChoice::ConfiguredMissing { .. } | HelperChoice::None => None,
        }
    }
}

/// Pick a helper from the config value and what is installed.
///
/// Pure: `present` is passed in rather than probed, so the whole decision is
/// testable without a `PATH`. An empty (or whitespace) `configured` means
/// autodetect; anything else pins, and a pin that is not installed falls back
/// to detection rather than failing.
///
/// An unknown name is treated exactly like a known-but-missing one: paclens
/// will not run a binary it does not know the argv for.
pub fn choose(configured: &str, present: impl Fn(AurHelper) -> bool) -> HelperChoice {
    let first_present = || AurHelper::ALL.into_iter().find(|h| present(*h));
    let configured = configured.trim();
    if configured.is_empty() {
        return match first_present() {
            Some(h) => HelperChoice::Detected(h),
            None => HelperChoice::None,
        };
    }
    if let Some(pinned) = AurHelper::parse(configured)
        && present(pinned)
    {
        return HelperChoice::Pinned(pinned);
    }
    match first_present() {
        Some(to) => HelperChoice::FellBack {
            configured: configured.to_string(),
            to,
        },
        None => HelperChoice::ConfiguredMissing {
            configured: configured.to_string(),
        },
    }
}

/// [`choose`], against the real `PATH`.
pub fn detect(configured: &str) -> HelperChoice {
    choose(configured, |h| super::binary_on_path(h.bin()))
}

/// The update step's argv: AUR packages only (`-Sua`); the pacman step owns
/// the repos. No `--noconfirm` — the helper's prompts run in the pty console.
pub fn update_command(helper: AurHelper) -> Vec<String> {
    vec![helper.bin().to_string(), "-Sua".to_string()]
}

/// Names of foreign (non-repo) packages, via `pacman -Qm`. Needs pacman, not
/// paru. `pacman -Qm` exits 1 with empty output on none — not an error.
pub fn foreign_names(runner: &dyn CommandRunner) -> Result<HashSet<String>, ProviderError> {
    let out = runner
        .run("pacman", &["-Qm"])
        .map_err(|source| ProviderError::Exec {
            program: "pacman".to_string(),
            source,
        })?;
    match out.exit_code {
        0 => Ok(out
            .stdout
            .lines()
            .filter_map(|l| l.split_whitespace().next())
            .map(|n| n.to_string())
            .collect()),
        1 if out.stdout.trim().is_empty() => Ok(HashSet::new()),
        code => Err(ProviderError::CommandFailed {
            program: "pacman -Qm".to_string(),
            exit_code: code,
            stderr: out.stderr,
        }),
    }
}

/// Pending AUR updates via `<helper> -Qua` — the same `name old -> new` line
/// format as `pacman -Qu`, and the same exit-1-means-none convention.
/// `devel` adds `--devel`: the helper compares VCS (`-git`) packages against
/// the upstream HEAD instead of the version string (config `scan.aur_devel`;
/// slower, off by default).
///
/// The argv is shared across helpers because `-Qua`, `-Sua` and `--devel` are
/// the same in all three. Whether each one *also* shares paru's
/// exit-1-means-none convention is verified per helper in #52 and #53; until
/// then a helper that disagrees reports a `CommandFailed` rather than a wrong
/// number, which is the failure mode design §3 asks for.
pub fn scan_updates(
    runner: &dyn CommandRunner,
    helper: AurHelper,
    devel: bool,
) -> Result<Vec<PendingUpdate>, ProviderError> {
    let mut args = vec!["-Qua"];
    if devel {
        args.push("--devel");
    }
    let bin = helper.bin();
    let out = runner
        .run(bin, &args)
        .map_err(|source| ProviderError::Exec {
            program: bin.to_string(),
            source,
        })?;
    match out.exit_code {
        0 => Ok(pacman::parse_updates_as(&out.stdout, SourceId::aur())),
        1 if out.stdout.trim().is_empty() => Ok(Vec::new()),
        code => Err(ProviderError::CommandFailed {
            program: format!("{bin} -Qua"),
            exit_code: code,
            stderr: out.stderr,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::test_support::MockRunner;

    const QM: &str = include_str!("../../tests/fixtures/aur/qm.txt");
    const QUA: &str = include_str!("../../tests/fixtures/aur/qua.txt");

    #[test]
    fn foreign_names_reads_qm() {
        let runner = MockRunner::new().with("pacman -Qm", QM, 0);
        let names = foreign_names(&runner).unwrap();
        assert_eq!(names.len(), 6);
        assert!(names.contains("timr-bin"));
        assert!(names.contains("visual-studio-code-bin"));
    }

    #[test]
    fn no_foreign_packages_is_empty_not_error() {
        let runner = MockRunner::new().with("pacman -Qm", "", 1);
        assert!(foreign_names(&runner).unwrap().is_empty());
    }

    #[test]
    fn qua_updates_parse_with_the_aur_source() {
        let runner = MockRunner::new().with("paru -Qua", QUA, 0);
        let ups = scan_updates(&runner, AurHelper::Paru, false).unwrap();
        assert_eq!(ups.len(), 2);
        assert_eq!(ups[0].package_name, "timr-bin");
        assert_eq!(ups[0].current_version, "1.11.0-1");
        assert_eq!(ups[0].available_version, "1.12.0-1");
        assert_eq!(ups[0].source_id, SourceId::aur());
    }

    #[test]
    fn exit_one_with_no_output_means_no_updates() {
        let runner = MockRunner::new().with("paru -Qua", "", 1);
        assert!(
            scan_updates(&runner, AurHelper::Paru, false)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn devel_flag_changes_the_command() {
        let runner = MockRunner::new().with("paru -Qua --devel", QUA, 0);
        assert_eq!(
            scan_updates(&runner, AurHelper::Paru, true).unwrap().len(),
            2
        );
        // Without the mock for plain -Qua, the non-devel call must fail.
        assert!(scan_updates(&runner, AurHelper::Paru, false).is_err());
    }

    #[test]
    fn real_failure_is_an_error() {
        let runner = MockRunner::new().with("paru -Qua", "boom", 4);
        assert!(scan_updates(&runner, AurHelper::Paru, false).is_err());
    }

    #[test]
    fn update_command_is_aur_only_without_noconfirm() {
        let cmd = update_command(AurHelper::Paru);
        assert_eq!(cmd, vec!["paru", "-Sua"]);
        assert!(!cmd.iter().any(|a| a.contains("noconfirm")));
    }

    #[test]
    fn every_helper_gets_its_own_argv() {
        assert_eq!(update_command(AurHelper::Yay), vec!["yay", "-Sua"]);
        assert_eq!(update_command(AurHelper::Pikaur), vec!["pikaur", "-Sua"]);
    }

    #[test]
    fn update_command_never_carries_noconfirm_for_any_helper() {
        for h in AurHelper::ALL {
            assert!(
                !update_command(h).iter().any(|a| a.contains("noconfirm")),
                "{} gained --noconfirm",
                h.bin()
            );
        }
    }

    #[test]
    fn scan_updates_calls_the_helper_it_was_given() {
        let runner = MockRunner::new().with("yay -Qua", QUA, 0);
        let ups = scan_updates(&runner, AurHelper::Yay, false).unwrap();
        assert_eq!(ups.len(), 2);
        // paru is not what ran, so asking for it must fail rather than
        // quietly reusing yay's output.
        assert!(scan_updates(&runner, AurHelper::Paru, false).is_err());
    }

    #[test]
    fn pikaur_is_driven_the_same_way() {
        let runner = MockRunner::new().with("pikaur -Qua", QUA, 0);
        assert_eq!(
            scan_updates(&runner, AurHelper::Pikaur, false)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn parse_accepts_known_names_and_rejects_the_rest() {
        assert_eq!(AurHelper::parse("paru"), Some(AurHelper::Paru));
        assert_eq!(AurHelper::parse("yay"), Some(AurHelper::Yay));
        assert_eq!(AurHelper::parse("pikaur"), Some(AurHelper::Pikaur));
        assert_eq!(AurHelper::parse("  YAY "), Some(AurHelper::Yay));
        assert_eq!(AurHelper::parse("aurman"), None);
        assert_eq!(AurHelper::parse(""), None);
    }

    #[test]
    fn bin_names_are_distinct_and_match_parse() {
        for h in AurHelper::ALL {
            assert_eq!(AurHelper::parse(h.bin()), Some(h));
        }
    }

    /// `present` for a fixed set of installed helpers.
    fn installed(set: &[AurHelper]) -> impl Fn(AurHelper) -> bool + use<'_> {
        move |h| set.contains(&h)
    }

    #[test]
    fn empty_config_detects_the_first_helper_on_path() {
        let c = choose("", installed(&[AurHelper::Paru]));
        assert_eq!(c, HelperChoice::Detected(AurHelper::Paru));
        assert_eq!(c.helper(), Some(AurHelper::Paru));
    }

    #[test]
    fn detection_prefers_paru_then_yay_then_pikaur() {
        let all = choose("", installed(&AurHelper::ALL));
        assert_eq!(all.helper(), Some(AurHelper::Paru));

        let no_paru = choose("", installed(&[AurHelper::Yay, AurHelper::Pikaur]));
        assert_eq!(no_paru.helper(), Some(AurHelper::Yay));

        let only_pikaur = choose("", installed(&[AurHelper::Pikaur]));
        assert_eq!(only_pikaur.helper(), Some(AurHelper::Pikaur));
    }

    #[test]
    fn detection_order_does_not_depend_on_the_order_given() {
        // The caller's slice order must not leak into the decision.
        let c = choose("", installed(&[AurHelper::Pikaur, AurHelper::Yay]));
        assert_eq!(c.helper(), Some(AurHelper::Yay));
    }

    #[test]
    fn a_configured_helper_pins_it_even_when_paru_is_present() {
        let c = choose("yay", installed(&[AurHelper::Paru, AurHelper::Yay]));
        assert_eq!(c, HelperChoice::Pinned(AurHelper::Yay));
    }

    #[test]
    fn configured_but_missing_falls_back_and_reports_it() {
        let c = choose("pikaur", installed(&[AurHelper::Yay]));
        assert_eq!(
            c,
            HelperChoice::FellBack {
                configured: "pikaur".to_string(),
                to: AurHelper::Yay,
            }
        );
        // Falling back still yields a usable helper — not a fatal error.
        assert_eq!(c.helper(), Some(AurHelper::Yay));
    }

    #[test]
    fn configured_and_nothing_installed_is_reported_not_guessed() {
        let c = choose("paru", installed(&[]));
        assert_eq!(
            c,
            HelperChoice::ConfiguredMissing {
                configured: "paru".to_string(),
            }
        );
        assert_eq!(c.helper(), None);
    }

    #[test]
    fn no_config_and_no_helper_is_none() {
        let c = choose("", installed(&[]));
        assert_eq!(c, HelperChoice::None);
        assert_eq!(c.helper(), None);
    }

    #[test]
    fn an_unknown_name_is_treated_as_missing_never_executed() {
        // paclens does not know aurman's argv, so it must not run it — the
        // installed helper wins instead.
        let c = choose("aurman", installed(&[AurHelper::Paru]));
        assert_eq!(
            c,
            HelperChoice::FellBack {
                configured: "aurman".to_string(),
                to: AurHelper::Paru,
            }
        );
        assert_eq!(c.helper(), Some(AurHelper::Paru));
    }

    #[test]
    fn whitespace_only_config_means_autodetect_not_a_pin() {
        assert_eq!(
            choose("   ", installed(&[AurHelper::Yay])),
            HelperChoice::Detected(AurHelper::Yay)
        );
    }

    #[test]
    fn a_pin_is_case_insensitive() {
        assert_eq!(
            choose("PiKaUr", installed(&[AurHelper::Paru, AurHelper::Pikaur])),
            HelperChoice::Pinned(AurHelper::Pikaur)
        );
    }

    #[test]
    fn choose_short_circuits_at_the_first_present_helper() {
        use std::cell::RefCell;
        let asked = RefCell::new(Vec::new());
        let c = choose("", |h| {
            asked.borrow_mut().push(h);
            true
        });
        assert_eq!(c.helper(), Some(AurHelper::Paru));
        // A paru user pays exactly one lookup, as they do today.
        assert_eq!(*asked.borrow(), vec![AurHelper::Paru]);
    }
}
