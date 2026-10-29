//! The run model drawn as text (#81): the live region, the lines printed
//! once above it, the receipt or failure block, and the end line. Pure — no
//! terminal, no clock; the caller passes width, height and `now`.

use std::time::Duration;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::cli::style::{Ghost, Styles};
use crate::config::{Layout, Motif};

use super::medians::Median;
use super::model::{Hook, Note, NoteKind, Outcome, Phase, PhaseKind, PhaseState, Run};
use super::screen_line::{ScreenEvent, classify_screen, is_prompt};

pub struct View {
    pub layout: Layout,
    pub motif: Motif,
    pub candy: bool,
}

/// `above` = lines to print permanently over the region; `region` = the
/// live lines, redrawn each frame.
#[derive(Debug, Default)]
pub struct Frame {
    pub above: Vec<String>,
    pub region: Vec<String>,
}

/// How much is already in `above`: `run.lines` and `run.phases` by count,
/// unknown lines by line position in `run.lines`.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Delivered {
    pub lines: usize,
    pub unknown: usize,
    pub phases: usize,
}

/// How one segment is painted, once it has been fitted to the width.
#[derive(Clone, Copy)]
enum Paint {
    Plain,
    Dim,
    Bold,
    Ok,
    Bad,
    /// Bold yellow: the running mark, the Pac-Man head.
    Warn,
    Ghost(Ghost),
}

type Seg = (Paint, String);

fn seg(p: Paint, t: impl Into<String>) -> Seg {
    (p, t.into())
}

fn paint(p: Paint, t: &str, s: &Styles) -> String {
    if t.is_empty() {
        return String::new();
    }
    match p {
        Paint::Plain => t.to_string(),
        Paint::Dim => s.dim(t),
        Paint::Bold => s.title(t),
        Paint::Ok => s.success(t),
        Paint::Bad => s.error(t),
        Paint::Warn => s.warn(t),
        Paint::Ghost(g) => s.ghost(g, t),
    }
}

/// Fit plain segments into `width` columns, **then** style each one: a
/// styled string is never cut, so no reset code is ever lost.
fn line(segs: &[Seg], width: usize, s: &Styles) -> String {
    let mut out = String::new();
    let mut left = width;
    for (p, t) in segs {
        let w = visible_width(t);
        if w <= left {
            out += &paint(*p, t, s);
            left -= w;
        } else {
            // A segment that is already coloured (a bar) is never cut.
            if !t.contains('\u{1b}') {
                out += &paint(*p, &cut(t, left, s), s);
            }
            break;
        }
    }
    out
}

/// Columns `t` takes on screen, colour escapes left out.
fn visible_width(t: &str) -> usize {
    if !t.contains('\u{1b}') {
        return t.width();
    }
    let mut esc = false;
    t.chars()
        .filter(|&c| match (esc, c) {
            (false, '\u{1b}') => {
                esc = true;
                false
            }
            (true, c) => {
                esc = !c.is_ascii_alphabetic();
                false
            }
            (false, _) => true,
        })
        .map(|c| c.width().unwrap_or(0))
        .sum()
}

/// `t` cut to `w` columns, the last of them an ellipsis.
fn cut(t: &str, w: usize, s: &Styles) -> String {
    if t.width() <= w {
        return t.to_string();
    }
    let Some(room) = w.checked_sub(1) else {
        return String::new();
    };
    let mut out = String::new();
    let mut used = 0;
    for c in t.chars() {
        let cw = c.width().unwrap_or(0);
        if used + cw > room {
            break;
        }
        used += cw;
        out.push(c);
    }
    out.push_str(if s.plain() { "~" } else { "…" });
    out
}

/// The Unicode glyph, or its ASCII stand-in on the plain path.
fn g(s: &Styles, uni: &'static str, ascii: &'static str) -> &'static str {
    if s.plain() { ascii } else { uni }
}

fn name(k: PhaseKind) -> &'static str {
    match k {
        PhaseKind::Sync => "sync",
        PhaseKind::Plan => "plan",
        PhaseKind::Download => "download",
        PhaseKind::Checks => "checks",
        PhaseKind::PreHooks => "pre-hooks",
        PhaseKind::Packages => "packages",
        PhaseKind::PostHooks => "post-hooks",
    }
}

fn count(p: &Phase) -> String {
    match (p.state, p.total) {
        (PhaseState::Pending, _) => String::new(),
        // Each check counts the packages again; the check's name says more.
        (PhaseState::Running, _) if p.kind == PhaseKind::Checks => String::new(),
        (PhaseState::Running, None) if p.done == 0 => String::new(),
        (PhaseState::Running, Some(t)) => format!("{}/{t}", p.done),
        _ => p.done.to_string(),
    }
}

fn took(started: Option<Duration>, ended: Option<Duration>, now: Duration) -> Option<Duration> {
    Some(ended.unwrap_or(now).saturating_sub(started?))
}

fn phase(run: &Run, k: PhaseKind) -> Option<&Phase> {
    run.phases.iter().find(|p| p.kind == k)
}

fn all_notes(run: &Run) -> impl Iterator<Item = &Note> {
    let hooks = run.pre_hooks.iter().chain(&run.post_hooks);
    run.notes.iter().chain(hooks.flat_map(|h| &h.notes))
}

fn download_size(run: &Run, s: &Styles) -> String {
    run.totals
        .iter()
        .find(|t| t.0 == "Total Download Size")
        .map(|t| format!(" {} {}", s.bullet(), t.1))
        .unwrap_or_default()
}

/// `pacman -Syu · packages · 2m00s`, plus one ghost per kind of note that
/// wants the user (arcade).
fn header(run: &Run, view: &View, now: Duration, s: &Styles) -> Vec<Seg> {
    let b = s.bullet();
    let at = match run.current() {
        Some(k) => name(k),
        None if run.finished => "done",
        None => "starting",
    };
    let mut segs = vec![
        seg(Paint::Bold, run.command.clone()),
        seg(
            Paint::Dim,
            format!(" {b} {at} {b} {}", duration(run.ended.unwrap_or(now))),
        ),
    ];
    if view.motif == Motif::Arcade {
        let ghosts = [
            (Ghost::Blinky, NoteKind::Error { recovered: false }),
            (Ghost::Pinky, NoteKind::Pacnew),
            (Ghost::Inky, NoteKind::Optdepends),
            (Ghost::Clyde, NoteKind::Warning),
        ];
        for (ghost, kind) in ghosts {
            if all_notes(run).any(|n| n.kind == kind) {
                segs.push(seg(Paint::Ghost(ghost), format!(" {}", g(s, "ᗣ", "G"))));
            }
        }
    }
    segs
}

fn phase_row(p: &Phase, now: Duration, s: &Styles) -> Vec<Seg> {
    let (mark_paint, mark) = match p.state {
        PhaseState::Done => (Paint::Ok, s.check()),
        PhaseState::Running => (Paint::Warn, g(s, "▸", ">")),
        PhaseState::Pending => (Paint::Dim, g(s, "·", ".")),
    };
    let time = took(p.started, p.ended, now)
        .map(duration)
        .unwrap_or_default();
    let n = count(p);
    let noun = match (p.kind, n.is_empty()) {
        (_, true) => "",
        (PhaseKind::Sync, _) => "dbs",
        (PhaseKind::PreHooks | PhaseKind::PostHooks, _) => "hooks",
        _ => "pkgs",
    };
    let text = format!(" {:<10} {n:>7} {noun:<5} {time}", name(p.kind));
    // The running phase stands out; finished ones step back.
    let text_paint = match p.state {
        PhaseState::Running => Paint::Bold,
        PhaseState::Done | PhaseState::Pending => Paint::Dim,
    };
    vec![
        seg(mark_paint, mark),
        seg(text_paint, text.trim_end().to_string()),
    ]
}

/// Done phases folded into one row: `✓ sync 5 · plan 38 · download 38  1m17s`.
fn done_summary(run: &Run, now: Duration, s: &Styles) -> Option<Vec<Seg>> {
    let done: Vec<&Phase> = run
        .phases
        .iter()
        .filter(|p| p.state == PhaseState::Done)
        .collect();
    if done.is_empty() {
        return None;
    }
    let list: Vec<String> = done
        .iter()
        .map(|p| format!("{} {}", name(p.kind), p.done))
        .collect();
    let total: Duration = done
        .iter()
        .filter_map(|p| took(p.started, p.ended, now))
        .sum();
    let sep = format!(" {} ", s.bullet());
    Some(vec![
        seg(Paint::Ok, s.check()),
        seg(
            Paint::Dim,
            format!(" {}  {}", list.join(&sep), duration(total)),
        ),
    ])
}

/// `12s · usually 23s`: the time so far, and the median of past runs.
fn hook_time(h: &Hook, now: Duration, s: &Styles) -> String {
    let t = took(h.started, h.ended, now)
        .map(duration)
        .unwrap_or_default();
    match h.median {
        // Only while it runs: once done, its own time says it all.
        Some(Median { secs, .. }) if h.ended.is_none() => format!(
            "{t} {} usually {}",
            s.bullet(),
            duration(Duration::from_secs(secs))
        ),
        _ => t,
    }
}

/// What the running phase is on now: a package, a hook, the download.
fn item(run: &Run, now: Duration, s: &Styles) -> Option<Vec<Seg>> {
    let k = run.current()?;
    let ph = phase(run, k)?;
    let total = ph.total.map_or_else(|| "?".to_string(), |t| t.to_string());
    match k {
        PhaseKind::Packages => {
            // The model keeps packages in plan order; the one on screen now
            // is the last `(i/n) verb name` line pacman settled.
            let (verb, pname) =
                run.lines
                    .iter()
                    .rev()
                    .find_map(|l| match classify_screen(l, false) {
                        ScreenEvent::Package { verb, name, .. } => Some((verb, name)),
                        _ => None,
                    })?;
            let p = run.packages.iter().find(|p| p.name == pname)?;
            let mut segs = vec![seg(
                Paint::Plain,
                format!("({}/{total}) {verb} {pname}", ph.done),
            )];
            if let (Some(old), Some(new)) = (&p.old, &p.new) {
                segs.push(seg(Paint::Dim, format!("  {old} {} {new}", s.arrow())));
            }
            Some(segs)
        }
        PhaseKind::PreHooks | PhaseKind::PostHooks => {
            let list = if k == PhaseKind::PreHooks {
                &run.pre_hooks
            } else {
                &run.post_hooks
            };
            let (i, h) = list
                .iter()
                .enumerate()
                .rfind(|(_, h)| h.started.is_some())?;
            let desc = h.desc.as_deref().or(h.file.as_deref()).unwrap_or_default();
            Some(vec![
                seg(Paint::Plain, format!("({}/{total}) {desc}", i + 1)),
                seg(Paint::Dim, format!("  {}", hook_time(h, now, s))),
            ])
        }
        PhaseKind::Download => {
            let mut segs = vec![seg(
                Paint::Plain,
                format!("{}/{total}{}", ph.done, download_size(run, s)),
            )];
            if let Some(d) = &ph.detail {
                segs.push(seg(Paint::Dim, format!("  {d}")));
            }
            Some(segs)
        }
        PhaseKind::Sync => Some(vec![seg(Paint::Plain, run.dbs.last()?.clone())]),
        _ => None,
    }
}

