//! The run model drawn as text (#81): the live region, the lines printed
//! once above it, the receipt or failure block, and the end line. Pure — no
//! terminal, no clock; the caller passes width, height and `now`.

use std::time::Duration;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::cli::style::{Ghost, Styles};
use crate::config::{Layout, Motif};

use super::medians::Median;
use super::model::{BEAT, Hook, Note, NoteKind, Outcome, Phase, PhaseKind, PhaseState, Run};
use super::screen_line::{ScreenEvent, classify_screen, is_prompt};

pub struct View {
    pub layout: Layout,
    pub motif: Motif,
    pub candy: bool,
}

/// `above` = lines to print permanently over the region; `region` = the
/// live lines, redrawn each frame; `cursor` = pacman waits for an answer, so
/// the cursor shows at the end of the region's last line, where it is typed.
#[derive(Debug, Default)]
pub struct Frame {
    pub above: Vec<String>,
    pub region: Vec<String>,
    pub cursor: bool,
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
#[derive(Clone, Copy, PartialEq)]
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
        // Steps are known only as each starts: the count waits for the end.
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

/// Time since pacman's first phase began: the password prompt before it
/// is not pacman's time.
fn elapsed(run: &Run, now: Duration) -> Duration {
    let t0 = run
        .phases
        .iter()
        .filter_map(|p| p.started)
        .min()
        .unwrap_or_default();
    run.ended.unwrap_or(now).saturating_sub(t0)
}

/// `pacman -Syu · packages · 2m00s`, plus one ghost per kind of note that
/// wants the user (arcade).
fn header(run: &Run, view: &View, now: Duration, s: &Styles) -> Vec<Seg> {
    let b = s.bullet();
    let mut segs = vec![
        seg(Paint::Bold, run.command.clone()),
        seg(Paint::Dim, format!(" {b} {}", duration(elapsed(run, now)))),
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
        // The running mark in its phase's colour, as the receipt's
        // ribbon paints it.
        PhaseState::Running => (kind_paint(p.kind), g(s, "▸", ">")),
        PhaseState::Pending => (Paint::Dim, g(s, "·", ".")),
    };
    let time = took(p.started, p.ended, now)
        .map(duration)
        .unwrap_or_default();
    let n = count(p);
    let noun = match (p.kind, n.is_empty()) {
        (_, true) => "",
        (PhaseKind::Checks, _) => "steps",
        (PhaseKind::Sync, _) => "dbs",
        (PhaseKind::PreHooks | PhaseKind::PostHooks, _) => "hooks",
        _ => "pkgs",
    };
    // The running phase stands out; finished ones step back — but a phase
    // that just finished stays lit for a beat, and a count that just rose
    // lights green: each real step lands.
    let fresh = |t: Option<Duration>| t.is_some_and(|t| now.saturating_sub(t) < BEAT);
    let text_paint = match p.state {
        PhaseState::Done if fresh(p.ended) => Paint::Ok,
        PhaseState::Running => Paint::Bold,
        PhaseState::Done | PhaseState::Pending => Paint::Dim,
    };
    let count_paint = if p.state == PhaseState::Running && fresh(p.bumped) {
        Paint::Ok
    } else {
        text_paint
    };
    let rest = format!(" {noun:<5}  {time}");
    vec![
        seg(mark_paint, mark),
        seg(text_paint, format!(" {:<10} ", name(p.kind))),
        seg(count_paint, format!("{n:>7}")),
        seg(text_paint, rest.trim_end().to_string()),
    ]
}

/// A phase's colour: download cyan, packages green, hooks magenta, the
/// rest amber while running (dim on the ribbon).
fn kind_paint(k: PhaseKind) -> Paint {
    match k {
        PhaseKind::Download => Paint::Ghost(Ghost::Inky),
        PhaseKind::Packages => Paint::Ok,
        PhaseKind::PreHooks | PhaseKind::PostHooks => Paint::Ghost(Ghost::Pinky),
        _ => Paint::Warn,
    }
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
    // One set of columns for the whole run, so no phase moves the bars:
    // `  ├─ ` + the longest planned name + a space, unless the phase label
    // is wider; a third of the screen at most.
    let name_w = run
        .packages
        .iter()
        .map(|p| p.name.width())
        .max()
        .unwrap_or(0)
        .min(w / 3);
    let mut g = Gauge {
        left: PHASE_W.max(5 + name_w + 1),
        cells: 0,
        size: true,
        rate: true,
    };
    // Numbers give way before the bar gets short: the rate first, then sizes.
    loop {
        g.cells = w.saturating_sub(g.left + numbers_w(&g) + 3 + 5 + 1);
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

/// Columns the numbers before a bar take, gaps included.
fn numbers_w(g: &Gauge) -> usize {
    (if g.size { SIZE_W + 2 } else { 0 }) + if g.rate { RATE_W + 2 + ETA_W + 2 } else { 0 }
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
        // Fixed halves, so a column of them lines up on the `/`.
        Some(t) => {
            let (a, b) = (format!("{:.dec$}", g / div), format!("{:.dec$}", t / div));
            format!("{a:>6}/{:<10}", format!("{b} {unit}"))
        }
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

/// `  53.7/155.5 MiB  2.86 MiB/s  00:29  [----C o  o ]  42%`, pacman's
/// own order: the numbers by the name, the bar after them. Columns the
/// gauge has no room for are left out whole. `approx`: the percent is an
/// estimate (`~42%`).
fn gauge_tail(
    g: &Gauge,
    pct: u8,
    approx: bool,
    heavy: bool,
    from: Option<u8>,
    numbers: Option<(String, String, String)>,
    s: &Styles,
) -> Vec<Seg> {
    let (size, rate, eta) = numbers.unwrap_or_default();
    // A second or two without bytes is ordinary jitter: shown dim, the
    // mouth shut. Three is a stall: amber, and the ghost comes.
    let stall = [&size, &rate]
        .iter()
        .find(|t| t.starts_with("stalled"))
        .map(|t| {
            let n = t.trim_start_matches("stalled").trim().trim_end_matches('s');
            if n.is_empty() {
                0
            } else {
                n.parse::<u64>().unwrap_or(u64::MAX)
            }
        });
    let stall_paint = if stall.is_some_and(|n| n >= 3) {
        Paint::Warn
    } else {
        Paint::Dim
    };
    // Live figures read plain.
    let mut segs = Vec::new();
    if g.size {
        let p = if size.starts_with("stalled") {
            stall_paint
        } else {
            Paint::Plain
        };
        // A stall reads from the cell's left, so its count grows rightward.
        segs.push(seg(
            p,
            if size.starts_with("stalled") {
                format!("{size:<SIZE_W$}")
            } else {
                format!("{size:>SIZE_W$}")
            },
        ));
        segs.push(seg(Paint::Plain, "  "));
    }
    if g.rate {
        let p = if rate.starts_with("stalled") {
            stall_paint
        } else {
            Paint::Plain
        };
        segs.push(seg(
            p,
            if rate.starts_with("stalled") {
                format!("{rate:<RATE_W$}")
            } else {
                format!("{rate:>RATE_W$}")
            },
        ));
        segs.push(seg(Paint::Plain, format!("  {eta:>ETA_W$}  ")));
    }
    if g.cells >= 6 {
        let mouth = match stall {
            Some(n) if n >= 3 => Mouth::Haunt,
            Some(_) => Mouth::Wait,
            None => Mouth::Chew,
        };
        segs.push(seg(
            Paint::Plain,
            // The phase's own bar leads its files' bars: a heavier track.
            format!("{} ", {
                let t = comet(pct, g.cells, mouth, from, s);
                if heavy { t.replace('─', "━") } else { t }
            }),
        ));
    }
    let p = if approx {
        format!("~{pct}%")
    } else {
        format!("{pct}%")
    };
    segs.push(seg(Paint::Plain, format!("{p:>4}")));
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
        // The count says how many hooks are done; the one bar is the
        // running hook's own, under it.
        PhaseKind::PreHooks | PhaseKind::PostHooks => None,
        _ => run.progress(now),
    };
    let Some(pct) = pct else {
        // Checks list their own steps as children.
        if let Some(d) = p.detail.as_ref().filter(|_| p.kind != PhaseKind::Checks) {
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
        // At 100% pacman's last figure is the time taken, not time left.
        let eta = if p.pct.unwrap_or(0) >= 100 {
            String::new()
        } else {
            eta
        };
        (size_pair(&got, total), rate(&r), eta)
    });
    // Packages: how fast they go, counted here — done over time so far.
    let numbers = numbers.or_else(|| {
        let t = took(p.started, p.ended, now)?.as_secs_f64();
        let r = p.done as f64 / t;
        // Shown once there is enough to divide: early on it swings.
        (p.kind == PhaseKind::Packages && t >= 3.0 && p.done >= 10 && r >= 0.1)
            .then(|| (String::new(), format!("{r:.1} pkgs/s"), String::new()))
    });
    row.extend(gauge_tail(g, pct, false, true, None, numbers, s));
    row
}

/// The children of the running phase: what pacman is doing right now, one
/// row each — files downloading, the packages being upgraded, the hook that
/// runs and its last lines of output.
fn children(
    run: &Run,
    ga: &Gauge,
    w: usize,
    room: usize,
    now: Duration,
    s: &Styles,
) -> Vec<Vec<Seg>> {
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
                    // Stalled: the last bytes pacman drew, and for how long
                    // no new ones came.
                    let stall = f.stalled.then(|| {
                        let t = f
                            .stalled_since
                            .map(|t| now.saturating_sub(t))
                            .unwrap_or_default();
                        // Under a second it is not yet a figure to show.
                        if t.as_secs() == 0 {
                            "stalled".to_string()
                        } else {
                            format!("stalled {}", duration(t))
                        }
                    });
                    let (r, eta) = match &stall {
                        Some(st) => (st.clone(), String::new()),
                        None => (rate(&r), eta),
                    };
                    // No rate column: the size column says it instead.
                    // Finished: all of it came, so it reads full.
                    let size = match stall {
                        Some(st) if !ga.rate => st,
                        _ if f.done_at.is_some() && total.is_some() => {
                            size_pair(total.unwrap_or_default(), total)
                        }
                        _ => size_pair(&got, total),
                    };
                    // Just finished: the name lights green for its beat.
                    let done = f.done_at.is_some();
                    let mut row = vec![seg(if done { Paint::Ok } else { Paint::Plain }, name)];
                    row.extend(gauge_tail(
                        ga,
                        f.pct,
                        false,
                        false,
                        f.tail_from().filter(|_| !done),
                        Some((size, r, eta)),
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
            // What pacman counted finished, collected on one row, newest
            // first and green: the pile grows as the queue shrinks.
            if !run.eaten.is_empty() {
                let sep = format!(" {} ", s.bullet());
                // The count is pacman's; the names are the ones seen go.
                let head = format!(
                    "eaten  {:>2}  ",
                    phase(run, k).map_or(0, |p| p.done).max(run.eaten.len())
                );
                let mut segs = vec![seg(Paint::Dim, head.clone())];
                let mut used = 5 + head.width();
                for (k, e) in run.eaten.iter().rev().enumerate() {
                    let add = if k == 0 {
                        e.clone()
                    } else {
                        format!("{sep}{e}")
                    };
                    if used + add.width() + 4 > w {
                        segs.push(seg(Paint::Dim, format!("{sep}…")));
                        break;
                    }
                    used += add.width();
                    segs.push(seg(if k == 0 { Paint::Ok } else { Paint::Dim }, add));
                }
                rows.push(segs);
            }
            rows
        }
        PhaseKind::Packages => {
            // The last `(i/n) verb name` lines that fit: done ones dim, the
            // current one bold. Columns fixed for the whole phase from the
            // plan, so the arrows never move; the verb only when it is not
            // the usual `upgrading`.
            // Settled lines are the packages done; the live row (when
            // pacman is still on one) is the current one, with its percent.
            let live = run.live_package.as_ref();
            let cap = room.clamp(1, 8);
            let mut recent: Vec<(String, String, Option<u8>)> = run
                .lines
                .iter()
                .rev()
                .filter_map(|l| match classify_screen(l, false) {
                    ScreenEvent::Package { verb, name, .. } => Some((verb, name, None)),
                    _ => None,
                })
                .filter(|(_, n, _)| live.is_none_or(|(_, l, _)| l != n))
                .take(cap - usize::from(live.is_some() && cap > 1))
                .collect();
            recent.reverse();
            if let Some((v, n, p)) = live {
                recent.push((v.clone(), n.clone(), Some(*p)));
            }
            recent.reverse();
            let n = recent.len();
            let verb_of = |v: &str| {
                if v == "upgrading" {
                    String::new()
                } else {
                    format!("{v} ")
                }
            };
            let verb_w = recent
                .iter()
                .map(|(v, _, _)| verb_of(v).width())
                .max()
                .unwrap_or(0);
            let name_w = run
                .packages
                .iter()
                .map(|p| p.name.width())
                .max()
                .unwrap_or(0)
                .min(w / 3);
            // As wide as the longest old version, while the row keeps room
            // for the new one beside it; past that, cut where versions agree.
            let longest = |f: fn(&Pkg) -> Option<&str>| {
                run.packages
                    .iter()
                    .filter_map(f)
                    .map(UnicodeWidthStr::width)
                    .max()
                    .unwrap_or(0)
            };
            // With bars on, the row ends before the bar column, so the
            // running package's bar starts where every other bar does.
            let lim = if ga.cells >= 6 {
                ga.left + numbers_w(ga)
            } else {
                w
            };
            let room =
                lim.saturating_sub(7 + verb_w + name_w + 2 + 3 + longest(|p| p.new.as_deref()) + 4);
            let old_w = longest(|p| p.old.as_deref()).min(room.max(16));
            recent
                .into_iter()
                .rev()
                .enumerate()
                .map(|(i, (verb, name, pct))| {
                    // Bold is the package pacman is on now, by its own
                    // percent; a settled last row stays dim.
                    let current = i + 1 == n && pct.is_some();
                    // The newest one pacman finished lights green: each
                    // package lands, then steps back as the next lands.
                    let newest = pct.is_none() && i + 1 + usize::from(live.is_some()) == n;
                    // Each row says its state: done `✓`, the live one `▸`.
                    let mark = if pct.is_some() {
                        seg(Paint::Ok, format!("{} ", g(s, "▸", ">")))
                    } else {
                        seg(Paint::Ok, format!("{} ", s.check()))
                    };
                    let mut row = vec![
                        mark,
                        seg(Paint::Dim, format!("{:<verb_w$}", verb_of(&verb))),
                        seg(
                            if current {
                                Paint::Bold
                            } else if newest {
                                Paint::Ok
                            } else {
                                Paint::Dim
                            },
                            format!("{:<name_w$}  ", cut_plain(&name, name_w)),
                        ),
                    ];
                    let pkg = run.packages.iter().find(|p| p.name == name);
                    if let Some((Some(old), Some(new))) = pkg.map(|p| (&p.old, &p.new)) {
                        let old = cut_version(old, Some(new), old_w);
                        row.push(seg(
                            Paint::Plain,
                            " ".repeat(old_w.saturating_sub(old.width())),
                        ));
                        if current {
                            let (o, n) = version_diff(&old, new);
                            row.extend(o);
                            row.push(seg(Paint::Dim, format!(" {} ", s.arrow())));
                            row.extend(n);
                        } else if newest {
                            // Its new version lands green with it.
                            row.push(seg(Paint::Dim, format!("{old} {} ", s.arrow())));
                            row.push(seg(Paint::Ok, new.clone()));
                        } else {
                            row.push(seg(Paint::Dim, format!("{old} {} {new}", s.arrow())));
                        }
                    }
                    // pacman's own percent for the one it is on, in the run's
                    // percent column, under the phase's own.
                    if let Some(p) = pct {
                        let mut used: usize = 5 + row.iter().map(|(_, t)| t.width()).sum::<usize>();
                        // Narrow: the live row gives up its old version so
                        // its bar still starts in the bar column.
                        let at = ga.left + numbers_w(ga);
                        if ga.cells >= 6
                            && used + 2 > at
                            && let Some(new) = pkg.and_then(|p| p.new.as_deref())
                        {
                            row.truncate(3);
                            row.push(seg(Paint::Dim, format!("{} ", s.arrow())));
                            row.push(seg(Paint::Ok, new.to_string()));
                            used = 5 + row.iter().map(|(_, t)| t.width()).sum::<usize>();
                        }
                        // Its own bar in the run's bar column, when the row
                        // leaves it free: pacman's percent for this package.
                        // Still too long: a shorter bar, its right end on
                        // the column's, rather than none.
                        let col = ga.left + numbers_w(ga);
                        let at = col.max(used + 2);
                        let cells = (col + ga.cells).saturating_sub(at);
                        if ga.cells >= 6 && cells >= 8 {
                            row.push(seg(Paint::Plain, " ".repeat(at - used)));
                            row.push(seg(Paint::Plain, track(p, cells, Mouth::Chew, s)));
                            row.push(seg(Paint::Plain, format!(" {:>4}", format!("{p}%"))));
                            return row;
                        }
                        let end = ga.left
                            + numbers_w(ga)
                            + if ga.cells >= 6 { ga.cells + 3 } else { 0 }
                            + 4;
                        let gap = end.saturating_sub(used + 4).max(2);
                        row.push(seg(
                            Paint::Dim,
                            format!("{}{:>4}", " ".repeat(gap), format!("{p}%")),
                        ));
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
            // The one before (done, dim) and the one running (bold), each
            // whole. Under the running one, when it usually takes a while,
            // its time so far against the median of past runs: a bar in
            // the run's columns, marked as the estimate it is (`~`).
            let started: Vec<&Hook> = list.iter().filter(|h| h.started.is_some()).collect();
            let from = started.len().saturating_sub(2);
            // pacman's trailing `...` says "running" and a `.` ends a
            // sentence; the mark says the first, the row needs neither.
            let desc_of = |h: &Hook| {
                h.desc
                    .as_deref()
                    .or(h.file.as_deref())
                    .unwrap_or_default()
                    .trim_end_matches('.')
                    .to_string()
            };
            // Done times in one column after the longest name shown.
            let desc_w = started[from..]
                .iter()
                .map(|h| desc_of(h).width())
                .max()
                .unwrap_or(0)
                .min(w.saturating_sub(5 + 2 + 8));
            let mut rows = Vec::new();
            // Every hook time ends in one column: the end of the number
            // cells, just before the bar column (or a short way after the
            // longest name when there are no number cells).
            let cells_w = numbers_w(ga).saturating_sub(2);
            let edge = if cells_w >= 6 {
                ga.left + cells_w
            } else {
                5 + 2 + desc_w + 2 + 6
            };
            for (i, h) in started[from..].iter().enumerate() {
                let desc = desc_of(h);
                let running = h.ended.is_none() && from + i + 1 == started.len();
                let took = took(h.started, h.ended, now).unwrap_or_default();
                let median = h.median.filter(|m| m.secs >= 2).filter(|_| running);
                let times = match median {
                    Some(Median { secs, .. }) => format!(
                        "{} {} usually {}",
                        duration(took),
                        s.bullet(),
                        duration(Duration::from_secs(secs))
                    ),
                    None => duration(took),
                };
                // Room for the name before the times on the same row.
                let room = edge.saturating_sub(5 + 2 + 2 + times.width());
                let (mark, paint) = if running {
                    (seg(Paint::Warn, g(s, "▸", ">")), Paint::Bold)
                } else {
                    (seg(Paint::Ok, s.check()), Paint::Dim)
                };
                let one_row = desc.width() <= room;
                // A finished hook too long for the time column keeps its
                // whole name (to the screen's edge), its time just after.
                let shown = if one_row {
                    desc.clone()
                } else if !running {
                    cut_plain(&desc, w.saturating_sub(5 + 2 + 2 + times.width() + 1))
                } else {
                    desc.clone()
                };
                let mut row = vec![mark, seg(paint, format!(" {shown}"))];
                let used = 5 + 2 + shown.width();
                let pad = |used: usize| " ".repeat(edge.saturating_sub(used + times.width()));
                let Some(Median { secs, .. }) = median else {
                    let gap = if one_row { pad(used) } else { "  ".to_string() };
                    row.push(seg(Paint::Dim, format!("{gap}{times}")));
                    rows.push(row);
                    continue;
                };
                // The time so far against the median of past runs: a bar in
                // the run's bar column, marked as the estimate it is (`~`).
                // Past the median by a second or more the bar is full,
                // Pac-Man waits dim at its end, and the overrun counts up.
                let typical = Duration::from_secs(secs);
                let over = took.saturating_sub(typical);
                let (rail, label) = if over.as_secs() == 0 {
                    let pct = u8::try_from(took.as_millis() * 100 / typical.as_millis().max(1))
                        .unwrap_or(99)
                        .min(99);
                    (track(pct, ga.cells, Mouth::Guess, s), format!("~{pct}%"))
                } else {
                    (
                        track(100, ga.cells, Mouth::Over, s),
                        format!("+{}", duration(over)),
                    )
                };
                let mut tail = Vec::new();
                if ga.cells >= 6 {
                    tail.push(seg(Paint::Plain, format!("  {rail}")));
                }
                tail.push(seg(Paint::Plain, format!(" {label:>4}")));
                if one_row {
                    row.push(seg(Paint::Dim, format!("{}{times}", pad(used))));
                    row.extend(tail);
                    rows.push(row);
                } else {
                    // A long name keeps its own row; times and bar go under
                    // it, in the same columns.
                    rows.push(row);
                    let mut under = vec![seg(Paint::Dim, format!("{NESTED}{}{times}", pad(5)))];
                    under.extend(tail);
                    rows.push(under);
                }
            }
            rows
        }
        PhaseKind::Checks => {
            // Each check pacman finished (its line settled), then the one
            // it is on.
            let mut names: Vec<String> = Vec::new();
            for l in &run.lines {
                if let ScreenEvent::Check { name, .. } = classify_screen(l, false)
                    && !names.contains(&name)
                {
                    names.push(name);
                }
            }
            let now_on = phase(run, k).and_then(|p| p.detail.clone());
            let mut rows: Vec<Vec<Seg>> = names
                .iter()
                .filter(|n| Some(*n) != now_on.as_ref())
                .map(|n| vec![seg(Paint::Ok, s.check()), seg(Paint::Dim, format!(" {n}"))])
                .collect();
            if let Some(n) = now_on {
                rows.push(vec![
                    seg(Paint::Warn, g(s, "▸", ">")),
                    seg(Paint::Bold, format!(" {n}")),
                ]);
            }
            rows
        }
        _ => item(run, now, s).into_iter().collect(),
    };
    // After the answer: the transaction's own output (hooks, scriptlets),
    // last few lines, under what produced it; the newest one reads plain.
    let tail = log_lines(run, LOG_TAIL);
    let n = tail.len();
    for (i, l) in tail.into_iter().enumerate() {
        let paint = if i + 1 == n { Paint::Plain } else { Paint::Dim };
        // The same dim gutter as a package's message: source, then output.
        rows.push(vec![
            seg(Paint::Dim, format!("{NESTED}  {} ", g(s, "▎", "|"))),
            seg(paint, l.trim_end().to_string()),
        ]);
    }
    rows
}

/// Marks a child row that hangs under the one before it (output of the
/// running hook), so the tree gives it no branch of its own.
const NESTED: &str = "\u{0}";

/// `v` in `w` columns, cut where it agrees with `other` first: the front
/// both share, back to a component edge (`…8.2110-1` against `2026.8.2115-1`);
/// if that is not enough, the end. The first changed component stays in
/// view as long as anything does (`…155.0.8059…` keeps the 155).
fn cut_version(v: &str, other: Option<&str>, w: usize) -> String {
    if v.width() <= w {
        return v.to_string();
    }
    let same = other.map_or(0, |o| {
        let n = v.chars().zip(o.chars()).take_while(|(a, b)| a == b).count();
        v.char_indices()
            .take(n)
            .filter(|(_, c)| matches!(c, '.' | '-' | ':' | '+'))
            .map(|(i, c)| i + c.len_utf8())
            .last()
            .unwrap_or(0)
    });
    // The first change already in view when only the end is cut: cut
    // there, once. Else drop as much of the shared front as it takes.
    let differs = other.map_or(0, |o| {
        v.chars().zip(o.chars()).take_while(|(a, b)| a == b).count()
    });
    if differs + 2 < w {
        return cut_plain(v, w);
    }
    let over = v.width() + 1 - w;
    let drop = v
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(v.len()))
        .find(|&i| i >= over.min(same))
        .unwrap_or(0)
        .min(same);
    let rest = if drop > 0 {
        format!("…{}", &v[drop..])
    } else {
        v.to_string()
    };
    cut_plain(&rest, w)
}

/// `t` cut to `w` columns at a word's end, with an ellipsis.
fn cut_words(t: &str, w: usize) -> String {
    if t.width() <= w {
        return t.to_string();
    }
    let c = cut_plain(t, w);
    let body = c.trim_end_matches('…');
    match body.rfind(' ') {
        Some(i) if i > w / 2 => format!("{}…", body[..i].trim_end_matches([',', ';', ':'])),
        _ => c,
    }
}

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
        .map(|p| {
            let mut row = phase_row(p, now, s);
            // The download keeps what it moved, and how fast on average,
            // in the columns those figures had while it ran.
            if p.kind == PhaseKind::Download
                && let Some(t) = run.totals.iter().find(|t| t.0 == "Total Download Size")
                && let Some(b) = bytes(&t.1)
            {
                let secs = took(p.started, p.ended, now)
                    .unwrap_or_default()
                    .as_secs_f64();
                let avg = if secs > 0.0 {
                    rate(&format!("{} B", b / secs))
                } else {
                    String::new()
                };
                let size = size_pair(&t.1, None);
                let used: usize = row.iter().map(|(_, t)| t.width()).sum();
                if gauge.size {
                    let mut cells = format!(
                        "{}{size:>SIZE_W$}",
                        " ".repeat(gauge.left.saturating_sub(used))
                    );
                    if gauge.rate {
                        cells += &format!("  {avg:>RATE_W$}");
                    }
                    row.push(seg(Paint::Dim, cells));
                } else {
                    row.push(seg(Paint::Dim, format!("  {size} {} {avg}", s.bullet())));
                }
            }
            row
        })
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
        // The route still ahead, in order.
        let dot = g(s, "·", ".");
        let to = g(s, "›", ">");
        vec![seg(
            Paint::Dim,
            format!("{dot} next  {}", pending.join(&format!(" {to} "))),
        )]
    });
    let fixed = 1 + usize::from(running.is_some()) + usize::from(pending.is_some());
    // Folded by the screen's size alone, so the shape does not flip as
    // children come and go: a dozen rows for them, or fold.
    let done_rows: Vec<Vec<Seg>> = if fixed + done.len() + 12 > max {
        done_summary(run, now, s).into_iter().collect()
    } else {
        done
    };
    let room = max.saturating_sub(fixed + done_rows.len());
    // Room left after the transaction's output tail.
    let kids = children(run, &gauge, w, room.saturating_sub(LOG_TAIL), now, s);
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

    let mut head = header(run, view, now, s);
    // No room for the rate on the rows: the header carries the total's.
    if run.current() == Some(PhaseKind::Download) && !gauge.rate {
        let d = phase(run, PhaseKind::Download).and_then(|p| p.detail.as_deref());
        if let Some(d) = d {
            let (_, r, eta) = split_detail(d);
            let b = s.bullet();
            let done = phase(run, PhaseKind::Download)
                .and_then(|p| p.pct)
                .unwrap_or(0)
                >= 100;
            if !done {
                head.push(seg(Paint::Dim, format!(" {b} {} {b} {eta} left", rate(&r))));
            }
        }
    }
    let mut rows = vec![fit(&head)];
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
/// repo, written once: `linux-firmware-{amd,intel,nvidia}`. Every member is
/// still named; only the shared front is not repeated.
struct Family {
    prefix: String,
    members: Vec<String>,
    old: Option<String>,
    new: Option<String>,
    /// Their download sizes added up, when every member has one.
    size: Option<String>,
}

/// `list` split into single packages and families. `any`: fold every family;
/// otherwise only those whose members share both versions, which loses
/// nothing.
fn fold_families(list: &[&Pkg], any: bool) -> (Vec<Pkg>, Vec<Family>) {
    let key = |p: &Pkg| p.name.rfind('-').map(|i| p.name[..=i].to_string());
    let (mut singles, mut families) = (Vec::new(), Vec::<Family>::new());
    for p in list {
        let k = key(p);
        if families.iter().any(|f| Some(&f.prefix) == k.as_ref()) {
            continue;
        }
        let members: Vec<&&Pkg> = match &k {
            Some(k) => list.iter().filter(|q| key(q).as_ref() == Some(k)).collect(),
            None => Vec::new(),
        };
        let same = |f: &dyn Fn(&Pkg) -> &Option<String>| {
            let first = f(members.first()?);
            members
                .iter()
                .all(|m| f(m) == first)
                .then(|| first.clone())
                .flatten()
        };
        let shared = same(&|m| &m.old).is_some() && same(&|m| &m.new).is_some();
        match k {
            Some(prefix) if members.len() >= 3 && (any || shared) => {
                let sizes: Option<f64> = members
                    .iter()
                    .map(|m| m.download.as_deref().and_then(bytes))
                    .sum();
                families.push(Family {
                    members: members
                        .iter()
                        .map(|m| m.name[prefix.len()..].to_string())
                        .collect(),
                    old: same(&|m| &m.old),
                    new: same(&|m| &m.new),
                    size: sizes.map(|b| size_pair(&format!("{b} B"), None)),
                    prefix,
                });
            }
            _ => singles.push((*p).clone()),
        }
    }
    (singles, families)
}

/// One family as rows of at most `w` columns: the members wrapped at commas
/// under the `{`, the versions after them on the last row, or on a row of
/// their own when they do not fit.
fn family_rows(f: &Family, tail: Vec<Seg>, w: usize) -> Vec<Vec<Seg>> {
    let indent = f.prefix.width() + 1;
    let mut rows: Vec<Vec<Seg>> = Vec::new();
    let mut cur = vec![seg(Paint::Plain, f.prefix.clone()), seg(Paint::Dim, "{")];
    let mut used = indent;
    for (i, m) in f.members.iter().enumerate() {
        let end = if i + 1 == f.members.len() { "}" } else { "," };
        if used + m.width() + 1 > w && used > indent {
            rows.push(std::mem::take(&mut cur));
            cur.push(seg(Paint::Plain, " ".repeat(indent)));
            used = indent;
        }
        cur.push(seg(Paint::Plain, m.clone()));
        cur.push(seg(Paint::Dim, end));
        used += m.width() + 1;
    }
    let tw: usize = tail.iter().map(|(_, t)| t.width()).sum();
    if tw > 0 {
        if used + 2 + tw > w {
            rows.push(std::mem::take(&mut cur));
            cur.push(seg(Paint::Plain, " ".repeat(indent)));
        } else {
            cur.push(seg(Paint::Plain, "  "));
        }
        cur.extend(tail);
    }
    rows.push(cur);
    rows
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
    /// name only, whole
    Names,
    /// name only, the longest few cut: one more column
    TightNames,
}

/// The plan at `[Y/n]`: one summary line, then the packages grouped by the
/// repo they come from, each group under its own heading and spread over as
/// many columns as the width allows (20 packages in 2 columns: 10 and 10;
/// in 3: 7, 7, 6), read down then across. Versions line up across groups.
/// Families that share their versions are written once
/// (`linux-firmware-{amd,intel}`). When it does not fit in `max` rows,
/// detail goes before packages do: sizes, then old versions, then the other
/// families fold too, then versions, then the longest names are cut; past
/// that, "+N more".
fn compact_plan(run: &Run, w: usize, max: usize, s: &Styles) -> Vec<String> {
    let ladder = [
        (Detail::Full, false),
        (Detail::Versions, false),
        (Detail::New, false),
        (Detail::New, true),
        (Detail::Names, true),
        (Detail::TightNames, true),
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

/// The unit that suits the figure: `1.19 GiB`, `36.9 MiB`, `12 KiB`
/// (pacman writes `0.01 MiB`).
fn tidy_size(v: &str) -> String {
    match bytes(v) {
        Some(b) => {
            let neg = if b < 0.0 { "-" } else { "" };
            format!("{neg}{}", size_pair(&format!("{} B", b.abs()), None))
        }
        None => v.to_string(),
    }
}

/// `n` entries over `k` columns, read down: each column's (start, length),
/// the first `n % k` one longer — 40 in 3 is 14, 13, 13.
fn split(n: usize, k: usize) -> Vec<(usize, usize)> {
    let k = k.max(1);
    let (q, rem) = (n / k, n % k);
    (0..k)
        .map(|c| (c * q + c.min(rem), q + usize::from(c < rem)))
        .collect()
}

/// The plan in one form; `true` when every package made it in.
fn plan_rows(
    run: &Run,
    w: usize,
    max: usize,
    detail: Detail,
    fold_any: bool,
    s: &Styles,
) -> (Vec<String>, bool) {
    let arrow = s.arrow();
    // Groups by repo, in name order.
    let mut groups: Vec<(&str, Vec<&Pkg>)> = Vec::new();
    for p in &run.packages {
        let repo = p.repo.as_deref().unwrap_or("packages");
        match groups.iter_mut().find(|g| g.0 == repo) {
            Some(g) => g.1.push(p),
            None => groups.push((repo, vec![p])),
        }
    }
    groups.sort_by(|a, b| a.0.cmp(b.0));
    let groups: Vec<(&str, usize, Vec<Pkg>, Vec<Family>)> = groups
        .into_iter()
        .map(|(repo, list)| {
            let (singles, families) = fold_families(&list, fold_any);
            (repo, list.len(), singles, families)
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
    let names = matches!(detail, Detail::Names | Detail::TightNames);
    // Every name whole unless this is the last resort.
    let longest = pk.iter().map(|p| p.name.width()).max().unwrap_or(0);
    let name_w = match detail {
        Detail::TightNames => width(&|p| p.name.width(), 28),
        // Names alone: they get the room versions would have had.
        Detail::Names => longest.min(32),
        _ => longest.min(27),
    };
    let old_w = if matches!(detail, Detail::Full | Detail::Versions) {
        width(&|p| p.old.as_deref().map_or(0, |v| v.width()), 20)
    } else {
        0
    };
    let new_w = if names {
        0
    } else {
        width(&|p| p.new.as_deref().map_or(0, |v| v.width()), 20)
    };
    // Sizes are short; the widest one sets the column so they line up.
    let size_w = if detail == Detail::Full {
        pk.iter()
            .map(|p| p.download.as_deref().map_or(0, |v| tidy_size(v).width()))
            .max()
            .unwrap_or(0)
    } else {
        0
    };
    let cell = name_w
        + if old_w > 0 {
            1 + old_w + 2 + arrow.width()
        } else {
            0
        }
        + if new_w > 0 && old_w == 0 { 1 } else { 0 }
        + new_w
        + if size_w > 0 { 2 + size_w } else { 0 };
    let gap = if names { 2 } else { 3 };
    let cols = ((w + gap) / (cell + gap)).max(1);
    // Width the columns leave over goes to the names, then the versions,
    // so fewer are cut.
    let mut spare = ((w + gap) / cols).saturating_sub(gap + cell);
    let mut grow = |have: usize, most: usize| {
        let more = most.saturating_sub(have).min(spare);
        spare -= more;
        have + more
    };
    // ponytail: 16 holds every common version; a 27-column git build (luajit) is cut.
    let widest = |f: &dyn Fn(&Pkg) -> usize| pk.iter().map(|p| f(p)).max().unwrap_or(0).min(16);
    let name_w = grow(name_w, longest);
    let old_w = if old_w > 0 {
        grow(
            old_w,
            widest(&|p| p.old.as_deref().map_or(0, |v| v.width())),
        )
    } else {
        0
    };
    let new_w = if new_w > 0 {
        grow(
            new_w,
            widest(&|p| p.new.as_deref().map_or(0, |v| v.width())),
        )
    } else {
        0
    };

    // `pad`: in a column, so cut and padded to it; else (a family row)
    // whole.
    let versions = |old: &Option<String>,
                    new: &Option<String>,
                    pad: bool,
                    old_w: usize,
                    new_w: usize|
     -> Vec<Seg> {
        let mut segs = Vec::new();
        // Only what both versions share is cut, so the change stays.
        let fit = |v: &str, other: Option<&String>, w: usize| {
            if pad {
                cut_version(v, other.map(String::as_str), w)
            } else {
                v.to_string()
            }
        };
        let (o, n) = match (old, new) {
            (Some(o), Some(n)) => version_diff(&fit(o, Some(n), old_w), &fit(n, Some(o), new_w)),
            (None, Some(n)) => (vec![], vec![seg(Paint::Ok, fit(n, None, new_w))]),
            _ => (vec![], vec![]),
        };
        // Old versions not shown: the new one still lights only its change.
        let (o, n) = match (old, new) {
            (Some(ov), Some(nv)) if old_w == 0 => {
                (o, version_diff(ov, &fit(nv, Some(ov), new_w)).1)
            }
            _ => (o, n),
        };
        let nw: usize = n.iter().map(|(_, t)| t.width()).sum();
        if old_w > 0 {
            let ow: usize = o.iter().map(|(_, t)| t.width()).sum();
            if pad {
                segs.push(seg(Paint::Plain, " ".repeat(old_w.saturating_sub(ow))));
            }
            if !o.is_empty() {
                segs.extend(o);
                segs.push(seg(Paint::Dim, format!(" {arrow} ")));
            } else if pad {
                segs.push(seg(Paint::Plain, " ".repeat(2 + arrow.width())));
            }
        }
        segs.extend(n);
        if pad {
            segs.push(seg(Paint::Plain, " ".repeat(new_w.saturating_sub(nw))));
        }
        segs
    };
    // `cw`: this column's widths — name, old version, new version.
    let entry = |p: &Pkg, cw: [usize; 3]| -> Vec<Seg> {
        let [name_w, old_w, new_w] = cw;
        let name = cut_plain(&p.name, name_w);
        let mut segs = vec![seg(Paint::Plain, format!("{name:<name_w$}"))];
        if names {
            return segs;
        }
        segs.push(seg(Paint::Plain, " "));
        segs.extend(versions(&p.old, &p.new, true, old_w, new_w));
        if size_w > 0 {
            let d = p.download.as_deref().map(tidy_size).unwrap_or_default();
            segs.push(seg(Paint::Dim, format!("  {d:>size_w$}")));
        }
        segs
    };
    let family = |f: &Family| -> Vec<Vec<Seg>> {
        let mut tail = Vec::new();
        if !names {
            tail = versions(&f.old, &f.new, false, old_w, new_w);
            if size_w > 0
                && let Some(sz) = &f.size
            {
                tail.push(seg(Paint::Dim, format!("  {sz}")));
            }
        }
        family_rows(f, tail, w)
    };

    let b = s.bullet();
    let mut head = vec![seg(Paint::Bold, format!("{} packages", run.packages.len()))];
    for (label, value) in &run.totals {
        let label = label
            .to_lowercase()
            .replace("total ", "")
            .replace(" size", "");
        head.push(seg(Paint::Dim, format!(" {b} ")));
        head.push(seg(Paint::Plain, format!("{} {label}", tidy_size(value))));
    }
    let mut out = vec![line(&head, w, s)];
    // Versions: each column as wide as its own entries need, across every
    // group so the columns still line up — when that fits the width. It
    // does whenever one long name or version, not every column, set the
    // shared widths; versions past 16 columns are still cut, keeping the
    // change.
    let per_col: Option<Vec<[usize; 3]>> = (!names)
        .then(|| {
            let mut cw = vec![[0usize; 3]; cols];
            for (_, _, list, _) in &groups {
                for (c, &(a, n)) in split(list.len(), cols.min(list.len().max(1)))
                    .iter()
                    .enumerate()
                {
                    for p in &list[a..a + n] {
                        let v = |x: &Option<String>| {
                            x.as_deref().map_or(0, UnicodeWidthStr::width).min(16)
                        };
                        cw[c][0] = cw[c][0].max(p.name.width());
                        cw[c][1] = cw[c][1].max(if old_w > 0 { v(&p.old) } else { 0 });
                        cw[c][2] = cw[c][2].max(if new_w > 0 { v(&p.new) } else { 0 });
                    }
                }
            }
            cw
        })
        .filter(|cw| {
            let total: usize = cw
                .iter()
                .map(|[n, o, nn]| {
                    n + if old_w > 0 {
                        1 + o + 2 + arrow.width()
                    } else {
                        0
                    } + if new_w > 0 && old_w == 0 { 1 } else { 0 }
                        + nn
                        + if size_w > 0 { 2 + size_w } else { 0 }
                })
                .sum();
            total + gap * cols.saturating_sub(1) <= w
        });
    // A form with versions is only good if nothing in it is cut (but a
    // version past 16 columns): else the next, sparer form.
    if !names && per_col.is_none() {
        return (Vec::new(), false);
    }
    // A repo's download in bytes, when pacman listed sizes.
    let repo_bytes = |repo: &str| -> Option<f64> {
        run.packages
            .iter()
            .filter(|p| p.repo.as_deref().unwrap_or("packages") == repo)
            .map(|p| p.download.as_deref().and_then(bytes))
            .sum()
    };
    let heading = |repo: &str, n: usize| -> (String, String) {
        let size = repo_bytes(repo)
            .map(|by| format!(" {} {}", s.bullet(), size_pair(&format!("{by} B"), None)))
            .unwrap_or_default();
        (repo.to_string(), format!(" ({n}){size}"))
    };
    // Every heading's rule starts in one column, so their lengths compare.
    let rule_at = groups
        .iter()
        .map(|(r, n, _, _)| {
            let (a, b) = heading(r, *n);
            a.width() + b.width() + 1
        })
        .max()
        .unwrap_or(0);
    let mut shown = 0;
    let mut left: Vec<String> = Vec::new();
    for (gi, (repo, n, list, fams)) in groups.iter().enumerate() {
        // Column c holds `n / k` entries, one more for the first `n % k`:
        // 40 in 3 is 14, 13, 13. Names alone: each column as wide as its
        // own longest name, and as many columns as then fit.
        let (k, widths) = if names {
            let cap = if detail == Detail::TightNames {
                name_w
            } else {
                w
            };
            let fits = |k: usize| {
                let ws: Vec<usize> = split(list.len(), k)
                    .iter()
                    .map(|&(a, n)| {
                        list[a..a + n]
                            .iter()
                            .map(|p| p.name.width())
                            .max()
                            .unwrap_or(0)
                            .min(cap)
                    })
                    .collect();
                (ws.iter().sum::<usize>() + gap * (k - 1) <= w).then_some(ws)
            };
            (1..=list.len().max(1))
                .rev()
                .find_map(|k| fits(k).map(|ws| (k, ws)))
                .unwrap_or_else(|| (1, vec![name_w.min(w)]))
        } else {
            let k = cols.min(list.len().max(1));
            (k, vec![name_w; k])
        };
        let widths: Vec<[usize; 3]> = if names {
            widths.iter().map(|&n| [n, 0, 0]).collect()
        } else {
            per_col
                .clone()
                .unwrap_or_else(|| vec![[name_w, old_w, new_w]; k])
        };
        let parts = split(list.len(), k);
        let rows = parts.first().map_or(0, |p| p.1);
        let fam_rows: Vec<Vec<Seg>> = fams.iter().flat_map(&family).collect();
        // Room for this group, and — unless it is the last — a line to
        // say what is left after it.
        let more = usize::from(gi + 1 < groups.len());
        if !left.is_empty() || out.len() + 1 + rows + fam_rows.len() + more > max {
            left.push(format!("{repo} {n}"));
            continue;
        }
        // The repo's share of the download, when pacman listed sizes.
        let by = repo_bytes(repo);
        let (name, rest) = heading(repo, *n);
        let mut head = vec![seg(Paint::Bold, name), seg(Paint::Dim, rest)];
        // A rule to the edge, its heavy part this repo's share of the
        // whole download: where the bytes come from, at a glance.
        let all: Option<f64> = run
            .packages
            .iter()
            .map(|p| p.download.as_deref().and_then(bytes))
            .sum();
        if let (Some(by), Some(all)) = (by, all.filter(|a| *a > 0.0))
            && s.colored()
        {
            let used: usize = head.iter().map(|x| x.1.width()).sum();
            head.push(seg(Paint::Plain, " ".repeat(rule_at.saturating_sub(used))));
            let len = w.saturating_sub(rule_at.max(used) + 1);
            let heavy = ((by / all * len as f64).round() as usize).min(len);
            head.push(seg(Paint::Ghost(Ghost::Inky), g(s, "━", "=").repeat(heavy)));
            head.push(seg(Paint::Dim, g(s, "─", "-").repeat(len - heavy)));
        }
        out.push(line(&head, w, s));
        for f in &fam_rows {
            out.push(line(f, w, s));
        }
        for r in 0..rows {
            let mut segs = Vec::new();
            for (c, &(a, n)) in parts.iter().enumerate() {
                if r >= n {
                    break;
                }
                let Some(p) = list.get(a + r) else {
                    break;
                };
                if c > 0 {
                    segs.push(seg(Paint::Plain, " ".repeat(gap)));
                }
                segs.extend(entry(p, widths[c]));
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

/// The package pacman was installing when it printed line `at`.
fn package_at(run: &Run, at: usize) -> Option<String> {
    run.lines
        .get(..at)?
        .iter()
        .rev()
        .find_map(|l| match classify_screen(l, false) {
            ScreenEvent::Package { name, .. } => Some(name),
            ScreenEvent::PreHooks | ScreenEvent::PostHooks | ScreenEvent::Processing => {
                Some(String::new())
            }
            _ => None,
        })
        .filter(|n| !n.is_empty())
}

/// `t` broken at spaces into lines of at most `w` columns, each after the
/// first indented two; a word longer than a line stands alone.
fn wrap_words(t: &str, w: usize) -> Vec<String> {
    if t.width() <= w || w < 20 {
        return vec![t.to_string()];
    }
    let lead = t.len() - t.trim_start().len();
    let mut out = vec![t[..lead].to_string()];
    for word in t.split_whitespace() {
        let Some(cur) = out.last_mut() else { break };
        let fresh = cur.trim().is_empty();
        if !fresh && cur.width() + 1 + word.width() > w {
            out.push(format!("  {word}"));
        } else {
            if !fresh {
                cur.push(' ');
            }
            cur.push_str(word);
        }
    }
    out
}

/// `Warning:` painted as pacman paints its own `warning:`, when nothing
/// coloured it yet (systemd's, from a scriptlet).
fn warning_mark(l: &str, s: &Styles) -> String {
    match l.strip_prefix("Warning:") {
        Some(rest) if s.colored() => format!("{}{rest}", s.warn("Warning:")),
        _ => l.to_string(),
    }
}

/// `t` wrapped to `w`, breaking first at sentence ends (`. `) so a
/// message reads one sentence a line; a sentence too long still wraps.
fn sentences(t: &str, w: usize) -> Vec<String> {
    if t.width() <= w {
        return vec![t.to_string()];
    }
    let lead = &t[..t.len() - t.trim_start().len()];
    t.trim_start()
        .split_inclusive(". ")
        .flat_map(|x| wrap_flat(x.trim_end(), w.saturating_sub(lead.len())))
        .map(|r| format!("{lead}{r}"))
        .collect()
}

/// The command a message asks for, bold where it stands: the text of
/// `Run '…'`, or a line's `$ …`. A line pacman coloured stays as it is.
fn command_bold(l: &str, s: &Styles) -> String {
    if !s.colored() || l.contains('\u{1b}') && !l.contains("Run '") {
        return l.to_string();
    }
    if let Some((a, r)) = l.split_once("Run '")
        && let Some((c, z)) = r.split_once('\'')
    {
        return format!("{a}Run '{}'{z}", s.title(c));
    }
    let t = l.trim_start();
    match t.strip_prefix("$ ") {
        Some(c) => format!("{}$ {}", &l[..l.len() - t.len()], s.title(c)),
        None => l.to_string(),
    }
}

/// `t` wrapped to `w` at spaces, every row flush left.
fn wrap_flat(t: &str, w: usize) -> Vec<String> {
    wrap_words(t, w)
        .into_iter()
        .map(|r| r.trim_start().to_string())
        .collect()
}

/// pacman's `warning: NAME: local (A) is newer than REPO (B)` in a quiet
/// form that asks nothing: `≥ newer than repo  NAME  A › REPO B`.
fn newer_than_repo(l: &str, pad: usize, s: &Styles) -> Option<String> {
    let rest = l.strip_prefix("warning: ")?;
    let (name, rest) = rest.split_once(": local (")?;
    let (local, rest) = rest.split_once(") is newer than ")?;
    let (repo, rest) = rest.split_once(" (")?;
    let theirs = rest.strip_suffix(')')?;
    Some(format!(
        "{} {}",
        s.dim(g(s, "≥", ">=")),
        s.dim(&format!(
            "newer than repo  {name:<pad$}  {local} {} {repo} {theirs}",
            g(s, "›", ">")
        ))
    ))
}

/// Does the package message at line `at` start its block (no earlier line
/// of the same package's message), and does any line of the block ask the
/// user for something: a warning, a command to run, "make sure"?
fn message_block(run: &Run, at: usize, whose: &str, from: usize) -> (bool, bool) {
    let mine: Vec<usize> = run
        .unknown_at
        .iter()
        .chain(&run.warning_at)
        .copied()
        .filter(|&i| i >= from && package_at(run, i).as_deref() == Some(whose))
        .collect();
    let first = !mine.iter().any(|&i| i < at);
    let asks = mine
        .iter()
        .filter_map(|&i| run.lines.get(i))
        .any(|l| asks_user(l));
    (first, asks)
}

/// A message line that asks the user for something, by its own words.
fn asks_user(l: &str) -> bool {
    let low = l.to_lowercase();
    low.contains("warning") || low.contains("run '") || low.contains("make sure") || {
        let t = l.trim_start();
        t.starts_with("$ ") || t.starts_with("# ")
    }
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
    let mine: Vec<&String> = all
        .into_iter()
        .filter(|(at, _)| *at >= from && in_hooks(run, *at))
        .map(|(_, l)| l)
        .collect();
    let mut tail: Vec<&String> = mine.iter().rev().take(n).rev().copied().collect();
    // A hook that builds one thing after another (an initramfs per kernel)
    // repeats the same steps: keep the latest heading that says which.
    let heading = mine
        .iter()
        .rev()
        .find(|l| l.contains("==> Starting build:") || l.contains("Building initramfs"));
    if let Some(h) = heading
        && !tail.contains(h)
        && n > 1
    {
        tail.remove(0);
        tail.insert(0, h);
    }
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
            // Package messages hang off one name column: the name once per
            // message, right-aligned, then a gutter in the message's tier.
            let col = run
                .packages
                .iter()
                .map(|p| p.name.width())
                .max()
                .unwrap_or(0)
                .min(24)
                .min(w / 4);
            // Names in `newer than repo` lines share one width.
            let newer_w = run
                .lines
                .iter()
                .filter_map(|l| {
                    let r = l.strip_prefix("warning: ")?;
                    let (n, r) = r.split_once(": local (")?;
                    r.contains(") is newer than ").then(|| n.width())
                })
                .max()
                .unwrap_or(0);
            for (at, l) in new {
                if let Some(t) = run
                    .lines
                    .get(at)
                    .and_then(|p| newer_than_repo(p, newer_w, s))
                {
                    above.push(t);
                    continue;
                }
                // A package's own message says whose it is.
                let whose = (at >= quiet_from).then(|| package_at(run, at)).flatten();
                let Some(whose) = whose else {
                    // Plain text wraps at spaces under its own start; a line
                    // pacman coloured goes up as it is.
                    let body = if l.contains('\u{1b}') {
                        vec![l.clone()]
                    } else {
                        wrap_words(l, w)
                    };
                    for (i, b) in body.iter().enumerate() {
                        above.push(if i == 0 {
                            warning_mark(b, s)
                        } else {
                            b.clone()
                        });
                    }
                    continue;
                };
                let (first, asks) = message_block(run, at, &whose, quiet_from);
                let gutter = if asks {
                    s.warn(g(s, "▎", "|"))
                } else {
                    s.dim(g(s, "▎", "|"))
                };
                let pad = " ".repeat(col);
                if first && whose.width() > col {
                    above.push(whose.clone());
                }
                let body = if l.contains('\u{1b}') {
                    vec![l.clone()]
                } else {
                    sentences(l.trim_end(), w.saturating_sub(col + 3))
                };
                for (i, b) in body.iter().enumerate() {
                    let lead = if first && i == 0 && whose.width() <= col {
                        format!("{whose:>col$}")
                    } else {
                        pad.clone()
                    };
                    let b = if i == 0 {
                        warning_mark(b, s)
                    } else {
                        b.clone()
                    };
                    above.push(format!("{lead} {gutter} {}", command_bold(&b, s)));
                }
            }
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
                    let mut r = compact_plan(run, w, height.saturating_sub(3), s);
                    // A blank row, then pacman's question as it asked it,
                    // bold, its answers amber: the one thing to do now.
                    r.push(String::new());
                    r.push(match prompt.rsplit_once('[') {
                        Some((q, a)) if !q.contains('\u{1b}') => {
                            format!("{}{}", s.title(q), s.warn(&format!("[{a}")))
                        }
                        _ => prompt,
                    });
                    r
                }
                // Verbatim: a cut prompt can lose its answer choices.
                _ => rows.to_vec(),
            }
        }
        None => live(run, view, w, height, now, s),
    };
    let cursor = idle_prompt.is_some();
    (
        Frame {
            above,
            region,
            cursor,
        },
        d,
    )
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
    closing_at(run, view, width, transcript, s, None)
}

/// The receipt's way in, on success with colour: Pac-Man eats the time
/// ribbon left to right over the run's real phase times, each legend item
/// arriving as it is reached; the last frame is the receipt itself. Drawn
/// once the run is over, so it shows no progress that is not done.
pub fn closing_frames(
    run: &Run,
    view: &View,
    width: u16,
    transcript: &str,
    s: &Styles,
) -> Vec<Vec<String>> {
    // Nothing to eat without a ribbon: a phase of a second or more.
    let timed = run
        .phases
        .iter()
        .any(|p| took(p.started, p.ended, Duration::ZERO).is_some_and(|t| t.as_secs() >= 1));
    if !s.colored() || run.outcome() != Outcome::Succeeded || !timed {
        return Vec::new();
    }
    (1..=EAT_IN)
        .map(|i| {
            let f = i as f64 / EAT_IN as f64;
            closing_at(run, view, width, transcript, s, Some(f))
        })
        .collect()
}

/// Frames in the receipt's eat-in.
pub const EAT_IN: usize = 8;

fn closing_at(
    run: &Run,
    view: &View,
    width: u16,
    transcript: &str,
    s: &Styles,
    reveal: Option<f64>,
) -> Vec<String> {
    let w = usize::from(width);
    // Set apart from pacman's own last lines, under the command it is for.
    let mut out = vec![
        String::new(),
        line(&[seg(Paint::Dim, run.command.clone())], w, s),
        line(&end_segs(run, view.motif, s), w, s),
    ];
    // Where the time went: each phase that took a second or more.
    let spans: Vec<(String, Duration, PhaseKind)> = run
        .phases
        .iter()
        .filter_map(|p| {
            let t = took(p.started, p.ended, Duration::ZERO)?;
            (t.as_secs() >= 1).then(|| (format!("{} {}", name(p.kind), duration(t)), t, p.kind))
        })
        .collect();
    if run.outcome() == Outcome::Succeeded && !spans.is_empty() {
        out.extend(ribbon(&spans, w, reveal, s));
    }
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
    let texts = |want: &dyn Fn(&NoteKind) -> bool| -> Vec<String> {
        run.needs_you()
            .into_iter()
            .filter(|n| want(&n.kind))
            .map(|n| n.text.clone())
            .collect()
    };
    // Three tiers. To do: a command or step the run's own output asked for,
    // with whose words they were. Good to know: records and notes that ask
    // nothing. Quiet: counts of what stays in scrollback and the transcript.
    let mut todo: Vec<(String, String, String)> = Vec::new();
    let mut know: Vec<(String, String)> = Vec::new();
    let mut quiet: Vec<String> = Vec::new();
    if let Some(t) = texts(&|k| *k == NoteKind::Reboot).first() {
        // The hook's own reason, when it gives one.
        // The hook's own words.
        todo.push((
            "reboot".into(),
            sentence_head(t),
            hook_of(run, t).unwrap_or_default(),
        ));
    }
    let rebuild: Vec<String> = texts(&|k| *k == NoteKind::Rebuild)
        .iter()
        .filter_map(|t| t.split_whitespace().last().map(str::to_string))
        .collect();
    if !rebuild.is_empty() {
        todo.push(("rebuild".into(), rebuild.join(", "), String::new()));
    }
    // "local is newer than repo" was shown before the plan and asks nothing.
    let newer = texts(&|k| *k == NoteKind::Warning)
        .iter()
        .filter(|w| w.contains(" is newer than "))
        .count();
    for w in texts(&|k| *k == NoteKind::Warning)
        .into_iter()
        .filter(|w| !w.contains(" is newer than "))
    {
        let whose = run
            .lines
            .iter()
            .position(|l| l.contains(w.as_str()))
            .and_then(|at| package_at(run, at))
            .unwrap_or_default();
        let (cmd, why) = action(&w);
        todo.push((cmd, why, whose));
    }
    let pacnew = texts(&|k| *k == NoteKind::Pacnew);
    if !pacnew.is_empty() {
        todo.push((".pacnew".into(), pacnew.join(", "), String::new()));
    }
    // Package messages: a `$ command` line is a step to do, in the
    // package's words; a message that asks nothing is good to know.
    let from = transaction_from(run).unwrap_or(usize::MAX);
    let mut said: Vec<(String, Vec<&String>)> = Vec::new();
    for (at, l) in run.unknown_at.iter().zip(&run.unknown) {
        if *at < from || in_hooks(run, *at) || l.trim().is_empty() {
            continue;
        }
        let Some(p) = package_at(run, *at) else {
            continue;
        };
        match said.iter_mut().find(|x| x.0 == p) {
            Some(x) => x.1.push(l),
            None => said.push((p, vec![l])),
        }
    }
    for (p, lines) in &said {
        let mut asked = false;
        for (i, l) in lines.iter().enumerate() {
            let t = l.trim_start();
            if let Some(cmd) = t.strip_prefix("$ ").or_else(|| t.strip_prefix("# ")) {
                let why = i
                    .checked_sub(1)
                    .and_then(|j| lines.get(j))
                    .map(|w| {
                        // The step itself is the command; its lead-in
                        // ("… by running the following command") is not.
                        let w = w.trim().trim_end_matches(':');
                        [
                            " by running the following",
                            " using the following",
                            " with the following",
                        ]
                        .iter()
                        .find_map(|x| w.split_once(x).map(|(a, _)| a))
                        .unwrap_or(w)
                        .to_string()
                    })
                    .unwrap_or_default();
                todo.push((cmd.trim().to_string(), why, p.clone()));
                asked = true;
            }
        }
        if !asked && !lines.iter().any(|l| asks_user(l)) {
            // Whole above, in scrollback; here its start, to recognise it.
            let text: Vec<&str> = lines.iter().map(|l| l.trim()).collect();
            know.push((p.clone(), text.join(" ")));
        }
    }
    let opt = texts(&|k| *k == NoteKind::Optdepends);
    if !opt.is_empty() {
        know.push(("optional".into(), opt.join("; ")));
    }
    let retried = texts(&|k| *k == NoteKind::Error { recovered: true });
    for t in &retried {
        // Keyed by the package, like the other rows: `luajit  retried · …`.
        let r = retry(run, t, s.bullet());
        let sep = format!(" {} ", s.bullet());
        know.push(match r.split_once(&sep) {
            Some((p, rest)) => (p.to_string(), format!("retried{sep}{rest}")),
            None => ("retried".into(), r),
        });
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
        know.push(("snap-pac".into(), list.join(", ")));
    }
    if newer > 0 {
        quiet.push(format!("{newer} newer than repo"));
    }
    let n = run.unknown.len();
    if n > 0 {
        let lines = if n == 1 { "line" } else { "lines" };
        quiet.push(format!(
            "{n} more {lines} of output {} paclens transcript",
            s.bullet()
        ));
    }
    // Not cut to the width: the receipt stays on screen, and a cut line loses
    // the part that says what to do. The terminal wraps it.
    let todo_mark = if arcade {
        paint(Paint::Ghost(Ghost::Clyde), g(s, "ᗣ", "G"), s)
    } else {
        s.warn(g(s, "▲", "!"))
    };
    // Wide screens: to do on the left, the notes and counts beside it.
    // Only when both sides fit uncut: a second column that cuts what one
    // column shows whole is worse than stacking.
    let need_left = {
        let cw = todo.iter().map(|t| t.0.width()).max().unwrap_or(0).min(28);
        let nw = todo.len().to_string().len();
        let why = todo.iter().map(|t| t.1.width()).max().unwrap_or(0);
        let src = todo.iter().map(|t| t.2.width() + 4).max().unwrap_or(0);
        4 + nw + 1 + cw + 2 + why + src
    };
    let need_right = {
        let kw = know.iter().map(|k| k.0.width()).max().unwrap_or(0).min(24);
        let notes = know.iter().map(|k| 8 + kw + k.1.width()).max().unwrap_or(0);
        let b = format!(" {} ", s.bullet());
        notes.max(11 + quiet.join(&b).width())
    };
    let two = !todo.is_empty()
        && !(know.is_empty() && quiet.is_empty())
        && need_left + 4 + need_right <= w;
    let tw = w;
    let mut left: Vec<String> = Vec::new();
    let mut right: Vec<String> = Vec::new();
    if !todo.is_empty() || !know.is_empty() || !quiet.is_empty() {
        out.push(String::new());
    }
    if !todo.is_empty() {
        // The end line says there is something to do, and how much.
        if let Some(end) = out.get_mut(2) {
            *end += &format!(
                "{}{}",
                s.dim(&format!(" {} ", s.bullet())),
                s.warn(&format!("{} {} to do", g(s, "▲", "!"), todo.len()))
            );
        }
        left.push(format!("  {todo_mark} {}", s.title("to do")));
        // A reboot ends the session: it goes last, after the steps that
        // can be done before it.
        todo.sort_by_key(|t| t.0 == "reboot");
        let cw = todo.iter().map(|t| t.0.width()).max().unwrap_or(0).min(28);
        let nw = todo.len().to_string().len();
        // Columns: number, command, why, source. Why wraps inside its own
        // column; a source that does not fit after it takes its own row.
        let ind = 4 + nw + 1 + cw + 2;
        let avail = tw.saturating_sub(ind).max(20);
        let sw = todo.iter().map(|t| t.2.width() + 4).max().unwrap_or(0);
        let ww = todo
            .iter()
            .map(|t| t.1.width())
            .max()
            .unwrap_or(0)
            .min(avail.saturating_sub(sw));
        // Wide enough: one row each, the reason cut with `…` (its source
        // is whole above), sources in one column. Too narrow for that: the
        // reason wraps in its column and every source goes under it.
        let one_row = ww >= 30;
        for (i, (cmd, why, whose)) in todo.iter().enumerate() {
            let src = (!whose.is_empty()).then(|| format!("{} {whose}", g(s, "←", "<-")));
            let lead = format!(
                "    {} {}{}",
                s.dim(&format!("{:>nw$}", i + 1)),
                s.title(cmd),
                " ".repeat(cw.saturating_sub(cmd.width()) + 2)
            );
            let mut rows = if one_row {
                let why = cut_words(why, ww);
                vec![match &src {
                    Some(src) => format!("{why}{}  {}", " ".repeat(ww - why.width()), s.dim(src)),
                    None => why,
                }]
            } else {
                let mut r = wrap_flat(why, avail);
                r.extend(src.iter().map(|x| s.dim(x)));
                r
            };
            rows.retain(|r| !r.is_empty());
            for (k, r) in rows.iter().enumerate() {
                let head = if k == 0 {
                    lead.clone()
                } else {
                    " ".repeat(ind)
                };
                left.push(format!("{head}{r}"));
            }
        }
    }
    // The right block starts four columns after the left one's widest row.
    let at = left.iter().map(|l| visible_width(l)).max().unwrap_or(0) + 4;
    let nw_ = if two { w.saturating_sub(at) } else { w };
    let side = if two { &mut right } else { &mut left };
    if !know.is_empty() {
        side.push(format!("  {} {}", s.dim(s.bullet()), s.dim("good to know")));
        let kw = know.iter().map(|k| k.0.width()).max().unwrap_or(0).min(24);
        for (k, t) in &know {
            let t = preview(t, nw_.saturating_sub(8 + kw));
            // A path in the note reads plain: the part to act on.
            let path = t
                .split_whitespace()
                .find(|x| x.contains('/') && !x.contains("://"))
                .map(|x| x.trim_end_matches(['.', ',', ':', ';']));
            let t = match path.and_then(|p| t.split_once(p).map(|(a, z)| (a, p, z))) {
                Some((a, p, z)) => format!("{}{p}{}", s.dim(a), s.dim(z)),
                None => s.dim(&t),
            };
            side.push(format!("      {k:<kw$}  {t}"));
        }
    }
    if !quiet.is_empty() {
        let b = format!(" {} ", s.bullet());
        side.push(s.dim(&format!("  {} quiet  {}", s.bullet(), quiet.join(&b))));
    }
    if two {
        for i in 0..left.len().max(right.len()) {
            let l = left.get(i).cloned().unwrap_or_default();
            let pad = at.saturating_sub(visible_width(&l));
            let r = right.get(i).cloned().unwrap_or_default();
            out.push(format!("{l}{}{r}", " ".repeat(pad)).trim_end().to_string());
        }
    } else {
        out.extend(left);
    }
    out
}

/// Where the time went. With colour, a solid ribbon as wide as the bars:
/// one segment per phase, its length the phase's share of the run, coloured
/// by what the phase is (network cyan, packages green, hooks magenta, the
/// rest dim), and a legend under it. Without colour, or too narrow, the
/// labels on one row.
fn ribbon(
    spans: &[(String, Duration, PhaseKind)],
    w: usize,
    reveal: Option<f64>,
    s: &Styles,
) -> Vec<String> {
    let b = format!(" {} ", s.bullet());
    let flat = || {
        let all: Vec<&str> = spans.iter().map(|x| x.0.as_str()).collect();
        vec![line(
            &[seg(Paint::Dim, format!("  {}", all.join(&b)))],
            w,
            s,
        )]
    };
    // The bars end five columns from the edge, before their percent.
    let cells = w.saturating_sub(7);
    if !s.colored() || cells < 30 {
        return flat();
    }
    let total: f64 = spans.iter().map(|x| x.1.as_secs_f64()).sum();
    let mut widths: Vec<usize> = spans
        .iter()
        .map(|x| ((x.1.as_secs_f64() / total * cells as f64).round() as usize).max(2))
        .collect();
    // Rounding may overshoot or fall short; the longest segment evens it.
    let sum: usize = widths.iter().sum();
    if let Some(m) = widths.iter_mut().max() {
        *m = (*m + cells).saturating_sub(sum).max(1);
    }
    // A solid ribbon, then one legend row in run order: each phase's
    // swatch in its ribbon colour, its name and time.
    let paint_kind = |k: PhaseKind, t: &str| match kind_paint(k) {
        Paint::Warn => s.dim(t),
        p => paint(p, t, s),
    };
    // Cell by cell: each phase's colour; two touching segments of one
    // colour get a gap cell at the seam.
    let mut cell: Vec<(usize, bool)> = Vec::new();
    for (i, (cw, x)) in widths.iter().zip(spans).enumerate() {
        let seam = i > 0 && kind_paint(spans[i - 1].2) == kind_paint(x.2);
        for c in 0..*cw {
            cell.push((i, !(seam && c == 0 && *cw > 1)));
        }
    }
    // Eating in: cells up to Pac-Man drawn, pellets ahead.
    let upto = reveal.map_or(cell.len(), |f| (f * cell.len() as f64) as usize);
    let mut bar = String::from("  ");
    for (j, &(i, fill)) in cell.iter().enumerate() {
        bar += &if j < upto {
            if fill {
                paint_kind(spans[i].2, g(s, "━", "="))
            } else {
                " ".to_string()
            }
        } else if j == upto {
            s.warn(g(s, "ᗧ", "C"))
        } else if (cell.len() - 1 - j).is_multiple_of(3) {
            s.dim(g(s, "·", "."))
        } else {
            " ".to_string()
        };
    }
    // A legend item arrives once Pac-Man reaches its segment.
    let start = |i: usize| widths[..i].iter().sum::<usize>();
    let spans: Vec<&(String, Duration, PhaseKind)> = spans
        .iter()
        .enumerate()
        .filter(|(i, _)| start(*i) < upto.max(1))
        .map(|(_, x)| x)
        .collect();
    let swatch = g(s, "■", "#");
    let items: Vec<(String, usize)> = spans
        .iter()
        .map(|x| {
            (
                format!("{} {}", paint_kind(x.2, swatch), s.dim(&x.0)),
                2 + x.0.width(),
            )
        })
        .collect();
    // As few legend rows as fit, the items shared evenly between them, so
    // no row holds one stray item.
    let fits = |k: usize| {
        let per = items.len().div_ceil(k);
        items
            .chunks(per.max(1))
            .all(|c| 2 + c.iter().map(|i| i.1 + 2).sum::<usize>() <= w)
    };
    let k = (1..=items.len()).find(|&k| fits(k)).unwrap_or(items.len());
    let mut out = vec![bar];
    for c in items.chunks(items.len().div_ceil(k).max(1)) {
        let row: Vec<&str> = c.iter().map(|i| i.0.as_str()).collect();
        out.push(format!("  {}", row.join("  ")));
    }
    out
}

/// A warning as the thing to do: `systemctl daemon-reload` and why,
/// `warp-svc.service changed on disk`, when it names a command to run and a
/// unit; else `warning` and the warning whole.
fn action(w: &str) -> (String, String) {
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
        (Some(c), Some(_)) => (c.to_string(), sentence_head(w)),
        _ => ("warning".to_string(), w.to_string()),
    }
}

/// `t` in `w` columns: whole when it fits; else its start and, when it
/// names a path, that path whole (`You can add … · .config/x/flags.conf`).
fn preview(t: &str, w: usize) -> String {
    if t.width() <= w {
        return t.to_string();
    }
    let path = t
        .split_whitespace()
        .find(|x| x.contains('/') && !x.contains("://"))
        .map(|x| x.trim_end_matches(['.', ',', ':', ';']));
    match path {
        Some(p) if p.width() + 12 < w => {
            let head = t.split_once(p).map_or(t, |(a, _)| a).trim_end();
            format!("{} {p}", cut_words(head, w - p.width() - 1))
        }
        _ => cut_words(t, w),
    }
}

/// A message's first sentence, less its leading marks (`==> INFO:`,
/// `Warning:`) and closing stop: `Reboot is recommended due to …`.
fn sentence_head(t: &str) -> String {
    let mut t = t.trim();
    for m in ["==> ", "-> ", ":: ", "INFO: ", "Warning: ", "warning: "] {
        t = t.strip_prefix(m).unwrap_or(t);
    }
    let first = t.split_once(". ").map_or(t, |(a, _)| a);
    first.trim_end_matches('.').to_string()
}

/// The hook whose output carried note text `t`: its description, else its
/// file name.
fn hook_of(run: &Run, t: &str) -> Option<String> {
    run.pre_hooks
        .iter()
        .chain(&run.post_hooks)
        .find(|h| h.notes.iter().any(|n| n.text == t))
        .and_then(|h| h.file.clone().or_else(|| h.desc.clone()))
        .map(|d| d.trim_end_matches(".hook").to_string())
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
    let took = duration(elapsed(run, Duration::ZERO));
    let eaten = run.packages.iter().filter(|p| p.done).count();
    let b = s.bullet();
    let ((p, mark), text) = match run.outcome() {
        Outcome::Succeeded => {
            let total = |name: &str| run.totals.iter().find(|t| t.0 == name).map(|t| &t.1);
            // What came down and how fast on average, then what it did to
            // the disk: pacman's own figures.
            let dl = phase(run, PhaseKind::Download)
                .and_then(|p| p.ended.zip(p.started).map(|(e, s)| e.saturating_sub(s)))
                .filter(|t| t.as_secs() > 0);
            let mut got = total("Total Download Size")
                .map(|t| {
                    let at = dl
                        .zip(bytes(t))
                        .map(|(d, b)| {
                            format!(" at {}", rate(&format!("{} B", b / d.as_secs_f64())))
                        })
                        .unwrap_or_default();
                    format!(" {b} {} downloaded{at}", size_pair(t, None))
                })
                .unwrap_or_default();
            if let Some(n) = total("Net Upgrade Size") {
                let n = tidy_size(n);
                let sign = if n.starts_with('-') { "" } else { "+" };
                got += &format!(" {b} {sign}{n} on disk");
            }
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
    vec![seg(p, mark), seg(Paint::Bold, text)]
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

/// How the head of a bar looks: chewing while the figure moves; shut and
/// dim while a file waits for bytes, with a ghost behind it once that is a
/// stall; on an estimate (a hook's time against its usual time) a dashed
/// track and hollow pellets, never chewing, and past the usual time shut at
/// the end of the track with the ghost behind.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mouth {
    Chew,
    Wait,
    Haunt,
    Guess,
    Over,
}

/// The run's bar, `cells + 2` columns wide. Without colour: pacman's own,
/// in brackets. With colour, the maze: the eaten track a thin dim line,
/// Pac-Man `ᗧ` at the real position in half cells — open and shut on
/// alternate half steps, so it chews only as bytes arrive — pellets `·`
/// ahead every third cell as pacman spaces them, and a power pellet `•` on
/// the file's last byte.
fn track(pct: u8, cells: usize, mouth: Mouth, s: &Styles) -> String {
    comet(pct, cells, mouth, None, s)
}

/// `track` with a comet tail: the cells eaten since the file stood at
/// `from` percent (two seconds ago, as seen) drawn heavy and amber — a
/// long tail is a fast file, none is a still one. Measured, not smoothed.
fn comet(pct: u8, cells: usize, mouth: Mouth, from: Option<u8>, s: &Styles) -> String {
    if !s.colored() {
        return match mouth {
            Mouth::Over => format!("[{}c]", "-".repeat(cells.saturating_sub(1))),
            _ => format!("[{}]", bar(pct, cells, true, s)),
        };
    }
    let n = cells + 2;
    let guess = matches!(mouth, Mouth::Guess | Mouth::Over);
    let line = if guess {
        g(s, "╌", "-")
    } else {
        g(s, "─", "-")
    };
    let (pellet, power) = if guess {
        (g(s, "◦", "."), g(s, "○", "o"))
    } else {
        (g(s, "·", "."), g(s, "•", "o"))
    };
    let ghost = || s.ghost(Ghost::Pinky, g(s, "ᗣ", "G"));
    let shut = || s.dim(g(s, "●", "o"));
    if mouth == Mouth::Over {
        return s.dim(&line.repeat(n.saturating_sub(3))) + &ghost() + &s.dim(line) + &shut();
    }
    let pct = usize::from(pct.min(100));
    let halves = pct * n * 2 / 100;
    let at = (halves / 2).min(n);
    // All eaten: the power pellet's cell becomes the done mark.
    if at >= n {
        return s.dim(&line.repeat(n.saturating_sub(1))) + &paint(Paint::Ok, s.check(), s);
    }
    let head = match mouth {
        Mouth::Wait | Mouth::Haunt => shut(),
        // Shut one half step in four: a chew, still read as Pac-Man.
        Mouth::Chew if halves % 4 == 3 => s.warn(g(s, "●", "o")),
        _ => s.warn(g(s, "ᗧ", "C")),
    };
    let eaten = if mouth == Mouth::Haunt && at >= 3 {
        s.dim(&line.repeat(at - 2)) + &ghost() + &s.dim(line)
    } else if let Some(f) = from.filter(|_| mouth == Mouth::Chew) {
        let mut tail_at = (usize::from(f.min(100)) * n * 2 / 100 / 2).min(at);
        // At most six cells: past that a long tail says more about the
        // file's size than its speed.
        tail_at = tail_at.max(at.saturating_sub(6));
        // Bytes moved but under a cell: still one cell of tail.
        if f < pct as u8 && tail_at == at && at > 0 {
            tail_at = at - 1;
        }
        s.dim(&line.repeat(tail_at)) + &s.warn(&g(s, "━", "=").repeat(at - tail_at))
    } else {
        s.dim(&line.repeat(at))
    };
    let mut dots = String::new();
    for j in at + 1..n.saturating_sub(1) {
        dots.push_str(if j > at + 1 && (n - 1 - j).is_multiple_of(3) {
            pellet
        } else {
            " "
        });
    }
    let end = if at + 1 < n { power } else { "" };
    eaten + &head + &s.dim(&dots) + end
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
            // A blank row and the command set the receipt apart.
            assert_eq!(c[..2], ["", "pacman -Syu"], "{c:?}");
            assert!(c[2].contains("38 eaten"), "{c:?}");
            assert!(c.len() <= 14, "{c:?}");
        }
    }

    #[test]
    fn receipt_keeps_actionable_warnings_whole() {
        let mut r = fixture_run();
        let w = "Warning: The unit file of warp-svc.service changed on disk. Run 'systemctl daemon-reload' to reload units.";
        r.apply_line(w, Z);
        let c = closing(
            &r,
            &view(Layout::Inline, Motif::Subtle),
            120,
            "/t",
            &plain(),
        );
        // The thing to do, with what it is about.
        assert!(
            c.iter().any(|l| l.contains("systemctl daemon-reload")
                && l.contains("warp-svc.service changed on disk")),
            "{c:?}"
        );
        // A warning that names no command stays whole.
        let mut r = fixture_run();
        let odd = "Warning: something odd happened and nothing says what to run about it.";
        r.apply_line(odd, Z);
        let c = closing(
            &r,
            &view(Layout::Inline, Motif::Subtle),
            120,
            "/t",
            &plain(),
        );
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
        assert_eq!(f.above, [">= newer than repo  foo  2-1 > extra 1-1"]);
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
        assert_eq!(c[2], "✗ pacman exit 1");
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
    fn install_message_names_its_package() {
        let mut r = partial(60);
        r.apply_line(":: Processing package changes...", Z);
        r.apply_line("(1/2) upgrading ydotool     [----] 100%", Z);
        r.apply_line("Make sure your user is in the input group", Z);
        let at = r.lines.len() - 1;
        assert_eq!(package_at(&r, at).as_deref(), Some("ydotool"));
        r.apply_line(":: Running post-transaction hooks...", Z);
        assert_eq!(package_at(&r, r.lines.len()), None);
    }

    #[test]
    fn long_message_wraps_at_spaces_under_itself() {
        let t = "Warning: The unit file of warp-svc.service changed on disk. Run 'systemctl daemon-reload' to reload units.";
        let out = wrap_words(t, 40);
        assert!(out.iter().all(|l| l.width() <= 40), "{out:?}");
        assert!(out[1..].iter().all(|l| l.starts_with("  ")), "{out:?}");
        let joined: Vec<&str> = out.iter().flat_map(|l| l.split_whitespace()).collect();
        assert_eq!(joined, t.split_whitespace().collect::<Vec<_>>());
        assert_eq!(
            wrap_words("  $ usermod -aG input", 40),
            ["  $ usermod -aG input"]
        );
    }

    #[test]
    fn version_cut_keeps_the_change() {
        // Shared front goes first.
        assert_eq!(
            cut_version("2026.8.2110-1", Some("2026.8.2115-1"), 11),
            "…6.8.2110-1"
        );
        // Nothing shared past the first component: the end goes, the 155 stays.
        assert_eq!(
            cut_version("155.0.8059.84-1", Some("156.0.8078.47-1"), 11),
            "155.0.8059…"
        );
        assert_eq!(cut_version("1.2-1", Some("1.3-1"), 11), "1.2-1");
        assert_eq!(cut_version("1:20260916-1", None, 10), "1:2026091…");
    }

    #[test]
    fn plain_warning_gets_pacmans_colour() {
        let t = "Warning: unit changed";
        assert_eq!(warning_mark(t, &plain()), t);
        let c = warning_mark(t, &colour());
        assert!(c.contains('\u{1b}') && strip(&c) == t, "{c:?}");
    }

    #[test]
    fn signal_failure_says_so() {
        let mut r = empty();
        r.finish(None, Z);
        let c = closing(&r, &view(Layout::Nom, Motif::Arcade), 80, "/t", &uni());
        assert!(c[2].contains("pacman stopped by a signal"), "{c:?}");
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
        assert!(f.region[0].starts_with("pacman -Syu · 2m00s"), "{t}");
        assert!(t.contains("download"), "{t}");
        assert!(t.contains("13/38"), "{t}");
        assert!(t.contains("openssh"), "{t}");
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
                .any(|l| l.contains("12s · usually 23s") && l.ends_with("~52%")),
            "{:?}",
            f.region
        );
    }

    #[test]
    fn hook_past_its_median_counts_the_overrun() {
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
            secs(30),
            None,
            &uni(),
        )
        .0;
        let t = f.region.join("\n");
        assert!(t.contains("+4s") && !t.contains("~99%"), "{t}");
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
        // name, old, ` -> ` (plain arrow), new; columns 3 apart.
        let cell = 5 + 1 + 5 + 4 + 5;
        assert_eq!(rows_under_heading(cell), 20);
        assert_eq!(rows_under_heading(2 * cell + 3), 10);
        assert_eq!(rows_under_heading(3 * cell + 6), 7);
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
        // 43 columns: one full entry (30 wide) per row would need 14 rows;
        // without sizes two (20 wide) fit side by side.
        let out = compact_plan(&r, 43, 9, &plain());
        let t = out.join("\n");
        assert!(!t.contains("more"), "{t}");
        // Sizes left the rows; the repo heading keeps the total.
        assert!(!out[2..].join("\n").contains("MiB"), "{t}");
        assert!(out[1].ends_with("12.0 MiB"), "{t}");
        assert!(t.contains("1.0-1 -> 1.1-1"), "{t}");
        // Roomy: everything, sizes too.
        let t = compact_plan(&r, 40, 40, &plain()).join("\n");
        assert!(t.contains("1.0 MiB"), "{t}");
    }

    #[test]
    fn names_only_columns_take_their_own_width() {
        let mut r = empty();
        for n in ["a-very-long-package-x", "b", "c", "d"] {
            r.packages.push(crate::update_view::model::Pkg {
                name: n.into(),
                repo: Some("extra".into()),
                ..Default::default()
            });
        }
        // One shared width (21) would fit one column in 30; each column at
        // its own width fits all four on one row, nothing cut.
        let out = compact_plan(&r, 30, 3, &plain());
        assert_eq!(out.len(), 3, "{out:?}");
        assert_eq!(out[2].trim_end(), "a-very-long-package-x  b  c  d");
        assert_eq!(split(40, 3), [(0, 14), (14, 13), (27, 13)]);
    }

    #[test]
    fn families_fold_into_one_entry() {
        let pkgs: Vec<crate::update_view::model::Pkg> = ["fw-a", "fw-b", "fw-c", "zed", "x-1"]
            .iter()
            .map(|n| crate::update_view::model::Pkg {
                name: (*n).into(),
                new: Some("2-1".into()),
                old: Some(if *n == "fw-c" { "0-1" } else { "1-1" }.into()),
                download: Some("1.00 MiB".into()),
                ..Default::default()
            })
            .collect();
        let refs: Vec<&crate::update_view::model::Pkg> = pkgs.iter().collect();
        // Versions differ: folded only when anything may fold.
        let (singles, fams) = fold_families(&refs, false);
        assert_eq!((singles.len(), fams.len()), (5, 0));
        let (singles, fams) = fold_families(&refs, true);
        let names: Vec<&str> = singles.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["zed", "x-1"]);
        assert_eq!(fams[0].prefix, "fw-");
        assert_eq!(fams[0].members, ["a", "b", "c"]);
        assert_eq!(fams[0].new.as_deref(), Some("2-1"));
        assert_eq!(fams[0].old, None);
        assert_eq!(fams[0].size.as_deref(), Some("3.0 MiB"));
    }

    #[test]
    fn family_wraps_its_members_under_the_brace() {
        let f = Family {
            prefix: "linux-firmware-".into(),
            members: ["amd", "amdgpu", "atheros", "intel", "nvidia"]
                .map(String::from)
                .to_vec(),
            old: None,
            new: None,
            size: None,
        };
        let rows: Vec<String> = family_rows(&f, vec![seg(Paint::Plain, "1-1")], 32)
            .iter()
            .map(|r| line(r, 32, &plain()))
            .collect();
        assert_eq!(
            rows,
            [
                "linux-firmware-{amd,amdgpu,",
                "                atheros,intel,",
                "                nvidia}  1-1",
            ]
        );
    }

    #[test]
    fn plan_columns_differ_by_one_at_most() {
        let mut r = empty();
        for i in 0..40 {
            r.packages.push(crate::update_view::model::Pkg {
                name: format!("p{i:02}"),
                repo: Some("extra".into()),
                ..Default::default()
            });
        }
        // 16 rows: too few for 2 columns of 20, so names pack closer
        // (3 wide + 2 gap) into 3 columns.
        let out = compact_plan(&r, 13, 16, &plain());
        assert_eq!(out[2].trim_end(), "p00  p14  p27");
        assert_eq!(out[14].trim_end(), "p12  p26  p39");
        assert_eq!(out[15].trim_end(), "p13");
        assert_eq!(out.len(), 2 + 14);
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
            stalled_since: None,
            seen: Vec::new(),
            done_at: None,
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
            stalled_since: None,
            seen: Vec::new(),
            done_at: None,
        });
        // Colour too: the bar's escape codes must not eat the line's width.
        for st in [plain(), colour()] {
            for w in [80u16, 120, 154, 181] {
                let v = view(Layout::Inline, Motif::Subtle);
                let f = frame(&r, &v, Delivered::default(), w, 40, Z, None, &st).0;
                let row = f.region.iter().find(|l| l.contains("zed")).unwrap();
                let row = strip(row);
                // A narrow screen drops the numbers whole rather than cut them.
                assert!(row.ends_with("40%"), "w={w}: {row:?}");
                assert_eq!(row.contains("00:02:51"), w >= 120, "w={w}: {row:?}");
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
            "ᗧ·· 38 eaten in 2m04s · 702.9 MiB downloaded · +1.3 MiB on disk"
        );
        assert_eq!(
            end_line(&r, Motif::Subtle, &uni()),
            "✓ 38 eaten in 2m04s · 702.9 MiB downloaded · +1.3 MiB on disk"
        );
        assert_eq!(
            end_line(&r, Motif::Arcade, &plain()),
            "C.. 38 eaten in 2m04s - 702.9 MiB downloaded - +1.3 MiB on disk"
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
    fn arcade_receipt_heads_the_to_do_tier_with_a_ghost() {
        let c = closing(
            &fixture_run(),
            &view(Layout::Inline, Motif::Arcade),
            120,
            "/t",
            &uni(),
        );
        assert!(c.iter().any(|l| l == "  ᗣ to do"), "{c:?}");
        // A reboot ends the session: it is the last step.
        assert!(c.iter().any(|l| l.starts_with("    2 reboot")), "{c:?}");
    }

    #[test]
    fn newer_than_repo_reads_quiet_and_keeps_every_figure() {
        assert_eq!(
            newer_than_repo(
                "warning: foo: local (2-1) is newer than extra (1-1)",
                5,
                &uni()
            )
            .as_deref(),
            Some("≥ newer than repo  foo    2-1 › extra 1-1")
        );
        assert_eq!(newer_than_repo("warning: something else", 0, &uni()), None);
    }

    #[test]
    fn sentences_break_at_sentence_ends_first() {
        let t = "One sentence here. Run 'this' to fix it.";
        assert_eq!(
            sentences(t, 30),
            ["One sentence here.", "Run 'this' to fix it."]
        );
        assert_eq!(sentences(t, 80), [t]);
    }

    #[test]
    fn asks_user_reads_the_message_own_words() {
        assert!(asks_user("Warning: unit changed"));
        assert!(asks_user("  $ usermod -aG input $USER"));
        assert!(asks_user("Make sure your user is in the group"));
        assert!(!asks_user("You can add custom flags in user-flags.conf"));
    }

    /// A package's message at line `at` after the plan is answered.
    fn with_message(lines: &[&str]) -> Run {
        let all = fixture_lines();
        let mut r = partial(58);
        r.apply_line(&format!("{} y", all[58]), Z);
        r.apply_line(":: Processing package changes...", Z);
        r.apply_line("( 1/38) upgrading ydotool  [##] 100%", Z);
        for l in lines {
            r.apply_line(l, Z);
        }
        r.apply_line("(2/38) upgrading zed", Z);
        r
    }

    #[test]
    fn package_messages_hang_off_the_name_column() {
        let r = with_message(&[
            "Make sure your user is in the `input` group:",
            "  $ usermod -aG input $USER",
        ]);
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
        let msg: Vec<&String> = f.above.iter().filter(|l| l.contains(" | ")).collect();
        assert_eq!(msg.len(), 2, "{:?}", f.above);
        // The name once, right-aligned, then the gutter on every line.
        assert!(
            msg[0].trim_start().starts_with("ydotool | Make sure"),
            "{msg:?}"
        );
        assert!(msg[1].trim_start().starts_with("|   $ usermod"), "{msg:?}");
        assert_eq!(msg[0].find('|'), msg[1].find('|'));
    }

    #[test]
    fn a_dollar_line_in_a_package_message_is_a_step_to_do() {
        let r = with_message(&[
            "Make sure your user is in the `input` group:",
            "  $ usermod -aG input $USER",
        ]);
        let c = closing(
            &r,
            &view(Layout::Inline, Motif::Subtle),
            160,
            "/t",
            &plain(),
        );
        let step = c.iter().find(|l| l.contains("usermod -aG input $USER"));
        assert!(
            step.is_some_and(|l| l.contains("<- ydotool") && l.contains("input` group")),
            "{c:?}"
        );
    }

    #[test]
    fn a_quiet_package_message_is_good_to_know() {
        let r = with_message(&["You can add custom flags in user-flags.conf"]);
        let c = closing(
            &r,
            &view(Layout::Inline, Motif::Subtle),
            160,
            "/t",
            &plain(),
        );
        let at = c.iter().position(|l| l.contains("good to know"));
        let row = c
            .iter()
            .position(|l| l.contains("ydotool") && l.contains("custom flags"));
        assert!(at.zip(row).is_some_and(|(a, b)| b > a), "{c:?}");
    }

    #[test]
    fn track_without_colour_is_pacman_bar_in_brackets() {
        assert_eq!(track(50, 10, Mouth::Chew, &plain()), "[-----C o  ]");
        assert_eq!(track(100, 4, Mouth::Over, &plain()), "[---c]");
    }

    #[test]
    fn track_in_colour_is_the_maze() {
        let strip = |t: String| strip(&t);
        let t = strip(track(50, 10, Mouth::Chew, &colour()));
        assert_eq!(t.width(), 12);
        assert!(t.starts_with("──────ᗧ"), "{t}");
        assert!(t.ends_with('•'), "{t}");
        // Half a cell further, the mouth is shut: it chews as bytes arrive.
        // One half step in four the mouth is shut: it chews as bytes arrive.
        let t = strip(track(63, 10, Mouth::Chew, &colour()));
        assert!(t.contains('●') && !t.contains('ᗧ'), "{t}");
        // Stalled: shut; a ghost behind once it is a stall.
        let t = strip(track(50, 10, Mouth::Wait, &colour()));
        assert!(t.contains("─●") && !t.contains('ᗣ'), "{t}");
        let t = strip(track(50, 10, Mouth::Haunt, &colour()));
        assert!(t.contains("ᗣ─●"), "{t}");
        // An estimate looks like one.
        let t = strip(track(50, 10, Mouth::Guess, &colour()));
        assert!(
            t.starts_with("╌╌") && t.contains('◦') && t.ends_with('○'),
            "{t}"
        );
        let t = strip(track(99, 10, Mouth::Over, &colour()));
        assert!(t.ends_with("ᗣ╌●") && t.width() == 12, "{t}");
        assert_eq!(
            strip(track(100, 10, Mouth::Chew, &colour())),
            format!("{}✓", "─".repeat(11))
        );
    }

    #[test]
    fn ribbon_without_colour_is_one_row_of_labels() {
        let spans = [(
            "sync 2s".to_string(),
            Duration::from_secs(2),
            PhaseKind::Sync,
        )];
        assert_eq!(ribbon(&spans, 80, None, &plain()), ["  sync 2s"]);
    }

    #[test]
    fn ribbon_segments_follow_time_and_labels_fit() {
        let sec = Duration::from_secs;
        let spans = [
            ("sync 2s".to_string(), sec(2), PhaseKind::Sync),
            ("download 2m10s".to_string(), sec(130), PhaseKind::Download),
            ("packages 20s".to_string(), sec(20), PhaseKind::Packages),
        ];
        let r: Vec<String> = ribbon(&spans, 120, None, &colour())
            .iter()
            .map(|l| strip(l))
            .collect();
        // As wide as the bars: they end five columns from the edge.
        assert_eq!(r[0].trim().width(), 113, "{r:?}");
        // Solid; the legend under it in run order.
        assert!(r[0].trim().chars().all(|c| c == '━'), "{r:?}");
        assert_eq!(r[1], "  ■ sync 2s  ■ download 2m10s  ■ packages 20s");
        assert!(r.iter().all(|l| l.width() <= 120), "{r:?}");
        assert!(r.iter().any(|l| l.contains("packages 20s")), "{r:?}");
        assert!(r.iter().any(|l| l.contains("sync 2s")), "{r:?}");
    }

    #[test]
    fn command_bold_marks_only_the_command() {
        let c = colour();
        let l = command_bold("Run 'systemctl daemon-reload' to reload units.", &c);
        assert_eq!(strip(&l), "Run 'systemctl daemon-reload' to reload units.");
        assert!(l.contains(&c.title("systemctl daemon-reload")), "{l:?}");
        let l = command_bold("  $ usermod -aG input $USER", &c);
        assert!(l.starts_with("  $ ") && l.contains(&c.title("usermod -aG input $USER")));
        assert_eq!(command_bold("Run 'x' now", &plain()), "Run 'x' now");
        assert_eq!(command_bold("nothing here", &c), "nothing here");
    }

    #[test]
    fn a_step_drops_its_lead_in() {
        let r = with_message(&[
            "Make sure your user is in the `input` group by running the following command:",
            "  $ usermod -aG input $USER",
        ]);
        let c = closing(
            &r,
            &view(Layout::Inline, Motif::Subtle),
            160,
            "/t",
            &plain(),
        );
        assert!(
            c.iter().any(|l| l.contains(
                "usermod -aG input $USER  Make sure your user is in the `input` group  "
            )),
            "{c:?}"
        );
    }

    #[test]
    fn sentence_head_is_the_first_sentence_less_its_marks() {
        assert_eq!(
            sentence_head("==> INFO: Reboot is recommended due to x. More."),
            "Reboot is recommended due to x"
        );
        assert_eq!(
            sentence_head("Warning: The unit changed on disk."),
            "The unit changed on disk"
        );
    }

    #[test]
    fn preview_keeps_a_path_whole() {
        let t =
            "You can add custom permanent flags for Obsidian in .config/obsidian/user-flags.conf";
        assert_eq!(preview(t, 200), t);
        let p = preview(t, 60);
        assert!(
            p.ends_with(" .config/obsidian/user-flags.conf") && p.contains('…'),
            "{p}"
        );
        assert!(p.width() <= 60, "{p}");
        assert_eq!(preview("no path at all here", 10), "no path…");
    }

    #[test]
    fn a_short_wait_is_dim_and_a_stall_is_amber() {
        let ga = Gauge {
            left: 10,
            size: true,
            rate: true,
            cells: 10,
        };
        let c = colour();
        let t = |r: &str| {
            gauge_tail(
                &ga,
                50,
                false,
                false,
                None,
                Some(("1/2 MiB".into(), r.into(), String::new())),
                &c,
            )
        };
        let paint_of = |segs: Vec<Seg>| {
            segs.into_iter()
                .find(|x| x.1.contains("stalled"))
                .map(|x| x.0)
        };
        assert!(matches!(paint_of(t("stalled 2s")), Some(Paint::Dim)));
        assert!(matches!(paint_of(t("stalled")), Some(Paint::Dim)));
        assert!(matches!(paint_of(t("stalled 3s")), Some(Paint::Warn)));
    }

    #[test]
    fn comet_tail_spans_what_was_eaten_since() {
        let t = strip(&comet(50, 10, Mouth::Chew, Some(25), &colour()));
        assert!(t.starts_with("───━━━ᗧ"), "{t}");
        // No tail on a still file, or past a stall.
        let t = strip(&comet(50, 10, Mouth::Chew, Some(50), &colour()));
        assert!(t.starts_with("──────ᗧ"), "{t}");
        let t = strip(&comet(50, 10, Mouth::Wait, Some(25), &colour()));
        assert!(!t.contains('━'), "{t}");
    }

    #[test]
    fn cut_words_ends_on_a_word() {
        assert_eq!(
            cut_words("Make sure your user is in the group", 22),
            "Make sure your user…"
        );
        assert_eq!(cut_words("short", 22), "short");
    }

    #[test]
    fn ribbon_legend_rows_share_items_evenly() {
        let sec = Duration::from_secs;
        let spans: Vec<(String, Duration, PhaseKind)> = (0..7)
            .map(|i| (format!("phase{i} 10s"), sec(10), PhaseKind::Checks))
            .collect();
        let r: Vec<String> = ribbon(&spans, 60, None, &colour())
            .iter()
            .map(|l| strip(l))
            .collect();
        let counts: Vec<usize> = r[1..].iter().map(|l| l.matches('■').count()).collect();
        assert!(counts.iter().all(|&c| c >= 3), "{r:?}");
        assert_eq!(counts.iter().sum::<usize>(), 7);
    }

    #[test]
    fn receipt_eats_in_and_ends_as_itself() {
        let sec = Duration::from_secs;
        let spans = [
            ("download 2m10s".to_string(), sec(130), PhaseKind::Download),
            ("packages 20s".to_string(), sec(20), PhaseKind::Packages),
        ];
        let half: Vec<String> = ribbon(&spans, 80, Some(0.5), &colour())
            .iter()
            .map(|l| strip(l))
            .collect();
        // Pac-Man half way, pellets ahead; only the reached phase's legend.
        assert!(half[0].contains('ᗧ') && half[0].contains('·'), "{half:?}");
        assert!(
            half[1].contains("download") && !half[1].contains("packages"),
            "{half:?}"
        );
        assert_eq!(
            ribbon(&spans, 80, Some(1.0), &colour()),
            ribbon(&spans, 80, None, &colour())
        );
        // No ribbon (no timed phase), no colour: the receipt just appears.
        let r = fixture_run();
        let v = view(Layout::Inline, Motif::Subtle);
        assert!(closing_frames(&r, &v, 120, "/t", &colour()).is_empty());
        assert!(closing_frames(&r, &v, 120, "/t", &plain()).is_empty());
    }
}
