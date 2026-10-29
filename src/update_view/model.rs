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
use super::screen_line::{ScreenEvent, bar_pct, classify_screen, plan_names};

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
    /// pacman's own measured percent, when it draws one (download: bytes,
    /// not files).
    pub pct: Option<u8>,
    /// pacman's own words on the bar, e.g. bytes, rate and time left.
    pub detail: Option<String>,
}

/// One file pacman is downloading right now, as its live row shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InFlight {
    pub name: String,
    pub pct: u8,
    /// pacman's `1.8 MiB  460 KiB/s 00:33`: bytes so far, rate, time left.
    pub detail: String,
    /// Listed but not started: no numbers of its own yet.
    pub queued: bool,
    /// Started, but pacman shows no time left now (`--:--`): the numbers
    /// are the last ones it drew.
    pub stalled: bool,
    /// When it stalled.
    pub stalled_since: Option<Duration>,
}

impl InFlight {
    /// `detail` split into bytes so far, rate, time left.
    pub fn parts(&self) -> (String, String, String) {
        let t: Vec<&str> = self.detail.split_whitespace().collect();
        let two = |i: usize| t.get(i..i + 2).map(|p| p.join(" ")).unwrap_or_default();
        (
            two(0),
            two(2),
            t.get(4).copied().unwrap_or_default().to_string(),
        )
    }
}

impl Phase {
    /// How far along, in percent: pacman's own measure when it draws one
    /// (download bytes), else the done count over the total.
    pub fn share(&self) -> Option<u8> {
        let total = self.total.filter(|&t| t > 0)?;
        Some(
            self.pct
                .unwrap_or_else(|| u8::try_from(self.done.min(total) * 100 / total).unwrap_or(100)),
        )
    }
}

