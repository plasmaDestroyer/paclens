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

    /// The suggested cache-clean command for the cleanup screen — copiable
    /// text the user runs themselves, never executed by paclens.
    ///
    /// paru and yay both read `-a`/`--aur` as "restrict this to the AUR", so
    /// they get it and leave the repo cache to `paccache`. pikaur documents
    /// `--aur` as a *query* filter only, and its `-Sc` prompts for each cache
    /// it would clear anyway, so it is suggested bare rather than with a flag
    /// whose meaning here is unverified.
    pub fn clean_command(self) -> Vec<String> {
        let mut cmd = vec![self.bin().to_string(), "-Sc".to_string()];
        if self != AurHelper::Pikaur {
            cmd.push("--aur".to_string());
        }
        cmd
    }
}

/// What [`choose`] decided, and why — the "why" exists so the caller can say
/// so rather than silently doing something other than what the config asked.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
    #[default]
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

    /// The one-line explanation the dashboard and `paclens status` both print,
    /// or `None` when there is nothing to explain.
    ///
    /// Shared rather than written twice so the two surfaces cannot drift (P5).
    /// It names the capability that is missing and what restores it — design
    /// §3 rules out a bare "optional dependency missing", because a source
    /// reading "not found" tells you nothing about what to do next.
    ///
    /// Deliberately plain ASCII: `--no-color` also switches to ASCII glyphs,
    /// and a note is no place for a character that renders as a box.
    pub fn note(&self) -> Option<String> {
        const INSTALL: &str = "install paru, yay or pikaur for update detection";
        match self {
            HelperChoice::Detected(_) | HelperChoice::Pinned(_) => Option::None,
            HelperChoice::FellBack { configured, to } => Some(format!(
                "aur: config asks for {configured}, which is not installed; using {}",
                to.bin()
            )),
            HelperChoice::ConfiguredMissing { configured } => Some(format!(
                "aur: config asks for {configured}, which is not installed; {INSTALL}"
            )),
            HelperChoice::None => Some(format!("aur: {INSTALL}")),
        }
    }

    /// [`note`](Self::note) squeezed onto one line for the dashboard's system
    /// pane, which has exactly one row to spare.
    ///
    /// A separate string rather than a wrapped one: wrapping pushes the last
    /// row out of a fixed-height pane, and a note clipped at "aur: config asks
    /// for yay, which is not" has lost the half that matters. Kept short
    /// enough that the pane truncates only on genuinely narrow terminals,
    /// where every other row truncates too.
    ///
    /// The long form stays in `paclens status`, which has the width for it.
    pub fn compact_note(&self) -> Option<String> {
        match self {
            HelperChoice::Detected(_) | HelperChoice::Pinned(_) => Option::None,
            HelperChoice::FellBack { configured, to } => {
                Some(format!("aur: no {configured}, using {}", to.bin()))
            }
            HelperChoice::ConfiguredMissing { configured } => {
                Some(format!("aur: no {configured}, no helper at all"))
            }
            HelperChoice::None => Some("aur: no helper - install paru/yay/pikaur".to_string()),
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
    const QUA_YAY: &str = include_str!("../../tests/fixtures/aur/qua_yay.txt");
    const QUA_PIKAUR: &str = include_str!("../../tests/fixtures/aur/qua_pikaur.txt");

    /// yay appends a `[20h47m]` age column that paru does not emit. The parser
    /// reads four fields and stops, so the extra one is ignored rather than
    /// mistaken for a version — captured from real `yay -Qua` output.
    #[test]
    fn yay_qua_parses_despite_the_trailing_age_column() {
        assert!(
            QUA_YAY.contains('['),
            "fixture must still carry the age column"
        );
        let runner = MockRunner::new().with("yay -Qua", QUA_YAY, 0);
        let ups = scan_updates(&runner, AurHelper::Yay, false).unwrap();
        assert_eq!(ups.len(), 2);
        assert_eq!(ups[0].package_name, "t3code-bin");
        assert_eq!(ups[0].current_version, "0.0.33-1");
        assert_eq!(ups[0].available_version, "0.0.34-1");
        assert_eq!(ups[0].source_id, SourceId::aur());
    }

    /// pikaur indents every row and pads to aligned columns. `split_whitespace`
    /// skips both, but that is worth pinning: a switch to `split(' ')` would
    /// silently produce empty package names — captured from real output.
    #[test]
    fn pikaur_qua_parses_despite_indentation_and_column_padding() {
        assert!(
            QUA_PIKAUR.starts_with(' '),
            "fixture must still carry the leading indent"
        );
        let runner = MockRunner::new().with("pikaur -Qua", QUA_PIKAUR, 0);
        let ups = scan_updates(&runner, AurHelper::Pikaur, false).unwrap();
        assert_eq!(ups.len(), 2);
        assert_eq!(ups[0].package_name, "t3code-bin");
        assert_eq!(ups[0].available_version, "0.0.34-1");
        assert!(ups.iter().all(|u| !u.package_name.is_empty()));
    }

    /// paru and yay both exit 1 with no output when nothing is out of date;
    /// pikaur's behaviour there could not be produced on the capture machine,
    /// so both endings are accepted. Either way the answer is "no updates",
    /// never an error.
    #[test]
    fn no_updates_is_empty_for_every_helper_on_either_ending() {
        for helper in AurHelper::ALL {
            for code in [0, 1] {
                let runner = MockRunner::new().with(&format!("{} -Qua", helper.bin()), "", code);
                assert!(
                    scan_updates(&runner, helper, false)
                        .unwrap_or_else(|e| panic!("{} exit {code}: {e}", helper.bin()))
                        .is_empty(),
                    "{} exit {code} should mean no updates",
                    helper.bin()
                );
            }
        }
    }

    /// A working helper has nothing to explain; every degraded state does, and
    /// the sentence has to name what is missing and what restores it rather
    /// than leaving a bare "not found" (design §3).
    #[test]
    fn only_degraded_choices_carry_a_note() {
        assert_eq!(HelperChoice::Detected(AurHelper::Paru).note(), None);
        assert_eq!(HelperChoice::Pinned(AurHelper::Yay).note(), None);

        let none = HelperChoice::None.note().expect("no helper needs a note");
        assert!(none.contains("paru"), "{none}");
        assert!(none.contains("yay"), "{none}");
        assert!(none.contains("pikaur"), "{none}");
        assert!(none.contains("update detection"), "{none}");

        let fell_back = HelperChoice::FellBack {
            configured: "yay".to_string(),
            to: AurHelper::Paru,
        }
        .note()
        .expect("a stale pin needs a note");
        assert!(fell_back.contains("yay"), "must name what was configured");
        assert!(fell_back.contains("paru"), "must name what is being used");

        let missing = HelperChoice::ConfiguredMissing {
            configured: "trizen".to_string(),
        }
        .note()
        .expect("an unusable pin needs a note");
        assert!(missing.contains("trizen"), "{missing}");
        assert!(missing.contains("update detection"), "{missing}");
    }

    /// `--no-color` also switches to ASCII glyphs, so a note carrying an em
    /// dash or any other non-ASCII character would render as a box there.
    #[test]
    fn notes_are_plain_ascii() {
        let choices = [
            HelperChoice::None,
            HelperChoice::FellBack {
                configured: "yay".to_string(),
                to: AurHelper::Paru,
            },
            HelperChoice::ConfiguredMissing {
                configured: "trizen".to_string(),
            },
        ];
        for c in choices {
            let note = c.note().expect("degraded");
            assert!(note.is_ascii(), "note must be ASCII: {note:?}");
        }
    }

    /// The cache round-trips the whole choice, not just the resolved helper —
    /// dropping the configured name is what would make the stale-pin note
    /// impossible to write on a cached scan.
    #[test]
    fn a_stale_pin_survives_the_cache_round_trip() {
        let original = HelperChoice::FellBack {
            configured: "yay".to_string(),
            to: AurHelper::Paru,
        };
        let toml = toml::to_string(&Wrapper {
            choice: original.clone(),
        })
        .expect("serialize");
        let back: Wrapper = toml::from_str(&toml).expect("deserialize");
        assert_eq!(back.choice, original);
        assert_eq!(back.choice.helper(), Some(AurHelper::Paru));
    }

    #[derive(Serialize, Deserialize)]
    struct Wrapper {
        choice: HelperChoice,
    }

    /// The dashboard's system pane has exactly one row to spare and does not
    /// wrap, so a compact note that outgrows the pane gets clipped — losing
    /// the half that says what to do. 44 is the usable width of that pane on a
    /// 100-column terminal; anything longer needs the pane rethought, not the
    /// sentence quietly truncated.
    #[test]
    fn compact_notes_fit_one_row_of_the_system_pane() {
        const MAX: usize = 44;
        let choices = [
            HelperChoice::None,
            HelperChoice::FellBack {
                configured: "pikaur".to_string(),
                to: AurHelper::Paru,
            },
            HelperChoice::ConfiguredMissing {
                configured: "pikaur".to_string(),
            },
        ];
        for c in choices {
            let note = c.compact_note().expect("degraded");
            assert!(
                note.len() <= MAX,
                "compact note is {} chars, over {MAX}: {note:?}",
                note.len()
            );
            assert!(note.is_ascii(), "must be ASCII: {note:?}");
        }
        assert_eq!(HelperChoice::Detected(AurHelper::Paru).compact_note(), None);
        assert_eq!(HelperChoice::Pinned(AurHelper::Yay).compact_note(), None);
    }

    /// The compact form is shorter, not different: whatever the long note
    /// names, the short one names too. Two strings that drift would have the
    /// dashboard and `paclens status` disagreeing about the same machine.
    #[test]
    fn the_compact_note_names_whatever_the_long_one_does() {
        let fell_back = HelperChoice::FellBack {
            configured: "yay".to_string(),
            to: AurHelper::Paru,
        };
        let short = fell_back.compact_note().expect("degraded");
        assert!(short.contains("yay"), "must name the stale pin: {short}");
        assert!(short.contains("paru"), "must name what is used: {short}");

        let missing = HelperChoice::ConfiguredMissing {
            configured: "trizen".to_string(),
        };
        let short = missing.compact_note().expect("degraded");
        assert!(short.contains("trizen"), "{short}");

        // Both forms exist for exactly the same set of states.
        for c in [
            HelperChoice::None,
            HelperChoice::Detected(AurHelper::Paru),
            HelperChoice::Pinned(AurHelper::Yay),
            HelperChoice::FellBack {
                configured: "yay".to_string(),
                to: AurHelper::Paru,
            },
            HelperChoice::ConfiguredMissing {
                configured: "trizen".to_string(),
            },
        ] {
            assert_eq!(
                c.note().is_some(),
                c.compact_note().is_some(),
                "long and short disagree about whether {c:?} needs a note"
            );
        }
    }

    #[test]
    fn clean_command_restricts_to_the_aur_where_the_helper_supports_it() {
        assert_eq!(
            AurHelper::Paru.clean_command(),
            vec!["paru", "-Sc", "--aur"]
        );
        assert_eq!(AurHelper::Yay.clean_command(), vec!["yay", "-Sc", "--aur"]);
        // pikaur reads --aur as a query filter only, so it is left off.
        assert_eq!(AurHelper::Pikaur.clean_command(), vec!["pikaur", "-Sc"]);
    }

    /// Whatever the helper, the suggestion must never carry --noconfirm: the
    /// cleanup screen hands the user text to review, not a command to trust.
    #[test]
    fn no_clean_command_suppresses_confirmation() {
        for helper in AurHelper::ALL {
            assert!(
                !helper
                    .clean_command()
                    .iter()
                    .any(|a| a.contains("noconfirm")),
                "{} clean command must still prompt",
                helper.bin()
            );
        }
    }

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