/// Arcade's package lane under the tree, once packages are known; the
/// measured bar itself sits on the running phase's row.
fn arcade_lane(run: &Run, view: &View, w: usize, s: &Styles) -> Option<String> {
    (view.motif == Motif::Arcade && !run.packages.is_empty()).then(|| lane(run, w, s))
}

/// One `·` per real package, `ᗧ` at the done count, a pellet `●` at the end
/// when a reboot is due. Scaled down only when it would not fit.
fn lane(run: &Run, w: usize, s: &Styles) -> String {
    let n = run.packages.len();
    let done = run.packages.iter().filter(|p| p.done).count().min(n);
    let room = w.saturating_sub(3).max(1);
    let (cells, eaten) = if n > room {
        (room, done * room / n)
    } else {
        (n, done)
    };
    let mut segs = vec![
        seg(Paint::Plain, " ".repeat(eaten)),
        seg(Paint::Warn, g(s, "ᗧ", "C")),
        seg(Paint::Dim, g(s, "·", ".").repeat(cells - eaten)),
    ];
    if all_notes(run).any(|n| n.kind == NoteKind::Reboot) {
        segs.push(seg(Paint::Bold, format!(" {}", g(s, "●", "o"))));
    }
    line(&segs, w, s)
}

/// Width of a phase row's label: mark, name, count, noun, time.
const PHASE_W: usize = 35;
/// `1048.1/1223.3 MiB`, `10.12 MiB/s`, `00:02:51`.
const SIZE_W: usize = 17;
const RATE_W: usize = 11;
const ETA_W: usize = 8;

/// Columns the running phase row and its download rows share, so every bar
/// starts and ends in the same place: `left` columns of label, `[` + `cells`
/// + `] `, the percent, then the numbers that fit.
struct Gauge {
    left: usize,
    cells: usize,
    size: bool,
    rate: bool,
}

fn gauge(run: &Run, w: usize) -> Gauge {
    let download = run.current() == Some(PhaseKind::Download);
    // `  ├─ ` + the longest planned name + a space, unless the phase label
    // is wider; a third of the screen at most.
    let name_w = if download {
        run.packages
            .iter()
            .map(|p| p.name.width())
            .max()
            .unwrap_or(0)
            .min(w / 3)
    } else {
        0
    };
    let mut g = Gauge {
        left: PHASE_W.max(5 + name_w + 1),
        cells: 0,
        size: download,
        rate: download,
    };
    // Numbers give way before the bar gets short: the rate first, then sizes.
    loop {
        let right = 5
            + if g.size { 2 + SIZE_W } else { 0 }
            + if g.rate { 2 + RATE_W + 2 + ETA_W } else { 0 };
        g.cells = w.saturating_sub(g.left + 3 + right + 1);
        if g.cells >= 24 || !(g.size || g.rate) {
            return g;
        }
        if g.rate {
            g.rate = false;
        } else {
            g.size = false;
        }
    }
}