#[derive(Clone, Debug, Default)]
pub struct Pkg {
    pub name: String,
    pub repo: Option<String>,
    /// Download size from pacman's verbose plan (`1.99 MiB`).
    pub download: Option<String>,
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
    /// `lines` with pacman's colours, same index.
    pub styled: Vec<String>,
    /// Files downloading now (download phase only), in screen order.
    pub in_flight: Vec<InFlight>,
    /// Every download Total row seen, to spot its numbers left on a file row.
    pub totals_seen: std::collections::HashSet<(u8, String)>,
    /// Every file that has shown progress of its own, in the order it first
    /// did, with its last real row: a started file never goes back to queued.
    pub started_files: Vec<InFlight>,
    /// The package pacman is working on now (verb, name, percent), read
    /// from its live row: settled lines only say which ones are done.
    pub live_package: Option<(String, String, u8)>,
    /// Unrecognised lines, verbatim, in order — unrecognised hook output too.
    pub unknown: Vec<String>,
    /// Where each `unknown` line sits in `lines`.
    pub unknown_at: Vec<usize>,
    /// Where each warning line sits in `lines`: shown verbatim, like an
    /// unknown line, but not counted as one.
    pub warning_at: Vec<usize>,
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
                    pct: None,
                    detail: None,
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
            styled: Vec::new(),
            in_flight: Vec::new(),
            totals_seen: Default::default(),
            started_files: Vec::new(),
            live_package: None,
            unknown: Vec::new(),
            unknown_at: Vec::new(),
            warning_at: Vec::new(),
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
        self.apply_line_styled(line, line, at);
    }

    /// [`Run::apply_line`], with the line as pacman coloured it.
    pub fn apply_line_styled(&mut self, line: &str, styled: &str, at: Duration) {
        self.styled.push(styled.to_string());
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
            S::Warning(w) => {
                self.note(NoteKind::Warning, w);
                self.warning_at.push(self.lines.len().saturating_sub(1));
            }
            S::Error(e) => self.note(NoteKind::Error { recovered: false }, e),
            S::Pacnew(p) => self.note(NoteKind::Pacnew, p),
            S::PlanHeader { count } => {
                self.enter(PhaseKind::Plan, at);
                self.phase_mut(PhaseKind::Plan).total = Some(count);
            }
            S::PlanRow(row) => {
                let p = self.pkg(&row.name);
                p.repo = Some(row.repo);
                p.download = row.download;
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
            S::DownloadTotal {
                done,
                total,
                pct,
                detail,
            } => {
                self.enter(PhaseKind::Download, at);
                let ph = self.phase_mut(PhaseKind::Download);
                ph.done = done;
                ph.total = Some(total);
                ph.pct = Some(pct);
                ph.detail = Some(detail);
            }
            S::Check { name, .. } => {
                // Counted in steps (keys, integrity, files, conflicts, disk
                // space): each one walks every package again.
                self.enter(PhaseKind::Checks, at);
                let ph = self.phase_mut(PhaseKind::Checks);
                if ph.detail.as_ref() != Some(&name) {
                    ph.done += 1;
                }
                ph.detail = Some(name);
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

    /// The live screen rows (not settled yet): pacman redraws its download
    /// `Total` row in place and settles it only when the download ends, so
    /// it is read from here while the download runs.
    pub fn observe_screen(&mut self, rows: &[String], at: Duration) {
        // The package pacman is on now: its row is drawn, not yet settled.
        self.live_package = None;
        if self.current() == Some(PhaseKind::Packages) {
            self.live_package = rows
                .iter()
                .rev()
                .find_map(|r| match classify_screen(r, false) {
                    ScreenEvent::Package {
                        verb, name, pct, ..
                    } if pct < 100 => Some((verb, name, pct)),
                    _ => None,
                });
        }
        if self.current() != Some(PhaseKind::Download) {
            self.in_flight.clear();
            return;
        }
        let total = rows
            .iter()
            .rev()
            .find_map(|r| match classify_screen(r, false) {
                ev @ ScreenEvent::DownloadTotal { .. } => Some(ev),
                _ => None,
            });
        // pacman writes a new file's name over the row a Total line used to
        // be on, and that Total's numbers stay there until the file's first
        // update: a file row that repeats any Total seen so far is queued.
        if let Some(ScreenEvent::DownloadTotal { pct, detail, .. }) = &total {
            self.totals_seen.insert((*pct, detail.clone()));
        }
        let rows: Vec<InFlight> = rows.iter().filter_map(|r| self.in_flight_row(r)).collect();
        let mut flight: Vec<InFlight> = Vec::new();
        // Finished files leave, except the last one: it stays at 100%
        // until pacman closes the download, so the tree never stands empty.
        let open = rows.iter().any(|f| f.pct < 100);
        for mut f in rows.into_iter().filter(|f| f.pct < 100 || !open) {
            if self.totals_seen.contains(&(f.pct, f.detail.clone())) {
                f.queued = true;
            }
            let known = self.started_files.iter_mut().find(|k| k.name == f.name);
            match (f.queued, known) {
                // Real numbers: remember them.
                (false, Some(k)) => *k = f.clone(),
                (false, None) => self.started_files.push(f.clone()),
                // A started file that looks queued has stalled: keep what
                // it last showed, and say so.
                (true, Some(k)) => {
                    k.stalled_since.get_or_insert(at);
                    f = k.clone();
                    f.stalled = true;
                }
                (true, None) => {}
            }
            flight.push(f);
        }
        // Started files in the order they started, then the queue: rows
        // never swap places while pacman reshuffles its own.
        let order = |f: &InFlight| {
            self.started_files
                .iter()
                .position(|k| k.name == f.name)
                .unwrap_or(usize::MAX)
        };
        flight.sort_by_key(|f| order(f));
        self.in_flight = flight;
        if let Some(ev) = total {
            self.apply_screen(ev, at);
        }
    }

    pub fn apply_log(&mut self, ev: LogEvent, at: Duration) {
        // Evidence that the log speaks for this run: its `Running` line or a
        // transaction (pacman's db lock keeps those ours). A stray
        // `synchronizing` line alone must not read as "nothing to do".
        if matches!(
            ev,
            LogEvent::RunStart(_) | LogEvent::TxStarted | LogEvent::TxCompleted
        ) {
            self.log_seen = true;
        }
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
                let (list, count) = if self.tx_completed {
                    (&mut self.post_hooks, &mut self.log_post)
                } else if !self.tx_started {
                    (&mut self.pre_hooks, &mut self.log_pre)
                } else {
                    return;
                };
                if list.iter().any(|h| h.file.as_ref() == Some(&file)) {
                    return;
                }
                *count += 1;
                let h = hook_at(list, *count - 1);
                h.file = Some(file);
                h.median = median;
                reclassify(h, &mut self.unknown, &mut self.unknown_at);
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
        // A hook the screen started after the log ended its phase is open still.
        for h in self.pre_hooks.iter_mut().chain(self.post_hooks.iter_mut()) {
            if h.started.is_some() {
                h.ended.get_or_insert(at);
            }
        }
        for kind in [PhaseKind::PreHooks, PhaseKind::PostHooks] {
            let list = if kind == PhaseKind::PreHooks {
                &self.pre_hooks
            } else {
                &self.post_hooks
            };
            let started = list.iter().filter(|h| h.started.is_some()).count();
            let ph = self.phase_mut(kind);
            ph.done = ph.done.max(started);
        }
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

    /// How far along the running phase is, in percent. Hook phases go by
    /// time, not count: one initramfs rebuild can take most of the phase,
    /// and a count would sit still through it, then jump. Finished hooks
    /// count what they took, the running one its elapsed time against its
    /// median, hooks still to come the typical median. No history → count.
    pub fn progress(&self, now: Duration) -> Option<u8> {
        let k = self.current()?;
        let ph = self.phases.iter().find(|p| p.kind == k)?;
        let hooks = match k {
            PhaseKind::PreHooks => &self.pre_hooks,
            PhaseKind::PostHooks => &self.post_hooks,
            _ => return ph.share(),
        };
        let mut known: Vec<u64> = self.medians.values().map(|m| m.secs).collect();
        if known.is_empty() {
            return ph.share();
        }
        known.sort_unstable();
        let typical =
            Duration::from_secs(known[(known.len() - 1) / 2]).max(Duration::from_millis(500));
        let total = ph.total?;
        let started: Vec<&Hook> = hooks.iter().filter(|h| h.started.is_some()).collect();
        let (mut done, mut all) = (Duration::ZERO, Duration::ZERO);
        for h in &started {
            let took = h
                .ended
                .unwrap_or(now)
                .saturating_sub(h.started.unwrap_or(now));
            let expect = h.median.map_or(typical, |m| Duration::from_secs(m.secs));
            if h.ended.is_some() {
                done += took;
                all += took;
            } else {
                // A hook past its median is not done: keep a little room.
                done += took;
                all += expect.max(took + took / 10);
            }
        }
        all += typical * u32::try_from(total.saturating_sub(started.len())).unwrap_or(0);
        if all.is_zero() {
            return ph.share();
        }
        Some(
            u8::try_from(done.as_millis() * 100 / all.as_millis())
                .unwrap_or(100)
                .min(100),
        )
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
                        | NoteKind::Warning
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
            // pacman moved on, so every hook it counted ran — even one
            // whose line never settled on screen.
            ph.done = ph
                .done
                .max(hooks.iter().filter(|h| h.started.is_some()).count())
                .max(ph.total.unwrap_or(0));
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
        let list = match kind {
            PhaseKind::PreHooks => &mut self.pre_hooks,
            PhaseKind::PostHooks => &mut self.post_hooks,
            _ => return,
        };
        for h in list.iter_mut().take(i).filter(|h| h.started.is_some()) {
            h.ended.get_or_insert(at);
        }
        let h = hook_at(list, i);
        h.desc = Some(desc);
        h.started.get_or_insert(at);
        reclassify(h, &mut self.unknown, &mut self.unknown_at);
        self.screen_hook = Some(i);
        let ph = self.phase_mut(kind);
        // `(i/n)` starts hook i: the ones before it are done, it is not.
        ph.done = ph.done.max(index.saturating_sub(1));
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
        self.unknown_at.push(self.lines.len().saturating_sub(1));
        self.unknown.push(line);
    }

    /// A per-file download row for a planned package: its file stem
    /// `name-ver-rel-arch` alone, or the stem followed by a bar ending `NN%`.
    fn download_row(&self, t: &str) -> bool {
        let mut words = t.split_whitespace();
        let (Some(stem), more) = (words.next(), words.next().is_some()) else {
            return false;
        };
        self.current() == Some(PhaseKind::Download)
            && self.planned(stem).is_some()
            && (!more || bar_pct(t).is_some())
    }

    /// The planned package a download file stem `name-ver-rel-arch` is for.
    fn planned(&self, stem: &str) -> Option<String> {
        let names = stem
            .rsplit_once('-')
            .and_then(|(nvr, _arch)| plan_names(nvr))?;
        let all = names.iter().all(|(name, ver)| {
            self.packages
                .iter()
                .any(|p| &p.name == name && p.new.as_ref() == Some(ver))
        });
        all.then(|| names.into_iter().next().map(|(n, _)| n))
            .flatten()
    }

    /// A live per-file download row: ` stem  1.8 MiB  460 KiB/s 00:33 [bar]  9%`.
    fn in_flight_row(&self, row: &str) -> Option<InFlight> {
        let t = row.trim();
        let stem = t.split_whitespace().next()?;
        let pct = bar_pct(t)?;
        let name = self.planned(stem)?;
        let detail = t[stem.len()..].split('[').next().unwrap_or_default();
        let detail = detail.split_whitespace().collect::<Vec<_>>().join(" ");
        // `0.0 B 0.00 B/s --:--`: listed, not started.
        let queued = detail.ends_with("--:--");
        Some(InFlight {
            name,
            pct,
            detail,
            queued,
            stalled: false,
            stalled_since: None,
        })
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

/// The hook just learned its file or description: read its notes again, and
/// a note no longer `Plain` leaves `unknown` (which holds the raw line).
fn reclassify(h: &mut Hook, unknown: &mut Vec<String>, unknown_at: &mut Vec<usize>) {
    let kinds: Vec<_> = h.notes.iter().map(|n| hook_note_kind(&n.text, h)).collect();
    for (n, kind) in h.notes.iter_mut().zip(kinds) {
        if n.kind == NoteKind::Plain
            && kind != NoteKind::Plain
            && let Some(i) = unknown.iter().rposition(|l| l.trim() == n.text)
        {
            unknown.remove(i);
            unknown_at.remove(i);
        }
        n.kind = kind;
    }
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
    fn live_total_row_drives_download_progress() {
        let mut r = run();
        r.apply_line(":: Retrieving packages...", Z);
        let rows = [
            " zed-1.25.3-1-x86_64   40.1 MiB  5.0 MiB/s 00:08 [---C o o]  50%".to_string(),
            " Total ( 3/79)   363.0 MiB  5.12 MiB/s 00:02:51 [---C o o] 29%".to_string(),
        ];
        r.packages.push(Pkg {
            name: "zed".into(),
            new: Some("1.25.3-1".into()),
            ..Pkg::default()
        });
        r.observe_screen(&rows, Z);
        assert_eq!(
            r.in_flight,
            [InFlight {
                name: "zed".into(),
                pct: 50,
                detail: "40.1 MiB 5.0 MiB/s 00:08".into(),
                queued: false,
                stalled: false,
                stalled_since: None,
            }]
        );
        let d = &r.phases[idx(PhaseKind::Download)];
        assert_eq!((d.done, d.total, d.pct), (3, Some(79), Some(29)));
        assert_eq!(d.detail.as_deref(), Some("363.0 MiB 5.12 MiB/s 00:02:51"));
        r.apply_line(":: Processing package changes...", Z);
        r.observe_screen(&rows, Z);
        assert!(r.in_flight.is_empty());
        assert_eq!(r.phases[idx(PhaseKind::Download)].pct, Some(29));
    }

    #[test]
    fn checks_count_their_steps_not_packages() {
        let mut r = run();
        for l in [
            "(38/38) checking keys in keyring     [----] 100%",
            "(38/38) checking package integrity   [----] 100%",
            "(38/38) checking package integrity   [----] 100%",
            "(38/38) loading package files        [----] 100%",
        ] {
            r.apply_line(l, Z);
        }
        assert_eq!(r.phases[idx(PhaseKind::Checks)].done, 3);
    }

    #[test]
    fn live_package_row_is_the_current_one() {
        let mut r = run();
        r.apply_line(":: Processing package changes...", Z);
        r.apply_line("( 1/38) upgrading alpha     [----] 100%", Z);
        let rows = [
            "( 1/38) upgrading alpha     [----] 100%".to_string(),
            "( 2/38) upgrading beta      [--C ]  50%".to_string(),
        ];
        r.observe_screen(&rows, Z);
        assert_eq!(
            r.live_package,
            Some(("upgrading".to_string(), "beta".to_string(), 50))
        );
    }

    #[test]
    fn hooks_phase_counts_every_hook_once_pacman_moves_on() {
        let mut r = run();
        r.apply_line(":: Running pre-transaction hooks...", Z);
        r.apply_line("(1/4) One...", Z);
        r.apply_line("(2/4) Two...", Z);
        r.apply_line("(4/4) Four...", Z);
        r.apply_line(":: Processing package changes...", Z);
        let ph = &r.phases[idx(PhaseKind::PreHooks)];
        assert_eq!((ph.done, ph.total), (4, Some(4)));
    }

    #[test]
    fn started_file_that_stalls_keeps_its_numbers_and_its_place() {
        let mut r = run();
        r.apply_line(":: Retrieving packages...", Z);
        for n in ["zed", "deno"] {
            r.packages.push(Pkg {
                name: n.into(),
                new: Some("1-1".into()),
                ..Pkg::default()
            });
        }
        let first = [
            " zed-1-1-x86_64   40.1 MiB  5.0 MiB/s 00:08 [---C o o]  50%".to_string(),
            " deno-1-1-x86_64   2.0 MiB  1.0 MiB/s 00:30 [C o o o]   5%".to_string(),
        ];
        r.observe_screen(&first, Z);
        // pacman swaps the rows, and zed's rate drops to nothing.
        let later = [
            " deno-1-1-x86_64   4.0 MiB  1.0 MiB/s 00:28 [-C o o]  10%".to_string(),
            " zed-1-1-x86_64   40.1 MiB  0.00 B/s --:-- [---C o o]  50%".to_string(),
        ];
        r.observe_screen(&later, Z);
        let names: Vec<&str> = r.in_flight.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["zed", "deno"]);
        let zed = &r.in_flight[0];
        assert!(zed.stalled && !zed.queued, "{zed:?}");
        assert_eq!(zed.detail, "40.1 MiB 5.0 MiB/s 00:08");
    }

    #[test]
    fn file_row_with_the_totals_numbers_is_queued_not_in_flight() {
        let mut r = run();
        r.apply_line(":: Retrieving packages...", Z);
        r.packages.push(Pkg {
            name: "libuv".into(),
            new: Some("1.51.0-1".into()),
            ..Pkg::default()
        });
        let rows = [
            " libuv-1.51.0-1-x86_64   1022.0 MiB  9.10 MiB/s 00:12 [------C o] 83%".to_string(),
            " Total ( 6/79)   1022.0 MiB  9.10 MiB/s 00:12 [------C o] 83%".to_string(),
        ];
        r.observe_screen(&rows, Z);
        assert!(r.in_flight.iter().all(|f| f.queued), "{:?}", r.in_flight);
        // An older Total's numbers, left behind after the Total moved on.
        let later = [
            rows[0].clone(),
            " Total ( 7/79)   1030.0 MiB  9.20 MiB/s 00:11 [------C o] 84%".to_string(),
        ];
        r.observe_screen(&later, Z);
        assert!(r.in_flight.iter().all(|f| f.queued), "{:?}", r.in_flight);
    }

    #[test]
    fn long_hook_moves_the_bar_by_time() {
        let mut medians = HashMap::new();
        medians.insert("90-mkinitcpio.hook".into(), Median { secs: 20, runs: 50 });
        medians.insert("x.hook".into(), Median { secs: 1, runs: 50 });
        let mut r = Run::new("pacman -Syu", medians, HashMap::new());
        log(
            &mut r,
            "[2026-10-16T11:37:21+0530] [ALPM] transaction completed",
        );
        log(
            &mut r,
            "[2026-10-16T11:37:21+0530] [ALPM] running '90-mkinitcpio.hook'...",
        );
        r.apply_line(":: Running post-transaction hooks...", secs(0));
        r.apply_line("(1/3) Updating linux initcpios...", secs(0));
        let early = r.progress(secs(2)).unwrap();
        let late = r.progress(secs(16)).unwrap();
        assert!(early < 15 && late > 60, "{early} {late}");
        // Count alone would sit at 0% for all of it.
        assert_eq!(r.phases[idx(PhaseKind::PostHooks)].share(), Some(0));
    }

    #[test]
    fn running_hook_is_not_counted_done() {
        let mut r = run();
        log(
            &mut r,
            "[2026-10-16T11:37:21+0530] [ALPM] transaction completed",
        );
        r.apply_line(":: Running post-transaction hooks...", Z);
        r.apply_line("(1/2) Rebuilding initramfs", Z);
        let post = |r: &Run| r.phases[idx(PhaseKind::PostHooks)].done;
        assert_eq!(post(&r), 0);
        r.apply_line("(2/2) Updating icon caches", Z);
        assert_eq!(post(&r), 1);
        r.finish(Some(0), Z);
        assert_eq!(post(&r), 2);
    }

    #[test]
    fn stray_log_lines_are_not_a_clean_board() {
        let mut r = run();
        log(
            &mut r,
            "[2026-10-16T11:35:36+0530] [PACMAN] synchronizing package lists",
        );
        r.finish(Some(0), Z);
        assert_eq!(r.outcome(), Outcome::Unknown);
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
        r.apply_line("(1/1) Performing snapper pre snapshots...", secs(3));
        r.apply_line("==> root: 1001", secs(4));
        r.finish(Some(0), secs(8));
        assert!(
            r.pre_hooks
                .iter()
                .all(|h| h.started == Some(secs(3)) && h.ended == Some(secs(8)))
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

    fn foreign_check(screen_first: bool) -> Run {
        let mut r = run();
        log(
            &mut r,
            "[2026-10-16T11:37:21+0530] [ALPM] transaction completed",
        );
        let screen = [
            ":: Running post-transaction hooks...",
            "(1/1) Check foreign packages",
            "foreign foo",
        ];
        let hook = "[2026-10-16T11:37:25+0530] [ALPM] running 'rebuild-detector.hook'...";
        if screen_first {
            feed(&mut r, &screen);
            log(&mut r, hook);
        } else {
            log(&mut r, hook);
            feed(&mut r, &screen);
        }
        r
    }

    #[test]
    fn hook_file_arriving_late_reclassifies_its_notes() {
        for screen_first in [true, false] {
            let r = foreign_check(screen_first);
            assert_eq!(r.unknown.len(), r.unknown_at.len());
            assert_eq!(
                r.post_hooks[0].notes,
                [Note {
                    kind: NoteKind::Rebuild,
                    text: "foreign foo".into()
                }],
                "screen_first={screen_first}"
            );
            assert!(r.unknown.is_empty(), "screen_first={screen_first}");
            assert_eq!(r.needs_you().len(), 1);
        }
    }

    #[test]
    fn finish_closes_hooks_whose_phase_already_ended() {
        let mut r = run();
        r.apply_line(":: Running pre-transaction hooks...", secs(1));
        log(
            &mut r,
            "[2026-10-16T11:37:05+0530] [ALPM] transaction started",
        );
        r.apply_line("(1/1) a", secs(2));
        r.finish(Some(0), secs(7));
        assert_eq!(r.phases[idx(PhaseKind::PreHooks)].done, 1);
        assert_eq!(
            (r.pre_hooks[0].started, r.pre_hooks[0].ended),
            (Some(secs(2)), Some(secs(7)))
        );
    }

    #[test]
    fn download_prefix_without_progress_stays_unknown() {
        let mut r = run();
        feed(
            &mut r,
            &[
                "Packages (1) curl-8.23.0-1",
                ":: Retrieving packages...",
                " curl-8.23.0-1-x86_64",
                " curl-8.23.0-1-x86_64   1.2 MiB  1.0 MiB/s 00:01 [--C o] 40%",
                " curl-8.23.0-1-x86_64 unexpected server response",
                " curl-9.0-1-x86_64   1.2 MiB  1.0 MiB/s 00:01 [--C o] 40%",
            ],
        );
        assert_eq!(
            r.unknown,
            [
                " curl-8.23.0-1-x86_64 unexpected server response",
                " curl-9.0-1-x86_64   1.2 MiB  1.0 MiB/s 00:01 [--C o] 40%"
            ]
        );
    }

    #[test]
    fn duplicate_log_hook_is_not_a_second_hook() {
        let mut r = run();
        let at = "[2026-10-16T11:37:03+0530] [ALPM] running '";
        log(&mut r, &format!("{at}a.hook'..."));
        log(&mut r, &format!("{at}a.hook'..."));
        log(&mut r, &format!("{at}b.hook'..."));
        let files: Vec<_> = r.pre_hooks.iter().map(|h| h.file.as_deref()).collect();
        assert_eq!(files, [Some("a.hook"), Some("b.hook")]);
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
