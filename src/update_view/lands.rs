//! The `lands` layout: the pacman view rebuilt from the 2026-11-18 design
//! study (paclens-research/inspire). One home per stat; the running work
//! gets the weight; finished phases fold to one line; success leaves the
//! result and the user's steps, nothing else.
//!
//! A child of `render`: it reuses the painter, bars and the parsed run, and
//! draws only what pacman's screen and log support.

use std::time::Duration;

use unicode_width::UnicodeWidthStr;

use super::super::model::InFlight;
use super::*;

/// Widest file name column, and the bar's least and most cells: a wide
/// screen gets longer names and calmer spacing, not a longer track.
const NAME_MAX: usize = 38;
const BAR_MIN: usize = 12;
const BAR_MAX: usize = 36;
/// Hook output lines shown under the running hook.
const TAIL: usize = 3;

/// `8 packages`, `1 package`.
fn n_of(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// What happened to the packages, from pacman's log verbs: `8 upgraded`,
/// or `5 upgraded · 2 installed`. `None` when no package event was seen.
pub(super) fn changed(run: &Run, s: &Styles) -> Option<String> {
    let mut verbs: Vec<(String, usize)> = Vec::new();
    for p in run.packages.iter().filter(|p| p.done) {
        // The screen says `upgrading`; the receipt says what was done.
        let v = match p.verb.as_deref() {
            Some(v) => v
                .strip_suffix("ing")
                .map_or(v.to_string(), |s| format!("{s}ed")),
            None => "changed".into(),
        };
        match verbs.iter_mut().find(|x| x.0 == v) {
            Some(x) => x.1 += 1,
            None => verbs.push((v, 1)),
        }
    }
    (!verbs.is_empty()).then(|| {
        verbs
            .iter()
            .map(|(v, n)| format!("{n} {v}"))
            .collect::<Vec<_>>()
            .join(&format!(" {} ", s.bullet()))
    })
}

/// A finished phase on one line: `✓ download    267 MiB    16s`.
fn done_row(run: &Run, p: &Phase, s: &Styles) -> Vec<Seg> {
    let what = match p.kind {
        PhaseKind::Sync => n_of(p.done.max(run.dbs.len()), "db", "dbs"),
        PhaseKind::Plan => n_of(run.packages.len(), "package", "packages"),
        PhaseKind::Download => run
            .totals
            .iter()
            .find(|t| t.0 == "Total Download Size")
            .map(|t| tidy_size(&t.1))
            .unwrap_or_else(|| n_of(p.done, "file", "files")),
        PhaseKind::Checks => n_of(p.done, "step", "steps"),
        PhaseKind::PreHooks | PhaseKind::PostHooks => n_of(p.done, "hook", "hooks"),
        PhaseKind::Packages => {
            changed(run, s).unwrap_or_else(|| n_of(p.done, "package", "packages"))
        }
    };
    let t = took(p.started, p.ended, Duration::ZERO)
        .map(duration)
        .unwrap_or_default();
    vec![
        seg(Paint::Ok, s.check()),
        seg(
            Paint::Dim,
            format!(" {:<11} {what:<16} {t:>5}", name(p.kind)),
        ),
    ]
}

/// The running phase's head: spinner, name, then its own facts.
fn head(run: &Run, p: &Phase, now: Duration, s: &Styles) -> Vec<Seg> {
    let b = s.bullet();
    let waiting = p.kind == PhaseKind::Plan && run.asking.is_some();
    let mark = if waiting {
        seg(Paint::Warn, "?")
    } else {
        seg(Paint::Warn, spinner(now, s))
    };
    let facts = match p.kind {
        PhaseKind::Sync => format!("{} done", n_of(dbs_done(run), "db", "dbs")),
        PhaseKind::Plan if waiting => plan_facts(run, s),
        PhaseKind::Plan => "resolving".into(),
        PhaseKind::Download => download_facts(run, p, s),
        PhaseKind::Checks => match &run.live_check {
            Some((n, _)) => format!("step {} {b} {n}", p.done.max(1)),
            None => format!("step {}", p.done.max(1)),
        },
        _ => match p.total {
            Some(t) => format!("{}/{t} done", hooks_or_pkgs_done(run, p)),
            None => format!("{} done", hooks_or_pkgs_done(run, p)),
        },
    };
    vec![
        mark,
        seg(Paint::Bold, format!(" {}", name(p.kind))),
        seg(Paint::Plain, format!("  {facts}")),
    ]
}

/// Done count for hooks and packages: the log's word when it has one.
fn hooks_or_pkgs_done(run: &Run, p: &Phase) -> usize {
    match p.kind {
        PhaseKind::Packages => run.packages.iter().filter(|x| x.done).count(),
        PhaseKind::PreHooks => run.pre_hooks.iter().filter(|h| h.ended.is_some()).count(),
        PhaseKind::PostHooks => run.post_hooks.iter().filter(|h| h.ended.is_some()).count(),
        _ => p.done,
    }
}

/// `8 packages · 267 MiB download · +12.4 MiB net`
fn plan_facts(run: &Run, s: &Styles) -> String {
    let b = s.bullet();
    let total = |n: &str| {
        run.totals
            .iter()
            .find(|t| t.0 == n)
            .map(|t| tidy_size(&t.1))
    };
    let mut out = n_of(run.packages.len(), "package", "packages");
    if let Some(d) = total("Total Download Size") {
        out += &format!(" {b} {d} download");
    }
    if let Some(n) = total("Net Upgrade Size") {
        let sign = if n.starts_with('-') { "" } else { "+" };
        out += &format!(" {b} {sign}{n} net");
    }
    out
}

/// `4/8 files · 69.6/267 MiB · 7.8 MiB/s · 00:25 left`: pacman's Total row,
/// once it prints one. Before that, only the files seen.
fn download_facts(run: &Run, p: &Phase, s: &Styles) -> String {
    let b = s.bullet();
    let Some(d) = p.detail.as_deref().filter(|_| p.pct.is_some()) else {
        return format!("{} done", n_of(p.done, "file", "files"));
    };
    let (got, rt, eta) = split_detail(d);
    let size = run
        .totals
        .iter()
        .find(|t| t.0 == "Total Download Size")
        .map(|t| t.1.as_str());
    let files = match p.total {
        Some(t) => format!("{}/{t} files", p.done),
        None => n_of(p.done, "file", "files"),
    };
    let eta = known_eta(&rt, eta);
    format!(
        "{files} {b} {} {b} {} {b} {eta} left",
        size_pair(&got, size).trim(),
        rate(&rt)
    )
}

/// Columns every bar row shares.
struct Cols {
    name: usize,
    cells: usize,
}

fn cols(run: &Run, w: usize) -> Cols {
    let longest = run
        .packages
        .iter()
        .map(|p| p.name.width())
        .chain(run.dbs.iter().map(|d| d.width()))
        .max()
        .unwrap_or(8);
    // About 29 name cells at 100 columns, 38 at 160.
    let name = longest.min(NAME_MAX).min(w * 29 / 100);
    // `  ├─ ` name, two spaces, 17 of size, two spaces, bar + 2, space, 4
    // of %, and one spare column so no row touches the edge.
    let cells = w
        .saturating_sub(5 + name + 2 + 17 + 2 + 2 + 1 + 4 + 1)
        .clamp(BAR_MIN, BAR_MAX);
    Cols { name, cells }
}

/// `name   20.0/80.0 MiB  ──────ᗧ · · ·   25%`
fn bar_row(c: &Cols, label: &str, size: &str, pct: u8, mouth: Mouth, s: &Styles) -> Vec<Seg> {
    let bar = if pct >= 100 {
        track(100, c.cells, Mouth::Done, s)
    } else {
        track(pct, c.cells, mouth, s)
    };
    vec![
        seg(
            Paint::Plain,
            format!("{:<w$}  ", cut_plain(label, c.name), w = c.name),
        ),
        seg(Paint::Dim, format!("{size:>17}  ")),
        seg(Paint::Plain, bar),
        seg(Paint::Plain, format!(" {pct:>3}%")),
    ]
}

/// Prefix each child row with its branch: `├─` and `└─` for the last.
fn branch(rows: Vec<Vec<Seg>>, s: &Styles) -> Vec<Vec<Seg>> {
    let (mid, end, _, _) = s.tree_glyphs();
    let n = rows.len();
    rows.into_iter()
        .enumerate()
        .map(|(i, mut r)| {
            let b = if i + 1 == n { end } else { mid };
            r.insert(0, seg(Paint::Dim, format!("  {}", b.trim_end())));
            r.insert(1, seg(Paint::Plain, " "));
            r
        })
        .collect()
}

/// Rows hanging under the running phase.
fn kids(
    run: &Run,
    p: &Phase,
    c: &Cols,
    w: usize,
    room: usize,
    now: Duration,
    s: &Styles,
) -> Vec<Vec<Seg>> {
    match p.kind {
        PhaseKind::Sync => branch(
            run.dbs
                .iter()
                .map(|d| match run.db_live.get(d) {
                    Some((pct, _)) if *pct >= 100 => {
                        vec![seg(Paint::Ok, s.check()), seg(Paint::Dim, format!(" {d}"))]
                    }
                    Some((pct, words)) => {
                        let (got, _, _) = split_detail(words);
                        bar_row(c, d, &size_pair(&got, None), *pct, Mouth::Chew, s)
                    }
                    None => vec![seg(Paint::Plain, d.clone())],
                })
                .collect(),
            s,
        ),
        PhaseKind::Download => download_kids(run, p, c, w, room, now, s),
        PhaseKind::Checks => {
            let mut r = Vec::new();
            if let Some((_, pct)) = &run.live_check {
                let bar = track(*pct, c.cells, Mouth::Chew, s);
                r.push(vec![seg(Paint::Plain, format!("    {bar} {pct:>3}%"))]);
            }
            if let Some(prev) = last_check(run) {
                r.push(vec![
                    seg(Paint::Ok, format!("    {}", s.check())),
                    seg(Paint::Dim, format!(" {prev}")),
                ]);
            }
            r
        }
        PhaseKind::Packages => package_kids(run, c, w, s),
        PhaseKind::PreHooks | PhaseKind::PostHooks => hook_kids(run, p, w, now, s),
        PhaseKind::Plan => Vec::new(),
    }
}

/// The last check pacman finished, other than the one running.
fn last_check(run: &Run) -> Option<String> {
    let now = run.live_check.as_ref().map(|c| c.0.as_str());
    run.lines
        .iter()
        .rev()
        .find_map(|l| match classify_screen(l, false) {
            ScreenEvent::Check { name, .. } if Some(name.as_str()) != now => Some(name),
            _ => None,
        })
}

fn download_kids(
    run: &Run,
    p: &Phase,
    c: &Cols,
    w: usize,
    room: usize,
    now: Duration,
    s: &Styles,
) -> Vec<Vec<Seg>> {
    // Files keep the order they first moved in, never re-sorted by speed:
    // a row stays put while its file runs.
    let order = |f: &InFlight| {
        run.started_files
            .iter()
            .position(|x| x.name == f.name)
            .unwrap_or(usize::MAX)
    };
    let mut active: Vec<&InFlight> = run.in_flight.iter().filter(|f| !f.queued).collect();
    active.sort_by_key(|f| order(f));
    let mut rows: Vec<Vec<Seg>> = Vec::new();
    // Room for the queue and stall rows under the files.
    let max = room.saturating_sub(2).max(1);
    let hidden = active.len().saturating_sub(max);
    for f in active.iter().take(max) {
        let (got, _, _) = f.parts();
        let total = run
            .packages
            .iter()
            .find(|x| x.name == f.name)
            .and_then(|x| x.download.as_deref());
        let pct = if f.done_at.is_some() { 100 } else { f.pct };
        let mouth = if f.stalled { Mouth::Wait } else { Mouth::Chew };
        rows.push(bar_row(c, &f.name, &size_pair(&got, total), pct, mouth, s));
    }
    let mut rows = branch(rows, s);
    if hidden > 0 {
        rows.push(vec![seg(
            Paint::Dim,
            format!("     +{hidden} more downloading"),
        )]);
    }
    // Every file not done and not moving waits, but only once pacman's
    // Total row gives the file count: a cached package never waits.
    let queued: Vec<&str> = run
        .in_flight
        .iter()
        .filter(|f| f.queued)
        .map(|f| f.name.as_str())
        .collect();
    let waits = match (p.pct, p.total) {
        (Some(_), Some(t)) => {
            let moving = active.iter().filter(|f| f.done_at.is_none()).count();
            t.saturating_sub(p.done + moving)
        }
        _ => queued.len(),
    };
    if waits > 0 {
        let listed = &queued[..queued.len().min(waits)];
        rows.push(vec![seg(
            Paint::Dim,
            format!("     {}", queued_row(listed, waits, w.saturating_sub(5), s)),
        )]);
    }
    // All moving files still: say for how long. Bars freeze; only the
    // phase spinner turns.
    let still = active
        .iter()
        .filter(|f| f.done_at.is_none())
        .map(|f| f.stalled_since)
        .collect::<Option<Vec<_>>>()
        .and_then(|v| v.into_iter().min());
    if let Some(since) = still.filter(|_| !active.is_empty()) {
        let t = now.saturating_sub(since);
        if t.as_secs() >= 2 {
            rows.push(vec![seg(
                Paint::Dim,
                format!("     no bytes for {}", duration(t)),
            )]);
        }
    }
    rows
}

/// `old → new` for a planned package, when the plan gave both.
fn versions(run: &Run, name: &str, s: &Styles) -> String {
    run.packages
        .iter()
        .find(|p| p.name == name)
        .map(|p| match (&p.old, &p.new) {
            (Some(o), Some(n)) => format!("{o} {} {n}", s.arrow()),
            (None, Some(n)) => n.clone(),
            _ => String::new(),
        })
        .unwrap_or_default()
}

/// The package finished last, then the one pacman is on, with its (i/n)
/// and its own percent.
fn package_kids(run: &Run, c: &Cols, w: usize, s: &Styles) -> Vec<Vec<Seg>> {
    let live = run.live_package.as_ref();
    let prev = run
        .lines
        .iter()
        .rev()
        .find_map(|l| match classify_screen(l, false) {
            ScreenEvent::Package { name, .. } if live.is_none_or(|x| x.1 != name) => Some(name),
            _ => None,
        });
    let mut rows = Vec::new();
    if let Some(n) = prev {
        rows.push(vec![
            seg(Paint::Ok, s.check()),
            seg(Paint::Dim, format!(" {n}  {}", versions(run, &n, s))),
        ]);
    }
    let mut bar = None;
    if let Some((verb, n, pct)) = live {
        let at = run
            .live_at
            .map(|(i, t)| format!("  ({i}/{t})"))
            .unwrap_or_default();
        let verb = if verb == "upgrading" {
            String::new()
        } else {
            format!("{verb} ")
        };
        rows.push(vec![
            seg(Paint::Warn, "▸"),
            seg(Paint::Bold, format!(" {verb}{n}")),
            seg(Paint::Plain, format!("  {}", versions(run, n, s))),
            seg(Paint::Dim, at),
        ]);
        bar = Some(*pct);
    }
    let mut rows = branch(rows, s);
    if let Some(pct) = bar {
        let t = track(pct, c.cells, Mouth::Chew, s);
        rows.push(vec![seg(Paint::Plain, format!("       {t} {pct:>3}%"))]);
    }
    let _ = w;
    rows
}

/// The running hook: its file name, time against its usual time, its
/// description, the last few lines it printed.
fn hook_kids(run: &Run, p: &Phase, w: usize, now: Duration, s: &Styles) -> Vec<Vec<Seg>> {
    let hooks = if p.kind == PhaseKind::PreHooks {
        &run.pre_hooks
    } else {
        &run.post_hooks
    };
    let Some(h) = hooks
        .iter()
        .rev()
        .find(|h| h.started.is_some() && h.ended.is_none())
        .or(hooks.last())
    else {
        return Vec::new();
    };
    let (_, end, _, _) = s.tree_glyphs();
    let label = h
        .file
        .clone()
        .or_else(|| h.desc.clone())
        .unwrap_or_default();
    let mut time = Vec::new();
    if let Some(t) = took(h.started, h.ended, now).filter(|t| t.as_secs() >= 1) {
        time.push(duration(t));
    }
    if let Some(m) = h.median.as_ref().filter(|m| m.secs >= 1) {
        time.push(format!("usual {}", duration(Duration::from_secs(m.secs))));
    }
    let time = time.join(&format!(" {} ", s.bullet()));
    let mut rows = vec![vec![
        seg(Paint::Dim, format!("  {} ", end.trim_end())),
        seg(Paint::Warn, "▸"),
        seg(Paint::Bold, format!(" {label}")),
        seg(Paint::Dim, format!("  {time}")),
    ]];
    if h.file.is_some()
        && let Some(d) = &h.desc
    {
        rows.push(vec![seg(Paint::Dim, format!("       {d}"))]);
    }
    let (_, _, pipe, _) = s.tree_glyphs();
    for l in log_lines(run, TAIL) {
        rows.push(vec![seg(
            Paint::Dim,
            cut(&format!("       {pipe} {}", l.trim_end()), w, s),
        )]);
    }
    rows
}

/// The live region: header, finished phases folded, the running one open,
/// then what comes next.
pub(super) fn live(run: &Run, w: usize, height: usize, now: Duration, s: &Styles) -> Vec<String> {
    // Nothing of pacman's has run yet (the password prompt): nothing to draw.
    if run.phases.iter().all(|p| p.started.is_none()) {
        return Vec::new();
    }
    let mut rows: Vec<Vec<Seg>> = vec![vec![
        seg(Paint::Bold, run.command.clone()),
        seg(
            Paint::Dim,
            format!(" {} {}", s.bullet(), duration(elapsed(run, now))),
        ),
    ]];
    let c = cols(run, w);
    let running = run
        .phases
        .iter()
        .position(|p| p.state == PhaseState::Running);
    for p in &run.phases {
        if p.state == PhaseState::Done {
            rows.push(done_row(run, p, s));
        }
    }
    if let Some(i) = running {
        let p = &run.phases[i];
        rows.push(head(run, p, now, s));
        // Keep the `next` row and a spare line.
        let room = height.saturating_sub(rows.len() + 2);
        rows.extend(kids(run, p, &c, w, room, now, s));
        let next: Vec<&str> = run.phases[i + 1..]
            .iter()
            .filter(|q| q.state == PhaseState::Pending)
            .map(|q| name(q.kind))
            .collect();
        if !next.is_empty() {
            let sep = format!(" {} ", g(s, "›", ">"));
            rows.push(vec![seg(
                Paint::Dim,
                format!("  next  {}", next.join(&sep)),
            )]);
        }
    }
    rows.iter().map(|r| line(r, w, s)).collect()
}

/// The plan while pacman asks: lands' header, then the shared plan grouped
/// by repo in columns. Then pacman's question, verbatim.
pub(super) fn plan_rows(
    run: &Run,
    prompt: &str,
    w: usize,
    height: usize,
    now: Duration,
    s: &Styles,
) -> Vec<String> {
    let mut out = live(run, w, height, now, s);
    // The `next` row says nothing while pacman waits.
    if out.last().is_some_and(|l| l.contains("next")) {
        out.pop();
    }
    let room = height.saturating_sub(out.len() + 3);
    // Its first row repeats the totals the `? plan` row already shows.
    out.extend(compact_plan(run, w, room + 1, s).into_iter().skip(1));
    out.push(String::new());
    out.push(match prompt.rsplit_once('[') {
        Some((q, a)) if !q.contains('\u{1b}') => {
            format!("{}{}", s.title(q), s.warn(&format!("[{a}")))
        }
        _ => prompt.to_string(),
    });
    out
}

/// On exit: the result, then each step the run's own output asked of the
/// user, its source named, reboot last. A failure keeps pacman's errors.
pub(super) fn closing(run: &Run, w: usize, transcript: &str, s: &Styles) -> Vec<String> {
    let b = s.bullet();
    let took = duration(elapsed(run, Duration::ZERO));
    let pac = g(s, "ᗧ", "C");
    let head = match run.outcome() {
        Outcome::Succeeded => match changed(run, s) {
            Some(c) => format!("{} done {b} {c} {b} {took}", s.warn(pac)),
            None => format!("{} done {b} {took}", s.warn(pac)),
        },
        Outcome::NothingChanged => format!("{} no changes", s.warn(pac)),
        Outcome::Unknown => format!("? pacman exit 0 {b} changes unknown"),
        Outcome::Running => "… pacman running".into(),
        Outcome::Failed(code) => {
            let at = run
                .phases
                .iter()
                .rev()
                .find(|p| p.state != PhaseState::Pending)
                .map(|p| format!(" {b} {}", name(p.kind)))
                .unwrap_or_default();
            let what = code.map_or("pacman stopped by a signal".into(), |n| {
                format!("pacman exit {n}")
            });
            let mut out = vec![s.error(&format!("{} {what}{at}", s.cross()))];
            let errors: Vec<&Note> = all_notes(run)
                .filter(|n| matches!(n.kind, NoteKind::Error { .. }))
                .collect();
            out.extend(errors.iter().map(|n| format!("error: {}", n.text)));
            // pacman's own last words, for context.
            let tail: Vec<&String> = run
                .lines
                .iter()
                .rev()
                .filter(|l| !l.trim().is_empty() && !l.starts_with("error: "))
                .take(3)
                .collect();
            out.extend(tail.into_iter().rev().map(|l| s.dim(l)));
            out.push(s.dim(&format!("full output: paclens transcript ({transcript})")));
            return out;
        }
    };
    let mut out = vec![line(&[seg(Paint::Bold, head)], w, s)];
    let needs = run.needs_you();
    let text = |k: &NoteKind| -> Vec<String> {
        needs
            .iter()
            .filter(|n| &n.kind == k)
            .map(|n| n.text.clone())
            .collect()
    };
    let mut steps: Vec<(String, String)> = Vec::new();
    let rebuild: Vec<String> = text(&NoteKind::Rebuild)
        .iter()
        .filter_map(|t| t.split_whitespace().last().map(str::to_string))
        .collect();
    if !rebuild.is_empty() {
        steps.push((
            format!("needs rebuild: {}", rebuild.join(", ")),
            String::new(),
        ));
    }
    for w in text(&NoteKind::Warning)
        .into_iter()
        .filter(|w| !w.contains(" is newer than "))
    {
        let (cmd, why) = action(&w);
        let whose = run
            .lines
            .iter()
            .position(|l| l.contains(w.as_str()))
            .and_then(|at| package_at(run, at))
            .unwrap_or_default();
        steps.push(if cmd == "warning" {
            (why, whose)
        } else {
            (format!("{cmd}  {why}"), whose)
        });
    }
    // A package's own `$ command` line is a step, in the package's name.
    let from = transaction_from(run).unwrap_or(usize::MAX);
    for (at, l) in run.unknown_at.iter().zip(&run.unknown) {
        let t = l.trim_start();
        let Some(cmd) = t.strip_prefix("$ ").or_else(|| t.strip_prefix("# ")) else {
            continue;
        };
        if *at < from || in_hooks(run, *at) {
            continue;
        }
        if let Some(p) = package_at(run, *at) {
            steps.push((cmd.trim().to_string(), p));
        }
    }
    let pacnew = text(&NoteKind::Pacnew);
    if !pacnew.is_empty() {
        steps.push((format!(".pacnew: {}", pacnew.join(", ")), String::new()));
    }
    if let Some(t) = text(&NoteKind::Reboot).first() {
        steps.push((
            sentence_head(t).to_lowercase(),
            hook_of(run, t).unwrap_or_default(),
        ));
    }
    for (what, from) in steps {
        let from = if from.is_empty() {
            String::new()
        } else {
            format!("  {} {from}", g(s, "←", "<-"))
        };
        out.push(line(
            &[
                seg(Paint::Warn, "!"),
                seg(Paint::Plain, format!(" {what}")),
                seg(Paint::Dim, from),
            ],
            w,
            s,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ColorTheme;
    use crate::update_view::model::fixture_run;
    use crate::update_view::screen::fixture_lines;

    /// Unicode glyphs, no colour.
    fn plain() -> Styles {
        Styles::resolve(false, ColorTheme::Dark, false)
    }

    /// The fixture's first `n` settled lines, not finished.
    fn partial(n: usize) -> Run {
        let mut r = Run::new("pacman -Syu", Default::default(), Default::default());
        for l in fixture_lines().iter().take(n) {
            r.apply_line(l, Duration::ZERO);
        }
        r
    }

    fn text(v: &[String]) -> String {
        v.join("\n")
    }

    #[test]
    fn n_of_picks_the_word() {
        assert_eq!(n_of(1, "db", "dbs"), "1 db");
        assert_eq!(n_of(3, "db", "dbs"), "3 dbs");
    }

    #[test]
    fn bar_cells_stay_between_min_and_max() {
        let r = fixture_run();
        assert_eq!(cols(&r, 400).cells, BAR_MAX);
        assert_eq!(cols(&r, 60).cells, BAR_MIN);
        assert!(cols(&r, 400).name <= NAME_MAX);
    }

    #[test]
    fn live_rows_never_pass_the_width() {
        for n in [5, 20, 60, 120, 200, 400] {
            let r = partial(n);
            for w in [100, 160] {
                for l in live(&r, w, 40, Duration::from_secs(30), &plain()) {
                    assert!(l.width() <= w, "n={n} w={w}: {l:?}");
                }
            }
        }
    }

    #[test]
    fn finished_phases_fold_to_one_line_each() {
        let r = fixture_run();
        let t = text(&live(&r, 120, 40, Duration::from_secs(300), &plain()));
        for k in ["sync", "plan", "download", "checks", "packages"] {
            let mark = format!("✓ {k} ");
            assert_eq!(
                t.lines().filter(|l| l.starts_with(&mark)).count(),
                1,
                "{k}\n{t}"
            );
        }
        assert!(!t.contains("next"), "{t}");
    }

    #[test]
    fn success_receipt_says_what_changed_not_eaten() {
        let r = fixture_run();
        let c = text(&closing(&r, 120, "/tmp/x", &plain()));
        let first = c.lines().next().unwrap_or_default();
        assert!(first.contains("done"), "{c}");
        assert!(
            first.contains("38 upgraded") || first.contains("upgraded"),
            "{c}"
        );
        assert!(!c.contains("eaten"), "{c}");
        assert!(!c.contains("good to know"), "{c}");
    }

    #[test]
    fn reboot_is_the_last_step() {
        let r = fixture_run();
        let c = closing(&r, 160, "/tmp/x", &plain());
        if let Some(i) = c.iter().position(|l| l.to_lowercase().contains("reboot")) {
            assert_eq!(i + 1, c.len(), "{c:?}");
        }
    }

    #[test]
    fn failure_keeps_the_exit_and_the_transcript() {
        let mut r = fixture_run();
        r.exit = Some(1);
        let c = text(&closing(&r, 120, "/tmp/x", &plain()));
        assert!(c.contains("pacman exit 1"), "{c}");
        assert!(c.contains("paclens transcript"), "{c}");
        assert!(!c.contains(" done "), "{c}");
    }

    #[test]
    fn changed_counts_each_verb() {
        let mut r = fixture_run();
        let s = plain();
        let n = r.packages.iter().filter(|p| p.done).count();
        assert_eq!(changed(&r, &s), Some(format!("{n} upgraded")));
        if let Some(p) = r.packages.iter_mut().find(|p| p.done) {
            p.verb = Some("installed".into());
        }
        let c = changed(&r, &s).unwrap_or_default();
        assert!(c.contains("1 installed"), "{c}");
        for p in &mut r.packages {
            p.done = false;
        }
        assert_eq!(changed(&r, &s), None);
    }
}