/// `"53.7 MiB"` or `"939 KiB/s"` in bytes.
fn bytes(t: &str) -> Option<f64> {
    let mut it = t.split_whitespace();
    let n: f64 = it.next()?.parse().ok()?;
    let mul = match it.next()?.trim_end_matches("/s") {
        "B" => 1.0,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some(n * mul)
}

/// `53.7/155.5 MiB`: bytes so far over the planned size, both in the unit
/// that suits the total. Without a total, just the bytes.
fn size_pair(got: &str, total: Option<&str>) -> String {
    let Some(g) = bytes(got) else {
        return got.to_string();
    };
    let t = total.and_then(bytes);
    let top = t.unwrap_or(g);
    let (div, unit, dec) = if top >= 1024.0 * 1024.0 * 1024.0 {
        (1024.0 * 1024.0 * 1024.0, "GiB", 2)
    } else if top >= 1024.0 * 1024.0 {
        (1024.0 * 1024.0, "MiB", 1)
    } else {
        (1024.0, "KiB", 0)
    };
    match t {
        Some(t) => format!("{:.dec$}/{:.dec$} {unit}", g / div, t / div),
        None => format!("{:.dec$} {unit}", g / div),
    }
}

/// Every rate in MiB/s, so the column reads down.
fn rate(t: &str) -> String {
    bytes(t).map_or_else(
        || t.to_string(),
        |b| format!("{:.2} MiB/s", b / (1024.0 * 1024.0)),
    )
}

/// `[----C o  o ]  42%  53.7/155.5 MiB  2.86 MiB/s  00:29`, as much as
/// the gauge has room for. `approx`: the percent is an estimate (`~42%`).
fn gauge_tail(
    g: &Gauge,
    pct: u8,
    approx: bool,
    numbers: Option<(String, String, String)>,
    s: &Styles,
) -> Vec<Seg> {
    let mut segs = Vec::new();
    if g.cells >= 6 {
        segs.push(seg(
            Paint::Plain,
            format!("[{}] ", bar(pct, g.cells, true, s)),
        ));
    }
    let p = if approx {
        format!("~{pct}%")
    } else {
        format!("{pct}%")
    };
    segs.push(seg(Paint::Plain, format!("{p:>4}")));
    if let Some((size, rate, eta)) = numbers {
        let mut t = String::new();
        if g.size {
            t += &format!("  {size:>SIZE_W$}");
        }
        if g.rate {
            t += &format!("  {rate:>RATE_W$}  {eta:>ETA_W$}");
        }
        segs.push(seg(Paint::Dim, t));
    }
    segs
}

/// pacman's `363.0 MiB 5.12 MiB/s 00:02:51` split into its three parts.
fn split_detail(d: &str) -> (String, String, String) {
    let t: Vec<&str> = d.split_whitespace().collect();
    let two = |i: usize| t.get(i..i + 2).map(|p| p.join(" ")).unwrap_or_default();
    (
        two(0),
        two(2),
        t.get(4).copied().unwrap_or_default().to_string(),
    )
}

/// The running phase's own row: its label, then — when there is a measure —
/// the bar, in the same columns as its children's.
fn running_row(run: &Run, p: &Phase, g: &Gauge, now: Duration, s: &Styles) -> Vec<Seg> {
    let mut row = phase_row(p, now, s);
    let pct = match p.kind {
        // Each check fills its own bar in a blink; its name says more.
        PhaseKind::Checks => None,
        _ => run.progress(now),
    };
    let Some(pct) = pct else {
        if let Some(d) = &p.detail {
            row.push(seg(Paint::Dim, format!("  {d}")));
        }
        return row;
    };
    let used: usize = row.iter().map(|(_, t)| t.width()).sum();
    row.push(seg(Paint::Plain, " ".repeat(g.left.saturating_sub(used))));
    let numbers = (p.kind == PhaseKind::Download).then(|| {
        let (got, r, eta) = split_detail(p.detail.as_deref().unwrap_or_default());
        let total = run
            .totals
            .iter()
            .find(|t| t.0 == "Total Download Size")
            .map(|t| t.1.as_str());
        (size_pair(&got, total), rate(&r), eta)
    });
    row.extend(gauge_tail(g, pct, run.progress_estimated(), numbers, s));
    row
}

/// The children of the running phase: what pacman is doing right now, one
/// row each — files downloading, the packages being upgraded, the hook that
/// runs and its last lines of output.
fn children(run: &Run, ga: &Gauge, w: usize, now: Duration, s: &Styles) -> Vec<Vec<Seg>> {
    let Some(k) = run.current() else {
        return Vec::new();
    };
    let mut rows: Vec<Vec<Seg>> = match k {
        PhaseKind::Download => {
            // The name column ends where the phase label does, so the bars
            // line up with the phase's own.
            let name_w = ga.left.saturating_sub(5 + 1);
            let mut rows: Vec<Vec<Seg>> = run
                .in_flight
                .iter()
                .filter(|f| !f.queued)
                .map(|f| {
                    let name = format!("{:<name_w$} ", cut_plain(&f.name, name_w));
                    let (got, r, eta) = f.parts();
                    let total = run
                        .packages
                        .iter()
                        .find(|p| p.name == f.name)
                        .and_then(|p| p.download.as_deref());
                    let (r, eta) = if f.stalled {
                        ("stalled".to_string(), String::new())
                    } else {
                        (rate(&r), eta)
                    };
                    let mut row = vec![seg(Paint::Plain, name)];
                    row.extend(gauge_tail(
                        ga,
                        f.pct,
                        false,
                        Some((size_pair(&got, total), r, eta)),
                        s,
                    ));
                    row
                })
                .collect();
            // The queue on one line: names until the width runs out.
            let queued: Vec<&str> = run
                .in_flight
                .iter()
                .filter(|f| f.queued)
                .map(|f| f.name.as_str())
                .collect();
            if !queued.is_empty() {
                let sep = format!(" {} ", s.bullet());
                let mut text = String::from("queued  ");
                let mut n = 0;
                for q in &queued {
                    let more = format!("{sep}+{}", queued.len() - n);
                    let add = if n == 0 {
                        q.to_string()
                    } else {
                        format!("{sep}{q}")
                    };
                    if 5 + text.width() + add.width() + more.width() > w {
                        break;
                    }
                    text += &add;
                    n += 1;
                }
                if n < queued.len() {
                    if n > 0 {
                        text += &sep;
                    }
                    text += &format!("+{}", queued.len() - n);
                }
                rows.push(vec![seg(Paint::Dim, text)]);
            }
            rows
        }
        PhaseKind::Packages => {
            // The last few `(i/n) verb name` lines: done ones dim, the
            // current one bold, versions in aligned columns.
            let recent: Vec<(String, String)> = run
                .lines
                .iter()
                .rev()
                .filter_map(|l| match classify_screen(l, false) {
                    ScreenEvent::Package { verb, name, .. } => Some((verb, name)),
                    _ => None,
                })
                .take(3)
                .collect();
            let n = recent.len();
            let verb_w = recent.iter().map(|(v, _)| v.width()).max().unwrap_or(0);
            let name_w = run
                .packages
                .iter()
                .map(|p| p.name.width())
                .max()
                .unwrap_or(0)
                .min(w / 3);
            recent
                .into_iter()
                .rev()
                .enumerate()
                .map(|(i, (verb, name))| {
                    let current = i + 1 == n;
                    let mut row = vec![
                        seg(Paint::Dim, format!("{verb:<verb_w$} ")),
                        seg(
                            if current { Paint::Bold } else { Paint::Dim },
                            format!("{:<name_w$}  ", cut_plain(&name, name_w)),
                        ),
                    ];
                    let pkg = run.packages.iter().find(|p| p.name == name);
                    if let Some((Some(old), Some(new))) = pkg.map(|p| (&p.old, &p.new)) {
                        if current {
                            let (o, n) = version_diff(old, new);
                            row.extend(o);
                            row.push(seg(Paint::Dim, format!(" {} ", s.arrow())));
                            row.extend(n);
                        } else {
                            row.push(seg(Paint::Dim, format!("{old} {} {new}", s.arrow())));
                        }
                    }
                    row
                })
                .collect()
        }
        PhaseKind::PreHooks | PhaseKind::PostHooks => {
            let list = if k == PhaseKind::PreHooks {
                &run.pre_hooks
            } else {
                &run.post_hooks
            };
            // The one before (done, dim) and the one running (bold); the
            // phase row carries the count.
            let started: Vec<&Hook> = list.iter().filter(|h| h.started.is_some()).collect();
            let from = started.len().saturating_sub(2);
            started[from..]
                .iter()
                .enumerate()
                .map(|(i, h)| {
                    let desc = h.desc.as_deref().or(h.file.as_deref()).unwrap_or_default();
                    let running = h.ended.is_none() && from + i + 1 == started.len();
                    let (mark, paint) = if running {
                        (seg(Paint::Warn, g(s, "▸", ">")), Paint::Bold)
                    } else {
                        (seg(Paint::Ok, s.check()), Paint::Dim)
                    };
                    vec![
                        mark,
                        seg(paint, format!(" {desc}")),
                        seg(Paint::Dim, format!("  {}", hook_time(h, now, s))),
                    ]
                })
                .collect()
        }
        PhaseKind::Checks => Vec::new(),
        _ => item(run, now, s).into_iter().collect(),
    };
    // After the answer: the transaction's own output (hooks, scriptlets),
    // last few lines, under what produced it; the newest one reads plain.
    let (_, _, pipe, _) = s.tree_glyphs();
    let tail = log_lines(run, LOG_TAIL);
    let n = tail.len();
    for (i, l) in tail.into_iter().enumerate() {
        let paint = if i + 1 == n { Paint::Plain } else { Paint::Dim };
        rows.push(vec![
            seg(Paint::Dim, format!("{NESTED}{pipe}")),
            seg(paint, l.trim_end().to_string()),
        ]);
    }
    rows
}

/// Marks a child row that hangs under the one before it (output of the
/// running hook), so the tree gives it no branch of its own.
const NESTED: &str = "\u{0}";

/// `name` cut to `w` columns with an ellipsis, uncoloured (it is padded after).
fn cut_plain(name: &str, w: usize) -> String {
    if name.width() <= w {
        return name.to_string();
    }
    let mut out = String::new();
    for c in name.chars() {
        if out.width() + c.width().unwrap_or(0) >= w {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

/// The tree: header, each finished phase on one line, the running phase
/// open with its children, the pending ones on one dim line. At most `max`
/// rows: finished phases fold into one line first, then children give way
/// ("+N more").
fn tree(run: &Run, view: &View, w: usize, max: usize, now: Duration, s: &Styles) -> Vec<String> {
    let fit = |segs: &Vec<Seg>| line(segs, w, s);
    let gauge = gauge(run, w);
    let (branch, last, pipe, blank) = s.tree_glyphs();
    let done: Vec<Vec<Seg>> = run
        .phases
        .iter()
        .filter(|p| p.state == PhaseState::Done)
        .map(|p| phase_row(p, now, s))
        .collect();
    let running: Option<Vec<Seg>> = run
        .phases
        .iter()
        .find(|p| p.state == PhaseState::Running)
        .map(|p| running_row(run, p, &gauge, now, s));
    let pending: Vec<&str> = run
        .phases
        .iter()
        .filter(|p| p.state == PhaseState::Pending)
        .map(|p| name(p.kind))
        .collect();
    let pending = (!pending.is_empty()).then(|| {
        let dot = g(s, "·", ".");
        vec![seg(
            Paint::Dim,
            format!("{dot} {}", pending.join(&format!(" {dot} "))),
        )]
    });
    let kids = children(run, &gauge, w, now, s);

    let fixed = 1 + usize::from(running.is_some()) + usize::from(pending.is_some());
    // Folded by the screen's size alone, so the shape does not flip as
    // children come and go: a dozen rows for them, or fold.
    let done_rows: Vec<Vec<Seg>> = if fixed + done.len() + 12 > max {
        done_summary(run, now, s).into_iter().collect()
    } else {
        done
    };
    let room = max.saturating_sub(fixed + done_rows.len());
    let kid_rows: Vec<Vec<Seg>> = if kids.len() > room {
        let keep = room.saturating_sub(1);
        let mut v: Vec<Vec<Seg>> = kids.iter().take(keep).cloned().collect();
        if room > 0 {
            let what = if run.current() == Some(PhaseKind::Download) {
                " in flight"
            } else {
                ""
            };
            v.push(vec![seg(
                Paint::Dim,
                format!("+{} more{what}", kids.len() - keep),
            )]);
        }
        v
    } else {
        kids
    };

    let mut rows = vec![fit(&header(run, view, now, s))];
    rows.extend(done_rows.iter().map(fit));
    if let Some(r) = &running {
        rows.push(fit(r));
        let nested = |k: &Vec<Seg>| k.first().is_some_and(|(_, t)| t.starts_with(NESTED));
        let last_branch = kid_rows.iter().rposition(|k| !nested(k));
        for (i, mut k) in kid_rows.into_iter().enumerate() {
            if nested(&k) {
                if let Some(first) = k.first_mut() {
                    first.1 = first.1.replacen(NESTED, "", 1);
                }
                let up = if last_branch.is_some_and(|l| i > l) {
                    blank
                } else {
                    pipe
                };
                k.insert(0, seg(Paint::Dim, format!("  {up}")));
            } else {
                let glyph = if Some(i) == last_branch { last } else { branch };
                k.insert(0, seg(Paint::Dim, format!("  {glyph}")));
            }
            rows.push(fit(&k));
        }
    }
    rows.extend(pending.iter().map(fit));
    rows.truncate(max.max(1));
    rows
}

fn live(run: &Run, view: &View, w: usize, height: usize, now: Duration, s: &Styles) -> Vec<String> {
    let fit = |segs: &Vec<Seg>| line(segs, w, s);
    let room = height.saturating_sub(1).max(1);
    match view.layout {
        Layout::Inline => {
            // All but two rows: every download in flight gets a row, and
            // a line of what came before stays in view.
            let h = height.saturating_sub(2).max(6).min(room);
            let lane = arcade_lane(run, view, w, s);
            let mut rows = tree(
                run,
                view,
                w,
                h.saturating_sub(usize::from(lane.is_some())).max(1),
                now,
                s,
            );
            rows.extend(lane);
            rows.truncate(h);
            rows
        }
        Layout::InlineGrow | Layout::Nom => {
            let lane = arcade_lane(run, view, w, s);
            let mut rows = tree(
                run,
                view,
                w,
                room.saturating_sub(usize::from(lane.is_some())).max(1),
                now,
                s,
            );
            rows.extend(lane);
            rows
        }
        Layout::Receipt => {
            let one = match run.current().and_then(|k| phase(run, k)) {
                Some(p) => {
                    let mut segs = phase_row(p, now, s);
                    if let Some(it) = item(run, now, s) {
                        segs.push(seg(Paint::Plain, "  "));
                        segs.extend(it);
                    }
                    segs
                }
                None => header(run, view, now, s),
            };
            let mut rows = log_tail(run, w, s);
            rows.push(fit(&one));
            rows
        }
    }
}

/// `old → new` with the shared front dimmed and the changed part green:
/// `1.24.4-1 → 1.25.3-1` shows `1.` dim on both sides, `25.3-1` green.
fn version_diff(old: &str, new: &str) -> (Vec<Seg>, Vec<Seg>) {
    let same = old
        .chars()
        .zip(new.chars())
        .take_while(|(a, b)| a == b)
        .count();
    // Back to the start of the component the first change is in.
    let cut = new
        .char_indices()
        .take(same)
        .filter(|(_, c)| matches!(c, '.' | '-' | ':' | '+'))
        .map(|(i, c)| i + c.len_utf8())
        .last()
        .unwrap_or(0);
    let (o1, o2) = old.split_at(cut.min(old.len()));
    let (n1, n2) = new.split_at(cut);
    (
        vec![seg(Paint::Dim, o1), seg(Paint::Plain, o2)],
        vec![seg(Paint::Dim, n1), seg(Paint::Ok, n2)],
    )
}

type Pkg = crate::update_view::model::Pkg;

/// Packages that share a name up to their last `-`, three or more in one
/// repo, as one entry: `linux-firmware-* (15)`, with the versions when they
/// all share them.
fn fold_families(list: &[&Pkg]) -> Vec<Pkg> {
    let key = |p: &Pkg| p.name.rfind('-').map(|i| p.name[..=i].to_string());
    let mut out: Vec<Pkg> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for p in list {
        let Some(k) = key(p) else {
            out.push((*p).clone());
            continue;
        };
        let members: Vec<&&Pkg> = list
            .iter()
            .filter(|q| key(q).as_ref() == Some(&k))
            .collect();
        if members.len() < 3 {
            out.push((*p).clone());
        } else if !seen.contains(&k) {
            let same = |f: &dyn Fn(&Pkg) -> &Option<String>| {
                let first = f(members[0]);
                members
                    .iter()
                    .all(|m| f(m) == first)
                    .then(|| first.clone())
                    .flatten()
            };
            out.push(Pkg {
                name: format!("{k}* ({})", members.len()),
                old: same(&|m| &m.old),
                new: same(&|m| &m.new),
                ..Pkg::default()
            });
            seen.push(k);
        }
    }
    out
}

/// How much of each package the plan shows: the richest form in which every
/// package fits wins.
#[derive(Clone, Copy, PartialEq)]
enum Detail {
    /// name, old → new, download size
    Full,
    /// name, old → new
    Versions,
    /// name, new version
    New,
    /// name only
    Names,
}

/// The plan at `[Y/n]`: one summary line, then the packages grouped by the
/// repo they come from, each group under its own heading and spread over as
/// many columns as the width allows (20 packages in 2 columns: 10 and 10;
/// in 3: 7, 7, 6), read down then across. Versions line up across groups.
/// When it does not fit in `max` rows, detail goes before packages do:
/// sizes, then old versions, then families fold (`linux-firmware-* (15)`),
/// then versions; past that, "+N more".
fn compact_plan(run: &Run, w: usize, max: usize, s: &Styles) -> Vec<String> {
    let ladder = [
        (Detail::Full, false),
        (Detail::Versions, false),
        (Detail::New, false),
        (Detail::New, true),
        (Detail::Names, true),
    ];
    let mut out = Vec::new();
    for (detail, fold) in ladder {
        let (rows, all) = plan_rows(run, w, max, detail, fold, s);
        out = rows;
        if all {
            break;
        }
    }
    out
}

/// The plan in one form; `true` when every package made it in.
fn plan_rows(
    run: &Run,
    w: usize,
    max: usize,
    detail: Detail,
    fold: bool,
    s: &Styles,
) -> (Vec<String>, bool) {
    let arrow = s.arrow();
    // Groups in the order pacman lists their first package.
    let mut groups: Vec<(&str, Vec<&Pkg>)> = Vec::new();
    for p in &run.packages {
        let repo = p.repo.as_deref().unwrap_or("packages");
        match groups.iter_mut().find(|g| g.0 == repo) {
            Some(g) => g.1.push(p),
            None => groups.push((repo, vec![p])),
        }
    }
    groups.sort_by(|a, b| a.0.cmp(b.0));
    let groups: Vec<(&str, usize, Vec<Pkg>)> = groups
        .into_iter()
        .map(|(repo, list)| {
            let n = list.len();
            let entries = if fold {
                fold_families(&list)
            } else {
                list.into_iter().cloned().collect()
            };
            (repo, n, entries)
        })
        .collect();
    let pk: Vec<&Pkg> = groups.iter().flat_map(|g| &g.2).collect();
    // Sized to nine in ten entries, not the longest: one long version
    // (luajit's) would cost every column; the few longer ones end in `…`.
    let width = |f: &dyn Fn(&Pkg) -> usize, cap: usize| {
        let mut v: Vec<usize> = pk.iter().map(|p| f(p)).collect();
        v.sort_unstable();
        v.get(v.len().saturating_sub(1) * 9 / 10)
            .copied()
            .unwrap_or(0)
            .min(cap)
    };
    let name_w = width(&|p| p.name.width(), 28);
    let old_w = if matches!(detail, Detail::Full | Detail::Versions) {
        width(&|p| p.old.as_deref().map_or(0, |v| v.width()), 20)
    } else {
        0
    };
    let new_w = if detail == Detail::Names {
        0
    } else {
        width(&|p| p.new.as_deref().map_or(0, |v| v.width()), 20)
    };
    // Sizes are short; the widest one sets the column so they line up.
    let size_w = if detail == Detail::Full {
        pk.iter()
            .map(|p| p.download.as_deref().map_or(0, |v| v.width()))
            .max()
            .unwrap_or(0)
    } else {
        0
    };
    let cell = name_w
        + if old_w > 0 { 1 + old_w + 3 } else { 0 }
        + if new_w > 0 && old_w == 0 { 1 } else { 0 }
        + new_w
        + if size_w > 0 { 2 + size_w } else { 0 };
    let gap = if detail == Detail::Names { 2 } else { 4 };
    let cols = ((w + gap) / (cell + gap)).max(1);

    let entry = |p: &Pkg| -> Vec<Seg> {
        let name = cut_plain(&p.name, name_w);
        let mut segs = vec![seg(Paint::Plain, format!("{name:<name_w$}"))];
        if detail == Detail::Names {
            return segs;
        }
        segs.push(seg(Paint::Plain, " "));
        let (old, new) = match (&p.old, &p.new) {
            (Some(o), Some(n)) => version_diff(&cut_plain(o, old_w), &cut_plain(n, new_w)),
            (None, Some(n)) => (vec![], vec![seg(Paint::Ok, cut_plain(n, new_w))]),
            _ => (vec![], vec![]),
        };
        let nw: usize = new.iter().map(|(_, t)| t.width()).sum();
        if old_w > 0 {
            let ow: usize = old.iter().map(|(_, t)| t.width()).sum();
            segs.push(seg(Paint::Plain, " ".repeat(old_w.saturating_sub(ow))));
            segs.extend(old);
            let a = if p.old.is_some() { arrow } else { " " };
            segs.push(seg(Paint::Dim, format!(" {a} ")));
        }
        segs.extend(new);
        segs.push(seg(Paint::Plain, " ".repeat(new_w.saturating_sub(nw))));
        if size_w > 0 {
            let d = p.download.as_deref().unwrap_or("");
            segs.push(seg(Paint::Dim, format!("  {d:>size_w$}")));
        }
        segs
    };

    let b = s.bullet();
    let mut head = vec![seg(Paint::Bold, format!("{} packages", run.packages.len()))];
    for (label, value) in &run.totals {
        let label = label
            .to_lowercase()
            .replace("total ", "")
            .replace(" size", "");
        head.push(seg(Paint::Dim, format!(" {b} ")));
        head.push(seg(Paint::Plain, format!("{value} {label}")));
    }
    let mut out = vec![line(&head, w, s)];
    let mut shown = 0;
    let mut left: Vec<String> = Vec::new();
    for (repo, n, list) in &groups {
        let rows = list.len().div_ceil(cols);
        // Room for this group, and a line to say what is left after it.
        if !left.is_empty() || out.len() + 1 + rows + 1 > max {
            left.push(format!("{repo} {n}"));
            continue;
        }
        out.push(line(
            &[
                seg(Paint::Bold, (*repo).to_string()),
                seg(Paint::Dim, format!(" ({n})")),
            ],
            w,
            s,
        ));
        for r in 0..rows {
            let mut segs = Vec::new();
            for c in 0..cols {
                let Some(p) = list.get(c * rows + r) else {
                    break;
                };
                if c > 0 {
                    segs.push(seg(Paint::Plain, " ".repeat(gap)));
                }
                segs.extend(entry(p));
            }
            out.push(line(&segs, w, s));
        }
        shown += n;
    }
    let all = shown == run.packages.len();
    if !all {
        let more = run.packages.len() - shown;
        out.push(line(
            &[seg(
                Paint::Dim,
                format!(
                    "+{more} more in {} {b} the full list: paclens transcript",
                    left.join(", ")
                ),
            )],
            w,
            s,
        ));
    }
    (out, all)
}

/// Lines from here on are the transaction's own output: after the plan's
/// prompt line settled, i.e. after the user answered.
fn transaction_from(run: &Run) -> Option<usize> {
    plan(run).and_then(|(_, _, p)| p).map(|p| p + 1)
}

/// Is line `at` hook output? Inside `:: Running pre-transaction hooks...`
/// up to `:: Processing package changes...`, or after `:: Running
/// post-transaction hooks...`. A package's own install text (scriptlet
/// messages such as "add yourself to the input group") is not: it is a
/// message for the user and stays in scrollback.
fn in_hooks(run: &Run, at: usize) -> bool {
    let mut hooks = false;
    for l in run.lines.iter().take(at + 1) {
        match classify_screen(l, false) {
            ScreenEvent::PreHooks | ScreenEvent::PostHooks => hooks = true,
            ScreenEvent::Processing => hooks = false,
            _ => {}
        }
    }
    hooks
}

/// How many lines of the transaction's output the live region shows.
const LOG_TAIL: usize = 5;

/// The last `n` unrecognised lines of the transaction (hook and scriptlet
/// chatter), oldest first. The full text stays in the transcript; a failure
/// shows its own context.
fn log_lines(run: &Run, n: usize) -> Vec<&String> {
    let Some(from) = transaction_from(run) else {
        return Vec::new();
    };
    // Only what the current hook or package printed: output belongs under
    // what produced it, and a new item starts with an empty tail.
    let item = run.lines.iter().rposition(|l| {
        matches!(
            classify_screen(l, false),
            ScreenEvent::Hook { .. }
                | ScreenEvent::Package { .. }
                | ScreenEvent::PreHooks
                | ScreenEvent::PostHooks
                | ScreenEvent::Processing
        )
    });
    let from = from.max(item.unwrap_or(0));
    // Unknown lines and the transaction's warnings (a warning shows here
    // while it is current, then in the receipt).
    let warnings = run
        .warning_at
        .iter()
        .filter_map(|&at| Some((at, run.lines.get(at)?)));
    let mut all: Vec<(usize, &String)> = run
        .unknown_at
        .iter()
        .copied()
        .zip(&run.unknown)
        .chain(warnings)
        .collect();
    all.sort_by_key(|&(at, _)| at);
    let mut tail: Vec<&String> = all
        .into_iter()
        .rev()
        .filter(|(at, _)| *at >= from && in_hooks(run, *at))
        .take(n)
        .map(|(_, l)| l)
        .collect();
    tail.reverse();
    tail
}

/// [`log_lines`] as dim region rows, for the one-line receipt layout.
fn log_tail(run: &Run, w: usize, s: &Styles) -> Vec<String> {
    let (_, _, pipe, _) = s.tree_glyphs();
    log_lines(run, LOG_TAIL)
        .into_iter()
        .map(|l| line(&[seg(Paint::Dim, format!("{pipe} {}", l.trim_end()))], w, s))
        .collect()
}

/// The plan as pacman printed it: from `Package (N)` to the last total
/// (`start..end` in `run.lines`), and where its prompt line settled, if it has.
fn plan(run: &Run) -> Option<(usize, usize, Option<usize>)> {
    let start = run
        .lines
        .iter()
        .rposition(|l| l.starts_with("Package (") || l.starts_with("Packages ("))?;
    let prompt = run.lines[start..]
        .iter()
        .position(|l| is_prompt(l))
        .map(|i| start + i);
    let mut end = prompt.unwrap_or(run.lines.len());
    while end > start && run.lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    Some((start, end, prompt))
}

/// One frame of the live view. `sent` says what earlier frames already put
/// in `above`; the returned `Delivered` covers this frame too, so the same
/// run drawn again with it gives an empty `above`.
///
/// `run.lines` is counted in `Delivered::lines` by `nom` (every settled
/// line) and by the others for the plan only, which they print once —
/// at the prompt when it does not fit, otherwise once it is answered.
#[allow(clippy::too_many_arguments)]
pub fn frame(
    run: &Run,
    view: &View,
    sent: Delivered,
    width: u16,
    height: u16,
    now: Duration,
    idle_prompt: Option<&[String]>,
    s: &Styles,
) -> (Frame, Delivered) {
    let w = usize::from(width);
    let height = usize::from(height);
    let mut above = Vec::new();
    let mut d = sent;
    let from = |v: &[String], i: usize| v.get(i..).unwrap_or_default().to_vec();
    // pacman's own lines as it coloured them, when colour is on.
    let shown: &[String] = if s.colored() && run.styled.len() == run.lines.len() {
        &run.styled
    } else {
        &run.lines
    };
    let plan = plan(run);
    // Unknown lines before line `stop` go up; with `print` false they are
    // passed over, because the plan already carried them.
    // `d.unknown` is a line position, not a count: the model may drop an
    // unknown line once a hook claims it, and a count would then skip one.
    let unknown = |above: &mut Vec<String>, d: &mut Delivered, stop: usize, print: bool| {
        let stop = stop.min(run.lines.len());
        if print {
            // Unknown lines and warnings, merged in screen order. The
            // transaction's unknown lines stay in the live tail instead.
            let quiet_from = transaction_from(run).unwrap_or(usize::MAX);
            // A warning from the transaction itself is in the live tail and
            // the receipt; up here it would be a third copy.
            let warnings = run
                .warning_at
                .iter()
                .filter(|&&at| at < quiet_from || !in_hooks(run, at))
                .filter_map(|&at| Some((at, shown.get(at)?)));
            let unknown = run.unknown_at.iter().zip(&run.unknown);
            let unknown = unknown
                .filter(|(at, _)| **at < quiet_from || !in_hooks(run, **at))
                .map(|(&at, l)| (at, shown.get(at).unwrap_or(l)));
            let mut new: Vec<(usize, &String)> = unknown
                .chain(warnings)
                .filter(|(at, _)| (d.unknown..stop).contains(at))
                .collect();
            new.sort_by_key(|&(at, _)| at);
            above.extend(new.into_iter().map(|(_, l)| l.clone()));
        }
        d.unknown = d.unknown.max(stop);
    };
    if view.layout == Layout::Nom {
        // Every settled line goes up as printed, unrecognised ones included.
        above.extend(from(shown, sent.lines));
        d.lines = run.lines.len();
        d.unknown = run.lines.len();
    } else {
        // Unknown lines inside a plan not yet sent wait: the plan goes up
        // whole, in place, so sending them now would print them twice.
        let stop = match plan {
            Some((a, _, p)) if p.is_none_or(|p| sent.lines <= p) => a.max(sent.lines),
            _ => usize::MAX,
        };
        unknown(&mut above, &mut d, stop, true);
    }

    // Once answered, the plan is gone: it was there to decide on, and the
    // region redraws over it. Nothing of it goes to scrollback.
    if idle_prompt.is_none()
        && let Some((_, _, Some(p))) = plan
        && d.lines <= p
    {
        d.lines = p + 1;
        // Lines pacman printed inside the plan that were not plan: they
        // go up now, since the plan that carried them is gone.
        unknown(&mut above, &mut d, usize::MAX, true);
    }

    if view.layout == Layout::Receipt {
        // One permanent line per phase as it completes; a phase that never
        // ran is passed over once a later one has started.
        while let Some(p) = run.phases.get(d.phases) {
            let later = run.phases[d.phases + 1..]
                .iter()
                .any(|q| q.state != PhaseState::Pending);
            match p.state {
                PhaseState::Done => above.push(phase_done(run, p, w, s)),
                PhaseState::Pending if later => {}
                _ => break,
            }
            d.phases += 1;
        }
    }

    let region = match idle_prompt {
        Some(rows) => {
            let prompt = rows.last().cloned().unwrap_or_default();
            match plan {
                // The plan, grouped by repo; it lives in the region only, so
                // answering wipes it.
                Some((_, _, None)) if is_prompt(&prompt) => {
                    let mut r = compact_plan(run, w, height.saturating_sub(2), s);
                    r.push(prompt);
                    r
                }
                // Verbatim: a cut prompt can lose its answer choices.
                _ => rows.to_vec(),
            }
        }
        None => live(run, view, w, height, now, s),
    };
    (Frame { above, region }, d)
}

/// `✓ download 38 · 702.91 MiB · 1m17s`
fn phase_done(run: &Run, p: &Phase, w: usize, s: &Styles) -> String {
    let size = match p.kind {
        PhaseKind::Download => download_size(run, s),
        _ => String::new(),
    };
    let time = took(p.started, p.ended, Duration::ZERO)
        .map(duration)
        .unwrap_or_default();
    let text = format!(" {} {}{size} {} {time}", name(p.kind), p.done, s.bullet());
    line(&[seg(Paint::Ok, s.check()), seg(Paint::Plain, text)], w, s)
}

/// The receipt on exit 0; the failure block otherwise.
pub fn closing(run: &Run, view: &View, width: u16, transcript: &str, s: &Styles) -> Vec<String> {
    let w = usize::from(width);
    let mut out = vec![line(&end_segs(run, view.motif, s), w, s)];
    if let Outcome::Failed(_) = run.outcome() {
        // pacman's errors verbatim, a little context, where the rest is.
        let errors = all_notes(run).filter(|n| matches!(n.kind, NoteKind::Error { .. }));
        out.extend(errors.map(|n| format!("error: {}", n.text)));
        let context: Vec<&String> = run
            .lines
            .iter()
            .rev()
            .filter(|l| !l.trim().is_empty() && !l.starts_with("error: "))
            .take(5)
            .collect();
        out.extend(context.into_iter().rev().map(|l| s.dim(l)));
        out.push(format!("transcript: {transcript}"));
        return out;
    }
    let arcade = view.motif == Motif::Arcade;
    let bullet = (Paint::Dim, s.bullet());
    // Something for the user to do: an amber `!`, ahead of the records.
    let act = (Paint::Warn, "!");
    let mark = |ghost: Ghost, todo: bool| {
        if arcade {
            (Paint::Ghost(ghost), g(s, "ᗣ", "G"))
        } else if todo {
            act
        } else {
            bullet
        }
    };
    let texts = |want: &dyn Fn(&NoteKind) -> bool| -> Vec<String> {
        run.needs_you()
            .into_iter()
            .filter(|n| want(&n.kind))
            .map(|n| n.text.clone())
            .collect()
    };
    let mut groups: Vec<((Paint, &str), String)> = Vec::new();
    if let Some(t) = texts(&|k| *k == NoteKind::Reboot).first() {
        let pellet = if arcade {
            (Paint::Bold, g(s, "●", "o"))
        } else {
            act
        };
        // The hook's own reason, when it gives one.
        let why = t
            .split_once("due to ")
            .map(|(_, r)| format!(", due to {}", r.trim_end_matches('.')))
            .unwrap_or_default();
        groups.push((pellet, format!("reboot recommended{why}")));
    }
    let rebuild: Vec<String> = texts(&|k| *k == NoteKind::Rebuild)
        .iter()
        .filter_map(|t| t.split_whitespace().last().map(str::to_string))
        .collect();
    if !rebuild.is_empty() {
        groups.push((act, format!("rebuild: {}", rebuild.join(", "))));
    }
    // One line each: a warning is usually something to do (daemon-reload).
    // "local is newer than repo" was shown before the plan and asks nothing.
    let to_do = |w: &String| !w.contains(" is newer than ");
    for w in texts(&|k| *k == NoteKind::Warning)
        .into_iter()
        .filter(to_do)
    {
        groups.push((mark(Ghost::Clyde, true), action(&w)));
    }
    let pacnew = texts(&|k| *k == NoteKind::Pacnew);
    if !pacnew.is_empty() {
        groups.push((
            mark(Ghost::Pinky, true),
            format!(".pacnew: {}", pacnew.join(", ")),
        ));
    }
    let opt = texts(&|k| *k == NoteKind::Optdepends);
    if !opt.is_empty() {
        groups.push((
            mark(Ghost::Inky, false),
            format!("optional: {}", opt.join("; ")),
        ));
    }
    let retried = texts(&|k| *k == NoteKind::Error { recovered: true });
    if !retried.is_empty() {
        let list: Vec<String> = retried.iter().map(|t| retry(run, t, s.bullet())).collect();
        groups.push((
            mark(Ghost::Blinky, false),
            format!("retried {}", list.join("; ")),
        ));
    }
    // Snapshots by config, first → last: `snap-pac root 1001 → 1002`.
    let mut snaps: Vec<(&str, u64, u64)> = Vec::new();
    for n in run.needs_you() {
        if let NoteKind::Snapshot { config, number } = &n.kind {
            match snaps.iter_mut().find(|x| x.0 == config) {
                Some(x) => x.2 = *number,
                None => snaps.push((config, *number, *number)),
            }
        }
    }
    if !snaps.is_empty() {
        let list: Vec<String> = snaps
            .iter()
            .map(|(c, a, b)| {
                if a == b {
                    format!("{c} {a}")
                } else {
                    format!("{c} {a} {} {b}", s.arrow())
                }
            })
            .collect();
        groups.push((bullet, format!("snap-pac {}", list.join(", "))));
    }
    let n = run.unknown.len();
    if n > 0 {
        let lines = if n == 1 { "line" } else { "lines" };
        let b = s.bullet();
        groups.push((
            bullet,
            format!("{n} more {lines} of output {b} paclens transcript"),
        ));
    }
    // Not cut to the width: the receipt stays on screen, and a cut line loses
    // the part that says what to do. The terminal wraps it.
    for ((p, m), text) in groups {
        out.push(format!(
            "{}{}",
            paint(p, m, s),
            paint(Paint::Plain, &format!(" {text}"), s)
        ));
    }
    out
}

/// A warning as the thing to do: `systemctl daemon-reload (warp-svc.service)`
/// when it names a command to run and a unit, else the warning whole.
fn action(w: &str) -> String {
    let cmd = w
        .split_once("Run '")
        .and_then(|(_, r)| r.split_once('\''))
        .map(|(c, _)| c);
    let unit = w.split_whitespace().find(|t| {
        [".service", ".socket", ".timer", ".mount", ".path"]
            .iter()
            .any(|e| t.ends_with(e))
    });
    match (cmd, unit) {
        (Some(c), Some(u)) => format!("{c} ({u} changed on disk)"),
        _ => format!("warning: {w}"),
    }
}

/// `luajit · archlinux.cachyos.org 404` from pacman's `failed retrieving
/// file 'luajit-2.1…pkg.tar.zst' from archlinux.cachyos.org : The requested
/// URL returned error: 404`; the line whole when it does not read so.
fn retry(run: &Run, t: &str, b: &str) -> String {
    let file = t
        .split_once('\'')
        .and_then(|(_, r)| r.split_once('\''))
        .map(|(f, _)| f);
    let pkg = file.and_then(|f| {
        run.packages
            .iter()
            .filter(|p| f.starts_with(&format!("{}-", p.name)))
            .max_by_key(|p| p.name.len())
    });
    let host = t
        .split_once(" from ")
        .and_then(|(_, r)| r.split_whitespace().next());
    let code = t.rsplit_once("error: ").map(|(_, c)| c.trim());
    match (pkg, host, code) {
        (Some(p), Some(h), Some(c)) => format!("{} {b} {h} {c}", p.name),
        _ => t.to_string(),
    }
}

fn end_segs(run: &Run, motif: Motif, s: &Styles) -> Vec<Seg> {
    let arcade = motif == Motif::Arcade;
    let ok = if arcade {
        (Paint::Warn, g(s, "ᗧ··", "C.."))
    } else {
        (Paint::Ok, s.check())
    };
    let fail = if arcade {
        (Paint::Ghost(Ghost::Blinky), g(s, "ᗣ", "G"))
    } else {
        (Paint::Bad, s.cross())
    };
    let took = duration(run.ended.unwrap_or_default());
    let eaten = run.packages.iter().filter(|p| p.done).count();
    let b = s.bullet();
    let ((p, mark), text) = match run.outcome() {
        Outcome::Succeeded => {
            let got = run
                .totals
                .iter()
                .find(|t| t.0 == "Total Download Size")
                .map(|t| format!(" {b} {} downloaded", size_pair(&t.1, None)))
                .unwrap_or_default();
            (ok, format!(" {eaten} eaten in {took}{got}"))
        }
        Outcome::NothingChanged => {
            let mark = if arcade {
                (Paint::Warn, g(s, "ᗧ", "C"))
            } else {
                ok
            };
            (mark, format!(" nothing to do {b} clean board"))
        }
        Outcome::Unknown => (
            (Paint::Warn, "?"),
            format!(" pacman exit 0 {b} result unknown (no pacman.log entry)"),
        ),
        Outcome::Failed(Some(n)) => (fail, format!(" pacman exit {n}")),
        Outcome::Failed(None) => (fail, " pacman stopped by a signal".to_string()),
        Outcome::Running => (
            (Paint::Dim, g(s, "…", "...")),
            " pacman running".to_string(),
        ),
    };
    vec![seg(p, mark), seg(Paint::Plain, text)]
}

/// `ᗧ·· 38 eaten in 2m04s` and its clean-board, unknown and failure variants.
pub fn end_line(run: &Run, motif: Motif, s: &Styles) -> String {
    line(&end_segs(run, motif, s), usize::MAX, s)
}

/// pacman's own progress bar, `cells` wide — pacman 7 `callback.c`
/// `fill_progress`. `eaten = pct * cells / 100` (integer division).
///
/// No candy: `#` for each eaten cell, `-` for the rest.
/// Candy (`ILoveCandy`): cell `j` (0 = left) is `-` while `j < eaten`; the
/// head sits at `j == eaten` (none at 100%); past it, counting `i = cells - j`
/// down from the right as pacman does, `o` where `i % 3 == 0`, else a space.
/// pacman flips the mouth each time the head moves; that needs state across
/// frames, so here the mouth follows the percent: `C` even, `c` odd. The
/// head is bold yellow, as in pacman; the track is dim, and the cell right
/// after the head is never a pellet.
pub fn bar(pct: u8, cells: usize, candy: bool, s: &Styles) -> String {
    let pct = usize::from(pct.min(100));
    let eaten = pct * cells / 100;
    if !candy {
        return "#".repeat(eaten) + &s.dim(&"-".repeat(cells - eaten));
    }
    // The track steps back so the mouths stand out down a column of bars.
    let mut rest = String::new();
    for j in eaten + 1..cells {
        // No pellet right at the mouth: `C o`, never `Co`.
        let pellet = j > eaten + 1 && (cells - j).is_multiple_of(3);
        rest.push(if pellet { 'o' } else { ' ' });
    }
    let head = if eaten < cells {
        s.warn(if pct.is_multiple_of(2) { "C" } else { "c" })
    } else {
        String::new()
    };
    s.dim(&"-".repeat(eaten)) + &head + &s.dim(&rest)
}

/// An uncommented `ILoveCandy` line in pacman.conf.
pub fn candy_enabled(pacman_conf: &str) -> bool {
    pacman_conf
        .lines()
        .any(|l| l.split('#').next().unwrap_or_default().trim() == "ILoveCandy")
}

/// `12s`, `2m04s`, `1h02m`.
pub fn duration(d: Duration) -> String {
    let t = d.as_secs();
    match t {
        0..60 => format!("{t}s"),
        60..3600 => format!("{}m{:02}s", t / 60, t % 60),
        _ => format!("{}h{:02}m", t / 3600, t % 3600 / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ColorTheme;
    use crate::update_view::model::fixture_run;
    use crate::update_view::screen::fixture_lines;
    use std::collections::HashMap;
    use unicode_width::UnicodeWidthStr;

    const Z: Duration = Duration::ZERO;
    const ESC: char = '\u{1b}';

    fn plain() -> Styles {
        Styles::resolve(true, ColorTheme::Dark, false)
    }
    /// Unicode glyphs, no colour.
    fn uni() -> Styles {
        Styles::resolve(false, ColorTheme::Dark, false)
    }
    fn colour() -> Styles {
        Styles::resolve(false, ColorTheme::Dark, true)
    }

    fn empty() -> Run {
        Run::new("pacman -Syu", HashMap::new(), HashMap::new())
    }

    /// The fixture's first `n` settled lines, not finished.
    fn partial(n: usize) -> Run {
        let mut r = empty();
        for l in fixture_lines().iter().take(n) {
            r.apply_line(l, Z);
        }
        r
    }

    fn view(layout: Layout, motif: Motif) -> View {
        View {
            layout,
            motif,
            candy: true,
        }
    }

    fn draw(r: &Run, v: &View, h: u16, s: &Styles) -> Frame {
        frame(r, v, Delivered::default(), 120, h, secs(120), None, s).0
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn strip(s: &str) -> String {
        let mut out = String::new();
        let mut esc = false;
        for c in s.chars() {
            match (esc, c) {
                (false, ESC) => esc = true,
                (true, c) if c.is_ascii_alphabetic() => esc = false,
                (true, _) => {}
                (false, c) => out.push(c),
            }
        }
        out
    }

    fn all_views() -> Vec<View> {
        Layout::ALL
            .iter()
            .flat_map(|&l| Motif::ALL.iter().map(move |&m| view(l, m)))
            .collect()
    }

    #[test]
    fn every_layout_and_motif_renders_the_fixture() {
        let r = fixture_run();
        for v in all_views() {
            let f = draw(&r, &v, 40, &plain());
            assert!(!f.region.is_empty(), "{:?}/{:?}", v.layout, v.motif);
            let c = closing(&r, &v, 120, "/t", &plain());
            assert!(c[0].contains("38 eaten"), "{c:?}");
            assert!(c.len() <= 6, "{c:?}");
        }
    }

    #[test]
    fn receipt_keeps_actionable_warnings_whole() {
        let mut r = fixture_run();
        let w = "Warning: The unit file of warp-svc.service changed on disk. Run 'systemctl daemon-reload' to reload units.";
        r.apply_line(w, Z);
        let c = closing(&r, &view(Layout::Inline, Motif::Subtle), 40, "/t", &plain());
        // The thing to do, with what it is about.
        assert!(
            c.iter()
                .any(|l| l == "! systemctl daemon-reload (warp-svc.service changed on disk)"),
            "{c:?}"
        );
        // A warning that names no command stays whole.
        let mut r = fixture_run();
        let odd = "Warning: something odd happened and nothing says what to run about it.";
        r.apply_line(odd, Z);
        let c = closing(&r, &view(Layout::Inline, Motif::Subtle), 40, "/t", &plain());
        assert!(
            c.iter()
                .any(|l| l.ends_with("nothing says what to run about it.")),
            "{c:?}"
        );
        assert!(!c.iter().any(|l| l.contains("is newer than")), "{c:?}");
    }

    #[test]
    fn coloured_lines_go_up_in_colour_only_when_colour_is_on() {
        let mut r = empty();
        r.apply_line_styled("odd line", "\u{1b}[0;1modd line\u{1b}[0m", Z);
        let v = view(Layout::Inline, Motif::Subtle);
        let (f, _) = frame(&r, &v, Delivered::default(), 120, 40, Z, None, &colour());
        assert_eq!(f.above, ["\u{1b}[0;1modd line\u{1b}[0m"]);
        let (f, _) = frame(&r, &v, Delivered::default(), 120, 40, Z, None, &plain());
        assert_eq!(f.above, ["odd line"]);
    }

    #[test]
    fn warnings_go_up_in_screen_order() {
        let mut r = empty();
        r.apply_line("warning: foo: local (2-1) is newer than extra (1-1)", Z);
        let (f, _) = frame(
            &r,
            &view(Layout::Inline, Motif::Subtle),
            Delivered::default(),
            120,
            40,
            Z,
            None,
            &plain(),
        );
        assert_eq!(
            f.above,
            ["warning: foo: local (2-1) is newer than extra (1-1)"]
        );
    }

    #[test]
    fn receipt_names_reboot_rebuild_snapshot_and_mirror() {
        let c = closing(
            &fixture_run(),
            &view(Layout::Inline, Motif::Subtle),
            120,
            "/t",
            &uni(),
        )
        .join("\n");
        for want in ["reboot", "wps-office", "root 1001 → 1002", "luajit"] {
            assert!(c.contains(want), "{want} missing in\n{c}");
        }
        assert!(!c.contains("not recognised"), "{c}");
    }

    #[test]
    fn receipt_counts_unrecognised_lines() {
        let mut r = fixture_run();
        r.unknown = vec!["odd".into(), "odder".into()];
        let c = closing(&r, &view(Layout::Inline, Motif::Subtle), 120, "/t", &uni());
        assert!(
            c.iter()
                .any(|l| l.ends_with(" 2 more lines of output · paclens transcript")),
            "{c:?}"
        );
    }

    #[test]
    fn failure_block_pins_errors_and_transcript() {
        let mut r = partial(151);
        r.apply_line("error: failed to commit transaction (conflicting files)", Z);
        r.finish(Some(1), secs(64));
        let c = closing(&r, &view(Layout::Inline, Motif::Subtle), 120, "/t", &uni());
        assert_eq!(c[0], "✗ pacman exit 1");
        assert!(
            c.iter()
                .any(|l| l == "error: failed to commit transaction (conflicting files)")
        );
        // The mirror error never recovered on a failed run: pinned too.
        assert!(c.iter().any(|l| l.contains("failed retrieving file")));
        assert!(c.iter().any(|l| l.contains("( 3/38) upgrading zstd")));
        assert_eq!(c.last().map(String::as_str), Some("transcript: /t"));
    }

    #[test]
    fn signal_failure_says_so() {
        let mut r = empty();
        r.finish(None, Z);
        let c = closing(&r, &view(Layout::Nom, Motif::Arcade), 80, "/t", &uni());
        assert!(c[0].contains("pacman stopped by a signal"), "{c:?}");
    }

    #[test]
    fn inline_leaves_two_rows_and_no_blank_rows() {
        let mid = partial(161);
        for h in [40u16, 15, 6] {
            let f = draw(&mid, &view(Layout::Inline, Motif::Subtle), h, &plain());
            let cap = (usize::from(h) - 2).max(6).min(usize::from(h) - 1);
            assert!(f.region.len() <= cap, "h={h}: {:?}", f.region);
            assert!(
                f.region.iter().all(|l| !l.is_empty()),
                "h={h}: {:?}",
                f.region
            );
        }
    }

    #[test]
    fn inline_shows_header_phases_item_and_bar() {
        let f = draw(
            &partial(161),
            &view(Layout::Inline, Motif::Subtle),
            40,
            &uni(),
        );
        let t = f.region.join("\n");
        assert!(
            f.region[0].starts_with("pacman -Syu · packages · 2m00s"),
            "{t}"
        );
        assert!(t.contains("download"), "{t}");
        assert!(t.contains("13/38"), "{t}");
        assert!(t.contains("upgrading openssh"), "{t}");
        assert!(t.contains("] 34%") || t.contains("]  34%"), "{t}");
    }

    #[test]
    fn hook_row_shows_elapsed_and_median() {
        use crate::update_view::medians::Median;
        let medians = HashMap::from([("60-x.hook".to_string(), Median { secs: 23, runs: 51 })]);
        let mut r = Run::new("pacman -Syu", medians, HashMap::new());
        r.apply_log(
            crate::update_view::log_line::classify_log(
                "[2026-10-16T11:37:21+0530] [ALPM] transaction completed",
            )
            .map(|x| x.1)
            .unwrap_or(crate::update_view::log_line::LogEvent::Other),
            secs(1),
        );
        r.apply_log(
            crate::update_view::log_line::classify_log(
                "[2026-10-16T11:37:21+0530] [ALPM] running '60-x.hook'...",
            )
            .map(|x| x.1)
            .unwrap_or(crate::update_view::log_line::LogEvent::Other),
            secs(1),
        );
        r.apply_line(":: Running post-transaction hooks...", secs(1));
        r.apply_line("(1/2) Doing x...", secs(3));
        let f = frame(
            &r,
            &view(Layout::Inline, Motif::Subtle),
            Delivered::default(),
            120,
            40,
            secs(15),
            None,
            &uni(),
        )
        .0;
        assert!(
            f.region
                .iter()
                .any(|l| l.contains("Doing x...") && l.ends_with("12s · usually 23s")),
            "{:?}",
            f.region
        );
    }

    #[test]
    fn frame_lines_never_exceed_width() {
        let runs = [partial(30), partial(120), partial(161), fixture_run()];
        for s in [plain(), colour()] {
            for r in &runs {
                for v in all_views() {
                    let f = frame(r, &v, Delivered::default(), 40, 30, secs(99), None, &s).0;
                    for l in &f.region {
                        let bare = strip(l);
                        assert!(bare.width() <= 40, "{:?}: {bare:?}", v.layout);
                        // The last escape in a line is a reset (all, or colour).
                        if let Some(i) = l.rfind(ESC) {
                            let tail = &l[i..];
                            assert!(
                                ["\u{1b}[0m", "\u{1b}[m", "\u{1b}[39m"]
                                    .iter()
                                    .any(|r| tail.starts_with(r)),
                                "{l:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn colour_reaches_the_region() {
        let f = draw(
            &partial(161),
            &view(Layout::Inline, Motif::Subtle),
            40,
            &colour(),
        );
        assert!(f.region.iter().any(|l| l.contains(ESC)));
    }

    #[test]
    fn permanent_lines_are_delivered_once() {
        let mut r = partial(120);
        for v in all_views() {
            let (f1, d1) = frame(&r, &v, Delivered::default(), 120, 40, Z, None, &plain());
            let (f2, d2) = frame(&r, &v, d1, 120, 40, Z, None, &plain());
            assert!(f2.above.is_empty(), "{:?}: {:?}", v.layout, f2.above);
            assert_eq!(d1, d2);
            let _ = f1;
        }
        let v = view(Layout::Inline, Motif::Subtle);
        let (_, d) = frame(&r, &v, Delivered::default(), 120, 40, Z, None, &plain());
        r.apply_line(":: Running post-transaction hooks...", Z);
        for l in ["odd one", "odd two", "odd three"] {
            r.apply_line(l, Z);
        }
        // Hook chatter: the live tail, never scrollback.
        let (f, _) = frame(&r, &v, d, 120, 40, Z, None, &plain());
        assert!(f.above.is_empty(), "{:?}", f.above);
        assert!(
            f.region.iter().any(|l| l.ends_with("odd three")),
            "{:?}",
            f.region
        );
    }

    #[test]
    fn nom_prints_settled_lines_once() {
        let r = partial(12);
        let v = view(Layout::Nom, Motif::Subtle);
        let (f, d) = frame(&r, &v, Delivered::default(), 120, 40, Z, None, &plain());
        assert_eq!(f.above, fixture_lines()[..12]);
        assert_eq!(d.lines, 12);
    }

    #[test]
    fn receipt_layout_prints_each_done_phase_once() {
        let r = fixture_run();
        let v = view(Layout::Receipt, Motif::Subtle);
        let (f, d) = frame(&r, &v, Delivered::default(), 120, 40, Z, None, &uni());
        assert_eq!(f.region.len(), 1);
        assert!(
            f.above
                .contains(&"✓ download 38 · 702.91 MiB · 0s".to_string()),
            "{:?}",
            f.above
        );
        assert_eq!(d.phases, 7);
        let mid = partial(120);
        let (f, d) = frame(&mid, &v, Delivered::default(), 120, 40, Z, None, &uni());
        assert!(
            f.above.iter().any(|l| l.starts_with("✓ sync 5")),
            "{:?}",
            f.above
        );
        assert_eq!(d.phases, 2);
    }

    #[test]
    fn whole_plan_shown_at_prompt() {
        let lines = fixture_lines();
        let r = partial(58);
        let prompt = vec![lines[58].clone()];
        let v = view(Layout::Inline, Motif::Subtle);
        let (f, d) = frame(
            &r,
            &v,
            Delivered::default(),
            120,
            49,
            Z,
            Some(&prompt),
            &plain(),
        );
        assert!(f.region[0].starts_with("38 packages"), "{:?}", f.region);
        for repo in ["cachyos (", "core (", "extra (", "multilib ("] {
            assert!(
                f.region.iter().any(|l| l.starts_with(repo)),
                "no {repo}: {:?}",
                f.region
            );
        }
        assert!(
            f.region
                .iter()
                .any(|l| l.contains("zed") && l.contains("1.24.2-1"))
        );
        assert_eq!(f.region.last(), Some(&lines[58]));
        // Lives in the region only.
        assert!(f.above.iter().all(|l| !l.contains("zed")), "{:?}", f.above);
        let (f, _) = frame(&r, &v, d, 120, 49, Z, Some(&prompt), &plain());
        assert!(f.above.is_empty());
        // Too short for all groups: the rest is counted, not dropped silently.
        let (f, _) = frame(
            &r,
            &v,
            Delivered::default(),
            120,
            12,
            Z,
            Some(&prompt),
            &plain(),
        );
        assert!(
            f.region.iter().any(|l| l.contains(" more")),
            "{:?}",
            f.region
        );
    }

    #[test]
    fn plan_group_is_split_evenly_over_the_columns() {
        let mut r = empty();
        for i in 0..20 {
            r.packages.push(crate::update_view::model::Pkg {
                name: format!("pkg{i:02}"),
                repo: Some("cachyos".into()),
                old: Some("1.0-1".into()),
                new: Some("1.1-1".into()),
                ..Default::default()
            });
        }
        let rows_under_heading = |w: usize| {
            let out = compact_plan(&r, w, 40, &plain());
            out.iter().skip(2).count()
        };
        let cell = 5 + 1 + 5 + 3 + 5;
        assert_eq!(rows_under_heading(cell), 20);
        assert_eq!(rows_under_heading(2 * cell + 4), 10);
        assert_eq!(rows_under_heading(3 * cell + 8), 7);
    }

    #[test]
    fn plan_drops_detail_before_packages() {
        let mut r = empty();
        for i in 0..12 {
            r.packages.push(crate::update_view::model::Pkg {
                name: format!("pkg{i:02}"),
                repo: Some("core".into()),
                old: Some("1.0-1".into()),
                new: Some("1.1-1".into()),
                download: Some("1.00 MiB".into()),
                ..Default::default()
            });
        }
        // 42 columns: one full entry (29 wide) per row would need 14 rows;
        // without sizes two (19 wide) fit side by side.
        let out = compact_plan(&r, 42, 9, &plain());
        let t = out.join("\n");
        assert!(!t.contains("more"), "{t}");
        assert!(!t.contains("MiB"), "{t}");
        assert!(t.contains("1.0-1 -> 1.1-1"), "{t}");
        // Roomy: everything, sizes too.
        let t = compact_plan(&r, 40, 40, &plain()).join("\n");
        assert!(t.contains("1.00 MiB"), "{t}");
    }

    #[test]
    fn families_fold_into_one_entry() {
        let pkgs: Vec<crate::update_view::model::Pkg> = ["fw-a", "fw-b", "fw-c", "zed", "x-1"]
            .iter()
            .map(|n| crate::update_view::model::Pkg {
                name: (*n).into(),
                new: Some("2-1".into()),
                old: Some(if *n == "fw-c" { "0-1" } else { "1-1" }.into()),
                ..Default::default()
            })
            .collect();
        let refs: Vec<&crate::update_view::model::Pkg> = pkgs.iter().collect();
        let out = fold_families(&refs);
        let names: Vec<&str> = out.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["fw-* (3)", "zed", "x-1"]);
        // Versions only when every member shares them.
        assert_eq!(out[0].new.as_deref(), Some("2-1"));
        assert_eq!(out[0].old, None);
    }

    #[test]
    fn plan_is_wiped_after_the_answer() {
        let lines = fixture_lines();
        let r = partial(60);
        let (f, _) = frame(
            &r,
            &view(Layout::Inline, Motif::Subtle),
            Delivered::default(),
            120,
            49,
            Z,
            None,
            &plain(),
        );
        assert!(
            !f.above.contains(&lines[15]) && !f.above.contains(&lines[58]),
            "{:?}",
            f.above
        );
    }

    #[test]
    fn prompt_shows_pacman_screen_tail() {
        let r = partial(1);
        let rows = vec![
            "…".to_string(),
            ":: Proceed with installation? [Y/n]".to_string(),
        ];
        let (f, _) = frame(
            &r,
            &view(Layout::Receipt, Motif::Arcade),
            Delivered::default(),
            120,
            40,
            Z,
            Some(&rows),
            &plain(),
        );
        assert_eq!(f.region, rows);
    }

    #[test]
    fn unknown_line_claimed_by_a_late_hook_does_not_break_delivery() {
        let log = |r: &mut Run, l: &str| {
            if let Some((_, ev)) = crate::update_view::log_line::classify_log(l) {
                r.apply_log(ev, Z);
            }
        };
        let hook = "[2026-10-16T11:37:25+0530] [ALPM] running 'rebuild-detector.hook'...";
        let v = view(Layout::Inline, Motif::Subtle);
        for frame_between in [true, false] {
            let mut r = empty();
            log(
                &mut r,
                "[2026-10-16T11:37:21+0530] [ALPM] transaction completed",
            );
            for l in [
                ":: Running post-transaction hooks...",
                "(1/1) Check foreign packages",
                "foreign foo",
            ] {
                r.apply_line(l, Z);
            }
            let mut d = Delivered::default();
            if frame_between {
                let (f, sent) = frame(&r, &v, d, 120, 40, Z, None, &plain());
                assert!(f.above.contains(&"foreign foo".to_string()));
                d = sent;
            }
            log(&mut r, hook);
            let (f, _) = frame(&r, &v, d, 120, 40, Z, None, &plain());
            assert!(
                !f.above.contains(&"foreign foo".to_string()),
                "{:?}",
                f.above
            );
        }
    }

    #[test]
    fn prompt_with_echoed_answer_is_a_prompt() {
        for l in [
            ":: Proceed with installation? [Y/n]",
            ":: Proceed with installation? [Y/n] y",
            ":: Proceed with installation? [Y/n] n",
            ":: Remove foo? [y/N] yes",
        ] {
            assert!(is_prompt(l), "{l}");
        }
        assert!(!is_prompt("[Y/n] is what pacman asks, said the wiki"));
    }

    #[test]
    fn unknown_line_after_an_answered_prompt_goes_to_the_live_tail() {
        let lines = fixture_lines();
        let mut r = partial(58);
        r.apply_line(&format!("{} y", lines[58]), Z);
        r.apply_line(":: Running post-transaction hooks...", Z);
        r.apply_line("custom hook notice", Z);
        let v = view(Layout::Inline, Motif::Subtle);
        let (f, _) = frame(&r, &v, Delivered::default(), 120, 49, Z, None, &plain());
        assert!(
            !f.above.contains(&"custom hook notice".to_string()),
            "{:?}",
            f.above
        );
        assert!(
            f.region.iter().any(|l| l.ends_with("custom hook notice")),
            "{:?}",
            f.region
        );
    }

    #[test]
    fn hook_output_stays_with_its_own_hook() {
        let lines = fixture_lines();
        let mut r = partial(60);
        for l in [
            ":: Running post-transaction hooks...",
            "(1/2) Updating linux initcpios...",
            "==> Building image",
            "(2/2) Updating the MIME type database...",
        ] {
            r.apply_line(l, Z);
        }
        let _ = lines;
        let f = draw(&r, &view(Layout::Inline, Motif::Subtle), 40, &plain());
        assert!(
            !f.region.iter().any(|l| l.contains("Building image")),
            "{:?}",
            f.region
        );
        r.apply_line("mime output", Z);
        let f = draw(&r, &view(Layout::Inline, Motif::Subtle), 40, &plain());
        assert!(
            f.region.iter().any(|l| l.contains("mime output")),
            "{:?}",
            f.region
        );
    }

    #[test]
    fn narrow_terminal_download_frame_does_not_panic() {
        let mut r = partial(120);
        r.in_flight.push(crate::update_view::model::InFlight {
            name: "zed".into(),
            pct: 40,
            detail: "1.0 MiB 1.0 MiB/s 00:10".into(),
            queued: false,
            stalled: false,
        });
        for w in [1u16, 10, 23, 24] {
            let _ = frame(
                &r,
                &view(Layout::Inline, Motif::Subtle),
                Delivered::default(),
                w,
                40,
                Z,
                None,
                &plain(),
            );
        }
    }

    #[test]
    fn download_rows_keep_their_last_column() {
        let mut r = partial(120);
        r.in_flight.push(crate::update_view::model::InFlight {
            name: "zed".into(),
            pct: 40,
            detail: "1055.4 MiB 1503 KiB/s 00:02:51".into(),
            queued: false,
            stalled: false,
        });
        // Colour too: the bar's escape codes must not eat the line's width.
        for st in [plain(), colour()] {
            for w in [80u16, 120, 154, 181] {
                let v = view(Layout::Inline, Motif::Subtle);
                let f = frame(&r, &v, Delivered::default(), w, 40, Z, None, &st).0;
                let row = f.region.iter().find(|l| l.contains("zed")).unwrap();
                let row = strip(row);
                // A narrow screen drops the numbers whole rather than cut them.
                let last = if w >= 120 { "00:02:51" } else { "%" };
                assert!(row.ends_with(last), "w={w}: {row:?}");
                assert!(row.width() < usize::from(w), "w={w}");
            }
        }
    }

    #[test]
    fn transaction_warning_shows_live_before_the_end() {
        let lines = fixture_lines();
        let mut r = partial(60);
        r.apply_line(":: Processing package changes...", Z);
        r.apply_line("(1/38) upgrading foo", Z);
        r.apply_line(
            "Warning: The unit file of x.service changed on disk. Run 'systemctl daemon-reload'.",
            Z,
        );
        let _ = lines;
        // Outside the hooks a warning is a message for the user: it goes up
        // at once and stays.
        let f = draw(&r, &view(Layout::Inline, Motif::Subtle), 40, &plain());
        assert!(
            f.above.iter().any(|l| l.contains("daemon-reload")),
            "{:?}",
            f.above
        );
    }

    #[test]
    fn package_install_message_stays_in_scrollback() {
        let lines = fixture_lines();
        let mut r = partial(58);
        r.apply_line(&format!("{} y", lines[58]), Z);
        r.apply_line(":: Processing package changes...", Z);
        r.apply_line("(1/38) upgrading ydotool", Z);
        r.apply_line("Make sure your user is in the `input` group", Z);
        r.apply_line("(2/38) upgrading zed", Z);
        let v = view(Layout::Inline, Motif::Subtle);
        let (f, _) = frame(&r, &v, Delivered::default(), 120, 49, Z, None, &plain());
        assert!(
            f.above.iter().any(|l| l.contains("`input` group")),
            "{:?}",
            f.above
        );
    }

    #[test]
    fn long_idle_prompt_is_kept_verbatim() {
        let r = partial(1);
        let rows = vec![":: Replace foo with extra/bar? [Y/n]".to_string()];
        let (f, _) = frame(
            &r,
            &view(Layout::Inline, Motif::Subtle),
            Delivered::default(),
            12,
            40,
            Z,
            Some(&rows),
            &plain(),
        );
        assert_eq!(f.region, rows);
    }

    #[test]
    fn unknown_line_in_plan_goes_up_once() {
        let lines = fixture_lines();
        for height in [49, 20] {
            let mut r = partial(13);
            r.apply_line("before plan", Z);
            r.apply_line(&lines[13], Z);
            r.apply_line("odd notice", Z);
            let v = view(Layout::Inline, Motif::Subtle);
            let (f, d) = frame(&r, &v, Delivered::default(), 120, height, Z, None, &plain());
            let mut above = f.above;
            for l in &lines[14..59] {
                r.apply_line(l, Z);
            }
            let prompt = vec![lines[58].clone()];
            let (f, d) = frame(&r, &v, d, 120, height, Z, Some(&prompt), &plain());
            above.extend(f.above);
            r.apply_line(&lines[59], Z);
            r.apply_line(":: Running post-transaction hooks...", Z);
            r.apply_line("after prompt", Z);
            let (f, _) = frame(&r, &v, d, 120, height, Z, None, &plain());
            above.extend(f.above);
            for t in ["before plan", "odd notice"] {
                let n = above.iter().filter(|l| *l == t).count();
                assert_eq!(n, 1, "height {height} {t:?}: {above:?}");
            }
            assert!(
                !above.iter().any(|l| l == "after prompt"),
                "height {height}"
            );
            let at = |t: &str| above.iter().position(|l| l == t);
            assert!(at("before plan") < at("odd notice"));
            assert!(at(&lines[13]).is_none(), "plan header went to scrollback");
            assert!(!above.contains(&lines[58]), "height {height}");
        }
    }

    #[test]
    fn candy_bar_follows_pacman_rule() {
        let p = plain();
        assert_eq!(bar(50, 10, true, &p), "-----C o  ");
        assert_eq!(bar(51, 10, true, &p), "-----c o  ");
        assert_eq!(bar(0, 10, true, &p), "C   o  o  ");
        assert_eq!(bar(100, 10, true, &p), "----------");
    }

    #[test]
    fn candy_bar_matches_pacman_on_screen() {
        // Fixture lines 101 and 105: 3% and 4% of pacman's 64-cell bar.
        let lines = fixture_lines();
        for (i, pct) in [(101, 3u8), (105, 4)] {
            let on_screen = lines[i]
                .split_once('[')
                .and_then(|(_, r)| r.split_once(']'))
                .map(|(b, _)| b.to_string());
            assert_eq!(Some(bar(pct, 64, true, &plain())), on_screen);
        }
    }

    #[test]
    fn hash_bar_without_candy() {
        assert_eq!(bar(50, 10, false, &plain()), "#####-----");
    }

    #[test]
    fn candy_head_is_bold_yellow() {
        let b = bar(50, 10, true, &colour());
        assert!(b.contains(ESC) && strip(&b) == "-----C o  ", "{b:?}");
    }

    #[test]
    fn candy_read_from_pacman_conf() {
        assert!(candy_enabled("[options]\nILoveCandy\n"));
        assert!(candy_enabled("[options]\n  ILoveCandy   # yes\n"));
        assert!(!candy_enabled("#ILoveCandy\n"));
        assert!(!candy_enabled("[options]\nColor\n"));
    }

    #[test]
    fn durations_read_like_a_stopwatch() {
        assert_eq!(duration(secs(12)), "12s");
        assert_eq!(duration(secs(124)), "2m04s");
        assert_eq!(duration(secs(3725)), "1h02m");
    }

    #[test]
    fn end_line_per_outcome_and_motif() {
        let mut r = fixture_run();
        r.finish(Some(0), secs(124));
        assert_eq!(
            end_line(&r, Motif::Arcade, &uni()),
            "ᗧ·· 38 eaten in 2m04s · 702.9 MiB downloaded"
        );
        assert_eq!(
            end_line(&r, Motif::Subtle, &uni()),
            "✓ 38 eaten in 2m04s · 702.9 MiB downloaded"
        );
        assert_eq!(
            end_line(&r, Motif::Arcade, &plain()),
            "C.. 38 eaten in 2m04s - 702.9 MiB downloaded"
        );
        let mut f = empty();
        f.finish(Some(1), Z);
        assert_eq!(end_line(&f, Motif::Subtle, &uni()), "✗ pacman exit 1");
        assert_eq!(end_line(&f, Motif::Arcade, &uni()), "ᗣ pacman exit 1");
    }

    #[test]
    fn clean_board_when_nothing_changed() {
        let mut r = empty();
        r.apply_log(
            crate::update_view::log_line::classify_log(
                "[2026-10-16T11:35:36+0530] [PACMAN] Running 'pacman -Syu'",
            )
            .map(|x| x.1)
            .unwrap_or(crate::update_view::log_line::LogEvent::Other),
            Z,
        );
        r.apply_line(" there is nothing to do", Z);
        r.finish(Some(0), secs(3));
        let l = end_line(&r, Motif::Subtle, &uni());
        assert!(l.contains("nothing to do") && !l.contains("eaten"), "{l}");
    }

    #[test]
    fn unknown_outcome_says_so() {
        let mut r = partial(12);
        r.finish(Some(0), Z);
        let l = end_line(&r, Motif::Subtle, &uni());
        assert_eq!(l, "? pacman exit 0 · result unknown (no pacman.log entry)");
    }

    #[test]
    fn arcade_lane_has_one_dot_per_package_left() {
        let f = draw(
            &partial(161),
            &view(Layout::Inline, Motif::Arcade),
            40,
            &uni(),
        );
        let lane = f.region.iter().find(|l| l.contains('ᗧ'));
        let dots = lane.map(|l| l.matches('·').count());
        assert_eq!(dots, Some(38 - 13), "{:?}", f.region);
    }

    #[test]
    fn arcade_receipt_marks_reboot_with_a_pellet() {
        let c = closing(
            &fixture_run(),
            &view(Layout::Inline, Motif::Arcade),
            120,
            "/t",
            &uni(),
        );
        assert!(
            c.iter().any(|l| l.starts_with('●') && l.contains("reboot")),
            "{c:?}"
        );
    }
}
