//! One picture of a pacman run, folded from screen and pacman.log (#81).
//!
//! The classifiers read one line at a time; whatever spans lines (the hook
//! a line belongs to, an optdepends block, a wrapped package list) is kept
//! here. Re-seen items change nothing: packages are keyed by name, hooks by
//! index, databases by name.

use std::collections::HashMap;
use std::time::Duration;

use crate::analyzer::history::EventKind;

use super::log_line::LogEvent;
use super::medians::Median;
use super::screen_line::{ScreenEvent, classify_screen, plan_names};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PhaseKind {
    Sync,
    Plan,
    Download,
    Checks,
    PreHooks,
    Packages,
    PostHooks,
}

const PHASES: [PhaseKind; 7] = [
    PhaseKind::Sync,
    PhaseKind::Plan,
    PhaseKind::Download,
    PhaseKind::Checks,
    PhaseKind::PreHooks,
    PhaseKind::Packages,
    PhaseKind::PostHooks,
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PhaseState {
    Pending,
    Running,
    Done,
}

#[derive(Clone, Debug)]
pub struct Phase {
    pub kind: PhaseKind,
    pub state: PhaseState,
    pub done: usize,
    pub total: Option<usize>,
    pub started: Option<Duration>,
    pub ended: Option<Duration>,
}

#[derive(Clone, Debug, Default)]
pub struct Pkg {
    pub name: String,
    pub repo: Option<String>,
    pub old: Option<String>,
    pub new: Option<String>,
    pub verb: Option<String>,
    pub done: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Hook {
    pub desc: Option<String>,
    pub file: Option<String>,
    pub started: Option<Duration>,
    pub ended: Option<Duration>,
    pub median: Option<Median>,
    pub notes: Vec<Note>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum NoteKind {
    Reboot,
    Rebuild,
    Snapshot { config: String, number: u64 },
    Pacnew,
    Optdepends,
    Warning,
    Error { recovered: bool },
    Plain,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Note {
    pub kind: NoteKind,
    pub text: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    Running,
    Succeeded,
    NothingChanged,
    /// `None` = killed by a signal.
    Failed(Option<i32>),
    Unknown,
}

/// pacman's own chatter that carries nothing to show.
const QUIET: [&str; 3] = [
    "",
    "resolving dependencies...",
    "looking for conflicting packages...",
];

#[derive(Debug)]
pub struct Run {
    pub command: String,
    pub phases: Vec<Phase>,
    pub dbs: Vec<String>,
    pub packages: Vec<Pkg>,
    pub pre_hooks: Vec<Hook>,
    pub post_hooks: Vec<Hook>,
    /// Not tied to a hook: warnings, errors, pacnew, optdepends.
    pub notes: Vec<Note>,
    pub totals: Vec<(String, String)>,
    pub prompt: Option<String>,
    /// Every settled screen line, in order.
    pub lines: Vec<String>,
    /// Unrecognised lines, verbatim, in order — unrecognised hook output too.
    pub unknown: Vec<String>,
    /// name → version from the scan cache: the old version for `PlanNames`.
    pub installed: HashMap<String, String>,
    pub log_seen: bool,
    pub tx_started: bool,
    pub finished: bool,
    pub exit: Option<i32>,
    pub ended: Option<Duration>,
    medians: HashMap<String, Median>,
    /// Hooks the log has named so far, before / after the transaction.
    log_pre: usize,
    log_post: usize,
    tx_completed: bool,
    /// The hook the screen is in (index into the current phase's list).
    screen_hook: Option<usize>,
    /// The hook section the screen is in: `PreHooks`, `PostHooks` or none.
    screen_section: Option<PhaseKind>,
    /// Inside an `Optional dependencies for X` block.
    optdeps_for: Option<String>,
    /// Between a `Packages (N)` name list and the plan totals.
    in_names: bool,
    saw_package: bool,
}

impl Run {
    pub fn new(
        command: &str,
        medians: HashMap<String, Median>,
        installed: HashMap<String, String>,
    ) -> Self {
        Self {
            command: command.to_string(),
            phases: PHASES
                .iter()
                .map(|&kind| Phase {
                    kind,
                    state: PhaseState::Pending,
                    done: 0,
                    total: None,
                    started: None,
                    ended: None,
                })
                .collect(),
            dbs: Vec::new(),
            packages: Vec::new(),
            pre_hooks: Vec::new(),
            post_hooks: Vec::new(),
            notes: Vec::new(),
            totals: Vec::new(),
            prompt: None,
            lines: Vec::new(),
            unknown: Vec::new(),
            installed,
            log_seen: false,
            tx_started: false,
            finished: false,
            exit: None,
            ended: None,
            medians,
            log_pre: 0,
            log_post: 0,
            tx_completed: false,
            screen_hook: None,
            screen_section: None,
            optdeps_for: None,
            in_names: false,
            saw_package: false,
        }
    }

    /// One settled screen line: kept in `lines`, classified, applied.
    pub fn apply_line(&mut self, line: &str, at: Duration) {
        self.lines.push(line.to_string());
        let ev = classify_screen(line, self.current() == Some(PhaseKind::Sync));
        self.apply_screen(ev, at);
    }

    pub fn apply_screen(&mut self, ev: ScreenEvent, at: Duration) {
        use ScreenEvent as S;
        if !matches!(ev, S::Other(_)) {
            self.optdeps_for = None;
            self.in_names = matches!(ev, S::PlanNames(_));
        }
        match ev {
            S::SyncStart => self.enter(PhaseKind::Sync, at),
            S::Db(db) => {
                if !self.dbs.contains(&db) {
                    self.dbs.push(db);
                }
                self.phase_mut(PhaseKind::Sync).done = self.dbs.len();
            }
            S::UpgradeStart | S::NothingToDo => self.enter(PhaseKind::Plan, at),
            S::Warning(w) => self.note(NoteKind::Warning, w),
            S::Error(e) => self.note(NoteKind::Error { recovered: false }, e),
            S::Pacnew(p) => self.note(NoteKind::Pacnew, p),
            S::PlanHeader { count } => {
                self.enter(PhaseKind::Plan, at);
                self.phase_mut(PhaseKind::Plan).total = Some(count);
            }
            S::PlanRow(row) => {
                let p = self.pkg(&row.name);
                p.repo = Some(row.repo);
                p.old = row.old;
                p.new = Some(row.new);
                self.count_plan();
            }
            S::PlanNames(names) => self.add_names(names),
            S::PlanTotal { label, value } => match self.totals.iter_mut().find(|t| t.0 == label) {
                Some(t) => t.1 = value,
                None => self.totals.push((label, value)),
            },
            S::Prompt(p) => self.prompt = Some(p),
            S::RetrieveStart => {
                self.enter(PhaseKind::Download, at);
                let n = self.packages.len();
                self.phase_mut(PhaseKind::Download).total = (n > 0).then_some(n);
            }
            S::DownloadTotal { done, total, .. } => {
                self.enter(PhaseKind::Download, at);
                let ph = self.phase_mut(PhaseKind::Download);
                ph.done = done;
                ph.total = Some(total);
            }
            S::Check { done, total, .. } => {
                self.enter(PhaseKind::Checks, at);
                let ph = self.phase_mut(PhaseKind::Checks);
                ph.done = done;
                ph.total = Some(total);
            }
            S::PreHooks | S::PostHooks | S::Processing => {
                let kind = match ev {
                    S::PreHooks => PhaseKind::PreHooks,
                    S::PostHooks => PhaseKind::PostHooks,
                    _ => PhaseKind::Packages,
                };
                self.enter(kind, at);
                self.screen_section = (kind != PhaseKind::Packages).then_some(kind);
                self.screen_hook = None;
            }
            S::Hook { index, total, desc } => self.screen_hook_line(index, total, desc, at),
            S::Package {
                index,
                total,
                verb,
                name,
                pct,
            } => {
                self.enter(PhaseKind::Packages, at);
                self.saw_package = true;
                let p = self.pkg(&name);
                p.verb = Some(verb);
                p.done |= pct == 100;
                let ph = self.phase_mut(PhaseKind::Packages);
                ph.done = ph.done.max(index);
                ph.total = Some(total);
            }
            S::Optdepends(pkg) => self.optdeps_for = Some(pkg),
            S::Other(line) => self.other(line),
        }
    }

    pub fn apply_log(&mut self, ev: LogEvent, at: Duration) {
        self.log_seen = true;
        match ev {
            LogEvent::TxStarted => {
                self.tx_started = true;
                self.enter(PhaseKind::Packages, at);
            }
            LogEvent::TxCompleted => {
                self.tx_completed = true;
                self.enter(PhaseKind::PostHooks, at);
            }
            LogEvent::HookRunning(file) => {
                let median = self.medians.get(&file).copied();
                let (list, i) = if self.tx_completed {
                    self.log_post += 1;
                    (&mut self.post_hooks, self.log_post - 1)
                } else if !self.tx_started {
                    self.log_pre += 1;
                    (&mut self.pre_hooks, self.log_pre - 1)
                } else {
                    return;
                };
                let h = hook_at(list, i);
                h.file = Some(file);
                h.median = median;
            }
            LogEvent::Package(e) => {
                let p = self.pkg(&e.name);
                if p.old.is_none() {
                    p.old = e.from;
                }
                if p.new.is_none() {
                    p.new = e.to;
                }
                if p.verb.is_none() {
                    p.verb = Some(verb_of(e.kind).to_string());
                }
                p.done = true;
            }
            // Scriptlet output also reaches the screen, where it is placed
            // under its hook; the log copy would only duplicate it.
            LogEvent::RunStart(_) | LogEvent::Scriptlet(_) | LogEvent::TxFailed => {}
            LogEvent::Other => {}
        }
    }

    /// `exit == None`: killed by a signal.
    pub fn finish(&mut self, exit: Option<i32>, at: Duration) {
        self.finished = true;
        self.exit = exit;
        self.ended = Some(at);
        self.end_running(PHASES.len(), at);
        let recovered = exit == Some(0);
        let hook_notes = self
            .pre_hooks
            .iter_mut()
            .chain(self.post_hooks.iter_mut())
            .flat_map(|h| h.notes.iter_mut());
        for n in self.notes.iter_mut().chain(hook_notes) {
            if let NoteKind::Error { .. } = n.kind {
                n.kind = NoteKind::Error { recovered };
            }
        }
    }

    pub fn outcome(&self) -> Outcome {
        match (self.finished, self.exit) {
            (false, _) => Outcome::Running,
            (true, Some(0)) if self.log_seen && !self.tx_started => Outcome::NothingChanged,
            (true, Some(0)) if self.log_seen || self.saw_package => Outcome::Succeeded,
            (true, Some(0)) => Outcome::Unknown,
            (true, exit) => Outcome::Failed(exit),
        }
    }

    /// The phase that is running now.
    pub fn current(&self) -> Option<PhaseKind> {
        self.phases
            .iter()
            .rfind(|p| p.state == PhaseState::Running)
            .map(|p| p.kind)
    }

    /// What the user should act on or know after the run.
    pub fn needs_you(&self) -> Vec<&Note> {
        let hook_notes = self
            .pre_hooks
            .iter()
            .chain(&self.post_hooks)
            .flat_map(|h| &h.notes);
        self.notes
            .iter()
            .chain(hook_notes)
            .filter(|n| {
                matches!(
                    n.kind,
                    NoteKind::Reboot
                        | NoteKind::Rebuild
                        | NoteKind::Snapshot { .. }
                        | NoteKind::Pacnew
                        | NoteKind::Optdepends
                        | NoteKind::Error { recovered: true }
                )
            })
            .collect()
    }

    fn phase_mut(&mut self, kind: PhaseKind) -> &mut Phase {
        &mut self.phases[idx(kind)]
    }

    /// Start `kind`, ending whatever runs before it. A phase already
    /// reached — or passed — is left alone, so late or repeated events
    /// never move the run backwards.
    fn enter(&mut self, kind: PhaseKind, at: Duration) {
        let i = idx(kind);
        if self.phases[i..]
            .iter()
            .any(|p| p.state != PhaseState::Pending)
        {
            return;
        }
        self.end_running(i, at);
        let ph = &mut self.phases[i];
        ph.state = PhaseState::Running;
        ph.started = Some(at);
    }

    /// Phases before index `upto` that still run are done at `at`, and so is
    /// any hook still open in them.
    fn end_running(&mut self, upto: usize, at: Duration) {
        for ph in self.phases.iter_mut().take(upto) {
            if ph.state != PhaseState::Running {
                continue;
            }
            ph.state = PhaseState::Done;
            ph.ended = Some(at);
            let hooks = match ph.kind {
                PhaseKind::PreHooks => &mut self.pre_hooks,
                PhaseKind::PostHooks => &mut self.post_hooks,
                _ => continue,
            };
            for h in hooks.iter_mut().filter(|h| h.started.is_some()) {
                h.ended.get_or_insert(at);
            }
        }
    }

    /// The hook list the screen is in — by its own section marker, so a log
    /// that runs ahead cannot pull screen output into the wrong list.
    fn hooks_now(&mut self) -> Option<&mut Vec<Hook>> {
        match self.screen_section? {
            PhaseKind::PreHooks => Some(&mut self.pre_hooks),
            PhaseKind::PostHooks => Some(&mut self.post_hooks),
            _ => None,
        }
    }

    fn screen_hook_line(&mut self, index: usize, total: usize, desc: String, at: Duration) {
        let Some(i) = index.checked_sub(1) else {
            return;
        };
        let Some(kind) = self.screen_section else {
            return;
        };
        let Some(list) = self.hooks_now() else {
            return;
        };
        for h in list.iter_mut().take(i).filter(|h| h.started.is_some()) {
            h.ended.get_or_insert(at);
        }
        let h = hook_at(list, i);
        h.desc = Some(desc);
        h.started.get_or_insert(at);
        self.screen_hook = Some(i);
        let ph = self.phase_mut(kind);
        ph.done = ph.done.max(index);
        ph.total = Some(total);
    }

    fn other(&mut self, line: String) {
        let t = line.trim();
        if let Some(pkg) = &self.optdeps_for {
            if line.starts_with(char::is_whitespace) && !t.is_empty() {
                let text = format!("{pkg}: {t}");
                self.note(NoteKind::Optdepends, text);
                return;
            }
            self.optdeps_for = None;
        }
        if self.in_names {
            if let Some(names) = plan_names(t) {
                self.add_names(names);
                return;
            }
            self.in_names = false;
        }
        if QUIET.contains(&t) || line.starts_with("[sudo] password for ") || self.download_row(t) {
            return;
        }
        if let Some(i) = self.screen_hook
            && let Some(h) = self.hooks_now().and_then(|l| l.get_mut(i))
        {
            let kind = hook_note_kind(t, h);
            let plain = kind == NoteKind::Plain;
            h.notes.push(Note {
                kind,
                text: t.to_string(),
            });
            if !plain {
                return;
            }
        }
        self.unknown.push(line);
    }

    /// A per-file download row (` name-ver-rel-arch   … NN%`) for a planned package.
    fn download_row(&self, t: &str) -> bool {
        self.current() == Some(PhaseKind::Download)
            && t.split_whitespace()
                .next()
                .and_then(|w| w.rsplit_once('-'))
                .and_then(|(nvr, _arch)| plan_names(nvr))
                .is_some_and(|n| n.iter().all(|(name, _)| self.has_pkg(name)))
    }

    fn has_pkg(&self, name: &str) -> bool {
        self.packages.iter().any(|p| p.name == name)
    }

    fn add_names(&mut self, names: Vec<(String, String)>) {
        for (name, new) in names {
            let old = self.installed.get(&name).cloned();
            let p = self.pkg(&name);
            p.new = Some(new);
            if p.old.is_none() {
                p.old = old;
            }
        }
        self.count_plan();
    }

    fn count_plan(&mut self) {
        let n = self.packages.len();
        self.phase_mut(PhaseKind::Plan).done = n;
    }

    fn pkg(&mut self, name: &str) -> &mut Pkg {
        let i = match self.packages.iter().position(|p| p.name == name) {
            Some(i) => i,
            None => {
                self.packages.push(Pkg {
                    name: name.to_string(),
                    ..Pkg::default()
                });
                self.packages.len() - 1
            }
        };
        &mut self.packages[i]
    }

    fn note(&mut self, kind: NoteKind, text: String) {
        self.notes.push(Note { kind, text });
    }
}

fn idx(kind: PhaseKind) -> usize {
    PHASES.iter().position(|&k| k == kind).unwrap_or(0)
}

fn hook_at(list: &mut Vec<Hook>, i: usize) -> &mut Hook {
    if list.len() <= i {
        list.resize_with(i + 1, Hook::default);
    }
    &mut list[i]
}

/// What one line of hook output means.
fn hook_note_kind(t: &str, h: &Hook) -> NoteKind {
    if let Some((config, n)) = t.strip_prefix("==> ").and_then(|r| r.split_once(": "))
        && !config.contains(char::is_whitespace)
        && let Ok(number) = n.trim().parse()
    {
        return NoteKind::Snapshot {
            config: config.to_string(),
            number,
        };
    }
    if t.contains("Reboot") || t.contains("reboot") {
        return NoteKind::Reboot;
    }
    let rebuild = [&h.file, &h.desc]
        .iter()
        .any(|s| s.as_deref().is_some_and(|s| s.contains("rebuil")));
    if rebuild {
        NoteKind::Rebuild
    } else {
        NoteKind::Plain
    }
}

fn verb_of(kind: EventKind) -> &'static str {
    match kind {
        EventKind::Installed => "installing",
        EventKind::Upgraded => "upgrading",
        EventKind::Removed => "removing",
        EventKind::Reinstalled => "reinstalling",
        EventKind::Downgraded => "downgrading",
    }
}

/// The fixture run: every settled screen line, then the log, then exit 0.
#[cfg(test)]
pub(crate) fn fixture_run() -> Run {
    let mut r = Run::new("pacman -Syu", HashMap::new(), HashMap::new());
    for l in super::screen::fixture_lines() {
        r.apply_line(&l, Duration::ZERO);
    }
    let log = include_str!("../../tests/fixtures/pacman-run/2026-10-16.log");
    for (_, ev) in log.lines().filter_map(super::log_line::classify_log) {
        r.apply_log(ev, Duration::ZERO);
    }
    r.finish(Some(0), Duration::ZERO);
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update_view::log_line::classify_log;
    use crate::update_view::screen_line::classify_screen;

    const Z: Duration = Duration::ZERO;

    fn run() -> Run {
        Run::new("pacman -Syu", HashMap::new(), HashMap::new())
    }

    fn feed(r: &mut Run, lines: &[&str]) {
        for l in lines {
            r.apply_line(l, Z);
        }
    }

    fn log(r: &mut Run, line: &str) {
        let (_, ev) = classify_log(line).unwrap();
        r.apply_log(ev, Z);
    }

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn fixture_builds_the_right_model() {
        let r = fixture_run();
        assert_eq!(
            r.dbs,
            ["cachyos-v3", "cachyos", "core", "extra", "multilib"]
        );
        assert_eq!(r.packages.len(), 38);
        let lj = r.packages.iter().find(|p| p.name == "luajit").unwrap();
        assert_eq!(
            (lj.old.as_deref(), lj.new.as_deref()),
            (
                Some("2.1.1791890110+634daf2-1"),
                Some("2.1.1791988261+2a8e017-1")
            )
        );
        assert!(
            r.packages
                .iter()
                .all(|p| p.old.is_some() && p.new.is_some() && p.done)
        );
        assert_eq!(r.pre_hooks.len(), 3);
        assert_eq!(r.post_hooks.len(), 13);
        assert_eq!(
            r.post_hooks[6].desc.as_deref(),
            Some("Restart a running sshd.service")
        );
        assert_eq!(
            r.post_hooks[6].file.as_deref(),
            Some("70-openssh-restart-sshd.hook")
        );
        assert_eq!(r.pre_hooks[0].file.as_deref(), Some("05-snap-pac-pre.hook"));
        assert!(r.notes.iter().any(
            |n| n.kind == NoteKind::Error { recovered: true } && n.text.contains("luajit")
        ));
        let kinds: Vec<_> = r.needs_you().iter().map(|n| n.kind.clone()).collect();
        assert!(kinds.contains(&NoteKind::Reboot));
        assert!(kinds.contains(&NoteKind::Rebuild));
        assert!(kinds.contains(&NoteKind::Snapshot {
            config: "root".into(),
            number: 1001
        }));
        assert!(kinds.contains(&NoteKind::Snapshot {
            config: "root".into(),
            number: 1002
        }));
        assert_eq!(r.outcome(), Outcome::Succeeded);
        assert!(r.phases.iter().all(|p| p.state == PhaseState::Done));
    }

    #[test]
    fn fixture_has_no_unknown_lines() {
        // Download rows, blank lines and pacman's own chatter are all known.
        assert_eq!(fixture_run().unknown, Vec::<String>::new());
    }

    #[test]
    fn fixture_keeps_every_settled_line() {
        let lines = crate::update_view::screen::fixture_lines();
        assert_eq!(fixture_run().lines, lines);
    }

    #[test]
    fn error_line_with_nonzero_exit_is_not_recovered() {
        let mut r = run();
        feed(&mut r, &["error: failed to commit transaction"]);
        r.finish(Some(1), Z);
        assert_eq!(r.notes[0].kind, NoteKind::Error { recovered: false });
        assert_eq!(r.outcome(), Outcome::Failed(Some(1)));
        assert!(r.needs_you().is_empty());
    }

    #[test]
    fn signal_is_a_failure_without_a_code() {
        let mut r = run();
        r.finish(None, Z);
        assert_eq!(r.outcome(), Outcome::Failed(None));
    }

    #[test]
    fn running_until_finished() {
        assert_eq!(run().outcome(), Outcome::Running);
    }

    #[test]
    fn nonverbose_plan_takes_old_version_from_scan_cache() {
        let installed = HashMap::from([("curl".to_string(), "8.22.0-1".to_string())]);
        let mut r = Run::new("pacman -Syu", HashMap::new(), installed);
        feed(&mut r, &["Packages (2) curl-8.23.0-1  zstd-1.5.7-4"]);
        let curl = r.packages.iter().find(|p| p.name == "curl").unwrap();
        assert_eq!(curl.old.as_deref(), Some("8.22.0-1"));
        assert_eq!(curl.new.as_deref(), Some("8.23.0-1"));
        let zstd = r.packages.iter().find(|p| p.name == "zstd").unwrap();
        assert_eq!(zstd.old, None);
    }

    #[test]
    fn wrapped_package_names_continue_the_list() {
        let mut r = run();
        feed(
            &mut r,
            &[
                "Packages (3) curl-8.23.0-1  zstd-1.5.7-4",
                "             uv-0.13.1-1",
                "",
                "Total Download Size:    1.00 MiB",
                "             not-a-name",
            ],
        );
        let names: Vec<_> = r.packages.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["curl", "zstd", "uv"]);
        assert_eq!(r.unknown, ["             not-a-name"]);
    }

    #[test]
    fn optdepends_entries_are_notes_until_a_flush_line() {
        let mut r = run();
        feed(
            &mut r,
            &[
                ":: Processing package changes...",
                "Optional dependencies for firefox",
                "    speech-dispatcher: Text-to-Speech",
                "    hunspell-en_US: Spell checking",
                "stray line",
                "    not an optdepend",
            ],
        );
        let opt: Vec<_> = r
            .notes
            .iter()
            .filter(|n| n.kind == NoteKind::Optdepends)
            .map(|n| n.text.as_str())
            .collect();
        assert_eq!(
            opt,
            [
                "firefox: speech-dispatcher: Text-to-Speech",
                "firefox: hunspell-en_US: Spell checking"
            ]
        );
        assert_eq!(r.unknown, ["stray line", "    not an optdepend"]);
    }

    #[test]
    fn unrecognised_hook_output_is_counted() {
        let mut r = run();
        feed(
            &mut r,
            &[
                ":: Running post-transaction hooks...",
                "(1/1) Updating icon theme caches...",
                "something odd",
            ],
        );
        assert_eq!(r.post_hooks[0].notes[0].kind, NoteKind::Plain);
        assert_eq!(r.post_hooks[0].notes[0].text, "something odd");
        assert_eq!(r.unknown, ["something odd"]);
    }

    #[test]
    fn hook_notes_snapshot_reboot_rebuild() {
        let mut r = run();
        feed(
            &mut r,
            &[
                ":: Running post-transaction hooks...",
                "(1/3) Performing snapper post snapshots...",
                "==> root: 7",
                "(2/3) Check reboot",
                "==> INFO: Reboot is recommended",
                "(3/3) Checking which packages need to be rebuilt",
                "foreign   wps-office",
            ],
        );
        let k = |i: usize| r.post_hooks[i].notes[0].kind.clone();
        assert_eq!(
            k(0),
            NoteKind::Snapshot {
                config: "root".into(),
                number: 7
            }
        );
        assert_eq!(k(1), NoteKind::Reboot);
        assert_eq!(k(2), NoteKind::Rebuild);
        assert!(r.unknown.is_empty());
    }

    #[test]
    fn no_log_means_unknown_not_nothing_changed() {
        let mut r = run();
        feed(
            &mut r,
            &[
                ":: Starting full system upgrade...",
                " there is nothing to do",
            ],
        );
        r.finish(Some(0), Z);
        assert_eq!(r.outcome(), Outcome::Unknown);
    }

    #[test]
    fn no_log_but_packages_seen_is_success() {
        let mut r = run();
        feed(&mut r, &["(1/1) upgrading zstd  [##] 100%"]);
        r.finish(Some(0), Z);
        assert_eq!(r.outcome(), Outcome::Succeeded);
    }

    #[test]
    fn nothing_to_do_is_a_clean_board() {
        let mut r = run();
        log(
            &mut r,
            "[2026-10-16T11:35:36+0530] [PACMAN] Running 'pacman -Syu'",
        );
        feed(&mut r, &[" there is nothing to do"]);
        r.finish(Some(0), Z);
        assert_eq!(r.outcome(), Outcome::NothingChanged);
    }

    #[test]
    fn reapplying_a_line_changes_nothing() {
        let mut r = run();
        let ev = classify_screen("(1/1) upgrading zstd  [##] 100%", false);
        r.apply_screen(ev.clone(), Z);
        r.apply_screen(ev, Z);
        assert_eq!(r.packages.len(), 1);
        let db = classify_screen(" core", true);
        r.apply_screen(ScreenEvent::SyncStart, Z);
        r.apply_screen(db.clone(), Z);
        r.apply_screen(db, Z);
        assert_eq!(r.dbs, ["core"]);
    }

    #[test]
    fn hook_median_attached_by_file() {
        let medians = HashMap::from([("x.hook".to_string(), Median { secs: 23, runs: 51 })]);
        let mut r = Run::new("pacman -Syu", medians, HashMap::new());
        log(
            &mut r,
            "[2026-10-16T11:37:21+0530] [ALPM] transaction completed",
        );
        log(
            &mut r,
            "[2026-10-16T11:37:21+0530] [ALPM] running 'x.hook'...",
        );
        assert_eq!(r.post_hooks[0].median, Some(Median { secs: 23, runs: 51 }));
    }

    #[test]
    fn hook_ends_when_the_next_starts_or_at_finish() {
        let mut r = run();
        r.apply_line(":: Running post-transaction hooks...", secs(1));
        r.apply_line("(1/2) a", secs(2));
        r.apply_line("(2/2) b", secs(5));
        r.finish(Some(0), secs(9));
        let h = &r.post_hooks;
        assert_eq!((h[0].started, h[0].ended), (Some(secs(2)), Some(secs(5))));
        assert_eq!(h[1].ended, Some(secs(9)));
    }

    #[test]
    fn log_ahead_of_screen_keeps_hook_output_under_its_hook() {
        let mut r = run();
        feed(&mut r, &[":: Running pre-transaction hooks..."]);
        log(
            &mut r,
            "[2026-10-16T11:37:05+0530] [ALPM] transaction started",
        );
        log(
            &mut r,
            "[2026-10-16T11:37:21+0530] [ALPM] transaction completed",
        );
        feed(
            &mut r,
            &[
                "(1/1) Performing snapper pre snapshots...",
                "==> root: 1001",
            ],
        );
        assert_eq!(r.pre_hooks.len(), 1);
        assert_eq!(
            r.pre_hooks[0].notes[0].kind,
            NoteKind::Snapshot {
                config: "root".into(),
                number: 1001
            }
        );
        assert!(r.post_hooks.is_empty() && r.unknown.is_empty());
    }

    #[test]
    fn unrecognised_lines_are_kept_verbatim() {
        let mut r = run();
        feed(
            &mut r,
            &[":: Starting full system upgrade...", "  weird  output "],
        );
        assert_eq!(r.unknown, ["  weird  output "]);
    }

    #[test]
    fn phases_advance_and_current_follows() {
        let mut r = run();
        feed(&mut r, &[":: Synchronizing package databases..."]);
        assert_eq!(r.current(), Some(PhaseKind::Sync));
        feed(&mut r, &[":: Starting full system upgrade..."]);
        assert_eq!(r.current(), Some(PhaseKind::Plan));
        assert_eq!(r.phases[0].state, PhaseState::Done);
        log(
            &mut r,
            "[2026-10-16T11:37:05+0530] [ALPM] transaction started",
        );
        assert_eq!(r.current(), Some(PhaseKind::Packages));
        // A late line for an earlier phase does not move the run back.
        feed(&mut r, &[":: Running pre-transaction hooks..."]);
        assert_eq!(r.current(), Some(PhaseKind::Packages));
    }
}
